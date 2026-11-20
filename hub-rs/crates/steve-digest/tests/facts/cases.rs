//! Characterization of facts::build before its structural extraction. No model or network.
use super::digest_env::TestEnv;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_digest::{ConvRef, conv, facts, profile};
use steve_modules::testkit::WS;

fn env() -> TestEnv {
    let e = super::env();
    e.store().with_conn(|c| {
        c.execute("INSERT INTO settings (workspace_id,key,value,updated) VALUES ('default','tz','\"UTC\"',1) ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value", [])?;
        Ok(())
    }).unwrap();
    e
}

fn inputs(kind: &str) -> (conv::ConvInfo, profile::Profile) {
    let conv = ConvRef::new(kind, "s1");
    let info = conv::ConvInfo {
        conv: conv.clone(),
        name: "Facts".into(),
        source_kind: "group".into(),
        adapter: "fixture".into(),
        members: 3,
        has_external: true,
        role: "owner duty".into(),
        muted: false,
        attention: "realtime".into(),
        active: true,
    };
    let profile = profile::Profile {
        conv,
        kind: "team".into(),
        kind_source: "rule".into(),
        settings: profile::defaults("team"),
        own: vec![],
        notify_source: "default".into(),
        revision: 0,
    };
    (info, profile)
}

fn person(id: Option<&str>, name: &str, bot: bool, mine: bool) -> conv::Person {
    conv::Person {
        party: id.map(str::to_string),
        name: name.into(),
        bot,
        mine,
        ..Default::default()
    }
}

fn message(id: &str, sender: conv::Person, text: &str) -> conv::Msg {
    conv::Msg {
        id: id.into(),
        ext_id: Some(format!("provider-{id}")),
        at: 0,
        sender,
        mentions: vec![],
        reply_to: None,
        thread: None,
        text: text.into(),
        attachments: vec![],
        subject: None,
    }
}

fn build(e: &TestEnv, kind: &str, context: &[conv::Msg], messages: &[conv::Msg]) -> facts::Built {
    let (info, profile) = inputs(kind);
    e.store()
        .reader(|c| facts::build(c, WS, &info, &profile, 7, "manual", context, messages))
        .unwrap()
}

fn stored(
    e: &TestEnv,
    id: &str,
    space: &str,
    reply: Option<&str>,
    thread: Option<&str>,
    text: &str,
) {
    // Reuse the audited conversation fixture; new cases do not add another source-table writer.
    super::msg(e, space, id, 0, reply, thread, text);
}

#[test]
fn sg5_sender_display_and_owner_clock_are_characterized() {
    let e = env();
    for (name, bot, mine, shown) in [
        (" Alice ", false, false, "Alice"),
        ("   ", false, false, "?"),
        (" Bot ", true, false, "Bot〔机器人〕"),
        (" Owner ", false, true, "我（Owner）"),
        (" Bot ", true, true, "我（Bot）"),
    ] {
        let m = message("m", person(Some("alice"), name, bot, mine), "hello");
        let b = build(&e, "space", &[], &[m]);
        assert_eq!(b.lines[0].text, format!("[1] 01-01 00:00 {shown}：hello"));
        assert_eq!(b.lines[0].body, "hello");
        assert_eq!(
            b.parties[0].name, name,
            "display trimming must not rewrite fact names"
        );
        assert_eq!(b.facts["messages"][0]["name"], name);
        assert_eq!(b.my_messages, i64::from(mine));
        assert_eq!(
            b.facts["me"]["wrote"],
            if mine { json!([1]) } else { json!([]) }
        );
    }
    let b = build(
        &e,
        "space",
        &[],
        &[message(
            "m",
            person(Some("alice"), "Alice", false, false),
            "hello",
        )],
    );
    assert_eq!(
        serde_json::to_value(&b.lines[0]).unwrap(),
        json!({
            "pos":1,"id":"m","at":0,"chain":null,"text":"[1] 01-01 00:00 Alice：hello","tokens":9
        })
    );
    e.store().with_conn(|c| {
        c.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated)
            VALUES ('default','usr_dev',1,'log','Pacific/Honolulu',1,0,1)", [])?;
        Ok(())
    }).unwrap();
    let b = build(
        &e,
        "space",
        &[],
        &[message("m", person(None, "Alice", false, false), "hello")],
    );
    assert_eq!(
        b.lines[0].text, "[1] 12-31 14:00 Alice：hello",
        "owner policy wins over workspace UTC"
    );
}

#[test]
fn sg5_context_subjects_and_mail_recipients_keep_their_original_order() {
    let e = env();
    for (kind, recipient) in [("space", ""), ("mail_thread", " → One、Two")] {
        let mut context = message(
            "ctx",
            person(Some("me"), "Owner", false, true),
            " before  \n\n\n old ",
        );
        context.subject = Some("A".into());
        context.mentions = vec![person(Some("me"), "Owner", false, true)];
        let mut messages = Vec::new();
        for (id, subject) in [
            ("one", None),
            ("two", Some("A")),
            ("three", Some("B")),
            ("four", Some("")),
        ] {
            let mut m = message(id, person(Some("alice"), "Alice", false, false), "body");
            m.subject = subject.map(str::to_string);
            m.mentions = vec![
                person(None, "One", false, false),
                person(None, "Two", false, false),
            ];
            messages.push(m);
        }
        let b = build(&e, kind, &[context], &messages);
        assert_eq!(
            b.lines.iter().map(|l| l.pos).collect::<Vec<_>>(),
            [-1, 1, 2, 3, 4]
        );
        assert_eq!(
            b.lines[0].text,
            if kind == "mail_thread" {
                "[-1] 01-01 00:00 我（Owner） → Owner：主题：A\nbefore\n\n old"
            } else {
                "[-1] 01-01 00:00 我（Owner）：主题：A\nbefore\n\n old"
            }
        );
        assert_eq!(
            b.lines[1].text,
            format!("[1] 01-01 00:00 Alice{recipient}：body")
        );
        assert_eq!(
            b.lines[2].text,
            format!("[2] 01-01 00:00 Alice{recipient}：body"),
            "None does not reset the context subject"
        );
        assert_eq!(
            b.lines[3].text,
            format!("[3] 01-01 00:00 Alice{recipient}：主题：B\nbody")
        );
        assert_eq!(
            b.lines[4].text,
            format!("[4] 01-01 00:00 Alice{recipient}：主题：\nbody")
        );
        assert_eq!(
            b.facts["window"],
            json!({"seq":7,"trigger":"manual","count":4,"context":1,"first_id":"one","last_id":"four","first_at":0,"last_at":0})
        );
        assert_eq!(b.facts["messages"].as_array().unwrap().len(), 4);
        assert_eq!(b.my_messages, 0, "context is read-only participation");
        assert!(!b.mentions_me, "context mentions do not notify the owner");
        assert_eq!(
            b.facts["me"],
            json!({"party":"me","participated":false,"wrote":[],"mentioned":[]})
        );
        assert_eq!(
            b.parties.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["alice"]
        );
    }
}

#[test]
fn sg5_reference_order_keeps_context_positions_and_workspace_item_scope() {
    let e = env();
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
            VALUES ('ST-42','default',42,'Local item','run','定义',1,1),('AB-9','w2',9,'Foreign item','run','定义',1,1);")?;
        Ok(())
    }).unwrap();
    for (with_item, expected_items, expected_refs) in
        [(false, json!([]), 9), (true, json!(["ST-42"]), 10)]
    {
        let mut context = message("ctx", person(Some("alice"), "Alice", false, false), "ST-42");
        context.attachments = vec![json!({"type":"image","key":"context"})];
        let text = format!(
            "{} https://github.com/o/r/pull/7 https://github.com/o/r/pull/7 https://x.feishu.cn/docx/AbC https://example.com/page.",
            if with_item {
                "ST-42 ST-42 ST-404 AB-9"
            } else {
                "hello"
            }
        );
        let mut m = message("m", person(Some("alice"), "Alice", false, false), &text);
        m.attachments = vec![
            json!({"type":"file","key":"f","name":"report","size":2048}),
            json!({"type":"forward","messages":[{"id":"fw","sender":{"name":"Forwarder"},"at":0,"text":"inside https://hidden.example/path"},{"text":"unknown"}]}),
            json!({"type":"image","key":"i","width":3,"height":4}),
            json!({"type":"card","title":"Card","format":"json"}),
        ];
        let b = build(&e, "space", &[context], &[m]);
        assert_eq!(b.refs.len(), expected_refs);
        assert_eq!(
            b.refs.iter().map(|r| r.n).collect::<Vec<_>>(),
            (1..=expected_refs).collect::<Vec<_>>()
        );
        assert_eq!(
            b.refs
                .iter()
                .take(9)
                .map(|r| r.kind.as_str())
                .collect::<Vec<_>>(),
            [
                "image", "item", "file", "image", "card", "forward", "pr", "doc", "link"
            ]
        );
        assert_eq!(
            b.refs.iter().map(|r| r.pos).collect::<Vec<_>>(),
            [vec![-1, -1], vec![1; expected_refs - 2]].concat()
        );
        assert_eq!(
            b.facts["messages"][0]["refs"],
            Value::from((3..=expected_refs).collect::<Vec<_>>())
        );
        assert_eq!(b.facts["items"], expected_items);
        assert_eq!(b.items, if with_item { vec!["ST-42"] } else { vec![] });
        assert_eq!(b.refs[5].title, "2 条");
        assert_eq!(b.refs[5].at, json!({"forwarded":["fw",""]}));
        assert_eq!(b.refs[6].at["repo"], "o/r");
        assert_eq!(b.refs[7].at["token"], "AbC");
        assert_eq!(b.refs[8].at["url"], "https://example.com/page");
        assert!(b.lines[1].text.contains(
            "〔文件 r3 · report · 2 KB〕 〔图片 r4 · 3×4〕 〔卡片 r5 · Card〕 〔转发 r6 · 2 条〕"
        ));
        assert!(b.lines[1].text.contains(
            "↳ 转发 Forwarder 01-01 00:00（原消息 fw）：inside https://hidden.example/path"
        ));
        assert!(b.lines[1].text.contains("↳ 转发 ?（原消息 -）：unknown"));
        assert!(!b.refs.iter().any(|r| r.at["domain"] == "hidden.example"));
    }
}

#[test]
fn sg5_reply_chain_table_keeps_positions_topics_quotes_and_cycle_roots() {
    let e = env();
    stored(&e, "root", "s1", None, None, &"q".repeat(70));
    stored(&e, "reply", "s1", Some("root"), None, "stored reply");
    stored(
        &e,
        "topic",
        "s1",
        Some("root"),
        Some("old-topic"),
        "stored topic",
    );
    stored(&e, "cycle-a", "s1", Some("cycle-b"), None, "a");
    stored(&e, "cycle-b", "s1", Some("cycle-a"), None, "b");
    stored(&e, "elsewhere", "s2", None, None, "must not leak");
    for (reply, topic, chain, quote) in [
        ("missing", None, "r:missing", "（回复 missing）".to_string()),
        (
            "elsewhere",
            None,
            "r:elsewhere",
            "（回复 elsewhere）".to_string(),
        ),
        (
            "root",
            None,
            "r:root",
            format!("（回复 上文 root ?：「{}…」）", "q".repeat(60)),
        ),
        (
            "reply",
            None,
            "r:root",
            "（回复 上文 reply ?：「stored reply」）".to_string(),
        ),
        (
            "topic",
            None,
            "t:old-topic",
            "（回复 上文 topic ?：「stored topic」）".to_string(),
        ),
        (
            "cycle-a",
            None,
            "r:cycle-a",
            "（回复 上文 cycle-a ?：「a」）".to_string(),
        ),
        (
            "topic",
            Some("new-topic"),
            "t:new-topic",
            "（回复 上文 topic ?：「stored topic」）".to_string(),
        ),
    ] {
        let mut m = message("m", person(Some("alice"), "Alice", false, false), "hello");
        m.reply_to = Some(reply.into());
        m.thread = topic.map(str::to_string);
        let b = build(&e, "space", &[], &[m]);
        assert_eq!(b.lines[0].chain.as_deref(), Some(chain), "{reply}");
        assert_eq!(b.chains.get(chain), Some(&vec![1]));
        assert!(b.lines[0].text.contains(&quote), "{}", b.lines[0].text);
        assert_eq!(b.facts["messages"][0]["reply_to"], reply);
    }
    for in_context in [false, true] {
        let mut parent = message("parent", person(Some("bob"), "Bob", false, false), "before");
        parent.thread = Some("known".into());
        let mut m = message("m", person(Some("alice"), "Alice", false, false), "hello");
        m.reply_to = Some("parent".into());
        let b = if in_context {
            build(&e, "space", &[parent], &[m])
        } else {
            build(&e, "space", &[], &[parent, m])
        };
        let positions = if in_context { vec![-1, 1] } else { vec![1, 2] };
        assert_eq!(b.chains["t:known"], positions);
        assert_eq!(
            b.facts["messages"].as_array().unwrap().last().unwrap()["reply_to"],
            positions[0]
        );
        assert!(
            b.lines
                .last()
                .unwrap()
                .text
                .contains(&format!("（回复 [{}]）", positions[0]))
        );
    }
}

#[test]
fn sg5_participant_table_keeps_first_seen_metadata_and_distinct_owner_flags() {
    let e = env();
    for (mention_party, mention_mine, mentions_me, mentioned) in [
        (Some("me"), false, true, json!([])),
        (Some("me"), true, true, json!([2])),
        (Some("alice"), false, false, json!([])),
        (None, true, true, json!([2])),
        (None, false, false, json!([])),
    ] {
        let mut ctx = message(
            "ctx",
            person(Some("me"), "Context owner", false, true),
            "context",
        );
        ctx.mentions = vec![person(Some("me"), "Context owner", false, true)];
        let mut first = message("first", person(Some("bob"), "Bot", true, false), "one");
        first.at = 30;
        first.mentions = vec![person(Some("alice"), "Alias first", false, false)];
        let mut second = message(
            "second",
            person(Some("alice"), "Alice later", true, false),
            "two",
        );
        second.at = 10;
        second.mentions = vec![person(mention_party, "Mention", false, mention_mine)];
        let third = message("third", person(None, "Owner", false, true), "three");
        let b = build(&e, "space", &[ctx], &[first, second, third]);
        assert_eq!(b.my_messages, 1);
        assert_eq!(b.mentions_me, mentions_me);
        assert_eq!(b.facts["me"]["wrote"], json!([3]));
        assert_eq!(b.facts["me"]["mentioned"], mentioned);
        assert_eq!(
            b.lines.iter().map(|l| l.at).collect::<Vec<_>>(),
            [0, 30, 10, 0]
        );
        assert_eq!(b.facts["window"]["first_at"], 30);
        assert_eq!(b.facts["window"]["last_at"], 0);
        let alice = b.parties.iter().find(|p| p.id == "alice").unwrap();
        assert_eq!(
            (
                alice.name.as_str(),
                alice.bot,
                alice.mine,
                alice.messages,
                alice.mentioned,
                alice.last_at
            ),
            (
                "Alias first",
                false,
                false,
                1,
                1 + i64::from(mention_party == Some("alice")),
                30
            )
        );
        let bob = b.parties.iter().find(|p| p.id == "bob").unwrap();
        assert_eq!(
            (bob.bot, bob.messages, bob.mentioned, bob.last_at),
            (true, 1, 0, 30)
        );
        assert_eq!(
            b.parties.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            if mention_party == Some("me") {
                vec!["alice", "bob", "me"]
            } else {
                vec!["alice", "bob"]
            }
        );
    }
}

#[test]
fn sg5_empty_cached_parent_and_storage_errors_are_characterized() {
    let (info, p) = inputs("space");
    let c = Connection::open_in_memory().unwrap();
    let err = facts::build(&c, WS, &info, &p, 7, "manual", &[], &[]).unwrap_err();
    assert!(err.to_string().contains("parties"), "{err}");
    c.execute_batch("CREATE TABLE parties(id TEXT,workspace_id TEXT,user_id TEXT); CREATE TABLE workspace_members(workspace_id TEXT,user_id TEXT,role TEXT,created INTEGER);
        CREATE TABLE settings(workspace_id TEXT,key TEXT,value TEXT); INSERT INTO settings VALUES('default','tz','\"UTC\"');").unwrap();
    let b = facts::build(&c, WS, &info, &p, 7, "manual", &[], &[]).unwrap();
    assert_eq!(
        b.facts["window"],
        json!({"seq":7,"trigger":"manual","count":0,"context":0,"first_id":null,"last_id":null,"first_at":null,"last_at":null})
    );
    assert!(b.lines.is_empty() && b.refs.is_empty() && b.chains.is_empty() && b.parties.is_empty());
    assert_eq!(
        b.facts["me"],
        json!({"party":null,"participated":false,"wrote":[],"mentioned":[]})
    );
    let mut ctx = message("ctx", person(None, "Context", false, false), "context");
    ctx.thread = Some("cached".into());
    let mut m = message("m", person(None, "Sender", false, false), "hello");
    m.reply_to = Some("ctx".into());
    let b = facts::build(&c, WS, &info, &p, 7, "manual", &[ctx], &[m]).unwrap();
    assert_eq!(
        b.chains["t:cached"],
        [-1, 1],
        "known parent chain needs no message table"
    );
    for (text, reply, missing) in [
        ("ST-42", None, "items"),
        ("hello", Some("outside"), "no such table"),
    ] {
        let mut m = message("m", person(None, "Sender", false, false), text);
        m.reply_to = reply.map(str::to_string);
        let err = facts::build(&c, WS, &info, &p, 7, "manual", &[], &[m]).unwrap_err();
        assert!(err.to_string().contains(missing), "{err}");
    }
}
