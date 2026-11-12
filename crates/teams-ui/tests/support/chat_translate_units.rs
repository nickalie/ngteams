//! A chat's translation's own parts (synthetic messages only): how the account's sent originals are matched to their
//! messages, kept rows read back, and outgoing answers taken.

use super::*;

fn source(id: &str, client: Option<&str>, edited: Option<i64>, text: &str) -> Source {
    let text = Arc::new(RichText::plain(text));
    Source {
        conversation_id: "19:synthetic@thread.v2".to_owned(),
        id: id.to_owned(),
        version: 9,
        client_message_id: client.map(str::to_owned),
        from: "Lab A".to_owned(),
        own: true,
        edit_time: edited,
        line: one_line(&text, CONTEXT_LINE),
        text,
    }
}

fn original(key: &str, sent: &str, at: i64) -> SentOriginal {
    SentOriginal {
        conversation_id: "19:synthetic@thread.v2".to_owned(),
        key: key.to_owned(),
        language: "de".to_owned(),
        content: serde_json::to_string(&RichText::plain("Wie geschrieben")).unwrap(),
        sent_text: sent.to_owned(),
        created_at: at,
    }
}

#[test]
fn a_sent_original_is_matched_by_its_post_or_its_edit() {
    let post = source("1", Some("client-1"), None, "As sent");
    let kept = [original("client-1", "Something else", 0)];
    // a post's by its row, unless it was edited since to other words
    assert!(stored_original(&post, "de", &kept).is_some());
    assert!(
        stored_original(&post, "fr", &kept).is_none(),
        "another language's isn't it"
    );
    let edited = source("1", Some("client-1"), Some(50), "Changed elsewhere");
    assert!(stored_original(&edited, "de", &kept).is_none());
    // an edit's by its message: the words sent, or an edit made after it was kept
    let edit = source("1", None, Some(1_000_000), "Edited text");
    assert!(stored_original(&edit, "de", &[original("edit:1", "Edited text", 5_000_000)]).is_some());
    assert!(stored_original(&edit, "de", &[original("edit:1", "Other", 1_000_000 + 10)]).is_some());
    assert!(stored_original(&edit, "de", &[original("edit:1", "Other", 1_000_000 + EDIT_SKEW + 1)]).is_none());
    assert!(stored_original(&source("1", None, None, "x"), "de", &[original("edit:1", "Other", 0)]).is_none());
}

#[test]
fn kept_rows_read_back_and_outgoing_answers_are_taken() {
    let message = source("1", None, None, "Hello");
    for entry in [
        Entry::Translated {
            version: 9,
            text: Arc::new(RichText::plain("Hallo")),
            source: Some("en".to_owned()),
        },
        Entry::Same {
            version: 9,
            source: Some("de".to_owned()),
        },
        Entry::Original {
            version: 9,
            text: Arc::new(RichText::plain("Hallo")),
        },
    ] {
        let row = row_of(&message, "de", &entry).unwrap();
        assert_eq!(entry_of_row(&row), Some(entry));
    }
    assert_eq!(row_of(&message, "de", &Entry::Failed { version: 9 }), None);
    let mut broken = row_of(
        &message,
        "de",
        &Entry::Original {
            version: 9,
            text: Arc::new(RichText::plain("x")),
        },
    )
    .unwrap();
    broken.content = "not json".to_owned();
    assert_eq!(entry_of_row(&broken), None);

    let (_, shells) = codec::encode(&RichText::plain("Hallo"));
    assert_eq!(
        outgoing_of(Ok((Some("Hello".to_owned()), Some("en".to_owned()))), &shells)
            .map(|text| text.map(|text| text.plain_text())),
        Ok(Some("Hello".to_owned()))
    );
    assert_eq!(outgoing_of(Ok((None, None)), &shells), Ok(None));
    assert_eq!(
        outgoing_of(Err(Failure::TimedOut(Backend::Codex)), &shells),
        Err(SendFailure::Tool(Failure::TimedOut(Backend::Codex)))
    );
    assert!(translatable(&Message {
        conversation_id: String::new(),
        id: "1".to_owned(),
        version: 1,
        client_message_id: None,
        sender: None,
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: "<p>hi</p>".to_owned(),
        properties: None,
        arrival_time: 0,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }));
}

#[test]
fn what_the_view_shows_follows_the_entries() {
    let message = Message {
        conversation_id: "19:synthetic@thread.v2".to_owned(),
        id: "1".to_owned(),
        version: 3,
        client_message_id: None,
        sender: None,
        sender_name: None,
        message_type: "Text".to_owned(),
        content: "Hello".to_owned(),
        properties: None,
        arrival_time: 0,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    };
    let mut entries = HashMap::new();
    let mut shown = Shown {
        language: Some("de"),
        ..Shown::default()
    };
    assert_eq!(shown.mark_of(&message), Some(Mark::Waiting));
    shown.failed = true;
    assert_eq!(
        shown.mark_of(&message),
        None,
        "a stopped translation marks nothing it didn't do"
    );
    entries.insert("1".to_owned(), Entry::Failed { version: 3 });
    shown.entries = Arc::new(entries.clone());
    assert_eq!(shown.mark_of(&message), Some(Mark::Failed));
    entries.insert(
        "1".to_owned(),
        Entry::Translated {
            version: 2,
            text: Arc::new(RichText::plain("old")),
            source: None,
        },
    );
    shown.entries = Arc::new(entries.clone());
    assert_eq!(shown.text_of(&message), None, "an older version's isn't shown");
    entries.insert(
        "1".to_owned(),
        Entry::Translated {
            version: 3,
            text: Arc::new(RichText::plain("Hallo")),
            source: Some("xx".to_owned()),
        },
    );
    shown.entries = Arc::new(entries.clone());
    assert_eq!(
        shown.mark_of(&message),
        Some(Mark::Translated {
            tip: "See original message".into()
        })
    );
    entries.insert(
        "1".to_owned(),
        Entry::Original {
            version: 3,
            text: Arc::new(RichText::plain("Hallo")),
        },
    );
    shown.entries = Arc::new(entries);
    shown.originals = Arc::new(["1".to_owned()].into());
    assert_eq!(
        shown.mark_of(&message),
        Some(Mark::Original {
            tip: "See what you wrote (German)".into()
        })
    );
    assert_eq!(shown.text_of(&message), None);
    shown.language = None;
    assert_eq!(shown.mark_of(&message), None);
    assert_eq!(shown.original_of_sent(&RichText::plain("x")), None);
}

/// A source replying to `to` (its id), with the preview Teams quotes.
fn reply_source(id: &str, to: &str, text: &str) -> Source {
    let text = Arc::new(parse_html(&format!(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"{to}\"><strong \
         itemprop=\"mri\" itemid=\"8:orgid:synthetic-b\">Lab B</strong><p itemprop=\"preview\">Which…</p>\
         </blockquote><p>{text}</p>"
    )));
    Source {
        line: one_line(&text, CONTEXT_LINE),
        text,
        ..source(id, None, None, "")
    }
}

#[test]
fn a_window_holds_the_nearest_lines_and_every_message_replied_to() {
    let mut sources: Vec<Source> = (0..CONTEXT_LINES + 20)
        .map(|n| source(&format!("m{n}"), None, None, &format!("line {n}")))
        .collect();
    let last = sources.len();
    // the newest replies to the very first message (far out of the window), the one before to the newest
    sources.push(reply_source("r1", "m0", "That one"));
    sources.push(reply_source("r2", "r1", "Agreed"));
    let picked = [last, last + 1];
    let replies: Vec<Vec<String>> = picked.iter().map(|at| brief::replied_to(&sources[*at].text)).collect();
    assert_eq!(replies, [vec!["m0".to_owned()], vec!["r1".to_owned()]]);
    let (context, re) = window(&sources, &picked, &replies, last + 1);
    assert_eq!(re, [Some("q1".to_owned()), Some("1".to_owned())]);
    assert_eq!(context.len(), CONTEXT_LINES + 1);
    assert_eq!(context[0].text, "line 0");
    assert_eq!(context[0].label.as_deref(), Some("q1"));
    assert_eq!(context[1].text, format!("line {}", last - CONTEXT_LINES));
    assert_eq!(
        context.last().map(|said| said.text.as_str()),
        Some(&*format!("line {}", last - 1))
    );
    assert!(context.iter().skip(1).all(|said| said.label.is_none()));
    assert_eq!(context[0].key, key_of(&sources[0]));
    // a reply to a message not held names nothing; the same one twice has one label
    let replies = vec![vec!["gone".to_owned()], vec!["m0".to_owned()], vec!["m0".to_owned()]];
    let (context, re) = window(&sources, &[], &replies, 2);
    assert_eq!(re, [None, Some("q1".to_owned()), Some("q1".to_owned())]);
    assert_eq!(context.len(), 2);
    // the characters bound the window too
    let long: Vec<Source> = (0..3)
        .map(|n| source(&format!("l{n}"), None, None, &"w".repeat(CONTEXT_LINE)))
        .map(|mut long| {
            long.line = "w".repeat(CONTEXT_CHARS / 2);
            long
        })
        .collect();
    assert_eq!(window(&long, &[], &[], 3).0.len(), 2);
}
