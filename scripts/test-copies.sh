#!/usr/bin/env bash
# Runs several input-free copies of Hopwyn on this machine, each shown in a
# browser tab through GTK's broadway backend, to test discovery, pairing and
# the window (not the cursor handoff, which needs real input).
#
# Usage: scripts/test-copies.sh <state-dir> [count]   (count 1-4, default 2)
#        scripts/test-copies.sh --stop
# Stop one copy with: kill "$(cat <state-dir>/copy-N.pid)"
# Copy N is named "Test N", keeps its settings in <state-dir>/N and is shown
# at http://127.0.0.1:808(4+N).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/hopwyn-app"

stop() {
  pkill -f "^$BIN" 2>/dev/null || true
  pkill -f "^gtk4-broadwayd --address 127.0.0.1 :[5-8]" 2>/dev/null || true
}

if [[ "${1:-}" == "--stop" ]]; then
  stop
  exit 0
fi

STATE="${1:?usage: $0 <state-dir> [count] | --stop}"
COUNT="${2:-2}"
[[ "$COUNT" =~ ^[1-4]$ ]] || { echo "count must be 1-4" >&2; exit 2; }

cargo build -p hopwyn-app --manifest-path "$ROOT/Cargo.toml" >/dev/null
stop
sleep 1
for n in $(seq 1 "$COUNT"); do
  display=$((4 + n))
  mkdir -p "$STATE/$n"
  setsid gtk4-broadwayd --address 127.0.0.1 ":$display" >"$STATE/broadway-$n.log" 2>&1 &
  sleep 0.5
  GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" \
  HOPWYN_NO_INPUT=1 HOPWYN_DEBUG=1 HOPWYN_NAME="Test $n" \
  XDG_CONFIG_HOME="$STATE/$n" \
  HOPWYN_LINK_PORT=$((4250 + 10 * n + 2)) HOPWYN_PAIRING_PORT=$((4250 + 10 * n + 3)) \
    setsid "$BIN" >"$STATE/copy-$n.log" 2>&1 &
  echo $! >"$STATE/copy-$n.pid"
  echo "Test $n: http://127.0.0.1:808$display  (log: $STATE/copy-$n.log)"
done
