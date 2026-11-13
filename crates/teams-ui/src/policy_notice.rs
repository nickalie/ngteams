//! What a message a tenant's policy acted on says over its text, in Teams' words ([`PolicyNotice`]): an icon, a line,
//! and under it a link to Microsoft's page about it. The account's own blocked message keeps its text and files under
//! the notice (the engine put them back); anyone else's shows the notice alone.
//!
//! Teams offers its sender an override or a report only with the policy tip its compliance service adds, which the
//! chat service never carries: the sender gets the page about the policy instead, as Teams shows it without a tip.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
pub use teams_proto::chat::PolicyNotice;
use teams_proto::chat::PolicyViolation;

use crate::icons::Glyph;
use crate::theme::{self, Palette};

pub const FILE_TYPE_BLOCKED: &str = "This message was blocked because it contains a file type that is not permitted.";
pub const HARMFUL_CONTENT_BLOCKED: &str = "This message was blocked due to potentially harmful content.";
pub const HARMFUL_LINK: &str = "This message contains a link that might be harmful.";
pub const PHISHING: &str = "This message could be a phishing attempt.";
pub const BLOCKED_OWN: &str = "This message was blocked.";
pub const BLOCKED_OWN_REPORTED: &str = "Blocked. You've reported this to your admin.";
pub const BLOCKED_OTHERS: &str = "This message was blocked due to organization policy.";
pub const FLAGGED: &str = "This message was flagged.";
pub const FLAGGED_REPORTED: &str = "Flagged. You've reported this to your admin.";

pub const FILE_AND_LINK_SAFETY: &str = "Learn about file and link safety";
pub const LEARN_MORE: &str = "Learn more.";
pub const WHATS_THIS: &str = "What's this?";

/// Microsoft's page on file and link safety in Teams.
pub const FILE_AND_LINK_SAFETY_PAGE: &str = "https://go.microsoft.com/fwlink/?linkid=2293545";
/// Microsoft's page on harmful content removed after delivery.
pub const HARMFUL_CONTENT_PAGE: &str = "https://aka.ms/AAinvpp";
/// Microsoft's page on data loss prevention in Teams.
pub const POLICY_PAGE: &str = "https://go.microsoft.com/fwlink/?LinkId=2132837";

/// The notice a message's properties (JSON text) call for, to its sender (`own`) or to anyone else.
#[must_use]
pub fn of(properties: Option<&str>, own: bool) -> Option<PolicyNotice> {
    let properties: serde_json::Value = serde_json::from_str(properties?).ok()?;
    PolicyViolation::of(properties.get("policyViolation"))
        .known()?
        .notice(own)
}

/// The notice's line, as Teams words it for the sender (`own`) or anyone else.
#[must_use]
pub fn words(notice: PolicyNotice, own: bool) -> &'static str {
    match notice {
        PolicyNotice::FileTypeBlocked => FILE_TYPE_BLOCKED,
        PolicyNotice::HarmfulContentBlocked => HARMFUL_CONTENT_BLOCKED,
        PolicyNotice::HarmfulLink => HARMFUL_LINK,
        PolicyNotice::Phishing => PHISHING,
        PolicyNotice::Blocked { reported: true } if own => BLOCKED_OWN_REPORTED,
        PolicyNotice::Blocked { .. } if own => BLOCKED_OWN,
        PolicyNotice::Blocked { .. } => BLOCKED_OTHERS,
        PolicyNotice::Flagged { reported: true } => FLAGGED_REPORTED,
        PolicyNotice::Flagged { reported: false } => FLAGGED,
    }
}

/// The link under the line and the page it opens: none for phishing, nor once the sender reported the verdict.
#[must_use]
pub fn link(notice: PolicyNotice, own: bool) -> Option<(&'static str, &'static str)> {
    match notice {
        PolicyNotice::FileTypeBlocked | PolicyNotice::HarmfulLink => {
            Some((FILE_AND_LINK_SAFETY, FILE_AND_LINK_SAFETY_PAGE))
        }
        PolicyNotice::HarmfulContentBlocked => Some((LEARN_MORE, HARMFUL_CONTENT_PAGE)),
        PolicyNotice::Phishing
        | PolicyNotice::Blocked { reported: true }
        | PolicyNotice::Flagged { reported: true } => None,
        PolicyNotice::Blocked { .. } | PolicyNotice::Flagged { .. } if own => Some((LEARN_MORE, POLICY_PAGE)),
        PolicyNotice::Blocked { .. } | PolicyNotice::Flagged { .. } => Some((WHATS_THIS, POLICY_PAGE)),
    }
}

/// Whether Teams draws the notice as an error (red): a file type, a link or phishing, or the sender's own; else muted.
#[must_use]
pub fn is_error(notice: PolicyNotice, own: bool) -> bool {
    own || matches!(
        notice,
        PolicyNotice::FileTypeBlocked | PolicyNotice::HarmfulLink | PolicyNotice::Phishing
    )
}

/// The red Teams draws an error line in: its darker red on a light theme, its lighter one on a dark one.
fn error_colour(palette: Palette) -> u32 {
    if theme::palette_is_dark(palette) {
        0x00e3_7d80
    } else {
        theme::DANGER
    }
}

/// The notice over message `id`'s text: the icon, the line, and the link under the line.
#[must_use]
pub fn line(id: &str, notice: PolicyNotice, own: bool, palette: Palette) -> AnyElement {
    let colour = if is_error(notice, own) {
        error_colour(palette)
    } else {
        palette.muted_foreground
    };
    let glyph = match notice {
        PolicyNotice::HarmfulLink | PolicyNotice::Phishing => Glyph::WARNING,
        PolicyNotice::Flagged { .. } => Glyph::FLAG,
        _ => Glyph::BAN,
    };
    let selector = format!("policy-notice-{id}");
    let text = div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(div().text_color(rgb(colour)).child(words(notice, own)))
        .children(link(notice, own).map(|(label, page)| learn_link(id, label, page, palette)));
    div()
        .debug_selector(move || selector)
        .flex()
        .items_start()
        .gap_1()
        .pb_1()
        .child(
            div()
                .flex_none()
                .pt(px(2.0))
                .child(glyph.icon().size(px(16.0)).text_color(rgb(colour))),
        )
        .child(text)
        .into_any_element()
}

fn learn_link(id: &str, label: &'static str, page: &'static str, palette: Palette) -> AnyElement {
    let selector = format!("policy-learn-{id}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .cursor_pointer()
        .underline()
        .text_color(rgb(palette.brand))
        .child(label)
        .on_click(move |_, _, cx: &mut App| cx.open_url(page))
        .into_any_element()
}
