#!/usr/bin/env bash
# A lab account in ngteams, in its own UI sandbox. Before every live start the agent checks whom it reaches.
#
# Usage:
#   scripts/run-lab.sh --build-only             normal lab build only; no account, broker or window is opened
#   scripts/run-lab.sh <alias> [--pipewire] [--timeout <seconds>] start (default shutdown deadline: 1800 s)
#   scripts/run-lab.sh <alias> --shot <name>    window screenshot (--screen captures the output)
#   scripts/run-lab.sh <alias> --key <spec>     key, e.g. ctrl+k, Down, Enter
#   scripts/run-lab.sh <alias> --type <text>    type text (never a password)
#   scripts/run-lab.sh <alias> --click <x> <y>  click in output pixels
#   scripts/run-lab.sh <alias> --clipboard <mime> <path> [<mime> <path>]...  the files on the sandbox's clipboard,
#                                               one offer, for the app to paste (e.g. image/png shot.png text/plain a.txt)
#   scripts/run-lab.sh <alias> --windows        list this sandbox's windows
#   scripts/run-lab.sh <alias> --status         status and file locations
#   scripts/run-lab.sh <alias> --stop           stop app, broker and sandbox; retain logs and account data
#
# Each alias has a distinct state directory and exclusive alias lock. Holders share the old lab live.lock,
# excluding externally locked browser/capture runs while allowing several ngteams aliases together.
# The deadline requests ordered shutdown; a stuck holder retains its locks and diagnostic state until it exits.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state_home="${XDG_STATE_HOME:-}"
[[ "$state_home" == /* ]] || state_home="$HOME/.local/state"
state="$state_home/ngteams-lab"
data_home="${XDG_DATA_HOME:-}"
[[ "$data_home" == /* ]] || data_home="$HOME/.local/share"
config_home="${XDG_CONFIG_HOME:-}"
[[ "$config_home" == /* ]] || config_home="$HOME/.config"
# Existing externally flocked browser runs use this fixed path, even with a custom XDG_DATA_HOME.
live_lock="$HOME/.local/share/teams-lab/live.lock"
runner="$root/target/lab/debug/ui-sandbox-run"
teams="$root/target/lab/debug/ngteams"

die() {
    echo "run-lab: $*" >&2
    exit 1
}

usage() { sed -n '2,19s/^# \{0,1\}//p' "$0"; }

build() {
    echo "building ngteams and ui-sandbox-run (target/lab)..."
    (exec 8>&- 9>&-; cd "$root" && timeout 1800 env -u TEAMS_PERSONAL_INSTALL TEAMS_LAB_BUILD=1 \
        cargo build --locked -p ngteams -p ui-sandbox --bin ngteams --bin ui-sandbox-run --target-dir "$root/target/lab")
}

holder_pid() {
    [[ -f "$run/holder.pid" ]] || return 1
    local pid
    read -r pid <"$run/holder.pid"
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
    echo "$pid"
}

alive() {
    local pid="$1" status
    kill -0 "$pid" 2>/dev/null || return 1
    status="$(ps -p "$pid" -o stat= 2>/dev/null)" || return 1
    [[ -n "$status" && "$status" != Z* ]]
}

running() {
    local pid
    [[ -S "$run/control.sock" && -f "$state/.locks/$alias.lock" ]] || return 1
    # A retained PID can belong to an unrelated process after PID reuse. Only a holder owning this alias is live.
    if flock -xn "$state/.locks/$alias.lock" true; then return 1; fi
    pid="$(holder_pid)" && alive "$pid"
}

where() {
    cat <<EOF
alias:        $alias
run:          $run
screenshots:  $run/shots/
view trace:   $run/trace.jsonl
app log:      $data_home/ngteams-lab/$alias/logs/  and  $run/app.log
broker log:   $run/broker.log
holder log:   $run/holder.log
EOF
}

ask() {
    running || die "not running: start it with $0 $alias"
    timeout 45 "$runner" "$@" --dir "$run"
}

wait_stopped() {
    local pid="$1"
    for _ in {1..200}; do
        alive "$pid" || return 0
        sleep 0.1
    done
    return 1
}

interrupt_start() {
    trap - INT TERM
    local pid
    pid="$(holder_pid)" || pid="$launched"
    kill -TERM "$pid" 2>/dev/null || true
    if wait_stopped "$pid"; then
        wait "$launched" 2>/dev/null || true
        echo "run-lab: interrupted startup; holder stopped (state kept: $run)" >&2
    else
        echo "run-lab: interrupted startup; holder $pid still cleaning up (state kept: $run)" >&2
    fi
    exit 130
}

stop() {
    local pid
    if ! running; then
        echo "not running"
        return
    fi
    pid="$(holder_pid)"
    timeout 45 "$runner" stop --dir "$run" >/dev/null ||
        die "could not ask the holder to stop; retained state: $run"
    wait_stopped "$pid" || die "holder $pid did not finish cleanup; retained state: $run"
    echo "stopped $alias (logs kept in $run)"
}

start() {
    local pipewire="$1" seconds="$2" pid launched
    install -d -m 0700 "$state" "$state/.locks"
    exec 8>"$state/.locks/$alias.lock"
    flock -xn 8 || die "already running or starting: $alias"
    # The exclusive lock proves no holder remains; discard its stale PID before build/status can inspect it.
    rm -f -- "$run/holder.pid"
    build
    install -d -m 0700 "$(dirname "$live_lock")"
    exec 9>"$live_lock"
    flock -sn 9 || die "the lab live lock is held exclusively; finish that run first"
    # Keep files: XDG state and data homes may coincide, including this alias's accounts and stores.
    install -d -m 0700 "$run"
    local media=()
    [[ "$pipewire" != yes ]] || media+=(--pipewire)
    # timeout signals only serve, whose pass_only boundary closes lock FDs in broker/app; cleanup retains locks.
    setsid timeout --foreground --signal=TERM "$seconds" "$runner" serve --dir "$run" "${media[@]}" \
        --broker "$teams" \
        --broker-arg secrets --broker-arg --lab --broker-arg "$alias" --broker-arg --fd --broker-arg '{broker-fd}' \
        --app /usr/bin/env \
        --app-arg "XDG_DATA_HOME=$data_home" --app-arg "XDG_CONFIG_HOME=$config_home" --app-arg "$teams" \
        --app-arg --lab --app-arg "$alias" \
        --app-arg --secrets-fd --app-arg '{secrets-fd}' --app-arg --trace-fd --app-arg '{trace-fd}' \
        >"$run/holder.log" 2>&1 </dev/null &
    launched=$!
    trap interrupt_start INT TERM
    # Close duplicates, never unlock the shared open-file descriptions now owned by serve.
    exec 8>&- 9>&-
    for _ in {1..600}; do
        if [[ -S "$run/control.sock" ]] && grep -q '^ready sandbox=' "$run/holder.log" && running; then
            echo "running lab account $alias"
            where
            trap - INT TERM
            return
        fi
        if ! alive "$launched" && ! running; then
            wait "$launched" 2>/dev/null || true
            trap - INT TERM
            die "sandbox failed to start: $run/holder.log"
        fi
        sleep 0.1
    done
    # This is our newly created run, so the recorded holder (or not-yet-exec'd child) is ours to terminate.
    pid="$(holder_pid)" || pid="$launched"
    kill -TERM "$pid" 2>/dev/null || true
    wait_stopped "$pid" || die "startup timed out and holder $pid did not finish cleanup; retained state: $run"
    wait "$launched" 2>/dev/null || true
    trap - INT TERM
    die "sandbox did not become ready; stopped its holder: $run/holder.log"
}

case "${1:-}" in
"" | -h | --help) usage; exit 0 ;;
--build-only) [[ $# == 1 ]] || die "--build-only takes no arguments"; build; exit 0 ;;
-*) die "unknown option $1 (see --help)" ;;
esac

alias="$1"
[[ "$alias" =~ ^[a-zA-Z0-9_-]+$ ]] || die "an alias is ASCII letters, digits, '_' and '-'"
run="$state/$alias"
shift
case "${1:-}" in
"" | --pipewire | --timeout)
    pipewire=no
    seconds=1800
    while (($#)); do
        case "$1" in
        --pipewire) pipewire=yes; shift ;;
        --timeout)
            [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--timeout needs positive whole seconds"
            seconds="$2"
            shift 2
            ;;
        *) die "unknown start option $1 (see --help)" ;;
        esac
    done
    start "$pipewire" "$seconds"
    ;;
--status)
    [[ $# == 1 ]] || die "--status takes no arguments"
    if running; then echo "running"; else echo "not running"; fi
    where
    ;;
--stop) [[ $# == 1 ]] || die "--stop takes no arguments"; stop ;;
--shot | --screen | --key | --type)
    [[ $# == 2 ]] || die "$1 needs one argument"
    ask "${1#--}" "$2"
    ;;
--click) [[ $# == 3 ]] || die "--click needs x and y"; ask click "$2" "$3" ;;
--clipboard)
    shift
    (($# >= 2 && $# % 2 == 0)) || die "--clipboard needs <mime> <path> pairs"
    offers=()
    while (($#)); do
        [[ "$1" =~ ^[^[:space:]/]+/[^[:space:]]+$ ]] || die "not a MIME type: $1"
        # The runner reads the file, from its own directory: forward an absolute path, which the control line
        # (words split at whitespace) carries only without whitespace.
        file="$(realpath -e -- "$2")" || die "no such file: $2"
        [[ "$file" != *[[:space:]]* ]] || die "a clipboard file's path may not contain whitespace: $file"
        offers+=("$1" "$file")
        shift 2
    done
    ask clipboard "${offers[@]}"
    ;;
--windows) [[ $# == 1 ]] || die "--windows takes no arguments"; ask windows ;;
-h | --help) usage ;;
*) die "unknown option $1 (see --help)" ;;
esac
