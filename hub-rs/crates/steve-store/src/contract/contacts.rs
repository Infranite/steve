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
