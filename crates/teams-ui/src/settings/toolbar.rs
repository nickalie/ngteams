//! The buttons the user shows on the message box's toolbar, the app's for every box and account. A button that does
//! not show has its action in the box's "+" menu, with its key. Send and "+" never hide.
//!
//! Teams' own buttons (Format, Emoji, Attach, Loop) show until the user hides them
//! ([`super::Settings::compose_hidden`]). The AI tools (Correct, Translate) are in "+" until the user shows them
//! ([`super::Settings::compose_shown`]). A file of an earlier build named the one AI button `ai`: that name is passed
//! over, so the AI tools start in "+".

use toml::{Table, Value};

use super::{Problem, keys};

/// A button of the message box's toolbar that the user can show or hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComposeTool {
    Format,
    Emoji,
    Attach,
    Loop,
    /// Correct the draft with the user's AI tool.
    Correct,
    /// Translate the draft with the user's AI tool.
    Translate,
}

impl ComposeTool {
    /// Every button that can show or hide, in the toolbar's order.
    pub const ALL: [Self; 6] = [
        Self::Format,
        Self::Emoji,
        Self::Attach,
        Self::Loop,
        Self::Correct,
        Self::Translate,
    ];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Emoji => "emoji",
            Self::Attach => "attach",
            Self::Loop => "loop",
            Self::Correct => "correct",
            Self::Translate => "translate",
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
            Self::Correct => "Correct",
            Self::Translate => "Translate",
        }
    }

    /// Whether the toolbar shows it until the user hides it: Teams' own buttons do, the AI tools do not.
    #[must_use]
    pub fn shown_by_default(self) -> bool {
        !self.uses_ai()
    }

    /// Whether it is an AI tool, offered only while the AI features are on.
    #[must_use]
    pub fn uses_ai(self) -> bool {
        matches!(self, Self::Correct | Self::Translate)
    }
}

/// `tools` as the file keeps them: each once, in the toolbar's order.
pub(super) fn tidy(tools: &mut Vec<ComposeTool>) {
    tools.sort_unstable();
    tools.dedup();
}

/// Takes a list of buttons under `key`: the ones whose default is not `shown`. A name this build does not know, or a
/// button of the other list, is passed over, so a file of another build still reads.
pub(super) fn take(key: &'static str, shown: bool, value: &Value) -> Result<Vec<ComposeTool>, Problem> {
    let bad = Problem::BadValue {
        key,
        why: "not a list of button names",
    };
    let names = value.as_array().ok_or(bad.clone())?;
    let mut tools = Vec::with_capacity(names.len());
    for name in names {
        let name = name.as_str().ok_or(bad.clone())?;
        tools.extend(ComposeTool::from_key(name).filter(|tool| tool.shown_by_default() != shown));
    }
    tidy(&mut tools);
    Ok(tools)
}

/// The buttons hidden and the buttons shown into the file's table, each list when it has any.
pub(super) fn render(hidden: &[ComposeTool], shown: &[ComposeTool], table: &mut Table) {
    for (key, tools) in [(keys::COMPOSE_HIDDEN, hidden), (keys::COMPOSE_SHOWN, shown)] {
        if tools.is_empty() {
            continue;
        }
        let names = tools.iter().map(|tool| Value::String(tool.key().to_owned())).collect();
        table.insert(key.to_owned(), Value::Array(names));
    }
}
