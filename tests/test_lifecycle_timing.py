"""Timing fidelity, bounded cost, privacy, erasure and product equivalence."""
import ast
import builtins
import dis
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import test_lifecycle_windows as fixtures
from test_lifecycle_windows import SOURCE, timing, window, worker

spec = importlib.util.spec_from_file_location("lifecycle_sources", SOURCE.parents[1] / "build-support/lifecycle_sources.py")
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class Timings(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="update-timing-test-")
        self.addCleanup(self.root.cleanup)
        self.job = Path(self.root.name) / "job"
        self.job.mkdir(mode=0o700)
        timing.current = timing.DISABLED
        self.addCleanup(setattr, timing, "current", timing.DISABLED)

    def trace(self):
        window.atomic_record(self.job / "timing-request.json", dict(schema_version=1,
            deadline_unix_ms=time.time_ns() // 1_000_000 + 120000, build_id="private-test-build"))
        result = timing.from_directory(self.job)
        self.assertIsInstance(result, timing.Trace)
        self.addCleanup(result.close)
        return result

    def rows(self, trace):
        return [json.loads(line) for line in trace.path.read_text().splitlines()]

    def test_disabled_has_no_clocks_output_buffers_or_helper_threads(self):
        with patch.object(timing.time, "monotonic_ns", side_effect=AssertionError("disabled clock")), \
                patch.object(timing.time, "process_time_ns", side_effect=AssertionError("disabled CPU clock")), \
                patch.object(timing.time, "time_ns", side_effect=AssertionError("disabled wall clock")):
            trace = timing.from_directory(self.job)
            for _ in range(100):
                with trace.span("restart_plan"):
                    pass
            trace.close()
        self.assertIs(trace, timing.DISABLED)
        self.assertFalse((self.job.parent / "timings").exists())

    def test_confirmation_and_nested_capture_have_separate_wall_and_cpu_times(self):
        trace = self.trace()
        clock, cpu = [trace.started + 1000], [1000]
        with patch.object(timing.time, "monotonic_ns", side_effect=lambda: clock[0]), \
                patch.object(timing.time, "process_time_ns", side_effect=lambda: cpu[0]):
            with trace.span("confirmation_wait"):
                clock[0] += 2_000_000_000
            with trace.span("capture"):
                with trace.span("capture_terminal", route="compatibility"):
                    clock[0] += 75_000_000
                    cpu[0] += 70_000_000
        rows = self.rows(trace)
        ends = {row["stage"]: row for row in rows if row["event"] == "end"}
        self.assertEqual(ends["confirmation_wait"]["duration_ns"], 2_000_000_000)
        self.assertEqual(ends["confirmation_wait"]["cpu_ns"], 0)
        self.assertEqual(ends["capture_terminal"]["duration_ns"], 75_000_000)
        self.assertEqual(ends["capture_terminal"]["cpu_ns"], 70_000_000)
        self.assertEqual(ends["capture_terminal"]["parent_span_id"], ends["capture"]["span_id"])
        self.assertEqual([row["sequence"] for row in rows], list(range(len(rows))))
        self.assertTrue(all(row["operation_id"] == "job" and row["pid"] == os.getpid() for row in rows))

    def test_errors_preserve_the_original_exception_without_capturing_payload(self):
        trace = self.trace()
        original = worker.Refused("SECRET_PROMPT and /private/session.jsonl")
        with self.assertRaises(worker.Refused) as raised:
            with trace.span("stage_replacement", command="SECRET_ARGUMENT", path="/private/file"):
                raise original
        self.assertIs(raised.exception, original)
        last = self.rows(trace)[-1]
        self.assertEqual((last["outcome"], last["error_type"]), ("error", "Refused"))
        self.assertNotIn("SECRET", trace.path.read_text())
        self.assertNotIn("/private", trace.path.read_text())
        self.assertEqual(trace.path.stat().st_mode & 0o777, 0o600)

    def test_event_output_depth_and_expiry_are_bounded_and_report_truncation(self):
        trace = self.trace()
        for _ in range(timing.MAX_EVENTS * 2):
            with trace.span("revalidate"):
                pass
        trace.close()
        rows = self.rows(trace)
        self.assertLessEqual(len(rows), timing.MAX_EVENTS)
        self.assertLessEqual(trace.path.stat().st_size, timing.MAX_BYTES)
        self.assertTrue(any(row["event"] == "truncated" for row in rows))
        self.assertGreater(trace.dropped, 0)
        self.assertEqual(trace.state, "truncated")

    def test_activation_expires_even_during_confirmation(self):
        trace = self.trace()
        with trace.span("confirmation_wait"):
            with patch.object(timing.time, "monotonic_ns", return_value=trace.deadline + 1):
                self.assertIs(trace.span("restart_plan"), timing.DISABLED)
        self.assertEqual(trace.state, "expired")
        self.assertTrue(any(row["event"] == "expired" for row in self.rows(trace)))

    def test_optional_sink_failure_never_blocks_or_changes_product_work(self):
        trace = self.trace()
        with patch.object(timing.os, "write", side_effect=OSError(28, "SECRET_DISK_ERROR")):
            with trace.span("restart_plan"):
                result = "product completed"
        self.assertEqual(result, "product completed")
        self.assertEqual(trace.status()["state"], "unavailable")
        self.assertEqual(trace.status()["first_error"]["errno"], 28)
        self.assertNotIn("SECRET", trace.path.read_text())
        trace.close()
        self.assertIsNone(trace.descriptor)

    def test_restart_records_metadata_without_copy_or_hash_stages(self):
        from test_lifecycle_recovery_builds import forbid_image_io
        with forbid_image_io():
            result, _, _, records = fixtures.Coordinator().run_job(timings=True)
        self.assertTrue(result["ok"], result)
        stages = {row.get("stage") for row in records["timing-events"]}
        self.assertIn("restart_plan", stages)
        self.assertFalse(stages & {"recovery_builds", "hash_source", "hash_cache", "copy_image", "cache_lookup"})

    def test_fallback_capture_has_no_terminal_hash_stages(self):
        import test_lifecycle_external as external_fixtures
        trace = self.trace()
        external_fixtures.Mapping().test_missing_updater_and_tab_apis_use_only_an_unambiguous_proc_session()
        rows = [row for row in self.rows(trace) if row["event"] == "end"]
        self.assertEqual({row["stage"] for row in rows}, {"capture_terminal"})

    def test_unsafe_malformed_and_expired_leases_do_not_enable_capture(self):
        lease = self.job / "timing-request.json"
        for contents in ("invalid", "x" * 5000, json.dumps(dict(schema_version=1, deadline_unix_ms=0, build_id="test"))):
            window.atomic_record(lease, {})
            lease.write_text(contents)
            self.assertNotIsInstance(timing.from_directory(self.job), timing.Trace)
        lease.chmod(0o644)
        self.assertEqual(timing.from_directory(self.job).status()["state"], "unavailable")
        lease.unlink()
        outside = self.job / "outside"
        outside.write_text("do not read")
        lease.symlink_to(outside)
        self.assertEqual(timing.from_directory(self.job).status()["state"], "unavailable")
        self.assertFalse((self.job.parent / "timings").exists())

    def test_retention_is_bounded_without_removing_active_traces_or_recovery_plans(self):
        active = self.trace()
        recovery = self.job / "restart-plan.json"
        recovery.write_text("must remain")
        with patch.object(timing, "MAX_FILES", 3):
            for index in range(8):
                job = self.job.parent / f"job-{index}"
                job.mkdir(mode=0o700)
                window.atomic_record(job / "timing-request.json", dict(schema_version=1,
                    deadline_unix_ms=time.time_ns() // 1_000_000 + 120000, build_id="test"))
                trace = timing.from_directory(job)
                self.assertIsInstance(trace, timing.Trace)
                trace.close()
                self.assertLessEqual(len(list(active.path.parent.glob("*.jsonl"))), 3)
        self.assertTrue(active.path.is_file())
        self.assertEqual(recovery.read_text(), "must remain")

    def test_concurrent_trace_allocation_declines_without_waiting_or_exceeding_retention(self):
        active = self.trace()
        job = self.job.parent / "other-job"
        job.mkdir(mode=0o700)
        window.atomic_record(job / "timing-request.json", dict(schema_version=1,
            deadline_unix_ms=time.time_ns() // 1_000_000 + 120000, build_id="test"))
        with patch.object(timing, "MAX_FILES", 1):
            with open(active.path.parent / ".retention.lock", "rb") as lock:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                contended = timing.from_directory(job)
                self.assertEqual(contended.state, "unavailable")
                self.assertIn(contended.fault["errno"], {11, 35})
                self.assertFalse((active.path.parent / "other-job.jsonl").exists())
            full = timing.from_directory(job)
            self.assertEqual(full.state, "unavailable")
            active.close()
            allocated = timing.from_directory(job)
            self.assertIsInstance(allocated, timing.Trace)
            allocated.close()
            self.assertEqual(len(list(active.path.parent.glob("*.jsonl"))), 1)

    def test_a_process_exit_leaves_the_last_started_stage_readable(self):
        trace = self.trace()
        trace.close()
        trace.path.unlink()
        code = f"import sys,os;sys.path.insert(0,{str(SOURCE)!r});import timing;t=timing.from_directory({str(self.job)!r});s=t.span('restart_plan');s.__enter__();os._exit(0)"
        subprocess.run([sys.executable, "-I", "-c", code], check=True, timeout=5)
        rows = self.rows(trace)
        self.assertEqual((rows[-1]["event"], rows[-1]["stage"]), ("begin", "restart_plan"))
        self.assertNotEqual(rows[-1]["pid"], os.getpid())

    def test_real_coordinator_records_distinct_success_and_failure_stages(self):
        for fault in (None, "Stage", "stop"):
            result, events, statuses, records = fixtures.Coordinator().run_job(fault=fault, timings=True)
            rows = records["timing-events"]
            ends = [row for row in rows if row["event"] == "end"]
            stages = {row["stage"] for row in ends}
            self.assertTrue({"capture", "tab_plan", "confirmation_wait", "revalidate", "restart_plan", "stage_replacement"} <= stages)
            self.assertEqual(result["ok"], fault is None)
            self.assertTrue(any(row["outcome"] == "error" for row in ends) if fault else all(row["outcome"] == "ok" for row in ends))
            self.assertEqual(rows[-1]["event"], "finished")


class Erasure(unittest.TestCase):
    def test_minimal_runner_preserves_recovery_records_and_erases_only_optional_stacks(self):
        source = (SOURCE / "runner.py").read_text()
        minimal, digest = generator.generate(source, False, minimal=True)
        observable, observed_digest = generator.generate(source, True)
        self.assertEqual(digest, observed_digest)
        self.assertNotIn("traceback", minimal)
        self.assertIn("traceback.format_exc", observable)
        self.assertIn("atomic_record", minimal)
        self.assertIn(".error.json", minimal)
        self.assertIn("failure_details(error)", minimal)
        self.assertIn("result.json", minimal)

    def test_first_typed_cause_and_errno_survive_without_recording_exception_payloads(self):
        original = PermissionError(13, "SECRET_DATA")
        refused = worker.Refused("Cannot inspect this exact process")
        refused.__cause__ = original
        details = worker.failure_details(refused)
        self.assertEqual(details["code"], "policy-refused")
        self.assertEqual(details["causes"], [dict(kind="Refused", errno=None), dict(kind="PermissionError", errno=13)])
        self.assertNotIn("SECRET_DATA", json.dumps(details))
        self.assertEqual(worker.failure_details(TimeoutError())["code"], "timeout")
        self.assertEqual(worker.failure_details(ValueError())["code"], "invalid-input")

    def test_postimages_and_bytecode_have_no_timing_imports_calls_strings_or_checks(self):
        for name in ("window.py", "external.py"):
            source = (SOURCE / name).read_text()
            generated, semantic = generator.generate(source, False)
            observed, observed_semantic = generator.generate(source, True)
            self.assertEqual(semantic, observed_semantic)
            self.assertEqual(observed, source)
            tree = ast.parse(generated)
            self.assertFalse(generator.observer(tree))
            for marker in ("lifecycle_timing", "hash_source", "confirmation_wait", "trace_role"):
                self.assertNotIn(marker, generated)
            def check(code):
                for instruction in dis.get_instructions(code):
                    self.assertNotIn(instruction.argval if isinstance(instruction.argval, str) else "", {"timings", "update_timing", "lifecycle_timing"})
                for constant in code.co_consts:
                    if isinstance(constant, type(code)):
                        check(constant)
            code = compile(generated, name, "exec")
            check(code)
            original_import = builtins.__import__
            def no_timing(module, *args, **kwargs):
                if module == "lifecycle_timing":
                    raise AssertionError("Erased worker imports optional timings")
                return original_import(module, *args, **kwargs)
            with patch("builtins.__import__", side_effect=no_timing):
                exec(code, {"__name__": "erased_worker"})

    def test_erased_and_observable_coordinators_have_identical_product_results(self):
        from test_lifecycle_recovery_builds import forbid_image_io
        code, _ = generator.generate((SOURCE / "window.py").read_text(), False)
        namespace = {"__name__": "erased_window"}
        exec(code, namespace)
        for external, hold in ((False, True), (True, False), (False, False)):
            for fault in (None, "Stage", "stop", "verify"):
                with self.subTest(external=external, hold=hold, fault=fault), forbid_image_io():
                    options = dict(fault=fault, external=external, capabilities=hold)
                    observed = fixtures.Coordinator().run_job(**options)
                    with patch.object(window, "WindowUpdate", namespace["WindowUpdate"]):
                        erased = fixtures.Coordinator().run_job(**options)
                    self.assertEqual(observed[0], erased[0])
                    self.assertEqual(observed[1], erased[1])
                    self.assertEqual([row["phase"] for row in observed[2]], [row["phase"] for row in erased[2]])


if __name__ == "__main__":
    unittest.main()
