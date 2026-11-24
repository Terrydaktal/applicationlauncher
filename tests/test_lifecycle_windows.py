"""Coordinator safety/ordering tests. No user's terminals or services are used."""
import copy
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "src/lifecycle"
spec = importlib.util.spec_from_file_location("lifecycle_worker", SOURCE / "worker.py")
worker = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = worker
spec.loader.exec_module(worker)
spec = importlib.util.spec_from_file_location("lifecycle_timing", SOURCE / "timing.py")
timing = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = timing
spec.loader.exec_module(timing)
spec = importlib.util.spec_from_file_location("lifecycle_window", SOURCE / "window.py")
window = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = window
spec.loader.exec_module(window)


class Coordinator(unittest.TestCase):
    def run_job(self, fault=None, application="codex", capabilities=True, external=False, timings=False, preview_scope=None, return_shell=False):
        events, statuses = [], []
        saved = dict(pid=60, start=70, image=[1, 2, 3, 4], cwd="/fixture/project", executable=f"/fixture/{application}")
        record = dict(child_pid=50, foreground_pgid=50, active=True)
        program = dict(kind=application, process=saved, cwd=saved["cwd"], argv=[f"/fixture/{application}", "resume", "exact-session"], installed=saved["image"])
        tab = dict(argv=program["argv"], cwd=program["cwd"], active=True, program=program, record=record)
        class System:
            def call(self, owner, path, interface, method, signature, arguments, timeout=0):
                token, action, value = arguments
                value = json.loads(value)
                events.append(action)
                if action == fault:
                    raise worker.Refused(f"injected {fault} failure")
                if action == "Capture":
                    result = dict(records=[record], capabilities=dict(hold=capabilities))
                elif action == "Publish":
                    statuses.append(value)
                    result = {}
                elif action == "Records": result = dict(records=[dict(child_pid=80)])
                elif action == "Commit":
                    if "verified" not in events:
                        raise AssertionError("Original window was closed before the application was verified")
                    result = dict(original_closed=True, replacement_pid=90, replacement_window_id="new-window")
                else: result = {}
                return [json.dumps(dict(ok=result))]
        with tempfile.TemporaryDirectory(prefix="lifecycle-order-") as root, ExitStack() as patches:
            directory = str(Path(root) / "job")
            Path(directory).mkdir(mode=0o700)
            if timings:
                window.atomic_record(Path(directory) / "timing-request.json", dict(schema_version=1,
                    deadline_unix_ms=time.time_ns() // 1_000_000 + 120000, build_id="private-test-build"))
            patches.enter_context(patch.dict(os.environ, XDG_STATE_HOME=directory))
            patches.enter_context(patch.object(window.WindowUpdate, "capture_tabs", return_value=[tab]))
            patches.enter_context(patch.object(window.WindowUpdate, "revalidate", side_effect=lambda: events.append("revalidated")))
            patches.enter_context(patch.object(window.WindowUpdate, "preserve_builds", side_effect=lambda: events.append("builds-preserved")))
            def stop(target, retire_tmux=False):
                events.append("retired-tmux-client" if retire_tmux else "stopped")
                job.interrupted = application != "tmux"
                if fault == "stop": raise worker.Refused("No graceful exit")
            patches.enter_context(patch.object(window.WindowUpdate, "stop_tab", side_effect=stop))
            def verify(*arguments):
                events.append("verified")
                if fault == "verify": raise worker.Refused("Wrong conversation")
            patches.enter_context(patch.object(worker, "verify_program", side_effect=verify))
            patches.enter_context(patch.object(worker, "process", return_value=dict(pid=80, foreground=80)))
            patches.enter_context(patch.object(worker, "foreground", return_value=({}, saved)))
            patches.enter_context(patch.object(window.WindowUpdate, "verify_tmux", side_effect=verify))
            system = System()
            backend = None
            if external:
                class Backend:
                    def operate(self, action, value):
                        return json.loads(system.call("unused", "unused", "unused", "Operate", "(sss)", ["private", action, json.dumps(value)])[0])["ok"]
                backend = Backend()
            job = window.WindowUpdate(dict(helper_owner=":1.2", token="private"), system, directory, backend=backend)
            result = job.run()
            records = {path.name: json.loads(path.read_text()) for path in Path(directory).glob("*.json")}
            if timings:
                path = Path(root) / "timings/job.jsonl"
                records["timing-events"] = [json.loads(line) for line in path.read_text().splitlines()]
            return result, events, statuses, records

    def test_shared_coordinator_verifies_before_closing_the_original_window(self):
        result, events, statuses, records = self.run_job()
        self.assertTrue(result["ok"], result)
        for first, second in [("AwaitConfirmation", "Stage"), ("Stage", "Hold"), ("Hold", "stopped"),
                              ("stopped", "Release"), ("Release", "verified"), ("verified", "Commit")]:
            self.assertLess(events.index(first), events.index(second), (first, second, events))
        self.assertEqual(records["restart-plan.json"]["scope"], "terminal-only; preserve running application builds")
        self.assertEqual(statuses[-1]["phase"], "complete")
        self.assertTrue(statuses[-1]["original_closed"])

    def test_decline_or_staging_failure_never_stops_an_application_or_closes_a_window(self):
        for fault in ("AwaitConfirmation", "Stage"):
            with self.subTest(fault=fault):
                result, events, statuses, _ = self.run_job(fault)
                self.assertFalse(result["ok"])
                self.assertNotIn("stopped", events)
                self.assertNotIn("Release", events)
                self.assertNotIn("Commit", events)
                self.assertIn("Discard", events)
                self.assertEqual(statuses[-1]["phase"], "failed")

    def test_shutdown_or_verification_failure_retains_the_original_and_never_replays(self):
        for fault in ("stop", "verify"):
            with self.subTest(fault=fault):
                result, events, statuses, records = self.run_job(fault)
                self.assertFalse(result["ok"])
                self.assertNotIn("Commit", events)
                self.assertIn("Recover", events)
                self.assertEqual(events.count("Stage"), 1)
                self.assertIn("restart-plan.json", records)
                self.assertFalse(statuses[-1]["original_closed"])
                if fault == "stop": self.assertNotIn("Release", events)

    def test_old_terminals_without_native_hold_are_refused_before_staging_non_tmux_apps(self):
        result, events, _, _ = self.run_job(capabilities=False)
        self.assertFalse(result["ok"])
        self.assertIn("window-hold API", result["message"])
        self.assertNotIn("Stage", events)
        self.assertNotIn("stopped", events)

    def test_daemon_owned_compatibility_update_needs_no_native_hold(self):
        result, events, _, _ = self.run_job(capabilities=False, external=True)
        self.assertTrue(result["ok"], result)
        self.assertNotIn("Hold", events)
        self.assertLess(events.index("Stage"), events.index("stopped"))
        self.assertLess(events.index("verified"), events.index("Commit"))

    def test_tmux_reattachment_does_not_require_stopping_a_server_or_pane(self):
        result, events, statuses, _ = self.run_job(application="tmux", capabilities=False)
        self.assertTrue(result["ok"], result)
        self.assertNotIn("Hold", events)
        ready = next(status for status in statuses if status["phase"] == "ready")
        self.assertTrue(ready["preserves_processes"])

    def test_external_tmux_client_retires_only_after_replacement_verification(self):
        result, events, _, _ = self.run_job(application="tmux", capabilities=False, external=True)
        self.assertTrue(result["ok"], result)
        self.assertLess(events.index("verified"), events.index("retired-tmux-client"))
        self.assertLess(events.index("retired-tmux-client"), events.index("Commit"))
        result, events, _, _ = self.run_job(application="tmux", capabilities=False, external=True, fault="verify")
        self.assertFalse(result["ok"])
        self.assertNotIn("retired-tmux-client", events)


class SharedAdapters(unittest.TestCase):
    def test_tmux_window_with_sibling_shell_jobs_is_refused_before_replacement(self):
        root = dict(pid=50)
        active = dict(pid=60, executable="/fixture/tmux", arguments=["tmux"], cwd="/fixture", image=[1, 2, 3, 4])
        record = dict(child_pid=50, foreground_pgid=60, active=True)
        with patch.object(worker, "foreground", return_value=(root, active)), \
                patch.object(window.WindowUpdate, "pin", return_value="/fixture/pinned"), \
                patch.object(worker, "tmux_client", return_value=(dict(socket="/fixture/tmux.sock"), "$1", "fixture")), \
                patch.object(worker, "run_tmux", return_value="created"), \
                patch.object(worker, "require_dedicated_tree", side_effect=worker.Refused("The shell has other jobs")) as guard:
            with self.assertRaisesRegex(worker.Refused, "other jobs"):
                window.WindowUpdate({}, object(), "/not-used").capture_tabs(dict(records=[record]))
        self.assertEqual(guard.call_args.args[:2], (root, active))

    def test_window_planning_preserves_raw_monitor_titles_and_legacy_directory_fallbacks(self):
        for name in ("htop", "nvtop"):
            for cwd, expected in ((str(Path.home()), "~"), (str(Path.home() / "tasks/monitor"), "~/tasks/monitor"), ("/tmp/monitor", "/tmp/monitor")):
                for title in (None, "raw ~/100%/literal-%D 'quoted'"):
                    with self.subTest(name=name, cwd=cwd, title=title), ExitStack() as patches:
                        active = dict(pid=60, executable=f"/fixture/{name}", arguments=[name], cwd=cwd)
                        program = dict(kind=name, process=active, argv=["/fixture/pinned"], cwd=cwd)
                        record = dict(child_pid=60, foreground_pgid=60, active=True)
                        if title is not None: record["terminal_title"] = title
                        patches.enter_context(patch.object(worker, "foreground", return_value=(active, active)))
                        patches.enter_context(patch.object(worker, "program_plan", return_value=program))
                        patches.enter_context(patch.object(worker, "require_dedicated_tree"))
                        patches.enter_context(patch.object(worker, "terminal_environment", return_value={}))
                        patches.enter_context(patch.object(window.WindowUpdate, "pin", return_value="/fixture/pinned"))
                        job = window.WindowUpdate({}, object(), "/not-used")
                        tab = job.capture_tabs(dict(records=[record]))[0]
                        self.assertEqual(tab["title"], title if title is not None else f"{name} {expected}")

    def test_terminal_environment_preserves_only_allowlisted_values_and_absence(self):
        with patch.object(worker, "read_limited", return_value=b"NO_COLOR=\0COLORTERM=truecolor\0LANG=C.UTF-8\0SECRET_TOKEN=never-save-this\0"):
            environment = worker.terminal_environment(123)
        self.assertEqual(environment["NO_COLOR"], "")
        self.assertEqual(environment["COLORTERM"], "truecolor")
        self.assertEqual(environment["LANG"], "C.UTF-8")
        self.assertIsNone(environment["FORCE_COLOR"])
        self.assertNotIn("SECRET_TOKEN", environment)

    def test_window_planning_uses_the_shared_adapter_and_pins_the_running_build(self):
        with tempfile.TemporaryDirectory(prefix="lifecycle-pin-") as directory, patch.dict(os.environ, XDG_STATE_HOME=directory):
            executable = Path(directory) / "fish"
            shutil.copyfile("/bin/sleep", executable)
            executable.chmod(0o700)
            child = subprocess.Popen([str(executable), "30"])
            job = window.WindowUpdate(dict(job_id="private-test-job"), object(), directory)
            try:
                deadline = time.monotonic() + 3
                while True:
                    active = worker.process(child.pid)
                    if active["executable"] == str(executable): break
                    self.assertIsNone(child.poll(), "The private executable exited before readiness")
                    self.assertLess(time.monotonic(), deadline, "The private child never execed its fixture image")
                    time.sleep(0.01)
                binding = dict(child_pid=child.pid, foreground_pgid=active["group"], active=True)
                with patch.object(worker, "foreground", return_value=(active, active)), \
                        patch.object(worker, "children", return_value=[]), \
                        patch.object(worker, "program_plan", wraps=worker.program_plan) as adapter, \
                        patch.object(worker, "latest_binary", side_effect=AssertionError("A terminal-only update upgraded an application")):
                    tabs = job.capture_tabs(dict(records=[binding]))
                adapter.assert_called_once()
                pinned = tabs[0]["executable"]
                self.assertEqual(tabs[0]["argv"][0], active["arguments"][0])
                job.tabs = tabs
                job.preserve_builds()
                recovery = tabs[0]["recovery_argv"][0]
                self.assertEqual(worker.file_identity(pinned), active["image"])
                self.assertEqual(Path(recovery).read_bytes(), Path(pinned).read_bytes())
                executable.unlink()
                shutil.copyfile("/bin/true", executable)
                self.assertNotEqual(worker.file_identity(executable), active["image"])
                child.terminate()
                child.wait(timeout=3)
                self.assertEqual(worker.file_identity(pinned), active["image"])
            finally:
                if child.poll() is None: child.terminate(); child.wait(timeout=3)
                for descriptor in job.descriptors: os.close(descriptor)

    def test_shared_lock_prevents_overlapping_restart_kinds(self):
        with tempfile.TemporaryDirectory(prefix="lifecycle-lock-") as directory, patch.dict(os.environ, XDG_STATE_HOME=directory):
            with worker.operation_lock():
                with self.assertRaisesRegex(worker.Refused, "Another lifecycle"):
                    with worker.operation_lock(): pass
            with worker.operation_lock(): pass

    def test_private_plan_storage_is_durable_and_rejects_symlinks_and_public_files(self):
        with tempfile.TemporaryDirectory(prefix="lifecycle-record-") as directory:
            path = Path(directory) / "plan.json"
            window.atomic_record(path, dict(phase="ready"))
            self.assertEqual(window.private_request(path), dict(phase="ready"))
            alias = Path(directory) / "alias.json"
            alias.symlink_to(path)
            with self.assertRaises(OSError): window.private_request(alias)
            path.chmod(0o644)
            with self.assertRaises(worker.Refused): window.private_request(path)

    def test_terminal_source_contains_no_application_detection_or_resume_policy(self):
        terminal = Path(__file__).resolve().parents[2] / "../repos/xfce4-terminal/terminal/update"
        if not terminal.exists():
            self.skipTest("The separate xfce4-terminal checkout is not available")
        for name in ("processes.py", "core.py"):
            source = (terminal / name).read_text()
            for removed in ("codex_session", "def capture_tab", "def tmux_plan", "resume", "--last"):
                self.assertNotIn(removed, source)


if __name__ == "__main__": unittest.main()
