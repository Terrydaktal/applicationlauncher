"""Retain hash-verified compile inputs and immutable registry source packages."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MAX_SOURCE_BYTES = 512 * 1024 * 1024


def archive(binary, directory):
    identity = json.loads(subprocess.check_output([str(binary), "--build-info"], timeout=5))
    entries = []
    total = 0
    for item in identity["source_inputs"]:
        path = (ROOT / item["path"]).resolve()
        if not any(path.is_relative_to(ROOT.parent / name) for name in (ROOT.name, "program-metadata", "fuzzy-rank")):
            raise ValueError("Compile input is outside the declared source closure")
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != item["sha256"]:
            raise ValueError(f"Compile input changed after linking: {item['path']}; rebuild before archiving")
        entries.append(("source/" + str(path.relative_to(ROOT.parent)), data))
        total += len(data)
        if total > MAX_SOURCE_BYTES or len(entries) > 8192:
            raise ValueError("Source archive exceeded the declared input budget")
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--offline", "--locked", "--format-version", "1"], cwd=ROOT, timeout=30))
    for package in metadata["packages"]:
        if not (package.get("source") or "").startswith("registry+"):
            continue
        manifest = Path(package["manifest_path"])
        index = manifest.parents[1].name
        cache = manifest.parents[3] / "cache" / index / f"{package['name']}-{package['version']}.crate"
        data = cache.read_bytes()
        entries.append(("registry/" + cache.name, data))
        total += len(data)
        if total > MAX_SOURCE_BYTES or len(entries) > 8192:
            raise ValueError("Source archive exceeded the declared input budget")
    build_root = ROOT / "target/release-minimal-source" if identity["artifact_profile"] == "release-minimal" else ROOT
    lock = (build_root / "Cargo.lock").read_bytes()
    if hashlib.sha256(lock).hexdigest() != identity["actual_lock_sha256"]:
        raise ValueError("The resolved Cargo lock no longer matches this exact artifact")
    entries.extend([("generation/Cargo.lock", lock), ("generation/Cargo.toml", (build_root / "Cargo.toml").read_bytes())])
    postimages = json.loads(subprocess.check_output([str(binary), "--lifecycle-build-info"], timeout=5))
    for name, item in postimages["files"].items():
        candidates = (ROOT / "target/profile-build/release/build").glob("applicationlauncher-*/out/lifecycle/" + name)
        data = next((path.read_bytes() for path in candidates if hashlib.sha256(path.read_bytes()).hexdigest() == item["postimage_sha256"]), None)
        if data is None:
            raise ValueError(f"Exact generated postimage unavailable: {name}")
        entries.append(("generated/lifecycle/" + name, data))
        total += len(data)
    native = []
    dependencies = subprocess.check_output(["ldd", str(binary)], text=True, timeout=10)
    paths = set(re.findall(r"(?:=>\s+|^\s*)(/\S+)", dependencies, re.MULTILINE))
    if len(paths) > 128:
        raise ValueError("Native dependency count exceeds the archive budget")
    for name in sorted(paths):
        path = Path(name).resolve(strict=True)
        data = path.read_bytes()
        total += len(data)
        if total > MAX_SOURCE_BYTES:
            raise ValueError("Native dependency closure exceeds the archive budget")
        digest = hashlib.sha256(data).hexdigest()
        member = "native/" + digest + "/" + path.name
        entries.append((member, data))
        notes = subprocess.check_output(["readelf", "-n", str(path)], text=True, timeout=10)
        build_id = re.search(r"Build ID: ([a-f0-9]+)", notes)
        native.append(dict(path=str(path), member=member, sha256=digest, elf_build_id=build_id[1] if build_id else None))
    identity["native_modules"] = native
    manifest_bytes = (json.dumps(identity, sort_keys=True, separators=(",", ":")) + "\n").encode()
    key = hashlib.sha256(manifest_bytes).hexdigest()
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    target = directory / (key + ".tar.gz")
    if target.exists():
        return target
    descriptor, temporary = tempfile.mkstemp(prefix=".source-", dir=directory)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            with gzip.GzipFile(filename="", mode="wb", compresslevel=6, mtime=0, fileobj=stream) as compressed:
                with tarfile.open(mode="w|", fileobj=compressed) as output:
                    for name, data in sorted([*entries, ("artifact-source.json", manifest_bytes)]):
                        header = tarfile.TarInfo(name)
                        header.size, header.mode, header.mtime = len(data), 0o600, 0
                        output.addfile(header, io.BytesIO(data))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, target)
    finally:
        Path(temporary).unlink(missing_ok=True)
    return target


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("directory", type=Path)
    arguments = parser.parse_args()
    print(archive(arguments.binary.resolve(), arguments.directory.resolve()))


if __name__ == "__main__":
    main()
