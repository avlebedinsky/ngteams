#!/usr/bin/env bash
# Offline coverage: one selection at a time, with isolated instrumentation artifacts.
set -euo pipefail
caller_dir=$PWD
cd "$(dirname "$0")/.."

packages=()
report_args=()
report_only=false
test_threads=1
profile_mode=ordinary
[[ $(uname -s) != Linux ]] || profile_mode=continuous
webrtc_cache=
while (($#)); do
    case "$1" in
        -p|--package)
            [[ $# -ge 2 ]] || { echo "missing package name" >&2; exit 2; }
            packages+=("$2")
            shift 2
            ;;
        --no-gate|--require-target)
            report_args+=("$1")
            shift
            ;;
        --report-only)
            report_only=true
            shift
            ;;
        --test-threads)
            [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || {
                echo "--test-threads requires a positive integer" >&2
                exit 2
            }
            test_threads=$2
            shift 2
            ;;
        --profile-mode)
            [[ $# -ge 2 && ( "$2" == continuous || "$2" == ordinary ) ]] || {
                echo "--profile-mode requires continuous or ordinary" >&2; exit 2;
            }
            profile_mode=$2
            shift 2
            ;;
        --webrtc-cache)
            webrtc_cache=${2:-}
            if [[ -n "$webrtc_cache" && "$webrtc_cache" != /* ]]; then
                webrtc_cache="$caller_dir/$webrtc_cache"
            fi
            [[ -n "$webrtc_cache" && -d "$webrtc_cache/include" && -f "$webrtc_cache/lib/libwebrtc.a" ]] || {
                echo "--webrtc-cache requires an SDK directory with include/ and lib/libwebrtc.a" >&2
                exit 2
            }
            webrtc_cache=$(cd -- "$webrtc_cache" && pwd -P)
            shift 2
            ;;
        -h|--help)
            echo "Usage: scripts/coverage.sh [-p crate ...] [--no-gate|--require-target] [--report-only] [--test-threads N] [--profile-mode continuous|ordinary] [--webrtc-cache DIR]"
            exit 0
            ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
mapfile -t selected < <(python3 scripts/coverage-report.py list "${packages[@]}")
((${#selected[@]})) || { echo "no coverage packages selected" >&2; exit 2; }
package_args=()
for package in "${selected[@]}"; do package_args+=(-p "$package"); done
selection=$(python3 scripts/coverage-report.py directory "${selected[@]}")
output="target/coverage/$selection"
build_dir=target/coverage-build
if [[ "$profile_mode" == continuous ]]; then
    [[ $(uname -s) == Linux ]] || { echo "continuous coverage is verified only on Linux" >&2; exit 2; }
    output="target/coverage/continuous/$selection"
    build_dir=target/coverage-continuous-build
fi
mkdir -p "$output"
exec 9>target/coverage/measurement.lock
flock 9
if ! "$report_only"; then
    # llvm-cov creates this directory before Cargo does, so Cargo may leave its cache tag absent.
    cache_tag="$build_dir/CACHEDIR.TAG"
    mkdir -p "$build_dir"
    if [[ ! -e "$cache_tag" ]]; then
        printf '%s\n' 'Signature: 8a477f597d28d172789f06886806bc55' > "$cache_tag"
    elif ! head -n 1 "$cache_tag" | grep -Fxq 'Signature: 8a477f597d28d172789f06886806bc55'; then
        echo "invalid cache signature: $cache_tag; inspect the directory before measuring" >&2
        exit 2
    fi
    build_env=()
    if [[ "$profile_mode" == continuous ]]; then
        # LLVM's mapped counters survive killed sandbox workers. Keep Cargo's encoded flags
        # precedence, including an explicitly empty value; never split existing encoded args.
        if [[ ${CARGO_ENCODED_RUSTFLAGS+x} ]]; then
            flags=${CARGO_ENCODED_RUSTFLAGS}
            [[ -z "$flags" ]] || flags+=$'\x1f'
            build_env+=("CARGO_ENCODED_RUSTFLAGS=${flags}"$'-C\x1fllvm-args=-runtime-counter-relocation')
        else
            build_env+=("RUSTFLAGS=${RUSTFLAGS:+$RUSTFLAGS }-C llvm-args=-runtime-counter-relocation")
        fi
        build_env+=('LLVM_PROFILE_FILE_NAME=%p-%m%c.profraw')
    else
        # An inherited continuous pattern must not contaminate the ordinary build.
        build_env+=('LLVM_PROFILE_FILE_NAME=%p-%m.profraw')
    fi
    [[ -z "$webrtc_cache" ]] || build_env+=("LK_CUSTOM_WEBRTC=$webrtc_cache")
    # Never enable the live feature or accept cargo arguments that could enable it.
    env -u PROTO_LAB_LIVE -u TEAMS_MEDIA_TURN_SERVER -u TEAMS_MEDIA_TURN_USER \
        -u TEAMS_MEDIA_TURN_PASSWORD CARGO_LLVM_COV_TARGET_DIR="$PWD/$build_dir" \
        PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 \
        "${build_env[@]}" cargo llvm-cov "${package_args[@]}" --locked --json --output-path "$output/llvm.json" \
        -- --test-threads "$test_threads"
fi
python3 scripts/coverage-report.py report "$output/llvm.json" \
    --output "$output/summary.json" "${report_args[@]}" --packages "${selected[@]}"
