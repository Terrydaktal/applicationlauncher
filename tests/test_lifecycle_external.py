"""Compatibility replacement guards. Only disposable processes and files are used."""
import copy
from contextlib import ExitStack
import fcntl
import importlib.util
import os
import json
from pathlib import Path
import subprocess
import socket
import sys
import tempfile
import termios
import unittest
from unittest.mock import Mock, mock_open, patch

from test_lifecycle_windows import SOURCE, worker, window

spec = importlib.util.spec_from_file_location("lifecycle_external", SOURCE / "external.py")
external = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = external
spec.loader.exec_module(external)


def saved_window(identifier="window", title="Terminal"):
    rectangle = dict(x=-5, y=40, width=900, height=600)
    result = dict(id=identifier, pid=50, title=title, geometry=rectangle, normal=rectangle.copy(),
        output=dict(name="virtual", x=0, y=0, width=1280, height=800, scale=1000),
        maximized=0, fullscreen=False, minimized=False, active=True, desktops=["desktop"], activities=[], above=[])
    result.update(dict.fromkeys(("keepAbove", "keepBelow", "noBorder", "shaded", "skipTaskbar", "skipPager", "skipSwitcher"), False))
    return result


class Mapping(unittest.TestCase):
    def test_missing_updater_and_tab_apis_use_only_an_unambiguous_proc_session(self):
        actual = saved_window()
        desktop = Mock()
        desktop.call.side_effect = [actual, dict(windows=[actual])]
        owner = dict(pid=50, executable="/fixture/xfce4-terminal", arguments=["xfce4-terminal"], start=60)
        record = dict(child_pid=70, window_uuid="window", active=True)
        with patch.object(worker, "process", return_value=owner), \
                patch.object(external, "native_records", return_value=(None, None)), \
                patch.object(external, "proc_record", return_value=record) as proc, \
                patch.object(worker, "latest_binary", return_value="/fixture/new-terminal"), \
                patch.object(worker, "file_identity", return_value=[1, 2, 3, 4]), \
                patch.object(worker, "read_limited", return_value=b"DISPLAY=:5\0WAYLAND_DISPLAY=private-wayland\0SECRET=never-save\0"), \
                patch("builtins.open", mock_open()), \
                patch.object(external.hashlib, "file_digest", side_effect=[Mock(hexdigest=lambda: "new"), Mock(hexdigest=lambda: "old")]):
            result = external.capture(object(), desktop, dict(id="window", pid=50))
        proc.assert_called_once_with(50, "window")
        self.assertEqual(result["records"], [record])
        self.assertFalse(result["capabilities"]["hold"])
        self.assertEqual(result["display_environment"], dict(DISPLAY=":5", WAYLAND_DISPLAY="private-wayland"))

    def test_multiple_windows_without_tab_metadata_are_refused_without_guessing(self):
        actual = saved_window()
        desktop = Mock()
        desktop.call.side_effect = [actual, dict(windows=[actual, saved_window("other")])]
        with patch.object(worker, "process", return_value=dict(executable="/fixture/xfce4-terminal", arguments=[])), \
                patch.object(external, "native_records", return_value=(None, None)), \
                patch.object(external, "proc_record", side_effect=AssertionError("Ambiguous window mapping was guessed")):
            with self.assertRaisesRegex(worker.Refused, "shared process"):
                external.capture(object(), desktop, dict(id="window", pid=50))

    def test_identical_captions_with_native_tab_metadata_are_still_ambiguous(self):
        actual = saved_window()
        desktop = Mock()
        desktop.call.side_effect = [actual, dict(windows=[actual, saved_window("other")])]
        records = [dict(window_uuid=identifier, window_title="Terminal") for identifier in ("one", "two")]
        with patch.object(worker, "process", return_value=dict(executable="/fixture/xfce4-terminal", arguments=[])), \
                patch.object(external, "native_records", return_value=(":1.7", records)):
            with self.assertRaisesRegex(worker.Refused, "ambiguous"):
                external.capture(object(), desktop, dict(id="window", pid=50))

    def test_proc_mapping_uses_the_real_controlling_pty_and_refuses_multiple_tabs(self):
        children, descriptors = [], []
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        try:
            for count in range(2):
                master, slave = os.openpty()
                descriptors.extend([master, slave])
                child = subprocess.Popen(["/bin/sleep", "30"], stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
                children.append(child)
                if count == 0:
                    # Wait until the child's session and controlling terminal exist.
                    import time
                    deadline = time.monotonic() + 3
                    while not worker.process(child.pid)["tty"]:
                        self.assertLess(time.monotonic(), deadline)
                    row = external.proc_record(os.getpid(), "private-window")
                    self.assertEqual(row["child_pid"], child.pid)
                    self.assertEqual(row["pty"], os.ttyname(slave))
                else:
                    with self.assertRaisesRegex(worker.Refused, "exactly one terminal session"):
                        external.proc_record(os.getpid(), "private-window")
        finally:
            for child in children:
                child.terminate()
                child.wait(timeout=3)
            for descriptor in descriptors: os.close(descriptor)

    def test_tab_api_errors_are_not_all_treated_as_absent_capabilities(self):
        system = Mock()
        def bus(method, signature="()", arguments=()):
            return {"ListNames": [["org.xfce.Terminal5"]], "GetNameOwner": [":1.8"], "GetConnectionUnixProcessID": [50]}[method]
        system.dbus.side_effect = bus
        system.call.side_effect = RuntimeError("org.freedesktop.DBus.Error.UnknownMethod")
        self.assertEqual(external.native_records(system, 50), (None, None))
        system.call.side_effect = RuntimeError("Timed out reading tabs")
        with self.assertRaisesRegex(worker.Refused, "not safe to guess"):
            external.native_records(system, 50)


class Guards(unittest.TestCase):
    def test_connected_gate_waits_for_asynchronous_tab_pid_publication(self):
        backend = external.Backend.__new__(external.Backend)
        backend.connections = []
        with tempfile.TemporaryDirectory(prefix="lifecycle-gate-race-") as directory, \
                socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener, \
                socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            listener.bind(str(Path(directory) / "socket"))
            listener.listen(1)
            listener.settimeout(1)
            backend.sockets = [listener]
            client.connect(listener.getsockname())
            client.setblocking(False)
            try:
                self.assertFalse(backend.accept_gates([dict(child_pid=0)]))
                self.assertEqual(backend.connections, [])
                with self.assertRaises(BlockingIOError): client.recv(1)
                self.assertTrue(backend.accept_gates([dict(child_pid=os.getpid())]))
                backend.connections[0].sendall(b"S")
                client.settimeout(1)
                self.assertEqual(client.recv(1), b"S")
            finally:
                for connection in backend.connections: connection.close()

    def test_gate_with_different_process_credentials_is_a_fatal_staging_error(self):
        backend = external.Backend.__new__(external.Backend)
        backend.connections = []
        listener, connection = Mock(), Mock()
        listener.accept.return_value = (connection, None)
        connection.getsockopt.return_value = external.struct.pack("3i", os.getpid() + 1, os.getuid(), 0)
        backend.sockets = [listener]
        with self.assertRaises(external.GateIdentityError):
            backend.accept_gates([dict(child_pid=os.getpid())])
        connection.close.assert_called_once()
        self.assertEqual(backend.connections, [])

    def test_release_startup_gate_preserves_pty_stdin_command_name_and_colour(self):
        binary = SOURCE.parents[1] / "target/release/applicationlauncherd"
        self.assertTrue(binary.is_file(), "Build the release daemon before the startup-gate regression")
        with tempfile.TemporaryDirectory(prefix="lifecycle-gate-") as directory:
            root = Path(directory)
            master, slave = os.openpty()
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.settimeout(5)
            listener.bind(str(root / "socket"))
            listener.listen(1)
            plan = root / "gate.json"
            window.atomic_record(plan, dict(protocol=1, socket=listener.getsockname(), executable="/bin/sh",
                argv=["preserved-command-name", "-c", 'test -t 0 && test -z "${NO_COLOR+x}" && printf "%s" "$0"'],
                environment=dict(NO_COLOR=None)))
            child = subprocess.Popen([str(binary), "--lifecycle-gate", str(plan)], stdin=slave,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=dict(os.environ, NO_COLOR="1"))
            try:
                connection, _ = listener.accept()
                connection.sendall(b"S")
                connection.close()
                output, error = child.communicate(timeout=5)
                self.assertEqual(child.returncode, 0, error)
                self.assertEqual(output, b"preserved-command-name")
            finally:
                listener.close()
                os.close(master)
                os.close(slave)
                if child.poll() is None:
                    child.terminate()
                    child.communicate(timeout=3)

    def test_cancelled_release_gate_never_executes_the_application(self):
        binary = SOURCE.parents[1] / "target/release/applicationlauncherd"
        with tempfile.TemporaryDirectory(prefix="lifecycle-gate-cancel-") as directory, \
                socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            root = Path(directory)
            listener.settimeout(5)
            listener.bind(str(root / "socket"))
            listener.listen(1)
            plan = root / "gate.json"
            window.atomic_record(plan, dict(protocol=1, socket=listener.getsockname(), executable="/usr/bin/touch",
                argv=["touch", str(root / "must-not-exist")], environment={}))
            child = subprocess.Popen([str(binary), "--lifecycle-gate", str(plan)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                connection, _ = listener.accept()
                connection.close()
                child.communicate(timeout=5)
                self.assertNotEqual(child.returncode, 0)
                self.assertFalse((root / "must-not-exist").exists())
                self.assertIn("cancelled before release", json.loads(plan.with_suffix(".error.json").read_text())["error"])
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.communicate(timeout=3)

    def test_prepared_commands_validate_before_any_application_can_be_stopped(self):
        valid = dict(protocol=1, executable="/fixture/exe", argv=["original-name"], environment=dict(NO_COLOR=None), title="literal %D")
        external.validate_gate(valid)
        for change in (dict(executable="relative"), dict(argv=[]), dict(argv=["bad\0argument"]),
                       dict(environment=dict(SECRET="not-allowed")), dict(title="bad\033]52;clipboard\007")):
            with self.subTest(change=change), self.assertRaises(worker.Refused):
                external.validate_gate(dict(valid, **change))

    def test_proc_descriptor_paths_do_not_invalidate_scope_but_sessions_and_processes_do(self):
        tab = dict(program=dict(kind="codex", argv=["/proc/100/fd/5", "resume", "conversation-one"],
            process=dict(pid=60, start=70, image=[1, 2, 3, 4])), record=dict(child_pid=60, active=True),
            active=True, cwd="/fixture", environment={})
        reference = window.scope_fingerprint([tab])
        changed = copy.deepcopy(tab)
        changed["program"]["argv"][0] = "/proc/200/fd/8"
        self.assertEqual(reference, window.scope_fingerprint([changed]))
        changed["program"]["argv"][2] = "conversation-two"
        self.assertNotEqual(reference, window.scope_fingerprint([changed]))
        changed = copy.deepcopy(tab)
        changed["record"]["child_pid"] += 1
        self.assertNotEqual(reference, window.scope_fingerprint([changed]))

    def test_only_expected_post_shutdown_window_exit_is_allowed(self):
        backend = external.Backend.__new__(external.Backend)
        backend.request = dict(external=dict(window=dict(pid=50, id="exact-window"), binary="/fixture/new", installed=[1, 2, 3, 4]))
        backend.desktop = Mock()
        backend.desktop.call.return_value = dict(missing=True)
        with self.assertRaisesRegex(worker.Refused, "before interruption"):
            backend.preflight()
        with patch.object(worker, "latest_binary", return_value="/fixture/new"), patch.object(worker, "file_identity", return_value=[1, 2, 3, 4]):
            backend.preflight(stopped=True)
        self.assertTrue(backend.closed)
        self.assertEqual(backend.desktop.call.call_args.kwargs["id"], "exact-window")


class NativeMenu(unittest.TestCase):
    def setUp(self):
        self.request = dict(job_id="00000000-0000-0000-0000-000000000001", owner_pid=50,
            owner_bus=":1.7", executable="/fixture/new-terminal", daemon_binary="/fixture/applicationlauncherd",
            service="org.xfce.Terminal5.Update.i00000000_0000_0000_0000_000000000001")
        self.snapshot = dict(owner=dict(pid=50, start=70), binary=self.request["executable"],
            pinned=[1, 2, 3, 4], window=saved_window(), records=[dict(window_uuid="native-window",
                tab_uuid="exact-tab", active=True, child_pid=60, foreground_pgid=60,
                pty="/dev/pts/1", input_enabled=True)])
        self.saved = dict(owner=dict(pid=50, start=70), owner_bus=":1.7", binary=self.snapshot["binary"],
            installed=self.snapshot["pinned"].copy(), window=copy.deepcopy(self.snapshot["window"]),
            records=copy.deepcopy(self.snapshot["records"]), display_environment=dict(WAYLAND_DISPLAY="wayland-test"))
        self.system, self.desktop = Mock(), Mock()
        self.system.dbus.return_value = [1]
        self.native = Mock(return_value={})
        self.patches = self.enterContext(ExitStack())
        self.patches.enter_context(patch.object(external, "Desktop", return_value=self.desktop))
        self.capture = self.patches.enter_context(patch.object(external, "capture", return_value=self.saved))

    def create(self):
        backend = external.MenuBackend(self.request, self.system, "/fixture/job", self.snapshot, self.native)
        self.addCleanup(backend.close)
        return backend

    def test_menu_fallback_uses_a_separate_service_and_preserves_the_launch_target(self):
        backend = self.create()
        self.assertNotEqual(backend.service, self.request["service"])
        self.assertEqual(backend.request["job_id"], self.request["job_id"])
        self.assertEqual(backend.request["external"]["window"], dict(id="window", pid=50))
        self.assertEqual(backend.request["display_environment"], self.saved["display_environment"])
        with patch.object(worker, "latest_binary", side_effect=AssertionError("Native launch target was replaced with PATH")):
            backend.capture()
        self.capture.assert_called_with(self.system, self.desktop, dict(id="window", pid=50), binary=self.request["executable"])

    def test_confirmation_and_status_use_the_original_helper_but_mechanics_are_shared(self):
        backend = self.create()
        backend.operate("AwaitConfirmation", {})
        self.native.assert_called_once_with("AwaitConfirmation", {}, timeout=310000)
        backend.operate("Publish", dict(phase="ready"))
        self.native.assert_called_with("Publish", dict(phase="ready"), timeout=45000)
        with patch.object(external.Backend, "operate", return_value={}) as operate:
            for action in ("Capture", "Preflight", "Stage", "Release", "Records", "Commit", "Recover", "Discard"):
                backend.operate(action, {})
                operate.assert_called_with(action, {})

    def test_native_preflight_is_not_called_after_the_old_window_may_have_closed(self):
        backend = self.create()
        with patch.object(external.Backend, "preflight", return_value={}) as preflight:
            backend.preflight()
            self.native.assert_called_once_with("Preflight", {})
            self.native.reset_mock()
            backend.preflight(stopped=True)
            self.native.assert_not_called()
            preflight.assert_called_with(True)

    def test_fallback_refuses_retargeted_owners_tabs_builds_and_placement(self):
        valid = copy.deepcopy(self.saved)
        changes = {
            "owner pid": lambda saved: saved["owner"].update(pid=51),
            "owner start": lambda saved: saved["owner"].update(start=71),
            "bus owner": lambda saved: saved.update(owner_bus=":1.8"),
            "binary": lambda saved: saved.update(binary="/fixture/other-terminal"),
            "installed image": lambda saved: saved.update(installed=[9, 2, 3, 4]),
            "tab": lambda saved: saved["records"][0].update(tab_uuid="other-tab"),
            "PTY": lambda saved: saved["records"][0].update(pty="/dev/pts/2"),
            "input hold": lambda saved: saved["records"][0].update(input_enabled=False),
            "geometry": lambda saved: saved["window"]["geometry"].update(x=100),
            "monitor": lambda saved: saved["window"]["output"].update(scale=2000),
        }
        for reason, change in changes.items():
            with self.subTest(reason=reason):
                saved = copy.deepcopy(valid)
                change(saved)
                self.capture.return_value = saved
                self.desktop.close.reset_mock()
                with self.assertRaises(worker.Refused):
                    self.create()
                self.desktop.close.assert_called_once()
                self.native.assert_not_called()


if __name__ == "__main__": unittest.main()
