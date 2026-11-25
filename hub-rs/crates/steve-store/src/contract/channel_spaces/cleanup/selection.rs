//! The references that protect channel data from cleanup.

use sea_query::{CaseStatement, Expr, ExprTrait, Func, JoinType, Order, Query};

use crate::{Read, Result};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Item,
    Decision,
    Memory,
    Link,
    Knowledge,
    TeamSpace,
    Steward,
    Hidden,
    Confirmed,
    Inactive,
    Edited,
    OtherAccount,
    Merge,
    MergeProposal,
    TeamMember,
    OtherMembership,
    OtherMessage,
    OtherSignal,
    Mail,
}

fn observed(table: &'static str, alias: &'static str, condition: Expr) -> Expr {
    Expr::exists(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as(table, alias)
            .and_where(condition)
            .take(),
    )
}

fn ref_to(
    table: &'static str,
    type_column: &'static str,
    id_column: &'static str,
    kind: &'static str,
    id: Expr,
    workspace: Option<Expr>,
) -> Expr {
    let mut condition = Expr::col(("x", type_column))
        .eq(kind)
        .and(Expr::col(("x", id_column)).eq(id));
    if let Some(ws) = workspace {
        condition = condition.and(Expr::col(("x", "workspace_id")).eq(ws));
    }
    observed(table, "x", condition)
}

fn link(id: Expr, ws: Expr, kind: &'static str, no_inferred: bool) -> Expr {
    let mut condition = Expr::col(("l", "workspace_id")).eq(ws).and(
        Expr::col(("l", "subject_type"))
            .eq(kind)
            .and(Expr::col(("l", "subject_id")).eq(id.clone()))
            .or(Expr::col(("l", "object_type"))
                .eq(kind)
                .and(Expr::col(("l", "object_id")).eq(id))),
    );
    if no_inferred {
        condition = condition.and(Expr::col(("l", "source")).ne("infer"));
    }
    observed("links", "l", condition)
}

fn reason(checks: &[(Expr, Keep)]) -> Expr {
    // CASE precedence is the preview's explanation order.
    let mut case = CaseStatement::new();
    for (i, (condition, _)) in checks.iter().enumerate() {
        case = case.case(condition.clone(), Expr::Constant((i as i64).into()));
    }
    case.into()
}

fn decode_reason(
    row: &crate::contract::Row<'_>,
    column: usize,
    checks: &[(Expr, Keep)],
) -> Result<Option<Keep>> {
    row.get::<Option<i64>>(column)?
        .map(|i| {
            checks
                .get(i as usize)
                .map(|(_, reason)| *reason)
                .ok_or_else(|| crate::StoreError::Storage("invalid cleanup reference kind".into()))
        })
        .transpose()
}

pub enum Target<'a> {
    Connection(&'a str),
    Account(&'a str),
}

pub fn spaces(r: &mut impl Read, ws: &str, target: Target<'_>) -> Result<Vec<(String, String)>> {
    let another = {
        let mut q = Query::select();
        q.expr(Expr::cust("1"))
            .from_as("space_identities", "i")
            .join_as(
                JoinType::InnerJoin,
                "connections",
                "c",
                Expr::col(("c", "id")).eq(Expr::col(("i", "connection_id"))),
            )
            .and_where(Expr::col(("i", "space_id")).eq(Expr::col(("s", "id"))))
            .and_where(Expr::col(("i", "left_at")).is_null());
        if let Target::Connection(conn) = target {
            q.and_where(Expr::col(("i", "connection_id")).ne(conn));
        }
        Expr::exists(q.take())
    };
    let owned = match target {
        Target::Connection(conn) => observed(
            "space_identities",
            "i",
            Expr::col(("i", "space_id"))
                .eq(Expr::col(("s", "id")))
                .and(Expr::col(("i", "connection_id")).eq(conn)),
        ),
        Target::Account(account) => Expr::col(("s", "connection_id"))
            .is_null()
            .and(Expr::col(("s", "account_id")).eq(account)),
    };
    r.all(
        Query::select()
            .columns([("s", "id"), ("s", "name")])
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(owned)
            .and_where(another.not())
            .order_by(("s", "id"), Order::Asc),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn messages(
    r: &mut impl Read,
    space: &str,
    with_text: bool,
    decisions: &[String],
) -> Result<Vec<(String, Option<Keep>)>> {
    let decisions = r.db().dialect().text_array(decisions)?;
    let msg = || Expr::col(("m", "id"));
    let signal = || Expr::col(("e", "message_id")).eq(msg());
    let checks = [
        (Expr::col(("m", "work_item_id")).is_not_null(), Keep::Item),
        (
            Expr::col(("m", "decision_id"))
                .is_not_null()
                .and(Expr::col(("m", "decision_id")).not_in_subquery(decisions.clone())),
            Keep::Decision,
        ),
        (
            observed(
                "ext_events",
                "e",
                signal().and(Expr::col(("e", "work_item_id")).is_not_null()),
            ),
            Keep::Item,
        ),
        (
            observed(
                "ext_events",
                "e",
                signal()
                    .and(Expr::col(("e", "decision_id")).is_not_null())
                    .and(Expr::col(("e", "decision_id")).not_in_subquery(decisions.clone())),
            ),
            Keep::Decision,
        ),
        (
            observed(
                "thread_dispatch_refs",
                "t",
                Expr::col(("t", "origin_message_id"))
                    .eq(msg())
                    .or(Expr::col(("t", "reply_message_id")).eq(msg())),
            ),
            Keep::Item,
        ),
        (
            observed(
                "decisions",
                "d",
                Expr::col(("d", "subject_type"))
                    .eq("chan_message")
                    .and(Expr::col(("d", "subject_id")).eq(msg()))
                    .and(Expr::col(("d", "id")).not_in_subquery(decisions)),
            ),
            Keep::Decision,
        ),
        (
            ref_to(
                "mems",
                "source_type",
                "source_id",
                "chan_message",
                msg(),
                None,
            ),
            Keep::Memory,
        ),
        (
            link(
                msg(),
                Expr::col(("m", "workspace_id")),
                "chan_message",
                false,
            ),
            Keep::Link,
        ),
    ];
    let mut q = Query::select();
    q.from_as("chan_messages", "m")
        .column(("m", "id"))
        .expr(reason(&checks))
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)]);
    if with_text {
        q.and_where(
            Expr::col(("m", "text"))
                .is_not_null()
                .or(Expr::col(("m", "attachments")).ne("[]")),
        );
    }
    r.all(&q, |row| Ok((row.get(0)?, decode_reason(row, 1, &checks)?)))
}

pub fn space_reference(r: &mut impl Read, space: &str) -> Result<Option<Keep>> {
    let id = || Expr::col(("s", "id"));
    let ws = || Expr::col(("s", "workspace_id"));
    let checks = [
        (
            ref_to(
                "mem_about",
                "object_type",
                "object_id",
                "space",
                id(),
                Some(ws()),
            ),
            Keep::Memory,
        ),
        (
            ref_to("mem_candidates", "origin", "origin_id", "space", id(), None),
            Keep::Memory,
        ),
        (
            ref_to(
                "kb_doc_about",
                "object_type",
                "object_id",
                "space",
                id(),
                Some(ws()),
            ),
            Keep::Knowledge,
        ),
        (
            observed("team_details", "t", Expr::col(("t", "space_id")).eq(id())),
            Keep::TeamSpace,
        ),
        (
            ref_to(
                "stewards",
                "object_type",
                "object_id",
                "space",
                id(),
                Some(ws()),
            ),
            Keep::Steward,
        ),
        (
            observed(
                "decisions",
                "d",
                Expr::col(("d", "subject_type"))
                    .eq("space")
                    .and(Expr::col(("d", "subject_id")).eq(id()))
                    .and(Expr::col(("d", "item_id")).is_not_null()),
            ),
            Keep::Item,
        ),
        (link(id(), ws(), "space", false), Keep::Link),
    ];
    Ok(r.one(
        Query::select()
            .expr(reason(&checks))
            .from_as("spaces", "s")
            .and_where(id().eq(space)),
        |row| decode_reason(row, 0, &checks),
    )?
    .flatten())
}

pub fn protected_space_decision(
    r: &mut impl Read,
    space: &str,
    removed: &[String],
) -> Result<bool> {
    let excluded = r.db().dialect().text_array(removed)?;
    Ok(r.one(
        Query::select()
            .column("id")
            .from("decisions")
            .and_where(Expr::col("subject_type").eq("space"))
            .and_where(Expr::col("subject_id").eq(space))
            .and_where(Expr::col("id").not_in_subquery(excluded))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn removable_decisions(r: &mut impl Read, ws: &str, space: &str) -> Result<Vec<String>> {
    let dialect = r.db().dialect();
    let decision = || Expr::col(("d", "id"));
    let inside = |alias, field: Expr| {
        Expr::col((alias, "id"))
            .eq(field)
            .and(Expr::col((alias, "space_id")).eq(space))
            .and(Expr::col((alias, "workspace_id")).eq(ws))
    };
    let subject = Expr::col(("d", "subject_type"));
    let subject_id = Expr::col(("d", "subject_id"));
    let outside = observed(
        "chan_messages",
        "m",
        Expr::col(("m", "decision_id")).eq(decision()).and(
            Expr::col(("m", "space_id"))
                .ne(space)
                .or(Expr::col(("m", "workspace_id")).ne(ws))
                .or(Expr::col(("m", "work_item_id")).is_not_null()),
        ),
    );
    let other_signal = observed(
        "ext_events",
        "e",
        Expr::col(("e", "decision_id")).eq(decision()).and(
            dialect
                .null_safe_eq(Expr::col(("e", "space_id")), Expr::val(space))
                .not()
                .or(Expr::col(("e", "workspace_id")).ne(ws))
                .or(Expr::col(("e", "work_item_id")).is_not_null())
                .or(Expr::col(("e", "message_id")).is_not_null().and(
                    observed(
                        "chan_messages",
                        "m",
                        inside("m", Expr::col(("e", "message_id"))),
                    )
                    .not(),
                )),
        ),
    );
    let mut q = Query::select();
    q.column(("d", "id"))
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "workspace_id")).eq(ws))
        .and_where(Expr::col(("d", "item_id")).is_null())
        .and_where(Expr::col(("d", "session_id")).is_null())
        .and_where(
            subject
                .clone()
                .eq("space")
                .and(subject_id.clone().eq(space))
                .or(subject.eq("chan_message").and(observed(
                    "chan_messages",
                    "m",
                    inside("m", subject_id),
                ))),
        )
        .and_where(outside.not())
        .and_where(other_signal.not());
    for table in [
        "flow_waits",
        "grants",
        "secret_uses",
        "mems",
        "attr_values",
        "merge_proposals",
        "stewards",
        "mail_drafts",
    ] {
        q.and_where(observed(table, "x", Expr::col(("x", "decision_id")).eq(decision())).not());
    }
    for (table, kind, id) in [
        ("links", "subject_type", "subject_id"),
        ("links", "object_type", "object_id"),
        ("mem_about", "object_type", "object_id"),
        ("kb_doc_about", "object_type", "object_id"),
        ("entity_tags", "object_type", "object_id"),
        ("mems", "source_type", "source_id"),
        ("decisions", "subject_type", "subject_id"),
    ] {
        q.and_where(ref_to(table, kind, id, "decision", decision(), None).not());
    }
    q.order_by(("d", "id"), Order::Asc);
    r.all(&q, |row| row.get(0))
}

pub fn contact_ids(r: &mut impl Read, spaces: &[String]) -> Result<Vec<String>> {
    let spaces = r.db().dialect().text_array(spaces)?;
    let senders = Query::select()
        .column("sender_party_id")
        .from("chan_messages")
        .and_where(Expr::col("space_id").in_subquery(spaces.clone()))
        .and_where(Expr::col("sender_party_id").is_not_null())
        .take();
    r.all(
        Query::select()
            .column(("a", "party_id"))
            .from_as("space_members", "m")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).eq(Expr::col(("m", "account_id"))),
            )
            .and_where(Expr::col(("m", "space_id")).in_subquery(spaces))
            .union(sea_query::UnionType::Distinct, senders)
            .order_by("party_id", Order::Asc),
        |row| row.get(0),
    )
}

pub struct Contact {
    pub id: String,
    pub name: String,
    pub reason: Option<Keep>,
}

pub fn contact(
    r: &mut impl Read,
    id: &str,
    spaces: &[String],
    account: &str,
) -> Result<Option<Contact>> {
    let spaces = r.db().dialect().text_array(spaces)?;
    let party = || Expr::col(("p", "id"));
    let party_kind = || Expr::col(("p", "contact_kind"));
    let source = || Expr::col(("p", "source"));
    let account_of = || Expr::col(("a", "party_id")).eq(party());
    let matching = |table, id| observed(table, "x", Expr::col(("x", id)).eq(party()));
    let merged = |table| {
        observed(
            table,
            "m",
            Expr::col(("m", "from_id"))
                .eq(party())
                .or(Expr::col(("m", "into_id")).eq(party())),
        )
    };
    let shown = Expr::col(("p", "user_id"))
        .is_not_null()
        .or(observed(
            "accounts",
            "a",
            account_of().and(Expr::col(("a", "id")).eq(account)),
        ))
        .or(party_kind().eq("bot").and(source().ne("identify")));
    let confirmed =
        source()
            .ne("identify")
            .or(Expr::from(Func::coalesce([party_kind(), Expr::cust("''")]))
                .is_not_in(["unconfirmed", "bot"]));
    let edited = Expr::col(("p", "revision"))
        .gt(1i64)
        .or(ref_to(
            "revisions",
            "object_type",
            "object_id",
            "party",
            party(),
            None,
        ))
        .or(observed(
            "changesets",
            "x",
            Expr::col(("x", "target_id"))
                .eq(party())
                .and(Expr::col(("x", "status")).eq("applied")),
        ))
        .or(observed(
            "audit",
            "x",
            Expr::col(("x", "target"))
                .eq(party())
                .and(Expr::col(("x", "action")).ne("party.create")),
        ))
        .or(matching("attr_values", "party_id"));
    let other_membership = Expr::exists(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("space_members", "m")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).eq(Expr::col(("m", "account_id"))),
            )
            .and_where(account_of())
            .and_where(Expr::col(("m", "space_id")).not_in_subquery(spaces.clone()))
            .take(),
    );
    let checks = [
        (shown, Keep::Hidden),
        (confirmed, Keep::Confirmed),
        (Expr::col(("p", "status")).ne("active"), Keep::Inactive),
        (edited, Keep::Edited),
        (
            observed(
                "accounts",
                "a",
                account_of().and(Expr::col(("a", "source")).ne("identify")),
            ),
            Keep::OtherAccount,
        ),
        (merged("merges"), Keep::Merge),
        (merged("merge_proposals"), Keep::MergeProposal),
        (
            matching("stewards", "person_id").or(ref_to(
                "stewards",
                "object_type",
                "object_id",
                "party",
                party(),
                None,
            )),
            Keep::Steward,
        ),
        (
            observed(
                "team_members",
                "t",
                Expr::col(("t", "contact_id"))
                    .eq(party())
                    .or(Expr::col(("t", "team_id")).eq(party())),
            ),
            Keep::TeamMember,
        ),
        (
            ref_to(
                "mem_about",
                "object_type",
                "object_id",
                "party",
                party(),
                None,
            ),
            Keep::Memory,
        ),
        (
            ref_to(
                "kb_doc_about",
                "object_type",
                "object_id",
                "party",
                party(),
                None,
            ),
            Keep::Knowledge,
        ),
        (
            link(party(), Expr::col(("p", "workspace_id")), "party", true),
            Keep::Link,
        ),
        (other_membership, Keep::OtherMembership),
        (
            observed(
                "chan_messages",
                "m",
                Expr::col(("m", "sender_party_id"))
                    .eq(party())
                    .and(Expr::col(("m", "space_id")).not_in_subquery(spaces.clone())),
            ),
            Keep::OtherMessage,
        ),
        (
            observed(
                "ext_events",
                "e",
                Expr::col(("e", "actor_party_id")).eq(party()).and(
                    Expr::col(("e", "space_id"))
                        .is_null()
                        .or(Expr::col(("e", "space_id")).not_in_subquery(spaces)),
                ),
            ),
            Keep::OtherSignal,
        ),
        (matching("mail_messages", "from_party_id"), Keep::Mail),
    ];
    r.one(
        Query::select()
            .columns([("p", "id"), ("p", "name")])
            .expr(reason(&checks))
            .from_as("parties", "p")
            .and_where(party().eq(id))
            .and_where(Expr::col(("p", "type")).eq("contact")),
        |row| {
            Ok(Contact {
                id: row.get(0)?,
                name: row.get(1)?,
                reason: decode_reason(row, 2, &checks)?,
            })
        },
    )
}

pub fn account_ids(r: &mut impl Read, party: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column("id")
            .from("accounts")
            .and_where(Expr::col("party_id").eq(party))
            .order_by("id", Order::Asc),
        |row| row.get(0),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Leftover {
    pub account_id: String,
    pub name: String,
    pub system_id: String,
    pub spaces: i64,
    pub messages: i64,
}

pub fn leftovers(r: &mut impl Read, ws: &str) -> Result<Vec<Leftover>> {
    let name: sea_query::SubQueryStatement = Query::select()
        .expr(Func::coalesce([
            Func::cust("nullif")
                .args([Expr::col("display_name"), Expr::cust("''")])
                .into(),
            Func::cust("nullif")
                .args([Expr::col("handle_display"), Expr::cust("''")])
                .into(),
            Expr::col("handle"),
        ]))
        .from("accounts")
        .and_where(Expr::col("id").eq(Expr::col(("s", "account_id"))))
        .take()
        .into();
    let messages: sea_query::SubQueryStatement = Query::select()
        .expr(Func::count(Expr::col(("m", "id"))))
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "x",
            Expr::col(("x", "id")).eq(Expr::col(("m", "space_id"))),
        )
        .and_where(Expr::col(("x", "workspace_id")).eq(Expr::col(("s", "workspace_id"))))
        .and_where(Expr::col(("x", "account_id")).eq(Expr::col(("s", "account_id"))))
        .and_where(Expr::col(("x", "connection_id")).is_null())
        .take()
        .into();
    r.all(
        Query::select()
            .column(("s", "account_id"))
            .expr_as(Func::coalesce([Expr::from(name), Expr::cust("''")]), "name")
            .expr(Func::min(Expr::col(("s", "system_id"))))
            .expr(Func::count(Expr::col(("s", "id"))))
            .expr(Expr::from(messages))
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "connection_id")).is_null())
            .and_where(Expr::col(("s", "account_id")).is_not_null())
            .group_by_columns([("s", "workspace_id"), ("s", "account_id")])
            .order_by("name", Order::Asc)
            .order_by(("s", "account_id"), Order::Asc),
        |row| {
            Ok(Leftover {
                account_id: row.get(0)?,
                name: row.get(1)?,
                system_id: row.get(2)?,
                spaces: row.get(3)?,
                messages: row.get(4)?,
            })
        },
    )
}
