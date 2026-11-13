//! The models each tool reports, read from synthetic catalogs and fake tools (no tool here asks a model anything).

use crate::fake_tool;

use std::fs;

use gpui::TestAppContext;

use super::*;

/// A synthetic catalog in the shape Claude Code caches its own: two families, a model of each, and an older one.
const CLAUDE_CATALOG: &str = r#"{"version":2,"fetchedAt":200,"catalog":{"surface":"cc","config":{"id":"cc",
  "models":[
    {"id":"claude-alpha-9","name":"Alpha 9","short_name":"Alpha","section":"main","runtime":{"family":"alpha"}},
    {"id":"claude-beta-9","name":"Beta 9","short_name":"Beta","section":"main","runtime":{"family":"beta"}},
    {"id":"claude-alpha-8","name":"Alpha 8","section":"overflow","runtime":{"family":"alpha"}},
    {"id":"claude-gamma-1","section":"main"}
  ],
  "provider_alias_targets":{"alpha":{"default":"claude-alpha-9"},"beta":{"default":"claude-beta-9"},
    "delta":{"default":"claude-delta-1"}}}}}"#;

/// A synthetic Codex catalog: one model hidden from its picker, listed out of priority order.
const CODEX_CATALOG: &str = r#"{"models":[
  {"slug":"gpt-two","display_name":"GPT-Two","visibility":"list","priority":2},
  {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hide","priority":0},
  {"slug":"gpt-one","display_name":"GPT-One","visibility":"list","priority":1},
  {"slug":"gpt-plain"}
]}"#;

/// Claude Code's help for `--model`, in the shape it words it.
const CLAUDE_HELP: &str = "Options:\n  --model <model>     Model for the current session. Provide\n                      an alias for the latest model (e.g.\n                      'fable', 'opus', or 'sonnet') or a\n                      model's full name.\n  -n, --name <name>   A 'name' not a model\n";

fn listed(id: &str, name: &str, latest: bool) -> Listed {
    Listed {
        id: id.to_owned(),
        name: name.to_owned(),
        latest,
    }
}

/// A script `name` in `dir`, put there by a child: written by this process, it couldn't be run (`ETXTBSY`) while
/// another test's child, forked before the write's descriptor closed, hadn't reached its own `exec`.
fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    fake_tool::write(dir, name, body)
}

#[test]
fn claude_s_catalog_lists_its_aliases_then_its_models_newest_first() {
    assert_eq!(
        claude_catalog(CLAUDE_CATALOG),
        vec![
            listed("alpha", "Alpha", true),
            listed("beta", "Beta", true),
            listed("delta", "Delta", true),
            listed("claude-alpha-9", "Alpha 9", false),
            listed("claude-beta-9", "Beta 9", false),
            listed("claude-gamma-1", "claude-gamma-1", false),
            listed("claude-alpha-8", "Alpha 8", false),
        ]
    );
    assert_eq!(listed("alpha", "Alpha", true).label(), "Alpha (latest)");
    assert_eq!(listed("claude-alpha-9", "Alpha 9", false).label(), "Alpha 9");
    assert!(claude_catalog("not json").is_empty());
    assert!(claude_catalog("{}").is_empty());
}

#[test]
fn claude_s_help_names_its_aliases() {
    assert_eq!(
        claude_help(CLAUDE_HELP),
        vec![
            listed("fable", "Fable", true),
            listed("opus", "Opus", true),
            listed("sonnet", "Sonnet", true),
        ]
    );
    assert!(claude_help("no such option").is_empty());
}

#[test]
fn codex_s_catalog_lists_what_its_picker_shows_by_priority() {
    assert_eq!(
        codex_catalog(CODEX_CATALOG),
        vec![
            listed("gpt-one", "GPT-One", false),
            listed("gpt-two", "GPT-Two", false),
            listed("gpt-plain", "gpt-plain", false),
        ]
    );
    assert!(codex_catalog("[]").is_empty());
    assert!(codex_catalog("nope").is_empty());
}

#[test]
fn claude_s_newest_cached_catalog_is_read_else_its_help() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "claude",
        &format!("printf '%s' \"{}\"", CLAUDE_HELP.replace('\'', "'\\''")),
    );
    // no catalog: the help's aliases
    let home = dir.path().join("home");
    assert_eq!(list_claude(&program, &home).len(), 3);
    let cache = home.join("cache").join("model-catalog");
    fs::create_dir_all(&cache).unwrap();
    fs::write(
        cache.join("old-cc.json"),
        CLAUDE_CATALOG
            .replace("\"fetchedAt\":200", "\"fetchedAt\":100")
            .replace("Alpha 9", "Stale 9"),
    )
    .unwrap();
    fs::write(cache.join("new-cc.json"), CLAUDE_CATALOG).unwrap();
    fs::write(cache.join("other-surface.json"), "{}").unwrap();
    let models = list_claude(&program, &home);
    assert_eq!(models.len(), 7);
    assert!(
        models.iter().any(|model| model.name == "Alpha 9"),
        "the newest: {models:?}"
    );
}

#[test]
fn codex_lists_its_models_itself_else_from_its_cache() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let catalog = dir.path().join("catalog.json");
    fs::write(&catalog, CODEX_CATALOG).unwrap();
    let program = fake(
        dir.path(),
        "codex",
        &format!("[ \"$1 $2\" = \"debug models\" ] && cat '{}'", catalog.display()),
    );
    assert_eq!(list_codex(&program, &home).len(), 3);
    // the listing fails: the cache
    let failing = fake(dir.path(), "codex-failing", "exit 1");
    assert!(list_codex(&failing, &home).is_empty());
    fs::write(
        home.join("models_cache.json"),
        CODEX_CATALOG.replace("GPT-One", "Cached One"),
    )
    .unwrap();
    assert_eq!(list_codex(&failing, &home)[0].name, "Cached One");
}

#[test]
fn a_listing_that_hangs_is_given_up() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "slow", "echo early; sleep 600");
    let started = Instant::now();
    assert_eq!(run_within(&program, &[], Duration::from_millis(200)), None);
    assert!(started.elapsed() < Duration::from_secs(10));
    let quick = fake(dir.path(), "quick", "echo \"$@\"");
    assert_eq!(run(&quick, &["debug", "models"]).as_deref(), Some("debug models\n"));
}

#[gpui::test]
fn the_catalog_lists_each_installed_tool_and_the_app_s_is_listed_in_the_background(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog.json");
    fs::write(&catalog, CODEX_CATALOG).unwrap();
    let codex = fake(dir.path(), "codex", &format!("cat '{}'", catalog.display()));
    let tools = Tools::with(vec![(Backend::Codex, codex)]);
    let listed = Catalog::list(&tools);
    assert!(listed.is_listed());
    assert_eq!(listed.models(Backend::Codex).len(), 3);
    assert!(listed.models(Backend::Claude).is_empty());

    cx.update(|cx| cx.set_global(tools));
    let first = cx.update(Catalog::of);
    assert!(!first.is_listed(), "being listed");
    cx.run_until_parked();
    assert!(cx.update(Catalog::of).is_listed());
    assert_eq!(cx.update(Catalog::of).models(Backend::Codex).len(), 3);
    let set = Catalog::with(vec![listed_one()], Vec::new());
    cx.update(|cx| cx.set_global(set.clone()));
    assert_eq!(cx.update(Catalog::of), set);
}

#[gpui::test]
fn a_tool_found_on_a_later_look_has_its_models_listed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog.json");
    fs::write(&catalog, CODEX_CATALOG).unwrap();
    let codex = fake(dir.path(), "codex", &format!("cat '{}'", catalog.display()));

    cx.update(|cx| cx.set_global(Tools::default()));
    cx.update(Catalog::of);
    cx.run_until_parked();
    assert!(cx.update(Catalog::of).models(Backend::Codex).is_empty());

    // Try again finds the tool installed since
    let found = Tools::with(vec![(Backend::Codex, codex)]);
    cx.update(|cx| Tools::found_again(found.clone(), cx));
    assert!(!cx.update(Catalog::of).is_listed(), "being listed again");
    cx.run_until_parked();
    assert_eq!(cx.update(Catalog::of).models(Backend::Codex).len(), 3);

    // the same tools found again: the models aren't asked for again
    let set = Catalog::with(Vec::new(), vec![listed("gpt-set", "GPT-Set", false)]);
    cx.update(|cx| cx.set_global(set.clone()));
    cx.update(|cx| Tools::found_again(found, cx));
    cx.run_until_parked();
    assert_eq!(cx.update(Catalog::of), set);
}

#[gpui::test]
fn a_listing_of_tools_since_found_again_is_dropped(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog.json");
    fs::write(&catalog, CODEX_CATALOG).unwrap();
    let codex = fake(dir.path(), "codex", &format!("cat '{}'", catalog.display()));

    cx.update(|cx| cx.set_global(Tools::with(vec![(Backend::Codex, codex)])));
    cx.update(Catalog::of);
    // the tool is gone before its listing comes back
    cx.update(|cx| Tools::found_again(Tools::default(), cx));
    cx.run_until_parked();
    let catalog = cx.update(Catalog::of);
    assert!(catalog.is_listed());
    assert!(catalog.models(Backend::Codex).is_empty());
}

fn listed_one() -> Listed {
    listed("opus", "Opus", true)
}
