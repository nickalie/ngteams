//! QA of task 715 (round 1): the call's chat bubble shows a mention of everyone in the reader's words. b's client (the
//! mock plays it) writes in the call's chat as Teams in Russian does: the mention's text "Все" in a paragraph of its
//! own, its `properties.mentions` entry `mentionType` `everyone`, then the text. The bubble at the stage's top is
//! found as it came (what changed from the calm stage), cut out and read by OCR (`tesseract`, when it is there), in
//! Teams Light and Dark: "Everyone Setup is ready" on one line, never "Все" (which OCR reads as "Bce").

use std::thread;
use std::time::{Duration, Instant};

use image::RgbImage;
use serde_json::{Value, json};
use teams_mock::{ABC_CHAT, LAB_B};
use teams_ui::call_stage::BUBBLE_FOR;
use ui_sandbox::SandboxResult;

use super::pixels::{self, Rect};
use super::qa_order::{qa_call, speak};
use super::{EVIDENCE_ENV, Rig, SETTLE, TOOLBAR_BAND, View, save_shot, say};

const MENTION_TYPE: &str = "http://schema.skype.com/Mention";

/// The step `qa-everyone` (see the module docs).
pub(super) fn qa_everyone(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    let mut failed = Vec::new();
    for (key, theme) in [
        (teams_ui::theme::LIGHT_KEY, "light"),
        (teams_ui::theme::DARK_KEY, "dark"),
    ] {
        rig.theme(key)?;
        if let Err(error) = bubble_reads_everyone(rig, theme) {
            say(&format!("  FAIL qa-everyone {theme}: {error}"));
            failed.push(format!("{theme}: {error}"));
        }
        // the bubble gone before the next message
        thread::sleep(BUBBLE_FOR + SETTLE);
    }
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; ").into())
    }
}

/// b writes; the bubble that comes reads "Everyone Setup is ready, try it".
fn bubble_reads_everyone(rig: &mut Rig<'_>, theme: &str) -> SandboxResult {
    rig.park()?;
    thread::sleep(SETTLE);
    let calm = rig.shot()?;
    post_everyone(rig)?;
    let (bubble, shot) = wait_bubble(rig, &calm)?;
    save_shot(rig, &shot, &format!("qa715-call-bubble-{theme}"))?;
    let cut = pixels::crop(&shot, bubble);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(std::path::PathBuf::from) {
        cut.save(dir.join(format!("qa715-call-bubble-{theme}-cut.png")))?;
    }
    let Some(text) = ocr(&cut, theme == "dark", &rig.sandbox.path("qa715-bubble-ocr.png"))? else {
        say("  no tesseract: the bubble's words are not read");
        return Ok(());
    };
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    say(&format!("  qa-everyone {theme}: the bubble reads {flat:?}"));
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(std::path::PathBuf::from) {
        std::fs::write(dir.join(format!("qa715-call-bubble-{theme}-ocr.txt")), &text)?;
    }
    if !flat.contains("Everyone Setup is ready") || flat.contains("Bce") || flat.contains("Все") {
        return Err(format!("the bubble reads {flat:?}").into());
    }
    say(&format!(
        "  pass qa-everyone {theme}: the bubble reads \"Everyone Setup is ready\""
    ));
    Ok(())
}

/// b's client posts to the call's chat a mention of everyone labelled "Все" in a paragraph of its own, an empty
/// paragraph, then the text, as Teams in Russian writes it.
fn post_everyone(rig: &Rig<'_>) -> SandboxResult {
    let mention = json!([{
        "@type": MENTION_TYPE, "itemid": "0", "mri": ABC_CHAT, "mentionType": "everyone", "displayName": "Все",
    }]);
    let html = format!(
        r#"<p><span itemscope="" itemtype="{MENTION_TYPE}" itemid="0">Все</span></p><p>&nbsp;</p><p>Setup is ready, try it</p>"#
    );
    let body = json!({
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "content": html,
        "clientmessageid": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis())
            .to_string(),
        "imdisplayname": "Lab B",
        "properties": {"mentions": mention.to_string(), "files": "[]"},
    });
    let url = format!(
        "{}/api/chatsvc/amer/v1/users/ME/conversations/{ABC_CHAT}/messages",
        rig.mock.origin()
    );
    let token = rig.mock.token(LAB_B).ok_or("no token for b")?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let status = runtime.block_on(async {
        reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .header("X-MS-Migration", "True")
            .header("BehaviorOverride", "redirectAs404")
            .header("content-type", "application/json")
            .body(Value::to_string(&body))
            .send()
            .await
            .map(|answer| answer.status().as_u16())
    })?;
    if !(200..300).contains(&status) {
        return Err(format!("b's post answered {status}").into());
    }
    Ok(())
}

/// The bubble once it shows: what changed from `calm` in the band under the toolbar, at least 200×40; and its shot.
fn wait_bubble(rig: &mut Rig<'_>, calm: &RgbImage) -> SandboxResult<(Rect, RgbImage)> {
    let window = rig.window()?;
    let band = Rect {
        x: window.x + 4,
        y: window.y + TOOLBAR_BAND,
        w: window.w - 8,
        h: 130,
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        let shot = rig.shot()?;
        let changed = pixels::changes(calm, &shot, band)
            .into_iter()
            .filter(|blob| blob.pixels >= 30)
            .map(|blob| blob.bounds)
            .reduce(Rect::union)
            .filter(|bubble| bubble.w >= 200 && bubble.h >= 40);
        if let Some(bubble) = changed {
            // the text settles a frame after the bubble's box
            thread::sleep(Duration::from_millis(300));
            let shot = rig.shot()?;
            return Ok((bubble, shot));
        }
    }
    rig.save("failed-qa715-no-bubble")?;
    Err("no bubble in 12 s".into())
}

/// The text `tesseract` reads in `cut` (light text on dark inverted, scaled three times); `None` without it.
fn ocr(cut: &RgbImage, dark: bool, input: &std::path::Path) -> SandboxResult<Option<String>> {
    let mut grey = image::DynamicImage::ImageRgb8(cut.clone()).to_luma8();
    if dark {
        image::imageops::invert(&mut grey);
    }
    let scaled = image::imageops::resize(
        &grey,
        grey.width() * 3,
        grey.height() * 3,
        image::imageops::FilterType::CatmullRom,
    );
    scaled.save(input)?;
    let output = std::process::Command::new("tesseract")
        .arg(input)
        .arg("stdout")
        .args(["--psm", "6", "-l", "eng"])
        .output();
    let _ = std::fs::remove_file(input);
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !output.status.success() {
        return Err(format!("tesseract: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}
