"""Return-to-Fish regressions using private files, PTYs and disposable processes."""
import copy
import fcntl
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import tempfile
import termios
import time
import unittest
from unittest.mock import patch

from test_lifecycle_windows import worker, window


def wait_for(check):
    deadline = time.monotonic() + 5
    while not check():
        if time.monotonic() >= deadline: raise AssertionError("Private shell did not become ready")
        time.sleep(0.02)


class ReturnShell(unittest.TestCase):
    def test_window_codex_updates_parent_fish_and_binds_both_builds_to_preview(self):
        app = dict(pid=60, parent=50, executable="/fixture/codex", arguments=["original-codex", "resume", "exact"],
            cwd="/fixture", image=[1, 2, 3, 4])
        shell = dict(pid=50, executable="/fixture/fish", arguments=["original-fish"], image=[5, 6, 7, 8])
        program = dict(kind="codex", process=app, argv=["/fixture/pinned-codex", "resume", "exact"], cwd="/fixture", installed=app["image"])
        with patch.object(worker, "foreground", return_value=(shell, app)), \
                patch.object(worker, "process", return_value=shell), \
                patch.object(worker, "children", return_value=[]), \
                patch.object(worker, "program_plan", return_value=program), \
                patch.object(worker, "require_dedicated_tree"), \
                patch.object(worker, "terminal_environment", return_value={}), \
                patch.object(worker, "latest_binary", side_effect=lambda name: f"/fixture/latest-{name}"), \
                patch.object(worker, "file_identity", return_value=[9, 10, 11, 12]), \
                patch.object(window.WindowUpdate, "pin", return_value="/fixture/pinned-codex"), \
                patch.object(window.WindowUpdate, "pin_image", side_effect=lambda source, image: source + "-pinned") as pin:
            job = window.WindowUpdate({}, object(), "/not-used")
            tab = job.capture_tabs(dict(records=[dict(child_pid=50, foreground_pgid=60, active=True)]))[0]
        self.assertEqual([call.args for call in pin.call_args_list], [
            ("/fixture/latest-codex", [9, 10, 11, 12]), ("/fixture/latest-fish", [9, 10, 11, 12])])
        self.assertEqual(tab["executable"], "/fixture/latest-fish-pinned")
        self.assertEqual(tab["application_target"]["source"], "/fixture/latest-codex")
        self.assertEqual(tab["return_shell"]["source"], "/fixture/latest-fish")
        self.assertEqual(tab["argv"][:3], ["original-fish", "--interactive", "--init-command"])
        self.assertIn("original-codex", tab["argv"][3])
        before = window.scope_fingerprint([tab])
        changed = copy.deepcopy(tab)
        changed["return_shell"]["executable"] = "/proc/999/fd/10"
        self.assertEqual(before, window.scope_fingerprint([changed]))
        changed["return_shell"]["image"][1] += 1
        self.assertNotEqual(before, window.scope_fingerprint([changed]))

    def test_return_shell_and_application_use_live_pins_not_archived_copies(self):
        from test_lifecycle_recovery_builds import forbid_image_io
        with tempfile.TemporaryDirectory(prefix="return-shell-pins-") as directory:
            job = window.WindowUpdate({}, object(), directory)
            try:
                with forbid_image_io():
                    app = job.pin_image("/bin/true", worker.file_identity("/bin/true"))
                    shell = job.pin_image("/bin/sh", worker.file_identity("/bin/sh"))
                    arguments = worker.fish_command(app, ["original-codex", "resume", "exact"], "original-fish")
                self.assertEqual(arguments[:3], ["original-fish", "--interactive", "--init-command"])
                self.assertIn(app, arguments[3])
                self.assertIn("original-codex", arguments[3])
                self.assertEqual(worker.file_identity(shell), worker.file_identity("/bin/sh"))
                self.assertEqual(list(Path(directory).iterdir()), [])
            finally:
                for descriptor in job.descriptors:
                    os.close(descriptor)

    def test_fish_stays_interactive_after_success_failure_and_ctrl_c(self):
        fish = Path.home() / ".local/bin/fish"
        if not fish.exists(): fish = Path(shutil.which("fish") or "/missing-fish")
        self.assertTrue(fish.exists(), "A real Fish binary is required for the PTY regression")
        for outcome in (0, 7, "interrupt"):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory(prefix="return-fish-") as directory:
                root = Path(directory)
                environment = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=str(root / "config"),
                    XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                    XDG_STATE_HOME=str(root / "state"), XDG_RUNTIME_DIR=str(root), TERM="dumb")
                marker, ready = root / "literal args", root / "ready"
                literal = "literal 'quotes' ; $HOME $(false)\nnot shell code"
                if outcome == "interrupt":
                    arguments = worker.fish_command("/bin/sleep", ["preserved-sleep", "30"], str(fish))
                else:
                    command = f'printf "%s" "$1" > {shlex.quote(str(marker))}; exit {outcome}'
                    arguments = worker.fish_command("/bin/sh", ["preserved-name", "-c", command, "script-name", literal], str(fish))
                syntax = subprocess.run([str(fish), "--no-config", "--no-execute", "--command", arguments[3]],
                    env=environment, capture_output=True, timeout=5)
                self.assertEqual(syntax.returncode, 0, syntax.stderr)
                master, slave = os.openpty()
                def controlling_pty():
                    os.setsid()
                    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
                child = subprocess.Popen(arguments, env=environment, cwd=directory, stdin=slave,
                    stdout=slave, stderr=slave, preexec_fn=controlling_pty)
                try:
                    if outcome == "interrupt":
                        def running_sleep():
                            return any(Path(worker.process(pid)["executable"]).name == "sleep" for pid in worker.children(child.pid))
                        wait_for(running_sleep)
                        os.write(master, b"\x03")
                    else:
                        wait_for(marker.exists)
                        self.assertEqual(marker.read_text(), literal)
                    os.write(master, (shlex.join(["printf", "%s", "Fish accepted input"]) + " > " + shlex.quote(str(ready)) + "\n").encode())
                    wait_for(ready.exists)
                    self.assertIsNone(child.poll(), "The shell exited with its application")
                    self.assertEqual(ready.read_text(), "Fish accepted input")
                finally:
                    for pid in worker.children(child.pid):
                        try: os.kill(pid, signal.SIGTERM)
                        except ProcessLookupError: pass
                    os.close(master)
                    os.close(slave)
                    if child.poll() is None: child.terminate()
                    try: child.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=3)


if __name__ == "__main__": unittest.main()
