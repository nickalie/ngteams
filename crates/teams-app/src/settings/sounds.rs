//! The sounds' settings: which sound plays for each event ([`SoundEvent`]), how loud, and the pack they come from
//! ([`SoundSource`]: one of the app's, [`BUILTIN_PACKS`], or one the user imported), as the settings file keeps them.
//! The desktop (`teams-ui`'s `sounds`) makes and plays the sounds.
//!
//! - **Quiet while busy**: "Play sounds when I'm busy or in a meeting" off keeps [notification](SoundEvent::notifies)
//!   sounds and the ringtone quiet in those states; a call's own cues still play ([`allowed`]).

use std::collections::BTreeMap;

use toml::{Table, Value};

use super::{ManualStatus, Problem, Settings, keys};

/// The key of the app's default pack.
pub const DEFAULT_PACK: &str = "default";
/// The keys of the app's own packs, in the order the settings list them.
pub const BUILTIN_PACKS: [&str; 9] = [
    DEFAULT_PACK,
    "chimes",
    "retro",
    "marimba",
    "glass",
    "organ",
    "soft",
    "arcade",
    "big-beat",
];
/// The longest id of a pack the user imported, in bytes.
pub const MAX_PACK_ID: usize = 72;

/// Whether `id` could be a pack's id: 1 to [`MAX_PACK_ID`] lowercase ASCII letters, digits and dashes, starting with a
/// letter or a digit. Nothing else ever becomes a directory's name.
#[must_use]
pub fn is_pack_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PACK_ID
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The highest volume, in percent.
pub const MAX_VOLUME: u8 = 100;
/// One step of an event's volume stepper, in percent.
pub const VOLUME_STEP: u8 = 10;
/// A roster larger than this makes no sound as people come and go.
pub const LARGE_CALL: usize = 15;

/// Something the app makes a sound for: every sound Teams has and more besides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SoundEvent {
    Message,
    Mention,
    ChannelMessage,
    MissedCall,
    IncomingCall,
    OutgoingCall,
    CallJoined,
    CallEnded,
    Reconnecting,
    Reconnected,
    PersonJoined,
    PersonLeft,
    LobbyRequest,
    OnHold,
    Resumed,
    RecordingStarted,
    RecordingStopped,
    ScreenShareStarted,
    ScreenShareEnded,
    HandRaised,
    LiveReaction,
    MicMuted,
    MicUnmuted,
}

impl SoundEvent {
    pub const ALL: [Self; 23] = [
        Self::Message,
        Self::Mention,
        Self::ChannelMessage,
        Self::MissedCall,
        Self::IncomingCall,
        Self::OutgoingCall,
        Self::CallJoined,
        Self::CallEnded,
        Self::Reconnecting,
        Self::Reconnected,
        Self::PersonJoined,
        Self::PersonLeft,
        Self::LobbyRequest,
        Self::OnHold,
        Self::Resumed,
        Self::RecordingStarted,
        Self::RecordingStopped,
        Self::ScreenShareStarted,
        Self::ScreenShareEnded,
        Self::HandRaised,
        Self::LiveReaction,
        Self::MicMuted,
        Self::MicUnmuted,
    ];

    /// Its name in the settings file and a pack's manifest.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Mention => "mention",
            Self::ChannelMessage => "channel-message",
            Self::MissedCall => "missed-call",
            Self::IncomingCall => "incoming-call",
            Self::OutgoingCall => "outgoing-call",
            Self::CallJoined => "call-joined",
            Self::CallEnded => "call-ended",
            Self::Reconnecting => "reconnecting",
            Self::Reconnected => "reconnected",
            Self::PersonJoined => "person-joined",
            Self::PersonLeft => "person-left",
            Self::LobbyRequest => "lobby-request",
            Self::OnHold => "on-hold",
            Self::Resumed => "resumed",
            Self::RecordingStarted => "recording-started",
            Self::RecordingStopped => "recording-stopped",
            Self::ScreenShareStarted => "screen-share-started",
            Self::ScreenShareEnded => "screen-share-ended",
            Self::HandRaised => "hand-raised",
            Self::LiveReaction => "live-reaction",
            Self::MicMuted => "mic-muted",
            Self::MicUnmuted => "mic-unmuted",
        }
    }

    /// Its picker's test selector and element id; each of its rows' is `<this>-<key>`.
    #[must_use]
    pub fn picker(self) -> &'static str {
        match self {
            Self::Message => "sound-message",
            Self::Mention => "sound-mention",
            Self::ChannelMessage => "sound-channel-message",
            Self::MissedCall => "sound-missed-call",
            Self::IncomingCall => "sound-incoming-call",
            Self::OutgoingCall => "sound-outgoing-call",
            Self::CallJoined => "sound-call-joined",
            Self::CallEnded => "sound-call-ended",
            Self::Reconnecting => "sound-reconnecting",
            Self::Reconnected => "sound-reconnected",
            Self::PersonJoined => "sound-person-joined",
            Self::PersonLeft => "sound-person-left",
            Self::LobbyRequest => "sound-lobby-request",
            Self::OnHold => "sound-on-hold",
            Self::Resumed => "sound-resumed",
            Self::RecordingStarted => "sound-recording-started",
            Self::RecordingStopped => "sound-recording-stopped",
            Self::ScreenShareStarted => "sound-screen-share-started",
            Self::ScreenShareEnded => "sound-screen-share-ended",
            Self::HandRaised => "sound-hand-raised",
            Self::LiveReaction => "sound-live-reaction",
            Self::MicMuted => "sound-mic-muted",
            Self::MicUnmuted => "sound-mic-unmuted",
        }
    }

    /// What the settings call it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Message => "New messages",
            Self::Mention => "Mentions of you",
            Self::ChannelMessage => "Channel posts",
            Self::MissedCall => "Missed calls",
            Self::IncomingCall => "Incoming calls",
            Self::OutgoingCall => "Calling someone",
            Self::CallJoined => "You join a call",
            Self::CallEnded => "A call ends",
            Self::Reconnecting => "Call connection lost",
            Self::Reconnected => "Call connection back",
            Self::PersonJoined => "Someone joins",
            Self::PersonLeft => "Someone leaves",
            Self::LobbyRequest => "Requests to join",
            Self::OnHold => "You are put on hold",
            Self::Resumed => "Your call resumes",
            Self::RecordingStarted => "Recording starts",
            Self::RecordingStopped => "Recording stops",
            Self::ScreenShareStarted => "Someone shares a screen",
            Self::ScreenShareEnded => "Someone stops sharing",
            Self::HandRaised => "Someone raises a hand",
            Self::LiveReaction => "Someone reacts in a call",
            Self::MicMuted => "You mute",
            Self::MicUnmuted => "You unmute",
        }
    }

    /// Whether it plays over and over until it is stopped, rather than once.
    #[must_use]
    pub fn loops(self) -> bool {
        matches!(self, Self::IncomingCall | Self::OutgoingCall)
    }

    /// Whether it is a notification's or the ringtone's, kept quiet while busy if the user asked
    /// ([`SoundSettings::while_busy`]); the others are the call's own.
    #[must_use]
    pub fn notifies(self) -> bool {
        matches!(
            self,
            Self::Message | Self::Mention | Self::ChannelMessage | Self::MissedCall | Self::IncomingCall
        )
    }

    /// Whether it is in the notifications' list of the settings, rather than the calls'.
    #[must_use]
    pub fn is_notification(self) -> bool {
        matches!(
            self,
            Self::Message | Self::Mention | Self::ChannelMessage | Self::MissedCall
        )
    }

    /// Whether it plays until the user turns it off. What a busy meeting does often starts off.
    #[must_use]
    pub fn on_by_default(self) -> bool {
        !matches!(
            self,
            Self::RecordingStopped
                | Self::ScreenShareStarted
                | Self::ScreenShareEnded
                | Self::HandRaised
                | Self::LiveReaction
                | Self::MicMuted
                | Self::MicUnmuted
        )
    }

    /// The event named `key` in the settings file.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.key() == key)
    }
}

/// Where a sound comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SoundSource {
    /// One of the app's packs, by its key ([`BUILTIN_PACKS`]).
    Builtin(&'static str),
    /// A pack the user imported, by its id ([`is_pack_id`]).
    Imported(String),
}

impl Default for SoundSource {
    fn default() -> Self {
        Self::Builtin(DEFAULT_PACK)
    }
}

impl SoundSource {
    /// Its name in the file: `builtin/<key>` or `added/<id>`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Builtin(key) => format!("builtin/{key}"),
            Self::Imported(id) => format!("added/{id}"),
        }
    }

    /// The source of a `key`, if it could be one.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        if let Some(builtin) = key.strip_prefix("builtin/") {
            return BUILTIN_PACKS.into_iter().find(|key| *key == builtin).map(Self::Builtin);
        }
        key.strip_prefix("added/")
            .filter(|id| is_pack_id(id))
            .map(|id| Self::Imported(id.to_owned()))
    }
}

/// One event's sound as the user set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSound {
    pub on: bool,
    /// In percent, up to [`MAX_VOLUME`].
    pub volume: u8,
    /// The pack whose sound it plays; `None`: the chosen pack's.
    pub sound: Option<SoundSource>,
}

impl EventSound {
    /// The event as the app starts it.
    #[must_use]
    pub fn default_of(event: SoundEvent) -> Self {
        Self {
            on: event.on_by_default(),
            volume: MAX_VOLUME,
            sound: None,
        }
    }
}

/// The sounds' settings, the app's for every account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundSettings {
    /// The pack chosen as a whole.
    pub pack: SoundSource,
    /// "Play sounds when I'm busy or in a meeting".
    pub while_busy: bool,
    /// Every sound's volume, in percent up to [`MAX_VOLUME`], on top of each event's own.
    pub volume: u8,
    /// The events the user changed; the others are as [`EventSound::default_of`] says.
    events: BTreeMap<SoundEvent, EventSound>,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            pack: SoundSource::default(),
            while_busy: true,
            volume: MAX_VOLUME,
            events: BTreeMap::new(),
        }
    }
}

impl SoundSettings {
    /// `event`'s sound.
    #[must_use]
    pub fn event(&self, event: SoundEvent) -> EventSound {
        self.events
            .get(&event)
            .cloned()
            .unwrap_or_else(|| EventSound::default_of(event))
    }

    /// Changes `event`'s sound; one that ends as the default isn't kept.
    pub fn change_event(&mut self, event: SoundEvent, change: impl FnOnce(&mut EventSound)) {
        let mut sound = self.event(event);
        change(&mut sound);
        sound.volume = sound.volume.min(MAX_VOLUME);
        if sound == EventSound::default_of(event) {
            self.events.remove(&event);
        } else {
            self.events.insert(event, sound);
        }
    }

    /// Forgets a pack the user removed: chosen or picked for an event, the default takes its place.
    pub fn forget_pack(&mut self, removed: &SoundSource) {
        if self.pack == *removed {
            self.pack = SoundSource::default();
        }
        for event in SoundEvent::ALL {
            if self.event(event).sound.as_ref() == Some(removed) {
                self.change_event(event, |sound| sound.sound = None);
            }
        }
    }
}

/// Whether `event` may sound now for `account`, as the settings say (`in_call`: the account is in a call).
#[must_use]
pub fn allowed(settings: &Settings, event: SoundEvent, account: &str, in_call: bool) -> bool {
    let sounds = &settings.sounds;
    if !sounds.event(event).on {
        return false;
    }
    let busy = in_call
        || matches!(
            settings.manual_status_of(account),
            ManualStatus::Busy | ManualStatus::DoNotDisturb
        );
    sounds.while_busy || !event.notifies() || !busy
}

/// What a sound setting that can't be read isn't.
const SOUNDS_SHAPE: &str = "not a table of events, each with on, a volume from 0 to 100 and a sound";

/// Takes one of the sounds' settings from the file.
pub(super) fn take(settings: &mut SoundSettings, key: &str, value: &Value) -> Result<(), Problem> {
    match key {
        keys::SOUND_PACK => {
            settings.pack = value
                .as_str()
                .and_then(SoundSource::from_key)
                .ok_or(Problem::BadValue {
                    key: keys::SOUND_PACK,
                    why: "not a sound pack: builtin/<a pack of the app's> or added/<an id>",
                })?;
        }
        keys::SOUNDS_VOLUME => {
            settings.volume = value
                .as_integer()
                .and_then(|volume| u8::try_from(volume).ok())
                .filter(|volume| *volume <= MAX_VOLUME)
                .ok_or(Problem::BadValue {
                    key: keys::SOUNDS_VOLUME,
                    why: "not a whole percentage from 0 to 100",
                })?;
        }
        keys::SOUNDS_WHILE_BUSY => {
            settings.while_busy = value.as_bool().ok_or(Problem::BadValue {
                key: keys::SOUNDS_WHILE_BUSY,
                why: "not true or false",
            })?;
        }
        _ => take_events(settings, value)?,
    }
    Ok(())
}

/// Takes the events' sounds: each a table under the event's name. One that is bad is left at its default, the others
/// are taken; an event this build doesn't know counts as bad.
fn take_events(settings: &mut SoundSettings, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::SOUNDS,
        why: SOUNDS_SHAPE,
    };
    let table = value.as_table().ok_or(bad.clone())?;
    let mut failed = false;
    for (name, kept) in table {
        let taken = SoundEvent::from_key(name)
            .zip(kept.as_table())
            .and_then(|(event, kept)| {
                let mut sound = EventSound::default_of(event);
                for (key, value) in kept {
                    match key.as_str() {
                        "on" => sound.on = value.as_bool()?,
                        "volume" => {
                            sound.volume = value
                                .as_integer()
                                .and_then(|volume| u8::try_from(volume).ok())
                                .filter(|volume| *volume <= MAX_VOLUME)?;
                        }
                        "sound" => sound.sound = Some(value.as_str().and_then(SoundSource::from_key)?),
                        _ => return None,
                    }
                }
                Some((event, sound))
            });
        match taken {
            Some((event, sound)) => settings.change_event(event, |kept| *kept = sound),
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// The sounds' settings, into the file's table (the events only as they differ from their defaults).
pub(super) fn render(settings: &SoundSettings, table: &mut Table) {
    table.insert(keys::SOUND_PACK.to_owned(), Value::String(settings.pack.key()));
    table.insert(keys::SOUNDS_WHILE_BUSY.to_owned(), Value::Boolean(settings.while_busy));
    table.insert(
        keys::SOUNDS_VOLUME.to_owned(),
        Value::Integer(i64::from(settings.volume)),
    );
    let events: Table = settings
        .events
        .iter()
        .map(|(event, sound)| {
            let mut kept = Table::new();
            kept.insert("on".to_owned(), Value::Boolean(sound.on));
            kept.insert("volume".to_owned(), Value::Integer(i64::from(sound.volume)));
            if let Some(source) = &sound.sound {
                kept.insert("sound".to_owned(), Value::String(source.key()));
            }
            (event.key().to_owned(), Value::Table(kept))
        })
        .collect();
    if !events.is_empty() {
        table.insert(keys::SOUNDS.to_owned(), Value::Table(events));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings;

    #[test]
    fn events_keep_their_names_and_defaults() {
        for event in SoundEvent::ALL {
            assert_eq!(SoundEvent::from_key(event.key()), Some(event));
            assert!(event.picker().ends_with(event.key()));
            assert!(!event.label().is_empty());
        }
        assert!(SoundEvent::IncomingCall.loops() && !SoundEvent::Message.loops());
        assert!(!SoundEvent::MicMuted.on_by_default() && SoundEvent::Message.on_by_default());
    }

    #[test]
    fn a_source_reads_back_from_its_key() {
        for source in [SoundSource::default(), SoundSource::Imported("my-pack".to_owned())] {
            assert_eq!(SoundSource::from_key(&source.key()), Some(source));
        }
        for bad in ["builtin/nope", "added/../evil", "added/", "other/x", ""] {
            assert_eq!(SoundSource::from_key(bad), None);
        }
    }

    #[test]
    fn an_event_left_at_its_default_is_not_kept() {
        let mut sounds = SoundSettings::default();
        sounds.change_event(SoundEvent::Message, |sound| sound.volume = 40);
        assert_eq!(sounds.event(SoundEvent::Message).volume, 40);
        sounds.change_event(SoundEvent::Message, |sound| sound.volume = 200);
        assert_eq!(sounds.event(SoundEvent::Message).volume, MAX_VOLUME);
        assert!(sounds.events.is_empty());
    }

    #[test]
    fn a_removed_pack_is_forgotten_everywhere() {
        let gone = SoundSource::Imported("gone".to_owned());
        let mut sounds = SoundSettings {
            pack: gone.clone(),
            ..SoundSettings::default()
        };
        sounds.change_event(SoundEvent::Mention, |sound| sound.sound = Some(gone.clone()));
        sounds.forget_pack(&gone);
        assert_eq!(sounds, SoundSettings::default());
    }

    #[test]
    fn the_sounds_round_trip_through_the_file() {
        let mut settings = Settings::default();
        settings.sounds.pack = SoundSource::Imported("my-pack".to_owned());
        settings.sounds.while_busy = false;
        settings.sounds.volume = 40;
        settings.sounds.change_event(SoundEvent::IncomingCall, |sound| {
            sound.volume = 70;
            sound.sound = Some(SoundSource::Builtin("chimes"));
        });
        settings
            .sounds
            .change_event(SoundEvent::Message, |sound| sound.on = false);
        let (read, problems) = settings::parse(&settings::render(&settings)).expect("valid TOML");
        assert_eq!(problems, Vec::new());
        assert_eq!(read.sounds, settings.sounds);
    }

    #[test]
    fn a_bad_sound_setting_keeps_its_default_and_takes_the_others() {
        let text = r#"
            sound_pack = "added/../../evil"
            sounds_while_busy = "yes"
            [sounds.message]
            volume = 30
            [sounds.mention]
            volume = 900
            [sounds.not-an-event]
            on = true
        "#;
        let (read, problems) = settings::parse(text).expect("valid TOML");
        assert_eq!(read.sounds.pack, SoundSource::default());
        assert!(read.sounds.while_busy);
        assert_eq!(read.sounds.event(SoundEvent::Message).volume, 30);
        assert_eq!(read.sounds.event(SoundEvent::Mention).volume, MAX_VOLUME);
        assert_eq!(problems.len(), 3);
        assert_eq!(settings::parse("sounds = 3").expect("valid TOML").1.len(), 1);
        assert_eq!(settings::parse("sounds_volume = 300").expect("valid TOML").1.len(), 1);
    }

    #[test]
    fn busy_keeps_only_notifications_quiet_and_only_when_asked() {
        let mut settings = Settings::default();
        assert!(allowed(&settings, SoundEvent::Message, "me", true), "on by default");
        settings.sounds.while_busy = false;
        assert!(allowed(&settings, SoundEvent::Message, "me", false));
        assert!(!allowed(&settings, SoundEvent::Message, "me", true));
        assert!(!allowed(&settings, SoundEvent::IncomingCall, "me", true));
        assert!(allowed(&settings, SoundEvent::CallEnded, "me", true), "the call's own");
        settings.set_manual_status("me", ManualStatus::DoNotDisturb);
        assert!(!allowed(&settings, SoundEvent::Mention, "me", false));
        settings.set_manual_status("me", ManualStatus::Busy);
        assert!(!allowed(&settings, SoundEvent::Mention, "me", false));
        settings.set_manual_status("me", ManualStatus::Away);
        assert!(allowed(&settings, SoundEvent::Mention, "me", false));
        settings
            .sounds
            .change_event(SoundEvent::Mention, |sound| sound.on = false);
        assert!(!allowed(&settings, SoundEvent::Mention, "me", false), "off is off");
    }
}
