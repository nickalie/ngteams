//! The keys of the reactions Teams offers, as the settings keep them: the catalog's text
//! (`assets/reactions/catalog.tsv`, built by `scripts/reaction-pack.py`), which the desktop also draws its picker from,
//! and the reaction a key names with the skin tone it carries. A key of a reaction that takes a skin tone may carry the
//! tone Teams appends (`yes-tone2`, tones 1–5).

use std::collections::HashMap;
use std::sync::OnceLock;

/// The catalog: per category a `@<title>` line, then per reaction its id, code points, `d` when it takes a skin tone,
/// its picture's offset and length, and its description, tab-separated.
pub const CATALOG: &str = include_str!("../assets/reactions/catalog.tsv");
/// The skin tones a key may carry (`-tone1` … `-tone5`).
pub const TONES: u8 = 5;
/// Teams' own quick reactions, in the order its message hover bar shows them.
pub const DEFAULT_QUICK_REACTIONS: [&str; 6] = ["like", "heart", "laugh", "surprised", "sad", "angry"];

/// One line of the catalog that counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line<'a> {
    /// A category starts, with this title.
    Category(&'a str),
    /// A reaction of the last category: its id, code points, skin-tone flag, picture offset, picture length and
    /// description.
    Reaction([&'a str; 6]),
}

/// The lines of a catalog that count: no empty or `#` line, no reaction before the first category, and no reaction
/// line without its six fields.
pub fn lines(text: &str) -> impl Iterator<Item = Line<'_>> {
    let mut in_category = false;
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(move |line| {
            if let Some(title) = line.strip_prefix('@') {
                in_category = true;
                Some(Line::Category(title))
            } else {
                fields(line).filter(|_| in_category).map(Line::Reaction)
            }
        })
}

fn fields(line: &str) -> Option<[&str; 6]> {
    let mut fields = line.split('\t');
    let mut taken = [""; 6];
    for slot in &mut taken {
        *slot = fields.next()?;
    }
    Some(taken)
}

/// Each reaction's id in the catalog, and whether it takes a skin tone.
fn ids() -> &'static HashMap<&'static str, bool> {
    static IDS: OnceLock<HashMap<&'static str, bool>> = OnceLock::new();
    IDS.get_or_init(|| {
        lines(CATALOG)
            .filter_map(|line| match line {
                Line::Reaction([id, _, diverse, ..]) => Some((id, diverse == "d")),
                Line::Category(_) => None,
            })
            .collect()
    })
}

/// The reaction a key names, by its id in the catalog, and the skin tone it carries (0 for none).
#[must_use]
pub fn reaction(key: &str) -> Option<(&'static str, u8)> {
    if let Some((id, _)) = ids().get_key_value(key) {
        return Some((id, 0));
    }
    let (id, tone) = key.rsplit_once("-tone")?;
    let tone = tone.parse::<u8>().ok().filter(|tone| (1..=TONES).contains(tone))?;
    let (id, diverse) = ids().get_key_value(id)?;
    diverse.then_some((id, tone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_names_its_reaction_and_its_skin_tone() {
        assert_eq!(reaction("like"), Some(("like", 0)));
        assert_eq!(reaction("yes-tone2"), Some(("yes", 2)));
        assert_eq!(reaction("yes-tone6"), None);
        assert_eq!(reaction("yes-tonex"), None);
        assert_eq!(
            reaction("heart-tone1"),
            None,
            "a reaction without skin tones takes none"
        );
        assert_eq!(reaction("nothing-tone1"), None);
        assert_eq!(reaction("nothing"), None);
        for quick in DEFAULT_QUICK_REACTIONS {
            assert_eq!(reaction(quick), Some((quick, 0)));
        }
    }

    #[test]
    fn only_a_reaction_line_with_six_fields_under_a_category_counts() {
        let text = "# a comment\nearly\tb\t\t1\t2\tdesc\n\n@Smilies\na\tb\tc\nyes\tb\td\t1\t2\tdesc\n";
        assert_eq!(
            lines(text).collect::<Vec<_>>(),
            [
                Line::Category("Smilies"),
                Line::Reaction(["yes", "b", "d", "1", "2", "desc"])
            ]
        );
    }
}
