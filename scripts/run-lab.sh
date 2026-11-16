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
# srv-<x> aliases (srv-a, srv-b, srv-c) are synthetic users (srv-a@srv.lab.test, ...) on an own server
# (ngteams-server): one the runner starts on loopback (SQLite, files and its own CA under
# $XDG_STATE_HOME/ngteams-lab/.srv-server), or the running one NGTEAMS_LAB_SERVER names (its https origin, e.g. a
# deployment of several nodes; NGTEAMS_LAB_SERVER_CA its CA file when the system doesn't trust it, and
# NGTEAMS_LAB_SERVER_CONFIG a node's config, which the runner creates the users with). The runner creates the srv
# users, keeps their passwords 0600 beside the server's data and signs the alias in before its window opens
# (`ngteams sign-in`, the password on standard input): no password is typed. No Microsoft tenant is reached, so the
# lab-contact check is local: the srv users on that server. --stop of the last running srv alias stops the server
# the runner started.
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
server_bin="$root/target/lab/debug/ngteams-server"
srv_domain="srv.lab.test"

die() {
    echo "run-lab: $*" >&2
    exit 1
}

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }

# build [server]: ngteams-server too, for a srv alias
build() {
    local server=()
    [[ "${1:-}" != server ]] || server=(-p ngteams-server --bin ngteams-server)
    echo "building ngteams and ui-sandbox-run${server[*]:+ and ngteams-server} (target/lab)..."
    (exec 7>&- 8>&- 9>&-; cd "$root" && timeout 1800 env -u TEAMS_PERSONAL_INSTALL TEAMS_LAB_BUILD=1 \
        cargo build --locked -p ngteams -p teams-wpe -p ui-sandbox "${server[@]}" --bin ngteams --bin ngteams-web \
        --bin ui-sandbox-run --target-dir "$root/target/lab")
}

# --- srv aliases: synthetic users on an own server (see the usage) ---

srv_alias() { [[ "$1" == srv-* ]]; }

# the server, its users' passwords and its logs: the runner's own, or a directory per NGTEAMS_LAB_SERVER origin
srv_home() {
    if [[ -n "${NGTEAMS_LAB_SERVER:-}" ]]; then
        echo "$state/.srv-$(printf '%s' "${NGTEAMS_LAB_SERVER%/}" | sha256sum | cut -c1-12)"
    else
        echo "$state/.srv-server"
    fi
}

srv_lock() {
    install -d -m 0700 "$state" "$state/.locks"
    exec 7>"$state/.locks/.srv-server.lock"
    flock -x -w 120 7 || die "another run holds the srv server's lock"
}

# the PID of the runner's own server while it runs
srv_pid() {
    local pid
    [[ -f "$srv_dir/server.pid" ]] || return 1
    read -r pid <"$srv_dir/server.pid"
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] && alive "$pid" || return 1
    # a reused PID is another program's
    tr '\0' '\n' <"/proc/$pid/cmdline" 2>/dev/null | grep -qxF -- "$server_bin" || return 1
    echo "$pid"
}

srv_certificates() {
    (
        umask 077
        cd "$srv_dir"
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout ca.key -out ca.pem \
            -days 3650 -subj "/CN=ngteams lab srv CA" -addext "basicConstraints=critical,CA:TRUE" \
            -addext "keyUsage=critical,keyCertSign,cRLSign"
        openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout key.pem -subj "/CN=127.0.0.1" \
            -out leaf.csr
        printf '%s\n' "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" \
            "extendedKeyUsage=serverAuth" "subjectAltName=IP:127.0.0.1" >leaf.ext
        openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -days 825 -extfile leaf.ext -out cert.pem
        rm -f leaf.csr leaf.ext
    ) >>"$srv_dir/setup.log" 2>&1 || die "the srv server's certificates can't be made: $srv_dir/setup.log"
}

srv_free_port() {
    local port
    for _ in {1..50}; do
        port=$((20000 + RANDOM % 40000))
        if [[ -z "$(ss -Htln "sport = :$port" 2>/dev/null)" ]]; then
            echo "$port"
            return
        fi
    done
    die "no free port for the srv server"
}

# the runner's own server on loopback, started unless it runs: sets srv_origin, srv_ca, srv_config, srv_fresh
srv_local_up() {
    local port pid
    install -d -m 0700 "$srv_dir" "$srv_dir/files"
    [[ -s "$srv_dir/ca.pem" && -s "$srv_dir/cert.pem" ]] || srv_certificates
    if [[ -s "$srv_dir/port" ]]; then
        read -r port <"$srv_dir/port"
    else
        port="$(srv_free_port)"
        echo "$port" >"$srv_dir/port"
    fi
    [[ "$port" =~ ^[1-9][0-9]*$ ]] || die "not a port: $srv_dir/port"
    srv_origin="https://127.0.0.1:$port"
    srv_ca="$srv_dir/ca.pem"
    srv_config="$srv_dir/server.toml"
    [[ -f "$srv_dir/server.sqlite" ]] || srv_fresh=yes
    printf '%s\n' "listen = \"127.0.0.1:$port\"" "public_origin = \"$srv_origin\"" \
        "database_url = \"sqlite://$srv_dir/server.sqlite\"" "storage_url = \"file://$srv_dir/files\"" "" \
        "[tls]" "cert = \"$srv_dir/cert.pem\"" "key = \"$srv_dir/key.pem\"" "" \
        "[mixer]" "udp_listen = \"127.0.0.1:0\"" >"$srv_config"
    srv_pid >/dev/null && return
    "$server_bin" --config "$srv_config" migrate >>"$srv_dir/server.log" 2>&1 ||
        die "the srv server's database can't be made: $srv_dir/server.log"
    # a session of its own and none of the runner's lock descriptors: it outlives this start, until the last srv --stop
    (
        exec 7>&- 8>&- 9>&-
        cd "$srv_dir" || exit 1
        setsid "$server_bin" --config "$srv_config" serve >>"$srv_dir/server.log" 2>&1 </dev/null &
        echo $! >"$srv_dir/server.pid"
    )
    for _ in {1..600}; do
        curl -fsS -o /dev/null --max-time 2 --cacert "$srv_ca" "$srv_origin/health/ready" 2>/dev/null && return
        pid="$(srv_pid)" || die "the srv server didn't start: $srv_dir/server.log"
        sleep 0.1
    done
    kill -TERM "$pid" 2>/dev/null || true
    die "the srv server didn't become ready: $srv_dir/server.log"
}

srv_external() {
    srv_origin="${NGTEAMS_LAB_SERVER%/}"
    [[ "$srv_origin" =~ ^https://[^/[:space:]]+$ ]] || die "NGTEAMS_LAB_SERVER is an https origin: $srv_origin"
    srv_ca="${NGTEAMS_LAB_SERVER_CA:-}"
    if [[ -n "$srv_ca" ]]; then
        srv_ca="$(realpath -e -- "$srv_ca")" || die "no such file: NGTEAMS_LAB_SERVER_CA"
    fi
    srv_config="${NGTEAMS_LAB_SERVER_CONFIG:-}"
    [[ -f "$srv_config" ]] ||
        die "NGTEAMS_LAB_SERVER needs NGTEAMS_LAB_SERVER_CONFIG, a node's config, to create the srv users"
    install -d -m 0700 "$srv_dir"
}

srv_cli() { "$server_bin" --config "$srv_config" "$@" 2>>"$srv_dir/setup.log"; }

# srv-a, srv-b, srv-c and this alias, each with a password of its own kept 0600 beside the server
srv_users() {
    local listed user upn pw made name users=(srv-a srv-b srv-c)
    [[ " ${users[*]} " == *" $alias "* ]] || users+=("$alias")
    install -d -m 0700 "$srv_dir/passwords"
    listed="$(srv_cli user list)" || die "the srv server's users can't be listed: $srv_dir/setup.log"
    for user in "${users[@]}"; do
        upn="$user@$srv_domain"
        pw="$srv_dir/passwords/$user"
        made=no
        if [[ ! -s "$pw" ]]; then
            (umask 077; head -c 24 /dev/urandom | base64 | tr '+/' '-_' >"$pw")
            made=yes
        fi
        if ! grep -q "^$upn"$'\t' <<<"$listed"; then
            name="${user#srv-}"
            srv_cli user add "$upn" --name "Srv ${name^^}" <"$pw" >/dev/null ||
                die "the srv user $upn can't be made: $srv_dir/setup.log"
        elif [[ "$made" == yes ]]; then
            srv_cli user reset-password "$upn" <"$pw" >/dev/null ||
                die "the srv user $upn's password can't be set: $srv_dir/setup.log"
        fi
    done
}

# the alias's one account, on that server: written afresh when it names another server or the server is new
srv_account() {
    local dir="$data_home/ngteams-lab/$alias" file
    file="$dir/accounts.toml"
    if [[ "$srv_fresh" == yes || ! -f "$file" ]] || ! grep -qxF "server = \"$srv_origin\"" "$file"; then
        install -d -m 0700 "$dir"
        (
            umask 077
            printf '[[account]]\nupn = "%s"\nserver = "%s"\n' "$alias@$srv_domain" "$srv_origin"
            [[ -z "$srv_ca" ]] || printf "ca_file = '%s'\n" "$srv_ca"
        ) >"$file.new"
        mv -f -- "$file.new" "$file"
    fi
}

srv_prepare() {
    srv_lock
    srv_fresh=no
    if [[ -n "${NGTEAMS_LAB_SERVER:-}" ]]; then srv_external; else srv_local_up; fi
    srv_users
    srv_account
    (exec 7>&- 8>&- 9>&-; env XDG_DATA_HOME="$data_home" XDG_CONFIG_HOME="$config_home" "$teams" sign-in \
        --lab "$alias" <"$srv_dir/passwords/$alias") || die "$alias couldn't sign in on $srv_origin"
    exec 7>&-
}

# after a srv alias's --stop: the runner's own server stops once no srv alias runs
srv_stop_unused() {
    local other pid
    [[ -z "${NGTEAMS_LAB_SERVER:-}" ]] || return 0
    srv_lock
    for other in "$state"/srv-*; do
        [[ -d "$other" ]] || continue
        if running "${other##*/}"; then
            exec 7>&-
            return 0
        fi
    done
    if pid="$(srv_pid)"; then
        kill -TERM "$pid" 2>/dev/null || true
        for _ in {1..300}; do
            alive "$pid" || break
            sleep 0.1
        done
        ! alive "$pid" || kill -KILL "$pid" 2>/dev/null || true
        echo "stopped the srv server (its data kept in $srv_dir)"
    fi
    rm -f -- "$srv_dir/server.pid"
    exec 7>&-
}

# holder_pid [<run dir>], this alias's by default
holder_pid() {
    local dir="${1:-$run}" pid
    [[ -f "$dir/holder.pid" ]] || return 1
    read -r pid <"$dir/holder.pid"
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
    echo "$pid"
}

alive() {
    local pid="$1" status
    kill -0 "$pid" 2>/dev/null || return 1
    status="$(ps -p "$pid" -o stat= 2>/dev/null)" || return 1
    [[ -n "$status" && "$status" != Z* ]]
}

# running [<alias>], this one by default
running() {
    local name="${1:-$alias}" pid
    local dir="$state/$name"
    [[ -S "$dir/control.sock" && -f "$state/.locks/$name.lock" ]] || return 1
    # A retained PID can belong to an unrelated process after PID reuse. Only a holder owning this alias is live.
    if flock -xn "$state/.locks/$name.lock" true; then return 1; fi
    pid="$(holder_pid "$dir")" && alive "$pid"
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
ctl socket:   $run/ctl.sock  (ngteamsctl --lab $alias)
EOF
    if srv_alias "$alias"; then
        echo "own server:   ${NGTEAMS_LAB_SERVER:-on loopback, run by this runner}  (passwords and logs: $srv_dir/)"
    fi
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
    else
        pid="$(holder_pid)"
        timeout 45 "$runner" stop --dir "$run" >/dev/null ||
            die "could not ask the holder to stop; retained state: $run"
        wait_stopped "$pid" || die "holder $pid did not finish cleanup; retained state: $run"
        echo "stopped $alias (logs kept in $run)"
    fi
    ! srv_alias "$alias" || srv_stop_unused
}

start() {
    local pipewire="$1" seconds="$2" pid launched
    install -d -m 0700 "$state" "$state/.locks"
    exec 8>"$state/.locks/$alias.lock"
    flock -xn 8 || die "already running or starting: $alias"
    # The exclusive lock proves no holder remains; discard its stale PID before build/status can inspect it.
    rm -f -- "$run/holder.pid"
    if srv_alias "$alias"; then build server; else build; fi
    install -d -m 0700 "$(dirname "$live_lock")"
    exec 9>"$live_lock"
    flock -sn 9 || die "the lab live lock is held exclusively; finish that run first"
    # Keep files: XDG state and data homes may coincide, including this alias's accounts and stores.
    install -d -m 0700 "$run"
    local media=()
    [[ "$pipewire" != yes ]] || media+=(--pipewire)
    ! srv_alias "$alias" || srv_prepare
    # timeout signals only serve, whose pass_only boundary closes lock FDs in broker/app; cleanup retains locks.
    # The sandbox clears the app's environment, so it is told where its control socket is: the run dir, where
    # teams_ctl::socket_path(Mode::Lab) looks for it.
    setsid timeout --foreground --signal=TERM "$seconds" "$runner" serve --dir "$run" "${media[@]}" \
        --broker "$teams" \
        --broker-arg secrets --broker-arg --lab --broker-arg "$alias" --broker-arg --fd --broker-arg '{broker-fd}' \
        --app /usr/bin/env \
        --app-arg "XDG_DATA_HOME=$data_home" --app-arg "XDG_CONFIG_HOME=$config_home" \
        --app-arg "$teams" \
        --app-arg --lab --app-arg "$alias" \
        --app-arg --secrets-fd --app-arg '{secrets-fd}' --app-arg --trace-fd --app-arg '{trace-fd}' \
        --app-arg --ctl-socket --app-arg "$run/ctl.sock" \
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
srv_dir="$(srv_home)"
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
