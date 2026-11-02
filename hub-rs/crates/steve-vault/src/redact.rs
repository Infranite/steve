//! The redactor (accounts-vault.md §4.4): replaces injected secret values in text with
//! `[已打码：<字段>]`, in every form a command is likely to print them in:
//!
//! - the raw value, and JSON-escaped when that differs;
//! - base64, standard and URL-safe alphabets, with and without padding; plus the part of the
//!   encoding that is the same wherever the value sits inside a longer base64 stream (one per offset
//!   mod 3: `echo "user:$TOKEN" | base64` still loses the token);
//! - URL encoding (`%2F` and `%2f`, and `+` for spaces);
//! - hex, lower and upper case.
//!
//! Two uses:
//! - [`Redactor`]: one set of values, used on a command's output before anyone sees it;
//! - the session registry: every injection registers its values for its session for an hour
//!   ([`register`]); steve-core calls [`scrub`] (JSON) or [`scrub_text`] before it writes ACP events
//!   to `events`, tool output to the timeline, or a log line. That keeps a value the agent did get
//!   hold of out of Steve's records; it can't take it back from the model.
//!
//! Matching is byte-wise with Aho-Corasick (leftmost-longest). Values are kept in [`Zeroizing`]
//! buffers; the automaton's own copy of the patterns can't be zeroed, it is dropped with the entry.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use data_encoding::{
    BASE64, BASE64_NOPAD, BASE64URL, BASE64URL_NOPAD, Encoding, HEXLOWER, HEXUPPER,
};
use serde_json::Value;
use zeroize::Zeroizing;

/// How long a session keeps an injected value in its redactor (§4.4).
pub const SESSION_TTL: Duration = Duration::from_secs(3600);

/// Values shorter than this are only matched raw, and derived forms shorter than `MIN_DERIVED` are
/// left out: the hex of a 4-digit PIN would hit ordinary text.
const MIN_VALUE: usize = 6;
const MIN_DERIVED: usize = 8;

/// The text a value is replaced with.
pub fn mask(label: &str) -> String {
    format!("[已打码：{label}]")
}

fn push(out: &mut Vec<Zeroizing<Vec<u8>>>, v: Vec<u8>) {
    let v = Zeroizing::new(v);
    if !v.is_empty() && !out.iter().any(|x| **x == *v) {
        out.push(v);
    }
}

/// Every form of `value` the redactor looks for (raw first).
pub fn variants(value: &[u8]) -> Vec<Zeroizing<Vec<u8>>> {
    let mut out = Vec::new();
    if value.is_empty() {
        return out;
    }
    push(&mut out, value.to_vec());
    if value.len() < MIN_VALUE {
        return out;
    }
    let mut derived = Vec::new();
    if let Ok(s) = std::str::from_utf8(value) {
        let json = Zeroizing::new(Value::String(s.to_string()).to_string());
        derived.push(json.as_bytes()[1..json.len() - 1].to_vec());
    }
    let encs: [&Encoding; 4] = [&BASE64, &BASE64_NOPAD, &BASE64URL, &BASE64URL_NOPAD];
    for e in encs {
        derived.push(Zeroizing::new(e.encode(value)).as_bytes().to_vec());
    }
    // Inside a longer stream the value starts at some offset mod 3. Skipping its first `skip` bytes
    // puts the rest on a group boundary; whole groups of that encode the same at any position.
    for skip in 0..3usize.min(value.len()) {
        let rest = &value[skip..];
        let whole = rest.len() / 3 * 3;
        if whole == 0 {
            continue;
        }
        for e in [&BASE64_NOPAD, &BASE64URL_NOPAD] {
            derived.push(Zeroizing::new(e.encode(&rest[..whole])).as_bytes().to_vec());
        }
    }
    derived.push(Zeroizing::new(HEXLOWER.encode(value)).as_bytes().to_vec());
    derived.push(Zeroizing::new(HEXUPPER.encode(value)).as_bytes().to_vec());
    for (upper, plus) in [(true, false), (false, false), (true, true), (false, true)] {
        derived.push(url_encode(value, upper, plus));
    }
    for d in derived {
        let d = Zeroizing::new(d);
        if d.len() >= MIN_DERIVED {
            push(&mut out, d.to_vec());
        }
    }
    out
}

/// Percent-encoding of everything but RFC 3986 unreserved characters.
fn url_encode(v: &[u8], upper: bool, plus_for_space: bool) -> Vec<u8> {
    let hex = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut out = Vec::with_capacity(v.len() * 3);
    for &b in v {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b),
            b' ' if plus_for_space => out.push(b'+'),
            _ => {
                out.push(b'%');
                out.push(hex[usize::from(b >> 4)]);
                out.push(hex[usize::from(b & 15)]);
            }
        }
    }
    out
}

/// A redactor for a fixed set of `(label, value)` pairs.
pub struct Redactor {
    ac: Option<AhoCorasick>,
    /// The replacement of each pattern, by pattern index.
    masks: Vec<String>,
    longest: usize,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("patterns", &self.masks.len())
            .finish_non_exhaustive()
    }
}

impl Redactor {
    pub fn new<'a>(secrets: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let mut pats: Vec<Zeroizing<Vec<u8>>> = Vec::new();
        let mut masks = Vec::new();
        for (label, value) in secrets {
            for v in variants(value) {
                pats.push(v);
                masks.push(mask(label));
            }
        }
        let longest = pats.iter().map(|p| p.len()).max().unwrap_or(0);
        let ac = if pats.is_empty() {
            None
        } else {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .build(pats.iter().map(|p| p.as_slice()))
                .ok()
        };
        Self { ac, masks, longest }
    }

    pub fn is_empty(&self) -> bool {
        self.ac.is_none()
    }

    /// The longest pattern: keep this many bytes beyond a cut so a value at the cut is still found.
    pub fn longest(&self) -> usize {
        self.longest
    }

    pub fn scrub_bytes<'b>(&self, b: &'b [u8]) -> Cow<'b, [u8]> {
        let Some(ac) = &self.ac else {
            return Cow::Borrowed(b);
        };
        if !ac.is_match(b) {
            return Cow::Borrowed(b);
        }
        let mut out = Vec::with_capacity(b.len());
        ac.replace_all_with_bytes(b, &mut out, |m, _, dst| {
            dst.extend_from_slice(self.masks[m.pattern().as_usize()].as_bytes());
            true
        });
        Cow::Owned(out)
    }

    pub fn scrub_str<'s>(&self, s: &'s str) -> Cow<'s, str> {
        match self.scrub_bytes(s.as_bytes()) {
            Cow::Borrowed(_) => Cow::Borrowed(s),
            Cow::Owned(b) => Cow::Owned(match String::from_utf8(b) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            }),
        }
    }

    /// Every string (and object key) in `v`.
    pub fn scrub_value(&self, v: &mut Value) {
        if self.ac.is_none() {
            return;
        }
        match v {
            Value::String(s) => {
                if let Cow::Owned(n) = self.scrub_str(s) {
                    *s = n;
                }
            }
            Value::Array(xs) => xs.iter_mut().for_each(|x| self.scrub_value(x)),
            Value::Object(m) => {
                if m.keys().any(|k| matches!(self.scrub_str(k), Cow::Owned(_))) {
                    let old = std::mem::take(m);
                    for (k, mut x) in old {
                        self.scrub_value(&mut x);
                        m.insert(self.scrub_str(&k).into_owned(), x);
                    }
                } else {
                    m.values_mut().for_each(|x| self.scrub_value(x));
                }
            }
            _ => {}
        }
    }
}

// ----------------------------------------------------------------------------- session registry

struct Entry {
    label: String,
    value: Zeroizing<Vec<u8>>,
    expires: Instant,
}

#[derive(Default)]
struct SessionSet {
    entries: Vec<Entry>,
    /// Built on first use after a change.
    redactor: Option<Arc<Redactor>>,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, SessionSet>>> = LazyLock::new(Mutex::default);

fn sessions() -> std::sync::MutexGuard<'static, HashMap<String, SessionSet>> {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Adds a value to a session's redactor for [`SESSION_TTL`] (again: the hour starts over).
pub fn register(session_id: &str, label: &str, value: &[u8]) {
    register_at(session_id, label, value, Instant::now() + SESSION_TTL);
}

fn register_at(session_id: &str, label: &str, value: &[u8], expires: Instant) {
    if value.is_empty() {
        return;
    }
    let mut map = sessions();
    let set = map.entry(session_id.to_string()).or_default();
    set.entries.retain(|e| *e.value != value);
    set.entries.push(Entry {
        label: label.to_string(),
        value: Zeroizing::new(value.to_vec()),
        expires,
    });
    set.redactor = None;
}

/// The session's current redactor (expired values dropped), if it has any values.
fn redactor_at(session_id: &str, now: Instant) -> Option<Arc<Redactor>> {
    let mut map = sessions();
    let set = map.get_mut(session_id)?;
    let before = set.entries.len();
    set.entries.retain(|e| e.expires > now);
    if set.entries.is_empty() {
        map.remove(session_id);
        return None;
    }
    if set.entries.len() != before {
        set.redactor = None;
    }
    let r = set.redactor.get_or_insert_with(|| {
        Arc::new(Redactor::new(
            set.entries
                .iter()
                .map(|e| (e.label.as_str(), e.value.as_slice())),
        ))
    });
    Some(r.clone())
}

/// Removes the session's values from every string in `v` (ACP events, tool output, timeline rows,
/// log fields). A no-op for sessions without injections.
pub fn scrub(session_id: &str, v: &mut Value) {
    if let Some(r) = redactor_at(session_id, Instant::now()) {
        r.scrub_value(v);
    }
}

/// [`scrub`] for plain text.
pub fn scrub_text<'s>(session_id: &str, s: &'s str) -> Cow<'s, str> {
    match redactor_at(session_id, Instant::now()) {
        Some(r) => Cow::Owned(r.scrub_str(s).into_owned()),
        None => Cow::Borrowed(s),
    }
}

/// [`scrub_text`] with the values of every live session: for text whose session is not known
/// where it is logged (an adapter's stderr, which may serve several sessions).
pub fn scrub_any(s: &str) -> Cow<'_, str> {
    let now = Instant::now();
    let ids: Vec<String> = sessions().keys().cloned().collect();
    let mut out: Cow<'_, str> = Cow::Borrowed(s);
    for id in ids {
        if let Some(r) = redactor_at(&id, now)
            && let Cow::Owned(t) = r.scrub_str(&out)
        {
            out = Cow::Owned(t);
        }
    }
    out
}

/// The labels of every live injected value (any session) that occurs in `text`, in any of its
/// encoded forms: what an outgoing mail must not carry (E3's `SecretScan`).
pub fn find_labels(text: &str) -> Vec<String> {
    let now = Instant::now();
    let hay = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    for set in sessions().values() {
        for e in set.entries.iter().filter(|e| e.expires > now) {
            let hit = variants(&e.value)
                .iter()
                .any(|v| !v.is_empty() && hay.windows(v.len()).any(|w| w == v.as_slice()));
            if hit && !out.contains(&e.label) {
                out.push(e.label.clone());
            }
        }
    }
    out
}

/// Whether the session has any live values (tests, diagnostics).
pub fn is_registered(session_id: &str) -> bool {
    redactor_at(session_id, Instant::now()).is_some()
}

/// Drops a session's values now (session deleted).
pub fn forget(session_id: &str) {
    sessions().remove(session_id);
}

#[cfg(test)]
mod tests {

    #[test]
    fn scrub_any_covers_every_live_session() {
        register("ses_scrub_any", "令牌", b"tok_scrubany_0123456789");
        let out = scrub_any("stderr: auth tok_scrubany_0123456789 failed");
        assert!(!out.contains("tok_scrubany_0123456789"), "{out}");
        assert!(matches!(scrub_any("nothing here"), Cow::Borrowed(_)));
        forget("ses_scrub_any");
        assert!(scrub_any("tok_scrubany_0123456789").contains("tok_scrubany"));
    }

    #[test]
    fn finds_the_labels_of_injected_values_in_text() {
        register(
            "ses_find_labels",
            "GitHub·token",
            b"ghp_findme0123456789abcdef",
        );
        assert_eq!(
            find_labels("body: ghp_findme0123456789abcdef"),
            vec!["GitHub·token".to_string()]
        );
        assert!(find_labels("nothing secret here").is_empty());
        forget("ses_find_labels");
        assert!(find_labels("body: ghp_findme0123456789abcdef").is_empty());
    }

    use super::*;
    use serde_json::json;

    const V: &str = "tok-Secret/9271+x y";

    fn r() -> Redactor {
        Redactor::new([("令牌", V.as_bytes())])
    }

    #[test]
    fn raw_and_encoded_forms_are_masked() {
        let r = r();
        let b64 = BASE64.encode(V.as_bytes());
        let b64u = BASE64URL_NOPAD.encode(V.as_bytes());
        let hex = HEXLOWER.encode(V.as_bytes());
        let hexu = HEXUPPER.encode(V.as_bytes());
        let url = String::from_utf8(url_encode(V.as_bytes(), true, false)).unwrap();
        let urll = String::from_utf8(url_encode(V.as_bytes(), false, true)).unwrap();
        for form in [V, &b64, &b64u, &hex, &hexu, &url, &urll] {
            let s = format!("a {form} b");
            assert_eq!(r.scrub_str(&s), "a [已打码：令牌] b", "{form}");
        }
        assert_eq!(r.scrub_str("nothing here"), "nothing here");
        assert!(matches!(r.scrub_str("nothing"), Cow::Borrowed(_)));
    }

    #[test]
    fn value_inside_a_longer_base64_stream() {
        let r = r();
        for prefix in ["", "u:", "us:", "user:"] {
            let stream = BASE64.encode(format!("{prefix}{V}\n").as_bytes());
            let out = r.scrub_str(&stream);
            assert!(out.contains("[已打码：令牌]"), "{prefix}: {out}");
            // what's left of the encoding can't be decoded back to the value
            for part in out.split("[已打码：令牌]") {
                assert!(part.len() <= 12, "{prefix}: leftover {part}");
            }
        }
    }

    #[test]
    fn json_values_and_keys() {
        let r = r();
        let mut v = json!({"text": format!("echo {V}"), "list": [V, 1], V: "k"});
        r.scrub_value(&mut v);
        let s = v.to_string();
        assert!(!s.contains("Secret/9271"), "{s}");
        assert_eq!(v["text"], "echo [已打码：令牌]");
        assert_eq!(v["[已打码：令牌]"], "k");
        // JSON-escaped form in a string that is itself JSON text
        let q = Redactor::new([("密码", "a\"b\\c-long".as_bytes())]);
        assert_eq!(
            q.scrub_str(r#"{"pw":"a\"b\\c-long"}"#),
            r#"{"pw":"[已打码：密码]"}"#
        );
    }

    #[test]
    fn short_values_only_raw() {
        let r = Redactor::new([("PIN", "1234".as_bytes())]);
        assert_eq!(
            r.scrub_str("pin 1234 hex 31323334"),
            "pin [已打码：PIN] hex 31323334"
        );
        assert!(Redactor::new([("x", &b""[..])]).is_empty());
    }

    #[test]
    fn session_registry_expires() {
        let sid = "ses_test_registry";
        let mut v = json!({"t": format!("x{V}x")});
        scrub(sid, &mut v);
        assert_eq!(v["t"], format!("x{V}x"));
        register(sid, "令牌", V.as_bytes());
        scrub(sid, &mut v);
        assert_eq!(v["t"], "x[已打码：令牌]x");
        assert_eq!(scrub_text(sid, &hex(V)), "[已打码：令牌]");
        assert_eq!(scrub_text("ses_other", V), V);
        // past the hour the value is gone
        register_at(sid, "令牌", V.as_bytes(), Instant::now());
        assert!(redactor_at(sid, Instant::now() + Duration::from_millis(1)).is_none());
        assert!(!is_registered(sid));
        register(sid, "a", b"something-long");
        forget(sid);
        assert!(!is_registered(sid));
    }

    fn hex(s: &str) -> String {
        HEXLOWER.encode(s.as_bytes())
    }
}
