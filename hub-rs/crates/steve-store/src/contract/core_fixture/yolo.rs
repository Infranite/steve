//! Persisted situations the YOLO decisions and scheduler read in Core tests.

use rusqlite::params;
use sea_query::{Expr, ExprTrait, Query};

use crate::contract::Q;
use crate::{Store, StoreError, WS};

pub struct Outbound<'a> {
    pub space: &'a str,
    pub system: &'a str,
    pub external: &'a str,
    pub name: &'a str,
    pub message: &'a str,
    pub status: &'a str,
    pub text: Option<&'a str>,
    pub basis: Option<(&'a str, &'a str)>,
}

pub fn outbound(store: &Store, fixture: Outbound<'_>) {
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
            "INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
             VALUES (?, ?, ?, 'group', ?, ?, 1, 1)",
            params![fixture.space, WS, fixture.system, fixture.external, fixture.name],
        )?;
            if let Some(text) = fixture.text {
                tx.execute(
                "INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,status,created)
                 VALUES (?, ?, ?, 'out', ?, ?, 1)",
                params![fixture.message, WS, fixture.space, text, fixture.status],
            )?;
            } else {
                tx.execute(
                    "INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,created)
                 VALUES (?, ?, ?, 'out', ?, 1)",
                    params![fixture.message, WS, fixture.space, fixture.status],
                )?;
            }
            if let Some((id, verdict)) = fixture.basis {
                tx.execute(
                    "INSERT INTO reply_basis (id,workspace_id,message_id,entries,verdict,created)
                 VALUES (?, ?, ?, '[]', ?, 1)",
                    params![id, WS, fixture.message, verdict],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

pub fn reject_action_settlement(store: &Store) {
    store
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER yolo_no_decided BEFORE UPDATE OF outcome ON yolo_actions
            WHEN NEW.outcome = 'decided' BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn corrupt_decision_options(store: &Store, id: &str) {
    store
        .with_conn(|c| {
            c.execute("UPDATE decisions SET options=x'80' WHERE id=?", [id])?;
            Ok(())
        })
        .unwrap();
}

pub fn terminal_action_ids(store: &Store) -> Vec<String> {
    store
        .read(|r| {
            r.all(
                Query::select()
                    .column("id")
                    .from("yolo_actions")
                    .and_where(Expr::col("outcome").ne("scheduled")),
                |row| row.get(0),
            )
        })
        .unwrap()
}

/// Simulate a writer that updates the due source without using the scheduler repository.
pub fn change_due_at(store: &Store, id: &str, at: i64) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("yolo_actions")
                    .value("due_at", at)
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}
