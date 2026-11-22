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
