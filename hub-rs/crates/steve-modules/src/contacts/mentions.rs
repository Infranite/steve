//! The `@` menu (`GET /mention-options`) and the ⌘K party groups (`GET /parties/search`) of slice P3.
//! Matching is case-insensitive over names and aliases; names that start with
//! the query come before names that only contain it.

use serde::{Deserialize, Serialize};
use steve_store::{BotRows, Contacts, Read, Teams, base::Viewer};

use super::repo::Party;
use super::service::{jobs_in, start};
use crate::{ApiError, ApiResult, Caller, Modules};

/// Entries per group of the `@` menu.
pub const MENTION_LIMIT: usize = 8;
/// Entries per group of ⌘K.
pub const SEARCH_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BotOption {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartyOption {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// The second line: organization and title, `团队 · N 人`, `组织`.
    pub sub: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MentionOptions {
    pub bots: Vec<BotOption>,
    pub parties: Vec<PartyOption>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Q {
    pub q: Option<String>,
}

/// 0 = a name starts with `q`, 1 = a name contains it, `None` = no match. Empty `q` matches everything.
fn rank(names: &[&str], q: &str) -> Option<u8> {
    if q.is_empty() {
        return Some(0);
    }
    let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    if names.iter().any(|n| n.starts_with(q)) {
        Some(0)
    } else if names.iter().any(|n| n.contains(q)) {
        Some(1)
    } else {
        None
    }
}

/// The best `limit` of `xs` for `q`, prefix matches first, otherwise in the given order.
fn pick<T>(xs: Vec<T>, names: impl Fn(&T) -> Vec<&str>, q: &str, limit: usize) -> Vec<T> {
    let mut ranked: Vec<(u8, usize, T)> = xs
        .into_iter()
        .enumerate()
        .filter_map(|(i, x)| rank(&names(&x), q).map(|r| (r, i, x)))
        .collect();
    ranked.sort_by_key(|(r, i, _)| (*r, *i));
    ranked.into_iter().take(limit).map(|(_, _, x)| x).collect()
}

fn party_names(p: &Party) -> Vec<&str> {
    std::iter::once(p.name.as_str())
        .chain(p.aliases.iter().map(String::as_str))
        .collect()
}

/// The second line of a party in menus.
fn sub(c: &mut impl Read, viewer: &Viewer, p: &Party) -> ApiResult<String> {
    Ok(match p.kind.as_str() {
        "team" => {
            let mut teams = Teams(c);
            let chart = teams.details(&p.id)?.is_chart();
            let n = teams.headcount(&viewer.ws, &p.id, chart)?;
            steve_i18n::t!("modules.mentions.team", n = n)
        }
        "org" => steve_i18n::t!("modules.mentions.org"),
        _ => match jobs_in(c, viewer, &p.id)?
            .into_iter()
            .find(|j| j.to.is_none())
        {
            Some(j) => match j.title.filter(|t| !t.is_empty()) {
                Some(t) => format!("{} · {t}", j.org_name),
                None => j.org_name,
            },
            None => String::new(),
        },
    })
}

fn option(c: &mut impl Read, viewer: &Viewer, p: &Party) -> ApiResult<PartyOption> {
    Ok(PartyOption {
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        sub: sub(c, viewer, p)?,
    })
}

fn query(q: Q) -> ApiResult<String> {
    let q = q.q.unwrap_or_default().trim().to_lowercase();
    if q.chars().count() > 100 {
        return Err(ApiError::invalid("q"));
    }
    Ok(q)
}

/// `GET /mention-options?q=`: the workspace's bots and the contacts and teams you can mention (not
/// yourself, not unconfirmed contacts), at most 8 each.
pub fn mention_options(m: &Modules, who: &Caller, q: Q) -> ApiResult<MentionOptions> {
    let (viewer, _) = start(m, who)?;
    let q = query(q)?;
    m.store.read(|r| Ok(mention_options_in(r, &viewer, &q)))?
}

fn mention_options_in(c: &mut impl Read, viewer: &Viewer, q: &str) -> ApiResult<MentionOptions> {
    let bots = BotRows(c)
        .workspace(&viewer.ws)?
        .into_iter()
        .filter(|bot| bot.enabled)
        .map(|bot| BotOption {
            id: bot.id,
            name: bot.name,
        })
        .collect();
    let bots = pick(
        bots,
        |b: &BotOption| vec![b.name.as_str()],
        q,
        MENTION_LIMIT,
    );
    let ps: Vec<Party> = Contacts::new(c)
        .active(viewer, &["contact", "team"], true)?
        .into_iter()
        .filter(|p| !p.is_me())
        .collect();
    let parties = pick(ps, party_names, q, MENTION_LIMIT)
        .iter()
        .map(|p| option(c, viewer, p))
        .collect::<ApiResult<_>>()?;
    Ok(MentionOptions { bots, parties })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    pub contacts: Vec<PartyOption>,
    pub orgs: Vec<PartyOption>,
    pub teams: Vec<PartyOption>,
}

/// `GET /parties/search?q=` (⌘K): contacts (“我” included), organizations and teams, at most 10 each.
/// An empty `q` finds nothing.
pub fn search(m: &Modules, who: &Caller, q: Q) -> ApiResult<SearchResult> {
    let (viewer, _) = start(m, who)?;
    let q = query(q)?;
    if q.is_empty() {
        return Ok(SearchResult {
            contacts: vec![],
            orgs: vec![],
            teams: vec![],
        });
    }
    m.store.read(|r| Ok(search_in(r, &viewer, &q)))?
}

fn search_in(c: &mut impl Read, viewer: &Viewer, q: &str) -> ApiResult<SearchResult> {
    let mut group = |kind: &str| -> ApiResult<Vec<PartyOption>> {
        let ps = Contacts::new(c).active(viewer, &[kind], true)?;
        pick(ps, party_names, q, SEARCH_LIMIT)
            .iter()
            .map(|p| option(c, viewer, p))
            .collect()
    };
    Ok(SearchResult {
        contacts: group("contact")?,
        orgs: group("org")?,
        teams: group("team")?,
    })
}
