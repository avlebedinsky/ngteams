#!/usr/bin/env bash
set -euo pipefail

# Run before bootstrap, then again with the installed Chrome path. No Rust build is needed.
if (( $# > 1 )); then
    echo "usage: smoke.sh [CHROME_PATH]" >&2
    exit 2
fi
if [[ ${TZ:-} != Etc/UTC && ${TZ:-} != UTC ]] || [[ $(date +%z) != +0000 ]]; then
    echo "CI preflight requires TZ=Etc/UTC so calendar fixtures use a deterministic timezone." >&2
    exit 1
fi
if [[ -e /dev/dri ]]; then
    # CI mounts an empty /dev/dri; no hardware render node may enter these CPU tests.
    if compgen -G '/dev/dri/*' >/dev/null; then
        echo "CI preflight found a DRM device in the CPU-only container." >&2
        exit 1
    fi
fi

if ! timeout --kill-after=5s 10s unshare --user --map-root-user true; then
    echo "CI preflight cannot map a user namespace; check the hosted VM's AppArmor namespace policy." >&2
    exit 1
fi
if ! timeout --kill-after=5s 10s bwrap --unshare-all --die-with-parent \
    --ro-bind / / --dev /dev --proc /proc -- true; then
    echo "CI preflight cannot create WebKit's required bubblewrap namespaces." >&2
    exit 1
fi
echo "CI preflight: UTC, CPU-only devices and sandbox namespaces are ready."

if (( $# == 0 )); then
    exit 0
fi
chrome=$1
if [[ $chrome != /* || ! -x $chrome ]]; then
    echo "CI preflight needs an absolute path to the installed Chrome executable." >&2
    exit 1
fi

smoke_dir=$(mktemp -d /tmp/ngteams-ci-smoke.XXXXXX)
trap 'rm -rf -- "$smoke_dir"' EXIT
mkdir "$smoke_dir/home"

# Like the headless browser tests, this has no visible GUI. The extra outer sandbox gives it no network,
# desktop/session environment or existing profile. Chrome's own sandbox remains enabled.
if ! timeout --kill-after=5s 30s bwrap --unshare-all --die-with-parent \
    --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp --tmpfs /run \
    --bind "$smoke_dir" "$smoke_dir" --clearenv \
    --setenv PATH /usr/bin --setenv HOME "$smoke_dir/home" --setenv TZ Etc/UTC \
    -- "$chrome" \
    --headless=new --user-data-dir="$smoke_dir/profile" \
    --no-first-run --no-default-browser-check --password-store=basic \
    --disable-background-networking --disable-component-update --disable-default-apps \
    --disable-extensions --disable-field-trial-config --disable-sync \
    --disable-features=Reporting,NetworkErrorLogging \
    --disable-breakpad --disable-crash-reporter --disable-audio-output --mute-audio \
    --dump-dom 'data:text/html,<title>ngteams-ci-smoke</title><p>browser ready</p>' \
    >"$smoke_dir/dom.html" 2>"$smoke_dir/chrome.stderr"; then
    echo "CI preflight: sandboxed Chrome failed to render a synthetic page:" >&2
    cat "$smoke_dir/chrome.stderr" >&2
    exit 1
fi
if ! grep -Fq '<title>ngteams-ci-smoke</title>' "$smoke_dir/dom.html"; then
    echo "CI preflight: Chrome did not return the synthetic page:" >&2
    cat "$smoke_dir/chrome.stderr" "$smoke_dir/dom.html" >&2
    exit 1
fi
echo "CI preflight: sandboxed Chrome rendered a synthetic page without a GPU or network."
