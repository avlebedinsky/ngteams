#!/usr/bin/env bash
set -euo pipefail

# Privileged Docker can recreate host DRM nodes after mounting /dev/dri. A fresh /dev keeps every test on software
# graphics. Keep the container's large shared-memory mount, writable build/cache dirs and dependency-download
# network; all desktop, D-Bus, PipeWire and browser sessions are still created privately by the test harnesses.
exec bwrap --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts \
    --ro-bind / / --dev /dev --bind /dev/shm /dev/shm --proc /proc \
    --tmpfs /tmp --tmpfs /run --bind /home/ci /home/ci --bind /workspace /workspace \
    -- "$@"
