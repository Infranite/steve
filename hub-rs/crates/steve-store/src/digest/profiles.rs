use sea_query::{Expr, ExprTrait, OnConflict, Query};
use serde::{Deserialize, Serialize};

use crate::{R, Read, Result, W};

/// What is stored for a conversation (`None`: follows the kind).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub kind: Option<String>,
    pub kind_source: String,
    pub window: Option<i64>,
    pub quiet_ms: Option<i64>,
    pub max_wait_ms: Option<i64>,
    pub depth: Option<String>,
    pub daily_budget: Option<i64>,
    pub priority: Option<i64>,
    pub notify: Option<String>,
    pub notify_source: String,
    pub revision: i64,
}

pub struct Profiles<'a, C>(&'a mut C);

impl R<'_> {
    pub fn digest_profiles(&mut self) -> Profiles<'_, Self> {
        Profiles(self)
    }
}

impl W<'_> {
    pub fn digest_profiles(&mut self) -> Profiles<'_, Self> {
        Profiles(self)
    }
}

const SETTINGS: [&str; 11] = [
    "kind",
    "kind_source",
    "window_size",
    "quiet_ms",
    "max_wait_ms",
    "depth",
    "daily_budget",
    "priority",
    "notify",
    "notify_source",
    "revision",
];

impl<C: Read> Profiles<'_, C> {
    pub fn get(&mut self, ws: &str, conv: &str) -> Result<Option<Stored>> {
        self.0.one(
            Query::select()
                .columns(SETTINGS)
                .from("conv_profiles")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("conv").eq(conv)),
            |r| {
                Ok(Stored {
                    kind: r.get(0)?,
                    kind_source: r.get(1)?,
                    window: r.get(2)?,
                    quiet_ms: r.get(3)?,
                    max_wait_ms: r.get(4)?,
                    depth: r.get(5)?,
                    daily_budget: r.get(6)?,
                    priority: r.get(7)?,
                    notify: r.get(8)?,
                    notify_source: r.get(9)?,
                    revision: r.get(10)?,
                })
            },
        )
    }
}

impl Profiles<'_, W<'_>> {
    pub fn set(&mut self, ws: &str, conv: &str, p: &Stored, now: i64) -> Result<i64> {
        let revision = p.revision + 1;
        self.0.exec(
            Query::insert()
                .into_table("conv_profiles")
                .columns(
                    ["workspace_id", "conv"]
                        .into_iter()
                        .chain(SETTINGS)
                        .chain(["updated"]),
                )
                .values_panic([
                    ws.into(),
                    conv.into(),
                    p.kind.as_deref().into(),
                    p.kind_source.as_str().into(),
                    p.window.into(),
                    p.quiet_ms.into(),
                    p.max_wait_ms.into(),
                    p.depth.as_deref().into(),
                    p.daily_budget.into(),
                    p.priority.into(),
                    p.notify.as_deref().into(),
                    p.notify_source.as_str().into(),
                    revision.into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "conv"])
                        .update_columns(SETTINGS.into_iter().chain(["updated"]))
                        .to_owned(),
                ),
        )?;
        super::emit(self.0, ws, conv);
        Ok(revision)
    }
}
