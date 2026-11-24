#!/usr/bin/env python3
"""External, bounded evidence capture usable with the erased minimal artifact.

The default is passive /proc inspection. --stacks explicitly permits GDB to
briefly stop the target. This helper never escalates privileges, restarts an app,
executes a target function, or installs a watcher in the target process.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time

MAX_COMMAND_BYTES = 1024 * 1024
MAX_BUNDLE_BYTES = 32 * 1024 * 1024
MAX_SECONDS = 45


def bounded_command(arguments, timeout=10):
    started = time.monotonic()
    process = subprocess.Popen(arguments, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
    data = bytearray()
    truncated = timed_out = False
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                if time.monotonic() - started >= timeout:
                    timed_out = True
                    break
                for key, _ in selector.select(min(.1, max(0, timeout - (time.monotonic() - started)))):
                    block = os.read(key.fileobj.fileno(), 65536)
                    if not block:
                        selector.unregister(key.fileobj)
                    else:
                        remaining = MAX_COMMAND_BYTES - len(data)
                        data.extend(block[:remaining])
                        if len(block) > remaining:
                            truncated = True
                            break
                if truncated:
                    break
        if not truncated and not timed_out and process.poll() is None:
            try:
                process.wait(timeout=max(.01, timeout - (time.monotonic() - started)))
            except subprocess.TimeoutExpired:
                timed_out = True
    finally:
        if process.poll() is None:
            # GDB gets a chance to detach cleanly before its private command
            # process group is terminated. No signal is sent to a target PID.
            os.killpg(process.pid, signal.SIGINT)
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=2)
        process.stdout.close()
    return dict(returncode=process.returncode, timed_out=timed_out, truncated=truncated,
                seconds=time.monotonic() - started), bytes(data)


def process_identity(pid):
    stat = Path(f"/proc/{pid}/stat").read_text()
    fields = stat.rsplit(")", 1)[1].split()
    return dict(pid=pid, start_ticks=int(fields[19]), state=fields[0],
                cpu_ticks=int(fields[11]) + int(fields[12]), rss_pages=int(fields[21]),
                boot_id=Path("/proc/sys/kernel/random/boot_id").read_text().strip())


def capture(output, pid=None, stacks=False, executable=None, core=None):
    os.umask(0o077)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    report = dict(schema_version=1, consistency="best-effort", errors=[], commands={},
                  semantic_history="unavailable in release-minimal; not inferred from an assurance build")
    started, written = time.monotonic(), 0

    def write(name, data):
        nonlocal written
        if written + len(data) > MAX_BUNDLE_BYTES:
            raise ValueError("External evidence output budget exhausted")
        path = output / name
        path.write_bytes(data)
        path.chmod(0o600)
        written += len(data)

    def command(name, arguments):
        remaining = MAX_SECONDS - (time.monotonic() - started)
        if remaining <= 0:
            report["errors"].append(dict(stage=name, kind="timeout"))
            return
        try:
            status, data = bounded_command(arguments, timeout=min(15, remaining))
            report["commands"][name] = status
            write(name + ".txt", data)
            if status["returncode"] or status["timed_out"] or status["truncated"]:
                report["errors"].append(dict(stage=name, kind="command-incomplete", **status))
        except OSError as error:
            report["errors"].append(dict(stage=name, kind=type(error).__name__, errno=error.errno))

    if pid is not None:
        try:
            report["process"] = process_identity(pid)
        except OSError as error:
            report["errors"].append(dict(stage="initial-identity", kind=type(error).__name__, errno=error.errno))
            report["process"] = dict(pid=pid)
        for name in ("stat", "status", "wchan", "limits", "io", "maps", "cgroup", "oom_score"):
            try:
                with open(f"/proc/{pid}/{name}", "rb") as stream:
                    data = stream.read(MAX_COMMAND_BYTES + 1)
                write("proc-" + name + ".txt", data[:MAX_COMMAND_BYTES])
                if len(data) > MAX_COMMAND_BYTES:
                    report["errors"].append(dict(stage=name, kind="truncated"))
            except OSError as error:
                report["errors"].append(dict(stage=name, kind=type(error).__name__, errno=error.errno))
        command("elf-identity", ["readelf", "-n", f"/proc/{pid}/exe"])
        tasks = []
        try:
            for index, path in enumerate(Path(f"/proc/{pid}/task").iterdir()):
                if index >= 128:
                    report["errors"].append(dict(stage="threads", kind="truncated"))
                    break
                try:
                    tasks.append(dict(tid=int(path.name), name=(path / "comm").read_text()[:96].strip(),
                                      wait=(path / "wchan").read_text()[:256].strip()))
                except OSError as error:
                    tasks.append(dict(tid=int(path.name), unavailable=type(error).__name__, errno=error.errno))
            write("threads.json", (json.dumps(tasks, indent=2) + "\n").encode())
        except OSError as error:
            report["errors"].append(dict(stage="threads", kind=type(error).__name__, errno=error.errno))
        if stacks:
            command("stacks", ["gdb", "--nx", "--nh", "--batch", "--quiet",
                    "-iex", "set auto-load off", "-iex", "set debuginfod enabled off",
                    "-ex", "set pagination off", "-ex", "set print frame-arguments none",
                    "-ex", "thread apply all bt", "-ex", "detach", "--pid", str(pid)])
        try:
            current = process_identity(pid)
            report["identity_unchanged"] = all(report["process"].get(key) == current[key] for key in ("pid", "start_ticks", "boot_id"))
            report["process_after"] = current
        except OSError as error:
            report["identity_unchanged"] = False
            report["errors"].append(dict(stage="final-identity", kind=type(error).__name__, errno=error.errno))
    else:
        command("elf-identity", ["readelf", "-n", str(executable)])
        command("stacks", ["gdb", "--nx", "--nh", "--batch", "--quiet",
                "-iex", "set auto-load off", "-iex", "set debuginfod enabled off",
                "-ex", "set pagination off", "-ex", "set print frame-arguments none",
                "-ex", "thread apply all bt", str(executable), str(core)])
        report["post_exit"] = dict(executable=str(executable), core=str(core))
    report["seconds"] = time.monotonic() - started
    report["bytes"] = written
    write("capture.json", (json.dumps(report, indent=2) + "\n").encode())
    manifest = []
    for path in sorted(output.iterdir()):
        manifest.append(hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.name)
    write("manifest.sha256", ("\n".join(manifest) + "\n").encode())
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--stacks", action="store_true")
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--core", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    if (arguments.pid is None) == (arguments.core is None) or (arguments.core and not arguments.executable):
        parser.error("Provide --pid PID, or --executable ARCHIVED_ELF --core CORE")
    if arguments.pid is not None and arguments.pid <= 1:
        parser.error("PID must identify a non-init user process")
    report = capture(arguments.output, arguments.pid, arguments.stacks, arguments.executable, arguments.core)
    print(json.dumps(dict(output=str(arguments.output), errors=report["errors"], commands=report["commands"])))


if __name__ == "__main__":
    main()
