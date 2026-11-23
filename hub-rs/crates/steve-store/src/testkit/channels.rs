//! Storage fixtures shared by the channel integration tests and manual harnesses.
pub mod evaluation;
pub mod inspect;
pub mod notifications;
pub mod seed;

use std::path::{Path, PathBuf};

use crate::contract::Q;
use serde::Serialize;

use crate::{Decision, Result, Store, decision_repo};

#[derive(Serialize)]
pub struct Outbound {
    pub id: String,
    pub status: String,
    pub text: Option<String>,
    pub external_id: Option<String>,
    pub decision: Option<Decision>,
}

pub fn outbound(store: &Store) -> Result<Vec<Outbound>> {
    store.read(|r| {
        use sea_query::{Expr, ExprTrait, Query};

        let messages = r.all(
            Query::select()
                .columns(["id", "status", "text", "external_id", "decision_id"])
                .from("chan_messages")
                .and_where(Expr::col("direction").eq("out"))
                .order_by("rowid", sea_query::Order::Asc),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<Option<String>>(4)?,
                ))
            },
        )?;
        messages
            .into_iter()
            .map(|(id, status, text, external_id, decision)| {
                Ok(Outbound {
                    id,
                    status,
                    text,
                    external_id,
                    decision: decision
                        .map(|id| decision_repo::get(r, &id))
                        .transpose()?
                        .flatten(),
                })
            })
            .collect()
    })
}

#[derive(Serialize)]
pub struct Inbound {
    pub text: Option<String>,
    pub history: Option<String>,
    pub label: Option<String>,
}

pub fn inbound_history(store: &Store) -> Result<Vec<Inbound>> {
    use sea_query::{Expr, ExprTrait, JoinType, Order, Query};

    store.read(|r| {
        r.all(
            Query::select()
                .columns([("m", "text"), ("m", "history"), ("e", "outcome_label")])
                .from_as("chan_messages", "m")
                .join_as(
                    JoinType::LeftJoin,
                    "ext_events",
                    "e",
                    Expr::col(("e", "id")).equals(("m", "event_id")),
                )
                .and_where(Expr::col(("m", "direction")).eq("in"))
                .order_by(("m", "rowid"), Order::Asc),
            |row| {
                Ok(Inbound {
                    text: row.get(0)?,
                    history: row.get(1)?,
                    label: row.get(2)?,
                })
            },
        )
    })
}

pub fn destructive_backup(database: &Path, root: &Path, what: &str) -> Result<PathBuf> {
    use steve_store_sqlite::backup::{BackupPlan, CancelFlag, Reason, backup};

    let plan = BackupPlan::new(
        root.to_path_buf(),
        Reason::PreDestructive { what: what.into() },
    );
    let saved = backup(database, &plan, &|_| {}, &CancelFlag::default())
        .map_err(|e| crate::StoreError::Storage(e.to_string()))?;
    Ok(saved.database_path().strip_prefix(root).unwrap().to_owned())
}
fn project_write<T>(
    store: &Store,
    write: impl FnOnce(&rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    use std::collections::{BTreeMap, BTreeSet};
    use steve_store_sqlite::digest::{self, Scope, Source};
    type Rows = BTreeMap<(u8, String), (Scope, String, i64)>;
    fn rows(c: &rusqlite::Connection) -> rusqlite::Result<Rows> {
        let mut rows = Rows::new();
        for (kind, source) in [(0, Source::ChanMessage), (1, Source::MailMessage)] {
            for m in source.messages(c, steve_store_sqlite::digest::MessageFilter::All)? {
                rows.insert((kind, m.id), (m.scope, m.status, m.at));
            }
        }
        Ok(rows)
    }
    fn receipts(c: &rusqlite::Connection) -> rusqlite::Result<BTreeSet<(String, String, String)>> {
        c.prepare("SELECT workspace_id,conv,message_id FROM digest_receipts")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
    }
    store.write(|tx| {
        let before = rows(tx.conn)?;
        let old_receipts = receipts(tx.conn)?;
        let result = write(tx.conn)?;
        let after = rows(tx.conn)?;
        let removed = before
            .iter()
            .filter(|(key, _)| !after.contains_key(*key))
            .map(|((_, id), (scope, _, _))| (scope.clone(), id.clone()));
        digest::remove_message(tx.conn, removed)?;
        for ((kind, id), row) in &after {
            if before.get(&(*kind, id.clone())) != Some(row) {
                let source = if *kind == 0 {
                    Source::ChanMessage
                } else {
                    Source::MailMessage
                };
                digest::sync_message(
                    tx.conn,
                    source,
                    id,
                    before.get(&(*kind, id.clone())).map(|r| &r.0),
                )?;
            }
        }
        let mut acknowledgements: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for (ws, conv, id) in receipts(tx.conn)?.difference(&old_receipts) {
            acknowledgements
                .entry((ws.clone(), conv.clone()))
                .or_default()
                .push(id.clone());
        }
        for ((ws, conv), ids) in acknowledgements {
            digest::dequeue_on_receipt(tx.conn, &ws, &conv, ids)?;
        }
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_store_sqlite::digest::{self, Source};
    #[test]
    fn fixture_sql_does_not_repair_an_earlier_missing_production_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("fixture.sqlite3"), crate::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store.write(|tx| {
        tx.conn.execute("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated) VALUES('s','default','test','group','s','S',1,1)",[])?;
        tx.conn.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,created) VALUES('omitted','default','s','in','received',1)",[])?;
        Ok::<_,crate::StoreError>(())
    }).unwrap();
        project_write(&store, |c| {
            c.execute(
                "UPDATE chan_messages SET text='fixture body change' WHERE id='omitted'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        store
            .write(|tx| {
                assert_eq!(
                    digest::verify_pending(tx.conn, "default")?[0].kind,
                    digest::MismatchKind::Missing
                );
                digest::sync_message(tx.conn, Source::ChanMessage, "omitted", None)?;
                Ok::<_, crate::StoreError>(())
            })
            .unwrap();
    }
}

/// Prove that a watched write wakes a due source without changing business fields.
pub fn touch_first_watched_row(store: &Store, tables: &[&str]) -> Result<usize> {
    store.write(|w| {
        let c = w.fixture_connection();
        for table in tables {
            assert!(!table.is_empty() && table.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
            let touched = c.execute(
                &format!("UPDATE {table} SET rowid=rowid WHERE rowid IN (SELECT rowid FROM {table} LIMIT 1)"),
                [],
            )?;
            if touched > 0 {
                return Ok(touched);
            }
        }
        Ok(0)
    })
}
