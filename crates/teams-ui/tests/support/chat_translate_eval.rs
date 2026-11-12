//! A chat translation's quality in context, measured against the real local tool (Claude Code or Codex, whichever the
//! app would pick): made-up chats (SYNTHETIC: no real people, no real chat content) where context matters — pronouns
//! and gendered forms, an ambiguous word the chat's topic settles, a reply to an earlier question, a recurring term
//! across a new tool process, and the account's own message in the chat's form of address. Each chat's checked
//! messages are printed with their translation, the prompt's size and the batch's latency, for a person to compare.
//!
//! Ignored: it runs the user's own tool and costs its tokens. Run it on purpose:
//! `NGTEAMS_TRANSLATE_EVAL=1 cargo test -p teams-ui --lib chat_translate::eval -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::Instant;

use teams_proto::chat::{RichText, parse_html};

use super::brief::replied_to;
use super::codec::{decode, encode, one_line};
use super::prompt::{self, Brief, Item, Term};
use super::session::{Given, Job, Limits, Outcome, Session, Spec};
use super::{CONTEXT_LINE, Source, key_of, window};
use crate::local_ai::{Model, ToolChoice, Tools};

/// One made-up message: who, what, and the earlier message it replies to (its index) with the preview Teams quotes.
struct Said0 {
    from: &'static str,
    text: String,
    quote: Option<(usize, &'static str)>,
}

fn said(from: &'static str, text: impl Into<String>) -> Said0 {
    Said0 {
        from,
        text: text.into(),
        quote: None,
    }
}

fn reply(from: &'static str, to: usize, preview: &'static str, text: &'static str) -> Said0 {
    Said0 {
        from,
        text: text.to_owned(),
        quote: Some((to, preview)),
    }
}

/// What a chat checks.
enum Check {
    /// The newest batch translated, these messages' translations shown.
    Batch(Vec<usize>),
    /// The first `.0` messages translated by one process, then the last message by a new one: are terms kept?
    NewProcess(usize),
    /// The account's own messages translated into the chat's language.
    Outgoing(Vec<&'static str>),
}

struct Chat {
    name: &'static str,
    title: &'static str,
    you: &'static str,
    members: Vec<&'static str>,
    /// The reader's language's English name.
    language: &'static str,
    /// The chat's language's code (for an outgoing message).
    chat_language: &'static str,
    messages: Vec<Said0>,
    check: Check,
}

fn message_id(at: usize) -> String {
    format!("{}", 1_700_000_000_000_u64 + at as u64)
}

fn rich(chat: &Chat, at: usize) -> RichText {
    let message = &chat.messages[at];
    match message.quote {
        None => RichText::plain(&message.text),
        Some((to, preview)) => parse_html(&format!(
            "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"{id}\"><strong \
             itemprop=\"mri\" itemid=\"8:orgid:synthetic-{to}\">{who}</strong><span itemprop=\"time\" \
             itemid=\"{id}\"></span><p itemprop=\"preview\">{preview}</p></blockquote><p>{text}</p>",
            id = message_id(to),
            who = chat.messages[to].from,
            text = message.text,
        )),
    }
}

/// Twenty-odd lines of everyday office chat in German, about nothing the checks need.
fn german_filler(count: usize) -> Vec<Said0> {
    const LINES: [(&str, &str); 12] = [
        ("Mia Schulz", "Der Drucker im zweiten Stock geht wieder."),
        ("Jonas Weber", "Danke für die Info!"),
        ("Mia Schulz", "Wer hat noch Urlaubstage für Mai eingetragen?"),
        ("Lena Brandt", "Ich, die Woche nach Pfingsten."),
        ("Jonas Weber", "Ticket {n} ist erledigt."),
        ("Mia Schulz", "Die Kantine hat heute nur bis 13 Uhr offen."),
        ("Lena Brandt", "Gut zu wissen."),
        ("Jonas Weber", "Kann jemand kurz auf meinen Merge Request schauen?"),
        ("Mia Schulz", "Mach ich nach dem Mittag."),
        ("Lena Brandt", "Die Zahlen für Oktober liegen im Ordner."),
        ("Jonas Weber", "Super, danke."),
        ("Mia Schulz", "Bis morgen zusammen!"),
    ];
    (0..count)
        .map(|n| {
            let (from, text) = LINES[n % LINES.len()];
            said(from, text.replace("{n}", &format!("{}", 4100 + n)))
        })
        .collect()
}

fn chats() -> Vec<Chat> {
    vec![
        Chat {
            name: "gender (en→ru): Robin is she, Sam is he, said only early on",
            title: "Northwind onboarding",
            you: "Dana Whitfield",
            members: vec!["Dana Whitfield", "Chris Ortega", "Robin Park", "Sam Lee"],
            language: "Russian",
            chat_language: "en",
            messages: vec![
                said(
                    "Chris Ortega",
                    "Morning all! Kicking off the Northwind onboarding thread here.",
                ),
                said("Dana Whitfield", "Thanks Chris. I'll keep the tracker updated."),
                said(
                    "Chris Ortega",
                    "Robin has the client relationship, so she'll run the weekly calls with them.",
                ),
                said("Robin Park", "Yes, I'll send the invite today."),
                said("Chris Ortega", "Sam is out until Wednesday, he's got the flu."),
                said("Dana Whitfield", "Get well soon, Sam!"),
                said("Robin Park", "Invite is out for Thursday 10:00."),
                said("Chris Ortega", "Can someone check the SSO config?"),
                said("Dana Whitfield", "On it."),
                said("Dana Whitfield", "SSO works in staging, prod tomorrow."),
                said("Chris Ortega", "Great. What about the data import?"),
                said("Robin Park", "The CSV export from their old system is still missing."),
                said("Chris Ortega", "Ok, let's chase it on Thursday."),
                said("Dana Whitfield", "Tracker updated."),
                said("Chris Ortega", "Reminder: no deploys on Friday."),
                said("Robin Park", "Noted."),
                said("Dana Whitfield", "Prod SSO is live."),
                said("Chris Ortega", "Nice work!"),
                said("Robin Park", "The CSV arrived, 12k rows."),
                said("Dana Whitfield", "I'll run the import tonight."),
                said("Chris Ortega", "Any errors?"),
                said("Dana Whitfield", "Only 14 rows with bad dates, fixed by hand."),
                said("Chris Ortega", "Perfect."),
                said("Sam Lee", "I'm back! Thanks for covering for me."),
                said("Chris Ortega", "Welcome back Sam."),
                said("Dana Whitfield", "Sam, quick catch-up later?"),
                said("Sam Lee", "Sure, after lunch."),
                said("Chris Ortega", "How did the client visit go yesterday?"),
                said(
                    "Robin Park",
                    "I was at their office for three hours, we walked through the whole rollout.",
                ),
                said("Chris Ortega", "And the contract?"),
                said("Robin Park", "Signed. I brought the original back with me."),
                said(
                    "Sam Lee",
                    "I was still sick yesterday, otherwise I would have come along.",
                ),
                said("Chris Ortega", "No worries. Robin, did they ask about training?"),
                said("Robin Park", "They did. I promised them two sessions next week."),
                said("Dana Whitfield", "I can run one of them."),
                said(
                    "Sam Lee",
                    "I'll take the other one, I wrote the training material anyway.",
                ),
                said("Chris Ortega", "Great, thanks both."),
                said("Robin Park", "I'll tell their team lead, she was asking about it."),
                said("Chris Ortega", "Let's wrap up the week then."),
            ],
            check: Check::Batch(vec![28, 30, 31, 35]),
        },
        Chat {
            name: "topic (ru→en): кран and ключ in a plumbing chat",
            title: "Ремонт сантехники, 3 этаж",
            you: "Alex Turner",
            members: vec!["Alex Turner", "Игорь Петров", "Марина Соколова"],
            language: "English",
            chat_language: "ru",
            messages: vec![
                said("Игорь Петров", "Всем привет, по третьему этажу начинаем в понедельник."),
                said("Марина Соколова", "Хорошо. Доступ в помещение через охрану."),
                said("Игорь Петров", "Пропуска заказал на троих."),
                said("Alex Turner", "Спасибо!"),
                said("Марина Соколова", "Кран привезут в пятницу, раньше не получается."),
                said("Игорь Петров", "Тогда старый пока не снимаем."),
                said("Марина Соколова", "Ключ кто-нибудь брал? Не могу найти."),
                said("Игорь Петров", "Ключ у меня, верну завтра."),
                said("Марина Соколова", "Счёт за кран пришлю отдельно."),
                said("Игорь Петров", "Хорошо, жду."),
            ],
            check: Check::Batch(vec![4, 6, 7, 8]),
        },
        Chat {
            name: "reply (de→en): 'Die nehmen wir' answers a question 40+ messages back",
            title: "Offsite im März",
            you: "Alex Turner",
            members: vec!["Alex Turner", "Lena Brandt", "Jonas Weber", "Mia Schulz"],
            language: "English",
            chat_language: "de",
            messages: {
                let mut messages = vec![said(
                    "Lena Brandt",
                    "Für das Offsite im März haben wir zwei Angebote: das Hotel am See (teurer, aber mit eigenem \
                     Tagungsraum) oder die Jugendherberge in der Altstadt. Welches nehmen wir?",
                )];
                messages.extend(german_filler(44));
                messages.push(reply(
                    "Jonas Weber",
                    0,
                    "Für das Offsite im März haben wir zwei Angebote: das Hotel am See (teurer, aber mit…",
                    "Die nehmen wir. Ist eh näher am Bahnhof.",
                ));
                messages.push(said("Mia Schulz", "Einverstanden."));
                messages
            },
            check: Check::Batch(vec![45]),
        },
        Chat {
            name: "terms across a new process (de→en): Abnahme, Lastenheft",
            title: "Projekt Falke",
            you: "Alex Turner",
            members: vec![
                "Alex Turner",
                "Katrin Roth",
                "Tobias Klein",
                "Mia Schulz",
                "Jonas Weber",
                "Lena Brandt",
            ],
            language: "English",
            chat_language: "de",
            messages: {
                let mut messages = vec![
                    said(
                        "Katrin Roth",
                        "Kurzes Update zu Falke: die Abnahme mit dem Kunden ist für den 14. geplant.",
                    ),
                    said("Tobias Klein", "Ist das Lastenheft final?"),
                    said(
                        "Katrin Roth",
                        "Fast, im Lastenheft fehlt noch das Kapitel zur Datenmigration.",
                    ),
                    said(
                        "Tobias Klein",
                        "Ohne vollständiges Lastenheft keine Abnahme, das hat der Kunde klar gesagt.",
                    ),
                    said("Katrin Roth", "Ich kümmere mich drum."),
                ];
                messages.extend(german_filler(44));
                messages.push(said(
                    "Katrin Roth",
                    "Die Abnahme rutscht auf Donnerstag, das überarbeitete Lastenheft geht morgen raus.",
                ));
                messages
            },
            check: Check::NewProcess(5),
        },
        Chat {
            name: "outgoing (en→de): a client chat that says Sie, said only early on",
            title: "Kundenprojekt Hartmann GmbH – Abstimmung",
            you: "Jamie Fox",
            members: vec!["Jamie Fox", "Klaus Hartmann", "Petra Vogel"],
            language: "English",
            chat_language: "de",
            messages: {
                let mut messages = vec![
                    said(
                        "Klaus Hartmann",
                        "Sehr geehrter Herr Fox, vielen Dank für Ihr Angebot. Wir würden gern mit Ihnen starten.",
                    ),
                    said(
                        "Jamie Fox",
                        "Vielen Dank, Herr Hartmann! Ich freue mich auf die Zusammenarbeit.",
                    ),
                    said(
                        "Petra Vogel",
                        "Guten Tag Herr Fox, ich bin Ihre Ansprechpartnerin für die Technik.",
                    ),
                ];
                for (from, text) in [
                    ("Klaus Hartmann", "Anbei die Unterlagen."),
                    ("Jamie Fox", "Danke, ist angekommen."),
                    ("Petra Vogel", "Die Zugänge sind eingerichtet."),
                    ("Klaus Hartmann", "Termin am Dienstag passt."),
                    ("Jamie Fox", "Prima."),
                    ("Petra Vogel", "Der Testserver ist ab morgen erreichbar."),
                    ("Klaus Hartmann", "Danke!"),
                    ("Petra Vogel", "Die Logs liegen im geteilten Ordner."),
                    ("Jamie Fox", "Gefunden."),
                    ("Klaus Hartmann", "Bis Dienstag."),
                    ("Petra Vogel", "Kleine Änderung: Beginn 9:30 statt 9:00."),
                    ("Jamie Fox", "Passt."),
                    ("Klaus Hartmann", "Protokoll folgt."),
                    ("Petra Vogel", "VPN-Zugang ist erneuert."),
                    ("Klaus Hartmann", "Rechnung ist raus."),
                    ("Petra Vogel", "Backup läuft wieder."),
                    ("Klaus Hartmann", "Danke für den schnellen Support."),
                    ("Jamie Fox", "Gern."),
                ] {
                    messages.push(said(from, text));
                }
                messages
            },
            check: Check::Outgoing(vec![
                "Could you send me the signed contract by Friday? Thanks a lot!",
                "Petra, can you check the VPN access again? It drops every hour.",
            ]),
        },
    ]
}

/// What one run of a chat found.
struct Found {
    lines: Vec<String>,
}

fn next(session: &Session) -> (Outcome, Option<std::time::Duration>, std::time::Duration) {
    let started = Instant::now();
    let mut first = None;
    let outcomes = session.outcomes();
    loop {
        let outcome = outcomes.recv_blocking().expect("the session ended");
        if let Outcome::Partial { .. } = outcome {
            first.get_or_insert_with(|| started.elapsed());
            continue;
        }
        return (outcome, first, started.elapsed());
    }
}

/// The chat's messages as the view holds them.
fn sources(chat: &Chat) -> Vec<Source> {
    (0..chat.messages.len())
        .map(|at| {
            let text = Arc::new(rich(chat, at));
            Source {
                conversation_id: "19:synthetic-eval@thread.v2".to_owned(),
                id: message_id(at),
                version: 1,
                client_message_id: None,
                from: chat.messages[at].from.to_owned(),
                own: chat.messages[at].from == chat.you,
                edit_time: None,
                line: one_line(&text, CONTEXT_LINE),
                text,
            }
        })
        .collect()
}

/// What the tool is told of the chat, as the store would have it.
fn brief_of(chat: &Chat, terms: Vec<Term>) -> Arc<Brief> {
    Arc::new(Brief {
        conversation: "19:synthetic-eval@thread.v2".to_owned(),
        topic: Some(chat.title.to_owned()),
        you: Some(chat.you.to_owned()),
        members: chat
            .members
            .iter()
            .filter(|name| **name != chat.you)
            .map(|name| (*name).to_owned())
            .collect(),
        terms,
    })
}

fn start(language: &'static str) -> Session {
    let (backend, program) = Tools::detect()
        .pick(ToolChoice::Automatic)
        .expect("no claude or codex on the PATH");
    Session::start(Spec {
        backend,
        program,
        model: Model::Recommended,
        language,
        limits: Limits::default(),
    })
}

/// Translates messages `range` of `chat` as one batch in `session` (whose process was told `given`), as the view
/// would: the checked ones printed; the terms its answer added given back.
fn translate(
    chat: &Chat,
    (session, given): (&Session, &mut Given),
    brief: &Arc<Brief>,
    range: std::ops::Range<usize>,
    checked: &[usize],
    found: &mut Found,
) -> Vec<Term> {
    let held = sources(chat);
    let picked: Vec<usize> = range.clone().collect();
    let replies: Vec<Vec<String>> = picked.iter().map(|at| replied_to(&held[*at].text)).collect();
    let (context, re) = window(&held, &picked, &replies, range.end - 1);
    let mut items = Vec::new();
    let mut shells = Vec::new();
    for ((n, at), re) in range.clone().enumerate().zip(re) {
        let (text, shell) = encode(&held[at].text);
        items.push(Item {
            n: n + 1,
            key: key_of(&held[at]),
            from: chat.messages[at].from.to_owned(),
            text,
            re,
        });
        shells.push(shell);
    }
    let preface = given.preface(brief, &context);
    let prompt = prompt::batch(&preface, &items);
    if let Some(dir) = std::env::var_os("NGTEAMS_TRANSLATE_EVAL_DUMP") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::write(dir.join("instructions.txt"), prompt::instructions(chat.language));
        let _ = std::fs::write(dir.join(format!("batch-{}.txt", range.start)), &prompt);
    }
    let lines = preface.context.len();
    session.send(Job::Batch {
        id: 1,
        brief: Arc::clone(brief),
        context: context.clone(),
        items: items.clone(),
    });
    let (outcome, first, all) = next(session);
    found.lines.push(format!(
        "  batch of {} (+{lines} context lines): prompt {} chars; first answer {first:?}, all {all:?}",
        range.len(),
        prompt.len(),
    ));
    let Outcome::Batch {
        answers: Ok(answers),
        terms,
        ..
    } = outcome
    else {
        found.lines.push(format!("  failed: {outcome:?}"));
        return Vec::new();
    };
    let preface = given.preface(brief, &context);
    given.told(&preface, &items, &terms);
    for at in checked {
        let n = at - range.start + 1;
        let shown = match answers.get(&n) {
            Some(prompt::Answer::Translated { text, source }) => format!(
                "{} (said to be {})",
                decode(text, &shells[n - 1]).plain_text(),
                source.as_deref().unwrap_or("?")
            ),
            Some(prompt::Answer::Same { .. }) => "(same)".to_owned(),
            None => "(left out)".to_owned(),
        };
        found.lines.push(format!(
            "  [{at}] {}: {}\n      → {}",
            chat.messages[*at].from,
            one_line(&rich(chat, *at), 400),
            shown.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    for term in &terms {
        found.lines.push(format!("  term: {} = {}", term.term, term.rendering));
    }
    terms
}

fn run(chat: &Chat) -> Found {
    let mut found = Found { lines: Vec::new() };
    let last = chat.messages.len();
    match &chat.check {
        Check::Batch(checked) => {
            let session = start(chat.language);
            let brief = brief_of(chat, Vec::new());
            let range = last.saturating_sub(12)..last;
            translate(
                chat,
                (&session, &mut Given::default()),
                &brief,
                range,
                checked,
                &mut found,
            );
        }
        Check::NewProcess(first) => {
            let session = start(chat.language);
            let brief = brief_of(chat, Vec::new());
            let checked: Vec<usize> = (0..*first).collect();
            let terms = translate(
                chat,
                (&session, &mut Given::default()),
                &brief,
                0..*first,
                &checked,
                &mut found,
            );
            drop(session);
            // a new process, told the glossary the first one's answer started
            let session = start(chat.language);
            let brief = brief_of(chat, terms);
            translate(
                chat,
                (&session, &mut Given::default()),
                &brief,
                last - 1..last,
                &[last - 1],
                &mut found,
            );
        }
        Check::Outgoing(texts) => {
            let session = start(chat.language);
            let brief = brief_of(chat, Vec::new());
            let held = sources(chat);
            let mut given = Given::default();
            for (n, text) in texts.iter().enumerate() {
                let (context, _) = window(&held, &[], &[], held.len());
                let (encoded, shells) = encode(&RichText::plain(text));
                let preface = given.preface(&brief, &context);
                let prompt = prompt::outgoing(&preface, Some(chat.chat_language), None, &encoded);
                session.send(Job::Outgoing {
                    id: n as u64 + 1,
                    brief: Arc::clone(&brief),
                    context: context.clone(),
                    to: Some(chat.chat_language.to_owned()),
                    re: None,
                    text: encoded,
                });
                let (outcome, _, all) = next(&session);
                given.told(&preface, &[], &[]);
                let shown = match outcome {
                    Outcome::Outgoing {
                        answer: Ok((Some(text), _)),
                        ..
                    } => decode(&text, &shells).plain_text(),
                    other => format!("{other:?}"),
                };
                found.lines.push(format!(
                    "  outgoing: prompt {} chars, {all:?}\n  {text}\n      → {shown}",
                    prompt.len()
                ));
            }
        }
    }
    found
}

#[test]
#[ignore = "runs the user's own claude or codex"]
fn chats_translated_in_context() {
    if std::env::var_os("NGTEAMS_TRANSLATE_EVAL").is_none() {
        return;
    }
    let only = std::env::var("NGTEAMS_TRANSLATE_EVAL_ONLY").ok();
    let runs: usize = std::env::var("NGTEAMS_TRANSLATE_EVAL_RUNS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(1);
    println!("instructions: {} chars", prompt::instructions("English").len());
    for chat in chats() {
        if only.as_deref().is_some_and(|only| !chat.name.contains(only)) {
            continue;
        }
        for run_n in 1..=runs {
            println!("== {} (run {run_n})", chat.name);
            for line in run(&chat).lines {
                println!("{line}");
            }
        }
    }
}
