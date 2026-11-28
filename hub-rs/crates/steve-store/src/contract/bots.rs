use crate::{Avatar, Bot, NewBot, Read, Result, W};
use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Order, Query};

/// Creates a new (non-main) bot inside the caller's write transaction, joins it to `ws`,
/// records `bot.create` in the audit trail, and emits `ConfigEvent::Bot`.
pub fn create(w: &mut W<'_>, ws: &str, b: &NewBot, author: &str) -> Result<Bot> {
    let id = crate::uid("bot");
    let now = crate::now();
    let owner = if !author.is_empty() {
        Some(author.to_string())
    } else {
        crate::settings::workspace_owner(w, ws)?
    };
    let role = b.role.clone().unwrap_or_default();
    let rules = b.rules.clone().unwrap_or_default();
    let persona = b.persona.clone().unwrap_or_default();
    let proactive = b.proactive.unwrap_or(1);
    let accept_mode = b
        .accept_mode
        .clone()
        .unwrap_or_else(|| "inherit".to_string());
    let perm_mode = b.perm_mode.clone().unwrap_or_else(|| "ask".to_string());
    let avatar = b.avatar.clone().unwrap_or_else(Avatar::default);
    let specialties = b.specialties.clone().unwrap_or_default();
    let enabled = b.enabled.unwrap_or(true);
    let avatar_json = serde_json::to_string(&avatar)?;
    let specialties_json = serde_json::to_string(&specialties)?;

    w.exec(
        Query::insert()
            .into_table("bots")
            .columns([
                "id",
                "workspace_id",
                "name",
                "role",
                "main",
                "harness",
                "model",
                "reasoning_effort",
                "rules",
                "persona",
                "proactive",
                "accept_mode",
                "perm_mode",
                "avatar",
                "specialties",
                "enabled",
                "revision",
                "updated",
                "owner",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                b.name.as_str().into(),
                role.as_str().into(),
                0_i64.into(),
                b.harness.as_str().into(),
                b.model.as_deref().into(),
                b.reasoning_effort.as_str().into(),
                rules.as_str().into(),
                persona.as_str().into(),
                proactive.into(),
                accept_mode.as_str().into(),
                perm_mode.as_str().into(),
                avatar_json.as_str().into(),
                specialties_json.as_str().into(),
                i64::from(enabled).into(),
                1_i64.into(),
                now.into(),
                owner.as_deref().into(),
            ]),
    )?;
    if let Some(fallbacks) = b.fallback_surfaces.as_deref() {
        for (idx, surface) in fallbacks.iter().enumerate() {
            w.exec(
                Query::insert()
                    .into_table("bot_fallback_surfaces")
                    .columns(["bot_id", "ordinal", "harness", "model", "reasoning_effort"])
                    .values_panic([
                        id.as_str().into(),
                        (idx as i64).into(),
                        surface.harness.as_str().into(),
                        surface.model.as_deref().into(),
                        surface.reasoning_effort.as_str().into(),
                    ]),
            )?;
        }
    }
    w.exec(
        Query::insert()
            .into_table("bot_workspaces")
            .columns(["bot_id", "workspace_id", "default_project", "created"])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                b.default_project.as_deref().into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["bot_id", "workspace_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    let default_nodes = serde_json::to_string(crate::bot_dispatch::DEFAULT_NODES)?;
    w.exec(
        Query::insert()
            .into_table("bot_node_bindings")
            .columns(["workspace_id", "bot_id", "nodes"])
            .values_panic([ws.into(), id.as_str().into(), default_nodes.as_str().into()])
            .on_conflict(
                OnConflict::columns(["workspace_id", "bot_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    w.audit(
        ws,
        author,
        "bot.create",
        &id,
        &serde_json::json!({"name": b.name}),
    )?;
    w.emit(steve_types::ConfigEvent::Bot {
        ws: ws.into(),
        id: id.clone().into(),
    });
    Ok(Bot {
        id,
        workspace_id: ws.into(),
        name: b.name.clone(),
        role,
        main: false,
        harness: b.harness.clone(),
        model: b.model.clone(),
        rules,
        accept_mode,
        perm_mode,
        enabled,
        revision: 1,
        updated: now,
        persona,
        proactive,
        avatar,
        prompt: None,
        device: None,
        owner,
        reasoning_effort: b.reasoning_effort.clone(),
        specialties,
    })
}

/// A bot's display name.
pub fn bot_name(c: &mut impl Read, bot: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["name"])
            .from("bots")
            .and_where(Expr::col("id").eq(bot)),
        |r| r.get(0),
    )
}

pub fn scoped_bot(c: &mut impl Read, ws: &str, id: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("bots", "b")
            .join_as(
                JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn main_bot(c: &mut impl Read) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["id"])
            .from("bots")
            .and_where(Expr::col("main").eq(1_i64))
            .and_where(Expr::col("deleted").eq(0_i64))
            .order_by("updated", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn bot_enabled(c: &mut impl Read, id: &str) -> Result<Option<bool>> {
    c.one(
        Query::select()
            .columns(["enabled"])
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )
}

/// Soft-delete a bot. The main bot cannot be removed.
pub fn remove(w: &mut W<'_>, id: &str) -> Result<()> {
    use crate::contract::Q;
    w.exec(
        Query::update()
            .table("bots")
            .value("deleted", 1_i64)
            .value("enabled", 0_i64)
            .value("updated", crate::now())
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("main").eq(0_i64)),
    )?;
    let workspace: Option<String> = w.one(
        Query::select()
            .column("workspace_id")
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?;
    if let Some(ws) = workspace {
        w.emit(steve_types::ConfigEvent::Bot {
            ws: ws.into(),
            id: id.into(),
        });
    }
    Ok(())
}

/// Remove a non-main bot's membership from one workspace without deleting the bot itself.
pub fn remove_workspace(w: &mut W<'_>, id: &str, ws: &str) -> Result<bool> {
    use crate::contract::Q;
    let is_main: Option<bool> = w.one(
        Query::select()
            .column("main")
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?;
    if is_main == Some(true) {
        return Ok(false);
    }
    let deleted = w.exec(
        Query::delete()
            .from_table("bot_workspaces")
            .and_where(Expr::col("bot_id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    if deleted > 0 {
        w.emit(steve_types::ConfigEvent::Bot {
            ws: ws.into(),
            id: id.into(),
        });
    }
    Ok(deleted > 0)
}

pub fn named_bots(c: &mut impl Read, ws: &str, name: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns([("b", "id")])
            .from_as("bots", "b")
            .join_as(
                JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .and_where(
                Expr::col(("b", "id"))
                    .eq(name)
                    .or(Expr::col(("b", "name")).eq(name)),
            )
            .order_by_expr(Expr::col(("b", "id")).eq(name), Order::Desc)
            .limit(2),
        |r| r.get(0),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct BotInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub rules: String,
}

pub fn for_judgement(
    read: &mut impl Read,
    ws: &str,
    configured: Option<&str>,
) -> Result<Option<BotInfo>> {
    let mut q = Query::select();
    q.columns([("b", "id"), ("b", "name"), ("b", "role"), ("b", "rules")])
        .from_as("bots", "b")
        .join_as(
            JoinType::InnerJoin,
            "bot_workspaces",
            "w",
            Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
        )
        .and_where(Expr::col(("w", "workspace_id")).eq(ws))
        .and_where(Expr::col(("b", "deleted")).eq(0_i64))
        .limit(1);
    if let Some(id) = configured {
        q.and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "enabled")).eq(1_i64));
    } else {
        q.and_where(Expr::col(("b", "main")).eq(1_i64))
            .order_by(("b", "updated"), Order::Asc);
    }
    read.one(&q, |r| {
        Ok(BotInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            role: r.get(2)?,
            rules: r.get(3)?,
        })
    })
}

/// The workspace's bot `id` (`bot_workspaces`): `(name, role, rules)`.
pub fn bot(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
    c.one(
        Query::select()
            .columns([("b", "name"), ("b", "role"), ("b", "rules")])
            .from_as("bots", "b")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
            )
            .and_where(Expr::col(("w", "workspace_id")).eq(ws))
            .and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
}

/// The main bot (the default drafter when there is no mail bot).
pub fn workspace_main_bot(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns([("b", "id")])
            .from_as("bots", "b")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
            )
            .and_where(Expr::col(("w", "workspace_id")).eq(ws))
            .and_where(Expr::col(("b", "main")).eq(1_i64))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .order_by(("b", "updated"), Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub(crate) fn model(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(c.one(
        Query::select()
            .column("model")
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )?
    .flatten())
}

pub fn fill_missing_harness(w: &mut W<'_>, id: &str, harness: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("bots")
            .value("harness", harness)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("harness").eq("")),
    )?;
    Ok(())
}

pub struct SeededHarness {
    pub harness: String,
    pub workspace: String,
}

pub fn pristine_harness(c: &mut impl Read, id: &str) -> Result<Option<SeededHarness>> {
    c.one(
        Query::select()
            .columns(["harness", "workspace_id"])
            .from("bots")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("deleted").eq(0_i64))
            .and_where(Expr::col("revision").eq(1_i64))
            .and_where(Expr::col("model").is_null())
            .and_where(Expr::col("reasoning_effort").eq("default")),
        |r| {
            Ok(SeededHarness {
                harness: r.get(0)?,
                workspace: r.get(1)?,
            })
        },
    )
}

pub fn move_pristine_harness(
    w: &mut W<'_>,
    id: &str,
    harness: &str,
    permission: &str,
    at: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("bots")
            .value("harness", harness)
            .value("perm_mode", permission)
            .value("updated", at)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("revision").eq(1_i64)),
    )? > 0)
}
