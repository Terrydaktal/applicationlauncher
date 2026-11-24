"""Exercise the production Run in Terminal Fish argv in a private, bounded PTY."""

import argparse
import errno
import fcntl
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time


FIXTURE = r"""
function __reader_fixture_greeting --on-event fish_prompt
    functions --erase __reader_fixture_greeting
    printf '@@GREETING@@\n'
end
function fish_prompt
    set -l saved_status $status
    printf '@@PROMPT pid=%d status=%d@@ ' $fish_pid $saved_status
end
function __reader_fixture_preexec --on-event fish_preexec
    printf '@@PREEXEC@@\n'
end
function __reader_fixture_postexec --on-event fish_postexec
    set -l saved_status $status
    printf '@@POSTEXEC status=%d duration=%s@@\n' $saved_status $CMD_DURATION
end
"""


def run(arguments):
    requested = os.environ["APPLICATIONLAUNCHER_TERMINAL_COMMAND"]
    argv = arguments.argv
    if argv[0] == "--":
        argv = argv[1:]
    with tempfile.TemporaryDirectory(prefix="launcher-reader-") as directory:
        root = Path(directory)
        environment = dict(os.environ, HOME=directory, TERM="dumb", fish_history="reader_fixture",
                           XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                           XDG_CACHE_HOME=str(root / "cache"), XDG_STATE_HOME=str(root / "state"),
                           XDG_RUNTIME_DIR=directory, APPLICATIONLAUNCHER_READER_EXPECTED=requested)
        master, slave = os.openpty()
        child = None
        raw = bytearray()

        def text():
            value = raw.decode("utf-8", errors="replace").replace("\r", "")
            return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\].*?(?:\x07|\x1b\\)", "", value)

        def read_until(predicate):
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if predicate(text()):
                    return text()
                if not select.select([master], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                try:
                    chunk = os.read(master, 8192)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                raw.extend(chunk)
                assert len(raw) < 262144, "PTY output exceeded its bound"
            raise AssertionError(f"Reader did not reach expected state: {text()[-4096:]!r}")

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 240, 0, 0))
            child = subprocess.Popen(
                [argv[0], "--no-config", "--init-command", FIXTURE, *argv[1:]],
                env=environment, cwd=directory, stdin=slave, stdout=slave, stderr=slave,
                preexec_fn=controlling_terminal,
            )
            os.close(slave)
            slave = None
            status = arguments.expect_status
            completed = re.compile(
                rf"@@POSTEXEC status={status} duration=\d+@@.*@@PROMPT pid={child.pid} status={status}@@",
                re.S,
            )
            initial = read_until(lambda output: completed.search(output) is not None)
            assert initial.count("@@GREETING@@") == 1, initial
            assert initial.count("@@PREEXEC@@") == 1, initial
            assert initial.count("@@POSTEXEC ") == 1, initial
            assert initial.index("@@GREETING@@") < initial.index("@@PREEXEC@@"), initial
            execution_output = initial.split("@@PREEXEC@@\n", 1)[1].split("@@POSTEXEC ", 1)[0]
            assert arguments.expect_output in execution_output, initial
            if status == 127:
                assert initial.count(f"fish: Unknown command: {requested}") == 1, initial
                assert not re.search(r"\nfish:\s*\n", initial), initial
                assert "^~~~" not in initial, initial
            assert child.poll() is None, "Fish must remain open after the command"

            followup = (
                'builtin history search --exact -- "$APPLICATIONLAUNCHER_READER_EXPECTED"; '
                'printf "\\n@@HISTORY_STATUS %d@@\\n" $status; '
                'set --query APPLICATIONLAUNCHER_TERMINAL_COMMAND; '
                'printf "@@HANDOFF_QUERY_STATUS %d@@\\n" $status; '
                'printf "@@STATE %s@@\\n" "$applicationlauncher_reader_state"; exit\n'
            )
            os.write(master, followup.encode())
            final = read_until(lambda output: f"@@STATE {arguments.expect_state}@@\n" in output)
            assert "@@HISTORY_STATUS 0@@" in final, final
            assert "@@HANDOFF_QUERY_STATUS 1@@" in final, final
            assert child.wait(timeout=2) == 0
            print(f"reader status={status}; one greeting; hooks, history and shell state verified")
        finally:
            if child is not None and child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=1)
            os.close(master)
            if slave is not None:
                os.close(slave)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expect-status", type=int, required=True)
    parser.add_argument("--expect-output", default="")
    parser.add_argument("--expect-state", default="")
    parser.add_argument("argv", nargs=argparse.REMAINDER)
    run(parser.parse_args())
