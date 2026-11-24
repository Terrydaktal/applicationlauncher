"""Executable handover without content copies or hashes; private fixtures only."""
from contextlib import contextmanager, ExitStack
import fcntl
import hashlib
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import test_lifecycle_windows as fixtures
from test_lifecycle_windows import window, worker


@contextmanager
def forbid_image_io():
    with ExitStack() as patches:
        ioctl = fcntl.ioctl
        def no_reflink(descriptor, request, *arguments):
            if request == 0x40049409:
                raise AssertionError("Terminal update reflinked an executable")
            return ioctl(descriptor, request, *arguments)
        patches.enter_context(patch.object(fcntl, "ioctl", side_effect=no_reflink))
        for module, names in ((hashlib, ("file_digest", "sha256")),
                              (shutil, ("copyfile", "copyfileobj", "copy", "copy2")),
                              (os, ("copy_file_range", "sendfile", "link"))):
            for name in names:
                patches.enter_context(patch.object(module, name,
                    side_effect=AssertionError(f"Terminal update called {name}")))
        yield


class ExecutableHandover(unittest.TestCase):
    def test_both_routes_and_native_fallback_never_copy_or_hash(self):
        for external, hold in ((False, True), (True, False), (False, False)):
            for fault in (None, "Stage", "stop", "verify"):
                with self.subTest(external=external, hold=hold, fault=fault), forbid_image_io():
                    result, events, statuses, records = fixtures.Coordinator().run_job(
                        external=external, capabilities=hold, fault=fault)
                    self.assertEqual(result["ok"], fault is None, result)
                    plan = records["restart-plan.json"]
                    self.assertEqual(plan["image_retention"], "Open descriptors during handover only; no executable backups")
                    self.assertNotIn("recovery_argv", plan["tabs"][0])
                    self.assertNotIn("recovery_build_sha256", plan["tabs"][0])
                    if fault:
                        self.assertNotIn("Commit", events)

    def test_large_deleted_image_is_pinned_without_reading_or_copying(self):
        with tempfile.TemporaryDirectory(prefix="lifecycle-handover-") as directory:
            source = Path(directory) / "private-image"
            with source.open("wb") as stream:
                stream.write(b"\x7fELF")
                stream.truncate(9 * 1024**3)
            image = worker.file_identity(source)
            job = window.WindowUpdate({}, object(), directory)
            try:
                with forbid_image_io(), \
                        patch("builtins.open", side_effect=AssertionError("Read executable contents")), \
                        patch.object(os, "read", side_effect=AssertionError("Read executable contents")), \
                        patch.object(os, "pread", side_effect=AssertionError("Read executable contents")):
                    pinned = job.pin_image(source, image)
                    source.unlink()
                    self.assertEqual(worker.file_identity(pinned), image)
                    self.assertEqual(list(Path(directory).iterdir()), [])
            finally:
                for descriptor in job.descriptors:
                    os.close(descriptor)

    def test_scope_identity_compares_structure_without_hashing(self):
        tab = dict(program=dict(kind="fish", argv=["/proc/100/fd/5"],
            process=dict(pid=60, start=70, image=[1, 2, 3, 4])),
            record=dict(child_pid=60, active=True), active=True, cwd="/fixture")
        with forbid_image_io():
            self.assertIsInstance(window.scope_fingerprint([tab]), list)

    def test_old_hashed_preview_refuses_before_confirmation_or_stopping(self):
        with forbid_image_io():
            result, events, _, records = fixtures.Coordinator().run_job(preview_scope="a" * 64)
        self.assertFalse(result["ok"])
        self.assertIn("scope changed", result["message"])
        self.assertNotIn("AwaitConfirmation", events)
        self.assertNotIn("stopped", events)
        self.assertNotIn("restart-plan.json", records)

    def test_program_session_changes_still_refuse_without_hashing(self):
        saved = dict(pid=60, start=70, image=[1, 2, 3, 4], cwd="/fixture")
        program = dict(kind="fish", process=saved, cwd=saved["cwd"], argv=["/proc/100/fd/5"])
        job = window.WindowUpdate({}, object(), "/unused")
        job.tabs = [dict(program=program, record=dict(child_pid=60, foreground_pgid=60))]
        with patch.object(job, "remote", return_value={}), \
                patch.object(worker, "same_process", return_value=True), \
                patch.object(worker, "foreground", return_value=(saved, saved)), \
                patch.object(worker, "require_dedicated_tree"), \
                patch.object(worker, "program_plan", return_value=program) as planner, forbid_image_io():
            job.revalidate()
            planner.return_value = dict(program, argv=[program["argv"][0], "changed-session"])
            with self.assertRaisesRegex(worker.Refused, "session or restart options changed"):
                job.revalidate()


if __name__ == "__main__":
    unittest.main()
