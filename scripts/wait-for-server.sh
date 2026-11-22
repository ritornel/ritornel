# Wait for the demo server's ready line, or show its startup output.
wait_for_server() {
    local pid=$1 log=$2 attempts=$3 attempt
    for ((attempt=0; attempt<attempts; attempt++)); do
        grep -q '^ready' "$log" && return 0
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "The server exited before it was ready. Check its startup output below." >&2
            cat "$log" >&2
            return 1
        fi
        sleep 0.1
    done
    echo "Timed out waiting for the server. Check its startup output below." >&2
    cat "$log" >&2
    return 1
}
