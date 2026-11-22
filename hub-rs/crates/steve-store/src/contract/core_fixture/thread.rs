//! Thread and conversation fixtures with explicit persisted identities.

use sea_query::{Expr, ExprTrait, OnConflict, Query};

use crate::contract::Q;
use crate::{Store, StoreError, core_queue};

pub fn owner(store: &Store, ws: &str, id: &str, subject: &str, name: &str) {
    store
        .write::<_, StoreError>(|w| {
            w.exec(
                Query::insert()
                    .into_table("users")
                    .columns(["id", "workspace_id", "subject", "name", "role", "created"])
                    .values_panic([
                        id.into(),
                        ws.into(),
                        subject.into(),
                        name.into(),
                        "owner".into(),
                        1_i64.into(),
                    ])
                    .on_conflict(OnConflict::new().do_nothing().to_owned()),
            )?;
            w.exec(
                Query::insert()
                    .into_table("workspace_members")
                    .columns(["workspace_id", "user_id", "role", "created"])
                    .values_panic([ws.into(), id.into(), "owner".into(), 1_i64.into()])
                    .on_conflict(
                        OnConflict::columns(["workspace_id", "user_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn requested_by(store: &Store, item: &str) -> Option<String> {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .column("requested_by")
                    .from("items")
                    .and_where(Expr::col("id").eq(item)),
                |row| row.get(0),
            )
        })
        .unwrap()
        .flatten()
}

pub fn resume_attempts(store: &Store, run: &str, attempts: i64) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("thread_resume")
                    .value("attempts", attempts)
                    .and_where(Expr::col("run_id").eq(run)),
            )
        })
        .unwrap();
}

pub fn pending_payload(store: &Store, ws: &str, message: &str) -> Option<String> {
    store.read(|r| core_queue::pending(r, ws, message)).unwrap()
}

pub fn replace_pending(store: &Store, ws: &str, message: &str, payload: &str) {
    store
        .write(|w| core_queue::replace_pending(w, ws, message, payload))
        .unwrap();
}

pub fn set_pending(store: &Store, ws: &str, message: &str, payload: &str) {
    store
        .write(|w| core_queue::enqueue(w, ws, message, payload, 1))
        .unwrap();
}

pub fn thread_run_count(store: &Store, stored: Option<(&str, &str)>) -> i64 {
    let mut query = Query::select();
    query.expr(Expr::cust("count(*)")).from("thread_runs");
    if let Some((message, status)) = stored {
        query
            .and_where(Expr::col("message_id").eq(message))
            .and_where(Expr::col("status").eq(status));
    }
    store
        .read(|r| r.one(&query, |row| row.get(0)))
        .unwrap()
        .unwrap()
}

pub fn conversation_item(store: &Store, origin: &str) -> Option<String> {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .column("item_id")
                    .from("thread_items")
                    .and_where(Expr::col("origin_msg").eq(origin)),
                |row| row.get(0),
            )
        })
        .unwrap()
}

pub fn pending_count(store: &Store, message: &str) -> i64 {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .expr(Expr::cust("count(*)"))
                    .from("settings")
                    .and_where(Expr::col("key").eq(core_queue::pending_key(message))),
                |row| row.get(0),
            )
        })
        .unwrap()
        .unwrap()
}

pub struct ReceivedDelivery<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub workspace: &'a str,
    pub item: &'a str,
    pub session: &'a str,
    pub name: &'a str,
    pub mime: &'a str,
    pub size: i64,
    pub entrypoint: Option<&'a str>,
    pub sha256: &'a str,
}

pub fn received_delivery(store: &Store, delivery: ReceivedDelivery<'_>) {
    store
        .write(|w| {
            w.exec(
                Query::insert()
                    .into_table("deliveries")
                    .columns([
                        "id",
                        "url",
                        "workspace_id",
                        "item_id",
                        "session_id",
                        "title",
                        "name",
                        "mime",
                        "size",
                        "created",
                        "entrypoint",
                        "sha256",
                        "manifest",
                        "preview",
                    ])
                    .values_panic([
                        delivery.id.into(),
                        delivery.url.into(),
                        delivery.workspace.into(),
                        delivery.item.into(),
                        delivery.session.into(),
                        "Design".into(),
                        delivery.name.into(),
                        delivery.mime.into(),
                        delivery.size.into(),
                        1_i64.into(),
                        delivery.entrypoint.into(),
                        delivery.sha256.into(),
                        "{}".into(),
                        "{}".into(),
                    ]),
            )
        })
        .unwrap();
}

pub fn default_team(store: &Store, ws: &str, bot: &str) {
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO bot_teams(id,workspace_id,name,members) VALUES ('team',?,'Preset',?)",
                rusqlite::params![
                    ws,
                    serde_json::json!([{"bot": bot, "nodes": null}]).to_string()
                ],
            )?;
            tx.execute(
                "INSERT INTO bot_team_defaults(workspace_id,team_id) VALUES (?,'team')",
                [ws],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn sarah_contact(store: &Store) {
    owner(store, crate::WS, "usr_dev", "dev", "Dev");
    store.with_conn(|c| {
        c.execute_batch(
            "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated)
             VALUES ('ct_sarah', 'default', 'contact', 'Sarah Chen', '[\"Sarah\"]', 'usr_dev', 1, 1);",
        )?;
        Ok(())
    }).unwrap();
}

pub fn important_contact(store: &Store, id: &str) -> bool {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .column("important")
                    .from("parties")
                    .and_where(Expr::col("id").eq(id)),
                |row| Ok(row.get::<i64>(0)? != 0),
            )
        })
        .unwrap()
        .unwrap()
}
