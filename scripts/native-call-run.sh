#!/usr/bin/env bash
# One native lab-to-lab call (M3 Task 16): every callee's native client answers it (`teams-headless <callee> watch
# --answer-lab-calls`, which answers only an invitation the engine's fence offered: a lab caller's naming only lab
# accounts), a late joiner's native client (`teams-headless <joiner> join <chat>`) lets its ring end and joins the
# running call, and the caller's native client calls (`teams-headless <caller> call <chat>`). Each plays its own tone
# keyed (a 1000 Hz, b 660 Hz, c 880 Hz) and listens for every other's (`--hear`), recording to $OUT; each one's output
# goes to $OUT/<alias>-<command>.txt. Waits for every callee's and the joiner's calling registration before the caller
# starts, interrupts the watchers 5 s after the caller exits, and prints every exit status. A call in a chat starts at
# least 90 s after the chat's last run ended (its end stamped under the lab's data dir), and a call the service refused
# because the chat still had a running call (`remote 409`) is placed once more 90 s later.
#
# Usage: native-call-run.sh OUT CALLER CHAT [CALLEE…] [--join JOINER]
#   e.g. native-call-run.sh ~/.local/share/teams-lab/headless/ab a b b
#        native-call-run.sh ~/.local/share/teams-lab/headless/abc a abc b --join c
# HOLD (default 45) is how long the caller holds the call once connected; TONE_SECONDS, when set, how long its tone
# lasts (a long call's tone kept short keeps its files and their analysis small).
# EXTRA, when set, goes to every native client, and EXTRA_<alias> instead of it to that one's (e.g. EXTRA_a="--video
# --camera-after 5" EXTRA_b="--video --camera-after 15": a 1:1 call with video both ways, the cameras turned on apart).
#
# Run it under the live lock and an outer timeout, from a normal lab build (`cargo build --release -p teams-headless
# --target-dir target/lab`), as CLAUDE.md's "Live runs" says:
#   flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 600 scripts/native-call-run.sh …
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${TEAMS_HEADLESS_BIN:-$ROOT/target/lab/release/teams-headless}
OUT=$1
CALLER=$2
CHAT=$3
shift 3
CALLEES=()
JOINER=""
while [ $# -gt 0 ]; do
  case "$1" in
    --join) JOINER=$2; shift 2 ;;
    *) CALLEES+=("$1"); shift ;;
  esac
done
mkdir -p -m 0700 "$OUT"
declare -A TONE=([a]=1000 [b]=660 [c]=880)
# every lab account's tone but $1's, as --hear arguments
others() {
  local alias
  for alias in "${CALLEES[@]}" "$CALLER" ${JOINER:+"$JOINER"}; do
    [ "$alias" != "$1" ] && printf -- '--hear %s ' "${TONE[$alias]}"
  done
}
# the extra arguments of alias $1: EXTRA_<alias>, else EXTRA
extra() {
  local var="EXTRA_$1"
  printf '%s' "${!var:-${EXTRA:-}}"
}
WATCHERS=()
for CALLEE in "${CALLEES[@]}"; do
  # shellcheck disable=SC2046
  "$BIN" "$CALLEE" watch --answer-lab-calls --tone "${TONE[$CALLEE]}" $(others "$CALLEE") \
    --record "$OUT/$CALLEE-watch.wav" --tone-seconds 200 $(extra "$CALLEE") >"$OUT/$CALLEE-watch.txt" 2>&1 &
  WATCHERS+=($!)
done
JOINING=""
if [ -n "$JOINER" ]; then
  # shellcheck disable=SC2046
  "$BIN" "$JOINER" join "$CHAT" --tone "${TONE[$JOINER]}" $(others "$JOINER") --record "$OUT/$JOINER-join.wav" \
    --seconds 30 --wait 90 --ring-wait 240 $(extra "$JOINER") >"$OUT/$JOINER-join.txt" 2>&1 &
  JOINING=$!
fi
for WHO in "${CALLEES[@]}" ${JOINER:+"$JOINER"}; do
  FILE=$OUT/$WHO-watch.txt
  [ "$WHO" = "$JOINER" ] && FILE=$OUT/$WHO-join.txt
  for _ in $(seq 1 180); do
    grep -q "calling_registration registered=true" "$FILE" 2>/dev/null && break
    sleep 0.5
  done
  grep -q "calling_registration registered=true" "$FILE" || echo "warning: $WHO's calling stack didn't register" >&2
done
# the service refuses a chat's call for a while after the chat's last one (409, 5704: "failure to resolve ownership",
# ~60 s seen): a call in a chat starts at least $SPACING s after the chat's last run here ended
SPACING=90
LAB_DATA=${XDG_DATA_HOME:-$HOME/.local/share}/teams-lab
STAMPS=$LAB_DATA/headless
# the chat's own key, the same whoever calls and however the chat is named: a 1:1 chat named by the other account's
# alias by both aliases sorted, the recorded group chat (`abc`) by its thread id as chats.toml records it, any other
# chat by the id given
chat_key() {
  local id=$CHAT
  case "$CHAT" in
    a | b | c)
      printf '%s\n' "$CALLER" "$CHAT" | sort | tr -d '\n'
      return
      ;;
    abc) id=$(sed -n 's/^abc *= *"\(.*\)"$/\1/p' "$LAB_DATA/chats.toml" 2>/dev/null) ;;
  esac
  printf '%s' "${id:-$CHAT}" | tr -c 'A-Za-z0-9._-' '_'
}
STAMP=$STAMPS/.last-call-$(chat_key)
if [ -f "$STAMP" ]; then
  WAIT=$(($(cat "$STAMP") + SPACING - $(date +%s)))
  if [ "$WAIT" -gt 0 ]; then
    echo "waiting ${WAIT}s: the chat's last call ended less than ${SPACING}s ago"
    sleep "$WAIT"
  fi
fi
# the caller's call, its output into $OUT/$CALLER-call.txt
place() {
  echo "caller starts unix_ms $(date +%s%3N)"
  # shellcheck disable=SC2046
  "$BIN" "$CALLER" call "$CHAT" --tone "${TONE[$CALLER]}" $(others "$CALLER") --record "$OUT/$CALLER-call.wav" \
    --seconds "${HOLD:-45}" --wait 90 ${TONE_SECONDS:+--tone-seconds "$TONE_SECONDS"} $(extra "$CALLER") \
    >"$OUT/$CALLER-call.txt" 2>&1
}
place
STATUS=$?
if grep -q "remote 409" "$OUT/$CALLER-call.txt"; then
  # the chat still had a running call: once more, $SPACING s later (the refused try kept beside it)
  mv "$OUT/$CALLER-call.txt" "$OUT/$CALLER-call-409.txt"
  echo "$CALLER call refused (remote 409): trying again in ${SPACING}s"
  sleep "$SPACING"
  place
  STATUS=$?
fi
mkdir -p -m 0700 "$STAMPS"
date +%s >"$STAMP"
echo "$CALLER call exit $STATUS"
if [ -n "$JOINING" ]; then
  wait "$JOINING"
  echo "$JOINER join exit $?"
fi
sleep 5
for PID in "${WATCHERS[@]}"; do kill -INT "$PID" 2>/dev/null; done
INDEX=0
for PID in "${WATCHERS[@]}"; do
  wait "$PID"
  echo "${CALLEES[$INDEX]} watch exit $?"
  INDEX=$((INDEX + 1))
done
