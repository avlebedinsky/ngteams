//! The endpoint's call agent: where the call's services call the client back. Every callback a call hands out is the
//! calling endpoint's push URL, `callAgent/<agent id>/`, a random segment of its own, and `call/<name>/` or
//! `conversation/<name>/` (`docs/protocol/calling-echo.md`, "The call, step by step"), so a delivery at a path that is
//! none of a call's callbacks is another call's (or nobody's) and not taken as this one's.

use std::fmt::Write as _;

/// Whose callback it is: the call leg's (`call/…`) or the conversation's (`conversation/…`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Call,
    Conversation,
}

impl Kind {
    fn segment(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Conversation => "conversation",
        }
    }
}

/// One callback a call handed out: its URL, the path its deliveries arrive at, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Callback {
    pub(crate) url: String,
    pub(crate) path: String,
    pub(crate) kind: Kind,
    pub(crate) name: &'static str,
}

/// The calling endpoint's call agent: `<push url>callAgent/<agent id>/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAgent {
    /// `<push url>callAgent/<agent id>/`.
    prefix: String,
    /// Its path: `/v4/f/<endpoint>/callAgent/<agent id>/`.
    path: String,
}

impl CallAgent {
    /// The agent `agent_id` under `push_url` (the calling endpoint's `https://<host>/v4/f/<endpoint>/`); `None` for a
    /// push URL in another form.
    pub(crate) fn new(push_url: &str, agent_id: &str) -> Option<Self> {
        let rest = push_url.strip_prefix("https://")?;
        let (authority, path) = rest.split_at(rest.find('/')?);
        let endpoint = path.strip_prefix("/v4/f/")?.strip_suffix('/')?;
        let plain = |text: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        };
        if !plain(authority) || !plain(endpoint) || endpoint.contains(':') || !plain(agent_id) {
            return None;
        }
        Some(Self {
            prefix: format!("https://{authority}/v4/f/{endpoint}/callAgent/{agent_id}/"),
            path: format!("/v4/f/{endpoint}/callAgent/{agent_id}/"),
        })
    }

    /// A new callback `kind`/`name` with a random segment of its own.
    pub(crate) fn callback(&self, kind: Kind, name: &'static str) -> Result<Callback, getrandom::Error> {
        let segment = random_hex(4)?;
        let tail = format!("{segment}/{}/{name}/", kind.segment());
        Ok(Callback {
            url: format!("{}{tail}", self.prefix),
            path: format!("{}{tail}", self.path),
            kind,
            name,
        })
    }
}

/// `bytes` random bytes as lowercase hex.
pub(crate) fn random_hex(bytes: usize) -> Result<String, getrandom::Error> {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random)?;
    Ok(random.iter().fold(String::with_capacity(bytes * 2), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

/// A fresh random GUID, lowercase and hyphenated (a version 4 UUID).
pub(crate) fn guid() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::{CallAgent, Kind, guid};

    const PUSH: &str = "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00001/";

    #[test]
    fn a_callback_is_the_push_url_the_agent_a_segment_of_its_own_and_its_kind_and_name() {
        let agent = CallAgent::new(PUSH, "00000000-0000-4000-8000-0000000000a1").unwrap();
        let end = agent.callback(Kind::Call, "end").unwrap();
        let again = agent.callback(Kind::Call, "end").unwrap();
        assert!(
            end.url
                .starts_with(&format!("{PUSH}callAgent/00000000-0000-4000-8000-0000000000a1/"))
        );
        assert!(end.url.ends_with("/call/end/"));
        assert_eq!(
            end.path,
            end.url
                .strip_prefix("https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443")
                .unwrap()
        );
        assert_ne!(end.path, again.path, "each callback has a segment of its own");
        let roster = agent.callback(Kind::Conversation, "rosterUpdate").unwrap();
        assert!(roster.path.ends_with("/conversation/rosterUpdate/"));
    }

    #[test]
    fn a_push_url_in_another_form_makes_no_agent() {
        for push in [
            "http://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/E/",
            "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/E",
            "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/x/E/",
            "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/E/more/",
            "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f//",
            "https://user@host/v4/f/E/",
        ] {
            assert_eq!(CallAgent::new(push, "agent"), None, "{push}");
        }
        assert_eq!(CallAgent::new(PUSH, "a/b"), None);
    }

    #[test]
    fn a_guid_is_a_version_4_uuid() {
        let id = guid().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, guid().unwrap());
    }
}
