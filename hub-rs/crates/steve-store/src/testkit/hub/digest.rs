//! Durable digest schedule fixtures for the Hub wiring tests.

use sea_query::{Expr, ExprTrait, OnConflict, Query};

use crate::{Result, Store, W};

pub fn configure_packet(w: &mut W<'_>, ws: &str) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("workspaces")
            .columns(["id", "name", "created"])
            .values_panic([ws.into(), "digest test".into(), 1_i64.into()])
            .on_conflict(OnConflict::column("id").do_nothing().to_owned()),
    )?;
    crate::WorkspaceSettings(w).set(ws, "digest", r#"{"every_min":1,"packets":1000}"#, 1)?;
    Ok(())
}

pub fn seed_owner_workspace(w: &mut W<'_>, ws: &str, user: &str) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("workspaces")
            .columns(["id", "name", "created"])
            .values_panic([ws.into(), "startup contract".into(), 1_i64.into()]),
    )?;
    w.exec(
        Query::insert()
            .into_table("workspace_members")
            .columns(["workspace_id", "user_id", "role", "created"])
            .values_panic([ws.into(), user.into(), "owner".into(), 1_i64.into()]),
    )?;
    w.exec(
        Query::insert()
            .into_table("bot_workspaces")
            .columns(["bot_id", "workspace_id", "created"])
            .values_panic(["steve".into(), ws.into(), 1_i64.into()]),
    )?;
    Ok(())
}

pub fn break_graph(w: &mut W<'_>, id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("flows")
            .value("graph", "broken json")
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn reject_failed_start(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE flows SET graph='broken json' WHERE id=?", [id])?;
        tx.execute_batch("CREATE TRIGGER reject_failed_run BEFORE INSERT ON flow_runs WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'cannot persist failed start'); END")?;
        Ok(())
    })
}

pub fn recorded_workspaces(store: &Store, state: &str) -> Result<Vec<String>> {
    store.reader(|db| {
        let mut q = db.prepare("SELECT DISTINCT workspace_id FROM flow_runs WHERE json_extract(context,'$.source')='digest.bot' AND json_extract(context,'$.startup.state')=?")?;
        Ok(q.query_map([state], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn flow_graph(store: &Store, id: &str) -> Result<String> {
    store.reader(|db| Ok(db.query_row("SELECT graph FROM flows WHERE id=?", [id], |r| r.get(0))?))
}

pub fn set_graph_blob(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE flows SET graph=x'01' WHERE id=?", [id])?;
        Ok(())
    })
}

pub fn started_count(store: &Store, ws: &str) -> Result<i64> {
    store.reader(|db| Ok(db.query_row("SELECT COUNT(*) FROM flow_runs WHERE workspace_id=? AND json_extract(context,'$.startup.state')='started'", [ws], |r| r.get(0))?))
}

pub fn disable_flow(store: &Store, flow: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE flows SET enabled=0 WHERE id=?", [flow])?;
        Ok(())
    })
}

pub fn seed_busy_flow(store: &Store, ws: &str, flow: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE flows SET enabled=1 WHERE id=?", [flow])?;
        tx.execute("INSERT INTO flow_runs(id,workspace_id,flow_id,flow_version,trigger_kind,status,started) VALUES ('digest_busy',?,?,1,'manual','running',1000)", [ws, flow])?;
        Ok(())
    })
}

pub fn finish_busy_flow(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE flow_runs SET status='done' WHERE id='digest_busy'",
            [],
        )?;
        Ok(())
    })
}

pub fn damaged_run_columns(store: &Store, run: &str) -> Result<(String, String)> {
    store.reader(|db| Ok(db.query_row("SELECT json_extract(context,'$.startup.attempts'),typeof(cost) FROM flow_runs WHERE id=?", [run], |r| Ok((r.get(0)?,r.get(1)?)))?))
}
