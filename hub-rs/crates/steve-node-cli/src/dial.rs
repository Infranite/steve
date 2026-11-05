//! Pairing, and the dial loop: the Node always dials, the Hub never reaches in.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use steve_node::LocalNode;
use steve_node_api::{ExecutionControl, NodeId};
use steve_node_wire::handshake::{self, NodeIntro};
use steve_node_wire::identity::{self, Identity};
use steve_node_wire::tls::{self, Trust};
use steve_node_wire::{
    Conn, ConnOptions, HubLink, NodeServer, RemoteFiles, RemoteSink, RemoteTools, close,
};

use crate::config::{Home, Pairing, loopback_plaintext_allowed, ws_url};

pub fn trust(p: &Pairing) -> Result<Trust> {
    let url = ws_url(&p.hub_url)?;
    tls::policy(
        &url,
        p.pin.as_deref(),
        p.loopback && loopback_plaintext_allowed(),
    )
    .map_err(|e| anyhow!(e))
}

fn http(trust: &Trust) -> Result<reqwest::Client> {
    let b = reqwest::Client::builder().timeout(Duration::from_secs(20));
    let b = match tls::client_config(trust).map_err(|e| anyhow!(e))? {
        Some(cfg) => b.use_preconfigured_tls((*cfg).clone()),
        None => b,
    };
    Ok(b.build()?)
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
    let trust = tls::policy(
        &ws_url(&hub_url)?,
        pin.as_deref(),
        plain && loopback_plaintext_allowed(),
    )
    .map_err(|e| anyhow!(e))?;
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
    let res = http(&trust)?
        .post(format!("{hub_url}/node/pair"))
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
    let node = LocalNode::open(home.node_config(node_id)?).map_err(|e| anyhow!("{e}"))?;
    let r = serve(node.clone(), &p, &id, &trust).await;
    node.shutdown().await;
    r
}

/// The dial loop around an open LocalNode. Returns only when the Hub refuses this Node (revoked,
/// unknown, wrong Hub); every other failure is retried with backoff, the journal keeping what
/// happened meanwhile.
pub async fn serve(node: LocalNode, p: &Pairing, id: &Identity, trust: &Trust) -> Result<()> {
    let link = HubLink::new();
    node.connect(
        Arc::new(RemoteSink(link.clone())),
        Some(Arc::new(RemoteFiles(link.clone()))),
    );
    node.connect_tools(Arc::new(RemoteTools(link.clone())))
        .await
        .context("tool endpoint")?;
    let url = ws_url(&p.hub_url)?;
    let node_id = NodeId::new(p.node_id.clone()).map_err(|_| anyhow!("bad node id"))?;
    let server = NodeServer::new(Arc::new(node.clone()));
    let mut backoff = Duration::from_millis(200);
    loop {
        match link_once(&node, &server, &link, &url, &node_id, &p.hub_id, id, trust).await {
            Ok(()) => backoff = Duration::from_millis(200),
            Err(Stop::Refused(e)) => {
                link.set(None);
                bail!("the hub refused this node: {e}");
            }
            Err(Stop::Retry(e)) => tracing::info!("hub link: {e}"),
        }
        link.set(None);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

#[derive(Debug, PartialEq)]
enum Stop {
    Refused(String),
    Retry(String),
}

/// Only a verdict ends the loop: `denied` (unknown, pending, revoked, another Hub) or no common
/// protocol. A Hub that could not decide (`internal`: its store failed, the handshake timed out)
/// or a link that dropped is dialled again: a healthy Node must not stop over a busy Hub.
fn stop_for(e: &handshake::Refused) -> Stop {
    if e.code == handshake::DENIED || e.code == "unsupported" {
        Stop::Refused(e.to_string())
    } else {
        Stop::Retry(e.to_string())
    }
}

#[allow(clippy::too_many_arguments)]
async fn link_once(
    node: &LocalNode,
    server: &Arc<NodeServer<LocalNode>>,
    link: &Arc<HubLink>,
    url: &str,
    node_id: &NodeId,
    hub_id: &str,
    id: &Identity,
    trust: &Trust,
) -> Result<(), Stop> {
    let (mut sink, mut stream) = tls::connect(url, trust)
        .await
        .map_err(|e| Stop::Retry(e.to_string()))?;
    let heads = node.streams().await.unwrap_or_default();
    let (_, welcome) = handshake::dial(
        &mut sink,
        &mut stream,
        NodeIntro {
            node: node_id,
            identity: id,
            incarnation: node.incarnation(),
            heads,
            caps: None,
            hub_id: Some(hub_id),
        },
    )
    .await
    .map_err(|e| stop_for(&e))?;
    let conn = Conn::spawn(
        sink,
        stream,
        welcome.generation,
        server.clone(),
        ConnOptions {
            heartbeat: Duration::from_millis(welcome.heartbeat_ms.max(1000)),
            dead_after: Duration::from_millis(welcome.heartbeat_ms.max(1000) * 3),
            side: "n",
        },
    );
    tracing::info!(generation = welcome.generation, "linked to the hub");
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

    #[test]
    fn only_a_verdict_stops_the_node() {
        assert!(matches!(stop_for(&r(handshake::DENIED)), Stop::Refused(_)));
        assert!(matches!(stop_for(&r("unsupported")), Stop::Refused(_)));
        for code in [handshake::INTERNAL, "offline", "invalid"] {
            assert!(matches!(stop_for(&r(code)), Stop::Retry(_)), "{code}");
        }
    }
}
