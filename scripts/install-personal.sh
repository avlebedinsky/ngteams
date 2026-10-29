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
#   installed binary (an older install's `ngteams.desktop` removed).
# - Starts nothing. The first run is the user's: `ngteams`, which asks for the account (docs/personal-build.md,
#   "The first run").
#
# Usage: scripts/install-personal.sh   (PREFIX overrides ~/.local)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
target="$root/target/personal"

cd "$root"
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
sed "s|^Exec=ngteams$|Exec=$lib/ngteams|" "$root/packaging/$desktop" > "$apps/$desktop"
chmod 0644 "$apps/$desktop"
# The entry's name before the app id became `com.nickalie.ngteams`.
rm -f "$apps/ngteams.desktop"

echo "installed: $lib/ngteams (linked from $bin/ngteams) and $apps/$desktop"
echo "first run: start Teams and enter your work account   (then review the first-run screen before anything is sent on its own)"
