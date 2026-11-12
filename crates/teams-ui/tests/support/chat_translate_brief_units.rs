//! What a chat's translating tool is told of the chat besides its messages (synthetic names and terms only): its
//! members grown by the senders around a batch, its glossary learned, its rows, and what a message replies to.

use super::*;
use teams_proto::chat::parse_html;

fn brief() -> Arc<Brief> {
    Arc::new(Brief {
        conversation: "chat".to_owned(),
        you: Some("Lab A".to_owned()),
        members: vec!["Lab B".to_owned()],
        ..Brief::default()
    })
}

fn term(term: &str, rendering: &str) -> Term {
    Term {
        term: term.to_owned(),
        rendering: rendering.to_owned(),
    }
}

#[test]
fn senders_join_the_members_up_to_the_most_named() {
    let mut told = brief();
    let before = Arc::clone(&told);
    add_senders(&mut told, ["Lab A", "Lab B", " ", "Lab B"]);
    assert!(Arc::ptr_eq(&told, &before), "nothing new: the same brief");
    add_senders(&mut told, ["Lab C", "Lab C"]);
    assert_eq!(told.members, ["Lab B", "Lab C"]);
    let many: Vec<String> = (0..MEMBERS + 5).map(|n| format!("Person {n}")).collect();
    add_senders(&mut told, many.iter().map(String::as_str));
    assert_eq!(told.members.len(), MEMBERS);
}

#[test]
fn learned_terms_come_first_replace_their_own_and_are_bounded() {
    let mut told = brief();
    learn(&mut told, &[]);
    learn(&mut told, &[term("Abnahme", "acceptance"), term("Lastenheft", "spec")]);
    learn(&mut told, &[term("Abnahme", "sign-off")]);
    assert_eq!(told.terms, [term("Abnahme", "sign-off"), term("Lastenheft", "spec")]);
    let many: Vec<Term> = (0..GLOSSARY_TERMS + 3).map(|n| term(&format!("t{n}"), "r")).collect();
    learn(&mut told, &many);
    assert_eq!(told.terms.len(), GLOSSARY_TERMS);
    assert_eq!(told.terms[0].term, "t0");
    let kept = rows("chat", "en", &[term("Abnahme", "sign-off")]);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].conversation_id.as_str(), kept[0].language.as_str()),
        ("chat", "en")
    );
    assert_eq!(
        (kept[0].term.as_str(), kept[0].rendering.as_str()),
        ("Abnahme", "sign-off")
    );
    assert!(kept[0].used_at > 0);
}

#[test]
fn a_reply_names_what_it_quotes_and_a_forward_names_nothing() {
    let reply = parse_html(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"1700\"><strong \
         itemprop=\"mri\" itemid=\"8:orgid:synthetic-b\">Lab B</strong><p itemprop=\"preview\">Which?</p>\
         </blockquote><p>That one</p>",
    );
    assert_eq!(replied_to(&reply), ["1700"]);
    let nested = RichText {
        nodes: vec![Node::Indented {
            depth: 1,
            children: vec![Node::Paragraph(reply.nodes.clone()), reply.nodes[0].clone()],
        }],
        ..reply.clone()
    };
    assert_eq!(replied_to(&nested), ["1700"]);
    let forward = RichText {
        nodes: reply
            .nodes
            .iter()
            .map(|node| match node {
                Node::Quote(quote) => Node::Quote(teams_proto::chat::Quote {
                    forward: true,
                    ..quote.clone()
                }),
                other => other.clone(),
            })
            .collect(),
        ..reply.clone()
    };
    assert!(replied_to(&forward).is_empty());
    assert!(replied_to(&RichText::plain("plain")).is_empty());
}
