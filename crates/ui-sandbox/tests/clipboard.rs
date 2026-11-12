//! The sandbox's clipboard: what `Sandbox::set_clipboard` puts up reaches another data-control client byte for byte
//! and a GPUI window's paste, a newer offer replaces it, and clearing it empties the clipboard. Synthetic content
//! only, on the nested niri's selection; the desktop's clipboard is never read or written.
//!
//! Without weston 15, niri or `dbus-daemon` the test says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#![cfg(target_os = "linux")]

use std::{io::Cursor, path::Path, time::Duration};

use image::{ImageFormat, Rgb, RgbImage};
use ui_sandbox::{
    NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, clipboard::Offers, input::Modifier,
    sandbox_or_skip,
};

const PROBE: &str = env!("CARGO_BIN_EXE_ui-sandbox-probe");
/// From launch to the ready line (a cold dGPU wakes up within it).
const STARTUP: Duration = Duration::from_secs(30);
/// How long a paste gets to come back on the probe's stdout.
const ECHO: Duration = Duration::from_secs(10);
const CLOSE: Duration = Duration::from_secs(5);
const TEXT: &str = "synthetic clipboard text";

/// A small synthetic picture, as PNG.
fn synthetic_png() -> SandboxResult<Vec<u8>> {
    let mut bytes = Cursor::new(Vec::new());
    RgbImage::from_pixel(4, 3, Rgb([0x20, 0xc0, 0x60])).write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

#[test]
fn an_offer_reaches_data_control_and_a_gpui_paste_until_replaced_or_cleared() {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("the sandbox clipboard", &options).unwrap() else {
        return;
    };
    if let Err(error) = check(&sandbox) {
        panic!("{error}\n{}", sandbox.logs());
    }
}

fn check(sandbox: &Sandbox) -> SandboxResult {
    assert_eq!(
        sandbox.read_clipboard()?,
        Offers::new(),
        "a new sandbox's clipboard is empty"
    );
    let png = synthetic_png()?;
    let both: Offers = vec![
        ("image/png".to_owned(), png.clone()),
        ("text/plain;charset=utf-8".to_owned(), TEXT.as_bytes().to_vec()),
    ];
    sandbox.set_clipboard(both.clone())?;
    assert_eq!(sandbox.read_clipboard()?, both);
    // read twice: serving goes on after a paste
    assert_eq!(sandbox.read_clipboard()?, both);

    let mut probe = Running::new(sandbox.spawn(Path::new(PROBE), &[], &[])?)?;
    let pasted = paste_in_probe(sandbox, &probe, png.len());
    pasted.map_err(|error| probe.fail(&error))?;
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = probe.finish(CLOSE)?;
    assert!(status.success(), "exit {status}: {stderr}");

    sandbox.clear_clipboard()?;
    assert_eq!(
        sandbox.read_clipboard()?,
        Offers::new(),
        "cleared, the clipboard is empty"
    );
    Ok(())
}

/// The probe, focused, pastes the text (and the picture, where GPUI reads both) and then, after a newer offer
/// replaced it, the picture alone.
fn paste_in_probe(sandbox: &Sandbox, probe: &Running, png_len: usize) -> SandboxResult {
    // `ready` and the first `active true` come in either order
    let (mut ready, mut active) = (false, false);
    while !(ready && active) {
        let line = probe
            .line(STARTUP)
            .map_err(|error| format!("ready {ready}, focused {active}: {error:?}"))?;
        ready |= line == "ready";
        active |= line == "active true";
    }
    let mut input = sandbox.input()?;
    let (v, _) = input.keymap().key_for('v').ok_or("no v")?;
    let image = format!("image=image/png:{png_len}");

    input.key_with(&[Modifier::Ctrl], v)?;
    let line = probe
        .line_where(ECHO, |line| line.starts_with("clipboard "))
        .map_err(|error| format!("first paste: {error:?}"))?;
    let entries: Vec<&str> = line.trim_start_matches("clipboard ").split(' ').collect();
    assert!(line.contains(&format!("text={TEXT:?}")), "{line}");
    // the vendored GPUI reads every form an offer holds (build/patches/gpui-pre-linux/0003): the picture beside the text
    assert!(entries.contains(&image.as_str()), "{line}");

    let png_only: Offers = vec![("image/png".to_owned(), synthetic_png()?)];
    sandbox.set_clipboard(png_only.clone())?;
    assert_eq!(sandbox.read_clipboard()?, png_only, "the newer offer replaced the pair");
    input.key_with(&[Modifier::Ctrl], v)?;
    let line = probe
        .line_where(ECHO, |line| line.starts_with("clipboard "))
        .map_err(|error| format!("second paste: {error:?}"))?;
    assert_eq!(line, format!("clipboard {image}"));
    Ok(())
}
