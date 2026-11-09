#!/bin/sh
set -eu
# Load the profile that lets the sign-in window's WebKit sandbox get a user namespace where the kernel restricts them.
if [ "$(cat /sys/module/apparmor/parameters/enabled 2>/dev/null || true)" = Y ] \
  && [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo 0)" != 0 ] \
  && command -v apparmor_parser >/dev/null 2>&1; then
  apparmor_parser -r /etc/apparmor.d/ngteams-release
fi
