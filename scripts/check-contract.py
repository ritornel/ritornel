# Checks the contract version against main's: a wire that differs from
# main's is a new version, one above main's, however many commits or pull
# requests it takes to land. So builds with one number speak one wire, and a
# change made in several steps still lands as one raise.
#
# contract/contract.version holds the version and a digest of the wire, and a
# test checks that it is this build's (`mise run contract` writes it). This
# compares it with main's as this work builds on it: the commit where it
# leaves origin/main, which CI fetches first, so a change landing on main
# since does not fail it. $RITORNEL_CONTRACT_BASE names another base.

import os
import pathlib
import re
import subprocess
import sys

os.chdir(pathlib.Path(__file__).resolve().parent.parent)

FILE = "contract/contract.version"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def git(*args: str) -> str | None:
    """Git's output, or None if it fails."""
    done = subprocess.run(
        ["git", *args], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, encoding="utf-8"
    )
    return done.stdout if done.returncode == 0 else None


def field(text: str, name: str) -> str:
    """The value of each line `<name> <value>`, one a line, or nothing."""
    values = []
    for line in text.splitlines():
        words = line.split()
        if words and words[0] == name:
            values.append(words[1] if len(words) > 1 else "")
    return "\n".join(values)


base = os.environ.get("RITORNEL_CONTRACT_BASE") or (git("merge-base", "HEAD", "origin/main") or "").strip()
if not base:
    fail("cannot find where this leaves origin/main: fetch it (git fetch origin main), or set RITORNEL_CONTRACT_BASE")

theirs = git("show", f"{base}:{FILE}")
if theirs is None:
    fail(f"cannot read {FILE} at {base}: fetch it (git fetch origin main), or set RITORNEL_CONTRACT_BASE")
ours = pathlib.Path(FILE).read_text(encoding="utf-8")

base_version, base_digest = field(theirs, "version"), field(theirs, "digest")
version, digest = field(ours, "version"), field(ours, "digest")

if not (re.fullmatch(r"[0-9]+", base_version) and re.fullmatch(r"[0-9]+", version) and base_digest and digest):
    fail(f"{FILE} here or at {base} does not hold a version and a digest")
base_version, version = int(base_version), int(version)

if version < base_version:
    fail(f"the contract is at version {version}, behind {base}'s {base_version}: rebase")
if version == base_version and digest != base_digest:
    fail(
        f"the wire differs from {base}'s and CONTRACT_VERSION is still {base_version}: raise it to "
        f"{base_version + 1} in crates/ritornel-contract/src/lib.rs, then run `mise run contract`"
    )
if version > base_version + 1:
    fail(
        f"the contract is at version {version}, and {base}'s is {base_version}: "
        f"a change lands as one raise, to {base_version + 1}"
    )
print(f"Contract version: ok ({version}, {base} at {base_version}).")
