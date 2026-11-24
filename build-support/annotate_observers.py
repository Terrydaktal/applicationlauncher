"""One-shot migration: emit an apply_patch patch for coarse Rust observer calls.

This tool does not edit files or participate in production builds. The explicit
source markers it introduces are checked by the release artifact assurance.
"""
import difflib
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {"observability.rs", "diagnostic_capture.rs", "replay.rs", "timing.rs", "lib.rs"}


def end_call(source, opening):
    depth, offset = 0, opening
    while offset < len(source):
        raw = re.match(r'r(#+)?"', source[offset:])
        if raw:
            closing = '"' + (raw[1] or "")
            offset = source.index(closing, offset + len(raw[0])) + len(closing)
            continue
        if source.startswith("//", offset):
            offset = source.index("\n", offset)
            continue
        if source[offset] == '"':
            offset += 1
            while source[offset] != '"':
                offset += 2 if source[offset] == "\\" else 1
            offset += 1
            continue
        if source[offset] == "(":
            depth += 1
        elif source[offset] == ")":
            depth -= 1
            if depth == 0:
                return offset + 1
        offset += 1
    raise ValueError("Unbalanced observer call")


def migrate(source, path):
    source = re.sub(r'(?:crate::|applicationlauncher::)?observability::spawn_named\s*\(',
                    "applicationlauncher::spawn_worker!(", source)
    source = source.replace("crate::diagnostics::write_stderr_line(", "applicationlauncher::diagnostic_line!(")
    if path.name not in {"main.rs", "applicationlauncherd.rs"}:
        source = re.sub(r'(?<![:\w])eprintln!', "applicationlauncher::diagnostic_log!", source)
    pattern = re.compile(r'(?m)^[ \t]*(?P<call>(?:(?:applicationlauncher::|crate::)?observability::(?:increment|add|set_gauge|record_boundary|record_decision|record_search_decision|record|sample_gui_frame|initialize|install_panic_hook)|worker\.set_state)\s*\()')
    edits = []
    for match in pattern.finditer(source):
        begin = match.start("call")
        end = end_call(source, match.end("call") - 1)
        if source[end:end+1] == ";":
            end += 1
        edits.append((begin, end, "applicationlauncher::observe! { " + source[begin:end] + " }"))
    for begin, end, replacement in reversed(edits):
        source = source[:begin] + replacement + source[end:]
    # The binding must stay in the worker's scope when enabled.
    lines = source.splitlines(keepends=True)
    for index, line in enumerate(lines):
        if "observability::register_worker(" in line:
            start = index
            while "let " not in lines[start]:
                start -= 1
            indent = lines[start][:len(lines[start]) - len(lines[start].lstrip())]
            lines[start] = indent + '#[cfg(feature = "observability")]\n' + lines[start]
    return "".join(lines)


def main():
    print("*** Begin Patch")
    for path in sorted((ROOT / "src").rglob("*.rs")):
        if path.name in EXCLUDED or "tests" in path.name:
            continue
        before = path.read_text()
        after = migrate(before, path)
        if before != after:
            print(f"*** Update File: {path}")
            diff = list(difflib.unified_diff(before.splitlines(), after.splitlines(), n=3))
            for line in diff[2:]:
                print("@@" if line.startswith("@@") else line)
    print("*** End Patch")


if __name__ == "__main__":
    main()
