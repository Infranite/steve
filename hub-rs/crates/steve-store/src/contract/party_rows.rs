use sea_query::{
    CommonTableExpression, Cond, Expr, ExprTrait, Func, JoinType, LikeExpr, Order, Query,
    SelectStatement, UnionType, WithClause,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    Contacts, Read, Result, W,
    base::{Viewer, Visibility},
    now, uid,
};

fn json_or<T: DeserializeOwned>(value: Option<String>, default: T) -> T {
    value
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(default)
}

/// A `parties` row: a contact, an organization or a team.
#[derive(Debug, Clone, PartialEq)]
pub struct Party {
    pub id: String,
    pub workspace_id: String,
    /// `contact` | `org` | `team`
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub avatar: Value,
    pub rels: Vec<String>,
    pub important: bool,
    pub note: String,
    /// contacts: `real` | `virtual` | `unconfirmed`
    pub contact_kind: Option<String>,
    /// organizations: `self` | `customer` | `supplier` | `partner` | `other`
    pub org_kind: Option<String>,
    /// Set only on “我”.
    pub user_id: Option<String>,
    pub visibility: Visibility,
    /// `active` | `merged` | `deleted` | `dissolved`
    pub status: String,
    pub merged_into: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
    pub revision: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl Party {
    pub fn is_me(&self) -> bool {
        self.user_id.is_some()
    }

    pub fn is_self_org(&self) -> bool {
        self.org_kind.as_deref() == Some("self")
    }
}

/// A party to insert.
#[derive(Debug, Clone)]
pub struct NewParty {
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub rels: Vec<String>,
    pub important: bool,
    pub note: String,
    pub contact_kind: Option<String>,
    pub org_kind: Option<String>,
    pub user_id: Option<String>,
    pub visibility: Visibility,
    pub source: String,
    pub source_ref: Option<String>,
    pub created_by: String,
}

impl NewParty {
    pub fn new(kind: &str, name: &str, created_by: &str) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            aliases: Vec::new(),
            rels: Vec::new(),
            important: false,
            note: String::new(),
            contact_kind: None,
            org_kind: None,
            user_id: None,
            visibility: Visibility::Workspace,
            source: "manual".into(),
            source_ref: None,
            created_by: created_by.into(),
        }
    }
}

/// Id prefix per party type.
fn prefix(kind: &str) -> &'static str {
    match kind {
        "org" => "org",
        "team" => "tm",
        _ => "ct",
    }
}

pub(super) fn party(r: &super::Row<'_>) -> Result<Party> {
    Ok(Party {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        kind: r.get(2)?,
        name: r.get(3)?,
        aliases: json_or(r.get(4)?, Vec::new()),
        avatar: json_or(r.get(5)?, Value::Object(Default::default())),
        rels: json_or(r.get(6)?, Vec::new()),
        important: r.get(7)?,
        note: r.get(8)?,
        contact_kind: r.get(9)?,
        org_kind: r.get(10)?,
        user_id: r.get(11)?,
        visibility: Visibility::from_db(&r.get::<String>(12)?),
        status: r.get(13)?,
        merged_into: r.get(14)?,
        source: r.get(15)?,
        source_ref: r.get(16)?,
        revision: r.get(17)?,
        created_by: r.get(18)?,
        created: r.get(19)?,
        updated: r.get(20)?,
    })
}

pub(super) const PARTY_COLUMNS: [&str; 21] = [
    "id",
    "workspace_id",
    "type",
    "name",
    "aliases",
    "avatar",
    "rels",
    "important",
    "note",
    "contact_kind",
    "org_kind",
    "user_id",
    "visibility",
    "status",
    "merged_into",
    "source",
    "source_ref",
    "revision",
    "created_by",
    "created",
    "updated",
];

fn parties() -> SelectStatement {
    Query::select()
        .from("parties")
        .columns(PARTY_COLUMNS)
        .take()
}

impl<C: Read> Contacts<'_, C> {
    pub fn party(&mut self, id: &str) -> Result<Option<Party>> {
        self.0
            .one(parties().and_where(Expr::col("id").eq(id)), party)
    }

    pub fn by_ids(&mut self, ids: &[&str]) -> Result<std::collections::HashMap<String, Party>> {
        let mut result = std::collections::HashMap::new();
        for batch in ids.chunks(500) {
            for row in self.0.all(
                parties().and_where(Expr::col("id").is_in(batch.iter().copied())),
                party,
            )? {
                result.insert(row.id.clone(), row);
            }
        }
        Ok(result)
    }

    pub fn own_org(&mut self, ws: &str) -> Result<Option<Party>> {
        self.0.one(
            parties()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("type").eq("org"))
                .and_where(Expr::col("org_kind").eq("self"))
                .and_where(Expr::col("status").eq("active"))
                .order_by("created", Order::Asc)
                .limit(1),
            party,
        )
    }

    pub fn me(&mut self, ws: &str, user: &str) -> Result<Option<Party>> {
        self.0.one(
            parties()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user)),
            party,
        )
    }
}

impl Contacts<'_, W<'_>> {
    pub fn insert(&mut self, ws: &str, row: &NewParty) -> Result<Party> {
        let id = uid(prefix(&row.kind));
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("parties")
                .columns([
                    "id",
                    "workspace_id",
                    "type",
                    "name",
                    "aliases",
                    "rels",
                    "important",
                    "note",
                    "contact_kind",
                    "org_kind",
                    "user_id",
                    "visibility",
                    "source",
                    "source_ref",
                    "created_by",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    row.kind.as_str().into(),
                    row.name.as_str().into(),
                    serde_json::to_string(&row.aliases)?.into(),
                    serde_json::to_string(&row.rels)?.into(),
                    i64::from(row.important).into(),
                    row.note.as_str().into(),
                    row.contact_kind.as_deref().into(),
                    row.org_kind.as_deref().into(),
                    row.user_id.as_deref().into(),
                    row.visibility.code().into(),
                    row.source.as_str().into(),
                    row.source_ref.as_deref().into(),
                    row.created_by.as_str().into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        crate::repo::must(self.party(&id)?)
    }
}

/// Columns a party edit may set; `None` leaves the column as it was.
#[derive(Debug, Clone, Default)]
pub struct PartyPatch {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub rels: Option<Vec<String>>,
    pub important: Option<bool>,
    pub note: Option<String>,
    pub contact_kind: Option<String>,
    pub org_kind: Option<String>,
    pub visibility: Option<Visibility>,
    pub status: Option<String>,
}

fn party_update(id: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("parties")
        .and_where(Expr::col("id").eq(id))
        .take()
}

impl Contacts<'_, W<'_>> {
    pub fn update(&mut self, id: &str, p: &PartyPatch, revision: Option<i64>) -> Result<bool> {
        let columns = [
            ("name", p.name.as_deref().map(Expr::value)),
            (
                "aliases",
                p.aliases
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .map(Expr::value),
            ),
            (
                "rels",
                p.rels
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .map(Expr::value),
            ),
            (
                "important",
                p.important.map(|flag| Expr::value(i64::from(flag))),
            ),
            ("note", p.note.as_deref().map(Expr::value)),
            ("contact_kind", p.contact_kind.as_deref().map(Expr::value)),
            ("org_kind", p.org_kind.as_deref().map(Expr::value)),
            (
                "visibility",
                p.visibility.as_ref().map(|value| Expr::value(value.code())),
            ),
            ("status", p.status.as_deref().map(Expr::value)),
            ("revision", revision.map(Expr::value)),
        ];
        if columns.iter().all(|(_, value)| value.is_none()) {
            return Ok(false);
        }
        self.0.exec(
            party_update(id)
                .values(
                    columns
                        .into_iter()
                        .filter_map(|(column, value)| value.map(|expr| (column, expr))),
                )
                .value("updated", now()),
        )?;
        Ok(true)
    }

    pub fn set_source(&mut self, id: &str, source: &str) -> Result<()> {
        self.0.exec(
            party_update(id)
                .value("source", source)
                .value("updated", now()),
        )?;
        Ok(())
    }

    pub fn set_avatar(&mut self, id: &str, avatar: &Value) -> Result<()> {
        self.0.exec(
            party_update(id)
                .value("avatar", avatar.to_string())
                .value("updated", now()),
        )?;
        Ok(())
    }

    pub fn set_merged_into(&mut self, id: &str, into: Option<&str>) -> Result<()> {
        self.0.exec(
            party_update(id)
                .value("merged_into", into)
                .value("updated", now()),
        )?;
        Ok(())
    }

    pub fn touch(&mut self, id: &str) -> Result<()> {
        self.0.exec(party_update(id).value("updated", now()))?;
        Ok(())
    }

    pub fn mark_directory_source(
        &mut self,
        id: &str,
        source: &str,
        source_ref: &str,
    ) -> Result<()> {
        self.0.exec(
            party_update(id)
                .value("source", source)
                .value("source_ref", source_ref),
        )?;
        Ok(())
    }

    pub fn purge(&mut self, ws: &str, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("parties")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_domains(&mut self, ws: &str, org: &str, domains: &[String]) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("org_domains")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("org_id").eq(org)),
        )?;
        let at = now();
        for (index, domain) in domains.iter().enumerate() {
            self.0.exec(
                Query::insert()
                    .into_table("org_domains")
                    .columns(["workspace_id", "domain", "org_id", "created"])
                    .values_panic([
                        ws.into(),
                        domain.as_str().into(),
                        org.into(),
                        (at + index as i64).into(),
                    ]),
            )?;
        }
        Ok(())
    }
}

/// Filters of the contact list. Zero limit returns no rows; a negative limit returns every match.
#[derive(Debug, Clone, Default)]
pub struct ContactQuery {
    pub q: Option<String>,
    pub rel: Option<String>,
    /// Defaults to contacts other than unconfirmed ones.
    pub kind: Option<String>,
    pub org: Option<String>,
    pub tag: Option<String>,
    /// Account handle matches, resolved by the account repository.
    pub handle_ids: Vec<String>,
    pub limit: i64,
}

fn substring(query: &str) -> LikeExpr {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for ch in query.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    LikeExpr::new(pattern).escape('\\')
}

fn active_parties(viewer: &Viewer, kind: &str) -> SelectStatement {
    let mut q = Query::select();
    q.from_as("parties", "p")
        .columns(PARTY_COLUMNS.map(|name| ("p", name)))
        .and_where(Expr::col(("p", "workspace_id")).eq(viewer.ws.as_str()))
        .and_where(Expr::col(("p", "status")).eq("active"))
        .and_where(Expr::col(("p", "type")).eq(kind));
    if viewer.is_service() {
        q.and_where(Expr::col(("p", "visibility")).eq("workspace"));
    }
    q.take()
}

fn job_to_party(dialect: super::Dialect) -> Expr {
    Expr::col(("l", "workspace_id"))
        .equals(("p", "workspace_id"))
        .and(Expr::col(("l", "subject_type")).eq("party"))
        .and(Expr::col(("l", "subject_id")).equals(("p", "id")))
        .and(Expr::col(("l", "predicate")).eq("works_at"))
        .and(Expr::col(("l", "object_type")).eq("party"))
        .and(
            dialect
                .json_field(Expr::col(("l", "props")), "to")
                .is_null(),
        )
}

fn jobs(dialect: super::Dialect) -> SelectStatement {
    Query::select()
        .from_as("links", "l")
        .column(("l", "object_id"))
        .and_where(job_to_party(dialect))
        .take()
}

fn json_match(dialect: super::Dialect, column: &'static str, predicate: Expr) -> Expr {
    Expr::exists(
        Query::select()
            .column(("arr", "value"))
            .from_function(dialect.json_array_elements(Expr::col(("p", column))), "arr")
            .and_where(predicate)
            .take(),
    )
}

impl<C: Read> Contacts<'_, C> {
    pub fn active(
        &mut self,
        viewer: &Viewer,
        kinds: &[&str],
        confirmed_only: bool,
    ) -> Result<Vec<Party>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let mut q = parties();
        q.and_where(Expr::col("workspace_id").eq(&viewer.ws))
            .and_where(Expr::col("status").eq("active"))
            .and_where(Expr::col("type").is_in(kinds.iter().copied()))
            .order_by("important", Order::Desc)
            .order_by("updated", Order::Desc)
            .order_by("id", Order::Asc);
        if viewer.is_service() {
            q.and_where(Expr::col("visibility").eq("workspace"));
        }
        if confirmed_only {
            q.and_where(
                Func::coalesce([Expr::col("contact_kind"), Expr::value("")]).ne("unconfirmed"),
            );
        }
        self.0.all(&q, party)
    }

    pub fn current_party(&mut self, id: &str) -> Result<Option<Party>> {
        let mut party = self.party(id)?;
        for _ in 0..32 {
            let Some(next) = party
                .as_ref()
                .filter(|p| p.status == "merged")
                .and_then(|p| p.merged_into.as_deref())
            else {
                break;
            };
            party = self.party(next)?;
        }
        Ok(party)
    }

    pub fn merged_party_ids(&mut self, id: &str) -> Result<Vec<String>> {
        let attached = Query::select()
            .from_as("parties", "p")
            .column(("p", "id"))
            .join(
                JoinType::InnerJoin,
                "merged",
                Expr::col(("merged", "id")).equals(("p", "merged_into")),
            )
            .take();
        let root = Query::select()
            .from("parties")
            .column("id")
            .and_where(Expr::col("id").eq(id))
            .union(UnionType::Distinct, attached)
            .take();
        let cte = CommonTableExpression::new()
            .table_name("merged")
            .column("id")
            .query(root)
            .to_owned();
        let with = WithClause::new().recursive(true).cte(cte).to_owned();
        let query = Query::select()
            .from("merged")
            .column("id")
            .take()
            .with(with);
        self.0.all(&query, |row| row.get(0))
    }

    pub fn domain_owner(&mut self, ws: &str, domain: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("org_domains")
                .column("org_id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("domain").eq(domain)),
            |row| row.get(0),
        )
    }

    pub fn domains(&mut self, ws: &str, org: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .from("org_domains")
                .column("domain")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("org_id").eq(org))
                .order_by("created", Order::Asc)
                .order_by("domain", Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn has_audience(&mut self, ws: &str, audience: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("parties")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("visibility").eq(audience))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn organizations(&mut self, viewer: &Viewer) -> Result<Vec<Party>> {
        self.0.all(
            active_parties(viewer, "org")
                .order_by_expr(
                    Expr::col(("p", "org_kind")).eq(Expr::Constant("self".into())),
                    Order::Desc,
                )
                .order_by(("p", "name"), Order::Asc)
                .order_by(("p", "id"), Order::Asc),
            party,
        )
    }

    pub fn organization_people(
        &mut self,
        viewer: &Viewer,
        org: &str,
    ) -> Result<Vec<(String, String, Option<String>, Value)>> {
        let dialect = self.0.db().dialect();
        let mut q = active_parties(viewer, "contact");
        q.clear_selects()
            .columns([("p", "id"), ("p", "name")])
            .expr(dialect.json_field(Expr::col(("l", "props")), "title"))
            .column(("p", "avatar"))
            .join_as(
                JoinType::InnerJoin,
                "links",
                "l",
                job_to_party(dialect).and(Expr::col(("l", "object_id")).eq(org)),
            )
            .order_by(("p", "name"), Order::Asc)
            .order_by(("p", "id"), Order::Asc);
        self.0.all(&q, |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                json_or(row.get(3)?, Value::Object(Default::default())),
            ))
        })
    }

    pub fn contacts(&mut self, viewer: &Viewer, filters: &ContactQuery) -> Result<Vec<Party>> {
        let dialect = self.0.db().dialect();
        let mut q = active_parties(viewer, "contact");
        let kind = Expr::col(("p", "contact_kind"));
        q.and_where(match &filters.kind {
            Some(value) => kind.eq(value),
            None => Func::coalesce([kind, Expr::value("")]).ne("unconfirmed"),
        });
        if let Some(rel) = &filters.rel {
            q.and_where(json_match(
                dialect,
                "rels",
                Expr::col(("arr", "value")).eq(rel),
            ));
        }
        if let Some(org) = &filters.org {
            q.and_where(Expr::exists(
                jobs(dialect)
                    .and_where(Expr::col(("l", "object_id")).eq(org))
                    .take(),
            ));
        }
        if let Some(tag) = &filters.tag {
            q.and_where(Expr::exists(
                Query::select()
                    .from_as("entity_tags", "t")
                    .column(("t", "object_id"))
                    .and_where(Expr::col(("t", "workspace_id")).equals(("p", "workspace_id")))
                    .and_where(Expr::col(("t", "object_type")).eq("party"))
                    .and_where(Expr::col(("t", "object_id")).equals(("p", "id")))
                    .and_where(Expr::col(("t", "tag_id")).eq(tag))
                    .take(),
            ));
        }
        if let Some(text) = filters.q.as_deref().filter(|q| !q.is_empty()) {
            let pattern = substring(text);
            let org_match = jobs(dialect)
                .join_as(
                    JoinType::InnerJoin,
                    "parties",
                    "o",
                    Expr::col(("o", "id")).equals(("l", "object_id")),
                )
                .and_where(
                    Expr::col(("o", "name")).like(pattern.clone()).or(dialect
                        .json_field(Expr::col(("l", "props")), "title")
                        .like(pattern.clone())),
                )
                .take();
            let mut matches = Cond::any()
                .add(Expr::col(("p", "name")).like(pattern.clone()))
                .add(json_match(
                    dialect,
                    "aliases",
                    Expr::col(("arr", "value")).like(pattern),
                ))
                .add(Expr::exists(org_match));
            if !filters.handle_ids.is_empty() {
                matches = matches.add(
                    Expr::col(("p", "id")).is_in(filters.handle_ids.iter().map(String::as_str)),
                );
            }
            q.cond_where(matches);
        }
        q.order_by(("p", "important"), Order::Desc)
            .order_by(("p", "name"), Order::Asc)
            .order_by(("p", "id"), Order::Asc);
        if let Ok(limit) = u64::try_from(filters.limit) {
            q.limit(limit);
        }
        self.0.all(&q, party)
    }
}
