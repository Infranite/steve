//! What the knowledge module needs from the rest of the Hub, beyond [`steve_modules::Modules`]:
//! the secret scanner (steve-vault's `guard::scan`, wired by the integrator), a way to emit the
//! `{type:"kb"}` SSE event (not yet a `ChangeEvent` variant), and the document loaders the index
//! queue reads bodies with (notes and folders always; files need the data directory, see
//! [`crate::sources::files::FilesLoader`]).

use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tokio::sync::mpsc;

use crate::index::DocLoader;
use crate::repo::DocRow;
use crate::sources::folder::FolderPolicy;

/// What every secret found in indexed text is replaced with (§2.3).
pub const REDACTED: &str = "［已拦下的机密］";

/// `steve_vault::guard::scan` as this crate needs it: byte ranges of things that look like
/// passwords or tokens. The integrator wraps the real scanner; [`NoGuard`] finds nothing.
pub trait Guard: Send + Sync {
    fn scan(&self, text: &str) -> Vec<Range<usize>>;
}

/// A scanner that finds nothing (until steve-vault's guard is wired).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGuard;

impl Guard for NoGuard {
    fn scan(&self, _: &str) -> Vec<Range<usize>> {
        Vec::new()
    }
}

/// Replaces every range the guard reports with [`REDACTED`]. Overlapping or unsorted ranges and
/// ranges off char boundaries are tolerated (widened to the enclosing characters).
pub fn redact(guard: &dyn Guard, text: &str) -> String {
    let mut ranges = guard.scan(text);
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for r in ranges {
        let mut start = r.start.min(text.len()).max(pos);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = r.end.min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end <= pos {
            continue;
        }
        if start >= pos {
            out.push_str(&text[pos..start]);
            out.push_str(REDACTED);
        }
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// `{type:"kb", id, ws}` (§7): `id` is a document or source id. Sent after the commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename = "kb")]
pub struct KbEvent {
    pub id: String,
    pub ws: String,
}

type Notify = dyn Fn(KbEvent) + Send + Sync;

/// The knowledge module's own state, shared by its routes, jobs and the index worker. Cheap to clone.
#[derive(Clone)]
pub struct Knowledge(Arc<Inner>);

pub struct Inner {
    pub guard: Arc<dyn Guard>,
    notify: Arc<Notify>,
    pub(crate) loaders: Vec<Arc<dyn DocLoader>>,
    pub(crate) queue: OnceLock<mpsc::UnboundedSender<String>>,
    folder_policy: OnceLock<FolderPolicy>,
    /// Folder sources to scan soon (see [`Knowledge::request_scan`]).
    scan_requests: Mutex<Vec<String>>,
    pub(crate) scan_wake: tokio::sync::Notify,
}

impl std::ops::Deref for Knowledge {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Knowledge {
    /// `notify` forwards `{type:"kb"}` events to the SSE bus (integrator; tests collect them).
    /// Notes and folder files are always loadable; further loaders (uploads) come from
    /// [`Knowledge::with_loaders`].
    pub fn new(guard: Arc<dyn Guard>, notify: impl Fn(KbEvent) + Send + Sync + 'static) -> Self {
        Self::with_loaders(guard, notify, Vec::new())
    }

    pub fn with_loaders(
        guard: Arc<dyn Guard>,
        notify: impl Fn(KbEvent) + Send + Sync + 'static,
        mut loaders: Vec<Arc<dyn DocLoader>>,
    ) -> Self {
        loaders.insert(0, Arc::new(crate::notes::NotesLoader));
        loaders.insert(1, Arc::new(crate::sources::folder::FolderLoader));
        Self(Arc::new(Inner {
            guard,
            notify: Arc::new(notify),
            loaders,
            queue: OnceLock::new(),
            folder_policy: OnceLock::new(),
            scan_requests: Mutex::new(Vec::new()),
            scan_wake: tokio::sync::Notify::new(),
        }))
    }

    /// Asks the `kb.folder_scan` job ([`crate::spawn_scans`]) to scan a folder source now. Safe to
    /// call inside a write transaction: the scan claims the source in a write of its own, so it
    /// sees the committed state (and skips a source whose transaction rolled back).
    pub fn request_scan(&self, source_id: &str) {
        let mut q = self.scan_requests.lock().unwrap_or_else(|e| e.into_inner());
        if !q.iter().any(|x| x == source_id) {
            q.push(source_id.to_string());
        }
        drop(q);
        self.scan_wake.notify_one();
    }

    /// Takes the pending scan requests.
    pub fn take_scan_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.scan_requests.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// No guard, events dropped: for tests and tools.
    pub fn bare() -> Self {
        Self::new(Arc::new(NoGuard), |_| {})
    }

    pub fn notify(&self, ws: &str, id: &str) {
        (self.notify)(KbEvent {
            id: id.into(),
            ws: ws.into(),
        });
    }

    /// Sets where `$HOME` and `STEVE_HOME` are for the folder checks (§2.2). Without a call they
    /// come from the environment on first use. `false` if already set or used.
    pub fn set_folder_policy(&self, p: FolderPolicy) -> bool {
        self.folder_policy.set(p).is_ok()
    }

    pub fn folder_policy(&self) -> &FolderPolicy {
        self.folder_policy.get_or_init(FolderPolicy::from_env)
    }

    pub(crate) fn loader(&self, kind: &str, doc: &DocRow) -> Option<&Arc<dyn DocLoader>> {
        self.loaders
            .iter()
            .find(|l| l.kinds().contains(&kind) && l.accepts(doc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tok;
    impl Guard for Tok {
        fn scan(&self, text: &str) -> Vec<Range<usize>> {
            text.match_indices("ghp_x")
                .map(|(i, s)| i..i + s.len())
                .collect()
        }
    }

    #[test]
    fn redacts_ranges() {
        assert_eq!(
            redact(&Tok, "令牌 ghp_x 和 ghp_x。"),
            "令牌 ［已拦下的机密］ 和 ［已拦下的机密］。"
        );
        assert_eq!(redact(&Tok, "无"), "无");
        assert_eq!(
            serde_json::to_string(&KbEvent {
                id: "kd_1".into(),
                ws: "w".into()
            })
            .unwrap(),
            r#"{"type":"kb","id":"kd_1","ws":"w"}"#
        );
    }
}
