use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use serde_json::{Value, json};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, GraphEngine};
use steve_github::fake::{FakeGitHub, defaults};
use steve_github::{GitHub, GitHubEnv, RecordingCore, automation, flow, repos};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
use steve_runtime::testkit::eventually_sync;

pub const PROJECT: &str = "prj-sg30";
pub const BOT: &str = "bot-sg30";

pub struct Env {
    pub env: TestEnv,
    pub gh: GitHub,
    pub fake: Arc<FakeGitHub>,
    pub on_model: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    pub connection: String,
    pub repo: String,
    _keys: tempfile::TempDir,
}
impl Env {
    pub async fn new(link: bool) -> Self {
        let fake = Arc::new(FakeGitHub::start().await.unwrap());
        assert_ne!(fake.addr().port(), 8440);
        let core = Arc::new(FakeCore::default());
        let setup = FlowSetup::new(core.clone())
            .item_finder(flow::BY, Arc::new(flow::GhPrFinder))
            .action(Arc::new(flow::GhItemLink));
        let mut env = TestEnv::new(|_| {});
        let on_model = Arc::new(Mutex::new(None));
        let mut registry = steve_modules::Registry::new();
        connections::register(&mut registry);
        setup.register(&mut registry);
        env.modules = steve_modules::Modules::with_clock(
            env.store().clone(),
            Arc::new(ModelGate {
                inner: env.gateway.clone(),
                on_call: on_model.clone(),
            }),
            registry,
            env.clock.clone(),
        );
        *core.store.lock().unwrap() = Some(env.store().clone());
        env.store().transaction(|tx| -> steve_store::Result<_> {
            tx.execute("INSERT INTO projects (id,workspace_id,name,path,kind,created,updated) VALUES (?,?,'Fixture',?,'dir',1,1)",
                rusqlite::params![PROJECT,WS,env.dir.path().display().to_string()])?;
            tx.execute("INSERT INTO bots (id,workspace_id,name,role,harness,main,enabled,updated) VALUES (?,?,'Analysis bot','Review only','claude-code',1,1,1)",
                rusqlite::params![BOT,WS])?;
            tx.execute("INSERT INTO bot_workspaces (bot_id,workspace_id,created) VALUES (?,?,1)",rusqlite::params![BOT,WS])?;
            Ok(())
        }).unwrap();
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        vault.init(WS, ME).unwrap();
        let gh_env = GitHubEnv {
            api_base: fake.url(),
            ..Default::default()
        };
        let mut builder = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(vec!["http://hub.test:18990".into()], true, |_| None),
        );
        for provider in steve_github::providers_for(&gh_env) {
            builder = builder.provider(provider);
        }
        let connections = builder.build();
        let gh = GitHub::new(
            connections.clone(),
            gh_env,
            Arc::new(steve_runtime::cache::Caches::new(Default::default())),
        );
        let flow = setup
            .action(Arc::new(automation::Analyze::new(&gh)))
            .connections(connections.clone())
            .build(env.modules.clone(), Arc::new(GraphEngine));
        gh.set_core(RecordingCore::new(env.store().clone()));
        gh.set_flow(flow.clone());
        gh.set_event_sink(Arc::new(flow::FlowSink(Arc::new(flow))));
        let api = repos::routes(gh.clone())
            .merge(connections::routes(connections))
            .with_state(env.modules.clone());
        let (s,v)=call(&api,Some(&caller(WS,ME)),"POST","/connections",Some(&json!({
            "adapter":"github_pat","fields":{"令牌":defaults::PAT},"config":{"api_base":fake.url()}
        }))).await;
        assert_eq!(s, 200, "{v}");
        let connection = v["id"].as_str().unwrap().to_string();
        let mut h = Self {
            env,
            gh,
            fake,
            connection,
            on_model,
            repo: String::new(),
            _keys: keys,
        };
        if link {
            let v = h.link(defaults::REPO_ID as i64, Some(PROJECT)).await;
            h.repo = v["id"].as_str().unwrap().to_string();
            h.gh.settle().await;
        }
        h.fake.clear_calls();
        h
    }
    pub async fn link(&self, id: i64, project: Option<&str>) -> Value {
        let made = self
            .gh
            .link_repo(&caller(WS, ME), &self.connection, id, project)
            .await
            .unwrap();
        self.gh.settle().await;
        made
    }
    pub fn scalar(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Value {
        self.env
            .store()
            .reader(|c| -> steve_store::Result<_> {
                Ok(c.query_row(sql, args, |r| {
                    Ok(match r.get_ref(0)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Real(n) => json!(n),
                        rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        _ => panic!("unexpected scalar"),
                    })
                })?)
            })
            .unwrap()
    }
    pub fn patch(&self, settings: Value) {
        self.gh
            .patch_repo(&caller(WS, ME), &self.repo, &json!({"settings":settings}))
            .unwrap();
    }
    pub async fn object(&self, kind: &str, n: u64) -> Value {
        self.gh
            .conn(WS, &self.connection)
            .unwrap()
            .get(&format!("/repos/{}/{kind}/{n}", defaults::REPO))
            .await
            .unwrap()
    }
    pub async fn payload(&self, kind: &str, action: &str, n: u64) -> Value {
        let object = self
            .object(if kind == "issue" { "issues" } else { "pulls" }, n)
            .await;
        let mut p = json!({"action":action,"repository":{"id":defaults::REPO_ID,"full_name":defaults::REPO}});
        p[kind] = object;
        p
    }
    pub async fn send(&self, id: &str, event: &str, payload: &Value) -> u16 {
        let body = serde_json::to_vec(payload).unwrap();
        let secret = &self.fake.hooks(defaults::REPO)[0].secret;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-hub-signature-256",
            steve_github::signature::sign(secret.as_bytes(), &body)
                .parse()
                .unwrap(),
        );
        headers.insert("x-github-event", event.parse().unwrap());
        headers.insert("x-github-delivery", id.parse().unwrap());
        let result = self
            .gh
            .receive_connection(&self.connection, &headers, &body)
            .await;
        let status = result.status().as_u16();
        self.gh.settle().await;
        status
    }
    pub fn trace(&self, id: &str) -> Value {
        let rows = self
            .gh
            .deliveries_out(
                &caller(WS, ME),
                &repos::ListQuery {
                    repo: Some(self.repo.clone()),
                    automation: Some("only".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == id)
            .map(|d| d["automation"].clone())
            .unwrap_or_default()
    }
    pub async fn analyzed(&self, id: &str, status: &str) -> Value {
        let mut out = None;
        assert!(
            eventually_sync(format!("analysis {id} reaches {status}"), || {
                let v = self.trace(id);
                if v["runs"][0]["status"] == status {
                    out = Some(v);
                    true
                } else {
                    false
                }
            })
            .await
        );
        out.unwrap()
    }
    pub async fn issue(&self, title: &str, body: &str) -> u64 {
        self.fake
            .add_issue(defaults::REPO, title, body, &[])
            .unwrap()
    }
}

struct ModelGate {
    inner: Arc<steve_modules::testkit::FakeGateway>,
    on_call: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
}
#[async_trait::async_trait]
impl steve_modules::Gateway for ModelGate {
    async fn run(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        timeout: std::time::Duration,
    ) -> Result<steve_modules::GatewayOut, String> {
        let hook = self.on_call.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        steve_modules::Gateway::run(self.inner.as_ref(), tier, prompt, timeout).await
    }
}
