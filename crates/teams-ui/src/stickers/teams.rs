//! Teams' own sticker packs, as Teams' web client offers them to a work account (`docs/protocol/chat.md`, "Stickers").
//!
//! No request lists them: the web client has the list in its script. [`catalog`] reads our copy of that data, the
//! table `teams_catalog.txt` beside this file (its header says what it holds and what it leaves out), once.
//!
//! Every picture is a public file on Teams' sticker CDN, read with no credential: a sticker's preview and a pack's
//! header, as the picker shows them ([`teams_proto::StickerPreview`]), and the sticker's own picture, as a message
//! names it ([`teams_proto::StickerPicture`]). A message sends a sticker by that picture's address, as Teams' box
//! sends one: nothing goes to the media service. A sticker that takes a caption, a meme too, goes through "Edit
//! sticker" instead: its caption is drawn on its picture with no words (its template), and that PNG goes to the media
//! service ([`super::caption`]).
//!
//! The search is Teams' own: a sticker is found when its name or one of its keywords holds the query, in any case.

use std::collections::HashMap;
use std::sync::OnceLock;

use teams_proto::{StickerPicture, StickerPreview};

use super::caption::Caption;

/// Our copy of Teams' sticker list.
const TABLE: &str = include_str!("teams_catalog.txt");

/// The pack whose stickers are the [`Catalog::popular`] list, not stickers of its own.
pub const POPULAR: &str = "Popular";

/// The locale the pictures are read in: the one every pack has.
const LOCALE: &str = "en-us";

/// The size a sticker is shown and sent at, in CSS pixels, as Teams' box sends one.
pub const SHOWN: (u32, u32) = (180, 180);

/// One of Teams' packs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamsPack {
    pub id: String,
    pub name: String,
    /// Its header picture's file.
    header: String,
    /// Whether its pictures' path names the locale.
    localized: bool,
    /// Whether it is the Meme pack, whose files have their own paths and size.
    meme: bool,
}

impl TeamsPack {
    /// The address of its header picture, as the picker shows it.
    #[must_use]
    pub fn header(&self) -> String {
        preview_src(&self.id, self.meme, &self.header)
    }
}

/// One of Teams' stickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamsSticker {
    pub id: String,
    /// Its pack's id.
    pub pack: String,
    pub name: String,
    pub keywords: Vec<String>,
    file: String,
    preview: Option<String>,
    /// The words Teams' "Edit sticker" draws on it, when it takes a caption.
    pub caption: Option<Caption>,
}

/// Teams' packs and their stickers, in Teams' order.
#[derive(Debug, Default)]
pub struct Catalog {
    packs: Vec<TeamsPack>,
    stickers: Vec<TeamsSticker>,
    popular: Vec<String>,
    by_id: HashMap<String, usize>,
}

/// The catalog, read from our table the first time it is asked for. A table it can't read gives no packs.
#[must_use]
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        Catalog::parse(TABLE).unwrap_or_else(|line| {
            log::info!("the table of Teams' stickers can't be read at line {line}");
            Catalog::default()
        })
    })
}

impl Catalog {
    /// The catalog `table` holds, in `teams_catalog.txt`'s form.
    ///
    /// # Errors
    /// The number of the first line that isn't in that form: a field missing, a sticker before any pack, an id that is
    /// already there, or a popular sticker that isn't.
    pub fn parse(table: &str) -> Result<Self, usize> {
        let mut catalog = Self::default();
        for (index, line) in table.lines().enumerate() {
            let number = index + 1;
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            match fields[..] {
                ["pack", id, name, header, flags] => catalog.packs.push(TeamsPack {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    header: header.to_owned(),
                    localized: flags.split(',').any(|flag| flag == "localized"),
                    meme: flags.split(',').any(|flag| flag == "meme"),
                }),
                ["popular", ..] => catalog.popular = fields[1..].iter().map(|id| (*id).to_owned()).collect(),
                ["sticker", id, file, preview, name, keywords] => {
                    let pack = catalog.packs.last().ok_or(number)?.id.clone();
                    if catalog.by_id.insert(id.to_owned(), catalog.stickers.len()).is_some() {
                        return Err(number);
                    }
                    catalog.stickers.push(TeamsSticker {
                        id: id.to_owned(),
                        pack,
                        name: name.to_owned(),
                        keywords: given(keywords)
                            .map(|keywords| keywords.split(',').map(|word| word.trim().to_owned()).collect())
                            .unwrap_or_default(),
                        file: file.to_owned(),
                        preview: given(preview).map(str::to_owned),
                        caption: None,
                    });
                }
                ["caption", ref rest @ ..] => {
                    let sticker = catalog.stickers.last_mut().ok_or(number)?;
                    if sticker.caption.is_some() {
                        return Err(number);
                    }
                    sticker.caption = Some(Caption::parse(rest).ok_or(number)?);
                }
                _ => return Err(number),
            }
        }
        if let Some(missing) = catalog.popular.iter().find(|id| !catalog.by_id.contains_key(*id)) {
            let at = table
                .lines()
                .position(|line| line.contains(missing.as_str()))
                .unwrap_or_default();
            return Err(at + 1);
        }
        Ok(catalog)
    }

    /// The packs, in Teams' order.
    #[must_use]
    pub fn packs(&self) -> &[TeamsPack] {
        &self.packs
    }

    /// Pack `id`.
    #[must_use]
    pub fn pack(&self, id: &str) -> Option<&TeamsPack> {
        self.packs.iter().find(|pack| pack.id == id)
    }

    /// Sticker `id`.
    #[must_use]
    pub fn sticker(&self, id: &str) -> Option<&TeamsSticker> {
        self.by_id.get(id).map(|at| &self.stickers[*at])
    }

    /// The stickers pack `id` shows: the popular ones for [`POPULAR`], else its own, in Teams' order.
    #[must_use]
    pub fn stickers_of(&self, id: &str) -> Vec<&TeamsSticker> {
        if id == POPULAR {
            return self.popular();
        }
        self.stickers.iter().filter(|sticker| sticker.pack == id).collect()
    }

    /// Teams' popular stickers, in order.
    #[must_use]
    pub fn popular(&self) -> Vec<&TeamsSticker> {
        self.popular.iter().filter_map(|id| self.sticker(id)).collect()
    }

    /// The stickers whose name or one of whose keywords holds `query` (trimmed, in any case), in the packs' order; a
    /// sticker with no keywords is never found, as in Teams.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&TeamsSticker> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.packs
            .iter()
            .flat_map(|pack| self.stickers.iter().filter(move |sticker| sticker.pack == pack.id))
            .filter(|sticker| {
                !sticker.keywords.is_empty()
                    && (sticker.name.to_lowercase().contains(&query)
                        || sticker.keywords.iter().any(|word| word.to_lowercase().contains(&query)))
            })
            .collect()
    }

    /// Whether pack `id` has stickers that take a caption, which Teams marks with a pencil (Popular, which only
    /// shows other packs' stickers, is not marked).
    #[must_use]
    pub fn editable(&self, id: &str) -> bool {
        id != POPULAR
            && self
                .stickers
                .iter()
                .any(|sticker| sticker.pack == id && sticker.caption.is_some())
    }

    /// The pack sticker `sticker` is of.
    fn pack_of(&self, sticker: &TeamsSticker) -> Option<&TeamsPack> {
        self.pack(&sticker.pack)
    }

    /// The address of `sticker`'s preview, as the picker shows it.
    #[must_use]
    pub fn preview(&self, sticker: &TeamsSticker) -> String {
        let meme = self.pack_of(sticker).is_some_and(|pack| pack.meme);
        preview_src(&sticker.pack, meme, sticker.preview.as_deref().unwrap_or(&sticker.file))
    }

    /// The address of `sticker`'s picture with no words, which its caption is drawn on; `None` when it takes none.
    #[must_use]
    pub fn template(&self, sticker: &TeamsSticker) -> Option<String> {
        let caption = sticker.caption.as_ref()?;
        Some(self.address(sticker, &caption.template))
    }

    /// The address of `sticker`'s picture, as a message names it: `…/stickerassets/<pack>-<size>[/<locale>]/<file>?v=5`.
    /// Only a sticker with no caption is sent by it; a meme has no such picture, only its [`Self::template`].
    #[must_use]
    pub fn picture(&self, sticker: &TeamsSticker) -> String {
        self.address(sticker, &sticker.file)
    }

    /// The address of `file` in `sticker`'s pack folder.
    fn address(&self, sticker: &TeamsSticker, file: &str) -> String {
        let (localized, meme) = self
            .pack_of(sticker)
            .map_or((false, false), |pack| (pack.localized, pack.meme));
        let size = if meme { "500x500" } else { "250x250" };
        let folder = format!("{}-{size}", sticker.pack.to_lowercase());
        StickerPicture::src(&folder, localized.then_some(LOCALE), file)
    }
}

/// `field`, or `None` for `-`.
fn given(field: &str) -> Option<&str> {
    (field != "-" && !field.is_empty()).then_some(field)
}

/// The address of a preview `file` of pack `pack`: `…/funstuff/stickers/<locale>/assets/<pack>/Preview/<file>?v=5`,
/// or `…/funstuff/meme/assets/Preview/<file>?v=5` for the Meme pack.
fn preview_src(pack: &str, meme: bool, file: &str) -> String {
    if meme {
        StickerPreview::meme_src(file)
    } else {
        StickerPreview::src(LOCALE, pack, file)
    }
}
