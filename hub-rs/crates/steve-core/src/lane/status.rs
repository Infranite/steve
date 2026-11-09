//! `steve_lane_status`: what became of the calling item's release request, read from what is kept (the
//! item's newest lane request, its unit and the unit's cards; or, for a project released by its own
//! script, the item's release card and its recorded outcome). Read-only: it changes nothing and asks
//! nobody, so an agent can look instead of asking the user which batch it is in.
//!
//! The text is read by the agent (a tool result), so it stays Chinese and does not go through `t!`
//! (steve-i18n: "What stays Chinese").

use serde::Serialize;
use serde_json::Value;
use steve_store::lanes::{self, LaneRequest, LaneRow, LaneUnit};

use super::store::{label, subject};
use crate::Core;
use crate::cycle::OP_BUILD;
use crate::error::Result;
use crate::repo::{Decision, Item, ItemExt, Session};

/// What the agent should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Next {
    /// Nothing to do but end the turn: Steve (or the user) moves it on, the result arrives as a message.
    Wait,
    /// It came back: fix what `failure.reason` says, deliver again; it goes back into the lane once accepted.
    Fix,
    /// No release was asked for: call `steve_publish` when the item is accepted and should go live.
    Publish,
    /// It is done: nothing more to do for the release.
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request {
    pub id: String,
    pub branch: String,
    /// The commit that was handed in (once merged, it is in the target branch).
    pub head: String,
    /// The request's own state: queued | collecting | merging | merged | released | left
    pub state: String,
    pub arrived: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Batch {
    /// `MQ-<no>`, as the cards and the timeline name it.
    pub id: String,
    pub no: i64,
    pub lane_id: String,
    /// The target: upstream repository and branch.
    pub target: String,
    /// The batch branch (`steve/merge/<target branch>/MQ-<no>`).
    pub branch: String,
    /// The unit's own state: collecting | merging | releasing | release_failed | done
    pub state: String,
    /// 1-based place of this item among the batch's requests, in merge order.
    pub position: Option<usize>,
    pub size: usize,
    /// The other requests in the batch (`<item>（<branch>）`).
    pub others: Vec<String>,
    /// When the collecting window closes on its own (Unix ms), while it collects.
    pub closes_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
    pub decision_id: String,
    /// approve | fail
    pub kind: String,
    pub question: String,
    pub options: Vec<String>,
    /// Who answers it: `user` (on the card in Steve), or `yolo` when YOLO decides for this item.
    pub answer_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    pub reason: String,
    /// Where to read more: the release log file, the batch's worktree kept for a retry, or the timeline.
    pub log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Merged {
    /// The target branch it went into.
    pub into: String,
    /// The delivered commit, now contained in `into`.
    pub commit: Option<String>,
    /// The version released with it, when one was cut.
    pub version: Option<String>,
}

/// The item's newest build after its release (a GitHub Release the tag started), as followed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Build {
    pub status: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneStatus {
    pub item_id: Option<String>,
    /// lane (the merge lane) | queue (the merge queue) | release (the project's own release script);
    /// none when nothing was asked.
    pub path: Option<&'static str>,
    /// not_requested | queued | collecting | checking | waiting_for_approval | merging |
    /// merged_waiting_release | releasing | release_failed | left | released | declined | not_released |
    /// failed | expired
    pub state: &'static str,
    pub request: Option<Request>,
    pub queue_position: Option<usize>,
    pub batch: Option<Batch>,
    pub pending_card: Option<Card>,
    pub failure: Option<Failure>,
    pub merged: Option<Merged>,
    pub build: Option<Build>,
    pub item_status: Option<String>,
    pub next: Next,
    /// Plain words: where it is.
    pub summary: String,
    /// Plain words: what to do now.
    pub advice: String,
}

impl LaneStatus {
    fn new(item: Option<&Item>) -> Self {
        Self {
            item_id: item.map(|i| i.id.clone()),
            path: None,
            state: "not_requested",
            request: None,
            queue_position: None,
            batch: None,
            pending_card: None,
            failure: None,
            merged: None,
            build: None,
            item_status: item.map(|i| i.status.clone()),
            next: Next::Publish,
            summary: String::new(),
            advice: String::new(),
        }
    }

    /// The tool result: the plain words, then every field as JSON.
    pub fn render(&self) -> String {
        format!(
            "{}\n下一步：{}\n\n{}",
            self.summary,
            self.advice,
            serde_json::to_string_pretty(self).unwrap_or_default()
        )
    }
}

const WAIT: &str = "等待即可：结束这一回合，结果会作为新消息发给你。不要问用户在哪一批、进展如何，需要时再调用 steve_lane_status。";

impl Core {
    /// What became of the release request of `s`'s item ([`LaneStatus`]).
    pub fn lane_status(&self, s: &Session) -> Result<LaneStatus> {
        let Some(item_id) = s.item_id.as_deref() else {
            let mut st = LaneStatus::new(None);
            st.next = Next::Nothing;
            st.summary = "这个会话不属于任何事项，没有发布请求。".into();
            st.advice = "不需要做什么。".into();
            return Ok(st);
        };
        self.lane_status_of_item(item_id)
    }

    /// The same read-only release projection without impersonating an item's session.
    pub(crate) fn lane_status_of_item(&self, item_id: &str) -> Result<LaneStatus> {
        let Some(item) = self.store().item(item_id)? else {
            return Err(crate::error::msg(format!("没有这个事项：{item_id}")));
        };
        let store = self.store().store();
        // the merge queue's, when it has the item
        if let Some(e) = store.reader(|c| steve_store::mq::entry_last_of_item(c, item_id))? {
            return self.mq_status(&item, e);
        }
        let req = store.reader(|c| lanes::request_last_of_item(c, item_id))?;
        let card = self
            .store()
            .decisions_of_item(item_id)?
            .into_iter()
            .filter(|d| d.kind == "approve" && d.subject_type == "release")
            .max_by_key(|d| d.created);
        let mut st = LaneStatus::new(Some(&item));
        st.build = self.last_build(&item)?;
        // the newer of the two is what the last steve_publish made
        match (req, card) {
            (Some(r), Some(d)) if d.created > r.arrived => {
                self.release_card_status(&mut st, &item, &d)?
            }
            (Some(r), _) => self.lane_request_status(&mut st, &item, r)?,
            (None, Some(d)) => self.release_card_status(&mut st, &item, &d)?,
            (None, None) => {
                st.summary = "还没有发布请求：这个事项没有调用过 steve_publish。".into();
                st.advice = "事项验收通过、用户要上线时，用 steve_publish 请求发布。".into();
            }
        }
        Ok(st)
    }

    /// The status of an item the merge queue has (`entry`, its newest).
    fn mq_status(&self, item: &Item, e: steve_store::mq::MqEntry) -> Result<LaneStatus> {
        let store = self.store().store();
        let t = store.reader(|c| steve_store::mq::target_get(c, &e.target_id))?;
        let target = t
            .as_ref()
            .map(|t| format!("{} {}", t.upstream, t.branch))
            .unwrap_or_default();
        let into = t.as_ref().map(|t| t.branch.clone()).unwrap_or_default();
        let mut st = LaneStatus::new(Some(item));
        st.path = Some("queue");
        st.build = self.last_build(item)?;
        st.request = Some(Request {
            id: e.id.clone(),
            branch: e.branch.clone().unwrap_or_default(),
            head: e.head.clone(),
            state: e.state.clone(),
            arrived: e.arrived,
        });
        let batch = e.batch_no.map(|n| format!("MQ-{n}"));
        let (state, next, summary, advice): (&'static str, Next, String, String) = match e.state.as_str() {
            "queued" => (
                "queued",
                Next::Wait,
                format!("在合并队列里排队（{target}）：目标空闲就开始合入，同一目标忙时和其他改动一起成批。"),
                WAIT.into(),
            ),
            "batched" => (
                "merging",
                Next::Wait,
                format!("{} 正在合入 {into}：在 Node 上集成、按目标的检查验证后推送。", batch.clone().unwrap_or_default()),
                WAIT.into(),
            ),
            "landed" => {
                st.merged = Some(Merged {
                    into: into.clone(),
                    commit: e.landed_commit.clone(),
                    version: None,
                });
                (
                    "merged_waiting_release",
                    Next::Wait,
                    format!("已合入 {into}（{}），按项目约定核对完成条件。", batch.clone().unwrap_or_default()),
                    WAIT.into(),
                )
            }
            "released" => (
                "released",
                Next::Nothing,
                format!("已合入 {into} 并发布。"),
                "不需要再做什么。".into(),
            ),
            "returned" => {
                st.failure = Some(Failure {
                    reason: e.reason.clone().unwrap_or_default(),
                    log: None,
                });
                (
                    "left",
                    Next::Fix,
                    format!("合并队列退回了这一项：{}", e.reason.clone().unwrap_or_default()),
                    "冲突的话在最新的目标分支上 rebase（只是 rebase 不用重新验收，结束回合即可）；检查没通过就修好后用 steve_deliver 重新交付。".into(),
                )
            }
            _ => (
                "left",
                Next::Nothing,
                "这一项不在合并队列里了（见时间线），按项目原来的方式合并。".into(),
                "推送、开 PR、合并、打 tag 是对外动作，先用 steve_request_approval 请求批准。".into(),
            ),
        };
        st.state = state;
        st.next = next;
        st.summary = summary;
        st.advice = advice;
        Ok(st)
    }

    fn answer_by(&self, item: &Item) -> String {
        if self.yolo_applies(&item.workspace_id, Some(&item.id)) {
            "yolo".into()
        } else {
            "user".into()
        }
    }

    fn card(&self, item: &Item, d: &Decision) -> Card {
        Card {
            decision_id: d.id.clone(),
            kind: d.kind.clone(),
            question: d.question.clone(),
            options: d.options.clone(),
            answer_by: self.answer_by(item),
        }
    }

    fn last_build(&self, item: &Item) -> Result<Option<Build>> {
        Ok(self
            .store()
            .store()
            .items()
            .operations(&item.id, item.cycle)?
            .into_iter()
            .rfind(|o| o.kind == OP_BUILD)
            .map(|o| Build {
                status: o.status,
                label: o.label,
            }))
    }

    /// The open fail card of the item, if any (a refusal only the user can lift waits on it).
    fn open_fail(&self, item: &Item) -> Result<Option<Decision>> {
        Ok(self
            .store()
            .decisions_of_item(&item.id)?
            .into_iter()
            .rfind(|d| d.kind == "fail" && d.status == "open"))
    }

    fn lane_request_status(&self, st: &mut LaneStatus, item: &Item, r: LaneRequest) -> Result<()> {
        let store = self.store().store();
        st.path = Some("lane");
        let row: Option<LaneRow> = store.reader(|c| lanes::lane_get(c, &r.lane_id))?;
        let (target_branch, target) = row
            .as_ref()
            .map(|l| (l.branch.clone(), format!("{} {}", l.upstream, l.branch)))
            .unwrap_or_default();
        st.request = Some(Request {
            id: r.id.clone(),
            branch: r.branch.clone(),
            head: r.head.clone(),
            state: r.state.clone(),
            arrived: r.arrived,
        });
        // the unit it merges in, or (merged) the one its release goes out with
        let no = match r.state.as_str() {
            "merged" | "released" => r.released_in.or(r.unit),
            _ => r.unit,
        };
        let unit: Option<LaneUnit> = match no {
            Some(n) => store.reader(|c| lanes::unit_get(c, &r.lane_id, n))?,
            None => None,
        };
        let all = store.reader(|c| lanes::requests_of(c, &r.lane_id, false))?;
        // the lane's window as the project page shows it: what is collecting, what runs and whether approved
        let view = match &row {
            Some(l) => self.lane_view(&l.upstream, &l.branch)?.unwrap_or_default(),
            None => Value::Null,
        };
        let held = |k: &str| no.is_some() && view[k]["no"].as_i64() == no;
        if let Some(n) = no {
            // merged: the batch its release goes out with; before: the one it merges in
            let released = matches!(r.state.as_str(), "merged" | "released");
            let mates: Vec<&LaneRequest> = all
                .iter()
                .filter(|x| {
                    x.id == r.id
                        || if released {
                            x.released_in == Some(n) && x.state != "left"
                        } else {
                            x.unit == Some(n)
                                && matches!(x.state.as_str(), "collecting" | "merging")
                        }
                })
                .collect();
            let closes_at = (view["collecting"]["no"].as_i64() == Some(n))
                .then(|| view["collecting"]["closes_at"].as_i64())
                .flatten();
            st.batch = Some(Batch {
                id: format!("MQ-{n}"),
                no: n,
                lane_id: r.lane_id.clone(),
                target: target.clone(),
                branch: unit
                    .as_ref()
                    .and_then(|u| u.branch.clone())
                    .unwrap_or_else(|| format!("steve/merge/{target_branch}/MQ-{n}")),
                state: unit.as_ref().map(|u| u.state.clone()).unwrap_or_default(),
                position: mates.iter().position(|x| x.id == r.id).map(|p| p + 1),
                size: mates.len(),
                others: mates
                    .iter()
                    .filter(|x| x.id != r.id)
                    .map(|x| label(x))
                    .collect(),
                closes_at,
            });
        }
        let mq = no.map(|n| format!("MQ-{n}")).unwrap_or_default();
        let card_of = |kind: &str| -> Result<Option<Decision>> {
            Ok(match no {
                Some(n) => store.reader(|c| {
                    lanes::batch_card_decision(c, &subject(&r.lane_id, n as u64), kind)
                })?,
                None => None,
            })
        };
        let worktree = no.map(|n| {
            self.config()
                .home
                .join("worktrees/lanes")
                .join(&r.lane_id)
                .join(format!("MQ-{n}"))
        });
        st.next = Next::Wait;
        st.advice = WAIT.into();
        match r.state.as_str() {
            "queued" => {
                st.state = "queued";
                let ahead: Vec<&LaneRequest> = all.iter().filter(|x| x.state == "queued").collect();
                st.queue_position = ahead.iter().position(|x| x.id == r.id).map(|p| p + 1);
                st.summary = format!(
                    "在合并通道（{target}）里排队：前面的批次合并发布完，它会进下一批{}。",
                    st.queue_position
                        .map(|p| format!("（排第 {p} 位）"))
                        .unwrap_or_default()
                );
            }
            "collecting" => {
                st.state = "collecting";
                st.summary = if held("collecting") && view["collecting"]["closed"] == true {
                    format!(
                        "在合并通道（{target}）的 {mq} 里：这一批已经截止，等前一批合并完就开始，Steve 会请用户批准合并。"
                    )
                } else {
                    format!(
                        "在合并通道（{target}）的 {mq} 里：这一批正在收集同一目标的其他改动，合批窗口结束（或用户要求立即合并）后，Steve 会请用户批准合并。"
                    )
                };
            }
            "merging" => match card_of("approve")? {
                Some(d) if d.status == "open" => {
                    st.state = "waiting_for_approval";
                    let c = self.card(item, &d);
                    let who = if c.answer_by == "yolo" {
                        "YOLO"
                    } else {
                        "用户"
                    };
                    st.summary = format!(
                        "在合并通道（{target}）的 {mq} 里，这一批在等{who}批准合并（卡片「{}」）。",
                        d.question
                    );
                    st.pending_card = Some(c);
                }
                d if d.as_ref().is_some_and(|d| d.status == "resolved")
                    || (held("running") && view["running"]["approved"] == true) =>
                {
                    st.state = "merging";
                    st.summary = format!(
                        "{mq} 已批准，Steve 正在把这一批合并到 {target_branch}（建批次分支、推送）。"
                    );
                }
                _ => {
                    st.state = "checking";
                    st.summary = format!(
                        "{mq} 正在准备：Steve 在核对目标分支的合并规则，接着会请用户批准合并。"
                    );
                }
            },
            "merged" => {
                let u_state = unit.as_ref().map(|u| u.state.as_str()).unwrap_or_default();
                st.merged = Some(Merged {
                    into: target_branch.clone(),
                    commit: Some(r.head.clone()),
                    version: None,
                });
                match u_state {
                    "release_failed" => {
                        st.state = "release_failed";
                        let why = unit
                            .as_ref()
                            .and_then(|u| u.outcome.clone())
                            .unwrap_or_else(|| "发布失败".into());
                        st.failure = Some(Failure {
                            reason: why.clone(),
                            log: worktree
                                .as_ref()
                                .filter(|w| w.is_dir())
                                .map(|w| w.to_string_lossy().into_owned()),
                        });
                        if let Some(d) = card_of("fail")?.filter(|d| d.status == "open") {
                            st.pending_card = Some(self.card(item, &d));
                        }
                        st.summary = format!(
                            "{mq} 已合并到 {target_branch}，但发布失败：{why}。现在等用户在卡片上选择重试或放弃。"
                        );
                        st.advice = "等待即可：重试还是放弃由用户在卡片上决定，结束这一回合。失败原因要是出在你的改动上，可以在回复里说明该怎么修；用户放弃后它会退回给你。".into();
                    }
                    "releasing" => {
                        st.state = "releasing";
                        st.summary = format!(
                            "{mq} 已合并到 {target_branch}，Steve 正在发布这一批（打 tag，等 CI）。"
                        );
                    }
                    _ => {
                        st.state = "merged_waiting_release";
                        st.summary = format!(
                            "已合并到 {target_branch}，发布和 {mq} 一起进行：等那一批合并后统一发布。"
                        );
                    }
                }
            }
            "released" => {
                st.state = "released";
                let version = unit.as_ref().and_then(|u| u.version.clone());
                st.merged = Some(Merged {
                    into: target_branch.clone(),
                    commit: Some(r.head.clone()),
                    version: version.clone(),
                });
                st.summary = match &version {
                    Some(v) => format!("{mq} 已合并到 {target_branch} 并发布为 v{v}。"),
                    None => format!("{mq} 已合并到 {target_branch}，这个仓库合并即发布。"),
                };
                self.done_or_build(st, item);
            }
            _ => {
                // left: sent back (fix it), or refused for a reason only the user can change (a card asks)
                st.state = "left";
                let why = r.reason.clone().unwrap_or_else(|| "没有说明".into());
                st.failure = Some(Failure {
                    reason: why.clone(),
                    log: Some("事项时间线".into()),
                });
                st.summary = format!(
                    "没有合并：这一项退出了合并通道{}，原因：{why}。",
                    if mq.is_empty() {
                        String::new()
                    } else {
                        format!("（{mq}）")
                    }
                );
                if let Some(d) = self.open_fail(item)? {
                    st.pending_card = Some(self.card(item, &d));
                    st.advice =
                        "等待即可：这个原因你改不了，用户会在卡片上决定怎么处理，结束这一回合。"
                            .into();
                } else if item.is_settled() {
                    st.next = Next::Nothing;
                    st.advice = "事项已经结束，不需要做什么。".into();
                } else {
                    st.next = Next::Fix;
                    st.advice = "按上面的原因修好，提交后用 steve_deliver 重新交付；验收通过后再用 steve_publish，它会重新进合并通道。".into();
                }
            }
        }
        Ok(())
    }

    /// Released: done, unless the build the tag started is still followed or failed.
    fn done_or_build(&self, st: &mut LaneStatus, item: &Item) {
        match st.build.as_ref().map(|b| b.status.as_str()) {
            Some("pending" | "running" | "unknown") if !item.is_finished() => {
                st.next = Next::Wait;
                st.summary
                    .push_str("tag 触发的构建还在进行，Steve 在跟进。");
                st.advice = WAIT.into();
            }
            Some("failed") if !item.is_finished() => {
                st.next = Next::Fix;
                st.summary.push_str("但 tag 触发的构建失败了。");
                st.advice = "Steve 会把失败的构建告诉你；按它说的修好再交付。".into();
            }
            _ => {
                st.next = Next::Nothing;
                st.advice = "已经完成，不需要再做什么，也不要再调用 steve_publish。".into();
            }
        }
    }

    fn release_card_status(&self, st: &mut LaneStatus, item: &Item, d: &Decision) -> Result<()> {
        st.path = Some("release");
        let s = |k: &str| d.result.get(k).and_then(Value::as_str).map(str::to_string);
        st.next = Next::Wait;
        st.advice = WAIT.into();
        match d.status.as_str() {
            "open" => {
                st.state = "waiting_for_approval";
                let c = self.card(item, d);
                let who = if c.answer_by == "yolo" {
                    "YOLO"
                } else {
                    "用户"
                };
                st.summary = format!("发布请求在等{who}批准（卡片「{}」）。", d.question);
                st.pending_card = Some(c);
            }
            "expired" => {
                st.state = "expired";
                st.next = Next::Publish;
                st.summary = "上一次的发布请求已经失效，没有发布。".into();
                st.advice = "如果还需要上线，再用 steve_publish 请求一次。".into();
            }
            _ if !d.result.get("approved").is_some_and(crate::gateway::truthy) => {
                st.state = "declined";
                st.next = Next::Nothing;
                st.summary = "用户没有批准这次发布。".into();
                st.advice = "这个周期不再发布；按用户的意思还要做的做完就结束这一回合。".into();
            }
            _ => match s("outcome").as_deref() {
                Some("released") => {
                    st.state = "released";
                    let version = s("version");
                    st.merged = Some(Merged {
                        into: "main".into(),
                        commit: None,
                        version: version.clone(),
                    });
                    st.summary = match &version {
                        Some(v) => format!("已发布为 v{v}。"),
                        None => "已发布。".into(),
                    };
                    self.done_or_build(st, item);
                }
                Some(o @ ("failed" | "not_released")) => {
                    st.state = if o == "failed" {
                        "failed"
                    } else {
                        "not_released"
                    };
                    let fail = self.open_fail(item)?;
                    let reason = s("reason")
                        .or_else(|| s("error"))
                        .or_else(|| fail.as_ref().map(|f| f.question.clone()))
                        .unwrap_or_else(|| "发布没有成功".into());
                    st.failure = Some(Failure {
                        reason: reason.clone(),
                        log: s("log"),
                    });
                    st.summary = format!("没有发布成功：{reason}。");
                    if let Some(f) = fail {
                        st.pending_card = Some(self.card(item, &f));
                        st.advice =
                            "等待即可：用户会在失败卡片上决定重试、返工还是放弃，结束这一回合。"
                                .into();
                    } else if !item.is_settled() {
                        st.next = Next::Fix;
                        st.advice = "按上面的原因处理，然后再用 steve_publish 请求发布。".into();
                    }
                }
                Some(_) => {
                    st.state = "releasing";
                    st.summary = format!(
                        "已批准，正在发布{}。",
                        s("log")
                            .map(|l| format!("（日志 {l}）"))
                            .unwrap_or_default()
                    );
                }
                None => {
                    st.state = "releasing";
                    st.summary = "已批准，Steve 正在准备发布（rebase 到 main、构建）。".into();
                }
            },
        }
        Ok(())
    }
}
