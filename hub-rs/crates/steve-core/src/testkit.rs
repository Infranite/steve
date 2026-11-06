//! Test doubles: a scripted agent runtime that behaves like acp.ts's turn bookkeeping, a canned model
//! gateway, and a temp-dir harness.

use crate::error::{Result, msg};
use crate::gateway::{ModelGateway, Tier};
use crate::repo::{Db as Store, HubEvent};
use crate::runtime::{
    AgentRuntime, Answered, Configured, Flushed, Prompted, Released, Running, Stop, TurnEnd,
};
use crate::{Core, CoreConfig};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

#[derive(Default)]
pub struct FakeState {
    /// (session, text, via) of every turn started
    pub prompts: Vec<(String, String, String)>,
    pub turning: HashSet<String>,
    /// The attempt each turning session runs now (`a1`, `a2`, … across the runtime).
    pub attempts: HashMap<String, String>,
    pub next_attempt: u64,
    /// Sessions whose running turn a stop reached.
    pub cancelled: Vec<String>,
    /// Every `stop` and what it did.
    pub stops: Vec<(Running, Stop)>,
    /// The user's stops ([`AgentRuntime::stop`]), each a cancel too.
    pub stopped: Vec<String>,
    pub released: Vec<String>,
    /// What `release` answers per session ([`Released::Gone`] when not set).
    pub release_answers: HashMap<String, Released>,
    /// `created` calls: (session, the release it waits for)
    pub created: Vec<(String, Option<String>)>,
    /// decision id → still answerable
    pub permissions: HashSet<String>,
    /// decision id → answerable through a queued command (a Node's): answers are [`Answered::Pending`]
    pub pending_permissions: HashSet<String>,
    /// Every answer fails (the backend's store, say).
    pub answer_error: bool,
    pub answered: Vec<(String, Option<String>)>,
    pub holding: bool,
    /// `set_mode` calls: (session, mode)
    pub modes: Vec<(String, String)>,
    /// Every `set_mode` fails (not recorded in `modes`).
    pub mode_error: bool,
    /// Every `set_mode` is a mode the backend cannot express (recorded in `modes`).
    pub mode_unsupported: bool,
    /// (session, files) of every prompt sent right away
    pub files: Vec<(String, Vec<crate::repo::FileRef>)>,
    /// Per `flush`: whether it ran inside a tokio runtime (the real runtime spawns the turn there).
    pub flush_in_runtime: Vec<bool>,
    /// Sessions whose last attempt's outcome nobody can tell ([`AgentRuntime::uncertain`]).
    pub uncertain: HashSet<String>,
    /// Sessions whose backend recovers them after a restart ([`AgentRuntime::recovers_itself`]).
    pub recovers_itself: HashSet<String>,
    /// `flush` takes nothing and answers false (a session still opening, say).
    pub flush_stuck: bool,
    /// The next `stop` comes too late: the turn it names ended just before and took the queue as
    /// its follow-up (a new attempt), so the stop reaches nothing.
    pub follow_up_before_stop: bool,
    /// The adapter pid [`AgentRuntime::agent_pid`] gives a session (YOLO's `ps` root).
    pub agent_pids: HashMap<String, u32>,
    /// How often [`AgentRuntime::agent_pid`] was asked.
    pub pid_asks: usize,
}

/// acp.ts in miniature: turns are tracked, prompts queue behind a running turn or the hold, and a test
/// ends turns explicitly with [`FakeRuntime::end_turn`].
pub struct FakeRuntime {
    pub store: Store,
    pub s: Mutex<FakeState>,
    /// Signalled after every `flush`.
    pub flushed: tokio::sync::Notify,
    /// Every `stop` takes it shared first: a test holding it exclusively delays the stops.
    pub stop_gate: RwLock<()>,
}

impl FakeRuntime {
    pub fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.s.lock().unwrap()
    }
    fn run(&self, st: &mut FakeState, sid: &str, msgs: &[(String, String)]) {
        st.turning.insert(sid.into());
        st.next_attempt += 1;
        let attempt = format!("a{}", st.next_attempt);
        st.attempts.insert(sid.into(), attempt);
        for (t, v) in msgs {
            let _ = self
                .store
                .event_add(sid, "user", json!({"text": t, "via": v}));
        }
        let text = msgs
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let via = msgs.first().map(|(_, v)| v.clone()).unwrap_or_default();
        st.prompts.push((sid.into(), text, via));
        let s = self.store.session(sid).ok().flatten();
        let status = if s.and_then(|s| s.acp_session_id).is_some() {
            "running"
        } else {
            "starting"
        };
        let _ = self.store.session_patch(sid, crate::patch![status: status]);
    }
    /// Texts prompted to a session, in order.
    pub fn prompts_of(&self, sid: &str) -> Vec<String> {
        self.st()
            .prompts
            .iter()
            .filter(|p| p.0 == sid)
            .map(|p| p.1.clone())
            .collect()
    }
    /// End the running turn like acp.ts `runTurn`: queued rows run next (no turn-end callback), else the core hears about it.
    pub fn end_turn(&self, core: &Core, sid: &str, stop: &str, error: Option<&str>) {
        let (next, attempt) = {
            let mut st = self.st();
            st.turning.remove(sid);
            let attempt = st.attempts.remove(sid);
            let _ = self.store.session_patch(
                sid,
                crate::patch![status: if stop == "error" { "error" } else { "idle" }],
            );
            let _ = self
                .store
                .event_add(sid, "turn_end", json!({"stopReason": stop}));
            let q = if st.holding {
                vec![]
            } else {
                self.store.queue_take(sid).unwrap()
            };
            if !q.is_empty() {
                let msgs: Vec<_> = q.into_iter().map(|q| (q.text, q.via)).collect();
                self.run(&mut st, sid, &msgs);
                return;
            }
            (
                st.holding && !self.store.queued(sid).unwrap().is_empty(),
                attempt,
            )
        };
        if next {
            return;
        }
        core.on_turn_end(TurnEnd {
            session_id: sid.into(),
            attempt,
            stop_reason: stop.into(),
            error: error.map(String::from),
        })
        .unwrap();
    }
    /// The agent says something (stored like an adapter's `agent_message_chunk`).
    pub fn say(&self, sid: &str, text: &str) {
        self.store.event_add(sid, "agent_message_chunk", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})).unwrap();
    }
}

impl AgentRuntime for FakeRuntime {
    fn prompt(
        &self,
        sid: &str,
        text: &str,
        via: &str,
        files: &[crate::repo::FileRef],
    ) -> Result<Prompted> {
        let s = self.store.session(sid)?.ok_or_else(|| msg("no session"))?;
        let mut st = self.st();
        if st.turning.contains(sid) || st.holding {
            if st.holding && !st.turning.contains(sid) && s.status == "starting" {
                self.store
                    .session_patch(sid, crate::patch![status: "idle"])?;
            }
            if self.store.queue_add_unless_waiting(sid, text, via, files)? {
                let mut ev = json!({"text": text, "via": via});
                if !files.is_empty() {
                    ev["files"] = json!(files);
                }
                self.store.event_add(sid, "queued", ev)?;
            }
            return Ok(Prompted::Queued);
        }
        st.files.push((sid.into(), files.to_vec()));
        self.run(&mut st, sid, &[(text.into(), via.into())]);
        Ok(Prompted::Sent)
    }
    fn running(&self, sid: &str) -> Option<Running> {
        self.st().attempts.get(sid).map(|a| Running {
            session: sid.into(),
            attempt: a.clone(),
        })
    }
    fn stop(&self, turn: &Running) -> Result<Stop> {
        let _gate = self.stop_gate.read().unwrap();
        let mut st = self.st();
        if std::mem::take(&mut st.follow_up_before_stop) {
            let taken = self.store.queue_take(&turn.session).unwrap();
            let msgs: Vec<_> = taken.into_iter().map(|q| (q.text, q.via)).collect();
            self.run(&mut st, &turn.session, &msgs);
            self.store
                .store()
                .queued()
                .submitted(&turn.session)
                .unwrap();
        }
        let stop = if st.attempts.get(&turn.session) == Some(&turn.attempt) {
            st.cancelled.push(turn.session.clone());
            Stop::Requested
        } else {
            Stop::NotRunning
        };
        st.stops.push((turn.clone(), stop));
        Ok(stop)
    }
    fn user_stop(&self, sid: &str) -> Result<Stop> {
        self.st().stopped.push(sid.into());
        self.cancel(sid)
    }
    fn is_busy(&self, sid: &str) -> bool {
        self.st().turning.contains(sid)
    }
    fn created(&self, sid: &str, after: Option<&str>) {
        self.st()
            .created
            .push((sid.into(), after.map(String::from)));
    }
    fn release(&self, sid: &str) -> Result<Released> {
        let mut st = self.st();
        st.released.push(sid.into());
        Ok(st
            .release_answers
            .get(sid)
            .cloned()
            .unwrap_or(Released::Gone))
    }
    fn answer_permission(&self, decision: &str, option: Option<&str>) -> Result<Answered> {
        let mut st = self.st();
        if st.answer_error {
            return Err(msg("store unavailable"));
        }
        st.answered
            .push((decision.into(), option.map(String::from)));
        Ok(if st.permissions.remove(decision) {
            Answered::Applied
        } else if st.pending_permissions.remove(decision) {
            Answered::Pending
        } else {
            Answered::Gone
        })
    }
    fn hold(&self, on: bool) {
        self.st().holding = on;
    }
    fn set_mode(&self, sid: &str, mode: &str) -> Result<Configured> {
        let mut st = self.st();
        if st.mode_error {
            return Err(msg("store unavailable"));
        }
        st.modes.push((sid.into(), mode.into()));
        Ok(if st.mode_unsupported {
            Configured::Unsupported
        } else {
            Configured::Applied
        })
    }
    fn agent_pid(&self, sid: &str) -> Option<u32> {
        let mut st = self.st();
        st.pid_asks += 1;
        st.agent_pids.get(sid).copied()
    }
    fn is_holding(&self) -> bool {
        self.st().holding
    }
    fn running_turns(&self) -> usize {
        self.st().turning.len()
    }
    fn uncertain(&self, sid: &str) -> bool {
        self.st().uncertain.contains(sid)
    }
    fn recovers_itself(&self, sid: &str) -> bool {
        self.st().recovers_itself.contains(sid)
    }
    fn flush(&self, sid: &str, lead: Option<(&str, &str)>) -> Flushed {
        let sent = self.flush_now(sid, lead);
        self.flushed.notify_one();
        sent
    }
}

impl FakeRuntime {
    fn flush_now(&self, sid: &str, lead: Option<(&str, &str)>) -> Flushed {
        let mut st = self.st();
        st.flush_in_runtime
            .push(tokio::runtime::Handle::try_current().is_ok());
        if st.turning.contains(sid) {
            return Flushed::Busy;
        }
        if st.holding || st.flush_stuck {
            return Flushed::Held;
        }
        let mut seen = HashSet::new();
        let mut msgs: Vec<(String, String)> = lead
            .map(|(t, v)| vec![(t.to_string(), v.to_string())])
            .unwrap_or_default();
        let taken = self.store.queue_take(sid).unwrap();
        let files: Vec<_> = taken.iter().flat_map(|q| q.files.iter().cloned()).collect();
        if !files.is_empty() {
            st.files.push((sid.into(), files));
        }
        msgs.extend(taken.into_iter().map(|q| (q.text, q.via)));
        msgs.retain(|(t, _)| seen.insert(t.clone()));
        if msgs.is_empty() {
            return Flushed::Empty;
        }
        self.run(&mut st, sid, &msgs);
        self.store.store().queued().submitted(sid).unwrap();
        Flushed::Started(Running {
            session: sid.into(),
            attempt: st.attempts[sid].clone(),
        })
    }
}

type Script = Box<dyn Fn(&str, Tier) -> Result<String> + Send + Sync>;

/// Answers model calls from a script; records every prompt.
pub struct FakeGateway {
    pub script: Mutex<Script>,
    pub calls: Mutex<Vec<String>>,
    /// the tools of each call
    pub tools: Mutex<Vec<crate::gateway::ThinkTools>>,
    /// the image blocks of each call ([`crate::gateway::with_images`])
    pub images: Mutex<Vec<Vec<serde_json::Value>>>,
    /// how long a call whose prompt starts with the text takes (awaited, so a stop can cut it short)
    pub slow: Mutex<Option<(&'static str, Duration)>>,
}
impl FakeGateway {
    pub fn set(&self, f: impl Fn(&str, Tier) -> Result<String> + Send + Sync + 'static) {
        *self.script.lock().unwrap() = Box::new(f);
    }
}
#[async_trait]
impl ModelGateway for FakeGateway {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        _t: Duration,
        tools: &crate::gateway::ThinkTools,
    ) -> Result<crate::gateway::Printed> {
        self.calls.lock().unwrap().push(prompt.into());
        self.tools.lock().unwrap().push(tools.clone());
        let slow = *self.slow.lock().unwrap();
        if let Some((_, d)) = slow.filter(|(head, _)| prompt.starts_with(head)) {
            tokio::time::sleep(d).await;
        }
        self.images.lock().unwrap().push(
            crate::gateway::current_images()
                .map(|i| i.to_vec())
                .unwrap_or_default(),
        );
        let text = (self.script.lock().unwrap())(prompt, tier)?;
        Ok(crate::gateway::Printed {
            text,
            model: "fake-model".into(),
            cost_usd: None,
            ..Default::default()
        })
    }
}

pub struct Kit {
    pub dir: tempfile::TempDir,
    pub core: Core,
    pub rt: Arc<FakeRuntime>,
    pub gw: Arc<FakeGateway>,
    pub events: Arc<Mutex<Vec<HubEvent>>>,
}

impl Kit {
    pub fn new() -> Kit {
        Kit::with(|_| {})
    }
    pub fn with(f: impl FnOnce(&mut CoreConfig)) -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = CoreConfig::for_home(dir.path());
        /* the boot resume runs when a test says so (`core.fire_resume()`), not on a clock */
        cfg.resume_delay = Duration::from_secs(3600);
        /* a stopped agent reaches the card at once unless a test lets Steve continue it (`takeover`) */
        cfg.takeover_continues = 0;
        cfg.gh = "/nonexistent/gh".into();
        /* the machine's free space decides nothing unless a test sets thresholds (`workspace`) */
        cfg.disk = crate::workspace::DiskPolicy {
            floor: 0,
            low: 0,
            idle_ms: i64::MAX,
        };
        f(&mut cfg);
        for d in [&cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        let events = Arc::new(Mutex::new(vec![]));
        let ev = events.clone();
        let raw = steve_store::Store::open(
            dir.path().join("data/steve.sqlite3"),
            steve_store::FnPublisher(move |e| ev.lock().unwrap().push(e)),
        )
        .unwrap();
        let store = Store::new(raw);
        let rt = Arc::new(FakeRuntime {
            store: store.clone(),
            s: Mutex::new(FakeState::default()),
            flushed: tokio::sync::Notify::new(),
            stop_gate: RwLock::new(()),
        });
        let gw = Arc::new(FakeGateway {
            script: Mutex::new(Box::new(|_, _| Err(msg("no model in tests")))),
            calls: Mutex::new(vec![]),
            tools: Mutex::new(vec![]),
            images: Mutex::new(vec![]),
            slow: Mutex::new(None),
        });
        let core = Core::new(store, rt.clone(), gw.clone(), cfg);
        core.seed().unwrap();
        Kit {
            dir,
            core,
            rt,
            gw,
            events,
        }
    }
    pub fn db(&self) -> &Store {
        self.core.store()
    }
    /// One outbox pass with Work's consumers (`work.tidy`), as the Hub's dispatcher does after a commit.
    pub fn pump_outbox(&self) -> steve_store::outbox::Pumped {
        self.pump_outbox_at(steve_store::now())
    }
    /// [`Self::pump_outbox`] at `now` (past a back-off).
    pub fn pump_outbox_at(&self, now: i64) -> steve_store::outbox::Pumped {
        let s = self.core.store().store();
        let d = steve_store::outbox::Dispatcher::new(self.core.outbox_consumers());
        d.register(s).unwrap();
        d.pump(s, now).unwrap()
    }
    pub fn home(&self) -> &std::path::Path {
        self.dir.path()
    }
    /// Wait (≤3 s) until `f` holds.
    pub fn wait(&self, f: impl Fn() -> bool) -> bool {
        for _ in 0..300 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
    pub fn open_kinds(&self, item: &str) -> Vec<String> {
        self.db()
            .decisions_of_item(item)
            .unwrap()
            .into_iter()
            .filter(|d| d.status == "open")
            .map(|d| d.kind)
            .collect()
    }
    /// An uploaded attachment of the default workspace (row + blob).
    pub fn file(&self, name: &str, mime: &str, bytes: &[u8]) -> crate::repo::FileRef {
        let f = crate::repo::FileRow {
            id: crate::util::uid("fil"),
            workspace_id: crate::repo::WS.into(),
            sha256: crate::util::sha(&String::from_utf8_lossy(bytes)),
            name: name.into(),
            mime: mime.into(),
            size: bytes.len() as i64,
            created_by: "me".into(),
            created: crate::util::now(),
        };
        let p = crate::attachments::blob_path(&self.core.config().data_dir, &f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        self.db().file_create(&f).unwrap();
        f.file_ref()
    }
    pub fn timeline_texts(&self, item: &str) -> Vec<String> {
        self.db()
            .timeline(item)
            .unwrap()
            .into_iter()
            .map(|t| t.text)
            .collect()
    }
}

/// One step of a scripted research run.
#[derive(Clone)]
pub enum Step {
    /// the answer grows by this much
    Text(&'static str),
    /// a tool call finished
    Tool(&'static str, &'static str),
    ToolFailure(&'static str, &'static str, &'static str),
    /// pause (lets drafts go out, lets a test act while it runs)
    Sleep(u64),
    /// wait until the test opens the gate (or the run is stopped)
    Gate,
}

/// A research executor that plays a script.
pub struct FakeResearch {
    pub script: Mutex<Vec<Step>>,
    pub offer: Mutex<Option<crate::gateway::Offer>>,
    pub gate: tokio::sync::Semaphore,
    /// every request it got
    pub seen: Mutex<Vec<crate::thread::ResearchRequest>>,
}

impl FakeResearch {
    pub fn new(script: Vec<Step>) -> Arc<FakeResearch> {
        Arc::new(FakeResearch {
            script: Mutex::new(script),
            offer: Mutex::new(None),
            gate: tokio::sync::Semaphore::new(0),
            seen: Mutex::new(vec![]),
        })
    }
    pub fn open(&self, n: usize) {
        self.gate.add_permits(n);
    }
}

#[async_trait]
impl crate::thread::Researcher for FakeResearch {
    async fn research(
        &self,
        req: crate::thread::ResearchRequest,
        sink: &dyn crate::thread::ResearchSink,
    ) -> crate::thread::ResearchOutcome {
        use std::sync::atomic::Ordering;
        self.seen.lock().unwrap().push(req.clone());
        let script = self.script.lock().unwrap().clone();
        let mut text = String::new();
        let mut end = crate::thread::RunEnd::Done;
        for s in script {
            if req.stop.load(Ordering::SeqCst) {
                end = crate::thread::RunEnd::Stopped;
                break;
            }
            match s {
                Step::Text(t) => {
                    text.push_str(t);
                    sink.text(&text);
                }
                Step::Tool(name, brief) => sink.tool(&crate::thread::ToolSeen {
                    name: name.into(),
                    brief: brief.into(),
                    ok: true,
                    error: None,
                    ms: 3,
                }),
                Step::ToolFailure(name, brief, error) => sink.tool(&crate::thread::ToolSeen {
                    name: name.into(),
                    brief: brief.into(),
                    ok: false,
                    error: Some(error.into()),
                    ms: 1,
                }),
                Step::Sleep(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Step::Gate => loop {
                    if req.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(p) = self.gate.try_acquire() {
                        p.forget();
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                },
            }
        }
        if req.stop.load(Ordering::SeqCst) {
            end = crate::thread::RunEnd::Stopped;
        }
        crate::thread::ResearchOutcome {
            text,
            offer: self.offer.lock().unwrap().clone(),
            end,
            model: Some("fake-research".into()),
            claude_session: Some("cs_1".into()),
            turns: 2,
            ..Default::default()
        }
    }
}

/* release scripts run in their own process group (they must outlive the Hub they restart); a test's must not
 * outlive the test, or it polls the deleted temp dir forever */
impl Drop for Kit {
    fn drop(&mut self) {
        crate::release::test_spawned::kill(&self.core.config().home);
    }
}
