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
    /// work to offer after research or an answer (inline answer + clickable offer action)
    pub offer: Option<Offer>,
}

/// A guest conversation (an IM space): what the turn may become there. Configuration,
/// talking to items, corrections and the router's own clarifying questions never come from a chat
/// (→ an answer); research and items only
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
        // the router's clarifying question (and its options) is Steve asking the chat: the host's
        // composer answers in its own words instead (it may still ask, as a sentence)
        "ask" => ("answer", d.offer.clone()),
        "research" if !research => ("answer", d.offer.clone()),
        "item" if !items => (inline, offer()),
        _ => return d,
    };
    Decision {
        kind: kind.into(),
        guard: Some("guest"),
        offer,
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

pub fn is_config_phrase(text: &str) -> bool {
    let t = text.trim();
    CONFIG.is_match(t) && !explicit_item(t)
}

/// Short conversational greetings or acknowledgements that need only one answer generation
/// and zero router calls.
static OBVIOUS_REPLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(谢谢|谢谢你|多谢|感谢|好的|好呀|好滴|你好|您好|嗨|哈喽|收到|明白|明白了|了解了|知道了|辛苦了|早安|早上好|晚安|拜拜|再见|不客气|太好了|赞|棒|hi|hello|hey|thanks|thank you|thx|ok|okay|got it|bye)[。！!，,～~\.\s]*$",
    )
    .expect("valid regex")
});

pub fn obvious_reply(text: &str, files: bool) -> bool {
    !files && OBVIOUS_REPLY.is_match(text.trim())
}

/// Deterministic preflight for requests that require machine/local-inspection tools (disk, files,
/// logs, processes, ports, shell commands), so a tool-less answer call is never used as a probe.
static MACHINE_TOOL_NOUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)磁盘|硬盘|剩余空间|磁盘空间|内存|CPU|进程|端口|日志|目录|本地文件|文件|代码库|仓库|系统状态|终端|命令行|shell|stdout|stderr|\b(ls|df|ps|grep|git\s+(status|log|diff))\b")
        .expect("valid regex")
});

static MACHINE_TOOL_VERB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)看下|看一下|看看|查下|查一下|查看|检查|跑下|跑一下|运行|执行|读一下|读下|列一下|列出|统计一下")
        .expect("valid regex")
});

pub fn needs_machine_tools(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && MACHINE_TOOL_NOUN.is_match(t) && MACHINE_TOOL_VERB.is_match(t)
}

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

/// The message names a Bot or an item in so many words (`@Bot …`, “建个事项”, `ST-12`): the router
/// decides even where a conversation is otherwise not routed (the first meeting).
pub fn names_work(text: &str) -> bool {
    let t = text.trim();
    t.starts_with('@') || explicit_item(t) || ITEM_REF.is_match(t)
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

/// Apply the guards in order. Rule 6, the router failing, is already in `route.fallback`.
pub fn decide(r: &Route, text: &str, cx: DecideCtx) -> Decision {
    let model = r.kind.as_str();
    let d = |kind: &str, guard: Option<&'static str>| Decision {
        kind: kind.into(),
        guard: guard.filter(|_| kind != model),
        offer: r.offer.clone(),
    };
    let work = || Offer {
        title: r.title.clone(),
        goal: if r.goal.trim().is_empty() {
            r.title.clone()
        } else {
            r.goal.clone()
        },
    };
    let work_offer = || {
        r.offer
            .clone()
            .or_else(|| (!r.title.trim().is_empty()).then(work))
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
    if explicit_item(text) && matches!(model, "item" | "answer" | "research" | "supplement") {
        return d("item", Some("2"));
    }
    if model == "item" {
        /* 3: an item needs a work signal and confidence; otherwise answer inline with an offer */
        let worky = r.signals.iter().any(|s| WORK.contains(&s.as_str()));
        if !worky || r.confidence < 0.75 {
            return Decision {
                offer: work_offer(),
                ..d("research", Some("3"))
            };
        }
        /* 4: a pure question is answered here; the item the model saw becomes the offer */
        if pure_question(text) {
            return Decision {
                offer: r.offer.clone().or_else(|| Some(work())),
                ..d("research", Some("4"))
            };
        }
    }
    /* 5: an unsure supplement stays inline with a clickable offer rather than popping a 2-choice card */
    if model == "supplement" && r.confidence < 0.8 {
        return Decision {
            offer: work_offer(),
            ..d("research", Some("5"))
        };
    }
    d(model, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obvious_replies_and_machine_tool_preflight() {
        assert!(obvious_reply("谢谢", false));
        assert!(obvious_reply("谢谢！", false));
        assert!(obvious_reply("hello", false));
        assert!(!obvious_reply("谢谢", true));
        assert!(!obvious_reply("谢谢，顺便帮我改个按钮", false));
        assert!(needs_machine_tools("帮我看下磁盘还剩多少"));
        assert!(needs_machine_tools("看下我现在磁盘情况"));
        assert!(needs_machine_tools("查一下服务日志"));
        assert!(!needs_machine_tools("在吗"));
        assert!(!needs_machine_tools("谢谢"));
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
        assert_eq!(
            go(
                "supplement",
                0.95,
                &[],
                "ST-3 开了 issues，请给每个 issue 建一个事项"
            ),
            ("item".into(), Some("2"))
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
        let d1 = decide(
            &route("item", 0.9, &["vague"]),
            "事项页面信息太散了",
            DecideCtx::default(),
        );
        assert_eq!((d1.kind.as_str(), d1.guard), ("research", Some("3")));
        assert_eq!(
            d1.offer,
            Some(Offer {
                title: "标题".into(),
                goal: "目标".into()
            })
        );
        let d2 = decide(
            &route("item", 0.6, &["change"]),
            "事项页面信息太散了",
            DecideCtx::default(),
        );
        assert_eq!((d2.kind.as_str(), d2.guard), ("research", Some("3")));
        assert_eq!(
            d2.offer,
            Some(Offer {
                title: "标题".into(),
                goal: "目标".into()
            })
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
    fn rule_5_an_unsure_supplement_answers_inline_with_an_offer() {
        let d = decide(
            &route("supplement", 0.6, &[]),
            "先别改列表页",
            DecideCtx::default(),
        );
        assert_eq!((d.kind.as_str(), d.guard), ("research", Some("5")));
        assert_eq!(
            d.offer,
            Some(Offer {
                title: "标题".into(),
                goal: "目标".into()
            })
        );
        assert_eq!(
            go("supplement", 0.85, &[], "先别改列表页"),
            ("supplement".into(), None)
        );
        /* Even an id with explicit-looking wording cannot override the router's uncertainty. */
        for t in [
            "追加到 ST-3：统计按小时",
            "这个也给st3加上",
            "#st-3 再补一条",
        ] {
            assert_eq!(
                go("supplement", 0.5, &[], t),
                ("research".into(), Some("5")),
                "{t}"
            );
        }
        for t in ["追加到 ST-30", "追加到 ST-4", "TEST-3 那边也改"] {
            assert_eq!(go("supplement", 0.5, &[], t).0, "research", "{t}");
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
        for k in ["answer", "research", "ask", "status", "config", "reroute"] {
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
    fn guest_conversations_never_get_the_routers_question() {
        let r = route("ask", 0.9, &["question"]);
        let d = for_guest(decide(&r, "x", DecideCtx::default()), &r, true, true);
        assert_eq!(d.kind, "answer");
        assert_eq!(d.guard, Some("guest"));
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
