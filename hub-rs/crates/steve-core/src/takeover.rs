//! Steve looks first (ST-31): an agent that stopped without delivering or asking, or whose delivery went
//! void after it was frozen, is not handed to you straight away.
//!
//! 1. The agent's last words are a question → the card, as before (YOLO may answer it).
//! 2. An active persisted merge-lane request owns the wait → no automatic nudge.
//! 3. The project has a completion rule (`Release::done_when`: merged, or merged and tagged) and the item
//!    was delivered in this cycle → Steve checks the remote (off the lock, [`upstream::probe`]); when the
//!    rule holds, the item completes on that evidence, nobody asked.
//! 4. Otherwise Steve continues the agent with what it found — at most `takeover_continues` times in a
//!    row; anything you do on the item starts the count again. A continue never runs twice for one stop:
//!    a turn already running or a message already queued is the continue.
//! 5. Out of continues, or nothing to continue (no session) → one card for you, saying so. A card that
//!    is already open for the same thing is not opened again.
//!
//! A required release of a project with a completion rule is the rule itself: met when the remote shows
//! it, re-checked while the item waits to close. The build the tag starts is followed on its own
//! ([`OP_BUILD`]): it holds the close only when the item's goal asks for the build to pass
//! ([`crate::proof`]); otherwise only a failed build is worth telling you about.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cycle::{ACTIVE, CLOSING, OP_BUILD, OP_RELEASE, Policy};
use crate::error::Result;
use crate::lifecycle::settle;
use crate::orchestrator::To;
use crate::repo::{Item, ItemExt};
use crate::thread::flowback::{Update, flow_quiet};
use crate::upstream::{DONE_MERGE, DONE_TAG, Probe};
use crate::util::{js_head, now};
use crate::{Core, Tx, detach, patch};

/// Timeline lines of Steve's own continues start with this (they are counted).
pub const LINE: &str = "Steve 接手：";
/// In a card's question: Steve already continued the agent; YOLO does not continue it again.
pub const STEVE_TRIED: &str = "Steve 已让它继续";
/// A probe of the same `HEAD` younger than this decides without asking the remote again.
pub const PROBE_FRESH_MS: i64 = 2 * 60_000;
/// A probe that has not come back after this is taken for lost (its deadlines are far shorter).
const PROBE_STUCK_MS: i64 = 5 * 60_000;
/// How often a running build is looked at.
const BUILD_EVERY_MS: i64 = 60_000;
/// A build not finished (or not found) after this is no longer followed.
const BUILD_GIVE_UP_MS: i64 = 3 * 3_600_000;
const GH_DEADLINE: Duration = Duration::from_secs(30);

/// How much of the agent's last words is read for a report it stopped to give.
pub(crate) const WHOLE_CHARS: usize = 6000;

/// How the agent stopped.
#[derive(Debug, Clone)]
pub(crate) enum Stop {
    /// A turn ended without a delivery, a question or a pending card: `why` is the card's text,
    /// `last` the agent's last words (their tail), `whole` what it said in the turn that ended (up to
    /// [`WHOLE_CHARS`]), `answered_you` the turn answered what you asked it from the conversation.
    Stopped {
        why: String,
        last: String,
        whole: String,
        answered_you: bool,
    },
    /// The session sat idle with nothing going on (`crate::stall`).
    Idle { why: String },
}

impl Stop {
    fn why(&self) -> &str {
        match self {
            Stop::Stopped { why, .. } | Stop::Idle { why } => why,
        }
    }
}

/// Why a probe was started: what to do with its result.
#[derive(Debug, Clone)]
pub(crate) enum Probing {
    /// The agent stopped (in session `session`).
    Stall { session: String, stop: Stop },
    /// The item waits to close on its completion rule.
    Requirement,
}

/// A look at the remote under way.
#[derive(Debug, Clone)]
pub(crate) struct Looking {
    /// When it started.
    since: i64,
    /// Which look it is (`State::probe_seq`).
    seq: u64,
    /// What it looks at may have moved since it started ([`Tx::forget_probe`]): its answer is not kept.
    stale: bool,
}

/// A probe's result, kept for the `HEAD` it looked at.
#[derive(Debug, Clone)]
pub(crate) struct Probed {
    at: i64,
    head: String,
    probe: Probe,
}

/// The rule in words, for the agent and the timeline.
pub fn rule_text(rule: &str) -> &'static str {
    if rule == DONE_TAG {
        "PR 合并到默认分支、并推送包含它的 tag"
    } else {
        "PR 合并到默认分支"
    }
}

fn head_of(cwd: &str) -> Option<String> {
    crate::git::git_read(Path::new(cwd), &["rev-parse", "HEAD"]).ok()
}

impl Tx<'_> {
    /// The project's completion rule for this item, when it has one and works in a git worktree.
    pub(crate) fn done_rule(&self, i: &Item) -> Result<Option<String>> {
        /* a worktree on a Node is looked at there, never on the Hub's disk */
        if i.branch.as_deref().is_none_or(str::is_empty)
            || (!self.git_on_node(&i.id) && i.cwd.as_deref().is_none_or(|c| !Path::new(c).is_dir()))
        {
            return Ok(None);
        }
        Ok(self
            .db()
            .project(i.project_id.as_deref())?
            .and_then(|p| p.release)
            .and_then(|r| r.done_when)
            .filter(|r| r == DONE_MERGE || r == DONE_TAG))
    }

    /// An agent stopped: Steve looks first. `Some(why)` when it is for you after all — the caller opens
    /// the card; `None` when Steve took it (continued, or is checking).
    pub(crate) fn takeover(
        &mut self,
        item_id: &str,
        sid: &str,
        stop: Stop,
    ) -> Result<Option<String>> {
        let i = self.item(item_id)?;
        /* it stopped on purpose — a question, an answer to what you asked it from the
         * conversation, a plan or something for you to confirm — so it is yours, not continued */
        if let Stop::Stopped {
            last,
            whole,
            answered_you,
            ..
        } = &stop
            && (crate::yolo::answer::looks_like_question(last).is_some()
                || *answered_you
                || stopped_to_report(whole))
        {
            return Ok(Some(stop.why().into()));
        }
        if self.waiting_on_lane(item_id)? {
            return Ok(None);
        }
        if i.revision > 0
            && let Some(rule) = self.done_rule(&i)?
        {
            let why = Probing::Stall {
                session: sid.into(),
                stop: stop.clone(),
            };
            if self.probe_async(&i, &rule, why) {
                self.db().item_patch(
                    item_id,
                    patch![now: "Agent 停下了，Steve 在核对它是不是已经做完"],
                )?;
                return Ok(None);
            }
        }
        let why = stop.why().to_string();
        self.takeover_continue(&i, sid, &why, &continue_text(&stop, None, None, false, &i))
    }

    /// The frozen result moved and the closing went void: the agent hears why. Waiting for an answer
    /// of yours (or still at work), it hears it with the next message; idle, Steve continues it now.
    pub(crate) fn takeover_invalidated(&mut self, item_id: &str, why: &str) -> Result<()> {
        let i = self.item(item_id)?;
        let text = format!(
            "（Steve）你上一次的交付已经作废：{why}。确认这些改动该算进结果后，重新用 steve_deliver 交付；已经完成的推送、合并、打 tag、发布不要重做。"
        );
        let Some(s) = self.active_session(item_id)? else {
            return self.fail(item_id, why);
        };
        let waiting = self
            .db()
            .decisions_of_item(item_id)?
            .iter()
            .any(|d| d.status == "open" && matches!(d.kind.as_str(), "approve" | "ask"));
        if waiting
            || self.waiting_on_lane(item_id)?
            || self.busy(&s.id)
            || !self.db().queued(&s.id)?.is_empty()
        {
            self.c.steve_note(&s.id, &text);
            return Ok(());
        }
        if let Some(why) = self.takeover_continue(&i, &s.id, why, &text)? {
            self.fail(item_id, &why)?;
        }
        Ok(())
    }

    /// A persisted lane request explains an idle agent, including after a restart.
    /// The lane owns approval/release failure cards. Once it releases or returns the request,
    /// normal completion checks resume. This never suppresses explicit user messages.
    fn waiting_on_lane(&self, item_id: &str) -> Result<bool> {
        Ok(self
            .db()
            .store()
            .reader(|c| steve_store::lanes::request_of_item(c, item_id))?
            .is_some_and(|r| {
                matches!(
                    r.state.as_str(),
                    "queued" | "collecting" | "merging" | "merged"
                )
            }))
    }

    /// Steve's continues since you last did something on the item.
    fn takeover_count(&self, item_id: &str) -> Result<u32> {
        /* you answered a card of the item (not YOLO for you) */
        let answered = self
            .db()
            .decisions_of_item(item_id)?
            .iter()
            .filter(|d| d.status == "resolved" && d.result["decided_by"] != "yolo")
            .filter_map(|d| d.resolved)
            .max()
            .unwrap_or(0);
        let t = self.db().timeline(item_id)?;
        Ok(t.iter()
            .rev()
            .take_while(|e| e.kind != "me")
            .filter(|e| e.ts > answered && e.actor == "steve" && e.text.starts_with(LINE))
            .count() as u32)
    }

    /// Continues the agent with `text`, within the limit. `Some(why)` for the card when it can't.
    fn takeover_continue(
        &mut self,
        i: &Item,
        sid: &str,
        why: &str,
        text: &str,
    ) -> Result<Option<String>> {
        let Some(s) = self.db().session(sid)?.filter(|s| s.status != "ended") else {
            self.diagnostic_item(
                i,
                "takeover.evaluated",
                "session_unavailable",
                true,
                Default::default(),
            );
            return Ok(Some(why.into()));
        };
        /* the stop was answered already: a turn runs or a message waits for the session */
        if self.busy(&s.id) || !self.db().queued(&s.id)?.is_empty() {
            self.diagnostic_session(
                &s.id,
                None,
                "takeover.evaluated",
                "work_pending",
                true,
                Default::default(),
            );
            return Ok(None);
        }
        let n = self.takeover_count(&i.id)?;
        let cap = self.cfg().takeover_continues;
        if n >= cap {
            self.diagnostic_session(
                &s.id,
                None,
                "takeover.evaluated",
                "budget_exhausted",
                false,
                steve_store::diagnostics::Details {
                    attempt_ordinal: Some(u64::from(n)),
                    limit: Some(u64::from(cap)),
                    ..Default::default()
                },
            );
            return Ok(Some(if n == 0 {
                why.into()
            } else {
                format!("{why}（{STEVE_TRIED} {n} 次，仍然没有交付）")
            }));
        }
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &format!(
                "{LINE}{}，让它继续（第 {}/{cap} 次）",
                js_head(why, 120),
                n + 1
            ),
            Some(&s.id),
        )?;
        self.db().item_patch(&i.id, patch![now: "Steve 让它继续"])?;
        settle(self.db(), &i.id)?;
        self.prompt(&s.id, text, "steve");
        self.diagnostic_session(
            &s.id,
            None,
            "takeover.continue_requested",
            "no_delivery_or_question",
            false,
            steve_store::diagnostics::Details {
                attempt_ordinal: Some(u64::from(n + 1)),
                limit: Some(u64::from(cap)),
                ..Default::default()
            },
        );
        Ok(None)
    }

    /// The rule holds for an item the agent left without delivering: it completes on that evidence.
    fn takeover_close(&mut self, i: &Item, rule: &str, p: &Probe) -> Result<()> {
        let what = p.summary();
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &format!("Steve 核对：{what}；按约定（{}）判定完成", rule_text(rule)),
            None,
        )?;
        self.lc_request_complete(
            &i.id,
            "steve",
            Policy::Automatic,
            true,
            &format!("按约定完成：{what}"),
            None,
            vec![format!("Steve 核对远端：{what}")],
        )?;
        Ok(())
    }

    /// Starts a probe of the item's worktree against `rule` on a thread of its own (one per item at a
    /// time); the result comes back through [`Tx::probed`].
    /// `false` when it can't look (no worktree, no `HEAD`); `true` also when a look already runs.
    pub(crate) fn probe_async(&mut self, i: &Item, rule: &str, why: Probing) -> bool {
        let node = match self.c.node_git() {
            Some(g) if g.on_node(&i.id) => Some(g),
            _ => None,
        };
        /* on a Node its `HEAD` is read there with the rest, off the lock */
        let (cwd, head) = match &node {
            Some(_) => (String::new(), String::new()),
            None => {
                let Some(cwd) = i.cwd.clone() else {
                    return false;
                };
                let Some(head) = head_of(&cwd) else {
                    return false;
                };
                (cwd, head)
            }
        };
        /* one look at a time: the stop or the wait it was for is still the same one. A look begun
         * before what it looks at moved is not that one: a fresh look starts when it comes back */
        let t = now();
        if self
            .st
            .probing
            .get(&i.id)
            .is_some_and(|l| t - l.since < PROBE_STUCK_MS)
        {
            return true;
        }
        self.st.probe_seq += 1;
        let seq = self.st.probe_seq;
        self.st.probing.insert(
            i.id.clone(),
            Looking {
                since: t,
                seq,
                stale: false,
            },
        );
        let (core, item, rule) = (self.c.clone(), i.id.clone(), rule.to_string());
        if let Some(g) = node {
            detach(move || {
                let (head, p) = crate::node_git::probe_now(&*g, &item, &rule);
                if let Err(e) =
                    core.tx()
                        .probed(&item, seq, &rule, head.unwrap_or_default(), p, why)
                {
                    tracing::warn!(item, "completion probe: {e}");
                }
            });
            return true;
        }
        detach(move || {
            let p = crate::upstream::probe(Path::new(&cwd), &rule);
            if let Err(e) = core.tx().probed(&item, seq, &rule, head, p, why) {
                tracing::warn!(item, "completion probe: {e}");
            }
        });
        true
    }

    /// What the item's remote looked like may have changed (a turn ended: it may have merged or
    /// tagged): the next check looks afresh, and a look already under way, which may have read the
    /// remote before that, is not kept when it comes back.
    pub(crate) fn forget_probe(&mut self, item_id: &str) {
        self.st.probed.remove(item_id);
        if let Some(l) = self.st.probing.get_mut(item_id) {
            l.stale = true;
        }
    }

    /// A probe came back (`seq`: which look it was).
    pub(crate) fn probed(
        &mut self,
        item_id: &str,
        seq: u64,
        rule: &str,
        head: String,
        p: Probe,
        why: Probing,
    ) -> Result<()> {
        /* a look overtaken by a newer one (begun after this one seemed lost) leaves the answer to it;
         * one that was forgotten while it ran is not kept */
        let (overtaken, keep) = match self.st.probing.get(item_id) {
            Some(l) if l.seq == seq => {
                let keep = !l.stale;
                self.st.probing.remove(item_id);
                (false, keep)
            }
            _ => (true, false),
        };
        if keep {
            self.st.probed.insert(
                item_id.into(),
                Probed {
                    at: now(),
                    head,
                    probe: p.clone(),
                },
            );
        }
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        match why {
            Probing::Requirement => {
                /* an answer not kept decides nothing: the closing is checked again, which looks afresh */
                if i.phase == CLOSING {
                    self.lc_try_close(item_id)?;
                }
            }
            Probing::Stall { session, stop } => {
                /* something happened while Steve looked: it is not the stop that was looked at any more */
                if overtaken
                    || i.phase != ACTIVE
                    || i.is_settled()
                    || i.status == "block"
                    || self.dependency_waiting(item_id)?
                    || self.waiting_on_lane(item_id)?
                    || self.busy(&session)
                    || !self.db().queued(&session)?.is_empty()
                    || self
                        .db()
                        .decisions_of_item(item_id)?
                        .iter()
                        .any(|d| d.status == "open")
                {
                    return Ok(());
                }
                /* the goal asks for an actual operation, or for a build Steve can't follow: the remote
                 * alone is not that evidence (`proof`) */
                let more = self.proof_continue(&i, rule, &p)?;
                if p.met && more.is_none() {
                    return self.takeover_close(&i, rule, &p);
                }
                let build = crate::proof::build_followed(rule, &p) && self.needs_of(&i)?.build;
                let mut text = continue_text(&stop, Some(rule), Some(&p), build, &i);
                if p.met
                    && let Some(more) = more
                {
                    text.push_str(&more);
                }
                if let Some(why) = self.takeover_continue(&i, &session, stop.why(), &text)? {
                    let why = format!("{why}（Steve 核对：{}）", p.summary());
                    self.fail(item_id, &why)?;
                }
            }
        }
        Ok(())
    }

    /// The probe of this item's current `HEAD`, when it is fresh.
    fn fresh_probe(&self, i: &Item) -> Option<Probe> {
        if self.git_on_node(&i.id) {
            /* the `HEAD` it looked at, unless its Node said another one since (a look that could
             * read none stands for its time only) */
            let seen = self.git_seen(&i.id).map(|s| s.head.clone());
            return self
                .st
                .probed
                .get(&i.id)
                .filter(|p| p.head.is_empty() || seen.is_none_or(|h| h == p.head))
                .filter(|p| now() - p.at < PROBE_FRESH_MS)
                .map(|p| p.probe.clone());
        }
        let head = head_of(i.cwd.as_deref()?)?;
        self.st
            .probed
            .get(&i.id)
            .filter(|p| p.head == head && now() - p.at < PROBE_FRESH_MS)
            .map(|p| p.probe.clone())
    }

    /// The release adapter for a project with a completion rule: the required release is the rule.
    /// Called again before each look at the gate; idempotent.
    pub(crate) fn contract_requirement(&mut self, i: &Item, rule: &str) -> Result<()> {
        let key = format!("{}:{}:{}", OP_RELEASE, i.cycle, i.revision);
        let (op, _) = self.lc_op_add(
            i,
            OP_RELEASE,
            Some(&key),
            true,
            "pending",
            rule_text(rule),
            json!({"rule": rule}),
        )?;
        if op.status != "pending" {
            return Ok(());
        }
        let Some(p) = self.fresh_probe(i) else {
            /* a look starts (or already runs); it comes back to the gate */
            let _ = self.probe_async(i, rule, Probing::Requirement);
            return Ok(());
        };
        if p.met {
            let what = p.summary();
            let ev = json!({
                "rule": rule, "note": what, "branch": p.branch, "landed": p.landed,
                "tag": p.tag.as_ref().map(|t| &t.0), "commit": p.tag.as_ref().map(|t| &t.1),
            });
            self.db()
                .store()
                .transaction(|tx| steve_store::operation_set_in(tx, &op.id, "succeeded", &ev))?;
            self.db().timeline_add(
                &i.id,
                "steve",
                "sys",
                &format!("Steve 核对：{what}；按约定（{}）算完成", rule_text(rule)),
                None,
            )?;
            let required = self.build_required(i)?;
            self.track_build(i, &p, required)?;
            return Ok(());
        }
        /* not there yet: the agent hears once what is missing; Steve keeps looking while it waits */
        if op.evidence.get("asked").is_none() {
            self.db().store().transaction(|tx| {
                steve_store::operation_set_in(tx, &op.id, "pending", &json!({"asked": true}))
            })?;
            self.db().item_patch(
                &i.id,
                patch![stage: "收尾", now: format!("等{}", rule_text(rule))],
            )?;
            let to = match self.active_session(&i.id)? {
                Some(s) => To::Session(s.id),
                None => To::Decision {
                    live: None,
                    item: Some(i.id.clone()),
                },
            };
            let build =
                build_text(crate::proof::build_followed(rule, &p) && self.build_required(i)?);
            let lane = self
                .db()
                .project(i.project_id.as_deref())?
                .is_some_and(|p| crate::orchestrator::lane_releases(&p));
            let how = if lane {
                /* the lane merges, checks and tags: the agent only hands the branch over */
                crate::orchestrator::publish_text(true, i.branch.as_deref().unwrap_or_default())
            } else {
                "推送、开 PR、合并、打 tag 是对外动作，先用 steve_request_approval 请求批准；"
                    .into()
            };
            self.send(
                &to,
                &format!(
                    "验收通过。按这个项目的约定，{}后事项才算完成；现在还差：{}。{how}{build}。做完后结束这一回合即可，Steve 会自己核对远端。",
                    rule_text(rule),
                    p.summary()
                ),
            )?;
        }
        Ok(())
    }

    /// The build the completion tag started is followed from now on: once per tag, or, `required` by
    /// the item's goal (`proof`), once per result that has to show it.
    fn track_build(&mut self, i: &Item, p: &Probe, required: bool) -> Result<()> {
        let Some((tag, commit)) = &p.tag else {
            return Ok(());
        };
        let Some(slug) = &p.slug else {
            /* not on GitHub: nothing to follow; the delivery's build check is the goal's evidence */
            if required {
                self.db().timeline_add(
                    &i.id,
                    "steve",
                    "sys",
                    &format!("远端不在 GitHub 上，Steve 跟踪不了 {tag} 的 Release 构建；事项目标要求的构建通过以交付里的构建检查为准"),
                    None,
                )?;
            }
            return Ok(());
        };
        let key = if required {
            format!("{OP_BUILD}:{tag}:{}:{}", i.cycle, i.revision)
        } else {
            format!("{OP_BUILD}:{tag}")
        };
        let (_, new) = self.lc_op_add(
            i,
            OP_BUILD,
            Some(&key),
            required,
            "running",
            &format!("Release 构建（{tag}）"),
            json!({"tag": tag, "commit": commit, "slug": slug, "since": now()}),
        )?;
        if new {
            let line = if required {
                format!("开始跟踪 {tag} 的 Release 构建；事项目标要求构建通过，构建成功后才算完成")
            } else {
                format!("开始单独跟踪 {tag} 的 Release 构建；它的结果不影响事项完成")
            };
            self.db().timeline_add(&i.id, "steve", "sys", &line, None)?;
        }
        Ok(())
    }

    /// The builds due for a look: `(op id, item, slug, tag, commit, since)`.
    fn builds_due(&mut self) -> Result<Vec<Due>> {
        let t = now();
        let mut out = vec![];
        for o in self
            .db()
            .store()
            .items()
            .operations_with(OP_BUILD, "running")?
        {
            if self
                .st
                .build_checked
                .get(&o.id)
                .is_some_and(|at| t - at < BUILD_EVERY_MS)
            {
                continue;
            }
            self.st.build_checked.insert(o.id.clone(), t);
            let s = |k: &str| o.evidence[k].as_str().unwrap_or("").to_string();
            out.push((
                o.id.clone(),
                o.item_id.clone(),
                s("slug"),
                s("tag"),
                s("commit"),
                o.evidence["since"].as_i64().unwrap_or(o.created),
            ));
        }
        Ok(out)
    }

    /// What a look at a build found.
    fn build_seen(&mut self, op: &str, item: &str, tag: &str, since: i64, b: Build) -> Result<()> {
        let (status, ev, line, tell) = match b {
            Build::Running => {
                if now() - since < BUILD_GIVE_UP_MS {
                    return Ok(());
                }
                (
                    "waived",
                    json!({"note": "超过 3 小时还没结束，不再跟踪"}),
                    format!("{tag} 的 Release 构建超过 3 小时还没结束，Steve 不再跟踪"),
                    false,
                )
            }
            Build::Unknown(e) => {
                if now() - since < BUILD_GIVE_UP_MS {
                    return Ok(());
                }
                (
                    "waived",
                    json!({"note": e}),
                    format!("查不到 {tag} 的 Release 构建（{e}），Steve 不再跟踪"),
                    false,
                )
            }
            Build::Succeeded(url) => (
                "succeeded",
                json!({"url": url}),
                format!("{tag} 的 Release 构建成功：{url}"),
                false,
            ),
            Build::Failed(url, why) => (
                "failed",
                json!({"url": url, "conclusion": why}),
                format!("{tag} 的 Release 构建失败（{why}）：{url}"),
                true,
            ),
        };
        /* the goal asked for this build (`proof`): giving up on it is no evidence that it passed */
        let o = self.db().store().items().operation(op)?;
        let required = o.as_ref().is_some_and(|o| o.required);
        let status = if required && status == "waived" {
            "failed"
        } else {
            status
        };
        /* a status is set once: a second look at the same outcome changes and tells nothing */
        let changed = self
            .db()
            .store()
            .transaction(|tx| steve_store::operation_set_in(tx, op, status, &ev))?;
        self.st.build_checked.remove(op);
        if !changed {
            return Ok(());
        }
        self.db().timeline_add(item, "steve", "sys", &line, None)?;
        if required {
            /* only the result it was asked for waits on it: a result delivered since has its own */
            let current = self.db().item(item)?.is_some_and(|i| {
                i.phase == CLOSING && o.as_ref().is_some_and(|o| o.revision == i.revision)
            });
            if current {
                return self.build_settled(item, &line, status != "succeeded");
            }
            return Ok(());
        }
        if tell {
            flow_quiet(
                self.db(),
                item,
                Update {
                    kind: "build",
                    text: format!("{item} {line}。事项已按约定完成，构建需要你看一下。"),
                    ..Default::default()
                },
            );
        }
        Ok(())
    }
}

/// What the agent hears of the Release build: followed on its own, or (`required` by the goal) the
/// item closes only once it passed.
fn build_text(required: bool) -> &'static str {
    if required {
        "Release 构建不用在回合里等，Steve 会跟踪；这个事项的目标要求构建通过，构建成功后才算完成"
    } else {
        "Release 构建不用等，Steve 会单独跟踪"
    }
}

/// Words a heading of a report the agent stopped to give has: a plan, or something for the user to
/// confirm or decide.
const REPORT_HEADS: [&str; 6] = [
    "方案",
    "待确认",
    "待你确认",
    "需要你确认",
    "待决定",
    "需要你决定",
];

/// The agent's message has a heading of a plan or of something for you to confirm (`## 方案`,
/// `**待确认**`, `方案：`): it stopped to report, not because it lost its way.
pub(crate) fn stopped_to_report(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        let heading = l.starts_with('#')
            || (l.starts_with("**") && l.ends_with("**") && l.len() > 4)
            || ((l.ends_with('：') || l.ends_with(':')) && l.chars().count() <= 20);
        if !heading {
            return false;
        }
        let head = l
            .trim_start_matches('#')
            .trim_matches(|c: char| c == '*' || c == ':' || c == '：' || c.is_whitespace());
        head.chars().count() <= 20 && REPORT_HEADS.iter().any(|w| head.contains(w))
    })
}

/// What the continue says: why Steve stepped in, what it found, what is left, how not to repeat work.
/// `build`: the goal asks for the build the tag starts to pass.
fn continue_text(
    stop: &Stop,
    rule: Option<&str>,
    p: Option<&Probe>,
    build: bool,
    i: &Item,
) -> String {
    let mut s = match stop {
        Stop::Stopped { .. } => "（Steve）你这一回合结束了，但没有交付也没有提问。".to_string(),
        Stop::Idle { .. } => "（Steve）会话停了一会儿，没有在运行，也没有交付或提问。".to_string(),
    };
    s.push_str("如果你是有意停下来汇报、出方案或等用户决定，用 steve_ask 提问，不要直接结束回合。");
    if let (Some(rule), Some(p)) = (rule, p) {
        s.push_str(&format!(
            "按这个项目的约定，{}就算完成，{}；Steve 刚核对过远端：{}。",
            rule_text(rule),
            build_text(build),
            p.summary()
        ));
    }
    let open: Vec<&str> = i
        .plan
        .iter()
        .filter(|x| !x.done && !x.left)
        .map(|x| x.step.as_str())
        .collect();
    if !open.is_empty() {
        let shown: Vec<&str> = open.iter().take(3).copied().collect();
        s.push_str(&format!("计划里还没完成的：{}。", shown.join("；")));
    }
    s.push_str("先核对现状再动手：已经完成的推送、合并、打 tag、发布不要重做。做完用 steve_deliver 交付；确实需要用户决定时用 steve_ask（给出 recommended）。");
    s
}

/// A followed build due for a look: `(op id, item, slug, tag, commit, since)`.
type Due = (String, String, String, String, String, i64);

/// A build as `gh run list` shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Build {
    Running,
    Succeeded(String),
    Failed(String, String),
    Unknown(String),
}

/// The runs of `tag` at `commit`, from `gh run list --json …` output.
pub(crate) fn build_of(runs: &Value, commit: &str) -> Build {
    let Some(rs) = runs.as_array() else {
        return Build::Unknown("gh 的输出看不懂".into());
    };
    let rs: Vec<&Value> = rs
        .iter()
        .filter(|r| commit.is_empty() || r["headSha"].as_str().is_none_or(|h| h == commit))
        .collect();
    if rs.is_empty() {
        return Build::Running;
    }
    if rs.iter().any(|r| r["status"] != "completed") {
        return Build::Running;
    }
    let url = |r: &Value| r["url"].as_str().unwrap_or("").to_string();
    match rs.iter().find(|r| {
        !matches!(
            r["conclusion"].as_str(),
            Some("success" | "skipped" | "neutral")
        )
    }) {
        Some(r) => Build::Failed(
            url(r),
            r["conclusion"].as_str().unwrap_or("未知").to_string(),
        ),
        None => Build::Succeeded(url(rs[0])),
    }
}

/// `gh run list` for the runs `tag` started, with a deadline.
fn query_build(gh: &Path, slug: &str, tag: &str, commit: &str) -> Build {
    let mut cmd = std::process::Command::new(gh);
    cmd.args([
        "run",
        "list",
        "--repo",
        slug,
        "--branch",
        tag,
        "--limit",
        "10",
        "--json",
        "databaseId,status,conclusion,url,headSha,workflowName",
    ])
    .env("GH_PROMPT_DISABLED", "1");
    let out = match crate::util::run_deadline(cmd, GH_DEADLINE) {
        Ok((true, out, _)) => out,
        Ok((false, _, e)) => return Build::Unknown(js_head(e.trim(), 200).to_string()),
        Err(crate::util::RunError::Spawn(e)) => return Build::Unknown(format!("gh 不可用：{e}")),
        Err(_) => return Build::Unknown("gh 超时".into()),
    };
    match serde_json::from_str::<Value>(&out) {
        Ok(v) => build_of(&v, commit),
        Err(e) => Build::Unknown(e.to_string()),
    }
}

impl Core {
    /// One look at every followed build that is due (network calls outside the lock). Returns how many
    /// were looked at.
    pub fn check_builds(&self) -> Result<usize> {
        let due = self.tx().builds_due()?;
        let gh = self.config().gh.clone();
        for (op, item, slug, tag, commit, since) in &due {
            let b = if slug.is_empty() || tag.is_empty() {
                Build::Unknown("没有仓库或 tag".into())
            } else {
                query_build(&gh, slug, tag, commit)
            };
            self.tx().build_seen(op, item, tag, *since, b)?;
        }
        Ok(due.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_completion_probe_cannot_finish_or_continue_a_new_dependency_wait() {
        for explicit in [false, true] {
            for met in [false, true] {
                let k = crate::testkit::Kit::with(|c| c.takeover_continues = 1);
                let item = k.item("run", "执行", None);
                let dep = k.item("run", "执行", None);
                let sid = k.session(&item, "idle");
                // The probe began before the dependency was registered. Deliver its callback
                // only after registration, without relying on network timing.
                k.core.tx().st.probing.insert(
                    item.clone(),
                    Looking {
                        since: now(),
                        seq: 1,
                        stale: false,
                    },
                );
                if explicit {
                    let s = k.db().session(&sid).unwrap().unwrap();
                    k.core
                        .tool_item_dependencies(
                            &s,
                            json!({"action":"wait","deps":[dep],"reason":"等待前置验收","resume":"automatic"}),
                        )
                        .unwrap();
                } else {
                    k.core.add_dep(&item, &dep).unwrap();
                }
                assert_eq!(k.status(&item).0, "block");
                k.core
                    .tx()
                    .probed(
                        &item,
                        1,
                        DONE_MERGE,
                        "head".into(),
                        Probe {
                            met,
                            branch: "origin/main".into(),
                            landed: met.then(|| "commit".into()),
                            ..Default::default()
                        },
                        Probing::Stall {
                            session: sid.clone(),
                            stop: Stop::Idle {
                                why: "旧回合停下了".into(),
                            },
                        },
                    )
                    .unwrap();
                assert_eq!(k.status(&item).0, "block");
                assert_eq!(k.phase(&item), ACTIVE);
                assert!(k.rt.prompts_of(&sid).is_empty());
                assert!(k.open_kinds(&item).is_empty());
                if explicit {
                    assert!(k.core.tx().dependency_waiting(&item).unwrap());
                }

                k.db()
                    .exec(
                        "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id=?",
                        &[&dep],
                    )
                    .unwrap();
                k.core.lifecycle_tick().unwrap();
                assert_eq!(k.status(&item).0, "run");
                assert_eq!(k.rt.prompts_of(&sid).len(), 1);
                k.core.lifecycle_tick().unwrap();
                assert_eq!(k.rt.prompts_of(&sid).len(), 1);
            }
        }
    }

    #[test]
    fn builds_read_from_gh_runs() {
        let run = |status: &str, conclusion: &str, sha: &str| json!({"status": status, "conclusion": conclusion, "headSha": sha, "url": "u"});
        assert_eq!(build_of(&json!([]), "c"), Build::Running);
        assert_eq!(
            build_of(&json!([run("in_progress", "", "c")]), "c"),
            Build::Running
        );
        assert_eq!(
            build_of(&json!([run("completed", "success", "c")]), "c"),
            Build::Succeeded("u".into())
        );
        assert_eq!(
            build_of(
                &json!([
                    run("completed", "failure", "c"),
                    run("completed", "success", "c")
                ]),
                "c"
            ),
            Build::Failed("u".into(), "failure".into())
        );
        // a run of another commit under the same name is not this build
        assert_eq!(
            build_of(&json!([run("completed", "failure", "x")]), "c"),
            Build::Running
        );
        assert!(matches!(build_of(&json!({}), "c"), Build::Unknown(_)));
    }
}
