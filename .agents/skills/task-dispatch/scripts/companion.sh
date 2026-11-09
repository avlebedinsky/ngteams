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
#   companion.sh send --session <id> --file <file> [--via auto|type]
#       Gives the agent "Read <file> and follow it." and prints how: `mail`, `mail-held`, `typed` or `blocked`.
#       Claude at any status and a Codex in mid-turn get ngterm mail, so the message can't merge with what the user is
#       typing in the pane or land on a dialog. An idle Codex isn't woken by mail, so it has the message typed and
#       submitted (text, then a separate Enter: Codex takes a carriage return inside typed text as part of the input),
#       as does an agent without ngterm's mail hook, one ngterm can't name, a rejected mail, and mail still unread
#       seconds after the agent went idle. `mail-held` means a dialog holds the agent and it reads the mail only once
#       the dialog is gone; this command can't answer a dialog. `blocked` (exit 3) means typing was the only way left
#       and nothing was typed, because the Enter could confirm the dialog's highlighted option: clear the dialog and
#       send again.
#       --via type never mails: an agent follows mail only from the sender its brief names. A Codex's status is
#       checked against its screen first, since its hooks report `blocked` with no dialog and `active` when idle.
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
# Exit codes: 0 done; 1 ngterm refused or failed; 2 usage; 3 send refused to type on a dialog.
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

# The session's node of the tree, its status as `send` should read it; empty when ngterm can't be read or the session
# isn't open.
session_node() {
  local tree node
  tree=$(ctl tree --json 2>/dev/null) || return 0
  node=$(jq -c --arg id "$session" '[.. | objects | .sessions? // empty | .[] | select(.id == $id)] | first // empty' \
    <<<"$tree" 2>/dev/null) || return 0
  if [ "$(agent_of "$node")" = codex ]; then
    node=$(jq -c --arg status "$(codex_status "$node")" \
      '.turnEnded = ((.status // "") != "active") | .status = $status' <<<"$node")
  fi
  printf '%s\n' "$node"
}

# Codex's hooks report `blocked` for a turn that ended on a question, stay `active` after an interrupt, and have stopped
# watching by the time a popup follows a turn, so only its primary pane's screen tells a dialog or a running turn from
# Codex idling at its prompt. An unreadable screen keeps the reported status. A moment of a turn that shows neither
# reads as idle and is typed to, which Codex takes as a queued message.
codex_status() {
  local text
  if ! text=$(ctl session text --target "$session" --lines 12 --pane left 2>/dev/null); then
    jq -r '.status // ""' <<<"$1"
    return
  fi
  case ${text,,} in
    *"press enter to confirm"* | *"press enter to continue"* | *"enter to submit answer"* | *"enter to submit all"* | \
      *"allow command?"*) echo blocked ;;
    *"• esc to interrupt)"*) echo active ;;
    *) echo completed ;;
  esac
}

mid_turn() {
  case $(jq -r '.status // ""' <<<"$1") in
    active | blocked) return 0 ;;
    *) return 1 ;;
  esac
}

held_by_dialog() {
  [ "$(jq -r '.status // ""' <<<"$1")" = blocked ]
}

# claude or codex when one holds the pane, directly or under a wrapper such as `ngswap claude ...`; else nothing.
agent_of() {
  jq -r '[.foreground[0:2][]? | strings | split("/") | last | select(IN("claude", "codex"))] | first // ""' <<<"$1"
}

# ngterm hands mail to an agent only through its installed hooks.
has_mail_hook() {
  case $1 in
    claude) grep -q agent-mail "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json" 2>/dev/null ;;
    codex) grep -q ngterm-codex-status "${CODEX_HOME:-$HOME/.codex}/config.toml" 2>/dev/null ;;
    *) return 1 ;;
  esac
}

# A Codex reads mail only while a turn is open, and its hooks report `active` only then: `blocked` can follow a finished
# turn, and a popup after a turn isn't one. Nothing would read the mail once such a dialog closes.
takes_mail() {
  local agent
  agent=$(agent_of "$1")
  has_mail_hook "$agent" || return 1
  [ "$agent" = claude ] || { mid_turn "$1" && [ "$(jq -r '.turnEnded // false' <<<"$1")" = false ]; }
}

unread_mail() {
  [ "$(jq -r '[.surfaces[]? | select(.kind == "left") | .pendingMessages // 0] | add // 0' <<<"$1")" -gt 0 ]
}

# Nothing will read the mail: the agent has gone idle and left it there.
mail_stranded() {
  [ -n "$1" ] && ! mid_turn "$1" && unread_mail "$1"
}

mail_message() {
  local extra=()
  [ -z "${NGTERM_SOCKET:-}" ] || extra+=(--socket "$NGTERM_SOCKET")
  ngtermctl msg send "$1" --to "$session:left" "${extra[@]}" >/dev/null
}

type_message() {
  ctl session type --target "$session" --pane left "$1" >/dev/null
  sleep 0.5
  ctl session type --target "$session" --pane left $'\r' >/dev/null
}

[ $# -ge 1 ] || usage
cmd=$1
shift
runtime="" name="" cwd="" brief="" run_dir="" parent="${NGTERM_SESSION_ID:-}" session="" file="" lines=60 resume="" via=auto
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
    --via) via=$2 ;;
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
    case $via in auto | type) ;; *) usage ;; esac
    file=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
    message="Read $file and follow it."
    node=$(session_node)
    if [ "$via" = auto ] && [ -n "$node" ] && takes_mail "$node" && mail_message "$message"; then
      # An idle Claude's waiter takes the mail within moments unless its turn only just ended, and a turn can end just
      # as the mail lands on a Codex. An unread copy handed over later only repeats the instruction it was typed.
      for _ in 1 2 3; do
        sleep 2
        node=$(session_node)
        mail_stranded "$node" || break
      done
      if ! mail_stranded "$node"; then
        if held_by_dialog "$node"; then echo mail-held; else echo mail; fi
        exit 0
      fi
    fi
    if held_by_dialog "$node"; then
      echo "companion: a dialog holds session $session; nothing typed" >&2
      echo blocked
      exit 3
    fi
    type_message "$message"
    echo typed
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
    ctl session text --target "$session" --lines "$lines" --pane left
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
