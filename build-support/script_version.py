#!/usr/bin/env python3
"""Freeze the existing application version scheme for one loose script."""

import argparse
import hashlib
import importlib.util
import json
import os
import stat
from pathlib import Path

from script_runtime import (
    MAX_ENTRY_BYTES,
    MAX_RECORD_BYTES,
    PRODUCT,
    bounded_file,
    check_snapshots,
    identity,
    input_snapshots,
    validate_record,
)


def generator_module():
    path = Path(__file__).resolve().with_name("app_version.py")
    spec = importlib.util.spec_from_file_location("script_app_version", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def stamp_script(root, entry):
    root = Path(root).resolve(strict=True)
    entry = Path(entry)
    requested = root / entry if not entry.is_absolute() else entry
    canonical = requested.resolve(strict=True)
    if not canonical.is_relative_to(root) or not PRODUCT.fullmatch(canonical.name):
        raise ValueError(
            "script entry must be named and contained inside its application repository"
        )
    data, before = bounded_file(canonical, MAX_ENTRY_BYTES)
    if not data.startswith(b"#!"):
        raise ValueError("only standalone shebang scripts can receive a script stamp")
    data.decode("utf-8")
    output = canonical.with_name(canonical.name + ".program-metadata.json")
    if output.exists() or output.is_symlink():
        metadata = output.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_size > MAX_RECORD_BYTES
            or metadata.st_uid not in (0, os.geteuid())
        ):
            raise ValueError("existing script stamp has unsupported type, owner or size")
    generator = generator_module()
    build = generator.version_record(root, "script:" + canonical.name, allocate=True)
    record = {
        "schema_version": 1,
        "product": canonical.name,
        "version": build["version"],
        "source_commit": build["commit"],
        "entry_sha256": hashlib.sha256(data).hexdigest(),
    }
    if (
        requested.resolve(strict=True) != canonical
        or identity(canonical.stat(follow_symlinks=False)) != before
        or generator.current_commit(root) != build["commit"]
    ):
        raise ValueError("script or Git HEAD changed during version reservation")
    generator.atomic_write(output, json.dumps(record, indent=2, sort_keys=True) + "\n")
    return record


def stamp_declared(
    root, entry, version, *, source_commit=None, input_root_depth=0, inputs=(), absent_inputs=()
):
    """Preserve an existing release declaration; never reserve a new build number."""
    root = Path(root).resolve(strict=True)
    requested = Path(entry)
    if not requested.is_absolute():
        requested = root / requested
    canonical = requested.resolve(strict=True)
    if not canonical.is_relative_to(root) or not PRODUCT.fullmatch(canonical.name):
        raise ValueError("declared script entry must be contained inside its repository")
    data, before = bounded_file(canonical, MAX_ENTRY_BYTES)
    if not data.startswith(b"#!"):
        raise ValueError("only standalone shebang scripts can receive a script stamp")
    data.decode("utf-8")
    generator = generator_module()
    commit = generator.current_commit(root) if source_commit is None else source_commit
    record = {
        "schema_version": 1,
        "product": canonical.name,
        "version": version,
        "source_commit": commit,
        "entry_sha256": hashlib.sha256(data).hexdigest(),
    }
    states = {}
    if inputs or absent_inputs:
        record.update(
            schema_version=2,
            input_root_depth=input_root_depth,
            inputs=[{"path": path, "sha256": "0" * 64} for path in inputs]
            + [{"path": path, "sha256": None} for path in absent_inputs],
        )
        validate_record(record, canonical.name)
        record["inputs"], states = input_snapshots(
            canonical, input_root_depth, record["inputs"], verify=False
        )
    elif input_root_depth:
        raise ValueError("an input root depth requires explicit inputs")
    validate_record(record, canonical.name)
    raw = json.dumps(record, indent=2, sort_keys=True) + "\n"
    if len(raw.encode("utf-8")) > MAX_RECORD_BYTES:
        raise ValueError("declared stamp exceeds its inspection limit")
    output = canonical.with_name(canonical.name + ".program-metadata.json")
    if output.exists() or output.is_symlink():
        metadata = output.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_size > MAX_RECORD_BYTES
            or metadata.st_uid not in (0, os.geteuid())
        ):
            raise ValueError("existing script stamp has unsupported type, owner or size")
    if (
        requested.resolve(strict=True) != canonical
        or identity(canonical.stat(follow_symlinks=False)) != before
        or (source_commit is None and generator.current_commit(root) != commit)
    ):
        raise ValueError("script or declared source commit changed during stamping")
    check_snapshots(states)
    generator.atomic_write(output, raw)
    return record


def python_guard():
    return """# Application version: exact flags exit before imports or application startup.
if __name__ == "__main__":
    import sys as _app_version_sys

    if _app_version_sys.argv[1:] in (["--version"], ["-V"]):
        import os as _app_version_os
        from pathlib import Path as _AppVersionPath

        _app_version_os.execv(
            _app_version_sys.executable,
            [
                _app_version_sys.executable,
                "-I",
                "-S",
                str(_AppVersionPath(__file__).resolve().parent / "build-support/script_runtime.py"),
                __file__,
            ],
        )
"""


def shell_guard(product):
    if not PRODUCT.fullmatch(product):
        raise ValueError("invalid shell script product")
    return """# Application version: exact flags exit before normal startup.
# shellcheck disable=SC3028 # Optional Bash sourcing detection with a POSIX-safe fallback.
if [ "${BASH_SOURCE:-$0}" = "$0" ] && [ "$#" -eq 1 ] &&
    { [ "$1" = "--version" ] || [ "$1" = "-V" ]; }; then
    _app_version_entry=$(readlink -f -- "$0") || exit 1
    if [ "${_app_version_entry##*/}" = "PRODUCT" ]; then
        exec python3 -I -S "${_app_version_entry%/*}/build-support/script_runtime.py" "$_app_version_entry"
    fi
fi
""".replace('"PRODUCT"', json.dumps(product))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--entry", required=True, type=Path)
    parser.add_argument("--declared-version")
    parser.add_argument("--source-commit")
    parser.add_argument("--input-root-depth", type=int, default=0)
    parser.add_argument("--input", action="append", default=[])
    parser.add_argument("--absent-input", action="append", default=[])
    arguments = parser.parse_args()
    if arguments.declared_version is not None:
        record = stamp_declared(
            arguments.root,
            arguments.entry,
            arguments.declared_version,
            source_commit=arguments.source_commit,
            input_root_depth=arguments.input_root_depth,
            inputs=arguments.input,
            absent_inputs=arguments.absent_input,
        )
    else:
        if (
            arguments.source_commit
            or arguments.input_root_depth
            or arguments.input
            or arguments.absent_input
        ):
            parser.error("declared input options require --declared-version")
        record = stamp_script(arguments.root, arguments.entry)
    print(f"{record['product']} {record['version']}")


if __name__ == "__main__":
    main()
