# Resolve a display no one sees when RITORNEL_OFFSCREEN=1 (macOS only).
# Never stops the display at exit: it is shared across runs.
hide() {
    [[ ${RITORNEL_OFFSCREEN:-} == 1 ]] || return 0
    if [[ ${system:-$(uname -s)} != Darwin ]]; then
        echo "RITORNEL_OFFSCREEN works on macOS only: Linux has no display no one sees for it yet." >&2
        exit 1
    fi
    if [[ -n ${RITORNEL_TEST_DISPLAY:-} ]]; then
        return 0
    fi
    if ! command -v hidden-display >/dev/null 2>&1; then
        echo "RITORNEL_OFFSCREEN=1: no hidden display available. Set RITORNEL_TEST_DISPLAY to a display ID, or install a tool that provides one (such as hidden-display)." >&2
        exit 1
    fi
    local out rc=0 id x y extra
    out=$(hidden-display ensure) || rc=$?
    if ((rc != 0)); then
        echo "hidden-display ensure failed (exit $rc): no hidden display available." >&2
        exit 1
    fi
    if [[ $out == *$'\n'* ]] || ! read -r id x y extra <<<"$out" \
        || [[ ! $id =~ ^[0-9]+$ || ! $x =~ ^-?[0-9]+$ || ! $y =~ ^-?[0-9]+$ || -n ${extra:-} ]]; then
        echo "hidden-display ensure returned unexpected output: $out" >&2
        exit 1
    fi
    export RITORNEL_TEST_DISPLAY=$id RITORNEL_TEST_DISPLAY_AT=$x,$y
}
