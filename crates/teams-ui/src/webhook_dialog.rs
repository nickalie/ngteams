//! Manage team's outgoing webhook dialog (`docs/protocol/bots.md`, "Outgoing webhooks"), as Microsoft's documentation
//! describes Teams' own:
//!
//! - **Create an outgoing webhook**: Name, Callback URL (HTTPS), Description and a profile picture (Upload, a PNG or
//!   JPEG), then Create. Teams then shows the security token once, with Copy; Close tells the page to read the list
//!   again.
//! - **Edit**: the same form on a webhook, with Remove for its picture; Save sends only what changed.
//!
//! Nothing is sent but through [`Core::team_write`], and only on Create or Save.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Image,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions, Render, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, FocusTrapElement as _};
use teams_core::EngineError;
use teams_proto::{OutgoingWebhook, SecurityToken, TeamWrite, WebhookForm, WebhookPicture};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, selected_by};
use crate::team_app_dialogs::picture_format;
use crate::team_dialogs::{TeamDialogEvent, buttons, label, problem, selected_by_full, surface, title};
use crate::theme::{self, Palette};

/// The largest picture a webhook takes, as the own server's default (`outgoing_webhooks.max_picture_bytes`).
pub const WEBHOOK_PICTURE_LIMIT: usize = 1024 * 1024;

/// The longest name a webhook takes, as the own server's limit: Teams' own limit isn't documented.
pub const WEBHOOK_NAME_LIMIT: usize = 100;

/// What the token step says about the token.
pub const TOKEN_WORDS: &str = "Use this security token to check that the requests to your service come from Teams. \
                               Copy it now: Teams doesn't show it again.";

/// What a webhook whose name the team has gets.
pub const NAME_TAKEN: &str = "This team already has an outgoing webhook with this name. Choose another name.";

/// What a callback URL that isn't HTTPS gets.
pub const NOT_HTTPS: &str = "Enter a callback URL that starts with https://.";

/// The dialog's form, or its token once a create was answered.
enum Step {
    Form,
    Token(SecurityToken),
}

/// The outgoing webhook dialog (see the module docs).
pub struct WebhookDialog {
    core: Arc<dyn Core>,
    team: Team,
    /// The webhook it edits; none when it creates one.
    editing: Option<OutgoingWebhook>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    description: Entity<InputState>,
    picture: WebhookPicture,
    /// The picture the edited webhook has, once read.
    current: Option<Vec<u8>>,
    step: Step,
    copied: bool,
    saving: bool,
    /// An Upload's file picker or read is under way.
    uploading: bool,
    problem: Option<&'static str>,
    _reading: Option<Task<()>>,
    upload: Option<Task<()>>,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _typed: Vec<Subscription>,
}

impl EventEmitter<TeamDialogEvent> for WebhookDialog {}

impl WebhookDialog {
    /// "Create an outgoing webhook" on `team`, or the Edit of `editing`, its fields filled in.
    pub fn new(
        core: Arc<dyn Core>,
        team: Team,
        editing: Option<OutgoingWebhook>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let hook = editing.as_ref();
        let name = field(hook.map_or("", |hook| &hook.name), "Enter a name", window, cx);
        let url = field(
            hook.and_then(|hook| hook.callback_url.as_deref()).unwrap_or_default(),
            "https://",
            window,
            cx,
        );
        let description = field(
            hook.map_or("", |hook| &hook.description),
            "Describe what it does",
            window,
            cx,
        );
        let typed = [&name, &url, &description]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this: &mut Self, _, _: &InputEvent, cx| {
                    this.problem = None;
                    cx.notify();
                })
            })
            .collect();
        name.update(cx, |name, cx| name.focus(window, cx));
        let reading = editing
            .as_ref()
            .and_then(|hook| hook.app().definition.outgoing_webhook())
            .filter(|read| read.version.is_some())
            .map(|read| {
                let reply = core.outgoing_webhook_picture(read);
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let current = reply.await.ok().flatten();
                    let _ = this.update(cx, |this, cx| {
                        this.current = current;
                        cx.notify();
                    });
                })
            });
        Self {
            core,
            team,
            editing,
            name,
            url,
            description,
            picture: WebhookPicture::Keep,
            current: None,
            step: Step::Form,
            copied: false,
            saving: false,
            uploading: false,
            problem: None,
            _reading: reading,
            upload: None,
            working: None,
            focus: cx.focus_handle(),
            _typed: typed,
        }
    }

    /// The form's fields set to these words, as typed.
    pub fn fill(&mut self, (name, url, description): (&str, &str, &str), window: &mut Window, cx: &mut Context<Self>) {
        for (input, text) in [(&self.name, name), (&self.url, url), (&self.description, description)] {
            let text = text.to_owned();
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        cx.notify();
    }

    /// The name, callback URL and description as typed, trimmed.
    #[must_use]
    pub fn values(&self, cx: &Context<Self>) -> (String, String, String) {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        (text(&self.name), text(&self.url), text(&self.description))
    }

    /// Whether Create or Save may go: a name and a callback URL, nothing under way.
    #[must_use]
    pub fn can_send(&self, cx: &Context<Self>) -> bool {
        let (name, url, _) = self.values(cx);
        !self.saving
            && !self.uploading
            && !name.is_empty()
            && !url.is_empty()
            && name.chars().count() <= WEBHOOK_NAME_LIMIT
    }

    /// What the form says is wrong, while it says it.
    #[must_use]
    pub fn problem(&self) -> Option<&'static str> {
        self.problem
    }

    /// The token the create's answer showed, while the dialog shows it.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        match &self.step {
            Step::Token(token) => Some(token.as_str()),
            Step::Form => None,
        }
    }

    /// A picture's bytes chosen: taken when PNG or JPEG and not too large, else said why not.
    pub fn choose(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.len() > WEBHOOK_PICTURE_LIMIT {
            self.problem = Some("The picture is too large. Choose one under 1 MB.");
        } else if picture_format(&bytes).is_none() {
            self.problem = Some("That file can't be used. Choose a .png, .jpg or .jpeg picture.");
        } else {
            self.problem = None;
            self.picture = WebhookPicture::Set(bytes);
        }
        cx.notify();
    }

    /// The picture's Remove: the webhook keeps none.
    pub fn remove_picture(&mut self, cx: &mut Context<Self>) {
        self.picture = if self.editing.as_ref().is_some_and(|hook| hook.picture_url.is_some()) {
            WebhookPicture::Remove
        } else {
            WebhookPicture::Keep
        };
        cx.notify();
    }

    /// Upload: a picture picked from the disk.
    pub fn upload(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload".into()),
            ..PathPromptOptions::default()
        });
        self.uploading = true;
        cx.notify();
        self.upload = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let path: Option<PathBuf> = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let read = match path {
                Some(path) => Some(cx.background_spawn(async move { std::fs::read(path) }).await),
                None => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.uploading = false;
                match read {
                    Some(Ok(bytes)) => this.choose(bytes, cx),
                    Some(Err(error)) => {
                        log::info!("a webhook's picture couldn't be read: {error}");
                        this.problem = Some("That file can't be read.");
                    }
                    None => {}
                }
                cx.notify();
            });
        }));
    }

    /// The form as the write sends it: all of it for a create, only what changed for an edit.
    fn form(&self, cx: &Context<Self>) -> WebhookForm {
        let (name, url, description) = self.values(cx);
        let changed = |now: String, was: Option<&str>| (was != Some(now.as_str())).then_some(now);
        let hook = self.editing.as_ref();
        WebhookForm {
            name: changed(name, hook.map(|hook| hook.name.as_str())),
            callback_url: changed(url, hook.and_then(|hook| hook.callback_url.as_deref())),
            description: changed(description, hook.map(|hook| hook.description.as_str())),
            picture: self.picture.clone(),
        }
    }

    /// Create or Save.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let (_, url, _) = self.values(cx);
        if !url.to_ascii_lowercase().starts_with("https://") {
            self.problem = Some(NOT_HTTPS);
            cx.notify();
            return;
        }
        let team = self.team.id.clone();
        let form = self.form(cx);
        let write = match &self.editing {
            Some(hook) => TeamWrite::ChangeOutgoingWebhook {
                team,
                webhook: hook.id.clone(),
                form,
            },
            None => TeamWrite::CreateOutgoingWebhook { team, form },
        };
        self.saving = true;
        self.problem = None;
        cx.notify();
        let reply = self.core.team_write(write, None);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.answered(answer, cx));
        }));
    }

    fn answered(&mut self, answer: Result<teams_core::TeamWritten, EngineError>, cx: &mut Context<Self>) {
        self.saving = false;
        match answer {
            Ok(written) if self.editing.is_none() => match written.created_webhook {
                Some(created) => self.step = Step::Token(created.token),
                None => self.closed_changed("Outgoing webhook created", cx),
            },
            Ok(_) => self.closed_changed("Outgoing webhook updated", cx),
            Err(EngineError::TeamNotChanged { code: Some(code) }) if code == "OutgoingWebhookNameAlreadyExists" => {
                self.problem = Some(NAME_TAKEN);
            }
            Err(_) if self.editing.is_some() => {
                self.problem = Some("We couldn't update the outgoing webhook. Please try again.");
            }
            Err(_) => self.problem = Some("We couldn't create the outgoing webhook. Please try again."),
        }
        cx.notify();
    }

    fn closed_changed(&self, words: &'static str, cx: &mut Context<Self>) {
        cx.emit(TeamDialogEvent::Changed {
            team: self.team.id.clone(),
            words,
        });
    }

    /// The token step's Copy.
    pub fn copy_token(&mut self, cx: &mut Context<Self>) {
        if let Step::Token(token) = &self.step {
            cx.write_to_clipboard(ClipboardItem::new_string(token.as_str().to_owned()));
            self.copied = true;
            cx.notify();
        }
    }

    /// Close or Cancel: after a create, the page reads its list again.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        // A create's answer holds the only copy of the token, so the dialog stays until it comes.
        if self.saving {
            return;
        }
        match self.step {
            Step::Token(_) => self.closed_changed("Outgoing webhook created", cx),
            Step::Form => cx.emit(TeamDialogEvent::Closed),
        }
    }

    fn picture_row(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let face = div()
            .debug_selector(|| "webhook-picture".to_owned())
            .size(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border));
        let shown = match &self.picture {
            WebhookPicture::Set(bytes) => Some(bytes),
            WebhookPicture::Keep => self.current.as_ref(),
            WebhookPicture::Remove => None,
        };
        let face: AnyElement = match shown {
            Some(bytes) => match picture_format(bytes) {
                Some(format) => face
                    .child(gpui::img(Arc::new(Image::from_bytes(format, bytes.clone()))).size_full())
                    .into_any_element(),
                None => face.into_any_element(),
            },
            None => face
                .bg(rgb(palette.hover))
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::BOT.icon().size(px(24.0)))
                .into_any_element(),
        };
        let has_picture = matches!(self.picture, WebhookPicture::Set(_))
            || (matches!(self.picture, WebhookPicture::Keep)
                && self.editing.as_ref().is_some_and(|hook| hook.picture_url.is_some()));
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(face)
            .child(selected_by(
                "webhook-upload",
                Button::new("webhook-upload")
                    .outline()
                    .icon(Glyph::UPLOAD.icon())
                    .label("Upload an image")
                    .disabled(self.saving || self.uploading)
                    .on_click(cx.listener(|this, _, _, cx| this.upload(cx))),
            ))
            .when(has_picture, |row| {
                row.child(selected_by(
                    "webhook-picture-remove",
                    Button::new("webhook-picture-remove")
                        .ghost()
                        .icon(Glyph::TRASH.icon())
                        .label("Remove")
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.remove_picture(cx))),
                ))
            })
    }

    fn form_step(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let editing = self.editing.is_some();
        let (send, sending) = if editing {
            ("Save", "Saving…")
        } else {
            ("Create", "Creating…")
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(title(if editing {
                "Edit outgoing webhook"
            } else {
                "Create an outgoing webhook"
            }))
            .child(label("Name", true, palette))
            .child(selected_by_full("webhook-name", Input::new(&self.name)))
            .child(label("Callback URL", true, palette))
            .child(selected_by_full("webhook-url", Input::new(&self.url)))
            .child(label("Description", false, palette))
            .child(selected_by_full("webhook-description", Input::new(&self.description)))
            .child(label("Profile picture", false, palette))
            .child(self.picture_row(palette, cx))
            .children(self.problem.map(|words| problem("webhook-problem", words)))
            .child(
                buttons()
                    .child(selected_by(
                        "webhook-cancel",
                        Button::new("webhook-cancel")
                            .outline()
                            .label("Cancel")
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ))
                    .child(selected_by(
                        "webhook-send",
                        Button::new("webhook-send")
                            .primary()
                            .label(if self.saving { sending } else { send })
                            .disabled(!self.can_send(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                    )),
            )
    }

    fn token_step(&self, token: &str, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title("Outgoing webhook created"))
            .child(div().text_sm().child(TOKEN_WORDS))
            .child(label("Security token", false, palette))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "webhook-token".to_owned())
                            .flex_1()
                            .min_w_0()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.background))
                            .font_family("monospace")
                            .truncate()
                            .child(token.to_owned()),
                    )
                    .child(selected_by(
                        "webhook-token-copy",
                        Button::new("webhook-token-copy")
                            .outline()
                            .icon((if self.copied { Glyph::CHECK } else { Glyph::COPY }).icon())
                            .label(if self.copied { "Copied" } else { "Copy" })
                            .on_click(cx.listener(|this, _, _, cx| this.copy_token(cx))),
                    )),
            )
            .child(
                buttons().child(selected_by(
                    "webhook-close",
                    Button::new("webhook-close")
                        .primary()
                        .label("Close")
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                )),
            )
    }
}

impl Render for WebhookDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body = match &self.step {
            Step::Form => self.form_step(palette, cx),
            Step::Token(token) => {
                let token = token.as_str().to_owned();
                self.token_step(&token, palette, cx)
            }
        };
        surface("webhook-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(cx);
                }
            }))
            .child(body)
            .focus_trap("webhook-dialog-trap", &self.focus)
    }
}
