//! A fake Claude Code for a translated chat's tests (synthetic messages only): it logs each question beside itself;
//! translates each message of a batch as `DE <text>` (one that starts with "Guten" is German already) and an outgoing
//! message as `EN <text>`. Never the real tool.

#[path = "fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

use std::fs;
use std::path::{Path, PathBuf};

pub const FAKE_CLAUDE: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  echo '{"type":"system","subtype":"init"}'
  ans=""
  case "$line" in
    *'<translate>'*)
      while IFS= read -r m; do
        n=$(printf '%s' "$m" | sed 's/^<m id=\\"\([0-9]*\)\\".*/\1/')
        t=$(printf '%s' "$m" | sed 's/^.*\\">\(.*\)<\/m>$/\1/')
        case "$t" in
          Guten*) ans="$ans<m id=\\\"$n\\\" lang=\\\"de\\\" same/>";;
          *) ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">DE $t</m>";;
        esac
      done < <(printf '%s' "$line" | grep -o '<m id=\\"[0-9]*\\" from=\\"[^\\]*\\">[^<]*</m>')
      ;;
    *'<outgoing'*)
      t=$(printf '%s' "$line" | sed 's/.*<outgoing to=\\"[a-z]*\\">\(.*\)<\/outgoing>.*/\1/')
      ans="<out lang=\\\"en\\\">EN $t</out>"
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

/// A fake Claude Code that isn't signed in.
pub const SIGNED_OUT: &str = "#!/bin/bash\necho 'Invalid API key. Please run /login' >&2\nexit 1\n";

/// [`FAKE_CLAUDE`], answering each question only after a second.
#[must_use]
pub fn slow() -> String {
    FAKE_CLAUDE.replacen("  ans=\"\"\n", "  sleep 1\n  ans=\"\"\n", 1)
}

/// Writes `body` as the fake `claude` in `dir`.
pub fn fake(dir: &Path, body: &str) -> PathBuf {
    fake_tool::write(dir, "claude", body)
}

/// The questions the fake in `dir` was asked so far.
#[must_use]
pub fn questions(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("received.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}
