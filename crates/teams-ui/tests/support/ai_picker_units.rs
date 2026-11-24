//! The note under the provider's dropdown when an API provider does the work: it names all that goes to the provider,
//! as the note of a command-line tool does.

use super::*;

const WORKS: [Work; 5] = [Work::Captions, Work::Summary, Work::Chat, Work::Correction, Work::Draft];

#[test]
fn an_api_providers_note_names_all_that_its_requests_carry() {
    assert_eq!(
        api_note(Work::Correction, "QA Compatible"),
        "When you ask for a correction, your message, the chat's recent messages and the message a reply quotes, \
         with the names of their writers, go to QA Compatible with your API key. The model runs on its server."
    );
    assert_eq!(
        api_note(Work::Chat, "QA OpenAI"),
        "When you translate a chat or one of its messages, its messages, its title, its members' names and your own \
         messages before they are sent go to QA OpenAI with your API key. The model runs on its server."
    );
    assert_eq!(
        api_note(Work::Draft, "QA Anthropic"),
        "When you translate a message you are writing, its text, the language, the chat's recent messages and the \
         message a reply quotes, with the names of their writers, go to QA Anthropic with your API key. The model \
         runs on its server."
    );
    assert_eq!(
        api_note(Work::Captions, "QA Anthropic"),
        "When you translate live captions, their text, the speakers' names, the call's title and the names of the \
         people in it go to QA Anthropic with your API key. The model runs on its server."
    );
    assert_eq!(
        api_note(Work::Summary, "QA OpenAI"),
        "When you ask for a recap's AI summary, its transcript, the meeting's title and date, and the names of the \
         people who spoke go to QA OpenAI with your API key. The model runs on its server."
    );
}

#[test]
fn an_api_providers_note_says_what_a_tools_note_says() {
    for work in WORKS {
        let tool = words(work).sends;
        assert!(tool.contains(ON_THIS_COMPUTER), "{tool}");
        let api = api_note(work, "QA");
        assert_eq!(
            api.replace("QA with your API key", ON_THIS_COMPUTER),
            format!("{tool} The model runs on its server."),
        );
        assert!(!api.contains(" goes to "), "{api}");
    }
}
