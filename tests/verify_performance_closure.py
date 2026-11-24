"""Compare designated optimized product loops, allowing only link-address relocation."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

FUNCTIONS = ("applicationlauncher::search::rank_prepared_documents",
             "applicationlauncher::search::search_projection_with_map",
             "applicationlauncher::freshness::same_native_build")


def instructions(binary):
    symbols = subprocess.check_output(["nm", "--defined-only", "-S", "-C", str(binary)], text=True, timeout=30)
    result = {}
    for function in FUNCTIONS:
        matches = [line.split(maxsplit=3) for line in symbols.splitlines() if line.endswith(" " + function)]
        if len(matches) != 1:
            raise ValueError(f"Expected one optimized {function} in {binary}")
        address, size, _, _ = matches[0]
        start, end = int(address, 16), int(address, 16) + int(size, 16)
        raw = subprocess.check_output(["objdump", "-d", "-C", "--no-show-raw-insn", "-M", "intel",
                                      f"--start-address={start}", f"--stop-address={end}", str(binary)], text=True, timeout=30)
        rows = []
        for line in raw.splitlines():
            match = re.match(r"\s*([a-f0-9]+):\s+(.*)", line)
            if not match:
                continue
            text = match[2]
            # Preserve opcodes, registers, constants, stack offsets and relative
            # control flow. Only ELF layout-dependent addresses are normalized.
            def target(value):
                location = int(value[1], 16)
                label = f"self+{location - start:x}" if start <= location < end else re.sub(r"::h[a-f0-9]{16}\b", "", value[2])
                return "<" + label + ">"
            text = re.sub(r"\b([a-f0-9]+) <([^>]+)>", target, text)
            text = re.sub(r"\[rip[+-]0x[a-f0-9]+\]", "[rip+relocation]", text)
            text = text.split("#", 1)[0].strip()
            text = re.sub(r"\s+", " ", text)
            rows.append(text)
        if not rows:
            raise ValueError("The selected performance closure is empty")
        result[function] = dict(instructions=rows, sha256=hashlib.sha256("\n".join(rows).encode()).hexdigest())
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("binaries", type=Path, nargs="+")
    arguments = parser.parse_args()
    rows = [dict(path=str(path), functions=instructions(path)) for path in arguments.binaries]
    for function in FUNCTIONS:
        if len({row["functions"][function]["sha256"] for row in rows}) != 1:
            arguments.output.write_text(json.dumps(dict(status="failed", artifacts=rows), indent=2) + "\n")
            raise ValueError(f"Optimized product instructions changed across profiles: {function}; inspect {arguments.output}")
    arguments.output.write_text(json.dumps(dict(schema_version=1, status="passed", normalization="ELF relocation addresses only", artifacts=rows), indent=2) + "\n")


if __name__ == "__main__":
    main()
