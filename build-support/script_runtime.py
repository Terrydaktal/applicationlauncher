#!/usr/bin/env python3
"""Read an adjacent, exact-content script stamp without Git or allocation."""

import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path

MAX_ENTRY_BYTES = 1024 * 1024
MAX_RECORD_BYTES = 8 * 1024
MAX_INPUT_BYTES = 1024 * 1024
MAX_TOTAL_INPUT_BYTES = 4 * 1024 * 1024
FIELDS = {"schema_version", "product", "version", "source_commit", "entry_sha256"}
FIELDS_V2 = FIELDS | {"input_root_depth", "inputs"}
DECIMAL = r"(?:0|[1-9][0-9]*)"
VERSION = re.compile(rf"{DECIMAL}(?:\.{DECIMAL})+(?:[-+~][A-Za-z0-9][A-Za-z0-9._+~()-]*)?\Z")
ZERO_BUILD = re.compile(rf"0\.0\.0-{DECIMAL}\Z")
PRODUCT = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.+-]{0,127}\Z")
COMMIT = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64}|unborn)\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


def identity(metadata):
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def bounded_file(path, limit, *, protected=False):
    """No FIFO blocking or leaf symlink traversal; require stable regular files."""
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise ValueError("file type or inspection limit is unsupported")
        if protected and (before.st_uid not in (0, os.geteuid()) or before.st_mode & 0o022):
            raise ValueError("script stamp has unsafe ownership or permissions")
        chunks = []
        length = 0
        while length <= limit:
            chunk = os.read(descriptor, min(65536, limit + 1 - length))
            if not chunk:
                break
            chunks.append(chunk)
            length += len(chunk)
        if length > limit:
            raise ValueError("file exceeds inspection limit")
        if identity(os.fstat(descriptor)) != identity(before):
            raise ValueError("file changed during inspection")
        return b"".join(chunks), identity(before)
    finally:
        os.close(descriptor)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate script stamp field")
        result[key] = value
    return result


def valid_version(value):
    if not isinstance(value, str) or len(value) > 128 or not VERSION.fullmatch(value):
        return False
    core = re.split(r"[-+~]", value, maxsplit=1)[0]
    return (
        ".." not in value
        and not value.endswith(".")
        and (any(part != "0" for part in core.split(".")) or bool(ZERO_BUILD.fullmatch(value)))
    )


def input_path(value):
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > 512
        or value.startswith("/")
        or "\x00" in value
        or any(part in ("", ".", "..") for part in value.split("/"))
    ):
        raise ValueError("input paths must be bounded normalized relative paths")
    return value


def validate_record(record, product):
    if (
        not isinstance(record, dict)
        or type(record.get("schema_version")) is not int
        or record["schema_version"] not in (1, 2)
        or set(record) != (FIELDS if record["schema_version"] == 1 else FIELDS_V2)
    ):
        raise ValueError("unsupported script stamp schema")
    for name in ("product", "version", "source_commit", "entry_sha256"):
        if not isinstance(record[name], str):
            raise ValueError("script stamp fields must be strings")
    if not PRODUCT.fullmatch(record["product"]) or record["product"] != product:
        raise ValueError("script stamp product does not match the exact entry")
    if not valid_version(record["version"]):
        raise ValueError("unsupported script application version")
    if not COMMIT.fullmatch(record["source_commit"]):
        raise ValueError("unsupported script source commit")
    if not SHA256.fullmatch(record["entry_sha256"]):
        raise ValueError("invalid script content digest")
    if record["schema_version"] == 2:
        depth = record["input_root_depth"]
        if type(depth) is not int or not 0 <= depth <= 4:
            raise ValueError("unsupported input root depth")
        inputs = record["inputs"]
        if not isinstance(inputs, list) or not 1 <= len(inputs) <= 16:
            raise ValueError("input snapshots require between one and sixteen paths")
        seen = set()
        for item in inputs:
            if not isinstance(item, dict) or set(item) != {"path", "sha256"}:
                raise ValueError("unsupported input snapshot fields")
            path = input_path(item["path"])
            if path in seen:
                raise ValueError("duplicate input snapshot path")
            seen.add(path)
            if item["sha256"] is not None and (
                not isinstance(item["sha256"], str) or not SHA256.fullmatch(item["sha256"])
            ):
                raise ValueError("invalid input snapshot digest")
    return record


def input_snapshots(entry, depth, inputs, *, verify=True):
    """Hash only explicitly declared inputs, without following directory or leaf links."""
    root = entry.parent
    for _ in range(depth):
        if root.parent == root:
            raise ValueError("input root escapes the entry ancestry")
        root = root.parent
    states = {}

    def remember(path, *, directory=False, absent=False, ancestor=False):
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            if absent:
                states[path] = None
                return
            raise
        if absent or (directory and not stat.S_ISDIR(metadata.st_mode)):
            raise ValueError("input absence or directory snapshot does not match")
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError("input snapshots cannot traverse symbolic links")
        current = ("ancestor", metadata.st_dev, metadata.st_ino) if ancestor else identity(metadata)
        if path in states and states[path] != current:
            raise ValueError("input path changed during inspection")
        states[path] = current

    # Check every ancestor, not just the leaf, before opening any donor.
    for directory in reversed((root, *root.parents)):
        remember(directory, directory=True, ancestor=directory != root)
    result = []
    total = 0
    for item in inputs:
        relative = input_path(item["path"])
        path = root
        for component in relative.split("/")[:-1]:
            path = path / component
            remember(path, directory=True)
        path = path / relative.split("/")[-1]
        expected = item["sha256"]
        if expected is None:
            remember(path, absent=True)
            digest = None
        else:
            remember(path)
            data, before = bounded_file(path, MAX_INPUT_BYTES)
            if states[path] != before:
                raise ValueError("input changed before inspection")
            total += len(data)
            if total > MAX_TOTAL_INPUT_BYTES:
                raise ValueError("input snapshots exceed their total inspection limit")
            digest = hashlib.sha256(data).hexdigest()
            if verify and digest != expected:
                raise ValueError("declared version input changed since stamping")
        result.append({"path": relative, "sha256": digest})
    check_snapshots(states)
    return result, states


def check_snapshots(states):
    for path, before in states.items():
        try:
            metadata = path.lstat()
            if before is not None and before[0] == "ancestor":
                after = ("ancestor", metadata.st_dev, metadata.st_ino)
                if not stat.S_ISDIR(metadata.st_mode):
                    raise ValueError("input ancestor changed during inspection")
            else:
                after = identity(metadata)
        except FileNotFoundError:
            after = None
        if after != before:
            raise ValueError("input path changed during inspection")


def read_record(entry):
    requested = Path(entry)
    canonical = requested.resolve(strict=True)
    data, entry_identity = bounded_file(canonical, MAX_ENTRY_BYTES)
    if not data.startswith(b"#!"):
        raise ValueError("entry is not a standalone script")
    stamp = canonical.with_name(canonical.name + ".program-metadata.json")
    raw, stamp_identity = bounded_file(stamp, MAX_RECORD_BYTES, protected=True)
    record = validate_record(
        json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object), canonical.name
    )
    if hashlib.sha256(data).hexdigest() != record["entry_sha256"]:
        raise ValueError("script changed since its version was stamped; run make build")
    states = {}
    if record["schema_version"] == 2:
        _, states = input_snapshots(canonical, record["input_root_depth"], record["inputs"])
    if (
        requested.resolve(strict=True) != canonical
        or identity(canonical.stat(follow_symlinks=False)) != entry_identity
        or identity(stamp.stat(follow_symlinks=False)) != stamp_identity
    ):
        raise ValueError("script or stamp changed during inspection")
    check_snapshots(states)
    return record


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    if len(arguments) != 1:
        print("script_runtime.py: expected one exact script path", file=sys.stderr)
        return 2
    try:
        record = read_record(arguments[0])
    except (OSError, ValueError, UnicodeError, RecursionError) as error:
        print(f"script version unavailable: {error}", file=sys.stderr)
        return 1
    print(f"{record['product']} {record['version']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
