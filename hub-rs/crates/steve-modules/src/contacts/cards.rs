//! Contact cards for prompts (`contact_cards`, slice P3): what an Agent session (knowledge K4, layer 6),
//! a channel reply (C4) or a mail draft (E3) is told about the people involved — who they are, their
//! confirmed attributes and memories, each with its disclosure level. `never` never appears, nor do
//! candidates, stale values or parties the viewer cannot see.
//!
//! The text is Chinese like the rest of the prompts; levels are written as the knowledge
//! module's prompts do (可以引用 / 可转述结论 / 仅用于推理).

use rusqlite::Connection;
use steve_store::base::links;
use steve_store::base::mems::{self, MemFilter};
use steve_store::base::{Level, ObjRef, Viewer};

use super::item_people::{ITEM, role_label};
use super::people_repo::{self as prepo, ROLES};
use super::repo::{self, PARTY, Party};
use super::service::jobs;
use crate::ApiResult;

/// Memories per card, newest first.
pub const CARD_MEMS: i64 = 10;
/// Members listed on a team card.
pub const CARD_MEMBERS: usize = 20;

/// The prompt wording of a level. `never` has none: it is never shown.
pub fn level_label(l: Level) -> &'static str {
    match l {
        Level::Public => "可以引用",
        Level::Conclusion => "可转述结论",
        Level::Reasoning | Level::Never => "仅用于推理",
    }
}

/// The Chinese label of a relationship code (custom relationships are already Chinese).
pub fn rel_label(r: &str) -> &str {
    match r {
        "me" => "我",
        "colleague" => "同事",
        "manager" => "上级",
        "customer" => "客户",
        "partner" => "合作伙伴",
        "friend" => "朋友",
        "family" => "家人",
        "supplier" => "供应商",
        x => x,
    }
}

fn org_kind_label(k: &str) -> &str {
    match k {
        "self" => "本公司",
        "customer" => "客户",
        "supplier" => "供应商",
        "partner" => "合作伙伴",
        "other" => "其他",
        x => x,
    }
}

fn mem_kind_label(k: &str) -> &str {
    match k {
        "fact" => "事实",
        "preference" => "偏好",
        "rule" => "规则",
        "event" => "事件",
        "summary" => "摘要",
        x => x,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardAttr {
    pub label: String,
    pub value: String,
    pub level: Level,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardMem {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub level: Level,
}

/// One party as a prompt sees it. `text` is the rendered card; the fields are there for callers that
/// lay it out themselves or record what was disclosed (C5's basis).
#[derive(Debug, Clone, PartialEq)]
pub struct ContactCard {
    pub party_id: String,
    /// `contact` | `org` | `team`
    pub kind: String,
    pub name: String,
    /// Roles on the item, for [`item_contact_cards`] (`raised_by`, …); empty otherwise.
    pub roles: Vec<String>,
    pub attrs: Vec<CardAttr>,
    pub mems: Vec<CardMem>,
    pub text: String,
}

/// The cards of `party_ids` (in that order, duplicates once), as `viewer` may see them. Parties that are
/// gone, in another workspace or not visible are skipped.
pub fn contact_cards(
    c: &Connection,
    viewer: &Viewer,
    party_ids: &[String],
) -> ApiResult<Vec<ContactCard>> {
    let mut out: Vec<ContactCard> = Vec::new();
    for id in party_ids {
        if out.iter().any(|x| &x.party_id == id) {
            continue;
        }
        if let Some(card) = card(c, viewer, id, &[])? {
            out.push(card);
        }
    }
    Ok(out)
}

/// The cards of everyone linked to an item, in the order they were linked, with their roles.
pub fn item_contact_cards(
    c: &Connection,
    viewer: &Viewer,
    item_id: &str,
) -> ApiResult<Vec<ContactCard>> {
    let mut order: Vec<(String, Vec<String>)> = Vec::new();
    for l in links::from(c, &viewer.ws, &ObjRef::new(ITEM, item_id), None)? {
        if l.object_type != PARTY || !ROLES.contains(&l.predicate.as_str()) {
            continue;
        }
        match order.iter_mut().find(|(id, _)| *id == l.object_id) {
            Some((_, roles)) => roles.push(l.predicate),
            None => order.push((l.object_id, vec![l.predicate])),
        }
    }
    let mut out = Vec::new();
    for (id, roles) in order {
        if let Some(card) = card(c, viewer, &id, &roles)? {
            out.push(card);
        }
    }
    Ok(out)
}

/// The cards joined by a blank line; `""` for none (so a prompt without people stays unchanged).
pub fn render(cards: &[ContactCard]) -> String {
    cards
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn card(
    c: &Connection,
    viewer: &Viewer,
    id: &str,
    roles: &[String],
) -> ApiResult<Option<ContactCard>> {
    let Some(p) = repo::get(c, id)?.filter(|p| {
        p.workspace_id == viewer.ws
            && p.status == "active"
            && viewer.sees(&p.visibility, &p.created_by)
    }) else {
        return Ok(None);
    };
    let attrs: Vec<CardAttr> = if p.kind == "team" {
        Vec::new()
    } else {
        repo::values(c, &viewer.ws, &p.id)?
            .into_iter()
            .filter(|v| v.status == "confirmed" && v.level != Level::Never)
            .map(|v| CardAttr {
                label: v.label,
                value: v.value,
                level: v.level,
            })
            .collect()
    };
    let f = MemFilter {
        limit: CARD_MEMS,
        ..MemFilter::usable()
    };
    let mems: Vec<CardMem> = mems::about(c, viewer, &ObjRef::new(PARTY, &p.id), &f)?
        .into_iter()
        .map(|m| CardMem {
            id: m.id,
            kind: m.kind,
            text: m.text,
            level: m.level,
        })
        .collect();
    let mut lines = vec![header(c, viewer, &p, roles)?];
    if p.kind == "team" {
        let names = member_names(c, viewer, &p)?;
        if !names.is_empty() {
            lines.push(format!("- 成员：{}", names.join("、")));
        }
    }
    for a in &attrs {
        lines.push(format!(
            "- {}：{}（{}）",
            a.label,
            a.value,
            level_label(a.level)
        ));
    }
    for m in &mems {
        lines.push(format!(
            "- 记得（{}）：{}（{}）",
            mem_kind_label(&m.kind),
            m.text,
            level_label(m.level)
        ));
    }
    Ok(Some(ContactCard {
        party_id: p.id.clone(),
        kind: p.kind.clone(),
        name: p.name.clone(),
        roles: roles.to_vec(),
        attrs,
        mems,
        text: lines.join("\n"),
    }))
}

fn header(c: &Connection, viewer: &Viewer, p: &Party, roles: &[String]) -> ApiResult<String> {
    let is_bot = p.contact_kind.as_deref() == Some("bot");
    let mut what = match p.kind.as_str() {
        "org" => "组织".to_string(),
        "team" => "团队".to_string(),
        _ if is_bot => "机器人".to_string(),
        _ => "联系人".to_string(),
    };
    if !roles.is_empty() {
        let rs: Vec<&str> = roles.iter().map(|r| role_label(r)).collect();
        what = format!("{what} · {}", rs.join("、"));
    }
    let mut parts = vec![what];
    if !p.aliases.is_empty() {
        parts.push(format!("也叫 {}", p.aliases.join("、")));
    }
    match p.kind.as_str() {
        "org" => {
            if let Some(k) = &p.org_kind {
                parts.push(org_kind_label(k).to_string());
            }
            let ds = repo::domains(c, &viewer.ws, &p.id)?;
            if !ds.is_empty() {
                parts.push(format!("域名 {}", ds.join("、")));
            }
        }
        "team" => {
            let d = super::teams::repo::details(c, &p.id)?;
            if !d.purpose.is_empty() {
                parts.push(format!("目的：{}", d.purpose));
            }
        }
        _ => {
            for j in jobs(c, viewer, &p.id)?
                .into_iter()
                .filter(|j| j.to.is_none())
            {
                parts.push(match j.title.filter(|t| !t.is_empty()) {
                    Some(t) => format!("{} · {t}", j.org_name),
                    None => j.org_name,
                });
            }
            let rels: Vec<&str> = p
                .rels
                .iter()
                .filter(|r| *r != "me")
                .map(|r| rel_label(r))
                .collect();
            if p.is_me() {
                parts.push("就是你服务的用户本人".into());
            } else if !rels.is_empty() {
                parts.push(format!("和用户的关系：{}", rels.join("、")));
            }
            if p.contact_kind.as_deref() == Some("virtual") {
                parts.push("虚拟账号".into());
            }
            // a bot or service sender's 负责人 (“飞书机器人「告警小助手」· 负责人 张三”)
            let owners = super::stewards::party_owner_names(c, &viewer.ws, &p.id)?;
            if !owners.is_empty() {
                parts.push(format!("负责人 {}", owners.join("、")));
            } else if is_bot {
                parts.push("负责人未知".into());
            }
        }
    }
    Ok(format!("{}（{}）", p.name, parts.join("；")))
}

fn member_names(c: &Connection, viewer: &Viewer, team: &Party) -> ApiResult<Vec<String>> {
    let ids = if prepo::is_chart(c, &team.id)? {
        prepo::team_member_ids(c, &viewer.ws, &team.id)?
    } else {
        super::teams::repo::member_rows(c, &viewer.ws, &team.id)?
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    };
    let mut names = Vec::new();
    for id in ids {
        if let Some(p) = repo::get(c, &id)?
            .filter(|p| p.status == "active" && viewer.sees(&p.visibility, &p.created_by))
        {
            names.push(p.name);
            if names.len() == CARD_MEMBERS {
                break;
            }
        }
    }
    Ok(names)
}
