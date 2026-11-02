//! Fake SMTP submission server (email.md §12): `EHLO`/`HELO`, `STARTTLS`,
//! `AUTH PLAIN`/`AUTH LOGIN`, `MAIL FROM`, `RCPT TO`, `DATA` (dot
//! un-stuffing), `RSET`, `NOOP`, `QUIT`. Accepted messages land in
//! [`FakeSmtp::sent`] and, when a [`FakeImap`] is attached, in the INBOX of
//! every recipient that has an account there (round-trip tests).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::{
    BoxIo, FailRules, FakeError, FakeImap, Lifeline, SharedLifeline, TestCa, TlsMode, lock,
    now_secs,
};

/// Largest accepted message (advertised as `SIZE`).
const MAX_SIZE: usize = 35 * 1024 * 1024;

/// How to start a [`FakeSmtp`].
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub tls: TlsMode,
    /// Root to sign the server certificate with; generated when `None`.
    pub ca: Option<TestCa>,
    /// `(user, password)` accepted by `AUTH`.
    pub accounts: Vec<(String, String)>,
    /// Refuse `MAIL FROM` before `AUTH` (a submission server does).
    pub require_auth: bool,
    /// Also deliver accepted mail into this IMAP fake.
    pub deliver_to: Option<FakeImap>,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            tls: TlsMode::Implicit,
            ca: None,
            accounts: vec![("me@example.com".into(), "app-password".into())],
            require_auth: true,
            deliver_to: None,
        }
    }
}

/// One accepted message.
#[derive(Debug, Clone, PartialEq)]
pub struct SentMail {
    /// Envelope sender (`MAIL FROM`), without brackets.
    pub from: String,
    /// Envelope recipients (`RCPT TO`), without brackets.
    pub rcpt: Vec<String>,
    /// The message exactly as transmitted, dot-stuffing removed.
    pub data: Vec<u8>,
    /// Who authenticated, if anyone.
    pub auth_user: Option<String>,
    /// Whether the session was encrypted.
    pub tls: bool,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug)]
struct Shared {
    cfg: SmtpConfig,
    ca: TestCa,
    sent: Mutex<Vec<SentMail>>,
    fail: FailRules,
    log: Mutex<Vec<String>>,
}

/// Handle to a running fake SMTP server. Cloning shares the server.
#[derive(Debug, Clone)]
pub struct FakeSmtp {
    shared: Arc<Shared>,
    life: SharedLifeline,
    addr: SocketAddr,
}

impl FakeSmtp {
    /// Binds 127.0.0.1 on a random port and starts serving.
    pub async fn start(cfg: SmtpConfig) -> Result<Self, FakeError> {
        let ca = match &cfg.ca {
            Some(ca) => ca.clone(),
            None => TestCa::generate()?,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            cfg,
            ca,
            sent: Mutex::new(Vec::new()),
            fail: FailRules::default(),
            log: Mutex::new(Vec::new()),
        });
        let life = Arc::new(Lifeline::new());
        let task = {
            let shared = shared.clone();
            let kill = life.subscribe();
            tokio::spawn(async move {
                loop {
                    let Ok((sock, _)) = listener.accept().await else {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    };
                    let shared = shared.clone();
                    let kill = kill.clone();
                    tokio::spawn(async move {
                        if let Err(e) = serve(shared, sock, kill).await {
                            tracing::debug!("fake smtp connection ended: {e}");
                        }
                    });
                }
            })
        };
        life.set_task(task);
        Ok(Self { shared, life, addr })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// Always `localhost` — the name on the server certificate.
    pub fn host(&self) -> &'static str {
        "localhost"
    }
    pub fn tls_mode(&self) -> TlsMode {
        self.shared.cfg.tls
    }
    pub fn ca(&self) -> &TestCa {
        &self.shared.ca
    }

    /// Every accepted message, oldest first.
    pub fn sent(&self) -> Vec<SentMail> {
        lock(&self.shared.sent).clone()
    }

    /// The next time `command` arrives (`"EHLO"`, `"AUTH"`, `"MAIL"`,
    /// `"RCPT"`, `"DATA"`), answer `response` (e.g. `"451 4.3.0 try later"`)
    /// instead. `"."` fails the end of DATA: the message is read, then
    /// rejected and not captured. `"AFTER_ACCEPT"` with `"disconnect"` or `"hold"` captures the
    /// message but loses or withholds the final acknowledgement (uncertain-send tests).
    pub fn fail_next(&self, command: &str, response: &str) {
        self.shared.fail.push(command, response);
    }

    /// Cuts every open connection; the listener keeps accepting.
    pub fn drop_connections(&self) {
        self.life.kill_connections();
    }

    /// Commands received, `AUTH` arguments masked.
    pub fn commands(&self) -> Vec<String> {
        lock(&self.shared.log).clone()
    }

    /// Forgets captured mail, scripted failures and the log; cuts connections.
    pub fn reset(&self) {
        lock(&self.shared.sent).clear();
        lock(&self.shared.log).clear();
        self.shared.fail.clear();
        self.life.kill_connections();
    }
}

struct Envelope {
    from: Option<String>,
    rcpt: Vec<String>,
}

fn addr_arg(arg: &str, prefix: &str) -> Option<String> {
    let rest = arg.get(..prefix.len())?;
    if !rest.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = arg[prefix.len()..].trim_start();
    let addr = rest.split_whitespace().next()?;
    Some(
        addr.trim_start_matches('<')
            .trim_end_matches('>')
            .to_string(),
    )
}

async fn reply(io: &mut BufReader<BoxIo>, text: &str) -> std::io::Result<()> {
    io.get_mut().write_all(text.as_bytes()).await?;
    io.get_mut().write_all(b"\r\n").await?;
    io.get_mut().flush().await
}

async fn line(io: &mut BufReader<BoxIo>) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = (&mut *io)
        .take(64 * 1024)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    ))
}

async fn serve(
    shared: Arc<Shared>,
    sock: TcpStream,
    mut kill: watch::Receiver<u64>,
) -> std::io::Result<()> {
    kill.borrow_and_update();
    tokio::select! {
        r = session(shared, sock) => r,
        _ = kill.changed() => Ok(()),
    }
}

async fn session(shared: Arc<Shared>, sock: TcpStream) -> std::io::Result<()> {
    let _ = sock.set_nodelay(true);
    let mut tls = shared.cfg.tls == TlsMode::Implicit;
    let mut io: BufReader<BoxIo> = if tls {
        BufReader::new(Box::new(shared.ca.acceptor().accept(sock).await?))
    } else {
        BufReader::new(Box::new(sock))
    };
    reply(&mut io, "220 localhost ESMTP Steve fake SMTP").await?;
    let mut helo = false;
    let mut user: Option<String> = None;
    let mut env = Envelope {
        from: None,
        rcpt: Vec::new(),
    };
    let tls_first = shared.cfg.tls == TlsMode::StartTls;

    while let Some(l) = line(&mut io).await? {
        let (verb, arg) = match l.split_once(' ') {
            Some((v, a)) => (v.to_ascii_uppercase(), a.trim().to_string()),
            None => (l.trim().to_ascii_uppercase(), String::new()),
        };
        lock(&shared.log).push(if verb == "AUTH" {
            format!("AUTH {} ***", arg.split_whitespace().next().unwrap_or(""))
        } else {
            l.clone()
        });
        if let Some(resp) = shared.fail.take(&verb) {
            reply(&mut io, &resp).await?;
            continue;
        }
        match verb.as_str() {
            "EHLO" => {
                helo = true;
                env = Envelope {
                    from: None,
                    rcpt: Vec::new(),
                };
                let mut lines = vec![
                    format!("localhost greets {arg}"),
                    format!("SIZE {MAX_SIZE}"),
                    "8BITMIME".into(),
                    "PIPELINING".into(),
                    "ENHANCEDSTATUSCODES".into(),
                    "SMTPUTF8".into(),
                ];
                if tls_first && !tls {
                    lines.push("STARTTLS".into());
                } else {
                    lines.push("AUTH PLAIN LOGIN".into());
                }
                lines.push("HELP".into());
                let n = lines.len();
                let text: Vec<String> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, x)| format!("250{}{x}", if i + 1 == n { ' ' } else { '-' }))
                    .collect();
                reply(&mut io, &text.join("\r\n")).await?;
            }
            "HELO" => {
                helo = true;
                reply(&mut io, "250 localhost").await?;
            }
            "STARTTLS" => {
                if !tls_first || tls {
                    reply(&mut io, "502 5.5.1 STARTTLS not available").await?;
                    continue;
                }
                reply(&mut io, "220 2.0.0 Ready to start TLS").await?;
                if !io.buffer().is_empty() {
                    return Ok(());
                }
                io = BufReader::new(Box::new(
                    shared.ca.acceptor().accept(io.into_inner()).await?,
                ));
                tls = true;
                helo = false;
                user = None;
                env = Envelope {
                    from: None,
                    rcpt: Vec::new(),
                };
            }
            "AUTH" => {
                if tls_first && !tls {
                    reply(&mut io, "530 5.7.0 Must issue a STARTTLS command first").await?;
                    continue;
                }
                if user.is_some() {
                    reply(&mut io, "503 5.5.1 Already authenticated").await?;
                    continue;
                }
                let mut parts = arg.split_whitespace();
                let mech = parts.next().unwrap_or("").to_ascii_uppercase();
                let initial = parts.next().map(str::to_string);
                let creds = match mech.as_str() {
                    "PLAIN" => {
                        let resp = match initial {
                            Some(r) => r,
                            None => {
                                reply(&mut io, "334 ").await?;
                                line(&mut io).await?.unwrap_or_default()
                            }
                        };
                        B64.decode(resp.trim()).ok().and_then(|d| {
                            let f: Vec<&[u8]> = d.split(|&c| c == 0).collect();
                            (f.len() == 3).then(|| {
                                (
                                    String::from_utf8_lossy(f[1]).into_owned(),
                                    String::from_utf8_lossy(f[2]).into_owned(),
                                )
                            })
                        })
                    }
                    "LOGIN" => {
                        let u = match initial {
                            Some(u) => u,
                            None => {
                                reply(&mut io, "334 VXNlcm5hbWU6").await?;
                                line(&mut io).await?.unwrap_or_default()
                            }
                        };
                        reply(&mut io, "334 UGFzc3dvcmQ6").await?;
                        let p = line(&mut io).await?.unwrap_or_default();
                        match (B64.decode(u.trim()), B64.decode(p.trim())) {
                            (Ok(u), Ok(p)) => Some((
                                String::from_utf8_lossy(&u).into_owned(),
                                String::from_utf8_lossy(&p).into_owned(),
                            )),
                            _ => None,
                        }
                    }
                    _ => {
                        reply(&mut io, "504 5.5.4 Unrecognized authentication type").await?;
                        continue;
                    }
                };
                let ok = creds.as_ref().is_some_and(|(u, p)| {
                    shared
                        .cfg
                        .accounts
                        .iter()
                        .any(|(au, ap)| au == u && ap == p)
                });
                if ok {
                    user = creds.map(|c| c.0);
                    reply(&mut io, "235 2.7.0 Authentication successful").await?;
                } else {
                    reply(&mut io, "535 5.7.8 Authentication credentials invalid").await?;
                }
            }
            "MAIL" => {
                if !helo {
                    reply(&mut io, "503 5.5.1 EHLO first").await?;
                } else if tls_first && !tls {
                    reply(&mut io, "530 5.7.0 Must issue a STARTTLS command first").await?;
                } else if shared.cfg.require_auth && user.is_none() {
                    reply(&mut io, "530 5.7.0 Authentication required").await?;
                } else if let Some(a) = addr_arg(&arg, "FROM:") {
                    env.from = Some(a);
                    env.rcpt.clear();
                    reply(&mut io, "250 2.1.0 OK").await?;
                } else {
                    reply(&mut io, "501 5.5.4 Syntax: MAIL FROM:<address>").await?;
                }
            }
            "RCPT" => {
                if env.from.is_none() {
                    reply(&mut io, "503 5.5.1 MAIL first").await?;
                } else if let Some(a) = addr_arg(&arg, "TO:") {
                    env.rcpt.push(a);
                    reply(&mut io, "250 2.1.5 OK").await?;
                } else {
                    reply(&mut io, "501 5.5.4 Syntax: RCPT TO:<address>").await?;
                }
            }
            "DATA" => {
                if env.rcpt.is_empty() {
                    reply(&mut io, "554 5.5.1 No valid recipients").await?;
                    continue;
                }
                reply(&mut io, "354 End data with <CR><LF>.<CR><LF>").await?;
                let mut data = Vec::new();
                let mut too_big = false;
                loop {
                    let mut buf = Vec::new();
                    let n = (&mut io)
                        .take(1024 * 1024)
                        .read_until(b'\n', &mut buf)
                        .await?;
                    if n == 0 {
                        return Ok(());
                    }
                    if buf == b".\r\n" || buf == b".\n" {
                        break;
                    }
                    let content = if buf.starts_with(b"..") {
                        &buf[1..]
                    } else {
                        &buf[..]
                    };
                    if data.len() + content.len() > MAX_SIZE {
                        too_big = true;
                    } else {
                        data.extend_from_slice(content);
                    }
                }
                let from = env.from.take().unwrap_or_default();
                let rcpt = std::mem::take(&mut env.rcpt);
                if too_big {
                    reply(&mut io, "552 5.3.4 Message size exceeds fixed limit").await?;
                    continue;
                }
                if let Some(resp) = shared.fail.take(".") {
                    reply(&mut io, &resp).await?;
                    continue;
                }
                if let Some(imap) = &shared.cfg.deliver_to {
                    for r in &rcpt {
                        // Unknown recipients are fine: a submission server relays them.
                        let _ = imap.inject(r, "INBOX", &data);
                    }
                }
                let n = {
                    let mut sent = lock(&shared.sent);
                    sent.push(SentMail {
                        from,
                        rcpt,
                        data,
                        auth_user: user.clone(),
                        tls,
                        at: now_secs(),
                    });
                    sent.len()
                };
                match shared.fail.take("AFTER_ACCEPT").as_deref() {
                    Some("disconnect") => return Ok(()),
                    Some("hold") => std::future::pending::<()>().await,
                    _ => {}
                }
                reply(&mut io, &format!("250 2.0.0 OK queued as FAKE{n:06}")).await?;
            }
            "RSET" => {
                env = Envelope {
                    from: None,
                    rcpt: Vec::new(),
                };
                reply(&mut io, "250 2.0.0 OK").await?;
            }
            "NOOP" => reply(&mut io, "250 2.0.0 OK").await?,
            "VRFY" => reply(&mut io, "252 2.1.5 Cannot VRFY user").await?,
            "QUIT" => {
                reply(&mut io, "221 2.0.0 Bye").await?;
                return Ok(());
            }
            _ => reply(&mut io, "502 5.5.2 Command not recognized").await?,
        }
    }
    Ok(())
}
