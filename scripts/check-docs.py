# Checks documentation invariants:
#   1. Every `docs/internals/<file>.md` cited in tracked source, config, or docs exists.
#   2. Every quoted `"Heading"` paired with `docs/internals/<file>.md` matches
#      an actual Markdown heading in that file.
#   3. Every `docs/internals/*.md` follows the structural rules (no frontmatter,
#      no `## To do` section, no draft review markers).
#   4. No local user home paths appear in `ARCHITECTURE.md` or `docs/`.
#   5. No legacy `docs/design/` or `docs/plan.md` citations remain outside
#      frozen migration SQL.

import os
import pathlib
import re
import subprocess
import sys

os.chdir(pathlib.Path(__file__).resolve().parent.parent)

ROOT = pathlib.Path(".")
SCAN_EXTS = {".rs", ".sql", ".toml", ".sh", ".ps1", ".py", ".md", ".ts", ".yml", ".yaml"}
SKIP_DIRS = {".git", "target", "node_modules", ".agents"}

errors = []


def is_skipped(path: pathlib.Path) -> bool:
    return any(part in SKIP_DIRS for part in path.parts)


def repo_files() -> list[pathlib.Path]:
    """List tracked files plus untracked non-ignored files under docs/."""
    out = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        encoding="utf-8",
    )
    files = []
    for entry in out.split("\0"):
        if not entry:
            continue
        p = pathlib.Path(entry)
        if p.is_file() and p.suffix in SCAN_EXTS and not is_skipped(p):
            files.append(p)
    return sorted(files)


def extract_headings(md_path: pathlib.Path) -> set[str]:
    headings = set()
    fence_char = None
    for line in md_path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if fence_char is None:
            if stripped.startswith("```"):
                fence_char = "`"
                continue
            if stripped.startswith("~~~"):
                fence_char = "~"
                continue
        else:
            if stripped.startswith(fence_char * 3):
                fence_char = None
            continue
        m = re.match(r"^#{1,6}\s+(.+?)\s*$", stripped)
        if m:
            headings.add(m.group(1).strip())
    return headings


def collapse_comment_lines(text: str, ext: str) -> str:
    """Join wrapped comment lines so citations split across lines match cleanly."""
    if ext == ".md":
        return text
    out = []
    for line in text.splitlines():
        s = line.lstrip()
        for prefix in ("///", "//!", "//", "*", "#", "--"):
            if s.startswith(prefix):
                out.append(s[len(prefix):].strip())
                break
        else:
            out.append("\n")
    return " ".join(out)


# 1. Validate docs/internals/*.md structure and hygiene
internals_dir = ROOT / "docs" / "internals"
# Each guide's headings, by its file name as it is on disk. A citation must
# spell the name exactly: macOS and Windows find a file by a name in another
# case, and Linux does not.
headings_by_name: dict[str, set[str]] = {}

if internals_dir.exists():
    for md in sorted(internals_dir.glob("*.md")):
        content = md.read_text(encoding="utf-8")
        lines = content.splitlines()
        if lines and lines[0].strip() == "---":
            errors.append(f"{md.as_posix()}: unexpected YAML frontmatter on line 1")
        headings = extract_headings(md)
        headings_by_name[md.name] = headings
        for h in headings:
            if h.lower() == "to do":
                errors.append(f"{md.as_posix()}: contains forbidden '## To do' section (use GitHub Issues)")
        for marker in ("[plan]", "[new]", "[review §", "Flags for review"):
            if marker in content:
                errors.append(f"{md.as_posix()}: contains draft marker {marker!r}")

# Check ARCHITECTURE.md and docs/ for leaked local home paths
for doc_path in [ROOT / "ARCHITECTURE.md", *sorted((ROOT / "docs").rglob("*.md"))]:
    if not doc_path.exists():
        continue
    content = doc_path.read_text(encoding="utf-8")
    for pat in (r"/Users/[A-Za-z0-9._-]+", r"/home/[A-Za-z0-9._-]+", r"C:\\Users\\[A-Za-z0-9._-]+"):
        m = re.search(pat, content)
        if m:
            errors.append(f"{doc_path.as_posix()}: contains local user path {m.group(0)!r}")

# 2. Validate every `docs/internals/<file>.md` citation and paired heading,
#    and reject legacy `docs/design/` or `docs/plan.md` citations outside
#    already-frozen migration SQL.
FROZEN_MIGRATIONS = {
    "crates/ritornel-engine/migrations/library/0001_library.sql",
    "crates/ritornel-engine/migrations/library/0004_observation_recording_correction.sql",
    "crates/ritornel-engine/migrations/server/0001_server.sql",
    "crates/ritornel-engine/migrations/server/0002_hub_link.sql",
}
path_re = re.compile(r"docs/internals/([A-Za-z0-9._-]+\.md)")
legacy_doc_re = re.compile(r"docs/(?:design/[A-Za-z0-9._-]+|plan\.md)")
after_heading_re = re.compile(r'docs/internals/([A-Za-z0-9._-]+\.md),\s*"([^"]+)"')
before_heading_re = re.compile(r'"([^"]+)"\s+in\s+docs/internals/([A-Za-z0-9._-]+\.md)')

for p in repo_files():
    posix = p.as_posix()
    if posix == "scripts/check-docs.py":
        continue
    raw = p.read_text(encoding="utf-8", errors="replace")
    collapsed = collapse_comment_lines(raw, p.suffix)

    if posix not in FROZEN_MIGRATIONS:
        for m in legacy_doc_re.finditer(raw):
            errors.append(f"{posix}: cites legacy doc {m.group(0)!r} (use docs/internals/)")

    for m in path_re.finditer(raw):
        if m.group(1) not in headings_by_name:
            errors.append(f"{posix}: cites missing doc docs/internals/{m.group(1)}")

    cited = [(m.group(1), m.group(2)) for m in after_heading_re.finditer(collapsed)]
    cited += [(m.group(2), m.group(1)) for m in before_heading_re.finditer(collapsed)]
    for name, heading in cited:
        if name in headings_by_name and heading not in headings_by_name[name]:
            errors.append(f"{posix}: cites missing heading {heading!r} in docs/internals/{name}")

if errors:
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    sys.exit(1)

print("Documentation checks: ok.")
