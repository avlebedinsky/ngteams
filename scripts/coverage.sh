#!/usr/bin/env bash
# Offline coverage: one selection at a time, with isolated instrumentation artifacts.
set -euo pipefail
cd "$(dirname "$0")/.."

packages=()
report_args=()
report_only=false
test_threads=1
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
        -h|--help)
            echo "Usage: scripts/coverage.sh [-p crate ...] [--no-gate|--require-target] [--report-only] [--test-threads N]"
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
mkdir -p "$output"
exec 9>target/coverage/measurement.lock
flock 9
if ! "$report_only"; then
    # llvm-cov creates this directory before Cargo does, so Cargo may leave its cache tag absent.
    cache_tag=target/coverage-build/CACHEDIR.TAG
    mkdir -p target/coverage-build
    if [[ ! -e "$cache_tag" ]]; then
        printf '%s\n' 'Signature: 8a477f597d28d172789f06886806bc55' > "$cache_tag"
    elif ! head -n 1 "$cache_tag" | grep -Fxq 'Signature: 8a477f597d28d172789f06886806bc55'; then
        echo "invalid cache signature: $cache_tag; inspect the directory before measuring" >&2
        exit 2
    fi
    # Never enable the live feature or accept cargo arguments that could enable it.
    env -u PROTO_LAB_LIVE -u TEAMS_MEDIA_TURN_SERVER -u TEAMS_MEDIA_TURN_USER \
        -u TEAMS_MEDIA_TURN_PASSWORD CARGO_LLVM_COV_TARGET_DIR="$PWD/target/coverage-build" \
        PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 \
        cargo llvm-cov "${package_args[@]}" --locked --json --output-path "$output/llvm.json" \
        -- --test-threads "$test_threads"
fi
python3 scripts/coverage-report.py report "$output/llvm.json" \
    --output "$output/summary.json" "${report_args[@]}" --packages "${selected[@]}"
