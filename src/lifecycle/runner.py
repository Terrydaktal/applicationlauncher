"""Entry point for a private systemd-managed job, not an in-daemon task."""
import json
import os
from pathlib import Path
import sys
import traceback

import lifecycle_worker as worker
import lifecycle_external as external
from lifecycle_window import WindowUpdate, atomic_record, private_request


def run(mode, path):
    directory = Path(path).parent
    request = private_request(path)
    if mode == "window-preview":
        print(json.dumps(external.inspect(request["windows"], worker.System())))
        return
    if mode == "gate":
        try:
            external.gate(request)
        except Exception as error:
            atomic_record(Path(path).with_suffix(".error.json"), dict(error=str(error), failure=worker.failure_details(error), traceback=traceback.format_exc()))
            raise
        return
    result = None
    try:
        fields = worker.read_limited("/proc/self/stat", 16384).decode().rsplit(")", 1)[1].split()
        atomic_record(directory / "worker.json", dict(pid=os.getpid(), start=int(fields[19]), uid=os.getuid()))
        if mode == "window":
            result = WindowUpdate(request, worker.System(), directory).run()
        elif mode == "window-external":
            system = worker.System()
            backend = external.Backend(request, system, directory)
            try:
                result = WindowUpdate(request, system, directory, backend=backend).run()
            finally:
                backend.close()
        elif mode == "program":
            result = worker.execute(request, worker.System(), directory)
        else:
            raise worker.Refused("Unknown lifecycle operation mode")
    except Exception as error:
        result = dict(ok=False, message=str(error) + "; private recovery records retained; do not retry blindly",
                      failure=worker.failure_details(error))
        print(traceback.format_exc(), file=sys.stderr)
    finally:
        if result is not None:
            atomic_record(directory / "result.json", result)


run(sys.argv[1], sys.argv[2])
