//! A migrated store, the vault on a file key store, connections with the built-in calendar sources
//! and a scripted one, a [`Calendar`], and an ICS server on loopback. Synthetic data only.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use serde_json::Value;
use steve_calendar::profile::{
    Busy, CalendarSource, Changes, Features, RemoteCalendar, RemoteEvent, SourceError, Window,
};
use steve_calendar::sources::{DemoSource, IcsSource};
use steve_calendar::{Calendar, Features as F};
use steve_modules::Caller;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, Connections, ConnectionsConfig, FieldDef,
    ProviderError,
};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use zeroize::Zeroizing;

pub const DAY: i64 = 86_400_000;

/// A source whose answers the test sets: calendars, a changes script, free/busy.
#[derive(Default)]
pub struct Scripted {
    pub calendars: Mutex<Vec<RemoteCalendar>>,
    /// `changes` answers, in order (cursor in, answer out).
    pub changes: Mutex<Vec<Changes>>,
    pub cursors: Mutex<Vec<Option<String>>>,
    pub fail: Mutex<Option<SourceError>>,
    pub get: Mutex<Option<RemoteEvent>>,
}

pub struct ScriptedSource(pub Arc<Scripted>);

#[async_trait]
impl ConnectionProvider for ScriptedSource {
    fn id(&self) -> &str {
        "cal_script"
    }
    fn system(&self) -> &str {
        "cal_script"
    }
    fn name(&self) -> &str {
        "Scripted"
    }
    fn caps(&self) -> &[&'static str] {
        &["cal.read"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &[]
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        Ok(AccountInfo::new(format!("s:{}", cx.id()), "Scripted").kind("calendar"))
    }
}

#[async_trait]
impl CalendarSource for ScriptedSource {
    fn adapter(&self) -> &str {
        "cal_script"
    }
    async fn features(&self, _: &ConnCx) -> Result<Option<Features>, SourceError> {
        Ok(Some(F::of(&["changes", "instances", "ical_uid"])))
    }
    async fn calendars(&self, _: &ConnCx) -> Result<Vec<RemoteCalendar>, SourceError> {
        if let Some(e) = self.0.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.0.calendars.lock().unwrap().clone())
    }
    async fn changes(
        &self,
        _: &ConnCx,
        _: &str,
        cursor: Option<&str>,
        _: Window,
    ) -> Result<Changes, SourceError> {
        self.0
            .cursors
            .lock()
            .unwrap()
            .push(cursor.map(str::to_string));
        let mut xs = self.0.changes.lock().unwrap();
        if xs.is_empty() {
            return Ok(Changes {
                cursor: cursor.map(str::to_string),
                ..Default::default()
            });
        }
        Ok(xs.remove(0))
    }
    async fn list(&self, _: &ConnCx, _: &str, _: Window) -> Result<Vec<RemoteEvent>, SourceError> {
        Err(SourceError::Unsupported)
    }
    async fn get(&self, _: &ConnCx, _: &str, _: &str) -> Result<Option<RemoteEvent>, SourceError> {
        Ok(self.0.get.lock().unwrap().clone())
    }
    async fn freebusy(&self, _: &ConnCx, _: &str, _: Window) -> Result<Vec<Busy>, SourceError> {
        Err(SourceError::Unsupported)
    }
}

/// An ICS file served on loopback; the test changes what it answers.
#[derive(Clone)]
pub struct IcsServer {
    pub url: String,
    pub state: Arc<Mutex<(u16, String)>>,
}

impl IcsServer {
    pub async fn start(body: &str) -> Self {
        let state = Arc::new(Mutex::new((200u16, body.to_string())));
        let s2 = state.clone();
        let app = Router::new().route(
            "/cal.ics",
            axum::routing::get(move || {
                let s = s2.clone();
                async move {
                    let (code, body) = s.lock().unwrap().clone();
                    (axum::http::StatusCode::from_u16(code).unwrap(), body)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/cal.ics", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        Self { url, state }
    }
    pub fn set(&self, code: u16, body: &str) {
        *self.state.lock().unwrap() = (code, body.to_string());
    }
}

pub struct H {
    pub env: TestEnv,
    pub _keys: TempDir,
    pub conns: Connections,
    pub cal: Calendar,
    pub script: Arc<Scripted>,
    pub router: axum::Router,
}

pub fn me() -> Caller {
    caller(WS, ME)
}

pub async fn setup() -> H {
    let env = TestEnv::new(|r| steve_modules::accounts::connections::register(r));
    env.store()
        .transaction(|tx| {
            for (id, label) in [
                ("ics", "ICS"),
                ("cal_demo", "Demo"),
                ("cal_script", "Scripted"),
            ] {
                steve_modules::accounts::repo::upsert_declared(tx, id, label, "calendar")?;
            }
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    vault.init(WS, ME).unwrap();
    let script = Arc::new(Scripted::default());
    let ics = Arc::new(IcsSource::new());
    let demo = Arc::new(DemoSource::new());
    let scripted = Arc::new(ScriptedSource(script.clone()));
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://127.0.0.1:8441".into()], true, |_| None),
    )
    .provider(ics.clone())
    .provider(demo.clone())
    .provider(scripted.clone())
    .build();
    let cal = Calendar::builder(env.modules.clone(), conns.clone())
        .source(ics)
        .source(demo)
        .source(scripted)
        .build();
    let router = steve_calendar::api::routes(cal.clone()).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        conns,
        cal,
        script,
        router,
    }
}

impl H {
    pub async fn connect(&self, adapter: &str, fields: &[(&str, &str)]) -> String {
        let f: BTreeMap<String, Zeroizing<String>> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), Zeroizing::new(v.to_string())))
            .collect();
        self.conns
            .create(&me(), adapter, &f, Value::Object(Default::default()))
            .await
            .unwrap_or_else(|e| panic!("connect {adapter}: {e}"))
            .id
    }
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        steve_modules::testkit::call(&self.router, Some(&me()), method, path, body.as_ref()).await
    }
    pub async fn call_as(
        &self,
        who: &Caller,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        steve_modules::testkit::call(&self.router, Some(who), method, path, body.as_ref()).await
    }
    pub fn now(&self) -> i64 {
        self.env.modules.clock.now()
    }
    pub fn sql<T: rusqlite::types::FromSql>(&self, q: &str, p: &[&dyn rusqlite::ToSql]) -> T {
        self.env
            .store()
            .reader(|c| {
                c.query_row(q, p, |r| r.get(0))
                    .map_err(steve_store::StoreError::from)
            })
            .unwrap()
    }
    pub fn exec(&self, q: &str) {
        self.env
            .store()
            .with_conn(|c| c.execute_batch(q).map_err(steve_store::StoreError::from))
            .unwrap();
    }
    /// A contact with an email address Steve knows.
    pub fn contact(&self, id: &str, name: &str, email: &str) {
        self.exec(&format!(
            "INSERT INTO parties (id, workspace_id, type, name, contact_kind, created_by, created, updated) VALUES ('{id}', 'default', 'contact', '{name}', 'real', 'usr_dev', 1, 1);
             INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES ('acc_{id}', 'default', 'mail', 'email', '{id}', 'work_email', '{email}', '{email}', 'sync', 1, 1);
             INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES ('default', 'email', '{email}', 'acc_{id}', 1);"
        ));
    }
    pub fn me_party(&self) -> String {
        self.sql("SELECT id FROM parties WHERE user_id = ? AND workspace_id = 'default' AND status = 'active'", &[&ME])
    }
}
