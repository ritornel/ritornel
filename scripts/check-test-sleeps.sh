#!/usr/bin/env bash
# Checks that a test never sleeps to let something happen. Every
# `thread::sleep` or `time::sleep` in test code needs a comment beginning
# `sleep:`, on its line or in the comment right above it, that says why no
# event will do: a pause between looks at a condition that has a deadline,
# or time given for something that must not happen. See "Writing tests that
# don't depend on timing" in AGENTS.md.
#
# Test code is any file under a `tests/` folder, a file named `tests.rs`,
# `*_tests.rs` or `testing.rs`, and what follows a `#[cfg(test)]` before
# `mod` in any other file.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
import pathlib
import re
import subprocess
import sys

SLEEP = re.compile(r"\b(?:thread|time)::sleep\(")
TEST_MODULE = re.compile(r"^\s*#\[cfg\(test\)\]\s*$")
MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def is_test_file(path: pathlib.Path) -> bool:
    return (
        "tests" in path.parts[:-1]
        or path.name in {"tests.rs", "testing.rs"}
        or path.name.endswith("_tests.rs")
    )


files = subprocess.check_output(["git", "ls-files", "-z", "*.rs"], text=True).split("\0")
errors = []
for name in filter(None, files):
    path = pathlib.Path(name)
    lines = path.read_text(encoding="utf-8").splitlines()
    in_tests = is_test_file(path)
    for index, line in enumerate(lines):
        if not in_tests and TEST_MODULE.match(line):
            following = lines[index + 1 : index + 3]
            in_tests = any(MOD.match(next_line) for next_line in following)
        if not in_tests or not SLEEP.search(line):
            continue
        # The line itself, and the comment block right above it.
        said = [line]
        above = index - 1
        while above >= 0 and lines[above].strip().startswith("//"):
            said.append(lines[above])
            above -= 1
        if not any(re.search(r"//\s*sleep:", text) for text in said):
            errors.append(f"{path}:{index + 1}: {line.strip()}")

if errors:
    print("A sleep in a test needs a `// sleep:` comment saying why no event will do:")
    print("\n".join(f"  {error}" for error in errors))
    sys.exit(1)
PY
