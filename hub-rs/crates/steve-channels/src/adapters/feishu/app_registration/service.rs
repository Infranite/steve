//! Attempts on the database and their background work (poll, save, check and bind). The rules are
//! in the module docs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::vault::{VaultError, VaultResult};
use steve_modules::{ApiError, Caller, Modules};
use steve_store::{Tx, uid};
use zeroize::Zeroizing;

use super::super::auth::{APP_ADAPTER, APP_SECRET};
use super::super::client::guard_url;
use super::protocol::{
    ARCHETYPE, Bases, Brand, MAX_INTERVAL_S, PATH, Poll, Secret, classify, parse_begin,
};

const BEGIN_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// The vault item's title while it belongs to the registration.
const ITEM_TITLE: &str = "飞书应用（注册中）";
/// The vault item holding the bearer ticket while pending.
const TICKET_TITLE: &str = "飞书应用注册确认码";
/// Its one field.
const TICKET_FIELD: &str = "device_code";

/// The row states.
pub mod state {
    pub const PENDING: &str = "pending";
    pub const SAVED: &str = "credentials_saved";
    pub const VERIFYING: &str = "verifying";
    pub const SUCCEEDED: &str = "succeeded";
    pub const DENIED: &str = "denied";
    pub const EXPIRED: &str = "expired";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
    /// Active: holds the person's single slot.
    pub const ACTIVE: [&str; 3] = [PENDING, SAVED, VERIFYING];
}

/// Our own error kinds and texts (never the provider's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Denied,
    Expired,
    Network,
    Provider,
    VerifyFailed,
    Conflict,
    MemberGone,
    CredentialsLost,
}

impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Network => "network",
            Self::Provider => "provider",
            Self::VerifyFailed => "verify_failed",
            Self::Conflict => "conflict",
            Self::MemberGone => "member_gone",
            Self::CredentialsLost => "credentials_lost",
        }
    }
    pub fn text(self) -> &'static str {
        match self {
            Self::Denied => "你在飞书里拒绝了创建应用",
            Self::Expired => "确认码已经过期，请重新开始",
            Self::Network => "暂时连不上飞书，稍后再试",
            Self::Provider => "飞书没有完成这次创建，请重新开始",
            Self::VerifyFailed => "应用已经创建，但 Steve 用它连飞书没有成功；凭据已保存，可以重试",
            Self::Conflict => "这个应用已经接入了，没有替换已有的连接",
            Self::MemberGone => "发起人已经不在这个工作空间，没有接入",
            Self::CredentialsLost => {
                "飞书那边可能已经创建了应用，但 Steve 没有收到凭据；请在开发者后台重置 App Secret 后用“关联已有应用”接入"
            }
        }
    }
}

/// One row.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    ws: String,
    initiator: String,
    _brand_requested: String,
    brand: String,
    switched: bool,
    device_item: Option<String>,
    user_code: Option<String>,
    verification_uri_complete: Option<String>,
    interval_s: i64,
    expires_at: i64,
    state: String,
    error: Option<String>,
    error_kind: Option<String>,
    app_may_exist: bool,
    client_id: Option<String>,
    vault_item: Option<String>,
    connection_id: Option<String>,
    account_id: Option<String>,
}

const COLS: &str = "id, workspace_id, initiator, brand_requested, brand, switched, device_item, user_code, \
     verification_uri_complete, interval_s, expires_at, state, error, error_kind, app_may_exist, client_id, \
     vault_item, connection_id, account_id";

fn row_of(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        ws: r.get(1)?,
        initiator: r.get(2)?,
        _brand_requested: r.get(3)?,
        brand: r.get(4)?,
        switched: r.get::<_, i64>(5)? != 0,
        device_item: r.get(6)?,
        user_code: r.get(7)?,
        verification_uri_complete: r.get(8)?,
        interval_s: r.get(9)?,
        expires_at: r.get(10)?,
        state: r.get(11)?,
        error: r.get(12)?,
        error_kind: r.get(13)?,
        app_may_exist: r.get::<_, i64>(14)? != 0,
        client_id: r.get(15)?,
        vault_item: r.get(16)?,
        connection_id: r.get(17)?,
        account_id: r.get(18)?,
    })
}

fn load(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        &format!("SELECT {COLS} FROM chan_app_registrations WHERE id = ?"),
        [id],
        row_of,
    )
    .optional()
}

/// Whether `user` is (still) a member of `ws` — the real membership, not visibility.
fn is_member(c: &rusqlite::Connection, ws: &str, user: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![ws, user],
        |x| x.get(0),
    )
}

impl Row {
    fn who(&self) -> Caller {
        Caller {
            ws: self.ws.clone(),
            me: self.initiator.clone(),
        }
    }
    fn brand(&self) -> Brand {
        Brand::parse(&self.brand).unwrap_or(Brand::Feishu)
    }
    /// The API view: no device code, no secret, no App ID; the code and page only while pending.
    fn out(&self) -> Value {
        let pending = self.state == state::PENDING;
        let mut v = json!({
            "id": self.id,
            "brand": self.brand,
            "state": self.state,
            "expires_at": self.expires_at,
            "interval": self.interval_s,
            "app_may_exist": self.app_may_exist,
        });
        let o = v.as_object_mut().expect("object");
        let mut put = |k: &str, x: &Option<String>| {
            if let Some(x) = x {
                o.insert(k.into(), json!(x));
            }
        };
        if pending {
            put("verification_uri_complete", &self.verification_uri_complete);
            put("user_code", &self.user_code);
        }
        put("connection_id", &self.connection_id);
        put("account_id", &self.account_id);
        put("error", &self.error);
        put("error_kind", &self.error_kind);
        v
    }
}

fn store_err(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(format!("注册记录读写失败：{e}"))
}

/// Feishu / Lark app registrations of one Hub.
#[derive(Clone)]
pub struct Registrations {
    conns: Connections,
    bases: Arc<Bases>,
    /// One second in the Hub; tests shorten it (intervals are counted in it).
    tick: Duration,
    running: Arc<Mutex<HashSet<String>>>,
    /// One begin at a time per person and workspace (held across the provider call, not a DB lock).
    begins: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes a running mark when its task ends (or panics).
struct Running(Arc<Mutex<HashSet<String>>>, String);

impl Drop for Running {
    fn drop(&mut self) {
        lock(&self.0).remove(&self.1);
    }
}

impl Registrations {
    pub fn new(conns: Connections, bases: Bases) -> Self {
        Self {
            conns,
            bases: Arc::new(bases),
            tick: Duration::from_secs(1),
            running: Arc::new(Mutex::new(HashSet::new())),
            begins: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The unit of `interval` (tests).
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    fn modules(&self) -> &Modules {
        self.conns.modules()
    }

    fn now(&self) -> i64 {
        self.modules().clock.now()
    }

    fn read<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> VaultResult<T> {
        Ok(self
            .modules()
            .store
            .with_conn(|c| Ok(f(c)?))
            .map_err(store_err)?)
    }

    fn tx<T>(&self, f: impl FnOnce(&Tx) -> VaultResult<T>) -> VaultResult<T> {
        self.modules().store.transaction(f)
    }

    /// The caller's own attempt `id` (404 for anyone else's).
    fn own(&self, who: &Caller, id: &str) -> VaultResult<Row> {
        self.read(|c| load(c, id))?
            .filter(|r| r.ws == who.ws && r.initiator == who.me)
            .ok_or_else(|| ApiError::not_found("应用注册").into())
    }

    /// Settles an overdue `pending` row as expired (any reader may); returns whether it did.
    fn settle_overdue(&self, tx: &Tx, r: &Row) -> VaultResult<bool> {
        if r.state != state::PENDING || r.expires_at > self.now() {
            return Ok(false);
        }
        // a running poll ends its own attempt (it may be saving a credential issued in time)
        if lock(&self.running).contains(&format!("poll:{}", r.id)) {
            return Ok(false);
        }
        self.drop_ticket(tx, &r.id)?;
        Ok(tx.execute(
            "UPDATE chan_app_registrations SET state = ?, error = ?, error_kind = ?, updated = ?
              WHERE id = ? AND state = ?",
            params![
                state::EXPIRED,
                Kind::Expired.text(),
                Kind::Expired.code(),
                self.now(),
                r.id,
                state::PENDING
            ],
        )? == 1)
    }

    /// Destroys the row's bearer ticket (in `tx`) and forgets it.
    fn drop_ticket(&self, tx: &Tx, id: &str) -> VaultResult<()> {
        let Some(r) = load(tx, id)? else {
            return Ok(());
        };
        if let Some(item) = &r.device_item {
            self.conns.destroy_item_tx(tx, &r.ws, &r.initiator, item)?;
            tx.execute(
                "UPDATE chan_app_registrations SET device_item = NULL WHERE id = ?",
                [id],
            )?;
        }
        Ok(())
    }

    /// The bearer ticket, decrypted for one poll (logged as internal use). `Ok(None)`: it is gone
    /// for good (no item, destroyed, no field); `Err`: could not be read now (store or key service),
    /// try again later — never treated as gone.
    async fn ticket(&self, r: &Row) -> Result<Option<Zeroizing<String>>, ()> {
        let Some(item) = r.device_item.clone() else {
            return Ok(None);
        };
        let (conns, ws, id) = (self.conns.clone(), r.ws.clone(), r.id.clone());
        let res = tokio::task::spawn_blocking(move || {
            conns
                .vault()
                .with_credential(&ws, &item, &id, "应用注册", |c| {
                    c.text(TICKET_FIELD).map(|v| Zeroizing::new(v.to_string()))
                })
        })
        .await
        .map_err(|_| ())?;
        match res {
            Ok(v) => Ok(v),
            Err(VaultError::Api(ApiError::NotFound(_))) => Ok(None),
            Err(_) => Err(()),
        }
    }

    fn active_of(c: &rusqlite::Connection, who: &Caller) -> rusqlite::Result<Option<Row>> {
        c.query_row(
            &format!(
                "SELECT {COLS} FROM chan_app_registrations WHERE workspace_id = ? AND initiator = ?
                  AND state IN ('pending', 'credentials_saved', 'verifying') ORDER BY updated DESC LIMIT 1"
            ),
            params![who.ws, who.me],
            row_of,
        )
        .optional()
    }

    /// `GET current`: the caller's active attempt here, after settling an overdue one.
    pub fn current(&self, who: &Caller) -> VaultResult<Option<Value>> {
        let r = self.read(|c| Self::active_of(c, who))?;
        let Some(r) = r else { return Ok(None) };
        let settled = self.tx(|tx| self.settle_overdue(tx, &r))?;
        if settled {
            return Ok(None);
        }
        self.ensure_running(&r);
        Ok(Some(r.out()))
    }

    /// `GET :id`.
    pub fn get(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own(who, id)?;
        if self.tx(|tx| self.settle_overdue(tx, &r))? {
            return Ok(self.own(who, id)?.out());
        }
        self.ensure_running(&r);
        Ok(r.out())
    }

    /// `POST`: the caller's active attempt if there is one, else a new begin. Begins of one person
    /// in one workspace run one at a time, so a double click asks the provider once.
    pub async fn begin(&self, who: &Caller, brand: Brand) -> VaultResult<Value> {
        let gate = lock(&self.begins)
            .entry(format!("{}\u{1f}{}", who.ws, who.me))
            .or_default()
            .clone();
        let _one = gate.lock().await;
        if let Some(v) = self.current(who)? {
            return Ok(v);
        }
        // preflight before anything leaves the Hub: a member here, and a vault that can seal
        if !self.read(|c| is_member(c, &who.ws, &who.me))? {
            return Err(ApiError::forbidden("你不在这个工作空间里").into());
        }
        drop(
            self.conns
                .prepare_credential(
                    who,
                    TICKET_TITLE,
                    vec![(TICKET_FIELD.to_string(), Zeroizing::new("-".into()))],
                )
                .await?,
        );
        let url = format!("{}{PATH}", self.bases.accounts(Brand::Feishu));
        let form = [
            ("action", "begin"),
            ("archetype", ARCHETYPE),
            ("auth_method", "client_secret"),
            ("request_user_info", "open_id tenant_brand"),
        ];
        let (status, v) = self
            .post(&url, &form, BEGIN_TIMEOUT)
            .await
            .map_err(|k| ApiError::rule(k.text()))?;
        let b = (status < 400)
            .then(|| parse_begin(&v, &self.bases, brand))
            .flatten()
            .ok_or_else(|| {
                tracing::warn!(status, "feishu app registration: begin refused");
                ApiError::rule(Kind::Provider.text())
            })?;
        // the ticket is sealed now and written with the row (never in plain text)
        let ticket = self
            .conns
            .prepare_credential(
                who,
                TICKET_TITLE,
                vec![(TICKET_FIELD.to_string(), b.device_code.clone())],
            )
            .await?;
        let now = self.now();
        let id = uid("reg");
        let conns = self.conns.clone();
        let inserted = self.tx(|tx| {
            // a concurrent begin of the same person won the slot: answer with theirs
            if let Some(r) = Self::active_of(tx, who)? {
                return Ok(Err(r));
            }
            let item = conns.insert_prepared(tx, &ticket)?;
            tx.execute(
                "INSERT INTO chan_app_registrations (id, workspace_id, initiator, brand_requested, brand, switched,
                   device_item, user_code, verification_uri_complete, interval_s, expires_at, state, created, updated)
                 VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    who.ws,
                    who.me,
                    brand.code(),
                    Brand::Feishu.code(),
                    item,
                    b.user_code,
                    b.verification_uri_complete,
                    b.interval_s,
                    now + b.expire_s * 1000,
                    state::PENDING,
                    now,
                    now
                ],
            )?;
            Ok(Ok(()))
        })?;
        let r = match inserted {
            Ok(()) => self.own(who, &id)?,
            Err(theirs) => theirs,
        };
        tracing::info!(registration = %r.id, brand = brand.code(), "feishu app registration begun");
        self.ensure_running(&r);
        Ok(r.out())
    }

    /// `POST :id/cancel`: commits before or after a binding, never in between (the write lock).
    pub fn cancel(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own(who, id)?;
        let conns = self.conns.clone();
        let now = self.now();
        self.tx(|tx| {
            let Some(cur) = load(tx, &r.id)? else {
                return Ok(());
            };
            if !state::ACTIVE.contains(&cur.state.as_str()) {
                return Ok(());
            }
            for item in [&cur.vault_item, &cur.device_item].into_iter().flatten() {
                conns.destroy_item_tx(tx, &cur.ws, &cur.initiator, item)?;
            }
            tx.execute(
                "UPDATE chan_app_registrations SET state = ?, device_item = NULL, vault_item = NULL, app_may_exist = 1,
                   error = NULL, error_kind = NULL, updated = ? WHERE id = ?",
                params![state::CANCELLED, now, cur.id],
            )?;
            Ok(())
        })
        ?;
        tracing::info!(registration = %r.id, "feishu app registration cancel requested");
        Ok(self.own(who, id)?.out())
    }

    /// `POST :id/retry`: checks the saved credential again (`credentials_saved` only).
    pub fn retry(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own(who, id)?;
        if r.state != state::SAVED {
            return Err(ApiError::conflict("这次注册没有可以重试的凭据").into());
        }
        self.spawn_verify(r.id.clone());
        Ok(self.own(who, id)?.out())
    }

    /// At start: resumes every attempt a stop interrupted (pending until its own deadline, a
    /// half-done check, a saved credential not yet checked). Returns how many it picked up.
    pub fn resume(&self) -> VaultResult<usize> {
        let now = self.now();
        let rows: Vec<Row> = self.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {COLS} FROM chan_app_registrations WHERE state IN ('pending', 'credentials_saved', 'verifying')"
            ))?;
            st.query_map([], row_of)?.collect()
        })?;
        let mut n = 0;
        for r in rows {
            match r.state.as_str() {
                state::PENDING if r.expires_at <= now => {
                    self.tx(|tx| self.settle_overdue(tx, &r))?;
                }
                state::PENDING => {
                    self.spawn_poll(r.id.clone(), true);
                    n += 1;
                }
                state::VERIFYING => {
                    // stopped mid-check: the binding never committed (it would have said succeeded)
                    self.tx(|tx| {
                        tx.execute(
                            "UPDATE chan_app_registrations SET state = ?, updated = ? WHERE id = ? AND state = ?",
                            params![state::SAVED, now, r.id, state::VERIFYING],
                        )?;
                        Ok(())
                    })
                    ?;
                    self.spawn_verify(r.id.clone());
                    n += 1;
                }
                _ if r.error.is_none() => {
                    self.spawn_verify(r.id.clone());
                    n += 1;
                }
                _ => {}
            }
        }
        Ok(n)
    }

    /// The background work an active row needs, if it is not running.
    fn ensure_running(&self, r: &Row) {
        match r.state.as_str() {
            state::PENDING => self.spawn_poll(r.id.clone(), false),
            state::SAVED if r.error.is_none() => self.spawn_verify(r.id.clone()),
            // verifying with no check running (it died): check again
            state::VERIFYING if !lock(&self.running).contains(&format!("verify:{}", r.id)) => {
                let now = self.now();
                let reset = self.tx(|tx| {
                    tx.execute(
                        "UPDATE chan_app_registrations SET state = ?, updated = ? WHERE id = ? AND state = ?",
                        params![state::SAVED, now, r.id, state::VERIFYING],
                    )?;
                    Ok(())
                });
                if reset.is_ok() {
                    self.spawn_verify(r.id.clone());
                }
            }
            _ => {}
        }
    }

    fn mark(&self, key: String) -> Option<Running> {
        let mut m = lock(&self.running);
        if !m.insert(key.clone()) {
            return None;
        }
        Some(Running(self.running.clone(), key))
    }

    fn spawn_poll(&self, id: String, resumed: bool) {
        let Some(mark) = self.mark(format!("poll:{id}")) else {
            return;
        };
        let me = self.clone();
        tokio::spawn(async move {
            let _mark = mark;
            if let Err(e) = me.poll_until_done(&id, resumed).await {
                tracing::error!(registration = %id, "feishu app registration poll stopped: {e}");
            }
        });
    }

    fn spawn_verify(&self, id: String) {
        let Some(mark) = self.mark(format!("verify:{id}")) else {
            return;
        };
        let me = self.clone();
        tokio::spawn(async move {
            let _mark = mark;
            if let Err(e) = me.verify(&id).await {
                // an internal failure mid-check: back to a retryable saved credential, never stuck
                tracing::error!(registration = %id, "feishu app registration check stopped: {e}");
                if let Err(e) = me.back_to_saved(&id, Kind::Network) {
                    tracing::error!(registration = %id, "feishu app registration: could not reset: {e}");
                }
            }
        });
    }

    /// A form POST on one of our hosts; the JSON answer whatever the status (poll errors come
    /// with 400). Never puts the URL, the form or the answer in an error.
    async fn post(
        &self,
        url: &str,
        form: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<(u16, Value), Kind> {
        if guard_url(url).is_err() {
            return Err(Kind::Network);
        }
        let res = self
            .conns
            .http()
            .post(url)
            .form(form)
            .timeout(timeout)
            .send()
            .await
            .map_err(|_| Kind::Network)?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await.map_err(|_| Kind::Network)?;
        let v = serde_json::from_slice::<Value>(&bytes).map_err(|_| Kind::Provider)?;
        Ok((status, v))
    }

    /// Ends a pending row in a terminal state (if it is still pending).
    fn end(&self, id: &str, st: &str, kind: Kind, may_exist: bool) -> VaultResult<()> {
        let now = self.now();
        self.tx(|tx| {
            let n = tx.execute(
                "UPDATE chan_app_registrations SET state = ?, error = ?, error_kind = ?,
                   app_may_exist = max(app_may_exist, ?), updated = ? WHERE id = ? AND state = ?",
                params![
                    st,
                    kind.text(),
                    kind.code(),
                    may_exist as i64,
                    now,
                    id,
                    state::PENDING
                ],
            )?;
            if n == 1 {
                self.drop_ticket(tx, id)?;
            }
            Ok(())
        })
    }

    async fn poll_until_done(&self, id: &str, resumed: bool) -> VaultResult<()> {
        let mut first = true;
        let mut slept = false;
        loop {
            let Some(r) = self.read(|c| load(c, id))? else {
                return Ok(());
            };
            if r.state != state::PENDING {
                return Ok(());
            }
            if r.expires_at <= self.now() {
                // after a restart the credential may have been issued and missed
                let kind = if resumed {
                    Kind::CredentialsLost
                } else {
                    Kind::Expired
                };
                return self.end(id, state::EXPIRED, kind, resumed);
            }
            // wait between polls (not before the first, nor right after a brand switch); then
            // reload and re-check state and deadline before polling
            if !first && !slept {
                let wait = self
                    .tick
                    .saturating_mul(r.interval_s.clamp(1, MAX_INTERVAL_S) as u32);
                let left_ms = (r.expires_at - self.now()).max(0) as u64;
                tokio::time::sleep(wait.min(Duration::from_millis(left_ms).max(self.tick))).await;
                slept = true;
                continue;
            }
            first = false;
            slept = false;
            let code = match self.ticket(&r).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    tracing::warn!(registration = %id, "feishu app registration: ticket gone");
                    return self.end(id, state::FAILED, Kind::Provider, resumed);
                }
                Err(()) => {
                    // the vault could not be read now: keep the ticket, try again within the deadline
                    tracing::warn!(registration = %id, "feishu app registration: ticket not readable now, retrying");
                    continue;
                }
            };
            // never poll past the deadline: the request may take at most what is left
            let left = r.expires_at - self.now();
            if left <= 0 {
                let kind = if resumed {
                    Kind::CredentialsLost
                } else {
                    Kind::Expired
                };
                return self.end(id, state::EXPIRED, kind, resumed);
            }
            let url = format!("{}{PATH}", self.bases.accounts(r.brand()));
            let timeout = POLL_TIMEOUT.min(Duration::from_millis(left as u64));
            // a credential issued by a poll sent in time is kept even if it arrives late: the app
            // exists at the provider either way, dropping it would only orphan it
            let v = match self
                .post(
                    &url,
                    &[("action", "poll"), ("device_code", code.as_str())],
                    timeout,
                )
                .await
            {
                Ok((_, v)) => v,
                Err(_) => {
                    self.set_interval(id, (r.interval_s + 1).min(MAX_INTERVAL_S))?;
                    continue;
                }
            };
            match classify(&v, r.brand(), r.switched) {
                Poll::Pending => {}
                Poll::SlowDown => self.set_interval(id, (r.interval_s + 5).min(MAX_INTERVAL_S))?,
                Poll::Switch(b) => {
                    let now = self.now();
                    self.tx(|tx| {
                        tx.execute(
                            "UPDATE chan_app_registrations SET brand = ?, switched = 1, updated = ? WHERE id = ? AND state = ?",
                            params![b.code(), now, id, state::PENDING],
                        )?;
                        Ok(())
                    })?;
                    tracing::info!(registration = %id, brand = b.code(), "feishu app registration: tenant on another brand");
                    first = true;
                }
                Poll::Denied => return self.end(id, state::DENIED, Kind::Denied, false),
                Poll::Expired => {
                    let kind = if resumed {
                        Kind::CredentialsLost
                    } else {
                        Kind::Expired
                    };
                    return self.end(id, state::EXPIRED, kind, resumed);
                }
                Poll::Failed => {
                    tracing::warn!(registration = %id, "feishu app registration failed (provider error)");
                    return self.end(id, state::FAILED, Kind::Provider, false);
                }
                Poll::Issued {
                    client_id,
                    client_secret,
                    tenant_brand,
                } => {
                    // the issuing host is authoritative: a contradicting final report is a violation
                    if tenant_brand.is_some_and(|t| t != r.brand()) {
                        tracing::warn!(registration = %id, "feishu app registration: contradictory tenant brand");
                        return self.end(id, state::FAILED, Kind::Provider, true);
                    }
                    self.save(&r, &client_id, client_secret).await?;
                    self.spawn_verify(id.to_string());
                    return Ok(());
                }
            }
        }
    }

    fn set_interval(&self, id: &str, n: i64) -> VaultResult<()> {
        let now = self.now();
        self.tx(|tx| {
            tx.execute(
                "UPDATE chan_app_registrations SET interval_s = ?, updated = ? WHERE id = ? AND state = ?",
                params![n, now, id, state::PENDING],
            )?;
            Ok(())
        })
    }

    /// The issued credential: sealed now, written with the row's move to `credentials_saved` — or
    /// not at all when the row left `pending` meanwhile (cancelled: nothing is kept, the provider's
    /// app may exist).
    async fn save(&self, r: &Row, client_id: &str, secret: Secret) -> VaultResult<()> {
        let who = r.who();
        let fields = vec![(APP_SECRET.to_string(), secret.0)];
        let prepared = self
            .conns
            .prepare_credential(&who, ITEM_TITLE, fields)
            .await?;
        let (conns, now) = (self.conns.clone(), self.now());
        let me = self.clone();
        let saved = me.tx(|tx| {
            // still pending, or ended by its deadline meanwhile (the credential was issued in time:
            // keep it); a cancel is never overridden
            let still: bool = tx
                .query_row(
                    "SELECT state = ? OR (state = ? AND error_kind IN (?, ?)) FROM chan_app_registrations WHERE id = ?",
                    params![
                        state::PENDING,
                        state::EXPIRED,
                        Kind::Expired.code(),
                        Kind::CredentialsLost.code(),
                        r.id
                    ],
                    |x| x.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !still {
                tx.execute(
                    "UPDATE chan_app_registrations SET app_may_exist = 1, updated = ? WHERE id = ?",
                    params![now, r.id],
                )?;
                return Ok(false);
            }
            let item = conns.insert_prepared(tx, &prepared)?;
            // kept even if the initiator left (the only copy of the secret), but never bound for them
            let member = is_member(tx, &r.ws, &r.initiator)?;
            let (err, kind) = if member {
                (None, None)
            } else {
                (Some(Kind::MemberGone.text()), Some(Kind::MemberGone.code()))
            };
            tx.execute(
                "UPDATE chan_app_registrations SET state = ?, vault_item = ?, client_id = ?,
                   user_code = NULL, verification_uri_complete = NULL, error = ?, error_kind = ?, updated = ?
                  WHERE id = ? AND state IN ('pending', 'expired')",
                params![state::SAVED, item, client_id, err, kind, now, r.id],
            )?;
            self.drop_ticket(tx, &r.id)?;
            Ok(true)
        })?;
        if saved {
            tracing::info!(registration = %r.id, "feishu app registration: credential saved");
        } else {
            tracing::info!(registration = %r.id, "feishu app registration: issued after cancel, not kept");
        }
        Ok(())
    }

    /// Checks the saved credential and binds a new bot connection on it.
    async fn verify(&self, id: &str) -> VaultResult<()> {
        let now = self.now();
        let claimed = self.tx(|tx| {
            Ok(tx.execute(
                "UPDATE chan_app_registrations SET state = ?, error = NULL, error_kind = NULL, updated = ?
                  WHERE id = ? AND state = ? AND vault_item IS NOT NULL",
                params![state::VERIFYING, now, id, state::SAVED],
            )? == 1)
        })?;
        if !claimed {
            return Ok(());
        }
        let Some(r) = self.read(|c| load(c, id))? else {
            return Ok(());
        };
        let member = self.read(|c| is_member(c, &r.ws, &r.initiator))?;
        if !member {
            return self.back_to_saved(id, Kind::MemberGone);
        }
        let (Some(item), Some(app_id)) = (r.vault_item.clone(), r.client_id.clone()) else {
            return self.back_to_saved(id, Kind::VerifyFailed);
        };
        let mut config = json!({"app_id": app_id, "domain": r.brand});
        if let Some(b) = &self.bases.api_base {
            config["api_base"] = json!(b);
        }
        let rid = id.to_string();
        let (ws, initiator) = (r.ws.clone(), r.initiator.clone());
        let gone = Arc::new(Mutex::new(false));
        let gone_in = gone.clone();
        let res = self
            .conns
            .bind_existing(
                &r.who(),
                APP_ADAPTER,
                &item,
                config,
                Vec::new(),
                None,
                move |tx, done| {
                    // the binding commits only for a member, and only if nobody cancelled meanwhile
                    if !is_member(tx, &ws, &initiator)? {
                        *lock(&gone_in) = true;
                        return Err(VaultError::Api(ApiError::conflict(Kind::MemberGone.text())));
                    }
                    let n = tx.execute(
                        "UPDATE chan_app_registrations SET state = ?, vault_item = NULL, connection_id = ?,
                           account_id = ?, error = NULL, error_kind = NULL, updated = ? WHERE id = ? AND state = ?",
                        params![
                            state::SUCCEEDED,
                            done.connection_id,
                            done.account_id,
                            steve_store::now(),
                            rid,
                            state::VERIFYING
                        ],
                    )?;
                    if n != 1 {
                        return Err(VaultError::Api(ApiError::conflict("这次注册已经取消")));
                    }
                    Ok(())
                },
            )
            .await;
        match res {
            Ok(d) => {
                tracing::info!(registration = %id, connection = %d.connection_id, "feishu app registration succeeded");
                Ok(())
            }
            Err(_) if *lock(&gone) => self.back_to_saved(id, Kind::MemberGone),
            Err(e) => {
                let kind = match &e {
                    VaultError::Api(ApiError::Conflict(_)) => Kind::Conflict,
                    VaultError::Api(ApiError::Rule(_)) => Kind::VerifyFailed,
                    _ => Kind::Network,
                };
                tracing::info!(registration = %id, kind = kind.code(), "feishu app registration: binding did not go through");
                self.back_to_saved(id, kind)
            }
        }
    }

    /// A check that did not bind: back to `credentials_saved` with the reason (if still verifying;
    /// a cancel committed first stays cancelled).
    fn back_to_saved(&self, id: &str, kind: Kind) -> VaultResult<()> {
        let now = self.now();
        self.tx(|tx| {
            tx.execute(
                "UPDATE chan_app_registrations SET state = ?, error = ?, error_kind = ?, updated = ?
                  WHERE id = ? AND state = ?",
                params![state::SAVED, kind.text(), kind.code(), now, id, state::VERIFYING],
            )?;
            Ok(())
        })
    }
}
