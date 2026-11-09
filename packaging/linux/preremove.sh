#!/bin/sh
set -eu
# nFPM passes remove for Debian, 0 for RPM and the old version for Arch.
# Only unload when the package is actually removed, keeping upgrades uninterrupted.
case "${1:-}" in
  upgrade|1) exit 0 ;;
esac
if [ "$(cat /sys/module/apparmor/parameters/enabled 2>/dev/null || true)" = Y ] \
  && command -v apparmor_parser >/dev/null 2>&1 \
  && [ -f /etc/apparmor.d/ngteams-release ]; then
  apparmor_parser -R /etc/apparmor.d/ngteams-release || true
fi
