//! Public routing and gateway contracts, with finite model/ACP peers.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Duration,
};
use steve_core::{CoreError, gateway::*};
use steve_store::{Bot, Project, Tag};

fn bot(id: &str) -> Bot {
    Bot {
        id: id.into(),
        workspace_id: "default".into(),
        name: id.into(),
        role: format!("role of {id}"),
        main: id == "steve",
        harness: "claude-code".into(),
        model: None,
        reasoning_effort: None,
        rules: String::new(),
        accept_mode: "inherit".into(),
        perm_mode: "default".into(),
        enabled: true,
        revision: 1,
        updated: 0,
        persona: String::new(),
        proactive: 1,
        avatar: Default::default(),
        default_project: None,
        prompt: None,
        device: None,
    }
}
fn project(id: &str) -> Project {
    Project {
        id: id.into(),
        workspace_id: "default".into(),
        name: "Project".into(),
        description: "Contract evidence".into(),
        device_id: "hub".into(),
        path: "/fixture/project".into(),
        kind: "git".into(),
        worktree: false,
        default_bot: Some("dev".into()),
        release: None,
        archived: false,
        created: 0,
        updated: 0,
    }
}
fn context() -> RouteCtx {
    let tag = |id: &str, name: &str, cat: &str| Tag {
        id: id.into(),
        workspace_id: "default".into(),
        name: name.into(),
        cat: cat.into(),
        description: "Concrete scope".into(),
        created_by: "fixture".into(),
        created: 0,
    };
    RouteCtx {
        bots: vec![bot("steve"), bot("dev")],
        projects: vec![project("project")],
        tags: vec![
            tag("bug", "bug", steve_store::kinds::KIND_CAT),
            tag("feat", "feat", steve_store::kinds::KIND_CAT),
            tag("topic", "topic", "领域"),
        ],
        steve_project: Some("project".into()),
        items: vec!["ST-1".into()],
        closed_items: vec!["ST-9".into(), "ST-90".into()],
        focus: vec![FocusItem {
            id: "ST-1".into(),
            title: "Open work".into(),
            status: "run".into(),
            origin: "original instruction".into(),
        }],
        pending: vec!["earlier question".into()],
        researching: true,
        operator: true,
        debug: true,
    }
}

#[test]
fn prompt_bytes_preserve_static_scope_context_clipping_and_batch_instructions() {
    let c = context();
    let history: Vec<_> = (0..8)
        .map(|i| Said {
            author: if i % 2 == 0 { "me" } else { "steve" }.into(),
            text: if i == 7 {
                "界".repeat(810)
            } else {
                format!("message {i}")
            },
        })
        .collect();
    // Preserve the current recognizer's ASCII-prefix behavior; changing it is outside this refactor.
    assert!(
        !route_prompt_for(&c, &["follow ST-9".into()], &[], false)
            .contains("Closed items (context only")
    );
    for (name, texts, promote) in [
        ("single", vec!["查看 ST-9".into()], false),
        ("batch", vec!["first".into(), "second".into()], false),
        ("promote", vec!["make it work".into()], true),
    ] {
        let p = route_prompt_for(&c, &texts, &history, promote);
        let (head, tail) = split_static(&p);
        assert!(head.unwrap().starts_with(ROUTE_RULES.trim_end()));
        assert!(tail.contains("Recent conversation, oldest first:"));
        assert!(!tail.contains("message 0"));
        assert!(tail.contains("message 2"));
        assert!(p.contains("role of dev") && p.contains("Concrete scope"));
        if name == "single" {
            assert!(tail.contains("Closed items (context only; never supplement these): ST-9"));
            assert!(!tail.contains("ST-90"));
        }
        // Old-implementation byte contracts cover the full instructions and ordered context.
        let expected = match name {
            "single" => "607cd3f60a3a30dc5f7392af734162f620a00b3565906bd50cd338691fe1bca7",
            "batch" => "d1c44a9a456f37c20a89062f1c33fbbcbaa676eecc7f007e4601de6a79f77afe",
            "promote" => "8f2ef33fa26de16e4e693c135ed0bed2e283d82796335b71a8156f3486708f2d",
            _ => unreachable!(),
        };
        let actual = Sha256::digest(p.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(actual, expected, "{name} prompt changed");
    }
}

#[test]
fn parsed_route_preserves_bot_selection_scope_lists_and_javascript_field_rules() {
    let c = context();
    let raw=json!({"kind":"item","project":"project","bot":"steve","title":"界".repeat(50),"goal":true,"confidence":2,"tags":["topic","bug","feat","topic","missing"],"newTags":[{"name":"bad","cat":"invalid","description":"x"},{"name":"new","cat":"主题","description":"scope"},{"name":"ignored","cat":"主题","description":"scope"}],"signals":["change","change","invalid","explicit"],"options":[" a ","",1,"b","c","d"],"reply":"收到。 标签：重复。","why":"x".repeat(250)}).to_string();
    let r = parse_route(&c, "original", &raw, false).unwrap();
    assert_eq!(
        (r.bot.as_str(), r.bot_from_project, r.project.as_deref()),
        ("dev", true, Some("project"))
    );
    assert_eq!(r.tag_ids, ["bug", "topic"]);
    assert_eq!(r.new_tags.len(), 1);
    assert_eq!(r.new_tags[0].name, "new");
    assert_eq!(r.signals, ["change", "explicit"]);
    assert_eq!(r.options, ["a", "b", "c"]);
    assert_eq!(r.confidence, 1.0);
    assert_eq!(r.title, "界".repeat(40));
    assert_eq!(r.goal, "true");
    assert_eq!(r.why.len(), 200);
    assert_eq!(r.reply, "收到。");
    assert!(r.after);
    for bad in [
        json!({"kind":"unknown"}),
        json!({"kind":"status","item":"ST-2"}),
        json!({"kind":"item","tags":7}),
        json!({"kind":"item","newTags":"bad"}),
    ] {
        assert!(
            parse_route(&c, "text", &bad.to_string(), false).is_none(),
            "{bad}"
        );
    }
    let r = parse_route(
        &c,
        "text",
        &json!({"kind":"status","item":" #st-1 ","after":false}).to_string(),
        false,
    )
    .unwrap();
    assert_eq!(r.item.as_deref(), Some("ST-1"));
    assert!(!r.after);
    let r = parse_route(
        &c,
        "text",
        &json!({"kind":"chat","project":"missing","title":0}).to_string(),
        true,
    )
    .unwrap();
    assert_eq!(r.kind, "item");
    assert_eq!(r.title, "text");
    assert!(r.project.is_none());
}

struct Replies {
    answers: Mutex<VecDeque<Result<String, &'static str>>>,
    seen: Mutex<Vec<String>>,
}
#[async_trait::async_trait]
impl ModelGateway for Replies {
    async fn call(
        &self,
        p: &str,
        t: Tier,
        _: Duration,
        tools: &ThinkTools,
    ) -> steve_core::Result<Printed> {
        assert_eq!(t, Tier::Fast);
        assert_eq!(tools, &ThinkTools::None);
        self.seen.lock().unwrap().push(p.into());
        match self.answers.lock().unwrap().pop_front().unwrap() {
            Ok(text) => Ok(Printed {
                text,
                model: "router-model".into(),
                ..Default::default()
            }),
            Err(e) => Err(CoreError::Msg(e.into())),
        }
    }
}
#[tokio::test]
async fn single_and_batch_routes_preserve_failure_fallback_and_reported_model() {
    let c = context();
    let gw = Replies {
        answers: Mutex::new(VecDeque::from([
            Ok(
                json!([{"kind":"answer","reply":"one"},{"kind":"status","item":"ST-1"}])
                    .to_string(),
            ),
            Ok("unusable".into()),
            Err("provider refused"),
            Ok(json!({"kind":"answer","reply":"usable"}).to_string()),
            Ok("[]".into()),
        ])),
        seen: Mutex::new(Vec::new()),
    };
    let texts = vec!["one".into(), "two".into()];
    let rs = route_many(&gw, &c, &texts, &[]).await.unwrap();
    assert_eq!(rs.len(), 2);
    assert!(
        rs.iter()
            .all(|r| r.model.as_deref() == Some("router-model"))
    );
    let r = route(&gw, &c, "unusable", &[], false).await;
    assert_eq!(r.kind, "research");
    assert!(r.fallback);
    assert_eq!(r.project.as_deref(), Some("project"));
    let r = route(&gw, &c, "promote", &[], true).await;
    assert_eq!(r.kind, "item");
    assert!(r.fallback);
    assert!(r.project.is_none());
    assert_eq!(
        route(&gw, &c, "usable", &[], false).await.model.as_deref(),
        Some("router-model")
    );
    assert!(route_many(&gw, &c, &texts, &[]).await.is_none());
    assert_eq!(gw.seen.lock().unwrap().len(), 5);
}

struct Models {
    seen: Mutex<Vec<(String, String, Option<String>)>>,
    mode: &'static str,
}
#[async_trait::async_trait]
impl OneModel for Models {
    async fn once(
        &self,
        c: &OneCall<'_>,
        model: &str,
        effort: Option<&str>,
        on: Option<OnText<'_>>,
    ) -> Tried {
        self.seen.lock().unwrap().push((
            c.harness.into(),
            model.into(),
            effort.map(str::to_string),
        ));
        if let Some(on) = on {
            on(model);
        }
        let usage = vec![ModelUsage {
            model: model.into(),
            tokens: steve_store::Tokens {
                input: Some(2),
                ..Default::default()
            },
            cost_reported: Some(0.1),
            detail: Value::Null,
        }];
        Tried {
            reply: if model == "good" {
                Ok(("answer".into(), Some(0.2)))
            } else if self.mode == "tools" {
                Err(CoreError::NeedsTools)
            } else {
                Err(CoreError::Msg("failed".into()))
            },
            usage,
            timed_out: self.mode == "timeout",
            reported_model: Some("served".into()),
        }
    }
}
#[tokio::test]
async fn fallback_keeps_consumed_usage_choice_and_timeout_or_tools_stop_rules() {
    for mode in ["error", "timeout", "tools"] {
        let g = Fallback {
            one: Models {
                seen: Mutex::new(Vec::new()),
                mode,
            },
            fast: vec!["bad".into(), "good".into()],
            smart: vec![],
            models: None,
        };
        let choice = steve_core::models::Choice {
            harness: "chosen-harness".into(),
            models: vec!["bad".into(), "good".into()],
            effort: Some("high".into()),
        };
        let shown = Mutex::new(Vec::new());
        let on = |s: &str| shown.lock().unwrap().push(s.to_string());
        let run = with_choice(
            choice,
            g.attempt(
                "question",
                Tier::Fast,
                Duration::from_secs(1),
                &ThinkTools::None,
                Some(&on),
            ),
        );
        let a = if mode == "tools" {
            steve_modules::gateway::hands_on_needs_tools(run).await
        } else {
            run.await
        };
        let n = if mode == "error" { 2 } else { 1 };
        assert_eq!(a.attempts, n);
        assert_eq!(a.usage.len(), n as usize);
        assert_eq!(g.one.seen.lock().unwrap().len(), n as usize);
        assert_eq!(
            g.one.seen.lock().unwrap()[0],
            ("chosen-harness".into(), "bad".into(), Some("high".into()))
        );
        assert_eq!(shown.lock().unwrap().len(), n as usize);
        if mode == "error" {
            let p = a.result.unwrap();
            assert_eq!(p.model, "good");
            assert_eq!(p.reported_model.as_deref(), Some("served"));
            assert_eq!(p.usage.len(), 2);
        } else {
            assert!(a.result.is_err());
            assert_eq!(a.failures.len(), 1);
            assert_eq!(a.failures[0].contains(TIMED_OUT), mode == "timeout");
        }
    }
}

#[tokio::test]
async fn local_acp_preserves_static_prompt_message_boundaries_and_stop_errors() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let adapter = dir.path().join("adapter.py");
    let log = dir.path().join("requests.jsonl");
    let code = r#"#!/usr/bin/env python3
import json, sys
log = LOG_PATH
def send(v): print(json.dumps(v), flush=True)
def chunk(sid, text, mid): send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','messageId':mid,'content':{'type':'text','text':text}}}})
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); p=m.get('params') or {}
 with open(log,'a') as f: f.write(json.dumps(m)+'\n')
 result={}
 if method=='initialize': result={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{'systemPrompt':{'mode':{'override':{},'append':{}}}}}}
 elif method=='session/new': result={'sessionId':'s','modes':{'currentModeId':'default','availableModes':[{'id':'plan','name':'Plan'}]}}
 elif method=='session/prompt':
  chunk('s','draft','a'); chunk('s','answer','b')
  result={'stopReason':'max_tokens' if any('bad-stop' in b.get('text','') for b in p['prompt']) else 'end_turn','usage':{'inputTokens':3,'outputTokens':2}}
 if 'id' in m: send({'jsonrpc':'2.0','id':m['id'],'result':result})
"#;
    std::fs::write(
        &adapter,
        code.replace("LOG_PATH", &serde_json::to_string(&log).unwrap()),
    )
    .unwrap();
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = steve_acp::AcpConfig {
        declared: HashMap::from([(
            "claude-code".into(),
            serde_json::from_value(
                json!({"properties":{"nested_sandbox":false,"self_read_only":true}}),
            )
            .unwrap(),
        )]),
        adapters: HashMap::from([("claude-code".into(), adapter)]),
        node: "node".into(),
        claude_executable: "/usr/bin/false".into(),
        steve_home: dir.path().join("home"),
        codex_home: dir.path().join("codex"),
        user_home: dir.path().to_path_buf(),
        version: "test".into(),
    };
    let g = LocalAcp {
        acp: cfg,
        cwd: dir.path().join("work"),
    };
    let prompt = with_static("stable head", "question");
    let shown = Mutex::new(Vec::new());
    let on = |s: &str| shown.lock().unwrap().push(s.to_string());
    let a = g
        .once(
            &OneCall {
                prompt: &prompt,
                timeout: Duration::from_secs(5),
                tools: &ThinkTools::None,
                harness: "claude-code",
                images: &[],
            },
            "",
            None,
            Some(&on),
        )
        .await;
    assert_eq!(a.reply.unwrap().0, "answer");
    assert_eq!(*shown.lock().unwrap(), ["draft", "answer"]);
    assert_eq!(a.usage[0].tokens.output, Some(2));
    let lines: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let sent = lines
        .iter()
        .find(|l| l["method"] == "session/prompt")
        .unwrap();
    assert_eq!(
        sent["params"]["prompt"],
        json!([{"type":"text","text":"question"}])
    );
    assert!(
        lines
            .iter()
            .find(|l| l["method"] == "session/new")
            .unwrap()
            .to_string()
            .contains("stable head")
    );
    let a = g
        .once(
            &OneCall {
                prompt: "bad-stop",
                timeout: Duration::from_secs(5),
                tools: &ThinkTools::None,
                harness: "claude-code",
                images: &[],
            },
            "",
            None,
            None,
        )
        .await;
    assert!(a.reply.unwrap_err().to_string().contains("max_tokens"));
    assert_eq!(a.usage[0].tokens.input, Some(3));
}
