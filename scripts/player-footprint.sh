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

echo "== Size"
for program in ritornel-server ritornelctl; do
  stripped=$(mktemp)
  strip -o "$stripped" "$bin/$program"
  echo "$program: $(kib "$(stat -c %s "$bin/$program")") as built, $(kib "$(stat -c %s "$stripped")") stripped"
  rm -f "$stripped"
done

echo "== Linked"
ldd "$server" | awk '{print $1}'

echo "== Memory at idle"
data_dir=$(mktemp -d)
ready=$(mktemp)
cleanup() {
  [[ -n "${pid:-}" ]] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
  rm -rf "$data_dir" "$ready"
}
trap cleanup EXIT
"$server" serve --data-dir "$data_dir" --listen 127.0.0.1:0 --output default >"$ready" &
pid=$!
for _ in $(seq 100); do
  grep -q . "$ready" && break
  kill -0 "$pid" 2>/dev/null || { echo "the server stopped before it was ready" >&2; exit 1; }
  sleep 0.1
done
grep -q . "$ready" || { echo "the server was not ready in 10 seconds" >&2; exit 1; }
sleep "$idle_seconds"
grep -E '^(VmRSS|VmHWM|RssAnon|RssFile|Threads):' "/proc/$pid/status"
grep -E '^Pss:' "/proc/$pid/smaps_rollup"
