//! The storage operations for a confirmed channel cleanup.

use sea_query::{Expr, ExprTrait, Query, SelectStatement};
use steve_types::{AutonomyEvent, WorkEvent};

use crate::contract::Q;
use crate::{Result, W};

pub fn purge_text(w: &mut W<'_>, messages: &[String], now: i64) -> Result<()> {
    let keys = w.db().dialect().text_array(messages)?;
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", Option::<String>::None)
            .value("attachments", "[]")
            .value("purged", now)
            .and_where(Expr::col("id").in_subquery(keys.clone())),
    )?;
    // Keep the fingerprint and routing receipt, while clearing the duplicate body in the intent.
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value(
                "intent",
                Expr::cust_with_expr("json_set(?, '$.content', json('{}'))", Expr::col("intent")),
            )
            .and_where(Expr::col("message_id").in_subquery(keys.clone())),
    )?;
    w.exec(
        Query::update()
            .table("ext_events")
            .value("payload", Option::<String>::None)
            .value("payload_purged", now)
            .and_where(Expr::col("message_id").in_subquery(keys))
            .and_where(Expr::col("payload").is_not_null()),
    )?;
    Ok(())
}

pub struct Deletion<'a> {
    pub workspace: &'a str,
    pub decisions: &'a [String],
    pub spaces: &'a [String],
    pub messages: &'a [String],
    pub parties: &'a [String],
    pub accounts: &'a [String],
    pub messages_to_purge: &'a [String],
}

fn remove_rows(
    w: &mut W<'_>,
    table: &'static str,
    column: &'static str,
    ids: &SelectStatement,
) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table(table)
            .and_where(Expr::col(column).in_subquery(ids.clone())),
    )?;
    Ok(())
}

fn retire_decisions(w: &mut W<'_>, workspace: &str, decisions: &[String]) -> Result<()> {
    if decisions.is_empty() {
        return Ok(());
    }
    let ids = w.db().dialect().text_array(decisions)?;
    let actions: Vec<String> = w.all(
        Query::select()
            .column("id")
            .from("yolo_actions")
            .and_where(Expr::col("decision_id").in_subquery(ids.clone())),
        |row| row.get(0),
    )?;
    w.exec(
        Query::update()
            .table("yolo_actions")
            .value("decision_id", None::<String>)
            .value("undo", None::<String>)
            .and_where(Expr::col("decision_id").in_subquery(ids.clone())),
    )?;
    for table in ["chan_messages", "ext_events"] {
        w.exec(
            Query::update()
                .table(table)
                .value("decision_id", None::<String>)
                .and_where(Expr::col("decision_id").in_subquery(ids.clone())),
        )?;
    }
    remove_rows(w, "decisions", "id", &ids)?;
    for id in actions {
        w.emit(AutonomyEvent::YoloAction {
            id: id.into(),
            decision_id: None,
            ws: workspace.into(),
        });
    }
    for id in decisions {
        w.emit(WorkEvent::Decision {
            id: id.as_str().into(),
            item_id: Some(None),
            ws: workspace.into(),
        });
    }
    Ok(())
}

fn detach_signals(
    w: &mut W<'_>,
    spaces: &SelectStatement,
    msgs: &SelectStatement,
    now: i64,
) -> Result<()> {
    let targeted = Expr::col("message_id")
        .in_subquery(msgs.clone())
        .or(Expr::col("space_id").in_subquery(spaces.clone()));
    let referenced = |table| {
        Expr::exists(
            Query::select()
                .column(("x", "event_id"))
                .from_as(table, "x")
                .and_where(Expr::col(("x", "event_id")).eq(Expr::col(("ext_events", "id"))))
                .take(),
        )
    };
    w.exec(
        Query::delete()
            .from_table("ext_events")
            .and_where(targeted.clone())
            .and_where(Expr::col("work_item_id").is_null())
            .and_where(Expr::col("decision_id").is_null())
            .and_where(referenced("flow_runs").not())
            .and_where(referenced("digest_entries").not())
            .and_where(referenced("signal_corrections").not()),
    )?;
    w.exec(
        Query::update()
            .table("ext_events")
            .value("payload", None::<String>)
            .value("payload_purged", now)
            .value("message_id", None::<String>)
            .value("space_id", None::<String>)
            .and_where(targeted),
    )?;
    Ok(())
}

fn remove_deliveries(
    w: &mut W<'_>,
    spaces: &SelectStatement,
    msgs: &SelectStatement,
) -> Result<()> {
    w.exec(
        Query::delete().from_table("output_deliveries").and_where(
            Expr::col("message_id")
                .in_subquery(msgs.clone())
                .or(Expr::col("space_id").in_subquery(spaces.clone())),
        ),
    )?;
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value("reply_message_id", None::<String>)
            .and_where(Expr::col("reply_message_id").in_subquery(msgs.clone())),
    )?;
    for column in ["manual_space_id", "learned_space_id"] {
        w.exec(
            Query::update()
                .table("output_channel_preferences")
                .value(column, None::<String>)
                .and_where(Expr::col(column).in_subquery(spaces.clone())),
        )?;
    }
    Ok(())
}

fn untag(w: &mut W<'_>, kind: &str, ids: &SelectStatement) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("entity_tags")
            .and_where(Expr::col("object_type").eq(kind))
            .and_where(Expr::col("object_id").in_subquery(ids.clone())),
    )?;
    Ok(())
}

fn remove_contacts(
    w: &mut W<'_>,
    parties: &SelectStatement,
    accounts: &SelectStatement,
) -> Result<()> {
    remove_rows(w, "output_channel_preferences", "party_id", parties)?;
    remove_rows(w, "space_members", "account_id", accounts)?;
    remove_rows(w, "account_handles", "account_id", accounts)?;
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("sender_account_id", None::<String>)
            .and_where(Expr::col("sender_account_id").in_subquery(accounts.clone())),
    )?;
    remove_rows(w, "accounts", "id", accounts)?;
    let point = |kind, id| {
        Expr::col(kind)
            .eq("party")
            .and(Expr::col(id).in_subquery(parties.clone()))
    };
    w.exec(
        Query::delete()
            .from_table("links")
            .and_where(point("subject_type", "subject_id").or(point("object_type", "object_id"))),
    )?;
    untag(w, "party", parties)?;
    remove_rows(w, "parties", "id", parties)?;
    Ok(())
}

pub fn delete(w: &mut W<'_>, plan: Deletion<'_>, now: i64) -> Result<()> {
    let dialect = w.db().dialect();
    let spaces = dialect.text_array(plan.spaces)?;
    let messages = dialect.text_array(plan.messages)?;
    let parties = dialect.text_array(plan.parties)?;
    let accounts = dialect.text_array(plan.accounts)?;
    retire_decisions(w, plan.workspace, plan.decisions)?;
    detach_signals(w, &spaces, &messages, now)?;
    for table in ["reply_basis", "route_log", "thread_runs", "chan_reactions"] {
        remove_rows(w, table, "message_id", &messages)?;
    }
    remove_deliveries(w, &spaces, &messages)?;
    crate::contract::channel_messages::retention::remove(w, plan.messages)?;
    for table in [
        "space_members",
        "space_settings",
        "persona_bindings",
        "space_prefs",
        "space_identities",
    ] {
        remove_rows(w, table, "space_id", &spaces)?;
    }
    untag(w, "space", &spaces)?;
    remove_rows(w, "spaces", "id", &spaces)?;
    remove_contacts(w, &parties, &accounts)?;
    if !plan.messages_to_purge.is_empty() {
        purge_text(w, plan.messages_to_purge, now)?;
    }
    Ok(())
}

pub mod selection;
