//! Continue an interrupted turn when the executor has work it can resume.

use crate::Tx;
use crate::error::Result;
use crate::repo::Item;
use crate::util::js_head;

/// Timeline lines of Steve's own continues start with this (they are counted).
pub const LINE: &str = "Steve 接手：";
/// In a card's question: Steve already continued the agent.
pub const STEVE_TRIED: &str = "Steve 已让它继续";
/// Timeline lines of a refused `steve_deliver` start with this; the reason follows.
pub const REFUSED: &str = "交付被拒：";
/// The timeline line of a delivery taken in: refusals before it are old news.
pub(crate) const DELIVERED: &str = "交付，等你验收";
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

impl Tx<'_> {
    /// An agent stopped: Steve looks first. `Some(why)` when it is for you after all — the caller opens
    /// the card; `None` when Steve took it (continued, or is checking).
    pub(crate) fn takeover(
        &mut self,
        item_id: &str,
        sid: &str,
        stop: Stop,
    ) -> Result<Option<String>> {
        let i = self.item(item_id)?;
        /* it stopped on purpose — a question, or an answer to what you asked it from the conversation —
         * so it is yours, not continued. A report, a plan or something to confirm left in plain text is
         * reminded first: the agent may have lost Steve's tools after a compaction or in a long context
         * (decided by the user on 2026-11-09), and it decides then whether to deliver or ask */
        if let Stop::Stopped {
            last, answered_you, ..
        } = &stop
            && (crate::agent_questions::looks_like_question(last).is_some() || *answered_you)
        {
            return Ok(Some(stop.why().into()));
        }
        /* and it waits for something only you can do (sign in, scan a code, confirm on a device),
         * said anywhere in its final reply however long: continuing it would only have it take the
         * step as done. Whatever way it stopped. */
        if crate::agent_questions::own_action(&self.db().final_reply(sid)?).is_some() {
            return Ok(Some(stop.why().into()));
        }
        let why = stop.why().to_string();
        self.takeover_continue(&i, sid, &why, &continue_text(&stop, &i))
    }

    /// The agent's turn failed in the harness: it is continued once more (within the same limit as a
    /// stop), told what failed and to check before redoing anything. `Some(why)` for the card.
    pub(crate) fn takeover_error(
        &mut self,
        item_id: &str,
        sid: &str,
        why: &str,
        error: &str,
    ) -> Result<Option<String>> {
        let i = self.item(item_id)?;
        let text = format!(
            "（Steve）上一回合出错中断了：{}。先核对现状再动手：已经完成的推送、合并、打 tag、发布不要重做；然后接着做，做完用 steve_deliver 交付，需要用户决定时用 steve_ask。",
            js_head(error, 300)
        );
        self.takeover_continue(&i, sid, why, &text)
    }

    /// The turn was cut because the Node running it restarted (its harness gone, the attempt given
    /// up by the Hub): the agent is continued once more (within the same limit), told to check the
    /// state before redoing anything. `Some(why)` for the card.
    pub(crate) fn takeover_restarted(
        &mut self,
        item_id: &str,
        sid: &str,
        why: &str,
    ) -> Result<Option<String>> {
        let i = self.item(item_id)?;
        let text = "（Steve）上一回合被打断了：运行它的 Node 重启了。先核对现状再动手：已经完成的推送、合并、打 tag、发布不要重做；然后接着做，做完用 steve_deliver 交付，需要用户决定时用 steve_ask。";
        self.takeover_continue(&i, sid, why, text)
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
        /* its session is in error: a message would only wait in its queue (#347) */
        if s.status == "error" {
            return self.fail(item_id, why);
        }
        let waiting = self.db().decisions_of_item(item_id)?.iter().any(|d| {
            d.status == steve_types::DecisionStatus::Open
                && matches!(d.kind.as_str(), "approve" | "ask")
        });
        if waiting || self.busy(&s.id) || !self.db().queued(&s.id)?.is_empty() {
            self.c.steve_note(&s.id, &text);
            return Ok(());
        }
        if let Some(why) = self.takeover_continue(&i, &s.id, why, &text)? {
            self.fail(item_id, &why)?;
        }
        Ok(())
    }

    /// When you last answered a card of the item; 0 when never.
    fn last_answered(&self, item_id: &str) -> Result<i64> {
        Ok(self
            .db()
            .decisions_of_item(item_id)?
            .iter()
            .filter(|d| d.status == steve_types::DecisionStatus::Resolved)
            .filter_map(|d| d.resolved)
            .max()
            .unwrap_or(0))
    }

    /// Steve's continues since you last did something on the item.
    pub(crate) fn takeover_count(&self, item_id: &str) -> Result<u32> {
        let answered = self.last_answered(item_id)?;
        let t = self.db().timeline(item_id)?;
        Ok(t.iter()
            .rev()
            .take_while(|e| e.kind != "me")
            .filter(|e| e.ts > answered && e.actor == "steve" && e.text.starts_with(LINE))
            .count() as u32)
    }

    /// `steve_deliver` was refused for `why`: noted on the item, for the next continue to read.
    pub(crate) fn deliver_refused(&mut self, item_id: &str, sid: &str, why: &str) -> Result<()> {
        self.db().timeline_add(
            item_id,
            "steve",
            "sys",
            &format!("{REFUSED}{}", js_head(why, REFUSED_CHARS)),
            Some(sid),
        )?;
        Ok(())
    }

    /// The reasons `steve_deliver` was refused since the last delivery taken in or the last thing you
    /// did on the item, newest first.
    fn refusals(&self, item_id: &str) -> Result<Vec<String>> {
        let answered = self.last_answered(item_id)?;
        let t = self.db().timeline(item_id)?;
        Ok(t.iter()
            .rev()
            .take_while(|e| e.kind != "me" && e.ts > answered && e.text != DELIVERED)
            .filter(|e| e.actor == "steve")
            .filter_map(|e| e.text.strip_prefix(REFUSED))
            .map(str::to_string)
            .collect())
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
        /* looked at again right before a continue is spent: the stop may be old news (a probe
         * that came back late, a unit that started meanwhile), and a waiting item is not continued */
        if self.turn_has_wait(&self.item(&i.id)?, false)? {
            self.diagnostic_session(
                &s.id,
                None,
                "takeover.evaluated",
                "work_waiting",
                true,
                Default::default(),
            );
            return Ok(None);
        }
        /* you stopped its last turn: going on is yours, whatever stopped it besides (a failure the
         * coordinator was told of, a voided delivery); nothing of the budget is spent */
        if self
            .db()
            .store()
            .sessions()
            .stop(&s.id)?
            .is_some_and(|k| crate::turn_wait::held(&k))
        {
            self.diagnostic_session(
                &s.id,
                None,
                "takeover.evaluated",
                "user_stopped",
                false,
                Default::default(),
            );
            return Ok(Some(why.into()));
        }
        /* its delivery was refused for the same reason again and again: delivering once more would
         * only be refused once more, so it is yours; a different reason is the agent's to work on */
        let refusals = self.refusals(&i.id)?;
        let same = self.cfg().takeover_same_refusals as usize;
        if same > 0 && refusals.len() >= same && refusals[..same].iter().all(|r| *r == refusals[0])
        {
            self.diagnostic_session(
                &s.id,
                None,
                "takeover.evaluated",
                "same_refusal",
                false,
                steve_store::diagnostics::Details {
                    attempt_ordinal: Some(same as u64),
                    limit: Some(same as u64),
                    ..Default::default()
                },
            );
            return Ok(Some(format!(
                "{why}（交付连续 {same} 次因同一原因被拒：{}）",
                refusals[0]
            )));
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
        self.record_continue(i, &s, why, text, &refusals, (n, cap))
    }

    fn record_continue(
        &mut self,
        i: &Item,
        s: &crate::repo::Session,
        why: &str,
        text: &str,
        refusals: &[String],
        budget: (u32, u32),
    ) -> Result<Option<String>> {
        let (n, cap) = budget;
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
        let text = match refusals.first() {
            Some(r) => format!("{text}{}", refused_text(r)),
            None => text.to_string(),
        };
        self.prompt(&s.id, &text, "steve");
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

/// How much of a refusal's reason the timeline keeps.
const REFUSED_CHARS: usize = 300;

/// What a continue adds when the agent's last delivery was refused: the reason, and not to deliver the
/// same again.
fn refused_text(reason: &str) -> String {
    format!(
        "你上一次用 steve_deliver 交付被拒绝了，原因：{reason}。先解决这个原因再交付，不要原样再交一次；只有用户能解决时，用 steve_ask 提问。"
    )
}

fn continue_text(stop: &Stop, i: &Item) -> String {
    let mut s = match stop {
        Stop::Stopped { .. } => {
            "（Steve）你这一回合结束了，但没有用 steve_deliver 交付，也没有用 steve_ask 提问。"
                .to_string()
        }
        Stop::Idle { .. } => "（Steve）会话停了一会儿，没有在运行，也没有交付或提问。".to_string(),
    };
    s.push_str(
        "如果上下文压缩过，提醒你：Steve 的工具 steve_update、steve_ask、steve_deliver 仍然可用。",
    );
    s.push_str("如果工作已经完成，现在就用 steve_deliver 交付，可以直接用你刚才的汇报做摘要，列出实际跑过的检查；如果有需要用户决定的事，用 steve_ask 提问并给出 recommended；否则接着做完。");
    if let Stop::Stopped { whole, .. } = stop
        && stopped_to_report(whole)
    {
        s.push_str("你刚才的回复里有等用户确认的内容：请用 steve_ask 提出来，只写在回复里用户看不到要回答。");
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
    s.push_str("先核对现状再动手：已经完成的推送、合并、打 tag、发布不要重做。");
    s
}
