//! Finding someone to bring into the call: People's "Invite someone" box (a 1:1 call's and a group chat's call; Teams
//! words it "Invite someone or dial a number", but ngteams dials no numbers) and the person Transfer picks.
//! Typing names someone the account knows or the directory finds (the scheduling form's suggestions:
//! `Core::invitable` and the service's search); only work accounts are offered, never a number, nor anyone already in
//! the call.
use super::*;
use gpui::{AppContext as _, AsyncApp, Task, WeakEntity};
use gpui_component::input::{Input, InputEvent, InputState};
use teams_core::{Invitable, SearchScope};

/// A box that finds people, and what it found.
pub(super) struct Finder {
    pub(super) query: Entity<InputState>,
    pub(super) found: Vec<Invitable>,
    searching: Option<Task<()>>,
}

/// Which box looks: People's, or the transfer's dialog's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FinderOf {
    People,
    Dialog,
}

impl CallStage {
    /// A new box with `placeholder` for `of`, searching as it is typed in.
    pub(super) fn finder(
        of: FinderOf,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Finder {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe_in(
            &query,
            window,
            move |this: &mut Self, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().trim().to_owned();
                    this.search(of, text, cx);
                }
            },
        )
        .detach();
        Finder {
            query,
            found: Vec::new(),
            searching: None,
        }
    }

    /// The box of `of`, if one is open.
    fn finder_of(&mut self, of: FinderOf) -> Option<&mut Finder> {
        match of {
            FinderOf::People => self.inviting.as_mut(),
            FinderOf::Dialog => self.dialog.as_mut().map(|dialog| &mut dialog.finder),
        }
    }

    /// Looks for the people `query` names, a moment after the typing stops: work accounts only, nobody in the call
    /// and not the account itself.
    fn search(&mut self, of: FinderOf, query: String, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let me = core.me().to_owned();
        let present: Vec<String> = self
            .live(cx)
            .map(|active| {
                active
                    .view
                    .participants
                    .iter()
                    .map(|participant| participant.mri.clone())
                    .collect()
            })
            .unwrap_or_default();
        let Some(finder) = self.finder_of(of) else { return };
        if query.chars().count() < 2 {
            finder.found.clear();
            finder.searching = None;
            cx.notify();
            return;
        }
        finder.searching = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(crate::event_form::people::PAUSE).await;
            let known = core.invitable(query.clone());
            let found = core.search_service(query.clone(), SearchScope::Suggestions);
            let known = known.await.unwrap_or_default();
            let found = found.await.unwrap_or_default();
            let people: Vec<Invitable> = crate::event_form::people::merged(known, &found)
                .into_iter()
                .filter(|person| {
                    person.mri.starts_with("8:orgid:") && person.mri != me && !present.contains(&person.mri)
                })
                .collect();
            let _ = this.update(cx, |stage, cx| {
                let Some(finder) = stage.finder_of(of) else { return };
                if finder.query.read(cx).value().trim() == query {
                    finder.found = people;
                    cx.notify();
                }
            });
        }));
    }

    /// People's box, opened with the pane when the call takes someone added; put away with it.
    pub(super) fn follow_invite_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.pane == Pane::People && self.live(cx).is_some_and(|active| self.takes_add(&active));
        match (wanted, self.inviting.is_some()) {
            (true, false) => self.inviting = Some(Self::finder(FinderOf::People, INVITE_PLACEHOLDER, window, cx)),
            (false, true) => self.inviting = None,
            _ => {}
        }
    }

    /// Whether the call takes someone added from People, as Teams' does: a chat's call (1:1 or group) that is up,
    /// never a meeting's, the test call, or in the read-only mode.
    pub(super) fn takes_add(&self, active: &ActiveCall) -> bool {
        matches!(active.kind, CallKind::Chat | CallKind::Incoming | CallKind::Join)
            && !active.is_meeting()
            && active.status == CallStatus::Connected
            && !self.core.read_only()
    }

    /// `person` picked in People's box: they're added to the call, and the box is emptied.
    fn invite_person(&mut self, person: Invitable, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(finder) = &mut self.inviting {
            finder.found.clear();
            finder.query.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.ask(
            Ask::Add {
                invitee: Mri::new(&person.mri),
                name: person.name,
            },
            cx,
        );
    }

    /// People's "Invite someone" box, with what it found under it.
    pub(super) fn invite_box(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let finder = self.inviting.as_ref()?;
        let found = found_list(finder, "call-invite-found", palette, cx, |this, person, window, cx| {
            this.invite_person(person, window, cx);
        });
        Some(
            div()
                .debug_selector(|| "call-invite".to_owned())
                .relative()
                .px_4()
                .pb_2()
                .child(
                    Input::new(&finder.query)
                        .suffix(Glyph::SEARCH.icon().size(px(14.0)))
                        .cleanable(false),
                )
                .children(found)
                .into_any_element(),
        )
    }
}

impl CallStage {
    /// People's box with `typed` in it and `found` under it, as a search would leave it (the UI tour's).
    pub fn found_to_invite(&mut self, typed: &str, found: Vec<Invitable>, window: &mut Window, cx: &mut Context<Self>) {
        if self.inviting.is_none() {
            self.inviting = Some(Self::finder(FinderOf::People, INVITE_PLACEHOLDER, window, cx));
        }
        if let Some(finder) = &mut self.inviting {
            finder
                .query
                .update(cx, |input, cx| input.set_value(typed.to_owned(), window, cx));
            finder.searching = None;
            finder.found = found;
        }
        cx.notify();
    }
}

/// People's box's placeholder, as Teams words it.
pub(super) const INVITE_PLACEHOLDER: &str = "Invite someone";

/// What `finder` found, under its box (each row's selector `<selector>-<index>`): the person's initials, name and
/// address; picking one does `pick`.
pub(super) fn found_list(
    finder: &Finder,
    selector: &'static str,
    palette: Palette,
    cx: &mut Context<CallStage>,
    pick: impl Fn(&mut CallStage, Invitable, &mut Window, &mut Context<CallStage>) + Clone + 'static,
) -> Option<AnyElement> {
    if finder.found.is_empty() {
        return None;
    }
    let rows = finder.found.iter().enumerate().map(|(index, person)| {
        let picked = person.clone();
        let name = SharedString::from(person.name.clone());
        let pick = pick.clone();
        div()
            .debug_selector(move || format!("{selector}-{index}"))
            .id(SharedString::from(format!("{selector}-{index}")))
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(people::avatar(Some(&name), None, Size::Small))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(person.address.clone()),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| pick(this, picked.clone(), window, cx)))
    });
    // painted over what follows it (People's list, the dialog's buttons), as Teams' suggestions float
    Some(
        gpui::deferred(
            div()
                .debug_selector(move || selector.to_owned())
                .occlude()
                .absolute()
                .top(px(40.0))
                .left_4()
                .right_4()
                .py_1()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .children(rows),
        )
        .with_priority(1)
        .into_any_element(),
    )
}
