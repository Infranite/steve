use std::collections::{BTreeSet, HashMap};

use sea_query::{Expr, ExprTrait, Iden, Query};

use crate::{R, Read, Result, W};

#[derive(Iden, Clone, Copy)]
enum Parties {
    Table,
    Id,
    Name,
    WorkspaceId,
}

pub struct Contacts<'a, C>(pub(crate) &'a mut C);

impl R<'_> {
    pub fn contacts(&mut self) -> Contacts<'_, Self> {
        Contacts(self)
    }
}

impl W<'_> {
    pub fn contacts(&mut self) -> Contacts<'_, Self> {
        Contacts(self)
    }
}

impl<C: Read> Contacts<'_, C> {
    pub fn account_owners<'a>(
        &mut self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, (String, Option<String>)>> {
        use sea_query::JoinType;
        let ids: Vec<_> = ids.into_iter().collect();
        let mut owners = HashMap::new();
        for batch in ids.chunks(500) {
            owners.extend(
                self.0.all(
                    Query::select()
                        .columns([
                            ("a", "id"),
                            ("a", "handle"),
                            ("a", "handle_display"),
                            ("p", "user_id"),
                        ])
                        .from_as("accounts", "a")
                        .join_as(
                            JoinType::LeftJoin,
                            "parties",
                            "p",
                            Expr::col(("p", "id")).equals(("a", "party_id")),
                        )
                        .and_where(Expr::col(("a", "id")).is_in(batch.iter().copied())),
                    |r| {
                        let handle: String = r.get(1)?;
                        let display = r
                            .get::<Option<String>>(2)?
                            .filter(|v| !v.is_empty())
                            .unwrap_or(handle);
                        Ok((r.get::<String>(0)?, (display, r.get::<Option<String>>(3)?)))
                    },
                )?,
            );
        }
        Ok(owners)
    }

    pub fn name_and_relationships(
        &mut self,
        ws: &str,
        id: &str,
    ) -> Result<Option<(String, Vec<String>)>> {
        self.0.one(
            Query::select()
                .columns(["name", "rels"])
                .from("parties")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |row| Ok((row.get(0)?, row.get::<super::Json<_>>(1)?.0)),
        )
    }

    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(Parties::Id)
                    .from(Parties::Table)
                    .and_where(Expr::col(Parties::WorkspaceId).eq(ws))
                    .and_where(Expr::col(Parties::Id).eq(id)),
                |_| Ok(()),
            )?
            .is_some())
    }

    /// Names for already resolved party ids. Parties are global identifiers.
    pub fn party_names<'a>(
        &mut self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, String>> {
        let ids = ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut names = HashMap::new();
        for ids in ids.chunks(500) {
            names.extend(
                self.0.all(
                    Query::select()
                        .columns([Parties::Id, Parties::Name])
                        .from(Parties::Table)
                        .and_where(Expr::col(Parties::Id).is_in(ids.iter().copied())),
                    |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
                )?,
            );
        }
        Ok(names)
    }
}

pub struct AccountIdentity {
    pub party: String,
    pub name: String,
    pub bot: bool,
}

impl<C: Read> Contacts<'_, C> {
    pub fn new(handle: &mut C) -> Contacts<'_, C> {
        Contacts(handle)
    }

    pub fn user_party(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("id")
                .from("parties")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user))
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn account_identities<'a>(
        &mut self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, AccountIdentity>> {
        type Account = (String, String, String, String, String);
        let ids: Vec<_> = ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut rows: Vec<Account> = Vec::new();
        for batch in ids.chunks(500) {
            rows.extend(
                self.0.all(
                    Query::select()
                        .columns(["id", "party_id", "display_name", "handle_display", "kind"])
                        .from("accounts")
                        .and_where(Expr::col("id").is_in(batch.iter().copied())),
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?,
            );
        }
        let parties = self.party_names(
            rows.iter()
                .filter(|row| row.2.is_empty())
                .map(|row| row.1.as_str()),
        )?;
        Ok(rows
            .into_iter()
            .map(|(id, party, display, handle, kind)| {
                let name = if display.is_empty() {
                    parties
                        .get(&party)
                        .filter(|name| !name.is_empty())
                        .cloned()
                        .unwrap_or(handle)
                } else {
                    display
                };
                (
                    id,
                    AccountIdentity {
                        party,
                        name,
                        bot: matches!(kind.as_str(), "app_bot" | "bot"),
                    },
                )
            })
            .collect())
    }
}

pub struct PartyName {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub user_id: Option<String>,
}

impl<C: Read> Contacts<'_, C> {
    pub fn names(
        &mut self,
        viewer: &crate::base::Viewer,
        kinds: &[&str],
    ) -> Result<Vec<PartyName>> {
        let mut q = Query::select();
        q.columns(["id", "type", "name", "aliases", "user_id"])
            .from("parties")
            .and_where(Expr::col("workspace_id").eq(viewer.ws.as_str()))
            .and_where(Expr::col("status").eq("active"))
            .and_where(Expr::col("type").is_in(kinds.iter().copied()))
            .order_by("important", sea_query::Order::Desc)
            .order_by("updated", sea_query::Order::Desc)
            .order_by("id", sea_query::Order::Asc);
        if viewer.is_service() {
            q.and_where(Expr::col("visibility").eq("workspace"));
        }
        self.0.all(&q, |row| {
            Ok(PartyName {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                aliases: row.get::<crate::contract::Json<_>>(3)?.0,
                user_id: row.get(4)?,
            })
        })
    }
}

impl<C: Read> Contacts<'_, C> {
    pub fn internal_address(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        let Some((id, name)) = self.0.one(
            Query::select()
                .columns(["id", "name"])
                .from("parties")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user))
                .limit(1),
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
        )?
        else {
            return Ok(None);
        };
        let values = self.0.all(
            Query::select()
                .column("value")
                .from("attr_values")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").eq(id))
                .and_where(Expr::col("key").eq("call"))
                .and_where(Expr::col("status").eq("confirmed"))
                .and_where(Expr::col("level").ne("never"))
                .order_by("updated", sea_query::Order::Desc)
                .order_by("created", sea_query::Order::Desc)
                .order_by("rowid", sea_query::Order::Desc),
            |row| row.get::<String>(0),
        )?;
        Ok(Some(
            values
                .into_iter()
                .find(|value| !value.trim().is_empty())
                .unwrap_or(name),
        ))
    }
}

impl<C: Read> Contacts<'_, C> {
    pub fn visible_parties<'a>(
        &mut self,
        viewer: &crate::base::Viewer,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<std::collections::HashSet<String>> {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut visible = std::collections::HashSet::new();
        for batch in ids.chunks(500) {
            let mut q = Query::select();
            q.from("parties")
                .column("id")
                .and_where(Expr::col("workspace_id").eq(viewer.ws.as_str()))
                .and_where(Expr::col("status").ne("deleted"))
                .and_where(Expr::col("id").is_in(batch.iter().copied()));
            if viewer.is_service() {
                q.and_where(Expr::col("visibility").eq("workspace"));
            }
            visible.extend(self.0.all(&q, |row| row.get::<String>(0))?);
        }
        Ok(visible)
    }
}
