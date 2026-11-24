"""Inspect real linked postimages, not only feature settings or source annotations."""
import argparse
import ast
import dis
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

FORBIDDEN = (b"ALTRC001", b"APPLICATIONLAUNCHER_DIAGNOSTICS", b"flight_recorder_drops",
             b"trace-writer-", b"ActivateUpdateTimings", b"lifecycle_timing",
             b"hash_terminal_installed", b"record_search_decision", b"MAX_LOG_LEVEL_FILTER",
             b"APPLICATIONLAUNCHER_ALLOW_FAULT_INJECTION", b"intentional diagnostic panic probe")
SYMBOLS = re.compile(r"\b(?:applicationlauncher::(?:observability|diagnostic_capture|replay)|tracing|tracing_core|log)::")
ROOT = Path(__file__).resolve().parents[1]


def python_postimages(binary, data, minimal):
    manifest = json.loads(subprocess.check_output([str(binary), "--lifecycle-build-info"], timeout=5))
    rows = []
    for name, item in manifest["files"].items():
        candidates = (ROOT / "target/profile-build/release/build").glob("applicationlauncher-*/out/lifecycle/" + name)
        source = next((path.read_bytes() for path in candidates if hashlib.sha256(path.read_bytes()).hexdigest() == item["postimage_sha256"]), None)
        embedded = source is not None and source in data
        if source is None or (binary.name == "applicationlauncherd" and not embedded):
            raise ValueError(f"Cannot bind the generated {name} postimage to the actual ELF")
        tree = ast.parse(source)
        imports = [alias.name for node in ast.walk(tree) if isinstance(node, (ast.Import, ast.ImportFrom)) for alias in node.names]
        code = compile(tree, name, "exec")
        def names(value):
            result = set(value.co_names)
            for child in value.co_consts:
                if isinstance(child, type(value)):
                    result.update(names(child))
            return result
        referenced = names(code)
        forbidden = referenced.intersection({"update_timing", "timings", "lifecycle_timing", "traceback"})
        if minimal and (forbidden or "traceback" in imports):
            raise ValueError(f"Observer bytecode remains in {name}: {forbidden}")
        rows.append(dict(path=name, **item, embedded=embedded, imports=imports, observer_names=sorted(forbidden),
                         bytecode_sha256=hashlib.sha256(dis.Bytecode(code).dis().encode()).hexdigest()))
    return rows


def dependencies():
    workspace = ROOT / "target/release-minimal-source/Cargo.toml"
    base = ["cargo", "metadata", "--offline", "--locked", "--format-version", "1"]
    original = json.loads(subprocess.check_output(base, cwd=ROOT, timeout=30))
    generated = json.loads(subprocess.check_output(base + ["--manifest-path", str(workspace), "--no-default-features", "--features", "release-minimal"], cwd=ROOT, timeout=30))
    optional = {"log", "tracing", "tracing-core", "tracing-attributes", "valuable", "erased-tracing-attributes"}
    def product(graph):
        return {(row["name"], row["version"], row.get("source")) for row in graph["packages"] if row["name"] not in optional}
    if product(original) != product(generated):
        raise ValueError("The minimal workspace changed a non-observer dependency")
    for row in generated["packages"]:
        if row["name"] in {"log", "tracing", "tracing-attributes", "erased-tracing-attributes"} and not Path(row["manifest_path"]).is_relative_to(ROOT / "build-support"):
            raise ValueError("Minimal compilation did not use the declared token-erasure adapter")
    if any(row["name"] == "tracing-core" for row in generated["packages"]):
        raise ValueError("The tracing runtime is still in the minimal dependency graph")
    lock = workspace.with_name("Cargo.lock")
    return dict(product_dependency_count=len(product(generated)), generated_lock_sha256=hashlib.sha256(lock.read_bytes()).hexdigest(),
                packages=[dict(name=row["name"], version=row["version"], source=row.get("source")) for row in generated["packages"]])


def inspect(path, profile):
    path = path.resolve()
    identity = json.loads(subprocess.check_output([str(path), "--build-info"], timeout=5))
    if identity["artifact_profile"] != profile:
        raise ValueError(f"Incorrect artifact profile in {path}")
    full_symbols = subprocess.check_output(["nm", "--defined-only", "-C", str(path)], text=True, timeout=30)
    dependencies = subprocess.check_output(["readelf", "-dW", str(path)], text=True, timeout=10)
    notes = subprocess.check_output(["readelf", "-n", str(path)], text=True, timeout=10)
    build_id = re.search(r"Build ID: ([a-f0-9]+)", notes).group(1)
    with tempfile.TemporaryDirectory(prefix="release-postimage-") as temporary:
        stripped = Path(temporary) / "artifact"
        subprocess.run(["objcopy", "--strip-debug", str(path), str(stripped)], check=True, timeout=30)
        data = stripped.read_bytes()
    if len(data) > 64 * 1024 * 1024:
        raise ValueError("Stripped deployment artifact exceeds 64 MiB")
    markers = [marker.decode() for marker in FORBIDDEN if marker in data]
    residual_symbols = [line for line in full_symbols.splitlines() if SYMBOLS.search(line)]
    if profile == "release-minimal" and (markers or residual_symbols):
        raise ValueError(f"Residual optional diagnostics in {path}: markers={markers}, symbols={residual_symbols[:8]}")
    if profile != "release-minimal" and not any("applicationlauncher::observability::" in line for line in residual_symbols):
        raise ValueError("Observable artifact does not actually contain its diagnostic implementation")
    if profile != "diagnostic-build" and b"APPLICATIONLAUNCHER_ALLOW_FAULT_INJECTION" in data:
        raise ValueError("Fault injection leaked into an ordinary release")
    if b"--trace-updates" in data:
        raise ValueError("The removed tracing flag remains in the artifact")
    postimages = python_postimages(path, data, profile == "release-minimal")
    return dict(path=str(path), identity=identity, elf_build_id=build_id, python_postimages=postimages,
                sha256=hashlib.sha256(path.read_bytes()).hexdigest(), stripped_sha256=hashlib.sha256(data).hexdigest(),
                stripped_bytes=len(data), optional_markers=markers, optional_symbol_count=len(residual_symbols),
                dynamic_dependencies=[line.strip() for line in dependencies.splitlines() if "NEEDED" in line],
                analyzer=subprocess.check_output(["objcopy", "--version"], text=True).splitlines()[0])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("release-minimal", "release-observable", "diagnostic-build"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("artifacts", type=Path, nargs="+")
    arguments = parser.parse_args()
    rows = [inspect(path, arguments.profile) for path in arguments.artifacts]
    if len({row["identity"]["product_semantic_sha256"] for row in rows}) != 1:
        raise ValueError("The GUI and daemon do not share one canonical product source")
    arguments.output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    graph = dependencies() if arguments.profile == "release-minimal" else None
    arguments.output.write_text(json.dumps(dict(schema_version=1, profile=arguments.profile, artifacts=rows, dependency_proof=graph), indent=2) + "\n")
    print(f"Verified {arguments.profile}: {len(rows)} real ELF postimages")


if __name__ == "__main__":
    main()
