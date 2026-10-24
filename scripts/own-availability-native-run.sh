#!/usr/bin/env bash
# One native own-availability run (M2 Task 19, docs/protocol/trouter.md, "Own presence"): a's native client sets its
# own availability (`teams-headless a availability`: its endpoint's presence PUT once its push channel's registration
# went through, then the manual status $1 at $2 s and its reset at $3 s after the PUT, ending at $4 s) while b's native
# client reads a's presence (`teams-headless b presence`, UPS's read by POST under b's fence) about every 15 s, from
# before a starts until $5 samples after a ended. a's printed steps go to $6; this prints only availability words,
# a's steps and Unix times, and exits with a's status.
#
# Run it under the live lock and an outer timeout, from a normal lab build (`cargo build -p teams-headless
# --target-dir target/lab`), as CLAUDE.md's "Live runs" says:
#   flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 1500 \
#     scripts/own-availability-native-run.sh busy 120 240 360 30 ~/.local/share/teams-lab/runs/own-availability-native.txt
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LAB=$ROOT/target/lab/debug
MANUAL=$1
MANUAL_AFTER=$2
RESET_AFTER=$3
HOLD=$4
TAIL=$5
OUT=$6
cd "$ROOT" || exit 2
sample() {
  local words
  words=$(timeout --signal=INT --kill-after=10 60 "$LAB/teams-headless" b presence 2>&1 | grep '^presence a ' | tr '\n' ' ')
  echo "sample unix $(date -u +%s) ${words:-no answer}"
}
sample
timeout --signal=INT --kill-after=30 $((HOLD + 180)) "$LAB/teams-headless" a availability --manual "$MANUAL" \
  --manual-after "$MANUAL_AFTER" --reset-after "$RESET_AFTER" --hold "$HOLD" >"$OUT" 2>&1 &
RUN=$!
echo "a started unix $(date -u +%s)"
while kill -0 "$RUN" 2>/dev/null; do
  sleep 15
  sample
done
wait "$RUN"
STATUS=$?
echo "a exit $STATUS (unix $(date -u +%s))"
sed 's/^/a: /' "$OUT"
for _ in $(seq 1 "$TAIL"); do
  sleep 15
  sample
done
pgrep -f "teams-headless a " >/dev/null && echo "WARNING: a's client is still running"
exit "$STATUS"
