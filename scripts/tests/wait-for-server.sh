#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../wait-for-server.sh"
log=$(mktemp)
trap 'rm -f "$log"' EXIT
echo "still starting" >"$log"
if output=$(wait_for_server "$$" "$log" 1 2>&1); then
    exit 1
fi
[[ $output == *"Timed out"* && $output == *"still starting"* ]]
(echo "cannot open demo directory" >&2; exit 1) >"$log" 2>&1 &
child=$!
wait "$child" || true
if output=$(wait_for_server "$child" "$log" 1 2>&1); then
    exit 1
fi
[[ $output == *"exited before it was ready"* && $output == *"cannot open demo directory"* ]]
echo "ready" >>"$log"
wait_for_server "$$" "$log" 1
