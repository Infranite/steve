//! V4 acceptance tests (accounts-vault.md §11): no grant → decision + END; `grant_1h` and the second
//! use runs directly; injected values and their base64 are masked; cwd escape 400; `high` grants
//! 400; temp dir gone after the run; bank cards always ask; deny; restart expires waiting requests;
//! pasted `ghp_…` intercepted and not in the database; no value in any table or file.
//!
//! Every command runs in a temp dir created here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use data_encoding::BASE64;
use rusqlite::params;
use serde_json::{Value, json};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_modules::vault::grants::GRANT_HIGH;
use steve_modules::vault::inject::{CWD_OUTSIDE, secrets_root};
use steve_modules::vault::items::{NewField, NewItem};
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::vault::mcp::{self, ToolOut};
use steve_modules::vault::r#use::{NO_SUCH, SUBJECT, denied_message};
use steve_modules::vault::{SecretUses, SessionSink, UseCx, Vault, guard, redact, use_api};
use steve_modules::{Modules, Registry, decisions};
use tempfile::TempDir;
use zeroize::Zeroizing;

const TOKEN: &str = "ghs-test-T0KEN-value-77Aq";
const CARD: &str = "4111111111114821";
const CVV: &str = "CVV-secret-913";
const SESSION: &str = "ses_v4test";
const WORK_ITEM: &str = "itm_v4test";
const PROJECT: &str = "prj_v4test";
const BOT: &str = "fe";

/// Every log line of this test binary (all threads), for the leak check.
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

/// `(ws, item, session, text)` of one message to a session.
type Sent = (String, Option<String>, Option<String>, String);

#[derive(Default)]
struct Sink(Mutex<Vec<Sent>>);

#[async_trait]
impl SessionSink for Sink {
    async fn send(&self, ws: &str, item: Option<&str>, session: Option<&str>, text: &str) {
        self.0.lock().unwrap().push((
            ws.into(),
            item.map(Into::into),
            session.map(Into::into),
            text.into(),
        ));
    }
}

impl Sink {
    async fn wait(&self, n: usize) -> Vec<String> {
        for _ in 0..200 {
            {
                let v = self.0.lock().unwrap();
                if v.len() >= n {
                    return v.iter().map(|x| x.3.clone()).collect();
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "session got {} messages, wanted {n}",
            self.0.lock().unwrap().len()
        );
    }
}

struct Env {
    env: TestEnv,
    _keys: TempDir,
    home: TempDir,
    work: TempDir,
    vault: Vault,
    uses: SecretUses,
    modules: Modules,
    sink: Arc<Sink>,
    r: Router,
}

fn field(label: &str, kind: &str, value: &str) -> NewField {
    NewField {
        label: label.into(),
        kind: kind.into(),
        secret: false,
        value: Zeroizing::new(value.into()),
    }
}

impl Env {
    fn new() -> Self {
        LazyLock::force(&LOGS);
        let env = TestEnv::new(|_| {});
        let keys = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(work.path().join("sub")).unwrap();
        let store = env.store().clone();
        let vault = Vault::new(
            store.clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        vault.init(WS, ME).unwrap();
        let sink = Arc::new(Sink::default());
        let uses = SecretUses::new(vault.clone(), home.path(), sink.clone());
        let mut reg = Registry::new();
        reg.decision(SUBJECT, uses.decision_handler());
        let modules = Modules::new(store.clone(), env.gateway.clone(), reg);
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES (?, ?, '前端', 'claude', 1)",
                    params![BOT, WS],
                )?;
                c.execute(
                    "INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, 1)",
                    params![BOT, WS],
                )?;
                c.execute(
                    "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES (?, ?, 'steve', ?, 1, 1)",
                    params![PROJECT, WS, work.path().to_string_lossy()],
                )?;
                c.execute(
                    "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, project_id, created, updated)
                     VALUES (?, ?, 1, '推送分支', 'run', '执行', ?, ?, 1, 1)",
                    params![WORK_ITEM, WS, BOT, PROJECT],
                )?;
                c.execute(
                    "INSERT INTO sessions (id, workspace_id, item_id, bot_id, harness, cwd, status, created, updated)
                     VALUES (?, ?, ?, ?, 'claude', ?, 'running', 1, 1)",
                    params![SESSION, WS, WORK_ITEM, BOT, work.path().to_string_lossy()],
                )?;
                Ok(())
            })
            .unwrap();
        uses.start();
        let r = use_api::routes(vault.clone());
        Self {
            env,
            _keys: keys,
            home,
            work,
            vault,
            uses,
            modules,
            sink,
            r,
        }
    }

    fn vault_id(&self, scope: &str, user: &str) -> String {
        self.vault
            .list_vaults(WS, user)
            .unwrap()
            .into_iter()
            .find(|v| v.scope == scope)
            .unwrap()
            .id
    }

    fn token_item(&self) -> String {
        self.vault
            .create_item(
                WS,
                ME,
                NewItem {
                    vault_id: self.vault_id("shared", ME),
                    template: "api_credential".into(),
                    title: "GitHub 令牌".into(),
                    site: Some("github.com".into()),
                    account_id: None,
                    sensitivity: None,
                    fields: vec![
                        field("名称", "text", "ci"),
                        field("令牌", "password", TOKEN),
                    ],
                },
            )
            .unwrap()
            .id
    }

    fn card_item(&self) -> String {
        self.vault
            .create_item(
                WS,
                ME,
                NewItem {
                    vault_id: self.vault_id("personal", ME),
                    template: "bank_card".into(),
                    title: "招行卡".into(),
                    site: None,
                    account_id: None,
                    sensitivity: None,
                    fields: vec![
                        field("持卡人", "text", "Peng"),
                        field("卡号", "card_number", CARD),
                        field("CVV", "password", CVV),
                    ],
                },
            )
            .unwrap()
            .id
    }

    fn cx(&self) -> UseCx {
        UseCx {
            ws: WS.into(),
            user: ME.into(),
            bot_id: BOT.into(),
            item_id: Some(WORK_ITEM.into()),
            session_id: Some(SESSION.into()),
        }
    }

    async fn tool(&self, name: &str, args: Value) -> Result<ToolOut, String> {
        mcp::call(&self.uses, &self.cx(), name, &args).await
    }

    fn q<T: rusqlite::types::FromSql>(&self, sql: &str, p: impl rusqlite::Params) -> T {
        self.env
            .store()
            .with_conn(|c| Ok(c.query_row(sql, p, |r| r.get(0))?))
            .unwrap()
    }

    fn open_decision(&self, use_id: &str) -> steve_store::Decision {
        self.env
            .store()
            .reader(|c| steve_store::base::decisions::open_for(c, WS, SUBJECT, use_id))
            .unwrap()
            .expect("open decision")
    }

    fn last_use(&self) -> String {
        self.q(
            "SELECT id FROM secret_uses ORDER BY created DESC, rowid DESC LIMIT 1",
            [],
        )
    }

    fn answer(&self, use_id: &str, choice: &str) -> Result<steve_store::Decision, String> {
        let d = self.open_decision(use_id);
        decisions::dispatch(&self.modules, &caller(WS, ME), &d.id, choice, "")
            .expect("handled")
            .map_err(|e| format!("{} {e}", e.status()))
    }

    fn temp_root(&self) -> PathBuf {
        secrets_root(self.home.path())
    }
}

fn use_args(item: &str, cmd: &str) -> Value {
    json!({"item": item, "purpose": "推送分支 fix/return-to", "command": cmd, "env": {"GH_TOKEN": "令牌"}})
}

fn dir_empty(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_then_grant_then_direct_run() {
    let e = Env::new();
    let item = e.token_item();
    // no grant: a decision and END
    let out = e
        .tool(
            mcp::USE,
            use_args(
                &item,
                "echo \"t=$GH_TOKEN\"; printf %s \"$GH_TOKEN\" | base64",
            ),
        )
        .await
        .unwrap();
    assert!(out.awaiting_approval);
    assert_eq!(
        out.text,
        "Approval requested. End your turn now. The user's answer will arrive as your next message."
    );
    let su = e.last_use();
    let d = e.open_decision(&su);
    assert_eq!(d.kind, "approve");
    assert_eq!(d.item_id.as_deref(), Some(WORK_ITEM));
    assert_eq!(d.session_id.as_deref(), Some(SESSION));
    assert_eq!(
        d.question,
        "前端 想用「GitHub 令牌」：推送分支 fix/return-to"
    );
    assert_eq!(
        d.options,
        ["once", "grant_1h", "grant_8h", "grant_24h", "deny"]
    );
    let workdir = e.work.path().canonicalize().unwrap();
    assert_eq!(
        d.draft,
        format!(
            "命令：echo \"t=$GH_TOKEN\"; printf %s \"$GH_TOKEN\" | base64\n目录：{}\n注入：GH_TOKEN ← 令牌",
            workdir.display()
        )
    );
    let status: String = e.q("SELECT status FROM secret_uses WHERE id = ?", [&su]);
    assert_eq!(status, "pending");
    assert_eq!(e.uses.waiting(), 1);

    // the user grants an hour: the command runs and the session hears the redacted result
    e.answer(&su, "grant_1h").unwrap();
    let msgs = e.sink.wait(1).await;
    assert_eq!(
        msgs[0],
        "用户批准了使用「GitHub 令牌」。命令已执行，退出码 0。输出（机密已打码）：\nt=[已打码：令牌]\n[已打码：令牌]\n"
    );
    let (status, code): (String, i64) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, exit_code FROM secret_uses WHERE id = ?",
                [&su],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((status.as_str(), code), ("ok", 0));
    let (scope, until): (String, i64) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT scope, until FROM grants WHERE resource_id = ?",
                [&item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(scope, json!({"project_id": PROJECT}).to_string());
    let left = until - steve_store::now();
    assert!(left > 3_590_000 && left <= 3_600_000, "{left}");

    // the session redactor now knows the value and its forms
    let mut ev = json!({"text": format!("I saw {TOKEN} and {}", BASE64.encode(TOKEN.as_bytes()))});
    redact::scrub(SESSION, &mut ev);
    assert_eq!(ev["text"], "I saw [已打码：令牌] and [已打码：令牌]");

    // second use: covered by the grant, runs directly
    let out = e
        .tool(
            mcp::USE,
            use_args("GitHub 令牌", "printf %s \"$GH_TOKEN\" | xxd -p; exit 4"),
        )
        .await
        .unwrap();
    assert!(!out.awaiting_approval);
    assert_eq!(out.text, "Exit code 4.\n[已打码：令牌]\n");
    let open: i64 = e.q(
        "SELECT COUNT(*) FROM decisions WHERE subject_type = 'secret_use' AND status = 'open'",
        [],
    );
    assert_eq!(open, 0);
    let su2 = e.last_use();
    let (status, grant): (String, Option<String>) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, grant_id FROM secret_uses WHERE id = ?",
                [&su2],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(status, "failed");
    assert!(grant.is_some());

    // the use log
    let (code, list) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "GET",
        "/vault/uses?limit=5",
        None,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(
        list[0]["actor"],
        json!({"type": "bot", "id": BOT, "name": "前端"})
    );
    assert_eq!(list[0]["item_title"], "GitHub 令牌");
    assert_eq!(list[0]["mode"], "env");
    assert_eq!(list[0]["work_item_id"], WORK_ITEM);
    let (code, _) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "GET",
        "/vault/uses?limit=201",
        None,
    )
    .await;
    assert_eq!(code, 400);

    // revoking the grant: the next use asks again
    let gid: String = e.q("SELECT id FROM grants WHERE resource_id = ?", [&item]);
    let (code, body) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "DELETE",
        &format!("/grants/{gid}"),
        None,
    )
    .await;
    assert_eq!((code, body), (200, json!({"ok": true})));
    let out = e.tool(mcp::USE, use_args(&item, "true")).await.unwrap();
    assert!(out.awaiting_approval);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_stdin_temp_dir_gone_cwd_checked() {
    let e = Env::new();
    let item = e.token_item();
    // a grant from the REST form so the runs are direct
    let (code, g) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "POST",
        &format!("/vault/items/{item}/grants"),
        Some(&json!({"bot_id": BOT, "scope": {"project_id": PROJECT}, "hours": 1})),
    )
    .await;
    assert_eq!(code, 200, "{g}");
    assert_eq!(g["scope_label"], "项目 steve");

    let stat_mode = if cfg!(target_os = "macos") {
        "stat -f '%Lp'"
    } else {
        "stat -c '%a'"
    };
    let out = e
        .tool(
            mcp::USE,
            json!({
                "item": item, "purpose": "测试", "cwd": "sub",
                "command": format!("pwd; echo \"f=$TOKF\"; {stat_mode} \"$TOKF\"; cat \"$TOKF\"; echo; read -r x; echo \"in=$x\"; ls \"$(dirname \"$TOKF\")\""),
                "files": {"TOKF": "令牌"}, "stdin": "令牌"
            }),
        )
        .await
        .unwrap();
    let sub = e.work.path().canonicalize().unwrap().join("sub");
    assert_eq!(
        out.text,
        format!(
            "Exit code 0.\n{}\nf=[已打码：临时文件]/TOKF\n600\n[已打码：令牌]\nin=[已打码：令牌]\nTOKF\n",
            sub.display()
        )
    );
    assert!(dir_empty(&e.temp_root()), "temp dir left behind");

    // cwd escapes
    for bad in ["..", "/tmp", "sub/../.."] {
        let err = e
            .tool(mcp::USE, json!({"item": item, "purpose": "x", "command": "true", "cwd": bad, "env": {"T": "令牌"}}))
            .await
            .unwrap_err();
        assert_eq!(err, CWD_OUTSIDE, "{bad}");
    }
    // other refusals
    let err = e
        .tool(
            mcp::USE,
            json!({"item": item, "purpose": "x", "command": "true", "env": {"T": "密码"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "No field \"密码\" in this credential. Its fields: 名称, 令牌."
    );
    let err = e
        .tool(
            mcp::USE,
            json!({"item": item, "purpose": "x", "command": "true"}),
        )
        .await
        .unwrap_err();
    assert!(err.starts_with("Nothing to inject"), "{err}");
    let err = e
        .tool(
            mcp::USE,
            json!({"item": item, "purpose": "x", "command": "true", "env": {"A B": "令牌"}}),
        )
        .await
        .unwrap_err();
    assert!(err.contains("not a valid variable name"), "{err}");
    let err = e.tool(mcp::USE, json!({"item": 3})).await.unwrap_err();
    assert!(
        err.starts_with("MCP error -32602: Input validation error"),
        "{err}"
    );
    let err = e
        .tool(mcp::USE, use_args("nope", "true"))
        .await
        .unwrap_err();
    assert_eq!(err, NO_SUCH);
}

#[tokio::test(flavor = "multi_thread")]
async fn high_items_and_bank_cards_always_ask() {
    let e = Env::new();
    let card = e.card_item();
    // REST grant refused
    let (code, body) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "POST",
        &format!("/vault/items/{card}/grants"),
        Some(&json!({"bot_id": BOT, "scope": {"project_id": PROJECT}, "hours": 1})),
    )
    .await;
    assert_eq!((code, body), (400, json!({"error": GRANT_HIGH})));
    // even a grant row written behind the API's back is not honoured
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created)
                 VALUES ('gr_forged', ?, 'bot', ?, 'secret.use', 'vault_item', ?, ?, ?, 'x', 1)",
                params![WS, BOT, card, json!({"project_id": PROJECT}).to_string(), i64::MAX],
            )?;
            Ok(())
        })
        .unwrap();
    let out = e
        .tool(mcp::USE, json!({"item": "招行卡", "purpose": "买域名", "command": "printf %s \"$C\"", "env": {"C": "卡号", "V": "CVV"}}))
        .await
        .unwrap();
    assert!(out.awaiting_approval);
    let su = e.last_use();
    let d = e.open_decision(&su);
    assert_eq!(d.options, ["once", "deny"]);
    assert_eq!(
        d.question,
        "前端 想用「招行卡」：买域名（这次使用可能涉及付款）"
    );
    // a grant choice is not an option
    let err = e.answer(&su, "grant_1h").unwrap_err();
    assert!(err.starts_with("400"), "{err}");
    // once: runs, card number masked
    e.answer(&su, "once").unwrap();
    let msgs = e.sink.wait(1).await;
    assert!(
        msgs[0].ends_with("退出码 0。输出（机密已打码）：\n[已打码：卡号]"),
        "{}",
        msgs[0]
    );
    let grants: i64 = e.q(
        "SELECT COUNT(*) FROM grants WHERE resource_id = ? AND id != 'gr_forged'",
        [&card],
    );
    assert_eq!(grants, 0);

    // a sensitive item raised to high loses its grants and asks again
    let item = e.token_item();
    let (code, _) = call(
        &e.r,
        Some(&caller(WS, ME)),
        "POST",
        &format!("/vault/items/{item}/grants"),
        Some(&json!({"bot_id": BOT, "scope": {"project_id": PROJECT}, "hours": 24})),
    )
    .await;
    assert_eq!(code, 200);
    e.vault
        .patch_item(
            WS,
            ME,
            &item,
            serde_json::from_value(json!({"sensitivity": "high"})).unwrap(),
        )
        .unwrap();
    let out = e.tool(mcp::USE, use_args(&item, "true")).await.unwrap();
    assert!(out.awaiting_approval);
    assert_eq!(e.open_decision(&e.last_use()).options, ["once", "deny"]);
    // and grant rules: hours, bot, scope
    let item2 = e.token_item();
    for (body, want) in [
        (
            json!({"bot_id": BOT, "scope": {"project_id": PROJECT}, "hours": 48}),
            400,
        ),
        (
            json!({"bot_id": "nobot", "scope": {"project_id": PROJECT}, "hours": 1}),
            404,
        ),
        (
            json!({"bot_id": BOT, "scope": {"project_id": "prj_x"}, "hours": 1}),
            404,
        ),
        (json!({"bot_id": BOT, "scope": {}, "hours": 1}), 400),
        (
            json!({"bot_id": BOT, "scope": {"site": "GitHub.com"}, "hours": 8}),
            200,
        ),
    ] {
        let (code, b) = call(
            &e.r,
            Some(&caller(WS, ME)),
            "POST",
            &format!("/vault/items/{item2}/grants"),
            Some(&body),
        )
        .await;
        assert_eq!(code, want, "{body} → {b}");
    }
    // a site grant does not cover commands
    let out = e.tool(mcp::USE, use_args(&item2, "true")).await.unwrap();
    assert!(out.awaiting_approval);
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_and_visibility() {
    let e = Env::new();
    let item = e.token_item();
    e.tool(mcp::USE, use_args(&item, "echo nope"))
        .await
        .unwrap();
    let su = e.last_use();
    e.answer(&su, "deny").unwrap();
    let msgs = e.sink.wait(1).await;
    assert_eq!(msgs[0], denied_message("GitHub 令牌"));
    let status: String = e.q("SELECT status FROM secret_uses WHERE id = ?", [&su]);
    assert_eq!(status, "denied");
    // answered twice: 409
    let d: String = e.q("SELECT decision_id FROM secret_uses WHERE id = ?", [&su]);
    let r = decisions::dispatch(&e.modules, &caller(WS, ME), &d, "once", "").unwrap();
    assert_eq!(r.unwrap_err().status(), 409);

    // another user's personal item is invisible to this session's user
    let other = e
        .vault
        .create_item(
            WS,
            OTHER,
            NewItem {
                vault_id: e.vault_id("personal", OTHER),
                template: "login".into(),
                title: "别人的邮箱".into(),
                site: None,
                account_id: None,
                sensitivity: None,
                fields: vec![field("密码", "password", "other-pass-123")],
            },
        )
        .unwrap()
        .id;
    let err = e
        .tool(
            mcp::USE,
            json!({"item": other, "purpose": "x", "command": "true", "env": {"P": "密码"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, NO_SUCH);
    let list = e.tool(mcp::LIST, json!({})).await.unwrap().text;
    assert_eq!(
        list,
        format!(
            "{item} · GitHub 令牌 · api_credential · github.com · sensitive · fields: 名称, 令牌\nTo use one, call steve_secret_use."
        )
    );
    assert!(!list.contains("别人的邮箱"));
    let none = e
        .tool(mcp::LIST, json!({"query": "zzz"}))
        .await
        .unwrap()
        .text;
    assert_eq!(none, "No credentials match \"zzz\".");
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_expires_waiting_requests_and_cleans_temp_dirs() {
    let e = Env::new();
    let item = e.token_item();
    e.tool(mcp::USE, use_args(&item, "true")).await.unwrap();
    let su = e.last_use();
    let d = e.open_decision(&su);
    // a leftover temp dir from a crashed run
    let left = e.temp_root().join("su_crashed0001");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::write(left.join("T"), TOKEN).unwrap();

    // "restart": a new service over the same store
    let sink = Arc::new(Sink::default());
    let again = SecretUses::new(e.vault.clone(), e.home.path(), sink.clone());
    again.start();
    assert!(!left.exists());
    let status: String = e.q("SELECT status FROM secret_uses WHERE id = ?", [&su]);
    assert_eq!(status, "expired");
    let ds: String = e.q("SELECT status FROM decisions WHERE id = ?", [&d.id]);
    assert_eq!(ds, "expired");
    let msgs = sink.wait(1).await;
    assert!(msgs[0].contains("在 Steve 重启时失效了"), "{}", msgs[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_token_is_intercepted_and_no_value_reaches_the_database() {
    let e = Env::new();
    let item = e.token_item();
    let card = e.card_item();
    // a use under a grant, an approved one and a denied one
    e.vault
        .create_grant(
            WS,
            ME,
            &item,
            serde_json::from_value(
                json!({"bot_id": BOT, "scope": {"project_id": PROJECT}, "hours": 1}),
            )
            .unwrap(),
        )
        .unwrap();
    e.tool(mcp::USE, use_args(&item, "echo $GH_TOKEN"))
        .await
        .unwrap();
    e.tool(mcp::USE, json!({"item": card, "purpose": "付款", "command": "echo $C $V", "env": {"C": "卡号", "V": "CVV"}}))
        .await
        .unwrap();
    e.answer(&e.last_use(), "once").unwrap();
    e.sink.wait(1).await;

    // pasting a token into the conversation: what the integrator stores is the guarded text
    let pasted = "帮我推一下，令牌是 ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let i = guard::intercept(pasted);
    assert!(i.blocked);
    assert_eq!(i.text, "帮我推一下，令牌是 ［已拦下的机密］");
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_1', ?, 'thread', 'me', ?, 1)",
                params![WS, i.text],
            )?;
            c.execute(
                "INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_2', ?, 'thread', 'steve', ?, 2)",
                params![WS, guard::REPLY],
            )?;
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .unwrap();

    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(e.env.dir.path()).unwrap().flatten() {
        bytes.extend(std::fs::read(entry.path()).unwrap_or_default());
    }
    bytes.extend(LOGS.lock().unwrap().iter());
    assert!(!LOGS.lock().unwrap().is_empty(), "no logs captured");
    let hay = String::from_utf8_lossy(&bytes);
    for secret in [
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        TOKEN,
        CARD,
        CVV,
        &BASE64.encode(TOKEN.as_bytes()),
    ] {
        assert!(
            !hay.contains(secret),
            "{secret} found in the database files or logs"
        );
    }
    // the use log and audit rows exist, without values
    let n: i64 = e.q("SELECT COUNT(*) FROM audit WHERE action = 'vault.use'", []);
    assert_eq!(n, 2);
    assert!(dir_empty(&e.temp_root()));
}

#[tokio::test(flavor = "multi_thread")]
async fn hourly_cleanup_expires_day_old_requests_and_year_old_rows() {
    let e = Env::new();
    let item = e.token_item();
    e.tool(mcp::USE, use_args(&item, "true")).await.unwrap();
    let fresh = e.last_use();
    e.tool(mcp::USE, use_args(&item, "true")).await.unwrap();
    let old = e.last_use();
    let day = 86_400_000_i64;
    e.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE secret_uses SET created = created - ? WHERE id = ?", params![2 * day, old])?;
            c.execute(
                "INSERT INTO secret_uses (id, workspace_id, item_id, actor_type, actor_id, purpose, mode, status, created)
                 VALUES ('su_ancient', ?, ?, 'bot', ?, 'x', 'env', 'ok', ?)",
                params![WS, item, BOT, steve_store::now() - 400 * day],
            )?;
            Ok(())
        })
        .unwrap();
    e.uses.hourly().await.unwrap();
    let st = |id: &str| -> String { e.q("SELECT status FROM secret_uses WHERE id = ?", [id]) };
    assert_eq!(st(&old), "expired");
    assert_eq!(st(&fresh), "pending");
    let gone: i64 = e.q(
        "SELECT COUNT(*) FROM secret_uses WHERE id = 'su_ancient'",
        [],
    );
    assert_eq!(gone, 0);
    let msgs = e.sink.wait(1).await;
    assert!(msgs[0].contains("一天内没有处理"), "{}", msgs[0]);
    assert_eq!(e.uses.waiting(), 1);
    // the expired request can't be answered any more
    let d: String = e.q("SELECT decision_id FROM secret_uses WHERE id = ?", [&old]);
    let r = decisions::dispatch(&e.modules, &caller(WS, ME), &d, "once", "").unwrap();
    assert_eq!(r.unwrap_err().status(), 409);
}
