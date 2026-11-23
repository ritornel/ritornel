#!/usr/bin/env bash
# Checks that a test never sleeps to let something happen. Every
# `thread::sleep` or `time::sleep` in test code needs a comment beginning
# `sleep:`, on its line or in the comment right above it, that says why no
# event will do: a pause between looks at a condition that has a deadline,
# or time given for something that must not happen. See "Writing tests that
# don't depend on timing" in AGENTS.md.
#
# Test code is any file under a `tests/` folder, a file named `tests.rs`,
# `*_tests.rs` or `testing.rs`, and in any other file the item a `cfg`
# naming `test` stands on, such as `#[cfg(test)] mod tests { .. }`, to its
# closing brace.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
import pathlib
import re
import subprocess
import sys

# A call to sleep, written in full or brought in by `use`.
SLEEP = re.compile(r"(?<![\w.])(?:[\w]+::)*sleep(?:_until)?\s*\(")
TEST_CFG = re.compile(r"^\s*#\[cfg\(.*\btest\b.*\)\]")
REASON = re.compile(r"//\s*sleep:\s*\S")


def is_test_file(path: pathlib.Path) -> bool:
    return (
        "tests" in path.parts[:-1]
        or path.name in {"tests.rs", "testing.rs"}
        or path.name.endswith("_tests.rs")
    )


def code_of(line: str) -> str:
    """The line without its comment and with its strings emptied."""
    code, quoted, escaped = [], False, False
    index = 0
    while index < len(line):
        char = line[index]
        if quoted:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quoted = False
                code.append(char)
        elif char == '"':
            quoted = True
            code.append(char)
        elif line.startswith("//", index):
            break
        else:
            code.append(char)
        index += 1
    return "".join(code)


def test_lines(lines: list[str]) -> set[int]:
    """The lines inside an item that a `cfg` naming `test` stands on."""
    inside = set()
    index = 0
    while index < len(lines):
        if not TEST_CFG.match(lines[index]):
            index += 1
            continue
        # Past the item's other attributes and comments, to its first line,
        # then to where its braces close, or to its `;`.
        start = index
        index += 1
        while index < len(lines) and re.match(r"^\s*(#\[|//|$)", lines[index]):
            index += 1
        depth, opened = 0, False
        while index < len(lines):
            code = code_of(lines[index])
            depth += code.count("{") - code.count("}")
            opened |= "{" in code
            if (opened and depth <= 0) or (not opened and code.rstrip().endswith(";")):
                break
            index += 1
        inside.update(range(start, min(index + 1, len(lines))))
        index += 1
    return inside


files = subprocess.check_output(
    ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.rs"],
    text=True,
).split("\0")
errors = []
for name in filter(None, files):
    path = pathlib.Path(name)
    if not path.is_file():
        continue
    lines = path.read_text(encoding="utf-8").splitlines()
    tests = range(len(lines)) if is_test_file(path) else test_lines(lines)
    for index in sorted(tests):
        line = lines[index]
        if not SLEEP.search(code_of(line)):
            continue
        # A reason on the line itself, or in the comment right above it.
        said = [line]
        above = index - 1
        while above >= 0 and lines[above].strip().startswith("//"):
            said.append(lines[above])
            above -= 1
        if not any(REASON.search(text) for text in said):
            errors.append(f"{path}:{index + 1}: {line.strip()}")

if errors:
    print("A sleep in a test needs a `// sleep:` comment saying why no event will do:")
    print("\n".join(f"  {error}" for error in errors))
    sys.exit(1)
PY
