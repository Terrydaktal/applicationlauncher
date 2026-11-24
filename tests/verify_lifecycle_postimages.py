"""Check the actual stripped release ELFs and retain a scoped erasure receipt."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

MARKERS = (b"timing-request.json", b"lifecycle_timing", b"confirmation_wait",
           b"ActivateUpdateTimings", b"--trace-updates", b"hash_terminal_installed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--erased", action="store_true")
    parser.add_argument("--compare", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("artifacts", type=Path, nargs="+")
    arguments = parser.parse_args()
    report = dict(schema_version=1, scope="update-timings facility only; not application-wide release-minimal", erased=arguments.erased, artifacts=[])
    for path in arguments.artifacts:
        path = path.resolve()
        assert path.stat().st_size <= 64 * 1024 * 1024, "Inspect the stripped deployment artifact, not full DWARF"
        data = path.read_bytes()
        manifest = json.loads(subprocess.check_output([str(path), "--lifecycle-build-info"], timeout=5))
        assert manifest["timings"] is not arguments.erased
        found = [marker.decode() for marker in MARKERS if marker in data]
        if arguments.erased:
            assert not found, f"Optional timing markers remain in {path}: {found}"
        else:
            assert found, "The observable artifact must actually contain the timing facility"
        report["artifacts"].append(dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
            build_id=subprocess.check_output([str(path), "--build-id"], text=True, timeout=5).strip(),
            lifecycle_postimages=manifest, observed_markers=found))
    if arguments.compare:
        previous = json.loads(arguments.compare.read_text())
        for current, old in zip(report["artifacts"], previous["artifacts"], strict=True):
            for name in ("window.py", "external.py"):
                for digest in ("source_sha256", "semantic_sha256"):
                    assert current["lifecycle_postimages"]["files"][name][digest] == old["lifecycle_postimages"]["files"][name][digest]
        report["same_source_and_semantics_as"] = str(arguments.compare)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Verified {len(report['artifacts'])} stripped artifacts; receipt: {arguments.output}")


if __name__ == "__main__":
    main()
