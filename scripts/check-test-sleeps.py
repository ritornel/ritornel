# Checks that a test never sleeps to let something happen. Every
# `thread::sleep` or `time::sleep` in test code needs a comment beginning
# `sleep:`, on its line or in the comment right above it, that says why no
# event will do: a pause between looks at a condition that has a deadline,
# or time given for something that must not happen. See "Writing tests that
# don't depend on timing" in AGENTS.md.
#
# Test code is any file under a `tests/` folder, a file named `tests.rs`,
# `*_tests.rs` or `testing.rs`, and in any other file the item that a `cfg`
# holding for tests stands on, such as `#[cfg(test)] mod tests { .. }`, to
# its closing brace, or the `;` or `,` that ends it.

import os
import pathlib
import re
import subprocess
import sys

os.chdir(pathlib.Path(__file__).resolve().parent.parent)

# A call to sleep, written in full or brought in by `use`.
SLEEP = re.compile(r"(?<![\w.])(?:\w+::)*sleep(?:_until)?\s*\(")
# A `cfg` that holds for tests: it names `test`, and not inside `not(..)`.
CFG = re.compile(r"^\s*#\[cfg\((.*)\)\]")
REASON = re.compile(r"//\s*sleep:\s*\S")
# What `code_of` empties: a raw string, a string, or a character.
LITERAL = re.compile(r'b?r(#*)".*?"\1|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'')


def is_test_file(path: pathlib.Path) -> bool:
    return (
        "tests" in path.parts[:-1]
        or path.name in {"tests.rs", "testing.rs"}
        or path.name.endswith("_tests.rs")
    )


def code_of(line: str) -> str:
    """The line without its comment, with its strings and characters emptied."""
    line = LITERAL.sub('""', line)
    comment = line.find("//")
    return line if comment < 0 else line[:comment]


def for_tests(line: str) -> str | None:
    """What follows a `cfg` that holds for tests on this line, if one is there."""
    found = CFG.match(line)
    if not found:
        return None
    predicate = found.group(1)
    if not re.search(r"\btest\b", re.sub(r"not\s*\([^()]*\)", "", predicate)):
        return None
    return line[found.end():]


def test_lines(lines: list[str]) -> set[int]:
    """The lines of each item that a `cfg` holding for tests stands on."""
    inside = set()
    index = 0
    while index < len(lines):
        rest = for_tests(lines[index])
        if rest is None:
            index += 1
            continue
        start = index
        if not rest.strip():
            # The item starts after any other attributes and comments.
            index += 1
            while index < len(lines) and re.match(r"^\s*(#\[|//|$)", lines[index]):
                index += 1
        # To where the item's braces close, or to the `;` or `,` that ends
        # a one-line item or a field, never past the block around it.
        depth, opened = 0, False
        end = index
        while index < len(lines):
            code = rest if index == start else code_of(lines[index])
            for char in code:
                if char == "{":
                    depth, opened = depth + 1, True
                elif char == "}":
                    depth -= 1
            if depth < 0:
                end = index - 1
                break
            end = index
            if opened and depth == 0:
                break
            if not opened and code.rstrip().endswith((";", ",")):
                break
            index += 1
        inside.update(range(start, min(end, len(lines) - 1) + 1))
        index = max(end, start) + 1
    return inside


files = subprocess.check_output(
    ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.rs"],
    encoding="utf-8",
).split("\0")
errors = []
for name in filter(None, files):
    path = pathlib.Path(name)
    if not path.is_file():
        continue
    lines = path.read_text(encoding="utf-8").splitlines()
    tests = range(len(lines)) if is_test_file(path) else sorted(test_lines(lines))
    for index in tests:
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
            errors.append(f"{path.as_posix()}:{index + 1}: {line.strip()}")

if errors:
    print("A sleep in a test needs a `// sleep:` comment saying why no event will do:")
    print("\n".join(f"  {error}" for error in errors))
    sys.exit(1)
