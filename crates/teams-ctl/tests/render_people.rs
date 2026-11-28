//! `ngteamsctl`'s output for the people reads: the search's rows and the presence rows as tables, a card as
//! `key: value` lines with only what it holds, and an ambiguous person's candidates. Every name and id is made up.
//! Times are local, so the checks leave them out.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use teams_ctl::protocol::{CardPersonRow, ChatRef, CtlError, ErrorCode, PeopleRow, PersonCard, PresenceRow, Response};
use teams_ctl::render::{error, text};

fn s(text: &str) -> String {
    text.to_owned()
}

#[test]
fn the_people_found_read_as_a_table_with_their_presence_where_known() {
    let people = Response::People {
        people: vec![
            PeopleRow {
                mri: Some(s("8:orgid:b")),
                name: s("Lab B"),
                email: Some(s("b@lab.invalid")),
                job_title: Some(s("Tester")),
                department: Some(s("Lab")),
                presence: Some(s("available")),
            },
            PeopleRow {
                mri: None,
                name: s("Room 1"),
                email: None,
                job_title: None,
                department: None,
                presence: None,
            },
        ],
    };
    assert_eq!(
        text(&people),
        "MRI        PRESENCE   NAME    EMAIL          TITLE   DEPARTMENT\n\
         8:orgid:b  available  Lab B   b@lab.invalid  Tester  Lab\n\
         -          -          Room 1\n"
    );
    assert_eq!(
        text(&Response::People { people: Vec::new() }),
        "MRI  PRESENCE  NAME  EMAIL  TITLE  DEPARTMENT\n"
    );
}

#[test]
fn the_presence_of_people_reads_as_a_table() {
    let presence = Response::Presence {
        people: vec![
            PresenceRow {
                mri: s("8:orgid:b"),
                name: Some(s("Lab B")),
                availability: s("do-not-disturb"),
            },
            PresenceRow {
                mri: s("8:orgid:c"),
                name: None,
                availability: s("unknown"),
            },
        ],
    };
    assert_eq!(
        text(&presence),
        "MRI        PRESENCE        NAME\n\
         8:orgid:b  do-not-disturb  Lab B\n\
         8:orgid:c  unknown\n"
    );
}

fn full_card() -> PersonCard {
    PersonCard {
        mri: s("8:orgid:b"),
        name: Some(s("Lab B")),
        upn: Some(s("b@lab.invalid")),
        emails: vec![s("b@lab.invalid"), s("bee@lab.invalid")],
        user_type: Some(s("Member")),
        job_title: Some(s("Tester")),
        department: Some(s("Lab")),
        office: Some(s("1")),
        company: Some(s("Lab Inc")),
        mobile_phone: Some(s("+1 555 0100")),
        business_phones: vec![s("+1 555 0101")],
        presence: Some(s("busy")),
        last_active: Some(1_700_000_000_000),
        out_of_office: Some(true),
        free_busy: Some(s("busy")),
        next_free_busy: Some(s("free")),
        free_busy_until: Some(1_700_000_900_000),
        managers: vec![CardPersonRow {
            mri: s("8:orgid:c"),
            name: Some(s("Lab C")),
            email: Some(s("c@lab.invalid")),
        }],
        direct_reports: vec![CardPersonRow {
            mri: s("8:orgid:d"),
            name: None,
            email: None,
        }],
        works_with: vec![CardPersonRow {
            mri: s("8:orgid:a"),
            name: Some(s("Lab A")),
            email: None,
        }],
    }
}

#[test]
fn a_card_reads_as_its_fields_and_leaves_out_what_it_does_not_hold() {
    let shown = text(&Response::Person(Box::new(full_card())));
    let keys: Vec<&str> = shown.lines().map(|line| line.split_once(':').unwrap().0).collect();
    assert_eq!(
        keys,
        [
            "mri",
            "name",
            "upn",
            "email",
            "type",
            "title",
            "department",
            "office",
            "company",
            "mobile",
            "phone",
            "presence",
            "last active",
            "out of office",
            "calendar",
            "then",
            "managers",
            "reports",
            "works with",
        ]
    );
    for line in [
        "mri:           8:orgid:b",
        "email:         b@lab.invalid, bee@lab.invalid",
        "presence:      busy",
        "out of office: yes",
        "managers:      Lab C <c@lab.invalid>",
        "reports:       8:orgid:d",
        "works with:    Lab A",
    ] {
        assert!(shown.contains(line), "{line:?} not in\n{shown}");
    }
    assert!(shown.contains("then:          free from "), "{shown}");

    let mut card = full_card();
    card.out_of_office = Some(false);
    card.free_busy_until = None;
    let shown = text(&Response::Person(Box::new(card)));
    assert!(shown.contains("out of office: no\n"), "{shown}");
    assert!(shown.contains("then:          free\n"), "{shown}");

    let bare = PersonCard {
        mri: s("28:made-up-bot"),
        ..PersonCard::default()
    };
    assert_eq!(text(&Response::Person(Box::new(bare))), "mri: 28:made-up-bot\n");
}

#[test]
fn an_ambiguous_person_lists_each_candidate() {
    let ambiguous = CtlError {
        candidates: vec![
            ChatRef {
                id: s("8:orgid:b1"),
                title: s("Lab B <b1@lab.invalid>"),
            },
            ChatRef {
                id: s("8:orgid:b2"),
                title: s("Lab B"),
            },
        ],
        ..CtlError::new(ErrorCode::AmbiguousPerson, "2 people are called Lab B")
    };
    let mut told = Vec::new();
    error(&mut told, &ambiguous).unwrap();
    assert_eq!(
        String::from_utf8(told).unwrap(),
        "ngteamsctl: ambiguous_person: 2 people are called Lab B\n  8:orgid:b1  Lab B <b1@lab.invalid>\n  8:orgid:b2  Lab B\n"
    );
}
