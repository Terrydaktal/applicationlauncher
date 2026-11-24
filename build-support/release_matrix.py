"""Build the supported artifact matrix without changing optimization or live processes."""
import argparse
import json
import importlib.util
import fcntl
import os
from pathlib import Path
import shutil
import selectors
import signal
import subprocess
import sys
import tempfile
import time

_spec = importlib.util.spec_from_file_location("minimal_workspace", Path(__file__).with_name("minimal_workspace.py"))
_workspace = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_workspace)
prepare, ROOT = _workspace.prepare, _workspace.ROOT

COMPONENTS = ("applicationlauncher", "applicationlauncherd")
BUILD = ROOT / "target/profile-build"


def run(command, log):
    process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + 1800
    tail = bytearray(log.read_bytes()[-1048576:] if log.exists() else b"")
    try:
        while selector.get_map():
            if time.monotonic() > deadline:
                raise TimeoutError("Release gate exceeded 30 minutes")
            for key, _ in selector.select(timeout=0.25):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                else:
                    tail.extend(data)
                    del tail[:-1048576]
                    log.write_bytes(tail)
        process.wait(timeout=max(1, deadline - time.monotonic()))
        if process.returncode:
            raise RuntimeError(f"Command failed ({process.returncode}); inspect {log}: {command[0]}")
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        process.stdout.close()
        selector.close()


def package(profile, output):
    directory = output / profile
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in COMPONENTS:
        source = BUILD / "release" / name
        descriptor, temporary = tempfile.mkstemp(prefix=".package-", dir=directory)
        os.close(descriptor)
        try:
            subprocess.run(["objcopy", "--compress-debug-sections=zstd", str(source), temporary], check=True)
            os.chmod(temporary, 0o700)
            os.replace(temporary, directory / name)
        finally:
            Path(temporary).unlink(missing_ok=True)
    return directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--keep-builds", type=int, default=3)
    parser.add_argument("--only", choices=("release-minimal", "release-observable", "diagnostic-build"))
    arguments = parser.parse_args()
    os.umask(0o077)
    output = ROOT / "target/release-profiles"
    output.mkdir(mode=0o700, parents=True, exist_ok=True)
    lock = (output / ".matrix.lock").open("w")
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    if not (BUILD / "release").exists():
        BUILD.mkdir(mode=0o700, parents=True, exist_ok=True)
        # Seed immutable compilation inputs with COW copies, never writable links
        # to the installed executable or its active Cargo build directory.
        seeded = subprocess.run(["cp", "--reflink=always", "-a", str(ROOT / "target/release"), str(BUILD / "release")], capture_output=True)
        if seeded.returncode:
            (BUILD / "release").mkdir(parents=True, exist_ok=True)
    variants = ("release-minimal", "diagnostic-build", "release-observable")
    receipts = []
    for profile in variants:
        if arguments.only and profile != arguments.only:
            continue
        log = output / (profile + "-build.log")
        if log.exists() and log.stat().st_size:
            history = output / "gate-history"
            history.mkdir(mode=0o700, exist_ok=True)
            shutil.copyfile(log, history / f"{time.time_ns()}-{profile}.log")
            for old in sorted(history.glob("*.log"))[:-8]:
                old.unlink()
        log.write_bytes(b"")
        command = ["cargo", "build", "--offline", "--release", "--bins", "--quiet", "--message-format=short", "--target-dir", str(BUILD)]
        if profile == "release-minimal":
            workspace = prepare(ROOT / "target/release-minimal-source")
            command += ["--no-default-features", "--features", "release-minimal", "--manifest-path", str(workspace / "Cargo.toml")]
        elif profile == "diagnostic-build":
            command += ["--locked", "--features", "diagnostic-build"]
        else:
            command += ["--locked"]
        print(f"Building {profile}; log: {log}", flush=True)
        run(command, log)
        directory = package(profile, output)
        if profile == "release-minimal":
            run([sys.executable, "-I", "-B", str(ROOT / "build-support/minimal_workspace.py"), "--negative", "--test"], log)
        elif profile == "diagnostic-build":
            run(["cargo", "test", "--offline", "--locked", "--release", "--features", "diagnostic-build",
                 "--test", "production_diagnostics", "--quiet", "--message-format=short", "--target-dir", str(BUILD)], log)
        verify = [sys.executable, "-I", "-B", str(ROOT / "tests/verify_release_artifact.py"),
                  "--profile", profile, "--output", str(directory / "verification.json"), *map(str, (directory / name for name in COMPONENTS))]
        run(verify, log)
        archive = [str(ROOT / "scripts/build-debuggable-release"), "--no-build", "--binary-dir", str(directory), "--keep-builds", str(arguments.keep_builds)]
        if arguments.archive:
            archive += ["--archive", str(arguments.archive)]
        run(archive, log)
        archive_root = arguments.archive or Path(os.environ.get("APPLICATIONLAUNCHER_BUILD_ARCHIVE", str(Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "applicationlauncher/builds")))
        run([sys.executable, "-I", "-B", str(ROOT / "tests/verify_release_diagnosis.py"),
             "--profile", profile, "--artifacts", str(directory), "--archive", str(archive_root)], log)
        receipt = json.loads((directory / "verification.json").read_text())
        receipts.append(receipt)
    if not arguments.only:
        digests = {row["artifacts"][0]["identity"]["product_semantic_sha256"] for row in receipts}
        if len(digests) != 1:
            raise RuntimeError("Artifact matrix product semantics differ; source may have changed during the build")
        python_semantics = [
            {row["path"]: (row["source_sha256"], row["semantic_sha256"])
             for row in receipt["artifacts"][-1]["python_postimages"]}
            for receipt in receipts
        ]
        if any(value != python_semantics[0] for value in python_semantics[1:]):
            raise RuntimeError("Generated Python product semantics differ across release profiles")
        failures = [json.loads((output / profile / "acceptance/invalid-input.json").read_text())["failure"] for profile in variants]
        if any(value != failures[0] for value in failures[1:]):
            raise RuntimeError("Packaged workers changed a required typed failure across profiles")
        run([sys.executable, "-I", "-B", str(ROOT / "tests/verify_performance_closure.py"),
             "--output", str(output / "performance-closure.json"),
             *[str(output / profile / "applicationlauncher") for profile in variants]], output / "matrix-gates.log")
    run([sys.executable, "-I", "-B", "-m", "unittest", "discover", "-s", "tests", "-p", "test_debuggability_contract.py"], output / "matrix-gates.log")
    run([sys.executable, "-B", str(ROOT / "tests/benchmark_lifecycle_timing.py"), str(output / "lifecycle-overhead.json")], output / "matrix-gates.log")
    if not arguments.only or arguments.only == "release-observable":
        # Publish only after the matrix's gates, never while another variant is
        # compiling. A source change or a failed proof preserves the old pair.
        for name in COMPONENTS:
            descriptor, temporary = tempfile.mkstemp(prefix=".observable-", dir=ROOT / "target/release")
            os.close(descriptor)
            try:
                shutil.copyfile(output / "release-observable" / name, temporary)
                os.chmod(temporary, 0o700)
                os.replace(temporary, ROOT / "target/release" / name)
            finally:
                Path(temporary).unlink(missing_ok=True)
    receipt_path = output / ("matrix.json" if not arguments.only else arguments.only + "-matrix.json")
    receipt_path.write_text(json.dumps(dict(schema_version=1, complete_matrix=not bool(arguments.only), default="release-observable", artifacts=receipts), indent=2) + "\n")
    print(f"Verified artifact matrix: {receipt_path}; no running process was restarted")


if __name__ == "__main__":
    main()
