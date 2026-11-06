//! `steve_ask` answered for the user: the main bot's view of the question
//! from one model call (smart tier, 60 s, no ACP session), turned into [`Answer`] for
//! [`super::judge`]. The judge decides; this file only gathers. Whatever is unclear — no usable reply,
//! the model asks for the user, low confidence without a recommendation — ends with the user.
//!
//! The model is asked once per card: the reply is kept, so the check right before answering sees the
//! same answer (a second, different reply would read as the executor contradicting itself).

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use steve_modules::{ApiError, ApiResult};
use steve_store::yolo as repo;

use super::engine::Engine;
use super::judge::{ASK_PER_ITEM, Answer, Ask};
use crate::gateway::Tier;
use crate::repo::{Decision, Item};
use crate::util::{js_head, js_tail};

/// The model call's timeout.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// How much of the session's last words goes into the prompt.
const TAIL: usize = 2000;
/// Timeline entries in the prompt.
const TIMELINE: usize = 20;

/// The card's draft for the agent's leaning: `推荐：<recommended>\n理由：<why>` (none without a
/// recommendation).
pub fn hint_draft(recommended: Option<&str>, why: Option<&str>) -> Option<String> {
    let rec = recommended.map(str::trim).filter(|r| !r.is_empty())?;
    let mut d = format!("推荐：{rec}");
    if let Some(w) = why.map(str::trim).filter(|w| !w.is_empty()) {
        d.push_str(&format!("\n理由：{w}"));
    }
    Some(d)
}

/// `(recommended, why)` back from a card's draft.
pub fn parse_hint(draft: &str) -> (Option<String>, Option<String>) {
    let line = |p: &str| {
        draft
            .lines()
            .find_map(|l| l.trim().strip_prefix(p))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    (line("推荐："), line("理由："))
}

/// A question as compared for repeats: lower case, whitespace collapsed, trailing punctuation gone.
pub fn normalize(q: &str) -> String {
    let words: Vec<&str> = q.split_whitespace().collect();
    words
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['?', '？', '。', '.', '!', '！', '：', ':'])
        .trim()
        .to_string()
}

/// The agent's last words end on a question (a stall that is really a question in plain text).
pub fn looks_like_question(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let last = t
        .rsplit(['\n', '。', '！', '!'])
        .find(|s| !s.trim().is_empty())
        .unwrap_or(t)
        .trim();
    static ASKING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[?？]\s*$|(吗|呢|是否|要不要|还是)[^。]{0,12}$").expect("valid regex")
    });
    ASKING
        .is_match(last)
        .then(|| js_head(last, 400).to_string())
}

/// What the model is told.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub goal: String,
    pub plan: Vec<String>,
    pub project: String,
    pub timeline: Vec<String>,
    pub session_tail: String,
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub why: Option<String>,
    /// The main bot's prompt with its memories.
    pub main_bot: String,
    /// Module context (knowledge, contact cards).
    pub context: String,
}

pub fn prompt(cx: &Context) -> String {
    let list = |xs: &[String]| {
        if xs.is_empty() {
            "（没有）".to_string()
        } else {
            xs.iter()
                .map(|x| format!("- {x}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let mut out = vec![
        cx.main_bot.clone(),
        "用户不在，开着 YOLO 模式：希望尽量少被打扰。一个替用户干活的 Agent 用 steve_ask 问了用户一个问题。判断你能不能替用户回答。".to_string(),
        "只在答案能从下面的材料里合理推出、换成用户本人大概率也会这么选时才回答；涉及花钱、以用户名义对外说话、推送或发布、删除工作区以外的数据，或者材料不够、几种答案都说得通时，act 写 ask_user。".to_string(),
        format!("事项目标：{}", cx.goal),
        format!("计划：\n{}", list(&cx.plan)),
        format!("项目：{}", if cx.project.is_empty() { "（没有项目）" } else { &cx.project }),
        format!("事项最近的动态：\n{}", list(&cx.timeline)),
        format!("Agent 最后说的话：\n{}", if cx.session_tail.is_empty() { "（没有）" } else { &cx.session_tail }),
    ];
    if !cx.context.trim().is_empty() {
        out.push(format!("相关资料：\n{}", cx.context.trim()));
    }
    out.push(format!("问题：{}", cx.question));
    out.push(format!("选项：\n{}", list(&cx.options)));
    if let Some(r) = &cx.recommended {
        out.push(format!(
            "Agent 推荐：{r}（理由：{}）",
            cx.why.as_deref().unwrap_or("没有写")
        ));
    }
    out.push(
        "只输出 JSON：{\"act\":\"answer|ask_user\",\"answer\":\"给 Agent 的答案\",\"option\":\"选中的选项原文，或 null\",\"confidence\":0到1,\"reason\":\"一句话依据\",\"assumptions\":[\"你做的假设\"]}。如果只是因为信息不足而不能答，reason 以“信息不足”开头。".into(),
    );
    out.retain(|l| !l.trim().is_empty());
    out.join("\n\n")
}

/// The model's reply as an [`Answer`]; `None` when it holds no usable JSON. An option that is not one
/// of `options` is dropped (the free answer stays).
pub fn parse(reply: &str, options: &[String]) -> Option<Answer> {
    let v = steve_modules::changes::first_json(reply)?;
    let act = v.get("act")?.as_str()?.trim().to_string();
    if act != "answer" && act != "ask_user" {
        return None;
    }
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(|x| x.trim().to_string())
            .unwrap_or_default()
    };
    let option = v
        .get("option")
        .and_then(Value::as_str)
        .map(|o| o.trim().to_string())
        .filter(|o| options.iter().any(|x| x.trim() == o));
    let confidence = v
        .get("confidence")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let reason = s("reason");
    let only_lacks_info = act == "ask_user" && reason.starts_with("信息不足");
    let mut answer = s("answer");
    if answer.is_empty()
        && let Some(o) = &option
    {
        answer = o.clone();
    }
    Some(Answer {
        act,
        answer,
        option,
        confidence,
        reason,
        only_lacks_info,
    })
}

/// Runs `f` to completion from synchronous code: on the caller's runtime when there is one (the
/// executor runs on blocking threads, never on a runtime worker), else on a small runtime of its own.
fn block_on<F: std::future::Future>(f: F) -> Option<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => Some(h.block_on(f)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .map(|rt| rt.block_on(f)),
    }
}

fn ce(e: crate::CoreError) -> ApiError {
    match e {
        crate::CoreError::Module(e) => e,
        e => ApiError::Internal(e.to_string()),
    }
}

impl Engine {
    /// The [`Ask`] facts of question `question` (an `ask` card, or the words a stalled agent ended
    /// on) about `item`: the agent's hint, the repeat and count checks, and — when YOLO may answer at
    /// all — the model's answer (asked once per `cache_key`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ask_facts(
        &self,
        d: &Decision,
        item: Option<&Item>,
        question: &str,
        options: &[String],
        hint: (Option<String>, Option<String>),
        cache_key: &str,
        consult: bool,
    ) -> ApiResult<Ask> {
        let (recommended, why) = hint;
        // a recommendation that is not one of the options is no recommendation
        let recommended = recommended
            .filter(|r| options.is_empty() || options.iter().any(|o| o.trim() == r.trim()));
        let key = normalize(question);
        let (repeated, count) = match item {
            Some(i) => self.ask_history(&d.workspace_id, &i.id, &d.id, &key)?,
            None => (false, 0),
        };
        let full = item.is_some_and(|i| {
            self.yolo()
                .effective(&d.workspace_id, Some(&i.id))
                .is_ok_and(|e| e.full)
        });
        let mut a = Ask {
            recommended,
            why,
            answer: None,
            repeated,
            count,
            key,
            full,
        };
        let limited = super::limits::money_or_outside_words(question, full).is_some()
            || a.recommended
                .as_deref()
                .is_some_and(|r| super::limits::money_or_outside_words(r, full).is_some());
        if consult && !a.repeated && a.count < ASK_PER_ITEM && !limited {
            a.answer = self.model_answer(d, item, question, options, &a, cache_key)?;
        }
        Ok(a)
    }

    /// `(asked before, answered this window)` from the item's ledger.
    fn ask_history(
        &self,
        ws: &str,
        item: &str,
        decision: &str,
        key: &str,
    ) -> ApiResult<(bool, u32)> {
        let window = self
            .yolo()
            .store()
            .reader(|c| repo::window_current(c, ws))?
            .map(|w| w.id);
        let acts = self
            .yolo()
            .store()
            .reader(|c| repo::actions_of_item(c, item, 500))?;
        let asked = |a: &&repo::ActionRow| {
            a.outcome == "decided"
                && a.decision_id.as_deref() != Some(decision)
                && (a.rule.starts_with("ask.")
                    || a.rule == "continue.stall" && a.checks.get("question").is_some())
        };
        let repeated = acts
            .iter()
            .filter(asked)
            .any(|a| a.checks.get("question").and_then(Value::as_str) == Some(key));
        let count = acts
            .iter()
            .filter(asked)
            .filter(|a| window.is_some() && a.window_id == window)
            .count();
        Ok((repeated, u32::try_from(count).unwrap_or(u32::MAX)))
    }

    /// The model's answer, asked once per card; a failed call or an unusable reply is an answer that
    /// leaves the card with you.
    fn model_answer(
        &self,
        d: &Decision,
        item: Option<&Item>,
        question: &str,
        options: &[String],
        a: &Ask,
        cache_key: &str,
    ) -> ApiResult<Option<Answer>> {
        if let Some(x) = self.cached_answer(cache_key) {
            return Ok(Some(x));
        }
        let cx = self.ask_context(d, item, question, options, a)?;
        let p = prompt(&cx);
        let gw = self.core().gw_arc();
        let (ws, item_id) = (d.workspace_id.clone(), d.item_id.clone());
        let reply = block_on(async move {
            let call = gw.call(&p, Tier::Smart, TIMEOUT, &crate::gateway::ThinkTools::None);
            match item_id {
                Some(i) => {
                    crate::stats::tagged("yolo", &ws, crate::stats::for_item(&i, call)).await
                }
                None => crate::stats::tagged("yolo", &ws, call).await,
            }
        });
        let ans = match reply {
            Some(Ok(r)) => parse(&r.text, options).unwrap_or_else(|| Answer {
                act: "error".into(),
                answer: String::new(),
                option: None,
                confidence: 0.0,
                reason: "模型没有给出可用的答案".into(),
                only_lacks_info: false,
            }),
            Some(Err(e)) => {
                tracing::warn!(decision = d.id, "yolo answer model call: {e}");
                Answer {
                    act: "error".into(),
                    answer: String::new(),
                    option: None,
                    confidence: 0.0,
                    reason: format!("模型调用失败（{}）", js_head(&e.to_string(), 80)),
                    only_lacks_info: false,
                }
            }
            None => return Ok(None),
        };
        self.cache_answer(cache_key, ans.clone());
        Ok(Some(ans))
    }

    fn ask_context(
        &self,
        d: &Decision,
        item: Option<&Item>,
        question: &str,
        options: &[String],
        a: &Ask,
    ) -> ApiResult<Context> {
        let db = self.core().store();
        let ws = d.workspace_id.as_str();
        let main = db
            .bots(ws)
            .map_err(ce)?
            .into_iter()
            .find(|b| b.main && b.enabled);
        let main_bot = match &main {
            Some(b) => self.core().bot_prompt(b, ws).map_err(ce)?,
            None => String::new(),
        };
        let mut cx = Context {
            question: question.to_string(),
            options: options.to_vec(),
            recommended: a.recommended.clone(),
            why: a.why.clone(),
            main_bot,
            ..Default::default()
        };
        if let Some(i) = item {
            cx.goal = db.goal(&i.id).map_err(ce)?;
            cx.plan = i
                .plan
                .iter()
                .map(|p| format!("{} {}", if p.done { "✓" } else { "·" }, p.step))
                .collect();
            if let Some(p) = db.project(i.project_id.as_deref()).map_err(ce)? {
                cx.project = format!("{} — {}", p.name, p.description);
            } else {
                let names: Vec<String> = db
                    .projects(ws)
                    .map_err(ce)?
                    .into_iter()
                    .map(|p| format!("{}（{}）", p.name, p.description))
                    .collect();
                if !names.is_empty() {
                    cx.project = format!("没有项目；用户的项目：{}", names.join("、"));
                }
            }
            let tl = db.timeline(&i.id).map_err(ce)?;
            let from = tl.len().saturating_sub(TIMELINE);
            cx.timeline = tl[from..]
                .iter()
                .map(|t| format!("{}：{}", t.actor, js_head(&t.text, 300)))
                .collect();
            cx.context = self.core().session_context(i);
        }
        if let Some(sid) = d.session_id.as_deref() {
            cx.session_tail = self.session_tail(sid)?;
        }
        Ok(cx)
    }

    /// The session's last [`TAIL`] characters of agent text.
    pub(crate) fn session_tail(&self, sid: &str) -> ApiResult<String> {
        let evs = self.core().store().events_after(sid, 0).map_err(ce)?;
        let text: String = evs
            .iter()
            .filter_map(|e| steve_transcript::assistant_text(&e.kind, &e.data))
            .collect();
        Ok(js_tail(text.trim(), TAIL).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_go_to_the_draft_and_back() {
        assert_eq!(
            hint_draft(Some(" A "), Some("更快")).as_deref(),
            Some("推荐：A\n理由：更快")
        );
        assert_eq!(hint_draft(Some("A"), None).as_deref(), Some("推荐：A"));
        assert_eq!(hint_draft(None, Some("x")), None);
        assert_eq!(
            parse_hint("推荐：A\n理由：更快"),
            (Some("A".into()), Some("更快".into()))
        );
        assert_eq!(parse_hint(""), (None, None));
    }

    #[test]
    fn replies_parse_strictly() {
        let opts = vec!["A".to_string(), "B".to_string()];
        let a = parse(
            "好的 {\"act\":\"answer\",\"answer\":\"用 A\",\"option\":\"A\",\"confidence\":0.9,\"reason\":\"目标写了\"}",
            &opts,
        )
        .unwrap();
        assert_eq!(
            (a.act.as_str(), a.option.as_deref(), a.confidence),
            ("answer", Some("A"), 0.9)
        );
        let b = parse(
            "{\"act\":\"ask_user\",\"answer\":\"\",\"option\":\"C\",\"confidence\":3,\"reason\":\"信息不足：不知道预算\"}",
            &opts,
        )
        .unwrap();
        assert_eq!(b.option, None);
        assert_eq!(b.confidence, 1.0);
        assert!(b.only_lacks_info);
        assert!(parse("不知道", &opts).is_none());
        assert!(parse("{\"act\":\"maybe\"}", &opts).is_none());
    }

    #[test]
    fn questions_compare_normalised_and_stalls_that_ask_are_spotted() {
        assert_eq!(normalize("  用  A 还是 B？ "), "用 a 还是 b");
        assert_eq!(normalize("Use A?"), normalize("use   a"));
        assert!(looks_like_question("做完了一半。\n你想用 A 还是 B？").is_some());
        assert!(looks_like_question("要不要继续").is_some());
        assert!(looks_like_question("已经全部完成。").is_none());
        assert!(looks_like_question("").is_none());
    }
}
