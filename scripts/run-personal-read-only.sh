#!/usr/bin/env bash
# Runs the installed personal build (`ngteams`) in its read-only mode for an agent to look at the user's real chats
# (CLAUDE.md, "Agents run the personal build only read-only"; docs/personal-build.md, "What agents may do"):
#
# - a snapshot of the user's Teams files (`ngteams snapshot`: the accounts, the settings, the regions and a consistent
#   copy of each store, read without writing it), so the user's own running `ngteams` is never touched;
# - a UI sandbox of its own (`ui-sandbox-run serve`: a nested niri on a headless weston, a private bus), with
#   `ngteams secrets` outside it (the account's `ngteams` items, `get` only) and `ngteams --read-only` inside
#   it, on the snapshot, which sends nothing that anyone sees or that changes the account, opens no push channel and no
#   calls, and logs every refusal to the snapshot's diagnostic log.
#
# The agent never sees a token: the broker hands them to the app over a socket pair.
#
# Usage:
#   scripts/run-personal-read-only.sh <upn>             start (one run at a time); prints where everything is
#   scripts/run-personal-read-only.sh --shot <name>     the window, to <run>/shots/<name>.png (--screen: the output)
#   scripts/run-personal-read-only.sh --key <spec>      a key: ctrl+k, Down, Enter, Escape, Tab, a, ...
#   scripts/run-personal-read-only.sh --type <text>     text typed
#   scripts/run-personal-read-only.sh --click <x> <y>   a click, in output pixels (1280x800)
#   scripts/run-personal-read-only.sh --windows         niri's window list
#   scripts/run-personal-read-only.sh --status          whether it runs, and where everything is
#   scripts/run-personal-read-only.sh --stop [--keep]   stops it all; deletes the run (the snapshot of your chats
#                                                       included) unless --keep
#
# TEAMS_BIN overrides the installed binary (default: ~/.local/lib/ngteams/ngteams, as scripts/install-personal.sh
# puts it). It refuses a binary without the read-only mode. It needs no live lock (it isn't a lab run; its sandbox is
# its own).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state="${XDG_STATE_HOME:-$HOME/.local/state}/ngteams-read-only"
run="$state/run"
pidfile="$run/holder.pid"
runner="$root/target/lab/debug/ui-sandbox-run"
teams="${TEAMS_BIN:-$HOME/.local/lib/ngteams/ngteams}"

die() {
    echo "run-personal-read-only: $*" >&2
    exit 1
}

running() {
    [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null
}

where() {
    cat <<EOF
run:          $run
screenshots:  $run/shots/            ($0 --shot <name>)
view trace:   $run/trace.jsonl
app log:      $run/snapshot/data/logs/  and  $run/app.log  (refused sends: "refused_read_only")
broker log:   $run/broker.log
snapshot:     $run/snapshot/             (a copy of your chats: --stop deletes it)
EOF
}

ask() {
    running || die "not running: start it with $0 <upn>"
    "$runner" "$@" --dir "$run"
}

stop() {
    local keep="$1"
    if running; then
        "$runner" stop --dir "$run" >/dev/null 2>&1 || true
        local pid
        pid="$(cat "$pidfile")"
        for _ in $(seq 1 200); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            sleep 3
        fi
        kill -0 "$pid" 2>/dev/null && die "the sandbox holder ($pid) didn't stop: kill it by hand"
        echo "stopped"
    else
        echo "not running"
    fi
    if [[ "$keep" == keep ]]; then
        [[ -d "$run" ]] && echo "kept: $run"
    elif [[ -d "$run" ]]; then
        rm -rf "$run"
        echo "deleted: $run"
    fi
}

start() {
    local upn="$1"
    running && die "already running ($run): stop it first with $0 --stop"
    [[ -x "$teams" ]] || die "no installed ngteams at $teams (the user installs it: scripts/install-personal.sh)"
    # without executing it: a build with the read-only mode reads its switch's variable
    grep -qa "TEAMS_READ_ONLY" "$teams" ||
        die "$teams has no read-only mode: it needs a reinstall (the user's: scripts/install-personal.sh)"
    echo "building ui-sandbox-run (target/lab)..."
    (cd "$root" && cargo build -q -p ui-sandbox --bin ui-sandbox-run --target-dir "$root/target/lab")

    rm -rf "$run"
    install -d -m 0700 "$state" "$run"
    "$teams" snapshot --to "$run/snapshot" >/dev/null
    setsid "$runner" serve --dir "$run" \
        --broker "$teams" \
        --broker-arg secrets --broker-arg --account --broker-arg "$upn" --broker-arg --fd --broker-arg '{broker-fd}' \
        --app "$teams" \
        --app-arg --read-only --app-arg --account --app-arg "$upn" --app-arg --data --app-arg "$run/snapshot" \
        --app-arg --secrets-fd --app-arg '{secrets-fd}' --app-arg --trace-fd --app-arg '{trace-fd}' \
        >"$run/holder.log" 2>&1 </dev/null &
    for _ in $(seq 1 600); do
        [[ -S "$run/control.sock" ]] && grep -q '^ready' "$run/holder.log" && break
        if [[ -f "$pidfile" ]] && ! running; then
            die "the sandbox didn't start: $(cat "$run/holder.log")"
        fi
        sleep 0.1
    done
    [[ -S "$run/control.sock" ]] || die "the sandbox didn't start in time: $run/holder.log"
    # the window, once it is up
    for _ in $(seq 1 300); do
        running || die "it stopped: $run/app.log, $run/broker.log"
        if "$runner" windows --dir "$run" 2>/dev/null | grep -q "Window ID"; then
            echo "running read-only for $upn"
            where
            return
        fi
        sleep 0.1
    done
    echo "started, but no window yet: see $run/app.log"
    where
}

case "${1:-}" in
"" | -h | --help) sed -n '2,27s/^# \{0,1\}//p' "$0" ;;
--stop) stop "$([[ "${2:-}" == --keep ]] && echo keep || echo delete)" ;;
--status)
    if running; then echo "running"; else echo "not running"; fi
    where
    ;;
--shot) ask shot "${2:?a name}" ;;
--screen) ask screen "${2:?a name}" ;;
--key) ask key "${2:?a key}" ;;
--type) ask type "${2:?text}" ;;
--click) ask click "${2:?x}" "${3:?y}" ;;
--windows) ask windows ;;
-*) die "unknown option $1 (see --help)" ;;
*) start "$1" ;;
esac
