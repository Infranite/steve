//! The code-side guards on the router: the model proposes, these rules have the last
//! word, so a wrong model never quietly turns a question into an item. Pure functions, table-tested.

use crate::gateway::{FocusItem, Offer, Route};
use regex::Regex;
use std::sync::LazyLock;

/// What the message was decided to be, and which rule (if any) changed the model's kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub kind: String,
    /// the rule that changed the kind: "1".."6"; `None` when the model's kind stands
    pub guard: Option<&'static str>,
    /// work to offer after research or an answer (rule 4 keeps the model's item here)
    pub offer: Option<Offer>,
    /// a suggestion card about an existing item (rule 5: “……建议追加到 ST-n，追加还是单独建项？”)
    pub for_item: Option<String>,
}

/// A guest conversation (an IM space): what the turn may become there. Configuration,
/// talking to items and corrections never come from a chat (→ an answer); research and items only
/// when the host allows them — otherwise work is answered in place and offered to you instead
/// (`guard = "guest"` when this changed the kind).
pub fn for_guest(d: Decision, r: &Route, research: bool, items: bool) -> Decision {
    let offer = || {
        d.offer.clone().or_else(|| {
            (!r.title.trim().is_empty()).then(|| Offer {
                title: r.title.clone(),
                goal: if r.goal.trim().is_empty() {
                    r.title.clone()
                } else {
                    r.goal.clone()
                },
            })
        })
    };
    let inline = if research { "research" } else { "answer" };
    let (kind, offer) = match d.kind.as_str() {
        "config" | "supplement" | "status" | "reroute" => ("answer", d.offer.clone()),
        // “追加到 ST-n？” would name an item to the chat
        "suggest" if d.for_item.is_some() => ("answer", None),
        "research" if !research => ("answer", d.offer.clone()),
        "item" | "suggest" if !items => (inline, offer()),
        _ => return d,
    };
    Decision {
        kind: kind.into(),
        guard: Some("guest"),
        offer,
        for_item: None,
    }
}

/// A chat only the owner reads: no tool-less answer there, Steve answers with its read-only tools at hand
/// (an answer could only say what it can't see; the run costs about the same). Decided by the user on
/// 2026-11-03 (`guard = "owner"`).
pub fn for_owner(d: Decision, owner: bool) -> Decision {
    if !owner || d.kind != "answer" {
        return d;
    }
    Decision {
        kind: "research".into(),
        guard: Some("owner"),
        ..d
    }
}

/// Your own conversation: work that is not clearly an item is answered in place with a plan, and
/// offered as an item — Steve no longer asks “在这里出方案，还是建成事项？” first. Decided by the user
/// on 2026-11-04 (ST-33, `guard = "plan"`). “追加到 ST-n，还是单独建项？” still asks: it is about which item.
pub fn for_main(d: Decision, r: &Route) -> Decision {
    if d.kind != "suggest" || d.for_item.is_some() {
        return d;
    }
    let offer = d.offer.clone().or_else(|| {
        (!r.title.trim().is_empty()).then(|| Offer {
            title: r.title.clone(),
            goal: if r.goal.trim().is_empty() {
                r.title.clone()
            } else {
                r.goal.clone()
            },
        })
    });
    Decision {
        kind: "research".into(),
        guard: Some("plan"),
        offer,
        for_item: None,
    }
}

/// What the guards know besides the route and the text.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecideCtx {
    /// the message starts with “@Bot” naming an enabled bot
    pub named_bot: bool,
}

/// Explicit requests for an item (rule 2).
static EXPLICIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(建|开|做|记|整理)(个|一个|成|为)(新)?事项|交给.{0,16}(做|处理|去办|来做)|跟进一下|记下来跟进|放(到)?后台(做|跑)|后台去做")
        .expect("valid regex")
});
/// Words of a question (rule 4).
static QUESTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"怎么|为什么|为啥|是什么|是不是|有没有|讲一下|讲讲|介绍|解释|看一下|看下|查一下|查查",
    )
    .expect("valid regex")
});
/// Change verbs: a question with one of them is also a request (rule 4).
static CHANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"改|修|加|删|实现|重构|换成|发布|上线|写一份").expect("valid regex")
});

/// The model's “this is work” signals (rule 3).
const WORK: [&str; 4] = ["explicit", "change", "deliverable", "long"];

/// The user explicitly asks for an item.
pub fn explicit_item(text: &str) -> bool {
    EXPLICIT.is_match(text)
}

/// The message names item `id` itself (“追加到 ST-40”, “st40 也加上”): case, “-” and “#” ignored, and
/// “ST-4” is not named by “ST-40”.
pub fn names_item(text: &str, id: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '-' | '#' | ' '))
            .flat_map(char::to_uppercase)
            .collect()
    };
    let (t, id) = (norm(text), norm(id));
    !id.is_empty()
        && t.match_indices(&id).any(|(at, _)| {
            !t[at + id.len()..].starts_with(|c: char| c.is_ascii_digit())
                && !t[..at].ends_with(|c: char| c.is_ascii_alphabetic())
        })
}

/// A pure question: ends with ？/吗/呢 or asks with a question word, and asks for no change.
pub fn pure_question(text: &str) -> bool {
    let t = text.trim().trim_end_matches(|c: char| {
        c.is_whitespace() || matches!(c, '。' | '.' | '!' | '！' | '~' | '～')
    });
    let asks = t.ends_with(['？', '?', '吗', '呢']) || QUESTION.is_match(t);
    asks && !CHANGE.is_match(t)
}

/// An item number anywhere in the text (`ST-12`, `st-3`).
static ITEM_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|[^A-Za-z])[A-Z]{1,6}-\d+").expect("valid regex"));

/// Words of Steve's own settings: a question about them gets the settings as they are (router kind
/// `config`), not research.
static CONFIG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)设置|配置|开关|免打扰|提醒|通知|汇报|yolo|模型|权限|规则|默认|偏好|定时|Bot|机器人",
    )
    .expect("valid regex")
});

/// Asking Steve to do something, even as a question (“给我讲一下”, “你看看怎么设计”): the router may see
/// work in it and offer it as an item.
static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"帮我|给我|替我|你看看|你看下|你来|需要|优化|设计|整理|方案|做个|弄个|搞个|写个|出个|安排",
    )
    .expect("valid regex")
});

/// What a message must not have for the router to be skipped: attachments, `@Bot`, explicit item words,
/// an item number, a progress question, a spoken correction (“别建事项…”), words of a setting, a request,
/// or an open item of the conversation it names (its number, or its title of 4+ characters).
fn needs_router(text: &str, files: bool, focus: &[FocusItem]) -> bool {
    let t = text.trim();
    files
        || t.starts_with('@')
        || explicit_item(t)
        || ITEM_REF.is_match(t)
        || super::handle::PROGRESS.is_match(t)
        || super::handle::TO_INLINE.is_match(t)
        || CONFIG.is_match(t)
        || REQUEST.is_match(t)
        || focus.iter().any(|f| {
            names_item(t, &f.id)
                || (f.title.trim().chars().count() >= 4 && t.contains(f.title.trim()))
        })
}

/// The longest guest question (in characters) answered without the router: “短句” in the decision. A
/// longer one goes to the router, which may still pick research where the space allows it.
pub const GUEST_SHORT: usize = 30;

/// #52: the kind a message gets without the router (`route_log.guard = "q"`) when it is a plain question
/// ([`pure_question`], nothing in it the router would act on): researched in your own conversation,
/// answered in a guest one when it is short ([`GUEST_SHORT`]; `for_owner` still researches the owner's
/// private chat). `None`: the router decides. Decided by the user on 2026-11-06.
pub fn without_router(
    text: &str,
    files: bool,
    guest: bool,
    focus: &[FocusItem],
) -> Option<&'static str> {
    if text.trim().is_empty() || needs_router(text, files, focus) || !pure_question(text) {
        return None;
    }
    if !guest {
        return Some("research");
    }
    (text.trim().chars().count() <= GUEST_SHORT).then_some("answer")
}

/// The least confidence of a message handled while Steve is still answering earlier ones ([`now_while_answering`]).
pub const AHEAD_MIN: f64 = 0.8;

/// While the main conversation is still answering earlier messages: whether this one is handled now,
/// ahead of those answers, rather than after them. Only what stands on its own and has a clear object —
/// an instruction for an item, new work, a status question, a setting — the router sure of it and
/// saying it does not need the answers ([`Route::after`]). Anything else waits, as every message did
/// before: waiting only delays, while going ahead without an answer it refers to would act on a guess.
/// Decided by the user on 2026-11-09.
pub fn now_while_answering(r: &Route, d: &Decision) -> bool {
    !r.fallback
        && !r.after
        && r.confidence >= AHEAD_MIN
        && matches!(d.kind.as_str(), "item" | "supplement" | "status" | "config")
}

/// Apply the guards in order. Rule 6, the router failing, is already in `route.fallback`.
pub fn decide(r: &Route, text: &str, cx: DecideCtx) -> Decision {
    let model = r.kind.as_str();
    let d = |kind: &str, guard: Option<&'static str>| Decision {
        kind: kind.into(),
        guard: guard.filter(|_| kind != model),
        offer: r.offer.clone(),
        for_item: None,
    };
    let work = || Offer {
        title: r.title.clone(),
        goal: r.goal.clone(),
    };
    /* 6: no usable answer from the router — answer here, never an item */
    if r.fallback {
        return Decision {
            guard: Some("6"),
            ..d(model, None)
        };
    }
    /* 1: “@Bot …” names who does it */
    if cx.named_bot {
        return d("item", Some("1"));
    }
    /* 2: the user asked for an item in so many words */
    if explicit_item(text) && matches!(model, "item" | "answer" | "research" | "suggest") {
        return d("item", Some("2"));
    }
    if model == "item" {
        /* 3: an item needs a work signal and confidence; otherwise ask with a card */
        let worky = r.signals.iter().any(|s| WORK.contains(&s.as_str()));
        if !worky || r.confidence < 0.75 {
            return d("suggest", Some("3"));
        }
        /* 4: a pure question is answered here; the item the model saw becomes the offer */
        if pure_question(text) {
            return Decision {
                offer: r.offer.clone().or_else(|| Some(work())),
                ..d("research", Some("4"))
            };
        }
    }
    /* 5: a supplement the model is not sure about becomes a card “追加到 ST-n，还是单独建项？” — unless
    the user named the item: then it is theirs to say, and asked already */
    if model == "supplement"
        && r.confidence < 0.8
        && !r.item.as_deref().is_some_and(|i| names_item(text, i))
    {
        return Decision {
            for_item: r.item.clone(),
            ..d("suggest", Some("5"))
        };
    }
    d(model, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sure_standalone_messages_go_ahead_of_an_answer() {
        let r = |kind: &str, confidence: f64, after: bool| Route {
            kind: kind.into(),
            confidence,
            after,
            signals: vec!["change".into()],
            item: Some("ST-1".into()),
            ..Route::default()
        };
        let go = |r: &Route| {
            let d = decide(r, "x", DecideCtx { named_bot: false });
            now_while_answering(r, &for_main(d, r))
        };
        for k in ["item", "supplement", "status", "config"] {
            assert!(go(&r(k, 0.9, false)), "{k}");
            assert!(!go(&r(k, 0.9, true)), "{k} needs the answer");
            assert!(!go(&r(k, 0.7, false)), "{k} not sure");
        }
        for k in ["answer", "research", "suggest", "ask", "reroute"] {
            assert!(!go(&r(k, 1.0, false)), "{k}");
        }
        let failed = Route {
            fallback: true,
            ..r("item", 1.0, false)
        };
        assert!(!go(&failed));
    }

    #[test]
    fn plain_questions_skip_the_router() {
        let focus = [FocusItem {
            id: "ST-40".into(),
            title: "事项页面改版".into(),
            ..FocusItem::default()
        }];
        let go = |t: &str, files: bool, guest: bool| without_router(t, files, guest, &focus);
        assert_eq!(go("检索是怎么打分的？", false, false), Some("research"));
        assert_eq!(
            go("现在 steve 的知识系统是怎么运作的", false, false),
            Some("research")
        );
        // the router still decides
        for t in [
            "把登录页的按钮改成蓝色",
            "帮我建个事项：检索怎么打分？",
            "@运维 部署是怎么回事？",
            "ST-12 为什么停了？",
            "st-3 是什么情况？",
            "那个事项进展怎么样了？",
            "别建事项，直接在这告诉我为什么？",
            "事项页面改版做到哪一步了？",
            "st40 为什么还没好？",
            "现在免打扰是几点？",
            "YOLO 现在怎么设置的？",
            "现在steve的知识系统是怎么运作的？给我讲一下",
            "事项页面信息太散了，得优化一下，你看看怎么设计一下",
            "",
        ] {
            assert_eq!(go(t, false, false), None, "{t}");
        }
        assert_eq!(go("这张图是什么？", true, false), None, "attachments");
        // a guest's plain question is answered; anything else goes to the router
        assert_eq!(go("为什么周四的会议推迟了？", false, true), Some("answer"));
        assert_eq!(go("@Steve 为什么推迟了？", false, true), None);
        // a long guest question is not a “短句”: the router decides (research where the space allows it)
        let long = "为什么我们上周把订单服务的数据库从 MySQL 迁到 PostgreSQL 之后，夜间批处理的耗时反而变长了一倍多？";
        assert!(long.chars().count() > GUEST_SHORT);
        assert_eq!(go(long, false, true), None, "long guest question");
        assert_eq!(
            go(long, false, false),
            Some("research"),
            "owner: any length"
        );
        assert_eq!(go("把告警脚本修一下", false, true), None);
        assert_eq!(go("以后都直接回滚吧", false, true), None);
        assert_eq!(go("谢谢", false, false), None, "not a question");
    }

    fn route(kind: &str, conf: f64, signals: &[&str]) -> Route {
        Route {
            kind: kind.into(),
            confidence: conf,
            signals: signals.iter().map(|s| s.to_string()).collect(),
            title: "标题".into(),
            goal: "目标".into(),
            item: Some("ST-3".into()),
            ..Route::default()
        }
    }
    fn go(kind: &str, conf: f64, signals: &[&str], text: &str) -> (String, Option<&'static str>) {
        let d = decide(&route(kind, conf, signals), text, DecideCtx::default());
        (d.kind, d.guard)
    }

    #[test]
    fn your_conversation_plans_instead_of_asking() {
        for (kind, conf, signals) in [("suggest", 0.9, vec![]), ("item", 0.6, vec!["change"])] {
            let r = route(kind, conf, &signals);
            let d = for_main(
                decide(
                    &r,
                    "事项页面信息太散了，你看看怎么设计",
                    DecideCtx::default(),
                ),
                &r,
            );
            assert_eq!(
                (d.kind.as_str(), d.guard),
                ("research", Some("plan")),
                "{kind}"
            );
            let o = d.offer.expect("the work is offered");
            assert_eq!((o.title.as_str(), o.goal.as_str()), ("标题", "目标"));
        }
        /* which item it is for still asks */
        let r = route("supplement", 0.5, &[]);
        let d = for_main(decide(&r, "再加一个按钮", DecideCtx::default()), &r);
        assert_eq!(
            (d.kind.as_str(), d.for_item.as_deref()),
            ("suggest", Some("ST-3"))
        );
        /* the rest is left alone */
        let r = route("item", 0.9, &["change"]);
        assert_eq!(
            for_main(decide(&r, "把按钮改成蓝色", DecideCtx::default()), &r).kind,
            "item"
        );
    }

    #[test]
    fn rule_1_a_named_bot_makes_an_item() {
        let at = DecideCtx { named_bot: true };
        let d = decide(&route("answer", 0.9, &[]), "@研究 查下价格", at);
        assert_eq!((d.kind.as_str(), d.guard), ("item", Some("1")));
        let d = decide(&route("item", 0.9, &["explicit"]), "@研究 查下价格", at);
        assert_eq!(
            (d.kind.as_str(), d.guard),
            ("item", None),
            "unchanged: no guard"
        );
        assert_eq!(
            go("answer", 0.9, &[], "@ 是什么符号"),
            ("answer".into(), None)
        );
    }

    #[test]
    fn rule_2_explicit_words_allow_an_item() {
        for t in [
            "这个建个事项吧",
            "做成事项",
            "记成事项，回头再说",
            "交给开发 Bot 去做",
            "跟进一下",
            "放后台做吧",
            "整理成事项",
        ] {
            assert_eq!(
                go("research", 0.4, &[], t),
                ("item".into(), Some("2")),
                "{t}"
            );
        }
        /* even a question, if it explicitly asks for an item */
        assert_eq!(
            go("item", 0.3, &[], "为什么慢？建个事项查一下"),
            ("item".into(), None)
        );
        for t in ["事项是什么", "交给你了吗", "我想跟进度"] {
            assert!(!explicit_item(t), "{t}");
        }
        /* config and supplement are not overridden */
        assert_eq!(
            go("config", 0.9, &[], "以后都建个事项"),
            ("config".into(), None)
        );
    }

    #[test]
    fn rule_3_an_item_needs_a_work_signal_and_confidence() {
        assert_eq!(
            go("item", 0.9, &["vague"], "事项页面信息太散了"),
            ("suggest".into(), Some("3"))
        );
        assert_eq!(
            go("item", 0.6, &["change"], "事项页面信息太散了"),
            ("suggest".into(), Some("3"))
        );
        assert_eq!(
            go("item", 0.8, &["change"], "把菜单里的视觉规范页面删掉"),
            ("item".into(), None)
        );
        assert_eq!(
            go("item", 0.75, &["deliverable"], "先给我一个方案吧"),
            ("item".into(), None)
        );
    }

    #[test]
    fn rule_4_a_pure_question_is_researched() {
        let r = route("item", 0.95, &["deliverable"]);
        let d = decide(
            &r,
            "现在steve的知识系统是怎么运作的？给我讲一下",
            DecideCtx::default(),
        );
        assert_eq!((d.kind.as_str(), d.guard), ("research", Some("4")));
        assert_eq!(
            d.offer,
            Some(Offer {
                title: "标题".into(),
                goal: "目标".into()
            })
        );
        for t in [
            "这对agent感知有影响吗？",
            "为什么这么慢",
            "看下st15这个评论信息",
            "这是什么呢",
            "你现在有能力维护代码了吗",
        ] {
            assert!(pure_question(t), "{t}");
        }
        for t in [
            "帮我把 X 改成 Y",
            "为什么不能删？修一下",
            "怎么设计和实现，先写一份方案",
            "把菜单删掉",
            "steve 现在不会给事项关联项目，这与预期不符",
        ] {
            assert!(!pure_question(t), "{t}");
        }
        assert_eq!(
            go("item", 0.95, &["change"], "帮我把 X 改成 Y"),
            ("item".into(), None)
        );
        /* research stays research */
        assert_eq!(
            go("research", 0.5, &[], "为什么"),
            ("research".into(), None)
        );
    }

    #[test]
    fn rule_5_an_unsure_supplement_asks() {
        let d = decide(
            &route("supplement", 0.6, &[]),
            "先别改列表页",
            DecideCtx::default(),
        );
        assert_eq!((d.kind.as_str(), d.guard), ("suggest", Some("5")));
        assert_eq!(d.for_item.as_deref(), Some("ST-3"));
        assert_eq!(
            go("supplement", 0.85, &[], "先别改列表页"),
            ("supplement".into(), None)
        );
        /* the user named the item: no asking again */
        for t in [
            "追加到 ST-3：统计按小时",
            "这个也给st3加上",
            "#st-3 再补一条",
        ] {
            assert_eq!(
                go("supplement", 0.5, &[], t),
                ("supplement".into(), None),
                "{t}"
            );
        }
        /* another item, or one whose id only starts the same, still asks */
        for t in ["追加到 ST-30", "追加到 ST-4", "TEST-3 那边也改"] {
            assert_eq!(go("supplement", 0.5, &[], t).0, "suggest", "{t}");
        }
    }

    #[test]
    fn rule_6_a_failed_router_never_makes_an_item() {
        let r = Route::fallback("给我讲一下", Some("prj".into()));
        let d = decide(&r, "给我讲一下", DecideCtx::default());
        assert_eq!((d.kind.as_str(), d.guard), ("research", Some("6")));
        let d = decide(
            &Route::fallback("建个事项", None),
            "建个事项",
            DecideCtx::default(),
        );
        assert_eq!(
            d.kind, "answer",
            "even explicit words wait for a working router"
        );
    }

    #[test]
    fn other_kinds_pass_through() {
        for k in [
            "answer", "research", "suggest", "ask", "status", "config", "reroute",
        ] {
            assert_eq!(go(k, 0.5, &[], "随便说说"), (k.into(), None));
        }
    }

    #[test]
    fn guest_conversations_never_configure_or_talk_to_items() {
        for k in ["config", "supplement", "status", "reroute"] {
            let r = route(k, 0.9, &["question"]);
            let d = for_guest(decide(&r, "x", DecideCtx::default()), &r, true, true);
            assert_eq!(d.kind, "answer", "{k}");
            assert_eq!(d.guard, Some("guest"));
        }
    }

    #[test]
    fn the_owners_chat_answers_with_tools() {
        let r = route("answer", 0.9, &[]);
        let d = decide(&r, "你能看见我电脑在干嘛吗", DecideCtx::default());
        assert_eq!(for_owner(d.clone(), true).kind, "research");
        assert_eq!(for_owner(d.clone(), true).guard, Some("owner"));
        assert_eq!(for_owner(d, false).kind, "answer");
        let item = decide(
            &route("item", 0.9, &["explicit"]),
            "建个事项",
            DecideCtx::default(),
        );
        assert_eq!(for_owner(item, true).kind, "item");
    }

    #[test]
    fn guest_items_and_research_follow_the_host() {
        let mut r = route("item", 0.95, &["explicit", "change"]);
        r.title = "修评论排版".into();
        r.goal = "评论支持 markdown".into();
        let d = decide(&r, "把评论排版修一下", DecideCtx::default());
        assert_eq!(d.kind, "item");
        // allowed: unchanged
        assert_eq!(for_guest(d.clone(), &r, true, true).kind, "item");
        // no items: answered in place (research when allowed), the work offered instead
        let g = for_guest(d.clone(), &r, true, false);
        assert_eq!(g.kind, "research");
        assert_eq!(
            g.offer.as_ref().map(|o| o.title.as_str()),
            Some("修评论排版")
        );
        let g = for_guest(d, &r, false, false);
        assert_eq!(g.kind, "answer");
        let rr = route("research", 0.9, &["lookup"]);
        let d = decide(&rr, "看一下", DecideCtx::default());
        assert_eq!(for_guest(d.clone(), &rr, false, false).kind, "answer");
        assert_eq!(for_guest(d, &rr, true, false).kind, "research");
    }
}
