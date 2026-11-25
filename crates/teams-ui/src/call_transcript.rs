//! Every call's live captions saved on this computer as its transcript (Settings > Calls > "Save live captions of every
//! call"): an ngteams feature of its own. Teams and the call's other people see nothing of it; only the captions'
//! own service is asked for, as the account's Show live captions asks for it.
//!
//! - **The recorder** ([`Recorder`], one per account, [`follow`]): while the setting is on, a call that connects
//!   (1:1, group or meeting; not the test call) gets its captions turned on ([`Ask::RecordCaptions`]: the caption
//!   feature's own start), their pane left as the user has it (hidden unless they show it). A start that failed is
//!   tried again a few times, apart. Each utterance's final result is kept in the store as it comes, with who said it
//!   and when (first heard, final): the transcript of the chat the call is in, the call's connect time its start
//!   ([`TranscriptWrite::Open`]). A call back in the same chat within [`REJOIN_WITHIN`] of the last one's end goes on
//!   in its transcript. The call's end is kept as the transcript's end. A later final result of an utterance replaces
//!   its words. The setting turned off mid-call stops the saving, and the captions when only the saving ran them. The
//!   read-only app saves nothing (turning the captions on is a request of its own).
//! - **What views show of it** ([`location`], [`part`], [`merged`], [`vtt`], [`attach`]): a transcript is a recap part
//!   of its own ([`teams_proto::recap::RecapPart`]) whose transcript is at `local:<account>:<id>`, its lines written as
//!   the `WebVTT` Teams' own transcripts are (speakers in `<v>` tags, times from the call's start), so the recap's
//!   transcript, search, download and AI summary take it as they take Teams'. Teams' own transcript of the same time
//!   wins: ours is offered only when Teams has none. A call's end row in its chat carries a link to it ([`attach`]).
//! - **Changes** are told on [`SavedTranscripts`] (an app global): views that show transcripts read them again.
//!
//! Nothing of the captions' words is logged or traced: the log says only that lines were kept, by how many.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Global, Subscription};
use teams_core::{CallId, CallStatus, Caption, TranscriptWrite};
use teams_proto::recap::RecapPart;
use teams_proto::transcript::Cue;
use teams_store::{CallTranscript, TranscriptLine};

use crate::bridge::Core;
use crate::calls::{self, ActiveCall, Ask, CallKind, Calls};
use crate::settings::SettingsModel;
use crate::trace::ViewTrace;

/// A call back in the same chat within this of the last one's end goes on in its transcript.
pub const REJOIN_WITHIN: Duration = Duration::from_mins(5);
/// What a transcript saved here is marked with.
pub const SAVED_HERE: &str = "Saved on this computer";
/// What a link to a saved transcript says.
pub const TRANSCRIPT: &str = "Transcript";
/// How a transcript location of ours starts.
pub const LOCAL: &str = "local:";
/// How many times a start of the captions that failed is tried again, and how long after.
const RETRIES: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(20);
/// How near a call's end row in its chat a transcript's end must be to be linked from it.
const END_NEAR_MS: i64 = 3 * 60 * 1000;

/// The saved transcripts of an account's chat changed: kept, ended or deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptsChanged {
    /// The account, by its MRI.
    pub account: String,
    pub conversation: String,
}

/// Where changes to the saved transcripts are told (an app global, [`saved`]).
pub struct SavedTranscripts;

impl EventEmitter<TranscriptsChanged> for SavedTranscripts {}

struct SavedGlobal(Entity<SavedTranscripts>);

impl Global for SavedGlobal {}

/// The app's [`SavedTranscripts`], made when first asked for.
pub fn saved(cx: &mut App) -> Entity<SavedTranscripts> {
    if let Some(saved) = cx.try_global::<SavedGlobal>() {
        return saved.0.clone();
    }
    let saved = cx.new(|_| SavedTranscripts);
    cx.set_global(SavedGlobal(saved.clone()));
    saved
}

/// Tells the views that `account`'s transcripts of `conversation` changed.
pub fn changed(account: &str, conversation: &str, cx: &mut App) {
    let told = TranscriptsChanged {
        account: account.to_owned(),
        conversation: conversation.to_owned(),
    };
    saved(cx).update(cx, |_, cx| cx.emit(told));
}

/// The transcript location of `account`'s saved transcript `id`: what its recap part names as its transcript (and its
/// summary is kept by).
#[must_use]
pub fn location(account: &str, id: i64) -> String {
    format!("{LOCAL}{account}:{id}")
}

/// The saved transcript a location of ours names; `None` for Teams' own.
#[must_use]
pub fn local_id(location: &str) -> Option<i64> {
    location.strip_prefix(LOCAL)?.rsplit_once(':')?.1.parse().ok()
}

/// A saved transcript as a recap part: its transcript ours, its span the call's.
#[must_use]
pub fn part(account: &str, transcript: &CallTranscript) -> RecapPart {
    RecapPart {
        recording: None,
        transcript: Some(location(account, transcript.id)),
        sharing_link: None,
        call_id: None,
        drive_id: None,
        item_id: None,
        start_ms: Some(transcript.started_at),
        end_ms: Some(transcript.ended_at),
        organizer_id: None,
        tenant_id: None,
    }
}

/// Teams' parts with ours among them by their start: one of ours whose time a Teams transcript covers is left out
/// (Teams' own wins), as is every one of ours when a Teams transcript doesn't say its time.
#[must_use]
pub fn merged(teams: Vec<RecapPart>, ours: Vec<RecapPart>) -> Vec<RecapPart> {
    let transcribed: Vec<Option<(i64, i64)>> = teams
        .iter()
        .filter(|part| part.transcript.is_some())
        .map(|part| part.start_ms.zip(part.end_ms))
        .collect();
    let covered = |part: &RecapPart| {
        let (Some(start), Some(end)) = (part.start_ms, part.end_ms) else {
            return !transcribed.is_empty();
        };
        transcribed
            .iter()
            .any(|span| span.is_none_or(|(from, to)| from <= end + END_NEAR_MS && start <= to + END_NEAR_MS))
    };
    let mut parts = teams;
    parts.extend(ours.into_iter().filter(|part| !covered(part)));
    parts.sort_by_key(|part| part.start_ms.unwrap_or(i64::MAX));
    parts
}

/// A saved transcript's lines as cues, timed from the call's start (`started_at`, ms).
#[must_use]
pub fn cues(started_at: i64, lines: &[TranscriptLine]) -> Vec<Cue> {
    let since = |at: i64| Duration::from_millis(u64::try_from(at.saturating_sub(started_at)).unwrap_or(0));
    lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let start = since(line.started_at);
            Cue {
                id: Some((index + 1).to_string()),
                start,
                end: since(line.ended_at).max(start),
                speaker: line.speaker_name.clone().filter(|name| !name.trim().is_empty()),
                text: line.text.trim().to_owned(),
            }
        })
        .collect()
}

/// `cues` as a `WebVTT` file, as Teams writes its transcripts (each speaker in a `<v>` tag).
#[must_use]
pub fn vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        if let Some(id) = &cue.id {
            out.push_str(id);
            out.push('\n');
        }
        let _ = writeln!(out, "{} --> {}", stamp(cue.start), stamp(cue.end));
        let text = escaped(&cue.text.replace('\n', " "));
        match &cue.speaker {
            Some(speaker) => {
                let _ = writeln!(out, "<v {}>{text}</v>", escaped(speaker));
            }
            None => {
                let _ = writeln!(out, "{text}");
            }
        }
        out.push('\n');
    }
    out
}

/// `at` as a `WebVTT` timestamp, `hh:mm:ss.ttt`.
fn stamp(at: Duration) -> String {
    let millis = at.as_millis();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1_000 % 60,
        millis % 1_000
    )
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Which call end rows of a chat link which saved transcripts: each transcript to the end row (`(row id, its time in
/// ms)`) nearest its end, within a few minutes of it; a row links one transcript at most.
#[must_use]
pub fn attach(transcripts: &[CallTranscript], ends: &[(String, i64)]) -> HashMap<String, i64> {
    let mut linked: HashMap<String, (i64, i64)> = HashMap::new();
    for transcript in transcripts {
        let nearest = ends
            .iter()
            .map(|(row, at)| (row, (at - transcript.ended_at).abs()))
            .filter(|(_, apart)| *apart <= END_NEAR_MS)
            .min_by_key(|(_, apart)| *apart);
        if let Some((row, apart)) = nearest
            && linked.get(row).is_none_or(|(_, before)| apart < *before)
        {
            linked.insert(row.clone(), (transcript.id, apart));
        }
    }
    linked.into_iter().map(|(row, (id, _))| (row, id)).collect()
}

/// The key an utterance's lines are kept by in its transcript: the call's, then the speaker, track and audio time
/// the caption service continues it by (its result id when it gave no time).
#[must_use]
pub fn utterance(call: CallId, line: &Caption) -> String {
    if line.audio_timestamp == 0 {
        return format!("{}/{}", call.get(), line.id);
    }
    format!(
        "{}/{}/{}/{}",
        call.get(),
        line.speaker.as_deref().unwrap_or_default(),
        line.track_language,
        line.audio_timestamp
    )
}

/// The chat a call is in, for its transcript: the chat it was placed or joined in, else a 1:1 invitation's chat with
/// its caller. `None` for the test call, or a call whose chat isn't known.
#[must_use]
pub fn chat_of(active: &ActiveCall, me: &str) -> Option<String> {
    if active.kind == CallKind::TestCall {
        return None;
    }
    active.chat.clone().or_else(|| {
        let caller = active.caller.as_ref()?;
        teams_proto::one_to_one_id(me, caller.as_str())
    })
}

/// The call being saved.
struct Saving {
    call: CallId,
    conversation: String,
    title: Option<String>,
    /// When it connected, ms since the epoch: the transcript's start.
    connected_at: i64,
    /// Its captions were asked for (and how many times again after they failed).
    asked: bool,
    retries: u32,
    /// Its transcript, once the store opened it; whether that was asked.
    transcript: Option<i64>,
    opening: bool,
    /// Each utterance heard: when first, and the words last kept of it.
    heard: BTreeMap<String, (i64, Option<String>)>,
    /// Lines waiting for the transcript to be opened.
    waiting: Vec<TranscriptLine>,
}

/// One account's recorder (see the module docs).
pub struct Recorder {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    saving: Option<Saving>,
    _following: [Subscription; 2],
}

/// The recorders made, by account.
#[derive(Default)]
struct Recorders(HashMap<String, Entity<Recorder>>);

impl Global for Recorders {}

/// The recorder of `account`, once one was made.
#[must_use]
pub fn recorder_of(account: &str, cx: &App) -> Option<Entity<Recorder>> {
    cx.try_global::<Recorders>()?.0.get(account).cloned()
}

/// The recorder of the account `core` is (made once per account, whatever views ask for it).
pub fn follow(
    core: &Arc<dyn Core>,
    calls: &Entity<Calls>,
    settings: &Entity<SettingsModel>,
    trace: &Arc<ViewTrace>,
    cx: &mut App,
) -> Entity<Recorder> {
    let key = core.me().to_owned();
    if let Some(recorder) = cx.default_global::<Recorders>().0.get(&key) {
        return recorder.clone();
    }
    let recorder = cx.new(|cx| Recorder::new(Arc::clone(core), calls.clone(), settings.clone(), Arc::clone(trace), cx));
    cx.default_global::<Recorders>().0.insert(key, recorder.clone());
    recorder
}

impl Recorder {
    fn new(
        core: Arc<dyn Core>,
        calls: Entity<Calls>,
        settings: Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let following = [
            cx.observe(&calls, |this, _, cx| this.follow(cx)),
            cx.observe(&settings, |this, _, cx| this.follow(cx)),
        ];
        let mut recorder = Self {
            core,
            calls,
            settings,
            trace,
            saving: None,
            _following: following,
        };
        recorder.follow(cx);
        recorder
    }

    /// The call whose captions are being saved, while one is.
    #[must_use]
    pub fn saving(&self) -> Option<CallId> {
        self.saving.as_ref().map(|saving| saving.call)
    }

    /// The calls or the settings changed: the captions of the call up are saved as the setting says.
    fn follow(&mut self, cx: &mut Context<Self>) {
        // the read-only app turns nothing on: starting the captions is a request of its own
        let on = self.settings.read(cx).settings().save_call_captions
            && !self.core.read_only()
            && self.core.capabilities().captions;
        if self.calls.read(cx).saving_captions() != on {
            self.calls.update(cx, |calls, cx| calls.set_saving_captions(on, cx));
        }
        let me = self.core.me().to_owned();
        let active = self
            .calls
            .read(cx)
            .call()
            .filter(|active| !matches!(active.status, CallStatus::Ended(_)))
            .cloned();
        let up = active
            .as_ref()
            .filter(|active| active.status == CallStatus::Connected && on)
            .and_then(|active| Some((active, chat_of(active, &me)?)));
        if self
            .saving
            .as_ref()
            .is_some_and(|saving| up.as_ref().is_none_or(|(active, _)| active.id != saving.call))
        {
            self.stop(on, active.as_ref(), cx);
        }
        let Some((active, conversation)) = up else { return };
        if self.saving.is_none() {
            log::info!(
                "call transcript: saving the captions of call {} in {conversation}",
                active.id.get()
            );
            self.saving = Some(Saving {
                call: active.id,
                conversation,
                title: active.title.clone(),
                connected_at: chrono::Utc::now().timestamp_millis(),
                asked: false,
                retries: 0,
                transcript: None,
                opening: false,
                heard: BTreeMap::new(),
                waiting: Vec::new(),
            });
        }
        self.start_captions(cx);
        self.collect(cx);
    }

    /// Asks for the call's captions, once (and again a few times, apart, after they failed while their pane is hidden).
    fn start_captions(&mut self, cx: &mut Context<Self>) {
        let phase = self.calls.read(cx).captions().phase;
        let shown = self.calls.read(cx).captions_shown();
        let Some(saving) = self.saving.as_mut() else { return };
        if phase == teams_core::CaptionPhase::Failed && saving.asked && !shown && saving.retries < RETRIES {
            saving.retries += 1;
            let call = saving.call;
            cx.spawn(async move |this, cx: &mut AsyncApp| {
                cx.background_executor().timer(RETRY_AFTER).await;
                let _ = this.update(cx, |this, cx| this.retry(call, cx));
            })
            .detach();
            return;
        }
        if saving.asked {
            return;
        }
        saving.asked = true;
        if matches!(phase, teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting) {
            return;
        }
        calls::ask(&self.calls, &self.core, &self.trace, Ask::RecordCaptions(true), cx);
    }

    /// The captions of `call` are asked for again, when it is still the call saved.
    fn retry(&mut self, call: CallId, cx: &mut Context<Self>) {
        if let Some(saving) = self.saving.as_mut().filter(|saving| saving.call == call) {
            saving.asked = false;
            self.follow(cx);
        }
    }

    /// Keeps the final results not kept yet (or whose words changed).
    fn collect(&mut self, cx: &mut Context<Self>) {
        let lines = self.calls.read(cx).captions().lines.clone();
        let now = chrono::Utc::now().timestamp_millis();
        let Some(saving) = self.saving.as_mut() else { return };
        let mut kept = Vec::new();
        for line in &lines {
            let key = utterance(saving.call, line);
            let (heard, last) = saving.heard.entry(key.clone()).or_insert((now, None));
            let text = line.text.trim();
            if !line.final_result || text.is_empty() || last.as_deref() == Some(text) {
                continue;
            }
            *last = Some(text.to_owned());
            kept.push(TranscriptLine {
                utterance: key,
                speaker: line.speaker.clone(),
                speaker_name: line.display_name.clone(),
                started_at: *heard,
                ended_at: now.max(*heard),
                text: text.to_owned(),
            });
        }
        if kept.is_empty() {
            return;
        }
        if let Some(id) = saving.transcript {
            self.keep(id, kept, cx);
        } else {
            saving.waiting.extend(kept);
            self.open(cx);
        }
    }

    /// Asks the store for the call's transcript (once): the lines waiting go there once it is.
    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(saving) = self.saving.as_mut().filter(|saving| !saving.opening) else {
            return;
        };
        saving.opening = true;
        let call = saving.call;
        let rejoin_ms = i64::try_from(REJOIN_WITHIN.as_millis()).unwrap_or(i64::MAX);
        let reply = self.core.transcript(TranscriptWrite::Open {
            conversation: saving.conversation.clone(),
            title: saving.title.clone(),
            at: saving.connected_at,
            rejoin_ms,
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let opened = reply.await;
            let _ = this.update(cx, |this, cx| {
                let Some(saving) = this.saving.as_mut().filter(|saving| saving.call == call) else {
                    return;
                };
                saving.opening = false;
                match opened {
                    Ok(id) => {
                        saving.transcript = Some(id);
                        let waiting = std::mem::take(&mut saving.waiting);
                        this.keep(id, waiting, cx);
                    }
                    Err(error) => log::warn!("call transcript: the transcript couldn't be opened: {error}"),
                }
            });
        })
        .detach();
    }

    /// Keeps `lines` in transcript `id`; the views are told.
    fn keep(&self, id: i64, lines: Vec<TranscriptLine>, cx: &mut Context<Self>) {
        if lines.is_empty() {
            return;
        }
        let Some(conversation) = self.saving.as_ref().map(|saving| saving.conversation.clone()) else {
            return;
        };
        let count = lines.len();
        let reply = self.core.transcript(TranscriptWrite::Lines { id, lines });
        let account = self.core.me().to_owned();
        cx.spawn(async move |_, cx: &mut AsyncApp| match reply.await {
            Ok(_) => {
                log::debug!("call transcript: kept {count} lines");
                cx.update(|cx| changed(&account, &conversation, cx));
            }
            Err(error) => log::warn!("call transcript: {count} lines couldn't be kept: {error}"),
        })
        .detach();
    }

    /// The call saved ended, another came up or the setting went off: its end is kept, and its captions stopped when
    /// only the saving ran them (the setting turned off while it still runs, the pane hidden).
    fn stop(&mut self, on: bool, active: Option<&ActiveCall>, cx: &mut Context<Self>) {
        let Some(saving) = self.saving.take() else { return };
        log::info!(
            "call transcript: the captions of call {} are saved no more",
            saving.call.get()
        );
        let still_up = active.is_some_and(|active| active.id == saving.call);
        let calls = self.calls.read(cx);
        let running = matches!(
            calls.captions().phase,
            teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting
        );
        if still_up && !on && running && !calls.caption_pane_asked() && !calls.transcript_captions() {
            calls::ask(&self.calls, &self.core, &self.trace, Ask::RecordCaptions(false), cx);
        }
        let Some(id) = saving.transcript else { return };
        let reply = self.core.transcript(TranscriptWrite::End {
            id,
            at: chrono::Utc::now().timestamp_millis(),
        });
        let (account, conversation) = (self.core.me().to_owned(), saving.conversation);
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            if reply.await.is_ok() {
                cx.update(|cx| changed(&account, &conversation, cx));
            }
        })
        .detach();
    }
}

/// Deletes `account`'s saved transcript `id` of `conversation` (the user asked, in its recap): the views are told once
/// it is gone.
pub fn delete(core: &Arc<dyn Core>, conversation: String, id: i64, cx: &mut App) {
    let reply = core.transcript(TranscriptWrite::Delete { id });
    let account = core.me().to_owned();
    cx.spawn(async move |cx: &mut AsyncApp| match reply.await {
        Ok(_) => cx.update(|cx| changed(&account, &conversation, cx)),
        Err(error) => log::warn!("call transcript: the transcript couldn't be deleted: {error}"),
    })
    .detach();
}
