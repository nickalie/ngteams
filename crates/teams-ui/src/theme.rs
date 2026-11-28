//! The app's theme: the UI kit's (`gpui-component`), in compact sizes, from the catalog of built-in themes of
//! `teams-theme` ([`THEMES`]: Teams' light, dark and high contrast, and a few well-known palettes), following the
//! window's appearance or not as the user chose, in the font and size the user chose, switching live when any of it
//! changes.
//!
//! - **The choice** ([`Appearance`], kept in the settings): following the system's light or dark mode, a theme for
//!   each (`light_theme`, `dark_theme`), or one theme whatever the system says (`theme`); the base font size within
//!   [`FONT_SIZES`] (every text and every rem-sized gap scales with it, as Teams' zoom does) and the font family,
//!   the system's UI font when none is named.
//! - **Applying it**: [`init`] sets the kit up once per app with the defaults; [`follow`] keeps the kit in step with a
//!   window's appearance (the platform tells a window when the desktop's colour scheme changes: on Linux through the
//!   settings portal on the bus the app is given, which in the UI sandbox has none, so the window stays light), and
//!   [`follow_settings`] with the settings' [`Appearance`]. Nothing here reads or watches a theme file: the kit's
//!   theme directory watcher (`ThemeRegistry::watch_dir`) is never called.

use std::rc::Rc;

use gpui::{App, Entity, Global, SharedString, Subscription, Window, WindowAppearance};
use gpui_component::theme::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode};
pub use teams_theme::{
    Appearance, DANGER, DANGER_ACTIVE, DANGER_HOVER, DARK, DARK_KEY, DARK_PALETTE, DRACULA_PALETTE, FONT_SIZE,
    FONT_SIZES, FRAME_ANGLE, HIGH_CONTRAST_PALETTE, LIGHT, LIGHT_KEY, LIGHT_PALETTE, NORD_PALETTE, Palette, RADIUS,
    RADIUS_LG, SEARCH_MATCH, SHEET_GAP, SHEET_RADIUS, SOLARIZED_DARK_PALETTE, SOLARIZED_LIGHT_PALETTE, SUCCESS,
    SUCCESS_ACTIVE, SUCCESS_HOVER, SYSTEM_FONT, SystemMode, THEMES, ThemeSpec, WARNING, find, find_or, frame_at,
    gradient_t, luminance, mix, palette_is_dark,
};

use crate::settings::SettingsModel;

/// The kit's mode for `spec`.
fn kit_mode(spec: &ThemeSpec) -> ThemeMode {
    if spec.dark { ThemeMode::Dark } else { ThemeMode::Light }
}

/// The light or dark mode a window's appearance is.
fn system_mode(appearance: WindowAppearance) -> SystemMode {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => SystemMode::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => SystemMode::Light,
    }
}

/// The frame's background: its gradient, or its one colour when it has one.
#[must_use]
pub fn frame(palette: Palette) -> gpui::Background {
    if palette.frame_start == palette.frame_end {
        return gpui::rgb(palette.frame_start).into();
    }
    gpui::linear_gradient(
        FRAME_ANGLE,
        gpui::linear_color_stop(gpui::rgb(palette.frame_start), 0.0),
        gpui::linear_color_stop(gpui::rgb(palette.frame_end), 1.0),
    )
}

/// The brand gradient, from the brand colour at `angle` into the accent.
#[must_use]
pub fn brand_gradient(palette: Palette, angle: f32) -> gpui::Background {
    gradient(palette.brand, palette.accent, angle)
}

/// A two-stop gradient at `angle`.
#[must_use]
pub fn gradient(from: u32, to: u32, angle: f32) -> gpui::Background {
    if from == to {
        return gpui::rgb(from).into();
    }
    gpui::linear_gradient(
        angle,
        gpui::linear_color_stop(gpui::rgb(from), 0.0),
        gpui::linear_color_stop(gpui::rgb(to), 1.0),
    )
}

/// A soft shadow of `color` under an element: the sheet's, a glow's. Nothing in a theme without depth.
#[must_use]
pub fn glow(palette: Palette, color: u32, alpha: f32, blur: f32, y: f32) -> Vec<gpui::BoxShadow> {
    if !palette.depth {
        return Vec::new();
    }
    vec![gpui::BoxShadow {
        color: gpui::Hsla::from(gpui::rgb(color)).alpha(alpha),
        offset: gpui::point(gpui::px(0.0), gpui::px(y)),
        blur_radius: gpui::px(blur),
        spread_radius: gpui::px(0.0),
        inset: false,
    }]
}

/// `0xRRGGBB` as the kit's `#rrggbb`.
fn hex(color: u32) -> gpui::SharedString {
    format!("#{color:06x}").into()
}

/// The kit's colours from a palette; anything not named keeps the kit's own default for the mode.
fn colors(palette: Palette) -> ThemeConfigColors {
    // assigned one by one: the kit keeps some of its fields private, so there is no struct literal
    let mut colors = ThemeConfigColors::default();
    colors.background = Some(hex(palette.background));
    colors.foreground = Some(hex(palette.foreground));
    colors.border = Some(hex(palette.border));
    colors.muted = Some(hex(palette.hover));
    colors.muted_foreground = Some(hex(palette.muted_foreground));
    colors.accent = Some(hex(palette.hover));
    colors.accent_foreground = Some(hex(palette.foreground));
    colors.primary = Some(hex(palette.brand));
    colors.primary_hover = Some(hex(palette.brand_hover));
    colors.primary_active = Some(hex(palette.brand_active));
    colors.primary_foreground = Some(hex(palette.on_brand));
    colors.secondary = Some(hex(palette.surface));
    colors.secondary_hover = Some(hex(palette.hover));
    colors.secondary_active = Some(hex(palette.selected));
    colors.secondary_foreground = Some(hex(palette.foreground));
    colors.input = Some(hex(palette.border));
    colors.list = Some(hex(palette.surface));
    colors.list_hover = Some(hex(palette.hover));
    colors.list_active = Some(hex(palette.selected));
    colors.list_active_border = Some(hex(palette.brand));
    colors.popover = Some(hex(palette.surface));
    colors.popover_foreground = Some(hex(palette.foreground));
    colors.selection = Some(hex(palette.selection));
    colors.caret = Some(hex(palette.foreground));
    colors.ring = Some(hex(palette.brand));
    colors.sidebar = Some(hex(palette.rail));
    colors.sidebar_foreground = Some(hex(palette.foreground));
    colors.sidebar_border = Some(hex(palette.border));
    colors.title_bar = Some(hex(palette.rail));
    colors.title_bar_border = Some(hex(palette.border));
    // Teams' own red and green whatever the theme: leaving or declining a call, answering one
    colors.danger = Some(hex(DANGER));
    colors.danger_hover = Some(hex(DANGER_HOVER));
    colors.danger_active = Some(hex(DANGER_ACTIVE));
    colors.danger_foreground = Some(hex(0x00ff_ffff));
    colors.success = Some(hex(SUCCESS));
    colors.success_hover = Some(hex(SUCCESS_HOVER));
    colors.success_active = Some(hex(SUCCESS_ACTIVE));
    colors.success_foreground = Some(hex(0x00ff_ffff));
    colors
}

/// The kit's theme for `spec`, in the size and font of `appearance`.
#[must_use]
pub fn config(spec: &ThemeSpec, appearance: &Appearance) -> ThemeConfig {
    let family = appearance.font_family.as_deref().unwrap_or(SYSTEM_FONT);
    // assigned one by one: the kit's config is non-exhaustive, so there is no struct literal
    let mut config = ThemeConfig::default();
    config.is_default = false;
    config.name = spec.name.into();
    config.mode = kit_mode(spec);
    config.font_size = Some(f32::from(appearance.font_size));
    config.font_family = Some(SharedString::from(family.to_owned()));
    config.radius = Some(RADIUS);
    config.radius_lg = Some(RADIUS_LG);
    config.shadow = Some(false);
    config.colors = colors(spec.palette);
    config
}

/// What the kit shows now: the user's choice, the system's appearance it was last told, and the theme they give.
struct Current {
    appearance: Appearance,
    system: WindowAppearance,
    active: &'static ThemeSpec,
}

impl Global for Current {}

/// Sets the kit up for the app (its globals, key bindings and default theme), then installs our themes as the default
/// [`Appearance`] picks them, light first. Before any window opens; a later call does nothing.
pub fn init(cx: &mut App) {
    if cx.has_global::<Current>() {
        return;
    }
    gpui_component::init(cx);
    crate::keyboard::submenu_escape(cx);
    crate::keyboard::row::bind(cx);
    let appearance = Appearance::default();
    let active = appearance.active(SystemMode::Light);
    cx.set_global(Current {
        appearance,
        system: WindowAppearance::Light,
        active,
    });
    refresh(cx);
}

/// Tells the kit the system's appearance is `appearance` and redraws every window (in another theme while the
/// system is followed).
pub fn apply(appearance: WindowAppearance, cx: &mut App) {
    init(cx);
    cx.global_mut::<Current>().system = appearance;
    refresh(cx);
}

/// Shows `appearance` (the user's choice) from now on and redraws every window; the same choice again does nothing.
pub fn set_appearance(appearance: &Appearance, cx: &mut App) {
    init(cx);
    if cx.global::<Current>().appearance == *appearance {
        return;
    }
    cx.global_mut::<Current>().appearance = appearance.clone();
    refresh(cx);
}

/// Installs the theme the choice and the system's appearance give as the kit's theme of its mode (the other mode's
/// the choice's for that side), switches the kit to that mode and redraws every window.
fn refresh(cx: &mut App) {
    let current = cx.global::<Current>();
    let mut appearance = current.appearance.clone();
    let active = appearance.active(system_mode(current.system));
    appearance.font_family = installed_family(appearance.font_family, cx);
    let light = if active.dark {
        find_or(appearance.light_theme, false)
    } else {
        active
    };
    let dark = if active.dark {
        active
    } else {
        find_or(appearance.dark_theme, true)
    };
    cx.global_mut::<Current>().active = active;
    // the kit's own motion (a menu or dialog coming in, a spinner) and GPUI's animations follow the setting too
    cx.set_reduce_motion(!appearance.animations);
    Theme::global_mut(cx).light_theme = Rc::new(config(light, &appearance));
    Theme::global_mut(cx).dark_theme = Rc::new(config(dark, &appearance));
    Theme::change(kit_mode(active), None, cx);
}

/// `family` when the system has it, else `None` (the system's UI font). GPUI resolves a family it can't find through
/// its own fallback list at the regular weight and upright, which would draw every bold and italic text regular; the
/// kit names the family the system's UI font lands on, which keeps both. A text system that lists no family at all
/// (the test platform's) takes the name as it is.
fn installed_family(family: Option<String>, cx: &App) -> Option<String> {
    let name = family?;
    let installed = cx.text_system().all_font_names();
    (installed.is_empty() || installed.contains(&name)).then_some(name)
}

/// Applies `window`'s appearance now and again each time the platform says it changed, for as long as the returned
/// subscription is kept.
pub fn follow(window: &mut Window, cx: &mut App) -> Subscription {
    apply(window.appearance(), cx);
    window.observe_window_appearance(|window, cx| apply(window.appearance(), cx))
}

/// Applies the settings' [`Appearance`] now and again each time the settings change, for as long as the returned
/// subscription is kept.
pub fn follow_settings(settings: &Entity<SettingsModel>, cx: &mut App) -> Subscription {
    let appearance = settings.read(cx).settings().appearance.clone();
    set_appearance(&appearance, cx);
    cx.observe(settings, |settings, cx| {
        let appearance = settings.read(cx).settings().appearance.clone();
        set_appearance(&appearance, cx);
    })
}

/// The mode the kit is in: light until [`init`] has run.
#[must_use]
pub fn mode(cx: &App) -> ThemeMode {
    cx.try_global::<Theme>().map_or(ThemeMode::Light, |theme| theme.mode)
}

/// The theme windows are drawn with now: the light one until [`init`] has run.
#[must_use]
pub fn active(cx: &App) -> &'static ThemeSpec {
    cx.try_global::<Current>().map_or(&THEMES[0], |current| current.active)
}

/// Whether views move as they change ([`Appearance::animations`]): on until [`init`] has run.
#[must_use]
pub fn animations(cx: &App) -> bool {
    cx.try_global::<Current>()
        .is_none_or(|current| current.appearance.animations)
}

/// The palette windows are drawn with now.
#[must_use]
pub fn current(cx: &App) -> Palette {
    active(cx).palette
}
