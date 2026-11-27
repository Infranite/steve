//! One-shot HTTP/1.1 over UDS. Only a failed connect permits offline fallback.
#[path = "backup.rs"]
pub(crate) mod backup;
use crate::access::CliOutput;
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

fn segment(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
/// Queries `/admin/v1/storage/status` when a Hub is listening on the management socket.
/// Returns `Ok(None)` only when the socket cannot be connected (offline).
pub(crate) fn storage_status(home: &Path) -> Result<Option<super::routes::StorageStatus>> {
    let path = super::socket_path(home)?;
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    let (status, body) = exchange(stream, "GET", "/admin/v1/storage/status", &[])?;
    if !(200..300).contains(&status) {
        if let Ok(output) = serde_json::from_slice::<CliOutput>(&body) {
            let detail = output.stderr.trim();
            if !detail.is_empty() {
                anyhow::bail!("{detail}");
            }
        }
        anyhow::bail!("management storage status failed with HTTP {status}");
    }
    let info: super::routes::StorageStatus = serde_json::from_slice(&body)
        .context("invalid management storage status response; no offline retry was attempted")?;
    Ok(Some(info))
}

pub(crate) fn try_command(home: &Path, args: &[String]) -> Result<Option<CliOutput>> {
    let path = super::socket_path(home)?;
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    let json = args.iter().any(|v| v == "--json");
    let words: Vec<_> = args
        .iter()
        .map(String::as_str)
        .take_while(|a| !a.starts_with("--"))
        .collect();
    let (method, route, body) = match words.as_slice() {
        ["backup"] => {
            let reason = backup::reason(args)?;
            (
                "POST",
                "/admin/v1/backup".into(),
                serde_json::to_vec(&super::routes::BackupRequest {
                    reason: Some(reason),
                })?,
            )
        }
        ["token", "create"] => (
            "POST",
            "/admin/v1/access/tokens".into(),
            serde_json::to_vec(&super::routes::Create {
                args: args.to_vec(),
                bootstrap_user: Some(std::env::var("USER").unwrap_or_else(|_| "owner".into())),
            })?,
        ),
        ["token", "list"] => (
            "GET",
            format!("/admin/v1/access/tokens?json={json}"),
            vec![],
        ),
        ["client", "list"] => (
            "GET",
            format!("/admin/v1/access/clients?json={json}"),
            vec![],
        ),
        ["token", "revoke", id] => (
            "DELETE",
            format!("/admin/v1/access/tokens/{}", segment(id)),
            vec![],
        ),
        ["client", "revoke", id] => (
            "DELETE",
            format!("/admin/v1/access/clients/{}", segment(id)),
            vec![],
        ),
        _ => return Err(crate::access::usage()),
    };
    let (status, body) = exchange(stream, method, &route, &body)?;
    let output: CliOutput = serde_json::from_slice(&body)
        .context("invalid management response; no offline retry was attempted")?;
    ensure!(
        matches!(output.code, 0 | 2),
        "invalid management CLI exit code"
    );
    ensure!(
        (200..300).contains(&status) == (output.code == 0),
        "management HTTP status disagrees with command outcome"
    );
    Ok(Some(output))
}
pub(super) fn exchange(
    mut stream: UnixStream,
    method: &str,
    route: &str,
    body: &[u8],
) -> Result<(u16, Vec<u8>)> {
    stream.set_nonblocking(true)?;
    let started = Instant::now();
    let budget = if method == "POST" && route == "/admin/v1/backup" {
        crate::backup::BACKUP_BUDGET
    } else {
        super::DEADLINE
    } + Duration::from_secs(1);
    let remaining = || {
        budget.checked_sub(started.elapsed()).filter(|v| !v.is_zero())
        .context("management transport deadline exceeded; outcome may be unknown; no offline retry was attempted")
    };
    let mut request = format!("{method} {route} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    request.extend_from_slice(body);
    let mut pending = request.as_slice();
    while !pending.is_empty() {
        ready(&stream, libc::POLLOUT, remaining()?)?;
        let n = match stream.write(pending) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(n > 0, "management connection closed while writing");
        pending = &pending[n..];
    }
    // The CLI can list an arbitrarily large collection. The
    // endpoint is same-uid only; an absolute deadline bounds transport time, not output size.
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        ready(&stream, libc::POLLIN, remaining()?)?;
        let n = match stream.read(&mut buffer) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e).context(
                "management request failed; outcome may be unknown; no offline retry was attempted",
            ),
        };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    decode_response(&bytes)
}

fn decode_response(bytes: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = bytes
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .context("truncated management response")?;
    let headers = std::str::from_utf8(&bytes[..split])?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("missing management status")?
        .parse()?;
    let body = &bytes[split + 4..];
    if headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
    {
        let mut rest = body;
        let mut decoded = Vec::new();
        loop {
            let end = rest
                .windows(2)
                .position(|v| v == b"\r\n")
                .context("invalid HTTP chunk")?;
            let count = usize::from_str_radix(
                std::str::from_utf8(&rest[..end])?
                    .split(';')
                    .next()
                    .unwrap_or(""),
                16,
            )?;
            rest = &rest[end + 2..];
            if count == 0 {
                break;
            }
            ensure!(
                count <= rest.len().saturating_sub(2)
                    && rest.get(count..count + 2) == Some(b"\r\n"),
                "truncated HTTP chunk"
            );
            decoded.extend_from_slice(&rest[..count]);
            rest = &rest[count + 2..];
        }
        Ok((status, decoded))
    } else {
        if let Some(length) = headers
            .lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map(|(_, n)| n.trim().parse::<usize>())
            .transpose()?
        {
            ensure!(body.len() == length, "truncated management response body");
        }
        Ok((status, body.to_vec()))
    }
}

/// Polling also observes HUP/ERR, so buffered bytes are drained after a BSD peer closes.
/// Socket options cannot reliably be changed at that point. Every retry recomputes the
/// same absolute transport deadline rather than extending a per-read timeout.
fn ready(stream: &UnixStream, events: libc::c_short, timeout: Duration) -> Result<()> {
    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events,
        revents: 0,
    };
    let millis = timeout.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
    // SAFETY: fd points to one initialized pollfd, valid throughout this synchronous call.
    let count = unsafe { libc::poll(&mut fd, 1, millis) };
    ensure!(
        count != 0,
        "management transport deadline exceeded; outcome may be unknown; no offline retry was attempted"
    );
    if count < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    Ok(())
}
