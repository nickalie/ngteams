//! What the translating tool is told of a chat besides its messages ([`Brief`]), read through the engine, and what a
//! message replies to.
//!
//! - **The chat** ([`brief_of`]): its title or topic (the store's), the account's own name (the member translated
//!   for) and the other members' names, as the engine names them: a chat of at most [`MEMBERS`] others lists them all;
//!   a larger one (a channel) lists the senders around what is translated instead ([`add_senders`]).
//! - **Its glossary**: the renderings the tool chose for the chat's terms before, kept in the store per chat and
//!   language (`teams-store`'s schema 22), the newest named first; what a batch's answer adds is learned at once
//!   ([`learn`]) and kept ([`rows`]), so a new tool process, or a later run, renders them the same way.
//! - **Replies** ([`replied_to`]): a reply holds only a preview of what it quotes; the quoted message's id lets its
//!   whole text go to the tool beside it.

use std::sync::Arc;

use teams_proto::chat::{Node, RichText};
use teams_store::{GLOSSARY_TERMS, GlossaryTerm};

use super::prompt::{Brief, Term};
use crate::bridge::Core;

/// The most other members a brief names.
pub const MEMBERS: usize = 20;

/// The brief of `conversation` read in `language` (the app's code), as the engine knows it now; what it can't read is
/// left out.
pub async fn brief_of(core: &dyn Core, conversation: &str, language: &str) -> Brief {
    let topic = core
        .conversation(conversation.to_owned())
        .await
        .ok()
        .flatten()
        .and_then(|stored| stored.topic)
        .map(|topic| topic.trim().to_owned())
        .filter(|topic| !topic.is_empty());
    let me = core.me().to_owned();
    let you = name_of(core, &me).await;
    let members = core.members(conversation.to_owned()).await.unwrap_or_default();
    let others: Vec<&str> = members
        .iter()
        .map(|member| member.mri.as_str())
        .filter(|mri| *mri != me)
        .collect();
    let mut names: Vec<String> = Vec::new();
    if others.len() <= MEMBERS {
        for mri in others {
            if let Some(name) = name_of(core, mri).await
                && Some(&name) != you.as_ref()
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    let terms = core
        .glossary(conversation.to_owned(), language.to_owned())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|kept| Term {
            term: kept.term,
            rendering: kept.rendering,
        })
        .collect();
    Brief {
        conversation: conversation.to_owned(),
        topic,
        you,
        members: names,
        terms,
    }
}

/// Someone's name as the engine has it.
async fn name_of(core: &dyn Core, mri: &str) -> Option<String> {
    core.person(mri.to_owned())
        .await
        .ok()
        .and_then(|person| person.name)
        .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|name| !name.is_empty())
}

/// `brief` with `senders` among its members, those it doesn't name yet (nor as the account), up to [`MEMBERS`]: a
/// large chat's members are the people around what is translated. Unchanged (the same `Arc`) when it names them all.
pub fn add_senders<'a>(brief: &mut Arc<Brief>, senders: impl IntoIterator<Item = &'a str>) {
    for sender in senders {
        let sender = sender.trim();
        let known = sender.is_empty()
            || brief.you.as_deref() == Some(sender)
            || brief.members.iter().any(|name| name == sender);
        if known || brief.members.len() >= MEMBERS {
            continue;
        }
        Arc::make_mut(brief).members.push(sender.to_owned());
    }
}

/// `brief` with `terms` learned: each replaces what it held for the term and comes first; at most
/// [`GLOSSARY_TERMS`] kept.
pub fn learn(brief: &mut Arc<Brief>, terms: &[Term]) {
    if terms.is_empty() {
        return;
    }
    let held = &mut Arc::make_mut(brief).terms;
    held.retain(|old| !terms.iter().any(|new| new.term == old.term));
    held.splice(0..0, terms.iter().cloned());
    held.truncate(GLOSSARY_TERMS);
}

/// `terms` of `conversation` read in `language`, as the store keeps them, named now.
#[must_use]
pub fn rows(conversation: &str, language: &str, terms: &[Term]) -> Vec<GlossaryTerm> {
    let now = chrono::Utc::now().timestamp_millis();
    terms
        .iter()
        .map(|term| GlossaryTerm {
            conversation_id: conversation.to_owned(),
            language: language.to_owned(),
            term: term.term.clone(),
            rendering: term.rendering.clone(),
            used_at: now,
        })
        .collect()
}

/// The ids of the messages `text` replies to: its reply quotes' (a forward's quote names none).
#[must_use]
pub fn replied_to(text: &RichText) -> Vec<String> {
    let mut found = Vec::new();
    quotes_in(&text.nodes, &mut found);
    found
}

fn quotes_in(nodes: &[Node], found: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Quote(quote) if !quote.forward => {
                if let Some(id) = &quote.message_id
                    && !found.contains(id)
                {
                    found.push(id.clone());
                }
            }
            Node::Paragraph(children) | Node::Indented { children, .. } => quotes_in(children, found),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "../../tests/support/chat_translate_brief_units.rs"]
mod tests;
