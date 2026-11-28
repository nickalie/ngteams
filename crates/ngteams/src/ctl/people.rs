//! The people reads, through the addressed account's [`Core`] as the views read them. Nothing here builds a request,
//! and each is a read, so the read-only mode serves them too.
//!
//! - `people`: the service's people search ([`Core::search_service`] with the search's People tab), each person with
//!   the presence the app knows of them now (the presence badges' model; nobody is watched for it).
//! - `person`: one person's profile card ([`Core::profile_card`]), as the card reads it. What the card does not
//!   hold (someone without a card, not a directory user, or a read of the card that failed) is what the people
//!   search found and [`Core::person`] gives, as the card shows the name that the views have. The presence is the
//!   card's own read; if that failed, it is asked as `presence` asks it.
//! - `presence`: each person asked about through [`Core::presence`], as a presence badge asks: the engine watches
//!   them from then on. Someone that the app did not know yet is waited for, [`PRESENCE_WAIT`] at most, until the
//!   engine tells their presence; then `unknown` (in the lab, the presence of anyone but the lab accounts stays
//!   unknown). The engine watches [`PRESENCE_MOST`] people at most, so one `presence` asks about no more. The views
//!   share that watch: each person asked about who isn't watched yet drops the one asked about longest ago, whose
//!   badge shows unknown until its view asks again (as the profile card's Organization tab does too).
//!
//! **A person** is named by their MRI (as it is), or else found by the people search: by UPN or email when the
//! address is one that the search gives for them (their UPN or a mail address, as a guest's or an alias's mail is
//! not its UPN), by display name in any case. When no name is the one given, each person that the search found for it
//! is a match (a part of a name); an address matches only itself, so a mistyped one names nobody. A person that the
//! search gives more than once is one match. Nobody is [`ErrorCode::UnknownPerson`]; several people are
//! [`ErrorCode::AmbiguousPerson`], with each one's MRI and name.

use gpui::{App, AsyncApp, Entity, WeakEntity};
use teams_core::presence::has_presence;
use teams_core::profile_card::has_card;
use teams_core::{Availability, CardPerson, EngineError, ProfileCard, SearchScope};
use teams_ctl::protocol::{
    CardPersonRow, ChatRef, CtlError, ErrorCode, PEOPLE_ANSWER, PRESENCE_MOST, PRESENCE_WAIT, PeopleRow, PersonCard,
    PresenceRow, Request, Response,
};
use teams_proto::{FoundPerson, FreeBusyStatus, SearchKind};
use teams_ui::Core;
use teams_ui::presence::Presences;

use super::address::{self, engine_error};
use super::dispatch::alive;
use super::events::availability_name;
use super::writes::within;
use crate::app::Running;

/// Whether `request` is a people read (see the module docs).
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(
        request,
        Request::People { .. } | Request::Person { .. } | Request::Presence { .. }
    )
}

/// `request`, a people read, answered for `account` (see the module docs).
///
/// # Errors
/// The account isn't one, a person isn't named or found, the engine's read failed or took longer than
/// [`PEOPLE_ANSWER`].
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let (core, presences) = cx.update(|cx| {
        let running = alive(running, cx)?;
        let running = running.read(cx);
        let addressed = address::account(running, account, cx)?;
        let (_, _, bridge) = address::known(running, Some(&addressed.mri), cx)?;
        let presences = bridge.read(cx).models().presence.clone();
        Ok::<_, CtlError>((addressed.core, presences))
    })?;
    let core = &*core;
    let executor = cx.background_executor().clone();
    let late = format!("the people weren't read in {}s", PEOPLE_ANSWER.as_secs());
    match request {
        Request::People { query, limit } => {
            let found = within(PEOPLE_ANSWER, &late, search(core, query), &executor).await?;
            let limit = limit.map_or(usize::MAX, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
            let people = cx.update(|cx| {
                let presences = presences.read(cx);
                found
                    .into_iter()
                    .take(limit)
                    .map(|person| people_row(person, presences))
                    .collect()
            });
            Ok(Response::People { people })
        }
        Request::Person { who } => {
            let read = async {
                let person = resolve(core, who).await?;
                card(core, person).await
            };
            let mut card = within(PEOPLE_ANSWER, &late, read, &executor).await?;
            if card.presence.is_none() {
                let mri = card.mri.clone();
                let availability = presence_of(core, &presences, std::slice::from_ref(&mri), cx).await?;
                card.presence = availability.first().and_then(known_name);
            }
            Ok(Response::Person(Box::new(card)))
        }
        Request::Presence { people } => {
            let named = within(PEOPLE_ANSWER, &late, resolve_all(core, people), &executor).await?;
            let mris: Vec<String> = named.iter().map(|person| person.mri.clone()).collect();
            let availability = presence_of(core, &presences, &mris, cx).await?;
            let mut rows = Vec::with_capacity(named.len());
            for (person, availability) in named.into_iter().zip(availability) {
                let name = match person.name {
                    Some(name) => Some(name),
                    None => stored_name(core, &person.mri).await,
                };
                rows.push(PresenceRow {
                    mri: person.mri,
                    name,
                    availability: availability_name(&availability),
                });
            }
            Ok(Response::Presence { people: rows })
        }
        _ => Err(CtlError::new(ErrorCode::BadRequest, "not a people read")),
    }
}

/// A person named and found: their MRI, and what the people search said of them when it found them.
struct Named {
    mri: String,
    name: Option<String>,
    found: Option<FoundPerson>,
}

/// What the people search finds for `query`, as the search's People tab asks it.
async fn search(core: &dyn Core, query: &str) -> Result<Vec<FoundPerson>, CtlError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(CtlError::new(ErrorCode::BadRequest, "name the people to look for"));
    }
    core.search_service(query.to_owned(), SearchScope::Only(SearchKind::People))
        .await
        .map(|found| found.people)
        .map_err(|error| engine_error(&error))
}

/// Whether `who` is an MRI (`8:orgid:…`, `28:…`): its kind's digits and a colon first.
fn is_mri(who: &str) -> bool {
    who.split_once(':').is_some_and(|(kind, rest)| {
        !kind.is_empty() && kind.bytes().all(|byte| byte.is_ascii_digit()) && !rest.is_empty()
    })
}

/// The person `who` names (see the module docs).
async fn resolve(core: &dyn Core, who: &str) -> Result<Named, CtlError> {
    let who = who.trim();
    if who.is_empty() {
        return Err(CtlError::new(ErrorCode::BadRequest, "name the person"));
    }
    if is_mri(who) {
        return Ok(Named {
            mri: who.to_owned(),
            name: None,
            found: None,
        });
    }
    let found: Vec<(String, FoundPerson)> = search(core, who)
        .await?
        .into_iter()
        .filter_map(|person| Some((person.mri.clone()?, person)))
        .collect();
    let exact = matching(who, &found);
    match exact.as_slice() {
        [] => Err(CtlError::new(
            ErrorCode::UnknownPerson,
            format!("no one is {who}: `ngteamsctl people {who}` lists whom the search finds"),
        )),
        [(mri, person)] => Ok(Named {
            mri: mri.clone(),
            name: Some(person.name.clone()),
            found: Some(person.clone()),
        }),
        several => Err(ambiguous(who, several)),
    }
}

/// The people of `found` that `who` names, each MRI once: by an address that the search gives for them, else by
/// name, else (for a name only) each person found.
fn matching<'a>(who: &str, found: &'a [(String, FoundPerson)]) -> Vec<&'a (String, FoundPerson)> {
    let by_address = who.contains('@');
    let named = |(_, person): &&(String, FoundPerson)| {
        if by_address {
            person.addresses.iter().any(|address| address.eq_ignore_ascii_case(who))
        } else {
            person.name.to_lowercase() == who.to_lowercase()
        }
    };
    let mut exact: Vec<&(String, FoundPerson)> = found.iter().filter(named).collect();
    if exact.is_empty() && !by_address {
        exact = found.iter().collect();
    }
    let mut seen = std::collections::HashSet::new();
    exact.retain(|(mri, _)| seen.insert(mri.as_str()));
    exact
}

fn ambiguous(who: &str, several: &[&(String, FoundPerson)]) -> CtlError {
    let mut error = CtlError::new(
        ErrorCode::AmbiguousPerson,
        format!("{} people are called {who}: name one by MRI or email", several.len()),
    );
    error.candidates = several
        .iter()
        .map(|(mri, person)| ChatRef {
            id: mri.clone(),
            title: match &person.email {
                Some(email) => format!("{} <{email}>", person.name),
                None => person.name.clone(),
            },
        })
        .collect();
    error
}

/// Each of `people` named and found, each person once.
async fn resolve_all(core: &dyn Core, people: &[String]) -> Result<Vec<Named>, CtlError> {
    let people: Vec<&str> = people
        .iter()
        .map(|person| person.trim())
        .filter(|person| !person.is_empty())
        .collect();
    if people.is_empty() {
        return Err(CtlError::new(ErrorCode::BadRequest, "name the people"));
    }
    if people.len() > PRESENCE_MOST {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("ask about {PRESENCE_MOST} people at most: the app watches no more at once"),
        ));
    }
    let mut named: Vec<Named> = Vec::with_capacity(people.len());
    for person in people {
        let person = resolve(core, person).await?;
        match named.iter_mut().find(|known| known.mri == person.mri) {
            Some(known) => {
                if known.name.is_none() {
                    known.name = person.name;
                }
            }
            None => named.push(person),
        }
    }
    Ok(named)
}

/// `person`'s card (see the module docs).
async fn card(core: &dyn Core, person: Named) -> Result<PersonCard, CtlError> {
    let mut card = PersonCard {
        mri: person.mri.clone(),
        ..PersonCard::default()
    };
    if has_card(&person.mri) {
        match core.profile_card(person.mri.clone()).await {
            Ok(read) => card = person_card(read),
            Err(EngineError::ProfileUnread) => {}
            Err(error) => return Err(engine_error(&error)),
        }
    }
    if card.name.is_none() {
        card.name = match person.name {
            Some(name) => Some(name),
            None => stored_name(core, &person.mri).await,
        };
    }
    if let Some(found) = person.found {
        if card.emails.is_empty() {
            card.emails.extend(found.email);
        }
        if card.job_title.is_none() {
            card.job_title = found.job_title;
        }
        if card.department.is_none() {
            card.department = found.department;
        }
    }
    Ok(card)
}

/// `mri`'s name as the engine has it (none when it fails).
async fn stored_name(core: &dyn Core, mri: &str) -> Option<String> {
    core.person(mri.to_owned()).await.ok().and_then(|person| person.name)
}

/// The presence of each of `mris`, in their order (see the module docs).
///
/// # Errors
/// [`ErrorCode::Stopped`] when the account's engine stopped.
async fn presence_of(
    core: &dyn Core,
    presences: &Entity<Presences>,
    mris: &[String],
    cx: &mut AsyncApp,
) -> Result<Vec<Availability>, CtlError> {
    let mut asked = Vec::with_capacity(mris.len());
    for mri in mris {
        match core.presence(mri.clone()).await {
            Ok(availability) => asked.push(availability),
            Err(EngineError::Stopped) => return Err(engine_error(&EngineError::Stopped)),
            Err(_) => asked.push(Availability::Unknown),
        }
    }
    let now = |cx: &App| known_now(&asked, mris, presences.read(cx));
    let (done, finished) = async_channel::bounded::<()>(1);
    let (first, _observing) = cx.update(|cx| {
        let first = now(cx);
        if settled(&first, mris) {
            return (first, None);
        }
        let done = done.clone();
        let asked = asked.clone();
        let mris = mris.to_vec();
        let observing = cx.observe(presences, move |presences, cx| {
            if settled(&known_now(&asked, &mris, presences.read(cx)), &mris) {
                let _ = done.try_send(());
            }
        });
        (first, Some(observing))
    });
    if settled(&first, mris) {
        return Ok(first);
    }
    let timer = cx.background_executor().timer(PRESENCE_WAIT);
    // dropped (cancelled) once everyone is known first
    let _timeout = cx.background_executor().spawn(async move {
        timer.await;
        let _ = done.try_send(());
    });
    let _ = finished.recv().await;
    Ok(cx.update(|cx| now(cx)))
}

/// Each one's presence: the model's (the engine's latest), else the reply to the ask.
fn known_now(asked: &[Availability], mris: &[String], presences: &Presences) -> Vec<Availability> {
    mris.iter()
        .zip(asked)
        .map(|(mri, asked)| match presences.of(mri) {
            Availability::Unknown => asked.clone(),
            told => told,
        })
        .collect()
}

/// Whether nobody is left to wait for: each is known, or someone that UPS gives no presence for.
fn settled(availability: &[Availability], mris: &[String]) -> bool {
    availability
        .iter()
        .zip(mris)
        .all(|(availability, mri)| *availability != Availability::Unknown || !has_presence(mri))
}

/// An availability's name, none when it is unknown.
fn known_name(availability: &Availability) -> Option<String> {
    (*availability != Availability::Unknown).then(|| availability_name(availability))
}

fn people_row(person: FoundPerson, presences: &Presences) -> PeopleRow {
    let presence = person.mri.as_deref().and_then(|mri| known_name(&presences.of(mri)));
    PeopleRow {
        mri: person.mri,
        name: person.name,
        email: person.email,
        job_title: person.job_title,
        department: person.department,
        presence,
    }
}

/// The card's fields on the wire.
fn person_card(card: ProfileCard) -> PersonCard {
    let schedule = card.schedule.as_ref();
    let organization = card.organization.unwrap_or_default();
    PersonCard {
        mri: card.mri,
        name: card.name,
        upn: card.user_principal_name,
        emails: card.emails,
        user_type: card.user_type,
        job_title: card.job_title,
        department: card.department,
        office: card.office,
        company: card.company,
        mobile_phone: card.mobile_phone,
        business_phones: card.business_phones,
        presence: card
            .presence
            .as_ref()
            .and_then(|presence| known_name(&presence.availability)),
        last_active: card.presence.as_ref().and_then(|presence| presence.last_active_ms),
        out_of_office: card.out_of_office,
        free_busy: schedule
            .and_then(|schedule| schedule.current.as_ref())
            .map(free_busy_name),
        next_free_busy: schedule.and_then(|schedule| schedule.next.as_ref()).map(free_busy_name),
        free_busy_until: schedule.and_then(|schedule| schedule.until_ms),
        managers: organization.managers.into_iter().map(card_person).collect(),
        direct_reports: organization.directs.into_iter().map(card_person).collect(),
        works_with: card
            .working_with
            .unwrap_or_default()
            .into_iter()
            .map(card_person)
            .collect(),
    }
}

fn card_person(person: CardPerson) -> CardPersonRow {
    CardPersonRow {
        mri: person.mri,
        name: person.name,
        email: person.email,
    }
}

fn free_busy_name(status: &FreeBusyStatus) -> String {
    match status {
        FreeBusyStatus::Free => "free".to_owned(),
        FreeBusyStatus::Tentative => "tentative".to_owned(),
        FreeBusyStatus::Busy => "busy".to_owned(),
        FreeBusyStatus::OutOfOffice => "out_of_office".to_owned(),
        FreeBusyStatus::WorkingElsewhere => "working_elsewhere".to_owned(),
        FreeBusyStatus::Unknown => "unknown".to_owned(),
        FreeBusyStatus::Other(word) => word.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mri_is_its_kind_s_digits_and_a_colon_first() {
        assert!(is_mri("8:orgid:00000000-0000-4000-8000-0000000000b1"));
        assert!(is_mri("28:made-up-bot"));
        for not_one in ["bee@lab.example.test", "Bee Example", ":x", "8:", "a8:orgid:x"] {
            assert!(!is_mri(not_one), "{not_one}");
        }
    }

    fn found(mri: &str, name: &str, addresses: &[&str]) -> (String, FoundPerson) {
        let person = FoundPerson {
            mri: Some(mri.to_owned()),
            name: name.to_owned(),
            email: addresses.first().map(|address| (*address).to_owned()),
            addresses: addresses.iter().map(|address| (*address).to_owned()).collect(),
            job_title: None,
            department: None,
        };
        (mri.to_owned(), person)
    }

    fn mris<'a>(people: &[&'a (String, FoundPerson)]) -> Vec<&'a str> {
        people.iter().map(|(mri, _)| mri.as_str()).collect()
    }

    #[test]
    fn an_address_names_whoever_the_search_gives_it_for_and_nobody_else() {
        let jane = found(
            "8:orgid:jane",
            "Jane Guest",
            &["jane_ext.test#EXT#@lab.test", "jane@ext.test"],
        );
        let mary = found("8:orgid:mary", "Mary Jane", &["mary.jane@ext.test"]);
        let people = [mary.clone(), jane.clone(), jane];
        assert_eq!(mris(&matching("JANE@ext.test", &people)), ["8:orgid:jane"]);
        let jonathan = [found("8:orgid:jonathan", "Jonathan", &["jonathan@lab.test"])];
        assert!(matching("jon@lab.test", &jonathan).is_empty());
    }

    #[test]
    fn a_part_of_a_name_names_each_person_found_once() {
        let ann = found("8:orgid:ann", "Ann Lee", &["ann@lab.test"]);
        let anna = found("8:orgid:anna", "Anna Bell", &[]);
        let people = [ann.clone(), anna, ann];
        assert_eq!(mris(&matching("ann", &people)), ["8:orgid:ann", "8:orgid:anna"]);
        assert_eq!(mris(&matching("ann lee", &people)), ["8:orgid:ann"]);
    }

    #[test]
    fn the_wire_names_each_calendar_status_and_keeps_an_unknown_word() {
        let names: Vec<String> = [
            FreeBusyStatus::Free,
            FreeBusyStatus::Tentative,
            FreeBusyStatus::Busy,
            FreeBusyStatus::OutOfOffice,
            FreeBusyStatus::WorkingElsewhere,
            FreeBusyStatus::Unknown,
            FreeBusyStatus::Other("madeUpWord".to_owned()),
        ]
        .iter()
        .map(free_busy_name)
        .collect();
        assert_eq!(
            names,
            [
                "free",
                "tentative",
                "busy",
                "out_of_office",
                "working_elsewhere",
                "unknown",
                "madeUpWord"
            ]
        );
    }

    #[test]
    fn a_presence_ask_holds_no_more_people_than_the_engine_watches() {
        assert_eq!(PRESENCE_MOST, teams_core::MAX_WATCHED);
    }

    #[test]
    fn nobody_is_waited_for_once_each_is_known_or_has_no_presence() {
        let mris = ["8:orgid:b".to_owned(), "28:made-up-bot".to_owned()];
        assert!(settled(&[Availability::Busy, Availability::Unknown], &mris));
        assert!(!settled(&[Availability::Unknown, Availability::Unknown], &mris));
        assert_eq!(
            known_now(
                &[Availability::Away, Availability::Unknown],
                &mris,
                &Presences::default()
            ),
            [Availability::Away, Availability::Unknown]
        );
        assert_eq!(known_name(&Availability::Unknown), None);
        assert_eq!(
            known_name(&Availability::DoNotDisturb).as_deref(),
            Some("do-not-disturb")
        );
    }
}
