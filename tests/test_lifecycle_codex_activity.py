"""Codex activity guards use private rollout metadata, never real sessions."""
from contextlib import ExitStack
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from test_lifecycle_windows import worker, window


ROOT = "11111111-1111-4111-8111-111111111111"
CHILD = "22222222-2222-4222-8222-222222222222"


def event(kind):
    return dict(type="event_msg", payload=dict(type=kind, turn_id="fixture-turn"))


class CodexActivity(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="codex-idle-guard-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.proc = self.root / "proc"
        self.fds = self.proc / "123/fd"
        self.fds.mkdir(parents=True)
        self.saved = dict(pid=123, parent=50, start=70, image=[1, 2, 3, 4], tty=10, group=123,
                          cwd=str(self.root), executable="/fixture/codex", arguments=["codex"])
        self.program = dict(kind="codex", process=self.saved, cwd=str(self.root),
                            argv=["/fixture/pinned-codex", "resume", ROOT], installed=self.saved["image"])
        self.main = self.rollout(ROOT, [event("task_started"), event("task_complete")])
        for name, replacement in [
                ("same_process", dict(return_value=True)),
                ("session_id", dict(return_value=ROOT)),
                ("stop_program", {}),
        ]:
            mocked = patch.object(worker, name, **replacement)
            setattr(self, name, mocked.start())
            self.addCleanup(mocked.stop)

    def rollout(self, identity, rows, parent=None):
        path = self.root / f"rollout-{identity}.jsonl"
        payload = dict(id=identity)
        if parent is not None:
            payload["source"] = dict(subagent=dict(thread_spawn=dict(parent_thread_id=parent)))
        values = [dict(type="session_meta", payload=payload), *rows]
        path.write_text("".join(json.dumps(value) + "\n" for value in values))
        (self.fds / str(len(list(self.fds.iterdir())))).symlink_to(path)
        return path

    def require_idle(self):
        window.require_idle_codex(self.program, self.proc)
        self.stop_program.assert_not_called()

    def capture(self, child_pids):
        stack = self.enterContext(ExitStack())
        guard = window.require_idle_codex
        stack.enter_context(patch.object(window, "require_idle_codex", side_effect=lambda program: guard(program, self.proc)))
        stack.enter_context(patch.object(worker, "foreground", return_value=(self.saved, self.saved)))
        stack.enter_context(patch.object(worker, "children", return_value=child_pids))
        stack.enter_context(patch.object(worker, "program_plan", return_value=self.program))
        stack.enter_context(patch.object(worker, "require_dedicated_tree"))
        stack.enter_context(patch.object(worker, "terminal_environment", return_value={}))
        stack.enter_context(patch.object(window.WindowUpdate, "pin", return_value="/fixture/pinned-codex"))
        stack.enter_context(patch.object(window.WindowUpdate, "latest_image", return_value=(
            dict(name="codex", source="/fixture/codex", image=self.saved["image"]), "/fixture/pinned-codex")))
        stack.enter_context(patch.object(window.WindowUpdate, "require_target"))
        stack.enter_context(patch.object(window.WindowUpdate, "return_shell", return_value=dict(
            executable="/fixture/pinned-fish", argv0="fish", source="/fixture/fish", image=[5, 6, 7, 8])))
        job = window.WindowUpdate({}, object(), self.root)
        job.tabs = job.capture_tabs(dict(records=[dict(child_pid=123, foreground_pgid=123, active=True)]))
        return job

    def test_idle_codex_with_persistent_helpers_can_update_the_terminal(self):
        job = self.capture([201, 202])
        self.assertEqual(job.tabs[0]["program"], self.program)
        self.assertIn(ROOT, job.tabs[0]["argv"][3])
        self.stop_program.assert_not_called()

    def test_working_codex_without_any_children_is_blocked(self):
        with self.main.open("a") as stream:
            stream.write(json.dumps(event("task_started")) + "\n")
        with self.assertRaisesRegex(worker.Refused, "working on a turn"):
            self.capture([])
        self.stop_program.assert_not_called()

    def test_activity_is_rechecked_after_preview_and_immediately_before_interruption(self):
        job = self.capture([201])
        with self.main.open("a") as stream:
            stream.write(json.dumps(event("task_started")) + "\n")
        with patch.object(job, "remote", return_value={}):
            with self.assertRaisesRegex(worker.Refused, "working on a turn"):
                job.revalidate()
        with self.assertRaisesRegex(worker.Refused, "working on a turn"):
            job.stop_tab(job.tabs[0])
        self.assertFalse(job.interrupted)
        self.stop_program.assert_not_called()

    def test_completed_interrupted_modern_and_new_conversations_are_idle(self):
        for rows in ([], [event("task_complete")], [event("turn_started"), event("turn_complete")],
                     [event("task_started"), event("turn_aborted")]):
            with self.subTest(rows=rows):
                header = dict(type="session_meta", payload=dict(id=ROOT))
                self.main.write_text("".join(json.dumps(row) + "\n" for row in [header, *rows]))
                self.require_idle()

    def test_started_turn_and_newly_received_user_message_are_busy(self):
        for kind in ("task_started", "turn_started", "user_message"):
            with self.subTest(kind=kind):
                header = dict(type="session_meta", payload=dict(id=ROOT))
                self.main.write_text("".join(json.dumps(row) + "\n" for row in [header, event(kind)]))
                with self.assertRaisesRegex(worker.Refused, "working on a turn"):
                    self.require_idle()

    def test_running_child_agent_blocks_but_completed_child_does_not(self):
        child = self.rollout(CHILD, [event("task_started")], parent=ROOT)
        with self.assertRaisesRegex(worker.Refused, "working on a turn"):
            self.require_idle()
        with child.open("a") as stream:
            stream.write(json.dumps(event("task_complete")) + "\n")
        self.require_idle()

    def test_unrelated_or_missing_main_rollout_is_not_misreported_as_idle(self):
        self.rollout(CHILD, [event("task_complete")], parent="33333333-3333-4333-8333-333333333333")
        with self.assertRaisesRegex(worker.Refused, "could not be verified"):
            self.require_idle()
        for entry in self.fds.iterdir():
            entry.unlink()
        with self.assertRaisesRegex(worker.Refused, "could not be verified"):
            self.require_idle()

    def test_partial_malformed_and_unsafe_activity_is_unknown_not_idle(self):
        before = self.main.read_bytes()
        for suffix in (b'{"type":', b"invalid JSON\n"):
            with self.subTest(suffix=suffix):
                self.main.write_bytes(before + suffix)
                with self.assertRaisesRegex(worker.Refused, "could not be verified"):
                    self.require_idle()
        self.main.write_bytes(before)
        self.main.chmod(0o666)
        with self.assertRaisesRegex(worker.Refused, "could not be verified"):
            self.require_idle()

    def test_replaced_process_or_changed_activity_is_not_accepted(self):
        self.same_process.side_effect = [True, False]
        with self.assertRaisesRegex(worker.Refused, "could not be verified"):
            self.require_idle()
        self.same_process.side_effect = None
        self.same_process.return_value = True
        stat = os.stat(self.main)
        changed = type("Stat", (), dict(st_dev=stat.st_dev, st_ino=stat.st_ino + 1,
                                       st_size=stat.st_size, st_mtime_ns=stat.st_mtime_ns))()
        with patch.object(window.os, "stat", return_value=changed):
            with self.assertRaisesRegex(worker.Refused, "could not be verified"):
                self.require_idle()

    def test_tail_inspection_is_bounded_and_requires_state_inside_its_window(self):
        header = json.dumps(dict(type="session_meta", payload=dict(id=ROOT))).encode() + b"\n"
        large = json.dumps(dict(type="response_item", payload="x" * (2 * 1024 * 1024))).encode() + b"\n"
        terminal = json.dumps(event("task_complete")).encode() + b"\n"
        data = header + large + terminal
        identity, parent, idle, used = window.codex_rollout_idle(io.BytesIO(data), len(data), 8 * 1024 * 1024)
        self.assertEqual((identity, parent, idle), (ROOT, None, True))
        self.assertLessEqual(used, len(header) + 1024 * 1024)
        data = header + terminal + large
        self.assertIsNone(window.codex_rollout_idle(io.BytesIO(data), len(data), 8 * 1024 * 1024)[2])


if __name__ == "__main__":
    unittest.main()
