#!/usr/bin/env bash
# Runs a dispatch worker as an ngterm companion session: an interactive Claude Code or Codex placed under the
# dispatcher's session in the sidebar, where the user can watch it and the dispatcher can steer it.
#
# Usage:
#   companion.sh start --runtime claude|codex --name <name> --cwd <dir> --brief <file> --run-dir <dir>
#                      [--parent <session>] [--resume <claude session uuid>]
#       Starts the agent in <dir> on the brief file and prints one JSON line (--resume continues a closed Claude
#       companion's conversation with the brief as its next prompt):
#       {"session": "<ngterm session id>", "window": "<ngterm window id>", "runtime": "...",
#        "claude_session": "<uuid>"|null, "transcript": "<claude jsonl>"|null}
#       --parent defaults to $NGTERM_SESSION_ID. The launcher lands in <run-dir>/companions/<name>.sh. When ngterm
#       won't pin the pane's relaunch command, the session is closed again and start fails.
#   companion.sh send --session <id> --file <file>
#       Types "Read <file> and follow it." into the agent and submits it (text, then a separate Enter: Codex takes a
#       carriage return inside typed text as part of the input).
#   companion.sh state --session <id>
#       Prints `<status> <changed-at> agent|no-agent|unread`: the agent status (idle, active, completed, blocked, or
#       null), when it changed, and whether Claude or Codex runs in the pane (`unread`: ngterm can't see the pane's
#       program, as with an unshown pane after a relaunch; `wake` it). `gone` when the window has no such session;
#       `unknown` (exit 1) when ngterm can't be read.
#   companion.sh screen --session <id> [--lines <n>]
#       Prints the last lines of the agent's screen (default 60): what it said when it stopped or what it asks.
#   companion.sh wake --session <id>
#       Wakes a pane that hasn't spawned since an ngterm relaunch (an empty `session type`, which spawns a dormant
#       pane and types nothing): its pinned relaunch command runs, `claude --resume` or a plain shell for Codex.
#   companion.sh close --session <id>
#       Closes the session, which ends the agent and every process it started.
#
# Every command takes --window <id> (default $NGTERM_WINDOW_ID): ngterm reads and targets only the frontmost window
# otherwise. Pass the window `start` printed.
#
# Exit codes: 0 done; 1 ngterm refused or failed; 2 usage.
set -euo pipefail

usage() {
  awk 'NR > 1 && /^#/ { print; next } NR > 1 { exit }' "$0" >&2
  exit 2
}

ctl() {
  local extra=()
  [ -z "${NGTERM_SOCKET:-}" ] || extra+=(--socket "$NGTERM_SOCKET")
  [ -z "$window" ] || extra+=(--window "$window")
  ngtermctl "$@" "${extra[@]}"
}

[ $# -ge 1 ] || usage
cmd=$1
shift
runtime="" name="" cwd="" brief="" run_dir="" parent="${NGTERM_SESSION_ID:-}" session="" file="" lines=60 resume=""
window="${NGTERM_WINDOW_ID:-}"
while [ $# -gt 0 ]; do
  [ $# -ge 2 ] || usage
  case $1 in
    --runtime) runtime=$2 ;;
    --name) name=$2 ;;
    --cwd) cwd=$2 ;;
    --brief) brief=$2 ;;
    --run-dir) run_dir=$2 ;;
    --parent) parent=$2 ;;
    --session) session=$2 ;;
    --file) file=$2 ;;
    --lines) lines=$2 ;;
    --resume) resume=$2 ;;
    --window) window=$2 ;;
    *) usage ;;
  esac
  shift 2
done

case $cmd in
  start)
    [ -n "$name" ] && [ -n "$cwd" ] && [ -n "$brief" ] && [ -n "$run_dir" ] && [ -n "$parent" ] || usage
    [ -d "$cwd" ] || { echo "companion: no directory $cwd" >&2; exit 1; }
    [ -f "$brief" ] || { echo "companion: no brief $brief" >&2; exit 1; }
    cwd=$(cd "$cwd" && pwd)
    brief=$(cd "$(dirname "$brief")" && pwd)/$(basename "$brief")
    prompt="Read $brief and follow it: it is your whole task brief."
    mkdir -p "$run_dir/companions"
    launcher=$(cd "$run_dir/companions" && pwd)/$name.sh
    uuid="" transcript="" restore=""
    case $runtime in
      claude)
        uuid=${resume:-$(uuidgen | tr '[:upper:]' '[:lower:]')}
        session_flag=--session-id
        [ -z "$resume" ] || session_flag=--resume
        # Claude Code keeps a session's transcript under its working directory with every other character a dash.
        transcript="$HOME/.claude/projects/${cwd//[^A-Za-z0-9]/-}/$uuid.jsonl"
        printf '#!/usr/bin/env bash\ncd %q || exit 1\nexec claude --permission-mode auto %s %q -n %q %q\n' \
          "$cwd" "$session_flag" "$uuid" "$name" "$prompt" >"$launcher"
        restore="claude --resume $uuid --permission-mode auto"
        ;;
      codex)
        [ -z "$resume" ] || usage
        # --no-daemon: a TUI on Codex's shared daemon runs its hooks with the environment of whichever pane started the
        # daemon, so this companion's status would land on that pane. ngterm's shell wrapper adds the flag the same way.
        printf '#!/usr/bin/env bash\ncd %q || exit 1\nexec codex --no-daemon -C %q %q\n' \
          "$cwd" "$cwd" "$prompt" >"$launcher"
        ;;
      *) usage ;;
    esac
    chmod +x "$launcher"
    id=$(ctl session new --parent "$parent" --cwd "$cwd" --no-select --name "$name" \
      --command "bash $(printf %q "$launcher")" --json | jq -er '.result.id') ||
      { echo "companion: ngterm did not start the session" >&2; exit 1; }
    # A relaunch must never rerun the first prompt: that would start the task over. Claude resumes its conversation;
    # Codex's session id isn't known here, so its pane comes back as a plain shell (the waiter reports it `exited`).
    # Without the pin a relaunch reruns the launcher, so a refused pin ends the companion before it gets far.
    pinned=(--none)
    [ -z "$restore" ] || pinned=("$restore")
    if ! ctl session restore "${pinned[@]}" --target "$id" >/dev/null; then
      ctl session close --target "$id" >/dev/null || true
      echo "companion: ngterm refused to pin the relaunch command; closed session $id" >&2
      exit 1
    fi
    jq -cn --arg s "$id" --arg w "$window" --arg r "$runtime" --arg u "$uuid" --arg t "$transcript" \
      '{session: $s, window: (if $w == "" then null else $w end), runtime: $r,
        claude_session: (if $u == "" then null else $u end), transcript: (if $t == "" then null else $t end)}'
    ;;
  send)
    [ -n "$session" ] && [ -f "$file" ] || usage
    file=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
    ctl session type --target "$session" "Read $file and follow it." >/dev/null
    sleep 0.5
    ctl session type --target "$session" $'\r' >/dev/null
    ;;
  state)
    [ -n "$session" ] || usage
    if ! tree=$(ctl tree --json 2>/dev/null) || ! jq -e '.ok == true' >/dev/null 2>&1 <<<"$tree"; then
      echo unknown
      exit 1
    fi
    jq -r --arg id "$session" '
      [.. | objects | .sessions? // empty | .[] | select(.id == $id)] | first
      | if . == null then "gone" else
          "\(.status) \(.statusChangedAt // 0) \(
            if (.foreground | type) == "array" and (.foreground | length) > 0 then
              (if [.foreground[0:2][] | strings | split("/") | last] | any(IN("claude", "codex"))
               then "agent" else "no-agent" end)
            elif (.foregroundShell // "") != "" then "no-agent" else "unread" end)" end' <<<"$tree"
    ;;
  screen)
    [ -n "$session" ] || usage
    ctl session text --target "$session" --lines "$lines"
    ;;
  wake)
    [ -n "$session" ] || usage
    ctl session type --target "$session" "" >/dev/null
    ;;
  close)
    [ -n "$session" ] || usage
    ctl session close --target "$session" >/dev/null
    ;;
  *) usage ;;
esac
