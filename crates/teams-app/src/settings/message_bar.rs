//! The buttons that each account shows on a message's hover bar. A button that does not show has its action in the
//! message's "More options", with its key. "More options" itself never hides.
//!
//! The quick reactions, "React", Edit and Reply show until the user hides them. Copy text, Delete, Mark as unread and
//! Translate are in "More options" until the user shows them. The file keeps each account's choice as the message
//! box's toolbar choice is kept: the names hidden and the names shown, in a table under the account's MRI.
//!
//! A quick reaction is hidden by the reaction itself (`reaction-<key>`), not by its place: the quick reactions change
//! their order as the account uses them, and a hidden one stays hidden in any order.

use std::borrow::Cow;
use std::collections::BTreeMap;

use toml::{Table, Value};

use super::{Problem, chat_id, keys};
use crate::reactions::{self, DEFAULT_QUICK_REACTIONS};

/// The prefix of a quick reaction's name in the file: `reaction-<id>`.
const REACTION: &str = "reaction-";
/// The names of earlier builds, which kept a quick reaction by its place (1 is the first).
const PLACES: [&str; 4] = ["reaction-1", "reaction-2", "reaction-3", "reaction-4"];

/// A button of the message hover bar that the user can show or hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BarItem {
    /// A quick reaction, by its id in Teams' reactions and its skin tone (0 for none), at whichever place of the bar
    /// it is.
    Reaction(&'static str, u8),
    /// "React": every reaction.
    React,
    Edit,
    /// Reply, or "Reply in thread" on a channel's post.
    Reply,
    Copy,
    Delete,
    MarkUnread,
    Translate,
}

impl BarItem {
    /// Every button that can show or hide except the quick reactions, in the bar's order.
    pub const ACTIONS: [Self; 7] = [
        Self::React,
        Self::Edit,
        Self::Reply,
        Self::Copy,
        Self::Delete,
        Self::MarkUnread,
        Self::Translate,
    ];

    /// The quick reaction `key`, when Teams offers it.
    #[must_use]
    pub fn reaction(key: &str) -> Option<Self> {
        reactions::reaction(key).map(|(id, tone)| Self::Reaction(id, tone))
    }

    /// A quick reaction's key, as the account sets it.
    #[must_use]
    pub fn reaction_key(self) -> Option<Cow<'static, str>> {
        match self {
            Self::Reaction(id, 0) => Some(Cow::Borrowed(id)),
            Self::Reaction(id, tone) => Some(Cow::Owned(format!("{id}-tone{tone}"))),
            _ => None,
        }
    }

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> Cow<'static, str> {
        Cow::Borrowed(match self {
            Self::Reaction(..) => return Cow::Owned(format!("{REACTION}{}", self.reaction_key().unwrap_or_default())),
            Self::React => "react",
            Self::Edit => "edit",
            Self::Reply => "reply",
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::MarkUnread => "mark-unread",
            Self::Translate => "translate",
        })
    }

    /// The button `key` names. A place name of an earlier build names the reaction of Teams' own at that place, which
    /// was there unless the account had used other reactions more.
    fn from_key(key: &str) -> Option<Self> {
        if let Some(place) = PLACES.iter().position(|name| *name == key) {
            return DEFAULT_QUICK_REACTIONS.get(place).and_then(|id| Self::reaction(id));
        }
        if let Some(id) = key.strip_prefix(REACTION) {
            return Self::reaction(id);
        }
        Self::ACTIONS.into_iter().find(|item| item.key() == key)
    }

    /// Whether the bar shows it until the user hides it: the bar of Teams.
    #[must_use]
    pub fn shown_by_default(self) -> bool {
        matches!(self, Self::Reaction(..) | Self::React | Self::Edit | Self::Reply)
    }
}

/// One account's choice: the buttons it hid of the ones that show, and the ones it shows of the others, each once in
/// the bar's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BarChoice {
    pub hidden: Vec<BarItem>,
    pub shown: Vec<BarItem>,
}

impl BarChoice {
    /// Whether the bar shows `item`.
    #[must_use]
    pub fn shows(&self, item: BarItem) -> bool {
        if item.shown_by_default() {
            !self.hidden.contains(&item)
        } else {
            self.shown.contains(&item)
        }
    }

    /// Shows `item` on the bar, or hides it in "More options".
    pub fn show(&mut self, item: BarItem, shown: bool) {
        let (list, kept) = if item.shown_by_default() {
            (&mut self.hidden, !shown)
        } else {
            (&mut self.shown, shown)
        };
        list.retain(|other| *other != item);
        if kept {
            list.push(item);
            list.sort_unstable();
        }
    }

    fn is_default(&self) -> bool {
        self.hidden.is_empty() && self.shown.is_empty()
    }
}

/// Takes the accounts' choices into `choices`: a table of tables under the account's MRI, each with `hidden` and `shown` lists of
/// names. A name this build does not know, or a button of the other list, is passed over. An account whose table is
/// bad is left out, the others are taken.
pub(super) fn take(choices: &mut BTreeMap<String, BarChoice>, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::MESSAGE_BAR,
        why: "not a table of accounts, each with lists of button names",
    };
    let table = value.as_table().ok_or(bad.clone())?;
    let mut failed = false;
    for (account, kept) in table {
        let taken = chat_id(&Value::String(account.clone()))
            .zip(kept.as_table())
            .and_then(|(account, kept)| Some((account, take_choice(kept)?)));
        match taken {
            Some((account, choice)) if !choice.is_default() => {
                choices.insert(account, choice);
            }
            Some(_) => {}
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

fn take_choice(kept: &Table) -> Option<BarChoice> {
    let mut choice = BarChoice::default();
    for (key, value) in kept {
        let shown = match key.as_str() {
            "hidden" => false,
            "shown" => true,
            _ => return None,
        };
        let mut items = Vec::new();
        for name in value.as_array()? {
            items.extend(BarItem::from_key(name.as_str()?).filter(|item| item.shown_by_default() != shown));
        }
        items.sort_unstable();
        items.dedup();
        if shown {
            choice.shown = items;
        } else {
            choice.hidden = items;
        }
    }
    Some(choice)
}

/// The accounts' choices into the file's table, when any account has one.
pub(super) fn render(choices: &BTreeMap<String, BarChoice>, table: &mut Table) {
    let accounts: Table = choices
        .iter()
        .filter(|(_, choice)| !choice.is_default())
        .map(|(account, choice)| {
            let mut kept = Table::new();
            for (key, items) in [("hidden", &choice.hidden), ("shown", &choice.shown)] {
                if !items.is_empty() {
                    let names = items
                        .iter()
                        .map(|item| Value::String(item.key().into_owned()))
                        .collect();
                    kept.insert(key.to_owned(), Value::Array(names));
                }
            }
            (account.clone(), Value::Table(kept))
        })
        .collect();
    if !accounts.is_empty() {
        table.insert(keys::MESSAGE_BAR.to_owned(), Value::Table(accounts));
    }
}
