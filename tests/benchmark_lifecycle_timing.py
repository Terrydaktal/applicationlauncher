"""Private scalar-boundary benchmark; does not connect to a desktop or daemon."""
import gc
import importlib.util
import json
from pathlib import Path
import resource
import statistics
import sys
import tempfile
import time
import tracemalloc

from test_lifecycle_windows import timing, window


SOURCE = """class Boundary:
    def step(self, value):
        with self.timings.span('revalidate'):
            return value + 1
"""
spec = importlib.util.spec_from_file_location("timing_generator", Path(__file__).resolve().parents[1] / "build-support/lifecycle_sources.py")
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


def implementation(enabled, trace):
    source, semantic = generator.generate(SOURCE, enabled)
    namespace = {}
    exec(source, namespace)
    result = namespace["Boundary"]()
    result.timings = trace
    return result, semantic


def summary(samples):
    ordered = sorted(samples)
    return dict(samples=len(samples), median_ns=statistics.median(ordered),
                p95_ns=ordered[int((len(ordered) - 1) * .95)], p99_ns=ordered[int((len(ordered) - 1) * .99)], max_ns=max(ordered))


def main():
    report = {"schema_version": 1, "scope": "scalar lifecycle boundary, not GUI throughput or an actual update", "runs": {}}
    semantic = None
    for enabled, label in [(False, "timing_erased"), (True, "runtime_disabled")]:
        boundary, digest = implementation(enabled, timing.DISABLED)
        semantic = digest if semantic is None else semantic
        assert digest == semantic
        for _ in range(1000):
            boundary.step(1)
        samples = []
        cpu = time.process_time_ns()
        for _ in range(31):
            started = time.perf_counter_ns()
            for _ in range(2000):
                assert boundary.step(1) == 2
            samples.append((time.perf_counter_ns() - started) / 2000)
        report["runs"][label] = dict(summary(samples), cpu_ns=time.process_time_ns() - cpu)
    active_samples, initialization = [], []
    tracemalloc.start()
    with tempfile.TemporaryDirectory(prefix="lifecycle-timing-benchmark-") as root:
        for index in range(20):
            job = Path(root) / f"job-{index}"
            job.mkdir(mode=0o700)
            window.atomic_record(job / "timing-request.json", dict(schema_version=1,
                deadline_unix_ms=time.time_ns() // 1_000_000 + 120000, build_id="benchmark"))
            started = time.perf_counter_ns()
            trace = timing.from_directory(job)
            initialization.append(time.perf_counter_ns() - started)
            assert isinstance(trace, timing.Trace)
            boundary, _ = implementation(True, trace)
            for _ in range(32):
                started = time.perf_counter_ns()
                assert boundary.step(1) == 2
                active_samples.append(time.perf_counter_ns() - started)
            trace.close()
            assert trace.path.stat().st_size <= timing.MAX_BYTES
        report["retained_bytes"] = sum(path.stat().st_size for path in (Path(root) / "timings").glob("*.jsonl"))
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    gc.collect()
    timing.current = timing.DISABLED
    report["runs"]["runtime_activated"] = summary(active_samples)
    report["runs"]["activation_initialization"] = summary(initialization)
    report["activated_peak_python_bytes"] = peak
    report["process_peak_rss_kib"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    report["semantic_sha256"] = semantic
    report["budgets"] = dict(disabled_boundary_p99_ns=2000, activated_boundary_p99_ns=1000000,
                             activation_initialization_p99_ns=5000000, activated_python_peak_bytes=1048576)
    assert report["runs"]["runtime_disabled"]["p99_ns"] < report["budgets"]["disabled_boundary_p99_ns"]
    assert report["runs"]["runtime_activated"]["p99_ns"] < report["budgets"]["activated_boundary_p99_ns"]
    assert report["runs"]["activation_initialization"]["p99_ns"] < report["budgets"]["activation_initialization_p99_ns"]
    assert peak < report["budgets"]["activated_python_peak_bytes"]
    output = json.dumps(report, indent=2) + "\n"
    if len(sys.argv) == 2:
        path = Path(sys.argv[1])
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(output)
    print(output, end="")


if __name__ == "__main__":
    main()
