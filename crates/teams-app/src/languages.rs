//! The languages the settings keep: the ones captions and chats are translated into ([`TARGET_LANGUAGES`], by code),
//! and a language a message is read in or a draft written in, by its name ([`language_name`]).

/// The languages captions can be translated into: a code and its English name.
pub const TARGET_LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("ca", "Catalan"),
    ("zh-hans", "Chinese (Simplified)"),
    ("zh-hant", "Chinese (Traditional)"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("et", "Estonian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("de", "German"),
    ("el", "Greek"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("lv", "Latvian"),
    ("lt", "Lithuanian"),
    ("nb", "Norwegian"),
    ("pl", "Polish"),
    ("pt-br", "Portuguese (Brazil)"),
    ("pt-pt", "Portuguese (Portugal)"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("th", "Thai"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("vi", "Vietnamese"),
];

/// A target language's code as kept, and its name.
#[must_use]
pub fn language(code: &str) -> Option<(&'static str, &'static str)> {
    TARGET_LANGUAGES.iter().copied().find(|(known, _)| *known == code)
}

/// The language a draft is translated into until the user picks one.
pub const DEFAULT_LANGUAGE: &str = "English";
/// The longest language name kept, in bytes.
pub const MAX_LANGUAGE: usize = 60;

/// The languages the picker lists: every language captions and chats are translated into, and more that many people
/// write in. Any other is typed.
pub const COMMON_LANGUAGES: &[&str] = &[
    "Afrikaans",
    "Albanian",
    "Amharic",
    "Arabic",
    "Armenian",
    "Azerbaijani",
    "Basque",
    "Belarusian",
    "Bengali",
    "Bosnian",
    "Bulgarian",
    "Burmese",
    "Catalan",
    "Chinese (Simplified)",
    "Chinese (Traditional)",
    "Croatian",
    "Czech",
    "Danish",
    "Dutch",
    "English",
    "Esperanto",
    "Estonian",
    "Filipino",
    "Finnish",
    "French",
    "Galician",
    "Georgian",
    "German",
    "Greek",
    "Gujarati",
    "Hausa",
    "Hebrew",
    "Hindi",
    "Hungarian",
    "Icelandic",
    "Indonesian",
    "Irish",
    "Italian",
    "Japanese",
    "Kannada",
    "Kazakh",
    "Khmer",
    "Korean",
    "Kurdish",
    "Kyrgyz",
    "Lao",
    "Latin",
    "Latvian",
    "Lithuanian",
    "Luxembourgish",
    "Macedonian",
    "Malay",
    "Malayalam",
    "Maltese",
    "Marathi",
    "Mongolian",
    "Nepali",
    "Norwegian",
    "Pashto",
    "Persian",
    "Polish",
    "Portuguese (Brazil)",
    "Portuguese (Portugal)",
    "Punjabi",
    "Romanian",
    "Russian",
    "Serbian",
    "Sinhala",
    "Slovak",
    "Slovenian",
    "Somali",
    "Spanish",
    "Swahili",
    "Swedish",
    "Tajik",
    "Tamil",
    "Telugu",
    "Thai",
    "Turkish",
    "Turkmen",
    "Ukrainian",
    "Urdu",
    "Uzbek",
    "Vietnamese",
    "Welsh",
    "Yiddish",
    "Yoruba",
    "Zulu",
];

/// The language `typed` names, as the settings keep it: a listed one by its own name (whatever its case, or by its
/// code), else what was typed, trimmed. `None` when it holds no letter, is longer than [`MAX_LANGUAGE`] bytes, or has a
/// character that could be read as markup or a line of its own.
#[must_use]
pub fn language_name(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if let Some(listed) = listed(typed) {
        return Some(listed.to_owned());
    }
    let shaped = typed.len() <= MAX_LANGUAGE
        && typed.chars().any(char::is_alphabetic)
        && !typed
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '"' | '`' | '{' | '}'));
    shaped.then(|| typed.to_owned())
}

/// The listed language `typed` names, by its name or (one of [`TARGET_LANGUAGES`]) its code, ignoring case.
#[must_use]
pub fn listed(typed: &str) -> Option<&'static str> {
    let typed = typed.trim();
    COMMON_LANGUAGES
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(typed))
        .or_else(|| {
            TARGET_LANGUAGES
                .iter()
                .find(|(code, _)| code.eq_ignore_ascii_case(typed))
                .map(|(_, name)| *name)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_language_is_kept_by_its_own_name_and_any_other_as_typed() {
        assert_eq!(language_name("german").as_deref(), Some("German"));
        assert_eq!(language_name("  FRENCH ").as_deref(), Some("French"));
        assert_eq!(language_name("de").as_deref(), Some("German"));
        assert_eq!(language_name("pt-br").as_deref(), Some("Portuguese (Brazil)"));
        assert_eq!(language_name(" Klingon ").as_deref(), Some("Klingon"));
        assert_eq!(language_name("Scots Gaelic").as_deref(), Some("Scots Gaelic"));
        assert_eq!(listed("swahili"), Some("Swahili"));
        assert_eq!(listed("Klingon"), None);
        assert_eq!(DEFAULT_LANGUAGE, "English");
    }

    #[test]
    fn a_language_that_could_be_read_as_markup_or_isnt_a_name_is_refused() {
        for bad in [
            "",
            "   ",
            "1234",
            "<x0/>German",
            "German</language>",
            "a & b",
            "Ger\nman",
            "\"quoted\"",
            "{curly}",
            "`code`",
        ] {
            assert_eq!(language_name(bad), None, "{bad:?}");
        }
        assert_eq!(
            language_name(&"a".repeat(MAX_LANGUAGE)).map(|name| name.len()),
            Some(MAX_LANGUAGE)
        );
        assert_eq!(language_name(&"a".repeat(MAX_LANGUAGE + 1)), None);
    }

    #[test]
    fn a_target_language_is_found_by_its_code() {
        assert_eq!(language("de"), Some(("de", "German")));
        assert_eq!(language("xx"), None);
    }
}
