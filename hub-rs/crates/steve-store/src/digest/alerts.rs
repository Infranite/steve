use sea_query::{Expr, ExprTrait, Iden, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::{Contacts, Json, Q, Row};
use crate::{R, Read, Result, W};

#[derive(Iden, Clone, Copy)]
enum AlertRollups {
    Table,
    WorkspaceId,
    Conv,
    Category,
    Fired,
    Recovered,
    Open,
    Handlers,
    FirstAt,
    LastAt,
    LastId,
}
use AlertRollups as A;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Alert {
    pub category: String,
    pub fired: i64,
    pub recovered: i64,
    pub open: bool,
    pub handlers: Vec<String>,
    pub first_at: i64,
    pub last_at: i64,
    pub last_id: String,
}

#[derive(Debug, Serialize)]
pub struct Rollup {
    #[serde(flatten)]
    pub alert: Alert,
    pub handler_names: Vec<String>,
}

pub struct Alerts<'a, C>(&'a mut C);

impl R<'_> {
    pub fn digest_alerts(&mut self) -> Alerts<'_, Self> {
        Alerts(self)
    }
}

impl W<'_> {
    pub fn digest_alerts(&mut self) -> Alerts<'_, Self> {
        Alerts(self)
    }
}

const FIELDS: [A; 8] = [
    A::Category,
    A::Fired,
    A::Recovered,
    A::Open,
    A::Handlers,
    A::FirstAt,
    A::LastAt,
    A::LastId,
];

fn select(ws: &str, conv: &str) -> SelectStatement {
    Query::select()
        .columns(FIELDS)
        .from(A::Table)
        .and_where(Expr::col(A::WorkspaceId).eq(ws))
        .and_where(Expr::col(A::Conv).eq(conv))
        .to_owned()
}

fn alert(row: &Row<'_>) -> Result<Alert> {
    Ok(Alert {
        category: row.get(0)?,
        fired: row.get(1)?,
        recovered: row.get(2)?,
        open: row.get(3)?,
        handlers: row.get::<Json<_>>(4)?.0,
        first_at: row.get(5)?,
        last_at: row.get(6)?,
        last_id: row.get(7)?,
    })
}

impl<C: Read> Alerts<'_, C> {
    /// At most 30 categories, newest first, with the names of their handlers.
    pub fn rollup(&mut self, ws: &str, conv: &str) -> Result<Vec<Rollup>> {
        let alerts = self.0.all(
            select(ws, conv).order_by(A::LastAt, Order::Desc).limit(30),
            alert,
        )?;
        let names = Contacts(self.0).party_names(
            alerts
                .iter()
                .flat_map(|a| a.handlers.iter().map(String::as_str)),
        )?;
        Ok(alerts
            .into_iter()
            .map(|alert| Rollup {
                handler_names: alert
                    .handlers
                    .iter()
                    .filter_map(|id| names.get(id).cloned())
                    .collect(),
                alert,
            })
            .collect())
    }
}

impl Alerts<'_, W<'_>> {
    pub fn apply_window(
        &mut self,
        ws: &str,
        conv: &str,
        alerts: impl IntoIterator<Item = Alert>,
    ) -> Result<()> {
        for mut current in alerts {
            if let Some(old) = self.0.one(
                select(ws, conv).and_where(Expr::col(A::Category).eq(current.category.as_str())),
                alert,
            )? {
                current.fired += old.fired;
                current.recovered += old.recovered;
                current.first_at = old.first_at;
                let mut handlers = old.handlers;
                for id in current.handlers {
                    if !handlers.contains(&id) {
                        handlers.push(id);
                    }
                }
                current.handlers = handlers;
            }
            self.0.exec(
                Query::insert()
                    .into_table(A::Table)
                    .columns([A::WorkspaceId, A::Conv].into_iter().chain(FIELDS))
                    .values_panic([
                        ws.into(),
                        conv.into(),
                        current.category.into(),
                        current.fired.into(),
                        current.recovered.into(),
                        current.open.into(),
                        serde_json::to_string(&current.handlers)?.into(),
                        current.first_at.into(),
                        current.last_at.into(),
                        current.last_id.into(),
                    ])
                    .on_conflict(
                        OnConflict::columns([A::WorkspaceId, A::Conv, A::Category])
                            .update_columns(FIELDS.into_iter().skip(1))
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }
}
