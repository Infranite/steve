//! Attempts on the database and their background work (poll, save, check and bind). The rules are
//! in the module docs.

mod polling;
use polling::{PollControl, PollReply};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::vault::{VaultError, VaultResult};
use steve_modules::{ApiError, Caller, Modules};
use steve_store::feishu_registration::{self as repo, Registration as Row};
use steve_store::{R, W, uid};

pub use steve_store::feishu_registration::state;
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
    /// The kind of a stored `error_kind` code.
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::Denied,
            Self::Expired,
            Self::Network,
            Self::Provider,
            Self::VerifyFailed,
            Self::Conflict,
            Self::MemberGone,
            Self::CredentialsLost,
        ]
        .into_iter()
        .find(|k| k.code() == code)
    }

    /// [`Kind::text`] in the request's language, for API answers.
    pub fn message(self) -> String {
        match self {
            Self::Denied => steve_i18n::t!("channels.registration.denied"),
            Self::Expired => steve_i18n::t!("channels.registration.expired"),
            Self::Network => steve_i18n::t!("channels.registration.network"),
            Self::Provider => steve_i18n::t!("channels.registration.provider"),
            Self::VerifyFailed => steve_i18n::t!("channels.registration.verify_failed"),
            Self::Conflict => steve_i18n::t!("channels.registration.conflict"),
            Self::MemberGone => steve_i18n::t!("channels.registration.member_gone"),
            Self::CredentialsLost => steve_i18n::t!("channels.registration.credentials_lost"),
        }
    }

    /// The text stored in `error` (zh-CN); answers show [`Kind::message`] for its `error_kind`.
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

trait RegistrationView {
    fn who(&self) -> Caller;
    fn brand(&self) -> Brand;
    fn out(&self) -> Value;
}

impl RegistrationView for Row {
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
        // the stored text is zh-CN: an answer words a known kind in the request's language
        let error = match self.error_kind.as_deref().and_then(Kind::from_code) {
            Some(k) if self.error.is_some() => Some(k.message()),
            _ => self.error.clone(),
        };
        put("error", &error);
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

    fn read<T>(&self, f: impl FnOnce(&mut R<'_>) -> steve_store::Result<T>) -> VaultResult<T> {
        Ok(self.modules().store.read(f).map_err(store_err)?)
    }

    fn write<T>(&self, f: impl FnOnce(&mut W<'_>) -> VaultResult<T>) -> VaultResult<T> {
        self.modules().store.write(f)
    }

    /// The caller's own attempt `id` (404 for anyone else's).
    fn own(&self, who: &Caller, id: &str) -> VaultResult<Row> {
        self.read(|c| repo::get(c, id))?
            .filter(|r| r.ws == who.ws && r.initiator == who.me)
            .ok_or_else(|| ApiError::not_found("应用注册").into())
    }

    /// Settles an overdue `pending` row as expired (any reader may); returns whether it did.
    fn settle_overdue(&self, tx: &mut W<'_>, r: &Row) -> VaultResult<bool> {
        if r.state != state::PENDING || r.expires_at > self.now() {
            return Ok(false);
        }
        // a running poll ends its own attempt (it may be saving a credential issued in time)
        if lock(&self.running).contains(&format!("poll:{}", r.id)) {
            return Ok(false);
        }
        self.drop_ticket(tx, &r.id)?;
        Ok(repo::finish_pending(
            tx,
            &r.id,
            state::EXPIRED,
            (Kind::Expired.text(), Kind::Expired.code()),
            false,
            self.now(),
        )?)
    }

    /// Destroys the row's bearer ticket (in `tx`) and forgets it.
    fn drop_ticket(&self, tx: &mut W<'_>, id: &str) -> VaultResult<()> {
        let Some(r) = repo::get(tx, id)? else {
            return Ok(());
        };
        if let Some(item) = &r.device_item {
            self.conns.destroy_item(tx, &r.ws, &r.initiator, item)?;
            repo::clear_ticket(tx, id)?;
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

    /// `GET current`: the caller's active attempt here, after settling an overdue one.
    pub fn current(&self, who: &Caller) -> VaultResult<Option<Value>> {
        let r = self.read(|c| repo::for_initiator(c, &who.ws, &who.me))?;
        let Some(r) = r else { return Ok(None) };
        let settled = self.write(|tx| self.settle_overdue(tx, &r))?;
        if settled {
            return Ok(None);
        }
        self.ensure_running(&r);
        Ok(Some(r.out()))
    }

    /// `GET :id`.
    pub fn get(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own(who, id)?;
        if self.write(|tx| self.settle_overdue(tx, &r))? {
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
        if !self.read(|c| steve_store::settings::user_is_member(c, &who.ws, &who.me))? {
            return Err(ApiError::forbidden(steve_i18n::t!("channels.error.not_member")).into());
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
            .map_err(|k| ApiError::rule(k.message()))?;
        let b = (status < 400)
            .then(|| parse_begin(&v, &self.bases, brand))
            .flatten()
            .ok_or_else(|| {
                tracing::warn!(status, "feishu app registration: begin refused");
                ApiError::rule(Kind::Provider.message())
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
        let inserted = self.write(|tx| {
            // a concurrent begin of the same person won the slot: answer with theirs
            if let Some(r) = repo::for_initiator(tx, &who.ws, &who.me)? {
                return Ok(Err(r));
            }
            let item = conns.insert_prepared(tx, &ticket)?;
            repo::create(
                tx,
                repo::NewRegistration {
                    id: &id,
                    ws: &who.ws,
                    initiator: &who.me,
                    requested_brand: brand.code(),
                    brand: Brand::Feishu.code(),
                    device_item: &item,
                    user_code: &b.user_code,
                    page: &b.verification_uri_complete,
                    interval_s: b.interval_s,
                    expires_at: now + b.expire_s * 1000,
                    now,
                },
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
        self.write(|tx| {
            let Some(cur) = repo::get(tx, &r.id)? else {
                return Ok(());
            };
            if !state::ACTIVE.contains(&cur.state.as_str()) {
                return Ok(());
            }
            for item in [&cur.vault_item, &cur.device_item].into_iter().flatten() {
                conns.destroy_item(tx, &cur.ws, &cur.initiator, item)?;
            }
            repo::cancel(tx, &cur.id, now)?;
            Ok(())
        })?;
        tracing::info!(registration = %r.id, "feishu app registration cancel requested");
        Ok(self.own(who, id)?.out())
    }

    /// `POST :id/retry`: checks the saved credential again (`credentials_saved` only).
    pub fn retry(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own(who, id)?;
        if r.state != state::SAVED {
            return Err(
                ApiError::conflict(steve_i18n::t!("channels.error.nothing_to_retry")).into(),
            );
        }
        self.spawn_verify(r.id.clone());
        Ok(self.own(who, id)?.out())
    }

    /// At start: resumes every attempt a stop interrupted (pending until its own deadline, a
    /// half-done check, a saved credential not yet checked). Returns how many it picked up.
    pub fn resume(&self) -> VaultResult<usize> {
        let now = self.now();
        let rows = self.read(|r| repo::active(r))?;
        let mut n = 0;
        for r in rows {
            match r.state.as_str() {
                state::PENDING if r.expires_at <= now => {
                    self.write(|tx| self.settle_overdue(tx, &r))?;
                }
                state::PENDING => {
                    self.spawn_poll(r.id.clone(), true);
                    n += 1;
                }
                state::VERIFYING => {
                    // stopped mid-check: the binding never committed (it would have said succeeded)
                    self.write(|tx| {
                        repo::retry_interrupted_check(tx, &r.id, now)?;
                        Ok(())
                    })?;
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
                let reset = self.write(|tx| {
                    repo::retry_interrupted_check(tx, &r.id, now)?;
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
        self.write(|tx| {
            if repo::finish_pending(tx, id, st, (kind.text(), kind.code()), may_exist, now)? {
                self.drop_ticket(tx, id)?;
            }
            Ok(())
        })
    }

    async fn poll_until_done(&self, id: &str, resumed: bool) -> VaultResult<()> {
        let mut first = true;
        let mut slept = false;
        loop {
            let Some(r) = self.read(|c| repo::get(c, id))? else {
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
            let v = match self.request_poll(id, &r, code.as_str(), resumed).await? {
                PollReply::Answer(v) => v,
                PollReply::Retry => continue,
                PollReply::Finished => return Ok(()),
            };
            match self.apply_poll_response(id, &r, &v, resumed).await? {
                PollControl::Wait => {}
                PollControl::Switched => first = true,
                PollControl::Finished => return Ok(()),
            }
        }
    }

    fn set_interval(&self, id: &str, n: i64) -> VaultResult<()> {
        let now = self.now();
        self.write(|tx| {
            repo::set_interval(tx, id, n, now)?;
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
        let saved = me.write(|tx| {
            // still pending, or ended by its deadline meanwhile (the credential was issued in time:
            // keep it); a cancel is never overridden
            let still = repo::can_save(
                tx,
                &r.id,
                [Kind::Expired.code(), Kind::CredentialsLost.code()],
            )?;
            if !still {
                repo::mark_app_may_exist(tx, &r.id, now)?;
                return Ok(false);
            }
            let item = conns.insert_prepared(tx, &prepared)?;
            // kept even if the initiator left (the only copy of the secret), but never bound for them
            let member = steve_store::settings::user_is_member(tx, &r.ws, &r.initiator)?;
            let error = (!member).then(|| (Kind::MemberGone.text(), Kind::MemberGone.code()));
            repo::save(tx, &r.id, &item, client_id, error, now)?;
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
        let claimed = self.write(|w| Ok(repo::claim_verification(w, id, now)?))?;
        if !claimed {
            return Ok(());
        }
        let Some(r) = self.read(|c| repo::get(c, id))? else {
            return Ok(());
        };
        let member =
            self.read(|c| steve_store::settings::user_is_member(c, &r.ws, &r.initiator))?;
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
                move |w, done| {
                    if !steve_store::settings::user_is_member(w, &ws, &initiator)? {
                        *lock(&gone_in) = true;
                        return Err(VaultError::Api(ApiError::conflict(Kind::MemberGone.text())));
                    }
                    if !repo::bound(
                        w,
                        &rid,
                        &done.connection_id,
                        &done.account_id,
                        steve_store::now(),
                    )? {
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
        self.write(|tx| {
            repo::verification_failed(tx, id, (kind.text(), kind.code()), now)?;
            Ok(())
        })
    }
}
