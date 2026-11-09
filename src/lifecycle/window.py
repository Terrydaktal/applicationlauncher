"""Daemon-owned window update coordinator. Terminal helpers contain no app adapters."""
import json
import hashlib
import fcntl
import copy
import os
from pathlib import Path
import signal
import shutil
import stat
import time

import lifecycle_worker as worker


def private_request(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_mode & 0o077 or metadata.st_nlink != 1):
            raise worker.Refused("Lifecycle request must be a private same-user regular file")
        data = stream.read(worker.LIMIT + 1)
    if len(data) > worker.LIMIT:
        raise worker.Refused("Lifecycle request exceeds its size limit")
    return json.loads(data)


def atomic_record(path, value):
    path = Path(path)
    data = json.dumps(value).encode()
    if len(data) > worker.LIMIT:
        raise worker.Refused("Lifecycle recovery metadata exceeds its bounded size limit")
    temporary = path.with_name(path.name + ".tmp-" + str(os.getpid()))
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        temporary.unlink(missing_ok=True)


def scope_fingerprint(tabs):
    values = []
    for tab in tabs:
        program = copy.deepcopy(tab["program"])
        program["argv"][0] = "captured-running-image"
        values.append(dict(program=program, record=worker.binding_identity(tab["record"]),
                           active=tab["active"], cwd=tab["cwd"], environment=tab.get("environment", {}),
                           return_shell={key: tab["return_shell"][key] for key in ("source", "image", "argv0")}
                               if "return_shell" in tab else None))
    return worker.signature(values)
