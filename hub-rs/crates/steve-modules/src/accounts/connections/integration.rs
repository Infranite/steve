//! 接入 (integrations): every provider Steve can connect, the connections under each, and what each
//! connection can do — per capability area (渠道, 文档/知识库, 日历, 任务, 邮件, 代码仓库, 通讯录…)
//! one of 已开启 / 缺权限 / 已关闭 / 规划中.
//!
//! The areas are a coarser, user-facing grouping of the capability codes of [`super::CAPS`]: an
//! adapter describes itself with [`super::ConnectionProvider::integration`] (default: derived from
//! its caps), including the areas it only plans to support and, per area, the provider-side scopes it
//! needs. A connection's state comes from its enabled caps, the caps its granted scopes allow
//! (`available_caps`), and the permission check `verify` stored in `connections.config.permissions`
//! (`[{scope, status}]`, status `missing` = 未开通). Nothing here talks to a provider.

use serde::Serialize;
use serde_json::Value;

use super::provider::{AuthKind, ConnectionProvider, FieldDef, Pairing};
use super::repo::ConnRow;
use super::service::{Connections, HealthOut};
use crate::Caller;
use crate::accounts::repo::Account;
use crate::vault::VaultResult;

/// The capability areas, in display order: `(id, label)`.
pub const AREAS: [(&str, &str); 11] = [
    ("channel", "渠道"),
    ("send_as_you", "以你的名义发送"),
    ("docs", "文档/知识库"),
    ("sheets", "表格/多维表格"),
    ("calendar", "日历"),
    ("tasks", "任务"),
    ("approval", "审批"),
    ("minutes", "妙记"),
    ("mail", "邮件"),
    ("repo", "代码仓库"),
    ("directory", "通讯录"),
];

/// The area a capability code belongs to (`events` has none: it feeds the others).
pub fn area_of(cap: &str) -> Option<&'static str> {
    if cap == "msg.send_as_user" {
        return Some("send_as_you");
    }
    match cap.split('.').next().unwrap_or("") {
        "msg" => Some("channel"),
        "mail" => Some("mail"),
        "cal" => Some("calendar"),
        "directory" => Some("directory"),
        "repo" => Some("repo"),
        _ => None,
    }
}

/// One provider-side permission an area needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeSpec {
    pub scope: String,
    /// What Steve uses it for (Chinese).
    pub purpose: String,
    /// Without it the area doesn't work (`false`: 建议 / 可选).
    pub required: bool,
}

impl ScopeSpec {
    pub fn new(scope: impl Into<String>, purpose: impl Into<String>, required: bool) -> Self {
        Self {
            scope: scope.into(),
            purpose: purpose.into(),
            required,
        }
    }
}

/// One area an adapter supports today or plans to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapSpec {
    /// An id of [`AREAS`].
    pub area: String,
    /// The connection caps that make it live (any one is enough); empty when planned.
    pub caps: Vec<String>,
    /// What to enable at the provider for it, for the setup checklist.
    pub scopes: Vec<ScopeSpec>,
    /// Not built yet: shown as 规划中, never as available.
    pub planned: bool,
    /// What it covers, shown under the area (Chinese).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CapSpec {
    /// A supported area live through `caps`.
    pub fn supported(area: &str, caps: &[&str]) -> Self {
        Self {
            area: area.into(),
            caps: caps.iter().map(|c| c.to_string()).collect(),
            scopes: Vec::new(),
            planned: false,
            note: None,
        }
    }
    /// A planned area.
    pub fn planned(area: &str, note: &str) -> Self {
        Self {
            area: area.into(),
            caps: Vec::new(),
            scopes: Vec::new(),
            planned: true,
            note: (!note.is_empty()).then(|| note.to_string()),
        }
    }
    pub fn with_scopes(mut self, scopes: Vec<ScopeSpec>) -> Self {
        self.scopes = scopes;
        self
    }
    pub fn with_note(mut self, note: &str) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// How an adapter presents itself on the 接入 page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Integration {
    /// The provider it belongs to (`feishu`, `github`, `mail`): adapters of one provider are listed
    /// together.
    pub provider: String,
    pub provider_name: String,
    /// Whose identity a connection is: `bot` (an app's bot), `self` (you), `app` (an installed app),
    /// `account` (a mailbox or other account).
    pub identity: String,
    pub capabilities: Vec<CapSpec>,
}

impl Integration {
    /// The default: provider = system, one supported area per cap family.
    pub fn from_caps(system: &str, name: &str, caps: &[&'static str]) -> Self {
        let mut out: Vec<CapSpec> = Vec::new();
        for c in caps {
            let Some(area) = area_of(c) else { continue };
            match out.iter_mut().find(|s| s.area == area) {
                Some(s) => s.caps.push(c.to_string()),
                None => out.push(CapSpec::supported(area, &[c])),
            }
        }
        Self {
            provider: system.into(),
            provider_name: name.into(),
            identity: "account".into(),
            capabilities: out,
        }
    }
}

/// Display order of the known providers; others follow in registration order.
const ORDER: [&str; 3] = ["feishu", "github", "mail"];

// ---------------------------------------------------------------- shapes

/// `GET /connections/integrations`.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationsOut {
    pub areas: Vec<AreaOut>,
    pub providers: Vec<ProviderOut>,
    /// The same connections as installation → identity → capability.
    pub installations: Vec<InstallationOut>,
    /// `<origin>/auth/connect/callback` for every public origin: the redirect URI to register.
    pub callback_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AreaOut {
    pub id: &'static str,
    pub label: String,
}

/// An area's label in the request's language ([`AREAS`] holds the zh-CN one).
fn area_label(id: &str) -> String {
    use steve_i18n::t;
    match id {
        "channel" => t!("modules.integration.area.channel"),
        "send_as_you" => t!("modules.integration.area.send_as_you"),
        "docs" => t!("modules.integration.area.docs"),
        "sheets" => t!("modules.integration.area.sheets"),
        "calendar" => t!("modules.integration.area.calendar"),
        "tasks" => t!("modules.integration.area.tasks"),
        "approval" => t!("modules.integration.area.approval"),
        "minutes" => t!("modules.integration.area.minutes"),
        "mail" => t!("modules.integration.area.mail"),
        "repo" => t!("modules.integration.area.repo"),
        "directory" => t!("modules.integration.area.directory"),
        x => x.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderOut {
    pub id: String,
    pub name: String,
    /// `supported`: this Hub has an adapter (only those are listed).
    pub status: &'static str,
    pub adapters: Vec<AdapterInfo>,
    /// Every area of the provider: `supported` when an adapter offers it today, else `planned`.
    pub areas: Vec<ProviderArea>,
    pub connections: Vec<ConnectionInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderArea {
    pub area: String,
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterInfo {
    pub id: String,
    pub name: String,
    pub identity: String,
    pub auth: AuthKind,
    /// The device flow is offered.
    pub device: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldDef>>,
    pub capabilities: Vec<CapSpec>,
    /// The connections it pairs with ([`ConnectionProvider::pairing`]); `null` when it does not.
    pub pairing: Option<Pairing>,
    /// What its channel side declares (mode, features, `receive`, `start`…); `null` for an adapter
    /// that is not a channel side.
    pub channel: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub account_id: String,
    pub adapter: String,
    pub adapter_name: String,
    pub identity: String,
    pub name: String,
    pub handle: String,
    /// `connections.status`: `ok | reauth | error | paused | revoked`.
    pub status: String,
    pub health: HealthOut,
    pub last_sync: Option<i64>,
    pub capabilities: Vec<CapState>,
    /// A warning `verify` left (Feishu: no refresh token).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Provider-specific facts for the page ([`ConnectionProvider::integration_detail`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    /// The Hub has its adapter. One that is not installed shows 适配器未安装: it can be
    /// disconnected, not tested or re-authorized; its tokens are not refreshed meanwhile.
    pub installed: bool,
}

/// One installation of a provider: a Feishu self-built app, a GitHub App, a mailbox. Steve works
/// through it as itself (`steve`) and/or on your behalf (`you`).
#[derive(Debug, Clone, Serialize)]
pub struct InstallationOut {
    /// The connection that holds it (Feishu: the app bot's).
    pub id: String,
    pub provider: String,
    pub provider_name: String,
    /// The app's or account's name.
    pub name: String,
    /// The worst connection status among its identities (`ok` when all are).
    pub status: String,
    /// Permissions missing over all identities (distinct scopes): what the areas need plus what the
    /// provider reports 未开通 (`detail.missing`, any level).
    pub missing: usize,
    /// Steve itself first, then you.
    pub identities: Vec<IdentityOut>,
}

/// One identity of an installation: its connection with the state of every capability.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityOut {
    /// `steve` (an app's bot, an installed app) or `you` (your account, your grant).
    pub kind: &'static str,
    pub connection: ConnectionInfo,
}

/// `steve` for the bot and installed-app identities, `you` for the rest.
pub fn identity_kind(identity: &str) -> &'static str {
    match identity {
        "bot" | "app" => "steve",
        _ => "you",
    }
}

const STATUS_RANK: [&str; 5] = ["ok", "paused", "error", "reauth", "revoked"];

/// The installation a connection belongs to (one app of a provider, with up
/// to two identities — Steve itself and you): the id of the connection that holds it. A connection
/// that names its app's connection (`config.app_connection`: your Feishu user connection points at
/// the app's) is an identity of that installation; every other connection is one of its own.
///
/// The one rule: the 接入 page groups by it, and channel spaces are one per (installation, chat)
/// (`steve_channels::spaces::installation_of`; migration 019 applied it to the rows then).
pub fn installation_key(connection_id: &str, config: &Value) -> String {
    config
        .get("app_connection")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(connection_id)
        .to_string()
}

/// Groups connections (with their installation key) into installations.
pub fn installations(conns: Vec<(String, String, String, ConnectionInfo)>) -> Vec<InstallationOut> {
    // (installation key, provider, provider name, connection)
    let mut out: Vec<InstallationOut> = Vec::new();
    for (key, provider, provider_name, c) in conns {
        let k = match out.iter().position(|x| x.id == key) {
            Some(k) => k,
            None => {
                out.push(InstallationOut {
                    id: key,
                    provider,
                    provider_name,
                    name: String::new(),
                    status: "ok".into(),
                    missing: 0,
                    identities: Vec::new(),
                });
                out.len() - 1
            }
        };
        out[k].identities.push(IdentityOut {
            kind: identity_kind(&c.identity),
            connection: c,
        });
    }
    for x in &mut out {
        x.identities.sort_by_key(|i| i.kind != "steve");
        let holder = x
            .identities
            .iter()
            .find(|i| i.connection.id == x.id)
            .or(x.identities.first());
        x.name = holder
            .map(|i| i.connection.name.clone())
            .unwrap_or_default();
        let rank = |s: &str| STATUS_RANK.iter().position(|r| *r == s).unwrap_or(2);
        x.status = x
            .identities
            .iter()
            .map(|i| i.connection.status.as_str())
            .max_by_key(|s| rank(s))
            .unwrap_or("ok")
            .to_string();
        let mut missing: Vec<String> = Vec::new();
        for i in &x.identities {
            let c = &i.connection;
            let caps = c
                .capabilities
                .iter()
                .flat_map(|s| s.missing_scopes.iter().cloned());
            let found = c
                .detail
                .as_ref()
                .and_then(|d| d.get("missing"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| r.get("scope").and_then(Value::as_str).map(String::from));
            for s in caps.chain(found) {
                let s = format!("{}:{s}", i.kind);
                if !missing.contains(&s) {
                    missing.push(s);
                }
            }
        }
        x.missing = missing.len();
    }
    out
}

/// One area of one connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapState {
    pub area: String,
    /// `on` 已开启 · `missing` 缺权限 · `off` 已关闭（你关掉的）· `planned` 规划中.
    pub state: &'static str,
    /// Required scopes not granted (`missing`).
    pub missing_scopes: Vec<String>,
    /// Suggested or optional scopes not granted (the area works without them).
    pub suggested_scopes: Vec<String>,
    /// The caps of the area that are live.
    pub caps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---------------------------------------------------------------- the state of a connection

/// `config.permissions` rows by status: (missing, granted). `None` when `verify` stored no check.
fn checks(config: &Value) -> Option<(Vec<String>, Vec<String>)> {
    let rows = config.get("permissions")?.as_array()?;
    let mut missing = Vec::new();
    let mut granted = Vec::new();
    for r in rows {
        let Some(scope) = r.get("scope").and_then(Value::as_str) else {
            continue;
        };
        match r.get("status").and_then(Value::as_str) {
            Some("missing") => missing.push(scope.to_string()),
            Some("granted") => granted.push(scope.to_string()),
            _ => {}
        }
    }
    Some((missing, granted))
}

/// The state of one area for a connection with `enabled` caps, `available` caps (what its grant
/// allows) and the stored permission check in `config`.
pub fn cap_state(
    spec: &CapSpec,
    enabled: &[String],
    available: &[String],
    config: &Value,
) -> CapState {
    let mut out = CapState {
        area: spec.area.clone(),
        state: "planned",
        missing_scopes: Vec::new(),
        suggested_scopes: Vec::new(),
        caps: Vec::new(),
        note: spec.note.clone(),
    };
    if spec.planned {
        return out;
    }
    let check = checks(config);
    if let Some((missing, _)) = &check {
        for s in &spec.scopes {
            if missing.contains(&s.scope) {
                if s.required {
                    out.missing_scopes.push(s.scope.clone());
                } else {
                    out.suggested_scopes.push(s.scope.clone());
                }
            }
        }
    }
    let allowed: Vec<&String> = spec.caps.iter().filter(|c| available.contains(c)).collect();
    out.caps = allowed
        .iter()
        .filter(|c| enabled.contains(c))
        .map(|c| c.to_string())
        .collect();
    out.state = if !out.missing_scopes.is_empty() {
        "missing"
    } else if allowed.is_empty() {
        // the grant doesn't allow any of it: name what wasn't granted (all required ones when the
        // provider didn't tell what it granted)
        let granted = check.map(|(_, g)| g).unwrap_or_default();
        out.missing_scopes = spec
            .scopes
            .iter()
            .filter(|s| s.required && !granted.contains(&s.scope))
            .map(|s| s.scope.clone())
            .collect();
        "missing"
    } else if out.caps.is_empty() {
        "off"
    } else {
        "on"
    };
    out
}

impl Connections {
    /// `GET /connections/integrations`: the providers with their adapters, and the caller's
    /// connections with the state of every area.
    pub fn integrations(&self, who: &Caller) -> VaultResult<IntegrationsOut> {
        let rows = self.visible(who)?;
        let mut providers: Vec<ProviderOut> = Vec::new();
        let slot = |providers: &mut Vec<ProviderOut>, id: &str, name: &str| -> usize {
            if let Some(i) = providers.iter().position(|p| p.id == id) {
                return i;
            }
            providers.push(ProviderOut {
                id: id.into(),
                name: name.into(),
                status: "supported",
                adapters: Vec::new(),
                areas: Vec::new(),
                connections: Vec::new(),
            });
            providers.len() - 1
        };
        for p in self.providers() {
            let i = p.integration();
            let k = slot(&mut providers, &i.provider, &i.provider_name);
            providers[k].adapters.push(AdapterInfo {
                id: p.id().into(),
                name: p.name().into(),
                identity: i.identity.clone(),
                auth: p.auth(),
                device: p.oauth().is_some_and(|s| s.device_url.is_some()),
                fields: matches!(p.auth(), AuthKind::Credential).then(|| p.fields().to_vec()),
                capabilities: i.capabilities,
                pairing: p.pairing(),
                channel: self.channel_summary(p.id()).cloned(),
            });
        }
        let mut grouped = Vec::new();
        for (r, a) in &rows {
            let Some(p) = self.provider(&r.adapter) else {
                // its adapter is not installed: listed under the provider of its system when
                // there is one, so it can be seen and disconnected
                let info = uninstalled(r, a);
                let k = slot(&mut providers, &a.system_id, &a.system_id);
                grouped.push((
                    r.id.clone(),
                    providers[k].id.clone(),
                    providers[k].name.clone(),
                    info.clone(),
                ));
                providers[k].connections.push(info);
                continue;
            };
            let i = p.integration();
            let k = slot(&mut providers, &i.provider, &i.provider_name);
            let info = self.info(p.as_ref(), &i, r, a);
            let key = installation_key(&r.id, &r.config);
            // an installation whose holder isn't visible (or is gone) stands on its own
            let key = if rows.iter().any(|(x, _)| x.id == key) {
                key
            } else {
                r.id.clone()
            };
            grouped.push((
                key,
                i.provider.clone(),
                i.provider_name.clone(),
                info.clone(),
            ));
            providers[k].connections.push(info);
        }
        for p in &mut providers {
            let mut areas: Vec<ProviderArea> = Vec::new();
            let mut add =
                |area: &str, supported: bool| match areas.iter_mut().find(|a| a.area == area) {
                    Some(a) if supported => a.state = "supported",
                    Some(_) => {}
                    None => areas.push(ProviderArea {
                        area: area.into(),
                        state: if supported { "supported" } else { "planned" },
                    }),
                };
            for a in &p.adapters {
                for c in &a.capabilities {
                    add(&c.area, !c.planned);
                }
            }
            let rank = |id: &str| {
                AREAS
                    .iter()
                    .position(|(a, _)| *a == id)
                    .unwrap_or(AREAS.len())
            };
            areas.sort_by_key(|a| rank(&a.area));
            p.areas = areas;
        }
        let rank = |id: &str| ORDER.iter().position(|o| *o == id).unwrap_or(ORDER.len());
        providers.sort_by_key(|p| rank(&p.id));
        let mut installations = installations(grouped);
        installations.sort_by_key(|x| rank(&x.provider));
        Ok(IntegrationsOut {
            installations,
            areas: AREAS
                .iter()
                .map(|(id, _)| AreaOut {
                    id,
                    label: area_label(id),
                })
                .collect(),
            providers,
            callback_urls: self.callback_urls(),
        })
    }

    fn info(
        &self,
        p: &dyn ConnectionProvider,
        i: &Integration,
        r: &ConnRow,
        a: &Account,
    ) -> ConnectionInfo {
        let available = self.available_of(r);
        let capabilities: Vec<CapState> = i
            .capabilities
            .iter()
            .map(|s| cap_state(s, &r.caps, &available, &r.config))
            .collect();
        let detail = p.integration_detail(&r.config, &capabilities);
        ConnectionInfo {
            id: r.id.clone(),
            account_id: r.account_id.clone(),
            adapter: r.adapter.clone(),
            adapter_name: p.name().into(),
            identity: i.identity.clone(),
            name: if a.display_name.is_empty() {
                a.handle_display.clone()
            } else {
                a.display_name.clone()
            },
            handle: if a.handle_display.is_empty() {
                a.handle.clone()
            } else {
                a.handle_display.clone()
            },
            status: r.status.clone(),
            health: HealthOut {
                last_ok: r.health.last_ok,
                last_error: r.health.last_error.clone(),
                error_at: r.health.error_at,
            },
            last_sync: r.last_sync,
            capabilities,
            warning: r
                .config
                .get("warning")
                .and_then(Value::as_str)
                .map(String::from),
            detail,
            installed: true,
        }
    }
}

/// A connection whose adapter is not installed, as the page lists it: its name, handle, status
/// and health; no areas, no provider facts.
fn uninstalled(r: &ConnRow, a: &Account) -> ConnectionInfo {
    ConnectionInfo {
        id: r.id.clone(),
        account_id: r.account_id.clone(),
        adapter: r.adapter.clone(),
        adapter_name: r.adapter.clone(),
        identity: String::new(),
        name: if a.display_name.is_empty() {
            a.handle_display.clone()
        } else {
            a.display_name.clone()
        },
        handle: if a.handle_display.is_empty() {
            a.handle.clone()
        } else {
            a.handle_display.clone()
        },
        status: r.status.clone(),
        health: HealthOut {
            last_ok: r.health.last_ok,
            last_error: r.health.last_error.clone(),
            error_at: r.health.error_at,
        },
        last_sync: r.last_sync,
        capabilities: Vec::new(),
        warning: None,
        detail: None,
        installed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    fn channel() -> CapSpec {
        CapSpec::supported("channel", &["msg.receive", "msg.send"]).with_scopes(vec![
            ScopeSpec::new("im:message", "发消息", true),
            ScopeSpec::new("im:message.group_msg", "收群里所有消息", false),
        ])
    }

    #[test]
    fn derives_areas_from_caps() {
        let i = Integration::from_caps(
            "mail",
            "邮箱",
            &["mail.read", "mail.send", "events", "cal.read"],
        );
        let areas: Vec<_> = i.capabilities.iter().map(|c| c.area.as_str()).collect();
        assert_eq!(areas, ["mail", "calendar"]);
        assert_eq!(i.capabilities[0].caps, s(&["mail.read", "mail.send"]));
        assert!(i.capabilities.iter().all(|c| !c.planned));
    }

    #[test]
    fn on_off_and_missing() {
        let all = s(&["msg.receive", "msg.send"]);
        let ok = json!({"permissions": [{"scope": "im:message", "status": "granted"}]});
        let st = cap_state(&channel(), &all, &all, &ok);
        assert_eq!((st.state, st.caps.len()), ("on", 2));

        // a required scope reported 未开通
        let miss = json!({"permissions": [
            {"scope": "im:message", "status": "missing"},
            {"scope": "im:message.group_msg", "status": "missing"},
        ]});
        let st = cap_state(&channel(), &all, &all, &miss);
        assert_eq!(st.state, "missing");
        assert_eq!(st.missing_scopes, s(&["im:message"]));
        assert_eq!(st.suggested_scopes, s(&["im:message.group_msg"]));

        // only the suggested one missing: still on, with the hint
        let sug = json!({"permissions": [{"scope": "im:message.group_msg", "status": "missing"}]});
        let st = cap_state(&channel(), &all, &all, &sug);
        assert_eq!(st.state, "on");
        assert_eq!(st.suggested_scopes, s(&["im:message.group_msg"]));

        // switched off by you
        assert_eq!(cap_state(&channel(), &[], &all, &ok).state, "off");
    }

    #[test]
    fn grant_without_the_caps_is_missing() {
        let st = cap_state(&channel(), &[], &[], &Value::Null);
        assert_eq!(st.state, "missing");
        assert_eq!(st.missing_scopes, s(&["im:message"]));
        let granted = json!({"permissions": [{"scope": "im:message", "status": "granted"}]});
        assert!(
            cap_state(&channel(), &[], &[], &granted)
                .missing_scopes
                .is_empty()
        );
    }

    #[test]
    fn planned_never_live() {
        let spec = CapSpec::planned("calendar", "读日程");
        let all = s(&["cal.read"]);
        let st = cap_state(&spec, &all, &all, &Value::Null);
        assert_eq!(st.state, "planned");
        assert!(st.caps.is_empty());
        assert_eq!(st.note.as_deref(), Some("读日程"));
    }
}
