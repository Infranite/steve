//! Registry-owned query dispatch and reversible outbox mappings. Transfer capability
//! is explicit: registering a subset of a legacy domain never enables its export.
use super::*;
use crate::{scan, transfer::TransferState};
use serde::{Deserialize, Serialize};
use steve_storage_api::query::{
    CursorGet, Get, IndexName, KeyRange, Order, Pred, ResumeToken, ScanBudget,
};
pub(super) type QueryHandler =
    dyn Fn(&Connection, &AnyQuery) -> Result<RawJson, Error> + Send + Sync;
pub(super) type FactDecoder = fn(&RawJson) -> Result<FactRecord, CommitError>;
pub(super) fn builtin_queries() -> BTreeMap<(Domain, String), Arc<QueryHandler>> {
    [Domain::Core, Domain::Stream, Domain::Comms]
        .into_iter()
        .map(|domain| {
            let run: Arc<QueryHandler> = Arc::new(|c, q| {
                let body: CursorGet = q.body.decode()?;
                if body.domain != q.domain {
                    return Err(Error::from(invalid("mixed-domain cursor")));
                }
                let table = table(c, q.domain, "cursor")?;
                let position: Option<String> = c
                    .query_row(
                        &format!("SELECT key FROM {table} WHERE domain=?1 AND name=?2"),
                        params![domain_name(q.domain), body.name.0],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sql_error)
                    .map_err(Error::from)?;
                let position: Option<cursor::Position> = position
                    .map(|p| {
                        serde_json::from_str(&p)
                            .map_err(|_| Error::from(invalid("invalid stored cursor")))
                    })
                    .transpose()?;
                query::encode(&position)
            });
            ((domain, "cursor_get".into()), run)
        })
        .collect()
}
/// Qualify shared tables on core-main readers; physical writers select their own main.
pub(crate) fn table(c: &Connection, domain: Domain, name: &str) -> Result<String, Error> {
    let file: Option<String> = c
        .query_row(
            "SELECT value FROM main.meta WHERE domain=?1 AND key='file'",
            [domain_name(domain)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)
        .map_err(Error::from)?;
    let mut alias = "main".to_string();
    if let Some(file) = file {
        let mut stmt = c
            .prepare("PRAGMA database_list")
            .map_err(sql_error)
            .map_err(Error::from)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(sql_error)
            .map_err(Error::from)?;
        alias = rows
            .filter_map(|r| r.ok())
            .find(|(_, p)| {
                std::path::Path::new(p)
                    .file_name()
                    .is_some_and(|n| n == file.as_str())
            })
            .map(|(a, _)| a)
            .ok_or_else(|| Error::from(invalid("domain file is not attached")))?;
    }
    Ok(format!("{}.{}", quote(&alias), quote(name)))
}
#[derive(Deserialize, Serialize)]
struct Window {
    index: IndexName,
    range: KeyRange,
    filter: Vec<Pred>,
    budget: ScanBudget,
    #[serde(default)]
    order: Option<Order>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    resume: Option<ResumeToken>,
}
impl Registry {
    pub fn register_query(
        &mut self,
        name: impl Into<String>,
        domain: Domain,
        run: impl Fn(&Connection, &AnyQuery) -> Result<RawJson, Error> + Send + Sync + 'static,
    ) -> Result<(), CommitError> {
        let name = name.into();
        if name.is_empty()
            || name.contains('\0')
            || matches!(name.as_str(), "get" | "scan" | "count" | "exists")
            || self.queries.contains_key(&(domain, name.clone()))
            || self.queries.keys().filter(|(d, _)| *d == domain).count()
                + self
                    .operations
                    .values()
                    .filter(|op| op.domain == domain)
                    .count()
                >= steve_storage_api::registry::DOMAIN_LIMIT
        {
            return Err(invalid("duplicate or excessive named query"));
        }
        self.queries.insert((domain, name), Arc::new(run));
        Ok(())
    }
    pub fn query_names(&self, domain: Domain) -> impl Iterator<Item = &str> {
        self.queries
            .keys()
            .filter(move |(d, _)| *d == domain)
            .map(|(_, n)| n.as_str())
    }
    /// Named queries have no entity. Their descriptor is solely a domain context for
    /// read_batch_with, never a declaration, SQL table, or member of the export set.
    pub fn query_context(&self, q: &AnyQuery) -> Result<EntityDesc, Error> {
        let entity = match q.name.as_str() {
            "get" | "exists" => Some(q.body.decode::<Get<Row>>()?.key.entity),
            "scan" | "count" => Some(q.body.decode::<Window>()?.index.entity),
            _ => None,
        };
        if let Some(name) = entity {
            return Ok(self
                .entity(&name, q.domain)
                .map_err(Error::from)?
                .desc
                .clone());
        }
        if !self.queries.contains_key(&(q.domain, q.name.clone())) {
            return Err(Error::from(unsupported("unknown named query")));
        }
        Ok(EntityDesc {
            name: String::new(),
            domain: q.domain,
            key: vec![],
            fields: vec![],
            indexes: vec![],
            kind: entity::EntityKind::Log,
            retention: None,
        })
    }
    pub fn query_on(&self, c: &Connection, mut q: AnyQuery) -> Result<AnyOutput, Error> {
        let context = self.query_context(&q)?;
        if context.name.is_empty() {
            let raw = (self.queries[&(q.domain, q.name.clone())])(c, &q)?;
            return Ok(AnyOutput {
                name: q.name,
                domain: q.domain,
                body: Payload::Json(raw),
            });
        }
        let e = self.entity(&context.name, q.domain).map_err(Error::from)?;
        let mut physical = e.desc.clone();
        physical.name = e.table.clone();
        if e.table != e.desc.name {
            q.body = Payload::Json(if matches!(q.name.as_str(), "get" | "exists") {
                let mut body: Get<Row> = q.body.decode()?;
                body.key.entity = e.table.clone();
                query::encode(&body)?
            } else {
                let mut body: Window = q.body.decode()?;
                body.index.entity = e.table.clone();
                query::encode(&body)?
            });
        }
        scan::query(c, &physical, q)
    }
    pub fn register_transfer_topic(
        &mut self,
        name: impl Into<String>,
        domain: Domain,
        adapt: FactAdapter,
        decode: FactDecoder,
    ) -> Result<(), CommitError> {
        let name = name.into();
        self.register_topic(name.clone(), domain, adapt)?;
        self.topics.get_mut(&name).unwrap().decode = Some(decode);
        Ok(())
    }
    /// Mark the registered domain set complete only after all entities and reversible
    /// fact mappings have been registered. Later registrations revoke this assertion.
    pub fn allow_export(&mut self, domain: Domain) -> Result<(), CommitError> {
        if !self.entities().any(|e| e.desc.domain == domain)
            || self
                .topics
                .values()
                .any(|t| t.domain == domain && t.decode.is_none())
        {
            return Err(unsupported("domain export declaration is incomplete"));
        }
        self.export_complete.insert(domain);
        Ok(())
    }
    pub fn can_export(&self, domain: Domain) -> bool {
        self.export_complete.contains(&domain)
    }
    pub fn export_state(&self, c: &Connection, domain: Domain) -> Result<TransferState, Error> {
        if !self.can_export(domain) {
            return Err(Error::from(unsupported(
                "domain export declaration is incomplete",
            )));
        }
        let mut state = crate::transfer::read_state(c, domain)?;
        let source = table(c, domain, "outbox_events")?;
        let mut topics = c
            .prepare(&format!("SELECT DISTINCT topic FROM {source}"))
            .map_err(sql_error)
            .map_err(Error::from)?;
        for topic in topics
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql_error)
            .map_err(Error::from)?
        {
            if !self
                .topics
                .contains_key(&topic.map_err(sql_error).map_err(Error::from)?)
            {
                return Err(Error::from(unsupported(
                    "outbox contains an unregistered fact topic",
                )));
            }
        }
        for (name, topic) in self.topics.iter().filter(|(_, t)| t.domain == domain) {
            let mut stmt = c
                .prepare(&format!(
                    "SELECT id,payload FROM {source} WHERE topic=?1 ORDER BY id COLLATE BINARY"
                ))
                .map_err(sql_error)
                .map_err(Error::from)?;
            let rows = stmt
                .query_map([name], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(sql_error)
                .map_err(Error::from)?;
            for row in rows {
                let (id, payload) = row.map_err(sql_error).map_err(Error::from)?;
                let raw = RawJson::from_str(&payload)
                    .map_err(|_| Error::from(invalid("invalid outbox payload")))?;
                let fact = topic
                    .decode
                    .ok_or_else(|| Error::from(unsupported("fact decoder missing")))?(
                    &raw
                )
                .map_err(Error::from)?;
                if fact.fact_id != id || fact.topic != *name {
                    return Err(Error::from(invalid("outbox mapping identity mismatch")));
                }
                state.facts.push(((domain, id), fact));
            }
        }
        state.facts.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(state)
    }
    pub fn restore_state(
        &self,
        c: &Connection,
        domain: Domain,
        state: &TransferState,
        now: Millis,
    ) -> Result<(), Error> {
        crate::transfer::write_common_state(c, domain, state)?;
        for (_, fact) in &state.facts {
            self.check_topic(&fact.topic, domain).map_err(Error::from)?;
            let mapped = (self.topics[&fact.topic].adapt)(fact).map_err(Error::from)?;
            let existing: Option<(String, String)> = c
                .query_row(
                    "SELECT topic,payload FROM outbox_events WHERE id=?1",
                    [&fact.fact_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql_error)
                .map_err(Error::from)?;
            let payload = raw_text(&mapped.payload).map_err(Error::from)?;
            if let Some((topic, prior)) = existing {
                if topic != fact.topic || prior != payload {
                    return Err(Error::from(invalid("import fact identity mismatch")));
                }
            } else {
                c.execute("INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created) VALUES(?1,?2,?3,?4,?5,?6)",params![fact.fact_id,mapped.workspace_id,fact.topic,mapped.version,payload,now.0]).map_err(sql_error).map_err(Error::from)?;
            }
        }
        Ok(())
    }
}
