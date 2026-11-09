//! The local end of a Hub reached over tailcat (`tailcat:<address>:<port>`, see
//! [`steve_node_wire::tailcat`]): `tailcat forward <address> 0:<port>` on loopback, owned by this
//! process and killed with it.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use steve_node_wire::tailcat;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long the program may take to reach the relay and report its listener.
const READY: Duration = Duration::from_secs(60);

pub struct Tunnel {
    child: Child,
    /// `http://127.0.0.1:<port>`: the Hub as this machine reaches it.
    pub url: String,
}

impl Tunnel {
    pub async fn open(addr: &str, port: u16) -> Result<Tunnel> {
        let program = tailcat::program().ok_or_else(|| {
            anyhow!("this Hub is reached over tailcat, which is not installed here (or set STEVE_TAILCAT)")
        })?;
        let mut child = Command::new(program)
            .args(["forward", addr, &format!("0:{port}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting tailcat")?;
        let mut lines = BufReader::new(child.stderr.take().context("tailcat stderr")?).lines();
        let local = tokio::time::timeout(READY, async {
            while let Some(line) = lines.next_line().await? {
                if let Some(a) = tailcat::forwarding_in(&line) {
                    return Ok(a);
                }
                tracing::debug!("tailcat: {line}");
            }
            bail!("tailcat exited before forwarding")
        })
        .await
        .map_err(|_| {
            anyhow!(
                "tailcat did not start forwarding within {}s",
                READY.as_secs()
            )
        })??;
        // keep draining, or a full pipe would stall it
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("tailcat: {line}");
            }
        });
        if !local.ip().is_loopback() {
            bail!("tailcat forwards from {local}, not loopback");
        }
        Ok(Tunnel {
            child,
            url: format!("http://{local}"),
        })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
