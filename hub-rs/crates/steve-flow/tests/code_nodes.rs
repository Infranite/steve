//! S3 acceptance: a script with an endless loop is stopped within 1 s
//! and `fetch` is a ReferenceError; `http` to 127.0.0.1 (and localhost, private names, redirects to
//! them) is refused; `{{secrets}}` values appear nowhere in `context`, step output, decisions or
//! logs; an effect without a grant opens an approval, with one it runs (`http.effect`, `cmd.run`);
//! a secret without a `secret.use` grant opens an approval.
//!
//! The HTTP server is a tiny one on 127.0.0.1, reached through the name `api.test` (a fake resolver)
//! that the test puts in `http_allow_private`. Commands run only in temp dirs made here.

use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::NodeKind;
use steve_flow::nodes::cmd::{Cmd, CmdNode};
use steve_flow::nodes::http::{ALLOW_PRIVATE, Http, Lookup};
use steve_flow::testkit::FlowEnv;
use steve_modules::testkit::{ME, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::items::{NewField, NewItem};
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

const TOKEN: &str = "s3-SECRET-t0ken-9QxZ";

/// Every log line of this test binary.
static LOGS: LazyLock<Arc<Mutex<Vec<u8>>>> = LazyLock::new(|| {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let w = Capture(buf.clone());
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || w.clone())
            .with_ansi(false)
            .finish(),
    )
    .expect("subscriber");
    buf
});

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `api.test` and `rebind.test` → 127.0.0.1; anything else through the system.
struct FakeDns;

#[async_trait]
impl Lookup for FakeDns {
    async fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        match host {
            "api.test" | "rebind.test" | "other.test" => Ok(vec!["127.0.0.1".parse().unwrap()]),
            "meta.test" => Ok(vec![
                "93.184.216.34".parse().unwrap(),
                "169.254.169.254".parse().unwrap(),
            ]),
            h => steve_flow::nodes::http::SystemLookup.lookup(h).await,
        }
    }
}

/// What the server got: `(method, path, headers, body)`.
type Got = (String, String, HashMap<String, String>, String);

/// A one-request-per-connection HTTP/1.1 server: `/echo` answers with what it got (so the secret
/// comes back and must be redacted), `/to-ip` and `/to-name` redirect to internal targets,
/// `/claims` returns a claim id.
async fn server() -> (u16, Arc<Mutex<Vec<Got>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let got: Arc<Mutex<Vec<Got>>> = Arc::default();
    let g = got.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let g = g.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.split("\r\n");
                let first: Vec<&str> = lines.next().unwrap_or("").split(' ').collect();
                let mut headers = HashMap::new();
                for l in lines.filter(|l| !l.is_empty()) {
                    if let Some((k, v)) = l.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                let (method, path) = (first[0].to_string(), first[1].to_string());
                g.lock().unwrap().push((
                    method.clone(),
                    path.clone(),
                    headers.clone(),
                    body.clone(),
                ));
                let (status, extra, out) = match path.as_str() {
                    "/to-ip" => (
                        "302 Found",
                        format!("Location: http://127.0.0.1:{port}/echo\r\n"),
                        String::new(),
                    ),
                    "/to-name" => (
                        "302 Found",
                        format!("Location: http://rebind.test:{port}/echo\r\n"),
                        String::new(),
                    ),
                    "/to-other" => (
                        "302 Found",
                        format!("Location: http://other.test:{port}/echo\r\n"),
                        String::new(),
                    ),
                    "/claims" => (
                        "201 Created",
                        String::new(),
                        json!({"claimId": "CL-1", "status": "submitted"}).to_string(),
                    ),
                    "/fail" => ("500 Internal Server Error", String::new(), "boom".into()),
                    _ => (
                        "200 OK",
                        String::new(),
                        json!({"method": method, "auth": headers.get("authorization"), "body": body})
                            .to_string(),
                    ),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{out}",
                    out.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (port, got)
}

struct Env {
    e: FlowEnv,
    vault: Vault,
    item: String,
    work: TempDir,
    _keys: TempDir,
}

fn env() -> Env {
    env_with(|_| None)
}

/// [`env`], with `cmd` steps on the Node `node` gives for the project directory.
fn env_with(node: impl FnOnce(&std::path::Path) -> Option<Arc<dyn CmdNode>>) -> Env {
    LazyLock::force(&LOGS);
    // under STEVE_FORBID_REAL_HOSTS=1 the guard only knows loopback by name; these fake names are
    // answered by FakeDns above (loopback, or for meta.test an internal address the resolver refuses),
    // so none of them can reach a real host
    steve_modules::net::allow_test_names(&["api.test", "rebind.test", "other.test", "meta.test"]);
    // never the real keychain
    // SAFETY: set before any vault is opened; tests in this binary all want the same value.
    unsafe { std::env::set_var("STEVE_VAULT_KEYSTORE", "file") };
    let keys = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let on_node = node(work.path());
    let slot: Arc<Mutex<Option<Vault>>> = Arc::default();
    let kp = keys.path().to_path_buf();
    let s2 = slot.clone();
    let e = FlowEnv::with_late_nodes(Vec::new(), Vec::new(), move |env| {
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(&kp).unwrap()),
        );
        *s2.lock().unwrap() = Some(vault.clone());
        let http: Arc<dyn NodeKind> = Arc::new(
            Http::new(vault)
                .with_lookup(Arc::new(FakeDns))
                .with_timeout(Duration::from_secs(5)),
        );
        let cmd = Cmd::new(env.dir.path());
        let cmd: Arc<dyn NodeKind> = Arc::new(match on_node {
            Some(n) => cmd.with_node(n),
            None => cmd,
        });
        vec![http, cmd]
    });
    let vault = slot.lock().unwrap().clone().unwrap();
    vault.init(WS, ME).unwrap();
    let shared = vault
        .list_vaults(WS, ME)
        .unwrap()
        .into_iter()
        .find(|v| v.scope == "shared")
        .unwrap()
        .id;
    let field = |label: &str, kind: &str, value: &str| NewField {
        label: label.into(),
        kind: kind.into(),
        secret: false,
        value: Zeroizing::new(value.into()),
    };
    let item = vault
        .create_item(
            WS,
            ME,
            NewItem {
                vault_id: shared,
                template: "api_credential".into(),
                title: "报销系统令牌".into(),
                site: Some("api.test".into()),
                account_id: None,
                sensitivity: None,
                fields: vec![
                    field("名称", "text", "ci"),
                    field("令牌", "password", TOKEN),
                ],
            },
        )
        .unwrap()
        .id;
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, 1)",
                params![WS, ALLOW_PRIVATE, json!(["api.test", "other.test"]).to_string()],
            )?;
            c.execute(
                "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('prj_t', ?, 'T', ?, 1, 1)",
                params![WS, work.path().to_string_lossy()],
            )?;
            Ok(())
        })
        .unwrap();
    Env {
        e,
        vault,
        item,
        work,
        _keys: keys,
    }
}

async fn api(e: &FlowEnv, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(
        &e.router(),
        Some(&caller(WS, ME)),
        method,
        path,
        body.as_ref(),
    )
    .await
}

fn one_node(kind: &str, title: &str, config: Value) -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "n", "kind": kind, "title": title, "config": config}
        ],
        "edges": [{"from": "t", "to": "n"}]
    })
}

async fn create_flow(e: &FlowEnv, name: &str, graph: Value) -> String {
    let (st, v) = api(
        e,
        "POST",
        "/flows",
        Some(json!({ "name": name, "graph": graph })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v["id"].as_str().unwrap().to_string()
}

async fn start(e: &FlowEnv, flow: &str, input: Value) -> String {
    let (st, v) = api(
        e,
        "POST",
        &format!("/flows/{flow}/run"),
        Some(json!({ "input": input })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v["run_id"].as_str().unwrap().to_string()
}

fn q<T: rusqlite::types::FromSql>(e: &FlowEnv, sql: &str, p: impl rusqlite::Params) -> T {
    e.env
        .store()
        .with_conn(|c| Ok(c.query_row(sql, p, |r| r.get(0))?))
        .unwrap()
}

fn status(e: &FlowEnv, run: &str) -> String {
    q(e, "SELECT status FROM flow_runs WHERE id = ?", [run])
}

async fn settled(e: &FlowEnv, run: &str) -> String {
    for _ in 0..400 {
        let s = status(e, run);
        if s != "running" {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {run} never settled");
}

/// `(status, output, error)` of the node `n`'s last step.
fn step(e: &FlowEnv, run: &str) -> (String, Value, Option<String>) {
    e.env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, output, error FROM flow_run_steps WHERE run_id = ? AND node_id = 'n' ORDER BY seq DESC LIMIT 1",
                [run],
                |r| {
                    let o: String = r.get(1)?;
                    Ok((r.get(0)?, serde_json::from_str(&o).unwrap_or(Value::Null), r.get(2)?))
                },
            )?)
        })
        .unwrap()
}

fn open_gate(e: &FlowEnv, run: &str) -> Option<(String, String, String)> {
    e.env
        .store()
        .with_conn(|c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT id, question, coalesce(draft, '') FROM decisions WHERE subject_type = 'flow_step' AND subject_id = ? AND status = 'open'",
                [format!("{run}:n")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ))?)
        })
        .unwrap()
}

async fn answer(e: &FlowEnv, id: &str, choice: &str) {
    steve_modules::decisions::dispatch(&e.env.modules, &caller(WS, ME), id, choice, "")
        .expect("ours")
        .expect("resolved");
    e.flow.run_followups().await;
}

fn grant(e: &FlowEnv, flow: &str, capability: &str, rtype: &str, rid: &str) {
    e.env
        .store()
        .with_conn(|c| Ok(c.execute(
            "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created) VALUES (?, ?, 'flow', ?, ?, ?, ?, json_object('flow_version', 1), ?, ?, 1)",
            params![steve_store::uid("gr"), WS, flow, capability, rtype, rid, i64::MAX, ME],
        )?))
        .unwrap();
}

/// Every text column of the tables a run, its decisions and the vault's use log write.
fn all_rows_text(e: &FlowEnv) -> String {
    let mut out = String::new();
    e.env
        .store()
        .with_conn(|c| {
            for t in [
                "flow_runs",
                "flow_run_steps",
                "flow_waits",
                "decisions",
                "secret_uses",
                "audit",
            ] {
                let Ok(mut st) = c.prepare(&format!("SELECT * FROM {t}")) else {
                    continue;
                };
                let n = st.column_count();
                let mut rows = st.query([])?;
                while let Some(r) = rows.next()? {
                    for i in 0..n {
                        if let Ok(Some(s)) = r.get::<_, Option<String>>(i) {
                            out.push_str(&s);
                            out.push('\n');
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    out
}

fn logs() -> String {
    String::from_utf8_lossy(&LOGS.lock().unwrap()).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn script_endless_loop_is_stopped_and_fetch_is_not_there() {
    let Env { e, .. } = env();
    let f = create_flow(
        &e,
        "死循环",
        one_node(
            "script",
            "转圈",
            json!({"code": "export const handler = async (input) => { while (true) {} }"}),
        ),
    )
    .await;
    let t = Instant::now();
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "failed");
    assert!(
        t.elapsed() < Duration::from_millis(1800),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(
        step(&e, &run).2.as_deref(),
        Some(steve_flow::nodes::script::TIMED_OUT)
    );

    let f = create_flow(
        &e,
        "联网",
        one_node(
            "script",
            "偷偷联网",
            json!({"code": "export const handler = async () => await fetch('http://127.0.0.1:9/')"}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "failed");
    let err = step(&e, &run).2.unwrap();
    assert!(
        err.contains("ReferenceError") && err.contains("fetch"),
        "{err}"
    );

    // a working script's return value is the step output
    let f = create_flow(
        &e,
        "解析",
        one_node(
            "script",
            "解析发票",
            json!({"code": "export const handler = async (input) => ({ amount: input.amount * 2 })"}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({"amount": 43.25})).await;
    assert_eq!(settled(&e, &run).await, "done");
    assert_eq!(step(&e, &run).1, json!({"amount": 86.5}));
}

#[tokio::test(flavor = "multi_thread")]
async fn http_to_internal_addresses_is_refused() {
    let Env { e, .. } = env();
    let (port, got) = server().await;
    for url in [
        format!("http://127.0.0.1:{port}/echo"),
        format!("http://localhost:{port}/echo"),
        format!("http://[::1]:{port}/echo"),
        format!("http://2130706433:{port}/echo"),
        "http://169.254.169.254/latest/meta-data/".to_string(),
        format!("http://rebind.test:{port}/echo"),
        "http://meta.test/".to_string(),
        // allowed host, but the redirect leads inside
        format!("http://api.test:{port}/to-ip"),
        format!("http://api.test:{port}/to-name"),
    ] {
        let f = create_flow(&e, "内网", one_node("http", "取数据", json!({"url": url}))).await;
        let run = start(&e, &f, json!({})).await;
        assert_eq!(settled(&e, &run).await, "failed", "{url}");
        let err = step(&e, &run).2.unwrap();
        assert!(err.starts_with("不能请求内网地址"), "{url}: {err}");
    }
    // only the two allowed first hops reached the server; nothing followed a redirect
    let paths: Vec<String> = got.lock().unwrap().iter().map(|g| g.1.clone()).collect();
    assert_eq!(paths, vec!["/to-ip", "/to-name"]);

    // the allow-listed host itself works
    let f = create_flow(
        &e,
        "允许",
        one_node(
            "http",
            "取数据",
            json!({"url": format!("http://api.test:{port}/echo")}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "done");
    let (_, out, _) = step(&e, &run);
    assert_eq!(out["status"], 200);
    assert_eq!(out["method"], "GET");

    // a POST without effect is not even saved
    let (st, v) = api(
        &e,
        "POST",
        "/flows",
        Some(json!({"name": "x", "graph": one_node("http", "提交", json!({"method": "POST", "url": "https://a.example/"}))})),
    )
    .await;
    assert_eq!(st, 400, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_are_injected_but_never_stored_or_logged() {
    let Env { e, item, vault, .. } = env();
    let (port, got) = server().await;
    let cfg = json!({
        "url": format!("http://api.test:{port}/echo"),
        "headers": {"Authorization": format!("Bearer {{{{secrets.{item}.令牌}}}}")},
        "body": "note={{input.note}}",
    });
    let f = create_flow(&e, "查报销", one_node("http", "查询报销单", cfg)).await;
    // event text that names the secret must stay text
    let sneaky = format!("{{{{secrets.{item}.令牌}}}}");

    // no secret.use grant: the run waits on an approval that names the item
    let run = start(&e, &f, json!({"note": sneaky})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    assert!(
        got.lock().unwrap().is_empty(),
        "nothing sent before approval"
    );
    let (dec, question, draft) = open_gate(&e, &run).expect("an approval");
    assert_eq!(
        question,
        "「查报销」的「查询报销单」要用保险箱里的「报销系统令牌」。要执行吗？"
    );
    assert!(draft.contains("令牌"), "{draft}");
    answer(&e, &dec, "执行").await;
    assert_eq!(settled(&e, &run).await, "done");

    // the server got the value, and the event's template text unexpanded
    let (_, _, headers, body) = got.lock().unwrap()[0].clone();
    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(body, format!("note={sneaky}"));

    // the echo came back with the value: the step output has it masked
    let (st, out, _) = step(&e, &run);
    assert_eq!(st, "ok");
    assert_eq!(out["auth"], "Bearer [已打码：令牌]", "{out}");

    // a dry run neither asks for nor releases the secret
    let (st, v) = api(
        &e,
        "POST",
        &format!("/flows/{f}/dry-run"),
        Some(json!({"sample": {}})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let dry = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        step(&e, &dry).1,
        json!({"skipped": steve_flow::nodes::http::DRY_SECRET})
    );
    assert!(open_gate(&e, &dry).is_none());
    assert_eq!(got.lock().unwrap().len(), 1);

    // a grant for the flow: no approval the second time
    e.env
        .store()
        .with_conn(|c| Ok(c.execute(
            "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created) VALUES ('gr_s', ?, 'flow', ?, 'secret.use', 'vault_item', ?, json_object('flow_version', 1), ?, ?, 1)",
            params![WS, f, item, i64::MAX, ME],
        )?))
        .unwrap();
    let run2 = start(&e, &f, json!({"note": "hi"})).await;
    assert_eq!(settled(&e, &run2).await, "done");
    assert!(open_gate(&e, &run2).is_none());
    assert_eq!(got.lock().unwrap().len(), 2);

    // secret headers never follow a redirect to another host
    let cfg = json!({
        "url": format!("http://api.test:{port}/to-other"),
        "headers": {"Authorization": format!("Bearer {{{{secrets.{item}.令牌}}}}")},
    });
    let f3 = create_flow(&e, "跳转", one_node("http", "查询报销单", cfg)).await;
    e.env
        .store()
        .with_conn(|c| Ok(c.execute(
            "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created) VALUES ('gr_r', ?, 'flow', ?, 'secret.use', 'vault_item', ?, json_object('flow_version', 1), ?, ?, 1)",
            params![WS, f3, item, i64::MAX, ME],
        )?))
        .unwrap();
    let run3 = start(&e, &f3, json!({})).await;
    assert_eq!(settled(&e, &run3).await, "done");
    assert_eq!(step(&e, &run3).1["status"], 302);
    let paths: Vec<String> = got.lock().unwrap().iter().map(|g| g.1.clone()).collect();
    assert_eq!(paths.last().map(String::as_str), Some("/to-other"));
    assert_eq!(got.lock().unwrap().len(), 3);

    // the use log: one row per release, the flow as the actor, the run linked, no value
    let n: i64 = q(
        &e,
        "SELECT count(*) FROM secret_uses WHERE actor_type = 'flow' AND actor_id = ? AND status = 'ok' AND mode = 'internal' AND run_id IN (?, ?)",
        params![f, run, run2],
    );
    assert_eq!(n, 2);
    let g: Option<String> = q(
        &e,
        "SELECT grant_id FROM secret_uses WHERE run_id = ?",
        [&run2],
    );
    assert_eq!(g.as_deref(), Some("gr_s"));

    // the value is nowhere: context, steps, waits, decisions, use log, audit, logs
    let rows = all_rows_text(&e);
    assert!(rows.contains("[已打码：令牌]"), "the check reads the steps");
    assert!(!rows.contains(TOKEN), "a secret value is in the database");
    let ctx: String = q(&e, "SELECT context FROM flow_runs WHERE id = ?", [&run]);
    assert!(ctx.contains("[已打码：令牌]") && !ctx.contains(TOKEN));
    assert!(!logs().contains(TOKEN), "a secret value is in the logs");
    drop(vault);
}

#[tokio::test(flavor = "multi_thread")]
async fn effect_requests_need_approval_or_a_grant() {
    let Env { e, work, .. } = env();
    let (port, got) = server().await;
    let url = format!("http://api.test:{port}/claims");
    let f = create_flow(
        &e,
        "提交报销",
        one_node(
            "http",
            "提交到报销系统",
            json!({"method": "POST", "url": url, "effect": true, "body": {"amount": "{{input.amount}}"}}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({"amount": 86})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    assert!(got.lock().unwrap().is_empty());
    let (dec, question, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(
        question,
        "「提交报销」要执行「提交到报销系统」，这是对外动作。要执行吗？"
    );
    answer(&e, &dec, "执行").await;
    assert_eq!(settled(&e, &run).await, "done");
    let (_, out, _) = step(&e, &run);
    assert_eq!(out["claimId"], "CL-1");
    assert_eq!(out["status"], 201);
    let (m, _, h, body) = got.lock().unwrap()[0].clone();
    assert_eq!(m, "POST");
    assert_eq!(h["content-type"], "application/json");
    assert_eq!(body, r#"{"amount":"86"}"#);

    // 不执行: nothing sent
    let run = start(&e, &f, json!({"amount": 1})).await;
    settled(&e, &run).await;
    let (dec, _, _) = open_gate(&e, &run).unwrap();
    answer(&e, &dec, "不执行").await;
    assert_eq!(status(&e, &run), "done");
    assert_eq!(got.lock().unwrap().len(), 1);

    // http.effect grant for the host: runs without asking
    grant(&e, &f, "http.effect", "host", "api.test");
    let run = start(&e, &f, json!({"amount": 2})).await;
    assert_eq!(settled(&e, &run).await, "done");
    assert_eq!(got.lock().unwrap().len(), 2);

    cmd_steps(&e, work.path()).await;
}

/// The `cmd` steps: approval, grants, a failing command, the timeout.
async fn cmd_steps(e: &FlowEnv, work: &std::path::Path) {
    // cmd: approval, then runs in the project dir; event text is one shell word
    let f = create_flow(
        e,
        "写文件",
        one_node(
            "cmd",
            "写下名字",
            json!({"command": "printf '%s' {{input.name}} > out.txt && echo ok", "project_id": "prj_t", "timeout_s": 10}),
        ),
    )
    .await;
    let name = "x'; touch pwned; echo '";
    let run = start(e, &f, json!({"name": name})).await;
    assert_eq!(settled(e, &run).await, "waiting");
    assert!(!work.join("out.txt").exists());
    let (dec, question, _) = open_gate(e, &run).unwrap();
    assert_eq!(
        question,
        "「写文件」要执行「写下名字」，这是对外动作。要执行吗？"
    );
    answer(e, &dec, "执行").await;
    assert_eq!(settled(e, &run).await, "done");
    assert_eq!(std::fs::read_to_string(work.join("out.txt")).unwrap(), name);
    assert!(!work.join("pwned").exists());
    let (_, out, _) = step(e, &run);
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["stdout"], "ok\n");

    // cmd.run grant on the project: direct; a failing command fails the step with its stderr
    grant(e, &f, "cmd.run", "project", "prj_t");
    let run = start(e, &f, json!({"name": "again"})).await;
    assert_eq!(settled(e, &run).await, "done");
    let f2 = create_flow(
        e,
        "失败",
        one_node(
            "cmd",
            "会失败",
            json!({"command": "echo nope >&2; exit 3", "project_id": "prj_t"}),
        ),
    )
    .await;
    grant(e, &f2, "cmd.run", "project", "*");
    let run = start(e, &f2, json!({})).await;
    assert_eq!(settled(e, &run).await, "failed");
    assert_eq!(step(e, &run).2.as_deref(), Some("命令退出码 3：nope"));

    // a timeout kills it
    let f3 = create_flow(
        e,
        "太久",
        one_node(
            "cmd",
            "睡觉",
            json!({"command": "sleep 30", "project_id": "prj_t", "timeout_s": 1}),
        ),
    )
    .await;
    grant(e, &f3, "cmd.run", "project", "prj_t");
    let t = Instant::now();
    let run = start(e, &f3, json!({})).await;
    assert_eq!(settled(e, &run).await, "failed");
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(step(e, &run).2.as_deref(), Some("命令超过 1 秒，已停止"));
}

#[tokio::test(flavor = "multi_thread")]
async fn effect_approval_names_the_secrets_it_releases() {
    let Env { e, item, .. } = env();
    let (port, got) = server().await;
    // 令牌 twice (header and body), 名称 once: template order, each once, the item's title
    let cfg = json!({
        "method": "POST",
        "url": format!("http://api.test:{port}/claims"),
        "effect": true,
        "headers": {"Authorization": format!("Bearer {{{{secrets.{item}.令牌}}}}")},
        "body": {"who": format!("{{{{secrets.{item}.名称}}}}"), "again": format!("{{{{ secrets.{item}.令牌 }}}}")},
    });
    let f = create_flow(&e, "提交报销", one_node("http", "提交到报销系统", cfg)).await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (dec, question, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(
        question,
        "「提交报销」要执行「提交到报销系统」，这是对外动作。要执行吗？会用到机密：报销系统令牌·令牌、报销系统令牌·名称"
    );
    // one approval covers the effect and the secrets: sent, no second card
    answer(&e, &dec, "执行").await;
    assert_eq!(settled(&e, &run).await, "done");
    assert!(open_gate(&e, &run).is_none());
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(
        got.lock().unwrap()[0].2["authorization"],
        format!("Bearer {TOKEN}")
    );

    // an item the flow can't read: the reference as written
    let cfg = json!({
        "method": "POST",
        "url": format!("http://api.test:{port}/claims"),
        "effect": true,
        "headers": {"X-Key": "{{secrets.vi_missing.key}}"},
    });
    let f = create_flow(&e, "缺条目", one_node("http", "提交", cfg)).await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (_, question, _) = open_gate(&e, &run).expect("an approval");
    assert!(
        question.ends_with("要执行吗？会用到机密：vi_missing·key"),
        "{question}"
    );
}

// ── `cmd` on a Node ──

mod on_node {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_flow::nodes::cmd::{CmdRan, CmdRequest, NO_SECRETS, OFFLINE, UNKNOWN};
    use steve_node::{LocalNode, NodeConfig, Root, Storage};
    use steve_node_api::testkit::RecordingSink;
    use steve_node_api::testkit::build::policy;
    use steve_node_api::*;

    fn wall() -> i64 {
        steve_store::now()
    }

    fn command<B>(op: &str, body: B) -> Command<B> {
        Command {
            id: OperationId::new(op).unwrap(),
            issued_at: Timestamp(wall()),
            accept_before: Timestamp(wall() + 60_000),
            grant: GrantId::new("hub").unwrap(),
            body,
        }
    }

    async fn finished(n: &LocalNode, op: &OperationId) -> Observed {
        loop {
            let r = n.operation(op.clone()).await.unwrap();
            if r.state.is_finished() {
                return r.state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The Hub's side reduced to what a `cmd` step needs of the Node: the project directory as a
    /// workspace, one command per run and step.
    pub struct LocalCmd {
        node: LocalNode,
        pub runs: AtomicUsize,
        _home: TempDir,
    }

    impl LocalCmd {
        pub fn open(project: &std::path::Path) -> Arc<LocalCmd> {
            let home = tempfile::tempdir().unwrap();
            let node = LocalNode::open(NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: home.path().join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: HashMap::new(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: home.path().join("steve"),
                    codex_home: home.path().join("codex"),
                    user_home: home.path().to_path_buf(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: vec![Root {
                    id: RootId::new("prj").unwrap(),
                    path: project.to_path_buf(),
                    display: "prj".into(),
                    access: RootAccess::ReadWrite,
                }],
                capacity: Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 4,
                },
                storage: Storage::default(),
                jobs: Default::default(),
            })
            .unwrap();
            node.connect(Arc::new(RecordingSink::default()), None);
            Arc::new(LocalCmd {
                node,
                runs: AtomicUsize::new(0),
                _home: home,
            })
        }
    }

    #[async_trait]
    impl CmdNode for LocalCmd {
        async fn run(&self, req: CmdRequest) -> CmdRan {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let resource = format!("project:{}", req.project_id);
            let prep = command(
                &derived_id("op_prep", &format!("{}/{}", req.run_id, req.step)),
                Prepare {
                    resource,
                    kind: PrepareKind::Existing {
                        dir: RootPath {
                            root: RootId::new("prj").unwrap(),
                            rel: RelPath::new("").unwrap(),
                        },
                    },
                },
            );
            self.node.prepare_workspace(prep.clone()).await.unwrap();
            let Observed::Finished {
                result: Some(OperationResult::Workspace(w)),
                ..
            } = finished(&self.node, &prep.id).await
            else {
                return CmdRan::NotRun("prepare".into());
            };
            let c = command(
                &derived_id("op_cmd", &format!("{}/{}", req.run_id, req.step)),
                ExecSpec {
                    workspace: w.workspace,
                    command: req.command,
                    cwd: None,
                    policy: policy(),
                    limits: Limits {
                        deadline: Timestamp(wall() + req.timeout.as_millis() as i64),
                        offline_until: Timestamp(wall() + req.timeout.as_millis() as i64),
                        max_output_bytes: 64 * 1024,
                        max_tokens: None,
                        max_cost: None,
                    },
                    env: vec![],
                    secrets: vec![],
                },
            );
            if let Err(e) = self.node.exec(c.clone()).await {
                return CmdRan::NotRun(e.to_string());
            }
            match finished(&self.node, &c.id).await {
                Observed::Finished {
                    outcome: Outcome::TimedOut,
                    ..
                } => CmdRan::TimedOut,
                Observed::Finished {
                    outcome: Outcome::Succeeded,
                    result: Some(OperationResult::Exec(r)),
                    ..
                } => CmdRan::Ended {
                    exit_code: r.exit_code,
                    stdout: r.stdout,
                    stderr: r.stderr,
                },
                Observed::Finished { error: Some(e), .. } => CmdRan::NotRun(e.to_string()),
                _ => CmdRan::Unknown,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cmd_steps_are_the_same_on_the_local_node() {
        let slot: Arc<Mutex<Option<Arc<LocalCmd>>>> = Arc::default();
        let s2 = slot.clone();
        let Env { e, work, .. } = env_with(move |dir| {
            let n = LocalCmd::open(dir);
            *s2.lock().unwrap() = Some(n.clone());
            Some(n)
        });
        cmd_steps(&e, work.path()).await;
        let n = slot.lock().unwrap().clone().unwrap();
        assert_eq!(
            n.runs.load(Ordering::SeqCst),
            4,
            "every step that ran went to the Node"
        );
    }

    /// Answers every step with what it was given, counting the asks.
    struct Says(CmdRan, AtomicUsize);

    #[async_trait]
    impl CmdNode for Says {
        async fn run(&self, _: CmdRequest) -> CmdRan {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.clone()
        }
    }

    /// Hangs on the first ask (the Hub "restarts" while it runs), then answers; keeps every ask.
    struct Hangs(Mutex<Vec<CmdRequest>>);

    #[async_trait]
    impl CmdNode for Hangs {
        async fn run(&self, req: CmdRequest) -> CmdRan {
            let first = {
                let mut got = self.0.lock().unwrap();
                got.push(req);
                got.len() == 1
            };
            if first {
                std::future::pending::<()>().await;
            }
            CmdRan::Ended {
                exit_code: Some(0),
                stdout: "again\n".into(),
                stderr: String::new(),
            }
        }
    }

    /// A restart cut a step on a Node short, and you asked for it to run again: it is asked anew
    /// (a new rerun, so a new operation), never answered with the interrupted one's outcome.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_retry_after_a_restart_runs_the_step_anew() {
        let hangs = Arc::new(Hangs(Mutex::new(vec![])));
        let h2 = hangs.clone();
        let Env { e, .. } = env_with(move |_| Some(h2 as Arc<dyn CmdNode>));
        let f = create_flow(
            &e,
            "跑",
            one_node(
                "cmd",
                "写",
                json!({"command": "true", "project_id": "prj_t"}),
            ),
        )
        .await;
        grant(&e, &f, "cmd.run", "project", "prj_t");
        let run = start(&e, &f, json!({})).await;
        for _ in 0..200 {
            if !hangs.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(hangs.0.lock().unwrap().len(), 1);
        // the Hub restarts: the run stopped inside its cmd step, and you are asked
        assert!(steve_flow::engine::recover(&e.flow).unwrap().is_empty());
        assert_eq!(status(&e, &run), "failed");
        let dec: String = q(
            &e,
            "SELECT id FROM decisions WHERE subject_type = 'flow_run' AND subject_id = ? AND status = 'open'",
            [&run],
        );
        answer(&e, &dec, steve_flow::engine::RETRY).await;
        assert_eq!(settled(&e, &run).await, "done");
        let got = hangs.0.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].rerun, got[1].rerun), (0, 1));
        assert_eq!((&got[1].run_id, &got[1].step), (&run, &"n".to_string()));
        assert_eq!(step(&e, &run).1["stdout"], "again\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_offline_or_lost_node_fails_the_step_once() {
        for (said, error) in [(CmdRan::Offline, OFFLINE), (CmdRan::Unknown, UNKNOWN)] {
            let says = Arc::new(Says(said, AtomicUsize::new(0)));
            let s2 = says.clone();
            let Env { e, work, .. } = env_with(move |_| Some(s2 as Arc<dyn CmdNode>));
            let f = create_flow(
                &e,
                "跑",
                one_node(
                    "cmd",
                    "写",
                    json!({"command": "touch here", "project_id": "prj_t"}),
                ),
            )
            .await;
            grant(&e, &f, "cmd.run", "project", "prj_t");
            let run = start(&e, &f, json!({})).await;
            assert_eq!(settled(&e, &run).await, "failed");
            assert_eq!(step(&e, &run).2.as_deref(), Some(error));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(says.1.load(Ordering::SeqCst), 1, "asked once, never again");
            assert!(!work.path().join("here").exists(), "not run on the Hub");
            // a secret in the command is still refused before anything is asked
            let (st, v) = api(
                &e,
                "POST",
                "/flows",
                Some(json!({"name": "机密", "graph": one_node(
                    "cmd",
                    "带机密",
                    json!({"command": "echo {{secrets.vi_1.token}}", "project_id": "prj_t"}),
                )})),
            )
            .await;
            assert_eq!(st, 400, "{v}");
            assert!(v.to_string().contains(NO_SECRETS), "{v}");
            assert_eq!(says.1.load(Ordering::SeqCst), 1);
        }
    }
}

/// The options of an open decision.
fn options_of(e: &FlowEnv, id: &str) -> Vec<String> {
    let o: Option<String> = q(e, "SELECT options FROM decisions WHERE id = ?", [id]);
    serde_json::from_str(&o.unwrap_or_default()).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn always_on_the_gate_grants_the_current_version_only() {
    let Env { e, work, .. } = env();
    let graph = |text: &str| {
        one_node(
            "cmd",
            "写下名字",
            json!({"command": format!("printf {text} >> out.txt"), "project_id": "prj_t", "timeout_s": 10}),
        )
    };
    let f = create_flow(&e, "每日记录", graph("a")).await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (dec, _, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(
        options_of(&e, &dec),
        vec!["执行", steve_flow::engine::GATE_ALWAYS, "不执行"]
    );
    answer(&e, &dec, steve_flow::engine::GATE_ALWAYS).await;
    assert_eq!(settled(&e, &run).await, "done");
    // the grant: this flow, cmd.run on the project, version 1, from this decision
    let (cap, rid, ver, by): (String, String, i64, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT capability, resource_id, json_extract(scope, '$.flow_version'), decision_id FROM grants WHERE subject_type = 'flow' AND subject_id = ?",
                [&f],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!((cap.as_str(), rid.as_str(), ver), ("cmd.run", "prj_t", 1));
    assert_eq!(by, dec);

    // the next run of the same version runs unattended
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "done");
    assert!(open_gate(&e, &run).is_none());
    assert_eq!(
        std::fs::read_to_string(work.path().join("out.txt")).unwrap(),
        "aa"
    );

    // an edit is a new version: the grant no longer covers it
    let (st, v) = api(
        &e,
        "PUT",
        &format!("/flows/{f}"),
        Some(json!({ "graph": graph("b") })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    assert!(open_gate(&e, &run).is_some());
    assert_eq!(
        std::fs::read_to_string(work.path().join("out.txt")).unwrap(),
        "aa"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn http_with_a_templated_host_offers_no_grant() {
    let Env { e, .. } = env();
    let f = create_flow(
        &e,
        "转发",
        one_node(
            "http",
            "发出去",
            json!({"method": "POST", "url": "{{input.url}}", "effect": true}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({"url": "http://api.test:1/claims"})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (dec, _, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(options_of(&e, &dec), vec!["执行", "不执行"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_always_grants_normal_items_never_high_ones() {
    let Env { e, item, vault, .. } = env();
    let (port, got) = server().await;
    let cfg = |item: &str| {
        json!({
            "url": format!("http://api.test:{port}/echo"),
            "headers": {"Authorization": format!("Bearer {{{{secrets.{item}.令牌}}}}")},
        })
    };
    let f = create_flow(&e, "查报销", one_node("http", "查询报销单", cfg(&item))).await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (dec, _, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(
        options_of(&e, &dec),
        vec!["执行", steve_flow::engine::GATE_ALWAYS, "不执行"]
    );
    answer(&e, &dec, steve_flow::engine::GATE_ALWAYS).await;
    assert_eq!(settled(&e, &run).await, "done");
    let run2 = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run2).await, "done");
    assert!(open_gate(&e, &run2).is_none());
    assert_eq!(got.lock().unwrap().len(), 2);

    // a high item: approve each time, no grant offered
    let shared = vault
        .list_vaults(WS, ME)
        .unwrap()
        .into_iter()
        .find(|v| v.scope == "shared")
        .unwrap()
        .id;
    let high = vault
        .create_item(
            WS,
            ME,
            NewItem {
                vault_id: shared,
                template: "api_credential".into(),
                title: "要紧的令牌".into(),
                site: Some("api.test".into()),
                account_id: None,
                sensitivity: Some("high".into()),
                fields: vec![NewField {
                    label: "令牌".into(),
                    kind: "password".into(),
                    secret: false,
                    value: Zeroizing::new(TOKEN.into()),
                }],
            },
        )
        .unwrap()
        .id;
    let f2 = create_flow(&e, "要紧", one_node("http", "查询报销单", cfg(&high))).await;
    let run = start(&e, &f2, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let (dec, _, _) = open_gate(&e, &run).expect("an approval");
    assert_eq!(options_of(&e, &dec), vec!["执行", "不执行"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approve_node_on_the_path_releases_the_secrets() {
    let Env { e, item, .. } = env();
    let (port, got) = server().await;
    let graph = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "a", "kind": "approve", "title": "要查吗", "config": {"question": "要查吗？", "options": ["查", "不查"]}},
            {"id": "n", "kind": "http", "title": "查询报销单", "config": {
                "url": format!("http://api.test:{port}/echo"),
                "headers": {"Authorization": format!("Bearer {{{{secrets.{item}.令牌}}}}")},
            }}
        ],
        "edges": [{"from": "t", "to": "a"}, {"from": "a", "to": "n", "label": "查"}]
    });
    let f = create_flow(&e, "查报销", graph).await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "waiting");
    let dec: String = q(
        &e,
        "SELECT id FROM decisions WHERE subject_type = 'flow_step' AND subject_id = ? AND status = 'open'",
        [format!("{run}:a")],
    );
    answer(&e, &dec, "查").await;
    assert_eq!(settled(&e, &run).await, "done");
    assert!(
        open_gate(&e, &run).is_none(),
        "no second card for the secret"
    );
    assert_eq!(got.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_private_is_a_setting_only_the_owner_writes() {
    let Env { e, .. } = env();
    let (port, got) = server().await;
    // the env put api.test and other.test in; the setting reads back sorted
    let (st, v) = api(&e, "GET", "/flow-settings/allow-private", None).await;
    assert_eq!(
        (st, v["hosts"].clone()),
        (200, json!(["api.test", "other.test"]))
    );
    // the owner takes api.test out: the same request is now refused
    let (st, v) = api(
        &e,
        "PUT",
        "/flow-settings/allow-private",
        Some(json!({"hosts": [" Other.Test. ", "other.test", ""]})),
    )
    .await;
    assert_eq!(
        (st, v["hosts"].clone()),
        (200, json!(["other.test"])),
        "{v}"
    );
    let f = create_flow(
        &e,
        "内网",
        one_node(
            "http",
            "查询",
            json!({"url": format!("http://api.test:{port}/echo")}),
        ),
    )
    .await;
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "failed");
    assert!(
        step(&e, &run)
            .2
            .unwrap_or_default()
            .contains("允许访问的内网主机"),
        "{:?}",
        step(&e, &run)
    );
    assert!(got.lock().unwrap().is_empty());
    // back in: it goes through
    let (st, _) = api(
        &e,
        "PUT",
        "/flow-settings/allow-private",
        Some(json!({"hosts": ["api.test"]})),
    )
    .await;
    assert_eq!(st, 200);
    let run = start(&e, &f, json!({})).await;
    assert_eq!(settled(&e, &run).await, "done");
    // a bad host, and a member who is not the owner
    let (st, _) = api(
        &e,
        "PUT",
        "/flow-settings/allow-private",
        Some(json!({"hosts": ["http://x/"]})),
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, steve_modules::testkit::OTHER)),
        "PUT",
        "/flow-settings/allow-private",
        Some(&json!({"hosts": ["nas.local"]})),
    )
    .await;
    assert_eq!(st, 403);
    let n: i64 = q(
        &e,
        "SELECT count(*) FROM audit WHERE action = 'flow.allow_private'",
        [],
    );
    assert_eq!(n, 2);
}
