use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Read, Result, W, now};

pub struct Systems<'a, C>(pub &'a mut C);

/// A `systems` row; `workspace_id = '*'` for the built-in ones.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct System {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    pub name: String,
    /// `comm` | `calendar` | `code` | `cloud` | `finance` | `steve`
    pub kind: String,
    /// The handle family: `email` | `phone` | `feishu_user` | … | `card` | `other`.
    pub family: String,
    pub plugin: Option<String>,
    pub handle_rule: Value,
    pub builtin: bool,
}

impl System {
    /// `handle_rule.normalize`: `email` | `phone` | `lower` | `none` (default).
    pub fn normalize(&self) -> &str {
        self.handle_rule
            .get("normalize")
            .and_then(Value::as_str)
            .unwrap_or("none")
    }
}

/// Built-in system codes and normalization rules.
const BUILTIN_SYSTEMS: [(&str, &str, &str, &str); 14] = [
    ("steve", "steve", "other", "none"),
    ("mail", "comm", "email", "email"),
    ("gmail", "comm", "email", "email"),
    ("outlook", "comm", "email", "email"),
    ("icloud", "comm", "email", "email"),
    ("feishu", "comm", "feishu_user", "none"),
    ("feishu_app", "comm", "feishu_app", "none"),
    ("slack", "comm", "slack_user", "none"),
    ("gcal", "calendar", "email", "email"),
    ("github", "code", "github_user", "lower"),
    ("aws", "cloud", "aws_account", "none"),
    ("phone", "comm", "phone", "phone"),
    ("bank", "finance", "card", "none"),
    ("feishu_tenant", "org", "feishu_tenant", "none"),
];

pub fn is_builtin_system(id: &str) -> bool {
    BUILTIN_SYSTEMS.iter().any(|system| system.0 == id)
}

/// The seeded label, used to render existing built-in rows in the request language.
pub fn builtin_system_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "steve" => "Steve",
        "mail" => "邮件",
        "gmail" => "Gmail",
        "outlook" => "Outlook",
        "icloud" => "iCloud",
        "feishu" => "飞书",
        "feishu_app" => "飞书应用",
        "slack" => "Slack",
        "gcal" => "Google 日历",
        "github" => "GitHub",
        "aws" => "AWS",
        "phone" => "电话",
        "bank" => "银行卡",
        "feishu_tenant" => "飞书租户",
        _ => return None,
    })
}

/// A built-in system before `seed_builtins` ran (a fresh workspace validating its first change):
/// the same row the seed will insert, so validate and apply agree.
fn builtin(id: &str) -> Option<System> {
    let name = builtin_system_name(id)?;
    BUILTIN_SYSTEMS
        .iter()
        .find(|b| b.0 == id)
        .map(|&(id, kind, family, norm)| System {
            id: id.into(),
            workspace_id: "*".into(),
            name: name.into(),
            kind: kind.into(),
            family: family.into(),
            plugin: Some(format!("builtin:{id}")),
            handle_rule: json!({ "normalize": norm }),
            builtin: true,
        })
}

const SYSTEM_COLS: [&str; 7] = [
    "id",
    "workspace_id",
    "name",
    "kind",
    "family",
    "plugin",
    "handle_rule",
];

fn system(row: &super::Row<'_>) -> Result<System> {
    let ws: String = row.get(1)?;
    let rule: Option<String> = row.get(6)?;
    Ok(System {
        id: row.get(0)?,
        builtin: ws == "*",
        workspace_id: ws,
        name: row.get(2)?,
        kind: row.get(3)?,
        family: row.get(4)?,
        plugin: row.get(5)?,
        handle_rule: rule
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| json!({})),
    })
}

fn systems(ws: &str) -> SelectStatement {
    Query::select()
        .from("systems")
        .columns(SYSTEM_COLS)
        .and_where(Expr::col("workspace_id").is_in(["*", ws]))
        .take()
}

impl<C: Read> Systems<'_, C> {
    pub fn seeded(&mut self) -> Result<bool> {
        let count: i64 = self
            .0
            .one(
                Query::select()
                    .from("systems")
                    .expr(Func::count(Expr::col("id")))
                    .and_where(Expr::col("workspace_id").eq("*"))
                    .and_where(Expr::col("plugin").like("builtin:%")),
                |row| row.get(0),
            )?
            .unwrap_or(0);
        Ok(count >= BUILTIN_SYSTEMS.len() as i64)
    }

    pub fn list(&mut self, ws: &str) -> Result<Vec<System>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            systems(ws)
                .order_by_expr(
                    Expr::col("workspace_id").ne(Expr::Constant("*".into())),
                    Order::Asc,
                )
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc),
            system,
        )
    }

    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<System>> {
        Ok(self
            .0
            .one(
                systems(ws)
                    .and_where(Expr::col("id").eq(id))
                    .order_by_expr(
                        Expr::col("workspace_id").eq(Expr::Constant("*".into())),
                        Order::Asc,
                    )
                    .limit(1),
                system,
            )?
            .or_else(|| builtin(id)))
    }
}

impl Systems<'_, W<'_>> {
    pub fn seed(&mut self) -> Result<()> {
        let at = now();
        for (id, kind, family, norm) in BUILTIN_SYSTEMS {
            let name = builtin_system_name(id).expect("built-in system label");
            self.0.exec(
                Query::insert()
                    .into_table("systems")
                    .columns(SYSTEM_COLS.into_iter().chain(["created"]))
                    .values_panic([
                        id.into(),
                        "*".into(),
                        name.into(),
                        kind.into(),
                        family.into(),
                        format!("builtin:{id}").into(),
                        json!({"normalize":norm}).to_string().into(),
                        at.into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["workspace_id", "id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }

    pub fn upsert_declared(&mut self, id: &str, name: &str, ext: &str, at: i64) -> Result<()> {
        let plug = Expr::col(("systems", "plugin"));
        let changed = plug
            .clone()
            .is_null()
            .or(plug.ne(Expr::col(("excluded", "plugin"))))
            .or(Expr::col(("systems", "name")).ne(Expr::col(("excluded", "name"))));
        self.0.exec(
            Query::insert()
                .into_table("systems")
                .columns(SYSTEM_COLS.into_iter().chain(["created"]))
                .values_panic([
                    id.into(),
                    "*".into(),
                    name.into(),
                    "comm".into(),
                    id.into(),
                    format!("ext:{ext}").into(),
                    json!({"normalize":"none"}).to_string().into(),
                    at.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "id"])
                        .update_columns(["name", "plugin"])
                        .action_and_where(changed)
                        .to_owned(),
                ),
        )?;
        Ok(())
    }
}
