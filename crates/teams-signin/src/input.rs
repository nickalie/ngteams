//! GPUI's input in the page engine's terms (`teams_wpe_client::Input`, taken from ngterm's HTML overlay): keys as XKB keysyms, the
//! modifiers and pressed buttons as `WPEModifiers`, the wheel in steps or a touchpad's pixels, and the page's CSS cursor
//! names as GPUI cursors. Pure functions, so the element's input is tested without an engine.

use gpui::{CursorStyle, Keystroke, MouseButton, Pixels, Point, ScrollDelta};
use teams_wpe_client::{Button, Modifiers};

/// GPUI's lines per wheel notch (`SCROLL_LINES` of its Linux platform): a step of the engine's wheel is one notch.
const LINES_PER_STEP: f32 = 3.0;

/// The XKB keysyms of the named keys GPUI reports.
const NAMED: [(&str, u32); 30] = [
    ("backspace", 0xff08),
    ("tab", 0xff09),
    ("enter", 0xff0d),
    ("escape", 0xff1b),
    ("space", 0x0020),
    ("home", 0xff50),
    ("left", 0xff51),
    ("up", 0xff52),
    ("right", 0xff53),
    ("down", 0xff54),
    ("pageup", 0xff55),
    ("pagedown", 0xff56),
    ("end", 0xff57),
    ("insert", 0xff63),
    ("menu", 0xff67),
    ("delete", 0xffff),
    ("shift", 0xffe1),
    ("control", 0xffe3),
    ("capslock", 0xffe5),
    ("alt", 0xffe9),
    ("platform", 0xffeb),
    ("f1", 0xffbe),
    ("f2", 0xffbf),
    ("f3", 0xffc0),
    ("f4", 0xffc1),
    ("f5", 0xffc2),
    ("f6", 0xffc3),
    ("f7", 0xffc4),
    ("f8", 0xffc5),
    ("f9", 0xffc6),
];

/// The keysym of `c` (`xkb_utf32_to_keysym`): Latin-1 characters are their code points, the rest the Unicode range.
#[must_use]
pub fn char_keysym(c: char) -> u32 {
    let code = u32::from(c);
    if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) {
        code
    } else {
        0x0100_0000 | code
    }
}

/// The keysym of GPUI's `keystroke`: its named key, else the character it types (its key's own with a modifier that
/// types none, as Ctrl+V does), else `None`.
#[must_use]
pub fn keysym(keystroke: &Keystroke) -> Option<u32> {
    let key = keystroke.key.as_str();
    if let Some(&(_, keysym)) = NAMED.iter().find(|(name, _)| *name == key) {
        return Some(keysym);
    }
    if let Some(number) = key.strip_prefix('f').and_then(|number| number.parse::<u32>().ok())
        && (10..=24).contains(&number)
    {
        return Some(0xffbe + number - 1);
    }
    let typed = keystroke.key_char.as_deref().filter(|text| text.chars().count() == 1);
    let text = typed.unwrap_or(key);
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(char_keysym(c)),
        _ => None,
    }
}

/// GPUI's keyboard modifiers as the engine's (the platform key as Meta).
#[must_use]
pub fn modifiers(gpui: gpui::Modifiers) -> Modifiers {
    let mut wpe = Modifiers::empty();
    wpe.set(Modifiers::CONTROL, gpui.control);
    wpe.set(Modifiers::SHIFT, gpui.shift);
    wpe.set(Modifiers::ALT, gpui.alt);
    wpe.set(Modifiers::META, gpui.platform);
    wpe
}

/// A GPUI button as the engine's.
#[must_use]
pub fn button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Primary,
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Secondary,
        MouseButton::Navigate(gpui::NavigationDirection::Back) => Button::Back,
        MouseButton::Navigate(gpui::NavigationDirection::Forward) => Button::Forward,
    }
}

/// The pressed-button bit of `button`.
#[must_use]
pub fn button_bit(button: Button) -> Modifiers {
    match button {
        Button::Primary => Modifiers::BUTTON1,
        Button::Middle => Modifiers::BUTTON2,
        Button::Secondary => Modifiers::BUTTON3,
        Button::Back => Modifiers::BUTTON4,
        Button::Forward => Modifiers::BUTTON5,
    }
}

/// A wheel or touchpad delta as the engine's (`delta_x`, `delta_y`, precise): a touchpad's pixels as they are, a
/// wheel's lines in notches; GPUI's signs (negative down) are the engine's.
#[must_use]
pub fn scroll(delta: ScrollDelta) -> (f64, f64, bool) {
    match delta {
        ScrollDelta::Pixels(Point { x, y }) => (f64::from(f32::from(x)), f64::from(f32::from(y)), true),
        ScrollDelta::Lines(Point { x, y }) => (f64::from(x / LINES_PER_STEP), f64::from(y / LINES_PER_STEP), false),
    }
}

/// `position` relative to `origin`, in logical pixels.
#[must_use]
pub fn local(position: Point<Pixels>, origin: Point<Pixels>) -> (f64, f64) {
    (
        f64::from(f32::from(position.x - origin.x)),
        f64::from(f32::from(position.y - origin.y)),
    )
}

/// The GPUI cursor for the page's CSS cursor `name`.
#[must_use]
pub fn cursor(name: &str) -> CursorStyle {
    match name {
        "pointer" => CursorStyle::PointingHand,
        "text" => CursorStyle::IBeam,
        "vertical-text" => CursorStyle::IBeamCursorForVerticalLayout,
        "crosshair" => CursorStyle::Crosshair,
        "grab" => CursorStyle::OpenHand,
        "grabbing" | "move" | "all-scroll" => CursorStyle::ClosedHand,
        "not-allowed" | "no-drop" => CursorStyle::OperationNotAllowed,
        "alias" => CursorStyle::DragLink,
        "copy" => CursorStyle::DragCopy,
        "context-menu" => CursorStyle::ContextualMenu,
        "ew-resize" => CursorStyle::ResizeLeftRight,
        "ns-resize" => CursorStyle::ResizeUpDown,
        "col-resize" => CursorStyle::ResizeColumn,
        "row-resize" => CursorStyle::ResizeRow,
        "e-resize" => CursorStyle::ResizeRight,
        "w-resize" => CursorStyle::ResizeLeft,
        "n-resize" => CursorStyle::ResizeUp,
        "s-resize" => CursorStyle::ResizeDown,
        "nwse-resize" | "nw-resize" | "se-resize" => CursorStyle::ResizeUpLeftDownRight,
        "nesw-resize" | "ne-resize" | "sw-resize" => CursorStyle::ResizeUpRightDownLeft,
        _ => CursorStyle::Arrow,
    }
}

/// Whether `keystroke` pastes in a page (Ctrl+V, Ctrl+Shift+V, Shift+Insert): the desktop's clipboard is handed to
/// the engine before it goes in.
#[must_use]
pub fn pastes(keystroke: &Keystroke) -> bool {
    let mods = keystroke.modifiers;
    let key = keystroke.key.as_str();
    (mods.control && !mods.alt && key == "v") || (mods.shift && !mods.control && key == "insert")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(text: &'static str) -> Keystroke {
        Keystroke::parse(text).unwrap()
    }

    #[test]
    fn keys_map_to_xkb_keysyms() {
        assert_eq!(keysym(&stroke("enter")), Some(0xff0d));
        assert_eq!(keysym(&stroke("backspace")), Some(0xff08));
        assert_eq!(keysym(&stroke("f1")), Some(0xffbe));
        assert_eq!(keysym(&stroke("f12")), Some(0xffc9));
        assert_eq!(keysym(&stroke("f24")), Some(0xffd5));
        assert_eq!(keysym(&stroke("a")), Some(u32::from('a')));
        assert_eq!(
            keysym(&stroke("ctrl-v")),
            Some(u32::from('v')),
            "a modifier that types nothing"
        );
        let mut shifted = stroke("shift-a");
        shifted.key_char = Some("A".to_owned());
        assert_eq!(keysym(&shifted), Some(u32::from('A')), "what the key types");
        let mut cyrillic = stroke("a");
        cyrillic.key_char = Some("ф".to_owned());
        assert_eq!(keysym(&cyrillic), Some(0x0100_0000 | u32::from('ф')));
        assert_eq!(char_keysym('é'), 0xe9, "Latin-1 is its own code point");
    }

    #[test]
    fn modifiers_buttons_and_wheel_follow_the_engines_terms() {
        let mods = stroke("ctrl-shift-alt-super-x").modifiers;
        assert_eq!(
            modifiers(mods),
            Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT | Modifiers::META
        );
        assert_eq!(button(MouseButton::Right), Button::Secondary);
        assert_eq!(button_bit(Button::Primary), Modifiers::BUTTON1);
        let (_, down, precise) = scroll(ScrollDelta::Lines(gpui::point(0., -3.)));
        assert!(
            (down + 1.0).abs() < f64::EPSILON && !precise,
            "a notch down is one step down"
        );
        let (_, pixels, precise) = scroll(ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(12.))));
        assert!((pixels - 12.0).abs() < f64::EPSILON && precise);
    }

    #[test]
    fn page_cursors_and_paste_keys() {
        assert_eq!(cursor("pointer"), CursorStyle::PointingHand);
        assert_eq!(cursor("text"), CursorStyle::IBeam);
        assert_eq!(cursor("default"), CursorStyle::Arrow);
        assert_eq!(cursor("bogus"), CursorStyle::Arrow);
        assert!(pastes(&stroke("ctrl-v")));
        assert!(pastes(&stroke("ctrl-shift-v")));
        assert!(pastes(&stroke("shift-insert")));
        assert!(!pastes(&stroke("v")));
        assert!(!pastes(&stroke("ctrl-alt-v")));
    }
}
