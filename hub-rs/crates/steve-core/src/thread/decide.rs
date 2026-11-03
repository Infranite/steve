//! The code-side guards on the router (thread-routing §3.3): the model proposes, these rules have the last
//! word, so a wrong model never quietly turns a question into an item. Pure functions, table-tested.

use crate::gateway::{Offer, Route};
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
    /// a suggestion card about an existing item (rule 5: “这是给 ST-n 的吗？”)
    pub for_item: Option<String>,
}

/// A guest conversation (an IM space, channels §2.12): what the turn may become there. Configuration,
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
        // “这是给 ST-n 的吗？” would name an item to the chat
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

/// A pure question: ends with ？/吗/呢 or asks with a question word, and asks for no change.
pub fn pure_question(text: &str) -> bool {
    let t = text.trim().trim_end_matches(|c: char| {
        c.is_whitespace() || matches!(c, '。' | '.' | '!' | '！' | '~' | '～')
    });
    let asks = t.ends_with(['？', '?', '吗', '呢']) || QUESTION.is_match(t);
    asks && !CHANGE.is_match(t)
}

/// Apply the guards in order (§3.3). Rule 6, the router failing, is already in `route.fallback`.
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
    /* 5: a supplement the model is not sure about becomes a card “这是给 ST-n 的吗？” */
    if model == "supplement" && r.confidence < 0.8 {
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
