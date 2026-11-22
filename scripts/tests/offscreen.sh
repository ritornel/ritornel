#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
desktop="$root/scripts/desktop.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if [[ $(uname -s) == Darwin ]]; then
    # Empty state: status reports not running (exit 1); stop is idempotent (exit 0).
    if output=$(RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen status 2>&1); then
        echo "expected status to fail when no display is recorded" >&2
        exit 1
    fi
    [[ $output == *"not running"* ]]

    output=$(RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen stop)
    [[ $output == *"not running"* ]]

    # Stale record with a dead PID: status fails, stop cleans it up.
    state="$tmp/ritornel-test-display"
    printf 'pid=999999\ndisplay=42\norigin=100,200\n' >"$state"
    if RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen status >/dev/null 2>&1; then
        echo "expected status to reject stale dead PID" >&2
        exit 1
    fi
    RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen stop >/dev/null
    [[ ! -e $state ]]

    # Stale record pointing at a live non-offscreen PID (this shell): must not kill it.
    printf 'pid=%s\ndisplay=42\norigin=100,200\n' "$$" >"$state"
    if RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen status >/dev/null 2>&1; then
        echo "expected status to reject non-offscreen PID" >&2
        exit 1
    fi
    RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen stop >/dev/null
    [[ ! -e $state ]]

    # Corrupt record: status fails, stop cleans it up.
    printf 'garbage\n' >"$state"
    if RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen status >/dev/null 2>&1; then
        echo "expected status to reject corrupt state" >&2
        exit 1
    fi
    RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen stop >/dev/null
    [[ ! -e $state ]]

    # Unknown subcommand exits 2.
    set +e
    RITORNEL_TEST_DISPLAY_DIR="$tmp" "$desktop" offscreen bogus >/dev/null 2>&1
    rc=$?
    set -e
    [[ $rc -eq 2 ]]
else
    # Called from `scripts/desktop.sh check`, which already verified the Linux
    # desktop libraries are present so desktop.sh reaches `mac_only`.
    if output=$("$desktop" offscreen 2>&1); then
        echo "expected offscreen to fail on non-macOS" >&2
        exit 1
    fi
    [[ $output == *"macOS only"* ]]
fi

# Verify desktop-memory.py respects caller-supplied RITORNEL_TEST_DISPLAY and
# parses the shared offscreen command output without killing any display.
if command -v python3 >/dev/null 2>&1; then
    python3 -B - "$root" "$tmp" <<'PY'
import importlib.util
import os
import stat
import sys
from pathlib import Path

repo_root = Path(sys.argv[1])
tmp_root = Path(sys.argv[2])

spec = importlib.util.spec_from_file_location(
    "desktop_memory", repo_root / "scripts" / "desktop-memory.py"
)
mod = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(mod)

# Stub scripts/desktop.sh that records invocations.
stub_dir = tmp_root / "stub-repo"
(stub_dir / "scripts").mkdir(parents=True)
marker = tmp_root / "invoked"
stub = stub_dir / "scripts" / "desktop.sh"
stub.write_text(
    f"#!/usr/bin/env bash\n"
    f"echo invoked >> {str(marker)!r}\n"
    f"echo 'offscreen 77 -1600 240'\n",
    encoding="utf-8",
)
stub.chmod(stub.stat().st_mode | stat.S_IXUSR)

# Neither set -> empty, stub not called.
assert mod.resolve_offscreen_display(stub_dir, env={}) == {}
assert not marker.exists()

# Caller already set RITORNEL_TEST_DISPLAY -> empty (leave untouched), stub not called.
assert mod.resolve_offscreen_display(
    stub_dir, env={"RITORNEL_OFFSCREEN": "1", "RITORNEL_TEST_DISPLAY": "99"}
) == {}
assert not marker.exists()

# RITORNEL_OFFSCREEN=1 without RITORNEL_TEST_DISPLAY -> calls stub and parses output.
got = mod.resolve_offscreen_display(
    stub_dir, env={"PATH": os.environ.get("PATH", ""), "RITORNEL_OFFSCREEN": "1"}
)
assert got == {
    "RITORNEL_TEST_DISPLAY": "77",
    "RITORNEL_TEST_DISPLAY_AT": "-1600,240",
}, got
assert marker.exists()
PY
fi
