//! The people reads for a person: the search's rows and the presence rows as tables, a profile card as `key: value`
//! lines with only the fields that the card holds.

use super::{record, stamp, table};
use crate::protocol::{CardPersonRow, PeopleRow, PersonCard, PresenceRow};

pub(super) fn people_table(people: &[PeopleRow]) -> String {
    let rows = people.iter().map(|person| {
        vec![
            person.mri.clone().unwrap_or_else(|| "-".to_owned()),
            person.presence.clone().unwrap_or_else(|| "-".to_owned()),
            person.name.clone(),
            person.email.clone().unwrap_or_default(),
            person.job_title.clone().unwrap_or_default(),
            person.department.clone().unwrap_or_default(),
        ]
    });
    table(&["MRI", "PRESENCE", "NAME", "EMAIL", "TITLE", "DEPARTMENT"], rows)
}

pub(super) fn presence_table(people: &[PresenceRow]) -> String {
    let rows = people.iter().map(|person| {
        vec![
            person.mri.clone(),
            person.availability.clone(),
            person.name.clone().unwrap_or_default(),
        ]
    });
    table(&["MRI", "PRESENCE", "NAME"], rows)
}

pub(super) fn person_card(card: &PersonCard) -> String {
    let text = |key: &'static str, value: &Option<String>| value.clone().map(|value| (key, value));
    let list = |key: &'static str, values: &[String]| (!values.is_empty()).then(|| (key, values.join(", ")));
    let people = |key: &'static str, people: &[CardPersonRow]| {
        (!people.is_empty()).then(|| (key, people.iter().map(card_person).collect::<Vec<_>>().join(", ")))
    };
    let fields: Vec<(&str, String)> = [
        Some(("mri", card.mri.clone())),
        text("name", &card.name),
        text("upn", &card.upn),
        list("email", &card.emails),
        text("type", &card.user_type),
        text("title", &card.job_title),
        text("department", &card.department),
        text("office", &card.office),
        text("company", &card.company),
        text("mobile", &card.mobile_phone),
        list("phone", &card.business_phones),
        text("presence", &card.presence),
        card.last_active.map(|at| ("last active", stamp(at))),
        card.out_of_office
            .map(|away| ("out of office", if away { "yes" } else { "no" }.to_owned())),
        text("calendar", &card.free_busy),
        next_free_busy(card).map(|next| ("then", next)),
        people("managers", &card.managers),
        people("reports", &card.direct_reports),
        people("works with", &card.works_with),
    ]
    .into_iter()
    .flatten()
    .collect();
    record(&fields)
}

/// The status that follows the calendar's status now, with when it starts.
fn next_free_busy(card: &PersonCard) -> Option<String> {
    let next = card.next_free_busy.clone()?;
    Some(match card.free_busy_until {
        Some(at) => format!("{next} from {}", stamp(at)),
        None => next,
    })
}

fn card_person(person: &CardPersonRow) -> String {
    let name = person.name.as_deref().unwrap_or(&person.mri);
    match &person.email {
        Some(email) => format!("{name} <{email}>"),
        None => name.to_owned(),
    }
}
