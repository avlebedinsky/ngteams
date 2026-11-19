//! The buttons the user hid from the message box's toolbar ([`super::Settings::compose_hidden`]), the app's for every
//! box and account. A hidden button's action stays in the box's "+" menu, with its key. Send and "+" never hide.

use toml::{Table, Value};

use super::{Problem, keys};

/// A button of the message box's toolbar that the user can hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComposeTool {
    Format,
    Emoji,
    Attach,
    Loop,
    /// The one AI button: correct, translate and the translation language.
    Ai,
}

impl ComposeTool {
    /// Every button that can hide, in the toolbar's order.
    pub const ALL: [Self; 5] = [Self::Format, Self::Emoji, Self::Attach, Self::Loop, Self::Ai];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Emoji => "emoji",
            Self::Attach => "attach",
            Self::Loop => "loop",
            Self::Ai => "ai",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.key() == key)
    }

    /// What the customize list and the settings call it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Format => "Format",
            Self::Emoji => "Emoji",
            Self::Attach => "Attach files",
            Self::Loop => "Loop components",
            Self::Ai => "AI tools",
        }
    }
}

/// `hidden` as the file keeps it: each once, in the toolbar's order.
pub(super) fn tidy(hidden: &mut Vec<ComposeTool>) {
    hidden.sort_unstable();
    hidden.dedup();
}

/// Takes the hidden buttons. A name this build does not know is passed over, so a newer build's file still reads.
pub(super) fn take(value: &Value) -> Result<Vec<ComposeTool>, Problem> {
    let bad = Problem::BadValue {
        key: keys::COMPOSE_HIDDEN,
        why: "not a list of button names",
    };
    let names = value.as_array().ok_or(bad.clone())?;
    let mut hidden = Vec::with_capacity(names.len());
    for name in names {
        let name = name.as_str().ok_or(bad.clone())?;
        hidden.extend(ComposeTool::from_key(name));
    }
    tidy(&mut hidden);
    Ok(hidden)
}

/// The hidden buttons into the file's table, when there are any.
pub(super) fn render(hidden: &[ComposeTool], table: &mut Table) {
    if hidden.is_empty() {
        return;
    }
    let names = hidden.iter().map(|tool| Value::String(tool.key().to_owned())).collect();
    table.insert(keys::COMPOSE_HIDDEN.to_owned(), Value::Array(names));
}
