//! Local files staged in the editor. A drop never changes the text or sends a message.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::Sizable as _;
use teams_proto::chat::FileKind;

use super::Compose;
use crate::files::{kind_colour, kind_glyph};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// What Teams says over a channel's box when a file went to the channel's folder as a numbered copy.
pub const COPY_NOTICE: &str = "Uploading a copy because a file with the same name is shared in this channel.";

/// A local file waiting for Send, kept apart from the editor's rich text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub state: AttachmentState,
}

/// Uploads are private until Send; a failed file remains in the draft with Retry and Remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentState {
    Uploading,
    Ready(teams_proto::UploadedFile),
    Failed(gpui::SharedString),
}

impl Attachment {
    /// Reads only metadata: directories, devices, missing files and non-local paths aren't attachments.
    ///
    /// # Errors
    /// Rejects unreadable names, missing files, non-files, relative paths and files above the upload limit.
    pub fn of(path: &Path) -> Result<Self, &'static str> {
        if !path.is_absolute() {
            return Err("Choose a file on this device.");
        }
        let metadata = path
            .metadata()
            .map_err(|_| "That file can't be read. Choose it again.")?;
        if !metadata.is_file() {
            return Err("Folders can't be attached. Choose the files inside the folder.");
        }
        if metadata.len() > teams_proto::MAX_ATTACHMENT_SIZE {
            return Err("Choose a file smaller than 250 MB.");
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or("That file's name can't be read. Rename it and choose it again.")?;
        Ok(Self {
            id: 0,
            path: path.to_owned(),
            name: name.to_owned(),
            size: metadata.len(),
            state: AttachmentState::Uploading,
        })
    }
    fn complete(&mut self, answer: Result<teams_proto::UploadedFile, teams_core::EngineError>) {
        self.state = match answer {
            Ok(uploaded) => {
                uploaded.name().clone_into(&mut self.name);
                self.size = uploaded.size();
                AttachmentState::Ready(uploaded)
            }
            Err(error) => AttachmentState::Failed(error.to_string().into()),
        };
    }
}

fn size_label(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{} KB", size.div_ceil(1024))
    } else {
        format!("{} MB", size.div_ceil(1024 * 1024))
    }
}

impl Compose {
    /// Adds files to this chat's draft without replacing its text, reply, or files already chosen.
    pub fn attach_files(&mut self, paths: &[PathBuf], window: &mut gpui::Window, cx: &mut Context<Self>) {
        if !self.accepts_files() {
            return;
        }
        self.problem = None;
        for path in paths {
            if self.attachments.iter().any(|file| file.path == *path) {
                continue;
            }
            match Attachment::of(path) {
                Ok(mut file) => {
                    self.next_attachment += 1;
                    file.id = self.next_attachment;
                    self.attachments.push(file);
                    self.upload_attachment(self.next_attachment, cx);
                }
                Err(error) => self.problem = Some(error.into()),
            }
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// Says why files dropped on a box that takes none stay out ([`Compose::accepts_files`]); the read-only box says
    /// so already.
    pub fn refuse_files(&mut self, cx: &mut Context<Self>) {
        if self.read_only.is_some() {
            return;
        }
        self.show_problem(
            "Files can't be added to a message you edit. Send them in a new message.",
            cx,
        );
    }

    /// Files belong to new messages, posts and replies; an existing message's files aren't edited here.
    #[must_use]
    pub fn accepts_files(&self) -> bool {
        self.read_only.is_none() && !matches!(self.mode, super::Mode::Edit { .. })
    }

    #[must_use]
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// Takes `file` from another box of the same chat ([`Compose::hand_draft_to`]), unless the box has that file. The
    /// answer of an upload still on its way (`route`) comes here; with no route, the upload starts again here. A box
    /// that takes no files (it edits a message) keeps the file out and says so.
    pub(super) fn take_attachment(
        &mut self,
        mut file: Attachment,
        route: Option<super::Route<u64>>,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_files() {
            self.show_problem(format!("{} can't be attached here.", file.name), cx);
            return;
        }
        if self.attachments.iter().any(|held| held.path == file.path) {
            return;
        }
        self.next_attachment += 1;
        let id = self.next_attachment;
        file.id = id;
        let uploading = file.state == AttachmentState::Uploading;
        self.attachments.push(file);
        if uploading {
            match route {
                Some(route) => {
                    *route.borrow_mut() = (cx.weak_entity(), id);
                    self.routes.files.insert(id, route);
                }
                None => self.upload_attachment(id, cx),
            }
        }
        cx.notify();
    }

    fn upload_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        let in_channel = self.writes_a_channel();
        let Some(file) = self.attachments.iter_mut().find(|file| file.id == id) else {
            return;
        };
        file.state = AttachmentState::Uploading;
        let chosen = file.name.clone();
        let reply = if in_channel {
            self.core
                .upload_channel_file(self.conversation.clone(), file.path.clone())
        } else {
            self.core.upload_file(file.path.clone())
        };
        let route: super::Route<u64> = Rc::new(RefCell::new((cx.weak_entity(), id)));
        self.routes.files.insert(id, Rc::clone(&route));
        cx.spawn(async move |_, cx| {
            let answer = reply.await;
            // the file may have moved to another box meanwhile ([`Compose::hand_draft_to`])
            let (to, id) = route.borrow().clone();
            let _ = to.update(cx, |this, cx| {
                if this.routes.files.get(&id).is_some_and(|held| Rc::ptr_eq(held, &route)) {
                    this.routes.files.remove(&id);
                }
                if let Some(file) = this.attachments.iter_mut().find(|file| file.id == id) {
                    file.complete(answer);
                    // the folder had the name: Teams says the file went as a copy
                    if matches!(&file.state, AttachmentState::Ready(uploaded) if uploaded.channel().is_some() && uploaded.name() != chosen) {
                        this.copy_notice = true;
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Takes a file out of the draft. A channel file already in the channel's folder goes to its site's recycle bin,
    /// as Teams' Remove sends it: otherwise the team would see a file nobody posted.
    pub fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(index) = self.attachments.iter().position(|file| file.id == id) else {
            return;
        };
        let file = self.attachments.remove(index);
        if let AttachmentState::Ready(uploaded) = file.state
            && uploaded.channel().is_some()
        {
            let discarded = self.core.discard_channel_file(uploaded);
            cx.spawn(async move |_, _| {
                let _ = discarded.await;
            })
            .detach();
        }
        if !self
            .attachments
            .iter()
            .any(|file| matches!(&file.state, AttachmentState::Ready(uploaded) if uploaded.channel().is_some()))
        {
            self.copy_notice = false;
        }
        cx.notify();
    }

    /// Teams' notice over a channel's box once a file went to the channel's folder as a numbered copy.
    pub(super) fn copy_notice_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.copy_notice {
            return None;
        }
        let close = selected_by(
            "compose-copy-notice-close",
            icon_button("compose-copy-notice-close", Glyph::X, "Close")
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.copy_notice = false;
                    cx.notify();
                })),
        );
        Some(
            div()
                .debug_selector(|| "compose-copy-notice".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(crate::theme::mix(palette.background, crate::theme::WARNING, 0.55)))
                .bg(rgb(crate::theme::mix(palette.background, crate::theme::WARNING, 0.2)))
                .text_sm()
                .text_color(rgb(palette.foreground))
                .child(div().flex_1().min_w_0().child(COPY_NOTICE))
                .child(close)
                .into_any_element(),
        )
    }

    pub(super) fn files_ready(&self) -> bool {
        self.attachments
            .iter()
            .all(|file| matches!(file.state, AttachmentState::Ready(_)))
    }

    pub(super) fn attachment_cards(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "compose-attachments".to_owned())
                .id("compose-attachments")
                .max_h(px(260.0))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(
                    self.attachments
                        .iter()
                        .enumerate()
                        .map(|(index, file)| Self::attachment_card(index, file, palette, cx)),
                )
                .into_any_element(),
        )
    }

    fn attachment_card(index: usize, file: &Attachment, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (status, colour) = match &file.state {
            AttachmentState::Uploading => ("Uploading…".to_owned(), palette.brand),
            AttachmentState::Ready(_) => (size_label(file.size), palette.muted_foreground),
            AttachmentState::Failed(error) => (error.to_string(), crate::theme::DANGER),
        };
        let kind = FileKind::of(file.name.rsplit_once('.').map_or("", |(_, extension)| extension));
        div()
            .debug_selector(move || format!("compose-attachment-{index}"))
            .w(px(316.0))
            .max_w_full()
            .flex()
            .flex_col()
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_md()
            .bg(rgb(palette.surface))
            .child(Self::attachment_header(index, file, kind, palette, cx))
            .child(
                div()
                    .h(px(150.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(palette.background))
                    .child(
                        kind_glyph(kind)
                            .icon()
                            .size(px(40.0))
                            .text_color(rgb(kind_colour(kind))),
                    ),
            )
            .child(div().px_3().py_1().text_xs().text_color(rgb(colour)).child(status))
            .into_any_element()
    }

    fn attachment_header(
        index: usize,
        file: &Attachment,
        kind: FileKind,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = file.id;
        let remove = selected_by(
            format!("compose-attachment-remove-{index}"),
            icon_button(("attachment-remove", index), Glyph::X, "Remove attachment")
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.remove_attachment(id, cx);
                    this.focus(window, cx);
                })),
        );
        let retry = matches!(file.state, AttachmentState::Failed(_)).then(|| {
            selected_by(
                format!("compose-attachment-retry-{index}"),
                icon_button(("attachment-retry", index), Glyph::REFRESH, "Retry upload")
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.upload_attachment(id, cx);
                        this.focus(window, cx);
                    })),
            )
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                kind_glyph(kind)
                    .icon()
                    .size(px(24.0))
                    .text_color(rgb(kind_colour(kind))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .truncate()
                    .child(file.name.clone()),
            )
            .children(retry)
            .child(remove)
            .into_any_element()
    }
}
