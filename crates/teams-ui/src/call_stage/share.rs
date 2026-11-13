//! The call window's Share, as Teams' toolbar has it between the microphone and Leave: a flyout titled "Share content"
//! whose "Screen, window, or tab" asks the desktop which to share (its portal), and while the account shares, a red
//! Stop sharing in its place. Teams' other entries (Microsoft Whiteboard, `PowerPoint` Live, Excel Live, Browse OneDrive,
//! Browse my computer) are Microsoft's web apps and files shared through them: none of them is offered here.
use super::*;

impl CallStage {
    /// Whether the call takes Share: a call's that is up and carries video, 1:1 or group call or meeting, as Teams
    /// offers it in each; never the test call's or in the read-only mode.
    pub(super) fn takes_share(&self, active: &ActiveCall, cx: &App) -> bool {
        active.kind != CallKind::TestCall
            && active.status == CallStatus::Connected
            && self.calls.read(cx).video().is_some()
            && !self.core.read_only()
    }

    /// Share with its flyout, or Stop sharing while the account shares; none where the call takes no Share.
    pub(super) fn share_tool(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.takes_share(active, cx) {
            return None;
        }
        if active.sharing {
            return Some(stop_sharing(self.caption("Stop sharing"), palette, cx));
        }
        let stage = cx.weak_entity();
        Some(
            selected_by(
                "call-share",
                toolbar::frame_button("call-share", palette, cx)
                    .h(px(48.0))
                    .child(toolbar::tool_face(Glyph::SCREEN_SHARE, self.caption("Share")))
                    .tooltip("Share content")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                        let stage = stage.clone();
                        menu.min_w(px(340.0))
                            .item(
                                PopupMenuItem::element(move |_, cx| share_content(cx)).on_click(move |_, _, cx| {
                                    let _ = stage.update(cx, |stage, cx| stage.ask(Ask::ToggleShare, cx));
                                }),
                            )
                    }),
            )
            .into_any_element(),
        )
    }
}

/// The flyout's content, as Teams' "Share content" has it: its title over the tile that shares a screen, a window or a
/// tab (the desktop asks which).
fn share_content(cx: &App) -> AnyElement {
    let palette = theme::current(cx);
    div()
        .debug_selector(|| "call-share-content".to_owned())
        .w_full()
        .py_1()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Share content"),
        )
        .child(
            div()
                .debug_selector(|| "call-share-screen".to_owned())
                .w_full()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.foreground))
                .child(Glyph::MONITOR.icon().size(px(20.0)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Screen, window, or tab"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .child("Choose what you want to share"),
                        ),
                ),
        )
        .into_any_element()
}

/// Teams' Stop sharing: its red mark with a cross over its caption.
fn stop_sharing(caption: &'static str, palette: Palette, cx: &mut Context<CallStage>) -> AnyElement {
    let mark = div()
        .w(px(24.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .bg(rgb(theme::DANGER))
        .text_color(gpui::white())
        .child(Glyph::X.icon().size(px(14.0)));
    div()
        .debug_selector(|| "call-stop-sharing".to_owned())
        .id("call-stop-sharing")
        .h(px(48.0))
        .min_w(px(if caption.is_empty() { 40.0 } else { 56.0 }))
        .px_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(rgb(palette.on_frame))
        .hover(move |tool| tool.bg(rgb(palette.on_frame).opacity(0.12)))
        .tooltip(|window, cx| Tooltip::new("Stop sharing").build(window, cx))
        .on_click(cx.listener(|this, _, _, cx| this.ask(Ask::ToggleShare, cx)))
        .child(mark)
        .when(!caption.is_empty(), |tool| tool.child(div().text_xs().child(caption)))
        .into_any_element()
}
