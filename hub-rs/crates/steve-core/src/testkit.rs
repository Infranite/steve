//! Test doubles: a scripted agent runtime that behaves like acp.ts's turn bookkeeping, a canned model
//! gateway, and a temp-dir harness.

use crate::error::{Result, msg};
use crate::gateway::{ModelGateway, Tier};
use crate::repo::{BusEvent, Db as Store};
use crate::runtime::{AgentRuntime, Answered, Flushed, Prompted, Released, Running, Stop, TurnEnd};
use crate::{Core, CoreConfig};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use steve_store::Ingest;

#[derive(Default)]
pub struct FakeState {
    pub placement_rejections: HashMap<String, steve_types::PlacementRejection>,
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
    /// A Node starts only after another thread can read the committed session (ST-174).
    pub starts_require_commit: bool,
    /// decision id → still answerable
    pub permissions: HashSet<String>,
    /// decision id → answerable through a queued command (a Node's): answers are [`Answered::Pending`]
    pub pending_permissions: HashSet<String>,
    /// Every answer fails (the backend's store, say).
    pub answer_error: bool,
    /// A decision's input queue refuses the write.
    pub decision_prompt_error: bool,
    /// The user's stop fails: the backend cannot tell whether it reached the turn (its Node
    /// unreachable, say). Recorded in `stopped`, nothing is cancelled.
    pub user_stop_error: bool,
    pub answered: Vec<(String, Option<String>)>,
    pub holding: bool,
    /// `set_mode` calls: (session, mode)
    /// Every `set_mode` fails (not recorded in `modes`).
    /// Every `set_mode` is a mode the backend cannot express (recorded in `modes`).
    /// (session, files) of every prompt sent right away
    pub files: Vec<(String, Vec<crate::repo::FileRef>)>,
    /// Per `flush`: whether it ran inside a tokio runtime (the real runtime spawns the turn there).
    pub flush_in_runtime: Vec<bool>,
    /// Sessions whose last attempt's outcome nobody can tell ([`AgentRuntime::uncertain`]).
    pub uncertain: HashSet<String>,
    /// Sessions whose backend recovers them after a restart ([`AgentRuntime::recovers_itself`]).
    pub recovers_itself: HashSet<String>,
    /// Why new sessions cannot run ([`AgentRuntime::refusal`]); `None`: they run.
    pub refuse: Option<String>,
    /// Why no Bot can run an item now ([`AgentRuntime::can_run`], the preflight); `None`: they can.
    pub cannot_run: Option<String>,
    /// Every session's Node has no link, for this reason.
    pub node_offline: Option<String>,
    /// `flush` takes nothing and answers false (a session still opening, say).
    pub flush_stuck: bool,
    /// The next `stop` comes too late: the turn it names ended just before and took the queue as
    /// its follow-up (a new attempt), so the stop reaches nothing.
    pub follow_up_before_stop: bool,
    /// The backend makes items' worktrees ([`AgentRuntime::makes_worktrees`]).
    pub makes_worktrees: bool,
    /// `created` binds the session where its worktree is made (else it stays with the core).
    pub binds_worktrees: bool,
    /// `created` binds the session to a directory it only opens (its item was first prepared
    /// there as an existing directory): it makes no worktree.
    pub binds_existing: bool,
    /// Per `created` that bound a session: whether its directory was there by then.
    pub bound_with_dir: Vec<bool>,
    /// Sessions whose worktree the backend makes ([`AgentRuntime::makes_workspace`]).
    pub node_made: HashSet<String>,
    /// `tidy` calls: (session, keep_branch).
    pub tidies: Vec<(String, bool)>,
    /// What [`AgentRuntime::mode_state`] says per session (none: it cannot tell).
    pub mode_states: HashMap<String, crate::runtime::ModeState>,
    /// Optional BotProposal returned by [`AgentRuntime::propose_bot`].
    pub bot_proposal: Option<steve_types::BotProposal>,
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
    fn session_blocker(&self, sid: &str) -> Result<Option<steve_store::item_gate::GateBlocker>> {
        Ok(steve_store::core_fixture::thread::session_blocker(
            self.store.store(),
            sid,
        )?)
    }

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
            let q = if st.holding || self.session_blocker(sid).unwrap().is_some() {
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
    /// The agent says something (stored as a `steve.v1` text entry, as the Node path writes it).
    pub fn say(&self, sid: &str, text: &str) {
        self.store
            .event_add(
                sid,
                steve_transcript::TEXT,
                json!({"role": "assistant", "text": text}),
            )
            .unwrap();
    }
}

impl AgentRuntime for FakeRuntime {
    fn assess_placement_node(
        &self,
        _workspace: &str,
        _project: Option<&steve_store::Project>,
        node: &str,
        _choice: &steve_node_api::HarnessChoice,
        _mode: &str,
    ) -> Result<Option<steve_types::PlacementRejection>> {
        Ok(self.st().placement_rejections.get(node).copied())
    }

    fn can_run(&self, _item: &str, _bot: &str, _harness: &str) -> Result<()> {
        match self.st().cannot_run.clone() {
            Some(why) => Err(msg(why)),
            None => Ok(()),
        }
    }
    fn propose_bot(
        &self,
        req: &crate::CreateItem,
        _blocked: &steve_types::PlacementBlocked,
    ) -> Result<Option<steve_types::BotProposal>> {
        let Some(draft) = self.st().bot_proposal.clone() else {
            return Ok(None);
        };
        let actor = req
            .actor
            .as_deref()
            .or(req.requester.as_deref())
            .unwrap_or("owner");
        let project = req.project.as_ref().and_then(|p| p.as_deref());
        self.store.store().write(|w| {
            if steve_store::bot_dispatch::proposal_by_id(w, &req.ws, &draft.id)?.is_none() {
                steve_store::bot_dispatch::save_proposal(w, &req.ws, actor, project, &draft)?;
            }
            Ok::<_, steve_store::StoreError>(())
        })?;
        Ok(Some(draft))
    }
    fn confirm_bot_proposal(
        &self,
        ws: &str,
        actor: &str,
        id: &str,
        edit: steve_types::ConfirmBotProposal,
    ) -> Result<String> {
        self.store.store().write::<_, crate::CoreError>(|w| {
            let Some(proposal) = steve_store::bot_dispatch::proposal_by_id(w, ws, id)? else {
                return Err(msg("Bot proposal not found"));
            };
            if let Some(bot) = proposal.confirmed_bot {
                return Ok(bot);
            }
            if edit.expected_revision != proposal.draft.revision {
                return Err(msg("Bot proposal revision mismatch"));
            }
            let bot = self
                .store
                .store()
                .bots()
                .create(
                    ws,
                    &steve_store::NewBot {
                        name: edit.name,
                        role: Some(edit.role),
                        specialties: Some(edit.specialties),
                        harness: edit.harness,
                        model: edit.model,
                        perm_mode: Some("ask".into()),
                        ..Default::default()
                    },
                    actor,
                )?
                .ok_or_else(|| msg("created bot missing"))?;
            steve_store::bot_admin::set_bindings(
                w,
                ws,
                &bot.id,
                &[proposal.draft.node.as_str().to_owned()],
            )?;
            if let Some(team) = &proposal.draft.team
                && let Some(mut existing) =
                    steve_store::bot_dispatch::selected(w, ws, Some(&team.id))?
            {
                existing.members.push(steve_store::bot_dispatch::Member {
                    bot: bot.id.clone(),
                    nodes: None,
                });
                steve_store::bot_admin::save(
                    w,
                    ws,
                    Some(&existing.id),
                    &existing.name,
                    &existing.members,
                )?;
            }
            steve_store::bot_dispatch::confirm(w, id, &bot.id)?;
            Ok(bot.id)
        })
    }
    fn prompt(
        &self,
        sid: &str,
        text: &str,
        via: &str,
        files: &[crate::repo::FileRef],
    ) -> Result<Prompted> {
        if let Some(why) = self.session_blocker(sid)? {
            return Err(msg(why.code()));
        }
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
    fn stage_decision_prompt(
        &self,
        w: &mut steve_store::W<'_>,
        sid: &str,
        text: &str,
    ) -> Result<()> {
        if let Some(why) = steve_store::item_gate::session_blocker(w, sid)? {
            return Err(msg(why.code()));
        }
        if steve_store::node_sessions::ended(w, sid)? {
            return Err(msg("已结束的会话不能接收新消息，请使用当前会话。"));
        }
        let (err, queued, holding) = {
            let st = self.st();
            (
                st.decision_prompt_error,
                st.turning.contains(sid) || st.holding,
                st.holding && !st.turning.contains(sid),
            )
        };
        if err {
            return Err(msg("input queue refused"));
        }
        let at = steve_store::now();
        if holding {
            steve_store::node_sessions::set_status(w, sid, "idle", at)?;
        }
        steve_store::node_sessions::queue(w, &steve_store::uid("q"), sid, text, "steve", &[], at)?;
        if queued {
            w.events().add(
                sid,
                "queued",
                json!({"text": text, "via": "steve"}),
                None,
                at,
            )?;
        }
        steve_store::node_sessions::changed_event(w, sid)?;
        Ok(())
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
        {
            let mut st = self.st();
            st.stopped.push(sid.into());
            if st.user_stop_error {
                return Err(msg("Node 没有回应，不知道停止是否送达"));
            }
        }
        self.cancel(sid)
    }
    fn is_busy(&self, sid: &str) -> bool {
        self.st().turning.contains(sid)
    }
    fn created(&self, sid: &str, after: Option<&str>) {
        if self.st().starts_require_commit {
            let (store, id) = (self.store.clone(), sid.to_string());
            let visible = std::thread::spawn(move || store.session(&id).unwrap().is_some())
                .join()
                .unwrap();
            assert!(
                visible,
                "Node startup must follow the session transaction's commit"
            );
        }
        let mut st = self.st();
        st.created.push((sid.into(), after.map(String::from)));
        if st.makes_worktrees && st.binds_worktrees {
            if st.binds_existing {
                let there = self
                    .store
                    .session(sid)
                    .ok()
                    .flatten()
                    .is_some_and(|s| std::path::Path::new(&s.cwd).exists());
                st.bound_with_dir.push(there);
            } else {
                st.node_made.insert(sid.into());
            }
        }
    }
    fn makes_worktrees(&self) -> bool {
        self.st().makes_worktrees
    }
    fn would_make_workspace(&self, _sid: &str) -> bool {
        let st = self.st();
        st.makes_worktrees && st.binds_worktrees && !st.binds_existing
    }
    fn makes_workspace(&self, sid: &str) -> bool {
        self.st().node_made.contains(sid)
    }
    fn tidy(&self, sid: &str, keep_branch: bool) -> Result<bool> {
        let mut st = self.st();
        if !st.node_made.contains(sid) {
            return Ok(false);
        }
        st.tidies.push((sid.into(), keep_branch));
        Ok(true)
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
    fn answer_permission(&self, answer: crate::PermissionAnswer<'_>) -> Result<Answered> {
        let decision = answer.decision_id;
        let option = answer.option();
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
    fn mode_state(&self, sid: &str) -> Option<crate::runtime::ModeState> {
        self.st().mode_states.get(sid).cloned()
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
    fn node_offline(&self, _sid: &str) -> Option<String> {
        self.st().node_offline.clone()
    }
    fn recovers_itself(&self, sid: &str) -> bool {
        self.st().recovers_itself.contains(sid)
    }
    fn refusal(&self, _sid: &str) -> Option<String> {
        self.st().refuse.clone()
    }
    fn flush(&self, sid: &str, lead: Option<(&str, &str)>) -> Flushed {
        let sent = self.flush_now(sid, lead);
        self.flushed.notify_one();
        sent
    }
}

impl FakeRuntime {
    fn flush_now(&self, sid: &str, lead: Option<(&str, &str)>) -> Flushed {
        if self.session_blocker(sid).unwrap().is_some() {
            return Flushed::Held;
        }
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
    /// the model the fake harness names itself in its reports (`None`: it names none)
    pub reported: Mutex<Option<String>>,
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
        /* read before the call is listed: a test that sees the call may change `slow` for the next */
        let slow = *self.slow.lock().unwrap();
        self.calls.lock().unwrap().push(prompt.into());
        self.tools.lock().unwrap().push(tools.clone());
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
            reported_model: self.reported.lock().unwrap().clone(),
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
    pub events: Arc<Mutex<Vec<BusEvent>>>,
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
            reported: Mutex::new(Some("fake-model".into())),
        });
        let core = Core::new(store, rt.clone(), gw.clone(), cfg);
        core.seed().unwrap();
        /* the second Bot the tests give work to: an installation has it only beside Steve's own source */
        core.store()
            .bot_seed(
                "dev",
                false,
                crate::orchestrator::dev_bot(None),
                crate::repo::WS,
            )
            .unwrap();
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
    /// A Hub restart: a new core on the same SQLite file opened afresh (nothing kept in memory),
    /// with the same runtime, gateway and settings.
    pub fn reopened(&self) -> Core {
        let raw = steve_store::Store::open(
            self.dir.path().join("data/steve.sqlite3"),
            steve_store::NoPublisher,
        )
        .unwrap();
        Core::new(
            Store::new(raw),
            self.rt.clone(),
            self.gw.clone(),
            self.core.config().clone(),
        )
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
    /// Deliveries wait for your acceptance (the default before 2026-11-09), for tests of that path.
    pub fn manual_acceptance(&self) {
        self.db()
            .settings_set(
                crate::repo::WS,
                &serde_json::json!({"accDefault": "manual"})
                    .as_object()
                    .unwrap()
                    .clone(),
                "me",
            )
            .unwrap();
    }

    pub fn open_kinds(&self, item: &str) -> Vec<String> {
        self.db()
            .decisions_of_item(item)
            .unwrap()
            .into_iter()
            .filter(|d| d.status == steve_types::DecisionStatus::Open)
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
    /// a tool call of this category began (name, kind, brief)
    Running(&'static str, &'static str, &'static str),
    ToolFailure(&'static str, &'static str, &'static str),
    /// the executor stops the run with this outcome
    End(crate::thread::RunEnd),
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
                    call: None,
                    name: name.into(),
                    kind: String::new(),
                    brief: brief.into(),
                    ok: true,
                    error: None,
                    ms: 3,
                }),
                Step::Running(name, kind, brief) => sink.tool_running(None, name, kind, brief),
                Step::ToolFailure(name, brief, error) => sink.tool(&crate::thread::ToolSeen {
                    call: None,
                    name: name.into(),
                    kind: String::new(),
                    brief: brief.into(),
                    ok: false,
                    error: Some(error.into()),
                    ms: 1,
                }),
                Step::End(reason) => {
                    end = reason;
                    break;
                }
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
            session: Some("cs_1".into()),
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

/// Records model selection at the gateway boundary without launching an adapter.
#[derive(Clone)]
pub struct ModelScript {
    reply: Arc<Mutex<String>>,
    runs: Arc<Mutex<Vec<serde_json::Value>>>,
    pub absent: Option<String>,
}

impl ModelScript {
    pub fn new(reply: &str) -> Self {
        Self {
            reply: Arc::new(Mutex::new(reply.into())),
            runs: Default::default(),
            absent: None,
        }
    }

    pub fn set_reply(&self, reply: String) {
        *self.reply.lock().unwrap() = reply;
    }

    pub fn runs(&self) -> Vec<serde_json::Value> {
        std::mem::take(&mut *self.runs.lock().unwrap())
    }
}

#[async_trait::async_trait]
impl crate::OneModel for ModelScript {
    async fn once(
        &self,
        call: &crate::gateway::OneCall<'_>,
        model: &str,
        effort: Option<steve_types::ReasoningEffort>,
        on_text: Option<crate::gateway::OnText<'_>>,
    ) -> crate::gateway::Tried {
        use crate::gateway::Tried;
        if self.absent.as_deref() == Some(call.harness) {
            return Tried::failed(crate::error::msg(format!("{} unavailable", call.harness)));
        }
        self.runs.lock().unwrap().push(serde_json::json!({
            "harness": call.harness,
            "model": (!model.is_empty()).then_some(model),
            "effort": effort.map(|e| e.as_str()),
            "prompt": call.prompt,
            "images": call.images.iter().map(crate::gateway::Image::block).collect::<Vec<_>>(),
        }));
        if matches!(model, "broken" | "busy") {
            return Tried::failed(crate::error::msg("model unavailable"));
        }
        let text = self.reply.lock().unwrap().clone();
        if let Some(on_text) = on_text {
            on_text(&text);
        }
        Tried {
            reply: Ok((text, Some(0.000829045))),
            usage: vec![steve_store::ModelUsage {
                model: model.into(),
                tokens: steve_store::Tokens {
                    input: Some(2),
                    output: Some(4),
                    cache_read: Some(11_097),
                    cache_write: Some(5_727),
                    reasoning: None,
                },
                cost_reported: Some(0.000829045),
                detail: json!({"scope": "main"}),
            }],
            reported_model: (!model.is_empty()).then(|| model.to_string()),
        }
    }
}
