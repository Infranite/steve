//! Pairing, and the dial loop: the Node always dials, the Hub never reaches in.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use steve_node::LocalNode;
use steve_node_api::NodeId;
use steve_node_wire::handshake::{self, NodeIntro};
use steve_node_wire::identity::{self, Identity};
use steve_node_wire::tls::{self, Trust};
use steve_node_wire::{
    Conn, ConnOptions, HubLink, NodeServer, RemoteFiles, RemoteSecrets, RemoteSink, RemoteTools,
    close,
};

use crate::config::{Home, Pairing, loopback_plaintext_allowed, ws_url};
use crate::tunnel::Tunnel;
use steve_node_wire::tailcat;

pub fn trust(p: &Pairing) -> Result<Trust> {
    // the tunnel's local end: WireGuard to the key the address names stands where TLS would
    if tailcat::parse(&p.hub_url).is_some() {
        return Ok(Trust::LoopbackPlaintext);
    }
    let url = ws_url(&p.hub_url)?;
    tls::policy(
        &url,
        p.pin.as_deref(),
        p.loopback && loopback_plaintext_allowed(),
    )
    .map_err(|e| anyhow!(e))
}

/// The pairing request goes out through reqwest (the `url` crate) and the link through tungstenite
/// (`http::Uri`): both must read the same host out of `hub_url`, the one [`tls::policy`] judged.
fn same_host(hub_url: &str) -> Result<()> {
    let wire = tls::host_of(&ws_url(hub_url)?).map_err(|e| anyhow!(e))?;
    let parsed = reqwest::Url::parse(hub_url).context("the hub url does not parse")?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        bail!("the hub url must not contain userinfo or a fragment");
    }
    let http = parsed
        .host_str()
        .ok_or_else(|| anyhow!("the hub url has no host"))?;
    let http = http
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(http);
    let same = match (
        wire.parse::<std::net::IpAddr>(),
        http.parse::<std::net::IpAddr>(),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => wire.eq_ignore_ascii_case(http),
    };
    if !same {
        bail!("the hub url names two different hosts ({wire} and {http})");
    }
    Ok(())
}

fn http(trust: &Trust) -> Result<reqwest::Client> {
    let b = reqwest::Client::builder().timeout(Duration::from_secs(20));
    let b = match tls::client_config(trust).map_err(|e| anyhow!(e))? {
        Some(cfg) => b.use_preconfigured_tls((*cfg).clone()),
        None => b,
    };
    Ok(b.build()?)
}

/// How this machine reaches `hub_url` over HTTP: a client, the base to address, and the tunnel
/// (over tailcat) that must live as long as the requests.
async fn hub_http(
    hub_url: &str,
    pin: Option<&str>,
    plain_ok: bool,
) -> Result<(reqwest::Client, String, Option<Tunnel>)> {
    let tunnel = match tailcat::parse(hub_url) {
        Some((addr, port)) => Some(Tunnel::open(addr, port).await?),
        None => None,
    };
    let (trust, base) = match &tunnel {
        Some(t) => (Trust::LoopbackPlaintext, t.url.clone()),
        None => {
            let trust = tls::policy(&ws_url(hub_url)?, pin, plain_ok).map_err(|e| anyhow!(e))?;
            same_host(hub_url)?;
            (trust, hub_url.to_string())
        }
    };
    Ok((http(&trust)?, base, tunnel))
}

/// Replaces this Node's key (M4): the new key is made and kept aside, both keys sign the change
/// at the Hub, and only once the Hub took it does the new key replace the old one (the old one is
/// kept as `identity.key.previous`). The running Node is refused with its old key from then on and
/// must be restarted. Returns the new fingerprint.
pub async fn rotate(home: &Home) -> Result<String> {
    let p = home.pairing()?;
    let old = Identity::load(&home.identity()).context("identity key")?;
    let next_path = home.identity().with_extension("key.next");
    let _ = std::fs::remove_file(&next_path);
    let new = Identity::generate()?;
    new.save(&next_path)?;
    let msg = identity::rotate_message(&p.hub_id, &p.node_id, &old.public(), &new.public());
    let (client, base, _tunnel) = hub_http(
        &p.hub_url,
        p.pin.as_deref(),
        p.loopback && loopback_plaintext_allowed(),
    )
    .await?;
    let sent = client
        .post(format!("{base}/node/rotate"))
        .json(&json!({
            "node_id": p.node_id,
            "new_pubkey": hex::encode(new.public()),
            "sig_old": hex::encode(old.sign(&msg)),
            "sig_new": hex::encode(new.sign(&msg)),
        }))
        .send()
        .await
        .context("reaching the hub");
    let res = match sent {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let _ = std::fs::remove_file(&next_path);
            bail!("the hub refused the new key ({})", r.status());
        }
        Err(e) => {
            let _ = std::fs::remove_file(&next_path);
            return Err(e);
        }
    };
    let previous = home.identity().with_extension("key.previous");
    let _ = std::fs::remove_file(&previous);
    std::fs::rename(home.identity(), &previous).context("keeping the old key")?;
    std::fs::rename(&next_path, home.identity()).context("putting the new key in place")?;
    let v: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(v["fingerprint"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| new.fingerprint()))
}

/// Claims `code` at the Hub with a new (or the existing) key; the Node is pending until its owner
/// confirms the fingerprint this prints.
pub async fn pair(
    home: &Home,
    hub_url: &str,
    code: &str,
    pin: Option<String>,
    name: &str,
) -> Result<Pairing> {
    let hub_url = hub_url.trim_end_matches('/').to_string();
    let plain = hub_url.starts_with("http://");
    // over tailcat: the request goes to the tunnel's local end, the pairing keeps the address
    let (client, base, _tunnel) = hub_http(
        &hub_url,
        pin.as_deref(),
        plain && loopback_plaintext_allowed(),
    )
    .await?;
    let id = match Identity::load(&home.identity()) {
        Ok(id) => id,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Identity::generate()?;
            id.save(&home.identity())?;
            id
        }
        Err(e) => return Err(e).context("identity key"),
    };
    let pubkey = id.public();
    let sig = id.sign(&identity::pair_message(code, &pubkey));
    let res = client
        .post(format!("{base}/node/pair"))
        .json(&json!({
            "code": code,
            "pubkey": hex::encode(pubkey),
            "name": name,
            "sig": hex::encode(sig),
        }))
        .send()
        .await
        .context("reaching the hub")?;
    if !res.status().is_success() {
        bail!("the hub refused the pairing ({})", res.status());
    }
    let v: serde_json::Value = res.json().await?;
    let field = |k: &str| {
        v[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("the hub's answer has no {k}"))
    };
    let p = Pairing {
        hub_url,
        node_id: field("node_id")?,
        hub_id: field("hub_id")?,
        pin,
        loopback: plain,
        // a Hub of an earlier release has none: remembered from its first signed welcome then
        hub_key: v["hub_key"]
            .as_str()
            .filter(|k| key_of(k).is_some())
            .map(str::to_string),
        name: name.into(),
    };
    home.save_pairing(&p)?;
    Ok(p)
}

/// Opens this machine's LocalNode and keeps it linked to the Hub until refused.
pub async fn run(home: &Home) -> Result<()> {
    let p = home.pairing()?;
    let id = Identity::load(&home.identity()).context("identity key")?;
    let trust = trust(&p)?;
    let node_id = NodeId::new(p.node_id.clone()).map_err(|_| anyhow!("bad node id"))?;
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("installing the termination signal handler")?;
    let stopping = async {
        #[cfg(unix)]
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.context("waiting for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")
    };
    let node = LocalNode::open(home.node_config(node_id)?).map_err(|e| anyhow!("{e}"))?;
    run_until(
        &node,
        serve_at(node.clone(), Some(Home(home.0.clone())), &p, &id, &trust),
        stopping,
    )
    .await
}

/// [`run`] for a Node whose pairing and trust its host gives (the Hub's own Node in a process of
/// its own, B1): linked until Ctrl-C or SIGTERM, then shut down as a `steve-node` is. `home`: where
/// it notes why its last link failed ([`LINK_STATUS`]), for its Hub to show.
pub async fn serve_until_stopped(
    node: LocalNode,
    home: Option<&Path>,
    p: &Pairing,
    id: &Identity,
    trust: &Trust,
) -> Result<()> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("installing the termination signal handler")?;
    let stopping = async {
        #[cfg(unix)]
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.context("waiting for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")
    };
    let status = LinkStatus(home.map(|h| h.join(LINK_STATUS)));
    run_until(
        &node,
        serve_with(node.clone(), None, status, p, id, trust),
        stopping,
    )
    .await
}

fn key_of(hex_key: &str) -> Option<[u8; 32]> {
    hex::decode(hex_key).ok()?.try_into().ok()
}

/// Keeps the pins the Hub announced in its signed welcome (L4), when they changed.
fn remember_pins(home: &Home, pins: &str) {
    let saved = home.pairing().and_then(|mut p| {
        if p.pin.as_deref() != Some(pins) {
            p.pin = Some(pins.to_string());
            home.save_pairing(&p)?;
            tracing::info!("the hub's certificate pins are now {pins}");
        }
        Ok(())
    });
    if let Err(e) = saved {
        tracing::warn!("keeping the hub's pins: {e:#}");
    }
}

/// Keeps the Hub's identity key in the pairing, the first time a welcome proves one.
fn remember(home: &Home, key: [u8; 32]) {
    let saved = home.pairing().and_then(|mut p| {
        if p.hub_key.is_none() {
            p.hub_key = Some(hex::encode(key));
            home.save_pairing(&p)?;
            tracing::info!("remembered the hub's identity key; welcomes it did not sign are refused from now on");
        }
        Ok(())
    });
    if let Err(e) = saved {
        tracing::warn!("keeping the hub's identity key: {e:#}");
    }
}

/// Dropping the dial future first closes its connection and stops new commands. Shutdown then
/// joins owned work and records uncertain outcomes; an interrupted turn is never called finished.
async fn run_until(
    node: &LocalNode,
    serving: impl std::future::Future<Output = Result<()>>,
    stopping: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let r = tokio::select! {
        biased;
        r = stopping => r,
        r = serving => r,
    };
    node.shutdown().await;
    tracing::info!("node shutdown completed; unresolved outcomes remain in its journal");
    r
}

/// A cancelled dial future must not leave Conn's reader accepting requests in the background.
struct CloseLink(Arc<HubLink>);

impl Drop for CloseLink {
    fn drop(&mut self) {
        if let Some(conn) = self.0.current() {
            conn.close(None, close::GOODBYE, "node stopping");
        }
        self.0.set(None);
    }
}

/// The dial loop around an open LocalNode. Returns only when the Hub refuses this Node (revoked,
/// unknown, wrong Hub); every other failure is retried with backoff, the journal keeping what
/// happened meanwhile.
pub async fn serve(node: LocalNode, p: &Pairing, id: &Identity, trust: &Trust) -> Result<()> {
    serve_at(node, None, p, id, trust).await
}

/// [`serve`], keeping in `home`'s pairing the Hub key a first signed welcome proves, and in its
/// [`LINK_STATUS`] why the last link failed.
pub async fn serve_at(
    node: LocalNode,
    home: Option<Home>,
    p: &Pairing,
    id: &Identity,
    trust: &Trust,
) -> Result<()> {
    let status = LinkStatus(home.as_ref().map(|h| h.0.join(LINK_STATUS)));
    serve_with(node, home, status, p, id, trust).await
}

/// The file in a Node's home that says why its last link to the Hub failed,
/// `{"at": <ms>, "error": "…"}`; gone once it links. Its Hub shows its own Node's (a remote
/// Node's reason stays in that Node's log: it never reached the Hub).
pub const LINK_STATUS: &str = "link-status.json";

/// Why the Node in `home` could not link last, and when (ms): `None` once it linked.
pub fn link_status(home: &Path) -> Option<(i64, String)> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(LINK_STATUS)).ok()?).ok()?;
    Some((v["at"].as_i64()?, v["error"].as_str()?.to_string()))
}

/// Where the dial loop notes how linking went ([`LINK_STATUS`]); `None`: nowhere.
struct LinkStatus(Option<PathBuf>);

impl LinkStatus {
    fn failed(&self, why: &str) {
        let Some(path) = &self.0 else { return };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let body = json!({ "at": at, "error": why }).to_string();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
            tracing::warn!("noting the link failure in {}: {e}", path.display());
        }
    }

    fn linked(&self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn serve_with(
    node: LocalNode,
    home: Option<Home>,
    status: LinkStatus,
    p: &Pairing,
    id: &Identity,
    trust: &Trust,
) -> Result<()> {
    let known = std::sync::Mutex::new(p.hub_key.as_deref().and_then(key_of));
    // what the next link trusts: a pinned Hub may announce new pins (L4)
    let trust_now = std::sync::Mutex::new(trust.clone());
    let pins_now = std::sync::Mutex::new(p.pin.clone());
    let link = HubLink::new();
    let _close_link = CloseLink(link.clone());
    node.connect(
        Arc::new(RemoteSink(link.clone())),
        Some(Arc::new(RemoteFiles(link.clone()))),
    );
    node.connect_secrets(Arc::new(RemoteSecrets(link.clone())));
    node.connect_tools(Arc::new(RemoteTools(link.clone())))
        .await
        .context("tool endpoint")?;
    let over = tailcat::parse(&p.hub_url).map(|(a, port)| (a.to_string(), port));
    let fixed = match over {
        None => Some(ws_url(&p.hub_url)?),
        Some(_) => None,
    };
    let node_id = NodeId::new(p.node_id.clone()).map_err(|_| anyhow!("bad node id"))?;
    let server = NodeServer::new(Arc::new(node.clone()));
    let mut backoff = Duration::from_millis(200);
    loop {
        // over tailcat, a new tunnel for every link: one whose far end went away (a Hub restart)
        // can stay up and lead nowhere
        let mut tunnel = None;
        let url = match (&fixed, &over) {
            (Some(url), _) => Ok(url.clone()),
            (None, Some((addr, port))) => match Tunnel::open(addr, *port).await {
                Ok(t) => ws_url(&tunnel.insert(t).url),
                Err(e) => Err(e),
            },
            (None, None) => unreachable!(),
        };
        let linked = match url {
            Ok(url) => {
                let key = *known.lock().unwrap();
                let learnt = |k: [u8; 32]| {
                    *known.lock().unwrap() = Some(k);
                    if let Some(h) = &home {
                        remember(h, k);
                    }
                };
                let pins_learnt = |pins: Vec<String>| {
                    let joined = pins.join(",");
                    if pins_now.lock().unwrap().as_deref() == Some(joined.as_str()) {
                        return;
                    }
                    match tls::parse_pins(&joined) {
                        Ok(t) => {
                            *trust_now.lock().unwrap() = t;
                            *pins_now.lock().unwrap() = Some(joined.clone());
                            if let Some(h) = &home {
                                remember_pins(h, &joined);
                            }
                        }
                        Err(e) => tracing::warn!("the hub announced pins that do not parse: {e}"),
                    }
                };
                let hub = HubOf {
                    id: &p.hub_id,
                    key,
                    learnt: &learnt,
                    pinned: pins_now.lock().unwrap().is_some(),
                    pins_learnt: &pins_learnt,
                };
                let trust = trust_now.lock().unwrap().clone();
                let linked = || status.linked();
                link_once(
                    &node, &server, &link, &url, &node_id, hub, id, &trust, &linked,
                )
                .await
            }
            Err(e) => Err(Stop::Retry(format!("{e:#}"))),
        };
        drop(tunnel);
        match linked {
            Ok(()) => backoff = Duration::from_millis(200),
            Err(Stop::Refused(e)) => {
                link.set(None);
                status.failed(&format!("the hub refused this node: {e}"));
                bail!("the hub refused this node: {e}");
            }
            Err(Stop::Retry(e)) => {
                status.failed(&e);
                tracing::info!("hub link: {e}");
            }
            // dialling again alone will not mend it: say so where it is seen
            Err(Stop::Fault(e)) => {
                status.failed(&e);
                tracing::error!("hub link failed, trying again: {e}");
            }
        }
        link.set(None);
        // spread out: every Node of a restarted Hub dials again at once otherwise
        tokio::time::sleep(steve_node_wire::jitter(backoff)).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// Connecting and upgrading one link.
const CONNECT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq)]
enum Stop {
    Refused(String),
    /// The link dropped or the Hub was away: dialled again, quietly.
    Retry(String),
    /// Something on either side failed (`internal`: a hello over its limit, a Hub whose store
    /// failed; `invalid`): dialled again too, but logged as an error, since it may never pass.
    Fault(String),
}

/// Only a verdict ends the loop: `denied` (unknown, pending, revoked, another Hub) or no common
/// protocol. A Hub that could not decide (`internal`: its store failed, the handshake timed out)
/// or a link that dropped is dialled again: a healthy Node must not stop over a busy Hub.
fn stop_for(e: &handshake::Refused) -> Stop {
    if e.code == handshake::DENIED || e.code == "unsupported" {
        Stop::Refused(e.to_string())
    } else if e.code == "offline" {
        Stop::Retry(e.to_string())
    } else {
        Stop::Fault(e.to_string())
    }
}

/// The Hub this Node paired with: its id, its identity key when known, and what to do when a
/// welcome first proves one.
struct HubOf<'a> {
    id: &'a str,
    key: Option<[u8; 32]>,
    learnt: &'a (dyn Fn([u8; 32]) + Sync),
    /// This Node pinned the Hub's certificate: it takes the pins a signed welcome announces.
    pinned: bool,
    pins_learnt: &'a (dyn Fn(Vec<String>) + Sync),
}

#[allow(clippy::too_many_arguments)]
async fn link_once(
    node: &LocalNode,
    server: &Arc<NodeServer<LocalNode>>,
    link: &Arc<HubLink>,
    url: &str,
    node_id: &NodeId,
    hub: HubOf<'_>,
    id: &Identity,
    trust: &Trust,
    linked: &(dyn Fn() + Sync),
) -> Result<(), Stop> {
    // the TCP connect alone proves little (a tunnel's local end, a proxy): the upgrade is bounded too
    let (mut sink, mut stream) = tokio::time::timeout(CONNECT, tls::connect(url, trust))
        .await
        .map_err(|_| {
            Stop::Retry(format!(
                "no answer from the hub within {}s",
                CONNECT.as_secs()
            ))
        })?
        .map_err(|e| Stop::Retry(e.to_string()))?;
    let (_, welcome) = handshake::dial(
        &mut sink,
        &mut stream,
        NodeIntro {
            node: node_id,
            identity: id,
            incarnation: node.incarnation(),
            // The Hub obtains all stream heads from the paged post-link snapshot. Including
            // them here grows the fixed-size control hello with every historical stream and
            // eventually prevents the Node from linking at all.
            heads: Vec::new(),
            caps: None,
            hub_id: Some(hub.id),
            hub_key: hub.key,
        },
    )
    .await
    .map_err(|e| stop_for(&e))?;
    // checked by dial: a welcome's key holds; kept the first time (trust on first use)
    let signed = key_of(&welcome.hub_key);
    if hub.key.is_none()
        && let Some(k) = signed
    {
        (hub.learnt)(k);
    }
    if hub.pinned && signed.is_some() && !welcome.pins.is_empty() {
        (hub.pins_learnt)(welcome.pins.clone());
    }
    let conn = Conn::spawn(
        sink,
        stream,
        welcome.generation,
        server.clone(),
        ConnOptions {
            heartbeat: Duration::from_millis(welcome.heartbeat_ms.max(1000)),
            dead_after: Duration::from_millis(welcome.heartbeat_ms.max(1000) * 3),
            side: "n",
            peer: welcome.proto,
        },
    );
    tracing::info!(generation = welcome.generation, "linked to the hub");
    linked();
    link.set(Some(conn.clone()));
    let (code, reason) = conn.closed().await;
    link.set(None);
    tracing::info!(code, reason, "hub link closed");
    // a fenced link only means a newer one of ours took over; a revoked Node is refused at the
    // next hello
    if code == close::DENIED {
        return Err(Stop::Retry("revoked or refused".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(code: &str) -> handshake::Refused {
        handshake::Refused {
            code: code.into(),
            detail: String::new(),
        }
    }

    /// The service's stop path cancels a live connection, then lets another process open the
    /// same Node home. Dropping the serving future alone leaves LocalNode's owned tasks alive.
    #[tokio::test]
    async fn stopping_closes_the_connection_and_releases_the_node_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().into());
        let config = || {
            home.node_config(NodeId::new("shutdown-test").unwrap())
                .unwrap()
        };
        let node = LocalNode::open(config()).unwrap();
        let link = HubLink::new();
        let (wire, _peer) = steve_node_wire::testkit::pair();
        let conn = Conn::spawn(
            wire.sink,
            wire.stream,
            1,
            NodeServer::new(Arc::new(node.clone())),
            ConnOptions::default(),
        );
        link.set(Some(conn.clone()));
        let (started, waiting) = tokio::sync::oneshot::channel();
        let serving = {
            let link = link.clone();
            async move {
                let _close_link = CloseLink(link);
                started.send(()).unwrap();
                std::future::pending::<Result<()>>().await
            }
        };
        let stopping = async {
            waiting.await.unwrap();
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(5), run_until(&node, serving, stopping))
            .await
            .expect("stop must finish")
            .unwrap();
        assert!(link.current().is_none());
        assert!(conn.is_closed());
        drop(conn);
        drop(node);
        // The connection's reader releases its handler asynchronously after close.
        let again = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match LocalNode::open(config()) {
                    Ok(node) => break node,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .expect("the stopped service must release its home lock");
        again.shutdown().await;
    }

    #[tokio::test]
    async fn a_dial_failure_is_preserved_after_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().into());
        let config = || {
            home.node_config(NodeId::new("refusal-test").unwrap())
                .unwrap()
        };
        let node = LocalNode::open(config()).unwrap();
        let error = run_until(
            &node,
            async { bail!("the hub refused this node") },
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "the hub refused this node");
        drop(node);
        let again = LocalNode::open(config()).expect("failed dial must release its home lock");
        again.shutdown().await;
    }

    /// Why the last link failed is kept in the home until a link succeeds.
    #[test]
    fn the_last_link_failure_is_noted_until_it_links() {
        let dir = tempfile::tempdir().unwrap();
        let status = LinkStatus(Some(dir.path().join(LINK_STATUS)));
        assert_eq!(link_status(dir.path()), None);
        status.failed("internal: control message hello is 69214 bytes, over the 65536 limit");
        let (at, why) = link_status(dir.path()).unwrap();
        assert!(at > 0);
        assert!(why.contains("hello is 69214 bytes"), "{why}");
        status.linked();
        assert_eq!(link_status(dir.path()), None);
        LinkStatus(None).failed("nowhere");
    }

    #[test]
    fn only_a_verdict_stops_the_node() {
        assert!(matches!(stop_for(&r(handshake::DENIED)), Stop::Refused(_)));
        assert!(matches!(stop_for(&r("unsupported")), Stop::Refused(_)));
        assert!(matches!(stop_for(&r("offline")), Stop::Retry(_)));
        // a hello over its limit fails the same way every time: an error, still dialled again
        for code in [handshake::INTERNAL, "invalid"] {
            assert!(matches!(stop_for(&r(code)), Stop::Fault(_)), "{code}");
        }
    }

    /// M2: the pairing request and the link go to the same host, or pairing stops before either.
    #[test]
    fn pairing_reads_one_host() {
        for ok in [
            "http://127.0.0.1:7777",
            "http://[::1]:7777",
            "https://hub.example",
            "https://Hub.Example:8443/steve",
        ] {
            assert!(same_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://evil.example#@127.0.0.1:7777",
            "http://user@127.0.0.1:7777",
            "http://evil.example%40127.0.0.1",
            "https://hub.example/#frag",
            "http://127.0.0.1\\@evil.example",
        ] {
            assert!(same_host(bad).is_err(), "{bad}");
        }
    }
}
