//! The attendee picker's suggestions, as Teams' people picker finds them: the people the account knows whose names
//! hold what the box says (`Core::invitable`, their addresses from their profiles), then the people the directory
//! finds for it (the service's people search, `Core::search_service`'s suggestions, as the title bar's search asks
//! them), each once, by address. Asked a moment after the typing stops; an answer to what the box no longer says is
//! dropped.

use std::time::Duration;

use gpui::{AsyncApp, Context, WeakEntity};
use teams_core::{Invitable, SearchAnswer, SearchScope};

use super::{EventForm, Need};

/// How long the box waits after a key before it asks.
pub(crate) const PAUSE: Duration = Duration::from_millis(250);

/// The most suggestions shown.
const MOST: usize = 10;

impl EventForm {
    /// Asks for the people `query` (what the `need` box says) names, and shows them under the box.
    pub(super) fn suggest(&mut self, need: Need, query: String, cx: &mut Context<Self>) {
        let core = std::sync::Arc::clone(&self.core);
        self.suggesting = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(PAUSE).await;
            let known = core.invitable(query.clone());
            let found = core.search_service(query.clone(), SearchScope::Suggestions);
            let known = known.await.unwrap_or_default();
            let found = found.await.unwrap_or_default();
            let people = merged(known, &found);
            let _ = this.update(cx, |form, cx| {
                let input = match need {
                    Need::Required => &form.required,
                    Need::Optional => &form.optional,
                };
                // an answer to what the box no longer says is dropped
                if input.read(cx).value().trim() == query {
                    form.suggestions = Some((need, people));
                    form.suggested_for = query;
                    cx.notify();
                }
            });
        }));
    }
}

/// The people the account knows, then those the directory found with an address, each address once.
pub(crate) fn merged(known: Vec<Invitable>, found: &SearchAnswer) -> Vec<Invitable> {
    let mut people = known;
    for person in &found.people {
        let Some(address) = person.email.clone().filter(|address| address.contains('@')) else {
            continue;
        };
        if people.iter().any(|known| known.address.eq_ignore_ascii_case(&address)) {
            continue;
        }
        people.push(Invitable {
            mri: person.mri.clone().unwrap_or_default(),
            name: person.name.clone(),
            address,
        });
    }
    people.truncate(MOST);
    people
}

#[cfg(test)]
mod tests {
    use teams_proto::FoundPerson;

    use super::*;

    #[test]
    fn the_directory_adds_whom_the_account_doesn_t_know_once() {
        let known = vec![Invitable {
            mri: "8:orgid:b".to_owned(),
            name: "Bot B".to_owned(),
            address: "bot-b@lab.example.com".to_owned(),
        }];
        let found = SearchAnswer {
            people: vec![
                FoundPerson {
                    mri: Some("8:orgid:b".to_owned()),
                    name: "Bot B".to_owned(),
                    email: Some("BOT-B@lab.example.com".to_owned()),
                    addresses: Vec::new(),
                    job_title: None,
                    department: None,
                },
                FoundPerson {
                    mri: None,
                    name: "Bot C".to_owned(),
                    email: Some("bot-c@lab.example.com".to_owned()),
                    addresses: Vec::new(),
                    job_title: None,
                    department: None,
                },
                FoundPerson {
                    mri: None,
                    name: "No address".to_owned(),
                    email: None,
                    addresses: Vec::new(),
                    job_title: None,
                    department: None,
                },
            ],
            ..SearchAnswer::default()
        };
        let people = merged(known, &found);
        let addresses: Vec<&str> = people.iter().map(|person| person.address.as_str()).collect();
        assert_eq!(addresses, ["bot-b@lab.example.com", "bot-c@lab.example.com"]);
    }
}
