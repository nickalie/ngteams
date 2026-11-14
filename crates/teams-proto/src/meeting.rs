//! What joining a meeting reads beyond its calendar event (`docs/protocol/calling.md`, "Joining a meeting").
//!
//! - [`MeetingThread`]: the meeting's chat as the chat service keeps it, `GET v1/threads/<chat>?view=msnp24Equivalent`
//!   (the web client's read of a meeting's thread, `ab-meeting-join`). Its `properties.meeting` is a JSON object as a
//!   string naming the meeting's organizer and tenant (`organizerId`, `tenantId`), its title (`subject`) and its join
//!   link (`meetingJoinUrl`): what a meeting whose link names no organizer (a short `/meet/<code>` one) or whose chat is
//!   opened without its event is joined by.
//! - [`MeetingCode`]: a meeting's id and passcode, as Teams' "Join with an ID" takes them or a short join link carries
//!   them (`https://teams.microsoft.com/meet/<id>?p=<passcode>`).

use serde_json::Value;

use crate::endpoint::{Endpoint, Service};
use crate::read::Read;
use crate::write::MeetingInfo;

/// `GET v1/threads/<chat>?view=msnp24Equivalent` on the chat service: a meeting's chat, read for the meeting it is
/// ([`MeetingThread::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingThread {
    pub chat: String,
}

/// A meeting as its chat's thread names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingDetails {
    /// Its organizer and tenant.
    pub meeting: MeetingInfo,
    /// Its title, when it has one.
    pub title: Option<String>,
    /// Its join link, when the thread names one.
    pub join_url: Option<String>,
}

impl MeetingThread {
    /// The meeting the answer `body` names (`properties.meeting`); `None` for a thread that names none, or names it
    /// without a work account's organizer or a tenant (both GUIDs).
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<MeetingDetails> {
        Self::read_answer_in(&crate::Cloud::Microsoft, body)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn read_answer_in(cloud: &crate::Cloud, body: &[u8]) -> Option<MeetingDetails> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        // the batch read (`v1/threads?threadIds=`) answers a list of threads; the single one, the thread itself
        let thread = answer.as_array().and_then(|threads| threads.first()).unwrap_or(&answer);
        let meeting = thread.pointer("/properties/meeting")?;
        let meeting: Value = match meeting {
            Value::String(text) => serde_json::from_str(text).ok()?,
            Value::Object(_) => meeting.clone(),
            _ => return None,
        };
        let text = |key: &str| {
            meeting
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let guid = |key: &str| text(key).map(|id| id.to_ascii_lowercase()).filter(|id| is_guid(id));
        Some(MeetingDetails {
            meeting: MeetingInfo {
                organizer_id: guid("organizerId")?,
                tenant_id: guid("tenantId")?,
            },
            title: text("subject"),
            join_url: text("meetingJoinUrl").filter(|url| meeting_link_in(cloud, url)),
        })
    }
}

impl Read for MeetingThread {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", self.chat.as_str()]).with_query("view", "msnp24Equivalent")
    }
}

/// A meeting's id and passcode (Teams' "Join with an ID").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeetingCode {
    /// Its id: digits, spaces taken out.
    pub id: String,
    /// Its passcode, as given.
    pub passcode: String,
}

impl MeetingCode {
    /// A meeting's id as typed (digits, spaces between groups of them, as Teams shows it: `285 592 526 889 78`) and its
    /// passcode; `None` for an id that isn't one (too short, or anything but digits) or an empty passcode.
    #[must_use]
    pub fn typed(id: &str, passcode: &str) -> Option<Self> {
        let id: String = id.chars().filter(|c| !c.is_whitespace()).collect();
        let passcode = passcode.trim();
        let valid = (MIN_ID_DIGITS..=MAX_ID_DIGITS).contains(&id.len())
            && id.chars().all(|c| c.is_ascii_digit())
            && !passcode.is_empty()
            && passcode.chars().all(|c| c.is_ascii_alphanumeric());
        valid.then(|| Self {
            id,
            passcode: passcode.to_owned(),
        })
    }

    /// The id and passcode a short join link carries: `https://teams.microsoft.com/meet/<id>?p=<passcode>`.
    #[must_use]
    pub fn of_link(url: &str) -> Option<Self> {
        Self::of_link_in(&crate::Cloud::Microsoft, url)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn of_link_in(cloud: &crate::Cloud, url: &str) -> Option<Self> {
        if !cloud.is_microsoft() && !meeting_link_in(cloud, url) {
            return None;
        }
        let parsed = url::Url::parse(url).ok()?;
        let mut segments = parsed.path_segments()?;
        if segments.next()? != "meet" {
            return None;
        }
        let id = segments.next()?;
        let (_, passcode) = parsed.query_pairs().find(|(key, _)| key == "p")?;
        Self::typed(id, &passcode)
    }
}

pub(crate) fn meeting_link_in(cloud: &crate::Cloud, value: &str) -> bool {
    if cloud.is_microsoft() {
        return value.starts_with("https://");
    }
    url::Url::parse(value).is_ok_and(|url| {
        cloud.contains(&url) && (url.path().starts_with("/meet/") || url.path().starts_with("/l/meetup-join/"))
    })
}

/// The shortest meeting id Teams takes.
const MIN_ID_DIGITS: usize = 9;
/// The longest.
const MAX_ID_DIGITS: usize = 20;

fn is_guid(text: &str) -> bool {
    text.len() == 36
        && text.chars().enumerate().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000b";
    const TENANT: &str = "aaaaaaaa-0000-4000-8000-0000000000f1";

    fn thread(meeting: &Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "id": "19:meeting_MadeUp@thread.v2",
            "properties": {"meeting": meeting.to_string(), "threadType": "meeting"},
        }))
        .unwrap_or_default()
    }

    #[test]
    fn a_meeting_thread_names_its_organizer_title_and_link() {
        let body = thread(&serde_json::json!({
            "subject": "Made-up sync",
            "organizerId": ORGANIZER.to_uppercase(),
            "tenantId": TENANT,
            "meetingJoinUrl": "https://teams.microsoft.com/meet/123456789012?p=MadeUp",
            "meetingType": "Scheduled",
        }));
        assert_eq!(
            MeetingThread::read_answer(&body),
            Some(MeetingDetails {
                meeting: MeetingInfo {
                    organizer_id: ORGANIZER.to_owned(),
                    tenant_id: TENANT.to_owned(),
                },
                title: Some("Made-up sync".to_owned()),
                join_url: Some("https://teams.microsoft.com/meet/123456789012?p=MadeUp".to_owned()),
            })
        );
        // the batch read's list
        let list = format!("[{}]", String::from_utf8_lossy(&body));
        assert!(MeetingThread::read_answer(list.as_bytes()).is_some());
    }

    #[test]
    fn a_thread_without_a_readable_meeting_names_none() {
        for meeting in [
            serde_json::json!({"organizerId": ORGANIZER}),
            serde_json::json!({"organizerId": "not a guid", "tenantId": TENANT}),
            serde_json::json!({"organizerId": ORGANIZER, "tenantId": ""}),
        ] {
            assert_eq!(MeetingThread::read_answer(&thread(&meeting)), None, "{meeting}");
        }
        assert_eq!(MeetingThread::read_answer(br#"{"properties": {}}"#), None);
        assert_eq!(
            MeetingThread::read_answer(br#"{"properties": {"meeting": "not json"}}"#),
            None
        );
        assert_eq!(MeetingThread::read_answer(b"not json"), None);
        // the link is kept only when it is one
        let body = thread(&serde_json::json!({"organizerId": ORGANIZER, "tenantId": TENANT, "meetingJoinUrl": "x"}));
        assert_eq!(
            MeetingThread::read_answer(&body).and_then(|details| details.join_url),
            None
        );
    }

    #[test]
    fn the_thread_is_read_from_the_chat_service() {
        let read = MeetingThread {
            chat: "19:meeting_MadeUp@thread.v2".to_owned(),
        };
        assert_eq!(read.service(), Service::ChatService);
        let endpoint = read.endpoint();
        assert_eq!(
            endpoint,
            Endpoint::new(["v1", "threads", "19:meeting_MadeUp@thread.v2"]).with_query("view", "msnp24Equivalent")
        );
    }

    #[test]
    fn a_meeting_id_is_taken_as_teams_shows_it() {
        assert_eq!(
            MeetingCode::typed(" 285 592 526 889 78 ", " MadeUp9 "),
            Some(MeetingCode {
                id: "28559252688978".to_owned(),
                passcode: "MadeUp9".to_owned(),
            })
        );
        for (id, passcode) in [
            ("12345", "x"),
            ("12345678901a", "x"),
            ("123456789012", ""),
            ("123456789012", "a b"),
        ] {
            assert_eq!(MeetingCode::typed(id, passcode), None, "{id} {passcode}");
        }
    }

    #[test]
    fn a_short_join_link_carries_its_id_and_passcode() {
        assert_eq!(
            MeetingCode::of_link("https://teams.microsoft.com/meet/123456789012?p=MadeUp"),
            MeetingCode::typed("123456789012", "MadeUp")
        );
        for url in [
            "https://teams.microsoft.com/meet/123456789012",
            "https://teams.microsoft.com/l/meetup-join/x/0",
            "not a link",
        ] {
            assert_eq!(MeetingCode::of_link(url), None, "{url}");
        }
    }
}
