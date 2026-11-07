//! Conversational configuration of contacts, organizations and teams (slice P5):
//! the `contact`, `org` and `team` draft targets and the operations their drafts are made of.
//!
//! A target describes its fields and operations for the model gateway ([`DraftTarget::prepare`]); the
//! model's raw operations are parsed against a fixed schema per operation ([`Raw`]), checked against the
//! data, and turned into stored operations with preview lines ([`DraftTarget::plan`]). Names resolve to
//! ids only by exact name or alias (case-insensitive); anything else becomes a “（没认出：X）” line and no
//! operation. An organization named in `job.open` that does not exist is planned as `org.create`
//! (bound as `$orgN`) and the line says “（新建组织）”.
//!
//! Operations (each validated and applied in the changeset's transaction, undone in reverse order):
//! `party.patch`, `org.create`, `org.patch`, `job.open`, `job.close`, `attr.set`, `mem.add`,
//! `team.create`, `team.patch`, `team.members`. `account.add` belongs to the accounts module and is only
//! offered when that module is plugged into [`Ports`].
//!
//! Integration (integrator): `steve_modules::contacts::changes::register(&mut registry, &ports)` after
//! `accounts::register` / `accounts::plug`.

use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use steve_store::Tx;
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::mems::{self, NewMem};
use steve_store::base::{self, Change, Level, Viewer, show};

use super::Ports;
use super::repo::{self, ContactQuery, NewDef, NewParty, NewValue, Party, PartyPatch};
use super::service::{
    dedup, ensure_base, event, jobs, normalize_domain, own_party, pref, revise, this_month,
};
use super::teams::repo::{self as trepo, Details};
use super::teams::{chart_readonly, own_team};
use crate::changes::{Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared};
use crate::{ApiError, ApiResult, Caller, Check, Registry, own, parse};

/// Registers the three targets and the contacts operations. `ports` tells whether the accounts module is
/// installed (then contact drafts may plan its `account.add`).
pub fn register(reg: &mut Registry, ports: &Ports) {
    reg.target(
        "contact",
        Box::new(ContactTarget {
            accounts: ports.handles.is_some(),
        }),
    )
    .target("org", Box::new(OrgTarget))
    .target("team", Box::new(TeamTarget))
    .op("party.patch", Box::new(PartyPatchOp))
    .op("org.create", Box::new(OrgCreateOp))
    .op("org.patch", Box::new(OrgPatchOp))
    .op("job.open", Box::new(JobOpenOp))
    .op("job.close", Box::new(JobCloseOp))
    .op("attr.set", Box::new(AttrSetOp))
    .op("mem.add", Box::new(MemAddOp))
    .op("team.create", Box::new(TeamCreateOp))
    .op("team.patch", Box::new(TeamPatchOp))
    .op("team.members", Box::new(TeamMembersOp))
    .op("steward.set", Box::new(super::stewards::SetOp))
    .op("steward.remove", Box::new(super::stewards::RemoveOp));
}

// ---------------------------------------------------------------- codes and labels

static KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,31}$").expect("key regex"));
static MONTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-(0[1-9]|1[0-2])$").expect("month regex"));
static DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$")
        .expect("domain regex")
});

/// Relations to the user, code → label. Anything else is a custom Chinese relation.
const RELS: [(&str, &str); 8] = [
    ("me", "我"),
    ("colleague", "同事"),
    ("manager", "上级"),
    ("customer", "客户"),
    ("partner", "合作方"),
    ("friend", "朋友"),
    ("family", "家人"),
    ("supplier", "供应商"),
];
const ORG_KINDS: [(&str, &str); 4] = [
    ("customer", "客户"),
    ("supplier", "供应商"),
    ("partner", "合作方"),
    ("other", "其他"),
];
const MEM_KINDS: [(&str, &str); 4] = [
    ("fact", "事实"),
    ("preference", "偏好"),
    ("rule", "规则"),
    ("event", "事件"),
];
/// Systems a draft may add an account in (never a bank card: drafts do not touch secrets).
const ACCOUNT_SYSTEMS: [(&str, &str); 4] = [
    ("mail", "邮箱"),
    ("phone", "电话"),
    ("github", "GitHub"),
    ("slack", "Slack"),
];
const LEVELS: &str = "public|conclusion|reasoning|never";

fn label_of(table: &[(&str, &str)], code: &str) -> String {
    table
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| code.to_string())
}

/// A code from its code or its label; `None` if neither.
fn code_of(table: &[(&str, &str)], s: &str) -> Option<String> {
    let s = s.trim();
    table
        .iter()
        .find(|(c, l)| *c == s || *l == s)
        .map(|(c, _)| c.to_string())
}

fn labels(table: &[(&'static str, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, l)| *l).collect()
}

// The tables above are what the model reads and answers with; a preview line shows a code in the
// request's language instead (a custom value is shown as it is).

fn rel_shown(code: &str) -> String {
    use steve_i18n::t;
    match code {
        "me" => t!("modules.contacts_draft.rel.me"),
        "colleague" => t!("modules.contacts_draft.rel.colleague"),
        "manager" => t!("modules.contacts_draft.rel.manager"),
        "customer" => t!("modules.contacts_draft.rel.customer"),
        "partner" => t!("modules.contacts_draft.rel.partner"),
        "friend" => t!("modules.contacts_draft.rel.friend"),
        "family" => t!("modules.contacts_draft.rel.family"),
        "supplier" => t!("modules.contacts_draft.rel.supplier"),
        x => x.to_string(),
    }
}

fn org_kind_shown(code: &str) -> String {
    use steve_i18n::t;
    match code {
        "customer" => t!("modules.contacts_draft.org_kind.customer"),
        "supplier" => t!("modules.contacts_draft.org_kind.supplier"),
        "partner" => t!("modules.contacts_draft.org_kind.partner"),
        "other" => t!("modules.contacts_draft.org_kind.other"),
        x => x.to_string(),
    }
}

fn mem_kind_shown(code: &str) -> String {
    use steve_i18n::t;
    match code {
        "fact" => t!("modules.contacts_draft.mem_kind.fact"),
        "preference" => t!("modules.contacts_draft.mem_kind.preference"),
        "rule" => t!("modules.contacts_draft.mem_kind.rule"),
        "event" => t!("modules.contacts_draft.mem_kind.event"),
        x => x.to_string(),
    }
}

fn system_shown(code: &str) -> String {
    use steve_i18n::t;
    match code {
        "mail" => t!("modules.contacts_draft.system.mail"),
        "phone" => t!("modules.contacts_draft.system.phone"),
        x => label_of(&ACCOUNT_SYSTEMS, x),
    }
}

fn yes_no(b: bool) -> String {
    if b {
        steve_i18n::t!("modules.contacts_draft.yes")
    } else {
        steve_i18n::t!("modules.contacts_draft.no")
    }
}

/// A list on a preview line ("a、b" / "a, b").
fn join(xs: &[String]) -> String {
    xs.join(&steve_i18n::t!("modules.contacts_draft.list_sep"))
}

fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn named(p: &Party, n: &str) -> bool {
    same(&p.name, n) || p.aliases.iter().any(|a| same(a, n))
}

// ---------------------------------------------------------------- name resolution

/// The result of resolving a name: exactly one, none, or several (never picked automatically).
enum Found {
    One(Box<Party>),
    Missing,
    Many(usize),
}

fn pick(mut xs: Vec<Party>) -> Found {
    match xs.len() {
        0 => Found::Missing,
        1 => Found::One(Box::new(xs.remove(0))),
        n => Found::Many(n),
    }
}

/// An active contact by exact name or alias; “我” / `me` is the caller.
fn find_contact(c: &Connection, viewer: &Viewer, name: &str) -> ApiResult<Found> {
    let n = name.trim();
    if n.is_empty() {
        return Ok(Found::Missing);
    }
    if n == "我" || n.eq_ignore_ascii_case("me") {
        return Ok(match repo::me_of(c, &viewer.ws, &viewer.user)? {
            Some(me) => Found::One(Box::new(me)),
            None => Found::Missing,
        });
    }
    let q = ContactQuery {
        q: Some(n.to_string()),
        limit: 500,
        ..Default::default()
    };
    Ok(pick(
        repo::contacts(c, viewer, &q)?
            .into_iter()
            .filter(|p| named(p, n))
            .collect(),
    ))
}

/// An active organization by exact name or alias.
fn find_org(c: &Connection, viewer: &Viewer, name: &str) -> ApiResult<Found> {
    let n = name.trim();
    if n.is_empty() {
        return Ok(Found::Missing);
    }
    Ok(pick(
        repo::orgs(c, viewer)?
            .into_iter()
            .filter(|p| named(p, n))
            .collect(),
    ))
}

/// The line for a name that did not resolve to exactly one party.
fn unknown(name: &str, found: &Found) -> Change {
    let to = match found {
        Found::Many(n) => steve_i18n::t!("modules.contacts_draft.ambiguous", n = n),
        _ => steve_i18n::t!("modules.contacts_draft.no_change"),
    };
    Change::new(
        steve_i18n::t!("modules.contacts_draft.unrecognized", name = name.trim()),
        "—",
        to,
    )
}

// ---------------------------------------------------------------- the model's raw operations

/// The schema of what the model may answer, one variant per operation; anything that does not parse
/// is dropped (and logged), never guessed at.
#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
enum Raw {
    #[serde(rename = "set")]
    Set { key: String, value: Value },
    #[serde(rename = "job.open")]
    JobOpen {
        org: String,
        title: Option<String>,
        dept: Option<String>,
    },
    #[serde(rename = "job.close")]
    JobClose { org: String, to: Option<String> },
    #[serde(rename = "attr.set")]
    AttrSet {
        key: String,
        label: Option<String>,
        value: String,
        level: Option<Level>,
    },
    #[serde(rename = "account.add")]
    AccountAdd { system: String, handle: String },
    #[serde(rename = "mem.add")]
    MemAdd {
        kind: Option<String>,
        text: String,
        level: Option<Level>,
    },
    #[serde(rename = "members.add")]
    MembersAdd {
        #[serde(alias = "members")]
        names: Vec<String>,
    },
    #[serde(rename = "members.remove")]
    MembersRemove {
        #[serde(alias = "members")]
        names: Vec<String>,
    },
    #[serde(rename = "team.create")]
    TeamCreate {
        name: Option<String>,
        purpose: Option<String>,
        #[serde(default)]
        members: Vec<String>,
    },
}

fn raw_ops(raw: Vec<Value>) -> Vec<Raw> {
    raw.into_iter()
        .filter_map(|v| match serde_json::from_value::<Raw>(v.clone()) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("contacts draft: dropped model op {v}: {e}");
                None
            }
        })
        .collect()
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A list value: an array of strings, or one string separated by 、 , ，.
fn list(v: &Value) -> Option<Vec<String>> {
    let xs: Vec<String> = match v {
        Value::Array(xs) => xs.iter().filter_map(text).collect(),
        Value::String(s) => s
            .split(['、', ',', '，'])
            .map(|x| x.trim().to_string())
            .collect(),
        _ => return None,
    };
    Some(dedup(xs.into_iter().filter(|x| !x.is_empty()).collect()))
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "是" | "true" | "yes" => Some(true),
            "否" | "false" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// The free-text field rule: a value that does not start with the current text is appended to it.
fn appended(old: &str, new: &str) -> String {
    let new = new.trim();
    if old.is_empty() || new.starts_with(old) {
        new.to_string()
    } else {
        format!("{old}\n{new}")
    }
}

fn len_ok(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.encode_utf16().count())
}

/// Operations and preview lines being built.
#[derive(Default)]
struct Draft {
    ops: Vec<Value>,
    changes: Vec<Change>,
    vars: usize,
}

impl Draft {
    fn line(&mut self, label: impl Into<String>, from: impl Into<String>, to: impl Into<String>) {
        self.changes.push(Change::new(label, from, to));
    }
    fn var(&mut self, prefix: &str) -> String {
        self.vars += 1;
        format!("{prefix}{}", self.vars)
    }
    fn done(self, patch: Option<Value>) -> Plan {
        let mut ops = self.ops;
        if let Some(p) = patch {
            ops.insert(0, p);
        }
        Plan {
            ops,
            changes: self.changes,
        }
    }
}

/// `{"op": op, "id": id, …fields}` when `fields` is not empty.
fn patch_op(op: &str, id: &str, fields: Map<String, Value>) -> Option<Value> {
    if fields.is_empty() {
        return None;
    }
    let mut o = Map::new();
    o.insert("op".into(), json!(op));
    o.insert("id".into(), json!(id));
    o.extend(fields);
    Some(Value::Object(o))
}

/// A party of the viewer's workspace, active, for `revision()` and targets.
fn active_revision(c: &Connection, ws: &str, id: &str, kind: &str) -> ApiResult<Option<i64>> {
    Ok(repo::get(c, id)?
        .filter(|p| p.workspace_id == ws && p.kind == kind && p.status == "active")
        .map(|p| p.revision))
}

fn mem_op(d: &mut Draft, about: &str, kind: Option<String>, txt: &str, level: Option<Level>) {
    let kind = kind
        .and_then(|k| code_of(&MEM_KINDS, &k))
        .unwrap_or_else(|| "fact".into());
    let txt = txt.trim();
    if !len_ok(txt, 1, 2000) {
        return;
    }
    d.ops.push(json!({
        "op": "mem.add", "about": about, "kind": kind, "text": txt,
        "level": level.unwrap_or(Level::Conclusion),
    }));
    d.line(
        steve_i18n::t!("modules.contacts_draft.label.memory"),
        "—",
        steve_i18n::t!(
            "modules.contacts_draft.memory_value",
            text = show(txt),
            kind = mem_kind_shown(&kind)
        ),
    );
}

fn attr_op(
    c: &Connection,
    d: &mut Draft,
    p: &Party,
    key: &str,
    label: Option<String>,
    value: &str,
    level: Option<Level>,
) -> ApiResult<()> {
    let (key, value) = (key.trim(), value.trim());
    let def = repo::def_by_key(c, &p.workspace_id, key)?;
    let label = label
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| def.as_ref().map(|d| d.label.clone()))
        .unwrap_or_else(|| key.to_string());
    let fits = KEY.is_match(key)
        && len_ok(value, 1, 500)
        && len_ok(&label, 1, 20)
        && def.as_ref().is_none_or(|d| {
            d.applies_to(&p.kind)
                && (d.kind != "enum"
                    || d.options.is_empty()
                    || d.options.iter().any(|o| o == value))
        });
    if !fits {
        tracing::warn!("contacts draft: dropped attr.set {key}");
        return Ok(());
    }
    let current = repo::values(c, &p.workspace_id, &p.id)?
        .into_iter()
        .rfind(|v| v.key == key && v.label == label && v.status == "confirmed");
    if current.as_ref().is_some_and(|v| v.value == value) {
        return Ok(());
    }
    let mut op =
        json!({ "op": "attr.set", "party": p.id, "key": key, "label": label, "value": value });
    if let Some(l) = level {
        op["level"] = json!(l);
    }
    d.ops.push(op);
    d.line(
        label,
        show(current.map(|v| v.value).as_deref().unwrap_or("")),
        show(value),
    );
    Ok(())
}

// ---------------------------------------------------------------- contact target

/// `target.kind = "contact"` (`id` = a contact id or `me`).
pub struct ContactTarget {
    /// The accounts module is installed: `account.add` may be planned.
    pub accounts: bool,
}

fn contact_of(c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Party> {
    let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
    if id == "me" {
        return repo::me_of(c, &viewer.ws, &viewer.user)?
            .ok_or_else(|| ApiError::not_found("联系人"));
    }
    own_party(c, viewer, id, "contact", "联系人", true)
}

fn rel_labels(rels: &[String]) -> Vec<String> {
    rels.iter().map(|r| label_of(&RELS, r)).collect()
}

fn rels_shown(rels: &[String]) -> Vec<String> {
    rels.iter().map(|r| rel_shown(r)).collect()
}

/// The open jobs of a contact with their organizations (for `job.close` and the prompt).
fn current_jobs(c: &Connection, viewer: &Viewer, p: &Party) -> ApiResult<Vec<(Party, Value)>> {
    let mut out = Vec::new();
    for j in jobs(c, viewer, &p.id)?
        .into_iter()
        .filter(|j| j.to.is_none())
    {
        if let Some(o) = repo::get(c, &j.org_id)? {
            out.push((o, json!({ "title": j.title, "dept": j.dept })));
        }
    }
    Ok(out)
}

fn job_text(org: &str, title: Option<&str>) -> String {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => format!("{org} · {t}"),
        None => org.to_string(),
    }
}

impl DraftTarget for ContactTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let p = contact_of(c, viewer, id)?;
        let jobs = current_jobs(c, viewer, &p)?;
        let works = jobs
            .iter()
            .map(|(o, j)| job_text(&o.name, j["title"].as_str()))
            .collect::<Vec<_>>();
        let mut fields = vec![
            FieldSpec::new("name", "名字", "text", json!(p.name)),
            FieldSpec::new("aliases", "别名", "list", json!(p.aliases)),
        ];
        if !p.is_me() {
            fields.push(
                FieldSpec::new("rels", "和你的关系", "list", json!(rel_labels(&p.rels)))
                    .values(&labels(&RELS[1..])),
            );
        }
        fields.push(
            FieldSpec::new(
                "important",
                "重要",
                "enum",
                json!(if p.important { "是" } else { "否" }),
            )
            .values(&["是", "否"]),
        );
        fields.push(FieldSpec::new("note", "备注", "text", json!(p.note)));
        let keys = repo::defs(c, &viewer.ws)?
            .into_iter()
            .filter(|d| d.applies_to("contact") && d.status == "active")
            .map(|d| format!("{}={}", d.key, d.label))
            .collect::<Vec<_>>()
            .join(", ");
        let mut ops = vec![
            OpSpec::new(
                "job.open",
                r#"{"org": "<organization name as the user said it>", "title"?: "", "dept"?: ""} — starts (or retitles) a job; an unknown organization is created"#,
            ),
            OpSpec::new(
                "job.close",
                r#"{"org": "<one of the current organizations>", "to"?: "YYYY-MM"} — the job ended (changing companies = job.close + job.open)"#,
            ),
            OpSpec::new(
                "attr.set",
                &format!(
                    r#"{{"key": "<one of {keys}, or a new snake_case key>", "label"?: "", "value": "", "level"?: "{LEVELS}"}}"#
                ),
            ),
        ];
        if self.accounts {
            ops.push(OpSpec::new(
                "account.add",
                r#"{"system": "mail|phone|github|slack", "handle": "<address, number or user name>"}"#,
            ));
        }
        ops.push(OpSpec::new(
            "mem.add",
            &format!(
                r#"{{"kind": "fact|preference|rule|event", "text": "<one sentence in the user's language>", "level"?: "{LEVELS}"}} — a fact worth remembering that fits no field"#
            ),
        ));
        let subject = if p.is_me() {
            format!("the user's own contact card (\"{}\")", p.name)
        } else if works.is_empty() {
            format!("the contact \"{}\"", p.name)
        } else {
            format!(
                "the contact \"{}\" (currently at: {})",
                p.name,
                works.join("; ")
            )
        };
        Ok(Prepared {
            target_id: Some(p.id.clone()),
            revision: Some(p.revision),
            title: steve_i18n::t!("modules.contacts_draft.title.profile", name = p.name),
            href: format!("#/contacts/{}", if p.is_me() { "me" } else { &p.id }),
            subject,
            fields,
            ops,
            free_text: Some("note".into()),
            context: json!({}),
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        prepared: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let p = contact_of(c, viewer, prepared.target_id.as_deref())?;
        let current = current_jobs(c, viewer, &p)?;
        let mut d = Draft::default();
        let mut fields = Map::new();
        for r in raw_ops(raw) {
            match r {
                Raw::Set { key, value } => {
                    set_party_field(&mut d, &mut fields, &p, &key, &value);
                }
                Raw::JobClose { org, to } => {
                    let hit: Vec<&(Party, Value)> =
                        current.iter().filter(|(o, _)| named(o, &org)).collect();
                    let [(o, j)] = hit.as_slice() else {
                        let f = if hit.is_empty() {
                            Found::Missing
                        } else {
                            Found::Many(hit.len())
                        };
                        d.changes.push(unknown(&org, &f));
                        continue;
                    };
                    let to = to
                        .map(|t| t.trim().to_string())
                        .filter(|t| MONTH.is_match(t))
                        .unwrap_or_else(this_month);
                    d.ops
                        .push(json!({ "op": "job.close", "contact": p.id, "org": o.id, "to": to }));
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.job"),
                        job_text(&o.name, j["title"].as_str()),
                        steve_i18n::t!("modules.contacts_draft.job_left", to = to),
                    );
                }
                Raw::JobOpen { org, title, dept } => {
                    let title = title
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                    let dept = dept.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                    if !title.as_deref().is_none_or(|t| len_ok(t, 1, 80))
                        || !dept.as_deref().is_none_or(|t| len_ok(t, 1, 80))
                    {
                        continue;
                    }
                    let (org_id, org_name, created) = match find_org(c, viewer, &org)? {
                        Found::One(o) => (o.id.clone(), o.name.clone(), false),
                        Found::Missing if len_ok(org.trim(), 1, 80) => {
                            let v = d.var("org");
                            d.ops.push(json!({ "op": "org.create", "name": org.trim(), "kind": "partner", "as": v }));
                            (format!("${v}"), org.trim().to_string(), true)
                        }
                        f => {
                            d.changes.push(unknown(&org, &f));
                            continue;
                        }
                    };
                    let was = current.iter().find(|(o, _)| o.id == org_id);
                    if let Some((_, j)) = was
                        && title
                            .as_ref()
                            .is_none_or(|t| j["title"].as_str() == Some(t.as_str()))
                        && dept
                            .as_ref()
                            .is_none_or(|t| j["dept"].as_str() == Some(t.as_str()))
                    {
                        continue;
                    }
                    let mut op = json!({ "op": "job.open", "contact": p.id, "org": org_id });
                    if let Some(t) = &title {
                        op["title"] = json!(t);
                    }
                    if let Some(t) = &dept {
                        op["dept"] = json!(t);
                    }
                    d.ops.push(op);
                    let from = was
                        .map(|(o, j)| job_text(&o.name, j["title"].as_str()))
                        .unwrap_or_else(|| "—".into());
                    let mut to = job_text(&org_name, title.as_deref());
                    if created {
                        to = steve_i18n::t!("modules.contacts_draft.job_new_org", job = to);
                    }
                    d.line(steve_i18n::t!("modules.contacts_draft.label.job"), from, to);
                }
                Raw::AttrSet {
                    key,
                    label,
                    value,
                    level,
                } => attr_op(c, &mut d, &p, &key, label, &value, level)?,
                Raw::AccountAdd { system, handle } if self.accounts => {
                    let handle = handle.trim();
                    let Some(sys) = ACCOUNT_SYSTEMS.iter().find(|(s, _)| *s == system.trim())
                    else {
                        continue;
                    };
                    if !len_ok(handle, 1, 200) {
                        continue;
                    }
                    d.ops.push(json!({ "op": "account.add", "party": p.id, "system": sys.0, "handle": handle }));
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.new_account"),
                        "—",
                        steve_i18n::t!(
                            "modules.contacts_draft.account_value",
                            system = system_shown(sys.0),
                            handle = handle
                        ),
                    );
                }
                Raw::MemAdd { kind, text, level } => mem_op(&mut d, &p.id, kind, &text, level),
                _ => {}
            }
        }
        Ok(d.done(patch_op("party.patch", &p.id, fields)))
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        active_revision(c, ws, id, "contact")
    }

    /// `contact:<id> — <名字>（<组织>）`, the 50 most recently updated contacts.
    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let q = ContactQuery {
            limit: 500,
            ..Default::default()
        };
        let mut xs = repo::contacts(c, viewer, &q)?;
        xs.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        let mut out = Vec::new();
        for p in xs.into_iter().take(50) {
            let org = current_jobs(c, viewer, &p)?.into_iter().next();
            out.push(Candidate {
                code: format!("contact:{}", p.id),
                label: match org {
                    Some((o, _)) => format!("{}（{}）", p.name, o.name),
                    None => p.name.clone(),
                },
            });
        }
        Ok(out)
    }
}

/// One `set` of a contact or organization field into the `party.patch` / `org.patch` fields.
fn set_party_field(
    d: &mut Draft,
    fields: &mut Map<String, Value>,
    p: &Party,
    key: &str,
    value: &Value,
) {
    match key {
        "name" => {
            if let Some(n) = text(value).filter(|n| len_ok(n, 1, 80) && *n != p.name) {
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.name"),
                    show(&p.name),
                    show(&n),
                );
                fields.insert("name".into(), json!(n));
            }
        }
        "aliases" => {
            if let Some(a) = list(value)
                .filter(|a| a.len() <= 10 && a.iter().all(|x| len_ok(x, 1, 80)) && *a != p.aliases)
            {
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.aliases"),
                    show(&join(&p.aliases)),
                    show(&join(&a)),
                );
                fields.insert("aliases".into(), json!(a));
            }
        }
        "rels" if p.kind == "contact" && !p.is_me() => {
            let Some(xs) = list(value) else { return };
            let rels: Vec<String> = dedup(
                xs.iter()
                    .map(|x| code_of(&RELS, x).unwrap_or_else(|| x.clone()))
                    .filter(|r| r != "me")
                    .collect(),
            );
            if rels.len() <= 5 && rels.iter().all(|r| len_ok(r, 1, 20)) && rels != p.rels {
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.rels"),
                    show(&join(&rels_shown(&p.rels))),
                    show(&join(&rels_shown(&rels))),
                );
                fields.insert("rels".into(), json!(rels));
            }
        }
        "important" if p.kind == "contact" => {
            if let Some(b) = boolean(value).filter(|b| *b != p.important) {
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.important"),
                    yes_no(p.important),
                    yes_no(b),
                );
                fields.insert("important".into(), json!(b));
            }
        }
        "kind" if p.kind == "org" && !p.is_self_org() => {
            if let Some(k) = text(value)
                .and_then(|k| code_of(&ORG_KINDS, &k))
                .filter(|k| Some(k) != p.org_kind.as_ref())
            {
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.org_kind"),
                    show(&org_kind_shown(p.org_kind.as_deref().unwrap_or(""))),
                    org_kind_shown(&k),
                );
                fields.insert("kind".into(), json!(k));
            }
        }
        "note" => {
            if let Some(n) = text(value).filter(|n| !n.is_empty()) {
                let n = appended(&p.note, &n);
                if n != p.note && len_ok(&n, 0, 2000) {
                    let added = n.strip_prefix(p.note.as_str()).unwrap_or(&n).trim();
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.note_appended"),
                        show(&p.note),
                        show(added),
                    );
                    fields.insert("note".into(), json!(n));
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- org target

/// `target.kind = "org"`.
pub struct OrgTarget;

impl DraftTarget for OrgTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
        let o = own_party(c, viewer, id, "org", "组织", true)?;
        let mut fields = vec![
            FieldSpec::new("name", "名字", "text", json!(o.name)),
            FieldSpec::new("aliases", "别名", "list", json!(o.aliases)),
        ];
        if !o.is_self_org() {
            fields.push(
                FieldSpec::new(
                    "kind",
                    "类型",
                    "enum",
                    json!(label_of(&ORG_KINDS, o.org_kind.as_deref().unwrap_or(""))),
                )
                .values(&labels(&ORG_KINDS)),
            );
        }
        fields.push(FieldSpec::new(
            "domains",
            "域名",
            "list",
            json!(repo::domains(c, &viewer.ws, &o.id)?),
        ));
        fields.push(FieldSpec::new("note", "备注", "text", json!(o.note)));
        Ok(Prepared {
            target_id: Some(o.id.clone()),
            revision: Some(o.revision),
            title: steve_i18n::t!("modules.contacts_draft.title.profile", name = o.name),
            href: format!("#/orgs/{}", o.id),
            subject: format!("the organization \"{}\"", o.name),
            fields,
            ops: Vec::new(),
            free_text: Some("note".into()),
            context: json!({}),
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        prepared: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let id = prepared.target_id.as_deref().unwrap_or_default();
        let o = own_party(c, viewer, id, "org", "组织", true)?;
        let mut d = Draft::default();
        let mut taken = false;
        let mut fields = Map::new();
        for r in raw_ops(raw) {
            let Raw::Set { key, value } = r else { continue };
            if key != "domains" {
                set_party_field(&mut d, &mut fields, &o, &key, &value);
                continue;
            }
            let Some(xs) = list(&value) else { continue };
            let new: Vec<String> = dedup(xs.iter().map(|x| normalize_domain(x)).collect());
            if new.len() > 20 || !new.iter().all(|x| x.len() <= 253 && DOMAIN.is_match(x)) {
                continue;
            }
            let old = repo::domains(c, &viewer.ws, &o.id)?;
            if new == old {
                continue;
            }
            for x in &new {
                if let Some(owner) = repo::domain_owner(c, &viewer.ws, x)?.filter(|w| *w != o.id) {
                    let name = repo::get(c, &owner)?.map(|p| p.name).unwrap_or_default();
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.domain_taken", domain = x),
                        "—",
                        steve_i18n::t!("modules.contacts.domain_taken", name = name),
                    );
                    taken = true;
                }
            }
            if taken {
                continue;
            }
            d.line(
                steve_i18n::t!("modules.contacts_draft.label.domains"),
                show(&join(&old)),
                show(&join(&new)),
            );
            fields.insert("domains".into(), json!(new));
        }
        Ok(d.done(patch_op("org.patch", &o.id, fields)))
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        active_revision(c, ws, id, "org")
    }
}

// ---------------------------------------------------------------- team target

/// `target.kind = "team"`: an existing team (`id`), or a new virtual team (`id` missing or `new`).
pub struct TeamTarget;

fn is_new(id: Option<&str>) -> bool {
    id.is_none_or(|i| i == "new")
}

impl DraftTarget for TeamTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let member_ops = |verb: &str| {
            OpSpec::new(
                &format!("members.{verb}"),
                r#"{"names": ["<names exactly as the user said them>"]}"#,
            )
        };
        if is_new(id) {
            return Ok(Prepared {
                target_id: None,
                revision: None,
                title: steve_i18n::t!("modules.contacts_draft.title.new_team"),
                href: "#/contacts".into(),
                subject: "a new virtual team (the user is always a member and its lead)".into(),
                fields: Vec::new(),
                ops: vec![OpSpec::new(
                    "team.create",
                    r#"{"name": "<team name>", "purpose"?: "", "members": ["<names exactly as the user said them, without the user>"]}"#,
                )],
                free_text: None,
                context: json!({}),
            });
        }
        let (p, d) = own_team(c, viewer, id.unwrap_or_default(), true)?;
        let mut fields = Vec::new();
        let mut ops = Vec::new();
        if !d.is_chart() {
            fields.push(FieldSpec::new("name", "名字", "text", json!(p.name)));
            ops.push(member_ops("add"));
            ops.push(member_ops("remove"));
        }
        fields.push(FieldSpec::new("purpose", "用途", "text", json!(d.purpose)));
        let names: Vec<String> = trepo::members(c, &viewer.ws, &p.id)?
            .into_iter()
            .map(|m| m.contact.name)
            .collect();
        Ok(Prepared {
            target_id: Some(p.id.clone()),
            revision: Some(p.revision),
            title: steve_i18n::t!("modules.contacts_draft.title.team", name = p.name),
            href: format!("#/teams/{}", p.id),
            subject: format!(
                "the {} team \"{}\" (members: {})",
                if d.is_chart() { "org-chart" } else { "virtual" },
                p.name,
                names.join(", ")
            ),
            fields,
            ops,
            free_text: Some("purpose".into()),
            context: json!({}),
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        prepared: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        match prepared.target_id.as_deref() {
            None => plan_new_team(c, viewer, raw),
            Some(id) => {
                let (p, d) = own_team(c, viewer, id, true)?;
                plan_team(c, viewer, &p, &d, raw)
            }
        }
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        active_revision(c, ws, id, "team")
    }

    /// `team:<id> — <名字>` for every active team, then `team:new — 新建虚拟团队`.
    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let mut out: Vec<Candidate> = trepo::teams(c, viewer)?
            .into_iter()
            .map(|(p, _)| Candidate {
                code: format!("team:{}", p.id),
                label: p.name,
            })
            .collect();
        out.push(Candidate {
            code: "team:new".into(),
            label: "新建虚拟团队".into(),
        });
        Ok(out)
    }
}

/// Resolves names to contacts; unresolved ones become lines. “我” is left out (`me_out`).
fn resolve_names(
    c: &Connection,
    viewer: &Viewer,
    d: &mut Draft,
    names: &[String],
) -> ApiResult<Vec<Party>> {
    let mut out: Vec<Party> = Vec::new();
    for n in names {
        match find_contact(c, viewer, n)? {
            Found::One(p) => {
                if !p.is_me() && !out.iter().any(|x| x.id == p.id) {
                    out.push(*p);
                }
            }
            f => d.changes.push(unknown(n, &f)),
        }
    }
    Ok(out)
}

fn plan_new_team(c: &Connection, viewer: &Viewer, raw: Vec<Value>) -> ApiResult<Plan> {
    let mut d = Draft::default();
    let (mut name, mut purpose, mut names) = (None, None, Vec::new());
    for r in raw_ops(raw) {
        match r {
            Raw::TeamCreate {
                name: n,
                purpose: p,
                members,
            } => {
                name = n.or(name);
                purpose = p.or(purpose);
                names.extend(members);
            }
            Raw::Set { key, value } if key == "name" => name = text(&value).or(name),
            Raw::Set { key, value } if key == "purpose" => purpose = text(&value).or(purpose),
            Raw::MembersAdd { names: n } => names.extend(n),
            _ => {}
        }
    }
    let members = resolve_names(c, viewer, &mut d, &names)?;
    let name = name.map(|n| n.trim().to_string()).unwrap_or_default();
    let purpose = purpose
        .map(|p| p.trim().to_string())
        .filter(|p| len_ok(p, 1, 400));
    if !len_ok(&name, 1, 40) {
        d.line(
            steve_i18n::t!("modules.contacts_draft.label.team_unnamed"),
            "—",
            steve_i18n::t!("modules.contacts_draft.no_change"),
        );
        return Ok(d.done(None));
    }
    if members.is_empty() {
        d.line(
            steve_i18n::t!("modules.contacts_draft.label.too_few_members"),
            "—",
            steve_i18n::t!("modules.contacts_draft.team_min_two"),
        );
        return Ok(d.done(None));
    }
    let ids: Vec<&str> = members.iter().map(|p| p.id.as_str()).collect();
    d.ops.push(json!({ "op": "team.create", "name": name, "purpose": purpose, "members": ids, "as": "team" }));
    d.line(
        steve_i18n::t!("modules.contacts_draft.label.new_team"),
        "—",
        steve_i18n::t!("modules.contacts_draft.virtual_team", name = name),
    );
    let mut who = vec![steve_i18n::t!("modules.contacts_draft.me")];
    who.extend(members.iter().map(|p| p.name.clone()));
    d.line(
        steve_i18n::t!("modules.contacts_draft.label.members"),
        "—",
        join(&who),
    );
    if let Some(p) = &purpose {
        d.line(
            steve_i18n::t!("modules.contacts_draft.label.purpose"),
            show(""),
            show(p),
        );
    }
    Ok(d.done(None))
}

fn plan_team(
    c: &Connection,
    viewer: &Viewer,
    p: &Party,
    det: &Details,
    raw: Vec<Value>,
) -> ApiResult<Plan> {
    let mut d = Draft::default();
    let mut fields = Map::new();
    let (mut add, mut remove) = (Vec::new(), Vec::new());
    for r in raw_ops(raw) {
        match r {
            Raw::Set { key, value } if key == "name" => {
                let Some(n) = text(&value).filter(|n| len_ok(n, 1, 40) && *n != p.name) else {
                    continue;
                };
                if det.is_chart() {
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.readonly_name"),
                        "—",
                        chart_readonly(),
                    );
                    continue;
                }
                d.line(
                    steve_i18n::t!("modules.contacts_draft.label.name"),
                    show(&p.name),
                    show(&n),
                );
                fields.insert("name".into(), json!(n));
            }
            Raw::Set { key, value } if key == "purpose" => {
                let Some(x) = text(&value).filter(|x| !x.is_empty()) else {
                    continue;
                };
                let n = appended(&det.purpose, &x);
                if n != det.purpose && len_ok(&n, 0, 400) {
                    let added = n.strip_prefix(det.purpose.as_str()).unwrap_or(&n).trim();
                    d.line(
                        steve_i18n::t!("modules.contacts_draft.label.purpose_appended"),
                        show(&det.purpose),
                        show(added),
                    );
                    fields.insert("purpose".into(), json!(n));
                }
            }
            Raw::MembersAdd { names } | Raw::MembersRemove { names } if det.is_chart() => {
                d.line(
                    steve_i18n::t!(
                        "modules.contacts_draft.label.readonly_members",
                        names = join(&names)
                    ),
                    "—",
                    chart_readonly(),
                );
            }
            Raw::MembersAdd { names } => add.extend(names),
            Raw::MembersRemove { names } => remove.extend(names),
            _ => {}
        }
    }
    let rows = trepo::member_rows(c, &viewer.ws, &p.id)?;
    let is_member = |id: &str| rows.iter().any(|(c, _)| c == id);
    let add: Vec<Party> = resolve_names(c, viewer, &mut d, &add)?
        .into_iter()
        .filter(|x| !is_member(&x.id))
        .collect();
    let remove: Vec<Party> = resolve_names(c, viewer, &mut d, &remove)?
        .into_iter()
        .filter(|x| is_member(&x.id) && !add.iter().any(|a| a.id == x.id))
        .collect();
    if !add.is_empty() || !remove.is_empty() {
        let before = trepo::headcount(c, &viewer.ws, &p.id, false)?;
        let after = before + add.len() as i64 - remove.len() as i64;
        let mut what = Vec::new();
        if !add.is_empty() {
            what.push(steve_i18n::t!(
                "modules.contacts_draft.members_added",
                names = join(&add.iter().map(|x| x.name.clone()).collect::<Vec<_>>())
            ));
        }
        if !remove.is_empty() {
            what.push(steve_i18n::t!(
                "modules.contacts_draft.members_removed",
                names = join(&remove.iter().map(|x| x.name.clone()).collect::<Vec<_>>())
            ));
        }
        d.ops.push(json!({
            "op": "team.members", "id": p.id,
            "add": add.iter().map(|x| &x.id).collect::<Vec<_>>(),
            "remove": remove.iter().map(|x| &x.id).collect::<Vec<_>>(),
        }));
        d.line(
            steve_i18n::t!("modules.contacts_draft.label.members"),
            steve_i18n::t!("modules.contacts_draft.headcount", n = before),
            steve_i18n::t!(
                "modules.contacts_draft.headcount_changed",
                n = after,
                what = what.join(&steve_i18n::t!("modules.contacts_draft.clause_sep"))
            ),
        );
    }
    Ok(d.done(patch_op("team.patch", &p.id, fields)))
}

// ---------------------------------------------------------------- operations: shared

fn args<T: for<'de> Deserialize<'de>>(op: &Value) -> ApiResult<T> {
    parse(op.to_string().as_bytes())
}

fn viewer(c: &Connection, cx: &OpCx) -> ApiResult<Viewer> {
    Ok(Viewer::load(c, cx.ws, cx.actor)?)
}

fn audit(tx: &Tx, cx: &OpCx, action: &str, target: &str, mut detail: Value) -> ApiResult<()> {
    detail["changeset"] = json!(cx.changeset_id);
    Ok(base::audit(tx, cx.ws, cx.actor, action, target, &detail)?)
}

/// A party of the changeset's workspace by id, whatever its state (undo puts things back regardless).
fn party_in(c: &Connection, cx: &OpCx, id: &str) -> ApiResult<Option<Party>> {
    Ok(repo::get(c, id)?.filter(|p| p.workspace_id == cx.ws))
}

/// The editable columns of a party as JSON (`kind` = contact or org kind).
fn snap(p: &Party) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("name".into(), json!(p.name));
    m.insert("aliases".into(), json!(p.aliases));
    m.insert("rels".into(), json!(p.rels));
    m.insert("important".into(), json!(p.important));
    m.insert("note".into(), json!(p.note));
    m.insert(
        "kind".into(),
        json!(p.contact_kind.as_ref().or(p.org_kind.as_ref())),
    );
    m
}

fn to_patch(p: &Party, m: &Map<String, Value>) -> ApiResult<PartyPatch> {
    fn get<T: for<'de> Deserialize<'de>>(m: &Map<String, Value>, k: &str) -> ApiResult<Option<T>> {
        m.get(k)
            .map(|v| serde_json::from_value(v.clone()).map_err(|_| ApiError::invalid(k)))
            .transpose()
    }
    let kind: Option<String> = get(m, "kind")?;
    Ok(PartyPatch {
        name: get(m, "name")?,
        aliases: get(m, "aliases")?,
        rels: get(m, "rels")?,
        important: get(m, "important")?,
        note: get(m, "note")?,
        contact_kind: kind.clone().filter(|_| p.kind == "contact"),
        org_kind: kind.filter(|_| p.kind == "org"),
        ..Default::default()
    })
}

/// Writes the columns of `want` that differ (revision, audit, event) and returns their old values.
fn write_party(
    tx: &Tx,
    cx: &OpCx,
    p: &Party,
    want: &Map<String, Value>,
    action: &str,
) -> ApiResult<Map<String, Value>> {
    let cur = snap(p);
    let after: Map<String, Value> = want
        .iter()
        .filter(|(k, v)| cur.get(*k).is_some_and(|c| c != *v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if after.is_empty() {
        return Ok(Map::new());
    }
    let before: Map<String, Value> = after.keys().map(|k| (k.clone(), cur[k].clone())).collect();
    revise(
        tx,
        p,
        &to_patch(p, &after)?,
        Value::Object(before.clone()),
        Value::Object(after.clone()),
        cx.actor,
    )?;
    let fields: Vec<&String> = after.keys().collect();
    audit(
        tx,
        cx,
        action,
        &p.id,
        json!({ "type": p.kind, "fields": fields }),
    )?;
    event(tx, cx.ws, &p.id);
    Ok(before)
}

/// The fields of an operation object among `keys`.
fn fields_of(op: &Value, keys: &[&str]) -> Map<String, Value> {
    op.as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| keys.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn check_party_fields(p: &Party, f: &Map<String, Value>) -> ApiResult<()> {
    let mut ck = Check::new();
    let strs = |k: &str| -> Option<Vec<String>> {
        f.get(k).map(|v| {
            serde_json::from_value::<Vec<String>>(v.clone())
                .unwrap_or_else(|_| vec![String::new(); 99])
        })
    };
    if let Some(n) = f.get("name") {
        ck.that("name", n.as_str().is_some_and(|n| len_ok(n.trim(), 1, 80)));
    }
    if let Some(a) = strs("aliases") {
        ck.list("aliases", &a, 10, 80);
    }
    if let Some(r) = strs("rels") {
        ck.list("rels", &r, 5, 20)
            .that("rels", r.iter().any(|x| x == "me") == p.is_me());
    }
    if let Some(i) = f.get("important") {
        ck.that("important", i.is_boolean());
    }
    if let Some(n) = f.get("note") {
        ck.that("note", n.as_str().is_some_and(|n| len_ok(n, 0, 2000)));
    }
    if let Some(k) = f.get("kind") {
        let codes: Vec<&str> = ORG_KINDS.iter().map(|(c, _)| *c).collect();
        ck.that(
            "kind",
            p.kind == "org" && k.as_str().is_some_and(|k| codes.contains(&k)),
        );
    }
    ck.done()?;
    if p.is_self_org() && f.contains_key("kind") {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.contacts.self_org_kind"
        )));
    }
    Ok(())
}

/// Undoing a creation: the party row goes away when nothing else refers to it yet; otherwise it is
/// retired (`deleted` / `dissolved`) so the references keep a name to show.
fn purge_or_retire(tx: &Tx, cx: &OpCx, p: &Party, retired: &str) -> ApiResult<()> {
    let soft_refs: i64 = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM links WHERE workspace_id = ?1 AND ((subject_type = 'party' AND subject_id = ?2) OR (object_type = 'party' AND object_id = ?2)))
              + (SELECT COUNT(*) FROM mem_about WHERE workspace_id = ?1 AND object_type = 'party' AND object_id = ?2)
              + (SELECT COUNT(*) FROM entity_tags WHERE workspace_id = ?1 AND object_type = 'party' AND object_id = ?2)
              + (SELECT COUNT(*) FROM parties WHERE workspace_id = ?1 AND visibility = 'team:' || ?2)
              + (SELECT COUNT(*) FROM mems WHERE workspace_id = ?1 AND visibility = 'team:' || ?2)",
        params![cx.ws, p.id],
        |r| r.get(0),
    )?;
    if soft_refs == 0 {
        // rows with a foreign key to the party (attributes, members, domains, accounts…) make the
        // delete fail; then the savepoint is rolled back and the party retired instead
        tx.execute_batch("SAVEPOINT p5_purge")?;
        let gone = (|| -> rusqlite::Result<()> {
            tx.execute(
                "DELETE FROM team_members WHERE workspace_id = ? AND team_id = ?",
                params![cx.ws, p.id],
            )?;
            tx.execute("DELETE FROM team_details WHERE party_id = ?", [&p.id])?;
            tx.execute("DELETE FROM parties WHERE id = ?", [&p.id])?;
            Ok(())
        })();
        match gone {
            Ok(()) => {
                tx.execute_batch("RELEASE p5_purge")?;
                audit(tx, cx, "party.purge", &p.id, json!({ "type": p.kind }))?;
                event(tx, cx.ws, &p.id);
                return Ok(());
            }
            Err(_) => tx.execute_batch("ROLLBACK TO p5_purge; RELEASE p5_purge")?,
        }
    }
    let patch = PartyPatch {
        status: Some(retired.into()),
        ..Default::default()
    };
    revise(
        tx,
        p,
        &patch,
        json!({ "status": p.status }),
        json!({ "status": retired }),
        cx.actor,
    )?;
    audit(tx, cx, "party.delete", &p.id, json!({ "type": p.kind }))?;
    event(tx, cx.ws, &p.id);
    Ok(())
}

// ---------------------------------------------------------------- party.patch, org.create, org.patch

const CONTACT_FIELDS: [&str; 5] = ["name", "aliases", "rels", "important", "note"];
const ORG_FIELDS: [&str; 4] = ["name", "aliases", "kind", "note"];

#[derive(Deserialize)]
struct IdArg {
    id: String,
}

/// `party.patch {id, name?, aliases?, rels?, important?, note?}` on a contact.
pub struct PartyPatchOp;

impl ChangeOp for PartyPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: IdArg = args(op)?;
        let p = own_party(c, &viewer(c, cx)?, &a.id, "contact", "联系人", true)?;
        check_party_fields(&p, &fields_of(op, &CONTACT_FIELDS))
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: IdArg = args(op)?;
        let p = own_party(tx, &viewer(tx, cx)?, &a.id, "contact", "联系人", true)?;
        let before = write_party(tx, cx, &p, &fields_of(op, &CONTACT_FIELDS), "party.update")?;
        Ok(Value::Object(before))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: IdArg = args(op)?;
        if let (Some(p), Some(b)) = (party_in(tx, cx, &a.id)?, before.as_object()) {
            let back = crate::changes::still_ours(
                cx,
                b,
                &fields_of(op, &CONTACT_FIELDS),
                &snap(&p),
                field_label,
            );
            write_party(tx, cx, &p, &back, "party.update")?;
        }
        Ok(())
    }
}

/// A party field as the change lines name it (what an undo kept, in the request's language).
fn field_label(key: &str) -> String {
    match key {
        "name" => steve_i18n::t!("modules.contacts_draft.label.name"),
        "aliases" => steve_i18n::t!("modules.contacts_draft.label.aliases"),
        "rels" => steve_i18n::t!("modules.contacts_draft.label.rels"),
        "important" => steve_i18n::t!("modules.contacts_draft.label.important"),
        "note" => steve_i18n::t!("modules.contacts_draft.label.note"),
        "kind" => steve_i18n::t!("modules.contacts_draft.label.org_kind"),
        "domains" => steve_i18n::t!("modules.contacts_draft.label.domains"),
        "purpose" => steve_i18n::t!("modules.contacts_draft.label.purpose"),
        k => k.to_string(),
    }
}

#[derive(Deserialize)]
struct OrgCreate {
    name: String,
    kind: Option<String>,
}

/// `org.create {name, kind?, as?}`; an organization with that exact name created since the draft is
/// reused instead of duplicated. `as` binds the id.
pub struct OrgCreateOp;

impl ChangeOp for OrgCreateOp {
    fn validate(&self, _: &Connection, _: &OpCx, op: &Value) -> ApiResult<()> {
        let a: OrgCreate = args(op)?;
        let codes: Vec<&str> = ORG_KINDS.iter().map(|(c, _)| *c).collect();
        Check::new()
            .len("name", a.name.trim(), 1, 80)
            .one_of("kind", a.kind.as_deref().unwrap_or("partner"), &codes)
            .done()
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: OrgCreate = args(op)?;
        let bind = |id: &str| {
            if let Some(v) = op.get("as").and_then(Value::as_str) {
                cx.bind(v, json!(id));
            }
        };
        if let Found::One(o) = find_org(tx, &viewer(tx, cx)?, &a.name)? {
            bind(&o.id);
            return Ok(json!({ "id": null, "reused": o.id }));
        }
        let mut p = NewParty::new("org", a.name.trim(), cx.actor);
        p.org_kind = Some(a.kind.unwrap_or_else(|| "partner".into()));
        p.source = "chat".into();
        let o = repo::insert(tx, cx.ws, &p)?;
        audit(tx, cx, "party.create", &o.id, json!({ "type": "org" }))?;
        event(tx, cx.ws, &o.id);
        bind(&o.id);
        Ok(json!({ "id": o.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        match before.get("id").and_then(Value::as_str) {
            Some(id) => match party_in(tx, cx, id)? {
                Some(o) => purge_or_retire(tx, cx, &o, "deleted"),
                None => Ok(()),
            },
            None => Ok(()),
        }
    }
}

/// Normalized domains, each owned by no other organization (`409 这个域名已经属于「X」`).
fn check_domains(c: &Connection, ws: &str, org: &str, raw: &Value) -> ApiResult<Vec<String>> {
    let xs: Vec<String> =
        serde_json::from_value(raw.clone()).map_err(|_| ApiError::invalid("domains"))?;
    let out = dedup(xs.iter().map(|x| normalize_domain(x)).collect());
    let mut ck = Check::new();
    ck.that("domains", out.len() <= 20);
    for (i, d) in out.iter().enumerate() {
        ck.that(
            &format!("domains.{i}"),
            d.len() <= 253 && DOMAIN.is_match(d),
        );
    }
    ck.done()?;
    for d in &out {
        if let Some(owner) = repo::domain_owner(c, ws, d)?.filter(|o| o != org) {
            let name = repo::get(c, &owner)?.map(|o| o.name).unwrap_or_default();
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.contacts.domain_taken",
                name = name
            )));
        }
    }
    Ok(out)
}

/// `org.patch {id, name?, aliases?, kind?, domains?, note?}`.
pub struct OrgPatchOp;

impl ChangeOp for OrgPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: IdArg = args(op)?;
        let o = own_party(c, &viewer(c, cx)?, &a.id, "org", "组织", true)?;
        check_party_fields(&o, &fields_of(op, &ORG_FIELDS))?;
        if let Some(d) = op.get("domains") {
            check_domains(c, cx.ws, &o.id, d)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: IdArg = args(op)?;
        let o = own_party(tx, &viewer(tx, cx)?, &a.id, "org", "组织", true)?;
        let mut before = write_party(tx, cx, &o, &fields_of(op, &ORG_FIELDS), "party.update")?;
        if let Some(d) = op.get("domains") {
            let new = check_domains(tx, cx.ws, &o.id, d)?;
            let old = repo::domains(tx, cx.ws, &o.id)?;
            if new != old {
                repo::set_domains(tx, cx.ws, &o.id, &new)?;
                repo::touch(tx, &o.id)?;
                audit(
                    tx,
                    cx,
                    "party.update",
                    &o.id,
                    json!({ "type": "org", "fields": ["domains"] }),
                )?;
                event(tx, cx.ws, &o.id);
                before.insert("domains".into(), json!(old));
            }
        }
        Ok(Value::Object(before))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: IdArg = args(op)?;
        let (Some(o), Some(b)) = (party_in(tx, cx, &a.id)?, before.as_object()) else {
            return Ok(());
        };
        let mut b =
            crate::changes::still_ours(cx, b, &fields_of(op, &ORG_FIELDS), &snap(&o), field_label);
        if let Some(d) = b.remove("domains") {
            let old: Vec<String> = serde_json::from_value(d).unwrap_or_default();
            repo::set_domains(tx, cx.ws, &o.id, &old)?;
            repo::touch(tx, &o.id)?;
            event(tx, cx.ws, &o.id);
        }
        write_party(tx, cx, &o, &b, "party.update")?;
        Ok(())
    }
}

// ---------------------------------------------------------------- job.open, job.close

#[derive(Deserialize)]
struct JobArgs {
    contact: String,
    org: String,
    title: Option<String>,
    dept: Option<String>,
    to: Option<String>,
}

impl JobArgs {
    fn key(&self) -> LinkKey {
        LinkKey {
            subject: pref(&self.contact),
            predicate: "works_at".into(),
            object: pref(&self.org),
        }
    }
    fn check(&self) -> ApiResult<()> {
        let mut ck = Check::new();
        ck.opt_len("title", self.title.as_deref(), 1, 80).opt_len(
            "dept",
            self.dept.as_deref(),
            1,
            80,
        );
        if let Some(t) = &self.to {
            ck.pattern("to", t, &MONTH);
        }
        ck.done()
    }
    fn owned(&self, c: &Connection, cx: &OpCx) -> ApiResult<(Party, Party)> {
        let v = viewer(c, cx)?;
        let p = own_party(c, &v, &self.contact, "contact", "联系人", true)?;
        let o = own_party(c, &v, &self.org, "org", "组织", true)?;
        Ok((p, o))
    }
}

fn is_current(props: &Value) -> bool {
    props.get("to").is_none_or(Value::is_null)
}

/// Records a job change on the contact (revision, audit, event) like the REST job routes.
fn job_written(
    tx: &Tx,
    cx: &OpCx,
    p: &Party,
    org: &str,
    before: &Value,
    after: &Value,
    action: &str,
) -> ApiResult<()> {
    revise(
        tx,
        p,
        &PartyPatch::default(),
        json!({ "job": { "org_id": org, "props": before } }),
        json!({ "job": { "org_id": org, "props": after } }),
        cx.actor,
    )?;
    audit(tx, cx, action, &p.id, json!({ "org_id": org }))?;
    event(tx, cx.ws, &p.id);
    Ok(())
}

/// `job.open {contact, org, title?, dept?}`: a new job (`source=chat`); a closed one at the same
/// organization is reopened with its old interval in `props.history`; an open one gets the new title/dept.
pub struct JobOpenOp;

impl ChangeOp for JobOpenOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: JobArgs = args(op)?;
        a.check()?;
        a.owned(c, cx).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: JobArgs = args(op)?;
        let (p, o) = a.owned(tx, cx)?;
        let key = a.key();
        let (old, props) = match links::get(tx, cx.ws, &key)? {
            Some(l) if is_current(&l.props) => {
                let mut props = if l.props.is_object() {
                    l.props.clone()
                } else {
                    json!({})
                };
                if let Some(t) = &a.title {
                    props["title"] = json!(t);
                }
                if let Some(d) = &a.dept {
                    props["dept"] = json!(d);
                }
                links::set_props(tx, cx.ws, &key, &props)?;
                (l.props, props)
            }
            Some(l) => {
                let mut history = l
                    .props
                    .get("history")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut past = l.props.clone();
                if let Some(x) = past.as_object_mut() {
                    x.remove("history");
                }
                history.push(past);
                let props = json!({ "title": a.title, "dept": a.dept, "from": null, "to": null, "history": history });
                links::set_props(tx, cx.ws, &key, &props)?;
                (l.props, props)
            }
            None => {
                let mut l =
                    NewLink::new(key.subject.clone(), "works_at", key.object.clone(), "chat");
                l.props = json!({ "title": a.title, "dept": a.dept, "from": null, "to": null });
                links::add(tx, cx.ws, &l)?;
                (Value::Null, l.props)
            }
        };
        job_written(tx, cx, &p, &o.id, &old, &props, "party.job_add")?;
        Ok(json!({ "props": old }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: JobArgs = args(op)?;
        let key = a.key();
        let now = links::get(tx, cx.ws, &key)?
            .map(|l| l.props)
            .unwrap_or(Value::Null);
        let old = before.get("props").cloned().unwrap_or(Value::Null);
        if old.is_null() {
            links::remove(tx, cx.ws, &key)?;
        } else {
            links::set_props(tx, cx.ws, &key, &old)?;
        }
        if let Some(p) = party_in(tx, cx, &a.contact)? {
            job_written(tx, cx, &p, &a.org, &now, &old, "party.job_update")?;
        }
        Ok(())
    }
}

/// `job.close {contact, org, to?}`: the open job there ends (`to`, default this month).
pub struct JobCloseOp;

impl ChangeOp for JobCloseOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: JobArgs = args(op)?;
        a.check()?;
        a.owned(c, cx)?;
        match links::get(c, cx.ws, &a.key())? {
            Some(l) if is_current(&l.props) => Ok(()),
            _ => Err(ApiError::not_found("任职")),
        }
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: JobArgs = args(op)?;
        let (p, o) = a.owned(tx, cx)?;
        let key = a.key();
        let l = links::get(tx, cx.ws, &key)?
            .filter(|l| is_current(&l.props))
            .ok_or_else(|| ApiError::not_found("任职"))?;
        let mut props = if l.props.is_object() {
            l.props.clone()
        } else {
            json!({})
        };
        props["to"] = json!(a.to.clone().unwrap_or_else(this_month));
        links::set_props(tx, cx.ws, &key, &props)?;
        job_written(tx, cx, &p, &o.id, &l.props, &props, "party.job_update")?;
        Ok(json!({ "props": l.props }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: JobArgs = args(op)?;
        let key = a.key();
        let Some(now) = links::get(tx, cx.ws, &key)?.map(|l| l.props) else {
            return Ok(());
        };
        let old = before.get("props").cloned().unwrap_or_else(|| json!({}));
        links::set_props(tx, cx.ws, &key, &old)?;
        if let Some(p) = party_in(tx, cx, &a.contact)? {
            job_written(tx, cx, &p, &a.org, &now, &old, "party.job_update")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- attr.set, mem.add

#[derive(Deserialize)]
struct AttrArgs {
    party: String,
    key: String,
    label: Option<String>,
    value: String,
    level: Option<Level>,
}

/// A contact or organization the actor can write (`404 没有这个联系人` / `组织`).
fn own_contact_or_org(c: &Connection, cx: &OpCx, id: &str) -> ApiResult<Party> {
    let p = repo::get(c, id)?
        .filter(|p| p.status == "active" && (p.kind == "contact" || p.kind == "org"));
    let noun = if p.as_ref().is_some_and(|p| p.kind == "org") {
        "组织"
    } else {
        "联系人"
    };
    own(p, &viewer(c, cx)?, noun)
}

/// `attr.set {party, key, label?, value, level?}`: a value you said (`confirmed`, “你说的”); an unknown
/// key gets a `text` definition of yours; older confirmed values with the same key + label go stale.
pub struct AttrSetOp;

impl AttrSetOp {
    fn check(c: &Connection, cx: &OpCx, a: &AttrArgs, p: &Party) -> ApiResult<()> {
        let def = repo::def_by_key(c, cx.ws, &a.key)?;
        let mut ck = Check::new();
        ck.pattern("key", &a.key, &KEY)
            .opt_len("label", a.label.as_deref().map(str::trim), 1, 20)
            .len("value", a.value.trim(), 1, 500)
            .that("key", def.as_ref().is_none_or(|d| d.applies_to(&p.kind)));
        if let Some(d) = def.filter(|d| d.kind == "enum" && !d.options.is_empty()) {
            ck.that("value", d.options.iter().any(|o| o == a.value.trim()));
        }
        ck.done()
    }
}

impl ChangeOp for AttrSetOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: AttrArgs = args(op)?;
        let p = own_contact_or_org(c, cx, &a.party)?;
        Self::check(c, cx, &a, &p)
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: AttrArgs = args(op)?;
        let p = own_contact_or_org(tx, cx, &a.party)?;
        Self::check(tx, cx, &a, &p)?;
        let label = a.label.as_deref().map(str::trim);
        let (def, made) = match repo::def_by_key(tx, cx.ws, &a.key)? {
            Some(d) => (d, None),
            None => {
                let d = repo::def_insert(
                    tx,
                    cx.ws,
                    &NewDef {
                        applies: p.kind.clone(),
                        key: a.key.clone(),
                        label: label.unwrap_or(&a.key).to_string(),
                        kind: "text".into(),
                        options: Vec::new(),
                        default_level: a.level.unwrap_or(Level::Conclusion),
                        source: "me".into(),
                        status: "active".into(),
                    },
                )?;
                audit(tx, cx, "attr_def.create", &d.id, json!({ "key": d.key }))?;
                let id = d.id.clone();
                (d, Some(id))
            }
        };
        let label = label.unwrap_or(&def.label).to_string();
        let staled: Vec<String> = repo::values(tx, cx.ws, &p.id)?
            .into_iter()
            .filter(|v| v.key == def.key && v.label == label && v.status == "confirmed")
            .map(|v| v.id)
            .collect();
        let v = repo::value_insert(
            tx,
            cx.ws,
            &NewValue {
                party_id: p.id.clone(),
                key: def.key.clone(),
                label,
                value: a.value.trim().to_string(),
                source: "chat".into(),
                source_label: "你说的".into(),
                source_ref: None,
                level: a.level.unwrap_or(def.default_level),
                status: "confirmed".into(),
                decision_id: None,
            },
        )?;
        repo::stale_others(tx, &v)?;
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "attr": null }),
            json!({ "attr": { "id": v.id, "key": v.key, "label": v.label } }),
            cx.actor,
        )?;
        audit(
            tx,
            cx,
            "attr.set",
            &p.id,
            json!({ "attr_id": v.id, "key": v.key }),
        )?;
        event(tx, cx.ws, &p.id);
        Ok(json!({ "id": v.id, "def": made, "staled": staled }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: AttrArgs = args(op)?;
        let s = |k: &str| before.get(k).and_then(Value::as_str).map(String::from);
        if let Some(id) = s("id") {
            repo::value_delete(tx, &id)?;
        }
        for id in before
            .get("staled")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if repo::value_get(tx, id)?.is_some_and(|v| v.status == "stale") {
                repo::value_update(tx, id, None, None, Some("confirmed"))?;
            }
        }
        if let Some(def) = s("def")
            && repo::def_usage(tx, cx.ws, &a.key)? == 0
        {
            repo::def_delete(tx, &def)?;
        }
        if let Some(p) = party_in(tx, cx, &a.party)? {
            revise(
                tx,
                &p,
                &PartyPatch::default(),
                json!({ "attr": { "id": s("id"), "key": a.key } }),
                json!({ "attr": null }),
                cx.actor,
            )?;
            event(tx, cx.ws, &p.id);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct MemArgs {
    about: String,
    kind: String,
    text: String,
    level: Option<Level>,
}

/// `mem.add {about, kind, text, level?}`: a memory you said about a party (`confirmed`,
/// `source_type=me`, private, “你说的”).
pub struct MemAddOp;

impl MemAddOp {
    fn owned(c: &Connection, cx: &OpCx, a: &MemArgs) -> ApiResult<Party> {
        let codes: Vec<&str> = MEM_KINDS.iter().map(|(c, _)| *c).collect();
        Check::new()
            .one_of("kind", &a.kind, &codes)
            .len("text", a.text.trim(), 1, 2000)
            .done()?;
        let p = repo::get(c, &a.about)?.filter(|p| p.status == "active");
        own(p, &viewer(c, cx)?, "联系人")
    }
}

impl ChangeOp for MemAddOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        Self::owned(c, cx, &args(op)?).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: MemArgs = args(op)?;
        let p = Self::owned(tx, cx, &a)?;
        let m = mems::create(
            tx,
            cx.ws,
            &NewMem {
                kind: a.kind.clone(),
                text: a.text.trim().to_string(),
                source_type: "me".into(),
                source_label: "你说的".into(),
                level: a.level.unwrap_or(Level::Conclusion),
                writer: "me".into(),
                created_by: cx.actor.into(),
                about: vec![pref(&p.id)],
                ..Default::default()
            },
        )?;
        audit(
            tx,
            cx,
            "mem.create",
            &m.id,
            json!({ "about": p.id, "kind": a.kind }),
        )?;
        event(tx, cx.ws, &p.id);
        Ok(json!({ "id": m.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: MemArgs = args(op)?;
        if let Some(id) = before.get("id").and_then(Value::as_str) {
            mems::delete(tx, id)?;
        }
        event(tx, cx.ws, &a.about);
        Ok(())
    }
}

// ---------------------------------------------------------------- team.create, team.patch, team.members

#[derive(Deserialize)]
struct TeamCreate {
    name: String,
    purpose: Option<String>,
    members: Vec<String>,
}

/// `team.create {name, purpose?, members: [contact ids], as?}`: a virtual team of “我” (lead) and the
/// members (`source=chat`); at least two people.
pub struct TeamCreateOp;

impl TeamCreateOp {
    fn members(
        c: &Connection,
        cx: &OpCx,
        a: &TeamCreate,
        me: Option<&str>,
    ) -> ApiResult<Vec<Party>> {
        Check::new()
            .len("name", a.name.trim(), 1, 40)
            .opt_len("purpose", a.purpose.as_deref(), 0, 400)
            .that("members", a.members.len() <= 500)
            .done()?;
        let v = viewer(c, cx)?;
        let mut out: Vec<Party> = Vec::new();
        for id in &a.members {
            let p = own_party(c, &v, id, "contact", "联系人", true)?;
            if Some(p.id.as_str()) != me && !out.iter().any(|x| x.id == p.id) {
                out.push(p);
            }
        }
        if out.is_empty() {
            return Err(ApiError::invalid("members"));
        }
        Ok(out)
    }
}

impl ChangeOp for TeamCreateOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: TeamCreate = args(op)?;
        let me = repo::me_of(c, cx.ws, cx.actor)?.map(|p| p.id);
        Self::members(c, cx, &a, me.as_deref()).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: TeamCreate = args(op)?;
        let me = ensure_base(
            tx,
            &Caller {
                ws: cx.ws.into(),
                me: cx.actor.into(),
            },
        )?;
        let members = Self::members(tx, cx, &a, Some(&me.id))?;
        let mut p = NewParty::new("team", a.name.trim(), cx.actor);
        p.source = "chat".into();
        let t = repo::insert(tx, cx.ws, &p)?;
        trepo::details_insert(
            tx,
            cx.ws,
            &t.id,
            &Details {
                kind: "virtual".into(),
                purpose: a.purpose.clone().unwrap_or_default(),
                ..Default::default()
            },
        )?;
        trepo::member_add(tx, cx.ws, &t.id, &me.id, "lead", "chat")?;
        for m in &members {
            trepo::member_add(tx, cx.ws, &t.id, &m.id, "member", "chat")?;
        }
        audit(
            tx,
            cx,
            "team.create",
            &t.id,
            json!({ "kind": "virtual", "members": members.len() + 1 }),
        )?;
        event(tx, cx.ws, &t.id);
        if let Some(v) = op.get("as").and_then(Value::as_str) {
            cx.bind(v, json!(t.id));
        }
        Ok(json!({ "id": t.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        match before.get("id").and_then(Value::as_str) {
            Some(id) => match party_in(tx, cx, id)? {
                Some(t) => purge_or_retire(tx, cx, &t, "dissolved"),
                None => Ok(()),
            },
            None => Ok(()),
        }
    }
}

#[derive(Deserialize)]
struct TeamPatch {
    id: String,
    name: Option<String>,
    purpose: Option<String>,
}

/// `team.patch {id, name?, purpose?}` (org-chart teams: `purpose` only).
pub struct TeamPatchOp;

impl TeamPatchOp {
    fn owned(c: &Connection, cx: &OpCx, a: &TeamPatch) -> ApiResult<(Party, Details)> {
        let mut ck = Check::new();
        if let Some(n) = &a.name {
            ck.len("name", n.trim(), 1, 40);
        }
        ck.opt_len("purpose", a.purpose.as_deref(), 0, 400).done()?;
        let (p, d) = own_team(c, &viewer(c, cx)?, &a.id, true)?;
        if d.is_chart() && a.name.as_ref().is_some_and(|n| n.trim() != p.name) {
            return Err(ApiError::rule(chart_readonly()));
        }
        Ok((p, d))
    }

    /// Writes name/purpose and returns their old values (only the changed ones).
    fn write(tx: &Tx, cx: &OpCx, p: &Party, d: &Details, a: &TeamPatch) -> ApiResult<Value> {
        let name = a
            .name
            .as_ref()
            .map(|n| n.trim().to_string())
            .filter(|n| *n != p.name);
        let purpose = a.purpose.clone().filter(|x| *x != d.purpose);
        let (mut before, mut after) = (Map::new(), Map::new());
        if let Some(n) = &name {
            before.insert("name".into(), json!(p.name));
            after.insert("name".into(), json!(n));
        }
        if let Some(x) = &purpose {
            before.insert("purpose".into(), json!(d.purpose));
            after.insert("purpose".into(), json!(x));
        }
        if after.is_empty() {
            return Ok(json!({}));
        }
        trepo::details_update(tx, &p.id, purpose.as_deref(), None)?;
        let patch = PartyPatch {
            name,
            ..Default::default()
        };
        let fields: Vec<String> = after.keys().cloned().collect();
        revise(
            tx,
            p,
            &patch,
            Value::Object(before.clone()),
            Value::Object(after),
            cx.actor,
        )?;
        audit(tx, cx, "team.update", &p.id, json!({ "fields": fields }))?;
        event(tx, cx.ws, &p.id);
        Ok(Value::Object(before))
    }
}

impl ChangeOp for TeamPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        Self::owned(c, cx, &args(op)?).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: TeamPatch = args(op)?;
        let (p, d) = Self::owned(tx, cx, &a)?;
        Self::write(tx, cx, &p, &d, &a)
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let id: IdArg = args(op)?;
        let Some(p) = party_in(tx, cx, &id.id)? else {
            return Ok(());
        };
        let d = trepo::details(tx, &p.id)?;
        let mut written = Map::new();
        for k in ["name", "purpose"] {
            if let Some(v) = op.get(k).and_then(Value::as_str) {
                let v = if k == "name" { v.trim() } else { v };
                written.insert(k.into(), json!(v));
            }
        }
        let now: Map<String, Value> = [("name", json!(p.name)), ("purpose", json!(d.purpose))]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let before = Value::Object(crate::changes::still_ours(
            cx,
            before.as_object().unwrap_or(&Map::new()),
            &written,
            &now,
            field_label,
        ));
        let s = |k: &str| before.get(k).and_then(Value::as_str).map(String::from);
        let back = TeamPatch {
            id: p.id.clone(),
            name: s("name"),
            purpose: s("purpose"),
        };
        Self::write(tx, cx, &p, &d, &back).map(|_| ())
    }
}

#[derive(Deserialize)]
struct TeamMembers {
    id: String,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// `team.members {id, add: [contact ids], remove: [contact ids]}` on a virtual team (added members
/// `source=chat`, role `member`).
pub struct TeamMembersOp;

impl TeamMembersOp {
    fn owned(c: &Connection, cx: &OpCx, a: &TeamMembers) -> ApiResult<Party> {
        let v = viewer(c, cx)?;
        let (p, d) = own_team(c, &v, &a.id, true)?;
        if d.is_chart() {
            return Err(ApiError::rule(chart_readonly()));
        }
        for id in &a.add {
            own_party(c, &v, id, "contact", "联系人", true)?;
        }
        Ok(p)
    }

    fn record(tx: &Tx, cx: &OpCx, p: &Party, was: Value, now: Value) -> ApiResult<()> {
        revise(
            tx,
            p,
            &PartyPatch::default(),
            json!({ "members": was }),
            json!({ "members": now }),
            cx.actor,
        )?;
        audit(tx, cx, "team.members", &p.id, json!({}))?;
        event(tx, cx.ws, &p.id);
        Ok(())
    }
}

impl ChangeOp for TeamMembersOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        Self::owned(c, cx, &args(op)?).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: TeamMembers = args(op)?;
        let p = Self::owned(tx, cx, &a)?;
        let rows = trepo::member_rows(tx, cx.ws, &p.id)?;
        let sources: Vec<(String, String)> = trepo::members(tx, cx.ws, &p.id)?
            .into_iter()
            .map(|m| (m.contact.id, m.source))
            .collect();
        let row = |c: &str, r: &str, s: &str| json!({ "contact_id": c, "role": r, "source": s });
        let (mut added, mut removed) = (Vec::new(), Vec::new());
        for id in &a.remove {
            if let Some((_, role)) = rows.iter().find(|(c, _)| c == id) {
                let src = sources
                    .iter()
                    .find(|(c, _)| c == id)
                    .map(|(_, s)| s.as_str())
                    .unwrap_or("manual");
                trepo::member_remove(tx, cx.ws, &p.id, id)?;
                removed.push(row(id, role, src));
            }
        }
        for id in &a.add {
            if !rows.iter().any(|(c, _)| c == id) && !added.contains(id) {
                trepo::member_add(tx, cx.ws, &p.id, id, "member", "chat")?;
                added.push(id.clone());
            }
        }
        if !added.is_empty() || !removed.is_empty() {
            let now: Vec<Value> = added.iter().map(|c| row(c, "member", "chat")).collect();
            Self::record(tx, cx, &p, json!(removed), json!(now))?;
        }
        Ok(json!({ "added": added, "removed": removed }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let a: TeamMembers = args(op)?;
        let Some(p) = party_in(tx, cx, &a.id)? else {
            return Ok(());
        };
        let rows = trepo::member_rows(tx, cx.ws, &p.id)?;
        let empty = Vec::new();
        let added = before
            .get("added")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let removed = before
            .get("removed")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for id in added.iter().filter_map(Value::as_str) {
            trepo::member_remove(tx, cx.ws, &p.id, id)?;
        }
        for r in removed {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
            if !rows.iter().any(|(c, _)| c == s("contact_id")) {
                trepo::member_add(tx, cx.ws, &p.id, s("contact_id"), s("role"), s("source"))?;
            }
        }
        if !added.is_empty() || !removed.is_empty() {
            Self::record(tx, cx, &p, json!(added), json!(removed))?;
        }
        Ok(())
    }
}
