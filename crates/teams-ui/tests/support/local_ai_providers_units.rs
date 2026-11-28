//! The AI providers' list: the tools found added to it, Automatic's order, each pick's failure, and the settings
//! file's form of the list (never a key in it).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use teams_auth::secrets::Secret;

use super::*;
use crate::local_ai::{Model, Target, Work};

fn tools(found: &[Backend]) -> Tools {
    Tools::with(
        found
            .iter()
            .map(|backend| (*backend, PathBuf::from(format!("/synthetic/{}", backend.key()))))
            .collect(),
    )
}

fn api(id: &str, kind: Backend, has_key: bool) -> Provider {
    Provider {
        id: id.to_owned(),
        kind,
        name: format!("Synthetic {id}"),
        enabled: true,
        base_url: (kind == Backend::Compatible).then(|| "http://127.0.0.1:11434/v1".to_owned()),
        model: None,
        has_key,
    }
}

fn picked(list: &[Provider], found: &Tools, choice: &ToolChoice) -> Result<(Backend, String), Failure> {
    pick_from(list, found, &AiKeys::default(), choice).map(|picked| (picked.backend, picked.name))
}

#[test]
fn the_tools_found_come_after_the_users_providers_claude_first() {
    let both = tools(&[Backend::Codex, Backend::Claude]);
    let list = effective(&[], &both);
    assert_eq!(
        list.iter().map(|provider| provider.id.as_str()).collect::<Vec<_>>(),
        ["claude", "codex"]
    );
    assert!(list.iter().all(|provider| provider.enabled));
    let mut codex_off = Provider::cli(Backend::Codex);
    codex_off.enabled = false;
    let list = effective(&[api("api-1", Backend::OpenAi, true), codex_off.clone()], &both);
    assert_eq!(
        list.iter().map(|provider| provider.id.as_str()).collect::<Vec<_>>(),
        ["api-1", "codex", "claude"]
    );
    // a tool the user turned off stays off
    assert!(!list[1].enabled);
    assert!(effective(&[], &Tools::default()).is_empty());
}

#[test]
fn automatic_is_the_first_provider_on_that_can_work() {
    let both = tools(&[Backend::Claude, Backend::Codex]);
    let list = effective(&[], &both);
    assert_eq!(picked(&list, &both, &ToolChoice::Automatic).unwrap().0, Backend::Claude);
    let codex = tools(&[Backend::Codex]);
    let list = effective(&[], &codex);
    assert_eq!(picked(&list, &codex, &ToolChoice::Automatic).unwrap().0, Backend::Codex);
    // an API without its key and a tool turned off are passed over
    let mut claude_off = Provider::cli(Backend::Claude);
    claude_off.enabled = false;
    let list = effective(
        &[
            api("api-1", Backend::Anthropic, false),
            claude_off,
            api("api-2", Backend::OpenAi, true),
        ],
        &both,
    );
    assert_eq!(
        picked(&list, &both, &ToolChoice::Automatic).unwrap(),
        (Backend::OpenAi, "Synthetic api-2".to_owned())
    );
    let list = effective(&[api("api-1", Backend::Compatible, false)], &both);
    assert_eq!(
        picked(&list, &both, &ToolChoice::Automatic).unwrap().0,
        Backend::Compatible
    );
    assert_eq!(
        picked(&[], &Tools::default(), &ToolChoice::Automatic),
        Err(Failure::NotInstalled(None))
    );
}

#[test]
fn a_provider_chosen_says_why_it_cannot_work() {
    let none = Tools::default();
    assert_eq!(
        picked(&[], &none, &ToolChoice::cli(Backend::Claude)),
        Err(Failure::NotInstalled(Some(Backend::Claude)))
    );
    assert_eq!(
        picked(
            &[Provider::cli(Backend::Codex)],
            &none,
            &ToolChoice::cli(Backend::Codex)
        ),
        Err(Failure::NotInstalled(Some(Backend::Codex)))
    );
    assert_eq!(
        picked(&[], &none, &ToolChoice::Only("api-9".to_owned())),
        Err(Failure::NoProvider)
    );
    let mut off = api("api-1", Backend::OpenAi, true);
    off.enabled = false;
    assert_eq!(
        picked(&[off], &none, &ToolChoice::Only("api-1".to_owned())),
        Err(Failure::TurnedOff(Backend::OpenAi).of("Synthetic api-1"))
    );
    assert_eq!(
        picked(
            &[api("api-1", Backend::Anthropic, false)],
            &none,
            &ToolChoice::Only("api-1".to_owned())
        ),
        Err(Failure::NoKey(Backend::Anthropic).of("Synthetic api-1"))
    );
}

#[test]
fn a_picks_failure_names_the_api_provider_that_the_user_named() {
    let none = Tools::default();
    let mut off = api("api-1", Backend::OpenAi, true);
    off.name = "QA OpenAI".to_owned();
    off.enabled = false;
    let mut no_key = api("api-2", Backend::Anthropic, false);
    no_key.name = "QA Anthropic".to_owned();
    let list = [off, no_key];
    let message = |id: &str| {
        pick_from(&list, &none, &AiKeys::default(), &ToolChoice::Only(id.to_owned()))
            .unwrap_err()
            .message()
    };
    assert_eq!(
        message("api-1"),
        "QA OpenAI is turned off in Settings under General, AI providers."
    );
    assert_eq!(
        message("api-2"),
        "QA Anthropic has no API key. Add one in Settings under General, AI providers."
    );
}

#[test]
fn the_progress_names_the_api_provider_and_its_model() {
    let found = tools(&[Backend::Claude]);
    let mut openai = api("api-1", Backend::OpenAi, true);
    openai.name = "QA OpenAI".to_owned();
    let mut compatible = api("api-2", Backend::Compatible, false);
    compatible.name = "QA Server".to_owned();
    let mut served = api("api-3", Backend::Compatible, false);
    served.name = "QA Served".to_owned();
    served.model = Some("qa-own-model".to_owned());
    let list = [openai, compatible, served, Provider::cli(Backend::Claude)];
    let label = |id: &str, work: Work, model: &Model| {
        pick_from(&list, &found, &AiKeys::default(), &ToolChoice::Only(id.to_owned()))
            .unwrap()
            .label(work, model)
    };
    assert_eq!(
        label("api-1", Work::Draft, &Model::Recommended),
        "QA OpenAI (GPT-6.1-Terra)"
    );
    assert_eq!(
        label("api-1", Work::Chat, &Model::Recommended),
        "QA OpenAI (GPT-6-Luna)"
    );
    assert_eq!(
        label("api-1", Work::Draft, &Model::Named("qa-model-7".to_owned())),
        "QA OpenAI (qa-model-7)"
    );
    assert_eq!(label("api-2", Work::Draft, &Model::Recommended), "QA Server");
    assert_eq!(
        label("api-3", Work::Draft, &Model::ToolDefault),
        "QA Served (qa-own-model)"
    );
    // a command-line tool is one of its kind: its words stay
    assert_eq!(label("claude", Work::Draft, &Model::Recommended), "Claude Sonnet");
}

#[test]
fn a_pick_gives_a_tools_program_or_an_apis_target() {
    let claude = tools(&[Backend::Claude]);
    let picked = pick_from(
        &effective(&[], &claude),
        &claude,
        &AiKeys::default(),
        &ToolChoice::cli(Backend::Claude),
    )
    .unwrap();
    assert!(matches!(picked.target, Target::Program(path) if path == Path::new("/synthetic/claude")));
    let keys = AiKeys::new(Arc::new(MemoryKeys::default()));
    keys.put("api-1", &Secret::new("sk-synthetic")).unwrap();
    let mut provider = api("api-1", Backend::OpenAi, true);
    provider.model = Some("own-model".to_owned());
    let picked = pick_from(&[provider], &claude, &keys, &ToolChoice::Only("api-1".to_owned())).unwrap();
    let Target::Api(target) = picked.target else {
        panic!("not an API")
    };
    assert_eq!(target.base_url, "https://api.openai.com/v1");
    assert_eq!(target.model.as_deref(), Some("own-model"));
    assert_eq!(target.key(Backend::OpenAi).unwrap().unwrap().expose(), "sk-synthetic");
    assert!(!format!("{target:?}").contains("sk-synthetic"));
}

#[test]
fn each_failure_says_where_to_fix_it() {
    for failure in [
        Failure::NoKey(Backend::Anthropic),
        Failure::SignedOut(Backend::OpenAi),
        Failure::TurnedOff(Backend::Compatible),
    ] {
        assert!(failure.message().contains("AI providers"), "{}", failure.message());
    }
    assert!(Failure::SignedOut(Backend::Claude).message().contains("Run claude"));
    for failure in [
        Failure::NoCredit(Backend::OpenAi),
        Failure::RateLimited(Backend::Anthropic),
        Failure::UnknownModel(Backend::OpenAi, "gpt-x".to_owned()),
        Failure::NoModel(Backend::Compatible),
        Failure::NoProvider,
    ] {
        assert!(!failure.message().is_empty());
        assert_eq!(failure.install_hint(), "");
    }
}

/// A mock API on the loopback that lists three synthetic models, and counts the requests.
fn listing_mock() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead as _, BufReader, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line.trim_end() != "" {
                line.clear();
            }
            let body = r#"{"data":[{"id":"model-a"},{"id":"model-b"},{"id":"model-c"}]}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, asked)
}

#[gpui::test]
fn turning_ai_off_keeps_an_api_listing_unsent_and_on_again_lists_anew(cx: &mut gpui::TestAppContext) {
    let (base, asked) = listing_mock();
    let mut provider = api("api-1", Backend::Compatible, false);
    provider.base_url = Some(base);

    assert!(cx.update(|cx| ApiModels::of(&provider, cx)).is_none(), "being listed");
    cx.update(|cx| super::super::AiSwitch::publish(false, cx));
    cx.run_until_parked();
    assert_eq!(asked.load(Ordering::SeqCst), 0, "no request once AI went off");
    assert!(matches!(
        cx.update(|cx| ApiModels::of(&provider, cx)),
        Some(Err(Failure::Off))
    ));

    cx.update(|cx| super::super::AiSwitch::publish(true, cx));
    let mut listed = None;
    for _ in 0..500 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        listed = cx.update(|cx| ApiModels::of(&provider, cx));
        if listed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(listed.and_then(Result::ok).map(|models| models.len()), Some(3));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn a_listing_asked_before_ai_went_off_and_on_again_does_not_take_the_new_ones_place(cx: &mut gpui::TestAppContext) {
    let (base, asked) = listing_mock();
    let mut provider = api("api-1", Backend::Compatible, false);
    provider.base_url = Some(base);

    assert!(cx.update(|cx| ApiModels::of(&provider, cx)).is_none(), "being listed");
    cx.update(|cx| super::super::AiSwitch::publish(false, cx));
    cx.update(|cx| super::super::AiSwitch::publish(true, cx));
    assert!(cx.update(|cx| ApiModels::of(&provider, cx)).is_none(), "listed anew");
    let mut listed = None;
    for _ in 0..500 {
        cx.executor().allow_parking();
        cx.run_until_parked();
        listed = cx.update(|cx| ApiModels::of(&provider, cx));
        if listed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(listed.and_then(Result::ok).map(|models| models.len()), Some(3));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}
