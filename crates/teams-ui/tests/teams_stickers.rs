//! Teams' own sticker packs ([`teams_ui::stickers::teams`]): our table of them read whole, their pictures' addresses
//! as Teams' web client builds them (the capture of 2026-11-22 sent the two below), Teams' search, a table that
//! isn't in its form, and Recent keeping Teams' stickers beside the user's.

use teams_proto::{StickerPicture, StickerPreview};
use teams_ui::stickers::StickerRef;
use teams_ui::stickers::store;
use teams_ui::stickers::teams::{self, Catalog, POPULAR};

/// Teams' packs for a work account, in Teams' order, as its "See all" showed them.
const PACKS: [&str; 18] = [
    "Popular",
    "Clippy",
    "Office Drama",
    "Meme",
    "Designers",
    "Devs",
    "Legal",
    "Broccoli",
    "Bee",
    "Teamsquatch",
    "Calamity Bert",
    "Cats In Suits",
    "Word Art",
    "Monkey",
    "Octocorn",
    "Donut Friends",
    "Uncle Blob",
    "Animalopolis",
];

#[test]
fn the_table_gives_teams_packs_in_teams_order_with_every_picture_on_the_cdn() {
    let catalog = teams::catalog();
    let names: Vec<&str> = catalog.packs().iter().map(|pack| pack.name.as_str()).collect();
    assert_eq!(names, PACKS);
    assert!(
        catalog.packs().iter().all(|pack| !pack.id.starts_with("EDU_")),
        "no class packs"
    );
    assert_eq!(catalog.popular().len(), 12);
    assert_eq!(
        catalog.stickers_of(POPULAR).len(),
        12,
        "Popular shows the popular stickers"
    );
    let mut count = 0;
    for pack in catalog.packs() {
        assert!(StickerPreview::of(&pack.header()).is_some(), "{}", pack.header());
        for sticker in catalog.stickers_of(&pack.id) {
            assert!(
                StickerPicture::of(&catalog.picture(sticker)).is_some(),
                "{}",
                catalog.picture(sticker)
            );
            assert!(
                StickerPreview::of(&catalog.preview(sticker)).is_some(),
                "{}",
                catalog.preview(sticker)
            );
            assert!(!sticker.name.is_empty(), "{}", sticker.id);
            count += usize::from(pack.id != POPULAR);
        }
    }
    assert_eq!(count, 406);
}

#[test]
fn a_sticker_s_addresses_are_the_ones_teams_uses() {
    let catalog = teams::catalog();
    let picture = |id: &str| catalog.picture(catalog.sticker(id).unwrap());
    let preview = |id: &str| catalog.preview(catalog.sticker(id).unwrap());
    // as Teams' box sent them: a localized pack's GIF names the locale, a still one's PNG doesn't
    assert_eq!(
        picture("Clippy_HelloThere"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/clippy-250x250/en-us/Clippy_HelloThere.gif?v=5"
    );
    assert_eq!(
        picture("Teamsquatch_BugSquash"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/teamsquatch-250x250/Teamsquatch_BugSquash.png?v=5"
    );
    // a meme is its picture with no caption, at the meme size
    assert_eq!(
        picture("grumpycat"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/meme-500x500/grumpycat-template.png?v=5"
    );
    // as Teams' picker showed them: a preview of its own name, or of the sticker's file
    assert_eq!(
        preview("Broccoli_Wave"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/stickers/en-us/assets/Broccoli/Preview/stickerbroccoliwave.png?v=5"
    );
    assert_eq!(
        preview("Clippy_HelloThere"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/stickers/en-us/assets/Clippy/Preview/Clippy_HelloThere.png?v=5"
    );
    assert_eq!(
        preview("grumpycat"),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/meme/assets/Preview/grumpycat.png?v=5"
    );
    assert_eq!(
        catalog.pack("Meme").unwrap().header(),
        "https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/meme/assets/Preview/Meme_header_large.png?v=5"
    );
    assert_eq!(
        catalog.sticker("Clippy_HelloThere").unwrap().name,
        "Clippy says \"Hello there\""
    );
}

#[test]
fn the_search_finds_what_teams_found() {
    let catalog = teams::catalog();
    let ids = |query: &str| -> Vec<String> {
        catalog
            .search(query)
            .into_iter()
            .map(|sticker| sticker.id.clone())
            .collect()
    };
    // Teams' "hello", in its order: by name or keyword, pack by pack
    let hello = [
        "Clippy_HelloThere",
        "OfficeDrama_Hey",
        "Broccoli_Wave",
        "Bee_Hi",
        "WordArt_Hello",
        "Animalopolis_weird_lemur",
    ];
    assert_eq!(ids("hello"), hello);
    assert_eq!(ids("  HeLLo "), hello, "in any case, trimmed");
    assert!(ids("").is_empty() && ids("   ").is_empty());
    assert!(ids("no sticker has these words").is_empty());
    // a sticker with no keywords isn't found, even by its name
    assert!(!ids("dougnut in shame").contains(&"OfficeDrama_DonutShame".to_owned()));
    assert!(catalog.sticker("OfficeDrama_DonutShame").is_some());
}

#[test]
fn a_table_out_of_its_form_names_its_line() {
    let pack = "pack\tMadeUp\tMade up\tMadeUp_header.png\t-";
    let sticker = "sticker\tMadeUp_A\tMadeUp_A.png\t-\t-\tA\ta, b";
    let good = format!("# made up\n\n{pack}\n{sticker}\npopular\tMadeUp_A\n");
    let catalog = Catalog::parse(&good).unwrap();
    assert_eq!(catalog.packs().len(), 1);
    assert_eq!(catalog.sticker("MadeUp_A").unwrap().keywords, ["a", "b"]);
    assert_eq!(catalog.popular().len(), 1);
    assert_eq!(
        Catalog::parse(&format!("{sticker}\n")).unwrap_err(),
        1,
        "a sticker before any pack"
    );
    assert_eq!(
        Catalog::parse(&format!("{pack}\n{sticker}\n{sticker}\n")).unwrap_err(),
        3,
        "an id twice"
    );
    assert_eq!(
        Catalog::parse(&format!("{pack}\nsticker\tMadeUp_A\n")).unwrap_err(),
        2,
        "a field missing"
    );
    assert_eq!(Catalog::parse("what\n").unwrap_err(), 1);
    assert_eq!(
        Catalog::parse(&format!("{pack}\n{sticker}\npopular\tMadeUp_Gone\n")).unwrap_err(),
        3,
        "a popular sticker that isn't there"
    );
}

#[test]
fn recent_keeps_teams_stickers_beside_the_user_s() {
    let dir = tempfile::tempdir().unwrap();
    let clippy = StickerRef::teams("Clippy_HelloThere");
    assert!(clippy.is_teams());
    assert_eq!(
        store::record_recent(dir.path(), &clippy).unwrap(),
        std::slice::from_ref(&clippy)
    );
    assert_eq!(
        store::recent(dir.path()),
        std::slice::from_ref(&clippy),
        "read back after a restart"
    );
    // a sticker Teams no longer has is dropped as the list is read
    std::fs::write(
        dir.path().join(store::RECENT_FILE),
        "recent = [\"@teams/Clippy_HelloThere\", \"@teams/MadeUp_Gone\", \"@teams/../x\"]\n",
    )
    .unwrap();
    assert_eq!(store::recent(dir.path()), [clippy]);
    assert!(
        !store::is_id(teams_ui::stickers::TEAMS_PACK),
        "no pack the user adds has Teams' id"
    );
}
