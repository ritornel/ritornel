#!/usr/bin/env bash
# Says how big a player build is and how much memory its server holds at
# idle: the size of each program as built and stripped, the system
# libraries the server links, and the server's resident memory once it has
# been ready for a while with nothing to do. Linux only, since it reads
# /proc. CI runs it on the arm64 player build; it runs the same on the board
# itself.
#
#   scripts/player-footprint.sh [<dir with ritornel-server and ritornelctl>]
#
# The default is target/release. The server runs on a data directory made
# for the run and removed after, with the system's default output, which is
# opened only when something is first played, so it makes no sound.
set -euo pipefail

bin=${1:-target/release}
idle_seconds=${IDLE_SECONDS:-10}
server="$bin/ritornel-server"

if [[ "$(uname -s)" != Linux ]]; then
  echo "player-footprint.sh reads /proc, so it runs on Linux only" >&2
  exit 1
fi

kib() { echo "$(($1 / 1024)) KiB"; }

work=$(mktemp -d)
cleanup() {
  if [[ -n "${pid:-}" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

echo "== Size"
for program in ritornel-server ritornelctl; do
  size="$(kib "$(stat -c %s "$bin/$program")") as built"
  # A board may have no strip: the size as built is still said.
  if command -v strip >/dev/null; then
    strip -o "$work/$program" "$bin/$program"
    size="$size, $(kib "$(stat -c %s "$work/$program")") stripped"
  fi
  echo "$program: $size"
done

echo "== Linked"
ldd "$server" | awk '{print $1}'

echo "== Memory at idle"
mkdir "$work/data"
# Run as the settings in this shell would not have it: on its own data
# directory, named and logged as a new server is.
env -u RITORNEL_NAME -u RITORNEL_LOG_LEVEL -u RITORNEL_BACKUP_DIR -u RITORNEL_LISTEN \
  "$server" serve --data-dir "$work/data" --listen 127.0.0.1:0 --output default >"$work/ready" &
pid=$!
for _ in $(seq 100); do
  grep -q . "$work/ready" && break
  kill -0 "$pid" 2>/dev/null || { echo "the server stopped before it was ready" >&2; exit 1; }
  sleep 0.1
done
grep -q . "$work/ready" || { echo "the server was not ready in 10 seconds" >&2; exit 1; }
sleep "$idle_seconds"
grep -E '^(VmRSS|VmHWM|RssAnon|RssFile|Threads):' "/proc/$pid/status"
grep -E '^Pss:' "/proc/$pid/smaps_rollup"
