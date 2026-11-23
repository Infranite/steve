//! Durable Node registration, pairing, and link fencing.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Query, Returning};
use steve_node_api::NodeVitals;

use crate::{Read, Result, W};

use super::Q;

const CONNECTABLE: [&str; 2] = ["active", "paused"];

fn connectable(node: &str) -> Expr {
    Expr::col("node_id")
        .eq(node)
        .and(Expr::col("state").is_in(CONNECTABLE))
}

pub fn registered_key(
    c: &mut impl Read,
    public: &[u8; 32],
    fingerprint: Option<&str>,
) -> Result<bool> {
    let mut key = Expr::col("pubkey").eq(public.to_vec());
    if let Some(fingerprint) = fingerprint {
        key = key.or(Expr::col("fingerprint").eq(fingerprint));
    }
    Ok(c.one(
        Query::select()
            .column("node_id")
            .from("nodes")
            .and_where(key)
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn link_key(c: &mut impl Read, node: &str) -> Result<Option<Vec<u8>>> {
    c.one(
        Query::select()
            .column("pubkey")
            .from("nodes")
            .and_where(connectable(node)),
        |r| r.get(0),
    )
}

pub fn link_state(c: &mut impl Read, node: &str) -> Result<Option<(i64, String)>> {
    c.one(
        Query::select()
            .columns(["generation", "state"])
            .from("nodes")
            .and_where(Expr::col("node_id").eq(node)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn owner_if_connectable(c: &mut impl Read, node: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("owner")
            .from("nodes")
            .and_where(connectable(node)),
        |r| r.get(0),
    )
}

pub fn rotate_key(
    w: &mut W<'_>,
    node: &str,
    old: &[u8; 32],
    new: &[u8; 32],
    fingerprint: &str,
) -> Result<bool> {
    if registered_key(w, new, None)? {
        return Ok(false);
    }
    Ok(w.exec(
        Query::update()
            .table("nodes")
            .value("pubkey", new.to_vec())
            .value("fingerprint", fingerprint)
            .and_where(connectable(node))
            .and_where(Expr::col("pubkey").eq(old.to_vec())),
    )? == 1)
}

/// How a link arrived, before any ordinary restart metadata is written.
pub fn admit_link(
    w: &mut W<'_>,
    node: &str,
    arrival: &str,
    at: i64,
) -> Result<Option<(i64, Option<i64>)>> {
    w.one(
        Query::update()
            .table("nodes")
            .value("generation", Expr::col("generation").add(1_i64))
            .value("last_seen", at)
            .value("link", arrival)
            .and_where(connectable(node))
            .returning(Returning::new().columns(["generation", "incarnation"])),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn introduce_link(
    w: &mut W<'_>,
    node: &str,
    generation: i64,
    incarnation: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("nodes")
            .value("incarnation", incarnation)
            .and_where(connectable(node))
            .and_where(Expr::col("generation").eq(generation)),
    )? == 1)
}

pub fn pause_after_takeover(w: &mut W<'_>, node: &str) -> Result<Option<String>> {
    w.one(
        Query::update()
            .table("nodes")
            .value("state", "paused")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("state").eq("active"))
            .returning_col("name"),
        |r| r.get(0),
    )
}

/// Preserve a completed pairing and expire an unfinished installation's code.
pub fn finish_install(w: &mut W<'_>, code_hash: &str, at: i64) -> Result<Option<String>> {
    let node = w.one(
        Query::select()
            .column("node_id")
            .from("node_pairings")
            .and_where(Expr::col("code_hash").eq(code_hash))
            .and_where(Expr::col("claimed_at").is_not_null()),
        |r| r.get(0),
    )?;
    w.exec(
        Query::update()
            .table("node_pairings")
            .value("expires", at)
            .and_where(Expr::col("code_hash").eq(code_hash))
            .and_where(Expr::col("claimed_at").is_null()),
    )?;
    Ok(node)
}

/// A new local identity updates only the existing row's key and active state.
pub fn register_local(
    w: &mut W<'_>,
    node: &crate::nodes::NewNode<'_>,
    owner: &str,
    at: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("nodes")
            .columns([
                "node_id",
                "owner",
                "name",
                "pubkey",
                "fingerprint",
                "state",
                "local",
                "created",
            ])
            .values_panic([
                node.node_id.into(),
                owner.into(),
                node.name.into(),
                node.pubkey.to_vec().into(),
                node.fingerprint.into(),
                "active".into(),
                true.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::column("node_id")
                    .update_columns(["pubkey", "fingerprint", "state", "local"])
                    .values([
                        ("revoked_at", Expr::val(None::<i64>)),
                        ("revoked_by", Expr::val(None::<String>)),
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn set_upgrade_artifact(
    w: &mut W<'_>,
    operation: &str,
    file: &str,
    sha256: &str,
    size: i64,
) -> Result<()> {
    w.exec(
        Query::update()
            .table("node_upgrades")
            .values([
                ("file", Expr::val(file)),
                ("sha256", Expr::val(sha256)),
                ("size", Expr::val(size)),
            ])
            .and_where(Expr::col("operation_id").eq(operation)),
    )?;
    Ok(())
}

pub struct RemoteVitals {
    pub node: String,
    pub name: String,
    pub data: NodeVitals,
}

pub fn recent_remote_vitals(c: &mut impl Read, since: i64) -> Result<Vec<RemoteVitals>> {
    let latest = Query::select()
        .expr(Func::max(Expr::col(("w", "at"))))
        .from_as("node_vitals", "w")
        .and_where(Expr::col(("w", "node_id")).eq(Expr::col(("v", "node_id"))))
        .take();
    let rows = c.all(
        Query::select()
            .columns([("v", "node_id"), ("n", "name"), ("v", "data")])
            .from_as("node_vitals", "v")
            .join_as(
                JoinType::InnerJoin,
                "nodes",
                "n",
                Expr::col(("n", "node_id")).eq(Expr::col(("v", "node_id"))),
            )
            .and_where(Expr::col(("n", "local")).eq(false))
            .and_where(Expr::col(("v", "at")).gte(since))
            .and_where(Expr::col(("v", "at")).eq(Expr::from(latest))),
        |r| Ok((r.get(0)?, r.get(1)?, r.get::<String>(2)?)),
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|(node, name, data)| {
            serde_json::from_str(&data)
                .ok()
                .map(|data| RemoteVitals { node, name, data })
        })
        .collect())
}

pub fn owner(r: &mut impl Read, node: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("owner")
            .from("nodes")
            .and_where(Expr::col("node_id").eq(node)),
        |row| row.get(0),
    )
}
