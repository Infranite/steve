//! The `imap` adapter: any mailbox with IMAP + SMTP and an app password.
//!
//! - TLS only: IMAP implicit TLS or STARTTLS, SMTP 465 or 587 + STARTTLS; certificates checked
//!   against the system roots (plus the guarded test root, [`MailConfig`]).
//! - Folders by SPECIAL-USE, else by common names; names decoded from modified UTF-7.
//! - Sync per folder: `EXAMINE` → UIDVALIDITY; first time `UID SEARCH SINCE` (newest 500),
//!   afterwards `UID FETCH <last+1>:*`, 50 per batch; >25 MB only the header and BODYSTRUCTURE.
//!   Flags by CONDSTORE `CHANGEDSINCE`, else the newest 200 every 10 minutes. Gmail's
//!   `X-GM-THRID` is the thread when the server has `X-GM-EXT-1`.
//! - Push: `IDLE` on the inbox, re-entered every 25 minutes.
//! - Sending: SMTP `AUTH`, then `APPEND` to the sent folder (`\Seen`) unless the host is Gmail's.
//!
//! The password is read from the vault for each connection and never logged.

use std::collections::BTreeMap;
use std::time::Duration;

use async_imap::imap_proto::{
    AttributeValue, BodyStructure, MailboxDatum, NameAttribute, Response, SectionPath, Status,
};
use async_trait::async_trait;
use futures::TryStreamExt;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, CertificateStore, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use rustls::pki_types::ServerName;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, ProviderError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use super::utf7;
use crate::adapter::{
    FetchedMessage, FlagChange, FolderInfo, MailAdapter, MailSink, PartMeta, ScopeMode, SendMeta,
    SentRef, SyncReport, SyncSession, SyncState,
};
use crate::config::MailConfig;
use crate::error::AdapterError;

type Session = async_imap::Session<TlsStream<TcpStream>>;

/// Messages bigger than this are stored as header + structure only.
pub const LARGE_BYTES: u32 = 25 * 1024 * 1024;
/// The first sync of a folder takes at most this many messages.
pub const FIRST_SYNC_MAX: usize = 500;
/// Messages per `UID FETCH`.
pub const BATCH: usize = 50;
/// Flags re-checked without CONDSTORE: the newest this many, every [`FLAG_RECHECK_MS`].
pub const FLAG_RECHECK_COUNT: u32 = 200;
pub const FLAG_RECHECK_MS: i64 = 10 * 60 * 1000;
/// IDLE is re-entered after this (RFC 2177: servers may drop it after 29 minutes).
pub const IDLE_RENEW: Duration = Duration::from_secs(25 * 60);
const NET_TIMEOUT: Duration = Duration::from_secs(30);
/// LOGOUT is a courtesy: a server that does not answer it holds nothing up for long.
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Provider presets for the new-mailbox dialog. `id` is the preset, not an adapter. The texts are
/// the zh-CN ones here; [`presets`] has them in the request's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Preset<S = &'static str> {
    pub id: &'static str,
    pub name: S,
    pub imap_host: &'static str,
    pub imap_port: u16,
    pub smtp_host: &'static str,
    pub smtp_port: u16,
    /// `tls` | `starttls`
    pub smtp_security: &'static str,
    /// How to get the app password.
    pub hint: S,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "gmail",
        name: "Gmail",
        imap_host: "imap.gmail.com",
        imap_port: 993,
        smtp_host: "smtp.gmail.com",
        smtp_port: 465,
        smtp_security: "tls",
        hint: "需要先开启两步验证，再生成应用专用密码",
    },
    Preset {
        id: "icloud",
        name: "iCloud",
        imap_host: "imap.mail.me.com",
        imap_port: 993,
        smtp_host: "smtp.mail.me.com",
        smtp_port: 587,
        smtp_security: "starttls",
        hint: "在 Apple ID 账户设置里生成 App 专用密码",
    },
    Preset {
        id: "qq",
        name: "QQ 邮箱",
        imap_host: "imap.qq.com",
        imap_port: 993,
        smtp_host: "smtp.qq.com",
        smtp_port: 465,
        smtp_security: "tls",
        hint: "在 QQ 邮箱设置里开启 IMAP 服务并生成授权码",
    },
    Preset {
        id: "163",
        name: "网易 163",
        imap_host: "imap.163.com",
        imap_port: 993,
        smtp_host: "smtp.163.com",
        smtp_port: 465,
        smtp_security: "tls",
        hint: "在 163 邮箱设置里开启 IMAP 服务并生成授权码",
    },
    Preset {
        id: "fastmail",
        name: "Fastmail",
        imap_host: "imap.fastmail.com",
        imap_port: 993,
        smtp_host: "smtp.fastmail.com",
        smtp_port: 465,
        smtp_security: "tls",
        hint: "在 Fastmail 设置的 Privacy & Security 里生成应用专用密码",
    },
];

/// [`PRESETS`] with the names and hints in the request's language (`GET /mail/presets`).
pub fn presets() -> Vec<Preset<String>> {
    PRESETS
        .iter()
        .map(|p| {
            let (name, hint) = match p.id {
                "gmail" => (p.name.to_string(), steve_i18n::t!("mail.preset.gmail_hint")),
                "icloud" => (
                    p.name.to_string(),
                    steve_i18n::t!("mail.preset.icloud_hint"),
                ),
                "qq" => (
                    steve_i18n::t!("mail.preset.qq_name"),
                    steve_i18n::t!("mail.preset.qq_hint"),
                ),
                "163" => (
                    steve_i18n::t!("mail.preset.163_name"),
                    steve_i18n::t!("mail.preset.163_hint"),
                ),
                "fastmail" => (
                    p.name.to_string(),
                    steve_i18n::t!("mail.preset.fastmail_hint"),
                ),
                _ => (p.name.to_string(), p.hint.to_string()),
            };
            Preset {
                id: p.id,
                name,
                imap_host: p.imap_host,
                imap_port: p.imap_port,
                smtp_host: p.smtp_host,
                smtp_port: p.smtp_port,
                smtp_security: p.smtp_security,
                hint,
            }
        })
        .collect()
}

/// The IMAP + SMTP adapter. Register the same `Arc` with `Connections` and [`crate::Mail`].
pub struct ImapAdapter {
    cfg: MailConfig,
    fields: Vec<FieldDef>,
}

impl ImapAdapter {
    pub fn new(cfg: MailConfig) -> Self {
        Self {
            cfg,
            fields: vec![
                FieldDef::plain("address", "邮箱地址"),
                FieldDef::plain("username", "用户名")
                    .optional()
                    .with_hint("默认同邮箱地址"),
                FieldDef::secret("password", "应用专用密码"),
                FieldDef::plain("imap_host", "IMAP 服务器"),
                FieldDef::plain("imap_port", "IMAP 端口")
                    .with_default("993")
                    .with_kind("number"),
                FieldDef::plain("imap_security", "IMAP 加密")
                    .with_default("tls")
                    .with_kind("select")
                    .with_hint("tls | starttls"),
                FieldDef::plain("smtp_host", "SMTP 服务器"),
                FieldDef::plain("smtp_port", "SMTP 端口")
                    .with_default("465")
                    .with_kind("number"),
                FieldDef::plain("smtp_security", "SMTP 加密")
                    .with_default("tls")
                    .with_kind("select")
                    .with_hint("tls | starttls"),
            ],
        }
    }
}

/// `tls` (from the first byte) or `starttls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Security {
    Tls,
    StartTls,
}

/// The non-secret settings of a connection.
#[derive(Debug, Clone)]
struct Settings {
    address: String,
    username: String,
    imap_host: String,
    imap_port: u16,
    imap_security: Security,
    smtp_host: String,
    smtp_port: u16,
    smtp_security: Security,
}

fn security(v: Option<&str>, port: u16, implicit_port: u16) -> Result<Security, AdapterError> {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("tls") | Some("ssl") => Ok(Security::Tls),
        Some("starttls") => Ok(Security::StartTls),
        None | Some("") => Ok(if port == implicit_port {
            Security::Tls
        } else {
            Security::StartTls
        }),
        Some(_) => Err(AdapterError::Rejected(
            "只支持加密连接（TLS 或 STARTTLS）".into(),
        )),
    }
}

impl Settings {
    fn of(cx: &ConnCx) -> Result<Self, AdapterError> {
        let s = |k: &str| cx.config_str(k).map(str::trim).filter(|v| !v.is_empty());
        let need = |k: &str, label: &str| {
            s(k).map(String::from)
                .ok_or_else(|| AdapterError::Config(format!("缺少{label}")))
        };
        let port = |k: &str, d: u16| -> Result<u16, AdapterError> {
            match s(k) {
                None => Ok(d),
                Some(v) => v
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(|| AdapterError::Config(format!("端口不对：{v}"))),
            }
        };
        let address = need("address", "邮箱地址")?;
        let imap_port = port("imap_port", 993)?;
        let smtp_port = port("smtp_port", 465)?;
        Ok(Self {
            username: s("username").map(String::from).unwrap_or(address.clone()),
            address,
            imap_host: need("imap_host", "IMAP 服务器")?,
            imap_port,
            imap_security: security(s("imap_security"), imap_port, 993)?,
            smtp_host: need("smtp_host", "SMTP 服务器")?,
            smtp_port,
            smtp_security: security(s("smtp_security"), smtp_port, 465)?,
        })
    }

    fn is_gmail(&self) -> bool {
        let h = self.imap_host.to_ascii_lowercase();
        h.ends_with("gmail.com") || h.ends_with("googlemail.com")
    }
}

fn io_err(host: &str, port: u16) -> impl Fn(std::io::Error) -> AdapterError + '_ {
    move |e| {
        if is_tls_cert_error(&e) {
            AdapterError::Rejected("服务器的证书无法验证".into())
        } else if e.kind() == std::io::ErrorKind::InvalidData {
            AdapterError::Rejected("服务器不接受这种加密方式".into())
        } else {
            AdapterError::Transient(format!("连不上邮件服务器 {host}:{port}"))
        }
    }
}

fn is_tls_cert_error(e: &std::io::Error) -> bool {
    e.get_ref()
        .and_then(|i| i.downcast_ref::<rustls::Error>())
        .is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)))
}

fn imap_err(e: async_imap::error::Error) -> AdapterError {
    match e {
        async_imap::error::Error::No(_) => {
            AdapterError::Rejected("邮件服务器拒绝了这个操作".into())
        }
        async_imap::error::Error::Bad(_) => {
            AdapterError::Rejected("邮件服务器不认识这个命令".into())
        }
        _ => AdapterError::Transient("和邮件服务器的连接断了".into()),
    }
}

async fn timed<T>(
    f: impl std::future::Future<Output = Result<T, AdapterError>>,
) -> Result<T, AdapterError> {
    tokio::time::timeout(NET_TIMEOUT, f)
        .await
        .map_err(|_| AdapterError::Transient("邮件服务器没有响应".into()))?
}

/// LOGOUT, bounded by [`LOGOUT_TIMEOUT`]; its outcome does not matter.
async fn logout(mut s: Session) {
    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, s.logout()).await;
}

/// Reads one CRLF line byte by byte (no buffering beyond it: TLS starts right after).
async fn read_line(tcp: &mut TcpStream) -> std::io::Result<String> {
    let mut out = Vec::new();
    loop {
        let b = tcp.read_u8().await?;
        out.push(b);
        if b == b'\n' || out.len() > 8192 {
            return Ok(String::from_utf8_lossy(&out).into_owned());
        }
    }
}

impl ImapAdapter {
    async fn tls(
        &self,
        tcp: TcpStream,
        host: &str,
        port: u16,
    ) -> Result<TlsStream<TcpStream>, AdapterError> {
        let cfg = self.cfg.client_config().map_err(AdapterError::Config)?;
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| AdapterError::Config(format!("服务器地址不对：{host}")))?;
        TlsConnector::from(cfg)
            .connect(name, tcp)
            .await
            .map_err(io_err(host, port))
    }

    /// Connects, logs in, sends `ID` when offered (163 refuses SELECT without it). Returns the
    /// session and its capabilities.
    async fn session(&self, cx: &ConnCx) -> Result<(Session, Caps), AdapterError> {
        let st = Settings::of(cx)?;
        let password = cx.secret("password").await.map_err(AdapterError::from)?;
        timed(self.login(&st, &password)).await
    }

    async fn login(&self, st: &Settings, password: &str) -> Result<(Session, Caps), AdapterError> {
        let (host, port) = (st.imap_host.as_str(), st.imap_port);
        forbid(host)?;
        let mut tcp = TcpStream::connect((host, port))
            .await
            .map_err(io_err(host, port))?;
        let client = match st.imap_security {
            Security::Tls => {
                let tls = self.tls(tcp, host, port).await?;
                let mut c = async_imap::Client::new(tls);
                c.read_response()
                    .await
                    .map_err(io_err(host, port))?
                    .ok_or_else(|| AdapterError::Transient("邮件服务器没有打招呼".into()))?;
                c
            }
            Security::StartTls => {
                let greeting = read_line(&mut tcp).await.map_err(io_err(host, port))?;
                if !greeting.starts_with("* OK") {
                    return Err(AdapterError::Transient("邮件服务器没有打招呼".into()));
                }
                tcp.write_all(b"s0 STARTTLS\r\n")
                    .await
                    .map_err(io_err(host, port))?;
                loop {
                    let l = read_line(&mut tcp).await.map_err(io_err(host, port))?;
                    if l.is_empty() {
                        return Err(AdapterError::Transient("和邮件服务器的连接断了".into()));
                    }
                    if let Some(rest) = l.strip_prefix("s0 ") {
                        if !rest.to_ascii_uppercase().starts_with("OK") {
                            return Err(AdapterError::Rejected("服务器不接受这种加密方式".into()));
                        }
                        break;
                    }
                }
                async_imap::Client::new(self.tls(tcp, host, port).await?)
            }
        };
        let mut s = client
            .login(&st.username, password)
            .await
            .map_err(|(e, _)| match e {
                async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                    AdapterError::Auth("用户名或应用专用密码不对".into())
                }
                e => imap_err(e),
            })?;
        let caps = Caps::of(&mut s).await?;
        if caps.id {
            s.id([("name", Some("Steve")), ("version", Some("2.0"))])
                .await
                .map_err(imap_err)?;
        }
        Ok((s, caps))
    }

    fn smtp(
        &self,
        st: &Settings,
        password: &str,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, AdapterError> {
        // lettre connects lazily: refusing the transport refuses every connection it would make
        forbid(&st.smtp_host)?;
        let mut tls = TlsParameters::builder(st.smtp_host.clone())
            .certificate_store(CertificateStore::Default);
        for c in self.cfg.test_roots() {
            let cert = Certificate::from_der(c.to_vec())
                .map_err(|_| AdapterError::Config("测试根证书不对".into()))?;
            tls = tls.add_root_certificate(cert);
        }
        let tls = tls
            .build_rustls()
            .map_err(|_| AdapterError::Config("TLS 设置不对".into()))?;
        Ok(
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(st.smtp_host.clone())
                .port(st.smtp_port)
                .tls(match st.smtp_security {
                    Security::Tls => Tls::Wrapper(tls),
                    Security::StartTls => Tls::Required(tls),
                })
                .credentials(Credentials::new(st.username.clone(), password.to_string()))
                .timeout(Some(NET_TIMEOUT))
                .build(),
        )
    }
}

/// The test guard (`STEVE_FORBID_REAL_HOSTS=1`): checked before any IMAP or SMTP connection.
fn forbid(host: &str) -> Result<(), AdapterError> {
    steve_modules::net::check_host(host).map_err(|e| AdapterError::Config(e.to_string()))
}

fn smtp_err(e: lettre::transport::smtp::Error) -> AdapterError {
    // Both 4xx and 5xx are explicit negative SMTP replies. A transport error without a status
    // remains uncertain: it may have happened after DATA was accepted but before its ACK arrived.
    if e.is_permanent() || e.is_transient() {
        let code = e.status().map(|c| c.to_string()).unwrap_or_default();
        if code.starts_with("535") || code.starts_with("534") || code.starts_with("530") {
            return AdapterError::Auth("用户名或应用专用密码不对（SMTP）".into());
        }
        return AdapterError::Rejected(format!("SMTP 服务器拒绝了：{code}"));
    }
    if e.is_tls() {
        return AdapterError::Rejected("SMTP 服务器不接受这种加密方式".into());
    }
    if e.is_client() {
        // no common AUTH mechanism, STARTTLS not offered
        return AdapterError::Rejected("SMTP 服务器不接受这种加密方式或登录方式".into());
    }
    AdapterError::Transient("连不上 SMTP 服务器".into())
}

/// The capabilities the sync cares about.
#[derive(Debug, Clone, Copy, Default)]
struct Caps {
    id: bool,
    condstore: bool,
    gmail: bool,
    idle: bool,
    moves: bool,
}

impl Caps {
    async fn of(s: &mut Session) -> Result<Self, AdapterError> {
        let c = s.capabilities().await.map_err(imap_err)?;
        Ok(Self {
            id: c.has_str("ID"),
            condstore: c.has_str("CONDSTORE"),
            gmail: c.has_str("X-GM-EXT-1"),
            idle: c.has_str("IDLE"),
            moves: c.has_str("MOVE"),
        })
    }
}

// ---------------------------------------------------------------- folders

/// The role of a folder: SPECIAL-USE first, then common names.
fn role(name: &str, attrs: &[NameAttribute<'_>]) -> String {
    for a in attrs {
        let r = match a {
            NameAttribute::Sent => "sent",
            NameAttribute::Drafts => "drafts",
            NameAttribute::Archive => "archive",
            NameAttribute::Trash => "trash",
            NameAttribute::Junk => "junk",
            NameAttribute::All => "all",
            NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\Important") => "important",
            _ => continue,
        };
        return r.into();
    }
    if name.eq_ignore_ascii_case("INBOX") {
        return "inbox".into();
    }
    let leaf = name
        .rsplit(['/', '.'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    match leaf.as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "已发送" | "已发送邮件"
        | "已传送" => "sent",
        "drafts" | "draft" | "草稿" | "草稿箱" => "drafts",
        "archive" | "archives" | "归档" | "存档" => "archive",
        "trash" | "deleted" | "deleted items" | "deleted messages" | "已删除" | "已删除邮件" => {
            "trash"
        }
        "junk" | "spam" | "junk e-mail" | "垃圾邮件" | "垃圾箱" => "junk",
        _ => "other",
    }
    .into()
}

async fn list_folders(s: &mut Session) -> Result<Vec<(FolderInfo, String)>, AdapterError> {
    let names: Vec<_> = s
        .list(Some(""), Some("*"))
        .await
        .map_err(imap_err)?
        .try_collect()
        .await
        .map_err(imap_err)?;
    Ok(names
        .iter()
        .filter(|n| {
            !n.attributes()
                .iter()
                .any(|a| matches!(a, NameAttribute::NoSelect))
        })
        .map(|n| {
            let wire = n.name().to_string();
            let name = utf7::decode(&wire);
            let ext = if name.eq_ignore_ascii_case("INBOX") {
                "INBOX".to_string()
            } else {
                name.clone()
            };
            (
                FolderInfo {
                    role: role(&name, n.attributes()),
                    ext_id: ext,
                    name,
                },
                wire,
            )
        })
        .collect())
}

/// An IMAP quoted string.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ---------------------------------------------------------------- raw FETCH

/// One `* n FETCH (…)` response.
#[derive(Debug, Default)]
struct Item {
    uid: Option<u32>,
    flags: Vec<String>,
    internal_date: Option<i64>,
    size: Option<u32>,
    body: Option<Vec<u8>>,
    header: Option<Vec<u8>>,
    modseq: Option<u64>,
    thrid: Option<u64>,
    parts: Vec<PartMeta>,
}

fn norm_flags(fl: &[std::borrow::Cow<'_, str>]) -> Vec<String> {
    let mut out = Vec::new();
    for f in fl {
        let n = match f.to_ascii_lowercase().as_str() {
            "\\seen" => "seen",
            "\\flagged" => "flagged",
            "\\answered" => "answered",
            _ => continue,
        };
        if !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    }
    out.sort();
    out
}

fn imap_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s.trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|d| d.timestamp_millis())
}

fn param<'a>(
    ps: &'a Option<Vec<(std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>>,
    k: &str,
) -> Option<&'a str> {
    ps.as_ref()?
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(k))
        .map(|(_, v)| v.as_ref())
}

/// The attachments in a BODYSTRUCTURE, with IMAP part paths.
fn structure_parts(b: &BodyStructure<'_>, path: &str, out: &mut Vec<PartMeta>) {
    let sub = |i: usize| {
        if path.is_empty() {
            format!("{}", i + 1)
        } else {
            format!("{path}.{}", i + 1)
        }
    };
    let (common, other) = match b {
        BodyStructure::Multipart { bodies, .. } => {
            for (i, c) in bodies.iter().enumerate() {
                structure_parts(c, &sub(i), out);
            }
            return;
        }
        BodyStructure::Basic { common, other, .. }
        | BodyStructure::Text { common, other, .. }
        | BodyStructure::Message { common, other, .. } => (common, other),
    };
    let disp = common.disposition.as_ref();
    let is_text = matches!(b, BodyStructure::Text { .. });
    let attachment = disp.is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
    if is_text && !attachment {
        return;
    }
    let name = disp
        .and_then(|d| param(&d.params, "filename"))
        .or_else(|| param(&common.ty.params, "name"))
        .unwrap_or("未命名附件")
        .to_string();
    let inline = disp.is_some_and(|d| d.ty.eq_ignore_ascii_case("inline")) && other.id.is_some();
    out.push(PartMeta {
        part: if path.is_empty() {
            "1".into()
        } else {
            path.into()
        },
        name,
        mime: format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase(),
        size: other.octets as i64,
        content_id: other.id.as_ref().map(|i| {
            i.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        }),
        inline,
    });
}

fn item_of(attrs: &[AttributeValue<'_>]) -> Item {
    let mut it = Item::default();
    for a in attrs {
        match a {
            AttributeValue::Uid(u) => it.uid = Some(*u),
            AttributeValue::Flags(f) => it.flags = norm_flags(f),
            AttributeValue::InternalDate(d) => it.internal_date = imap_date(d),
            AttributeValue::Rfc822Size(n) => it.size = Some(*n),
            AttributeValue::ModSeq(m) => it.modseq = Some(*m),
            AttributeValue::GmailThrId(t) => it.thrid = Some(*t),
            AttributeValue::BodySection { section, data, .. } => {
                let bytes = data.as_ref().map(|d| d.to_vec());
                match section {
                    None => it.body = bytes,
                    Some(SectionPath::Full(_)) => it.header = bytes,
                    Some(SectionPath::Part(..)) => {}
                }
            }
            AttributeValue::Rfc822(d) => it.body = d.as_ref().map(|d| d.to_vec()),
            AttributeValue::Rfc822Header(d) => it.header = d.as_ref().map(|d| d.to_vec()),
            AttributeValue::BodyStructure(b) => structure_parts(b, "", &mut it.parts),
            _ => {}
        }
    }
    it
}

/// Runs a FETCH-like command and collects its FETCH responses (async-imap's `Fetch` does not expose
/// `X-GM-THRID`, so the responses are read directly).
async fn fetch(s: &mut Session, cmd: &str) -> Result<Vec<Item>, AdapterError> {
    let tag = timed(async { s.run_command(cmd).await.map_err(imap_err) }).await?;
    let mut out = Vec::new();
    loop {
        let r = timed(async {
            s.read_response()
                .await
                .map_err(|_| AdapterError::Transient("和邮件服务器的连接断了".into()))
        })
        .await?
        .ok_or_else(|| AdapterError::Transient("和邮件服务器的连接断了".into()))?;
        match r.parsed() {
            Response::Fetch(_, attrs) => out.push(item_of(attrs)),
            Response::Done { tag: t, status, .. } if *t == tag => {
                return match status {
                    Status::Ok => Ok(out),
                    _ => Err(AdapterError::Rejected("邮件服务器拒绝了这个操作".into())),
                };
            }
            Response::Data {
                status: Status::Bye,
                ..
            } => return Err(AdapterError::Transient("和邮件服务器的连接断了".into())),
            _ => {}
        }
    }
}

/// `UID STORE <uid> <op>FLAGS.SILENT (<flag>)`.
async fn store_flag(s: &mut Session, uid: u32, op: &str, flag: &str) -> Result<(), AdapterError> {
    timed(async {
        let _: Vec<_> = s
            .uid_store(uid.to_string(), format!("{op}FLAGS.SILENT ({flag})"))
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        Ok(())
    })
    .await
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// `1-Oct-2026` for SEARCH SINCE.
fn search_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%-d-%b-%Y").to_string())
        .unwrap_or_else(|| "1-Jan-1970".into())
}

/// `<folder>:<uidvalidity>:<uid>`.
pub fn ext_id(folder: &str, uidvalidity: i64, uid: u32) -> String {
    format!("{folder}:{uidvalidity}:{uid}")
}

/// Splits an [`ext_id`].
pub fn split_ext(ext: &str) -> Option<(&str, i64, u32)> {
    let mut it = ext.rsplitn(3, ':');
    let uid = it.next()?.parse().ok()?;
    let v = it.next()?.parse().ok()?;
    Some((it.next()?, v, uid))
}

impl ImapAdapter {
    /// The wire names of the folders by decoded name.
    async fn wire_names(
        s: &mut Session,
    ) -> Result<BTreeMap<String, (String, String)>, AdapterError> {
        Ok(list_folders(s)
            .await?
            .into_iter()
            .map(|(f, wire)| (f.ext_id, (wire, f.role)))
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_folder(
        s: &mut Session,
        caps: Caps,
        wire: &str,
        f: &mut crate::adapter::FolderState,
        sync_days: i64,
        now: i64,
        sink: &mut dyn MailSink,
        report: &mut SyncReport,
    ) -> Result<(), AdapterError> {
        let mb = timed(async { s.examine(wire).await.map_err(imap_err) }).await?;
        let validity = mb.uid_validity.map(i64::from).unwrap_or(0);
        if f.uidvalidity != Some(validity) {
            if f.uidvalidity.is_some() {
                // the UIDs mean something else now: rescan the window, the store dedupes
                report.reset.push(f.ext_id.clone());
            }
            f.uidvalidity = Some(validity);
            f.last_uid = None;
            f.modseq = None;
        }
        let first = f.last_uid.is_none();
        let backfill = first;
        let mut uids: Vec<u32> = if first {
            let since = search_date(now - sync_days.max(1) * 86_400_000);
            let mut u: Vec<u32> = timed(async {
                s.uid_search(format!("SINCE {since}"))
                    .await
                    .map_err(imap_err)
            })
            .await?
            .into_iter()
            .collect();
            u.sort_unstable();
            let skip = u.len().saturating_sub(FIRST_SYNC_MAX);
            u.split_off(skip)
        } else {
            let last = f.last_uid.unwrap_or(0) as u32;
            let mut u: Vec<u32> = fetch(s, &format!("UID FETCH {}:* (UID)", last + 1))
                .await?
                .into_iter()
                .filter_map(|i| i.uid)
                .filter(|u| *u > last)
                .collect();
            u.sort_unstable();
            u
        };
        uids.dedup();
        let thrid = if caps.gmail { " X-GM-THRID" } else { "" };
        for chunk in uids.chunks(BATCH) {
            let sizes = fetch(
                s,
                &format!("UID FETCH {} (UID RFC822.SIZE)", uid_set(chunk)),
            )
            .await?;
            let (big, small): (Vec<u32>, Vec<u32>) = chunk.iter().partition(|u| {
                sizes
                    .iter()
                    .any(|i| i.uid == Some(**u) && i.size.unwrap_or(0) > LARGE_BYTES)
            });
            let mut items = Vec::new();
            if !small.is_empty() {
                items.extend(
                    fetch(
                        s,
                        &format!(
                            "UID FETCH {} (UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[]{thrid})",
                            uid_set(&small)
                        ),
                    )
                    .await?,
                );
            }
            if !big.is_empty() {
                items.extend(
                    fetch(
                        s,
                        &format!(
                            "UID FETCH {} (UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER] BODYSTRUCTURE{thrid})",
                            uid_set(&big)
                        ),
                    )
                    .await?,
                );
            }
            items.sort_by_key(|i| i.uid);
            let mut batch = Vec::with_capacity(items.len());
            let mut top = None;
            for it in items {
                let Some(uid) = it.uid else { continue };
                let partial = it.body.is_none();
                let Some(raw) = it.body.or(it.header) else {
                    continue;
                };
                batch.push(FetchedMessage {
                    ext_id: ext_id(&f.ext_id, validity, uid),
                    folder: f.ext_id.clone(),
                    mode: f.mode,
                    raw,
                    partial,
                    parts: it.parts,
                    flags: it.flags,
                    internal_date: it.internal_date,
                    size: it.size.map(i64::from).unwrap_or(0),
                    provider_thread: it.thrid.map(|t| t.to_string()),
                    backfill,
                });
                top = top.max(Some(i64::from(uid)));
            }
            // the cursor moves only once the whole chunk is stored
            let n = batch.len();
            sink.put_batch(batch).await?;
            report.fetched += n;
            if let Some(top) = top {
                f.last_uid = Some(f.last_uid.unwrap_or(0).max(top));
            }
        }
        if first {
            // everything below UIDNEXT was looked at: older mail outside the window stays behind
            let seen = i64::from(mb.uid_next.unwrap_or(1)).saturating_sub(1);
            f.last_uid = Some(f.last_uid.unwrap_or(0).max(seen));
        }
        // flags
        let last = f.last_uid.unwrap_or(0);
        if !first && last > 0 {
            if caps.condstore
                && let (Some(old), Some(high)) = (f.modseq, mb.highest_modseq)
            {
                if high as i64 > old {
                    let updates: Vec<_> = fetch(
                        s,
                        &format!("UID FETCH 1:{last} (UID FLAGS) (CHANGEDSINCE {old})"),
                    )
                    .await?
                    .into_iter()
                    .filter_map(|it| Some((ext_id(&f.ext_id, validity, it.uid?), it.flags)))
                    .collect();
                    report.flag_updates += updates.len();
                    sink.flags_batch(updates).await?;
                }
            } else if !caps.condstore
                && f.flags_checked.is_none_or(|t| now - t >= FLAG_RECHECK_MS)
                && mb.exists > 0
            {
                let from = mb.exists.saturating_sub(FLAG_RECHECK_COUNT - 1).max(1);
                let updates: Vec<_> = fetch(s, &format!("FETCH {from}:* (UID FLAGS)"))
                    .await?
                    .into_iter()
                    .filter_map(|it| Some((ext_id(&f.ext_id, validity, it.uid?), it.flags)))
                    .collect();
                report.flag_updates += updates.len();
                sink.flags_batch(updates).await?;
                f.flags_checked = Some(now);
            }
        }
        if first {
            f.flags_checked = Some(now);
        }
        if caps.condstore {
            f.modseq = mb.highest_modseq.map(|m| m as i64).or(f.modseq);
        }
        f.synced = Some(now);
        Ok(())
    }

    /// [`MailAdapter::set_flags`] for one message over an open session.
    async fn apply_flags(
        s: &mut Session,
        caps: Caps,
        wires: &BTreeMap<String, (String, String)>,
        ext: &str,
        ch: &FlagChange,
    ) -> Result<(), AdapterError> {
        let (_, uid) = timed(Self::find_uid(s, ext, None, wires, true)).await?;
        if let Some(seen) = ch.seen {
            store_flag(s, uid, if seen { "+" } else { "-" }, "\\Seen").await?;
        }
        if let Some(fl) = ch.flagged {
            store_flag(s, uid, if fl { "+" } else { "-" }, "\\Flagged").await?;
        }
        if ch.archive {
            let target = wires
                .values()
                .find(|(_, role)| role == "archive")
                .map(|(w, _)| w.clone())
                .ok_or_else(|| AdapterError::Rejected("这个邮箱没有归档文件夹".into()))?;
            if caps.moves {
                timed(async {
                    s.uid_mv(uid.to_string(), target.as_str())
                        .await
                        .map_err(imap_err)
                })
                .await?;
            } else {
                timed(async {
                    s.uid_copy(uid.to_string(), target.as_str())
                        .await
                        .map_err(imap_err)
                })
                .await?;
                store_flag(s, uid, "+", "\\Deleted").await?;
                timed(async {
                    let r: Vec<_> = s
                        .uid_expunge(uid.to_string())
                        .await
                        .map_err(imap_err)?
                        .try_collect()
                        .await
                        .map_err(imap_err)?;
                    drop(r);
                    Ok(())
                })
                .await?;
            }
        }
        Ok(())
    }

    async fn find_uid(
        s: &mut Session,
        ext: &str,
        message_id: Option<&str>,
        wires: &BTreeMap<String, (String, String)>,
        write: bool,
    ) -> Result<(String, u32), AdapterError> {
        let gone = || AdapterError::Gone("原件已经不在邮箱里了".into());
        let (folder, validity, uid) = split_ext(ext).ok_or_else(gone)?;
        let mut candidates: Vec<&str> = vec![folder];
        candidates.extend(wires.keys().map(String::as_str).filter(|k| *k != folder));
        for (i, name) in candidates.iter().enumerate() {
            let Some((wire, _)) = wires.get(*name) else {
                continue;
            };
            let mb = if write {
                s.select(wire.as_str()).await
            } else {
                s.examine(wire.as_str()).await
            }
            .map_err(imap_err)?;
            if i == 0 && mb.uid_validity.map(i64::from) == Some(validity) {
                let hit = fetch(s, &format!("UID FETCH {uid} (UID)")).await?;
                if hit.iter().any(|h| h.uid == Some(uid)) {
                    return Ok((wire.clone(), uid));
                }
            }
            if let Some(mid) = message_id {
                let found = s
                    .uid_search(format!("HEADER Message-ID {}", quote(&format!("<{mid}>"))))
                    .await
                    .map_err(imap_err)?;
                if let Some(u) = found.into_iter().min() {
                    return Ok((wire.clone(), u));
                }
            } else {
                break;
            }
        }
        Err(gone())
    }
}

#[async_trait]
impl ConnectionProvider for ImapAdapter {
    fn id(&self) -> &str {
        "imap"
    }
    fn system(&self) -> &str {
        "mail"
    }
    fn name(&self) -> &str {
        "其他邮箱（IMAP）"
    }
    fn localize(&self, text: &str) -> Option<String> {
        const KEYS: &[&str] = &[
            "mail.text.imap_name",
            "mail.text.address",
            "mail.text.username",
            "mail.text.username_hint",
            "mail.text.password",
            "mail.text.imap_host",
            "mail.text.imap_port",
            "mail.text.imap_security",
            "mail.text.smtp_host",
            "mail.text.smtp_port",
            "mail.text.smtp_security",
        ];
        steve_i18n::reword!(text, KEYS)
    }
    fn caps(&self) -> &[&'static str] {
        &["mail.read", "mail.draft", "mail.send", "events"]
    }
    /// Provider “邮箱”: Gmail, Outlook and other mailboxes all come in through here for now.
    fn integration(&self) -> steve_modules::accounts::connections::Integration {
        use steve_modules::accounts::connections::{CapSpec, Integration};
        Integration {
            provider: "mail".into(),
            provider_name: steve_i18n::t!("mail.provider.name"),
            identity: "account".into(),
            capabilities: vec![
                CapSpec::supported("mail", &["mail.read", "mail.draft", "mail.send"])
                    .with_note(&steve_i18n::t!("mail.provider.note")),
            ],
        }
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }

    /// IMAP login + LIST (+ `ID`), then SMTP `EHLO` + `AUTH` without sending.
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let st = Settings::of(cx)?;
        let password = cx.secret("password").await?;
        let (mut s, _) = timed(self.login(&st, &password)).await?;
        timed(async { list_folders(&mut s).await }).await?;
        logout(s).await;
        let smtp = self.smtp(&st, &password)?;
        let ok = timed(async { smtp.test_connection().await.map_err(smtp_err) }).await?;
        if !ok {
            return Err(ProviderError::Transient("连不上 SMTP 服务器".into()));
        }
        let mut info = AccountInfo::new(st.address.clone(), st.address.clone());
        info.kind = "personal".into();
        Ok(info)
    }
}

#[async_trait]
impl MailAdapter for ImapAdapter {
    async fn folders(&self, cx: &ConnCx) -> Result<Vec<FolderInfo>, AdapterError> {
        let (mut s, _) = self.session(cx).await?;
        let out = timed(list_folders(&mut s)).await?;
        logout(s).await;
        Ok(out.into_iter().map(|(f, _)| f).collect())
    }

    async fn sync(
        &self,
        cx: &ConnCx,
        state: &mut SyncState,
        sink: &mut dyn MailSink,
    ) -> Result<SyncReport, AdapterError> {
        let (_, session) = self.open_sync(cx).await?;
        session.sync(state, sink).await
    }

    /// One login and one LIST serve both the folder list and the sync that follows.
    async fn open_sync<'a>(
        &'a self,
        cx: &'a ConnCx,
    ) -> Result<(Vec<FolderInfo>, Box<dyn SyncSession + 'a>), AdapterError> {
        let (mut s, caps) = self.session(cx).await?;
        let listed = timed(list_folders(&mut s)).await?;
        let wires = listed
            .iter()
            .map(|(f, wire)| (f.ext_id.clone(), (wire.clone(), f.role.clone())))
            .collect();
        let folders = listed.into_iter().map(|(f, _)| f).collect();
        Ok((folders, Box::new(ImapSync { s, caps, wires })))
    }

    async fn watch(
        &self,
        cx: &ConnCx,
        stop: CancellationToken,
        wake: mpsc::Sender<()>,
    ) -> Result<(), AdapterError> {
        let (mut s, caps) = self.session(cx).await?;
        if !caps.idle {
            logout(s).await;
            return Err(AdapterError::Unsupported);
        }
        loop {
            timed(async { s.examine("INBOX").await.map_err(imap_err) }).await?;
            let mut idle = s.idle();
            timed(async { idle.init().await.map_err(imap_err) }).await?;
            loop {
                let (wait, _interrupt) = idle.wait_with_timeout(IDLE_RENEW);
                let r = tokio::select! {
                    r = wait => r,
                    _ = stop.cancelled() => return Ok(()),
                };
                match r.map_err(imap_err)? {
                    async_imap::extensions::idle::IdleResponse::NewData(d) => {
                        if matches!(d.parsed(), Response::MailboxData(MailboxDatum::Exists(_)))
                            && wake.send(()).await.is_err()
                        {
                            return Ok(());
                        }
                        if matches!(
                            d.parsed(),
                            Response::Data {
                                status: Status::Bye,
                                ..
                            }
                        ) {
                            return Err(AdapterError::Transient("和邮件服务器的连接断了".into()));
                        }
                    }
                    async_imap::extensions::idle::IdleResponse::Timeout
                    | async_imap::extensions::idle::IdleResponse::ManualInterrupt => break,
                }
            }
            s = timed(async { idle.done().await.map_err(imap_err) }).await?;
        }
    }

    async fn fetch_raw(
        &self,
        cx: &ConnCx,
        ext: &str,
        message_id: Option<&str>,
    ) -> Result<Vec<u8>, AdapterError> {
        let (mut s, _) = self.session(cx).await?;
        let wires = timed(Self::wire_names(&mut s)).await?;
        let (_, uid) = timed(Self::find_uid(&mut s, ext, message_id, &wires, false)).await?;
        let items = timed(fetch(&mut s, &format!("UID FETCH {uid} (UID BODY.PEEK[])"))).await?;
        logout(s).await;
        items
            .into_iter()
            .find_map(|i| i.body)
            .ok_or_else(|| AdapterError::Gone("原件已经不在邮箱里了".into()))
    }

    async fn set_flags(&self, cx: &ConnCx, ext: &str, ch: &FlagChange) -> Result<(), AdapterError> {
        let (mut s, caps) = self.session(cx).await?;
        let wires = timed(Self::wire_names(&mut s)).await?;
        Self::apply_flags(&mut s, caps, &wires, ext, ch).await?;
        logout(s).await;
        Ok(())
    }

    /// One session and one LIST for all of them (an archived thread is many messages; providers
    /// throttle repeated logins).
    async fn set_flags_many(
        &self,
        cx: &ConnCx,
        ext_ids: &[&str],
        ch: &FlagChange,
    ) -> Vec<Result<(), AdapterError>> {
        let opened = async {
            let (mut s, caps) = self.session(cx).await?;
            let wires = timed(Self::wire_names(&mut s)).await?;
            Ok::<_, AdapterError>((s, caps, wires))
        };
        let (mut s, caps, wires) = match opened.await {
            Ok(o) => o,
            Err(e) => return vec![Err(e); ext_ids.len()],
        };
        let mut out = Vec::with_capacity(ext_ids.len());
        for ext in ext_ids {
            let r = Self::apply_flags(&mut s, caps, &wires, ext, ch).await;
            // after a broken connection or a timeout the session is in no known state: the
            // rest fail the same way
            if let Err(e @ AdapterError::Transient(_)) = &r {
                let e = e.clone();
                out.push(r);
                out.resize(ext_ids.len(), Err(e));
                return out;
            }
            out.push(r);
        }
        logout(s).await;
        out
    }

    async fn send(
        &self,
        cx: &ConnCx,
        mime: &[u8],
        meta: &SendMeta,
    ) -> Result<SentRef, AdapterError> {
        let st = Settings::of(cx)?;
        let password: Zeroizing<String> = cx.secret("password").await?;
        let from = meta
            .from
            .parse()
            .map_err(|_| AdapterError::Rejected("发件地址不对".into()))?;
        let rcpt = meta
            .rcpt
            .iter()
            .map(|r| r.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AdapterError::Rejected("收件地址不对".into()))?;
        let envelope = lettre::address::Envelope::new(Some(from), rcpt)
            .map_err(|_| AdapterError::Rejected("没有收件人".into()))?;
        let smtp = self.smtp(&st, &password)?;
        timed(async { smtp.send_raw(&envelope, mime).await.map_err(smtp_err) }).await?;
        Ok(SentRef::default())
    }

    async fn copy_sent(
        &self,
        cx: &ConnCx,
        mime: &[u8],
        meta: &SendMeta,
    ) -> Result<(), AdapterError> {
        let st = Settings::of(cx)?;
        if !meta.append_sent.unwrap_or(!st.is_gmail()) {
            return Ok(());
        }
        let password: Zeroizing<String> = cx.secret("password").await?;
        let (mut s, _) = self.login(&st, &password).await?;
        let wires = Self::wire_names(&mut s).await?;
        if let Some((wire, _)) = wires.values().find(|(_, role)| role == "sent") {
            s.append(wire.as_str(), Some("(\\Seen)"), None, mime)
                .await
                .map_err(imap_err)?;
        }
        // The sender's copy deadline includes this final exchange too.
        s.logout().await.map_err(imap_err)?;
        Ok(())
    }
}

/// The sync half of [`ImapAdapter::open_sync`]: the logged-in session and its LIST.
struct ImapSync {
    s: Session,
    caps: Caps,
    wires: BTreeMap<String, (String, String)>,
}

#[async_trait]
impl SyncSession for ImapSync {
    async fn sync(
        self: Box<Self>,
        state: &mut SyncState,
        sink: &mut dyn MailSink,
    ) -> Result<SyncReport, AdapterError> {
        let ImapSync { mut s, caps, wires } = *self;
        let mut report = SyncReport::default();
        for f in state.folders.iter_mut() {
            if f.mode == ScopeMode::Ignore {
                continue;
            }
            let Some((wire, _)) = wires.get(&f.ext_id) else {
                continue;
            };
            ImapAdapter::sync_folder(
                &mut s,
                caps,
                wire,
                f,
                state.sync_days,
                state.now,
                sink,
                &mut report,
            )
            .await?;
        }
        logout(s).await;
        Ok(report)
    }

    async fn close(self: Box<Self>) {
        logout(self.s).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form and the name in the reader's language; zh-CN as declared.
    #[test]
    fn form_text_by_language() {
        use steve_modules::accounts::connections::{fields_shown, shown};
        let a = ImapAdapter::new(MailConfig::new(std::env::temp_dir()));
        let texts = || {
            let mut out = vec![shown(&a, a.name())];
            for f in fields_shown(&a).unwrap() {
                out.push(f.label);
                out.extend(f.hint);
            }
            out
        };
        let en = steve_i18n::with(steve_i18n::EN, texts);
        assert_eq!(en[0], "Other mailbox (IMAP)");
        assert!(en.contains(&"Email address".to_string()), "{en:?}");
        assert!(
            en.iter()
                .all(|t| !t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))),
            "{en:?}"
        );
        let zh = steve_i18n::with(steve_i18n::ZH_CN, texts);
        assert_eq!(zh[0], "其他邮箱（IMAP）");
        assert_eq!(zh[1], "邮箱地址");
    }

    #[test]
    fn roles_by_name() {
        assert_eq!(role("INBOX", &[]), "inbox");
        assert_eq!(role("已发送", &[]), "sent");
        assert_eq!(role("[Gmail]/All Mail", &[NameAttribute::All]), "all");
        assert_eq!(role("Work", &[]), "other");
    }

    #[test]
    fn ext_ids() {
        assert_eq!(split_ext("a:b:7:9"), Some(("a:b", 7, 9)));
        assert_eq!(split_ext(&ext_id("INBOX", 3, 5)), Some(("INBOX", 3, 5)));
        assert_eq!(split_ext("x"), None);
        assert_eq!(search_date(1_793_491_200_000), "1-Nov-2026");
    }

    /// IMAP and SMTP to a real host are refused under `STEVE_FORBID_REAL_HOSTS=1` before
    /// connecting (`.invalid` hosts: even a broken guard would only fail a DNS lookup).
    #[tokio::test]
    async fn real_hosts_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child(
            "adapters::imap::tests::real_hosts_refused_under_the_flag",
        ) {
            return;
        }
        let a = ImapAdapter::new(MailConfig::new(std::env::temp_dir()));
        let st = Settings {
            address: "me@example.invalid".into(),
            username: "me".into(),
            imap_host: "imap.example.invalid".into(),
            imap_port: 993,
            imap_security: Security::Tls,
            smtp_host: "smtp.example.invalid".into(),
            smtp_port: 465,
            smtp_security: Security::Tls,
        };
        let e = a.login(&st, "fake-password").await.err();
        assert_eq!(
            e,
            Some(AdapterError::Config(
                "测试环境禁止连接外部主机：imap.example.invalid".into()
            ))
        );
        let e = a.smtp(&st, "fake-password").err();
        assert_eq!(
            e,
            Some(AdapterError::Config(
                "测试环境禁止连接外部主机：smtp.example.invalid".into()
            ))
        );
        let local = Settings {
            smtp_host: "127.0.0.1".into(),
            ..st
        };
        assert!(a.smtp(&local, "fake-password").is_ok());
    }
}
