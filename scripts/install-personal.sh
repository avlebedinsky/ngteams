#!/usr/bin/env bash
# Builds the personal build (`crates/ngteams`, docs/personal-build.md) as a release and installs it under ~/.local, for
# the user to run on their own desktop. **Agents run this script only when the user asks** (CLAUDE.md): it is the only
# thing that sets the install marker, without which both personal binaries refuse to start.
#
# - Builds `ngteams` and `ngteams-signin` with one `cargo build --release -p ngteams`, its target dir apart from
#   every other build (`target/personal`), so no `test-support` or lab build is unified in.
# - Installs both binaries and the sign-in window's web process extension into ~/.local/lib/ngteams/ (the window finds
#   the extension beside itself, `ngteams` finds the window beside itself), `ngteams` linked from ~/.local/bin/, and
#   `packaging/com.nickalie.ngteams.desktop` into ~/.local/share/applications/ with its `Exec` pointing at the
#   installed binary (an older install's `ngteams.desktop` removed), beside it the sign-in window's hidden entry
#   (`com.nickalie.ngteams.signin.desktop`, so the desktop shows that window with the app's icon), and the app icon
#   (`packaging/icons/`) into ~/.local/share/icons/hicolor/.
# - Starts nothing. The first run is the user's: `ngteams`, which asks for the account (docs/personal-build.md,
#   "The first run").
#
# Usage: scripts/install-personal.sh   (PREFIX overrides ~/.local)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
target="$root/target/personal"

cd "$root"
# The calls' H.264 (OpenH264, built from its bundled source) gets its SIMD assembly only when `nasm` is there at its
# first build; plain C is too slow for a 720p camera at 30 fps (docs/protocol/calling.md, "The H.264 codec").
if ! command -v nasm > /dev/null; then
  echo "warning: nasm isn't installed: the calls' video codec is built without its SIMD code (slow at 720p);" >&2
  echo "         install nasm, then: cargo clean --release -p openh264-sys2 --target-dir $target, and run this again" >&2
fi
TEAMS_PERSONAL_INSTALL=1 cargo build --release -p ngteams --target-dir "$target"

lib="$prefix/lib/ngteams"
bin="$prefix/bin"
apps="$prefix/share/applications"
install -d -m 0755 "$lib" "$bin" "$apps"
install -m 0755 "$target/release/ngteams" "$lib/ngteams"
install -m 0755 "$target/release/ngteams-signin" "$lib/ngteams-signin"
extension="$target/release/libteams_signin_extension.so"
[[ -f "$extension" ]] || extension="$target/release/deps/libteams_signin_extension.so"
install -m 0644 "$extension" "$lib/libteams_signin_extension.so"
ln -sfn "$lib/ngteams" "$bin/ngteams"
desktop="com.nickalie.ngteams.desktop"
# the sign-in window's entry, hidden from menus: the desktop finds that window's icon by its app id
for entry in "$desktop" com.nickalie.ngteams.signin.desktop; do
  sed "s|^Exec=ngteams$|Exec=$lib/ngteams|" "$root/packaging/$entry" > "$apps/$entry"
  chmod 0644 "$apps/$entry"
done
# The entry's name before the app id became `com.nickalie.ngteams`.
rm -f "$apps/ngteams.desktop"
icons="$prefix/share/icons/hicolor"
for icon in "$root"/packaging/icons/hicolor/*/apps/com.nickalie.ngteams.png; do
  size="$(basename "$(dirname "$(dirname "$icon")")")"
  install -D -m 0644 "$icon" "$icons/$size/apps/com.nickalie.ngteams.png"
done
# A cache already there must learn the icon, or desktops reading it never find it; without one they scan the dir, and
# making one here would hide what other apps install later.
if [[ -f "$icons/icon-theme.cache" ]]; then
  gtk-update-icon-cache -q -t "$icons" || true
fi

echo "installed: $lib/ngteams (linked from $bin/ngteams), $apps/$desktop and its icon under $icons"
echo "first run: start Teams and enter your work account   (then review the first-run screen before anything is sent on its own)"
