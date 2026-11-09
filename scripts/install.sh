#!/usr/bin/env bash
# Builds the personal build (`crates/ngteams`, docs/personal-build.md) as a release and installs it under ~/.local, for
# the user to run on their own desktop. **Agents run this script only when the user asks** (AGENTS.md): it is the only
# thing that sets the install marker, without which `ngteams` refuses to start.
#
# - Requires the system's `wpewebkit` (2.54 or newer, docs/development.md "WPE WebKit") for the sign-in window's page: the
#   build links it and the installed binary runs its helper processes, so without it the script stops before building.
# - Builds `ngteams` with `cargo build --release -p ngteams`, its target dir apart from every other build
#   (`target/personal`), so no `test-support` or lab build is unified in, and beside it `ngteams-web`, the sign-in
#   window's page engine (the only program linking WebKit). Then `ngteamsctl` (`crates/teams-ctl`), which
#   drives the running app over its control socket, in a build of its own into the same target dir.
# - Installs it and the sign-in window's web process extension into ~/.local/lib/ngteams/ (the app finds the extension
#   beside itself), `ngteams` linked from ~/.local/bin/, `ngteamsctl` beside it, linked from ~/.local/bin/ too, with
#   its bash, fish and zsh completions under ~/.local/share/ (zsh reads `site-functions` only when it is on `fpath`),
#   and `packaging/com.nickalie.ngteams.desktop` into
#   ~/.local/share/applications/ with its `Exec` pointing at the installed binary (it handles `msteams:` links,
#   handed to the app running: `crates/ngteams/src/handoff.rs`), and the app icon (`packaging/icons/`) into
#   ~/.local/share/icons/hicolor/. What an older install left that no longer exists goes
#   (`ngteams.desktop`, the separate sign-in window's `ngteams-signin` and its desktop entry).
# - Starts nothing. The first run is the user's: `ngteams`, which asks for the account (docs/personal-build.md,
#   "The first run").
#
# Usage: scripts/install.sh   (PREFIX overrides ~/.local)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
target="$root/target/personal"

cd "$root"
if ! pkg-config --atleast-version=2.54 wpe-webkit-2.0 wpe-web-process-extension-2.0 2> /dev/null; then
  echo "error: the sign-in window needs the system's WPE WebKit 2.54 or newer (the wpewebkit package)," >&2
  echo "       which isn't installed or is older: sudo pacman -S wpewebkit, then run this again" >&2
  exit 1
fi
TEAMS_PERSONAL_INSTALL=1 cargo build --release -p ngteams -p teams-wpe --bin ngteams --bin ngteams-web --target-dir "$target"
cargo build --release -p teams-ctl --bin ngteamsctl --target-dir "$target"

lib="$prefix/lib/ngteams"
bin="$prefix/bin"
apps="$prefix/share/applications"
install -d -m 0755 "$lib" "$bin" "$apps"
install -m 0755 "$target/release/ngteams" "$lib/ngteams"
install -m 0755 "$target/release/ngteams-web" "$lib/ngteams-web"
extension="$target/release/libteams_signin_extension.so"
[[ -f "$extension" ]] || extension="$target/release/deps/libteams_signin_extension.so"
install -m 0644 "$extension" "$lib/libteams_signin_extension.so"
ln -sfn "$lib/ngteams" "$bin/ngteams"
install -m 0755 "$target/release/ngteamsctl" "$lib/ngteamsctl"
ln -sfn "$lib/ngteamsctl" "$bin/ngteamsctl"
# Where each shell looks in the user's data dir (bash-completion's and fish's own; zsh's once on `fpath`). Writing a
# script runs only `ngteamsctl completions`, which asks no app.
share="$prefix/share"
for completion in "bash bash-completion/completions/ngteamsctl" "fish fish/vendor_completions.d/ngteamsctl.fish" \
  "zsh zsh/site-functions/_ngteamsctl"; do
  shell="${completion%% *}"
  file="$share/${completion#* }"
  install -d -m 0755 "$(dirname "$file")"
  "$lib/ngteamsctl" completions "$shell" > "$file"
  chmod 0644 "$file"
done
desktop="com.nickalie.ngteams.desktop"
sed "s|^Exec=ngteams %u$|Exec=$lib/ngteams %u|" "$root/packaging/$desktop" > "$apps/$desktop"
chmod 0644 "$apps/$desktop"
# The entry handles `msteams:` links (a meeting's join link from the browser): the desktop's handler cache learns it
# where the tool is there; without it, desktops read the entries themselves.
if command -v update-desktop-database > /dev/null; then
  update-desktop-database -q "$apps" || true
fi
# What older installs left: the entry's name before the app id became `com.nickalie.ngteams`, and the sign-in
# window's binary and hidden entry from before the window moved into the app.
rm -f "$apps/ngteams.desktop" "$apps/com.nickalie.ngteams.signin.desktop" "$lib/ngteams-signin"
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
echo "           $lib/ngteamsctl (linked from $bin/ngteamsctl) and its completions under $share"
echo "first run: start Teams and enter your work account   (then review the first-run screen before anything is sent on its own)"
