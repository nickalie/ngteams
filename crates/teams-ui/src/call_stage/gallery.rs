//! The stage of a call that is up, as Teams lays it out: the gallery (equal 16:9 tiles, as many to a page as View's
//! Max gallery size and the stage allow, the account's own tile among them in a group call, the rest on pages turned
//! with `<` `>` under it: [`pages`]), the speaker view (the one who speaks, or the one pinned, large over a strip of
//! the others that pages the same way), the self view floating at its bottom right in a 1:1 call and the speaker view
//! (in a stage too narrow to float beside the speaker view's strip, the strip's last tile) unless View hid it, and
//! each tile's own menu (Pin for me, Unpin). While the far end shares its screen, the share fills the stage with the people's tiles (the account's own among them) in a strip on its right, paged when they
//! don't fit, as Teams shows a share; View's Focus on content shows the share alone. Nothing leaves the stage: the
//! call's controls above it stay in sight and take every click, however many are in the call.
use gpui_component::Disableable as _;

use super::pages::{self, PAGER_HEIGHT, PAGER_WIDTH, Page, Strip, TILE_GAP};
use super::*;

/// How the stage lays its tiles out: View's Gallery or Speaker, or (while a share shows) Focus on content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Gallery,
    Speaker,
    /// The share alone, the people's strip put away (laid out as the gallery while nothing is shared).
    Focus,
}

impl Layout {
    /// The trace's (and the test selectors') name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::Speaker => "speaker",
            Self::Focus => "focus",
        }
    }
}

/// The height of the speaker view's strip of the others, px.
const STRIP_HEIGHT: f32 = 112.0;
/// The widest a tile in the speaker view's strip gets, px.
const STRIP_TILE: f32 = 184.0;
/// The width of the people's strip beside a share, and the tallest a tile in it gets, px (Teams' 16:9 tiles).
const SHARE_STRIP: (f32, f32) = (236.0, 132.0);

impl CallStage {
    /// The stage of the call up: who rings while nobody is in it, else the tiles as View lays them out.
    pub(super) fn stage(
        &mut self,
        active: &ActiveCall,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // waiting in the lobby, the account sees nobody: only that someone should let it in
        // waiting in the lobby, holding the call or held by the far end, the account sees nobody's tile
        let tiles = if active.status == CallStatus::Lobby || active.held.is_some() || active.held_by_far_end {
            Vec::new()
        } else {
            self.tiles_of(active)
        };
        let call = active.id.get();
        let faces = &self.faces;
        let lobby = crate::call_lobby::card(active, |mri| faces.picture(mri).cloned(), palette, cx);
        // the far end's share fills the stage, the people beside it (the account's own tile among them)
        let shared = active.remote_share && active.held.is_none() && active.status != CallStatus::Lobby;
        let area = self.paging.area_or(self.stage_estimate());
        let in_gallery = !tiles.is_empty() && self.self_in_gallery(active);
        let focus = self.focus_of(&tiles).filter(|_| tiles.len() > 1);
        let in_strip = !shared && focus.is_some() && !self.self_hidden && !floats_beside_strip(area.0);
        let body = if shared {
            self.share_stage(active, tiles, (title, area), palette, cx)
        } else if tiles.is_empty() {
            self.nobody_shown(active, title, palette, cx)
        } else if let Some(index) = focus {
            self.speaker_stage(active, (tiles, index), (area.0, in_strip), palette, cx)
        } else {
            self.gallery_stage(active, tiles, (area, in_gallery), palette, cx)
        };
        let own = if self.self_hidden || shared || in_gallery || in_strip {
            None
        } else {
            let face = self.own_face(call, palette, cx);
            Some(self_view(active, face, true, palette, cx))
        };
        div()
            .debug_selector(|| "call-stage".to_owned())
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            // whatever the stage holds stays on it, clear of the controls above
            .overflow_hidden()
            .bg(rgb(palette.background))
            .child(div().relative().size_full().child(self.paging.measure(cx)).child(body))
            .children(own)
            .children(self.reactions_shown(active, palette, cx))
            .children(lobby)
            .into_any_element()
    }

    /// The stage while nobody's tile shows: who the call is with and its state (on hold, or held by the far end), the
    /// rings spreading from them while it rings or waits.
    fn nobody_shown(
        &self,
        active: &ActiveCall,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let waiting = active.kind == CallKind::Meeting && active.status == CallStatus::Connected;
        if let Some(since) = active.held {
            // Teams' held call: who it is with, "On hold" and how long, no rings
            let lasted = calls::duration_text(since.elapsed().as_secs());
            calling(
                (title, self.partner_picture(Some(active.id))),
                &format!("On hold {lasted}"),
                palette,
                Vec::new(),
            )
        } else if active.held_by_far_end {
            // Teams' held party: who holds it over "On hold", no duration, no rings
            div()
                .debug_selector(|| "call-held-by-far-end".to_owned())
                .size_full()
                .child(calling(
                    (title, self.partner_picture(Some(active.id))),
                    "On hold",
                    palette,
                    Vec::new(),
                ))
                .into_any_element()
        } else {
            let state = if waiting {
                "Waiting for others to join".to_owned()
            } else {
                calls::status_text(active)
            };
            let rings = crate::motion::ripple("call-calling", palette.accent, BIG_AVATAR_PX, cx);
            calling((title, self.partner_picture(Some(active.id))), &state, palette, rings)
        }
    }

    /// The far end's share large, its sharer named at its bottom left (its picture fitted in, or that it is coming),
    /// and the people's tiles in a strip on its right with the account's own last (unless View hid it); Focus on
    /// content shows the share alone.
    fn share_stage(
        &mut self,
        active: &ActiveCall,
        tiles: Vec<StageTile>,
        (title, area): (&SharedString, (f32, f32)),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let call = active.id.get();
        // who the roster says shares (a group call's), else the far end
        let sharing = active
            .view
            .participants
            .iter()
            .find(|p| p.sharing)
            .map(|p| p.mri.as_str());
        let sharer = tiles
            .iter()
            .find(|tile| sharing.is_some() && tile.mri.as_deref() == sharing)
            .or(tiles.first())
            .map_or_else(|| title.clone(), |tile| tile.name.clone());
        let picture = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Share, call));
        let content = share_content(picture, sharer, palette);
        if self.layout == Layout::Focus {
            return div()
                .debug_selector(|| "call-share-focus".to_owned())
                .size_full()
                .child(content)
                .into_any_element();
        }
        // the account's own tile ends every page of the strip
        let own = usize::from(!self.self_hidden);
        let (page, tile_height) =
            self.strip_page(Strip::Share, (tiles.len(), own), (area.1, SHARE_STRIP.1), PAGER_HEIGHT);
        let mut strip: Vec<AnyElement> = self.page_tiles(tiles, page, (call, false), palette, cx);
        if own == 1 {
            let face = self.own_face(call, palette, cx);
            strip.push(self_view(active, face, false, palette, cx));
        }
        div()
            .debug_selector(|| "call-share-stage".to_owned())
            .size_full()
            .flex()
            .gap_2()
            .child(div().flex_1().min_w_0().h_full().child(content))
            .child(
                div()
                    .debug_selector(|| "call-share-strip".to_owned())
                    .w(px(SHARE_STRIP.0))
                    .flex_none()
                    .h_full()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_2()
                    .children(
                        strip
                            .into_iter()
                            .map(|tile| div().w_full().h(px(tile_height)).flex_none().child(tile)),
                    )
                    .children(pager("call-strip", page, palette, cx)),
            )
            .into_any_element()
    }

    /// The gallery: a page of equal 16:9 tiles, as many as View's Max gallery size and `area` (the stage's width and
    /// height, px) allow, the account's own last on each page while it is `in_gallery`; the pager under them while
    /// there is more than one page.
    fn gallery_stage(
        &mut self,
        active: &ActiveCall,
        tiles: Vec<StageTile>,
        ((width, height), in_gallery): ((f32, f32), bool),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let call = active.id.get();
        let selector = if active.view.participants.is_empty() {
            "call-people"
        } else {
            "call-grid"
        };
        let own = usize::from(in_gallery);
        let fits = |height: f32| {
            pages::page_size(self.paging.size, (width, height))
                .saturating_sub(own)
                .max(1)
        };
        let all = fits(height);
        let per = if tiles.len() > all {
            fits(height - PAGER_HEIGHT)
        } else {
            all
        };
        let page = pages::page_of(tiles.len(), per, self.paging.gallery);
        self.paging.gallery = page.index;
        let menus = tiles.len() > 1;
        let mut drawn = self.page_tiles(tiles, page, (call, menus), palette, cx);
        if in_gallery {
            let face = self.own_face(call, palette, cx);
            drawn.push(self_view(active, face, false, palette, cx));
        }
        let grid_height = if page.pages > 1 { height - PAGER_HEIGHT } else { height };
        let (columns, tile) = pages::arrange(drawn.len(), (width, grid_height));
        div()
            .debug_selector(move || selector.to_owned())
            .size_full()
            .flex()
            .flex_col()
            .child(grid(drawn, columns, tile))
            .children(pager("call-gallery", page, palette, cx))
            .into_any_element()
    }

    /// The speaker view: the tile at `main` large over a paged strip of the others.
    fn speaker_stage(
        &mut self,
        active: &ActiveCall,
        (mut tiles, main): (Vec<StageTile>, usize),
        (width, in_strip): (f32, bool),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let call = active.id.get();
        let selector = if active.view.participants.is_empty() {
            "call-people"
        } else {
            "call-grid"
        };
        let floating = !self.self_hidden && !in_strip;
        let large = tiles.remove(main);
        let pinned = large.mri.is_some() && large.mri == self.pinned;
        let face = self.face_of(&large, call, palette, cx);
        let large = tile_view(large, face, (pinned, true), palette, cx);
        let room = if floating { width - SELF_VIEW.0 - 16.0 } else { width };
        let own = usize::from(in_strip);
        let (page, tile_width) = self.strip_page(Strip::Speaker, (tiles.len(), own), (room, STRIP_TILE), PAGER_WIDTH);
        let mut others = self.page_tiles(tiles, page, (call, true), palette, cx);
        if in_strip {
            let face = self.own_face(call, palette, cx);
            others.push(self_view(active, face, false, palette, cx));
        }
        speaker(
            selector,
            large,
            (others, tile_width),
            (floating, pager("call-strip", page, palette, cx)),
        )
    }

    /// The page of `strip` that shows and its tiles' length; `own` (the account's tile) is added to every page.
    fn strip_page(
        &mut self,
        strip: Strip,
        (count, own): (usize, usize),
        (length, tile): (f32, f32),
        pager: f32,
    ) -> (Page, f32) {
        // a strip other than the one last paged starts on its first page, as Teams' does
        if self.paging.strip_of.replace(strip) != Some(strip) {
            self.paging.strip = 0;
        }
        let fits = |length: f32| pages::strip_size(length, tile).saturating_sub(own).max(1);
        let all = fits(length);
        let per = if count > all {
            fits(length - pager - TILE_GAP)
        } else {
            all
        };
        let page = pages::page_of(count, per, self.paging.strip);
        self.paging.strip = page.index;
        let shown = count.saturating_sub(page.first).min(per) + own;
        let pager = if page.pages > 1 { pager + TILE_GAP } else { 0.0 };
        (page, pages::strip_tile(length, tile, shown, pager))
    }

    /// The tiles of `page`, drawn: their faces, each pinned or not, each with its menu while `menus`.
    fn page_tiles(
        &mut self,
        tiles: Vec<StageTile>,
        page: Page,
        (call, menus): (u64, bool),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        tiles
            .into_iter()
            .skip(page.first)
            .take(page.per)
            .map(|tile| {
                let face = self.face_of(&tile, call, palette, cx);
                let pinned = tile.mri.is_some() && tile.mri == self.pinned;
                tile_view(tile, face, (pinned, menus), palette, cx)
            })
            .collect()
    }

    /// Whether the account's own tile is one of the gallery's, as Teams has had it since its new gallery: in a group
    /// call's gallery (not a 1:1 call's, where it floats), unless View hid it.
    fn self_in_gallery(&self, active: &ActiveCall) -> bool {
        !self.self_hidden
            && active.view.participants.len() > 1
            && self.layout != Layout::Speaker
            && self.pinned.is_none()
    }

    /// The stage's size before it was first laid out: the window's, less the header and the sheet's gaps.
    fn stage_estimate(&self) -> (f32, f32) {
        let (width, height) = self.paging.window;
        (width - 48.0, height - HEADER_HEIGHT - 48.0)
    }

    /// What `tile` shows: its video when a line shows it (and View didn't turn the others' video off), else its
    /// person's face.
    fn face_of(&mut self, tile: &StageTile, call: u64, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let video = tile
            .video
            .filter(|_| !self.others_off)
            .and_then(|line| self.tiles.as_mut()?.fill(line, call));
        video.unwrap_or_else(|| self.tile_face(&tile.selector, (tile.mri.as_deref(), Some(&tile.name)), palette, cx))
    }

    /// What the account's own tile shows: its self view while the camera sends, else its face.
    fn own_face(&mut self, call: u64, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let video = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Own, call));
        video.unwrap_or_else(|| {
            let me = self.core.me().to_owned();
            self.tile_face(SELF_TILE, (Some(&me), self.own_name.as_ref()), palette, cx)
        })
    }

    /// Which tile the stage shows large, if any: the one pinned; in the speaker view the one who speaks, else who
    /// spoke last, else the first.
    fn focus_of(&self, tiles: &[StageTile]) -> Option<usize> {
        let at = |mri: &Option<String>| {
            mri.as_ref()
                .and_then(|mri| tiles.iter().position(|tile| tile.mri.as_ref() == Some(mri)))
        };
        if let Some(index) = at(&self.pinned) {
            return Some(index);
        }
        if self.layout != Layout::Speaker {
            return None;
        }
        tiles
            .iter()
            .position(|tile| tile.speaking)
            .or_else(|| at(&self.last_speaker))
            .or(Some(0))
    }

    /// The stage's tiles: the roster's participants once it told who is in the call (the grid, traced when it
    /// changed); before that, once the media connected, the people the call is with (a 1:1 call's far end on line
    /// 0); none while it rings.
    pub(super) fn tiles_of(&mut self, active: &ActiveCall) -> Vec<StageTile> {
        let view = &active.view;
        if !view.participants.is_empty() {
            let people = self.people.of(active.id);
            let mut tiles: Vec<StageTile> = view
                .participants
                .iter()
                .enumerate()
                .map(|(index, participant)| StageTile {
                    selector: format!("call-participant-{index}"),
                    mri: Some(participant.mri.clone()),
                    name: participant_name(participant, people),
                    speaking: participant.speaking,
                    muted: participant.muted,
                    hand: participant.hand_raised,
                    video_on: participant.video_on,
                    video: participant.line.map(Tile::Line),
                })
                .collect();
            if let Some(speaking) = tiles.iter().find(|tile| tile.speaking) {
                self.last_speaker.clone_from(&speaking.mri);
            }
            if self.videos_first {
                // stable: the cameras on lead, each side in the roster's order
                tiles.sort_by_key(|tile| !tile.video_on);
            }
            self.trace_grid(active.id, view);
            return tiles;
        }
        if !matches!(active.status, CallStatus::Connected | CallStatus::Reconnecting) {
            return Vec::new();
        }
        let people = self.people.of(active.id);
        let one = people.len() == 1;
        people
            .iter()
            .enumerate()
            .map(|(index, (mri, name))| StageTile {
                selector: format!("call-person-{index}"),
                mri: Some(mri.clone()),
                name: name.clone(),
                speaking: false,
                muted: false,
                hand: false,
                video_on: false,
                video: one.then_some(Tile::REMOTE),
            })
            .collect()
    }

    /// Pins the participant `mri` for the account alone (Teams' Pin for me), or unpins whoever is pinned.
    pub fn pin(&mut self, mri: Option<String>, cx: &mut Context<Self>) {
        self.pinned = mri;
        self.trace.record(
            &TraceLine::new("call-window", "pinned")
                .call(self.call.get())
                .kind(if self.pinned.is_some() { "pinned" } else { "unpinned" }),
        );
        cx.notify();
    }

    /// Lays the stage out as View's `layout`.
    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        if self.layout != layout {
            self.layout = layout;
            self.trace.record(
                &TraceLine::new("call-window", "layout")
                    .call(self.call.get())
                    .kind(layout.name()),
            );
        }
        cx.notify();
    }

    /// The stage's layout, as View chose it.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Hides the self view, or shows it again (View's Hide for me).
    pub fn hide_self_view(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.self_hidden = hidden;
        cx.notify();
    }

    /// Shows the next page of the gallery or of a strip (`step` 1), or the one before (-1).
    pub fn turn_page(&mut self, pager: &str, step: isize, cx: &mut Context<Self>) {
        let page = if pager == "call-gallery" {
            &mut self.paging.gallery
        } else {
            &mut self.paging.strip
        };
        *page = page.saturating_add_signed(step);
        cx.notify();
    }

    /// Shows at most `size` tiles to a page of the gallery (View's Max gallery size), the first page again.
    pub fn set_gallery_size(&mut self, size: usize, cx: &mut Context<Self>) {
        self.paging.size = size.max(1);
        self.paging.gallery = 0;
        cx.notify();
    }

    /// Leads with the tiles whose camera is on, or keeps the roster's order (View's Show videos first).
    pub fn show_videos_first(&mut self, first: bool, cx: &mut Context<Self>) {
        self.videos_first = first;
        cx.notify();
    }

    /// Shows the others' pictures (or initials) in place of their video, or their video again (View's Turn off other videos), as
    /// Teams does to spare a slow connection or a busy screen. Only what this window draws changes.
    pub fn turn_off_other_videos(&mut self, off: bool, cx: &mut Context<Self>) {
        self.others_off = off;
        self.trace.record(
            &TraceLine::new("call-window", "other-videos")
                .call(self.call.get())
                .kind(if off { "off" } else { "on" }),
        );
        cx.notify();
    }

    /// Who has a hand up, kept in the order they raised it: a hand lowered leaves the line, one raised joins its end.
    pub(super) fn order_hands(&mut self, active: &ActiveCall) {
        let me = self.core.me().to_owned();
        let up: Vec<&str> = active
            .view
            .participants
            .iter()
            .filter(|participant| participant.hand_raised)
            .map(|participant| participant.mri.as_str())
            .chain(active.hand_raised.then_some(me.as_str()))
            .collect();
        self.hands.retain(|mri| up.contains(&mri.as_str()));
        for mri in up {
            if !self.hands.iter().any(|raised| raised == mri) {
                self.hands.push(mri.to_owned());
            }
        }
    }

    /// Where `mri`'s hand stands in the line of hands up, from 1; none while it is down.
    pub(super) fn hand_rank(&self, mri: &str) -> Option<usize> {
        self.hands
            .iter()
            .position(|raised| raised == mri)
            .map(|index| index + 1)
    }
}

/// The tiles in rows of `columns`, each `tile` (width, height, px), the rows and a short last row centred: Teams'
/// gallery.
fn grid(tiles: Vec<AnyElement>, columns: usize, (width, height): (f32, f32)) -> AnyElement {
    let mut rows: Vec<Vec<AnyElement>> = Vec::new();
    for tile in tiles {
        match rows.last_mut() {
            Some(row) if row.len() < columns => row.push(tile),
            _ => rows.push(vec![tile]),
        }
    }
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(TILE_GAP))
        .children(rows.into_iter().map(|row| {
            div().w_full().flex().justify_center().gap(px(TILE_GAP)).children(
                row.into_iter()
                    .map(|tile| div().w(px(width)).h(px(height)).flex_none().child(tile)),
            )
        }))
        .into_any_element()
}

/// Floating, the self view must leave the strip room for a tile and the pager.
fn floats_beside_strip(width: f32) -> bool {
    width - SELF_VIEW.0 - 16.0 >= pages::MIN_STRIP_TILE + TILE_GAP + PAGER_WIDTH
}

/// The speaker view: `main` large over the strip of the others.
fn speaker(
    selector: &'static str,
    main: AnyElement,
    (others, tile_width): (Vec<AnyElement>, f32),
    (floating, pager): (bool, Option<AnyElement>),
) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .size_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .debug_selector(|| "call-speaker-main".to_owned())
                .flex_1()
                .min_h_0()
                .w_full()
                .p_1()
                .child(main),
        )
        .child(
            div()
                .debug_selector(|| "call-speaker-strip".to_owned())
                .h(px(STRIP_HEIGHT))
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .overflow_hidden()
                .when(floating, |strip| strip.pr(px(SELF_VIEW.0 + 16.0)))
                .children(
                    others
                        .into_iter()
                        .map(|tile| div().w(px(tile_width)).flex_none().h_full().child(tile)),
                )
                .children(pager),
        )
        .into_any_element()
}

/// The `<` `>` that turn `page` of a gallery or a strip (`prefix`: `call-gallery`, `call-strip`), with which page
/// shows; none while everything fits on one.
fn pager(prefix: &'static str, page: Page, palette: Palette, cx: &mut Context<CallStage>) -> Option<AnyElement> {
    if page.pages < 2 {
        return None;
    }
    let stage = cx.weak_entity();
    let turn = |step: isize| {
        let stage = stage.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = stage.update(cx, |stage, cx| stage.turn_page(prefix, step, cx));
        }
    };
    let previous = format!("{prefix}-previous");
    let next = format!("{prefix}-next");
    Some(
        div()
            .debug_selector(move || format!("{prefix}-pager"))
            .h(px(PAGER_HEIGHT))
            .min_w(px(PAGER_WIDTH))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(selected_by(
                previous.clone(),
                icon_button(SharedString::from(previous), Glyph::CHEVRON_LEFT, "Previous page")
                    .disabled(page.index == 0)
                    .on_click(turn(-1)),
            ))
            .child(
                div()
                    .debug_selector(move || format!("{prefix}-page"))
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(format!("{} / {}", page.index + 1, page.pages)),
            )
            .child(selected_by(
                next.clone(),
                icon_button(SharedString::from(next), Glyph::CHEVRON_RIGHT, "Next page")
                    .disabled(page.index + 1 >= page.pages)
                    .on_click(turn(1)),
            ))
            .into_any_element(),
    )
}

/// One tile: `face` (its video when a line shows it, else its person's picture or initials); its name with its microphone muted and a raised
/// hand; a border in the brand colour while it speaks (Teams' gold while its hand is up); its menu on hover when the
/// stage has more than one (`menus`), its pin while `pinned`.
fn tile_view(
    tile: StageTile,
    face: AnyElement,
    (pinned, menus): (bool, bool),
    palette: Palette,
    cx: &mut Context<CallStage>,
) -> AnyElement {
    let StageTile {
        selector,
        mri,
        name,
        speaking,
        muted,
        hand,
        ..
    } = tile;
    let glow_id = SharedString::from(format!("{selector}-speaking"));
    let group = SharedString::from(format!("{selector}-tile"));
    let menu = mri
        .filter(|_| menus)
        .map(|mri| tile_menu(&selector, mri, pinned, &group, palette, cx));
    let border = if hand {
        HAND_COLOUR
    } else if speaking {
        palette.brand
    } else {
        palette.rail
    };
    let tile = div()
        .debug_selector(move || selector.clone())
        .group(group)
        .relative()
        .size_full()
        .rounded_lg()
        .overflow_hidden()
        // a dark tile with a hint of the brand colour from its top
        .bg(theme::gradient(
            theme::mix(palette.rail, palette.brand, 0.2),
            palette.rail,
            160.0,
        ))
        .border_2()
        .border_color(rgb(border))
        .child(face)
        .child(name_tag(name, (speaking, muted, hand, pinned), palette))
        .children(menu);
    // while its person speaks, a glow in the brand colour swells and settles round it
    crate::motion::breathe(tile, glow_id, speaking, palette, cx)
}

/// Teams' raised hand's gold, px colour.
pub(super) const HAND_COLOUR: u32 = 0x00f2_c94c;

/// A tile's "More options" at its top right, shown while the pointer is over the tile: Pin for me, or Unpin.
fn tile_menu(
    selector: &str,
    mri: String,
    pinned: bool,
    group: &SharedString,
    palette: Palette,
    cx: &mut Context<CallStage>,
) -> AnyElement {
    let id = SharedString::from(format!("{selector}-more"));
    let stage = cx.weak_entity();
    let item = SharedString::from(format!("{selector}-pin"));
    let group = group.clone();
    div()
        .absolute()
        .top_2()
        .right_2()
        .invisible()
        .group_hover(group, gpui::Styled::visible)
        .child(selected_by(
            id.to_string(),
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Glyph::MORE.icon())
                .bg(rgb(palette.surface))
                .tooltip("More options")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                    let (stage, mri, item) = (stage.clone(), mri.clone(), item.clone());
                    let (glyph, label) = if pinned {
                        (Glyph::PIN_OFF, "Unpin")
                    } else {
                        (Glyph::PIN, "Pin for me")
                    };
                    menu.min_w(px(180.0)).item(
                        PopupMenuItem::element(move |_, _| {
                            let item = item.clone();
                            div()
                                .debug_selector(move || item.to_string())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(glyph.icon().size(px(14.0)))
                                .child(label)
                        })
                        .on_click(move |_, _, cx| {
                            let mri = (!pinned).then(|| mri.clone());
                            let _ = stage.update(cx, |stage, cx| stage.pin(mri, cx));
                        }),
                    )
                }),
        ))
        .into_any_element()
}

/// A tile's name at its bottom left: a pin while pinned, a raised hand, its name, a speaker while it speaks and its
/// microphone while muted.
fn name_tag(
    name: SharedString,
    (speaking, muted, hand, pinned): (bool, bool, bool, bool),
    palette: Palette,
) -> gpui::Div {
    div()
        .absolute()
        .bottom_2()
        .left_2()
        .max_w(relative(0.9))
        .px_2()
        .py_0p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .bg(rgb(palette.surface))
        .text_xs()
        .when(pinned, |tag| tag.child(Glyph::PIN.icon().size(px(12.0))))
        .when(hand, |tag| {
            tag.child(Glyph::HAND.icon().size(px(12.0)).text_color(rgb(HAND_COLOUR)))
        })
        .child(div().truncate().child(name))
        .when(speaking && !muted, |tag| {
            tag.child(Glyph::VOLUME.icon().size(px(12.0)).text_color(rgb(palette.brand)))
        })
        .when(muted, |tag| {
            tag.child(
                Glyph::MIC_OFF
                    .icon()
                    .size(px(12.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
        })
}

/// The share as the stage shows it: its picture fitted in (on the stage's own ground, as Teams letterboxes it), or
/// while its first frame is coming the sharer's screen's mark and who shares; the sharer named at its bottom left.
fn share_content(picture: Option<AnyElement>, sharer: SharedString, palette: Palette) -> AnyElement {
    let face = picture.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::MONITOR.icon().size(px(40.0)))
            .child(div().text_sm().child(format!("{sharer} is sharing")))
            .into_any_element()
    });
    div()
        .debug_selector(|| "call-share".to_owned())
        .relative()
        .size_full()
        .overflow_hidden()
        .child(face)
        .child(name_tag(sharer, (false, false, false, false), palette))
        .into_any_element()
}

/// The account's own tile's selector.
const SELF_TILE: &str = "call-participant-self";

/// The account's own tile: floating at the stage's bottom right (`floating`), else filling its place in a strip;
/// `face` (its self view while the camera sends, else its picture or initials, as Teams shows them); a border while it speaks; its microphone while muted and its hand
/// while raised.
fn self_view(active: &ActiveCall, face: AnyElement, floating: bool, palette: Palette, cx: &App) -> AnyElement {
    let speaking = active.view.speaking && !active.muted;
    let border = if active.hand_raised {
        HAND_COLOUR
    } else if speaking {
        palette.brand
    } else {
        palette.border
    };
    let own = div()
        .debug_selector(|| SELF_TILE.to_owned())
        .when(floating, |own| {
            own.absolute()
                .bottom_5()
                .right_5()
                .w(px(SELF_VIEW.0))
                .h(px(SELF_VIEW.1))
        })
        .when(!floating, gpui::Styled::size_full)
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(palette.surface))
        .border_2()
        .border_color(rgb(border))
        .when(!speaking && floating, |own| {
            own.shadow(theme::glow(palette, 0x0000_0000, 0.3, 18.0, 6.0))
        })
        .child(face)
        .child(name_tag(
            SharedString::from("You"),
            (speaking, active.muted, active.hand_raised, false),
            palette,
        ));
    crate::motion::breathe(own, "call-self-speaking", speaking, palette, cx)
}

impl CallStage {
    /// The live reactions sent lately, floating up from the stage's bottom left as Teams floats them: each its picture
    /// and who sent it ("You" for the account's own), gone after [`calls::REACTION_SHOWN`].
    pub(super) fn reactions_shown(&self, active: &ActiveCall, palette: Palette, cx: &App) -> Option<AnyElement> {
        let now = std::time::Instant::now();
        let shown: Vec<&calls::LiveReaction> = active
            .reactions
            .iter()
            .filter(|reaction| now.duration_since(reaction.at) < calls::REACTION_SHOWN)
            .collect();
        if shown.is_empty() {
            return None;
        }
        let me = self.core.me().to_owned();
        let people = self.people.of(active.id);
        let floats: Vec<AnyElement> = shown
            .into_iter()
            .enumerate()
            .map(|(index, reaction)| {
                let key = toolbar::REACTIONS
                    .iter()
                    .find(|(name, _, _)| *name == reaction.reaction)
                    .map_or(reaction.reaction.as_str(), |(_, key, _)| key);
                let who = reaction_sender(reaction, &me, people);
                let float = div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_0p5()
                    .child(crate::reactions::emoji(key, px(40.0)))
                    .child(
                        div()
                            .max_w(px(120.0))
                            .px_1p5()
                            .rounded_md()
                            .bg(rgb(palette.surface))
                            .text_xs()
                            .truncate()
                            .child(who),
                    );
                // each one rises into place as it comes, as Teams' float up
                let id = SharedString::from(format!("call-reaction-{index}-{}", reaction.reaction));
                crate::motion::rise(float, id, 24.0, cx)
            })
            .collect();
        Some(
            div()
                .debug_selector(|| "call-reactions".to_owned())
                .absolute()
                .bottom(px(56.0))
                .left_5()
                .flex()
                .items_end()
                .gap_2()
                .children(floats)
                .into_any_element(),
        )
    }

    /// While a live reaction shows, the stage is drawn again as the next one is due to go.
    pub(super) fn float_reactions(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let Some(due) = active
            .reactions
            .iter()
            .map(|reaction| (reaction.at + calls::REACTION_SHOWN).saturating_duration_since(now))
            .filter(|due| !due.is_zero())
            .min()
        else {
            self.reacting = None;
            return;
        };
        self.reacting = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(due).await;
            // a stage that is gone has nothing left to draw
            let _ = this.update(cx, |this, cx| {
                if let Some(active) = this.live(cx) {
                    this.float_reactions(&active, cx);
                }
                cx.notify();
            });
        }));
    }
}

/// Who sent `reaction`: "You" for the account's own (`me`), else the name it came with, the call's people's name for
/// them, or "Someone".
fn reaction_sender(reaction: &calls::LiveReaction, me: &str, people: &[(String, SharedString)]) -> SharedString {
    if reaction.from.as_str() == me {
        return SharedString::from("You");
    }
    reaction
        .name
        .clone()
        .map(SharedString::from)
        .or_else(|| {
            people
                .iter()
                .find(|(mri, _)| mri == reaction.from.as_str())
                .map(|(_, name)| name.clone())
        })
        .unwrap_or_else(|| SharedString::from(SOMEONE))
}

/// While the call reconnects, a bar over the stage saying so, as Teams says it; nothing while its connection is fine.
pub(super) fn connection_banner(active: &ActiveCall, palette: Palette) -> Option<gpui::Div> {
    (active.status == CallStatus::Reconnecting).then(|| {
        div()
            .debug_selector(|| "call-reconnecting".to_owned())
            .flex_none()
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .bg(rgb(palette.surface))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(Glyph::WIFI.icon().size(px(14.0)).text_color(rgb(theme::DANGER)))
            .child("Reconnecting… Your call will continue once your connection is back.")
    })
}
