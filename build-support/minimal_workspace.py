"""Materialize a disposable same-source workspace; never alter the user's lockfile."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


ERASED_SOURCES = {"observability.rs", "diagnostic_capture.rs", "replay.rs", "lifecycle/timing.rs", "lifecycle/timing.py"}


def prepare(destination, *, negative=False):
    destination.mkdir(parents=True, exist_ok=True)
    if negative:
        source = destination / "src"
        source.mkdir(exist_ok=True)
        for path in (ROOT / "src").rglob("*"):
            relative = path.relative_to(ROOT / "src")
            if relative.as_posix() in ERASED_SOURCES or "__pycache__" in relative.parts:
                continue
            target = source / relative
            if path.is_dir():
                target.mkdir(exist_ok=True)
            elif not target.is_symlink():
                target.symlink_to(path)
    for name in (("build-support", ".cargo", "kwin") if negative else ("src", "build-support", ".cargo", "kwin")):
        link = destination / name
        if not link.is_symlink():
            link.symlink_to(ROOT / name, target_is_directory=True)
    for name in ("build.rs", "APP-VERSION"):
        link = destination / name
        if not link.is_symlink():
            link.symlink_to(ROOT / name)
    manifest = (ROOT / "Cargo.toml").read_text()
    for name in ("fuzzy-rank", "program-metadata"):
        manifest = manifest.replace(f'path = "../{name}"', f'path = "{ROOT.parent / name}"')
    manifest += f'\n[patch.crates-io]\ntracing = {{ path = "{ROOT / "build-support/erased-tracing"}" }}\nlog = {{ path = "{ROOT / "build-support/erased-log"}" }}\n'
    (destination / "Cargo.toml").write_text(manifest)
    shutil.copyfile(ROOT / "Cargo.lock", destination / "Cargo.lock")
    (destination / "source-root.json").write_text(json.dumps(dict(root=str(ROOT),
        manifest_sha256=hashlib.sha256((ROOT / "Cargo.toml").read_bytes()).hexdigest())) + "\n")
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--negative", action="store_true", help="Compile with optional observer source modules absent")
    parser.add_argument("--test", action="store_true", help="Run product library tests in the generated workspace")
    arguments = parser.parse_args()
    destination = prepare(ROOT / ("target/minimal-negative-source" if arguments.negative else "target/release-minimal-source"), negative=arguments.negative)
    command = ["cargo", "test" if arguments.test else "check" if arguments.check else "build", "--offline", "--release", "--no-default-features",
               "--features", "release-minimal", "--lib" if arguments.test else "--bins", "--manifest-path", str(destination / "Cargo.toml"),
               "--target-dir", str(ROOT / "target/profile-build"), "--quiet", "--message-format=short"]
    subprocess.run(command, check=True, cwd=ROOT)


if __name__ == "__main__":
    main()
