//! `http`: `{method, url, headers{}, body, effect}`.
//!
//! - `effect=false` allows only `GET`; `effect=true` is effectful: the engine asks you each time
//!   unless the flow holds `grants(capability='http.effect', resource_type='host', resource_id=<host>)`.
//! - Internal addresses are refused: loopback, private, link-local (cloud metadata 169.254.169.254),
//!   CGNAT/Tailscale, unspecified, multicast, reserved, and their IPv6 forms (including v4-mapped,
//!   NAT64 and 6to4). Checked for IP literals before sending and on every redirect, and for names
//!   after DNS resolution inside the connector (so the address checked is the one connected to; no
//!   rebinding window). Proxies from the environment are not used. Hosts listed in the setting
//!   `http_allow_private` are exempt; only you set it (`PUT /flow-settings/allow-private`), a flow
//!   Steve drafts never does.
//! - `{{secrets.<item>.<field>}}` (headers and body only) is expanded at send time from the vault
//!   ([`Vault::use_for_flow`]): a `secret.use` grant of the flow (`subject_type='flow'`) or your
//!   approval of this step, else the run waits on an approval. Only references the flow's author
//!   wrote are expanded: they are swapped for random placeholders before the other templates are
//!   rendered, so event text that contains `{{secrets.…}}` stays text. The run's context and step
//!   input keep the template; the response and errors become the step output as they are. With
//!   secrets, a redirect to another host is not followed.
//! - The response keeps 64 KiB. Output: `{status, content_type, body}` plus, when the body is a JSON
//!   object, its keys at the top level (`{{steps.h.claimId}}`). Status ≥ 400 fails the step.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Url, redirect};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use steve_modules::vault::Vault;
use steve_modules::vault::flow_use::{FlowSecretOutcome, FlowSecretRequest};
use steve_store::NewDecision;
use zeroize::Zeroizing;

use super::{
    Effects, Grant, NodeKind, NodeOutcome, Rule, RunCx, WaitSpec, one_of, opt_str, req_str,
};

/// The setting with the hosts exempt from the internal-address check.
pub const ALLOW_PRIVATE: &str = "http_allow_private";
/// Bytes of response body kept.
pub const BODY_CAP: usize = 64 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
pub const METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];
const URL_MAX: usize = 2000;

pub const GET_ONLY: &str = "没有副作用的请求只能用 GET；要 POST 等请把「有副作用」打开";
pub const SECRETS_PLACE: &str = "机密只能用在 headers 和 body 里";
pub const SECRET_FORM: &str = "机密要写成 {{secrets.<条目 id>.<字段>}}";
pub const DRY_SECRET: &str = "用到了机密，试运行时不发送";
pub const NO_VAULT: &str = "保险箱还没有接上，不能使用 {{secrets}}";

/// The refusal of an internal address.
pub fn refused(host: &str, ip: IpAddr) -> String {
    format!(
        "不能请求内网地址：{host}（{ip}）。如果确实需要，在「自动化 · 流程」页底部的「允许访问的内网主机」里加上它"
    )
}

// ------------------------------------------------------------------ address policy

/// Addresses a flow may not reach (unless the host is allow-listed).
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => internal_v4(v),
        IpAddr::V6(v) => internal_v6(v),
    }
}

fn internal_v4(v: Ipv4Addr) -> bool {
    let [a, b, c, _] = v.octets();
    v.is_loopback()                                 // 127/8
        || v.is_private()                           // 10/8, 172.16/12, 192.168/16
        || v.is_link_local()                        // 169.254/16, cloud metadata
        || v.is_unspecified() || a == 0             // 0/8
        || v.is_broadcast()
        || v.is_multicast()                         // 224/4
        || a >= 240                                 // 240/4 reserved
        || (a == 100 && (64..128).contains(&b))     // 100.64/10 CGNAT, Tailscale
        || (a == 192 && b == 0 && c == 0)           // 192.0.0/24 IETF
        || (a == 198 && (b == 18 || b == 19)) // 198.18/15 benchmarking
}

fn internal_v6(v: Ipv6Addr) -> bool {
    let s = v.segments();
    if let Some(v4) = v.to_ipv4_mapped() {
        return internal_v4(v4);
    }
    v.is_loopback()
        || v.is_unspecified()
        || (s[0] & 0xfe00) == 0xfc00                // fc00::/7 unique local (fd00:ec2::254 metadata)
        || (s[0] & 0xffc0) == 0xfe80                // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0                // fec0::/10 site-local
        || (s[0] & 0xff00) == 0xff00                // multicast
        || (s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0) // ::a.b.c.d compat
        || (s[0] == 0x64 && s[1] == 0xff9b && internal_v4(embedded_v4(s[6], s[7]))) // NAT64
        || (s[0] == 0x2002 && internal_v4(embedded_v4(s[1], s[2]))) // 6to4
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
}

fn embedded_v4(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
}

/// Lower-case host without a trailing dot (the allow-list form).
fn norm_host(h: &str) -> String {
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// The URL's IP literal, when it is one and not allowed. `None` = fine (or a name: the resolver
/// checks it).
fn literal_refusal(u: &Url, allow: &HashSet<String>) -> Option<String> {
    let host = u.host_str()?;
    if allow.contains(&norm_host(host)) {
        return None;
    }
    let ip = match u.host()? {
        url::Host::Ipv4(v) => IpAddr::V4(v),
        url::Host::Ipv6(v) => IpAddr::V6(v),
        url::Host::Domain(_) => return None,
    };
    is_internal(ip).then(|| refused(host, ip))
}

/// DNS, replaceable in tests.
#[async_trait]
pub trait Lookup: Send + Sync {
    async fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>>;
}

/// The system resolver (`getaddrinfo` via tokio).
pub struct SystemLookup;

#[async_trait]
impl Lookup for SystemLookup {
    async fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        Ok(tokio::net::lookup_host((host, 0))
            .await?
            .map(|a| a.ip())
            .collect())
    }
}

/// The connector's resolver: refuses a name if any of its addresses is internal.
struct Guard {
    lookup: Arc<dyn Lookup>,
    allow: Arc<HashSet<String>>,
    /// The refusal, for the step error (reqwest wraps resolver errors).
    refused: Arc<Mutex<Option<String>>>,
}

impl reqwest::dns::Resolve for Guard {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = self.lookup.clone();
        let allow = self.allow.clone();
        let refused_slot = self.refused.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let ips = lookup.lookup(&host).await?;
            if let Some(ip) = ips
                .iter()
                .copied()
                .find(|ip| is_internal(*ip))
                .filter(|_| !allow.contains(&norm_host(&host)))
            {
                let msg = refused(&host, ip);
                *refused_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                return Err(msg.into());
            }
            if ips.is_empty() {
                return Err(format!("找不到主机 {host}").into());
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// The workspace's `http_allow_private`: a JSON list, or text separated by commas, spaces or lines.
pub fn allow_private(c: &rusqlite::Connection, ws: &str) -> HashSet<String> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
            params![ws, ALLOW_PRIVATE],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(v) = v else {
        return HashSet::new();
    };
    let items: Vec<String> = match serde_json::from_str::<Value>(&v) {
        Ok(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Ok(Value::String(s)) => vec![s],
        _ => vec![v],
    };
    items
        .iter()
        .flat_map(|s| s.split(|c: char| c == ',' || c == '，' || c.is_whitespace()))
        .map(norm_host)
        .filter(|s| !s.is_empty())
        .collect()
}

// ------------------------------------------------------------------ secrets in templates

/// One `{{secrets.<item>.<field>}}` the author wrote, and its placeholder.
#[derive(Debug, Clone)]
struct SecretRef {
    item: String,
    field: String,
    placeholder: String,
}

/// `(item, field)` of a `{{ … }}` body that is a secret reference; `Err` for a malformed one.
fn parse_ref(inner: &str) -> Option<Result<(String, String), String>> {
    let p = inner.trim().strip_prefix("secrets.")?;
    Some(match p.split_once('.') {
        Some((i, f)) if !i.trim().is_empty() && !f.trim().is_empty() => {
            Ok((i.trim().to_string(), f.trim().to_string()))
        }
        _ => Err(steve_i18n::t!("flow.node.secret_form")),
    })
}

/// Every secret reference in a string.
fn refs_in(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else { break };
        if let Some(r) = parse_ref(&after[..j]) {
            out.push(r?);
        }
        rest = &after[j + 2..];
    }
    Ok(out)
}

fn refs_in_value(v: &Value, out: &mut Vec<(String, String)>) -> Result<(), String> {
    match v {
        Value::String(s) => out.extend(refs_in(s)?),
        Value::Array(a) => {
            for x in a {
                refs_in_value(x, out)?;
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                if !refs_in(k)?.is_empty() {
                    return Err(steve_i18n::t!("flow.node.secrets_place"));
                }
                refs_in_value(x, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The secret references of an `http` config, headers then body (the order they are sent in).
/// Malformed ones are left out (validation refuses them).
fn secret_refs(cfg: &Value) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    for (_, v) in cfg["headers"].as_object().into_iter().flatten() {
        refs.extend(refs_in(v.as_str().unwrap_or("")).unwrap_or_default());
    }
    let _ = refs_in_value(&cfg["body"], &mut refs);
    refs
}

fn has_secret_ref(s: &str) -> bool {
    refs_in(s).map(|r| !r.is_empty()).unwrap_or(true)
}

/// Swaps the author's secret references for placeholders (`nonce` makes them unguessable).
struct Swapper {
    nonce: String,
    refs: Vec<SecretRef>,
}

impl Swapper {
    fn new() -> Self {
        let n: String = format!("{}{}", steve_store::uid("x"), steve_store::uid("y"))
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        Self {
            nonce: n,
            refs: Vec::new(),
        }
    }

    fn swap(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("{{") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let Some(j) = after.find("}}") else {
                out.push_str(&rest[i..]);
                return out;
            };
            match parse_ref(&after[..j]) {
                Some(Ok((item, field))) => {
                    let placeholder = format!("SteveSecret{}N{}Z", self.nonce, self.refs.len());
                    out.push_str(&placeholder);
                    self.refs.push(SecretRef {
                        item,
                        field,
                        placeholder,
                    });
                }
                _ => out.push_str(&rest[i..i + 2 + j + 2]),
            }
            rest = &after[j + 2..];
        }
        out.push_str(rest);
        out
    }

    fn swap_value(&mut self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.swap(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.swap_value(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), self.swap_value(x)))
                    .collect(),
            ),
            x => x.clone(),
        }
    }
}

/// Puts the values in. `json`: inside JSON text, so values are written as JSON string content.
fn fill(
    s: &str,
    refs: &[SecretRef],
    values: &[(String, Zeroizing<String>)],
    json: bool,
) -> Zeroizing<String> {
    let mut out = Zeroizing::new(s.to_string());
    for r in refs {
        let key = format!("{}.{}", r.item, r.field);
        let Some((_, v)) = values.iter().find(|(k, _)| *k == key) else {
            continue;
        };
        let v: Zeroizing<String> = if json {
            let q = Zeroizing::new(serde_json::to_string(v.as_str()).unwrap_or_default());
            Zeroizing::new(q[1..q.len().saturating_sub(1)].to_string())
        } else {
            v.clone()
        };
        *out = out.replace(&r.placeholder, &v);
    }
    out
}

// ------------------------------------------------------------------ the node

pub struct Http {
    vault: Option<Vault>,
    lookup: Arc<dyn Lookup>,
    timeout: Duration,
}

impl Default for Http {
    /// Without the vault: `{{secrets}}` fails the step.
    fn default() -> Self {
        Self {
            vault: None,
            lookup: Arc::new(SystemLookup),
            timeout: TIMEOUT,
        }
    }
}

impl Http {
    pub fn new(vault: Vault) -> Self {
        Self {
            vault: Some(vault),
            ..Self::default()
        }
    }

    /// With another resolver (tests).
    pub fn with_lookup(mut self, lookup: Arc<dyn Lookup>) -> Self {
        self.lookup = lookup;
        self
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

fn method_of(cfg: &Value) -> String {
    cfg["method"]
        .as_str()
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase()
}

fn effect_of(cfg: &Value) -> bool {
    cfg["effect"] == json!(true)
}

fn host_of(cfg: &Value) -> Option<String> {
    let u = cfg["url"].as_str()?.trim();
    if u.contains("{{") {
        return None;
    }
    Url::parse(u).ok()?.host_str().map(norm_host)
}

/// The approval when a secret is not covered by a grant (same gate as an effect: 执行 / 不执行, and
/// 此流程当前版本以后都执行 when every item is grantable: `grants` is then not empty).
fn secret_gate(
    cx: &RunCx<'_>,
    flow_name: &str,
    titles: &[String],
    draft: String,
    grants: &[Grant],
) -> WaitSpec {
    let names = titles
        .iter()
        .map(|t| format!("「{t}」"))
        .collect::<Vec<_>>()
        .join("、");
    WaitSpec {
        mode: "decision",
        until: None,
        matcher: crate::engine::gate_matcher(grants),
        decision: Some(NewDecision {
            workspace_id: cx.run.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: super::approve::SUBJECT.into(),
            subject_id: String::new(),
            item_id: cx.run.work_item_id.clone(),
            session_id: None,
            question: format!(
                "「{flow_name}」的「{}」要用保险箱里的{names}。要执行吗？",
                cx.node.name()
            ),
            draft: Some(draft),
            options: Some(crate::engine::gate_options(!grants.is_empty())),
            requested_by: "steve".into(),
        }),
        output: json!({ "waiting": "approval" }),
    }
}

/// `(flow name, created_by)`.
fn flow_meta(cx: &RunCx<'_>) -> (String, String) {
    cx.flow
        .store()
        .reader(|c| {
            Ok::<_, crate::FlowError>(
                c.query_row(
                    "SELECT name, created_by FROM flows WHERE id = ?",
                    [&cx.run.flow_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?,
            )
        })
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[async_trait]
impl NodeKind for Http {
    fn kind(&self) -> &'static str {
        "http"
    }

    fn effectful(&self, cfg: &Value) -> bool {
        effect_of(cfg)
    }

    fn grant(&self, cfg: &Value) -> Option<Grant> {
        Some(Grant::new("http.effect", "host", &host_of(cfg)?))
    }

    /// The host grant, and a `secret.use` grant for each item the step uses that no grant covers
    /// yet; `None` when the host is templated or an item is not grantable (`high`, bank card,
    /// unreadable).
    fn gate_grants(&self, cx: &RunCx<'_>, cfg: &Value) -> Option<Vec<Grant>> {
        let mut grants = vec![self.grant(cfg)?];
        let mut items: Vec<String> = Vec::new();
        for (item, _) in secret_refs(cfg) {
            if !items.contains(&item) {
                items.push(item);
            }
        }
        if items.is_empty() {
            return Some(grants);
        }
        let vault = self.vault.as_ref()?;
        let (_, owner) = flow_meta(cx);
        for item in &items {
            let c = vault
                .check_flow_use(&FlowSecretRequest {
                    ws: &cx.run.workspace_id,
                    flow_id: &cx.run.flow_id,
                    flow_version: cx.run.flow_version,
                    flow_owner: &owner,
                    run_id: &cx.run.id,
                    item_id: item,
                    labels: &[],
                    purpose: "",
                    command: "",
                    approved: false,
                })
                .ok()?;
            if !c.grantable {
                return None;
            }
            if c.grant_id.is_none() {
                grants.push(Grant::new("secret.use", "vault_item", item));
            }
        }
        Some(grants)
    }

    /// “会用到机密：<条目>·<字段>、…”: the secrets approving this step releases, in template order,
    /// once each. <条目> is the vault item's title (what you see in the vault), or the reference as
    /// written when the item can't be read.
    fn gate_note(&self, cx: &RunCx<'_>, cfg: &Value) -> Option<String> {
        let mut refs: Vec<(String, String)> = Vec::new();
        for r in secret_refs(cfg) {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
        if refs.is_empty() {
            return None;
        }
        let (_, owner) = flow_meta(cx);
        let mut titles: Vec<(String, String)> = Vec::new();
        let names = refs
            .iter()
            .map(|(item, field)| {
                let title = match titles.iter().find(|(i, _)| i == item) {
                    Some((_, t)) => t.clone(),
                    None => {
                        let t = self
                            .vault
                            .as_ref()
                            .and_then(|v| {
                                v.check_flow_use(&FlowSecretRequest {
                                    ws: &cx.run.workspace_id,
                                    flow_id: &cx.run.flow_id,
                                    flow_version: cx.run.flow_version,
                                    flow_owner: &owner,
                                    run_id: &cx.run.id,
                                    item_id: item,
                                    labels: &[],
                                    purpose: "",
                                    command: "",
                                    approved: false,
                                })
                                .ok()
                            })
                            .map(|c| c.title)
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| item.clone());
                        titles.push((item.clone(), t.clone()));
                        t
                    }
                };
                format!("{title}·{field}")
            })
            .collect::<Vec<_>>()
            .join("、");
        Some(format!("会用到机密：{names}"))
    }

    fn rules(&self) -> Vec<Rule> {
        vec![
            Rule::new(
                &["flow.node.http_method"],
                format!("method 只能是 {}（大写）", one_of(&METHODS)),
            ),
            Rule::new(
                &["flow.node.http_effect", "flow.node.get_only"],
                "effect 只能是 true 或 false；不是 GET 的请求必须 effect=true",
            ),
            Rule::new(
                &[
                    "flow.node.missing_url",
                    "flow.node.too_long_url",
                    "flow.node.http_url",
                    "flow.node.http_scheme",
                ],
                format!("url 必填，最多 {URL_MAX} 字，是 http 或 https 的完整网址"),
            ),
            Rule::new(
                &[
                    "flow.node.headers",
                    "flow.node.header_name",
                    "flow.node.header_value",
                ],
                "headers 是 {\"名字\": \"文字值\"}",
            ),
            Rule::new(
                &["flow.node.secrets_place", "flow.node.secret_form"],
                "机密只能写在 headers 和 body 里，写成 {{secrets.<条目 id>.<字段>}}",
            ),
        ]
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let m = method_of(cfg);
        if !METHODS.contains(&m.as_str()) {
            return Err(steve_i18n::t!("flow.node.http_method", method = m));
        }
        match cfg.get("effect") {
            None | Some(Value::Null) | Some(Value::Bool(_)) => {}
            Some(_) => return Err(steve_i18n::t!("flow.node.http_effect")),
        }
        if !effect_of(cfg) && m != "GET" {
            return Err(steve_i18n::t!("flow.node.get_only"));
        }
        let url = req_str(cfg, "url", super::Required::Url, URL_MAX)?;
        if has_secret_ref(url) || opt_str(cfg, "method")?.is_some_and(has_secret_ref) {
            return Err(steve_i18n::t!("flow.node.secrets_place"));
        }
        crate::expr::check_template(url)?;
        if !url.contains("{{") {
            let u = Url::parse(url).map_err(|_| steve_i18n::t!("flow.node.http_url", url = url))?;
            if !matches!(u.scheme(), "http" | "https") {
                return Err(steve_i18n::t!("flow.node.http_scheme"));
            }
        }
        match cfg.get("headers") {
            None | Some(Value::Null) => {}
            Some(Value::Object(h)) => {
                for (k, v) in h {
                    if HeaderName::from_bytes(k.as_bytes()).is_err() || has_secret_ref(k) {
                        return Err(steve_i18n::t!("flow.node.header_name", name = k));
                    }
                    let Some(v) = v.as_str() else {
                        return Err(steve_i18n::t!("flow.node.header_value", name = k));
                    };
                    refs_in(v)?;
                }
            }
            Some(_) => return Err(steve_i18n::t!("flow.node.headers")),
        }
        if let Some(b) = cfg.get("body") {
            let mut v = Vec::new();
            refs_in_value(b, &mut v)?;
        }
        Ok(())
    }

    /// A dry run sends a plain `GET`, but never releases a secret or asks for one.
    async fn dry_run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if !secret_refs(cfg).is_empty() {
            return NodeOutcome::next(super::skipped(DRY_SECRET));
        }
        self.run(cx, cfg, input).await
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if let Err(e) = self.validate(cfg) {
            return NodeOutcome::Fail(e);
        }
        let method = method_of(cfg);
        let url_text = cx.render(cfg["url"].as_str().unwrap_or("").trim(), input);
        let Ok(url) = Url::parse(&url_text) else {
            return NodeOutcome::Fail(format!("网址不对：{url_text}"));
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return NodeOutcome::Fail("网址只能是 http 或 https".into());
        }
        let ws = cx.ws().to_string();
        let allow = Arc::new(
            cx.flow
                .store()
                .reader(|c| Ok::<_, crate::FlowError>(allow_private(c, &ws)))
                .unwrap_or_default(),
        );
        if let Some(msg) = literal_refusal(&url, &allow) {
            return NodeOutcome::Fail(msg);
        }

        // templates, with the author's secret references as placeholders
        let mut sw = Swapper::new();
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(h) = cfg["headers"].as_object() {
            for (k, v) in h {
                let v = sw.swap(v.as_str().unwrap_or(""));
                headers.push((k.clone(), cx.render(&v, input)));
            }
        }
        let (body, body_json) = match cfg.get("body") {
            None | Some(Value::Null) => (None, false),
            Some(Value::String(s)) => (Some(cx.render(&sw.swap(s), input)), false),
            Some(v) => {
                let v = cx.render_value(&sw.swap_value(v), input);
                (Some(v.to_string()), true)
            }
        };

        // secrets: grant or your approval, else wait
        let mut values: Vec<(String, Zeroizing<String>)> = Vec::new();
        let mut uses: Vec<String> = Vec::new();
        if !sw.refs.is_empty() {
            let Some(vault) = &self.vault else {
                return NodeOutcome::Fail(NO_VAULT.into());
            };
            let (flow_name, owner) = flow_meta(cx);
            // your approval of this step; an `approve` node earlier on the path stands in for it
            // only for grantable items (its card names neither the secrets nor the host)
            let step_approved = cx.run.context["approved"][&cx.node.id] == json!(true);
            let path_approved = cx
                .run
                .context
                .get("approved_by")
                .is_some_and(|v| !v.is_null());
            let mut items: Vec<(String, Vec<String>)> = Vec::new();
            for r in &sw.refs {
                match items.iter_mut().find(|(i, _)| *i == r.item) {
                    Some((_, f)) if !f.contains(&r.field) => f.push(r.field.clone()),
                    Some(_) => {}
                    None => items.push((r.item.clone(), vec![r.field.clone()])),
                }
            }
            let command = format!("{method} {url}");
            let node_name = cx.node.name().to_string();
            let purpose = format!("流程「{flow_name}」·「{node_name}」");
            let (ws, flow_id, run_id) = (
                cx.run.workspace_id.clone(),
                cx.run.flow_id.clone(),
                cx.run.id.clone(),
            );
            let base = FlowSecretRequest {
                ws: &ws,
                flow_id: &flow_id,
                flow_version: cx.run.flow_version,
                flow_owner: &owner,
                run_id: &run_id,
                item_id: "",
                labels: &[],
                purpose: &purpose,
                command: &command,
                approved: step_approved,
            };
            let mut need: Vec<String> = Vec::new();
            let mut grants: Vec<Grant> = Vec::new();
            let mut all_grantable = true;
            // per item: does the path's approval release it
            let mut by_path: Vec<bool> = Vec::new();
            for (item, labels) in &items {
                let check = vault.check_flow_use(&FlowSecretRequest {
                    item_id: item,
                    labels,
                    ..base.clone()
                });
                by_path.push(matches!(&check, Ok(c) if c.grantable && path_approved));
                match check {
                    Ok(c) if c.allowed || (c.grantable && path_approved) => {}
                    Ok(c) => {
                        all_grantable &= c.grantable;
                        grants.push(Grant::new("secret.use", "vault_item", item));
                        need.push(c.title);
                    }
                    Err(e) => return NodeOutcome::Fail(format!("机密「{item}」：{e}")),
                }
            }
            if !all_grantable {
                grants.clear();
            }
            if !need.is_empty() {
                let mut draft = format!("{command}\n注入：");
                for (item, labels) in &items {
                    draft.push_str(&format!("\n{item} ← {}", labels.join("、")));
                }
                cx.effects = Effects {
                    outcome: Some("need".into()),
                    label: Some(format!("等你批准：{node_name}")),
                    ..std::mem::take(&mut cx.effects)
                };
                return NodeOutcome::Wait(Box::new(secret_gate(
                    cx, &flow_name, &need, draft, &grants,
                )));
            }
            for ((item, labels), path) in items.iter().zip(&by_path) {
                match vault.use_for_flow(&FlowSecretRequest {
                    item_id: item,
                    labels,
                    approved: step_approved || *path,
                    ..base.clone()
                }) {
                    Ok(FlowSecretOutcome::Released { use_id, values: vs }) => {
                        uses.push(use_id);
                        for (label, v) in vs {
                            values.push((
                                format!("{item}.{label}"),
                                Zeroizing::new(String::from_utf8_lossy(v.expose()).into_owned()),
                            ));
                        }
                    }
                    Ok(FlowSecretOutcome::NeedsApproval { .. }) => {
                        finish_uses(vault, &uses, false);
                        return NodeOutcome::Fail("机密的授权刚刚失效了，请重新运行".into());
                    }
                    Err(e) => {
                        finish_uses(vault, &uses, false);
                        return NodeOutcome::Fail(format!("机密「{item}」：{e}"));
                    }
                }
            }
        }
        let out = self
            .send(
                &method,
                url,
                &headers,
                body.as_deref(),
                body_json,
                &sw.refs,
                &values,
                allow,
            )
            .await;
        if let Some(vault) = &self.vault {
            finish_uses(vault, &uses, out.is_ok());
        }
        drop(values);
        match out {
            Ok((status, content_type, raw)) => {
                let text = cap(&raw);
                let truncated = raw.len() > BODY_CAP;
                let parsed = if truncated {
                    None
                } else {
                    serde_json::from_str::<Value>(&text).ok()
                };
                let mut o = Map::new();
                if let Some(Value::Object(m)) = &parsed {
                    for (k, v) in m {
                        o.insert(k.clone(), v.clone());
                    }
                }
                o.insert("status".into(), json!(status));
                o.insert("content_type".into(), json!(content_type));
                o.insert("body".into(), parsed.unwrap_or(Value::String(text)));
                if truncated {
                    o.insert("truncated".into(), json!(true));
                }
                let output = Value::Object(o);
                if status >= 400 {
                    let mut e = format!("请求失败：HTTP {status}");
                    if let Some(b) = output["body"].as_str() {
                        let b: String = b.chars().take(300).collect();
                        if !b.trim().is_empty() {
                            e.push_str(&format!("：{}", b.trim()));
                        }
                    }
                    return NodeOutcome::Fail(e);
                }
                NodeOutcome::next(output)
            }
            Err(e) => NodeOutcome::Fail(e),
        }
    }
}

fn finish_uses(vault: &Vault, uses: &[String], ok: bool) {
    for u in uses {
        if let Err(e) = vault.finish_flow_use(u, ok) {
            tracing::warn!(use_id = %u, error = %e, "flow secret use not finished");
        }
    }
}

/// The first [`BODY_CAP`] bytes as text.
fn cap(b: &[u8]) -> String {
    let b = &b[..b.len().min(BODY_CAP)];
    String::from_utf8_lossy(b).into_owned()
}

impl Http {
    /// Sends; `Ok((status, content type, body bytes, at most one chunk past the cap))`. Errors are step errors.
    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        method: &str,
        url: Url,
        headers: &[(String, String)],
        body: Option<&str>,
        body_json: bool,
        refs: &[SecretRef],
        values: &[(String, Zeroizing<String>)],
        allow: Arc<HashSet<String>>,
    ) -> Result<(u16, String, Vec<u8>), String> {
        // the test guard (`STEVE_FORBID_REAL_HOSTS=1`), on the URL and on every redirect hop
        steve_modules::net::check_url(&url).map_err(|e| e.to_string())?;
        let refused_slot: Arc<Mutex<Option<String>>> = Arc::default();
        let origin = url.host_str().map(norm_host);
        let with_secrets = !refs.is_empty();
        let policy = {
            let allow = allow.clone();
            let slot = refused_slot.clone();
            redirect::Policy::custom(move |a| {
                if a.previous().len() >= MAX_REDIRECTS {
                    return a.error("重定向次数太多");
                }
                if !matches!(a.url().scheme(), "http" | "https") {
                    return a.error("重定向到了不支持的网址");
                }
                if let Err(e) = steve_modules::net::check_url(a.url()) {
                    let msg = e.to_string();
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                    return a.error(msg);
                }
                if let Some(msg) = literal_refusal(a.url(), &allow) {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                    return a.error(msg);
                }
                // secret headers are never carried to another host
                if with_secrets && a.url().host_str().map(norm_host) != origin {
                    return a.stop();
                }
                a.follow()
            })
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(policy)
            .timeout(self.timeout)
            .connect_timeout(Duration::from_secs(10))
            .dns_resolver(Arc::new(Guard {
                lookup: self.lookup.clone(),
                allow,
                refused: refused_slot.clone(),
            }))
            .build()
            .map_err(|e| format!("请求失败：{e}"))?;
        let method =
            Method::from_bytes(method.as_bytes()).map_err(|_| "请求方法不对".to_string())?;
        let mut hm = HeaderMap::new();
        for (k, v) in headers {
            let name =
                HeaderName::from_bytes(k.as_bytes()).map_err(|_| format!("请求头名字不对：{k}"))?;
            let secret = refs.iter().any(|r| v.contains(&r.placeholder));
            let v = fill(v, refs, values, false);
            let mut hv = HeaderValue::from_str(&v)
                .map_err(|_| format!("请求头「{k}」的值不合法（不能有换行等控制字符）"))?;
            hv.set_sensitive(secret);
            hm.append(name, hv);
        }
        if body_json && !hm.contains_key(CONTENT_TYPE) {
            hm.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        let mut req = client.request(method, url).headers(hm);
        if let Some(b) = body {
            let b = fill(b, refs, values, body_json);
            req = req.body(b.as_bytes().to_vec());
        }
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if let Some(msg) = refused_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                {
                    return Err(msg);
                }
                if e.is_timeout() {
                    return Err(format!("请求超时（{} 秒）", self.timeout.as_secs()));
                }
                return Err(format!("请求失败：{}", error_chain(&e)));
            }
        };
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut buf = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    buf.extend_from_slice(&c);
                    // one byte past the cap tells a truncated body apart
                    if buf.len() > BODY_CAP {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) if e.is_timeout() => {
                    return Err(format!("请求超时（{} 秒）", self.timeout.as_secs()));
                }
                Err(e) => return Err(format!("读取响应失败：{}", error_chain(&e))),
            }
        }
        Ok((status, content_type, buf))
    }
}

/// An error and its sources, one line.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        let t = c.to_string();
        if !s.contains(&t) {
            s.push_str(": ");
            s.push_str(&t);
        }
        cur = c.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn internal_addresses() {
        for s in [
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "172.16.5.4",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.100",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00:ec2::254",
            "fc00::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
        ] {
            assert!(is_internal(ip(s)), "{s}");
        }
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "192.169.0.1",
            "2606:4700:4700::1111",
            "64:ff9b::808:808",
            "2002:808:808::",
        ] {
            assert!(!is_internal(ip(s)), "{s}");
        }
    }

    #[test]
    fn literals_in_every_spelling() {
        let none = HashSet::new();
        for u in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0177.0.0.1/",
            "http://[::1]:8080/",
            "http://[::ffff:7f00:1]/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let url = Url::parse(u).unwrap();
            assert!(literal_refusal(&url, &none).is_some(), "{u}");
        }
        assert!(literal_refusal(&Url::parse("https://example.com/").unwrap(), &none).is_none());
        let allow: HashSet<String> = ["127.0.0.1".to_string()].into();
        assert!(literal_refusal(&Url::parse("http://127.0.0.1/").unwrap(), &allow).is_none());
    }

    #[test]
    fn secret_refs() {
        assert_eq!(
            refs_in("Bearer {{secrets.vi_1.令牌}} {{ secrets.vi_2.key }} {{event.x}}").unwrap(),
            vec![
                ("vi_1".to_string(), "令牌".to_string()),
                ("vi_2".to_string(), "key".to_string())
            ]
        );
        assert_eq!(refs_in("{{secrets.vi_1}}").unwrap_err(), SECRET_FORM);
        let mut sw = Swapper::new();
        let s = sw.swap("a {{secrets.vi_1.k}} b {{event.x}}");
        assert_eq!(sw.refs.len(), 1);
        assert!(
            s.starts_with("a SteveSecret") && s.ends_with(" b {{event.x}}"),
            "{s}"
        );
        let vals = vec![("vi_1.k".to_string(), Zeroizing::new("v\"1".to_string()))];
        assert_eq!(
            fill(&s, &sw.refs, &vals, false).as_str(),
            "a v\"1 b {{event.x}}"
        );
        assert_eq!(
            fill(&s, &sw.refs, &vals, true).as_str(),
            "a v\\\"1 b {{event.x}}"
        );
    }

    #[test]
    fn validate() {
        let h = Http::default();
        assert!(h.validate(&json!({"url": "https://a.example/x"})).is_ok());
        assert_eq!(
            h.validate(&json!({"method": "POST", "url": "https://a.example/x"}))
                .unwrap_err(),
            GET_ONLY
        );
        assert!(h
            .validate(&json!({"method": "POST", "url": "https://a.example/x", "effect": true, "body": {"k": "{{secrets.vi_1.t}}"}}))
            .is_ok());
        assert_eq!(
            h.validate(&json!({"url": "https://a.example/?k={{secrets.vi_1.t}}"}))
                .unwrap_err(),
            SECRETS_PLACE
        );
        assert!(h.validate(&json!({"url": "file:///etc/passwd"})).is_err());
        assert!(
            h.validate(&json!({"url": "https://a.example/", "headers": {"bad name": "x"}}))
                .is_err()
        );
        assert!(h.effectful(&json!({"effect": true})));
        assert!(!h.effectful(&json!({})));
        assert_eq!(
            h.grant(&json!({"url": "https://Finance.Example.com/api"})),
            Some(Grant::new("http.effect", "host", "finance.example.com"))
        );
        assert_eq!(h.grant(&json!({"url": "https://{{event.host}}/api"})), None);
    }

    /// Resolves every name to loopback: even a broken guard can't leave the machine (and the
    /// internal-address check would refuse it).
    struct Loopback;

    #[async_trait]
    impl Lookup for Loopback {
        async fn lookup(&self, _: &str) -> std::io::Result<Vec<IpAddr>> {
            Ok(vec![ip("127.0.0.1")])
        }
    }

    /// A real host is refused under `STEVE_FORBID_REAL_HOSTS=1`, directly and as a redirect target
    /// of an (allowed) loopback server.
    #[tokio::test]
    async fn real_hosts_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child(
            "nodes::http::tests::real_hosts_refused_under_the_flag",
        ) {
            return;
        }
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let http = Http::default().with_lookup(Arc::new(Loopback));
        let allow = Arc::new(HashSet::from(["127.0.0.1".to_string()]));
        let url = Url::parse("https://api.github.com/user").unwrap();
        let e = http
            .send("GET", url, &[], None, false, &[], &[], allow.clone())
            .await
            .err();
        assert_eq!(
            e.as_deref(),
            Some("测试环境禁止连接外部主机：api.github.com")
        );

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut c, _)) = l.accept().await {
                let mut buf = [0u8; 2048];
                let _ = c.read(&mut buf).await;
                let _ = c
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: https://api.github.com/user\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
            }
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/go")).unwrap();
        let e = http
            .send("GET", url, &[], None, false, &[], &[], allow)
            .await
            .err();
        assert_eq!(
            e.as_deref(),
            Some("测试环境禁止连接外部主机：api.github.com")
        );
    }
}
