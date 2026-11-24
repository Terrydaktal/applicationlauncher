"""Per-application restore results; no user terminal or tmux socket is used."""
from contextlib import ExitStack
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("restore_result_worker", Path(__file__).resolve().parents[1] / "src/lifecycle/worker.py")
worker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(worker)


class RestoreResults(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="launcher-restore-results-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.plan = dict(server=dict(process=dict(pid=90)), sessions=[["$1", "work", "0", "", "@1"]],
                         windows=[["$1", "@1", "0", "work", "layout", "80", "24", "0"]],
                         panes=[["@1", "%1", "0", "1"], ["@1", "%2", "1", "0"]],
                         bindings=[], programs={"%1": dict(program=dict(kind="pi", cwd="/work")),
                                                "%2": dict(program=dict(kind="fish", cwd="/work"))})
        self.pane_map = {"%1": "%10", "%2": "%11"}
        self.clock = 0

    def advance(self, seconds):
        self.clock += seconds

    def verify(self, check):
        with ExitStack() as stack:
            stack.enter_context(patch.object(worker, "server_restart_scope", return_value=dict(panes=[
                dict(session="work", window_index="0", pane_index=str(index), application=kind, cwd="/work")
                for index, kind in enumerate(("pi", "fish"))])))
            stack.enter_context(patch.object(worker, "run_tmux", return_value="%10\t100\n%11\t101\n"))
            stack.enter_context(patch.object(worker, "same_process", return_value=True))
            stack.enter_context(patch.object(worker, "server_listening", return_value=True))
            stack.enter_context(patch.object(worker, "check_program", side_effect=check))
            stack.enter_context(patch.object(worker.time, "monotonic", side_effect=lambda: self.clock))
            stack.enter_context(patch.object(worker.time, "sleep", side_effect=self.advance))
            return worker.verify_server_programs(self.plan, self.plan["server"], self.pane_map, self.directory)

    def test_failed_pi_launcher_is_reported_immediately_and_other_panes_are_verified(self):
        worker.atomic_record(worker.restore_receipt(self.directory, "%1"), dict(exit_code=2))
        seen = []
        def check(program, pid):
            seen.append((program["kind"], pid))
            if program["kind"] == "pi":
                raise worker.Refused("Expected pi; foreground program is fish")
        result = self.verify(check)
        self.assertTrue(result["server_replaced"])
        self.assertTrue(result["finished"])
        self.assertEqual([pane["status"] for pane in result["panes"]], ["failed", "restored"])
        self.assertIn("status 2", result["panes"][0]["message"])
        self.assertEqual(seen, [("pi", 100), ("fish", 101)])
        self.assertEqual(self.clock, 0)
        self.assertEqual(json.loads((self.directory / "restore-status.json").read_text()), result)
        events = [json.loads(line) for line in (self.directory / "restart-events.jsonl").read_text().splitlines()]
        self.assertEqual([event["phase"] for event in events], ["application_restore_failed", "application_restored"])

    def test_slow_pi_does_not_delay_verifying_other_panes_and_preserves_the_last_reason(self):
        verified_at = []
        def check(program, pid):
            if program["kind"] == "pi":
                raise worker.Refused("Pi has not opened the planned exact conversation")
            verified_at.append(self.clock)
        result = self.verify(check)
        self.assertEqual(verified_at, [0])
        self.assertLess(self.clock, 120.2)
        self.assertIn("Verification timed out", result["panes"][0]["message"])
        self.assertIn("exact conversation", result["panes"][0]["message"])

    def test_all_slow_pi_panes_share_one_deadline_instead_of_waiting_serially(self):
        self.plan["programs"]["%2"]["program"]["kind"] = "pi"
        def check(program, pid):
            raise worker.Refused("Waiting for process-bound session identity")
        result = self.verify(check)
        self.assertEqual([pane["status"] for pane in result["panes"]], ["failed", "failed"])
        self.assertLess(self.clock, 120.2)

    def test_exited_or_malformed_receipts_are_never_mistaken_for_success(self):
        for data in (dict(exit_code=0), {"exit_code": 127}, "partial JSON"):
            with self.subTest(data=data):
                path = worker.restore_receipt(self.directory, "%1")
                path.write_text(data if isinstance(data, str) else json.dumps(data))
                def check(program, pid):
                    if program["kind"] == "pi":
                        raise worker.Refused("Expected pi")
                result = self.verify(check)
                self.assertEqual(result["panes"][0]["status"], "failed")
                self.assertEqual(result["panes"][1]["status"], "restored")
                self.assertEqual(self.clock, 0)

    def test_offline_pi_verification_checks_identity_without_requiring_stop_eligibility(self):
        program = dict(kind="pi", installed=[1, 2], cwd="/work", argv=["pi", "--session", "/exact"],
                       session_id="exact", session_inode=[3, 4], entry_point="/pi.js",
                       installed_entry=[5, 6], installed_launcher=[7, 8])
        active = dict(kind="pi", image=[1, 2], cwd="/work")
        with patch.object(worker, "process", return_value=dict(foreground=100)), \
                patch.object(worker, "foreground", return_value=({}, active)), \
                patch.object(worker, "kind", return_value="pi"), \
                patch.object(worker, "pi_plan", return_value=program) as inspect:
            worker.check_program(program, 100)
            inspect.assert_called_once_with(active, require_idle=False)
            inspect.return_value = dict(program, session_id="different")
            with self.assertRaisesRegex(worker.Refused, "exact conversation"):
                worker.check_program(program, 100)

    def test_missing_root_process_is_distinguished_from_session_metadata_not_ready(self):
        with patch.object(worker, "process", side_effect=FileNotFoundError("/proc/100/exe")):
            with self.assertRaisesRegex(ProcessLookupError, "process exited"):
                worker.check_program(dict(kind="pi"), 100)

    def test_replacement_server_loss_is_still_a_failure_not_an_application_warning(self):
        def check(program, pid):
            worker.same_process.return_value = False
        with self.assertRaisesRegex(worker.Refused, "replacement tmux server"):
            self.verify(check)


if __name__ == "__main__":
    unittest.main()
