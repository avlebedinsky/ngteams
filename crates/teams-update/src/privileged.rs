//! A system package's update (`.deb`, `.rpm`, Arch): the system's package manager installs it with administrator
//! rights, so its package records stay right, behind the desktop's password prompt (`pkexec`), while the app is still
//! open. A dismissed prompt changes nothing. After ngterm's (MIT), without its terminal fallback: a desktop with no
//! prompt gets the release page instead.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Error, Installation, error, prepare::hex};

/// What `pkexec` says, untranslated, when it can't show a prompt.
const NO_PROMPT_ERRORS: [&str; 3] = [
    "No authentication agent",
    "Error getting authority",
    "must be setuid root",
];

// Runs as root over a package in a folder its user can write: a copy in a root-owned folder is what is hashed and
// installed, so the package can't change between the check and the install. The identity is compared last, since a
// prompt can wait indefinitely and `pacman -U` would downgrade a newer installation. Arguments are positional, never
// interpolated into shell code.
pub(crate) const ROOT_SCRIPT: &str = r#"set -eu
package=$1; expected=$2; kind=$3; identity=$4; installed=$5
digest() {
  sum=$(sha256sum "$1")
  echo "${sum%% *}"
}
stage=$(mktemp -d /var/tmp/ngteams-update.XXXXXX)
trap 'rm -rf "$stage"' EXIT
file="$stage/${package##*/}"
cp "$package" "$file"
if [ "$(digest "$file")" != "$expected" ]; then
  echo 'The downloaded package changed after it was verified.' >&2
  exit 3
fi
chmod 755 "$stage"
chmod 644 "$file"
if [ "$(digest "$identity")" != "$installed" ]; then
  echo 'ngteams was updated by something else after this download started.' >&2
  exit 4
fi
case "$kind" in
  deb) DEBIAN_FRONTEND=noninteractive apt-get install -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$file" ;;
  rpm)
    if command -v dnf >/dev/null 2>&1; then dnf install -y "$file"
    elif command -v zypper >/dev/null 2>&1; then zypper --non-interactive --no-gpg-checks install --allow-unsigned-rpm "$file"
    else rpm -U "$file"
    fi ;;
  pacman) pacman -U --noconfirm "$file" ;;
  *) echo "Unknown package kind: $kind" >&2; exit 2 ;;
esac
"#;

/// How the password prompt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prompted {
    Installed,
    /// The user closed it; nothing changed.
    Cancelled,
    /// This desktop can't show one; nothing changed.
    Unavailable,
}

/// Installs `package` (of `kind`) through the prompt, waiting for it; `identity` is the installation's when the
/// download started.
pub(crate) fn prompt(
    installation: &Installation,
    kind: &str,
    package: &Path,
    identity: &[u8],
) -> Result<Prompted, Error> {
    use sha2::Digest as _;
    let sha256 = hex(&sha2::Sha256::digest(std::fs::read(package)?));
    let mut command = Command::new(&installation.prompter);
    // pkexec's own text agent would otherwise prompt on a terminal nobody watches
    command
        .args([
            "--disable-internal-agent",
            "/bin/sh",
            "-c",
            ROOT_SCRIPT,
            "ngteams-update",
        ])
        .arg(package)
        .arg(sha256)
        .arg(kind)
        .arg(installation.root.join(crate::IDENTITY))
        .arg(hex(&sha2::Sha256::digest(identity)))
        .stdin(Stdio::null());
    match command.output() {
        Ok(output) => classify(output.status.code(), &String::from_utf8_lossy(&output.stderr)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Prompted::Unavailable),
        Err(error) => Err(error.into()),
    }
}

/// The prompt's outcome from its exit code and what it said.
pub(crate) fn classify(code: Option<i32>, stderr: &str) -> Result<Prompted, Error> {
    if code == Some(0) {
        Ok(Prompted::Installed)
    } else if NO_PROMPT_ERRORS.iter().any(|known| stderr.contains(known)) {
        Ok(Prompted::Unavailable)
    } else if code == Some(126) {
        // pkexec's exit for a dismissed prompt
        Ok(Prompted::Cancelled)
    } else {
        let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(3)..].join("\n");
        Err(error(format!(
            "The package manager couldn't install the update. The installed version is unchanged.\n{tail}"
        )))
    }
}
