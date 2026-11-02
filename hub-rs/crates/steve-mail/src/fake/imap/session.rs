//! One IMAP connection: read a command, run it against the shared state,
//! answer. Synchronous on the state (a std mutex never held across await).

use std::collections::BTreeSet;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::watch;

use super::parse::{
    SeqSet, Value, format_date_time, istr, mutf7_decode, mutf7_encode, parse_date, parse_date_time,
    tokenize, wildcard,
};
use super::render;
use super::{Folder, Msg, Shared, State};
use crate::fake::{BoxIo, TlsMode, lock, now_secs};

/// Commands larger than this are refused (the fake holds them in memory).
const MAX_COMMAND: usize = 64 * 1024 * 1024;

type Res<T> = Result<T, std::io::Error>;

struct Selected {
    folder: String,
    uidvalidity: u32,
    readonly: bool,
    /// What this session has been told: (uid, modseq) in sequence order.
    view: Vec<(u32, u64)>,
}

struct Session {
    shared: Arc<Shared>,
    tls: bool,
    user: Option<String>,
    id_sent: bool,
    condstore: bool,
    sel: Option<Selected>,
}

/// Outcome of one command.
enum Done {
    /// Tagged response text (`OK …`, `NO …`, `BAD …`).
    Tagged(String),
    /// Same, then close the connection.
    Close(String),
    /// `STARTTLS` accepted: upgrade the stream, no tagged reply pending.
    StartTls,
    /// `IDLE`: enter the idle loop.
    Idle,
    /// The selected folder's UIDVALIDITY changed under us.
    Bye,
}

fn ok(s: &str) -> Done {
    Done::Tagged(format!("OK {s}"))
}
fn no(s: &str) -> Done {
    Done::Tagged(format!("NO {s}"))
}
fn bad(s: &str) -> Done {
    Done::Tagged(format!("BAD {s}"))
}

pub(crate) async fn serve(
    shared: Arc<Shared>,
    sock: TcpStream,
    mut kill: watch::Receiver<u64>,
) -> Res<()> {
    // A connection only obeys kills issued after it was accepted.
    kill.borrow_and_update();
    let _ = sock.set_nodelay(true);
    let mut io: BufReader<BoxIo> = match shared.cfg.tls {
        TlsMode::Implicit => {
            let tls = shared.ca.acceptor().accept(sock).await?;
            BufReader::new(Box::new(tls))
        }
        _ => BufReader::new(Box::new(sock)),
    };
    let mut s = Session {
        tls: shared.cfg.tls == TlsMode::Implicit,
        shared,
        user: None,
        id_sent: false,
        condstore: false,
        sel: None,
    };
    let greeting = format!("* OK [CAPABILITY {}] Steve fake IMAP ready\r\n", s.caps());
    io.get_mut().write_all(greeting.as_bytes()).await?;
    io.get_mut().flush().await?;
    let mut changes = s.shared.changes.subscribe();

    loop {
        let line = tokio::select! {
            r = read_command(&mut io) => r?,
            _ = kill.changed() => return Ok(()),
        };
        let Some(line) = line else { return Ok(()) };
        let mut out = Vec::new();
        let (tag, done) = s.dispatch(&line, &mut io, &mut out).await?;
        match done {
            Done::Tagged(t) => {
                out.extend(format!("{tag} {t}\r\n").into_bytes());
                write(&mut io, &out).await?;
            }
            Done::Close(t) => {
                out.extend(format!("{tag} {t}\r\n").into_bytes());
                write(&mut io, &out).await?;
                return Ok(());
            }
            Done::Bye => {
                out.extend_from_slice(b"* BYE [UNAVAILABLE] mailbox UIDVALIDITY changed\r\n");
                write(&mut io, &out).await?;
                return Ok(());
            }
            Done::StartTls => {
                out.extend(format!("{tag} OK Begin TLS negotiation now\r\n").into_bytes());
                write(&mut io, &out).await?;
                if !io.buffer().is_empty() {
                    // Pipelined plaintext after STARTTLS is an injection attempt.
                    return Ok(());
                }
                let tls = s.shared.ca.acceptor().accept(io.into_inner()).await?;
                io = BufReader::new(Box::new(tls));
                s.tls = true;
            }
            Done::Idle => {
                write(&mut io, b"+ idling\r\n").await?;
                changes.borrow_and_update();
                // Report anything that happened before IDLE started.
                let mut pending = Vec::new();
                if s.sync(&mut pending, true, true).is_err() {
                    write(
                        &mut io,
                        b"* BYE [UNAVAILABLE] mailbox UIDVALIDITY changed\r\n",
                    )
                    .await?;
                    return Ok(());
                }
                write(&mut io, &pending).await?;
                // Persistent buffer: read_until is cancel-safe only with one.
                let mut l = Vec::new();
                loop {
                    tokio::select! {
                        r = io.read_until(b'\n', &mut l) => {
                            if r? == 0 { return Ok(()) }
                            if !l.ends_with(b"\n") { continue }
                            let text = String::from_utf8_lossy(&l);
                            if text.trim().eq_ignore_ascii_case("DONE") {
                                write(&mut io, format!("{tag} OK IDLE terminated\r\n").as_bytes()).await?;
                            } else {
                                write(&mut io, format!("{tag} BAD expected DONE\r\n").as_bytes()).await?;
                            }
                            break;
                        }
                        _ = changes.changed() => {
                            let mut out = Vec::new();
                            if s.sync(&mut out, true, true).is_err() {
                                write(&mut io, b"* BYE [UNAVAILABLE] mailbox UIDVALIDITY changed\r\n").await?;
                                return Ok(());
                            }
                            write(&mut io, &out).await?;
                        }
                        _ = kill.changed() => return Ok(()),
                    }
                }
            }
        }
    }
}

async fn write(io: &mut BufReader<BoxIo>, bytes: &[u8]) -> Res<()> {
    io.get_mut().write_all(bytes).await?;
    io.get_mut().flush().await
}

async fn read_line(io: &mut BufReader<BoxIo>) -> Res<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let n = (&mut *io)
        .take(MAX_COMMAND as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

/// Ends with `{n}` / `{n+}` before CRLF → `(n, synchronizing)`.
fn literal_suffix(line: &[u8]) -> Option<(usize, bool)> {
    let l = line
        .strip_suffix(b"\r\n")
        .or_else(|| line.strip_suffix(b"\n"))?;
    let l = l.strip_suffix(b"}")?;
    let open = l.iter().rposition(|&c| c == b'{')?;
    let spec = std::str::from_utf8(&l[open + 1..]).ok()?;
    let (num, plus) = match spec.strip_suffix('+') {
        Some(n) => (n, true),
        None => (spec, false),
    };
    Some((num.parse().ok()?, !plus))
}

/// Reads one full command, answering `+` to synchronizing literals.
async fn read_command(io: &mut BufReader<BoxIo>) -> Res<Option<Vec<u8>>> {
    let Some(mut buf) = read_line(io).await? else {
        return Ok(None);
    };
    while let Some((n, sync)) = literal_suffix(&buf) {
        if buf.len() + n > MAX_COMMAND {
            return Err(std::io::Error::other("command too large"));
        }
        if sync {
            write(io, b"+ Ready for literal data\r\n").await?;
        }
        let start = buf.len();
        buf.resize(start + n, 0);
        io.read_exact(&mut buf[start..]).await?;
        match read_line(io).await? {
            Some(rest) => buf.extend(rest),
            None => return Ok(None),
        }
    }
    Ok(Some(buf))
}

/// The command as logged: arguments of LOGIN/AUTHENTICATE masked, literal
/// bodies replaced by their size.
fn log_line(tag: &str, name: &str, args: &[u8]) -> String {
    if name == "LOGIN" || name.starts_with("AUTHENTICATE") {
        return format!("{tag} {name} ***");
    }
    let mut text = String::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == b'{'
            && let Some(close) = args[i..].iter().position(|&c| c == b'}')
        {
            let spec = String::from_utf8_lossy(&args[i + 1..i + close]).into_owned();
            if let Ok(n) = spec.trim_end_matches('+').parse::<usize>() {
                text.push_str(&format!("{{{spec}}}<{n} bytes>"));
                let mut j = i + close + 1;
                while matches!(args.get(j), Some(b'\r') | Some(b'\n')) {
                    j += 1;
                }
                i = j + n;
                continue;
            }
        }
        text.push(args[i] as char);
        i += 1;
    }
    format!("{tag} {name} {}", text.trim_end())
        .trim_end()
        .to_string()
}

fn flags_list(flags: &BTreeSet<String>) -> String {
    format!("({})", flags.iter().cloned().collect::<Vec<_>>().join(" "))
}

fn date_days(ts: i64) -> i64 {
    ts.div_euclid(86400)
}

impl Session {
    fn caps(&self) -> String {
        let cfg = &self.shared.cfg;
        let mut c = vec!["IMAP4rev1"];
        if cfg.tls == TlsMode::StartTls && !self.tls {
            c.extend(["STARTTLS", "LOGINDISABLED"]);
        } else {
            c.extend(["AUTH=PLAIN", "SASL-IR"]);
        }
        c.extend([
            "ID",
            "IDLE",
            "UIDPLUS",
            "LITERAL+",
            "MOVE",
            "SPECIAL-USE",
            "NAMESPACE",
            "ENABLE",
            "UNSELECT",
        ]);
        if cfg.condstore {
            c.push("CONDSTORE");
        }
        if cfg.gmail {
            c.push("X-GM-EXT-1");
        }
        c.join(" ")
    }

    async fn dispatch(
        &mut self,
        line: &[u8],
        io: &mut BufReader<BoxIo>,
        out: &mut Vec<u8>,
    ) -> Res<(String, Done)> {
        let trimmed = line
            .strip_suffix(b"\r\n")
            .or_else(|| line.strip_suffix(b"\n"))
            .unwrap_or(line);
        let mut parts = trimmed.splitn(3, |&c| c == b' ');
        let tag = String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
        let mut name =
            String::from_utf8_lossy(parts.next().unwrap_or_default()).to_ascii_uppercase();
        let mut args: &[u8] = parts.next().unwrap_or_default();
        if tag.is_empty() || name.is_empty() {
            return Ok(("*".into(), bad("empty command")));
        }
        let uid = name == "UID";
        if uid {
            let mut p = args.splitn(2, |&c| c == b' ');
            name = format!(
                "UID {}",
                String::from_utf8_lossy(p.next().unwrap_or_default()).to_ascii_uppercase()
            );
            args = p.next().unwrap_or_default();
        }
        self.shared.record(log_line(&tag, &name, args));
        let pause = {
            let mut pauses = lock(&self.shared.pauses);
            pauses
                .iter()
                .position(|(command, _, _)| command == &name)
                .map(|i| pauses.remove(i))
        };
        if let Some((_, entered, release)) = pause {
            let _ = entered.send(());
            let _ = release.await;
        }
        if let Some(resp) = self.shared.fail.take(&name) {
            if resp == "HOLD" {
                // Read until the bounded client abandons this exchange. This models a hung
                // command without keeping the fake task alive after the socket closes.
                let _ = io.read_u8().await;
            }
            return Ok((tag, Done::Tagged(resp)));
        }
        let tokens = match tokenize(args) {
            Ok(t) => t,
            Err(e) => return Ok((tag, bad(&format!("parse error: {e}")))),
        };
        // Announce what changed since the last command first, so a FETCH
        // can reference new messages (they must be EXISTS-announced before).
        // Non-UID FETCH/STORE/SEARCH must not cause EXPUNGE (RFC 3501 §7.4.1).
        let allow_expunge = !matches!(name.as_str(), "FETCH" | "STORE" | "SEARCH");
        let syncs = self.sel.is_some()
            && !matches!(name.as_str(), "SELECT" | "EXAMINE" | "IDLE" | "LOGOUT");
        if syncs && self.sync(out, allow_expunge, false).is_err() {
            return Ok((tag, Done::Bye));
        }
        let done = match name.as_str() {
            "CAPABILITY" => {
                out.extend(format!("* CAPABILITY {}\r\n", self.caps()).into_bytes());
                ok("CAPABILITY completed")
            }
            "NOOP" | "CHECK" => {
                if self.sync(out, true, true).is_err() {
                    return Ok((tag, Done::Bye));
                }
                ok(&format!("{name} completed"))
            }
            "LOGOUT" => {
                out.extend_from_slice(b"* BYE Steve fake IMAP logging out\r\n");
                Done::Close("OK LOGOUT completed".into())
            }
            "ID" => {
                self.id_sent = true;
                out.extend_from_slice(
                    b"* ID (\"name\" \"Steve fake IMAP\" \"vendor\" \"steve\")\r\n",
                );
                ok("ID completed")
            }
            "STARTTLS" => {
                if self.shared.cfg.tls != TlsMode::StartTls || self.tls {
                    bad("STARTTLS not available")
                } else {
                    Done::StartTls
                }
            }
            "LOGIN" => self.login(&tokens),
            "AUTHENTICATE" => self.authenticate(&tokens, io).await?,
            _ if self.user.is_none() => bad("not authenticated"),
            "ENABLE" => {
                let want: Vec<String> = tokens.iter().filter_map(Value::atom_upper).collect();
                if self.shared.cfg.condstore && want.iter().any(|w| w == "CONDSTORE") {
                    self.condstore = true;
                    out.extend_from_slice(b"* ENABLED CONDSTORE\r\n");
                } else {
                    out.extend_from_slice(b"* ENABLED\r\n");
                }
                ok("ENABLE completed")
            }
            "NAMESPACE" => {
                out.extend_from_slice(b"* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n");
                ok("NAMESPACE completed")
            }
            "LIST" | "LSUB" => self.list(&name, &tokens, out),
            "STATUS" => self.status(&tokens, out),
            "CREATE" => self.create(&tokens),
            "DELETE" => self.delete(&tokens),
            "SUBSCRIBE" | "UNSUBSCRIBE" => ok(&format!("{name} completed")),
            "SELECT" | "EXAMINE" => self.select(name == "EXAMINE", &tokens, out),
            "APPEND" => self.append(&tokens),
            "IDLE" => Done::Idle,
            _ if self.sel.is_none() => bad("no mailbox selected"),
            "CLOSE" => {
                if !self.sel.as_ref().is_some_and(|s| s.readonly) {
                    self.expunge(None, &mut Vec::new());
                }
                self.sel = None;
                ok("CLOSE completed")
            }
            "UNSELECT" => {
                self.sel = None;
                ok("UNSELECT completed")
            }
            "EXPUNGE" => {
                self.expunge(None, out);
                ok("EXPUNGE completed")
            }
            "UID EXPUNGE" => match tokens
                .first()
                .and_then(Value::text)
                .and_then(|s| SeqSet::parse(&s))
            {
                Some(set) => {
                    self.expunge(Some(&set), out);
                    ok("UID EXPUNGE completed")
                }
                None => bad("UID EXPUNGE needs a UID set"),
            },
            "SEARCH" | "UID SEARCH" => self.search(uid, &tokens, out),
            "FETCH" | "UID FETCH" => self.fetch(uid, &tokens, out),
            "STORE" | "UID STORE" => self.store(uid, &tokens, out),
            "COPY" | "UID COPY" => self.copy(uid, false, &tokens, out),
            "MOVE" | "UID MOVE" => self.copy(uid, true, &tokens, out),
            _ => bad(&format!("unknown command {name}")),
        };
        // And what the command itself changed (e.g. MOVE's EXPUNGEs).
        if let Done::Tagged(_) = &done
            && self.sel.is_some()
            && syncs
            && self.sync(out, allow_expunge, true).is_err()
        {
            return Ok((tag, Done::Bye));
        }
        Ok((tag, done))
    }

    fn check_login(&mut self, user: &str, pass: &str) -> Done {
        if self.shared.cfg.tls == TlsMode::StartTls && !self.tls {
            return no("[PRIVACYREQUIRED] Use STARTTLS first");
        }
        let good = lock(&self.shared.state)
            .accounts
            .get(user)
            .is_some_and(|a| a.password == pass);
        if good {
            self.user = Some(user.to_string());
            *lock(&self.shared.logins) += 1;
            ok(&format!("[CAPABILITY {}] Logged in", self.caps()))
        } else {
            no("[AUTHENTICATIONFAILED] Invalid credentials (Failure)")
        }
    }

    fn login(&mut self, t: &[Value]) -> Done {
        match (
            t.first().and_then(Value::text),
            t.get(1).and_then(Value::text),
        ) {
            (Some(u), Some(p)) => self.check_login(&u, &p),
            _ => bad("LOGIN needs user and password"),
        }
    }

    async fn authenticate(&mut self, t: &[Value], io: &mut BufReader<BoxIo>) -> Res<Done> {
        let mech = t.first().and_then(Value::atom_upper).unwrap_or_default();
        if mech != "PLAIN" {
            return Ok(no("[CANNOT] only PLAIN is supported"));
        }
        let resp = match t.get(1).and_then(Value::text) {
            Some(r) => r,
            None => {
                write(io, b"+ \r\n").await?;
                let Some(l) = read_line(io).await? else {
                    return Ok(bad("connection closed"));
                };
                String::from_utf8_lossy(&l).trim().to_string()
            }
        };
        if resp == "*" {
            return Ok(bad("AUTHENTICATE cancelled"));
        }
        let decoded = if resp == "=" {
            Vec::new()
        } else {
            match B64.decode(resp.as_bytes()) {
                Ok(d) => d,
                Err(_) => return Ok(bad("invalid base64")),
            }
        };
        let fields: Vec<&[u8]> = decoded.split(|&c| c == 0).collect();
        if fields.len() != 3 {
            return Ok(bad("invalid PLAIN response"));
        }
        let user = String::from_utf8_lossy(fields[1]).into_owned();
        let pass = String::from_utf8_lossy(fields[2]).into_owned();
        Ok(self.check_login(&user, &pass))
    }

    fn user(&self) -> String {
        self.user.clone().unwrap_or_default()
    }

    fn list(&self, cmd: &str, t: &[Value], out: &mut Vec<u8>) -> Done {
        // Skip RFC 5258 selection options, e.g. LIST (SPECIAL-USE) "" "*".
        let t: Vec<&Value> = t
            .iter()
            .skip_while(|v| matches!(v, Value::List(_)))
            .collect();
        let (Some(reference), Some(pattern)) = (
            t.first().and_then(|v| v.text()),
            t.get(1).and_then(|v| v.text()),
        ) else {
            return bad("LIST needs reference and pattern");
        };
        let pattern = mutf7_decode(&format!("{reference}{pattern}"));
        if pattern.is_empty() {
            out.extend_from_slice(format!("* {cmd} (\\Noselect) \"/\" \"\"\r\n").as_bytes());
            return ok(&format!("{cmd} completed"));
        }
        let s = lock(&self.shared.state);
        let Some(a) = s.accounts.get(&self.user()) else {
            return no("no account");
        };
        for f in &a.folders {
            let matches = if f.name.eq_ignore_ascii_case("INBOX") {
                wildcard(&pattern.to_ascii_uppercase(), "INBOX")
            } else {
                wildcard(&pattern, &f.name)
            };
            if !matches {
                continue;
            }
            let prefix = format!("{}/", f.name);
            let children = a.folders.iter().any(|c| c.name.starts_with(&prefix));
            let mut attrs = vec![if children {
                "\\HasChildren"
            } else {
                "\\HasNoChildren"
            }];
            if f.noselect {
                attrs.push("\\Noselect");
            }
            if let Some(su) = &f.special_use {
                attrs.push(su);
            }
            out.extend(format!("* {cmd} ({}) \"/\" ", attrs.join(" ")).into_bytes());
            out.extend(istr(mutf7_encode(&f.name).as_bytes()));
            out.extend_from_slice(b"\r\n");
        }
        ok(&format!("{cmd} completed"))
    }

    fn folder_name(t: &[Value]) -> Option<String> {
        t.first().and_then(Value::text).map(|n| mutf7_decode(&n))
    }

    fn status(&self, t: &[Value], out: &mut Vec<u8>) -> Done {
        let Some(name) = Self::folder_name(t) else {
            return bad("STATUS needs a mailbox");
        };
        let Some(Value::List(items)) = t.get(1) else {
            return bad("STATUS needs items");
        };
        let s = lock(&self.shared.state);
        let Some(f) = s
            .accounts
            .get(&self.user())
            .and_then(|a| a.folder(&name))
            .filter(|f| !f.noselect)
        else {
            return no("[NONEXISTENT] Unknown Mailbox");
        };
        let mut fields = Vec::new();
        for it in items.iter().filter_map(Value::atom_upper) {
            let v = match it.as_str() {
                "MESSAGES" => f.messages.len() as u64,
                "RECENT" => 0,
                "UIDNEXT" => f.uidnext as u64,
                "UIDVALIDITY" => f.uidvalidity as u64,
                "UNSEEN" => f
                    .messages
                    .iter()
                    .filter(|m| !m.flags.contains("\\Seen"))
                    .count() as u64,
                "HIGHESTMODSEQ" if self.shared.cfg.condstore => highest_modseq(f),
                _ => return bad(&format!("unknown STATUS item {it}")),
            };
            fields.push(format!("{it} {v}"));
        }
        out.extend_from_slice(b"* STATUS ");
        out.extend(istr(mutf7_encode(&f.name).as_bytes()));
        out.extend(format!(" ({})\r\n", fields.join(" ")).into_bytes());
        ok("STATUS completed")
    }

    fn create(&self, t: &[Value]) -> Done {
        let Some(name) = Self::folder_name(t) else {
            return bad("CREATE needs a mailbox");
        };
        let mut s = lock(&self.shared.state);
        let uidvalidity = s.new_uidvalidity();
        let Some(a) = s.accounts.get_mut(&self.user()) else {
            return no("no account");
        };
        if a.folder(&name).is_some() {
            return no("[ALREADYEXISTS] Mailbox exists");
        }
        a.folders.push(Folder {
            name,
            special_use: None,
            noselect: false,
            uidvalidity,
            uidnext: 1,
            messages: Vec::new(),
        });
        drop(s);
        self.shared.changed();
        ok("CREATE completed")
    }

    fn delete(&self, t: &[Value]) -> Done {
        let Some(name) = Self::folder_name(t) else {
            return bad("DELETE needs a mailbox");
        };
        if name.eq_ignore_ascii_case("INBOX") {
            return no("[CANNOT] INBOX cannot be deleted");
        }
        let mut s = lock(&self.shared.state);
        let Some(a) = s.accounts.get_mut(&self.user()) else {
            return no("no account");
        };
        let before = a.folders.len();
        a.folders.retain(|f| f.name != name);
        if a.folders.len() == before {
            return no("[NONEXISTENT] Unknown Mailbox");
        }
        drop(s);
        self.shared.changed();
        ok("DELETE completed")
    }

    fn select(&mut self, readonly: bool, t: &[Value], out: &mut Vec<u8>) -> Done {
        self.sel = None;
        if self.shared.cfg.require_id && !self.id_sent {
            return no("SELECT Unsafe Login. Please contact kefu@188.com for help");
        }
        let Some(name) = Self::folder_name(t) else {
            return bad("SELECT needs a mailbox");
        };
        if let Some(Value::List(opts)) = t.get(1)
            && opts
                .iter()
                .any(|o| o.atom_upper().as_deref() == Some("CONDSTORE"))
            && self.shared.cfg.condstore
        {
            self.condstore = true;
        }
        let s = lock(&self.shared.state);
        let Some(f) = s
            .accounts
            .get(&self.user())
            .and_then(|a| a.folder(&name))
            .filter(|f| !f.noselect)
        else {
            return no("[NONEXISTENT] Unknown Mailbox");
        };
        out.extend_from_slice(b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n");
        out.extend_from_slice(
            b"* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted.\r\n",
        );
        out.extend(format!("* {} EXISTS\r\n* 0 RECENT\r\n", f.messages.len()).into_bytes());
        if let Some(i) = f.messages.iter().position(|m| !m.flags.contains("\\Seen")) {
            out.extend(format!("* OK [UNSEEN {}] First unseen.\r\n", i + 1).into_bytes());
        }
        out.extend(format!("* OK [UIDVALIDITY {}] UIDs valid.\r\n", f.uidvalidity).into_bytes());
        out.extend(format!("* OK [UIDNEXT {}] Predicted next UID.\r\n", f.uidnext).into_bytes());
        if self.shared.cfg.condstore {
            out.extend(
                format!("* OK [HIGHESTMODSEQ {}] Highest\r\n", highest_modseq(f)).into_bytes(),
            );
        }
        self.sel = Some(Selected {
            folder: f.name.clone(),
            uidvalidity: f.uidvalidity,
            readonly,
            view: f.messages.iter().map(|m| (m.uid, m.modseq)).collect(),
        });
        let mode = if readonly { "READ-ONLY" } else { "READ-WRITE" };
        ok(&format!(
            "[{mode}] {} completed",
            if readonly { "EXAMINE" } else { "SELECT" }
        ))
    }

    fn append(&self, t: &[Value]) -> Done {
        let Some(name) = Self::folder_name(t) else {
            return bad("APPEND needs a mailbox");
        };
        let mut flags = BTreeSet::new();
        let mut date = None;
        let mut msg = None;
        for v in &t[1..] {
            match v {
                Value::List(fl) => flags = fl.iter().filter_map(Value::text).collect(),
                Value::Str(s) if msg.is_none() && date.is_none() && t.last() != Some(v) => {
                    date = parse_date_time(&String::from_utf8_lossy(s));
                }
                Value::Str(s) => msg = Some(s.clone()),
                Value::Atom(_) => return bad("unexpected APPEND argument"),
            }
        }
        let Some(msg) = msg else {
            return bad("APPEND needs a message literal");
        };
        let user = self.user();
        let r = lock(&self.shared.state).add_message(
            &user,
            &name,
            Arc::new(msg),
            flags,
            date.unwrap_or_else(now_secs),
            None,
        );
        match r {
            Ok((v, uid)) => {
                self.shared.changed();
                ok(&format!("[APPENDUID {v} {uid}] APPEND completed"))
            }
            Err(_) => no("[TRYCREATE] Unknown Mailbox"),
        }
    }

    /// Sends the untagged updates the session has not seen yet. Errors when
    /// the folder is gone or its UIDVALIDITY changed.
    fn sync(&mut self, out: &mut Vec<u8>, allow_expunge: bool, flags: bool) -> Result<(), ()> {
        let condstore = self.condstore;
        let user = self.user();
        let Some(sel) = self.sel.as_mut() else {
            return Ok(());
        };
        let s = lock(&self.shared.state);
        let f = s
            .accounts
            .get(&user)
            .and_then(|a| a.folder(&sel.folder))
            .ok_or(())?;
        if f.uidvalidity != sel.uidvalidity {
            return Err(());
        }
        if allow_expunge {
            let mut i = sel.view.len();
            while i > 0 {
                i -= 1;
                let uid = sel.view[i].0;
                if !f.messages.iter().any(|m| m.uid == uid) {
                    sel.view.remove(i);
                    out.extend(format!("* {} EXPUNGE\r\n", i + 1).into_bytes());
                }
            }
        }
        for (i, (uid, modseq)) in sel.view.iter_mut().enumerate() {
            if !flags {
                break;
            }
            if let Some(m) = f.messages.iter().find(|m| m.uid == *uid)
                && m.modseq != *modseq
            {
                *modseq = m.modseq;
                let ms = if condstore {
                    format!(" MODSEQ ({})", m.modseq)
                } else {
                    String::new()
                };
                out.extend(
                    format!(
                        "* {} FETCH (UID {} FLAGS {}{ms})\r\n",
                        i + 1,
                        m.uid,
                        flags_list(&m.flags)
                    )
                    .into_bytes(),
                );
            }
        }
        let max = sel.view.iter().map(|v| v.0).max().unwrap_or(0);
        let new: Vec<(u32, u64)> = f
            .messages
            .iter()
            .filter(|m| m.uid > max)
            .map(|m| (m.uid, m.modseq))
            .collect();
        if !new.is_empty() {
            sel.view.extend(new);
            out.extend(format!("* {} EXISTS\r\n", sel.view.len()).into_bytes());
        }
        Ok(())
    }

    /// `(seq, message)` of the view entries matched by `set`.
    fn matched<'s>(&self, s: &'s State, uid: bool, set: &SeqSet) -> Vec<(usize, &'s Msg)> {
        let Some(sel) = &self.sel else {
            return Vec::new();
        };
        let Some(f) = s
            .accounts
            .get(&self.user())
            .and_then(|a| a.folder(&sel.folder))
        else {
            return Vec::new();
        };
        let max_uid = sel.view.iter().map(|v| v.0).max().unwrap_or(0);
        let max_seq = sel.view.len() as u32;
        sel.view
            .iter()
            .enumerate()
            .filter(|(i, (u, _))| {
                if uid {
                    set.contains(*u, max_uid)
                } else {
                    set.contains(*i as u32 + 1, max_seq)
                }
            })
            .filter_map(|(i, (u, _))| f.messages.iter().find(|m| m.uid == *u).map(|m| (i + 1, m)))
            .collect()
    }

    fn search(&self, uid: bool, t: &[Value], out: &mut Vec<u8>) -> Done {
        let mut t = t;
        if t.first().and_then(Value::atom_upper).as_deref() == Some("CHARSET") {
            t = t.get(2..).unwrap_or_default();
        }
        let keys = match parse_search(&mut t.iter().peekable()) {
            Ok(k) => k,
            Err(e) => return bad(&format!("bad search: {e}")),
        };
        let s = lock(&self.shared.state);
        let all = SeqSet::all();
        let msgs = self.matched(&s, false, &all);
        let max_uid = msgs.iter().map(|(_, m)| m.uid).max().unwrap_or(0);
        let max_seq = msgs.len() as u32;
        let hits: Vec<String> = msgs
            .iter()
            .filter(|(seq, m)| {
                keys.iter()
                    .all(|k| k.matches(*seq as u32, m, max_seq, max_uid))
            })
            .map(|(seq, m)| if uid { m.uid } else { *seq as u32 }.to_string())
            .collect();
        out.extend(
            format!(
                "* SEARCH{}{}\r\n",
                if hits.is_empty() { "" } else { " " },
                hits.join(" ")
            )
            .into_bytes(),
        );
        ok("SEARCH completed")
    }

    fn fetch(&mut self, uid: bool, t: &[Value], out: &mut Vec<u8>) -> Done {
        let Some(set) = t
            .first()
            .and_then(Value::text)
            .and_then(|s| SeqSet::parse(&s))
        else {
            return bad("FETCH needs a sequence set");
        };
        let mut items: Vec<String> = match t.get(1) {
            Some(Value::List(l)) => l.iter().filter_map(Value::text).collect(),
            Some(v) => v.text().into_iter().collect(),
            None => return bad("FETCH needs items"),
        };
        let mut changed_since = None;
        if let Some(Value::List(m)) = t.get(2) {
            let m: Vec<String> = m.iter().filter_map(Value::text).collect();
            if m.first()
                .is_some_and(|x| x.eq_ignore_ascii_case("CHANGEDSINCE"))
            {
                if !self.shared.cfg.condstore {
                    return bad("CONDSTORE not supported");
                }
                changed_since = m.get(1).and_then(|n| n.parse::<u64>().ok());
                self.condstore = true;
            }
        }
        // Macros.
        let mut expanded = Vec::new();
        for it in items.drain(..) {
            match it.to_ascii_uppercase().as_str() {
                "ALL" => expanded
                    .extend(["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE"].map(String::from)),
                "FAST" => {
                    expanded.extend(["FLAGS", "INTERNALDATE", "RFC822.SIZE"].map(String::from))
                }
                "FULL" => expanded.extend(
                    ["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE", "BODY"].map(String::from),
                ),
                _ => expanded.push(it),
            }
        }
        let mut items = expanded;
        if uid && !items.iter().any(|i| i.eq_ignore_ascii_case("UID")) {
            items.insert(0, "UID".into());
        }
        if self.condstore && !items.iter().any(|i| i.eq_ignore_ascii_case("MODSEQ")) {
            items.push("MODSEQ".into());
        }

        let readonly = self.sel.as_ref().is_some_and(|s| s.readonly);
        let mut s = lock(&self.shared.state);
        let targets: Vec<(usize, u32)> = self
            .matched(&s, uid, &set)
            .into_iter()
            .filter(|(_, m)| changed_since.is_none_or(|c| m.modseq > c))
            .map(|(seq, m)| (seq, m.uid))
            .collect();
        let folder = self
            .sel
            .as_ref()
            .map(|s| s.folder.clone())
            .unwrap_or_default();
        let user = self.user();
        let mut changed = false;
        for (seq, msg_uid) in targets {
            // Non-PEEK body fetches set \Seen first, so FLAGS in the same response shows it.
            let sets_seen = !readonly
                && items.iter().any(|i| {
                    let u = i.to_ascii_uppercase();
                    u.starts_with("BODY[") || u == "RFC822" || u == "RFC822.TEXT"
                });
            if sets_seen {
                let has_seen = s
                    .accounts
                    .get(&user)
                    .and_then(|a| a.folder(&folder))
                    .and_then(|f| f.messages.iter().find(|m| m.uid == msg_uid))
                    .is_some_and(|m| m.flags.contains("\\Seen"));
                if !has_seen {
                    let ms = s.bump_modseq();
                    if let Some(m) = s
                        .accounts
                        .get_mut(&user)
                        .and_then(|a| a.folder_mut(&folder))
                        .and_then(|f| f.messages.iter_mut().find(|m| m.uid == msg_uid))
                    {
                        m.flags.insert("\\Seen".into());
                        m.modseq = ms;
                        changed = true;
                    }
                }
            }
            let Some(m) = s
                .accounts
                .get(&user)
                .and_then(|a| a.folder(&folder))
                .and_then(|f| f.messages.iter().find(|m| m.uid == msg_uid))
            else {
                continue;
            };
            if let Some(sel) = self.sel.as_mut()
                && let Some(v) = sel.view.iter_mut().find(|v| v.0 == msg_uid)
            {
                v.1 = m.modseq;
            }
            let mut parts: Vec<Vec<u8>> = Vec::new();
            for it in &items {
                match fetch_item(it, m) {
                    Some(p) => parts.push(p),
                    None => return bad(&format!("unknown FETCH item {it}")),
                }
            }
            out.extend(format!("* {seq} FETCH (").into_bytes());
            out.extend(parts.join(&b' '));
            out.extend_from_slice(b")\r\n");
        }
        drop(s);
        if changed {
            self.shared.changed();
        }
        ok("FETCH completed")
    }

    fn store(&mut self, uid: bool, t: &[Value], out: &mut Vec<u8>) -> Done {
        if self.sel.as_ref().is_some_and(|s| s.readonly) {
            return no("[READ-ONLY] mailbox is read-only");
        }
        let Some(set) = t
            .first()
            .and_then(Value::text)
            .and_then(|s| SeqSet::parse(&s))
        else {
            return bad("STORE needs a sequence set");
        };
        let mut rest = &t[1..];
        let mut unchanged_since = None;
        if let Some(Value::List(m)) = rest.first() {
            let m: Vec<String> = m.iter().filter_map(Value::text).collect();
            if m.first()
                .is_some_and(|x| x.eq_ignore_ascii_case("UNCHANGEDSINCE"))
            {
                unchanged_since = m.get(1).and_then(|n| n.parse::<u64>().ok());
            }
            rest = &rest[1..];
        }
        let Some(op) = rest.first().and_then(Value::atom_upper) else {
            return bad("STORE needs an operation");
        };
        let flags: BTreeSet<String> = match rest.get(1) {
            Some(Value::List(l)) => l.iter().filter_map(Value::text).collect(),
            Some(_) => rest[1..].iter().filter_map(Value::text).collect(),
            None => BTreeSet::new(),
        };
        let flags: BTreeSet<String> = flags.into_iter().filter(|f| f != "\\Recent").collect();
        let silent = op.ends_with(".SILENT");
        let op = op.trim_end_matches(".SILENT").to_string();
        if !matches!(op.as_str(), "FLAGS" | "+FLAGS" | "-FLAGS") {
            return bad("STORE operation must be FLAGS, +FLAGS or -FLAGS");
        }
        let user = self.user();
        let folder = self
            .sel
            .as_ref()
            .map(|s| s.folder.clone())
            .unwrap_or_default();
        let mut s = lock(&self.shared.state);
        let targets: Vec<(usize, u32, u64)> = self
            .matched(&s, uid, &set)
            .into_iter()
            .map(|(seq, m)| (seq, m.uid, m.modseq))
            .collect();
        let mut failed = Vec::new();
        let mut changed = false;
        for (seq, msg_uid, modseq) in targets {
            if unchanged_since.is_some_and(|u| modseq > u) {
                failed.push(if uid { msg_uid } else { seq as u32 });
                continue;
            }
            let ms = s.bump_modseq();
            let Some(m) = s
                .accounts
                .get_mut(&user)
                .and_then(|a| a.folder_mut(&folder))
                .and_then(|f| f.messages.iter_mut().find(|m| m.uid == msg_uid))
            else {
                continue;
            };
            let before = m.flags.clone();
            match op.as_str() {
                "FLAGS" => m.flags = flags.clone(),
                "+FLAGS" => m.flags.extend(flags.iter().cloned()),
                _ => m.flags.retain(|f| !flags.contains(f)),
            }
            if m.flags != before {
                m.modseq = ms;
                changed = true;
            }
            if let Some(sel) = self.sel.as_mut()
                && let Some(v) = sel.view.iter_mut().find(|v| v.0 == msg_uid)
            {
                v.1 = m.modseq;
            }
            if !silent {
                let mut items = Vec::new();
                if uid {
                    items.push(format!("UID {}", m.uid));
                }
                items.push(format!("FLAGS {}", flags_list(&m.flags)));
                if self.condstore {
                    items.push(format!("MODSEQ ({})", m.modseq));
                }
                out.extend(format!("* {seq} FETCH ({})\r\n", items.join(" ")).into_bytes());
            }
        }
        drop(s);
        if changed {
            self.shared.changed();
        }
        if failed.is_empty() {
            ok("STORE completed")
        } else {
            ok(&format!(
                "[MODIFIED {}] Conditional STORE failed",
                SeqSet::format(&failed)
            ))
        }
    }

    fn copy(&mut self, uid: bool, mv: bool, t: &[Value], out: &mut Vec<u8>) -> Done {
        let Some(set) = t
            .first()
            .and_then(Value::text)
            .and_then(|s| SeqSet::parse(&s))
        else {
            return bad("COPY needs a sequence set");
        };
        let Some(dest) = t.get(1).and_then(Value::text).map(|n| mutf7_decode(&n)) else {
            return bad("COPY needs a mailbox");
        };
        if mv && self.sel.as_ref().is_some_and(|s| s.readonly) {
            return no("[READ-ONLY] mailbox is read-only");
        }
        let user = self.user();
        let folder = self
            .sel
            .as_ref()
            .map(|s| s.folder.clone())
            .unwrap_or_default();
        let mut s = lock(&self.shared.state);
        let picked: Vec<Msg> = self
            .matched(&s, uid, &set)
            .into_iter()
            .map(|(_, m)| m.clone())
            .collect();
        let Some(dest_validity) = s
            .accounts
            .get(&user)
            .and_then(|a| a.folder(&dest))
            .filter(|f| !f.noselect)
            .map(|f| f.uidvalidity)
        else {
            return no("[TRYCREATE] Unknown Mailbox");
        };
        let mut src_uids = Vec::new();
        let mut dst_uids = Vec::new();
        for m in &picked {
            let flags = m.flags.clone();
            if let Ok((_, new_uid)) = s.add_message(
                &user,
                &dest,
                m.raw.clone(),
                flags,
                m.internal_date,
                Some((m.gm_msgid, m.gm_thrid)),
            ) {
                src_uids.push(m.uid);
                dst_uids.push(new_uid);
            }
        }
        let code = format!(
            "COPYUID {dest_validity} {} {}",
            SeqSet::format(&src_uids),
            SeqSet::format(&dst_uids)
        );
        if mv {
            if let Some(f) = s
                .accounts
                .get_mut(&user)
                .and_then(|a| a.folder_mut(&folder))
            {
                f.messages.retain(|m| !src_uids.contains(&m.uid));
            }
            drop(s);
            out.extend(format!("* OK [{code}] Moved\r\n").into_bytes());
            // EXPUNGEs for the moved messages come from sync() in dispatch.
            self.shared.changed();
            ok("MOVE completed")
        } else {
            drop(s);
            self.shared.changed();
            ok(&format!("[{code}] COPY completed"))
        }
    }

    fn expunge(&mut self, only: Option<&SeqSet>, out: &mut Vec<u8>) {
        let user = self.user();
        let Some(sel) = &self.sel else { return };
        if sel.readonly {
            return;
        }
        let folder = sel.folder.clone();
        let max_uid = sel.view.iter().map(|v| v.0).max().unwrap_or(0);
        {
            let mut s = lock(&self.shared.state);
            let Some(f) = s
                .accounts
                .get_mut(&user)
                .and_then(|a| a.folder_mut(&folder))
            else {
                return;
            };
            f.messages.retain(|m| {
                !(m.flags.contains("\\Deleted")
                    && only.is_none_or(|set| set.contains(m.uid, max_uid)))
            });
        }
        self.shared.changed();
        let _ = self.sync(out, true, true);
    }
}

fn highest_modseq(f: &Folder) -> u64 {
    f.messages.iter().map(|m| m.modseq).max().unwrap_or(1)
}

/// Renders one FETCH data item, `None` if unknown.
fn fetch_item(item: &str, m: &Msg) -> Option<Vec<u8>> {
    let upper = item.to_ascii_uppercase();
    let simple = |s: String| Some(s.into_bytes());
    match upper.as_str() {
        "UID" => return simple(format!("UID {}", m.uid)),
        "FLAGS" => return simple(format!("FLAGS {}", flags_list(&m.flags))),
        "INTERNALDATE" => {
            return simple(format!(
                "INTERNALDATE \"{}\"",
                format_date_time(m.internal_date)
            ));
        }
        "RFC822.SIZE" => return simple(format!("RFC822.SIZE {}", m.raw.len())),
        "MODSEQ" => return simple(format!("MODSEQ ({})", m.modseq)),
        "X-GM-THRID" => return simple(format!("X-GM-THRID {}", m.gm_thrid)),
        "X-GM-MSGID" => return simple(format!("X-GM-MSGID {}", m.gm_msgid)),
        "X-GM-LABELS" => return simple("X-GM-LABELS ()".into()),
        "ENVELOPE" => {
            let mut v = b"ENVELOPE ".to_vec();
            v.extend(render::envelope(&m.meta));
            return Some(v);
        }
        "BODYSTRUCTURE" | "BODY" => {
            let mut v = format!("{upper} ").into_bytes();
            v.extend(render::body_structure(&m.raw, upper == "BODYSTRUCTURE"));
            return Some(v);
        }
        "RFC822" | "RFC822.HEADER" | "RFC822.TEXT" => {
            let spec = match upper.as_str() {
                "RFC822" => "",
                "RFC822.HEADER" => "HEADER",
                _ => "TEXT",
            };
            let data = render::section(&m.raw, spec)?;
            let mut v = format!("{upper} ").into_bytes();
            v.extend(literal(&data));
            return Some(v);
        }
        _ => {}
    }
    // BODY[section]<partial> / BODY.PEEK[section]<partial>
    let rest = upper
        .strip_prefix("BODY.PEEK[")
        .or_else(|| upper.strip_prefix("BODY["))?;
    let close = rest.rfind(']')?;
    let spec_upper = &rest[..close];
    // Echo the section as the client wrote it (case preserved for field names).
    let orig_start = item.find('[')? + 1;
    let spec_orig = &item[orig_start..orig_start + spec_upper.len()];
    let partial = &rest[close + 1..];
    let mut data = render::section(&m.raw, spec_orig)?;
    let mut name = format!("BODY[{spec_orig}]");
    if let Some(p) = partial.strip_prefix('<').and_then(|p| p.strip_suffix('>')) {
        let (start, len) = p.split_once('.')?;
        let start: usize = start.parse().ok()?;
        let len: usize = len.parse().ok()?;
        let from = start.min(data.len());
        let to = from.saturating_add(len).min(data.len());
        data = data[from..to].to_vec();
        name.push_str(&format!("<{start}>"));
    }
    let mut v = name.into_bytes();
    v.push(b' ');
    v.extend(literal(&data));
    Some(v)
}

fn literal(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{{{}}}\r\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v
}

/// Search keys (RFC 3501 §6.4.4 subset, plus `MODSEQ`, `X-GM-THRID`).
enum Key {
    All,
    Seq(SeqSet),
    Uid(SeqSet),
    Flag(String, bool),
    Since(i64),
    Before(i64),
    On(i64),
    SentSince(i64),
    SentBefore(i64),
    Header(String, String),
    Body(String),
    Text(String),
    Larger(usize),
    Smaller(usize),
    ModSeq(u64),
    GmThrid(u64),
    Not(Box<Key>),
    Or(Box<Key>, Box<Key>),
    And(Vec<Key>),
}

type Tokens<'a> = std::iter::Peekable<std::slice::Iter<'a, Value>>;

fn parse_search(t: &mut Tokens<'_>) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    while t.peek().is_some() {
        keys.push(parse_key(t)?);
    }
    if keys.is_empty() {
        return Err("no search keys".into());
    }
    Ok(keys)
}

fn parse_key(t: &mut Tokens<'_>) -> Result<Key, String> {
    let v = t.next().ok_or("missing key")?;
    if let Value::List(l) = v {
        return Ok(Key::And(parse_search(&mut l.iter().peekable())?));
    }
    let word = v.text().ok_or("bad key")?;
    let mut arg = || -> Result<String, String> {
        t.next()
            .and_then(Value::text)
            .ok_or_else(|| format!("{word} needs an argument"))
    };
    let date = |s: String| parse_date(&s).ok_or_else(|| format!("bad date {s}"));
    let flag = |f: &str, on: bool| Ok(Key::Flag(f.into(), on));
    match word.to_ascii_uppercase().as_str() {
        "ALL" => Ok(Key::All),
        "SEEN" => flag("\\Seen", true),
        "UNSEEN" => flag("\\Seen", false),
        "FLAGGED" => flag("\\Flagged", true),
        "UNFLAGGED" => flag("\\Flagged", false),
        "ANSWERED" => flag("\\Answered", true),
        "UNANSWERED" => flag("\\Answered", false),
        "DELETED" => flag("\\Deleted", true),
        "UNDELETED" => flag("\\Deleted", false),
        "DRAFT" => flag("\\Draft", true),
        "UNDRAFT" => flag("\\Draft", false),
        "NEW" | "RECENT" => Ok(Key::Flag("\\Recent".into(), true)),
        "OLD" => Ok(Key::All),
        "KEYWORD" => Ok(Key::Flag(arg()?, true)),
        "UNKEYWORD" => Ok(Key::Flag(arg()?, false)),
        "SINCE" => Ok(Key::Since(date(arg()?)?)),
        "BEFORE" => Ok(Key::Before(date(arg()?)?)),
        "ON" => Ok(Key::On(date(arg()?)?)),
        "SENTSINCE" => Ok(Key::SentSince(date(arg()?)?)),
        "SENTBEFORE" => Ok(Key::SentBefore(date(arg()?)?)),
        "FROM" | "TO" | "CC" | "BCC" | "SUBJECT" => {
            Ok(Key::Header(word.to_ascii_uppercase(), arg()?))
        }
        "HEADER" => {
            let name = arg()?;
            Ok(Key::Header(name.to_ascii_uppercase(), arg()?))
        }
        "BODY" => Ok(Key::Body(arg()?)),
        "TEXT" => Ok(Key::Text(arg()?)),
        "LARGER" => Ok(Key::Larger(arg()?.parse().map_err(|_| "bad size")?)),
        "SMALLER" => Ok(Key::Smaller(arg()?.parse().map_err(|_| "bad size")?)),
        "MODSEQ" => Ok(Key::ModSeq(arg()?.parse().map_err(|_| "bad modseq")?)),
        "X-GM-THRID" => Ok(Key::GmThrid(arg()?.parse().map_err(|_| "bad thrid")?)),
        "UID" => Ok(Key::Uid(SeqSet::parse(&arg()?).ok_or("bad uid set")?)),
        "NOT" => Ok(Key::Not(Box::new(parse_key(t)?))),
        "OR" => {
            let a = parse_key(t)?;
            let b = parse_key(t)?;
            Ok(Key::Or(Box::new(a), Box::new(b)))
        }
        _ => SeqSet::parse(&word)
            .map(Key::Seq)
            .ok_or_else(|| format!("unknown search key {word}")),
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

impl Key {
    fn matches(&self, seq: u32, m: &Msg, max_seq: u32, max_uid: u32) -> bool {
        let day = date_days(m.internal_date);
        let sent_day = m.meta.date.map(date_days);
        let fmt = |l: &[(String, String)]| {
            l.iter()
                .map(|(n, a)| format!("{n} <{a}>"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Key::All => true,
            Key::Seq(s) => s.contains(seq, max_seq),
            Key::Uid(s) => s.contains(m.uid, max_uid),
            Key::Flag(f, on) => m.flags.iter().any(|x| x.eq_ignore_ascii_case(f)) == *on,
            Key::Since(d) => day >= *d,
            Key::Before(d) => day < *d,
            Key::On(d) => day == *d,
            Key::SentSince(d) => sent_day.is_some_and(|s| s >= *d),
            Key::SentBefore(d) => sent_day.is_some_and(|s| s < *d),
            Key::Header(name, value) => {
                let hay = match name.as_str() {
                    "FROM" => fmt(&m.meta.from.iter().cloned().collect::<Vec<_>>()),
                    "TO" => fmt(&m.meta.to),
                    "CC" => fmt(&m.meta.cc),
                    "BCC" => fmt(&m.meta.bcc),
                    "SUBJECT" => m.meta.subject.clone(),
                    other => {
                        let raw = render::section(&m.raw, &format!("HEADER.FIELDS ({other})"))
                            .unwrap_or_default();
                        let text = String::from_utf8_lossy(&raw).into_owned();
                        if text.trim().is_empty() {
                            return false;
                        }
                        text
                    }
                };
                contains_ci(&hay, value)
            }
            Key::Body(s) => {
                let body = render::section(&m.raw, "TEXT").unwrap_or_default();
                contains_ci(&String::from_utf8_lossy(&body), s) || contains_ci(&m.meta.snippet, s)
            }
            Key::Text(s) => {
                contains_ci(&String::from_utf8_lossy(&m.raw), s)
                    || contains_ci(&m.meta.subject, s)
                    || contains_ci(&m.meta.snippet, s)
            }
            Key::Larger(n) => m.raw.len() > *n,
            Key::Smaller(n) => m.raw.len() < *n,
            Key::ModSeq(n) => m.modseq >= *n,
            Key::GmThrid(id) => m.gm_thrid == *id,
            Key::Not(k) => !k.matches(seq, m, max_seq, max_uid),
            Key::Or(a, b) => {
                a.matches(seq, m, max_seq, max_uid) || b.matches(seq, m, max_seq, max_uid)
            }
            Key::And(ks) => ks.iter().all(|k| k.matches(seq, m, max_seq, max_uid)),
        }
    }
}
