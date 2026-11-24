"""Pure restart guards and real restarts confined to temporary tmux sockets.

Run: uv run --no-project --python /usr/bin/python3 python3 -m unittest discover -s tests -p test_application_updates.py -v
"""
import copy
import fcntl
import hashlib
from contextlib import ExitStack, contextmanager
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import termios
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("application_update_worker", Path(__file__).resolve().parents[1] / "src/lifecycle/worker.py")
worker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(worker)


def setUpModule():
    global TEST_STATE, TEST_ENVIRONMENT
    TEST_STATE = tempfile.TemporaryDirectory(prefix="launcher-lifecycle-tests-")
    TEST_ENVIRONMENT = patch.dict(os.environ, XDG_STATE_HOME=TEST_STATE.name)
    TEST_ENVIRONMENT.start()


def tearDownModule():
    TEST_ENVIRONMENT.stop()
    TEST_STATE.cleanup()


class CodexSessions(unittest.TestCase):
    root = "12345678-1234-5678-9abc-123456789abc"
    children = [f"abcdef0{index}-1234-5678-9abc-123456789abc" for index in range(1, 4)]
    other = "99999999-1234-5678-9abc-123456789abc"

    def metadata(self, identity, parent=None):
        payload = dict(id=identity, source="cli")
        if parent is not None:
            payload.update(parent_thread_id=parent,
                           source=dict(subagent=dict(thread_spawn=dict(parent_thread_id=parent, depth=1))))
        return dict(type="session_meta", payload=payload)

    @contextmanager
    def opened_rollouts(self, rows, arguments=None, unlink=False):
        with tempfile.TemporaryDirectory(prefix="launcher-codex-sessions-") as directory, ExitStack() as files:
            for index, row in enumerate(rows):
                path = Path(directory) / f"rollout-fixture-{index}.jsonl"
                data = row if isinstance(row, bytes) else json.dumps(row).encode()
                path.write_bytes(data + b"\nnot-another-metadata-record\n")
                files.enter_context(path.open("rb"))
                if unlink:
                    path.unlink()
            yield dict(pid=os.getpid(), arguments=arguments or ["codex", "resume", "--last"])

    def test_main_conversation_and_three_child_agents_resolve_to_the_main_session(self):
        children = [self.metadata(identity, self.root) for identity in self.children]
        for rows in [[self.metadata(self.root), *children], [*children, self.metadata(self.root)]]:
            with self.subTest(main_first=rows[0]["payload"]["id"] == self.root), self.opened_rollouts(rows) as saved:
                self.assertEqual(worker.session_id(saved), self.root)

    def test_verified_nested_children_and_duplicate_rollout_descriptors_are_not_main_sessions(self):
        rows = [self.metadata(self.root), self.metadata(self.root),
                self.metadata(self.children[0], self.root), self.metadata(self.children[1], self.children[0])]
        with self.opened_rollouts(rows) as saved:
            self.assertEqual(worker.session_id(saved), self.root)

    def test_either_parent_metadata_format_is_supported(self):
        for format in ["top-level", "source"]:
            child = self.metadata(self.children[0], self.root)
            if format == "top-level":
                del child["payload"]["source"]
            else:
                del child["payload"]["parent_thread_id"]
            with self.subTest(format=format), self.opened_rollouts([self.metadata(self.root), child]) as saved:
                self.assertEqual(worker.session_id(saved), self.root)

    def test_explicit_resume_id_must_agree_with_the_observed_main_session(self):
        rows = [self.metadata(self.root), self.metadata(self.children[0], self.root)]
        with self.opened_rollouts(rows, ["codex", "resume", self.root]) as saved:
            self.assertEqual(worker.session_id(saved), self.root)
        for identity in [self.children[0], self.other]:
            with self.subTest(identity=identity), self.opened_rollouts(rows, ["codex", "resume", identity]) as saved:
                with self.assertRaises(worker.Refused):
                    worker.session_id(saved)

    def test_explicit_resume_id_without_a_root_log_still_requires_verified_child_lineage(self):
        with self.opened_rollouts([self.metadata(self.children[0], self.root)], ["codex", "resume", self.root]) as saved:
            self.assertEqual(worker.session_id(saved), self.root)
        with self.opened_rollouts([self.metadata(self.children[0], self.other)], ["codex", "resume", self.root]) as saved:
            with self.assertRaises(worker.Refused):
                worker.session_id(saved)

    def test_deleted_but_still_open_rollouts_keep_their_verified_identity(self):
        rows = [self.metadata(self.root), self.metadata(self.children[0], self.root)]
        with self.opened_rollouts(rows, unlink=True) as saved:
            self.assertEqual(worker.session_id(saved), self.root)

    def test_multiple_main_conversations_still_refuse_the_restart(self):
        rows = [self.metadata(self.root), self.metadata(self.children[0], self.root), self.metadata(self.other)]
        with self.opened_rollouts(rows) as saved, self.assertRaisesRegex(worker.Refused, "ambiguous"):
            worker.session_id(saved)

    def test_children_without_a_verified_main_or_parent_chain_are_refused(self):
        cases = [[self.metadata(self.children[0], self.root)],
                 [self.metadata(self.root), self.metadata(self.children[0], self.other)],
                 [self.metadata(self.root), self.metadata(self.children[0], self.children[1])],
                 [self.metadata(self.root), self.metadata(self.children[0], self.children[1]),
                  self.metadata(self.children[1], self.children[0])]]
        for rows in cases:
            with self.subTest(rows=rows), self.opened_rollouts(rows) as saved, self.assertRaises(worker.Refused):
                worker.session_id(saved)

    def test_conflicting_invalid_and_incomplete_child_metadata_is_not_silently_ignored(self):
        conflicting = self.metadata(self.children[0], self.root)
        conflicting["payload"]["source"]["subagent"]["thread_spawn"]["parent_thread_id"] = self.other
        invalid = self.metadata(self.children[0], self.root)
        invalid["payload"]["parent_thread_id"] = "not-a-uuid"
        incomplete = self.metadata(self.children[0])
        incomplete["payload"]["source"] = dict(subagent="review")
        for child in [conflicting, invalid, incomplete]:
            with self.subTest(child=child), self.opened_rollouts([self.metadata(self.root), child]) as saved:
                with self.assertRaises(worker.Refused):
                    worker.session_id(saved)

    def test_duplicate_session_ids_with_conflicting_lineage_are_refused(self):
        rows = [self.metadata(self.root), self.metadata(self.children[0], self.root),
                self.metadata(self.children[0], self.other)]
        with self.opened_rollouts(rows) as saved, self.assertRaises(worker.Refused):
            worker.session_id(saved)

    def test_unreadable_or_malformed_rollout_headers_cannot_hide_a_competing_session(self):
        for row in [b"invalid json", b"null", b"[]", b"x" * (1024 * 1024 + 1),
                    dict(type="session_meta", payload=dict(id=123)),
                    dict(type="session_meta", payload=dict(id=self.other, parent_thread_id=[]))]:
            with self.subTest(header=str(row)[:80]), self.opened_rollouts([self.metadata(self.root), row]) as saved:
                with self.assertRaises(worker.Refused):
                    worker.session_id(saved)
        original_open = os.open
        def deny_competing_rollout(path, *args, **kwargs):
            if str(path).startswith("/proc/") and os.readlink(path).endswith("rollout-fixture-1.jsonl"):
                raise PermissionError("synthetic unreadable rollout")
            return original_open(path, *args, **kwargs)
        with self.opened_rollouts([self.metadata(self.root), self.metadata(self.other)]) as saved:
            with patch.object(worker.os, "open", side_effect=deny_competing_rollout), self.assertRaises(worker.Refused):
                worker.session_id(saved)

    def test_restart_recipe_resumes_the_main_uuid_without_last(self):
        rows = [self.metadata(self.root), *[self.metadata(identity, self.root) for identity in self.children]]
        with self.opened_rollouts(rows) as saved:
            saved.update(cwd=tempfile.gettempdir(), executable="/fixture/codex")
            with patch.object(worker, "latest_binary", return_value="/fixture/new-codex"), \
                    patch.object(worker, "file_identity", return_value=[1, 2, 3, 4]):
                plan = worker.program_plan(saved)
        self.assertEqual(plan["argv"], ["/fixture/new-codex", "resume", self.root])
        self.assertNotIn("--last", plan["argv"])

    def test_server_preview_accepts_codex_with_child_agents_without_stopping_any_sessions(self):
        rows = [self.metadata(self.root), *[self.metadata(identity, self.root) for identity in self.children]]
        server = dict(process=dict(pid=456, start=789), socket="/fixture/tmux.sock", binary="/fixture/tmux")
        replies = {"list-sessions": "$1\tdental\t1\t\t@1\n",
                   "list-windows": "$1\t@1\t0\tcodex\tlayout\t80\t24\t0\n",
                   "list-panes": "@1\t%2\t0\t1\n", "list-clients": "",
                   "display-message": "%2\t123\t0\n", "show-options": "off\n", "show-environment": ""}
        root = dict(pid=123, foreground=123)
        with self.opened_rollouts(rows) as saved:
            saved.update(cwd=tempfile.gettempdir(), executable="/fixture/codex")
            with patch.object(worker, "run_tmux", side_effect=lambda server, command, *args: replies[command]), \
                    patch.object(worker, "process", return_value=root), \
                    patch.object(worker, "foreground", return_value=(root, saved)), \
                    patch.object(worker, "require_dedicated_tree"), \
                    patch.object(worker, "latest_binary", return_value="/fixture/new-codex"), \
                    patch.object(worker, "file_identity", return_value=[1, 2, 3, 4]), \
                    patch.object(worker, "stop_codex", side_effect=AssertionError("preview interrupted Codex")):
                plan = worker.server_plan(server, [])
        self.assertEqual(plan["programs"]["%2"]["program"]["argv"], ["/fixture/new-codex", "resume", self.root])
        self.assertEqual(worker.server_restart_scope(plan)["sessions"], ["dental"])


class PiSessions(unittest.TestCase):
    session_id = "12345678-1234-5678-9abc-123456789abc"

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="launcher-pi-update-")
        self.addCleanup(self.directory.cleanup)
        self.home = Path(self.directory.name)
        self.cwd = self.home / "project"
        self.sessions = self.cwd / ".pi/sessions"
        self.sessions.mkdir(parents=True, mode=0o700)
        self.activity_root = self.home / ".local/state/qwen-r9700/pi-remote/radiance-public-clean-candidate"
        self.activity_directory = self.activity_root / "agent-8014/radiance-active"
        self.activity_directory.mkdir(parents=True, mode=0o700)
        self.script = self.home / worker.PI_LAUNCHER
        self.script.write_text("fixture; never execute\n")
        self.script.chmod(0o700)
        self.entry = self.home / ".local/bin/pi"
        self.entry.parent.mkdir(parents=True)
        self.entry.write_text("fixture Pi JavaScript; never execute\n")
        self.fd = os.memfd_create(worker.PI_LAUNCHER, os.MFD_ALLOW_SEALING)
        self.addCleanup(os.close, self.fd)
        os.write(self.fd, b"fixture sealed launcher; never execute\n")
        fcntl.fcntl(self.fd, fcntl.F_ADD_SEALS,
                    fcntl.F_SEAL_WRITE | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SEAL)
        self.launcher = dict(pid=os.getpid(), start=40, parent=100, group=200, tty=1, foreground=200,
                             executable="/usr/bin/bash", cwd=str(self.cwd), image=[1, 2, 3, 4],
                             arguments=["/usr/bin/bash", f"/proc/self/fd/{self.fd}", "--reuse-existing",
                                        "--no-install", "--thinking", "xhigh", "--session", "last"])
        self.saved = dict(pid=201, start=42, parent=self.launcher["pid"], group=200, tty=1,
                          foreground=200, executable="/usr/bin/node", image=worker.file_identity("/usr/bin/node"),
                          arguments=["pi"], cwd=str(self.cwd))
        self.root = dict(self.launcher, pid=100, parent=2, group=100, executable="/fixture/fish", arguments=["fish"])
        self.helper = dict(self.saved, pid=202, executable="/usr/bin/ssh",
                           arguments=["ssh", "-N", "-T", "-o", "ExitOnForwardFailure=yes", "-L",
                                      "127.0.0.1:8014:127.0.0.1:8080", "lewis@ai"])
        self.processes = {row["pid"]: row for row in (self.root, self.launcher, self.saved, self.helper)}
        self.children = {100: [self.launcher["pid"]], self.launcher["pid"]: [201, 202], 201: [], 202: []}
        self.session = self.sessions / "old-exact-session.jsonl"
        self.session.write_text(json.dumps(dict(type="session", id=self.session_id, cwd=str(self.cwd)))
                                + "\nTHIS IS NOT METADATA AND MUST NOT BE READ\n")
        self.chat_id = self.fingerprint(str(self.session))
        self.activity = dict(schema="urn:qwen-r9700:radiance-active-pi:v1", pid=201,
                             process_start_ticks="42", port=8014, chat_id=self.chat_id)
        self.activity_path = self.activity_directory / "201.json"
        self.publish()
        for target, kwargs in [
                ("process", dict(side_effect=lambda pid: self.processes[pid])),
                ("children", dict(side_effect=lambda pid: self.children.get(pid, []))),
                ("same_process", dict(return_value=True)),
                ("latest_binary", dict(return_value="/usr/bin/node"))]:
            replacement = patch.object(worker, target, **kwargs)
            replacement.start()
            self.addCleanup(replacement.stop)
        for replacement in [patch.object(worker.Path, "home", return_value=self.home),
                            patch.object(worker.shutil, "which", return_value=str(self.script)),
                            patch.object(worker.signal, "pidfd_send_signal", side_effect=AssertionError("preview sent a signal"))]:
            replacement.start()
            self.addCleanup(replacement.stop)

    def publish(self):
        self.activity_path.write_text(json.dumps(self.activity))
        self.activity_path.chmod(0o600)

    def fingerprint(self, path):
        return hashlib.sha256(json.dumps([path, self.session_id], ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()

    def test_pi_foreground_selects_the_application_not_its_shell_or_ssh_and_resumes_exactly(self):
        root, active = worker.foreground(100, 200)
        self.assertEqual(active, self.saved)
        plan = worker.program_plan(active)
        worker.require_dedicated_tree(root, active, plan)
        self.assertEqual(plan["kind"], "pi")
        self.assertEqual(plan["helpers"], [self.helper])
        self.assertEqual(plan["argv"], [str(self.script), "--reuse-existing", "--no-install", "--thinking", "xhigh", "--session", str(self.session)])
        self.assertNotIn("last", plan["argv"])
        self.assertIn(str(self.session), worker.pane_command(plan)[2])

    def test_pi_activity_pid_start_duplicates_and_session_hash_are_not_guessed(self):
        for key, value in [("pid", 999), ("process_start_ticks", "43"), ("chat_id", "0" * 64), ("port", 999)]:
            with self.subTest(key=key):
                before = self.activity.copy()
                self.activity[key] = value
                self.publish()
                with self.assertRaises(worker.Refused):
                    worker.pi_plan(self.saved)
                self.activity = before
        self.publish()
        other = self.activity_root / "agent-8015/radiance-active"
        other.mkdir(parents=True)
        (other / "201.json").write_text(json.dumps(dict(self.activity, port=8015)))
        with self.assertRaisesRegex(worker.Refused, "one process-bound"):
            worker.pi_plan(self.saved)

    def test_pi_history_migration_requires_the_original_identity_and_same_inode(self):
        metadata = self.session.stat()
        original = str(self.home / "previous-project/exact-session.jsonl")
        self.activity["chat_id"] = self.fingerprint(original)
        self.publish()
        record = dict(device=metadata.st_dev, inode=metadata.st_ino, identity_path=original)
        index = self.sessions / ".identity.json"
        index.write_text(json.dumps(dict(schema="urn:qwen-r9700:pi-project-history:v1", sessions={self.session.name: record})))
        self.assertEqual(worker.pi_plan(self.saved)["session_file"], str(self.session))
        record["inode"] += 1
        index.write_text(json.dumps(dict(schema="urn:qwen-r9700:pi-project-history:v1", sessions={self.session.name: record})))
        with self.assertRaisesRegex(worker.Refused, "no longer matches"):
            worker.pi_plan(self.saved)

    def test_pi_unrelated_sibling_jobs_and_active_tools_still_block_the_restart(self):
        for pid in (self.launcher["pid"], self.saved["pid"], self.helper["pid"], self.root["pid"]):
            with self.subTest(parent=pid):
                self.processes[999] = dict(self.saved, pid=999, executable="/usr/bin/sleep")
                self.children[pid].append(999)
                with self.assertRaises(worker.Refused):
                    plan = worker.pi_plan(self.saved)
                    worker.require_dedicated_tree(self.root, self.saved, plan)
                self.children[pid].remove(999)
        for position, value in [(6, "127.0.0.1:9999:127.0.0.1:8080"), (7, "someone@elsewhere")]:
            with self.subTest(argument=position):
                before = self.helper["arguments"][position]
                self.helper["arguments"][position] = value
                with self.assertRaisesRegex(worker.Refused, "unrelated background jobs"):
                    worker.pi_plan(self.saved)
                self.helper["arguments"][position] = before

    def test_pi_requires_a_sealed_known_launcher_and_refuses_process_title_spoofing(self):
        self.launcher["arguments"][1] = "/tmp/random-script"
        with self.assertRaisesRegex(worker.Refused, "verified Radiance launcher"):
            worker.pi_plan(self.saved)
        fd = os.memfd_create(worker.PI_LAUNCHER, os.MFD_ALLOW_SEALING)
        self.addCleanup(os.close, fd)
        self.launcher["arguments"][1] = f"/proc/self/fd/{fd}"
        with self.assertRaisesRegex(worker.Refused, "not sealed"):
            worker.pi_plan(self.saved)
        with patch.object(worker, "same_process", return_value=False):
            self.launcher["arguments"][1] = f"/proc/self/fd/{self.fd}"
            with self.assertRaisesRegex(worker.Refused, "changed during inspection"):
                worker.pi_plan(self.saved)

    def test_pi_metadata_symlinks_permissions_and_oversized_headers_are_refused(self):
        for path in (self.activity_path, self.session):
            with self.subTest(path=path):
                path.chmod(0o666)
                with self.assertRaisesRegex(worker.Refused, "Unsafe Pi"):
                    worker.pi_plan(self.saved)
                path.chmod(0o600)
                moved = path.with_suffix(".moved")
                path.rename(moved)
                path.symlink_to(moved)
                with self.assertRaises(OSError):
                    worker.pi_plan(self.saved)
                path.unlink()
                moved.rename(path)
        self.session.write_bytes(b"x" * 16385)
        with self.assertRaisesRegex(worker.Refused, "safety limit"):
            worker.pi_plan(self.saved)

    def test_pi_unsupported_launch_options_and_prompts_cannot_be_replayed(self):
        for arguments in [["--", "-p", "private prompt"], ["--thinking"], ["--thinking", "invalid"],
                          ["--session", "last", "another prompt"], ["--remote", "-oProxyCommand=bad"],
                          ["--no-install", "--no-install"], ["--local-port", "70000"]]:
            with self.subTest(arguments=arguments), self.assertRaises(worker.Refused):
                worker.pi_options(arguments)
        self.assertEqual(worker.pi_options(["--session=last"]), ["--reuse-existing", "--no-install"])

    def test_pi_session_changes_invalidate_the_restart_preview_but_new_messages_do_not(self):
        plan = worker.pi_plan(self.saved)
        self.session.write_text(self.session.read_text() + "another message\n")
        self.assertEqual(worker.signature(worker.pi_plan(self.saved)), worker.signature(plan))
        for key in ("session_id", "session_inode", "installed_entry", "installed_launcher", "launcher_image"):
            changed = copy.deepcopy(plan)
            changed[key] = "changed"
            self.assertNotEqual(worker.signature(changed), worker.signature(plan))

    def test_pi_stop_checks_jobs_and_signals_only_the_verified_application_then_waits_for_helpers(self):
        plan = worker.pi_plan(self.saved)
        with patch.object(worker.os, "pidfd_open", return_value=999), patch.object(worker.os, "close"):
            running = {value["pid"] for value in [self.saved, self.launcher, self.helper]}
            def stop(fd, sig):
                self.assertEqual((fd, sig), (999, worker.signal.SIGTERM))
                running.clear()
            with patch.object(worker, "same_process", side_effect=lambda value: value["pid"] in running), \
                    patch.object(worker.signal, "pidfd_send_signal", side_effect=stop) as sent:
                worker.stop_program(plan)
                self.assertEqual(sent.call_count, 1)
        self.children[self.saved["pid"]].append(999)
        with self.assertRaisesRegex(worker.Refused, "tool is running"):
            worker.stop_program(plan)

    def test_pi_verification_never_accepts_only_the_matching_node_binary(self):
        plan = worker.pi_plan(self.saved)
        wrong = dict(plan, session_id="wrong-session")
        with patch.object(worker, "foreground", return_value=(self.root, self.saved)), \
                patch.object(worker, "pi_plan", side_effect=[wrong, plan]) as inspect, \
                patch.object(worker.time, "sleep") as sleep:
            worker.verify_program(plan, self.root["pid"])
        self.assertEqual(inspect.call_count, 2)
        sleep.assert_called_once()

    def test_restored_pi_can_be_offline_or_have_started_tools_without_loosening_stop_guards(self):
        self.children[self.launcher["pid"]].remove(self.helper["pid"])
        self.children[self.saved["pid"]].append(999)
        self.processes[999] = dict(self.saved, pid=999, executable="/usr/bin/sleep")
        restored = worker.pi_plan(self.saved, require_idle=False)
        self.assertEqual(restored["session_file"], str(self.session))
        self.assertEqual(restored["helpers"], [])
        with self.assertRaisesRegex(worker.Refused, "active tool/background jobs"):
            worker.pi_plan(self.saved)

    def test_pi_changed_session_is_revalidated_before_sending_any_signal(self):
        plan = worker.pi_plan(self.saved)
        self.activity["chat_id"] = "0" * 64
        self.publish()
        with self.assertRaises(worker.Refused):
            worker.stop_program(plan)
        with patch.object(worker, "same_process", side_effect=lambda value: value["pid"] != self.saved["pid"]):
            with self.assertRaisesRegex(worker.Refused, "launcher/tunnel is still running"):
                worker.stop_program(plan)

    def test_pi_refuses_to_force_kill_a_slow_application_or_launcher(self):
        plan = worker.pi_plan(self.saved)
        with patch.object(worker.os, "pidfd_open", return_value=999), patch.object(worker.os, "close"), \
                patch.object(worker.signal, "pidfd_send_signal") as signal, \
                patch.object(worker.time, "monotonic", side_effect=[0, 10]):
            with self.assertRaisesRegex(worker.Refused, "no force-kill"):
                worker.stop_program(plan)
        signal.assert_called_once_with(999, worker.signal.SIGTERM)

    def test_server_preview_includes_pi_and_its_exact_session_without_executing_any_action(self):
        server = dict(process=dict(pid=500, start=789), socket="/fixture/tmux.sock", binary="/fixture/tmux")
        replies = {"list-sessions": "$1\tgirls\t1\t\t@1\n",
                   "list-windows": "$1\t@1\t0\tbash\tlayout\t80\t24\t0\n",
                   "list-panes": "@1\t%3\t0\t1\n", "list-clients": "",
                   "display-message": "%3\t100\t0\n", "show-options": "off\n", "show-environment": ""}
        with patch.object(worker, "run_tmux", side_effect=lambda server, command, *args: replies[command]) as tmux:
            plan = worker.server_plan(server, [])
        self.assertEqual(plan["programs"]["%3"]["program"]["session_id"], self.session_id)
        self.assertEqual(worker.server_restart_scope(plan)["panes"][0]["application"], "pi")
        self.assertEqual({call.args[1] for call in tmux.call_args_list}, set(replies))


class ProgramUpdates(unittest.TestCase):
    @contextmanager
    def preview(self, application, requested, tmux=False, active_start=700):
        root = dict(pid=50, parent=1, foreground=50, executable="/fixture/fish")
        active = dict(pid=60, start=active_start, parent=50, executable=f"/fixture/{application}")
        outer = dict(pid=30, parent=10, executable="/fixture/tmux") if tmux else active
        binding = dict(owner_pid=90, child_pid=10, foreground_pgid=10, active=True, restart_supported=True)
        payload = dict(window_id="window", windows=[dict(id="window", pid=90, title="fixture", **{"class":"xfce4-terminal"})], process=requested)
        program = dict(kind=application, process=active, argv=[f"/fixture/{application}"], cwd="/fixture/work")
        class System:
            def terminals(self):
                return [binding]
        def query(server, command, *arguments):
            if command == "show-options": return "off\n"
            if command == "display-message":
                return "%7\n" if arguments[-1] == "#{pane_id}" else "%7\t50\t0\n"
            raise AssertionError("A single-program preview queried unrelated server state")
        with patch.object(worker, "foreground", side_effect=lambda pid, pgid: (root, outer) if pid == 10 else (root, active)), \
                patch.object(worker, "process", return_value=root), \
                patch.object(worker, "program_plan", return_value=program) as recipe, \
                patch.object(worker, "require_dedicated_tree"), \
                patch.object(worker, "tmux_client", return_value=(dict(socket="/fixture/tmux.sock"), "$1", "work")), \
                patch.object(worker, "run_tmux", side_effect=query), \
                patch.object(worker, "server_plan", side_effect=AssertionError("An unrelated whole-server preview ran")) as whole_server, \
                patch.object(worker, "stop_program", side_effect=AssertionError("Preview stopped a process")), \
                patch.object(worker, "restart_pane", side_effect=AssertionError("Preview restarted a pane")):
            yield worker.inspect(payload, System()), recipe, System()
            whole_server.assert_not_called()

    def test_exact_program_previews_are_read_only_scoped_and_publish_the_clicked_identity(self):
        for application in ("fish", "codex"):
            requested = dict(pid=60, start_ticks=700, application=application)
            for tmux in (False, True):
                with self.subTest(application=application, tmux=tmux), self.preview(application, requested, tmux) as (report, recipe, _):
                    self.assertEqual([action["id"] for action in report["actions"]], ["program"])
                    action = report["actions"][0]
                    self.assertTrue(action["eligible"], action)
                    self.assertEqual(action["program"]["identity"], requested)
                    self.assertEqual(action["program"].get("tmux_pane"), "%7" if tmux else None)
                    self.assertEqual(set(report["plans"]), {"program"})
                    self.assertEqual(report["plans"]["program"]["input"]["process"], requested)
                    recipe.assert_called_once()

    def test_fish_with_a_foreground_codex_is_blocked_without_retargeting_codex(self):
        for tmux in (False, True):
            with self.preview("codex", dict(pid=50, start_ticks=100, application="fish"), tmux) as (report, recipe, _):
                self.assertFalse(report["actions"][0]["eligible"])
                self.assertIn("idle Fish prompt", report["actions"][0]["message"])
                self.assertEqual(report["plans"], {})
                recipe.assert_not_called()

    def test_replaced_or_changed_process_ids_cannot_retarget_a_program_update(self):
        for tmux in (False, True):
            for requested in (dict(pid=61, start_ticks=700, application="codex"),
                              dict(pid=60, start_ticks=701, application="codex"),
                              dict(pid=60, start_ticks=700, application="fish")):
                with self.preview("codex", requested, tmux) as (report, recipe, _):
                    self.assertFalse(report["actions"][0]["eligible"])
                    self.assertFalse(report["plans"])
                    recipe.assert_not_called()

    def test_changed_exact_process_is_revalidated_before_execution_without_signals(self):
        old = dict(pid=60, start_ticks=700, application="codex")
        with self.preview("codex", old) as (report, _, _):
            plan = report["plans"]["program"]
        with self.preview("codex", old, active_start=701) as (_, _, system):
            with self.assertRaisesRegex(worker.Refused, "changed; refresh the preview"):
                worker.execute(plan, system, "/not-used")

    def test_process_preview_rejects_invalid_identities_and_unsupported_programs(self):
        for requested in ({}, dict(pid=True, start_ticks=1, application="fish"),
                          dict(pid=60, start_ticks=0, application="codex"),
                          dict(pid=60, start_ticks=700, application="pi")):
            with self.assertRaisesRegex(worker.Refused, "exact running Fish, Codex or tmux client"):
                worker.inspect(dict(process=requested), object())


class TmuxClientGuards(unittest.TestCase):
    def test_control_activepane_suspended_unknown_and_inconsistent_readonly_modes_are_refused(self):
        server = dict(process=dict(pid=20))
        base = ["10", "20", "$1", "work", "12345", "/dev/pts/99", "0", "0", "attached,focused,UTF-8"]
        for field, value in [(6, "1"), (8, "attached,active-pane"), (8, "attached,suspended"),
                             (8, "attached,unknown"), (7, "1"), (8, "attached,read-only"),
                             (2, "$1;bad"), (4, "0"), (1, "21")]:
            row = base.copy()
            row[field] = value
            with self.subTest(field=field, value=value), patch.object(worker, "run_tmux", return_value="\t".join(row)):
                with self.assertRaises(worker.Refused):
                    worker.client_connection(server, 10)
        for flags, readonly in [("attached,UTF-8", "0"), ("attached,read-only,ignore-size", "1")]:
            row = base.copy()
            row[7], row[8] = readonly, flags
            with patch.object(worker, "run_tmux", return_value="\t".join(row)):
                self.assertEqual(worker.client_connection(server, 10)["read_only"], readonly == "1")


class Guards(unittest.TestCase):
    def test_server_preview_names_the_session_whose_terminal_cannot_be_replaced(self):
        replies = {"list-sessions": "$1\tdiet\t1\t\t@1\n",
                   "list-windows": "$1\t@1\t0\tfish\tlayout\t80\t24\t0\n",
                   "list-panes": "@1\t%0\t0\t1\n", "list-clients": "123\t$1\tdiet\n"}
        with patch.object(worker, "run_tmux", side_effect=lambda server, command, *args: replies[command]), \
                patch.object(worker, "pane_plan", return_value={}), patch.object(worker, "process", return_value={}), \
                patch.object(worker, "tab_for_client", side_effect=worker.Refused("Update this xfce4-terminal window first; it lacks the safe tab-replacement API")):
            with self.assertRaisesRegex(worker.Refused, "Session 'diet' .*123.*window first.*no sessions were stopped"):
                worker.server_plan({}, [])

    def test_exact_codex_session_is_required_and_conflicting_ids_are_refused(self):
        first = "12345678-1234-5678-9abc-123456789abc"
        second = "abcdef01-1234-5678-9abc-123456789abc"
        saved = dict(pid=os.getpid(), arguments=["codex", "resume", first])
        self.assertEqual(worker.session_id(saved), first)
        saved["arguments"] = ["codex", "resume", first, "resume", second]
        with self.assertRaisesRegex(worker.Refused, "ambiguous"):
            worker.session_id(saved)
        saved["arguments"] = ["codex", "resume", "--last"]
        with self.assertRaisesRegex(worker.Refused, "resume --last is not used"):
            worker.session_id(saved)

    def test_unrelated_sibling_jobs_block_tab_and_pane_replacement(self):
        root = dict(pid=10, parent=1)
        active = dict(pid=20, parent=10)
        with patch.object(worker, "process", return_value=root), patch.object(worker, "children", return_value=[20, 30]):
            with self.assertRaisesRegex(worker.Refused, "other jobs"):
                worker.require_dedicated_tree(root, active)
        with patch.object(worker, "process", return_value=root), patch.object(worker, "children", return_value=[20]):
            worker.require_dedicated_tree(root, active)

    def test_codex_preserves_permissions_and_configuration_without_replaying_prompts(self):
        session = "12345678-1234-5678-9abc-123456789abc"
        self.assertEqual(worker.codex_options(["codex", "resume", session, "--dangerously-bypass-approvals-and-sandbox", "--model", "example", "-c", 'x="a; b"']),
                         ["--dangerously-bypass-approvals-and-sandbox", "--model", "example", "-c", 'x="a; b"'])
        for arguments in (["codex", "explain a password"], ["codex", "--", "--yolo"], ["codex", "--model"]):
            with self.subTest(arguments=arguments), self.assertRaises(worker.Refused):
                worker.codex_options(arguments)

    def test_preview_signature_changes_for_process_binary_session_or_scope(self):
        plan = {"kind":"tmux_pane", "pane_id":"%1", "program":{"argv":["/bin/codex", "resume", "one"], "process":{"pid":12, "start":42, "image":[1,2,3,4]}}}
        original = worker.signature(plan)
        for mutate in [lambda item:item.update(pane_id="%2"), lambda item:item["program"]["process"].update(start=43),
                       lambda item:item["program"].update(argv=["/bin/codex", "resume", "two"]),
                       lambda item:item["program"]["process"].update(image=[1,2,3,5])]:
            changed = copy.deepcopy(plan)
            mutate(changed)
            self.assertNotEqual(worker.signature(changed), original)
        plan["window_title"] = "spinner changed"
        plan["working_directory"] = "Derived OSC working-directory metadata"
        self.assertEqual(worker.signature(plan), original)

    def test_changed_preview_never_prepares_signals_or_restarts(self):
        class Untouched:
            def prepare(self, *args):
                raise AssertionError("A stale preview reached the terminal")
        with patch.object(worker, "inspect", return_value={"plans":{}}), patch.object(worker, "stop_codex", side_effect=AssertionError("signal")):
            with self.assertRaises(worker.Refused):
                worker.execute({"input":{}, "kind":"terminal_program"}, Untouched(), "/not-used")

    def test_tmux_automatic_window_names_do_not_invalidate_a_preview_but_topology_does(self):
        plan = dict(kind="tmux_server", windows=[["$1", "@1", "0", "tmux", "layout", "80", "24", "0"]])
        renamed = copy.deepcopy(plan)
        renamed["windows"][0][3] = "fish"
        self.assertEqual(worker.signature(plan), worker.signature(renamed))
        for index in (0, 1, 2, 4, 5, 6, 7):
            changed = copy.deepcopy(plan)
            changed["windows"][0][index] = "changed"
            self.assertNotEqual(worker.signature(plan), worker.signature(changed))

    def test_unsupported_terminal_and_shell_jobs_are_refused(self):
        with self.assertRaisesRegex(worker.Refused, "Update this xfce4-terminal"):
            worker.require_handoff({"restart_supported":False})
        with patch.object(worker, "latest_binary", return_value="/bin/fish"), patch.object(worker, "children", return_value=[99]):
            with self.assertRaisesRegex(worker.Refused, "background jobs"):
                worker.program_plan({"pid":1, "cwd":"/tmp", "executable":"/bin/fish", "arguments":["fish"]})

    def test_server_scope_lists_every_pane_and_detached_session_with_exact_identity(self):
        plan = dict(server=dict(process=dict(pid=123, start=456), socket="/fixture/tmux.sock", binary="/fixture/tmux"),
                    sessions=[["$1", "work", "1", "", "@1"], ["$2", "detached", "2", "", "@2"]],
                    windows=[["$1", "@1", "3", "ignored", "layout", "80", "24", "0"],
                             ["$2", "@2", "1", "ignored", "layout", "80", "24", "0"]],
                    panes=[["@1", "%1", "0", "1"], ["@2", "%2", "2", "1"]],
                    programs={"%1": dict(program=dict(kind="codex", cwd="/one")), "%2": dict(program=dict(kind="fish", cwd="/two"))},
                    bindings=[dict(session="$1")])
        self.assertEqual(worker.server_restart_scope(plan), dict(
            identity=dict(pid=123, start_ticks=456, socket_path="/fixture/tmux.sock"), executable="/fixture/tmux",
            sessions=["work", "detached"], windows=2, terminal_tabs=1, remote_clients=[],
            panes=[dict(session="work", window_index="3", pane_index="0", application="codex", cwd="/one"),
                   dict(session="detached", window_index="1", pane_index="2", application="fish", cwd="/two")]))


class RemoteMoshClients(unittest.TestCase):
    def setUp(self):
        def saved(pid, parent, name, tty):
            return dict(pid=pid, parent=parent, executable=f"/fixture/{name}", arguments=[name],
                        tty=tty, group=pid, start=pid * 100, image=[1, pid, 100, 200])
        self.client = saved(30, 20, "tmux", 123)
        self.shell = saved(20, 10, "fish", 123)
        self.mosh = saved(10, 1, "mosh-server", 0)
        self.processes = {row["pid"]: row for row in [self.client, self.shell, self.mosh]}
        self.remote = dict(client=self.client, session="$1", session_name="netmgr", transport="mosh",
                           ancestry=[{key: row[key] for key in ("pid", "start", "parent", "group", "tty", "image", "executable")}
                                     for row in [self.client, self.shell, self.mosh]])

    def test_remote_client_requires_real_mosh_executable_ancestry_and_not_a_local_terminal(self):
        with patch.object(worker, "process", side_effect=self.processes.__getitem__):
            route = worker.remote_mosh_client(self.client)
            self.assertEqual(route, {key: self.remote[key] for key in ("transport", "ancestry")})
            for records in [[dict(child_pid=20)], [dict(owner_pid=30)]]:
                self.assertIsNone(worker.remote_mosh_client(self.client, records))
            for invalid in ["name-only", "different-tty", "local-terminal", "control-client", "broken-parent"]:
                changed = copy.deepcopy(self.processes)
                if invalid == "name-only":
                    changed[10].update(executable="/fixture/python", arguments=["mosh-server"])
                elif invalid == "different-tty":
                    changed[20]["tty"] += 1
                elif invalid == "local-terminal":
                    changed[20]["executable"] = "/fixture/xfce4-terminal"
                elif invalid == "control-client":
                    changed[30]["tty"] = 0
                else:
                    changed[20]["parent"] = 99
                with self.subTest(invalid=invalid), patch.object(worker, "process", side_effect=changed.__getitem__):
                    self.assertIsNone(worker.remote_mosh_client(changed[30]))
            with patch.object(worker, "process", side_effect=worker.Refused("not a same-user process")):
                self.assertIsNone(worker.remote_mosh_client(self.client))

    def test_server_preview_lists_mosh_disconnects_without_requiring_a_native_tab_or_stopping_work(self):
        replies = {"list-sessions": "$1\tnetmgr\t1\t\t@1\n",
                   "list-windows": "$1\t@1\t0\tfish\tlayout\t80\t24\t0\n",
                   "list-panes": "@1\t%0\t0\t1\n", "list-clients": "30\t$1\tnetmgr\n", "show-environment": ""}
        program = dict(program=dict(kind="fish", cwd="/fixture"))
        server = dict(process=dict(pid=100, start=123), socket="/fixture/server.sock", binary="/fixture/tmux")
        with patch.object(worker, "process", side_effect=self.processes.__getitem__), \
                patch.object(worker, "run_tmux", side_effect=lambda server, command, *args: replies[command]) as tmux, \
                patch.object(worker, "pane_plan", return_value=program), \
                patch.object(worker, "stop_program", side_effect=AssertionError("preview interrupted a program")):
            plan = worker.server_plan(server, [])
        self.assertEqual(plan["bindings"], [])
        self.assertEqual(plan["remote_clients"], [self.remote])
        scope = worker.server_restart_scope(plan)
        self.assertEqual(scope["remote_clients"], [dict(pid=30, session="netmgr", transport="mosh")])
        self.assertEqual(scope["terminal_tabs"], 0)
        self.assertEqual({call.args[1] for call in tmux.call_args_list}, set(replies))
        for key in ["start", "image", "parent"]:
            changed = copy.deepcopy(plan)
            changed["remote_clients"][0]["ancestry"][-1][key] = "changed"
            self.assertNotEqual(worker.signature(plan), worker.signature(changed))

    def test_remote_disconnect_revalidation_includes_all_clients_and_rejects_changed_ancestry(self):
        local = dict(client=dict(pid=40), session="$2", session_name="local", binding=dict(tab_uuid="local"))
        plan = dict(server={}, bindings=[local], remote_clients=[self.remote])
        with patch.object(worker, "same_process", return_value=True), \
                patch.object(worker, "process", side_effect=self.processes.__getitem__), \
                patch.object(worker, "run_tmux", return_value="30\t$1\tnetmgr\n40\t$2\tlocal\n") as tmux:
            worker.require_server_clients(plan)
            self.assertEqual(tmux.call_args.args[1], "list-clients")
            for current in ["40\t$2\tlocal\n", "30\t$1\tnetmgr\n40\t$2\tlocal\n50\t$3\tnew\n",
                            "30\t$1\tother\n40\t$2\tlocal\n"]:
                with self.subTest(current=current), patch.object(worker, "run_tmux", return_value=current):
                    with self.assertRaisesRegex(worker.Refused, "client bindings changed"):
                        worker.require_server_clients(plan)
            for key in ["start", "parent", "image"]:
                changed = copy.deepcopy(self.processes)
                changed[10][key] = 999 if key != "image" else [9, 9, 9, 9]
                with self.subTest(key=key), patch.object(worker, "process", side_effect=changed.__getitem__):
                    with self.assertRaisesRegex(worker.Refused, "ancestry changed"):
                        worker.require_server_clients(plan)
            with patch.object(worker, "same_process", return_value=False):
                with self.assertRaisesRegex(worker.Refused, "client bindings changed"):
                    worker.require_server_clients(plan)


class PrivateTmux(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="launcher-update-test-")
        self.root = Path(self.directory.name)
        self.socket = str(self.root / "server.socket")
        self.binary = worker.latest_binary("tmux")
        self.environment = patch.dict(os.environ, HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"))
        self.environment.start()
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "one", "-n", "first", "-c", str(self.root), "/usr/bin/bash", "--noprofile", "--norc")

    def tmux(self, *arguments):
        result = subprocess.run([self.binary, "-S", self.socket, *arguments], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result.stdout.decode().strip()

    def server(self):
        pid = int(self.tmux("display-message", "-p", "#{pid}"))
        metadata = os.stat(self.socket)
        return dict(socket=self.socket, binary=self.binary, process=worker.process(pid),
                    installed=worker.file_identity(self.binary), socket_identity=[metadata.st_dev, metadata.st_ino])

    @contextmanager
    def client_tab(self, binary=None, read_only=False):
        test = self
        master, slave = os.openpty()
        environment = dict(os.environ, TERM="xterm-256color")
        for key in ("TMUX", "TMUX_PANE"):
            environment.pop(key, None)
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        class Tab:
            phase, held, prepared, commits, cancelled = "", False, 0, 0, 0
            def spawn(self, argv, cwd):
                self.child = subprocess.Popen(argv, cwd=cwd, env=environment,
                    stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
            def terminals(self):
                saved = worker.process(self.child.pid)
                return [dict(owner="fixture", owner_pid=90, child_pid=self.child.pid,
                    foreground_pgid=saved["foreground"], active=True, restart_supported=True,
                    input_enabled=self.phase != "prepared", window_uuid="window", tab_uuid="tab")]
            def prepare(self, binding, argv, cwd):
                test.assertEqual(binding["child_pid"], self.child.pid)
                self.argv, self.cwd, self.phase, self.held = argv, cwd, "prepared", True
                self.prepared += 1
                return "private-lease"
            def tab(self, binding, token, method):
                test.assertEqual((binding["tab_uuid"], token), ("tab", "private-lease"))
                if method == "CommitTabRestart":
                    test.assertTrue(self.held, "The tab must be held before the old client exits")
                    if self.child.poll() is None:
                        self.child.terminate()
                    self.child.wait(timeout=5)
                    self.spawn(self.argv, self.cwd)
                    self.phase, self.held = "complete", False
                    self.commits += 1
                elif method == "CancelTabRestart":
                    self.phase = "cancelled"
                    self.cancelled += 1
                return dict(phase=self.phase, child_pid=self.child.pid if self.child.poll() is None else 0)
        tab = Tab()
        tab.spawn([binary or self.binary, "-N", "-S", self.socket, "attach-session", "-E", "-t", "one",
                   *(["-r"] if read_only else [])], self.root)
        try:
            deadline = time.monotonic() + 5
            while str(tab.child.pid) not in self.tmux("list-clients", "-F", "#{client_pid}").splitlines():
                self.assertIsNone(tab.child.poll(), "The private tmux client failed to attach")
                self.assertLess(time.monotonic(), deadline, "The private tmux client did not attach")
                time.sleep(0.02)
            yield tab
        finally:
            if tab.child.poll() is None:
                tab.child.terminate()
            tab.child.wait(timeout=5)
            os.close(master)
            os.close(slave)

    def client_preview(self, tab):
        saved = worker.process(tab.child.pid)
        payload = dict(window_id="window", windows=[dict(id="window", pid=90, title="fixture", **{"class":"xfce4-terminal"})],
                       process=dict(pid=saved["pid"], start_ticks=saved["start"], application="tmux"))
        return worker.inspect(payload, tab)

    def test_client_update_keeps_original_server_panes_and_other_client_with_readonly_preserved(self):
        (self.root / "old").mkdir()
        old_binary = self.root / "old/tmux"
        shutil.copyfile(self.binary, old_binary)
        old_binary.chmod(0o700)
        self.tmux("split-window", "-d", "-t", "one", "/usr/bin/sleep", "120")
        with self.client_tab() as other:
            for read_only in (False, True):
                with self.subTest(read_only=read_only), self.client_tab(str(old_binary), read_only) as tab, \
                        patch.object(worker, "pane_plan", side_effect=AssertionError("Client update inspected a pane")), \
                        patch.object(worker, "server_plan", side_effect=AssertionError("Client update inspected the whole server")), \
                        patch.object(worker, "stop_program", side_effect=AssertionError("Client update stopped a pane application")):
                    server = self.server()
                    panes = self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
                    original = worker.process(tab.child.pid)
                    report = self.client_preview(tab)
                    self.assertEqual([action["id"] for action in report["actions"]], ["program"])
                    self.assertTrue(report["actions"][0]["eligible"], report["actions"])
                    scope = report["actions"][0]["program"]["tmux_client"]
                    self.assertEqual(scope["server"]["pid"], server["process"]["pid"])
                    self.assertEqual(scope["read_only"], read_only)
                    self.assertEqual(tab.prepared, 0, "Preview must not prepare or detach the terminal")
                    result = worker.execute(report["plans"]["program"], tab, self.root)
                    self.assertTrue(result["ok"], result)
                    self.assertFalse(worker.same_process(original))
                    self.assertTrue(worker.same_process(server["process"]))
                    self.assertEqual(self.server()["socket_identity"], server["socket_identity"])
                    self.assertEqual(self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}"), panes)
                    self.assertIsNone(other.child.poll(), "Other clients must not be detached")
                    self.assertEqual(worker.client_connection(server, tab.child.pid)["read_only"], read_only)
                    self.assertEqual(worker.process(tab.child.pid)["image"], worker.file_identity(self.binary))
                    self.assertEqual((tab.prepared, tab.commits, tab.cancelled), (1, 1, 0))

    def test_client_update_refuses_incompatible_installed_binary_before_detaching(self):
        with self.client_tab() as tab, patch.object(worker, "latest_binary", return_value="/usr/bin/false"):
            original, server = worker.process(tab.child.pid), self.server()
            report = self.client_preview(tab)
            self.assertFalse(report["actions"][0]["eligible"])
            self.assertFalse(report["plans"])
            self.assertEqual(tab.prepared, 0)
            self.assertTrue(worker.same_process(original))
            self.assertTrue(worker.same_process(server["process"]))

    def test_client_update_refuses_unattached_destruction_without_changing_options(self):
        for option, arguments in [("exit-unattached", ["-s"]), ("destroy-unattached", ["-t", "one"]),
                                  ("destroy-unattached", ["-g"])]:
            with self.subTest(option=option, arguments=arguments), self.client_tab() as tab:
                self.tmux("set-option", *arguments, option, "on")
                try:
                    report = self.client_preview(tab)
                    self.assertFalse(report["actions"][0]["eligible"], report)
                    self.assertIn("must be off", report["actions"][0]["message"])
                    self.assertEqual(tab.prepared, 0)
                    self.assertIsNone(tab.child.poll())
                    self.assertEqual(self.tmux("show-options", *arguments, "-v", option), "on")
                finally:
                    self.tmux("set-option", *arguments, "-u", option)

    def test_client_update_requires_native_handoff_and_exact_pid_start_before_preparing(self):
        with self.client_tab() as tab:
            original = tab.terminals
            def unsupported():
                return [dict(original()[0], restart_supported=False)]
            with patch.object(tab, "terminals", side_effect=unsupported):
                report = self.client_preview(tab)
            self.assertFalse(report["actions"][0]["eligible"])
            self.assertIn("tab-replacement API", report["actions"][0]["message"])
            saved = worker.process(tab.child.pid)
            for identity in [dict(pid=saved["pid"] + 1, start_ticks=saved["start"], application="tmux"),
                             dict(pid=saved["pid"], start_ticks=saved["start"] + 1, application="tmux")]:
                payload = dict(window_id="window", windows=[dict(id="window", pid=90, title="fixture", **{"class":"xfce4-terminal"})], process=identity)
                report = worker.inspect(payload, tab)
                self.assertFalse(report["actions"][0]["eligible"])
                self.assertFalse(report["plans"])
            self.assertEqual(tab.prepared, 0)
            self.assertTrue(worker.same_process(saved))

    def test_client_update_rechecks_the_installed_image_after_preparing_without_signalling(self):
        with self.client_tab() as tab:
            plan = self.client_preview(tab)["plans"]["program"]
            original = worker.file_identity
            def changed_image(path):
                value = original(path)
                return [*value[:-1], value[-1] + 1] if path == self.binary and tab.prepared else value
            with patch.object(worker, "file_identity", side_effect=changed_image):
                result = worker.execute(plan, tab, self.root)
            self.assertFalse(result["ok"])
            self.assertIn("installed client binary changed", result["message"])
            self.assertEqual((tab.commits, tab.cancelled), (0, 1))
            self.assertIsNone(tab.child.poll())

    def test_client_update_revalidates_session_identity_without_stopping_any_client(self):
        self.tmux("new-session", "-d", "-s", "different", "/usr/bin/bash", "--noprofile", "--norc")
        with self.client_tab() as tab:
            report = self.client_preview(tab)
            tty = worker.client_connection(self.server(), tab.child.pid)["tty"]
            self.tmux("switch-client", "-c", tty, "-t", "different")
            with patch.object(worker.signal, "pidfd_send_signal", side_effect=AssertionError("A stale client plan sent a signal")):
                with self.assertRaisesRegex(worker.Refused, "changed; refresh the preview"):
                    worker.execute(report["plans"]["program"], tab, self.root)
            self.assertEqual(tab.prepared, 0)
            self.assertIsNone(tab.child.poll())

    def test_client_update_rechecks_dangerous_options_after_preparing_the_tab(self):
        with self.client_tab() as tab:
            plan = self.client_preview(tab)["plans"]["program"]
            original = tab.prepare
            def changed(*arguments):
                token = original(*arguments)
                self.tmux("set-option", "-s", "exit-unattached", "on")
                return token
            try:
                with patch.object(tab, "prepare", side_effect=changed):
                    result = worker.execute(plan, tab, self.root)
                self.assertFalse(result["ok"])
                self.assertIn("must be off", result["message"])
                self.assertEqual((tab.commits, tab.cancelled), (0, 1))
                self.assertIsNone(tab.child.poll())
            finally:
                self.tmux("set-option", "-s", "exit-unattached", "off")

    def test_failed_client_handover_retains_empty_tab_and_the_original_session(self):
        with self.client_tab() as tab:
            plan = self.client_preview(tab)["plans"]["program"]
            server = self.server()
            pane = self.tmux("display-message", "-p", "#{pane_pid}")
            original = tab.tab
            def fail_commit(binding, token, method):
                if method == "CommitTabRestart":
                    tab.child.terminate()
                    tab.child.wait(timeout=5)
                    tab.phase = "failed"
                    raise worker.Refused("Injected terminal handover failure")
                return original(binding, token, method)
            with patch.object(tab, "tab", side_effect=fail_commit):
                result = worker.execute(plan, tab, self.root)
            self.assertFalse(result["ok"])
            self.assertIn("retained reconnection command", result["message"])
            self.assertTrue(tab.held, "The empty terminal must remain available for recovery")
            self.assertEqual(tab.cancelled, 0)
            self.assertTrue(worker.same_process(server["process"]))
            self.assertEqual(self.tmux("display-message", "-p", "#{pane_pid}"), pane)

    def test_client_reconnect_command_never_creates_or_adopts_another_server(self):
        with self.client_tab() as tab:
            plan = self.client_preview(tab)["plans"]["program"]
            self.stop_server(plan["server"])
            tab.child.wait(timeout=5)
            argv = plan["program"]["argv"]
            tab.spawn(argv, self.root)
            self.assertNotEqual(tab.child.wait(timeout=5), 0, "No server may be auto-started")
            self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "one", "/usr/bin/bash", "--noprofile", "--norc")
            replacement = self.server()
            tab.spawn(argv, self.root)
            tab.child.wait(timeout=5)
            self.assertEqual(self.tmux("list-clients", "-F", "#{client_pid}"), "")
            self.assertTrue(worker.same_process(replacement["process"]))

    def wait_shells(self):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                return worker.server_plan(self.server(), [])
            except (worker.Refused, OSError):
                time.sleep(0.05)
        return worker.server_plan(self.server(), [])

    def stop_server(self, server):
        self.tmux("kill-server")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if not worker.same_process(server["process"]) and not worker.server_listening(server):
                return
            time.sleep(0.02)
        self.fail("The isolated test server did not close its listener")

    def tearDown(self):
        # This absolute, per-test socket is the only server cleanup can address.
        subprocess.run([self.binary, "-N", "-S", self.socket, "kill-server"], stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
        self.environment.stop()
        self.directory.cleanup()

    def test_actual_server_recreation_preserves_sessions_windows_panes_layouts_and_directories(self):
        self.tmux("new-session", "-d", "-s", "two", "-n", "second", "-c", str(self.root), "/usr/bin/bash", "--noprofile", "--norc")
        self.tmux("new-window", "-d", "-t", "one:3", "-n", "quoted ' window", "-c", str(self.root), "/usr/bin/bash", "--noprofile", "--norc")
        self.tmux("split-window", "-h", "-t", "one:3", "-c", str(self.root), "/usr/bin/bash", "--noprofile", "--norc")
        plan = self.wait_shells()
        original_pid = plan["server"]["process"]["pid"]
        before = self.tmux("list-windows", "-a", "-F", "#{session_name}:#{window_index}:#{window_name}:#{window_panes}")
        geometry_format = "#{session_name}:#{window_index}:#{pane_index}:#{pane_left}:#{pane_top}:#{pane_width}:#{pane_height}"
        geometry = self.tmux("list-panes", "-a", "-F", geometry_format)
        self.stop_server(plan["server"])
        gates, _ = worker.recreate_server(plan, self.root)
        for gate in gates:
            gate.touch(mode=0o600, exist_ok=False)
        self.assertNotEqual(int(self.tmux("display-message", "-p", "#{pid}")), original_pid)
        self.assertEqual(self.tmux("list-windows", "-a", "-F", "#{session_name}:#{window_index}:#{window_name}:#{window_panes}"), before)
        self.assertEqual(self.tmux("list-panes", "-a", "-F", geometry_format), geometry)
        self.assertEqual(len(gates), 4)
        self.assertEqual(set(self.tmux("list-panes", "-a", "-F", "#{pane_current_path}").splitlines()), {str(self.root)})

    def test_server_recreation_does_not_inherit_the_updaters_no_color_override(self):
        self.tmux("set-environment", "-g", "-u", "NO_COLOR")
        self.tmux("set-environment", "-g", "COLORTERM", "truecolor")
        plan = self.wait_shells()
        self.stop_server(plan["server"])
        with patch.dict(os.environ, NO_COLOR="1", COLORTERM="", FORCE_COLOR="0"):
            gates, _ = worker.recreate_server(plan, self.root)
        environment = [line for line in self.tmux("show-environment", "-g").splitlines()
                       if line.partition("=")[0] in {"NO_COLOR", "FORCE_COLOR", "COLORTERM"}]
        self.assertNotIn("NO_COLOR=1", environment)
        self.assertNotIn("FORCE_COLOR=0", environment)
        self.assertIn("COLORTERM=truecolor", environment)
        for gate in gates:
            gate.touch(mode=0o600, exist_ok=False)
        pid = int(self.tmux("display-message", "-p", "#{pane_pid}"))
        inherited = [line for line in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                     if line.partition(b"=")[0] in {b"NO_COLOR", b"COLORTERM"}]
        self.assertNotIn(b"NO_COLOR=1", inherited)
        self.assertIn(b"COLORTERM=truecolor", inherited)

    def test_server_recreation_preserves_intentional_original_colour_preferences(self):
        self.tmux("set-environment", "-g", "NO_COLOR", "1")
        self.tmux("set-environment", "-g", "CLICOLOR", "0")
        self.tmux("set-environment", "-g", "PRIVATE_TEST_TOKEN", "not-for-restart-plan")
        plan = self.wait_shells()
        self.assertNotIn("PRIVATE_TEST_TOKEN", plan["server"]["colour_environment"])
        self.stop_server(plan["server"])
        with patch.dict(os.environ, NO_COLOR="", CLICOLOR="1", FORCE_COLOR="1"):
            worker.recreate_server(plan, self.root)
        environment = [line for line in self.tmux("show-environment", "-g").splitlines()
                       if line.partition("=")[0] in {"NO_COLOR", "CLICOLOR", "FORCE_COLOR"}]
        self.assertIn("NO_COLOR=1", environment)
        self.assertIn("CLICOLOR=0", environment)
        self.assertNotIn("FORCE_COLOR=1", environment)

    def test_one_pane_restart_does_not_restart_server_or_other_pane(self):
        self.tmux("split-window", "-h", "-t", "one", "/usr/bin/bash", "--noprofile", "--norc")
        plan = self.wait_shells()
        pane_ids = list(plan["programs"])
        server_pid = plan["server"]["process"]["pid"]
        other_pid = self.tmux("display-message", "-p", "-t", pane_ids[1], "#{pane_pid}")
        selected = dict(kind="tmux_pane", server=plan["server"], **plan["programs"][pane_ids[0]])
        previous = selected["root"]["pid"]
        worker.restart_pane(selected)
        self.assertEqual(int(self.tmux("display-message", "-p", "#{pid}")), server_pid)
        self.assertEqual(self.tmux("display-message", "-p", "-t", pane_ids[1], "#{pane_pid}"), other_pid)
        self.assertNotEqual(int(self.tmux("display-message", "-p", "-t", pane_ids[0], "#{pane_pid}")), previous)

    def test_server_startup_exit_after_shutdown_retries_without_losing_the_saved_sessions(self):
        self.tmux("new-session", "-d", "-s", "detached", "/usr/bin/bash", "--noprofile", "--norc")
        plan = dict(self.wait_shells(), input={})
        original = worker.run_tmux
        failed = []
        def crash_first_replacement(server, *arguments, **options):
            if (server["socket"] == self.socket and arguments[0] == "new-session" and options.get("create")
                    and not failed and not worker.same_process(plan["server"]["process"])):
                failed.append(True)
                original(server, *arguments, **options)
                subprocess.run([self.binary, "-N", "-S", self.socket, "kill-server"],
                               stdin=subprocess.DEVNULL, capture_output=True, check=True, timeout=5)
                raise worker.Refused("tmux command failed: server exited unexpectedly")
            return original(server, *arguments, **options)
        with patch.object(worker, "inspect", return_value={"plans": {"tmux_server": plan}}), \
                patch.object(worker, "run_tmux", side_effect=crash_first_replacement):
            result = worker.execute(plan, object(), self.root)
        self.assertEqual(len(failed), 1, "The fault must occur after the original server stops")
        self.assertTrue(result["ok"], result)
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}:#{session_attached}"), "detached:0\none:0")
        self.assertEqual(len(self.tmux("list-panes", "-a", "-F", "#{pane_pid}").splitlines()), 2)
        events = [json.loads(line) for line in (self.root / "restart-events.jsonl").read_text().splitlines()]
        self.assertEqual([event["attempt"] for event in events if event["phase"] == "startup_failed"], [1])
        self.assertEqual([event["attempt"] for event in events if event["phase"] == "starting_server"], [1, 2])

    def test_shutdown_acknowledgement_loss_still_restores_sessions_after_the_old_server_exits(self):
        plan = dict(self.wait_shells(), input={})
        original = worker.run_tmux
        original_same_process = worker.same_process
        lost = []
        pending_exit_checks = [2]
        def delayed_exit(saved):
            if lost and saved["pid"] == plan["server"]["process"]["pid"] and pending_exit_checks[0]:
                pending_exit_checks[0] -= 1
                return True
            return original_same_process(saved)
        def lost_shutdown_reply(server, *arguments, **options):
            result = original(server, *arguments, **options)
            if (server["socket"] == self.socket and arguments[0] == "kill-server"
                    and server["process"]["pid"] == plan["server"]["process"]["pid"] and not lost):
                lost.append(True)
                raise worker.Refused("tmux command failed: server exited unexpectedly")
            return result
        with patch.object(worker, "inspect", return_value={"plans": {"tmux_server": plan}}), \
                patch.object(worker, "run_tmux", side_effect=lost_shutdown_reply), \
                patch.object(worker, "same_process", side_effect=delayed_exit):
            result = worker.execute(plan, object(), self.root)
        self.assertEqual(len(lost), 1)
        self.assertEqual(pending_exit_checks[0], 0, "Shutdown must tolerate a live identity briefly after acknowledgement loss")
        self.assertTrue(result["ok"], result)
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}:#{session_attached}"), "one:0")
        self.assertNotEqual(int(self.tmux("display-message", "-p", "#{pid}")), plan["server"]["process"]["pid"])

    def test_private_startup_check_never_opens_program_gates_or_changes_the_original_server(self):
        plan = self.wait_shells()
        original_pid = plan["server"]["process"]["pid"]
        before = self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
        marker = self.root / "must-not-start"
        for value in plan["programs"].values():
            value["program"]["argv"] = ["/usr/bin/touch", str(marker)]
        worker.preflight_server(plan, self.root)
        self.assertFalse(marker.exists(), "A startup check executed a captured application")
        self.assertTrue(worker.same_process(plan["server"]["process"]))
        self.assertEqual(int(self.tmux("display-message", "-p", "#{pid}")), original_pid)
        self.assertEqual(self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}"), before)

    def test_permanent_startup_failure_is_refused_before_tabs_or_applications_are_interrupted(self):
        plan = dict(self.wait_shells(), input={})
        before = self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
        original = worker.run_tmux
        failures = []
        def fail_startup(server, *arguments, **options):
            if options.get("create"):
                self.assertNotEqual(server["socket"], self.socket)
                self.assertTrue(worker.same_process(plan["server"]["process"]))
                failures.append(server["socket"])
                raise worker.Refused("tmux new-session failed: server exited unexpectedly")
            return original(server, *arguments, **options)
        with patch.object(worker, "inspect", return_value={"plans": {"tmux_server": plan}}), \
                patch.object(worker, "run_tmux", side_effect=fail_startup), \
                patch.object(worker, "stop_program", side_effect=AssertionError("startup check stopped an application")):
            result = worker.execute(plan, object(), self.root)
        self.assertFalse(result["ok"], result)
        self.assertIn("original sessions were not stopped", result["message"])
        self.assertEqual(len(failures), 3, "Startup retries must be bounded")
        self.assertTrue(worker.same_process(plan["server"]["process"]))
        self.assertEqual(self.tmux("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}"), before)
        events = [json.loads(line) for line in (self.root / "restart-events.jsonl").read_text().splitlines()]
        self.assertIn("startup_check_failed", [event["phase"] for event in events])
        self.assertNotIn("stopping_server", [event["phase"] for event in events])

    def test_recreation_never_adopts_or_kills_an_already_running_server(self):
        plan = self.wait_shells()
        with self.assertRaisesRegex(worker.Refused, "already owns this socket"):
            worker.recreate_server(plan, self.root)
        self.assertTrue(worker.same_process(plan["server"]["process"]))
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}"), "one")

    def test_gated_server_loss_between_sessions_does_not_start_another_server(self):
        self.tmux("new-session", "-d", "-s", "two", "/usr/bin/bash", "--noprofile", "--norc")
        plan = self.wait_shells()
        self.tmux("kill-server")
        original = worker.run_tmux
        starts = []
        killed = []
        def exit_after_first_session(server, *arguments, **options):
            if options.get("create"):
                starts.append(arguments[0])
            result = original(server, *arguments, **options)
            if arguments[0] == "select-window" and not killed:
                killed.append(True)
                original(server, "kill-server")
            return result
        with patch.object(worker, "run_tmux", side_effect=exit_after_first_session), \
                self.assertRaisesRegex(worker.Refused, "disappeared or changed"):
            worker.recreate_server(plan, self.root)
        self.assertEqual(starts, ["new-session"])
        self.assertFalse(worker.server_listening(plan["server"]))
        self.assertFalse(list(self.root.glob("gate-*")))

    def test_restart_journal_retains_stages_and_exact_rebuilt_pane_mapping(self):
        plan = dict(self.wait_shells(), input={})
        with patch.object(worker, "inspect", return_value={"plans": {"tmux_server": plan}}):
            result = worker.execute(plan, object(), self.root)
        self.assertTrue(result["ok"], result)
        path = self.root / "restart-events.jsonl"
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        events = [json.loads(line) for line in path.read_text().splitlines()]
        phases = [event["phase"] for event in events]
        self.assertLess(phases.index("startup_check_complete"), phases.index("stopping_server"))
        self.assertLess(phases.index("clients_reattached"), phases.index("programs_released"))
        self.assertEqual(phases[-1], "complete")
        gated = next(event for event in events if event["phase"] == "session_gated")
        self.assertEqual(gated["session"], "one")
        self.assertEqual(set(gated["pane_map"]), set(plan["programs"]))
        target = self.root / "keep-private"
        target.write_text("unchanged")
        path.unlink()
        path.symlink_to(target)
        with self.assertRaises(OSError):
            worker.restart_event(self.root, "must-not-overwrite")
        self.assertEqual(target.read_text(), "unchanged")

    def test_server_update_completes_with_a_warning_when_one_application_launcher_exits(self):
        self.tmux("split-window", "-d", "-h", "/usr/bin/bash", "--noprofile", "--norc")
        plan = dict(self.wait_shells(), input={})
        first = next(iter(plan["programs"]))
        old_server = plan["server"]["process"]
        program = plan["programs"][first]["program"]
        program.update(kind="pi", argv=["/usr/bin/false"])
        stop = worker.stop_program
        def stop_fixture(saved):
            stop(dict(saved, kind="bash"))
        started = time.monotonic()
        with patch.object(worker, "inspect", return_value={"plans": {"tmux_server": plan}}), \
                patch.object(worker, "stop_program", side_effect=stop_fixture):
            result = worker.execute(plan, object(), self.root)
        self.assertTrue(result["ok"], result)
        self.assertLess(time.monotonic() - started, 10, "An exited Pi launcher must not consume its 120-second startup timeout")
        self.assertFalse(worker.same_process(old_server))
        self.assertTrue(worker.same_process(self.server()["process"]))
        self.assertEqual(sorted(pane["status"] for pane in result["restoration"]["panes"]), ["failed", "restored"])
        failed = next(pane for pane in result["restoration"]["panes"] if pane["status"] == "failed")
        self.assertIn("status 1", failed["message"])
        self.assertIn("Recover only the affected applications", result["message"])
        self.assertEqual(json.loads(worker.restore_receipt(self.root, first).read_text())["exit_code"], 1)

    def test_confirmed_server_restart_disconnects_the_remote_client_and_recreates_its_detached_session(self):
        self.wait_shells()
        client = subprocess.Popen([self.binary, "-N", "-S", self.socket, "-C", "attach-session", "-t", "one"],
                                  stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  start_new_session=True)
        try:
            deadline = time.monotonic() + 5
            while str(client.pid) not in self.tmux("list-clients", "-F", "#{client_pid}").splitlines():
                self.assertIsNone(client.poll(), "the private client failed to attach")
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            # Real mosh ancestry is tested above; this private control-mode client exercises disconnection.
            def route(saved, records=()):
                self.assertEqual(saved["pid"], client.pid)
                return dict(transport="mosh", ancestry=[dict(pid=client.pid, start=saved["start"], image=saved["image"])])
            def preview(payload, system):
                value = worker.server_plan(self.server(), [])
                value["input"] = {}
                return dict(plans={"tmux_server": value})
            with patch.object(worker, "remote_mosh_client", side_effect=route), patch.object(worker, "inspect", side_effect=preview):
                plan = preview({}, None)["plans"]["tmux_server"]
                self.assertIsNone(client.poll(), "preview must not close a remote client")
                self.assertEqual(len(plan["remote_clients"]), 1)
                result = worker.execute(plan, object(), self.root)
            self.assertTrue(result["ok"], result)
            self.assertIn("remote mosh tmux client(s) were disconnected", result["message"])
            client.wait(timeout=5)
            self.assertNotEqual(int(self.tmux("display-message", "-p", "#{pid}")), plan["server"]["process"]["pid"])
            self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}:#{session_attached}"), "one:0")
        finally:
            if client.poll() is None:
                client.terminate()
                client.wait(timeout=5)
            client.stdin.close()

    def test_linked_windows_are_rejected_without_stopping_a_session(self):
        self.tmux("new-session", "-d", "-s", "two", "/usr/bin/bash", "--noprofile", "--norc")
        self.tmux("link-window", "-s", "one:0", "-t", "two:2")
        with self.assertRaisesRegex(worker.Refused, "Linked tmux windows"):
            worker.server_plan(self.server(), [])
        self.assertEqual(len(self.tmux("list-sessions", "-F", "#{session_name}").splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
