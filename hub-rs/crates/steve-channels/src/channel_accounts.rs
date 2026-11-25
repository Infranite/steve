//! 接入账号: a connection whose adapter can
//! receive messages (`msg.receive` among its provider's caps), with its `channel_settings`, its
//! default persona and the permission check `verify` stored on the connection.
//!
//! Visibility is the connections module's: a connection of someone's “我” (your own Feishu account)
//! is theirs alone (`404 没有这个连接` for everybody else); the bot's are the workspace's.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::{ApiError, Check};
use steve_store::base::Viewer;

use crate::bindings::{self, PersonaRef};
use crate::channel::ChannelFeatures;
use crate::ext::{ChannelDecl, Receive};
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels};

/// `channel_settings.trigger` of a new account as it reads (the column default's `where: []`
/// means everywhere).
pub fn default_trigger() -> Value {
    json!({"on": false, "words": ["/ai", "/bot"], "where": ["*"], "who": ["me"], "reply": "thread", "mark": true, "ignore_prefixes": []})
}

/// What the account page shows of a channel provider (`GET /channels` `providers`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderOut {
    pub system: String,
    pub name: String,
    pub features: ChannelFeatures,
    /// The Hub has a channel adapter for it (the Feishu bot app registered with [`Channels`]).
    pub adapter_installed: bool,
    /// The declared channel sides of the provider ([`decl_summary`]).
    pub sides: Vec<Value>,
}

/// What a channel side declares, as the API shows it (`GET /channels` `providers[].sides`,
/// `GET /connections/integrations` `adapters[].channel`): `{adapter, handle_system, mode,
/// features, receive, receive_cap, message_ids, created_precision_ms, start}`.
pub fn decl_summary(d: &ChannelDecl) -> Value {
    json!({
        "adapter": d.adapter,
        "handle_system": d.handle_system,
        "mode": d.mode,
        "features": d.features,
        "receive": d.receive,
        "receive_cap": d.receive_cap,
        "message_ids": d.message_ids,
        "created_precision_ms": d.created_precision_ms,
        "start": d.start,
    })
}

pub(crate) fn can_manage(
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    conn: &str,
) -> ChannelResult<()> {
    own_conn(r, viewer, conn).map(|_| ())
}

/// The `ChannelAccount` shape, plus the connection's permission check (`verify`):
/// `permissions` (rows `{scope, purpose, level, status}`), `missing_permissions` (scopes) and
/// `self_built` (`true` when the app is a 企业自建应用, `null` when unknown).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountOut {
    pub connection_id: String,
    pub account_id: String,
    /// `connections.adapter` (`feishu_app`, `feishu_user`, an extension's).
    pub adapter: String,
    pub system: String,
    pub provider: String,
    pub mode: Mode,
    pub name: String,
    pub platform_name: String,
    /// `connections.status`: `ok | reauth | error | paused | revoked`.
    pub status: String,
    pub caps: Vec<String>,
    pub persona: Option<PersonaRef>,
    pub duty: String,
    pub rules: Value,
    /// Only your own accounts; `null` for bots.
    pub trigger: Option<Value>,
    pub features: ChannelFeatures,
    /// How its messages come in, as its side declares (`null` without a declaration).
    pub receive: Option<Receive>,
    /// Active spaces of this account.
    pub spaces: i64,
    pub permissions: Option<Value>,
    pub missing_permissions: Vec<String>,
    pub self_built: Option<bool>,
    pub permissions_checked_at: Option<i64>,
    pub revision: i64,
    /// The undo window after you approve a message, in seconds, as set (`null`: the default).
    pub send_delay_s: Option<i64>,
    /// The window it has now: the setting, or the identity's default (bot 3 s, you 10 s).
    pub send_delay_default_s: i64,
    /// On your own account: hours after which a card about one of its chats that nobody answered
    /// closes (`null`: never on time; a starred contact's card and a commitment never do).
    pub card_ttl_h: Option<i64>,
    /// How late a message may arrive and still be acted on, in minutes.
    pub late_minutes: i64,
    /// Messages only recorded as history: `{before, late, at}` (written before the connection was
    /// on, arrived too late, the last one's time).
    pub history: Value,
}

/// `system` and `provider` of an adapter: its provider's integration (the adapters of one provider
/// are listed together; both Feishu connections are “飞书 / Lark”).
fn provider_of(ch: &Channels, adapter: &str) -> (String, String) {
    match ch.connections().provider(adapter) {
        Some(p) => {
            let i = p.integration();
            (i.provider, i.provider_name)
        }
        None => (adapter.to_string(), adapter.to_string()),
    }
}

/// What a channel account can do: what its side declares ([`crate::ext::ChannelDecl::features`]),
/// else its channel adapter's features (your own Feishu account sends as you, replies in threads,
/// reacts and recalls its own messages, but has no cards, buttons or edits).
fn features_of(ch: &Channels, adapter: &str) -> ChannelFeatures {
    if let Some(d) = ch.decl(adapter) {
        return d.features;
    }
    ch.adapter(adapter)
        .map(|a| a.features())
        .unwrap_or_default()
}

/// The channel providers: every provider with a declared channel side, a channel adapter, or a
/// connection provider supporting `msg.receive`. A provider is installed when one of its sides has
/// a channel adapter (or act-as-user side) and a connection provider; its features are that side's.
pub fn providers(ch: &Channels) -> Vec<ProviderOut> {
    let mut out: Vec<ProviderOut> = Vec::new();
    let mut ids: Vec<String> = ch.decls().map(|d| d.adapter.clone()).collect();
    for a in ch.adapters() {
        if !ids.iter().any(|x| x == a.id()) {
            ids.push(a.id().to_string());
        }
    }
    for p in ch.connections().providers() {
        if p.caps().contains(&"msg.receive") && !ids.iter().any(|x| x == p.id()) {
            ids.push(p.id().to_string());
        }
    }
    for id in ids {
        let (system, name) = provider_of(ch, &id);
        let k = match out.iter().position(|p| p.system == system) {
            Some(k) => k,
            None => {
                out.push(ProviderOut {
                    system,
                    name,
                    features: ChannelFeatures::default(),
                    adapter_installed: false,
                    sides: Vec::new(),
                });
                out.len() - 1
            }
        };
        if let Some(d) = ch.decl(&id) {
            out[k].sides.push(decl_summary(d));
        }
        if !out[k].adapter_installed
            && ch.adapter(&id).is_some()
            && ch.connections().provider(&id).is_some()
        {
            out[k].features = features_of(ch, &id);
            out[k].adapter_installed = true;
        }
    }
    out
}

/// Whether connections of `adapter` are channel accounts (their provider receives messages).
pub fn is_channel_adapter(ch: &Channels, adapter: &str) -> bool {
    ch.adapter(adapter).is_some()
        || ch
            .connections()
            .provider(adapter)
            .is_some_and(|p| p.caps().contains(&"msg.receive"))
}

struct ConnFacts {
    account_id: String,
    adapter: String,
    status: String,
    caps: Vec<String>,
    config: Value,
    name: String,
}

fn own_conn(
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    conn: &str,
) -> ChannelResult<ConnFacts> {
    use steve_store::{Accounts, Connections, parties};
    let row = Connections(r)
        .get(conn)?
        .filter(|row| row.workspace_id == viewer.ws)
        .ok_or_else(|| ApiError::not_found("连接"))?;
    let account = Accounts(r)
        .get(&row.account_id)?
        .ok_or_else(|| ApiError::not_found("连接"))?;
    if parties::user_id(r, &account.party_id)?.is_some_and(|user| user != viewer.user) {
        return Err(ApiError::not_found("连接"));
    }
    Ok(ConnFacts {
        account_id: row.account_id,
        adapter: row.adapter,
        status: row.status,
        caps: row.caps,
        config: row.config,
        name: if account.display_name.is_empty() {
            account.handle_display
        } else {
            account.display_name
        },
    })
}

/// The `ChannelAccount` of `conn` (no settings row yet: the mode's defaults, nothing written).
pub fn out(
    ch: &Channels,
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    conn: &str,
) -> ChannelResult<AccountOut> {
    let f = own_conn(r, viewer, conn)?;
    if !is_channel_adapter(ch, &f.adapter) {
        return Err(ApiError::not_found("连接"));
    }
    let ws = &viewer.ws;
    let (_, mode) = bindings::connection_account(r, ws, conn)?;
    let settings = spaces::channel_settings(r, ws, conn)?;
    let send_delay_s = settings.as_ref().and_then(|s| s.send_delay_s);
    let card_ttl_h = settings.as_ref().and_then(|s| s.card_ttl_h);
    let (late_ms, before, late, at) = settings.as_ref().map_or((None, 0, 0, None), |s| {
        (s.late_ms, s.before, s.late, s.history_at)
    });
    let (duty, rules, trigger, revision) = match settings {
        Some(s) => (s.duty, s.rules, s.trigger, s.revision),
        None => (
            String::new(),
            match mode {
                Mode::Bot => spaces::default_bot_rules(),
                Mode::Own => spaces::default_self_rules(),
            },
            Value::Null,
            0,
        ),
    };
    let trigger = match mode {
        Mode::Bot => None,
        Mode::Own => Some(normal_trigger(&trigger)),
    };
    let spaces = steve_store::channel_spaces::rows::active_count(r, ws, conn)?;
    let late_minutes = late_ms
        .filter(|l| crate::inbound::LATE_MS.contains(l))
        .unwrap_or(crate::inbound::DEFAULT_LATE_MS)
        / 60_000;
    let (system, provider) = provider_of(ch, &f.adapter);
    let platform_name = match mode {
        Mode::Bot => format!("{}（应用机器人）", f.name),
        Mode::Own => format!("{}（你本人）", f.name),
    };
    let permissions = f
        .config
        .get("permissions")
        .filter(|p| p.is_array())
        .cloned();
    let missing_permissions = f
        .config
        .get("missing_permissions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    Ok(AccountOut {
        connection_id: conn.to_string(),
        account_id: f.account_id,
        persona: bindings::account_persona(r, ws, conn)?,
        adapter: f.adapter.clone(),
        system,
        provider,
        mode,
        name: f.name,
        platform_name,
        status: f.status,
        caps: f.caps,
        duty,
        rules,
        trigger,
        features: features_of(ch, &f.adapter),
        receive: ch.decl(&f.adapter).map(|d| d.receive),
        spaces,
        permissions,
        missing_permissions,
        self_built: f.config.get("self_built").and_then(Value::as_bool),
        permissions_checked_at: f
            .config
            .get("permissions_checked_at")
            .and_then(Value::as_i64),
        revision,
        send_delay_s,
        send_delay_default_s: crate::outbound::default_send_delay_ms(mode) / 1000,
        card_ttl_h,
        late_minutes,
        history: json!({"before": before, "late": late, "at": at}),
    })
}

/// Every channel account the viewer sees (not disconnected), oldest first.
pub fn list(
    ch: &Channels,
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
) -> ChannelResult<Vec<AccountOut>> {
    let ids = steve_store::Connections(r)
        .list(&viewer.ws)?
        .into_iter()
        .filter(|conn| conn.status != "revoked")
        .map(|conn| (conn.id, conn.adapter));
    let mut out = Vec::new();
    for (id, adapter) in ids {
        if !is_channel_adapter(ch, &adapter) {
            continue;
        }
        match out_or_hidden(ch, r, viewer, &id)? {
            Some(a) => out.push(a),
            None => continue,
        }
    }
    Ok(out)
}

fn out_or_hidden(
    ch: &Channels,
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    id: &str,
) -> ChannelResult<Option<AccountOut>> {
    match out(ch, r, viewer, id) {
        Ok(a) => Ok(Some(a)),
        Err(ApiError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `trigger` with every key present.
fn normal_trigger(v: &Value) -> Value {
    let d = default_trigger();
    let pick = |k: &str, ok: fn(&Value) -> bool| {
        v.get(k)
            .filter(|x| ok(x))
            .cloned()
            .unwrap_or_else(|| d[k].clone())
    };
    let t = crate::rules::Trigger::from_value(v);
    json!({
        "on": pick("on", Value::is_boolean),
        "words": pick("words", Value::is_array),
        "where": t.r#where,
        "who": t.who,
        "reply": t.reply.code(),
        "mark": pick("mark", Value::is_boolean),
        "ignore_prefixes": t.ignore_prefixes,
    })
}

/// `PATCH /channels/:connection_id`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPatch {
    pub duty: Option<String>,
    pub rules: Option<Value>,
    pub trigger: Option<TriggerPatch>,
    pub persona_id: Option<String>,
    /// The undo window in seconds (0–60); `null` puts back the identity's default.
    #[serde(default, deserialize_with = "present")]
    pub send_delay_s: Option<Option<i64>>,
    /// How late a message may arrive and still be acted on, in minutes (1 … 10080; default 30).
    pub late_minutes: Option<i64>,
    /// Only on your own account: hours after which an unanswered card about one of its chats closes
    /// (1 … [`crate::decide::CARD_TTL_MAX_H`]); `null`: never on time (the default).
    #[serde(default, deserialize_with = "present")]
    pub card_ttl_h: Option<Option<i64>>,
}

/// A key that is there, `null` included (`Some(None)`); absent stays `None`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
    Option::<i64>::deserialize(d).map(Some)
}

/// `trigger` of [`ChannelPatch`]: only the keys given change.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerPatch {
    pub on: Option<bool>,
    pub words: Option<Vec<String>>,
    #[serde(rename = "where")]
    pub where_: Option<Vec<String>>,
    /// `me`, `*`, or contact ids.
    pub who: Option<Vec<String>>,
    /// `thread`, `quote` or `message`.
    pub reply: Option<String>,
    pub mark: Option<bool>,
    pub ignore_prefixes: Option<Vec<String>>,
}

/// `/^\/[A-Za-z0-9_]{1,20}$/`
fn trigger_word_ok(w: &str) -> bool {
    let Some(rest) = w.strip_prefix('/') else {
        return false;
    };
    (1..=20).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Where trigger words count: `*` (everywhere), `dm:*` (every DM) or a space of this account.
pub const ALL_DMS: &str = crate::rules::WHERE_ALL_DMS;

impl ChannelPatch {
    pub fn check(&self) -> ChannelResult<()> {
        let mut c = Check::new();
        c.opt_len("duty", self.duty.as_deref(), 0, 4000);
        if let Some(r) = &self.rules {
            c.that("rules", r.is_array());
        }
        if let Some(p) = &self.persona_id {
            c.len("persona_id", p, 1, 100);
        }
        if let Some(Some(d)) = self.send_delay_s {
            c.that("send_delay_s", (0..=60).contains(&d));
        }
        if let Some(Some(h)) = self.card_ttl_h {
            c.that(
                "card_ttl_h",
                (1..=crate::decide::CARD_TTL_MAX_H).contains(&h),
            );
        }
        if let Some(m) = self.late_minutes {
            c.that(
                "late_minutes",
                crate::inbound::LATE_MS.contains(&m.saturating_mul(60_000)),
            );
        }
        if let Some(t) = &self.trigger {
            if let Some(prefixes) = &t.ignore_prefixes {
                c.that("trigger.ignore_prefixes", prefixes.len() <= 20);
                for (i, prefix) in prefixes.iter().enumerate() {
                    c.len(
                        &format!("trigger.ignore_prefixes.{i}"),
                        prefix.trim(),
                        1,
                        100,
                    );
                    c.that("trigger.ignore_prefixes", !prefix.contains(['\n', '\r']));
                }
            }
            if let Some(w) = &t.words {
                c.that("trigger.words", w.len() <= 10);
                for (i, x) in w.iter().enumerate() {
                    c.that(&format!("trigger.words.{i}"), trigger_word_ok(x));
                }
            }
            if let Some(w) = &t.where_ {
                c.list("trigger.where", w, 200, 100);
            }
            if let Some(w) = &t.who {
                c.list("trigger.who", w, 200, 100);
            }
            if let Some(r) = &t.reply {
                c.that(
                    "trigger.reply",
                    crate::rules::ReplyStyle::parse(r).is_some(),
                );
            }
        }
        c.done()
    }

    fn fields(&self) -> Vec<&'static str> {
        [
            ("duty", self.duty.is_some()),
            ("rules", self.rules.is_some()),
            ("trigger", self.trigger.is_some()),
            ("persona_id", self.persona_id.is_some()),
            ("send_delay_s", self.send_delay_s.is_some()),
            ("late_minutes", self.late_minutes.is_some()),
            ("card_ttl_h", self.card_ttl_h.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, on)| on.then_some(k))
        .collect()
    }
}

/// Applies a patch in one revision of `channel` (`{persona_id, duty, rules, trigger}` before and
/// after), audit `channel.patch`, SSE `channel`. Your own account with `persona_id` → `400`; a bot
/// with `trigger` → `400`; rules validated (your own account cannot `reply`); trigger words
/// `/^\/[A-Za-z0-9_]{1,20}$/` (≤10), `where` = `*`, `dm:*` or spaces of this account (none = `*`).
pub fn patch(
    ch: &Channels,
    tx: &mut steve_store::W<'_>,
    viewer: &Viewer,
    conn: &str,
    p: &ChannelPatch,
    now: i64,
) -> ChannelResult<()> {
    p.check()?;
    let f = own_conn(tx, viewer, conn)?;
    if !is_channel_adapter(ch, &f.adapter) {
        return Err(ApiError::not_found("连接"));
    }
    let ws = &viewer.ws;
    let (_, mode) = bindings::connection_account(tx, ws, conn)?;
    if p.persona_id.is_some() && mode == Mode::Own {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.self_no_persona"
        )));
    }
    if p.trigger.is_some() && mode == Mode::Bot {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.trigger_self_only"
        )));
    }
    // a card's lifetime is about cards on your own account; a bot's hand-offs are left alone
    if matches!(p.card_ttl_h, Some(Some(_))) && mode == Mode::Bot {
        return Err(ApiError::invalid("card_ttl_h"));
    }
    let fields = p.fields();
    if fields.is_empty() {
        return Ok(());
    }
    let before = spaces::ensure_channel_settings(tx, ws, conn, mode, now)?;
    let persona_before = bindings::account_persona(tx, ws, conn)?.map(|x| x.id);
    let rules = match &p.rules {
        Some(v) => {
            let parsed = crate::rules::parse(v, mode)?;
            crate::rules::check_refs(tx, ws, &parsed)?;
            crate::rules::to_value(&parsed)
        }
        None => before.rules.clone(),
    };
    let trigger_before = match mode {
        Mode::Own => normal_trigger(&before.trigger),
        Mode::Bot => Value::Null,
    };
    let trigger = match &p.trigger {
        Some(t) => {
            let mut v = trigger_before.clone();
            if let Some(x) = t.on {
                v["on"] = json!(x);
            }
            if let Some(x) = &t.words {
                let mut words: Vec<&String> = Vec::new();
                for w in x {
                    if !words.contains(&w) {
                        words.push(w);
                    }
                }
                v["words"] = json!(words);
            }
            if let Some(x) = &t.where_ {
                let mut out: Vec<&String> = Vec::new();
                for (i, w) in x.iter().enumerate() {
                    // `/bot` is your command anywhere: any space of the workspace you can see
                    let ok = w == ALL_DMS
                        || w == crate::rules::WHERE_EVERYWHERE
                        || steve_store::channel_spaces::rows::get_in_workspace(tx, ws, w)?
                            .is_some();
                    if !ok {
                        return Err(ApiError::invalid(format!("trigger.where.{i}")));
                    }
                    // 屏蔽 wins over trigger words: choosing a blocked space would never work
                    if let Some(s) = steve_store::channel_spaces::rows::get(tx, w)?
                        && spaces::settings(tx, &s.id)
                            .is_ok_and(|x| x.attention == spaces::Attention::Blocked)
                    {
                        return Err(ApiError::rule(if s.name.trim().is_empty() {
                            steve_i18n::t!("channels.error.trigger_blocked_unnamed")
                        } else {
                            steve_i18n::t!("channels.error.trigger_blocked", space = s.name.trim())
                        }));
                    }
                    if !out.contains(&w) {
                        out.push(w);
                    }
                }
                // nothing chosen, or `*` among them: everywhere
                v["where"] = json!(crate::rules::normal_where(
                    out.into_iter().cloned().collect()
                ));
            }
            if let Some(x) = &t.who {
                let mut out: Vec<&String> = Vec::new();
                for (i, w) in x.iter().enumerate() {
                    // you, anyone, or a contact of this workspace
                    let ok = w == crate::rules::WHO_ME
                        || w == crate::rules::WHO_ANYONE
                        || steve_store::Contacts::new(tx)
                            .party(w)?
                            .is_some_and(|party| party.workspace_id == *ws);
                    if !ok {
                        return Err(ApiError::invalid(format!("trigger.who.{i}")));
                    }
                    if !out.contains(&w) {
                        out.push(w);
                    }
                }
                v["who"] = if out.is_empty() {
                    json!([crate::rules::WHO_ME])
                } else {
                    json!(out)
                };
            }
            if let Some(x) = &t.reply {
                v["reply"] = json!(x);
            }
            if let Some(x) = t.mark {
                v["mark"] = json!(x);
            }
            if let Some(prefixes) = &t.ignore_prefixes {
                let mut unique = Vec::new();
                for prefix in prefixes {
                    let prefix = prefix.trim();
                    if !unique.contains(&prefix) {
                        unique.push(prefix);
                    }
                }
                v["ignore_prefixes"] = json!(unique);
            }
            v
        }
        None => trigger_before.clone(),
    };
    let duty = p
        .duty
        .as_deref()
        .map(|d| d.trim().to_string())
        .unwrap_or_else(|| before.duty.clone());
    if let Some(pid) = &p.persona_id {
        bindings::set_account_default(tx, viewer, conn, pid, now)?;
    }
    let persona_after = bindings::account_persona(tx, ws, conn)?.map(|x| x.id);
    let revision = before.revision + 1;
    let send_delay_s = p.send_delay_s.unwrap_or(before.send_delay_s);
    let stored_trigger = match mode {
        Mode::Own => trigger.clone(),
        Mode::Bot if before.trigger.is_null() => default_trigger(),
        Mode::Bot => before.trigger.clone(),
    };
    let fields_patch = steve_store::channel_spaces::account_settings::SettingsPatch {
        duty: Some(&duty),
        rules: Some(&rules),
        trigger: Some(&stored_trigger),
        send_delay_s: Some(send_delay_s),
        revision,
        card_ttl_h: p.card_ttl_h,
        late_ms: p.late_minutes.map(|m| m * 60_000),
        ai_mark: None,
    };
    steve_store::channel_spaces::account_settings::patch(tx, conn, &fields_patch, now)?;
    let snap = |persona: &Option<String>,
                duty: &str,
                rules: &Value,
                trigger: &Value,
                delay: Option<i64>| json!({"persona_id": persona, "duty": duty, "rules": rules, "trigger": trigger, "send_delay_s": delay});
    steve_store::audit_repo::revise(
        tx,
        ws,
        "channel",
        conn,
        revision,
        &snap(
            &persona_before,
            &before.duty,
            &before.rules,
            &trigger_before,
            before.send_delay_s,
        ),
        &snap(&persona_after, &duty, &rules, &trigger, send_delay_s),
        &viewer.user,
    )?;
    steve_store::audit_repo::record(
        tx,
        ws,
        &viewer.user,
        "channel.patch",
        conn,
        &json!({ "fields": fields }),
    )?;
    tx.emit(steve_types::DomainEvent::from(
        steve_types::ChannelsEvent::Channel {
            id: conn.into(),
            ws: ws.clone().into(),
        },
    ));
    Ok(())
}

/// After `POST /channels` made a connection from a credential form: its
/// `channel_settings(mode)`, the default persona bound (a bot's, when given; else the account
/// speaks as the default persona “Steve”), audit `channel.create`, SSE `channel`.
pub fn created(
    tx: &mut steve_store::W<'_>,
    viewer: &Viewer,
    conn: &str,
    mode: Mode,
    persona: Option<&str>,
    now: i64,
) -> ChannelResult<()> {
    let ws = &viewer.ws;
    spaces::ensure_channel_settings(tx, ws, conn, mode, now)?;
    if let Some(p) = persona {
        bindings::set_account_default(tx, viewer, conn, p, now)?;
    }
    steve_store::audit_repo::record(
        tx,
        ws,
        &viewer.user,
        "channel.create",
        conn,
        &json!({"mode": mode.code(), "persona_id": persona}),
    )?;
    tx.emit(steve_types::DomainEvent::from(
        steve_types::ChannelsEvent::Channel {
            id: conn.into(),
            ws: ws.clone().into(),
        },
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_words() {
        for ok in ["/bot", "/a", "/Steve_2", "/abcdefghijklmnopqrst"] {
            assert!(trigger_word_ok(ok), "{ok}");
        }
        for bad in [
            "bot",
            "/",
            "/有",
            "/a b",
            "/abcdefghijklmnopqrstu",
            "//x",
            "",
        ] {
            assert!(!trigger_word_ok(bad), "{bad}");
        }
    }

    #[test]
    fn automation_prefix_validation_rejects_empty_multiline_and_unbounded_values() {
        for prefixes in [
            vec![" ".to_string()],
            vec!["a\nb".to_string()],
            vec!["x".repeat(101)],
            vec!["x".to_string(); 21],
        ] {
            let patch: ChannelPatch =
                serde_json::from_value(json!({"trigger":{"ignore_prefixes":prefixes}})).unwrap();
            assert!(patch.check().is_err());
        }
        let patch: ChannelPatch =
            serde_json::from_value(json!({"trigger":{"ignore_prefixes":["AUTO:","机器人："]}}))
                .unwrap();
        patch.check().unwrap();
    }

    #[test]
    fn triggers_get_every_key() {
        assert_eq!(normal_trigger(&Value::Null), default_trigger());
        assert_eq!(
            normal_trigger(&json!({"on": true, "words": "x"})),
            json!({"on": true, "words": ["/ai", "/bot"], "where": ["*"], "who": ["me"], "reply": "thread", "mark": true, "ignore_prefixes": []})
        );
        // prod: switched on without choosing where
        assert_eq!(
            normal_trigger(&json!({"on": true, "words": ["/bot"], "where": [], "mark": false}))["where"],
            json!(["*"])
        );
    }
}
