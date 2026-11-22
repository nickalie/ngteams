//! A send in a translated chat ([`crate::chat_translate`]): the message is translated into the chat's language before
//! it goes to the outbox, the one write path; until then it stays in the box, which says it is being translated and
//! starts no reply or edit meanwhile (the message stays as it was sent, to go as that or be sent again). When
//! translating fails nothing is sent: the box keeps the message and says why, with Try again and Send without
//! translating (the message as written). Once it went out, the chat's translation keeps it as written, so the account
//! keeps seeing what it wrote.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Task, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Sizable as _, Size};
use teams_proto::chat::{MessageText, RichText};

use super::{Compose, Draft, Mode};
use crate::chat_translate::{ChatTranslation, Outgoing};
use crate::icons::{Glyph, selected_by};
use crate::theme::{self, Palette};

/// The box's side of a translated chat.
#[derive(Default)]
pub struct Translated {
    pub(super) model: Option<Entity<ChatTranslation>>,
    /// The translation of the message sent, while it runs, and what the send took from the box (its text, and the
    /// post, reply or edit it is): it is written as that, whatever the box turned to meanwhile.
    running: Option<(Task<()>, Draft)>,
    /// Why the last send wasn't translated: nothing was sent.
    failed: Option<SharedString>,
    /// The next send goes as written (Send without translating).
    as_written: bool,
}

/// What a translated message is sent with: its translation and the message as written.
pub(super) struct Sending {
    pub text: MessageText,
    pub original: Option<RichText>,
}

impl Compose {
    /// The chat's translation: a send in a translated chat is translated first.
    pub fn set_translation(&mut self, model: Entity<ChatTranslation>) {
        self.translated.model = Some(model);
    }

    /// Whether a send's translation is running.
    #[must_use]
    pub fn translating(&self) -> bool {
        self.translated.running.is_some()
    }

    /// Whether the send held for its translation carries the attachment `id`.
    pub(super) fn holds_attachment(&self, id: u64) -> bool {
        self.translated
            .running
            .as_ref()
            .is_some_and(|(_, from)| from.attachments.iter().any(|file| file.id == id))
    }

    /// Why the last send wasn't translated (nothing was sent), if it wasn't.
    #[must_use]
    pub fn untranslated(&self) -> Option<&SharedString> {
        self.translated.failed.as_ref()
    }

    /// Hands `built` (the box's message, `from` what the send took from the box) to the chat's translation, when the
    /// chat is translated: both back to send at once otherwise.
    pub(super) fn translate_first(
        &mut self,
        built: MessageText,
        from: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(MessageText, Draft)> {
        if std::mem::take(&mut self.translated.as_written) {
            return Some((built, from));
        }
        let Some(model) = self.translated.model.clone() else {
            return Some((built, from));
        };
        let Some(task) = model.update(cx, |translation, cx| translation.outgoing(built.rich_text(), cx)) else {
            return Some((built, from));
        };
        self.translated.failed = None;
        self.problem = None;
        let running = cx.spawn_in(window, async move |this, cx| {
            let answer = task.await;
            let _ = this.update_in(cx, |this, window, cx| this.translation_came(answer, built, window, cx));
        });
        self.translated.running = Some((running, from));
        cx.notify();
        None
    }

    /// The translation of a send came: the message goes out translated (or as written, already in the chat's
    /// language); a failure sends nothing and keeps it in the box.
    fn translation_came(&mut self, answer: Outgoing, built: MessageText, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, from)) = self.translated.running.take() else {
            return;
        };
        let sending = match answer {
            Ok(None) => Sending {
                text: built,
                original: None,
            },
            Ok(Some(translation)) => {
                let mentioned = built.mentioned().clone();
                match MessageText::new_with_bots(translation, mentioned, built.bots()) {
                    Ok(text) => Sending {
                        text: text
                            .with_files(built.files().to_vec())
                            .with_cards(built.cards().to_vec()),
                        original: Some(built.rich_text().clone()),
                    },
                    Err(error) => {
                        self.translated.failed = Some(format!("Your message wasn't translated: {error}").into());
                        return cx.notify();
                    }
                }
            }
            Err(failure) => {
                self.translated.failed = Some(failure.message().into());
                return cx.notify();
            }
        };
        self.write_built(sending, from, true, window, cx);
    }

    /// Calls off the translation of a send made as `mode` (its reply or edit cancelled): nothing of it is sent.
    pub(super) fn drop_translation_of(&mut self, mode: &Mode, cx: &mut Context<Self>) {
        if self
            .translated
            .running
            .as_ref()
            .is_some_and(|(_, from)| from.mode == *mode)
        {
            self.translated.running = None;
            cx.notify();
        }
    }

    /// The box emptied: what its last send said goes with it.
    pub(super) fn forget_untranslated(&mut self) {
        self.translated.failed = None;
    }

    /// Try again: the message in the box sent again, translated.
    pub fn retry_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.translated.failed = None;
        self.send(window, cx);
    }

    /// Send without translating: the message in the box sent as written.
    pub fn send_as_written(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.translated.failed = None;
        self.translated.as_written = true;
        self.send(window, cx);
        self.translated.as_written = false;
    }

    /// The chat's translation keeps the message sent as written: an edit's by its message now, a post's once its row
    /// is known ([`Self::sent_post`]).
    pub(super) fn keep_original(
        &self,
        original: &RichText,
        sent: &RichText,
        edit_of: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(model) = &self.translated.model {
            let key = edit_of.map(teams_store::SentOriginal::edit_key);
            model.update(cx, |translation, cx| translation.sent(original, sent, key, cx));
        }
    }

    /// The post sent translated as `sent` went out as the row with `client_message_id` (nothing to keep for one sent
    /// as written).
    pub(super) fn sent_post(&self, client_message_id: String, sent: Option<&RichText>, cx: &mut Context<Self>) {
        if let (Some(model), Some(sent)) = (&self.translated.model, sent) {
            model.update(cx, |translation, _| translation.sent_as(client_message_id, sent));
        }
    }

    /// The line over the box while a send is translated, or after its translation failed.
    pub(super) fn translation_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.translated.running.is_some() {
            return Some(
                div()
                    .debug_selector(|| "compose-translating".to_owned())
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(Glyph::LANGUAGES.icon().size(px(14.0)))
                    .child("Translating your message…")
                    .into_any_element(),
            );
        }
        // a chat no longer translated has nothing to say of it
        let translating = self
            .translated
            .model
            .as_ref()
            .is_some_and(|model| model.read(cx).language().is_some());
        let failed = self.translated.failed.clone().filter(|_| translating)?;
        let again = Button::new("compose-translate-retry")
            .label("Try again")
            .outline()
            .with_size(Size::Small)
            .on_click(cx.listener(|this, _, window, cx| this.retry_translation(window, cx)));
        let as_written = Button::new("compose-send-as-written")
            .label("Send without translating")
            .ghost()
            .with_size(Size::Small)
            .on_click(cx.listener(|this, _, window, cx| this.send_as_written(window, cx)));
        Some(
            div()
                .debug_selector(|| "compose-untranslated".to_owned())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .border_l_2()
                .border_color(rgb(theme::DANGER))
                .text_xs()
                .child(Glyph::LANGUAGES.icon().size(px(14.0)).text_color(rgb(theme::DANGER)))
                .child(div().flex_1().min_w_0().child(failed))
                .child(selected_by("compose-translate-retry", again))
                .child(selected_by("compose-send-as-written", as_written))
                .into_any_element(),
        )
    }
}
