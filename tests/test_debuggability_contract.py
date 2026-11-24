"""Repository-local contract invariants; the skill's strict schema validator is separate."""
import importlib.util
from pathlib import Path
import re
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
STATUSES = {"implemented_tested", "implemented_untested", "planned", "absent", "not_applicable", "waived"}


class DebuggabilityContract(unittest.TestCase):
    def test_contract_is_current_and_has_no_dangling_provenance(self):
        data = tomllib.loads((ROOT / "debuggability.toml").read_text())
        self.assertEqual(data["schema_version"], 6)
        self.assertFalse(data["template"])
        self.assertEqual(data["profile"], "stateful")
        self.assertGreaterEqual(len(data["controls"]), 60)
        for name, control in data["controls"].items():
            with self.subTest(control=name):
                self.assertIn(control["status"], STATUSES)
                self.assertTrue(control["reason"].strip())
                self.assertNotIn("REPLACE_ME", str(control))
                if control["status"] not in {"not_applicable", "waived"}:
                    self.assertTrue(control["implementation"] and control["test"])
                    for path in re.findall(r"\b(?:src|scripts|build-support|tests)/[\w./-]+", str(control)):
                        self.assertTrue((ROOT / path.rstrip(".")).is_file(), path)
        self.assertEqual(data["artifact_isolation"]["release_minimal_runtime_diagnostic_checks"], 0)
        self.assertEqual(data["artifact_isolation"]["single_semantic_source"], True)
        self.assertFalse(data["instrumentation_policy"]["global_contended_event_index"])

    def test_transitive_erasure_adapters_match_the_pinned_dependency_versions(self):
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        versions = {item["name"]: item["version"] for item in lock["package"]}
        for name in ("log", "tracing"):
            adapter = tomllib.loads((ROOT / f"build-support/erased-{name}/Cargo.toml").read_text())
            self.assertEqual(adapter["package"]["version"], versions[name])
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())
        self.assertIn("observability", cargo["features"]["default"])
        self.assertNotIn("release-minimal", cargo["features"]["default"])

    def test_default_publication_follows_the_real_postimage_and_diagnosis_gates(self):
        matrix = (ROOT / "build-support/release_matrix.py").read_text()
        for gate in ("verify_release_artifact.py", "verify_release_diagnosis.py", "verify_performance_closure.py"):
            self.assertLess(matrix.index(gate), matrix.index('shutil.copyfile(output / "release-observable"'))
        wrapper = (ROOT / "scripts/lib/release-compatibility.sh").read_text()
        self.assertIn("build-release-matrix", wrapper)
        self.assertNotIn("cargo build", wrapper)
        for name in ("src/main.rs", "scripts/applicationlauncher"):
            self.assertNotIn("--trace-updates", (ROOT / name).read_text())

    def test_build_gate_logs_are_bounded_and_failures_are_preserved(self):
        spec = importlib.util.spec_from_file_location("release_matrix_contract", ROOT / "build-support/release_matrix.py")
        matrix = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(matrix)
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "gate.log"
            matrix.run([sys.executable, "-c", "import sys; sys.stdout.write('x'*2000000)"], log)
            self.assertEqual(log.stat().st_size, 1048576)
            with self.assertRaisesRegex(RuntimeError, "inspect"):
                matrix.run([sys.executable, "-c", "print('original gate failure'); raise SystemExit(7)"], log)
            self.assertLessEqual(log.stat().st_size, 1048576)
            self.assertIn("original gate failure", log.read_text())


if __name__ == "__main__":
    unittest.main()
