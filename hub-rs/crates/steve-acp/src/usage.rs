//! Shared usage collection for persistent sessions and one-shot calls. A sampler belongs to one
//! attempt, never a session lifetime. Native final accounting takes precedence field by field.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use steve_node_api::{
    Digest, Epoch, EventBody, Measurement, Severity, Tokens, UsageReport, UsageScope,
};

use crate::AcpConfig;
use crate::normalize::Fact;
use crate::rollout::Reader as Rollout;
use crate::transcript::Reader as Transcript;
use steve_harness_spec::UsageFallback;

pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const SETTLE_WAIT: Duration = Duration::from_secs(3);
/// Spacing between tail settlement reads inside [`Sampler::finish`].
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The effective home of the *spawned* adapter, including manifest, wrapper and principal
/// overrides (last wins). Relative homes follow the adapter's cwd, not the Node's cwd.
pub fn rollout_home(
    cfg: &AcpConfig,
    harness: &str,
    env: &[(OsString, OsString)],
    cwd: &Path,
) -> Option<PathBuf> {
    if cfg.adapter_properties(harness).usage_fallback != UsageFallback::CodexRollout {
        return None;
    }
    let home = env
        .iter()
        .rev()
        .find(|(k, _)| k == "CODEX_HOME")
        .map(|(_, v)| PathBuf::from(v))
        .unwrap_or_else(|| cfg.codex_home.clone());
    if home.as_os_str().is_empty() {
        return None;
    }
    Some(if home.is_absolute() {
        home
    } else {
        cwd.join(home)
    })
}

/// A local source chosen by adapter capability, resolved against its actual process env.
#[derive(Clone, Debug)]
pub struct Source {
    pub home: PathBuf,
    pub format: UsageFallback,
}

pub fn source(
    cfg: &AcpConfig,
    harness: &str,
    env: &[(OsString, OsString)],
    cwd: &Path,
) -> Option<Source> {
    source_for_spawn(cfg, harness, env, cwd, true)
}

/// `inherit_env` follows the spawn's clean-environment flag, not the Node process's login.
pub fn source_for_spawn(
    cfg: &AcpConfig,
    harness: &str,
    env: &[(OsString, OsString)],
    cwd: &Path,
    inherit_env: bool,
) -> Option<Source> {
    let format = cfg.adapter_properties(harness).usage_fallback;
    if format == UsageFallback::None {
        return None;
    }
    let home = match format {
        UsageFallback::CodexRollout => rollout_home(cfg, harness, env, cwd)?,
        UsageFallback::ClaudeTranscript => {
            let effective = |key: &str| {
                env.iter()
                    .rev()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
                    .or_else(|| inherit_env.then(|| std::env::var_os(key)).flatten())
            };
            let home = effective("CLAUDE_CONFIG_DIR")
                .map(|v| PathBuf::from(v.to_string_lossy().trim()))
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| {
                    effective("HOME")
                        .filter(|v| !v.is_empty())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| cfg.user_home.clone())
                        .join(".claude")
                });
            if home.is_absolute() {
                home
            } else {
                cwd.join(home)
            }
        }
        UsageFallback::None => return None,
    };
    Some(Source { home, format })
}

/// Content identity, not arrival order. A replay has the same id.
pub(crate) fn identify(prefix: &str, report: &mut UsageReport) {
    report.report_id.clear();
    let hash = Digest::sha256(json!([prefix, report]).to_string().as_bytes());
    report.report_id = format!("usage:{}", &hash.as_str()[7..]);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    pub model: Option<String>,
    pub scope: UsageScope,
    pub tokens: Tokens,
}

enum Reader {
    Rollout(Rollout),
    Transcript(Transcript),
}
impl Reader {
    fn read(&mut self, prime: bool) {
        match self {
            Self::Rollout(r) => {
                if prime {
                    r.prime()
                } else {
                    r.poll()
                }
            }
            Self::Transcript(r) => {
                if prime {
                    r.prime()
                } else {
                    r.poll()
                }
            }
        }
    }
    fn readings(&self) -> Vec<Reading> {
        match self {
            Self::Rollout(r) => {
                let tokens = r.tokens();
                if tokens == Tokens::default() {
                    vec![]
                } else {
                    vec![Reading {
                        model: None,
                        scope: UsageScope::Main,
                        tokens,
                    }]
                }
            }
            Self::Transcript(r) => r.readings(),
        }
    }
    fn incomplete(&self) -> bool {
        match self {
            Self::Rollout(r) => r.incomplete || !r.caught_up || r.unsettled(),
            Self::Transcript(r) => r.incomplete || !r.caught_up || r.unsettled(),
        }
    }
}

struct FinalReadings {
    incomplete: bool,
    fallback: Vec<Reading>,
}

/// Bound the async caller even on a slow filesystem. Only one blocking read can be in flight
/// per sampler. An OS read cannot be aborted; its worker owns only source state, no Node/home
/// lock or event sender. Dropping the sampler schedules no more I/O.
const READ_WAIT: Duration = Duration::from_millis(200);
pub struct Sampler {
    reader: Option<Arc<Mutex<Reader>>>,
    in_flight: Option<tokio::task::JoinHandle<()>>,
    snapshot: Vec<Reading>,
    observed: Vec<Reading>,
    source_end: Option<Arc<AtomicI64>>,
    prefix: String,
    meter: Epoch,
    last: Vec<Reading>,
    stalled: bool,
}

impl Sampler {
    /// Must finish before sending the prompt; historical rows establish only a baseline.
    pub async fn start_source(
        source: Option<Source>,
        sid: &str,
        prefix: &str,
        meter: Epoch,
    ) -> Self {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let reader = source.and_then(|s| match s.format {
            UsageFallback::None => None,
            UsageFallback::CodexRollout => {
                Some(Reader::Rollout(Rollout::new(s.home, sid.into(), started)))
            }
            UsageFallback::ClaudeTranscript => Some(Reader::Transcript(Transcript::new(
                s.home,
                sid.into(),
                started,
            ))),
        });
        let source_end = reader.as_ref().and_then(|r| match r {
            Reader::Transcript(r) => Some(r.end_fence.clone()),
            _ => None,
        });
        let mut sampler = Self {
            reader: reader.map(|r| Arc::new(Mutex::new(r))),
            in_flight: None,
            snapshot: vec![],
            observed: vec![],
            source_end,
            prefix: prefix.into(),
            meter,
            last: vec![],
            stalled: false,
        };
        sampler.read(true).await;
        sampler
    }

    async fn read(&mut self, prime: bool) {
        let Some(reader) = self.reader.clone() else {
            return;
        };
        if self.in_flight.is_none() {
            self.in_flight = Some(tokio::task::spawn_blocking(move || {
                reader.lock().unwrap_or_else(|e| e.into_inner()).read(prime);
            }));
        }
        let task = self.in_flight.as_mut().expect("in flight");
        match tokio::time::timeout(READ_WAIT, task).await {
            Ok(result) => {
                self.in_flight = None;
                self.stalled = result.is_err();
                if let Some(reader) = &self.reader
                    && let Ok(reader) = reader.try_lock()
                {
                    self.snapshot = reader.readings();
                    if !self.snapshot.is_empty() {
                        self.observed = self.snapshot.clone();
                    }
                }
            }
            Err(_) => self.stalled = true,
        }
    }

    fn report(&self, measurement: Measurement, reading: Reading) -> UsageReport {
        let mut report = UsageReport {
            report_id: String::new(),
            model: reading.model,
            meter_epoch: self.meter.clone(),
            scope: reading.scope,
            measurement,
            tokens: reading.tokens,
            context_used: None,
            context_limit: None,
            cost: None,
        };
        identify(&self.prefix, &mut report);
        report
    }
    pub async fn poll(&mut self) -> Vec<Fact> {
        #[cfg(any(test, feature = "testkit"))]
        let Some(_polling) = testkit::begin(&self.prefix) else {
            return vec![];
        };
        self.read(false).await;
        if self.snapshot == self.last {
            return vec![];
        }
        self.last = self.snapshot.clone();
        self.snapshot
            .iter()
            .cloned()
            .map(|r| {
                Fact::Event(EventBody::Usage {
                    report: self.report(Measurement::Cumulative, r),
                })
            })
            .collect()
    }

    /// Success, cancellation and error all get the same full bounded tail window. A recent live
    /// tick does not establish that the producer flushed its last response.
    pub async fn finish(&mut self, native: Vec<Fact>, wait: Duration) -> Vec<Fact> {
        let deadline = tokio::time::Instant::now() + wait;
        self.finish_with(native, move || async move {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::time::sleep(SETTLE_POLL_INTERVAL.min(remaining)).await;
            true
        })
        .await
    }

    async fn finish_with<F, Fut>(&mut self, mut native: Vec<Fact>, mut next_tick: F) -> Vec<Fact>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        // The producer has stopped. Late disk writes may contain existing responses, but new
        // source messages after this boundary belong elsewhere. The fence never waits on I/O.
        if let Some(end) = &self.source_end {
            end.fetch_min(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
                Ordering::Relaxed,
            );
        }
        if self.reader.is_some() {
            loop {
                self.read(false).await;
                if !next_tick().await {
                    break;
                }
            }
        }
        let FinalReadings {
            incomplete,
            fallback,
        } = self.final_readings();
        self.merge_final_readings(&mut native, fallback);
        self.complete_final_reports(&mut native, incomplete);
        native
    }

    fn final_readings(&self) -> FinalReadings {
        let incomplete = self.stalled
            || self
                .reader
                .as_ref()
                .is_some_and(|r| r.try_lock().map_or(true, |r| r.incomplete()));
        // Losing or stalling a source does not revoke consumption already emitted live.
        // Keep that observed lower bound; the diagnostic marks an unsettled final reading.
        let fallback = if incomplete && self.snapshot.is_empty() {
            self.observed.clone()
        } else {
            self.snapshot.clone()
        };
        FinalReadings {
            incomplete,
            fallback,
        }
    }

    fn merge_final_readings(&self, native: &mut Vec<Fact>, fallback: Vec<Reading>) {
        let root_models: Vec<_> = fallback
            .iter()
            .filter(|r| r.scope == UsageScope::Main)
            .filter_map(|r| r.model.clone())
            .collect();
        // The native Main is anonymous. Attribute it only when the persisted main chain gives
        // exactly one model. With several models its aggregate is replaced by source groups.
        for fact in native.iter_mut() {
            if let Fact::Event(EventBody::Usage { report }) = fact
                && report.meter_epoch == self.meter
                && report.measurement == Measurement::Final
                && report.scope == UsageScope::Main
                && report.model.is_none()
            {
                if root_models.len() == 1 {
                    report.model = Some(root_models[0].clone());
                } else if root_models.len() > 1 {
                    report.tokens = Tokens::default();
                }
            }
        }
        for reading in fallback {
            let found = native.iter_mut().find_map(|f| match f {
                Fact::Event(EventBody::Usage { report })
                    if report.meter_epoch == self.meter
                        && report.measurement == Measurement::Final
                        && report.scope == reading.scope
                        && report.model == reading.model =>
                {
                    Some(report)
                }
                _ => None,
            });
            if let Some(report) = found {
                let t = &mut report.tokens;
                // Claude can report a synthetic all-zero cancellation without a result. Such
                // a value is not evidence that previously persisted consumption disappeared.
                let zero = [t.input, t.output, t.cache_read, t.cache_write]
                    .into_iter()
                    .flatten()
                    .all(|v| v == 0);
                let spent = [
                    reading.tokens.input,
                    reading.tokens.output,
                    reading.tokens.cache_read,
                    reading.tokens.cache_write,
                ]
                .into_iter()
                .flatten()
                .any(|v| v > 0);
                if self.source_end.is_some() && zero && spent {
                    *t = reading.tokens;
                } else {
                    t.input = t.input.or(reading.tokens.input);
                    t.output = t.output.or(reading.tokens.output);
                    t.cache_read = t.cache_read.or(reading.tokens.cache_read);
                    t.cache_write = t.cache_write.or(reading.tokens.cache_write);
                    t.reasoning = t.reasoning.or(reading.tokens.reasoning);
                }
            } else {
                native.push(Fact::Event(EventBody::Usage {
                    report: self.report(Measurement::Final, reading),
                }));
            }
        }
    }

    fn complete_final_reports(&self, native: &mut Vec<Fact>, incomplete: bool) {
        let has_own = native.iter().any(|f| matches!(f, Fact::Event(EventBody::Usage { report }) if report.meter_epoch == self.meter && report.measurement == Measurement::Final && report.tokens != Tokens::default()));
        if !has_own {
            // Keep explicit native zero; do not create another main beside a native final.
            let main = native.iter().any(|f| matches!(f, Fact::Event(EventBody::Usage { report }) if report.meter_epoch == self.meter && report.measurement == Measurement::Final && report.scope == UsageScope::Main));
            if !main {
                native.push(Fact::Event(EventBody::Usage {
                    report: self.report(
                        Measurement::Final,
                        Reading {
                            model: None,
                            scope: UsageScope::Main,
                            tokens: Tokens::default(),
                        },
                    ),
                }));
            }
        }
        for fact in native.iter_mut() {
            if let Fact::Event(EventBody::Usage { report }) = fact
                && report.meter_epoch == self.meter
            {
                report.tokens.reasoning = report
                    .tokens
                    .reasoning
                    .filter(|r| report.tokens.output.is_none_or(|o| *r <= o));
                identify(&self.prefix, report);
            }
        }
        if !has_own || incomplete {
            native.push(Fact::Event(EventBody::Diagnostic {
                severity: Severity::Info,
                code: if has_own {
                    "usage_incomplete"
                } else {
                    "usage_unavailable"
                }
                .into(),
                safe_text: if has_own {
                    "Token accounting could not be fully settled within the source read budget."
                } else {
                    "No attributable token accounting was available; token fields are unknown."
                }
                .into(),
                details: None,
            }));
        }
    }
}

/// Test seam (feature `testkit`): hold the live polls of one attempt, named by its sampler
/// prefix. A held poll reads nothing and reports nothing, like a poll that found no change;
/// [`Sampler::finish`] is never held. A test that persists source bytes under a hold knows that
/// only the final settlement reads them, without racing [`POLL_INTERVAL`] on the wall clock.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex, MutexGuard};
    use std::time::Duration;

    #[derive(Default)]
    struct Attempt {
        holds: usize,
        polling: usize,
    }

    static ATTEMPTS: LazyLock<Mutex<HashMap<String, Attempt>>> = LazyLock::new(Default::default);

    fn attempts() -> MutexGuard<'static, HashMap<String, Attempt>> {
        ATTEMPTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn release(prefix: &str, f: impl FnOnce(&mut Attempt)) {
        let mut all = attempts();
        if let Some(a) = all.get_mut(prefix) {
            f(a);
            if a.holds == 0 && a.polling == 0 {
                all.remove(prefix);
            }
        }
    }

    /// Holds `prefix`'s live polls until the guard drops. Returns once none of them is in flight:
    /// a poll that began before the hold has already returned what it read. A poll is bounded by
    /// its read budget, so this wait is too.
    pub async fn hold_polls(prefix: &str) -> PollHold {
        attempts().entry(prefix.into()).or_default().holds += 1;
        while attempts().get(prefix).is_some_and(|a| a.polling > 0) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        PollHold(prefix.into())
    }

    #[must_use = "the hold ends when the guard drops"]
    pub struct PollHold(String);

    impl Drop for PollHold {
        fn drop(&mut self) {
            release(&self.0, |a| a.holds -= 1);
        }
    }

    /// One live poll in flight; `None` while the attempt is held.
    pub(super) struct Polling(String);

    pub(super) fn begin(prefix: &str) -> Option<Polling> {
        let mut all = attempts();
        let a = all.entry(prefix.into()).or_default();
        if a.holds > 0 {
            return None;
        }
        a.polling += 1;
        Some(Polling(prefix.into()))
    }

    impl Drop for Polling {
        fn drop(&mut self) {
            release(&self.0, |a| a.polling -= 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::Normalizer;
    use std::io::Write;

    fn report(facts: &[Fact], measurement: Measurement) -> &UsageReport {
        facts
            .iter()
            .find_map(|f| match f {
                Fact::Event(EventBody::Usage { report }) if report.measurement == measurement => {
                    Some(report)
                }
                _ => None,
            })
            .expect("report")
    }
    fn tick() -> String {
        format!(
            "{}\n",
            json!({"type":"event_msg","timestamp":"1970-01-01T00:00:00.030Z","payload":{
            "type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":20,"reasoning_output_tokens":5},
                "last_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":20,"reasoning_output_tokens":5}}}})
        )
    }
    async fn sampler(home: &Path) -> Sampler {
        let mut s = Sampler::start_source(None, "s", "attempt", Epoch::new("epoch").unwrap()).await;
        s.reader = Some(Arc::new(Mutex::new(Reader::Rollout(Rollout::new(
            home.into(),
            "s".into(),
            20,
        )))));
        s.read(true).await;
        s
    }
    fn path(home: &Path) -> PathBuf {
        let day = home.join("sessions/1970/01/01");
        std::fs::create_dir_all(&day).unwrap();
        day.join("rollout-t-s.jsonl")
    }

    #[tokio::test]
    async fn live_and_final_ids_are_stable_across_replay_but_distinct_measurements() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut a = sampler(dir.path()).await;
        let mut b = sampler(dir.path()).await;
        std::fs::write(&p, tick()).unwrap();
        let live = a.poll().await;
        let replay = b.poll().await;
        assert_eq!(live, replay);
        assert!(a.poll().await.is_empty());
        let final_a = a.finish(vec![], Duration::ZERO).await;
        let final_b = b.finish(vec![], Duration::ZERO).await;
        assert_eq!(final_a, final_b);
        let live = report(&live, Measurement::Cumulative);
        let end = report(&final_a, Measurement::Final);
        assert_ne!(live.report_id, end.report_id);
        assert_eq!(live.tokens, end.tokens);
        assert_eq!(live.meter_epoch, end.meter_epoch);
    }

    #[tokio::test]
    async fn held_polls_report_nothing_and_leave_the_tail_to_finish() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        // its own prefix: the other tests here sample "attempt" concurrently
        let mut s =
            Sampler::start_source(None, "s", "held-attempt", Epoch::new("epoch").unwrap()).await;
        s.reader = Some(Arc::new(Mutex::new(Reader::Rollout(Rollout::new(
            dir.path().into(),
            "s".into(),
            20,
        )))));
        s.read(true).await;
        let hold = testkit::hold_polls("held-attempt").await;
        std::fs::write(&p, tick()).unwrap();
        assert!(s.poll().await.is_empty());
        let facts = s.finish(vec![], Duration::ZERO).await;
        assert_eq!(report(&facts, Measurement::Final).tokens.input, Some(40));
        drop(hold);
        let live = s.poll().await;
        assert_eq!(
            report(&live, Measurement::Cumulative).tokens.input,
            Some(40)
        );
    }

    #[tokio::test]
    async fn late_creation_partial_record_and_second_tail_tick_are_settled() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        let line = tick();
        let cut = line.len() / 2;
        let mut file = None;
        let mut step = 0usize;
        // Drive each tail poll deterministically:
        // read 0 sees no file yet; tick 1 creates the file with a partial record;
        // tick 2 completes the first record; tick 3 appends a second tail tick.
        let facts = s
            .finish_with(vec![], || {
                step += 1;
                match step {
                    1 => {
                        let mut f = std::fs::OpenOptions::new()
                            .append(true)
                            .create(true)
                            .open(&p)
                            .unwrap();
                        f.write_all(line[..cut].as_bytes()).unwrap();
                        file = Some(f);
                    }
                    2 => {
                        file.as_mut()
                            .unwrap()
                            .write_all(line[cut..].as_bytes())
                            .unwrap();
                    }
                    3 => {
                        file.as_mut()
                            .unwrap()
                            .write_all(
                                line.replace("00.030Z", "00.040Z")
                                    .replace(":100", ":150")
                                    .replace(":60", ":80")
                                    .replace(":20", ":30")
                                    .as_bytes(),
                            )
                            .unwrap();
                    }
                    _ => return std::future::ready(false),
                }
                std::future::ready(true)
            })
            .await;
        let t = report(&facts, Measurement::Final).tokens;
        assert_eq!(
            (t.input, t.cache_read, t.output),
            (Some(70), Some(80), Some(30))
        );
        assert!(
            !facts
                .iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. }))),
            "settled tail must not emit an incomplete diagnostic: {facts:?}"
        );
    }

    #[tokio::test]
    async fn native_final_wins_and_only_unknown_fields_are_filled() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        std::fs::write(p, tick()).unwrap();
        let mut n = Normalizer::new("/", "attempt", Epoch::new("epoch").unwrap());
        let native = n.prompt_response(&json!({"usage":{"inputTokens":7,"outputTokens":9}}));
        let facts = s.finish(native, Duration::ZERO).await;
        let t = report(&facts, Measurement::Final).tokens;
        assert_eq!(
            (t.input, t.cache_read, t.output, t.reasoning),
            (Some(7), Some(60), Some(9), Some(5))
        );
        assert_eq!(facts.iter().filter(|f|matches!(f,Fact::Event(EventBody::Usage{report}) if report.scope==UsageScope::Main)).count(),1);
    }

    #[tokio::test]
    async fn missing_native_and_rollout_is_explicit_unknown_not_zero() {
        let mut s = Sampler::start_source(None, "s", "attempt", Epoch::new("epoch").unwrap()).await;
        let facts = s.finish(vec![], Duration::ZERO).await;
        assert_eq!(report(&facts, Measurement::Final).tokens, Tokens::default());
        assert!(facts.iter().any(
            |f| matches!(f,Fact::Event(EventBody::Diagnostic{code,..}) if code=="usage_unavailable")
        ));
        let mut n = Normalizer::new("/", "attempt", Epoch::new("epoch").unwrap());
        let native = n.prompt_response(&json!({"usage":{"inputTokens":0,"outputTokens":0}}));
        let facts = s.finish(native, Duration::ZERO).await;
        assert_eq!(report(&facts, Measurement::Final).tokens.input, Some(0));
        assert!(
            !facts
                .iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. })))
        );
    }

    #[test]
    fn native_reports_replay_and_inclusive_input_missing_invalid_cache_stays_unknown() {
        let mut n = Normalizer::new("/", "attempt", Epoch::new("epoch").unwrap())
            .input_includes_cache(true);
        let response = json!({"usage":{"inputTokens":100,"cachedInputTokens":60,"outputTokens":20,"reasoningOutputTokens":5}});
        let a = n.prompt_response(&response);
        let b = n.prompt_response(&response);
        assert_eq!(a, b);
        assert_eq!(
            report(&a, Measurement::Final).tokens,
            Tokens {
                input: Some(40),
                cache_read: Some(60),
                output: Some(20),
                reasoning: Some(5),
                cache_write: None
            }
        );
        let missing = n.prompt_response(&json!({"usage":{"inputTokens":100}}));
        assert_eq!(report(&missing, Measurement::Final).tokens.input, None);
        let invalid = n.prompt_response(&json!({"usage":{"inputTokens":2,"cachedInputTokens":60}}));
        assert_eq!(report(&invalid, Measurement::Final).tokens.input, None);
        let u = json!({"sessionUpdate":"usage_update","used":2000,"size":10000,"cost":{"amount":0.01,"currency":"USD"}});
        let a = n.update(&u);
        let b = n.update(&u);
        assert_eq!(a, b);
        assert_eq!(
            report(&a, Measurement::Cumulative).tokens,
            Tokens::default()
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_during_a_blocking_read_preserves_the_cursor_for_settlement() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        std::fs::write(p, tick()).unwrap();
        let reader = s.reader.as_ref().unwrap().clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let acquired = barrier.clone();
        let thread = std::thread::spawn(move || {
            let _guard = reader.lock().unwrap();
            acquired.wait();
            std::thread::sleep(Duration::from_millis(50));
        });
        barrier.wait();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), s.read(false))
                .await
                .is_err()
        );
        let facts = s.finish(vec![], Duration::ZERO).await;
        thread.join().unwrap();
        assert_eq!(report(&facts, Measurement::Final).tokens.input, Some(40));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_source_does_not_block_settlement_or_queue_more_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = sampler(dir.path()).await;
        let reader = s.reader.clone().unwrap();
        let (ready, acquired) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _lock = reader.lock().unwrap();
            ready.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        acquired.recv().unwrap();
        s.read(false).await;
        let id = s.in_flight.as_ref().unwrap().id();
        let started = tokio::time::Instant::now();
        let facts = s.finish(vec![], Duration::from_millis(10)).await;
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(
            s.in_flight.as_ref().unwrap().id(),
            id,
            "one worker, no queued reads"
        );
        assert_eq!(report(&facts, Measurement::Final).tokens, Tokens::default());
        release.send(()).unwrap();
        worker.join().unwrap();
        s.read(false).await;
    }

    #[tokio::test]
    async fn fallback_never_relabels_a_different_native_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        std::fs::write(p, tick()).unwrap();
        let mut n = Normalizer::new("/", "attempt", Epoch::new("other").unwrap());
        let native = n.prompt_response(&json!({"usage":{"inputTokens":2,"outputTokens":3}}));
        let facts = s.finish(native, Duration::ZERO).await;
        let epochs: std::collections::BTreeSet<_> = facts
            .iter()
            .filter_map(|f| match f {
                Fact::Event(EventBody::Usage { report }) => Some(report.meter_epoch.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(epochs, std::collections::BTreeSet::from(["epoch", "other"]));
    }

    #[tokio::test]
    async fn explicit_cache_creation_survives_live_replay_and_final() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        let mut replay = sampler(dir.path()).await;
        let tick = tick().replace(
            "\"cached_input_tokens\":60",
            "\"cached_input_tokens\":60,\"cache_write_input_tokens\":0",
        );
        std::fs::write(p, tick).unwrap();
        let live = s.poll().await;
        assert_eq!(
            report(&live, Measurement::Cumulative).tokens.cache_write,
            Some(0)
        );
        assert_eq!(live, replay.poll().await);
        let final_ = s.finish(vec![], Duration::ZERO).await;
        assert_eq!(
            report(&final_, Measurement::Final).tokens.cache_write,
            Some(0)
        );
    }
    #[tokio::test]
    async fn lost_transcript_after_live_cannot_turn_persisted_spend_into_synthetic_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("projects/test/s.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut s = Sampler::start_source(None, "s", "attempt", Epoch::new("epoch").unwrap()).await;
        let transcript = Transcript::new(dir.path().into(), "s".into(), 20);
        s.source_end = Some(transcript.end_fence.clone());
        s.reader = Some(Arc::new(Mutex::new(Reader::Transcript(transcript))));
        s.read(true).await;
        std::fs::write(&p,format!("{}\n",json!({"type":"assistant","sessionId":"s","timestamp":"1970-01-01T00:00:00.030Z","message":{"id":"one","model":"source","usage":{"input_tokens":6,"cache_creation_input_tokens":7,"cache_read_input_tokens":8,"output_tokens":9}}}))).unwrap();
        let live = s.poll().await;
        assert_eq!(report(&live, Measurement::Cumulative).tokens.input, Some(6));
        std::fs::remove_file(p).unwrap();
        s.poll().await;
        s.poll().await;
        let mut n = Normalizer::new("/", "attempt", Epoch::new("epoch").unwrap());
        let native=n.prompt_response(&json!({"usage":{"inputTokens":0,"outputTokens":0,"cachedReadTokens":0,"cachedWriteTokens":0}}));
        let facts = s.finish(native, Duration::ZERO).await;
        assert_eq!(report(&facts, Measurement::Final).tokens.input, Some(6));
        assert!(facts.iter().any(
            |f| matches!(f,Fact::Event(EventBody::Diagnostic{code,..})if code=="usage_incomplete")
        ));
    }
    #[tokio::test]
    async fn known_native_zero_still_wins_for_a_source_without_synthetic_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let mut s = sampler(dir.path()).await;
        std::fs::write(p, tick()).unwrap();
        s.poll().await;
        let mut n = Normalizer::new("/", "attempt", Epoch::new("epoch").unwrap());
        let native=n.prompt_response(&json!({"usage":{"inputTokens":0,"outputTokens":0,"cachedReadTokens":0,"cachedWriteTokens":0}}));
        let facts = s.finish(native, Duration::ZERO).await;
        let t = report(&facts, Measurement::Final).tokens;
        assert_eq!(t.input, Some(0));
        assert_eq!(t.output, Some(0));
        assert_eq!(t.cache_read, Some(0));
        assert_eq!(t.cache_write, Some(0));
    }
}
