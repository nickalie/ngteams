//! The fixture guard: scans files meant for the repository (synthetic fixtures, docs) and `proto-lab export`'s local
//! reference copies for what must never be committed, such as JWTs, `Bearer` and skypetoken values, OAuth tokens,
//! email addresses, real MRIs, GUIDs (other than Microsoft's public app ids), signed URLs' signatures, and Teams
//! meetings' join credentials (a join link's code or `p` passcode, a meeting id or a passcode under its key or label,
//! in any language Teams writes an invitation in: [`labels`]), and a call's network data (a public IP address, an ICE
//! username fragment or password, a DTLS fingerprint, a TURN credential: [`network`]). It is the backstop behind the
//! sanitizer and behind hand-written fixtures: what either lets through fails instead of being committed. Every
//! committed fixture also has to declare itself synthetic ([`synthetic`]).
//!
//! Each line is scanned twice: as written, and decoded the way captures hide text, layer after layer: `%XX` percent
//! escapes (`8%3Aorgid%3A…`, `someone%40contoso.com`), backslash escapes of a JSON or JavaScript string (`\"`,
//! `\u0026`, `\x26`) and HTML character references (`&amp;`, `&#38;`, `&#x26;`, `&nbsp;`).
//!
//! Sanitized placeholders (`<user:a>`, `<mri:b>`, `<guid:1>`, `<token:1>`) and prose that only names these things pass.
//! A finding reports where and what, never the matched value, so the report itself cannot leak it.
//!
//! The repo-wide checks are in `tests/fixture_guard.rs` (the scan on every `tests/fixtures/` of the workspace and on
//! `docs/`, the synthetic marker on those fixture dirs: [`synthetic::fixture_dirs`]); `proto-lab export` (M0 Task 9)
//! runs the same scan on a reference copy and warns about what it finds (it refuses the copy only under `--strict`).

use std::fmt;
use std::fs;
use std::io;
use std::iter;
use std::path::{Path, PathBuf};

use regex::Regex;

pub mod labels;
pub mod network;
pub mod synthetic;

use self::labels::{Label, Labels, is_passcode_value};
use self::network::Network;

/// What a finding looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A JSON Web Token (`eyJ…`.`…`.`…`); Entra access and id tokens and skypetokens are JWTs.
    Jwt,
    /// An `Authorization: Bearer` value.
    Bearer,
    /// A skypetoken value, in a header (`X-Skypetoken`, `skypetoken=`) or a JSON field.
    Skypetoken,
    /// An OAuth token value under an `access`/`refresh`/`id` token key in any spelling (`refresh_token`,
    /// `refreshToken`, `x-refresh-token`), or a `secret` value (the MSAL cache keeps opaque refresh tokens there).
    OauthToken,
    /// An email address outside the reserved `example.com`/`.org`/`.net` domains.
    Email,
    /// A real user or bot MRI (`8:orgid:<guid>`, `8:live:<name>`, `28:<guid>`, …).
    Mri,
    /// A GUID (a tenant, user, object or conversation id) other than Microsoft's own public app ids
    /// ([`PUBLIC_GUIDS`]); the sanitizer makes every GUID `<guid:N>`.
    Guid,
    /// A signed URL's signature (`sig=`, `signature=`, `X-Amz-Signature:`, …) of 16 characters or more.
    Signature,
    /// A Teams meeting join link that still holds its code (`/meet/1234567890123`, also under `/v2/meet/`, `/lm/meet/`)
    /// or a passcode (`p=` in the query of a `/meet/`, `/l/meetup-join/` or `/l/meetnow-join` link), on any host.
    MeetingLink,
    /// A meeting's code (6 digits or more, in groups or not) after its label in any language Teams writes an
    /// invitation in (`Meeting ID: 355 123 456 789`, `Besprechungs-ID: …`, `"conferenceId":"…"`; [`labels`]).
    MeetingId,
    /// A meeting passcode under its key or label in any of those languages (`"passcode":"…"`, `meetingPw=…`, an
    /// invitation's `Passcode: …` or `Code secret : …`, HTML tags between): 4 to 64 letters and digits of any mix,
    /// other than a literal or a setting ([`labels::NOT_A_PASSCODE`]).
    Passcode,
    /// A public IPv4 or IPv6 address, outside the documentation ranges and the ranges no public host has
    /// ([`network`]).
    IpAddress,
    /// An ICE username fragment or password (`a=ice-ufrag:`, `a=ice-pwd:`, a candidate string's `ufrag …`, or under a
    /// JSON key).
    IceCredential,
    /// A DTLS certificate fingerprint (`a=fingerprint:sha-256 …`): 20 colon-separated hex bytes or more.
    DtlsFingerprint,
    /// A TURN credential: a `password` or `credential` value, or a relay's `username` (a `trap/tokens` answer, an ICE
    /// server).
    TurnCredential,
}

/// The kinds [`Network`] finds.
const NETWORK_KINDS: [Kind; 4] = [
    Kind::IpAddress,
    Kind::IceCredential,
    Kind::DtlsFingerprint,
    Kind::TurnCredential,
];

/// GUIDs that may appear anywhere: Microsoft's first-party app ids, the same in every tenant and published in
/// Microsoft's own documentation and clients (the protocol notes name them), and the nil GUID. Nothing else: a tenant,
/// user or object id is never public.
pub const PUBLIC_GUIDS: [&str; 6] = [
    // Teams, native (desktop and mobile) client
    "1fec8e78-bce4-4aaf-ab1b-5451cc387264",
    // Teams, web client
    "5e3ce6c0-2b1f-4285-8d4b-75ee78787346",
    // Teams, consumer (MSA) client
    "8ec6bc83-69c8-4392-8f08-b3c986009232",
    // Teams meeting artifacts app
    "6bc3b958-689b-49f5-9006-36d165f30e00",
    // Substrate search: the resource of the `https://outlook.office.com/search` token (docs/spikes/auth.md)
    "66a88757-258c-4c72-893c-3e8bed4d6899",
    "00000000-0000-0000-0000-000000000000",
];

/// One hit: a file, a 1-based line, and what was found there. It never carries the matched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub kind: Kind,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {:?}", self.path.display(), self.line, self.kind)
    }
}

const GUID: &str = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
/// A token-shaped value: 20 or more URL-safe base64/JWT characters. Placeholders start with `<`, so they never match.
const VALUE: &str = r"[A-Za-z0-9._\-]{20,}";
/// The shortest meeting code in a link.
const MEETING_CODE_MIN: usize = 6;
/// Reserved documentation domains (RFC 2606), which a fixture or doc may use freely.
const RESERVED_DOMAINS: [&str; 3] = ["example.com", "example.org", "example.net"];

/// The compiled patterns. Build one with [`Guard::new`] and reuse it.
#[derive(Debug)]
pub struct Guard {
    rules: Vec<(Kind, Regex)>,
    /// The values after labels: [`Kind::MeetingId`] and [`Kind::Passcode`].
    labels: Labels,
    /// A call's network data: the [`NETWORK_KINDS`].
    network: Network,
}

impl Guard {
    /// Compiles the patterns.
    ///
    /// # Errors
    /// A pattern fails to compile; that is a bug in this crate, caught by its tests.
    pub fn new() -> Result<Self, regex::Error> {
        let patterns = [
            (
                Kind::Jwt,
                r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*".to_owned(),
            ),
            (Kind::Bearer, r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]{20,}".to_owned()),
            (
                Kind::Skypetoken,
                format!(r#"(?i)skypetoken["']?\s*[:=]\s*["']?{VALUE}"#),
            ),
            (
                Kind::OauthToken,
                format!(r#"(?i)(?:(?:access|refresh|id)[_-]?token|secret)["']?\s*[:=]\s*["']?{VALUE}"#),
            ),
            (
                Kind::Email,
                r"[A-Za-z0-9._%+\-]+@([A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,})\b".to_owned(),
            ),
            (
                Kind::Mri,
                format!(r"\b(?:8:(?:orgid|teamsvisitor|acs|gcch|dod):{GUID}|8:live:[A-Za-z0-9._\-]+|28:{GUID})"),
            ),
            // any GUID-shaped run, even inside a longer hex run: [`PUBLIC_GUIDS`] are let through in `matches_line`
            (Kind::Guid, GUID.to_owned()),
            (
                Kind::Signature,
                r#"(?i)(?:^|[^A-Za-z0-9])(?:sig|signature)["']?\s*[:=]\s*["']?[A-Za-z0-9+/=_.%\-]{16,}"#.to_owned(),
            ),
            // a join path, its code (group 1) and the rest of the link (group 2), placeholders included: checked in
            // `matches_line`
            (
                Kind::MeetingLink,
                r#"(?i)/(?:(?:v2/|lm/)?meet/([A-Za-z0-9]*)|l/meet(?:up|now)-join\b)((?:[^\s"'<>\\`]|<[a-z]+:[A-Za-z0-9]+>)*)"#
                    .to_owned(),
            ),
        ];
        let rules = patterns
            .into_iter()
            .map(|(kind, pattern)| Regex::new(&pattern).map(|regex| (kind, regex)))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            rules,
            labels: Labels::new()?,
            network: Network::new()?,
        })
    }

    /// Every hit in `text`, as (1-based line, kind), at most one per kind and line, in line order. A line is checked
    /// both as written and decoded (see the module docs).
    #[must_use]
    pub fn scan_text(&self, text: &str) -> Vec<(usize, Kind)> {
        let mut found = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let decoded = decode(line);
            let forms = iter::once(line).chain(decoded.as_deref());
            for (kind, regex) in &self.rules {
                if forms.clone().any(|form| matches_line(*kind, regex, form)) {
                    found.push((index + 1, *kind));
                }
            }
            for kind in [Kind::MeetingId, Kind::Passcode] {
                if forms.clone().any(|form| self.labelled(kind, form)) {
                    found.push((index + 1, kind));
                }
            }
            for kind in NETWORK_KINDS {
                if forms.clone().any(|form| self.networked(kind, form)) {
                    found.push((index + 1, kind));
                }
            }
        }
        found
    }

    /// Whether `line` holds a meeting's code after its label ([`Kind::MeetingId`]) or a passcode after its label or key
    /// ([`Kind::Passcode`]: a whole run of letters and digits, one cut short by a letter of another script is none).
    fn labelled(&self, kind: Kind, line: &str) -> bool {
        self.labels.values(line).iter().any(|value| match (kind, value.label) {
            (Kind::MeetingId, Label::MeetingId) => true,
            (Kind::Passcode, Label::Passcode) => {
                let whole = !line[value.value.end..].starts_with(char::is_alphanumeric);
                whole && is_passcode_value(&line[value.value.clone()])
            }
            _ => false,
        })
    }

    /// Whether `line` holds a call's network data of `kind` (one of the [`NETWORK_KINDS`]).
    fn networked(&self, kind: Kind, line: &str) -> bool {
        match kind {
            Kind::IpAddress => self.network.has_public_address(line),
            Kind::IceCredential => self.network.has_ice_credential(line),
            Kind::DtlsFingerprint => self.network.has_fingerprint(line),
            Kind::TurnCredential => self.network.has_turn_credential(line),
            _ => false,
        }
    }

    /// Scans each root, a file or a directory walked recursively, and returns every hit in path order. Symlinks are
    /// skipped; non-UTF-8 bytes are read lossily, so binary files are scanned too.
    ///
    /// # Errors
    /// A root is missing, or a file or directory under it can't be read; the error message names that path.
    pub fn scan_paths(&self, roots: &[&Path]) -> io::Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for root in roots {
            self.scan_entry(root, &mut findings)?;
        }
        Ok(findings)
    }

    fn scan_entry(&self, path: &Path, findings: &mut Vec<Finding>) -> io::Result<()> {
        let kind = fs::symlink_metadata(path).map_err(|e| at(path, &e))?.file_type();
        if kind.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|e| at(path, &e))?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<io::Result<Vec<_>>>()
                .map_err(|e| at(path, &e))?;
            entries.sort();
            for entry in entries {
                self.scan_entry(&entry, findings)?;
            }
        } else if kind.is_file() {
            let bytes = fs::read(path).map_err(|e| at(path, &e))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            findings.extend(self.scan_text(&text).into_iter().map(|(line, kind)| Finding {
                path: path.to_owned(),
                line,
                kind,
            }));
        }
        Ok(())
    }
}

/// `error` with `path` in its message.
fn at(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// How many nested encodings are peeled off at most (`%253A` → `%3A` → `:`).
const DECODE_ROUNDS: usize = 4;

/// `line` decoded (see [`decode_once`]), repeated until nothing changes, or `None` when the line has no escape.
fn decode(line: &str) -> Option<String> {
    if !line.contains(['%', '\\', '&']) {
        return None;
    }
    let mut current = line.to_owned();
    for _ in 0..DECODE_ROUNDS {
        let next = decode_once(&current);
        if next == current {
            break;
        }
        current = next;
    }
    Some(current)
}

/// One decoding pass: `%XX` becomes its byte; `\uXXXX` (a surrogate pair too) and `\xXX` their character, `\n`,
/// `\r` and `\t` a space so the words around them stay apart, and any other backslash is dropped; an HTML character
/// reference (`&amp;`, `&#38;`, `&#x26;`, `&nbsp;`) becomes its character.
fn decode_once(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let escape = match byte {
            b'%' => percent(bytes, i),
            b'\\' => Some(backslash(line, i)),
            b'&' => reference(line, i),
            _ => None,
        };
        let (decoded, len) = escape.unwrap_or_else(|| (vec![byte], 1));
        out.extend_from_slice(&decoded);
        i += len;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A percent escape at `at`: its byte, and its length.
fn percent(bytes: &[u8], at: usize) -> Option<(Vec<u8>, usize)> {
    let (high, low) = (hex(bytes.get(at + 1))?, hex(bytes.get(at + 2))?);
    Some((vec![high << 4 | low], 3))
}

/// A backslash escape at `at`: what it stands for, and its length.
fn backslash(line: &str, at: usize) -> (Vec<u8>, usize) {
    let rest = &line[at + 1..];
    let escaped = match rest.as_bytes().first() {
        Some(b'n' | b'r' | b't') => Some((' ', 1)),
        Some(b'u') => unicode_escape(rest),
        Some(b'x') => hex_number(rest.get(1..3))
            .and_then(|value| u8::try_from(value).ok())
            .map(|value| (char::from(value), 3)),
        _ => None,
    };
    match escaped {
        Some((char, len)) => (char.to_string().into_bytes(), 1 + len),
        None => (Vec::new(), 1),
    }
}

/// The character of a `uXXXX` escape (`rest` starts at the `u`), a surrogate pair (`uD83D\uDE00`) as one, and how
/// much of `rest` it takes.
fn unicode_escape(rest: &str) -> Option<(char, usize)> {
    let unit = hex_number(rest.get(1..5))?;
    if !(0xD800..0xDC00).contains(&unit) {
        return char::from_u32(unit).map(|char| (char, 5));
    }
    let low = rest
        .get(5..7)
        .filter(|escape| *escape == "\\u")
        .and_then(|_| hex_number(rest.get(7..11)))
        .filter(|low| (0xDC00..0xE000).contains(low))?;
    char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)).map(|char| (char, 11))
}

/// The value of a run of hex digits, `None` for anything else.
fn hex_number(digits: Option<&str>) -> Option<u32> {
    let digits = digits.filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))?;
    u32::from_str_radix(digits, 16).ok()
}

/// An HTML character reference at `at`: its character, and its length.
fn reference(line: &str, at: usize) -> Option<(Vec<u8>, usize)> {
    let rest = &line[at + 1..];
    let end = rest.bytes().take(10).position(|byte| byte == b';')?;
    let body = &rest[..end];
    let char = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => char::from(0xa0_u8),
        _ => {
            let number = body.strip_prefix('#')?;
            let value = match number.strip_prefix(['x', 'X']) {
                Some(digits) => hex_number(Some(digits))?,
                None => number
                    .parse()
                    .ok()
                    .filter(|_| number.bytes().all(|byte| byte.is_ascii_digit()))?,
            };
            char::from_u32(value)?
        }
    };
    Some((char.to_string().into_bytes(), end + 2))
}

/// The value of an ASCII hex digit.
fn hex(byte: Option<&u8>) -> Option<u8> {
    let digit = char::from(*byte?).to_digit(16)?;
    u8::try_from(digit).ok()
}

fn matches_line(kind: Kind, regex: &Regex, line: &str) -> bool {
    match kind {
        Kind::Email => regex
            .captures_iter(line)
            .any(|captures| captures.get(1).is_some_and(|domain| !is_reserved(domain.as_str()))),
        Kind::Guid => regex
            .find_iter(line)
            .any(|guid| !PUBLIC_GUIDS.contains(&guid.as_str().to_ascii_lowercase().as_str())),
        Kind::MeetingLink => regex.captures_iter(line).any(|captures| {
            let code = captures
                .get(1)
                .is_some_and(|code| is_meeting_code(code.as_str(), &line[code.end()..]));
            code || captures.get(2).is_some_and(|rest| has_passcode(rest.as_str()))
        }),
        _ => regex.is_match(line),
    }
}

/// Whether the text after `/meet/` (`after` follows it) is a meeting's code: 6 characters or more with a digit, a
/// whole word.
fn is_meeting_code(code: &str, after: &str) -> bool {
    code.len() >= MEETING_CODE_MIN && code.bytes().any(|byte| byte.is_ascii_digit()) && !after.starts_with(['-', '_'])
}

/// Whether the rest of a meeting link holds a `p` parameter whose value isn't a placeholder.
fn has_passcode(rest: &str) -> bool {
    let lower = rest.to_ascii_lowercase();
    lower.match_indices("p=").any(|(at, _)| {
        let after_separator = at > 0 && matches!(lower.as_bytes()[at - 1], b'?' | b'&' | b'#' | b';');
        let value = lower[at + 2..].chars().next();
        after_separator && value.is_some_and(|first| !matches!(first, '<' | '&' | '#' | ';'))
    })
}

fn is_reserved(domain: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    RESERVED_DOMAINS
        .iter()
        .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")))
}
