//! The ownership MCP tools (ownership.md §5): `steve_whois`, `steve_person_holdings` and
//! `steve_steward_propose`, plus the ownership lines of `steve_contacts_find`. Defined and routed in
//! [`super::mcp`]; the texts follow the other contacts tools (English frame, names as they are).

use rusqlite::{Connection, params};
use steve_store::base::Viewer;

use super::mcp::{McpCx, names_lower, resolve_who, viewer};
use super::repo::{self, Party};
use super::stewards::{self as st, ObjectInfo, Proposed};
use crate::{ApiError, ApiResult, Modules};

/// Matches `steve_whois` returns.
pub const WHOIS_LIMIT: usize = 5;
/// The answer of `steve_steward_propose` when a proposal was opened.
pub const PROPOSED: &str = "Saved as a proposal; the user will confirm it.";

fn person_line(c: &Connection, s: &st::Steward) -> ApiResult<String> {
    let name = repo::get(c, &s.person_id)?
        .map(|p| p.name)
        .unwrap_or_else(|| s.person_id.clone());
    let mut tail = vec![format!("source {}", s.source)];
    if s.status != "confirmed" {
        tail.push("proposed, not confirmed yet".into());
    }
    Ok(format!(
        "{} ({}): {name} (id {}; {})",
        st::role_label(&s.role),
        s.role,
        s.person_id,
        tail.join(", ")
    ))
}

fn object_line(o: &ObjectInfo) -> String {
    let detail = if o.detail.is_empty() {
        String::new()
    } else {
        format!(" — {}", o.detail)
    };
    format!(
        "{}「{}」({}, id {}){detail}",
        o.type_label, o.name, o.object_type, o.object_id
    )
}

/// The account → party → 负责人 chain of one object.
fn chain(c: &Connection, viewer: &Viewer, o: &ObjectInfo) -> ApiResult<String> {
    let ws = viewer.ws.as_str();
    let mut lines = vec![object_line(o)];
    let mut any = false;
    for s in st::of_object(c, ws, &o.object_type, &o.object_id)? {
        lines.push(format!("  {}", person_line(c, &s)?));
        any = true;
    }
    if let Some(pid) = o.party_id.as_deref().filter(|_| o.object_type != "party")
        && let Some(p) = repo::get(c, pid)?.filter(|p| viewer.sees(&p.visibility, &p.created_by))
    {
        let what = match (p.kind.as_str(), p.contact_kind.as_deref()) {
            (_, Some("bot")) => "a bot",
            ("org", _) if p.is_self_org() => "the user's own organization",
            ("org", _) => "an organization",
            ("team", _) => "a team",
            _ if p.is_me() => "the user",
            _ => "a person",
        };
        lines.push(format!("  Identity of: {} ({what}, id {})", p.name, p.id));
        for s in st::of_object(c, ws, "party", &p.id)? {
            lines.push(format!("    {}", person_line(c, &s)?));
            any = true;
        }
        if p.kind == "contact" && p.contact_kind.as_deref() != Some("bot") {
            // a person's own account: they are the one behind it
            any = true;
        }
    }
    if !any {
        lines.push("  Nobody is recorded as responsible (没有负责人).".into());
    }
    Ok(lines.join("\n"))
}

/// Objects whose handle, name or id matches `q`: exact matches first.
fn matches(c: &Connection, viewer: &Viewer, q: &str) -> ApiResult<Vec<ObjectInfo>> {
    let ws = viewer.ws.as_str();
    let ql = q.to_lowercase();
    let like = repo::like(&ql);
    let mut found: Vec<(bool, ObjectInfo)> = Vec::new();
    let push = |exact: bool, o: ObjectInfo, found: &mut Vec<(bool, ObjectInfo)>| {
        if !found
            .iter()
            .any(|(_, x)| x.object_type == o.object_type && x.object_id == o.object_id)
        {
            found.push((exact, o));
        }
    };
    // an id of any type
    for (t, _) in st::OBJECT_TYPES {
        if let Some(o) = st::object_info(c, ws, t, q)? {
            push(true, o, &mut found);
        }
    }
    // account handles (every handle, Feishu open_ids included), then names
    let accounts: Vec<(String, bool)> = c
        .prepare_cached(
            "SELECT DISTINCT a.id, (lower(h.handle) = ?2 OR lower(a.handle_display) = ?2 OR lower(a.display_name) = ?2)
             FROM accounts a LEFT JOIN account_handles h ON h.account_id = a.id
             WHERE a.workspace_id = ?1 AND a.status != 'deleted'
               AND (lower(h.handle) LIKE ?3 ESCAPE '\\' OR lower(a.handle_display) LIKE ?3 ESCAPE '\\' OR lower(a.display_name) LIKE ?3 ESCAPE '\\')
             LIMIT 20",
        )?
        .query_map(params![ws, ql, like], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, exact) in accounts {
        if let Some(o) = st::object_info(c, ws, "account", &id)? {
            push(exact, o, &mut found);
        }
    }
    let named = [
        (
            "bot",
            "SELECT id, lower(name) = ?2 FROM bots WHERE deleted = 0 AND (workspace_id = ?1 OR id IN (SELECT bot_id FROM bot_workspaces WHERE workspace_id = ?1)) AND lower(name) LIKE ?3 ESCAPE '\\'",
        ),
        (
            "persona",
            "SELECT id, lower(name) = ?2 FROM personas WHERE workspace_id = ?1 AND deleted = 0 AND lower(name) LIKE ?3 ESCAPE '\\'",
        ),
        (
            "device",
            "SELECT id, lower(name) = ?2 FROM devices WHERE ?1 IS NOT NULL AND lower(name) LIKE ?3 ESCAPE '\\'",
        ),
        (
            "space",
            "SELECT id, lower(name) = ?2 FROM spaces WHERE workspace_id = ?1 AND status != 'archived' AND lower(name) LIKE ?3 ESCAPE '\\'",
        ),
    ];
    for (t, sql) in named {
        let ids: Vec<(String, bool)> = c
            .prepare_cached(sql)?
            .query_map(params![ws, ql, like], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, exact) in ids {
            if let Some(o) = st::object_info(c, ws, t, &id)? {
                push(exact, o, &mut found);
            }
        }
    }
    // bot parties by name or alias
    for p in super::people_repo::parties(c, viewer, &["contact"], true)? {
        if p.contact_kind.as_deref() != Some("bot") {
            continue;
        }
        let ns = names_lower(&p);
        if ns.iter().any(|n| n.contains(&ql))
            && let Some(o) = st::object_info(c, ws, "party", &p.id)?
        {
            push(ns.contains(&ql), o, &mut found);
        }
    }
    found.sort_by_key(|(exact, _)| !*exact);
    Ok(found.into_iter().map(|(_, o)| o).collect())
}

/// `steve_whois`.
pub fn whois(m: &Modules, cx: &McpCx, query: &str) -> ApiResult<String> {
    let q = query.trim();
    if q.is_empty() {
        return Err(ApiError::rule("Give a handle, a name or an id."));
    }
    let viewer = viewer(m, cx)?;
    m.store.reader(|c| {
        let mut found = matches(c, &viewer, q)?;
        let more = found.len().saturating_sub(WHOIS_LIMIT);
        found.truncate(WHOIS_LIMIT);
        let mut blocks = Vec::new();
        for o in &found {
            blocks.push(chain(c, &viewer, o)?);
        }
        if blocks.is_empty() {
            blocks.push(format!(
                "No account, bot, device, persona, connection or group matches \"{q}\". For people use steve_contacts_find."
            ));
        } else if more > 0 {
            blocks.push(format!("({more} more matches; ask more precisely.)"));
        }
        Ok(blocks.join("\n\n"))
    })
}

/// `steve_person_holdings`.
pub fn holdings(m: &Modules, cx: &McpCx, person: &str) -> Result<String, String> {
    let viewer = viewer(m, cx).map_err(|e| e.to_string())?;
    m.store
        .reader(|c| {
            let p = resolve_person(c, &viewer, person)?;
            let h = st::holdings_of(c, &viewer, &p)?;
            let mut lines = vec![format!(
                "{} ({}, id {})",
                p.name,
                if h.is_bot {
                    "bot"
                } else if p.is_me() {
                    "the user"
                } else {
                    "contact"
                },
                p.id
            )];
            if h.accounts.is_empty() {
                lines.push("Own accounts: none recorded.".into());
            } else {
                lines.push("Own accounts:".into());
                for g in &h.accounts {
                    let xs: Vec<String> = g
                        .accounts
                        .iter()
                        .map(|a| {
                            if a.name.is_empty() || a.name == a.handle {
                                format!("{} ({})", a.handle, a.kind_label)
                            } else {
                                format!("{} {} ({})", a.name, a.handle, a.kind_label)
                            }
                        })
                        .collect();
                    lines.push(format!("- {}: {}", g.system, xs.join(", ")));
                }
            }
            if h.responsible.is_empty() {
                lines.push("Responsible for: nothing recorded.".into());
            } else {
                lines.push("Responsible for:".into());
                for x in &h.responsible {
                    let pending = if x.status == "confirmed" {
                        ""
                    } else {
                        ", proposed, not confirmed yet"
                    };
                    lines.push(format!(
                        "- {} as {} ({}{pending})",
                        object_line(&x.object),
                        x.role_label,
                        x.role
                    ));
                }
            }
            if h.is_bot {
                if h.stewards.is_empty() {
                    lines.push(
                        "Nobody is recorded as responsible for this bot (没有负责人).".into(),
                    );
                } else {
                    for s in &h.stewards {
                        lines.push(format!(
                            "{}: {} (id {})",
                            s.role_label, s.person_name, s.person_id
                        ));
                    }
                }
            }
            Ok::<_, ApiError>(lines.join("\n"))
        })
        .map_err(|e| e.to_string())
}

/// A contact by id, exact name or alias; “我” / `me` is the user.
fn resolve_person(c: &Connection, viewer: &Viewer, who: &str) -> ApiResult<Party> {
    let w = who.trim();
    if w == "我" || w.eq_ignore_ascii_case("me") {
        return repo::me_of(c, &viewer.ws, &viewer.user)?
            .ok_or_else(|| ApiError::not_found("联系人"));
    }
    let p = resolve_who(c, viewer, w).map_err(ApiError::Rule)?;
    if p.kind != "contact" {
        return Err(ApiError::Rule(format!(
            "\"{w}\" is an organization or a team, not a person."
        )));
    }
    Ok(p)
}

/// `steve_steward_propose`: the object must resolve to exactly one; the person must be a contact who
/// is not a bot. Opens the proposal and its confirm card.
pub fn propose(
    m: &Modules,
    cx: &McpCx,
    object: &str,
    person: &str,
    role: &str,
    basis: &str,
) -> Result<String, String> {
    let viewer = viewer(m, cx).map_err(|e| e.to_string())?;
    let q = object.trim();
    if q.is_empty() {
        return Err("Give the object's id, handle or exact name.".into());
    }
    m.store
        .transaction(|tx| {
            // a bot's account and the bot party itself are one thing: the party is what gets a 负责人
            let mut found = matches(tx, &viewer, q)?;
            let parties: Vec<String> = found
                .iter()
                .filter(|o| o.object_type == "party")
                .map(|o| o.object_id.clone())
                .collect();
            found.retain(|o| {
                o.object_type != "account"
                    || o.party_id.as_ref().is_none_or(|p| !parties.contains(p))
            });
            let exact: Vec<&ObjectInfo> = found
                .iter()
                .filter(|o| o.object_id == q || o.name.to_lowercase() == q.to_lowercase())
                .collect();
            let o = match (exact.as_slice(), found.as_slice()) {
                ([one], _) => (*one).clone(),
                ([], [one]) => one.clone(),
                ([], []) => {
                    return Err(ApiError::Rule(format!(
                        "Nothing matches \"{q}\". Look it up with steve_whois and pass the id."
                    )));
                }
                _ => {
                    let list: Vec<String> = found
                        .iter()
                        .take(WHOIS_LIMIT)
                        .map(|o| format!("{}「{}」(id {})", o.type_label, o.name, o.object_id))
                        .collect();
                    return Err(ApiError::Rule(format!(
                        "Several match \"{q}\": {}. Pass the id.",
                        list.join(", ")
                    )));
                }
            };
            let p = resolve_person(tx, &viewer, person)?;
            if p.contact_kind.as_deref() == Some("bot") {
                return Err(ApiError::Rule(format!(
                    "{} is a bot; a person has to be responsible.",
                    p.name
                )));
            }
            let source_ref = cx.item_id.as_deref().or(cx.session_id.as_deref());
            Ok(
                match st::propose(tx, &viewer.ws, &o, &p, role, basis, &cx.bot_id, source_ref)? {
                    Proposed::Opened(_) => PROPOSED.to_string(),
                    Proposed::Pending(_) => {
                        "Already proposed; waiting for the user to confirm.".to_string()
                    }
                    Proposed::Already => format!(
                        "Already recorded: {} is {} of {}「{}」.",
                        p.name,
                        st::role_label(role),
                        o.type_label,
                        o.name
                    ),
                },
            )
        })
        .map_err(|e: ApiError| e.to_string())
}

/// What `steve_contacts_find` adds for a contact: a bot's 负责人, or what a person is responsible for.
pub fn find_lines(c: &Connection, viewer: &Viewer, p: &Party) -> ApiResult<Vec<String>> {
    let ws = viewer.ws.as_str();
    let mut lines = Vec::new();
    if p.contact_kind.as_deref() == Some("bot") {
        let owners = st::party_owner_names(c, ws, &p.id)?;
        lines.push(if owners.is_empty() {
            "This is a bot; nobody is recorded as responsible.".into()
        } else {
            format!("This is a bot; responsible (负责人): {}", owners.join(", "))
        });
        return Ok(lines);
    }
    let mut xs = Vec::new();
    for s in st::of_person(c, ws, &p.id)?
        .into_iter()
        .filter(|s| s.status == "confirmed")
    {
        if let Some(o) = st::object_info(c, ws, &s.object_type, &s.object_id)? {
            xs.push(format!(
                "{}「{}」({})",
                o.type_label,
                o.name,
                st::role_label(&s.role)
            ));
        }
    }
    if !xs.is_empty() {
        lines.push(format!("Responsible for: {}", xs.join(", ")));
    }
    Ok(lines)
}
