//! Published skill snapshots. A Node's desired cache converges independently of session opens.
//! Only current, authorized payloads are reconstructed for delivery, never queued offline.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{StreamExt, stream};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::caps::skills as repo;
use steve_store::{Store, StoreError, now};

type Result<T> = steve_store::Result<T>;

fn store_error(e: impl ToString) -> StoreError {
    StoreError::Conflict(e.to_string())
}

pub(super) fn for_session(c: &Connection, session: &str) -> Result<Option<SkillSnapshot>> {
    let (ws, bot): (String, Option<String>) = c.query_row(
        "SELECT workspace_id,bot_id FROM sessions WHERE id=?",
        [session],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    match bot {
        Some(bot) => {
            let skills = snapshot(c, &ws, &bot, true)?;
            Ok((!skills.entries.is_empty()).then_some(skills))
        }
        None => Ok(None),
    }
}

fn snapshot(c: &Connection, ws: &str, bot: &str, take: bool) -> Result<SkillSnapshot> {
    let entries = if take {
        repo::published_for_bot(c, ws, bot)?
    } else {
        vec![]
    };
    SkillSnapshot::new(
        ws.into(),
        bot.into(),
        entries
            .into_iter()
            .map(|s| VersionedSkill {
                id: s.id,
                version: s.version.version,
                publication: s.publication,
                body: Skill {
                    name: s.version.name,
                    description: s.version.description,
                    content: s.version.content,
                },
            })
            .collect(),
    )
    .map_err(store_error)
}

pub(super) fn receipt_in(
    c: &Connection,
    session: &str,
    node: &str,
    receipt: &SkillUseReceipt,
) -> Result<()> {
    let expected: Option<(String, String)> = c.query_row(
        "SELECT skill_snapshot_digest,skill_expected FROM runtime_sessions WHERE session_id=? AND node_id=? AND skill_snapshot_digest IS NOT NULL",
        [session, node], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let Some((digest, refs)) = expected else {
        return Ok(());
    };
    let refs = serde_json::from_str::<Vec<SkillVersionRef>>(&refs)?;
    let mut facts = receipt
        .materialized
        .iter()
        .chain(&receipt.conflicts)
        .cloned()
        .collect::<Vec<_>>();
    facts.sort_by(|a, b| a.id.cmp(&b.id));
    if digest == receipt.digest.as_str() && facts == refs {
        c.execute(
            "UPDATE runtime_sessions SET skill_use_receipt=? WHERE session_id=? AND node_id=?",
            params![serde_json::to_string(receipt)?, session, node],
        )?;
    }
    Ok(())
}

pub(super) fn supports(features: &[String], adapter: &str) -> bool {
    features.iter().any(|f| f == FEATURE_MANAGED_SKILLS)
        && features
            .iter()
            .any(|f| f.strip_prefix(SKILLS_ADAPTER_PREFIX) == Some(adapter))
}

#[derive(Clone)]
struct Peer {
    node: Arc<dyn Node>,
    id: String,
    link: String,
    remote: Option<(Arc<steve_node_wire::RemoteNode>, u64)>,
}

impl Peer {
    async fn sync(&self, target: SyncSkills) -> ApiResult<SkillCacheReceipt> {
        match &self.remote {
            Some((node, generation)) => node.sync_skills_on(*generation, target).await,
            None => self.node.sync_skills(target).await,
        }
    }
}

pub(crate) fn local_link() -> String {
    static LOCAL: OnceLock<String> = OnceLock::new();
    LOCAL.get_or_init(|| steve_store::uid("local")).clone()
}

pub(crate) fn remote_link(generation: u64) -> String {
    format!("remote-{generation}")
}

pub(super) async fn run(
    store: Store,
    local: Option<Arc<dyn Node>>,
    service: Option<Arc<crate::nodes::NodeService>>,
    principal_state: bool,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(4));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut prune_at = 0;
    loop {
        tick.tick().await;
        if now() >= prune_at {
            let store = store.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = store.transaction::<_, StoreError>(|c| repo::prune(c, now())) {
                    tracing::warn!(%e, "skill history retention");
                }
            })
            .await;
            prune_at = now() + 60 * 60 * 1000;
        }
        let mut peers = Vec::new();
        if let Some(local) = &local {
            peers.push(Peer {
                node: local.clone(),
                id: "local".into(),
                link: local_link(),
                remote: None,
            });
        }
        if let Some(svc) = &service {
            peers.extend(svc.registry.linked().into_iter().filter_map(|id| {
                if id == "local" && local.is_some() {
                    return None;
                }
                let node = NodeId::new(id.clone()).ok()?;
                let remote = svc.fleet.remote(&node);
                let generation = remote.generation()?;
                Some(Peer {
                    node: remote.clone(),
                    id,
                    link: remote_link(generation),
                    remote: Some((remote, generation)),
                })
            }));
        }
        stream::iter(peers)
            .for_each_concurrent(8, |peer| {
                let store = store.clone();
                let service = service.clone();
                async move {
                    if let Err(e) =
                        reconcile(&store, peer, service.as_deref(), principal_state).await
                    {
                        tracing::debug!(%e, "skill cache reconciliation");
                    }
                }
            })
            .await;
    }
}

async fn reconcile(
    store: &Store,
    peer: Peer,
    service: Option<&crate::nodes::NodeService>,
    principal_state: bool,
) -> Result<()> {
    let previous: Option<(String, String, i64)> = store.reader::<_, StoreError>(|c| {
        Ok(c.query_row(
            "SELECT link,features,seen FROM node_skill_inventory WHERE node_id=?",
            [&peer.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
    })?;
    let features: Vec<String> = match previous {
        Some((link, features, seen)) if link == peer.link && now() - seen < 60_000 => {
            serde_json::from_str(&features)?
        }
        _ => {
            let Ok(Ok(caps)) =
                tokio::time::timeout(Duration::from_secs(8), peer.node.probe(false)).await
            else {
                return Ok(());
            };
            if caps.node.as_str() != peer.id {
                return Ok(());
            }
            let mut features = caps.features;
            if !principal_state {
                features.retain(|f| !f.starts_with(SKILLS_ADAPTER_PREFIX));
            }
            store.transaction::<_, StoreError>(|tx| {
                tx.execute("INSERT INTO node_skill_inventory(node_id,link,features,seen) VALUES(?,?,?,?)
                   ON CONFLICT(node_id) DO UPDATE SET link=excluded.link, features=excluded.features, seen=excluded.seen",
                   params![peer.id, peer.link, serde_json::to_string(&features)?, now()])?;
                Ok(())
            })?;
            features
        }
    };
    if !features.iter().any(|f| f == FEATURE_MANAGED_SKILLS) {
        return Ok(());
    }
    let keys: BTreeSet<(String, String)> = store.reader::<_, StoreError>(|c| {
        let mut q = c.prepare(
            "SELECT s.workspace_id,a.bot_id FROM bot_capabilities a JOIN skills s ON s.id=a.entry_id
              WHERE a.kind='skill' AND a.enabled=1 AND s.published_version IS NOT NULL
             UNION SELECT workspace_id,bot_id FROM node_skill_targets WHERE node_id=?",
        )?;
        Ok(q.query_map([&peer.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    })?;
    for (ws, bot) in keys {
        // Protect link identity against pause / revoke / replacement at send. The Node independently
        // fences generations; the payload is selected from current authorization and publication.
        if !current_link(&peer, service) {
            break;
        }
        let target =
            store.transaction(|c| plan(c, &peer, &ws, &bot, &features, principal_state))?;
        let Some(target) = target else {
            continue;
        };
        if !current_link(&peer, service) {
            break;
        }
        let ack = tokio::time::timeout(Duration::from_secs(6), peer.sync(target.clone())).await;
        if !current_link(&peer, service) {
            break;
        }
        let accepted = matches!(&ack, Ok(Ok(r)) if r.generation == target.generation && r.digest == target.snapshot.digest);
        let error = match &ack {
            Ok(Err(e)) => Some(format!("{:?}", e.code).to_lowercase()),
            Err(_) => Some("timeout".into()),
            Ok(Ok(_)) if !accepted => Some("receipt_mismatch".into()),
            _ => None,
        };
        store.transaction::<_, StoreError>(|c| {
            if !cache_access(c, &peer.id, &ws)? { return Ok(()); }
            if accepted {
                c.execute("UPDATE node_skill_targets SET acknowledged=digest, acknowledged_generation=generation,
                             acknowledged_at=?1, acknowledged_link=?2, versions=?3, error=NULL
                           WHERE workspace_id=?4 AND bot_id=?5 AND node_id=?6 AND generation=?7 AND digest=?8",
                    params![now(), peer.link, serde_json::to_string(&target.snapshot.refs())?,
                         ws, bot, peer.id, target.generation, target.snapshot.digest.as_str()])?;
            } else {
                c.execute("UPDATE node_skill_targets SET error=?1
                           WHERE workspace_id=?2 AND bot_id=?3 AND node_id=?4 AND generation=?5",
                    params![error, ws, bot, peer.id, target.generation])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn current_link(peer: &Peer, service: Option<&crate::nodes::NodeService>) -> bool {
    peer.link == local_link()
        || service
            .and_then(|svc| svc.registry.current(&peer.id))
            .is_some_and(|generation| remote_link(generation) == peer.link)
}

/// A paused Node may receive a tombstone but not new Markdown. Revoked or unrelated devices
/// cannot receive anything (and are not claimed to have erased their previous state).
fn cache_access(c: &Connection, node: &str, ws: &str) -> Result<bool> {
    if node == "local" {
        return Ok(true);
    }
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes n WHERE n.node_id=?1 AND n.state IN ('active','paused')
           AND EXISTS (SELECT 1 FROM workspace_members m WHERE m.workspace_id=?2 AND m.user_id=n.owner AND m.role='owner')
           AND NOT EXISTS (SELECT 1 FROM workspace_members m WHERE m.workspace_id=?2 AND m.user_id<>n.owner))",
        params![node, ws], |r| r.get(0),
    )?)
}

fn adapter(c: &Connection, ws: &str, bot: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT b.harness FROM bots b WHERE b.id=?2 AND b.enabled=1 AND b.deleted=0
          AND (b.workspace_id=?1 OR EXISTS(SELECT 1 FROM bot_workspaces WHERE workspace_id=?1 AND bot_id=?2))",
        params![ws, bot], |r| r.get(0),
    ).optional()?)
}

fn plan(
    c: &Connection,
    peer: &Peer,
    ws: &str,
    bot: &str,
    features: &[String],
    isolated: bool,
) -> Result<Option<SyncSkills>> {
    if !cache_access(c, &peer.id, ws)? {
        return Ok(None);
    }
    let adapter = adapter(c, ws, bot)?;
    let take = isolated
        && adapter.as_deref().is_some_and(|a| supports(features, a))
        && (peer.id == "local" || super::node_binding::node_takes(c, &peer.id, ws)?);
    let snapshot = snapshot(c, ws, bot, take)?;
    let old: Option<(String, String, i64, Option<String>, Option<i64>, Option<String>)> = c.query_row(
        "SELECT adapter,digest,generation,acknowledged_link,acknowledged_at,error FROM node_skill_targets
          WHERE workspace_id=? AND bot_id=? AND node_id=?", params![ws, bot, peer.id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional()?;
    let adapter = adapter.unwrap_or_default();
    let generation = match old {
        None if snapshot.entries.is_empty() => return Ok(None),
        Some((a, d, g, link, at, error)) if a == adapter && d == snapshot.digest.as_str() => {
            if error.is_none()
                && link.as_deref() == Some(&peer.link)
                && at.is_some_and(|at| now() - at < 60_000)
            {
                // A successful acknowledgement may belong to an older generation; compare it.
                let done: bool = c
                    .query_row(
                        "SELECT acknowledged_generation=generation FROM node_skill_targets
                      WHERE workspace_id=? AND bot_id=? AND node_id=?",
                        params![ws, bot, peer.id],
                        |r| r.get::<_, Option<bool>>(0),
                    )?
                    .unwrap_or(false);
                if done {
                    return Ok(None);
                }
            }
            g
        }
        before => {
            let g = before.map_or(1, |v| v.2 + 1);
            c.execute("INSERT INTO node_skill_targets(workspace_id,bot_id,node_id,adapter,generation,digest)
               VALUES(?,?,?,?,?,?) ON CONFLICT(workspace_id,bot_id,node_id) DO UPDATE SET
                 adapter=excluded.adapter,generation=excluded.generation,digest=excluded.digest,error=NULL",
                params![ws, bot, peer.id, adapter, g, snapshot.digest.as_str()])?;
            g
        }
    };
    Ok(Some(SyncSkills {
        generation,
        adapter,
        snapshot,
    }))
}

pub(crate) fn distribution(
    c: &Connection,
    ws: &str,
    skill: &str,
    me: &str,
    links: &HashMap<String, String>,
    local: bool,
) -> Result<Option<Value>> {
    let Some(s) = repo::get(c, skill)?.filter(|s| s.workspace_id == ws) else {
        return Ok(None);
    };
    let h = repo::history(c, &s.id)?;
    let mut q = c.prepare(
        "SELECT n.node_id,n.name FROM nodes n
          WHERE n.state IN ('active','paused') AND EXISTS (
            SELECT 1 FROM workspace_members m WHERE m.workspace_id=? AND m.user_id=n.owner AND m.role='owner')
          ORDER BY n.name,n.node_id",
    )?;
    let mut devices = q
        .query_map([ws], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if local {
        devices.insert(0, ("local".into(), "local".into()));
    }
    let mut rows = Vec::new();
    for a in steve_store::caps::of_entry(c, "skill", skill)? {
        let harness = adapter(c, ws, &a.bot_id)?;
        let desired = snapshot(c, ws, &a.bot_id, a.enabled)?;
        for (node, label) in &devices {
            let live = links.get(node);
            let features: Option<(String, String, i64)> = c
                .query_row(
                    "SELECT link,features,seen FROM node_skill_inventory WHERE node_id=?",
                    [node],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let features = features
                .filter(|(link, _, seen)| Some(link) == live && now() - seen < 90_000)
                .map(|(_, f, _)| serde_json::from_str::<Vec<String>>(&f))
                .transpose()?;
            let last: Option<(String, i64, Option<i64>, Option<String>, Option<i64>, String, Option<String>)> = c.query_row(
                "SELECT digest,generation,acknowledged_generation,acknowledged_link,acknowledged_at,versions,error
                   FROM node_skill_targets WHERE workspace_id=? AND bot_id=? AND node_id=?",
                params![ws,a.bot_id,node], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
            ).optional()?;
            let taken = node == "local" || super::node_binding::node_takes(c, node, ws)?;
            let eligible = if !taken || harness.is_none() {
                Some(false)
            } else {
                features
                    .as_ref()
                    .map(|f| supports(f, harness.as_deref().unwrap_or_default()))
            };
            let state = if !a.enabled {
                "disabled"
            } else if h.published_version.is_none() {
                "unpublished"
            } else if !taken || harness.is_none() {
                "ineligible"
            } else if live.is_none() {
                "offline"
            } else if features.is_none() {
                "unknown"
            } else if !supports(
                features.as_deref().unwrap_or_default(),
                harness.as_deref().unwrap_or_default(),
            ) {
                "unsupported"
            } else {
                match &last {
                    Some((d, g, Some(ack), Some(link), Some(at), _, None))
                        if d == desired.digest.as_str()
                            && g == ack
                            && Some(link) == live
                            && now() - at < 90_000 =>
                    {
                        "cached"
                    }
                    Some((d, _, _, _, _, _, Some(_))) if d == desired.digest.as_str() => "failed",
                    _ => "pending",
                }
            };
            let confirmed = last
                .as_ref()
                .and_then(|r| serde_json::from_str::<Vec<SkillVersionRef>>(&r.5).ok())
                .and_then(|refs| refs.into_iter().find(|r| r.id == skill))
                .map(|r| r.version);
            rows.push(json!({ "node":node,"label":label,"bot":a.bot_id,"enabled":a.enabled,
                "desired":if a.enabled { h.published_version } else { None }, "confirmed":confirmed,
                "received_at":last.as_ref().and_then(|r| r.4), "status":state,
                "generation":last.as_ref().filter(|r| r.0 == desired.digest.as_str()).map(|r| r.1),
                "confirmed_generation":last.as_ref().and_then(|r| r.2),
                "eligible":eligible,"online":live.is_some(),
                "error":last.and_then(|r| r.6).filter(|_| state == "failed"),
            }));
        }
        if devices.is_empty() {
            rows.push(
                json!({"bot":a.bot_id,"enabled":a.enabled,"status":"no_nodes",
                "desired":if a.enabled { h.published_version } else { None }}),
            );
        }
    }
    let mut q = c.prepare(
        "SELECT s.id,r.node_id,r.phase,r.skill_expected,r.skill_use_receipt FROM runtime_sessions r
          JOIN sessions s ON s.id=r.session_id LEFT JOIN items i ON i.id=s.item_id
          WHERE s.workspace_id=?1 AND r.skill_expected IS NOT NULL
            AND (i.requested_by=?2 OR EXISTS (SELECT 1 FROM workspace_members
                 WHERE workspace_id=?1 AND user_id=?2 AND role='owner'))
            AND EXISTS (SELECT 1 FROM json_each(r.skill_expected) j WHERE json_extract(j.value,'$.id')=?3)
          ORDER BY r.updated DESC LIMIT 30",
    )?;
    let sessions = q.query_map(params![ws,me,skill], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,
        r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter_map(|(id,node,phase,expected,receipt)| {
            let reference = serde_json::from_str::<Vec<SkillVersionRef>>(&expected).ok()?.into_iter().find(|r| r.id==skill)?;
            let receipt = receipt.and_then(|v| serde_json::from_str::<SkillUseReceipt>(&v).ok());
            let status = match receipt {
                Some(r) if r.conflicts.contains(&reference) => "conflict",
                Some(r) if r.materialized.contains(&reference) => "opened",
                _ => "unknown",
            };
            Some(json!({"session":id,"node":node,"phase":phase,"version":reference.version,"status":status}))
        }).collect::<Vec<_>>();
    Ok(Some(json!({"nodes":rows,"sessions":sessions})))
}
