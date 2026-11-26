//! The app's asset source serves every icon the UI kit draws by path, so none of the kit's icons (a button's loading
//! spinner, a submenu's chevron, a date field's calendar) is drawn blank.

// test code throughout (its helpers panic like the tests do)
#![cfg(test)]

use gpui::AssetSource as _;
use gpui_component::{IconName, IconNamed as _};
use teams_ui::AppAssets;
use teams_ui::assets::WITHHELD;

/// Every `IconName` the kit's components draw (`gpui-component` 0.11's sources, its tests' aside).
fn drawn_by_the_kit() -> Vec<IconName> {
    use IconName as I;
    vec![
        I::ArrowDown,
        I::ArrowUp,
        I::Asterisk,
        I::Ban,
        I::BookOpen,
        I::Calendar,
        I::CaseSensitive,
        I::Check,
        I::ChevronDown,
        I::ChevronLeft,
        I::ChevronRight,
        I::ChevronUp,
        I::ChevronsUpDown,
        I::CircleCheck,
        I::CircleX,
        I::Close,
        I::Copy,
        I::Ellipsis,
        I::ExternalLink,
        I::Eye,
        I::EyeOff,
        I::Inbox,
        I::Info,
        I::Inspector,
        I::Loader,
        I::Maximize,
        I::Mic,
        I::Minimize,
        I::Minus,
        I::PanelBottom,
        I::PanelBottomOpen,
        I::PanelLeft,
        I::PanelLeftClose,
        I::PanelLeftOpen,
        I::PanelRight,
        I::PanelRightClose,
        I::PanelRightOpen,
        I::Plus,
        I::RefreshCw,
        I::Replace,
        I::Search,
        I::SortAscending,
        I::SortDescending,
        I::Square,
        I::Star,
        I::StarFill,
        I::TriangleAlert,
        I::Undo2,
        I::User,
        I::WindowClose,
        I::WindowMaximize,
        I::WindowMinimize,
        I::WindowRestore,
    ]
}

fn served(path: &str) -> Vec<u8> {
    match AppAssets.load(path) {
        Ok(Some(bytes)) => bytes.into_owned(),
        other => panic!("{path} isn't served: {other:?}"),
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start();
    (text.starts_with("<svg") || text.starts_with("<?xml")) && text.trim_end().ends_with("</svg>")
}

#[test]
fn every_icon_the_kit_draws_is_served_as_an_svg() {
    for name in drawn_by_the_kit() {
        let path = name.path();
        assert!(is_svg(&served(&path)), "{path}");
    }
}

#[test]
fn every_icon_of_the_kit_s_bundle_is_served_but_the_two_withheld() {
    let bundle = gpui_kit_assets::Assets.list("icons/").expect("the kit's bundle lists");
    assert!(bundle.len() > 100, "the kit's bundle was read ({} icons)", bundle.len());
    for path in &bundle {
        if WITHHELD.contains(&path.as_ref()) {
            assert!(matches!(AppAssets.load(path), Ok(None)), "{path} is withheld");
        } else {
            assert!(is_svg(&served(path)), "{path}");
        }
    }
    let listed = AppAssets.list("icons/").expect("the app's assets list");
    assert_eq!(listed.len(), bundle.len() - WITHHELD.len());
    assert!(listed.iter().all(|path| !WITHHELD.contains(&path.as_ref())));
}

#[test]
fn the_spinner_is_teams_ring_and_the_window_controls_are_lucide_s() {
    let kit = |path: &str| {
        gpui_kit_assets::Assets
            .load(path)
            .ok()
            .flatten()
            .map(std::borrow::Cow::into_owned)
    };
    assert_eq!(Some(served("icons/loader.svg")), kit("icons/loader-circle.svg"));
    assert_ne!(Some(served("icons/loader.svg")), kit("icons/loader.svg"));
    for (asked, lucide) in [
        ("icons/window-close.svg", "icons/close.svg"),
        ("icons/window-minimize.svg", "icons/minus.svg"),
        ("icons/window-maximize.svg", "icons/square.svg"),
        ("icons/window-restore.svg", "icons/copy.svg"),
    ] {
        assert_eq!(Some(served(asked)), kit(lucide), "{asked}");
    }
}

#[test]
fn a_path_outside_the_bundle_is_an_error_and_empty_is_nothing() {
    assert!(AppAssets.load("icons/not-an-icon.svg").is_err());
    assert!(matches!(AppAssets.load(""), Ok(None)));
}
