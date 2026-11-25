//! Thread and conversation fixtures with explicit persisted identities.

use sea_query::{Expr, ExprTrait, OnConflict, Query};

use crate::contract::Q;
use crate::{Store, StoreError};

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

pub fn thread_task_count(store: &Store, stored: Option<(&str, &str)>) -> i64 {
    let mut query = Query::select();
    query
        .expr(Expr::cust("count(*)"))
        .from("thread_reply_tasks");
    if let Some((message, status)) = stored {
        query
            .and_where(Expr::col("origin_message_id").eq(message))
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
                    .column("target_item_id")
                    .from("thread_dispatch_refs")
                    .and_where(Expr::col("origin_message_id").eq(origin))
                    .and_where(Expr::col("effect_kind").eq("create_item"))
                    .and_where(Expr::col("target_item_id").is_not_null()),
                |row| row.get(0),
            )
        })
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
        .write::<_, StoreError>(|tx| {
            tx.conn.execute(
                "INSERT INTO bot_teams(id,workspace_id,name,members) VALUES ('team',?,'Preset',?)",
                rusqlite::params![
                    ws,
                    serde_json::json!([{"bot": bot, "nodes": null}]).to_string()
                ],
            )?;
            tx.conn.execute(
                "INSERT INTO bot_team_defaults(workspace_id,team_id) VALUES (?,'team')",
                [ws],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn sarah_contact(store: &Store) {
    owner(store, crate::WS, "usr_dev", "dev", "Dev");
    store.write_locked(|c| {
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

pub fn session_blocker(
    store: &crate::Store,
    session: &str,
) -> crate::Result<Option<crate::item_gate::GateBlocker>> {
    store.read_conn(|c| crate::item_gate::session_blocker_on(c, session))
}

pub fn attention(
    store: &crate::Store,
    ws: &str,
    user: &str,
) -> crate::Result<(Vec<crate::attention::Entry>, i64)> {
    store.read_conn(|c| crate::attention::replies_on(c, ws, user))
}

pub fn attention_target(
    store: &crate::Store,
    ws: &str,
    user: &str,
    id: &str,
) -> crate::Result<(String, i64)> {
    store.read_conn(|c| crate::attention::reply_on(c, ws, user, id))
}
