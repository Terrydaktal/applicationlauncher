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
            with self.assertRaisesRegex(worker.Refused, "exact running Fish or Codex"):
                worker.inspect(dict(process=requested), object())


class Guards(unittest.TestCase):

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


    def test_unsupported_terminal_and_shell_jobs_are_refused(self):
        with self.assertRaisesRegex(worker.Refused, "Update this xfce4-terminal"):
            worker.require_handoff({"restart_supported":False})
        with patch.object(worker, "latest_binary", return_value="/bin/fish"), patch.object(worker, "children", return_value=[99]):
            with self.assertRaisesRegex(worker.Refused, "background jobs"):
                worker.program_plan({"pid":1, "cwd":"/tmp", "executable":"/bin/fish", "arguments":["fish"]})



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















if __name__ == "__main__":
    unittest.main()
