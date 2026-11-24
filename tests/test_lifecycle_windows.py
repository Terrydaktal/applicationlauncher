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
        if return_shell:
            tab["argv"] = ["fish", "--interactive", "--init-command", "codex resume exact-session"]
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
            captures = 0
            def capture(snapshot):
                nonlocal captures
                captures += 1
                result = copy.deepcopy(tab)
                if fault == "scope" and captures > 1:
                    result["program"]["argv"][2] = "different-session"
                return [result]
            patches.enter_context(patch.object(window.WindowUpdate, "capture_tabs", side_effect=capture))
            patches.enter_context(patch.object(window.WindowUpdate, "revalidate", side_effect=lambda: events.append("revalidated")))
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
            class Backend:
                def operate(self, action, value):
                    return json.loads(system.call("unused", "unused", "unused", "Operate", "(sss)", ["private", action, json.dumps(value)])[0])["ok"]
                def close(self):
                    events.append("compatibility-closed")
            def compatibility():
                events.append("compatibility-selected")
                if fault == "compatibility":
                    raise worker.Refused("The original tabs changed during compatibility handover")
                return Backend()
            patches.enter_context(patch.object(window.WindowUpdate, "compatibility_backend", side_effect=compatibility))
            backend = None
            if external:
                backend = Backend()
            request = dict(helper_owner=":1.2", token="private")
            if preview_scope is not None:
                request["scope_fingerprint"] = preview_scope
            job = window.WindowUpdate(request, system, directory, backend=backend)
            result = job.run()
            self.assertFalse((Path(directory) / "applicationlauncher/lifecycle/builds").exists(),
                             "A terminal update archived executable contents")
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
        self.assertEqual(records["restart-plan.json"]["scope"], "terminal and supported applications; preserve tmux servers and panes")
        self.assertEqual(statuses[-1]["phase"], "complete")
        self.assertTrue(statuses[-1]["original_closed"])

    def test_confirmation_names_the_exact_codex_conversation_not_fish_options(self):
        result, _, statuses, _ = self.run_job(return_shell=True)
        self.assertTrue(result["ok"], result)
        ready = next(status for status in statuses if status["phase"] == "ready")
        self.assertIn("conversation exact-session", ready["message"])
        self.assertNotIn("--init-command", ready["message"])

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

    def test_old_terminal_menu_uses_compatibility_before_staging_non_tmux_apps(self):
        for application in ("codex", "fish", "htop"):
            with self.subTest(application=application):
                result, events, _, _ = self.run_job(application=application, capabilities=False)
                self.assertTrue(result["ok"], result)
                self.assertLess(events.index("compatibility-selected"), events.index("AwaitConfirmation"))
                self.assertLess(events.index("Stage"), events.index("stopped"))
                self.assertLess(events.index("verified"), events.index("Commit"))
                self.assertNotIn("Hold", events)
                self.assertEqual(events.count("Stage"), 1)
                self.assertIn("compatibility-closed", events)

    def test_compatibility_menu_failure_never_stops_before_confirmation_and_staging(self):
        for fault in ("compatibility", "scope", "AwaitConfirmation", "Stage"):
            with self.subTest(fault=fault):
                result, events, statuses, _ = self.run_job(fault, capabilities=False)
                self.assertFalse(result["ok"])
                self.assertNotIn("stopped", events)
                self.assertNotIn("Release", events)
                self.assertNotIn("Commit", events)
                self.assertEqual(statuses[-1]["phase"], "failed")

    def test_compatibility_shutdown_or_verification_failure_does_not_replay(self):
        for fault in ("stop", "verify"):
            with self.subTest(fault=fault):
                result, events, _, records = self.run_job(fault, capabilities=False)
                self.assertFalse(result["ok"])
                self.assertIn("Recover", events)
                self.assertNotIn("Commit", events)
                self.assertEqual(events.count("Stage"), 1)
                self.assertIn("restart-plan.json", records)
                self.assertIn("compatibility-closed", events)

    def test_daemon_owned_compatibility_update_needs_no_native_hold(self):
        result, events, _, _ = self.run_job(capabilities=False, external=True)
        self.assertTrue(result["ok"], result)
        self.assertNotIn("Hold", events)
        self.assertNotIn("compatibility-selected", events)
        self.assertLess(events.index("Stage"), events.index("stopped"))
        self.assertLess(events.index("verified"), events.index("Commit"))

    def test_tmux_reattachment_does_not_require_stopping_a_server_or_pane(self):
        result, events, statuses, _ = self.run_job(application="tmux", capabilities=False)
        self.assertTrue(result["ok"], result)
        self.assertNotIn("Hold", events)
        self.assertNotIn("compatibility-selected", events)
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
    def test_codex_return_shell_is_not_signalled_before_replacement_verification(self):
        codex = dict(pid=60, start=70, image=[1, 2, 3, 4], tty=1, parent=50, group=50,
                     executable="/fixture/codex")
        fish = dict(pid=50, start=65, image=[1, 3, 4, 5], tty=1, parent=40, group=50,
                    executable="/fixture/fish")
        bash = dict(pid=40, start=60, image=[1, 4, 5, 6], tty=1, parent=30, group=40,
                    executable="/fixture/bash")
        tab = dict(program=dict(kind="codex", process=codex), ancestors=[codex, fish, bash])
        processes = {50: fish, 40: bash}
        def process(pid):
            if pid not in processes: raise ProcessLookupError(pid)
            return processes[pid]
        job = window.WindowUpdate({}, object(), "/not-used")
        with patch.object(window, "require_idle_codex"), patch.object(job, "event"), \
                patch.object(worker, "stop_program") as stop, \
                patch.object(worker, "process", side_effect=process), \
                patch.object(worker, "children", side_effect=lambda pid: [50] if pid == 40 else []), \
                patch.object(window.signal, "pidfd_send_signal", side_effect=AssertionError("Signalled a return shell")):
            job.stop_tab(tab)
            stop.assert_called_once_with(tab["program"])
            self.assertEqual(tab["retained_shells"], [fish, bash])
            job.verify_return_shells(tab)
        self.assertTrue(job.interrupted)

    def test_return_shell_starting_a_job_or_changing_image_blocks_handover(self):
        saved = dict(pid=50, start=65, image=[1, 3, 4, 5], tty=1, parent=40, group=50,
                     executable="/fixture/fish")
        job = window.WindowUpdate({}, object(), "/not-used")
        for changed, children in (({}, [999]), ({"start": 66}, []), ({"image": [9, 9, 9, 9]}, [])):
            with self.subTest(changed=changed, children=children), \
                    patch.object(worker, "process", return_value={**saved, **changed}), \
                    patch.object(worker, "children", return_value=children):
                with self.assertRaisesRegex(worker.Refused, "return shell"):
                    job.verify_return_shells(dict(retained_shells=[saved]))

    def test_return_shell_ignores_exited_children_waiting_to_be_reaped(self):
        saved = dict(pid=50, start=65, image=[1, 3, 4, 5], tty=1, parent=40, group=50,
                     executable="/fixture/fish")
        job = window.WindowUpdate({}, object(), "/not-used")
        for failure in (FileNotFoundError, ProcessLookupError):
            def process(pid):
                if pid == 50: return saved
                raise failure(pid)
            with self.subTest(failure=failure), patch.object(worker, "process", side_effect=process), \
                    patch.object(worker, "children", return_value=[60]):
                job.verify_return_shells(dict(retained_shells=[saved]))

    def test_return_shell_permission_failure_is_not_treated_as_process_exit(self):
        saved = dict(pid=50)
        job = window.WindowUpdate({}, object(), "/not-used")
        with patch.object(worker, "process", side_effect=PermissionError("Cannot inspect the return shell")):
            with self.assertRaises(PermissionError):
                job.verify_return_shells(dict(retained_shells=[saved]))

    def test_tmux_window_with_sibling_shell_jobs_is_refused_before_replacement(self):
        root = dict(pid=50)
        active = dict(pid=60, executable="/fixture/tmux", arguments=["tmux"], cwd="/fixture", image=[1, 2, 3, 4])
        record = dict(child_pid=50, foreground_pgid=60, active=True)
        with patch.object(worker, "foreground", return_value=(root, active)), \
                patch.object(window.WindowUpdate, "pin", return_value="/fixture/pinned"), \
                patch.object(window.WindowUpdate, "latest_image", return_value=({}, "/fixture/latest")), \
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
                        patches.enter_context(patch.object(window.WindowUpdate, "latest_image", return_value=({}, "/fixture/pinned")))
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

    def test_window_planning_uses_latest_build_and_retains_the_original_image(self):
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
                executable.unlink()
                shutil.copyfile("/bin/true", executable)
                executable.chmod(0o700)
                installed = worker.file_identity(executable)
                with patch.object(worker, "foreground", return_value=(active, active)), \
                        patch.object(worker, "children", return_value=[]), \
                        patch.object(worker, "program_plan", wraps=worker.program_plan) as adapter, \
                        patch.object(worker, "latest_binary", return_value=str(executable)):
                    tabs = job.capture_tabs(dict(records=[binding]))
                adapter.assert_called_once()
                pinned = tabs[0]["executable"]
                self.assertEqual(tabs[0]["argv"][0], active["arguments"][0])
                self.assertNotEqual(installed, active["image"])
                self.assertEqual(worker.file_identity(pinned), installed)
                self.assertEqual(tabs[0]["program"]["installed"], installed)
                original = tabs[0]["original_executable"]
                self.assertEqual(worker.file_identity(original), active["image"])
                executable.unlink()
                child.terminate()
                child.wait(timeout=3)
                self.assertEqual(worker.file_identity(pinned), installed)
                self.assertEqual(worker.file_identity(original), active["image"])
                replacement = subprocess.Popen(["updated-fish"], executable=pinned)
                self.assertEqual(replacement.wait(timeout=3), 0)
                self.assertFalse((Path(directory) / "applicationlauncher/lifecycle/builds").exists())
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
