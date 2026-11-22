#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
desktop_mem=$script_dir/../desktop-memory.py

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# shellcheck source=../hidden-display.sh
source "$script_dir/../hidden-display.sh"

# 1. RITORNEL_OFFSCREEN unset: no-op even on Linux.
(
    system=Linux
    unset RITORNEL_OFFSCREEN RITORNEL_TEST_DISPLAY RITORNEL_TEST_DISPLAY_AT
    hide
    [[ -z ${RITORNEL_TEST_DISPLAY:-} && -z ${RITORNEL_TEST_DISPLAY_AT:-} ]]
)

# 2. On Linux, RITORNEL_OFFSCREEN=1 refuses to start even if RITORNEL_TEST_DISPLAY is set.
if output=$(
    system=Linux
    RITORNEL_OFFSCREEN=1
    RITORNEL_TEST_DISPLAY=42
    hide 2>&1
); then
    echo "expected Linux hide() to fail" >&2
    exit 1
fi
[[ $output == *"works on macOS only"* ]]

# 3. On macOS, RITORNEL_TEST_DISPLAY already set is used as-is without calling hidden-display.
mkdir -p "$tmp/bin-fail"
cat >"$tmp/bin-fail/hidden-display" <<'EOF'
#!/usr/bin/env bash
echo "should not have been called" >&2
exit 99
EOF
chmod +x "$tmp/bin-fail/hidden-display"
(
    system=Darwin
    PATH="$tmp/bin-fail:/usr/bin:/bin"
    RITORNEL_OFFSCREEN=1
    RITORNEL_TEST_DISPLAY=42
    unset RITORNEL_TEST_DISPLAY_AT
    hide
    [[ $RITORNEL_TEST_DISPLAY == 42 ]]
    [[ -z ${RITORNEL_TEST_DISPLAY_AT:-} ]]
)

# 4. On macOS, hidden-display ensure is parsed and exported; stop is never called.
mkdir -p "$tmp/bin-ok"
log=$tmp/calls.log
cat >"$tmp/bin-ok/hidden-display" <<EOF
#!/usr/bin/env bash
echo "\$*" >>"$log"
[[ \$1 == ensure ]] || exit 2
echo "77 1920 -100"
EOF
chmod +x "$tmp/bin-ok/hidden-display"
(
    system=Darwin
    PATH="$tmp/bin-ok:/usr/bin:/bin"
    RITORNEL_OFFSCREEN=1
    unset RITORNEL_TEST_DISPLAY RITORNEL_TEST_DISPLAY_AT
    hide
    [[ $RITORNEL_TEST_DISPLAY == 77 ]]
    [[ $RITORNEL_TEST_DISPLAY_AT == 1920,-100 ]]
)
[[ $(cat "$log") == "ensure" ]]

# 5. On macOS, missing hidden-display and unset RITORNEL_TEST_DISPLAY prints a clear error.
mkdir -p "$tmp/bin-empty"
if output=$(
    system=Darwin
    PATH="$tmp/bin-empty"
    RITORNEL_OFFSCREEN=1
    unset RITORNEL_TEST_DISPLAY RITORNEL_TEST_DISPLAY_AT
    hide 2>&1
); then
    echo "expected hide() without hidden-display to fail" >&2
    exit 1
fi
[[ $output == *"no hidden display available"* ]]
[[ $output == *"RITORNEL_TEST_DISPLAY"* ]]
[[ $output == *"hidden-display"* ]]

# 6. On macOS, failing or malformed hidden-display ensure fails cleanly.
if output=$(
    system=Darwin
    PATH="$tmp/bin-fail:/usr/bin:/bin"
    RITORNEL_OFFSCREEN=1
    unset RITORNEL_TEST_DISPLAY RITORNEL_TEST_DISPLAY_AT
    hide 2>&1
); then
    echo "expected hide() with failing hidden-display to fail" >&2
    exit 1
fi
[[ $output == *"hidden-display ensure failed (exit 99)"* ]]

mkdir -p "$tmp/bin-bad"
cat >"$tmp/bin-bad/hidden-display" <<'EOF'
#!/usr/bin/env bash
echo "not-a-display"
EOF
chmod +x "$tmp/bin-bad/hidden-display"
if output=$(
    system=Darwin
    PATH="$tmp/bin-bad:/usr/bin:/bin"
    RITORNEL_OFFSCREEN=1
    unset RITORNEL_TEST_DISPLAY RITORNEL_TEST_DISPLAY_AT
    hide 2>&1
); then
    echo "expected hide() with bad hidden-display output to fail" >&2
    exit 1
fi
[[ $output == *"unexpected output"* ]]

# 7. desktop-memory.py follows the same resolution order.
python3 -B - "$desktop_mem" "$tmp" <<'PY'
import importlib.util
import pathlib
import sys

spec = importlib.util.spec_from_file_location("desktop_memory", sys.argv[1])
assert spec and spec.loader
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

tmp = pathlib.Path(sys.argv[2])

# Offscreen unset -> empty overlay.
assert mod.resolve_offscreen_display({}) == {}

# RITORNEL_TEST_DISPLAY already set -> empty overlay, hidden-display not called.
assert mod.resolve_offscreen_display({
    "RITORNEL_OFFSCREEN": "1",
    "RITORNEL_TEST_DISPLAY": "42",
    "PATH": f"{tmp / 'bin-fail'}:/usr/bin:/bin",
}) == {}

# hidden-display ensure -> parsed overlay.
assert mod.resolve_offscreen_display({
    "RITORNEL_OFFSCREEN": "1",
    "PATH": f"{tmp / 'bin-ok'}:/usr/bin:/bin",
}) == {
    "RITORNEL_TEST_DISPLAY": "77",
    "RITORNEL_TEST_DISPLAY_AT": "1920,-100",
}

# Neither set nor on PATH -> clear error.
try:
    mod.resolve_offscreen_display({
        "RITORNEL_OFFSCREEN": "1",
        "PATH": str(tmp / "bin-empty"),
    })
except RuntimeError as exc:
    msg = str(exc)
    assert "no hidden display available" in msg, msg
    assert "RITORNEL_TEST_DISPLAY" in msg, msg
    assert "hidden-display" in msg, msg
else:
    raise AssertionError("expected RuntimeError when hidden-display is missing")

# Failing hidden-display ensure -> clear error.
try:
    mod.resolve_offscreen_display({
        "RITORNEL_OFFSCREEN": "1",
        "PATH": f"{tmp / 'bin-fail'}:/usr/bin:/bin",
    })
except RuntimeError as exc:
    assert "hidden-display ensure failed" in str(exc), str(exc)
else:
    raise AssertionError("expected RuntimeError when hidden-display fails")

# Unexpected output -> clear error.
try:
    mod.resolve_offscreen_display({
        "RITORNEL_OFFSCREEN": "1",
        "PATH": f"{tmp / 'bin-bad'}:/usr/bin:/bin",
    })
except RuntimeError as exc:
    assert "unexpected output" in str(exc), str(exc)
else:
    raise AssertionError("expected RuntimeError on unexpected output")
PY
