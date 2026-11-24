"""Isolated acceptance trials against packaged ELFs; never contact the desktop bus."""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import socket
import statistics
import subprocess
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release_capture", ROOT / "scripts/capture-release-diagnostics.py")
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def command(arguments, environment, timeout=20):
    result = subprocess.run(arguments, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"Fixture command failed: {arguments[0]}: {result.stdout[-4000:].decode(errors='replace')}")
    return result.stdout


@contextmanager
def private_bus(env):
    process = subprocess.Popen(["dbus-daemon", "--nofork", "--print-address=1", "--config-file=" + str(ROOT / "tests/firefox-private-bus.conf")],
                               env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        env["DBUS_SESSION_BUS_ADDRESS"] = process.stdout.readline().strip()
        if not env["DBUS_SESSION_BUS_ADDRESS"].startswith("unix:"):
            raise ValueError("Private fixture bus did not start")
        yield
    finally:
        process.terminate()
        process.wait(timeout=5)
        process.stdout.close()
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)


def environment(directory, mode="observable"):
    for name in ("home", "state", "runtime", "config", "data", "commands"):
        (directory / name).mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in ("systemctl", "kpackagetool6", "kwriteconfig6", "kbuildsycoca6", "kdotool"):
        path = directory / "commands" / name
        path.write_text("#!/bin/sh\nexit 1\n")
        path.chmod(0o700)
    # Deliberately do not inherit credentials, a display, a session bus, service
    # sockets, shell hooks or any of the user's application configuration.
    return dict(PATH=f"{directory / 'commands'}:/usr/bin:/bin", HOME=str(directory / "home"),
                XDG_STATE_HOME=str(directory / "state"), XDG_RUNTIME_DIR=str(directory / "runtime"),
                XDG_CONFIG_HOME=str(directory / "config"), XDG_DATA_HOME=str(directory / "data"),
                LC_ALL="C.UTF-8", APPLICATIONLAUNCHER_DIAGNOSTICS=mode)


def summary(samples):
    values = sorted(samples)
    return dict(samples=len(values), median=statistics.median(values), p95=values[int((len(values)-1)*.95)],
                p99=values[int((len(values)-1)*.99)], maximum=max(values))


def snapshot(path):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(2)
        stream.connect(str(path))
        stream.sendall(b"snapshot\n")
        stream.shutdown(socket.SHUT_WR)
        parts, total = [], 0
        while data := stream.recv(65536):
            total += len(data)
            if total > 1024 * 1024:
                raise ValueError("Semantic snapshot exceeded its budget")
            parts.append(data)
    return json.loads(b"".join(parts))


def daemon_trial(binary, directory, mode):
    fixture = tempfile.TemporaryDirectory(prefix="al-daemon-")
    env = environment(Path(fixture.name), mode)
    bus = subprocess.Popen(["dbus-daemon", "--nofork", "--print-address=1", "--config-file=" + str(ROOT / "tests/firefox-private-bus.conf")],
                           env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    child = None
    try:
        env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
        if not env["DBUS_SESSION_BUS_ADDRESS"].startswith("unix:"):
            raise ValueError("Fixture private bus did not start")
        child = subprocess.Popen([str(binary)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 10
        while True:
            if child.poll() is not None:
                raise ValueError("Private fixture daemon exited before readiness")
            ready = subprocess.run(["gdbus", "call", "--session", "--dest", "com.terrydaktal.ApplicationLauncher",
                                    "--object-path", "/Tracker", "--method", "org.freedesktop.DBus.Peer.Ping"],
                                   env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
            if ready.returncode == 0:
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Private daemon did not become ready")
            time.sleep(.02)
        before = capture.process_identity(child.pid)
        time.sleep(1.2)
        after = capture.process_identity(child.pid)
        ticks = os.sysconf("SC_CLK_TCK")
        cpu = (after["cpu_ticks"] - before["cpu_ticks"]) / ticks / 1.2 * 100
        report = dict(pid=child.pid, mode=mode, idle_cpu_percent=cpu,
                      rss_bytes=after["rss_pages"] * os.sysconf("SC_PAGE_SIZE"), process_identity=after)
        report["external_capture"] = capture.capture(directory / "capture", pid=child.pid)
        socket_path = Path(env["XDG_RUNTIME_DIR"]) / "applicationlauncher-diagnostics" / f"daemon-{child.pid}.sock"
        if socket_path.exists():
            value = snapshot(socket_path)
            if value["pid"] != child.pid or value["consistency"] != "best-effort":
                raise ValueError("Snapshot identity or consistency was incorrect")
            (directory / "semantic-snapshot.json").write_text(json.dumps(value, indent=2) + "\n")
            report["snapshot"] = dict(events=len(value["events"]), workers=len(value["workers"]), mode=value["mode"])
        elif mode != "release-minimal":
            raise ValueError("Observable fixture has no independent diagnostic endpoint")
        if cpu > 2.0:
            raise ValueError("Private idle workload exceeded its 2% CPU guard (short-window timer quantization included)")
        return report
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        bus.terminate()
        bus.wait(timeout=5)
        bus.stdout.close()
        fixture.cleanup()


def archived_symbols(binary, archive, directory):
    notes = subprocess.check_output(["readelf", "-n", str(binary)], text=True, timeout=10)
    build_id = re.search(r"Build ID: ([a-f0-9]+)", notes)[1]
    archived = archive / "by-build-id" / build_id
    info = json.loads((archived / "build-info.json").read_text())
    directory.mkdir(mode=0o700)
    for role in ("stripped", "debug"):
        item = info["artifacts"][role]
        data = (archived / item["file"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != item["sha256"]:
            raise ValueError("Archived ELF/symbol checksum mismatch")
        target = directory / item["file"]
        target.write_bytes(data)
        target.chmod(0o700 if role == "stripped" else 0o600)
    source = Path(info["source"]["archive"])
    if hashlib.sha256(source.read_bytes()).hexdigest() != info["source"]["archive_sha256"]:
        raise ValueError("Archived source checksum mismatch")
    with tarfile.open(source) as stream:
        stream.extractall(directory / "inputs", filter="data")
    source_info = json.loads((directory / "inputs/artifact-source.json").read_text())
    postimages = json.loads(subprocess.check_output([str(binary), "--lifecycle-build-info"], timeout=5))
    for name, item in postimages["files"].items():
        data = (directory / "inputs/generated/lifecycle" / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != item["postimage_sha256"]:
            raise ValueError("Archived generated Python does not match the exact executable")
    native_paths = []
    for item in source_info["native_modules"]:
        module = directory / "inputs" / item["member"]
        if hashlib.sha256(module.read_bytes()).hexdigest() != item["sha256"]:
            raise ValueError("Archived native module checksum mismatch")
        native_paths.append(str(module.parent))
    env = environment(directory / "fixture")
    executable = directory / "applicationlauncherd"
    core = directory / "fixture.core"
    prefix = ["gdb", "--nx", "--nh", "--batch", "--quiet", "-iex", "set auto-load off",
              "-iex", "set debuginfod enabled off", "-ex", "set pagination off", "-ex", "set print frame-arguments none",
              "-ex", "set sysroot " + str(directory / "empty-sysroot"), "-ex", "set solib-search-path " + ":".join(native_paths),
              "-ex", f"set substitute-path {ROOT.parent} {directory / 'inputs/source'}"]
    live = command(prefix + ["-ex", "break applicationlauncherd::main", "-ex", "run", "-ex", "thread apply all bt",
                            "-ex", "generate-core-file " + str(core), "-ex", "kill", "--args", str(executable), "--version"], env, timeout=45)
    (directory / "live-parent-debugger.txt").write_bytes(live)
    if not core.exists() or core.stat().st_size > 128 * 1024 * 1024:
        raise ValueError("Private startup core is missing or exceeds 128 MiB")
    post = command(prefix + ["-ex", "thread apply all bt", "-ex", "list applicationlauncherd::main", str(executable), str(core)], env)
    (directory / "post-exit-symbolization.txt").write_bytes(post)
    if b"applicationlauncherd::main" not in post or b"applicationlauncherd.rs" not in post:
        raise ValueError("Exact archived symbols did not recover the product source frame")
    # The fixture stops before product initialization, not at a planted crash.
    # This proves exact post-exit retrieval, not real crash causal coverage.
    size = core.stat().st_size
    core.unlink()
    return dict(elf_build_id=build_id, native_modules=len(native_paths), core_bytes=size,
                scope="archive-only startup stop; not a crash-localization experiment")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, choices=("release-minimal", "release-observable", "diagnostic-build"))
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    arguments = parser.parse_args()
    os.umask(0o077)
    output = arguments.artifacts / "acceptance"
    output.mkdir(mode=0o700, exist_ok=True)
    binary = (arguments.artifacts / "applicationlauncherd").resolve()
    started = time.monotonic()
    report = dict(schema_version=1, catalog_id="release-evidence-v1", profile=arguments.profile, trials=[], measurements={})
    try:
        with tempfile.TemporaryDirectory(prefix="release-identity-") as temporary:
            env = environment(Path(temporary))
            samples = []
            for _ in range(31):
                before = time.perf_counter_ns()
                value = json.loads(command([str(binary), "--build-info"], env))
                samples.append(time.perf_counter_ns() - before)
                if value["artifact_profile"] != arguments.profile:
                    raise ValueError("Clean identity control had the wrong build identity")
            report["measurements"]["identity_startup_ns"] = summary(samples)
            job = Path(temporary) / "job"
            job.mkdir(mode=0o700)
            request = job / "request.json"
            request.write_text("{}")
            with private_bus(env):
                command([str(binary), "--lifecycle-worker", "program", str(request)], env)
            result = json.loads((job / "result.json").read_text())
            (output / "invalid-input.json").write_text(json.dumps(result, indent=2) + "\n")
            if result["ok"] or result["failure"]["code"] != "invalid-input" or result["failure"]["causes"][0]["kind"] != "KeyError":
                raise ValueError("Malformed request did not retain its original typed cause; inspect invalid-input.json")
            report["trials"].append(dict(scenario="malformed-worker-input", phase="post_exit", status="complete", detected=True, correctly_localized=True))
        modes = ["release-minimal"] if arguments.profile == "release-minimal" else ["off", "observable"]
        for mode in modes:
            trial_dir = Path(tempfile.mkdtemp(prefix="daemon-", dir=output))
            report["measurements"][mode] = daemon_trial(binary, trial_dir, mode)
            report["trials"].append(dict(scenario="healthy-idle", phase="live", mode=mode, status="complete", false_alarm=False))
        report["symbolization"] = archived_symbols(binary, arguments.archive, Path(tempfile.mkdtemp(prefix="symbols-", dir=output)) / "archive")
        report["trials"].append(dict(scenario="archive-symbol-recovery", phase="post_exit", status="complete", detected=True, correctly_localized=True))
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["failure"] = dict(type=type(error).__name__, message=str(error))
        raise
    finally:
        report["seconds"] = time.monotonic() - started
        report["not_run"] = ["real GUI/GPU fault capture", "minimal live sibling-debugger permission", "heap-leak localization", "historical pre-instrumentation whole-GUI comparison"]
        (output / "trials.json").write_text(json.dumps(report, indent=2) + "\n")
        history = output / "history"
        history.mkdir(mode=0o700, exist_ok=True)
        receipt = history / f"{time.time_ns()}-{report['status']}.json"
        receipt.write_text(json.dumps(report, indent=2) + "\n")
        for old in sorted(history.glob("*.json"))[:-8]:
            old.unlink()


if __name__ == "__main__":
    main()
