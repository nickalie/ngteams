//! A meeting chat's Recap tab, as Teams draws it (`crate::recap` works out what it shows):
//!
//! - **The top row**: the meeting's line (its day and span) in a box with its menu, "Part 1", "Part 2" when the
//!   meeting was recorded more than once, then Share (its menu: Copy link, the recap's Teams link), Watch in browser
//!   (the recording in Stream, for a part with a recording) and "More options" (Download, for a recording the account
//!   may download, then Open in Teams on the web, Copy link, Refresh).
//! - **Download** saves the shown part's recording where the user chooses (the downloads folder without a chooser),
//!   named as its item names it: offered only when its item says the account may download it (Teams' Stream page,
//!   which Watch in browser opens, offers its Download only then; an attendee whose download the meeting's policy
//!   blocks is offered none). It runs apart from the view ([`crate::recording_download`]): a row under the top row
//!   shows how far it got with Cancel, then Show in folder, or what failed with Try again.
//! - **A recorded part**: the recording played here ([`VideoPlayer`], filling its column at 16:9); beside it the files
//!   shared in the chat and the panes Teams' Recap has there: AI summary and Transcript. A part with only a transcript
//!   shows the files and the panes across the whole width. Teams' Copilot (its AI summary, speaker insights and their
//!   upsells) is left out: the AI summary is made on this computer instead ([`crate::recap_summary`]),
//!   when the user asks for one, laid out as Fathom's call summaries are, its moments played from a click, copied, or
//!   sent to the meeting's chat after a preview.
//! - **The transcript**: Teams' note that it is AI-generated, who started transcription, each speaker's turn (their
//!   initials, name and time into the recording) with its paragraphs; the paragraph being spoken is highlighted and
//!   kept in view while the recording plays, and a click on one plays the recording from there. Search finds the
//!   paragraphs that hold its words ("1/3", Enter or the arrows go on, the match highlighted); Download saves the
//!   `WebVTT` as Teams' "Download as .vtt" does. Its cues are also the recording's captions (the player's CC menu,
//!   named by the transcript's language as its item's transcripts give it), synced to where it plays.
//! - **Calls' captions saved on this computer** ([`crate::call_transcript`]): each saved transcript of the chat's calls
//!   is a part of its own, marked "Saved on this computer", its transcript and AI summary as a Teams transcript's
//!   (`local`), with Delete transcript in More options. Teams' own transcript of the same time wins.
//! - **A 1:1 or group chat's calls**: their cloud recordings are listed by the same read as a meeting's (the chat's id
//!   for the meeting's), each a part played, downloaded and summarized as a meeting's; Teams lists their transcripts
//!   with no address to read (`docs/protocol/recap.md`), so such a part's Transcript says there is none, as Teams'
//!   does, and the chat's saved captions stay parts of their own. A call transcribed and not recorded isn't a session;
//!   its card shows it ([`transcribed`]).
//! - **Its states**: a spinner while the recap or a transcript is read; one that can't be read says so and offers to try
//!   again, or Teams on the web; a meeting that left nothing says so.
//!
//! The engine reads everything ([`Core::meeting_contents`], [`Core::meeting_transcript`],
//! [`Core::transcript_language`], [`Core::recording_file`], [`Core::file_stream`], the chat's stored messages for its
//! shared files); no view builds a request.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EntityId, FocusHandle, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use teams_media::player::State;
use teams_proto::RecordingAccess;
use teams_proto::chat::SharedFile;
use teams_proto::recap::{RecapPart, recap_link, recap_parts};
use teams_proto::transcript::{Cue, Turn, read_vtt};

mod local;
mod summary;
mod transcribed;

pub use local::{DELETE_ASK, DELETED, SAVED_NOTICE};
pub use summary::{COPIED, EMPTY_TRANSCRIPT, NO_TEAMS_TRANSCRIPT, NO_TRANSCRIPT, SENT, SUMMARY_FAILED};
pub use transcribed::TRANSCRIBED_ONLY;

use crate::bridge::Core;
use crate::clock;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::PictureSource;
use crate::keyboard;
use crate::people;
use crate::recap::{
    AI_NOTICE, NO_FILES, Search, SharedItem, Transcript, listed, local_line_of, part_label, recording_file_name,
    row_of, sessions, transcript_file_name,
};
use crate::recap_summary::RecapSummary;
use crate::recording_download::{self, Failure, Progress, RecordingDownload, byte_size, download_of};
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};
use crate::video_player::{Captions, Recording, VideoPlayer, clock as offset};

/// How far a turn's paragraphs stand in from its speaker's initials: past them, under the name.
const TURN_INDENT: f32 = 36.0;

/// The words a recap that couldn't be read shows.
pub const RECAP_UNREAD: &str = "The recap couldn't be loaded.";
/// The words a meeting that left nothing shows.
pub const NOTHING_LEFT: &str = "This meeting wasn't recorded or transcribed.";
/// The words a recap shows while a recording of the meeting is still being saved.
pub const SAVING: &str = "Saving recording\u{2026} It shows here once it's ready.";
/// The words a transcript that couldn't be read shows.
pub const TRANSCRIPT_UNREAD: &str = "The transcript couldn't be loaded.";
/// What the Transcript pane of a recording without a transcript says, as Teams' Recap says it.
pub const NO_TRANSCRIPT_YET: &str = "No transcript available";
/// Under [`NO_TRANSCRIPT_YET`].
pub const NO_TRANSCRIPT_WHY: &str =
    "The transcript for this meeting was never created, has expired, or has been deleted.";
/// The widest the session's box (and its menu) grows, as Teams' grows to about this width before it ellipsizes.
pub const SESSION_WIDTH: f32 = 392.0;

/// The panes beside the recording, as Teams' Recap has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    AiSummary,
    Transcript,
}

/// Where sending the summary to the chat stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Post {
    /// The message shown, waiting for Send.
    Preview,
    Sending,
    /// It wasn't sent: why.
    Failed(String),
}

/// What the recap read.
enum Contents {
    Reading,
    Failed,
    Read(Vec<RecapPart>),
}

/// A recording's file as read: whether the account may download it.
enum FileRead {
    Reading,
    Failed,
    Read(RecordingAccess),
}

/// A transcript as read.
enum Read {
    Reading,
    Failed,
    Shown {
        vtt: Arc<str>,
        transcript: Transcript,
        /// Its cues: the recording's captions.
        cues: Arc<[Cue]>,
    },
}

/// A meeting chat's Recap tab (see the module docs).
pub struct RecapView {
    core: Arc<dyn Core>,
    conversation: String,
    title: SharedString,
    contents: Contents,
    selected: usize,
    transcripts: HashMap<String, Read>,
    /// Each transcript's language, by its address, once read (`None`: its item names none, or it couldn't be read).
    languages: HashMap<String, Option<String>>,
    /// Each recording's player, by its sharing link (a part's place can change when the recap is read again).
    players: HashMap<String, (Entity<VideoPlayer>, Subscription)>,
    /// Each recording's file, by its sharing link, once asked for: whether the account may download it.
    recording_files: HashMap<String, FileRead>,
    /// The downloads of the recap's recordings watched, so the view follows them.
    watched: HashMap<EntityId, Subscription>,
    /// The paragraph being spoken, as last kept in view.
    spoken: Option<(usize, usize)>,
    /// The recording asked for ("View recap" on its card) before the parts were read.
    wanted: Option<String>,
    /// A call transcribed alone, as its card's View recap asked for it ([`transcribed`]).
    transcribed: transcribed::Transcribed,
    /// The recap card's thumbnail asked for the shown recording to play before the parts were read.
    play_wanted: bool,
    /// The recap card's Transcript chip asked for the Transcript pane before the parts were read.
    transcript_wanted: bool,
    search_box: Entity<InputState>,
    search: Option<Search>,
    /// What was searched for: searched again once the transcript it was asked of arrives.
    query: String,
    files: Vec<SharedItem>,
    /// A recording of the meeting is still being saved (its chat says so; MCPS doesn't list it yet).
    saving: bool,
    /// Who the transcript says started it: the meeting's organizer's name.
    organizer: Option<String>,
    scroll: ScrollHandle,
    note: Option<SharedString>,
    focus: FocusHandle,
    _typed: Subscription,
    /// The AI switch: the pane shown follows it (a summary being made stops by itself).
    _ai_switch: Subscription,
    reading: Option<Task<()>>,
    /// The app's settings: the tool summaries are made with, and where they are kept.
    settings: Option<Entity<SettingsModel>>,
    /// The pane the user picked; `None`: the one [`RecapView::pane`] works out.
    pane: Option<Pane>,
    /// Each part's summary followed, by its key ([`crate::recap_summary::cache_key`]).
    summaries: HashMap<String, (Entity<RecapSummary>, Subscription)>,
    summary_scroll: ScrollHandle,
    /// The focus of the pane pills and the summary points, which the view moves itself.
    keys: keyboard::Handles,
    /// Each summary point's row in the summary pane, by its moment's index, as last drawn.
    point_rows: Vec<usize>,
    /// The summary point last scrolled into view when it took the focus.
    revealed: Option<usize>,
    sending: Option<Post>,
    /// The chat's calls' captions saved on this computer, as last read, and Teams' own parts they were merged with.
    local: local::Local,
}

impl RecapView {
    /// The recap of the meeting whose chat is `conversation`, titled `title`; its read starts now.
    pub fn new(
        core: Arc<dyn Core>,
        conversation: String,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_box = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let typed = cx.subscribe(&search_box, |this, search_box, event, cx| {
            this.searched(&search_box, event, cx);
        });
        let mut view = Self {
            core,
            conversation,
            title,
            contents: Contents::Reading,
            selected: 0,
            transcripts: HashMap::new(),
            languages: HashMap::new(),
            players: HashMap::new(),
            recording_files: HashMap::new(),
            watched: HashMap::new(),
            spoken: None,
            wanted: None,
            transcribed: transcribed::Transcribed::default(),
            play_wanted: false,
            transcript_wanted: false,
            search_box,
            search: None,
            query: String::new(),
            files: Vec::new(),
            saving: false,
            organizer: None,
            scroll: ScrollHandle::new(),
            note: None,
            focus: cx.focus_handle(),
            _typed: typed,
            _ai_switch: cx.observe_global::<crate::local_ai::AiSwitch>(|_, cx| cx.notify()),
            reading: None,
            settings: None,
            pane: None,
            summaries: HashMap::new(),
            summary_scroll: ScrollHandle::new(),
            keys: keyboard::Handles::default(),
            point_rows: Vec::new(),
            revealed: None,
            sending: None,
            local: local::Local::follow(cx),
        };
        view.read(cx);
        view.read_files(cx);
        view
    }

    /// With the app's settings: the tool its summaries are made with, and where they are kept.
    #[must_use]
    pub fn with_settings(mut self, settings: Entity<SettingsModel>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// The chat the recap is of.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The part shown, counted from 0.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The parts read, once they were.
    #[must_use]
    pub fn parts(&self) -> Option<&[RecapPart]> {
        match &self.contents {
            Contents::Read(parts) => Some(parts),
            Contents::Reading | Contents::Failed => None,
        }
    }

    /// The search as it stands, when one was made.
    #[must_use]
    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    /// The meeting's name changed (the chat list read it): the recap's file names follow it.
    pub fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.title = title;
        cx.notify();
    }

    /// Pauses its recordings: the tab, or the chat, was put away (a player out of sight keeps no sound going).
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        for (player, _) in self.players.values() {
            player.update(cx, VideoPlayer::pause);
        }
    }

    /// The tab shown again (or "View recap" clicked): a recap that couldn't be read, or that held nothing yet (a
    /// recording still being saved, which MCPS lists only once it is), is read again with the chat's files.
    pub fn read_again_if_empty(&mut self, cx: &mut Context<Self>) {
        let empty = match &self.contents {
            Contents::Reading => false,
            Contents::Failed => true,
            Contents::Read(parts) => parts.is_empty(),
        };
        if empty {
            self.read(cx);
            self.read_files(cx);
        }
    }

    /// Reads the meeting's contents (again).
    pub fn read(&mut self, cx: &mut Context<Self>) {
        // the recording (or saved transcript) shown: still the one shown once the recap is read again, wherever its part
        // went
        let shown = self.part().and_then(shown_key);
        self.contents = Contents::Reading;
        // a recording's file that couldn't be read is read again with the recap
        self.recording_files.retain(|_, read| !matches!(read, FileRead::Failed));
        // a 1:1 or group chat's calls' recordings are listed by the same read as a meeting's, by the chat's id
        let reply = self.core.meeting_contents(self.conversation.clone());
        let saved = self.core.call_transcripts(self.conversation.clone());
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = reply.await.map(|contents| recap_parts(&contents));
            let saved = saved.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.contents = this.local.merge(answer.ok(), saved, this.core.me());
                this.settle_parts(shown.as_deref(), cx);
                if let Some(recording) = this.wanted.take() {
                    this.show_recording(recording, cx);
                }
                this.show_wanted_transcribed(cx);
                if std::mem::take(&mut this.transcript_wanted) {
                    this.show_transcript(cx);
                }
                this.show_wanted_saved(cx);
                if std::mem::take(&mut this.play_wanted) {
                    this.play_when_shown(cx);
                }
                this.organizer_name(cx);
                this.read_transcript(cx);
                this.read_recording_file(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The parts read again (Refresh): the recording that was shown stays shown at its new place. When it is gone (or
    /// none was), its player goes with it, every other recording pauses, and the first part shows when the one at the
    /// old place is gone too.
    fn settle_parts(&mut self, shown: Option<&str>, cx: &mut Context<Self>) {
        let parts = self.parts().unwrap_or_default();
        let recordings: Vec<String> = parts.iter().filter_map(|part| part.recording.clone()).collect();
        let at = shown.and_then(|key| parts.iter().position(|part| shown_key(part).as_deref() == Some(key)));
        let count = parts.len();
        self.players.retain(|link, _| recordings.contains(link));
        if let Some(at) = at {
            self.selected = at;
            return;
        }
        self.pause(cx);
        if shown.is_none() && !crate::system_message::is_meeting_chat(&self.conversation) {
            // a 1:1 or group chat's Recap opens at its latest call, as Teams' does
            self.selected = self
                .sessions()
                .first()
                .and_then(|session| session.first().copied())
                .unwrap_or(0);
        }
        if shown.is_some() || self.selected >= count {
            if self.selected >= count {
                self.selected = 0;
            }
            self.spoken = None;
            self.search = None;
            self.query.clear();
        }
    }

    /// The files the chat's stored messages shared, for the "Shared files" card, and whether a recording is still
    /// being saved.
    fn read_files(&mut self, cx: &mut Context<Self>) {
        let task = crate::shared_view::read_messages(Arc::clone(&self.core), self.conversation.clone(), cx);
        cx.spawn(async move |this, cx| {
            let messages = task.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = crate::recap::recording_saving(&messages);
                this.files = crate::recap::shared_items(&messages)
                    .into_iter()
                    .filter(SharedItem::is_file)
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks for the organizer's name, who the transcript says started it.
    fn organizer_name(&mut self, cx: &mut Context<Self>) {
        let Some(organizer) = self.part().and_then(|part| part.organizer_id.clone()) else {
            return;
        };
        let reply = self.core.person(format!("8:orgid:{organizer}"));
        cx.spawn(async move |this, cx| {
            if let Ok(person) = reply.await {
                let _ = this.update(cx, |this, cx| {
                    this.organizer = person.name;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The part shown.
    fn part(&self) -> Option<&RecapPart> {
        self.parts()?.get(self.selected)
    }

    /// Shows the session whose first part is `first` (its menu's row), from that part; the session shown stays as it is.
    pub fn select_session(&mut self, first: usize, cx: &mut Context<Self>) {
        if !self.session().contains(&first) {
            self.select_part(first, cx);
        }
    }

    /// Shows part `index`: its recording, its transcript (read now if it wasn't), the other parts' paused.
    pub fn select_part(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.parts().is_none_or(|parts| index >= parts.len()) {
            return;
        }
        self.pause(cx);
        self.selected = index;
        self.transcribed.note_closed = false;
        self.pane = None;
        self.sending = None;
        self.spoken = None;
        self.search = None;
        self.query.clear();
        self.read_transcript(cx);
        self.read_recording_file(cx);
        cx.notify();
    }

    /// Shows the part of the recording whose sharing link is `recording` (once the parts are read); a call transcribed
    /// alone whose item's sharing link it is is shown too, as its card's View recap shows it ([`transcribed`]).
    pub fn show_recording(&mut self, recording: String, cx: &mut Context<Self>) {
        let Some(parts) = self.parts() else {
            self.wanted = Some(recording);
            return;
        };
        if let Some(index) = parts
            .iter()
            .position(|part| part.recording.as_deref() == Some(recording.as_str()))
        {
            self.select_part(index, cx);
        } else if let Some(index) = parts
            .iter()
            .position(|part| part.is_unread_transcript() && part.sharing_link.as_deref() == Some(recording.as_str()))
        {
            self.reveal(index, cx);
        }
    }

    /// The recap card's Transcript chip: the shown part's Transcript pane, from its top.
    pub fn show_transcript(&mut self, cx: &mut Context<Self>) {
        // the part it is of is shown once the parts are read: the pane is shown again then
        self.transcript_wanted = self.parts().is_none();
        self.pane = Some(Pane::Transcript);
        self.spoken = None;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// The recap card's thumbnail: the shown part's recording plays (resumed where it was paused, from its start
    /// otherwise; left alone while it plays), once the parts are read.
    pub fn play_when_shown(&mut self, cx: &mut Context<Self>) {
        if self.parts().is_none() {
            self.play_wanted = true;
            return;
        }
        let Some(player) = self.player(cx) else { return };
        player.update(cx, |player, cx| match player.state() {
            Some(State::Playing | State::Loading | State::Buffering) => {}
            Some(State::Paused) => player.toggle(cx),
            _ => player.play_from(Duration::ZERO, cx),
        });
    }

    /// Reads the shown part's transcript, when it has one not read yet (or whose read failed and is asked again).
    fn read_transcript(&mut self, cx: &mut Context<Self>) {
        let Some(location) = self.part().and_then(|part| part.transcript.clone()) else {
            return;
        };
        if matches!(
            self.transcripts.get(&location),
            Some(Read::Reading | Read::Shown { .. })
        ) {
            return;
        }
        self.transcripts.insert(location.clone(), Read::Reading);
        if let Some(id) = crate::call_transcript::local_id(&location) {
            self.read_saved(location, id, cx);
            return;
        }
        self.read_language(&location, cx);
        let reply = self.core.meeting_transcript(location.clone());
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                let read = answer
                    .ok()
                    .and_then(|vtt| Some((Transcript::of(&vtt)?, read_vtt(&vtt)?, vtt)))
                    .map_or(Read::Failed, |(transcript, cues, vtt)| Read::Shown {
                        vtt: vtt.into(),
                        transcript,
                        cues: cues.into(),
                    });
                this.transcript_read(location, read, cx);
            });
        })
        .detach();
    }

    /// The transcript at `location` read (or not).
    fn transcript_read(&mut self, location: String, read: Read, cx: &mut Context<Self>) {
        self.transcripts.insert(location, read);
        self.caption_players(cx);
        // a search asked before the transcript came
        if !self.query.is_empty() && self.search.is_none() {
            let query = self.query.clone();
            self.find(&query);
        }
        cx.notify();
    }

    /// Reads the language of the transcript at `location` (once): what its captions are called.
    fn read_language(&mut self, location: &str, cx: &mut Context<Self>) {
        if self.languages.contains_key(location) {
            return;
        }
        let reply = self.core.transcript_language(location.to_owned());
        let location = location.to_owned();
        cx.spawn(async move |this, cx| {
            let language = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.languages.insert(location, language);
                this.caption_players(cx);
            });
        })
        .detach();
    }

    /// Hands each recording's player its transcript's cues as captions, once the transcript was read (with its
    /// language, once that was read).
    fn caption_players(&mut self, cx: &mut Context<Self>) {
        let Some(parts) = self.parts() else { return };
        let captioned: Vec<(Entity<VideoPlayer>, Captions)> = parts
            .iter()
            .filter_map(|part| {
                let (player, _) = self.players.get(part.recording.as_ref()?)?;
                let location = part.transcript.as_ref()?;
                let Some(Read::Shown { cues, .. }) = self.transcripts.get(location) else {
                    return None;
                };
                let language = self.languages.get(location).cloned().flatten();
                Some((player.clone(), Captions::new(cues, language)))
            })
            .collect();
        for (player, captions) in captioned {
            player.update(cx, |player, cx| player.set_captions(Some(captions), cx));
        }
    }

    /// The shown part's transcript, once read.
    fn transcript(&self) -> Option<&Transcript> {
        let location = self.part()?.transcript.as_ref()?;
        match self.transcripts.get(location)? {
            Read::Shown { transcript, .. } => Some(transcript),
            Read::Reading | Read::Failed => None,
        }
    }

    /// The shown part's player, made when first asked for.
    fn player(&mut self, cx: &mut Context<Self>) -> Option<Entity<VideoPlayer>> {
        let index = self.selected;
        let link = self.part()?.recording.clone()?;
        if let Some((player, _)) = self.players.get(&link) {
            return Some(player.clone());
        }
        let file = SharedFile {
            name: format!("{}.mp4", self.title),
            extension: "mp4".to_owned(),
            share_url: Some(link.clone()),
            object_url: None,
            preview: None,
            location: None,
            entry: None,
        };
        let source = PictureSource::new(Arc::clone(&self.core));
        let id = format!("recap-{index}");
        let part = self.part()?;
        let recording = part.drive_id.clone().zip(part.item_id.clone());
        let player = cx.new(|cx| {
            let player = VideoPlayer::new(id, file, source, cx).filling();
            match recording {
                Some((drive, item)) => player.streaming(Recording { drive, item }),
                None => player,
            }
        });
        let watched = cx.observe(&player, |this, player, cx| this.follow(&player, cx));
        self.players.insert(link, (player.clone(), watched));
        self.caption_players(cx);
        Some(player)
    }

    /// The recording moved on: the paragraph spoken now is highlighted and kept in view.
    fn follow(&mut self, player: &Entity<VideoPlayer>, cx: &mut Context<Self>) {
        // only the shown part's recording moves its transcript
        let shown = self
            .part()
            .and_then(|part| part.recording.as_ref())
            .and_then(|link| self.players.get(link));
        if shown.is_none_or(|(shown, _)| shown != player) {
            return;
        }
        let Some(at) = player.read(cx).position() else { return };
        let spoken = self.transcript().and_then(|transcript| transcript.spoken_at(at));
        if spoken != self.spoken {
            self.spoken = spoken;
            if let (Some(at), Some(transcript)) = (spoken, self.transcript()) {
                self.scroll.scroll_to_item(row_of(&transcript.turns, at));
            }
            cx.notify();
        }
    }

    /// A paragraph clicked: the recording plays from where it starts.
    pub fn play_from(&mut self, at: Duration, cx: &mut Context<Self>) {
        if let Some(player) = self.player(cx) {
            player.update(cx, |player, cx| player.play_from(at, cx));
        }
    }

    /// The search box changed or Enter was pressed in it.
    fn searched(&mut self, search_box: &Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        let query = search_box.read(cx).value().to_string();
        match event {
            InputEvent::PressEnter { shift, .. } => {
                match self.search.as_mut().filter(|search| search.query == query.trim()) {
                    Some(search) if *shift => search.previous(),
                    Some(search) => search.next(),
                    None => self.find(&query),
                }
                self.reveal_match();
            }
            InputEvent::Change => {
                self.find(&query);
                self.reveal_match();
            }
            _ => return,
        }
        cx.notify();
    }

    /// Searches the shown transcript for `query` (a blank one ends the search).
    pub fn find(&mut self, query: &str) {
        query.trim().clone_into(&mut self.query);
        self.search = if query.trim().is_empty() {
            None
        } else {
            self.transcript().map(|transcript| Search::new(transcript, query))
        };
    }

    /// Goes to the next (or, `back`, the previous) match.
    pub fn step_match(&mut self, back: bool, cx: &mut Context<Self>) {
        if let Some(search) = self.search.as_mut() {
            if back {
                search.previous();
            } else {
                search.next();
            }
        }
        self.reveal_match();
        cx.notify();
    }

    fn reveal_match(&self) {
        if let (Some(at), Some(transcript)) = (self.search.as_ref().and_then(Search::shown), self.transcript()) {
            self.scroll.scroll_to_item(row_of(&transcript.turns, at));
        }
    }

    /// Reads the shown part's recording's file, when it has a recording not asked of yet: whether the account may
    /// download it.
    fn read_recording_file(&mut self, cx: &mut Context<Self>) {
        let Some(part) = self.part() else { return };
        let (Some(link), Some(drive), Some(item)) =
            (part.recording.clone(), part.drive_id.clone(), part.item_id.clone())
        else {
            return;
        };
        if self.recording_files.contains_key(&link) {
            return;
        }
        self.recording_files.insert(link.clone(), FileRead::Reading);
        let reply = self.core.recording_file(link.clone(), drive, item);
        cx.spawn(async move |this, cx| {
            let read = reply.await.map_or(FileRead::Failed, FileRead::Read);
            let _ = this.update(cx, |this, cx| {
                this.recording_files.insert(link, read);
                cx.notify();
            });
        })
        .detach();
    }

    /// The shown part's recording, when the account may download it and no download of it runs: its sharing link and
    /// the name it is saved under (its item's, else the meeting's).
    #[must_use]
    pub fn downloadable(&self, cx: &App) -> Option<(String, String)> {
        let link = self.part()?.recording.clone()?;
        let Some(FileRead::Read(access)) = self.recording_files.get(&link) else {
            return None;
        };
        let running = download_of(&link, cx).is_some_and(|download| download.read(cx).progress().running());
        if !access.can_download || running {
            return None;
        }
        let name = access.name.clone().unwrap_or_else(|| recording_file_name(&self.title));
        Some((link, name))
    }

    /// Download (More options): the shown part's recording saved where the user chooses (the downloads folder without
    /// a chooser), under its own name.
    pub fn download_recording(&mut self, cx: &mut Context<Self>) {
        let Some((link, name)) = self.downloadable(cx) else {
            return;
        };
        let Some(folder) = self.download_folder(cx) else { return };
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn(async move |this, cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => crate::picture_viewer::free_name(&folder, &name),
            };
            let _ = this.update(cx, |this, cx| this.save_recording(link, target, cx));
        })
        .detach();
    }

    fn download_folder(&mut self, cx: &mut Context<Self>) -> Option<PathBuf> {
        match crate::picture_viewer::downloads() {
            Ok(folder) => Some(folder),
            Err(error) => {
                self.note = Some(format!("Couldn't open Downloads: {error}").into());
                cx.notify();
                None
            }
        }
    }

    /// Downloads the recording whose sharing link is `link` to `target`, its progress shown under the top row.
    pub fn save_recording(&mut self, link: String, target: PathBuf, cx: &mut Context<Self>) {
        let download = recording_download::start(Arc::clone(&self.core), link, target, cx);
        self.watch(&download, cx);
        cx.notify();
    }

    /// Follows `download`: the view is drawn again as it moves on.
    fn watch(&mut self, download: &Entity<RecordingDownload>, cx: &mut Context<Self>) {
        self.watched
            .entry(download.entity_id())
            .or_insert_with(|| cx.observe(download, |_, _, cx| cx.notify()));
    }

    /// Download as .vtt: the transcript saved where the user chooses (the downloads folder without a chooser).
    pub fn download(&mut self, cx: &mut Context<Self>) {
        let Some(location) = self.part().and_then(|part| part.transcript.clone()) else {
            return;
        };
        let Some(Read::Shown { vtt, .. }) = self.transcripts.get(&location) else {
            return;
        };
        let vtt = Arc::clone(vtt);
        let name = transcript_file_name(&self.title);
        let Some(folder) = self.download_folder(cx) else { return };
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn(async move |this, cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => crate::picture_viewer::free_name(&folder, &name),
            };
            let written = cx
                .background_executor()
                .spawn({
                    let target = target.clone();
                    async move { std::fs::write(&target, vtt.as_bytes()) }
                })
                .await;
            let shown = target.file_name().map_or_else(
                || target.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let note = match written {
                Ok(()) => format!("Saved as {shown}"),
                Err(error) => format!("Couldn't save {shown}: {error}"),
            };
            let _ = this.update(cx, |this, cx| {
                this.note = Some(note.into());
                cx.notify();
            });
        })
        .detach();
    }

    /// The link Share → Copy link copies: the recap's Teams link for the part shown.
    fn link(&self) -> Option<String> {
        recap_link(self.part()?, &self.conversation)
    }
}

impl Render for RecapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body = match &self.contents {
            Contents::Reading => centred(spinner(palette), palette).into_any_element(),
            Contents::Failed => self.unread(palette, cx),
            Contents::Read(parts) if parts.is_empty() => {
                let words = if self.saving { SAVING } else { NOTHING_LEFT };
                centred(empty_state(Glyph::VIDEO, words, palette), palette).into_any_element()
            }
            Contents::Read(_) => self.recap(window, palette, cx),
        };
        let preview = self.send_preview(palette, cx);
        div()
            .debug_selector(|| "recap-view".to_owned())
            .id("recap-view")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(body)
            .children(preview)
    }
}

/// The parts of the view.
impl RecapView {
    /// The recap that couldn't be read: what happened, Try again and Teams on the web.
    fn unread(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let again = Button::new("recap-retry")
            .label("Try again")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.read(cx)));
        let web = teams_proto::chat::web_chat_link(&self.conversation);
        let open = Button::new("recap-open-web")
            .label(crate::chat_view::OPEN_ON_THE_WEB)
            .outline()
            .on_click(move |_, _, cx| cx.open_url(&web));
        let state = empty_state(Glyph::CIRCLE_ALERT, RECAP_UNREAD, palette).child(
            div()
                .flex()
                .gap_2()
                .child(selected_by("recap-retry", again))
                .child(selected_by("recap-open-web", open)),
        );
        centred(state, palette).into_any_element()
    }

    fn recap(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let top = self.top_row(palette, cx);
        let recorded = self.part().is_some_and(|part| part.recording.is_some());
        let side = div()
            .flex()
            .flex_col()
            .gap_2()
            .min_h_0()
            .child(self.files_card(palette, cx))
            .child(self.panes_card(window, palette, cx));
        let body = if recorded {
            let player = self.player(cx);
            div()
                .id("recap-content")
                .debug_selector(|| "recap-content".to_owned())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_wrap()
                .overflow_y_scroll()
                .gap_2()
                .child(recording_card(player, palette))
                .child(side.w(px(400.0)).flex_none().max_w_full())
        } else {
            div()
                .id("recap-content")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(side.flex_1())
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pb_3()
            .child(top)
            .children(self.transcribed_note(palette, cx))
            .children(self.delete_confirmation(palette, cx))
            .children(self.download_rows(palette, cx))
            .children(self.saving.then(|| {
                div()
                    .debug_selector(|| "recap-saving".to_owned())
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SAVING)
            }))
            .child(body)
            .children(self.note.clone().map(|note| {
                div()
                    .debug_selector(|| "recap-note".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(note)
            }))
            .into_any_element()
    }

    /// The recap's sessions as its menu lists them, the newest first ([`sessions`]): a 1:1 or group chat's calls each
    /// one, a meeting chat's meeting one.
    fn sessions(&self) -> Vec<Vec<usize>> {
        let by_call = !crate::system_message::is_meeting_chat(&self.conversation);
        sessions(self.parts().unwrap_or_default(), by_call, self.revealed())
    }

    /// The session of the part shown: its parts' indices.
    fn session(&self) -> Vec<usize> {
        self.sessions()
            .into_iter()
            .find(|session| session.contains(&self.selected))
            .unwrap_or_default()
    }

    /// The shown session's line: its day and span, "Friday, November 6, 2026 5:29 PM - 5:30 PM".
    fn session_line(&self, cx: &Context<Self>) -> Option<String> {
        local_line_of(self.parts()?, &self.session(), clock::cycle(cx))
    }

    /// The session's line in its menu, the parts' row of a session recorded more than once, Share, Watch in browser,
    /// More options.
    fn top_row(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let session = self.session();
        let tabs = (session.len() > 1).then(|| self.part_tabs(&session, palette, cx));
        div()
            .min_h(px(48.0))
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(self.session_menu(palette, cx))
            .children(tabs)
            .children(self.shows_saved().then(|| local::saved_mark(palette)))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .ml_auto()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(self.actions(cx)),
            )
            .into_any_element()
    }

    /// The shown session's line in a box with its menu of the recap's sessions, as Teams draws it: the box hugs its line
    /// up to [`SESSION_WIDTH`], a longer line (and a row of the menu) cut short with an ellipsis at its end.
    fn session_menu(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let parts = self.parts().unwrap_or_default();
        let cycle = clock::cycle(cx);
        let line = self.session_line(cx).unwrap_or_else(|| self.title.to_string());
        if !listed(parts, &self.session()) {
            return transcribed::session_line(line);
        }
        // each session by its line, the one shown checked, the newest first
        let rows: Vec<(usize, String, bool)> = self
            .sessions()
            .into_iter()
            .filter(|indices| listed(parts, indices))
            .filter_map(|indices| {
                let first = *indices.first()?;
                let line = local_line_of(parts, &indices, cycle).unwrap_or_else(|| self.title.to_string());
                Some((first, line, indices.contains(&self.selected)))
            })
            .collect();
        let face = div()
            .debug_selector(|| "recap-session-face".to_owned())
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(ellipsized(line))
            .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)).flex_none());
        let view = cx.entity().downgrade();
        let menu = Button::new("recap-session")
            .outline()
            .w_full()
            .min_w_0()
            .flex_shrink(1.0)
            .max_w(px(SESSION_WIDTH))
            .child(face)
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.max_w(px(SESSION_WIDTH)).scrollable(true).max_h(px(320.0));
                rows.iter().fold(menu, |menu, (first, line, chosen)| {
                    let (first, view) = (*first, view.clone());
                    menu.item(session_row(first, line.clone(), *chosen).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.select_session(first, cx));
                    }))
                })
            });
        selected_by("recap-session", menu)
            .flex()
            .flex_none()
            .min_w(px(160.0))
            .max_w(px(SESSION_WIDTH))
            .bg(rgb(palette.surface))
            .into_any_element()
    }

    /// The shown session's parts, "Part 1", "Part 2", each selected by its index among all the recap's parts.
    fn part_tabs(&self, session: &[usize], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tab = |position: usize, index: usize, cx: &mut Context<Self>| {
            let chosen = index == self.selected;
            div()
                .id(SharedString::from(format!("recap-part-{index}")))
                .debug_selector(move || format!("recap-part-{index}"))
                .h(px(32.0))
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .border_b_2()
                .border_color(if chosen {
                    rgb(palette.brand)
                } else {
                    gpui::transparent_black().into()
                })
                .text_color(rgb(if chosen {
                    palette.foreground
                } else {
                    palette.muted_foreground
                }))
                .when(chosen, |tab| tab.font_weight(FontWeight::SEMIBOLD))
                .child(self.part_glyph(index).icon().size(px(14.0)))
                .child(part_label(position))
                .on_click(cx.listener(move |this, _, _, cx| this.select_part(index, cx)))
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .pl_3()
            .border_l_1()
            .border_color(rgb(palette.border));
        for (position, &index) in session.iter().enumerate() {
            row = row.child(tab(position, index, cx));
        }
        row.into_any_element()
    }

    /// Share, Watch in browser (a recorded part) and More options.
    fn actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.shows_saved() {
            return Self::saved_actions(cx);
        }
        let mut actions = Vec::new();
        let link = self.link();
        if let Some(link) = link.clone() {
            let share = Button::new("recap-share")
                .icon(Glyph::SHARE.icon())
                .label("Share")
                .ghost()
                .dropdown_menu(move |menu, _, _| {
                    let copied = link.clone();
                    menu.item(
                        menu_row("recap-share-copy", Glyph::LINK, "Copy link").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        }),
                    )
                });
            actions.push(selected_by("recap-share", share).into_any_element());
        }
        if let Some(watch) = self.part().and_then(|part| part.recording.clone()) {
            let button = Button::new("recap-watch")
                .icon(Glyph::EXTERNAL_LINK.icon())
                .label("Watch in browser")
                .ghost()
                .on_click(move |_, _, cx| cx.open_url(&watch));
            actions.push(selected_by("recap-watch", button).into_any_element());
        }
        let web = link.unwrap_or_else(|| teams_proto::chat::web_chat_link(&self.conversation));
        let me = cx.entity().downgrade();
        let downloadable = self.downloadable(cx).is_some();
        let more = icon_button("recap-more", Glyph::MORE, "More options").dropdown_menu(move |menu, _, _| {
            let (open, copied, again, saved) = (web.clone(), web.clone(), me.clone(), me.clone());
            let menu = if downloadable {
                menu.item(
                    menu_row("recap-more-download", Glyph::DOWNLOAD, "Download").on_click(move |_, _, cx| {
                        let _ = saved.update(cx, Self::download_recording);
                    }),
                )
            } else {
                menu
            };
            menu.item(
                menu_row(
                    "recap-more-web",
                    Glyph::EXTERNAL_LINK,
                    crate::chat_view::OPEN_ON_THE_WEB,
                )
                .on_click(move |_, _, cx| cx.open_url(&open)),
            )
            .item(
                menu_row("recap-more-copy", Glyph::LINK, "Copy link")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
            .item(
                menu_row("recap-more-refresh", Glyph::REFRESH, "Refresh").on_click(move |_, _, cx| {
                    let _ = again.update(cx, Self::read);
                }),
            )
        });
        actions.push(selected_by("recap-more", more).into_any_element());
        actions
    }

    /// A row for each download of the recap's recordings started this run: how far it got, or how it ended.
    fn download_rows(&mut self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut links: Vec<String> = self
            .parts()
            .unwrap_or_default()
            .iter()
            .filter_map(|part| part.recording.clone())
            .collect();
        links.dedup();
        let mut rows = Vec::new();
        for (index, link) in links.into_iter().enumerate() {
            let Some(download) = download_of(&link, cx) else {
                continue;
            };
            self.watch(&download, cx);
            rows.push(Self::download_row(index, &download, palette, cx));
        }
        rows
    }

    /// One download's row: its icon, what it does or did (with a bar while it runs), its size, and Cancel; Show in
    /// folder once saved; Try again once failed; Dismiss once ended.
    fn download_row(
        index: usize,
        download: &Entity<RecordingDownload>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = download.read(cx);
        let (name, link, target, progress) = (
            shown.name(),
            shown.link().to_owned(),
            shown.target().to_owned(),
            shown.progress().clone(),
        );
        let (glyph, colour, state) = match &progress {
            Progress::Running { .. } => (Glyph::DOWNLOAD, palette.brand, "running"),
            Progress::Saved => (Glyph::CIRCLE_CHECK, theme::SUCCESS, "saved"),
            Progress::Failed(_) => (Glyph::CIRCLE_ALERT, theme::DANGER, "failed"),
        };
        let size = match progress {
            Progress::Running {
                written,
                total: Some(total),
            } => Some(format!("{} of {}", byte_size(written), byte_size(total))),
            Progress::Running { total: None, .. } => Some("Starting\u{2026}".to_owned()),
            Progress::Saved | Progress::Failed(_) => None,
        };
        let text = download_text(&progress, &name, palette);
        let actions = Self::download_actions(index, &link, target, &progress, cx);
        div()
            .debug_selector(move || format!("recap-download-{index} {state}"))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(div().text_color(rgb(colour)).child(glyph.icon().size(px(16.0))))
            .child(text)
            .children(size.map(|size| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(size)
            }))
            .children(actions)
            .into_any_element()
    }

    /// A download's buttons: Cancel while it runs; Show in folder once saved; Try again once failed (not when another
    /// download saved to the same place: Try again would replace that file without a question); Dismiss once ended.
    fn download_actions(
        index: usize,
        link: &str,
        target: PathBuf,
        progress: &Progress,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let dismissed = link.to_owned();
        let dismiss = move |_: &mut Self, cx: &mut Context<Self>| {
            recording_download::dismiss(&dismissed, cx);
            cx.notify();
        };
        let mut actions = Vec::new();
        match progress {
            Progress::Running { .. } => {
                let id = format!("recap-download-cancel-{index}");
                let cancel = icon_button(SharedString::from(id.clone()), Glyph::X, "Cancel download")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| dismiss(this, cx)));
                actions.push(selected_by(id, cancel).into_any_element());
                return actions;
            }
            Progress::Saved => {
                let id = format!("recap-download-show-{index}");
                let show = Button::new(SharedString::from(id.clone()))
                    .icon(Glyph::FOLDER.icon())
                    .label("Show in folder")
                    .ghost()
                    .small()
                    .on_click(move |_, _, cx| cx.reveal_path(&target));
                actions.push(selected_by(id, show).into_any_element());
            }
            Progress::Failed(Failure::Busy) => {}
            Progress::Failed(_) => {
                let id = format!("recap-download-retry-{index}");
                let link = link.to_owned();
                let again = Button::new(SharedString::from(id.clone()))
                    .label("Try again")
                    .outline()
                    .small()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(download) = download_of(&link, cx) {
                            download.update(cx, RecordingDownload::start);
                        }
                    }));
                actions.push(selected_by(id, again).into_any_element());
            }
        }
        let id = format!("recap-download-dismiss-{index}");
        let close = icon_button(SharedString::from(id.clone()), Glyph::X, "Dismiss")
            .small()
            .on_click(cx.listener(move |this, _, _, cx| dismiss(this, cx)));
        actions.push(selected_by(id, close).into_any_element());
        actions
    }

    /// "Shared files": the files the chat shared, each opened in the browser; Teams' words when there are none.
    fn files_card(&self, palette: Palette, cx: &App) -> AnyElement {
        let _ = cx;
        let files: Vec<AnyElement> = self
            .files
            .iter()
            .take(8)
            .enumerate()
            .map(|(index, file)| file_chip(index, file, palette))
            .collect();
        let shown = if files.is_empty() {
            div()
                .debug_selector(|| "recap-no-files".to_owned())
                .child(NO_FILES)
                .into_any_element()
        } else {
            div().flex().flex_wrap().gap_2().children(files).into_any_element()
        };
        card(palette)
            .debug_selector(|| "recap-files".to_owned())
            .flex_none()
            .p_4()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Shared files"))
            .child(shown)
            .into_any_element()
    }

    /// The panes' pills (AI summary, and Transcript for a part with one) and the pane shown.
    fn panes_card(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let has_transcript = self.part().is_some_and(|part| part.transcript.is_some());
        // a recording without a transcript, or a transcript with nothing to read, keeps the pane, saying there is none,
        // as Teams' Recap does
        let offers_transcript = has_transcript
            || self
                .part()
                .is_some_and(|part| part.recording.is_some() || part.is_unread_transcript());
        let shown = self.pane(cx);
        let ai = crate::local_ai::enabled(cx);
        let order: Vec<Pane> = [(ai, Pane::AiSummary), (offers_transcript, Pane::Transcript)]
            .into_iter()
            .filter_map(|(drawn, pane)| drawn.then_some(pane))
            .collect();
        let pills = div()
            .flex_none()
            .flex()
            .gap_2()
            .px_4()
            .pt_4()
            .pb_2()
            .children(order.iter().map(|pane| self.pill(*pane, shown, &order, palette, cx)));
        self.reveal_point(window, cx);
        let body = match shown {
            Pane::Transcript if has_transcript => self.transcript_pane(window, palette, cx),
            Pane::Transcript if offers_transcript || !ai => {
                let state = empty_state(Glyph::TRANSCRIPT, NO_TRANSCRIPT_YET, palette).child(
                    div()
                        .max_w(px(320.0))
                        .text_sm()
                        .text_center()
                        .text_color(rgb(palette.muted_foreground))
                        .child(NO_TRANSCRIPT_WHY),
                );
                centred(state, palette).into_any_element()
            }
            Pane::AiSummary | Pane::Transcript => self.summary_pane(palette, cx),
        };
        card(palette)
            .debug_selector(|| "recap-transcript-card".to_owned())
            .flex_1()
            .min_h_0()
            .child(pills)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    fn transcript_pane(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let location = self.part().and_then(|part| part.transcript.clone()).unwrap_or_default();
        let toolbar = self.transcript_toolbar(palette, cx);
        let body = match self.transcripts.get(&location) {
            None | Some(Read::Reading) => centred(spinner(palette), palette).into_any_element(),
            Some(Read::Failed) => {
                let again = Button::new("recap-transcript-retry")
                    .label("Try again")
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(location) = this.part().and_then(|part| part.transcript.clone()) {
                            this.transcripts.remove(&location);
                        }
                        this.read_transcript(cx);
                        cx.notify();
                    }));
                let state = empty_state(Glyph::CIRCLE_ALERT, TRANSCRIPT_UNREAD, palette)
                    .child(selected_by("recap-transcript-retry", again));
                centred(state, palette).into_any_element()
            }
            Some(Read::Shown { transcript, .. }) => {
                let turns = transcript.turns.clone();
                self.transcript_rows(&turns, palette, cx)
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    /// Search (and its matches' count and arrows) and Download.
    fn transcript_toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let search = div().debug_selector(|| "recap-search".to_owned()).flex_1().child(
            Input::new(&self.search_box)
                .prefix(Glyph::SEARCH.icon().size(px(14.0)))
                .small(),
        );
        let matches = self.search.as_ref().map(|search| {
            let label = search.label();
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(selected_by(
                    "recap-search-previous",
                    icon_button("recap-search-previous", Glyph::CHEVRON_UP, "Previous match")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(true, cx))),
                ))
                .child(
                    div()
                        .debug_selector(move || format!("recap-search-count {label}"))
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(search.label()),
                )
                .child(selected_by(
                    "recap-search-next",
                    icon_button("recap-search-next", Glyph::CHEVRON_DOWN, "Next match")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(false, cx))),
                ))
        });
        let me = cx.entity().downgrade();
        let download = Button::new("recap-download")
            .icon(Glyph::DOWNLOAD.icon())
            .label("Download")
            .ghost()
            .small()
            .dropdown_menu(move |menu, _, _| {
                let me = me.clone();
                menu.item(
                    menu_row("recap-download-vtt", Glyph::FILE, "Download as .vtt").on_click(move |_, _, cx| {
                        let _ = me.update(cx, Self::download);
                    }),
                )
            });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .child(search)
            .children(matches)
            .child(selected_by("recap-download", download))
            .into_any_element()
    }

    /// The transcript's rows: the note, who started it, each turn, and for a transcript alone who stopped it.
    fn transcript_rows(&self, turns: &[Turn], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let who = self.organizer.clone().unwrap_or_else(|| "The organizer".to_owned());
        let saved = self.shows_saved();
        let only_transcript = !saved && self.part().is_some_and(|part| part.recording.is_none());
        let mut rows: Vec<AnyElement> = if saved {
            local::notice_rows(palette)
        } else {
            vec![
                event_row("recap-ai-notice", Glyph::INFO, AI_NOTICE.to_owned(), palette),
                event_row(
                    "recap-started",
                    Glyph::TRANSCRIPT,
                    format!("{who} started transcription"),
                    palette,
                ),
            ]
        };
        for (index, turn) in turns.iter().enumerate() {
            rows.extend(self.turn_rows(index, turn, palette, cx));
        }
        if only_transcript {
            rows.push(event_row(
                "recap-stopped",
                Glyph::TRANSCRIPT,
                format!("{who} stopped transcription"),
                palette,
            ));
        }
        div()
            .id("recap-transcript")
            .debug_selector(|| "recap-transcript".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_4()
            .pb_4()
            .children(rows)
            .into_any_element()
    }

    /// One speaker's turn, a row each (the transcript is followed by paragraph): their initials, name and time, then
    /// its paragraphs under them, each played from on a click.
    fn turn_rows(&self, index: usize, turn: &Turn, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let name = turn.speaker.clone().unwrap_or_else(|| "Speaker".to_owned());
        let avatar = people::avatar(
            Some(&SharedString::from(name.clone())),
            None,
            gpui_component::Size::Small,
        );
        let header = div()
            .debug_selector(move || format!("recap-turn-{index}"))
            .pt_3()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(px(TURN_INDENT - 12.0)).flex_none().child(avatar))
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(name))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(offset(turn.start)),
            );
        let query = self.search.as_ref().map(|search| search.query.clone());
        let paragraphs = turn.paragraphs.iter().enumerate().map(|(at, paragraph)| {
            let spoken = self.spoken == Some((index, at));
            let current = self.search.as_ref().and_then(|search| search.current_range(index, at));
            let start = paragraph.start;
            let text = highlighted(&paragraph.text, query.as_deref(), current.as_ref(), palette);
            div()
                .pl(px(TURN_INDENT - 4.0))
                .pt_1()
                .child(
                    div()
                        .id(SharedString::from(format!("recap-paragraph-{index}-{at}")))
                        .debug_selector(move || {
                            format!("recap-paragraph-{index}-{at}{}", if spoken { " spoken" } else { "" })
                        })
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(spoken, |paragraph| paragraph.bg(rgb(palette.selected)))
                        .hover(move |paragraph| paragraph.bg(rgb(palette.hover)))
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| this.play_from(start, cx))),
                )
                .into_any_element()
        });
        std::iter::once(header.into_any_element()).chain(paragraphs).collect()
    }
}

/// What a part is known by across a read again: its recording, else its transcript.
fn shown_key(part: &RecapPart) -> Option<String> {
    part.recording.clone().or_else(|| part.transcript.clone())
}

/// A paragraph's words, the query's matches highlighted (the one shown now, `current`, strongly).
fn highlighted(
    text: &str,
    query: Option<&str>,
    current: Option<&std::ops::Range<usize>>,
    palette: Palette,
) -> AnyElement {
    let ranges = query.map_or_else(Vec::new, |query| crate::recap::matched_ranges(text, query));
    if ranges.is_empty() {
        return div().text_sm().child(text.to_owned()).into_any_element();
    }
    let strong = HighlightStyle {
        background_color: Some(gpui::rgb(theme::SEARCH_MATCH).into()),
        color: Some(gpui::black()),
        ..HighlightStyle::default()
    };
    let other = HighlightStyle {
        background_color: Some(gpui::rgb(palette.selection).into()),
        ..HighlightStyle::default()
    };
    let highlights: Vec<_> = ranges
        .into_iter()
        .map(|range| {
            let mark = if current == Some(&range) { strong } else { other };
            (range, mark)
        })
        .collect();
    div()
        .text_sm()
        .child(StyledText::new(text.to_owned()).with_highlights(highlights))
        .into_any_element()
}

/// What a download does or did, by its file's name: under it why it failed, or a bar of how far it got.
fn download_text(progress: &Progress, name: &str, palette: Palette) -> gpui::Div {
    let words = match progress {
        Progress::Running { .. } => format!("Downloading {name}"),
        Progress::Saved => format!("Downloaded {name}"),
        Progress::Failed(_) => format!("Couldn't download {name}"),
    };
    let why = match progress {
        Progress::Failed(failure) => Some(
            div()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(failure.words()),
        ),
        Progress::Running { .. } | Progress::Saved => None,
    };
    let bar = progress.fraction().map(|fraction| {
        div().h(px(4.0)).w_full().rounded_full().bg(rgb(palette.border)).child(
            div()
                .h_full()
                .w(gpui::relative(fraction))
                .rounded_full()
                .bg(rgb(palette.brand)),
        )
    });
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().truncate().child(words))
        .children(why)
        .children(bar)
}

/// The left card: the recording, as tall as it plays.
fn recording_card(player: Option<Entity<VideoPlayer>>, palette: Palette) -> AnyElement {
    card(palette)
        .debug_selector(|| "recap-recording".to_owned())
        .self_start()
        .flex_1()
        // A narrow recap wraps its transcript below the picture rather than clipping the player's controls and
        // 300 px settings panels. Both cards can still grow to the full row on a small window.
        .flex_basis(px(380.0))
        .min_w(px(380.0))
        .max_w_full()
        .overflow_hidden()
        .children(player)
        .into_any_element()
}

/// A file the chat shared, as a chip that opens it in the browser.
fn file_chip(index: usize, file: &SharedItem, palette: Palette) -> AnyElement {
    let url = file.url.clone();
    div()
        .id(SharedString::from(format!("recap-file-{index}")))
        .debug_selector(move || format!("recap-file-{index}"))
        .max_w(px(240.0))
        .h(px(32.0))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .cursor_pointer()
        .hover(move |chip| chip.bg(rgb(palette.hover)))
        .child(Glyph::FILE.icon().size(px(14.0)))
        .child(div().text_sm().truncate().child(file.name.clone()))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}

/// A line of the transcript that isn't anyone's words: an icon and muted words.
fn event_row(selector: &'static str, glyph: Glyph, text: String, palette: Palette) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .pt_3()
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(
            div()
                .w(px(24.0))
                .flex()
                .justify_center()
                .child(glyph.icon().size(px(14.0))),
        )
        .child(text)
        .into_any_element()
}

/// A row of the sessions' menu: the session whose first part is `first`, by its line, checked when it is shown.
fn session_row(first: usize, line: String, chosen: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        let check = chosen.then(|| Glyph::CHECK.icon().size(px(14.0)));
        div()
            .debug_selector(move || format!("recap-session-row-{first}"))
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(14.0)).flex_none().children(check))
            .child(ellipsized(line.clone()))
    })
}

/// One line of text cut short with an ellipsis at its end when it doesn't fit.
fn ellipsized(text: String) -> gpui::Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(text)
}

/// A raised, rounded box.
fn card(palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
}

fn centred(child: impl IntoElement, palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(palette.muted_foreground))
        .child(child)
}

fn spinner(palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "recap-loading".to_owned())
        .text_color(rgb(palette.brand))
        .child(Glyph::LOADING.icon().size(px(28.0)))
        .into_any_element()
}

/// An empty or failed state: its icon in a tinted circle and its words.
fn empty_state(glyph: Glyph, words: &'static str, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(56.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(glyph.icon().size(px(26.0))),
        )
        .child(
            div()
                .debug_selector(move || format!("recap-state {words}"))
                .text_color(rgb(palette.foreground))
                .child(words),
        )
}

/// A row of one of the recap's menus.
pub(crate) fn menu_row(selector: &'static str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}
