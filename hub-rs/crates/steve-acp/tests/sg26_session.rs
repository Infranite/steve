//! Exercise real Conn NDJSON requests and public session::open against a finite scripted peer.
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use steve_acp::rpc::{Conn, Spawn};
use steve_acp::session::{self, ModeChoice, OpenConfig, Reports, SystemPrompt};
fn ok(method: &str, result: Value) -> Value {
    json!({"method":method,"result":result})
}
fn error(method: &str, code: i64) -> Value {
    json!({"method":method,"error":{"code":code,"message":"scripted refusal"}})
}
fn options(model_id: &str, model: &str, effort: &str) -> Value {
    json!([{"id":model_id,"category":"model","currentValue":model,"options":[{"value":"m1","name":"m1"}]},{"id":"reasoning","category":"thought_level","currentValue":effort,"options":[{"group":"levels","options":[{"value":"low"},{"value":"high"}]}]}])
}
struct Trace(PathBuf);
impl Trace {
    fn add(&self, kind: &str, value: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .unwrap();
        writeln!(f, "{}", json!({"callback":kind,"value":value})).unwrap();
    }
}
impl Reports for Trace {
    fn new_session(&self, id: &str) {
        self.add("new", id)
    }
    fn model(&self, id: &str) {
        self.add("model", id)
    }
    fn effort(&self, id: &str) {
        self.add("effort", id)
    }
}
struct Peer {
    conn: Arc<Conn>,
    trace: Trace,
    _dir: tempfile::TempDir,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.conn.kill();
    }
}
impl Peer {
    fn new(steps: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("steps.json");
        let log = dir.path().join("trace.jsonl");
        std::fs::write(&config, serde_json::to_vec(&steps).unwrap()).unwrap();
        let conn = Conn::spawn(
            Spawn {
                program: std::env::var_os("PYTHON")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "python3".into()),
                args: vec![
                    "-u".into(),
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/sg26/peer.py")
                        .into_os_string(),
                    config.into_os_string(),
                    log.clone().into_os_string(),
                ],
                cwd: dir.path().into(),
                env: vec![],
                log_tag: "sg26-scripted-peer".into(),
                clean_env: false,
                prefix: None,
            },
            |_, _| {},
            || {},
        )
        .unwrap();
        Self {
            conn,
            trace: Trace(log),
            _dir: dir,
        }
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.trace.0)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    async fn stop(&self) {
        self.conn.kill();
        tokio::time::timeout(Duration::from_secs(3), self.conn.wait_exit())
            .await
            .unwrap();
    }
}
fn mode(id: &str, fallback: Option<&str>, narrow: bool) -> ModeChoice {
    ModeChoice {
        requested: "plan",
        id: id.into(),
        fallback: fallback.map(String::from),
        narrow,
        blind: false,
        meta: None,
        native_execution: false,
    }
}
fn cfg<'a>(
    resume: Option<&'a str>,
    mode: Option<ModeChoice>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
) -> OpenConfig<'a> {
    OpenConfig {
        client_version: "sg26",
        cwd: "/virtual-work",
        resume,
        mcp_servers: json!([{ "name":"fake-mcp","url":"https://example.invalid/mcp"}]),
        mode,
        model,
        effort,
        system_prompt: None,
    }
}
fn labels(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|v| {
            if let Some(m) = v["request"].as_str() {
                format!("request:{m}")
            } else {
                format!(
                    "{}:{}",
                    v["callback"].as_str().unwrap(),
                    v["value"].as_str().unwrap()
                )
            }
        })
        .collect()
}
#[tokio::test]
async fn sg26_open_new_session_orders_callbacks_mode_model_and_effort_and_preserves_payloads() {
    let init = json!({"agentCapabilities":{"promptCapabilities":{"image":true},"sessionCapabilities":{"systemPrompt":{"mode":{"append":true}}}}});
    let initial = json!({"sessionId":"  mock-session  ","modes":{"availableModes":[{"id":"plan"}]},"models":{"currentModelId":"base"},"configOptions":options("model","base","low")});
    let peer = Peer::new(vec![
        ok("initialize", init),
        ok("session/new", initial),
        ok("session/set_mode", json!({})),
        ok(
            "session/set_config_option",
            json!({"configOptions":options("model","m1","low")}),
        ),
        ok(
            "session/set_config_option",
            json!({"configOptions":options("model","m1","high")}),
        ),
    ]);
    let mut c = cfg(
        None,
        Some(mode("plan", None, true)),
        Some("m1"),
        Some("high"),
    );
    c.system_prompt = Some(SystemPrompt {
        text: "system frame",
        replace: false,
    });
    let opened = tokio::time::timeout(
        Duration::from_secs(5),
        session::open(&peer.conn, c, Some(json!({"purpose":"sg26"})), &peer.trace),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            opened.acp_id.as_str(),
            opened.mode.as_deref(),
            opened.image,
            opened.system_prompt
        ),
        ("mock-session", Some("plan"), true, true)
    );
    let events = peer.events();
    assert_eq!(
        labels(&events),
        [
            "request:initialize",
            "request:session/new",
            "new:mock-session",
            "model:base",
            "request:session/set_mode",
            "request:session/set_config_option",
            "model:m1",
            "request:session/set_config_option",
            "effort:high"
        ]
    );
    assert_eq!(
        events[0]["params"],
        json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"_meta":{"parameterizedModelPicker":true}},"clientInfo":{"name":"steve","version":"sg26"}})
    );
    assert_eq!(
        events[1]["params"]["systemPrompt"],
        json!({"mode":"append","content":[{"type":"text","text":"system frame"}]})
    );
    assert_eq!(events[1]["params"]["_meta"], json!({"purpose":"sg26"}));
    assert_eq!(events[1]["params"]["mcpServers"][0]["name"], "fake-mcp");
    assert_eq!(
        events[4]["params"],
        json!({"sessionId":"mock-session","modeId":"plan"})
    );
    assert_eq!(events[5]["params"]["configId"], "model");
    assert_eq!(
        events[7]["params"],
        json!({"sessionId":"mock-session","configId":"reasoning","value":"high"})
    );
    peer.stop().await;
}
#[tokio::test]
async fn sg26_open_mode_method_not_found_falls_back_in_both_directions_and_uses_refreshed_config() {
    for prefer_mode in [false, true] {
        let mut initial = json!({"configOptions":[{"id":"permission","category":"mode","options":[{"value":"plan"}],"currentValue":"ask"},{"id":"old-model","category":"model","currentValue":"base"}]});
        if prefer_mode {
            initial["modes"] = json!({"availableModes":[{"id":"plan"}]});
        }
        let first = if prefer_mode {
            "session/set_mode"
        } else {
            "session/set_config_option"
        };
        let second = if prefer_mode {
            "session/set_config_option"
        } else {
            "session/set_mode"
        };
        let updated = json!({"configOptions":options("after-mode","base","low")});
        let peer = Peer::new(vec![
            ok("initialize", json!({})),
            ok("session/load", initial),
            error(first, -32601),
            ok(second, updated),
            ok(
                "session/set_config_option",
                json!({"configOptions":options("after-mode","m1","low")}),
            ),
        ]);
        let opened = session::open(
            &peer.conn,
            cfg(
                Some("saved-id"),
                Some(mode("plan", None, true)),
                Some("m1"),
                None,
            ),
            None,
            &peer.trace,
        )
        .await
        .unwrap();
        assert_eq!(opened.acp_id, "saved-id");
        let es = peer.events();
        let requests: Vec<_> = es.iter().filter(|v| v.get("request").is_some()).collect();
        assert_eq!(
            requests
                .iter()
                .map(|v| v["request"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "initialize",
                "session/load",
                first,
                second,
                "session/set_config_option"
            ]
        );
        assert_eq!(requests[4]["params"]["configId"], "after-mode");
        assert!(!es.iter().any(|e| e["callback"] == "new"));
        assert_eq!(labels(&es).last().unwrap(), "effort:low");
        peer.stop().await;
    }
}
#[tokio::test]
async fn sg26_open_model_alias_actual_mismatch_and_missing_confirmation_keep_the_original_rules() {
    for case in [
        "alias-config",
        "alias-models",
        "mismatch",
        "unreported",
        "fallback",
        "option-refused",
    ] {
        let option = case != "fallback";
        let mut initial = json!({"sessionId":"s","models":{"currentModelId":"base"}});
        if option {
            initial["configOptions"] = options("model", "base", "low");
        }
        let answer = match case {
            "alias-config" => {
                json!({"configOptions":[{"id":"model","category":"model","currentValue":"quick","options":[{"group":"g","options":[{"value":"quick","name":"wanted"}]}]}]})
            }
            "alias-models" => {
                json!({"models":{"currentModelId":"quick","availableModels":[{"modelId":"quick","name":"wanted"}]}})
            }
            "mismatch" => json!({"models":{"currentModelId":"other"}}),
            _ => json!({}),
        };
        let mut steps = vec![ok("initialize", json!({})), ok("session/new", initial)];
        if option {
            steps.push(if case == "option-refused" {
                error("session/set_config_option", -32601)
            } else {
                ok("session/set_config_option", answer)
            });
        } else {
            steps.push(error("session/set_model", -32601));
            steps.push(ok("session/set_config_option", answer));
        }
        let peer = Peer::new(steps);
        let result = session::open(
            &peer.conn,
            cfg(None, None, Some("wanted"), None),
            None,
            &peer.trace,
        )
        .await;
        let es = peer.events();
        if case == "mismatch" {
            assert!(
                result
                    .unwrap_err()
                    .message
                    .contains("请求 wanted，实际 other")
            );
            assert_eq!(labels(&es).last().unwrap(), "model:other");
        } else if case == "option-refused" {
            assert!(result.unwrap_err().message.contains("模型 wanted 没能生效"));
            assert_eq!(
                es.iter()
                    .filter(|v| v["request"] == "session/set_model")
                    .count(),
                0
            );
        } else {
            assert!(result.is_ok());
            assert!(labels(&es).contains(&"model:wanted".into()));
        }
        peer.stop().await;
    }
}
#[tokio::test]
async fn sg26_open_early_refusals_and_effort_confirmation_keep_error_order() {
    for case in [
        "init-error",
        "new-error",
        "load-error",
        "missing-id",
        "mode-refused",
        "mode-unavailable",
        "effort-unavailable",
        "effort-unsupported",
        "effort-wrong",
        "effort-missing",
    ] {
        let resume = (case == "load-error").then_some("saved-id");
        let mut initial = json!({"sessionId":"s","models":{"currentModelId":"base"},"modes":{"availableModes":[{"id":"plan"}]},"configOptions":options("model","base","low")});
        if case == "missing-id" {
            initial["sessionId"] = json!("  ");
        }
        if case == "effort-unavailable" {
            initial.as_object_mut().unwrap().remove("configOptions");
        }
        let mut steps = vec![if case == "init-error" {
            error("initialize", -32001)
        } else {
            ok("initialize", json!({}))
        }];
        if case != "init-error" {
            let method = if resume.is_some() {
                "session/load"
            } else {
                "session/new"
            };
            steps.push(if case == "new-error" || case == "load-error" {
                error(method, -32002)
            } else {
                ok(method, initial)
            });
        }
        if case == "mode-refused" {
            steps.push(error("session/set_mode", -32003));
        }
        if matches!(case, "effort-wrong" | "effort-missing") {
            steps.push(ok(
                "session/set_config_option",
                if case == "effort-wrong" {
                    json!({"configOptions":options("model","base","low")})
                } else {
                    json!({})
                },
            ));
        }
        let peer = Peer::new(steps);
        let chosen = match case {
            "mode-refused" => Some(mode("plan", None, true)),
            "mode-unavailable" => Some(mode("other", None, false)),
            _ => None,
        };
        let effort = if case.starts_with("effort-") {
            Some(if case == "effort-unsupported" {
                "extreme"
            } else {
                "high"
            })
        } else {
            None
        };
        let result = session::open(
            &peer.conn,
            cfg(resume, chosen, None, effort),
            None,
            &peer.trace,
        )
        .await;
        let e = result.unwrap_err().message;
        let es = peer.events();
        let wanted = match case {
            "missing-id" => "adapter returned no sessionId",
            "mode-refused" => "权限模式 plan 没能生效",
            "mode-unavailable" => "unsupported: permission mode other",
            "effort-unavailable" => "未报告可设置的思考强度",
            "effort-unsupported" => "不支持思考强度 extreme",
            "effort-wrong" => "实际 low",
            "effort-missing" => "实际 未知",
            _ => "scripted refusal",
        };
        assert!(e.contains(wanted), "{case}: {e}");
        if matches!(
            case,
            "init-error" | "new-error" | "load-error" | "missing-id"
        ) {
            assert!(!es.iter().any(|v| v.get("callback").is_some()));
        } else {
            assert_eq!(labels(&es)[2..4], ["new:s", "model:base"]);
        }
        if case == "effort-wrong" {
            assert_eq!(labels(&es).last().unwrap(), "effort:low");
        }
        peer.stop().await;
    }
}
#[tokio::test]
async fn sg26_open_narrow_mode_and_system_prompt_capability_do_not_invent_requests() {
    for allowed in [false, true] {
        let peer = Peer::new(vec![
            ok(
                "initialize",
                json!({"agentCapabilities":{"sessionCapabilities":{"systemPrompt":{"mode":{"override":allowed}}}}}),
            ),
            ok(
                "session/load",
                json!({"configOptions":options("model","base","low")}),
            ),
        ]);
        let mut c = cfg(Some(" saved "), Some(mode("plan", None, true)), None, None);
        c.system_prompt = Some(SystemPrompt {
            text: "replace frame",
            replace: true,
        });
        let opened = session::open(&peer.conn, c, Some(json!({"meta":"kept"})), &peer.trace)
            .await
            .unwrap();
        assert_eq!(opened.acp_id, " saved ");
        assert!(opened.mode.is_none());
        assert_eq!(opened.system_prompt, allowed);
        let es = peer.events();
        assert_eq!(
            labels(&es),
            [
                "request:initialize",
                "request:session/load",
                "model:base",
                "effort:low"
            ]
        );
        assert_eq!(es[1]["params"]["sessionId"], " saved ");
        assert_eq!(es[1]["params"]["_meta"], json!({"meta":"kept"}));
        assert_eq!(es[1]["params"].get("systemPrompt").is_some(), allowed);
        peer.stop().await;
    }
}
