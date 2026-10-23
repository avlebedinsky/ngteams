#!/usr/bin/env bash
# One own-presence experiment (M2 Task 18, docs/protocol/trouter.md, "Own presence"): a's lab browser runs the
# scenario $1 (`proto-lab run <scenario> --headless`) while b's native client reads a's presence (`teams-headless b
# presence`, UPS's read by POST under b's fence) about every 15 s, $2 samples in all, the scenario's run and a tail
# after it. The scenario's printed report goes to $3; this prints only availability words and Unix times, and exits
# with the scenario's status.
#
# Run it under the live lock and an outer timeout, from a normal lab build (`cargo build -p proto-lab -p teams-headless
# --target-dir target/lab`), as CLAUDE.md's "Live runs" says:
#   flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 2700 \
#     scripts/own-presence-run.sh own-presence-manual 85 ~/.local/share/teams-lab/runs/own-presence-manual.txt
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
LAB=$ROOT/target/lab/debug
SCENARIO=$1
SAMPLES=$2
OUT=$3
cd "$ROOT" || exit 2
sample() {
  local words
  words=$(timeout --signal=INT --kill-after=10 60 "$LAB/teams-headless" b presence 2>&1 | grep '^presence a ' | tr '\n' ' ')
  echo "sample unix $(date -u +%s) ${words:-no answer}"
}
sample
timeout --signal=INT --kill-after=60 1800 "$LAB/proto-lab" run "$SCENARIO" --headless >"$OUT" 2>&1 &
RUN=$!
echo "proto-lab started unix $(date -u +%s)"
for _ in $(seq 1 "$SAMPLES"); do
  sleep 15
  sample
done
wait "$RUN"
STATUS=$?
echo "proto-lab exit $STATUS (unix $(date -u +%s))"
pgrep -f "teams-lab/profiles" >/dev/null && echo "WARNING: a lab browser is still running"
sample
# the scenario's own exit is the run's: a failed or breached run fails the wrapper too
exit "$STATUS"
