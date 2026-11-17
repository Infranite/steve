//! The reply eval's deterministic part on the synthetic fixtures (`fixtures/reply_eval/`): twelve
//! anonymized cases, one per pattern seen on owner-voice replies (`pattern`), with their labels and a
//! style corpus. What a real model answers is measured offline (`examples/reply_eval.rs`); this
//! checks what the replay builds and how the checker scores.

use std::collections::{BTreeMap, HashSet};

use steve_channels::compose::{
    self, BotInfo, H_BOT, H_HISTORY, H_OCCASION, H_OWNER, H_PENDING, H_STYLE, H_WRITE, HISTORY,
};
use steve_channels::context::Shown;
use steve_channels::deliver::{SYSTEM_TEXTS, system_text_in};
use steve_channels::reply_eval::{
    self as re, Case, CheckParams, Label, Output, OwnerLine, Setting, Situation,
};

const CASES: &str = include_str!("fixtures/reply_eval/cases.jsonl");
const LABELS: &str = include_str!("fixtures/reply_eval/labels.jsonl");
const CORPUS: &str = include_str!("fixtures/reply_eval/owner_corpus.jsonl");

fn cases() -> Vec<Case> {
    re::read_jsonl(CASES).expect("cases")
}
fn labels() -> Vec<Label> {
    re::read_jsonl(LABELS).expect("labels")
}
fn corpus() -> Vec<OwnerLine> {
    re::read_jsonl(CORPUS).expect("corpus")
}
fn case(id: &str) -> Case {
    cases()
        .into_iter()
        .find(|c| c.case_id == id)
        .unwrap_or_else(|| panic!("no case {id}"))
}

fn setting() -> Setting {
    Setting {
        owner: "主人".into(),
        ..Default::default()
    }
}

fn system() -> Vec<String> {
    SYSTEM_TEXTS
        .iter()
        .chain([steve_core::thread::research::QUEUE_FULL].iter())
        .map(|s| s.to_string())
        .collect()
}

fn params() -> CheckParams {
    CheckParams {
        names: vec!["主人".into()],
        system: system(),
        live_secs: re::LIVE_WINDOW_SECS,
        latency_secs: None,
    }
}

fn names() -> Vec<String> {
    vec!["主人".into()]
}

#[test]
fn the_fixtures_cover_twelve_patterns_with_a_label_each() {
    let cs = cases();
    assert_eq!(cs.len(), 12);
    let raw: Vec<serde_json::Value> = re::read_jsonl(CASES).unwrap();
    let patterns: HashSet<&str> = raw.iter().filter_map(|c| c["pattern"].as_str()).collect();
    assert_eq!(patterns.len(), 12, "{patterns:?}");
    let labelled: HashSet<String> = labels().into_iter().map(|l| l.case_id).collect();
    assert!(cs.iter().all(|c| labelled.contains(&c.case_id)));
    // some cases call for acting, so skipping everything cannot pass
    assert!(labels().iter().any(|l| l.correct_outcome != "skip"));
}

/// The rendered lines of a history.
fn rendered(xs: &[Shown]) -> Vec<String> {
    xs.iter().map(Shown::render).collect()
}

#[test]
fn the_layers_are_the_composers_at_the_cases_time() {
    let c = case("fx-03-owner-replied-after");
    let l = re::layers(&c, &setting(), HISTORY, re::bare_voice(&c));
    assert_eq!(l.persona, compose::self_layer("主人"));
    assert_eq!(l.sender, "B");
    // the history: what was there before the message, received or sent, with its text — the
    // provider's placeholder as a typed note
    assert_eq!(l.history.len(), 1);
    assert!(
        rendered(&l.history)[0].ends_with("B：[图片：未读取内容]"),
        "{:?}",
        rendered(&l.history)
    );
    let p = compose::prompt(&l);
    assert!(p.contains(H_HISTORY) && p.contains(H_OCCASION) && p.contains(H_OWNER));
    assert!(p.contains("私聊：B 和我一对一。"), "{p}");
    // no style block unless the replay gives samples: the production prompt is unchanged
    assert!(!p.contains(H_STYLE));

    // drafts, failed sends and Steve's own lines are not in the history, as in the store's query;
    // a draft still waiting is listed apart
    let mut c = case("fx-02-dm-burst-owner-live");
    let mut draft = c.previous_space_messages[0].clone();
    draft.status = "draft".into();
    draft.is_steve_sent = true;
    draft.is_owner = false;
    draft.text = Some("好。".into());
    c.previous_space_messages.push(draft);
    let l = re::layers(&c, &setting(), HISTORY, re::bare_voice(&c));
    assert!(
        rendered(&l.history).iter().all(|t| !t.ends_with("好。")),
        "{:?}",
        rendered(&l.history)
    );
    assert_eq!(l.history.len(), 3);
    assert_eq!(l.pending.len(), 1);
    assert!(l.pending[0].ends_with("好。"));
    assert!(compose::prompt(&l).contains(H_PENDING));
    // only the last `history` lines by people
    assert_eq!(
        re::layers(&c, &setting(), 2, re::bare_voice(&c))
            .history
            .len(),
        2
    );
}

#[test]
fn an_unread_image_is_said_to_be_unread() {
    let mut c = case("fx-03-owner-replied-after");
    c.message.media = vec!["image".into()];
    let l = re::layers(&c, &setting(), HISTORY, re::bare_voice(&c));
    assert!(l.message.ends_with("[图片：未读取内容]"), "{}", l.message);
    assert!(compose::prompt(&l).contains("不要猜里面是什么"));
}

/// Pattern 12: your account's display name is a number. Your lines read as you, by the case's flags
/// (in production: by your account and contact ids), never by that name; ids are not among your
/// names.
#[test]
fn your_lines_read_as_you_whatever_your_display_name() {
    let mut c = case("fx-12-numeric-owner-name");
    for l in c.previous_space_messages.iter_mut().filter(|l| l.is_owner) {
        l.sender = "7300000000000000001".into();
    }
    let s = Setting {
        aliases: vec!["7300000000000000001".into(), "Lee".into()],
        ..setting()
    };
    let l = re::layers(&c, &s, HISTORY, re::bare_voice(&c));
    let lines = rendered(&l.history);
    assert!(
        lines.iter().any(|t| t.contains("主人（我）：")),
        "{lines:?}"
    );
    let p = compose::prompt(&l);
    assert!(!p.contains("730000000"), "{p}");
    assert!(
        l.owner_block.starts_with("我在这里的名字：主人、Lee。"),
        "{}",
        l.owner_block
    );
}

#[test]
fn style_samples_come_before_the_case_and_never_from_any_gold_answer() {
    let cs = cases();
    let gold = re::gold_ids(&cs);
    let gold_texts: HashSet<String> = cs
        .iter()
        .flat_map(|c| c.gold_owner_messages_60min.iter())
        .filter_map(|g| g.text.clone())
        .collect();
    let corpus = corpus();
    let people = re::people(&cs, &corpus);
    let at: BTreeMap<&str, &str> = corpus
        .iter()
        .map(|o| (o.text.as_str(), o.time.as_str()))
        .collect();
    for c in &cs {
        let s = re::style_samples(&corpus, c, &gold, &people, re::STYLE_MAX);
        assert!(s.len() <= re::STYLE_MAX);
        let target = re::millis(&c.message.time).unwrap();
        for t in &s {
            assert!(
                !gold_texts.contains(t),
                "{}: gold answer {t} used",
                c.case_id
            );
            let when = re::millis(at[t.as_str()]).unwrap();
            assert!(
                when < target,
                "{}: {t} is not before the message",
                c.case_id
            );
            assert!(!t.contains("http"), "{t}");
        }
    }
    // the last case sees every earlier message but the gold answers (several of them earlier still)
    let late = case("fx-12-numeric-owner-name");
    let s = re::style_samples(&corpus, &late, &gold, &people, 50);
    assert!(!s.iter().any(|t| t == "两种都行" || t == "先放着"), "{s:?}");
    // another space's sample only when it is plain: no digits, no @
    assert!(!s.iter().any(|t| t.contains("8080")), "{s:?}");
    assert!(!s.iter().any(|t| t.contains('@')), "{s:?}");
    // the space's own come first
    let d2 = case("fx-02-dm-burst-owner-live");
    let s = re::style_samples(&corpus, &d2, &gold, &people, 3);
    assert_eq!(s[0], "8080 那个先别管", "{s:?}");

    // the block appears with the samples and habits, and only then
    let v = re::voice(&corpus, &d2, &gold, &names(), &people);
    assert_eq!(v.samples[0], "8080 那个先别管", "{:?}", v.samples);
    let l = re::layers(&d2, &setting(), HISTORY, v);
    let p = compose::prompt(&l);
    assert!(
        p.contains(H_STYLE) && p.contains("- 8080 那个先别管"),
        "{p}"
    );
}

#[test]
fn presence_is_read_from_the_timeline() {
    let cs = cases();
    let lat = re::median_latency(&cs).expect("latencies");
    let present: Vec<&str> = cs
        .iter()
        .filter(|c| {
            re::presence(
                c,
                re::observed_latency(c).unwrap_or(lat),
                re::LIVE_WINDOW_SECS,
            )
            .any()
        })
        .map(|c| c.case_id.as_str())
        .collect();
    assert_eq!(
        present,
        vec![
            "fx-01-joke-on-own-line",
            "fx-02-dm-burst-owner-live",
            "fx-03-owner-replied-after",
            "fx-04-router-ask-bypass",
            "fx-12-numeric-owner-name",
        ]
    );
    // answered before Steve's draft came, without being there before the message
    let c = case("fx-03-owner-replied-after");
    let p = re::presence(&c, re::observed_latency(&c).unwrap(), re::LIVE_WINDOW_SECS);
    assert!(p.replied && !p.live, "{p:?}");
    // the window is a parameter: a shorter one does not see you in the burst
    let c = case("fx-02-dm-burst-owner-live");
    assert!(!re::presence(&c, 0, 30).live);
}

#[test]
fn system_texts_are_recognized_by_provenance_not_by_keywords() {
    let sys = system();
    let sys: Vec<&str> = sys.iter().map(String::as_str).collect();
    // the router's queue sentence and its options, said as you (patterns 4 and 5)
    assert!(
        re::check_draft(steve_core::thread::research::QUEUE_FULL, &sys)
            .leak
            .is_some()
    );
    let routed = case("fx-04-router-ask-bypass");
    let d = re::check_draft(routed.steve["draft_text"].as_str().unwrap(), &sys);
    assert!(d.leak.is_some() && d.option_list, "{d:?}");
    // templates match with their value, and only with it
    let ack = steve_channels::rules::handoff_ack("小王");
    assert!(system_text_in(&format!("好的\n{ack}"), SYSTEM_TEXTS).is_some());
    assert!(system_text_in("我请你吃饭吧", SYSTEM_TEXTS).is_none());
    assert!(system_text_in(&steve_channels::replies::item_ack("ST-9"), SYSTEM_TEXTS).is_some());
    // your own words may name Steve or an item: counted for the log, never a leak
    let own = re::check_draft("Steve 那个事项我晚点看", &sys);
    assert!(own.leak.is_none() && !own.keywords.is_empty(), "{own:?}");
}

#[test]
fn the_draft_checks() {
    let sys = system();
    let sys: Vec<&str> = sys.iter().map(String::as_str).collect();
    let d = re::check_draft("收到，这次要查清通知在哪一步丢了。", &sys);
    assert!(
        d.opening && d.full_stop && !d.question && d.messages == 1,
        "{d:?}"
    );
    let d = re::check_draft("先放着\n让它跑跑看", &sys);
    assert!(!d.opening && !d.full_stop && d.messages == 2, "{d:?}");
    assert_eq!(d.line_median_len, 4.0);
    let d = re::check_draft("你是要写入还是读取？还是都要？", &sys);
    assert!(d.question && d.option_list, "two questions: {d:?}");
    let d = re::check_draft("可以这样：\nA. 先重启\nB. 再观察", &sys);
    assert!(d.option_list, "{d:?}");
    // an opener only when it stands alone: “好的吧” is an answer
    assert!(!re::check_draft("好的吧", &sys).opening);
}

#[test]
fn your_statistics_come_from_your_own_messages_per_situation() {
    let s = re::owner_styles(&corpus(), &names());
    let dm = &s[&Situation::Dm];
    assert!(dm.n >= re::MIN_SUPPORT, "{dm:?}");
    assert_eq!(dm.full_stop_rate, 0.0);
    // two group messages answered an @ of you; too few of their own, so the group's stand in
    assert_eq!(s[&Situation::GroupAt].n, 2);
    let (used, _) = re::style_for(&s, Situation::GroupAt).unwrap();
    assert_ne!(used, Situation::GroupAt);
    // links are not style
    assert!(s[&Situation::All].n < corpus().len());
}

fn outputs(f: impl Fn(&Label) -> (&'static str, &'static str)) -> Vec<Output> {
    labels()
        .iter()
        .map(|l| {
            let (outcome, text) = f(l);
            Output {
                case_id: l.case_id.clone(),
                outcome: outcome.into(),
                text: text.into(),
                ..Default::default()
            }
        })
        .collect()
}

#[test]
fn skipping_everything_does_not_pass() {
    let (cs, ls) = (cases(), labels());
    let styles = re::owner_styles(&corpus(), &names());
    let skip_all = outputs(|_| ("skip", ""));
    let m = re::metrics(&re::rows(&cs, &ls, &skip_all, &styles, &params()));
    assert_eq!(m.presence_skip_recall, Some(1.0));
    assert_eq!(m.act_recall, Some(0.0), "{m:?}");
    assert_eq!(m.act_precision, None);
    assert!(m.decision_accuracy < 1.0);

    // the right decisions, in your voice
    let right = outputs(|l| match l.correct_outcome.as_str() {
        "reply" => ("reply", "@G 是自动推送的那条？"),
        "ask_owner" => ("ask_owner", "要你定"),
        "to_item" => ("to_item", ""),
        _ => ("skip", ""),
    });
    let m = re::metrics(&re::rows(&cs, &ls, &right, &styles, &params()));
    assert_eq!(
        (m.decision_accuracy, m.act_recall, m.act_precision),
        (1.0, Some(1.0), Some(1.0))
    );
    assert_eq!((m.leaks, m.option_lists), (0, 0));
    assert_eq!(m.full_stop_rate, Some(0.0));
}

#[test]
fn what_steve_did_then_scores_as_the_review_found() {
    let (cs, ls) = (cases(), labels());
    let styles = re::owner_styles(&corpus(), &names());
    let then: Vec<Output> = cs.iter().map(re::recorded_output).collect();
    let rows = re::rows(&cs, &ls, &then, &styles, &params());
    let m = re::metrics(&rows);
    // the queue sentence and the router's options went out as drafts
    assert_eq!(m.leaks, 2, "{m:?}");
    // it acted whenever you were there
    assert_eq!(m.presence_skip_recall, Some(0.0));
    assert!(m.length_ratio_median.unwrap() > 1.5, "{m:?}");
    assert!(m.full_stop_rate.unwrap() > 0.0);

    // against itself nothing regresses; against the right answers what it got wrong is listed
    assert!(re::regressions(&rows, &rows).is_empty());
    let right = outputs(|l| match l.correct_outcome.as_str() {
        "reply" => ("reply", "@G 是自动推送的那条？"),
        "ask_owner" => ("ask_owner", "要你定"),
        "to_item" => ("to_item", ""),
        _ => ("skip", ""),
    });
    let good = re::rows(&cs, &ls, &right, &styles, &params());
    // right with the good answers, wrong then: what a change to them would fix
    let fixed = re::regressions(&good, &rows);
    assert!(
        fixed.contains(&"fx-05-queue-full-leak".to_string())
            && fixed.contains(&"fx-07-remediation-notice".to_string()),
        "{fixed:?}"
    );
    assert!(re::regressions(&rows, &good).is_empty());
}

#[test]
fn several_samples_of_a_case_that_disagree_are_counted() {
    let (cs, ls) = (cases(), labels());
    let styles = re::owner_styles(&corpus(), &names());
    let mut outs = outputs(|_| ("skip", ""));
    outs.push(Output {
        case_id: "fx-01-joke-on-own-line".into(),
        sample: 1,
        outcome: "reply".into(),
        text: "哈哈".into(),
        ..Default::default()
    });
    let m = re::metrics(&re::rows(&cs, &ls, &outs, &styles, &params()));
    assert_eq!((m.answers, m.cases, m.unstable_cases), (13, 12, 1));
}

#[test]
fn the_split_keeps_a_space_on_one_side() {
    let cs = cases();
    let mut side: BTreeMap<&str, &str> = BTreeMap::new();
    for c in &cs {
        let s = re::split(c);
        assert_eq!(*side.entry(c.space.id.as_str()).or_insert(s), s);
    }
}

#[test]
fn the_judges_prompt_is_blind_and_parsed() {
    let c = case("fx-11-incident-note");
    let o = re::recorded_output(&c);
    let p = re::judge_prompt(&c, &o, None, HISTORY);
    assert!(!p.contains("（一小时内没有发）") && p.contains("自动推送的那条"));
    assert!(
        !p.contains("baseline") && !p.contains("model-drafter"),
        "{p}"
    );
    let j = re::parse_judgment(
        r#"好的 {"understanding": false, "understanding_why": "没看出是在问你", "voice": 2, "card_answerable": null, "note": "公文腔"}"#,
    )
    .unwrap();
    assert_eq!((j.understanding, j.voice), (Some(false), Some(2.0)));
}

#[test]
fn the_presence_gate_replayed_skips_where_you_were_there_and_keeps_what_needs_you() {
    let (cs, ls) = (cases(), labels());
    let styles = re::owner_styles(&corpus(), &names());
    let then: Vec<Output> = cs.iter().map(re::recorded_output).collect();
    let (gated, what) = re::apply_gates(&cs, &then, &re::GateParams::default());
    let gate_of = |id: &str| {
        what.iter()
            .find(|g| g.case_id == id)
            .and_then(|g| g.gate.clone())
    };
    // you had answered by the time the draft came, or were chatting right before
    assert_eq!(
        gate_of("fx-03-owner-replied-after").as_deref(),
        Some("owner_replied")
    );
    assert_eq!(
        gate_of("fx-02-dm-burst-owner-live").as_deref(),
        Some("owner_live")
    );
    // what needed you and you answered only later still reached you (and your answer closed it)
    for id in ["fx-08-long-analysis", "fx-10-schedule"] {
        let g = what.iter().find(|g| g.case_id == id).unwrap();
        assert_eq!(g.gate, None, "{id}");
        assert!(g.closed_later, "{id}");
    }
    // judged by the labels, never by the presence metric (a rule close to the gate's own)
    let m = re::metrics(&re::rows(&cs, &ls, &gated, &styles, &params()));
    let before = re::metrics(&re::rows(&cs, &ls, &then, &styles, &params()));
    assert!(m.decision_accuracy > before.decision_accuracy, "{m:?}");
    assert!(m.act_precision > before.act_precision, "{m:?}");
    // every case the labels say needs Steve and Steve acted on then, it still acts on
    let rows = re::rows(&cs, &ls, &gated, &styles, &params());
    let then_rows = re::rows(&cs, &ls, &then, &styles, &params());
    for r in then_rows
        .iter()
        .filter(|r| r.should_act && r.outcome != "skip")
    {
        let now = rows.iter().find(|x| x.case_id == r.case_id).unwrap();
        assert_ne!(
            now.outcome, "skip",
            "{} needs you and is skipped",
            r.case_id
        );
    }
    assert_eq!(m.act_recall, before.act_recall, "{m:?}");
}

/// On your own account the Bot behind it only picks the model: its name, duty, manner and rules are
/// not in the prompt (`--bot-layer` puts them back, the ablation that measures the removal).
#[test]
fn your_own_account_has_no_bot_layer_but_the_bots_model() {
    let c = case("fx-11-incident-note");
    let s = Setting {
        bot: Some(BotInfo {
            id: "b".into(),
            name: "助手".into(),
            role: "理解需求、分派、汇报".into(),
            persona: "沉稳".into(),
            rules: "建事项交给合适的 Bot".into(),
            model: Some("m1".into()),
            ..BotInfo::default()
        }),
        ..setting()
    };
    let l = re::layers(&c, &s, HISTORY, re::bare_voice(&c));
    assert!(l.bot.is_none());
    assert_eq!(l.model.as_deref(), Some("m1"));
    let p = compose::prompt(&l);
    assert!(!p.contains("你代表 Bot") && !p.contains("分派"), "{p}");
    assert!(p.contains(&format!("{H_BOT}\n（无）")), "{p}");
    // the rules of writing as you, always; your habits only with your messages
    assert!(p.contains(H_WRITE) && !p.contains(H_STYLE), "{p}");
    let ablated = Setting {
        bot_layer: true,
        ..s
    };
    let p = compose::prompt(&re::layers(&c, &ablated, HISTORY, re::bare_voice(&c)));
    assert!(
        p.contains("你代表 Bot「助手」说话") && p.contains("分派"),
        "{p}"
    );
}

/// The replay's habits come from your messages before the case, never from any case's gold answers,
/// per situation.
#[test]
fn the_replays_habits_leave_out_the_gold_answers() {
    let cs = cases();
    let gold = re::gold_ids(&cs);
    let corpus = corpus();
    let people = re::people(&cs, &corpus);
    let late = case("fx-12-numeric-owner-name");
    let with = re::voice(&corpus, &late, &HashSet::new(), &names(), &people);
    let without = re::voice(&corpus, &late, &gold, &names(), &people);
    let n = |v: &re::Voice| v.style.as_ref().map(|(_, s)| s.n).unwrap_or(0);
    assert!(n(&without) < n(&with), "{} vs {}", n(&without), n(&with));
    assert_eq!(without.situation, Some(re::situation(&late)));
}

#[test]
fn nothing_to_answer_is_replayed_before_any_model() {
    // a forwarded card nobody can read and no question of the sender waiting: G4
    assert!(re::no_content(&case("fx-09-forwarded-card")));
    for id in [
        "fx-10-schedule",
        "fx-08-long-analysis",
        "fx-01-joke-on-own-line",
    ] {
        assert!(!re::no_content(&case(id)), "{id}");
    }
    let outs = vec![Output {
        case_id: "fx-09-forwarded-card".into(),
        outcome: "ask_owner".into(),
        text: "要看吗".into(),
        ..Output::default()
    }];
    let (gated, why) = re::apply_gates(&cases(), &outs, &re::GateParams::default());
    assert_eq!(gated[0].outcome, "skip");
    assert_eq!(why[0].gate.as_deref(), Some("no_content"));
}

#[test]
fn the_judge_sees_the_card_the_owner_would_see() {
    let c = case("fx-10-schedule");
    let new = Output {
        case_id: c.case_id.clone(),
        outcome: "ask_owner".into(),
        code: "commitment".into(),
        summary: "F 想约周四下午半小时聊方案".into(),
        draft: "周四下午可以".into(),
        text: "要不要答应？".into(),
        ..Output::default()
    };
    let shown = re::card_shown(&c, &new);
    assert!(
        shown.contains("找你：F 想约周四下午半小时聊方案"),
        "{shown}"
    );
    assert!(shown.contains("为什么要你："), "{shown}");
    assert!(shown.contains("→ F："), "{shown}");
    assert!(shown.ends_with("可以直接发的草稿：周四下午可以"), "{shown}");
    assert!(
        !shown.contains("要不要答应"),
        "never the model's own question: {shown}"
    );
    // an answer from before the summary: the card as it was then
    let old = Output {
        summary: String::new(),
        code: String::new(),
        draft: String::new(),
        ..new
    };
    let shown = re::card_shown(&c, &old);
    assert!(shown.contains("Steve 想先问你：要不要答应？"), "{shown}");
    assert!(re::judge_prompt(&c, &old, None, 10).contains("主人看到的卡片"));
}
