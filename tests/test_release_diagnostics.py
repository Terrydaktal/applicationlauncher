"""External capture does not depend on instrumentation or mutate the target."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("external_capture", ROOT / "scripts/capture-release-diagnostics.py")
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


class ExternalCapture(unittest.TestCase):
    def test_passive_capture_is_private_identity_bound_and_leaves_the_target_running(self):
        with tempfile.TemporaryDirectory() as temporary:
            child = subprocess.Popen(["/usr/bin/sleep", "30"], env=dict(os.environ, SECRET_FOR_TEST="must-never-be-captured"))
            try:
                output = Path(temporary) / "capture"
                report = capture.capture(output, pid=child.pid)
                self.assertTrue(report["identity_unchanged"])
                self.assertEqual(report["process"]["pid"], child.pid)
                self.assertIsNone(child.poll())
                self.assertEqual(output.stat().st_mode & 0o777, 0o700)
                self.assertTrue((output / "threads.json").is_file())
                for path in output.iterdir():
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                    self.assertNotIn(b"must-never-be-captured", path.read_bytes())
                subprocess.run(["sha256sum", "--check", "--status", "manifest.sha256"], cwd=output, check=True)
            finally:
                child.terminate()
                child.wait(timeout=3)

    def test_missing_or_exited_process_retains_a_machine_readable_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "capture"
            report = capture.capture(output, pid=999999999)
            self.assertFalse(report["identity_unchanged"])
            self.assertEqual(report["errors"][0]["stage"], "initial-identity")
            self.assertTrue((output / "capture.json").exists())
            self.assertTrue((output / "manifest.sha256").exists())

    def test_external_tools_have_hard_time_and_output_bounds(self):
        with patch.object(capture, "MAX_COMMAND_BYTES", 4096):
            status, data = capture.bounded_command([sys.executable, "-I", "-c", "print('x' * 100000)"], timeout=3)
            self.assertTrue(status["truncated"])
            self.assertEqual(len(data), 4096)
        status, _ = capture.bounded_command([sys.executable, "-I", "-c", "import time; time.sleep(30)"], timeout=.05)
        self.assertTrue(status["timed_out"])
        self.assertLess(status["seconds"], 3)

    def test_capture_never_reuses_an_existing_or_symlinked_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "existing"
            output.mkdir()
            with self.assertRaises(FileExistsError):
                capture.capture(output, pid=os.getpid())
            self.assertEqual(list(output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
