#!/usr/bin/env bash
# Builds the personal build (`crates/teams`, docs/personal-build.md) as a release and installs it under ~/.local, for
# the user to run on their own desktop. **Agents never run this script** (CLAUDE.md): it is the only thing that sets
# the install marker, without which both personal binaries refuse to start.
#
# - Builds `teams` and `teams-signin-personal` with one `cargo build --release -p teams`, its target dir apart from
#   every other build (`target/personal`), so no `test-support` or lab build is unified in.
# - Installs both binaries and the sign-in window's web process extension into ~/.local/lib/teams/ (the window finds
#   the extension beside itself, `teams` finds the window beside itself), `teams` linked from ~/.local/bin/, and
#   `packaging/teams.desktop` into ~/.local/share/applications/ with its `Exec` pointing at the installed binary.
# - Starts nothing. The first run is the user's: `teams --account <your UPN>` (docs/personal-build.md, "The first run").
#
# Usage: scripts/install-personal.sh   (PREFIX overrides ~/.local)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
target="$root/target/personal"

cd "$root"
TEAMS_PERSONAL_INSTALL=1 cargo build --release -p teams --target-dir "$target"

lib="$prefix/lib/teams"
bin="$prefix/bin"
apps="$prefix/share/applications"
install -d -m 0755 "$lib" "$bin" "$apps"
install -m 0755 "$target/release/teams" "$lib/teams"
install -m 0755 "$target/release/teams-signin-personal" "$lib/teams-signin-personal"
extension="$target/release/libteams_signin_extension.so"
[[ -f "$extension" ]] || extension="$target/release/deps/libteams_signin_extension.so"
install -m 0644 "$extension" "$lib/libteams_signin_extension.so"
ln -sfn "$lib/teams" "$bin/teams"
sed "s|^Exec=teams$|Exec=$lib/teams|" "$root/packaging/teams.desktop" > "$apps/teams.desktop"
chmod 0644 "$apps/teams.desktop"

echo "installed: $lib/teams (linked from $bin/teams) and $apps/teams.desktop"
echo "first run: teams --account <your UPN>   (then review the first-run screen before anything is sent on its own)"
