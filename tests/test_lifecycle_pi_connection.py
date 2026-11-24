"""Restart planning recognizes only Pi's owned offline connection supervisor."""

import copy
import fcntl
import json
import os
import unittest
from unittest.mock import patch

import test_application_updates as updates

worker = updates.worker


class PiConnections(unittest.TestCase):
    session_id = updates.PiSessions.session_id
    setUp = updates.PiSessions.setUp
    publish = updates.PiSessions.publish
    fingerprint = updates.PiSessions.fingerprint

    def connection(self, *, connected=False):
        self.state_directory = self.home / "connection"
        self.state_directory.mkdir(mode=0o700)
        self.state_path = self.state_directory / "status.json"
        helper_file = self.script.with_name("pi-radiance-connection")
        helper_file.write_text("synthetic connection supervisor; never execute\n")
        helper_file.chmod(0o700)
        self.supervisor = dict(self.helper, start=43, executable="/usr/bin/python3",
                               image=worker.file_identity("/usr/bin/python3"),
                               arguments=["python3", str(helper_file), "--state", str(self.state_path),
                                          "--parent-pid", str(self.launcher["pid"]), "--launcher", str(self.script),
                                          "--remote", "lewis@ai", "--local-port", "8014"])
        self.processes[self.supervisor["pid"]] = self.supervisor
        self.receipt = {"schema": "coherence.pi.backend-connection.v1", "state": "offline", "local_port": 8014,
                        "supervisor_pid": self.supervisor["pid"], "supervisor_start_ticks": "43",
                        "updated_at_unix_ms": worker.time.time_ns() // 1_000_000, "stage": "Backend offline"}
        self.publish_connection()
        read = worker.read_limited
        replacement = patch.object(worker, "read_limited", side_effect=lambda path, limit=worker.LIMIT:
            f"QWEN_RADIANCE_CONNECTION_STATE={self.state_path}\0".encode()
            if path == f"/proc/{self.saved['pid']}/environ" else read(path, limit))
        replacement.start()
        self.addCleanup(replacement.stop)
        if connected:
            self.worker = dict(self.launcher, pid=203, start=44, parent=self.supervisor["pid"], group=203, tty=0,
                               arguments=["/usr/bin/bash", f"/proc/self/fd/{self.fd}", "--backend-connection",
                                          "--reuse-existing", "--no-install", "--remote", "lewis@ai", "--local-port", "8014"])
            self.tunnel = dict(self.helper, pid=204, start=45, parent=203, group=203, tty=0,
                               arguments=["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o",
                                          "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2", "-N", "-T", "-o",
                                          "ExitOnForwardFailure=yes", "-L", "127.0.0.1:8014:127.0.0.1:8080", "lewis@ai"])
            self.processes.update({203: self.worker, 204: self.tunnel})
            self.children.update({self.supervisor["pid"]: [203], 203: [204], 204: []})
            actual = f"/proc/{os.getpid()}/fd/{self.fd}"
            self.launcher_sources = {f"/proc/203/fd/{self.fd}": actual}
            original_open, original_readlink, original_identity = os.open, os.readlink, worker.file_identity
            for replacement in [
                    patch.object(worker.os, "open", side_effect=lambda path, *args, **kwargs:
                        original_open(self.launcher_sources.get(path, path), *args, **kwargs)),
                    patch.object(worker.os, "readlink", side_effect=lambda path, *args, **kwargs:
                        original_readlink(self.launcher_sources.get(path, path), *args, **kwargs)),
                    patch.object(worker, "file_identity", side_effect=lambda path:
                        original_identity(self.launcher_sources.get(path, path)))]:
                replacement.start()
                self.addCleanup(replacement.stop)
            self.receipt.update(state="ready", stage="Backend ready")
            self.publish_connection()

    def wrap_tunnel(self):
        self.wrapper = dict(self.worker, pid=205, start=46, parent=self.worker["pid"])
        self.processes[self.wrapper["pid"]] = self.wrapper
        self.tunnel["parent"] = self.wrapper["pid"]
        self.children[self.worker["pid"]] = [self.wrapper["pid"]]
        self.children[self.wrapper["pid"]] = [self.tunnel["pid"]]
        self.launcher_sources[f"/proc/{self.wrapper['pid']}/fd/{self.fd}"] = f"/proc/{os.getpid()}/fd/{self.fd}"

    def publish_connection(self):
        self.state_path.write_text(json.dumps(self.receipt))
        self.state_path.chmod(0o600)

    def test_offline_supervisor_allows_an_exact_session_plan_without_network(self):
        self.connection()
        with patch.object(worker.subprocess, "run", side_effect=AssertionError("Planning contacted a backend")):
            plan = worker.pi_plan(self.saved)
            worker.require_dedicated_tree(self.root, self.saved, plan)
        self.assertEqual(plan["helpers"], [self.supervisor])
        self.assertEqual(plan["connection_descendants"], [])
        self.assertEqual(plan["session_file"], str(self.session))
        self.assertEqual(plan["argv"][-2:], ["--session", str(self.session)])

    def test_owned_sealed_worker_and_exact_tunnel_are_nested_not_launcher_siblings(self):
        self.connection(connected=True)
        plan = worker.pi_plan(self.saved)
        worker.require_dedicated_tree(self.root, self.saved, plan)
        self.assertEqual(plan["helpers"], [self.supervisor])
        self.assertEqual(plan["connection_descendants"], [self.worker, self.tunnel])
        self.receipt.update(state="offline", stage="Backend offline")
        self.publish_connection()
        self.assertEqual(worker.signature(plan), worker.signature(worker.pi_plan(self.saved)))

    def test_receipt_refresh_and_worker_reconnection_do_not_change_the_session_preview(self):
        self.connection(connected=True)
        plan = worker.pi_plan(self.saved)
        self.receipt["updated_at_unix_ms"] += 1
        self.publish_connection()
        self.worker["start"] += 1
        self.tunnel["start"] += 1
        self.assertEqual(worker.signature(plan), worker.signature(worker.pi_plan(self.saved)))

    def test_backgrounded_ssh_function_wrapper_is_owned_without_changing_the_session_scope(self):
        self.connection(connected=True)
        direct = worker.pi_plan(self.saved)
        self.wrap_tunnel()
        with patch.object(worker.subprocess, "run", side_effect=AssertionError("Planning contacted a backend")):
            plan = worker.pi_plan(self.saved)
            worker.require_dedicated_tree(self.root, self.saved, plan)
        self.assertEqual(plan["helpers"], [self.supervisor])
        self.assertEqual(plan["connection_descendants"], [self.worker, self.wrapper, self.tunnel])
        self.assertEqual(plan["argv"][-2:], ["--session", str(self.session)])
        self.assertEqual(worker.signature(direct), worker.signature(plan))

    def test_ssh_wrapper_must_match_the_worker_image_arguments_parent_and_session(self):
        self.connection(connected=True)
        self.wrap_tunnel()
        for field, value in [("image", [0, 0, 0, 0]), ("executable", "/usr/bin/sleep"),
                             ("parent", 999), ("group", 999), ("tty", 1), ("cwd", str(self.home)),
                             ("arguments", [*self.worker["arguments"], "unrelated"])]:
            with self.subTest(field=field):
                before = self.wrapper[field]
                self.wrapper[field] = value
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                self.wrapper[field] = before

    def test_ssh_wrapper_requires_the_same_sealed_launcher_not_just_its_memfd_name(self):
        self.connection(connected=True)
        self.wrap_tunnel()
        for sealed in (False, True):
            with self.subTest(sealed=sealed):
                descriptor = os.memfd_create(worker.PI_LAUNCHER, os.MFD_ALLOW_SEALING)
                self.addCleanup(os.close, descriptor)
                os.write(descriptor, b"different launcher with the same name; never execute\n")
                if sealed:
                    fcntl.fcntl(descriptor, fcntl.F_ADD_SEALS,
                                fcntl.F_SEAL_WRITE | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SEAL)
                self.launcher_sources[f"/proc/{self.wrapper['pid']}/fd/{self.fd}"] = f"/proc/{os.getpid()}/fd/{descriptor}"
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)

    def test_wrapped_tunnel_does_not_allow_tools_sibling_jobs_or_more_shell_layers(self):
        self.connection(connected=True)
        self.wrap_tunnel()
        self.processes[999] = dict(self.saved, pid=999, executable="/usr/bin/sleep")
        for parent in (self.launcher["pid"], self.saved["pid"], self.supervisor["pid"],
                       self.worker["pid"], self.wrapper["pid"], self.tunnel["pid"]):
            with self.subTest(parent=parent):
                self.children[parent].append(999)
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                self.children[parent].remove(999)
        self.processes[999] = dict(self.wrapper, pid=999, parent=self.wrapper["pid"])
        self.children[self.wrapper["pid"]] = [999]
        self.children[999] = [self.tunnel["pid"]]
        self.tunnel["parent"] = 999
        with self.assertRaises(worker.Refused):
            worker.pi_plan(self.saved)

    def test_ssh_wrapper_and_leaf_are_revalidated_after_inspection(self):
        self.connection(connected=True)
        self.wrap_tunnel()
        for parent in (self.worker, self.wrapper, self.tunnel):
            with self.subTest(changed_children=parent["pid"]):
                calls = 0
                def changing_children(pid):
                    nonlocal calls
                    if pid == parent["pid"]:
                        calls += 1
                        if calls > 1:
                            return [*self.children[pid], 999]
                    return self.children.get(pid, [])
                with patch.object(worker, "children", side_effect=changing_children):
                    with self.assertRaisesRegex(worker.Refused, "changed during inspection"):
                        worker.pi_plan(self.saved)
        with patch.object(worker, "same_process", side_effect=lambda value: value["pid"] != self.wrapper["pid"]):
            with self.assertRaisesRegex(worker.Refused, "changed during inspection"):
                worker.pi_plan(self.saved)

    def test_stop_signals_only_pi_and_waits_for_the_verified_ssh_wrapper_to_exit(self):
        self.connection(connected=True)
        self.wrap_tunnel()
        plan = worker.pi_plan(self.saved)
        running = {value["pid"] for value in [self.saved, self.launcher, self.supervisor,
                                              self.worker, self.wrapper, self.tunnel]}
        def stop(fd, sig):
            self.assertEqual((fd, sig), (999, worker.signal.SIGTERM))
            running.intersection_update({self.wrapper["pid"]})
        with patch.object(worker.os, "pidfd_open", return_value=999) as opened, \
                patch.object(worker.os, "close"), \
                patch.object(worker, "same_process", side_effect=lambda value: value["pid"] in running), \
                patch.object(worker.signal, "pidfd_send_signal", side_effect=stop) as sent, \
                patch.object(worker.time, "sleep", side_effect=lambda delay: running.clear()) as slept:
            worker.stop_program(plan)
        opened.assert_called_once_with(self.saved["pid"])
        sent.assert_called_once_with(999, worker.signal.SIGTERM)
        slept.assert_called_once_with(0.05)

    def test_receipt_must_bind_the_live_supervisor_port_and_fresh_private_file(self):
        self.connection()
        for key, value in [("schema", "wrong"), ("supervisor_pid", 999), ("supervisor_start_ticks", "44"),
                           ("local_port", 8015), ("updated_at_unix_ms", 1), ("updated_at_unix_ms", None)]:
            with self.subTest(field=key, value=value):
                original = copy.deepcopy(self.receipt)
                self.receipt[key] = value
                self.publish_connection()
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                self.receipt = original
        self.publish_connection()
        for path, mode, reset in [(self.state_path, 0o644, 0o600), (self.state_directory, 0o755, 0o700)]:
            with self.subTest(path=path):
                path.chmod(mode)
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                path.chmod(reset)
        original = self.state_path.with_suffix(".original")
        self.state_path.rename(original)
        self.state_path.symlink_to(original)
        with self.assertRaises(OSError):
            worker.pi_plan(self.saved)

    def test_unrelated_children_and_changed_supervisor_worker_or_tunnel_are_refused(self):
        self.connection(connected=True)
        for parent in (self.launcher["pid"], self.saved["pid"], self.supervisor["pid"], 203, 204):
            with self.subTest(parent=parent):
                self.processes[999] = dict(self.saved, pid=999, executable="/usr/bin/sleep")
                self.children[parent].append(999)
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                self.children[parent].remove(999)
        for process, field, value in [(self.supervisor, "image", [0, 0, 0, 0]),
                                      (self.supervisor, "group", 999), (self.worker, "tty", 1),
                                      (self.worker, "group", 999), (self.worker, "arguments", ["bash", "/tmp/job"]),
                                      (self.tunnel, "arguments", ["ssh", "unrelated-host"])]:
            with self.subTest(pid=process["pid"], field=field):
                before = process[field]
                process[field] = value
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                process[field] = before
        self.supervisor["arguments"][-1] = "8015"
        with self.assertRaises(worker.Refused):
            worker.pi_plan(self.saved)

    def test_connection_receipt_is_revalidated_before_signalling_only_pi(self):
        self.connection(connected=True)
        plan = worker.pi_plan(self.saved)
        self.receipt["supervisor_start_ticks"] = "stale"
        self.publish_connection()
        with self.assertRaises(worker.Refused):
            worker.stop_program(plan)
        self.receipt["supervisor_start_ticks"] = "43"
        self.publish_connection()
        running = {value["pid"] for value in [self.saved, self.launcher, self.supervisor, self.worker, self.tunnel]}
        def stop(fd, sig):
            self.assertEqual((fd, sig), (999, worker.signal.SIGTERM))
            running.clear()
        with patch.object(worker.os, "pidfd_open", return_value=999) as opened, \
                patch.object(worker.os, "close"), \
                patch.object(worker, "same_process", side_effect=lambda value: value["pid"] in running), \
                patch.object(worker.signal, "pidfd_send_signal", side_effect=stop) as sent:
            worker.stop_program(plan)
        opened.assert_called_once_with(self.saved["pid"])
        sent.assert_called_once_with(999, worker.signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
