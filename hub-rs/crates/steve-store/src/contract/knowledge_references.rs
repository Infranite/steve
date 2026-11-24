use crate::base::{ObjRef, Viewer, Visibility};
use crate::{MailSources, Members, Read, Result};
use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

pub struct References<'a, C>(pub &'a mut C);
pub struct ItemEvidence<'a, C>(pub &'a mut C);
pub struct SessionEvidence<'a, C>(pub &'a mut C);
pub struct MessageEvidence<'a, C>(pub &'a mut C);
pub struct FileReads<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
    /// True when the project is bound to a git repository (`projects.kind == "git"` with a path).
    pub has_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub id: String,
    pub workspace_id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

pub struct CompletionCard {
    pub kind: String,
    pub question: String,
    pub draft: String,
    pub status: String,
    pub result: serde_json::Value,
}

fn entity(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "party" => Some(("parties", "name")),
        "item" => Some(("items", "title")),
        "bot" => Some(("bots", "name")),
        "project" => Some(("projects", "name")),
        "space" => Some(("spaces", "name")),
        "matter" => Some(("matters", "title")),
        "persona" => Some(("personas", "name")),
        _ => None,
    }
}

fn by_id(table: &'static str, id: &str) -> SelectStatement {
    Query::select()
        .from(table)
        .and_where(Expr::col("id").eq(id))
        .take()
}

impl<C: Read> References<'_, C> {
    pub fn exists(&mut self, viewer: &Viewer, target: &ObjRef) -> Result<bool> {
        Ok(self.in_workspace(viewer, target)?.is_some())
    }

    pub fn in_workspace(&mut self, viewer: &Viewer, target: &ObjRef) -> Result<Option<String>> {
        match target.kind.as_str() {
            "me" => return Ok((target.id == viewer.user || target.id == "me").then(|| "我".into())),
            "mail_thread" => {
                return Ok(MailSources(self.0)
                    .thread(&viewer.ws, &target.id)?
                    .filter(|thread| thread.visible_to(viewer))
                    .map(|thread| thread.subject));
            }
            "bot" => {
                let q = Query::select()
                    .column(("b", "name"))
                    .from_as("bots", "b")
                    .join_as(
                        JoinType::InnerJoin,
                        "bot_workspaces",
                        "w",
                        Expr::col(("w", "bot_id")).equals(("b", "id")),
                    )
                    .and_where(Expr::col(("w", "workspace_id")).eq(viewer.ws.as_str()))
                    .and_where(Expr::col(("b", "id")).eq(target.id.as_str()))
                    .take();
                return self.0.one(&q, |r| r.get(0));
            }
            _ => (),
        }
        let Some((table, column)) = entity(&target.kind) else {
            return Ok(None);
        };
        self.0.one(
            by_id(table, &target.id)
                .column(column)
                .and_where(Expr::col("workspace_id").eq(viewer.ws.as_str())),
            |r| r.get(0),
        )
    }

    pub fn name(&mut self, target: &ObjRef) -> Result<Option<String>> {
        if target.kind == "me" {
            return Ok(Some("我".into()));
        }
        let Some((table, column)) = entity(&target.kind) else {
            return Ok(None);
        };
        self.0
            .one(by_id(table, &target.id).column(column), |r| r.get(0))
    }

    pub fn scope_exists(&mut self, ws: &str, visibility: &Visibility) -> Result<bool> {
        let (table, column, id) = match visibility {
            Visibility::Workspace | Visibility::Private => return Ok(true),
            Visibility::Team(id) => ("team_details", "party_id", id),
            Visibility::Space(id) => ("spaces", "id", id),
        };
        Ok(self
            .0
            .one(
                Query::select()
                    .column(column)
                    .from(table)
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col(column).eq(id.as_str())),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn counted_in(&mut self, ws: &str, kind: &str, id: &str) -> Result<bool> {
        let table = match kind {
            "session" => "sessions",
            "chat" => "messages",
            "draft" => "mail_drafts",
            "flow" => "flow_runs",
            _ => return Ok(false),
        };
        Ok(self
            .0
            .one(
                by_id(table, id)
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws)),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn projects(&mut self, ws: &str) -> Result<Vec<ProjectRef>> {
        self.0.all(
            Query::select()
                .columns(["id", "name", "kind", "path"])
                .from("projects")
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("name", Order::Asc)
                .order_by("id", Order::Asc),
            |r| {
                let kind: String = r.get(2)?;
                let path: String = r.get(3)?;
                Ok(ProjectRef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    has_repo: kind == "git" && !path.trim().is_empty(),
                })
            },
        )
    }

    pub fn party_kind(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("type")
                .from("parties")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn bot_names(&mut self, ids: &[&str]) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        for chunk in ids.chunks(500) {
            out.extend(
                self.0.all(
                    Query::select()
                        .columns(["id", "name"])
                        .from("bots")
                        .and_where(Expr::col("id").is_in(chunk.iter().copied())),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?,
            );
        }
        Ok(out)
    }

    pub fn bots(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns([("b", "id"), ("b", "name")])
                .from_as("bots", "b")
                .join_as(
                    JoinType::InnerJoin,
                    "bot_workspaces",
                    "w",
                    Expr::col(("w", "bot_id")).equals(("b", "id")),
                )
                .and_where(Expr::col(("w", "workspace_id")).eq(ws))
                .order_by(("b", "name"), Order::Asc)
                .order_by(("b", "id"), Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

impl<C: Read> Members<'_, C> {
    pub fn count(&mut self, ws: &str) -> Result<i64> {
        Ok(self
            .raw()
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("user_id")))
                    .from("workspace_members")
                    .and_where(Expr::col("workspace_id").eq(ws)),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn first(&mut self, ws: &str) -> Result<Option<String>> {
        let owner = Expr::col("role").eq(Expr::Constant("owner".into()));
        self.raw().one(
            Query::select()
                .column("user_id")
                .from("workspace_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by_expr(owner, Order::Desc)
                .order_by("created", Order::Asc)
                .order_by("user_id", Order::Asc)
                .limit(1),
            |r| r.get(0),
        )
    }
}

impl<C: Read> ItemEvidence<'_, C> {
    pub fn header(&mut self, id: &str) -> Result<Option<(String, String, Option<String>, i64)>> {
        self.0.one(
            by_id("items", id).columns(["workspace_id", "title", "cwd", "updated"]),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
    }

    pub fn project_id(&mut self, id: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(by_id("items", id).column("project_id"), |r| {
                r.get::<Option<String>>(0)
            })?
            .flatten())
    }

    pub fn goals(&mut self, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("text")
                .from("item_goals")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("version", Order::Asc),
            |r| r.get(0),
        )
    }

    pub fn user_text(&mut self, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("text")
                .from("timeline")
                .and_where(Expr::col("item_id").eq(id))
                .and_where(Expr::col("kind").eq("me"))
                .order_by("id", Order::Asc),
            |r| r.get(0),
        )
    }

    pub fn outputs(&mut self, id: &str) -> Result<Vec<(String, String, Option<String>)>> {
        self.0.all(
            Query::select()
                .columns(["kind", "title", "location"])
                .from("artifacts")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    pub fn completion_cards(&mut self, item: &str) -> Result<Vec<CompletionCard>> {
        self.0.all(
            Query::select()
                .columns(["kind", "question", "draft", "status", "result"])
                .from("decisions")
                .and_where(Expr::col("item_id").eq(item))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |row| {
                let result = row
                    .get::<Option<String>>(4)?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default();
                Ok(CompletionCard {
                    kind: row.get(0)?,
                    question: row.get(1)?,
                    draft: row.get(2)?,
                    status: row.get(3)?,
                    result,
                })
            },
        )
    }

    pub fn requester(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        let requester = self
            .0
            .one(
                by_id("items", id)
                    .column("requested_by")
                    .and_where(Expr::col("workspace_id").eq(ws)),
                |r| r.get::<Option<String>>(0),
            )?
            .flatten();
        let Some(requester) = requester else {
            return Ok(None);
        };
        Ok(Members::new(self.0)
            .role(ws, &requester)?
            .map(|_| requester))
    }

    pub fn creator(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        let q = Query::select()
            .column(("a", "actor"))
            .from_as("audit", "a")
            .join_as(
                JoinType::InnerJoin,
                "workspace_members",
                "m",
                Expr::col(("m", "user_id"))
                    .equals(("a", "actor"))
                    .and(Expr::col(("m", "workspace_id")).equals(("a", "workspace_id"))),
            )
            .and_where(Expr::col(("a", "workspace_id")).eq(ws))
            .and_where(Expr::col(("a", "action")).eq("item.create"))
            .and_where(Expr::col(("a", "target")).eq(id))
            .order_by(("a", "id"), Order::Asc)
            .limit(1)
            .take();
        self.0.one(&q, |r| r.get(0))
    }

    pub fn latest_goal(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("text")
                .from("item_goals")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("version", Order::Desc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn projects(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "project_id"])
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("project_id").is_not_null()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn linked_parties(&mut self, ws: &str, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("object_id")
                .from("links")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq("item"))
                .and_where(Expr::col("subject_id").eq(id))
                .and_where(Expr::col("object_type").eq("party"))
                .order_by("created", Order::Asc),
            |r| r.get(0),
        )
    }
}

impl<C: Read> SessionEvidence<'_, C> {
    pub fn item_scope(&mut self, session: &str) -> Result<Option<(String, String)>> {
        self.0.one(
            by_id("sessions", session)
                .columns(["workspace_id", "item_id"])
                .and_where(Expr::col("item_id").is_not_null()),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn cost_for_items(&mut self, ws: &str, items: &[String]) -> Result<f64> {
        let mut cost = 0.0;
        for batch in items.chunks(500) {
            cost += self
                .0
                .one(
                    Query::select()
                        .expr(Func::sum(Expr::col("cost")))
                        .from("sessions")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("item_id").is_in(batch.iter().map(String::as_str))),
                    |row| row.get::<Option<f64>>(0),
                )?
                .flatten()
                .unwrap_or(0.0);
        }
        Ok(cost)
    }

    pub fn latest(&mut self, item: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("id")
                .from("sessions")
                .and_where(Expr::col("item_id").eq(item))
                .order_by("no", Order::Desc)
                .order_by("created", Order::Desc)
                .order_by("rowid", Order::Desc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn item_bot(&mut self, id: &str) -> Result<Option<(Option<String>, Option<String>)>> {
        self.0
            .one(by_id("sessions", id).columns(["item_id", "bot_id"]), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
    }
}

impl<C: Read> MessageEvidence<'_, C> {
    pub fn in_range(
        &mut self,
        ws: &str,
        space: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<(String, String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["author", "text", "created"])
                .from("messages")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space").eq(space))
                .and_where(Expr::col("created").gte(from))
                .and_where(Expr::col("created").lt(to))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }
}

impl<C: Read> FileReads<'_, C> {
    pub fn in_workspace(&mut self, ws: &str, id: &str) -> Result<Option<FileInfo>> {
        self.0.one(
            by_id("files", id)
                .columns(["id", "workspace_id", "sha256", "name", "mime", "size"])
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| {
                Ok(FileInfo {
                    id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    sha256: r.get(2)?,
                    name: r.get(3)?,
                    mime: r.get(4)?,
                    size: r.get(5)?,
                })
            },
        )
    }
}

impl MessageEvidence<'_, crate::W<'_>> {
    pub fn add(
        &mut self,
        ws: &str,
        author: &str,
        text: &str,
        item_id: Option<&str>,
        meta: &serde_json::Value,
        space: &str,
    ) -> Result<String> {
        let id = crate::uid("msg");
        let statement = crate::repo::message_statement(
            self.0.dialect(),
            &id,
            ws,
            author,
            text,
            item_id,
            meta,
            space,
        );
        self.0.exec(&statement)?;
        self.0.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Message {
                space: space.into(),
                ws: ws.into(),
                id: id.clone().into(),
            },
        ));
        Ok(id)
    }
}

impl<C: Read> SessionEvidence<'_, C> {
    pub fn workdir(&mut self, ws: &str, session: &str) -> Result<Option<(String, Option<String>)>> {
        self.0.one(
            Query::select()
                .from("sessions")
                .columns(["cwd", "item_id"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(session)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }
}
