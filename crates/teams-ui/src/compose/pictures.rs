//! A picture pasted into the box is sent at once, as Teams' box sends one: to the media service, readable by this
//! conversation alone (`Core::upload_picture`, through the engine's outbox), while it shows in the box; the message
//! then names it ([`crate::editor::pictures`]). Files a paste brings that aren't pictures are attached as a drop's are.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{Context, Window};

use super::Compose;
use crate::editor::pictures::PictureState;

impl Compose {
    /// Sends picture `id` of the editor (just put in the box, or its Retry pressed).
    pub(super) fn send_picture(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(picture) = self.editor.read(cx).picture(id).map(|picture| picture.bytes.clone()) else {
            return;
        };
        if self.read_only.is_some() {
            return;
        }
        let reply = self.core.upload_picture(self.conversation.clone(), (*picture).clone());
        let route: super::Route<u64> = Rc::new(RefCell::new((cx.weak_entity(), id)));
        self.routes.pictures.insert(id, Rc::clone(&route));
        cx.spawn(async move |_, cx| {
            let state = match reply.await {
                Ok(object) => PictureState::Sent(object),
                Err(error) => PictureState::Failed(error.to_string().into()),
            };
            // the picture may have moved to another box meanwhile ([`Compose::hand_draft_to`])
            let (to, id) = route.borrow().clone();
            let _ = to.update(cx, |this, cx| {
                if this
                    .routes
                    .pictures
                    .get(&id)
                    .is_some_and(|held| Rc::ptr_eq(held, &route))
                {
                    this.routes.pictures.remove(&id);
                }
                this.editor
                    .update(cx, |editor, cx| editor.set_picture_state(id, state, cx));
            });
        })
        .detach();
    }

    /// The box as first shown grows to show a picture whole, as Teams' does: its most height is the expanded box's while
    /// it shows one, else its few lines'.
    pub(super) fn fit_pictures(&mut self, cx: &mut Context<Self>) {
        if self.expanded {
            return;
        }
        let pictures = !self.editor.read(cx).shown_pictures().is_empty();
        let most = gpui::px(if pictures {
            super::EXPANDED_MAX
        } else {
            super::COLLAPSED_MAX
        });
        self.editor
            .update(cx, |editor, cx| editor.set_heights(None, Some(most), cx));
    }

    /// Attaches the files a paste brought that aren't pictures, where the box takes files.
    pub(super) fn pasted_files(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_files() {
            self.attach_files(paths, window, cx);
        } else {
            self.refuse_files(cx);
        }
    }
}
