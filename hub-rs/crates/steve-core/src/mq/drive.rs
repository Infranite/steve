//! Driving the merge queue (S4): what an accepted item of a project finished by its completion rule
//! becomes, and what the queue's actions do to items. On while `STEVE_MQ=1` and the Hub's own Node
//! runs the staging ([`Core::set_mq_node`]); the merge lanes of 031 are converted at boot then.
//!
//! - an accepted result goes into its target's queue by itself: the acceptance is the consent to
//!   merge (no card per batch, no round of the agent's). "Accept, don't merge yet" keeps it out; the
//!   agent's `steve_publish` puts it in later;
//! - the first entry of a target asks you once whether Steve may merge into it (a migrated lane was
//!   allowed already); until you answer, and until the target's host rules are read as a direct
//!   push, its queue starts nothing;
//! - a target whose host takes changes only through a PR or its merge queue is not merged into
//!   here: its entries leave, and their items go on as without the queue (the host modes come next);
//! - a batch runs on the Node off the core's lock ([`super::exec::run_batch`]); what it came to is one
//!   event, in one transaction with the rows; landed items record it (a `land` operation, evidence
//!   only: the close still rests on the completion probe), returned ones go back to their agent with
//!   why. A plain rebase keeps the acceptance (the same own change): the rebased head goes in again;
//! - no result or a limit reached asks you with a card (YOLO declines these for now).
//!
//! Everything that may wait (git, the Node, the item's lock) runs on a thread of its own: callers
//! holding the core's lock only write rows and detach.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_store::mq::{self as rows, MqEntry, MqTarget, NewEntry};

use super::exec::{self, Member, MqNode, Staging};
use super::policy::Policy;
use super::queue::{Action as Qa, Answer, CardKind, Event as Qe, Outcome, Why};
use super::store::{self as rowstore, Loaded};
use crate::error::Result;
use crate::repo::{Item, NewDecision};
use crate::{Core, Tx, detach};

/// The item operation recording what the queue landed (evidence; the close waits on the probe).
pub const OP_LAND: &str = "land";
/// Decision subjects: a target's one-time permission, a batch's card.
pub const SUBJECT_TARGET: &str = "mq_target";
pub const SUBJECT_BATCH: &str = "mq_batch";
/// What `granted_by` says for a target you did not allow.
const REFUSED: &str = "refused";
/// How long a host's rules read for a target stay good.
const MODE_FRESH_MS: i64 = 10 * 60_000;

/// The batches run in this process now (a restart's recovery and a tick never start a second).
static RUNNING: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

struct Running((String, u64));

impl Running {
    fn take(target: &str, batch: u64) -> Option<Running> {
        let mut r = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let k = (target.to_string(), batch);
        if r.contains(&k) {
            return None;
        }
        r.push(k.clone());
        Some(Running(k))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k| *k != self.0);
    }
}

/// `<target>:<no>`, a batch card's subject.
pub fn batch_subject(target: &str, no: u64) -> String {
    format!("{target}:{no}")
}

pub fn parse_batch(s: &str) -> Option<(String, u64)> {
    let (t, n) = s.rsplit_once(':')?;
    Some((t.into(), n.parse().ok()?))
}

/// What the queue says on an item's timeline about why it was returned.
fn why_text(w: &Why) -> String {
    match w {
        Why::Left(s) => s.clone(),
        Why::Failed { tail, with } if with.is_empty() => {
            steve_i18n::t!("core.mq.why_failed", tail = tail)
        }
        Why::Failed { tail, with } => steve_i18n::t!(
            "core.mq.why_failed_with",
            tail = tail,
            with = with.join(steve_i18n::t!("core.lane.list_sep").as_str())
        ),
        Why::Returned => steve_i18n::t!("core.mq.why_returned"),
    }
}

/// The effective policy of a target: paused until you allowed it and its host takes a direct push.
fn effective(t: &MqTarget, p: &Policy) -> Policy {
    let mut p = p.clone();
    if t.granted_at.is_none() || t.mode.as_deref() != Some("push") {
        p.paused = true;
    }
    p
}

impl Core {
    /// The Node the queue's staging is on, once wired.
    pub fn set_mq_node(&self, n: Arc<dyn MqNode>) {
        let _ = self.0.mq_node.set(n);
    }

    pub(crate) fn mq_node(&self) -> Option<Arc<dyn MqNode>> {
        self.0.mq_node.get().cloned()
    }

    /// The merge queue merges (and the lanes do not): `STEVE_MQ=1` with its Node wired.
    pub fn mq_on(&self) -> bool {
        self.config().mq && self.mq_node().is_some()
    }

    /// Boot: the 031 lanes converted (once each), and the batches a restart cut short picked up.
    pub fn mq_boot(&self) {
        if !self.mq_on() {
            return;
        }
        match rowstore::convert_lanes(self.store().store()) {
            Ok(c) if c.targets > 0 => {
                tracing::info!(
                    targets = c.targets,
                    queued = c.queued,
                    landed = c.landed,
                    "merge lanes converted to the merge queue"
                )
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("merge queue: converting the lanes: {e}"),
        }
        self.mq_recover();
    }

    /// Time passed: every target's queue may start what waited (a debounce, a grant); batches that
    /// are running but not in this process are picked up.
    pub fn mq_tick(&self) {
        if !self.mq_on() {
            return;
        }
        let Ok(all) = self.store().store().reader(rows::targets_all) else {
            return;
        };
        for t in all {
            let at = crate::util::now();
            let id = t.id.clone();
            if let Err(e) = self.mq_event(&id, |l, p| l.queue.apply(p, Qe::Tick { at })) {
                tracing::warn!(target = id, "merge queue tick: {e}");
            }
        }
        self.mq_recover();
    }

    /// Batches recorded as running whose run is not going on in this process: run again under the
    /// same run, so each step's recorded result is taken, nothing done twice.
    pub fn mq_recover(&self) {
        let s = self.store().store();
        let Ok(all) = s.reader(rows::targets_all) else {
            return;
        };
        for t in all {
            let Ok(live) = s.reader(|c| rows::batches_live(c, &t.id)) else {
                continue;
            };
            for b in live.iter().filter(|b| b.state == "running") {
                let running = RUNNING
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&(t.id.clone(), b.no as u64));
                if !running {
                    let (c, tid, no) = (self.clone(), t.id.clone(), b.no as u64);
                    detach(move || c.mq_run(&tid, no));
                }
            }
        }
    }

    /// One event on `target`'s queue: its rows loaded, `f` applied, the rows written back with what
    /// the actions record, in one transaction; then the actions carried out.
    pub(crate) fn mq_event(
        &self,
        target: &str,
        f: impl FnOnce(&mut Loaded, &Policy) -> Vec<Qa>,
    ) -> Result<()> {
        let at = crate::util::now();
        let s = self.store().store();
        let done = s.transaction(|tx| {
            let Some(mut l) = rowstore::load(tx, target, None)? else {
                return Ok(None);
            };
            let p = effective(&l.target, &l.policy);
            let out = f(&mut l, &p);
            for a in &out {
                match a {
                    Qa::Land { members, commit, .. } => {
                        for m in members {
                            l.landed.push((m.clone(), commit.clone()));
                        }
                    }
                    Qa::Return { id, why } => {
                        l.reasons.push((id.clone(), steve_i18n::stored(|| why_text(why))))
                    }
                    _ => {}
                }
            }
            l.save(tx, at)?;
            for a in &out {
                let (kind, batch, data) = match a {
                    Qa::Start { batch, members, checks, .. } => (
                        "batch_started",
                        Some(*batch as i64),
                        json!({"members": members.len(), "checks": checks}),
                    ),
                    Qa::Land { batch, members, .. } => (
                        "batch_landed",
                        batch.map(|b| b as i64),
                        json!({"members": members.len()}),
                    ),
                    Qa::Return { why, .. } => (
                        "returned",
                        None,
                        json!({"why": match why { Why::Left(_) => "left", Why::Failed { .. } => "failed", Why::Returned => "returned" }}),
                    ),
                    Qa::Split { batch, .. } => ("split", Some(*batch as i64), json!({})),
                    Qa::Card { batch, kind } => (
                        "card",
                        Some(*batch as i64),
                        json!({"kind": match kind { CardKind::NoResult(_) => "no_result", CardKind::OverLimit => "over_limit" }}),
                    ),
                    Qa::Requeue { .. } => ("requeued", None, json!({})),
                };
                rows::stat_add(tx, target, kind, batch, None, &data)?;
            }
            Ok::<_, steve_store::StoreError>(Some((l.target.clone(), out)))
        })?;
        if let Some((t, out)) = done {
            self.mq_carry(&t, out);
        }
        Ok(())
    }

    /// What the actions do outside the rows: runs started, items told, cards opened.
    fn mq_carry(&self, t: &MqTarget, out: Vec<Qa>) {
        for a in out {
            match a {
                Qa::Start { batch, members, .. } => {
                    for m in &members {
                        self.mq_note(
                            m,
                            &steve_i18n::stored(|| {
                                steve_i18n::t!(
                                    "core.mq.merging",
                                    batch = batch,
                                    branch = t.branch.as_str()
                                )
                            }),
                        );
                    }
                    let (c, tid) = (self.clone(), t.id.clone());
                    detach(move || c.mq_run(&tid, batch));
                }
                Qa::Land {
                    batch,
                    members,
                    commit,
                } => {
                    for m in &members {
                        self.mq_landed(t, m, batch, commit.as_deref());
                    }
                }
                Qa::Return { id, why } => self.mq_returned(t, &id, &why),
                Qa::Requeue { id } => self.mq_note(
                    &id,
                    &steve_i18n::stored(|| steve_i18n::t!("core.mq.requeued")),
                ),
                Qa::Split { batch, halves } => {
                    if let Ok(Some(b)) = self
                        .store()
                        .store()
                        .reader(|c| rows::batch_get(c, &t.id, batch as i64))
                    {
                        let members: Vec<String> =
                            serde_json::from_str(&b.members).unwrap_or_default();
                        for m in &members {
                            self.mq_note(
                                m,
                                &steve_i18n::stored(|| {
                                    steve_i18n::t!(
                                        "core.mq.split",
                                        batch = batch,
                                        a = halves[0],
                                        b = halves[1]
                                    )
                                }),
                            );
                        }
                    }
                }
                Qa::Card { batch, kind } => self.mq_card(t, batch, &kind),
            }
        }
    }

    /// Run batch `no` of `target` on its Node (off the lock), then record what it came to.
    fn mq_run(&self, target: &str, no: u64) {
        let Some(_running) = Running::take(target, no) else {
            return;
        };
        let Some(node) = self.mq_node() else {
            return;
        };
        let s = self.store().store();
        let (Ok(Some(t)), Ok(Some(b))) = (
            s.reader(|c| rows::target_get(c, target)),
            s.reader(|c| rows::batch_get(c, target, no as i64)),
        ) else {
            return;
        };
        if b.state != "running" {
            return;
        }
        let ids: Vec<String> = serde_json::from_str(&b.members).unwrap_or_default();
        let entries: Vec<MqEntry> = ids
            .iter()
            .filter_map(|id| s.reader(|c| rows::entry_get(c, id)).ok().flatten())
            .collect();
        let project = entries
            .iter()
            .filter_map(|e| e.item_id.as_deref())
            .filter_map(|i| self.store().item(i).ok().flatten())
            .find_map(|i| i.project_id);
        let Some(project) = project else {
            let why = steve_i18n::stored(|| steve_i18n::t!("core.mq.no_project"));
            let _ = self.mq_event(target, |l, p| {
                l.queue.apply(
                    p,
                    Qe::Done {
                        batch: no,
                        outcome: Outcome::Unknown { why },
                    },
                )
            });
            return;
        };
        let members: Vec<Member> = entries
            .iter()
            .map(|e| Member {
                id: e.id.clone(),
                head: e.head.clone(),
                label: self.mq_label(e),
                verified_base: None,
            })
            .collect();
        let staging = Staging {
            target_id: t.id.clone(),
            project_id: project,
            upstream: t.upstream.clone(),
            branch: t.branch.clone(),
            keep: steve_toolchains::Toolchains::builtin()
                .mq_keep_names()
                .into_iter()
                .map(String::from)
                .collect(),
        };
        let policy = Policy::of(&t.policy, None);
        let skip = b
            .checks
            .as_deref()
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
            .is_some_and(|v| v["skip"] == true);
        let run = b.reruns as u32 + if skip { 1000 } else { 0 };
        let st = s.clone();
        let tid = t.id.clone();
        let record = move |r: exec::Record| {
            let _ = st.transaction(|tx| {
                rows::step_put(
                    tx,
                    &rows::MqStep {
                        operation_id: r.op.clone(),
                        target_id: tid.clone(),
                        batch_no: Some(no as i64),
                        train_no: None,
                        kind: r.kind.into(),
                        state: r.state.into(),
                        result: r.detail.clone(),
                        created: 0,
                        updated: 0,
                    },
                )
            });
        };
        let _ = s.transaction(|tx| {
            rows::batch_step(tx, target, no as i64, Some("integrating"), None, None)
        });
        let started = crate::util::now();
        let ran = crate::node_git::block_on(async {
            steve_i18n::scope(
                steve_i18n::ZH_CN,
                exec::run_batch(&*node, &staging, no, run, &members, &policy, !skip, &record),
            )
            .await
        });
        let Some(ran) = ran else {
            return;
        };
        let checks = json!({"skip": skip, "ran": ran.checks.iter().map(|(c, s)| json!({"cmd": c, "result": s})).collect::<Vec<_>>()});
        let outcome_text = match &ran.outcome {
            Some(Outcome::Landed { .. }) => "landed",
            Some(Outcome::Failed { .. }) => "failed",
            Some(Outcome::Unknown { .. }) => "unknown",
            None => "emptied",
        };
        let _ = s.transaction(|tx| {
            rows::batch_step(tx, target, no as i64, None, ran.base.as_deref(), ran.tip.as_deref())?;
            rows::batch_result(tx, target, no as i64, Some(&checks.to_string()), None, Some(outcome_text))?;
            rows::stat_add(
                tx,
                target,
                "batch_ran",
                Some(no as i64),
                None,
                &json!({"members": members.len(), "left": ran.left.len(), "moved": ran.moved,
                        "checks": ran.checks.len(), "outcome": outcome_text, "ms": crate::util::now() - started}),
            )
        });
        let left = ran.left.clone();
        let outcome = ran.outcome.clone();
        let r = self.mq_event(target, move |l, p| {
            let mut out = vec![];
            for (id, why) in left {
                out.extend(l.queue.apply(p, Qe::Left { batch: no, id, why }));
            }
            if let Some(o) = outcome {
                out.extend(l.queue.apply(
                    p,
                    Qe::Done {
                        batch: no,
                        outcome: o,
                    },
                ));
            }
            out
        });
        if let Err(e) = r {
            tracing::warn!(target, batch = no, "merge queue: recording the batch: {e}");
        }
    }

    /// How a merge commit names an entry: its item and branch.
    fn mq_label(&self, e: &MqEntry) -> String {
        match (&e.item_id, &e.branch) {
            (Some(i), Some(b)) => format!("{i} ({b})"),
            (Some(i), None) => i.clone(),
            _ => e.id.clone(),
        }
    }

    /// A line on the timeline of an entry's item.
    fn mq_note(&self, entry: &str, text: &str) {
        let item = self
            .store()
            .store()
            .reader(|c| rows::entry_get(c, entry))
            .ok()
            .flatten()
            .and_then(|e| e.item_id);
        if let Some(i) = item
            && let Err(e) = self.store().timeline_add(&i, "steve", "sys", text, None)
        {
            tracing::warn!(item = i, "merge queue note: {e}");
        }
    }

    /// An entry landed: its item records it and looks at its completion again.
    fn mq_landed(&self, t: &MqTarget, entry: &str, batch: Option<u64>, commit: Option<&str>) {
        let Ok(Some(e)) = self.store().store().reader(|c| rows::entry_get(c, entry)) else {
            return;
        };
        let Some(item) = e.item_id.clone() else {
            return;
        };
        let text = steve_i18n::stored(|| match batch {
            Some(b) => steve_i18n::t!("core.mq.landed", batch = b, branch = t.branch.as_str()),
            None => steve_i18n::t!("core.mq.present", branch = t.branch.as_str()),
        });
        let mut x = self.tx();
        let r = (|| -> Result<()> {
            let i = x.item(&item)?;
            x.mq_land_op(
                &i,
                "succeeded",
                json!({"target": format!("{}#{}", t.upstream, t.branch), "commit": commit, "batch": batch, "entry": entry}),
            )?;
            x.db().timeline_add(&item, "steve", "sys", &text, None)?;
            x.db()
                .item_patch(&item, crate::patch![now: text.as_str()])?;
            // merged: the completion is looked at afresh
            x.forget_probe(&item);
            x.lc_try_close(&item)?;
            Ok(())
        })();
        if let Err(err) = r {
            tracing::warn!(item, "merge queue: landed: {err}");
        }
    }

    /// An entry went back: its item's agent hears why and what to do.
    fn mq_returned(&self, _t: &MqTarget, entry: &str, why: &Why) {
        let Ok(Some(e)) = self.store().store().reader(|c| rows::entry_get(c, entry)) else {
            return;
        };
        let Some(item) = e.item_id.clone() else {
            return;
        };
        let text = steve_i18n::stored(|| why_text(why));
        let mut x = self.tx();
        let r = (|| -> Result<()> {
            let i = x.item(&item)?;
            x.mq_land_op(&i, "failed", json!({"entry": entry, "why": text}))?;
            let note =
                steve_i18n::stored(|| steve_i18n::t!("core.mq.returned", why = text.as_str()));
            x.db().timeline_add(&item, "steve", "sys", &note, None)?;
            x.db()
                .item_patch(&item, crate::patch![now: note.as_str()])?;
            let prompt = match why {
                // a conflict: a rebase is enough, and keeps the acceptance
                Why::Left(_) => format!(
                    "合并队列退回：{text}\n在最新的目标分支上 rebase 并解决冲突。只是 rebase（改动本身不变）的话，结束这一回合即可：Steve 会把新的提交重新放进合并队列，不用重新验收。改了内容就用 steve_deliver 重新交付。"
                ),
                _ => format!(
                    "合并队列退回：{text}\n请修好后用 steve_deliver 重新交付，通过验收后会再进合并队列。"
                ),
            };
            match x.active_session(&item)? {
                Some(s) => x.prompt(&s.id, &prompt, "steve"),
                None => x.fail(&item, &note)?,
            }
            Ok(())
        })();
        if let Err(err) = r {
            tracing::warn!(item, "merge queue: returned: {err}");
        }
    }

    /// A batch's card: its checks gave no result, or its bisection reached the limit.
    fn mq_card(&self, t: &MqTarget, batch: u64, kind: &CardKind) {
        let (question, draft, options) = steve_i18n::stored(|| match kind {
            CardKind::NoResult(why) => (
                steve_i18n::t!(
                    "core.mq.card_no_result",
                    batch = batch,
                    branch = t.branch.as_str()
                ),
                why.clone(),
                vec![
                    "rerun".to_string(),
                    "return".to_string(),
                    "skip".to_string(),
                ],
            ),
            CardKind::OverLimit => (
                steve_i18n::t!("core.mq.card_over_limit", batch = batch),
                String::new(),
                vec!["return".to_string(), "rerun".to_string()],
            ),
        });
        let ws = self.mq_workspace(t);
        if let Err(e) = self.store().decision_create(NewDecision {
            workspace_id: ws,
            kind: "fail".into(),
            subject_type: SUBJECT_BATCH.into(),
            subject_id: batch_subject(&t.id, batch),
            question,
            draft: Some(draft),
            options: Some(options),
            requested_by: "steve".into(),
            ..Default::default()
        }) {
            tracing::warn!(target = t.id, batch, "merge queue card: {e}");
        }
    }

    /// The workspace a target's cards go to: its latest entry's.
    fn mq_workspace(&self, t: &MqTarget) -> String {
        self.store()
            .store()
            .reader(|c| rows::entries_recent(c, &t.id, 1))
            .ok()
            .and_then(|v| v.into_iter().next())
            .and_then(|e| e.workspace_id)
            .unwrap_or_else(|| crate::repo::WS.into())
    }

    /// Your answer on a queue's card (`mq_target`: may Steve merge into it; `mq_batch`: what next for
    /// a batch). Off the lock.
    pub(crate) fn mq_answered(&self, subject_type: &str, subject: &str, choice: &str, actor: &str) {
        match subject_type {
            SUBJECT_TARGET => {
                let yes = choice == "yes";
                let s = self.store().store();
                let r = s.transaction(|tx| {
                    if yes {
                        rows::target_grant(tx, subject, actor)
                    } else {
                        tx.execute(
                            "UPDATE mq_targets SET granted_by = ?, granted_at = NULL, updated = ? WHERE id = ?",
                            rusqlite::params![REFUSED, crate::util::now(), subject],
                        )?;
                        Ok(())
                    }
                });
                if let Err(e) = r {
                    tracing::warn!(target = subject, "merge queue grant: {e}");
                    return;
                }
                self.mq_admit(subject);
            }
            SUBJECT_BATCH => {
                let Some((target, batch)) = parse_batch(subject) else {
                    return;
                };
                let answer = match choice {
                    "rerun" => Answer::Rerun,
                    "skip" => Answer::Skip,
                    _ => Answer::Return,
                };
                if answer == Answer::Skip {
                    let _ = self.store().store().transaction(|tx| {
                        rows::batch_result(
                            tx,
                            &target,
                            batch as i64,
                            Some(r#"{"skip":true}"#),
                            None,
                            None,
                        )
                    });
                }
                if let Err(e) = self.mq_event(&target, |l, p| {
                    l.queue.apply(p, Qe::Answer { batch, answer })
                }) {
                    tracing::warn!(target, batch, "merge queue answer: {e}");
                }
            }
            _ => {}
        }
    }

    /// Whether a target's queue may run: you allowed it and its host takes a direct push. Read off
    /// the lock (the host's rules may be a network call), then its queue goes on, or its entries
    /// leave the queue and their items go on as without it.
    pub(crate) fn mq_admit(&self, target: &str) {
        let s = self.store().store();
        let Ok(Some(t)) = s.reader(|c| rows::target_get(c, target)) else {
            return;
        };
        let refused = t.granted_by.as_deref() == Some(REFUSED);
        let mode = if refused || t.granted_at.is_none() {
            t.mode.clone()
        } else {
            self.mq_mode(&t)
        };
        if refused || (t.granted_at.is_some() && mode.as_deref() != Some("push")) {
            let why = steve_i18n::stored(|| {
                if refused {
                    steve_i18n::t!("core.mq.refused", branch = t.branch.as_str())
                } else {
                    steve_i18n::t!("core.mq.host_rules", branch = t.branch.as_str())
                }
            });
            self.mq_withdraw_all(&t, &why);
            return;
        }
        let at = crate::util::now();
        if let Err(e) = self.mq_event(target, |l, p| l.queue.apply(p, Qe::Tick { at })) {
            tracing::warn!(target, "merge queue admit: {e}");
        }
    }

    /// How changes get into `t` (cached a while): `push` when its host takes a direct push, else
    /// `host_pr`, `host_queue`, or `None` when its rules could not be read.
    fn mq_mode(&self, t: &MqTarget) -> Option<String> {
        let fresh = t
            .mode_evidence
            .as_deref()
            .and_then(|e| serde_json::from_str::<Value>(e).ok())
            .and_then(|v| v["at"].as_i64())
            .is_some_and(|at| crate::util::now() - at < MODE_FRESH_MS);
        if fresh && t.mode.is_some() {
            return t.mode.clone();
        }
        use crate::lane::profile::{How, Merge};
        let target = crate::lane::target::Target {
            upstream: t.upstream.clone(),
            branch: t.branch.clone(),
        };
        let m = crate::lane::profile::merge_of(&target, &self.config().gh);
        let mode = match (m.how, m.value) {
            (How::Found, Merge::Push) => Some("push"),
            (How::Found, Merge::HostPr) => Some("host_pr"),
            (How::Found, Merge::HostQueue) => Some("host_queue"),
            _ => None,
        };
        let ev = json!({"at": crate::util::now(), "evidence": m.evidence});
        let _ = self
            .store()
            .store()
            .transaction(|tx| rows::target_mode_set(tx, &t.id, mode, &ev.to_string()));
        mode.map(String::from)
    }

    /// Every queued entry of `t` leaves the queue; their items go on without it (`why` on their
    /// timeline; the agent is told the project's own way).
    fn mq_withdraw_all(&self, t: &MqTarget, why: &str) {
        let s = self.store().store();
        let queued: Vec<MqEntry> = s
            .reader(|c| rows::entries_live(c, &t.id))
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.state == "queued")
            .collect();
        let ids: Vec<String> = queued.iter().map(|e| e.id.clone()).collect();
        let _ = self.mq_event(&t.id, |l, p| {
            let mut out = vec![];
            for id in ids {
                out.extend(l.queue.apply(p, Qe::Withdraw { id }));
            }
            out
        });
        for e in queued {
            let Some(item) = e.item_id else { continue };
            let mut x = self.tx();
            let r = (|| -> Result<()> {
                let i = x.item(&item)?;
                x.mq_land_op(&i, "waived", json!({"why": why}))?;
                x.db().timeline_add(&item, "steve", "sys", why, None)?;
                // the item goes on as without the queue: the completion rule's own instruction
                x.mq_without(&i)?;
                Ok(())
            })();
            if let Err(err) = r {
                tracing::warn!(item, "merge queue: withdrawn: {err}");
            }
        }
    }
}

impl Tx<'_> {
    /// The `land` operation of the item's current cycle and revision, set to `status`.
    pub(crate) fn mq_land_op(&mut self, i: &Item, status: &str, evidence: Value) -> Result<()> {
        let key = format!("{OP_LAND}:{}:{}", i.cycle, i.revision);
        let (op, _) = self.lc_op_add(
            i,
            OP_LAND,
            Some(&key),
            false,
            status,
            "合并队列合入",
            evidence.clone(),
        )?;
        if op.status != status {
            self.db()
                .store()
                .transaction(|tx| steve_store::operation_set_in(tx, &op.id, status, &evidence))?;
        }
        Ok(())
    }

    /// Whether the queue takes this item's merge: the queue is on, its project is finished by its
    /// completion rule the lane way, and the acceptance did not say "don't merge yet".
    pub(crate) fn mq_applies(&self, i: &Item) -> Result<bool> {
        if !self.c.mq_on() {
            return Ok(false);
        }
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(false);
        };
        if !crate::orchestrator::lane_releases(&p) {
            return Ok(false);
        }
        let held = crate::cycle::closing_of(i)
            .and_then(|c| c.decision)
            .and_then(|d| self.db().decision(&d).ok().flatten())
            .is_some_and(|d| d.result.get("merge").and_then(Value::as_str) == Some("held"));
        Ok(!held)
    }

    /// The item's head now (its worktree's, or as its Node said last).
    fn mq_head(&self, i: &Item) -> Option<String> {
        if self.git_on_node(&i.id) {
            return self.git_seen(&i.id).map(|s| s.head.clone());
        }
        let cwd = i.cwd.as_deref()?;
        crate::git::git_read(Path::new(cwd), &["rev-parse", "HEAD"]).ok()
    }

    /// The accepted item goes into its target's queue, once per head: one already in (queued,
    /// batched, landed) stays; one that went back comes in again with a new head (a plain rebase
    /// keeps the acceptance: the gate saw the same own change). `Ok(false)`: the queue does not
    /// take it (no target, not allowed, the host's rules): the caller goes on without the queue.
    pub(crate) fn mq_requirement(&mut self, i: &Item) -> Result<bool> {
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(false);
        };
        let s = self.db().store().clone();
        if let Some(e) = s.reader(|c| rows::entry_live_of_item(c, &i.id))? {
            return Ok(e.state != "withdrawn");
        }
        let last = s.reader(|c| rows::entry_last_of_item(c, &i.id))?;
        let Some(head) = self.mq_head(i) else {
            return Ok(false);
        };
        match &last {
            // withdrawn (not allowed, the host's rules): not again by itself
            Some(e) if e.state == "withdrawn" && e.cycle == Some(i.cycle) => return Ok(false),
            // returned and nothing new since: it waits for its agent
            Some(e) if e.state == "returned" && e.head == head && e.cycle == Some(i.cycle) => {
                return Ok(true);
            }
            _ => {}
        }
        let target = match crate::lane::target::target_of(Path::new(&p.path), None) {
            Ok(t) => t,
            Err(e) => {
                tracing::info!(item = i.id, "merge queue: no target: {e}");
                return Ok(false);
            }
        };
        let t = s.transaction(|tx| rows::target_for(tx, &target.upstream, &target.branch))?;
        if t.granted_by.as_deref() == Some(REFUSED) {
            return Ok(false);
        }
        let consent = crate::cycle::closing_of(i).and_then(|c| c.decision);
        let e = s.transaction(|tx| {
            rows::entry_add(
                tx,
                &NewEntry {
                    target_id: &t.id,
                    workspace_id: Some(&i.workspace_id),
                    item_id: Some(&i.id),
                    cycle: Some(i.cycle),
                    key: None,
                    after: &[],
                    head: &head,
                    branch: i.branch.as_deref(),
                    consent: consent.as_deref(),
                    node_id: None,
                },
            )
        })?;
        self.mq_land_op(i, "pending", json!({"entry": e.id, "head": head}))?;
        let again = last.is_some_and(|l| l.state == "returned" && l.cycle == Some(i.cycle));
        if again {
            // the same change on a new base: frozen on it, so what lands is what was frozen
            self.lc_refreeze(&i.id)?;
        }
        let note = steve_i18n::stored(|| {
            if again {
                steve_i18n::t!("core.mq.again", branch = target.branch.as_str())
            } else {
                steve_i18n::t!(
                    "core.mq.queued",
                    upstream = target.upstream.as_str(),
                    branch = target.branch.as_str()
                )
            }
        });
        self.db().timeline_add(&i.id, "steve", "sys", &note, None)?;
        if t.granted_at.is_none() {
            self.mq_grant_card(&t)?;
        }
        let (c, tid) = (self.c.clone(), t.id.clone());
        detach(move || c.mq_admit(&tid));
        Ok(true)
    }

    /// The one card asking whether Steve may merge into a target (none while one is open).
    fn mq_grant_card(&mut self, t: &MqTarget) -> Result<()> {
        let open: bool = self.db().store().reader(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM decisions WHERE subject_type = ? AND subject_id = ? AND status = 'open')",
                [SUBJECT_TARGET, t.id.as_str()],
                |r| r.get(0),
            )
            .map_err(steve_store::StoreError::from)
        })?;
        if open {
            return Ok(());
        }
        let ws = self.c.mq_workspace(t);
        self.db().decision_create(NewDecision {
            workspace_id: ws,
            kind: "approve".into(),
            subject_type: SUBJECT_TARGET.into(),
            subject_id: t.id.clone(),
            question: steve_i18n::stored(|| {
                steve_i18n::t!(
                    "core.mq.grant",
                    upstream = t.upstream.as_str(),
                    branch = t.branch.as_str()
                )
            }),
            draft: Some(steve_i18n::stored(|| steve_i18n::t!("core.mq.grant_draft"))),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        Ok(())
    }

    /// The item goes on as without the queue: its agent hears the project's own way to finish.
    pub(crate) fn mq_without(&mut self, i: &Item) -> Result<()> {
        let to = match self.active_session(&i.id)? {
            Some(s) => crate::orchestrator::To::Session(s.id),
            None => return Ok(()),
        };
        self.send(
            &to,
            "合并队列没有合入这一项（见时间线）。按这个项目的约定完成合并：推送、开 PR、合并、打 tag 是对外动作，先用 steve_request_approval 请求批准。做完后结束这一回合即可，Steve 会自己核对远端。",
        )
    }

    /// What `steve_publish` says for an item in the queue, when it is.
    pub(crate) fn mq_publish(&mut self, i: &Item) -> Result<Option<String>> {
        if !self.mq_applies(i)?
            && !(self.c.mq_on()
                && self
                    .db()
                    .project(i.project_id.as_deref())?
                    .is_some_and(|p| crate::orchestrator::lane_releases(&p)))
        {
            return Ok(None);
        }
        let accepted = crate::cycle::closing_of(i).is_some_and(|c| {
            i.phase == crate::cycle::CLOSING && c.kind == crate::cycle::COMPLETE && c.accepted
        });
        if !accepted {
            return Ok(Some(
                "这个项目由合并队列合入：交付并验收通过后，Steve 会自动把它放进合并队列，不用调用 steve_publish。结束这一回合即可。".into(),
            ));
        }
        // "accept, don't merge yet" is lifted by an explicit publish
        if let Some(d) = crate::cycle::closing_of(i).and_then(|c| c.decision) {
            self.db()
                .store()
                .decisions()
                .merge_result(&d, &json!({"merge": "asked"}))?;
        }
        if self.mq_requirement(i)? {
            return Ok(Some(
                "已在合并队列里：Steve 会和同一目标的其他改动一起合入并按项目约定发布，完成后事项自动结束。结束这一回合即可；想知道进展用 steve_lane_status。".into(),
            ));
        }
        Ok(None)
    }
}
