//! The app's theme: the UI kit's (`gpui-component`), in compact sizes, from a catalog of built-in themes ([`THEMES`]:
//! Teams' light, dark and high contrast, and a few well-known palettes), following the window's appearance or not as
//! the user chose, in the font and size the user chose, switching live when any of it changes.
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

use std::ops::RangeInclusive;
use std::rc::Rc;

use gpui::{App, Entity, Global, SharedString, Subscription, Window, WindowAppearance};
use gpui_component::theme::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode};

use crate::settings::SettingsModel;

/// The light theme's name.
pub const LIGHT: &str = "Teams Light";
/// The dark theme's name.
pub const DARK: &str = "Teams Dark";

/// The base font size, px, by default: compact, as Teams' own (the kit's default is 16). It reaches a window as its
/// rem size only through the kit's root ([`crate::window::KitRoot`]), which sets it each time the window draws; the
/// other values here (radius, colours, shadow) the kit's widgets read from the theme as they draw.
pub const FONT_SIZE: u16 = 14;
/// The base font sizes the user can choose, px.
pub const FONT_SIZES: RangeInclusive<u16> = 10..=24;
/// The kit's name for the system's UI font, used when the user named none.
pub const SYSTEM_FONT: &str = ".SystemUIFont";
/// The corner radius of general elements, px.
pub const RADIUS: usize = 4;
/// The corner radius of large elements (dialogs, popovers), px.
pub const RADIUS_LG: usize = 6;

/// One mode's colours, 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The app's background.
    pub background: u32,
    /// Text.
    pub foreground: u32,
    /// Secondary text (times, previews).
    pub muted_foreground: u32,
    /// Raised surfaces: the list's rows, inputs, popovers.
    pub surface: u32,
    /// Borders and separators.
    pub border: u32,
    /// The brand colour: the selected row's mark, buttons, focus.
    pub brand: u32,
    /// The brand colour under the pointer.
    pub brand_hover: u32,
    /// The brand colour pressed.
    pub brand_active: u32,
    /// Text on the brand colour.
    pub on_brand: u32,
    /// A row under the pointer.
    pub hover: u32,
    /// The selected row.
    pub selected: u32,
    /// Selected text.
    pub selection: u32,
    /// The kit's sidebar and title bar, and a call tile's backdrop (the app's own rail and title bar are drawn on the
    /// frame).
    pub rail: u32,
    /// A mention of the account itself, and the mark beside a message that has one: Teams' orange.
    pub mention: u32,
    /// The window's frame (the title bar and the rail, one surface the panes float on): a gradient from the top left
    /// to the bottom right. A light theme's is vivid; a dark theme's sits darker than its panes, its own hue only a
    /// tint, so the sheet reads as raised and the frame doesn't outshine what is on it.
    pub frame_start: u32,
    pub frame_end: u32,
    /// Icons and text on the frame.
    pub on_frame: u32,
    /// The brand gradient's second stop: the brand colour runs into it on the marks that carry it (the chosen row,
    /// unread counts, the account's own messages, the send button).
    pub accent: u32,
    /// Whether the panes float on the frame as a rounded, shadowed sheet; off (High Contrast) everything stays flat
    /// and square, edges drawn as borders.
    pub depth: bool,
}

/// The light palette.
pub const LIGHT_PALETTE: Palette = Palette {
    background: 0x00f3_f4fa,
    foreground: 0x0024_2424,
    muted_foreground: 0x0061_6161,
    surface: 0x00ff_ffff,
    border: 0x00e0_e0e0,
    brand: 0x005b_5fc7,
    brand_hover: 0x004f_52b2,
    brand_active: 0x0044_4791,
    on_brand: 0x00ff_ffff,
    hover: 0x00f0_f0f0,
    selected: 0x00e8_ebfa,
    selection: 0x00c5_cbfa,
    rail: 0x00eb_ebeb,
    mention: 0x00c4_3501,
    frame_start: 0x003b_37b3,
    frame_end: 0x0016_77cf,
    on_frame: 0x00ff_ffff,
    accent: 0x002b_88e8,
    depth: true,
};

/// The dark palette.
pub const DARK_PALETTE: Palette = Palette {
    background: 0x001f_1f1f,
    foreground: 0x00ff_ffff,
    muted_foreground: 0x00ad_adad,
    surface: 0x0029_2929,
    border: 0x003d_3d3d,
    brand: 0x007f_85f5,
    brand_hover: 0x0092_99f7,
    brand_active: 0x00a3_a8f8,
    on_brand: 0x0014_1414,
    hover: 0x0033_3333,
    selected: 0x002f_2f4a,
    selection: 0x0044_4791,
    rail: 0x0014_1414,
    mention: 0x00e9_835e,
    frame_start: 0x001d_1b36,
    frame_end: 0x0011_1a2a,
    on_frame: 0x00ff_ffff,
    accent: 0x005c_b2f7,
    depth: true,
};

/// The high contrast palette (Teams' own: white on black, cyan and yellow marks).
pub const HIGH_CONTRAST_PALETTE: Palette = Palette {
    background: 0x0000_0000,
    foreground: 0x00ff_ffff,
    muted_foreground: 0x00e6_e6e6,
    surface: 0x0000_0000,
    border: 0x00ff_ffff,
    brand: 0x001a_ebff,
    brand_hover: 0x00ff_ff00,
    brand_active: 0x00ff_ff00,
    on_brand: 0x0000_0000,
    hover: 0x0026_2626,
    selected: 0x0000_3a40,
    selection: 0x0047_4700,
    rail: 0x0000_0000,
    mention: 0x00ff_ff00,
    frame_start: 0x0000_0000,
    frame_end: 0x0000_0000,
    on_frame: 0x00ff_ffff,
    accent: 0x001a_ebff,
    depth: false,
};

/// Solarized's light palette.
pub const SOLARIZED_LIGHT_PALETTE: Palette = Palette {
    background: 0x00ee_e8d5,
    foreground: 0x0058_6e75,
    muted_foreground: 0x0065_7b83,
    surface: 0x00fd_f6e3,
    border: 0x00dc_d4bc,
    brand: 0x0026_8bd2,
    brand_hover: 0x0020_79b8,
    brand_active: 0x001b_6aa1,
    on_brand: 0x00fd_f6e3,
    hover: 0x00f5_efdc,
    selected: 0x00de_e6dc,
    selection: 0x00c9_dbe6,
    rail: 0x00e4_dcc5,
    mention: 0x00cb_4b16,
    frame_start: 0x0007_3642,
    frame_end: 0x000b_5d6b,
    on_frame: 0x00fd_f6e3,
    accent: 0x002a_a198,
    depth: true,
};

/// Solarized's dark palette.
pub const SOLARIZED_DARK_PALETTE: Palette = Palette {
    background: 0x0000_2b36,
    foreground: 0x0093_a1a1,
    muted_foreground: 0x0065_7b83,
    surface: 0x0007_3642,
    border: 0x000d_4654,
    brand: 0x0026_8bd2,
    brand_hover: 0x003a_9ad9,
    brand_active: 0x004f_a6e0,
    on_brand: 0x00fd_f6e3,
    hover: 0x000a_404e,
    selected: 0x000c_4a5c,
    selection: 0x001b_5a6e,
    rail: 0x0000_212b,
    mention: 0x00cb_4b16,
    frame_start: 0x0000_1a22,
    frame_end: 0x0003_222c,
    on_frame: 0x00ee_e8d5,
    accent: 0x002a_a198,
    depth: true,
};

/// Nord's palette.
pub const NORD_PALETTE: Palette = Palette {
    background: 0x002e_3440,
    foreground: 0x00ec_eff4,
    muted_foreground: 0x00a3_acbe,
    surface: 0x003b_4252,
    border: 0x0043_4c5e,
    brand: 0x0088_c0d0,
    brand_hover: 0x009d_cbd9,
    brand_active: 0x00af_d5e0,
    on_brand: 0x002e_3440,
    hover: 0x0043_4c5e,
    selected: 0x004c_566a,
    selection: 0x005e_81ac,
    rail: 0x0027_2c36,
    mention: 0x00d0_8770,
    frame_start: 0x0023_2833,
    frame_end: 0x0021_2d3c,
    on_frame: 0x00ec_eff4,
    accent: 0x00b4_8ead,
    depth: true,
};

/// Dracula's palette.
pub const DRACULA_PALETTE: Palette = Palette {
    background: 0x0028_2a36,
    foreground: 0x00f8_f8f2,
    muted_foreground: 0x009e_a8c7,
    surface: 0x0034_3746,
    border: 0x0044_475a,
    brand: 0x00bd_93f9,
    brand_hover: 0x00c9_a6fa,
    brand_active: 0x00d4_b8fb,
    on_brand: 0x0028_2a36,
    hover: 0x003a_3d4e,
    selected: 0x0044_475a,
    selection: 0x005a_5e7a,
    rail: 0x0021_222c,
    mention: 0x00ff_b86c,
    frame_start: 0x001c_1d27,
    frame_end: 0x0028_1d37,
    on_frame: 0x00f8_f8f2,
    accent: 0x00ff_79c6,
    depth: true,
};

/// A theme of the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeSpec {
    /// What the settings file calls it.
    pub key: &'static str,
    /// What the settings view (and the kit) call it.
    pub name: &'static str,
    /// Whether it is light or dark: the kit's defaults for what the palette doesn't name follow it.
    pub dark: bool,
    pub palette: Palette,
}

impl ThemeSpec {
    /// The kit's mode for it.
    #[must_use]
    pub fn mode(&self) -> ThemeMode {
        if self.dark { ThemeMode::Dark } else { ThemeMode::Light }
    }
}

/// The light theme's key, the light side's default.
pub const LIGHT_KEY: &str = "teams-light";
/// The dark theme's key, the dark side's default.
pub const DARK_KEY: &str = "teams-dark";

/// The built-in themes, in the order the settings view lists them.
pub const THEMES: [ThemeSpec; 7] = [
    ThemeSpec {
        key: LIGHT_KEY,
        name: LIGHT,
        dark: false,
        palette: LIGHT_PALETTE,
    },
    ThemeSpec {
        key: DARK_KEY,
        name: DARK,
        dark: true,
        palette: DARK_PALETTE,
    },
    ThemeSpec {
        key: "high-contrast",
        name: "High Contrast",
        dark: true,
        palette: HIGH_CONTRAST_PALETTE,
    },
    ThemeSpec {
        key: "solarized-light",
        name: "Solarized Light",
        dark: false,
        palette: SOLARIZED_LIGHT_PALETTE,
    },
    ThemeSpec {
        key: "solarized-dark",
        name: "Solarized Dark",
        dark: true,
        palette: SOLARIZED_DARK_PALETTE,
    },
    ThemeSpec {
        key: "nord",
        name: "Nord",
        dark: true,
        palette: NORD_PALETTE,
    },
    ThemeSpec {
        key: "dracula",
        name: "Dracula",
        dark: true,
        palette: DRACULA_PALETTE,
    },
];

/// The catalog's theme `key`, as the settings file names it.
#[must_use]
pub fn find(key: &str) -> Option<&'static ThemeSpec> {
    THEMES.iter().find(|spec| spec.key == key)
}

/// The theme of a key the settings hold, the default of its side when no theme has it (the settings take only keys of
/// the catalog, so this is a fallback that doesn't happen).
fn spec_or(key: &str, dark: bool) -> &'static ThemeSpec {
    find(key).unwrap_or(if dark { &THEMES[1] } else { &THEMES[0] })
}

/// How the app looks, as the user chose it (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    /// The theme follows the system's light or dark mode: [`Appearance::light_theme`] or [`Appearance::dark_theme`].
    /// Off: [`Appearance::theme`], whatever the system says.
    pub follow_system: bool,
    /// The theme's key when the system isn't followed.
    pub theme: &'static str,
    /// The theme's key while the system is light (and followed).
    pub light_theme: &'static str,
    /// The theme's key while the system is dark (and followed).
    pub dark_theme: &'static str,
    /// The base font size, px, within [`FONT_SIZES`].
    pub font_size: u16,
    /// The font family; `None`: the system's UI font.
    pub font_family: Option<String>,
    /// Whether views move as they change (a chat fading in, a new message rising, a chosen row's mark growing);
    /// off, every change shows at once.
    pub animations: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            follow_system: true,
            theme: LIGHT_KEY,
            light_theme: LIGHT_KEY,
            dark_theme: DARK_KEY,
            font_size: FONT_SIZE,
            font_family: None,
            animations: true,
        }
    }
}

impl Appearance {
    /// The theme shown while the system's appearance is `system`.
    #[must_use]
    pub fn active(&self, system: WindowAppearance) -> &'static ThemeSpec {
        if !self.follow_system {
            return spec_or(self.theme, false);
        }
        if is_dark(system) {
            spec_or(self.dark_theme, true)
        } else {
            spec_or(self.light_theme, false)
        }
    }

    /// The base font size one step larger (`step` 1) or smaller (-1), kept within [`FONT_SIZES`].
    #[must_use]
    pub fn stepped_font_size(&self, step: i32) -> u16 {
        let size = i32::from(self.font_size).saturating_add(step);
        let (low, high) = (i32::from(*FONT_SIZES.start()), i32::from(*FONT_SIZES.end()));
        u16::try_from(size.clamp(low, high)).unwrap_or(FONT_SIZE)
    }
}

/// The frame's gradient angle, degrees, as CSS measures it (0 points up, clockwise): from the top left to the bottom
/// right.
pub const FRAME_ANGLE: f32 = 120.0;
/// The gap between the sheet the panes float on and the window's right and bottom edges, px.
pub const SHEET_GAP: f32 = 8.0;
/// The sheet's corner radius, px.
pub const SHEET_RADIUS: f32 = 12.0;

/// `from` mixed toward `to` by `t` (0: `from`, 1: `to`), channel by channel, as the GPU mixes a gradient's stops.
#[must_use]
pub fn mix(from: u32, to: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    [16_u32, 8, 0].iter().fold(0, |mixed, shift| {
        let a = f32::from(u8::try_from((from >> shift) & 0xff).unwrap_or(0));
        let b = f32::from(u8::try_from((to >> shift) & 0xff).unwrap_or(0));
        // within 0..=255 by construction
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a channel within 0..=255"
        )]
        let channel = (a + (b - a) * t).round() as u32;
        mixed | (channel << shift)
    })
}

/// Where along a linear gradient at `angle` (degrees, as [`gpui::linear_gradient`] takes it) over a box of `size` the
/// point `at` (from the box's top left) falls, 0 to 1: the GPU's own sum, so a colour drawn to meet the gradient
/// matches it.
#[must_use]
pub fn gradient_t(angle: f32, size: (f32, f32), at: (f32, f32)) -> f32 {
    let (width, height) = (size.0.max(1.0), size.1.max(1.0));
    let radians = (angle % 360.0 - 90.0).to_radians();
    let (mut x, mut y) = (radians.cos(), radians.sin());
    if width > height {
        y *= height / width;
    } else {
        x *= width / height;
    }
    let length = x.hypot(y).max(f32::EPSILON);
    let along = ((at.0 - width / 2.0) * x + (at.1 - height / 2.0) * y) / length;
    let t = if x.abs() > y.abs() {
        (along + width / 2.0) / width
    } else {
        (along + height / 2.0) / height
    };
    t.clamp(0.0, 1.0)
}

/// The frame's colour at `at` in a window of `size`.
#[must_use]
pub fn frame_at(palette: Palette, size: (f32, f32), at: (f32, f32)) -> u32 {
    mix(
        palette.frame_start,
        palette.frame_end,
        gradient_t(FRAME_ANGLE, size, at),
    )
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

/// The relative luminance of `color` (WCAG's: 0 black, 1 white).
#[must_use]
pub fn luminance(color: u32) -> f32 {
    let channel = |shift: u32| {
        let value = f32::from(u8::try_from((color >> shift) & 0xff).unwrap_or(0)) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// Whether a palette is a dark one: its background darker than the middle grey.
#[must_use]
pub fn palette_is_dark(palette: Palette) -> bool {
    luminance(palette.background) < 0.18
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

/// Whether a window's appearance is a dark one.
fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Teams' red: leaving a call, declining one, an unread count, deleting, and Busy's presence dot.
pub const DANGER: u32 = 0x00c4_314b;
pub const DANGER_HOVER: u32 = 0x00a7_2037;
const DANGER_ACTIVE: u32 = 0x008e_192e;
/// Teams' highlight of the match a search in a transcript shows now (its other matches take the palette's selection),
/// the same in every theme as Teams draws it, with dark text on it.
pub const SEARCH_MATCH: u32 = 0x00ff_b74d;
/// Teams' green: answering a call.
pub const SUCCESS: u32 = 0x0023_7b4b;
pub const SUCCESS_HOVER: u32 = 0x001e_6b41;
const SUCCESS_ACTIVE: u32 = 0x0019_5a37;

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
    ThemeConfig {
        is_default: false,
        name: spec.name.into(),
        mode: spec.mode(),
        font_size: Some(f32::from(appearance.font_size)),
        font_family: Some(SharedString::from(family.to_owned())),
        radius: Some(RADIUS),
        radius_lg: Some(RADIUS_LG),
        shadow: Some(false),
        colors: colors(spec.palette),
        ..ThemeConfig::default()
    }
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
    let appearance = Appearance::default();
    let active = appearance.active(WindowAppearance::Light);
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
    let active = appearance.active(current.system);
    appearance.font_family = installed_family(appearance.font_family, cx);
    let light = if active.dark {
        spec_or(appearance.light_theme, false)
    } else {
        active
    };
    let dark = if active.dark {
        active
    } else {
        spec_or(appearance.dark_theme, true)
    };
    cx.global_mut::<Current>().active = active;
    // the kit's own motion (a menu or dialog coming in, a spinner) and GPUI's animations follow the setting too
    cx.set_reduce_motion(!appearance.animations);
    Theme::global_mut(cx).light_theme = Rc::new(config(light, &appearance));
    Theme::global_mut(cx).dark_theme = Rc::new(config(dark, &appearance));
    Theme::change(active.mode(), None, cx);
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
