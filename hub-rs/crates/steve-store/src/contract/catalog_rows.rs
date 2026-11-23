use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result};

pub struct DeviceRows<'a, C>(pub &'a mut C);

pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    pub roles: Vec<String>,
}

impl<C: Read> DeviceRows<'_, C> {
    pub fn identities(&mut self) -> Result<Vec<DeviceIdentity>> {
        self.0.all(
            Query::select()
                .from("devices")
                .columns(["id", "name", "roles"])
                .order_by("created", Order::Asc),
            |row| {
                Ok(DeviceIdentity {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    roles: serde_json::from_str(&row.get::<String>(2)?).unwrap_or_default(),
                })
            },
        )
    }
}

pub struct PersonaRows<'a, C>(pub &'a mut C);

pub struct PersonaIdentity {
    pub id: String,
    pub name: String,
    pub intro: String,
}

impl<C: Read> PersonaRows<'_, C> {
    pub fn identities(&mut self, ws: &str) -> Result<Vec<PersonaIdentity>> {
        self.0.all(
            Query::select()
                .from("personas")
                .columns(["id", "name", "intro"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("deleted").eq(0_i64))
                .order_by("is_default", Order::Desc)
                .order_by("name", Order::Asc),
            |row| {
                Ok(PersonaIdentity {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    intro: row.get(2)?,
                })
            },
        )
    }
}
