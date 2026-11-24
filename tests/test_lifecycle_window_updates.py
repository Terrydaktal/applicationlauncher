"""Latest-build selection and tmux reconnection, using disposable files/processes."""
import copy
import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import termios
import unittest
from unittest.mock import patch

from test_lifecycle_windows import worker, window


class InstalledImages(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="window-installed-images-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.job = window.WindowUpdate({}, object(), self.root)
        self.addCleanup(lambda: [os.close(descriptor) for descriptor in self.job.descriptors])

    def executable(self, name):
        path = self.root / name
        shutil.copyfile("/bin/true", path)
        path.chmod(0o700)
        return str(path)

    def test_latest_native_images_are_pinned_for_fish_codex_and_tmux(self):
        for name in ("fish", "codex", "tmux"):
            with self.subTest(name=name):
                source = self.executable(name)
                with patch.object(worker, "latest_binary", return_value=source) as latest:
                    target, pinned = self.job.latest_image(name)
                latest.assert_called_once_with(name)
                self.assertEqual(target, dict(name=name, source=source, image=worker.file_identity(source)))
                self.assertEqual(worker.file_identity(pinned), target["image"])
                self.assertEqual(subprocess.run([name], executable=pinned, timeout=3).returncode, 0)

    def test_replaced_target_is_refused_even_though_its_pinned_image_survives(self):
        source = self.executable("fish")
        with patch.object(worker, "latest_binary", return_value=source):
            target, pinned = self.job.latest_image("fish")
            Path(source).unlink()
            self.executable("fish")
            with self.assertRaisesRegex(worker.Refused, "application changed after preview"):
                self.job.require_target(target)
        self.assertEqual(worker.file_identity(pinned), target["image"])

    def test_republished_launch_path_is_not_silently_adopted(self):
        first, second = self.executable("fish"), self.executable("other-fish")
        with patch.object(worker, "latest_binary", return_value=first) as latest:
            target, _ = self.job.latest_image("fish")
            latest.return_value = second
            with self.assertRaisesRegex(worker.Refused, "application changed after preview"):
                self.job.require_target(target)

    def test_revalidation_checks_the_selected_build_before_touching_the_original(self):
        source = self.executable("fish")
        with patch.object(worker, "latest_binary", return_value=source):
            target, _ = self.job.latest_image("fish")
            self.job.tabs = [dict(application_target=target)]
            Path(source).unlink()
            self.executable("fish")
            with patch.object(self.job, "remote", return_value={}) as remote, \
                    patch.object(worker, "same_process", side_effect=AssertionError("Continued after a changed build")):
                with self.assertRaisesRegex(worker.Refused, "application changed after preview"):
                    self.job.revalidate()
            remote.assert_called_once_with("Preflight")

    def test_missing_target_never_falls_back_to_the_old_application(self):
        with patch.object(worker, "latest_binary", side_effect=worker.Refused("No installed fish executable")), \
                patch.object(self.job, "pin", side_effect=AssertionError("Retried the old executable")):
            with self.assertRaisesRegex(worker.Refused, "No installed fish"):
                self.job.latest_image("fish")

    def test_preview_binds_the_selected_application_image_and_path(self):
        source = self.executable("fish")
        target = dict(name="fish", source=source, image=worker.file_identity(source))
        tab = dict(program=dict(kind="fish", argv=["/proc/100/fd/4"], installed=target["image"]),
                   record={}, active=True, cwd=str(self.root), application_target=target)
        before = window.scope_fingerprint([tab])
        for key, value in (("source", "/another/fish"), ("image", [9, 8, 7, 6])):
            changed = copy.deepcopy(tab)
            changed["application_target"][key] = value
            self.assertNotEqual(before, window.scope_fingerprint([changed]))
        changed = copy.deepcopy(tab)
        changed["program"]["argv"][0] = "/proc/200/fd/8"
        self.assertEqual(before, window.scope_fingerprint([changed]))

    def test_tmux_verification_rejects_a_client_using_the_old_image(self):
        self.job.tabs = [dict(program=dict(kind="tmux", installed=[1, 2, 3, 4]))]
        with patch.object(self.job, "remote", return_value=dict(records=[dict(child_pid=123)])), \
                patch.object(worker, "process", return_value=dict(pid=123, foreground=123)), \
                patch.object(worker, "foreground", return_value=({}, dict(image=[5, 6, 7, 8]))), \
                patch.object(self.job, "verify_tmux", side_effect=AssertionError("Accepted the old client")):
            with self.assertRaisesRegex(worker.Refused, "not running the selected installed build"):
                self.job.verify_replacement()

    def test_unsupported_pi_is_refused_before_selecting_an_executable(self):
        active = dict(executable="/fixture/node", arguments=["pi"])
        with patch.object(worker, "foreground", return_value=({}, active)), \
                patch.object(self.job, "pin", side_effect=AssertionError("Pinned an unsupported runtime")), \
                patch.object(self.job, "latest_image", side_effect=AssertionError("Selected an unsupported runtime")):
            with self.assertRaisesRegex(worker.Refused, "non-tmux Pi"):
                self.job.capture_tabs(dict(records=[dict(child_pid=123, foreground_pgid=123, active=True)]))


class TmuxWindowUpdates(unittest.TestCase):
    def private_server(self):
        import test_application_updates as applications
        server = applications.PrivateTmux()
        server.setUp()
        self.addCleanup(server.tearDown)
        return server

    def test_window_launches_latest_client_and_preserves_server_panes_and_client_flags(self):
        for read_only in (False, True):
            with self.subTest(read_only=read_only):
                fixture = self.private_server()
                old = fixture.root / "old/tmux"
                old.parent.mkdir()
                shutil.copyfile(fixture.binary, old)
                old.chmod(0o700)
                job = window.WindowUpdate({}, object(), fixture.root)
                try:
                    with fixture.client_tab(binary=str(old), read_only=read_only) as original:
                        server = fixture.server()
                        panes = fixture.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
                        original_record = original.terminals()[0]
                        job.tabs = job.capture_tabs(dict(records=[original_record]))
                        tab = job.tabs[0]
                        installed = worker.file_identity(fixture.binary)
                        self.assertEqual(tab["program"]["installed"], installed)
                        self.assertNotEqual(installed, worker.process(original.child.pid)["image"])
                        master, slave = os.openpty()
                        replacement = None
                        try:
                            environment = dict(os.environ, TERM="xterm-256color")
                            for key in ("TMUX", "TMUX_PANE"):
                                environment.pop(key, None)
                            def controlling_terminal():
                                os.setsid()
                                fcntl.ioctl(0, termios.TIOCSCTTY, 0)
                            argv = list(tab["argv"])
                            attach_index = argv.index("if-shell")
                            argv[attach_index:attach_index] = ["wait-for", "allow-attachment", ";"]
                            replacement = subprocess.Popen(argv, executable=tab["executable"],
                                cwd=tab["cwd"], env=environment, stdin=slave, stdout=slave, stderr=slave,
                                preexec_fn=controlling_terminal)
                            self.assertEqual(worker.process(replacement.pid)["image"], installed)
                            run_tmux = worker.run_tmux
                            saw_pending = False
                            def query(*args, **options):
                                nonlocal saw_pending
                                result = run_tmux(*args, **options)
                                if args[1] == "list-clients" and not saw_pending:
                                    self.assertNotIn(str(replacement.pid), [row.split("\t")[0] for row in result.splitlines()])
                                    saw_pending = True
                                    fixture.tmux("wait-for", "-S", "allow-attachment")
                                return result
                            with patch.object(job, "remote", return_value=dict(records=[dict(child_pid=replacement.pid)])), \
                                    patch.object(worker, "run_tmux", side_effect=query):
                                job.verify_replacement()
                            self.assertTrue(saw_pending)
                            self.assertTrue(worker.same_process(server["process"]))
                            self.assertEqual(fixture.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}"), panes)
                            self.assertIsNone(original.child.poll(), "The old client exited before verification")
                            connection = worker.client_connection(server, replacement.pid)
                            self.assertEqual(connection["read_only"], read_only)

                            fixture.tmux("new-session", "-d", "-s", "wrong", "/bin/sleep", "60")
                            fixture.tmux("switch-client", "-c", connection["tty"], "-t", "=wrong")
                            with patch.object(job, "remote", return_value=dict(records=[dict(child_pid=replacement.pid)])), \
                                    patch.object(worker, "run_tmux", wraps=worker.run_tmux) as query:
                                with self.assertRaisesRegex(worker.Refused, "exact existing server/session"):
                                    job.verify_replacement()
                            query.assert_called_once()
                        finally:
                            if replacement is not None:
                                if replacement.poll() is None:
                                    replacement.terminate()
                                replacement.wait(timeout=5)
                            os.close(master)
                            os.close(slave)
                finally:
                    for descriptor in job.descriptors:
                        os.close(descriptor)

    def test_incompatible_installed_client_is_refused_while_original_stays_attached(self):
        fixture = self.private_server()
        job = window.WindowUpdate({}, object(), fixture.root)
        run_tmux = worker.run_tmux
        def incompatible(server, *arguments, **options):
            if options.get("installed"):
                raise worker.Refused("Protocol version mismatch")
            return run_tmux(server, *arguments, **options)
        try:
            with fixture.client_tab() as original:
                with patch.object(worker, "run_tmux", side_effect=incompatible):
                    with self.assertRaisesRegex(worker.Refused, "Protocol version mismatch"):
                        job.capture_tabs(dict(records=original.terminals()))
                self.assertIsNone(original.child.poll())
                self.assertIn(str(original.child.pid), fixture.tmux("list-clients", "-F", "#{client_pid}").splitlines())
        finally:
            for descriptor in job.descriptors:
                os.close(descriptor)


if __name__ == "__main__":
    unittest.main()
