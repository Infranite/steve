//! Conversational configuration: the `gh_repo` draft target (`gh_repo:<ghr>`) with
//! the repo's project and its §3.4 settings, and the `github.comment` grant: its Chinese name and
//! how a grant draft names the bot, the repo and the hours ([`CommentGrants`], a
//! [`GrantCapability`] for accounts-vault's `grant.add` drafting).
//!
//! Operations (all `gh.`-prefixed so they never collide with other modules' names):
//! - `gh.repo.patch {id, project_id?, settings?}`: `PATCH /github/repos/:id` inside the changeset's
//!   transaction; undo puts back the project and settings as they were;
//! - `gh.grant.add {bot, repo, hours}`: a `github.comment` grant (1, 8 or 24 hours); undo revokes it.

use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use steve_modules::changes::{
    Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared,
};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::{self, Change, Viewer, show};
use steve_store::{Tx, uid};

use crate::accept::Slot;
use crate::repo::{self, COMMENT_CAP, RepoRow};
use crate::repos::{RepoPatch, Settings, check_project, patch_repo_tx, repo_snapshot};

/// `target.kind` of `POST /api/config/draft`.
pub const KIND: &str = "gh_repo";
pub const OP_PATCH: &str = "gh.repo.patch";
pub const OP_GRANT: &str = "gh.grant.add";
/// The grant operation as the model is told about it on a `gh_repo` target.
const MODEL_GRANT: &str = "grant.add";

/// The Chinese name of the `github.comment` capability.
pub const COMMENT_LABEL: &str = "在 GitHub 评论";
/// Grant durations; 24 hours is the cap (same as vault grants).
pub const GRANT_HOURS: [i64; 3] = [1, 8, 24];
const HOUR_MS: i64 = 3_600_000;

const UNRECOGNIZED: &str = "（没认出）";
const NO_PROJECT: &str = "（无）";

/// Registers the target and its operations. Called by [`crate::accept::register`].
pub fn register(r: &mut Registry, slot: Slot) {
    r.target(KIND, Box::new(RepoTarget))
        .op(OP_PATCH, Box::new(RepoPatchOp(slot.clone())))
        .op(OP_GRANT, Box::new(GrantOp(slot)));
}

/// `(field key, label, [(code, Chinese)])` of the enum settings.
/// `(code, Chinese label)` pairs of an enum setting.
type Choices = &'static [(&'static str, &'static str)];

const ENUM_FIELDS: [(&str, &str, Choices); 4] = [
    (
        "after_accept",
        "验收通过后",
        &[("ask_pr", "问我要不要开 PR"), ("none", "不开")],
    ),
    (
        "finish_on",
        "什么时候算完成",
        &[("accepted", "开了 PR 就完成"), ("merged", "合并后才完成")],
    ),
    (
        "merge_method",
        "合并方式",
        &[
            ("squash", "squash"),
            ("merge", "merge"),
            ("rebase", "rebase"),
        ],
    ),
    (
        "ci_fail",
        "检查失败时",
        &[("supplement", "交给事项修"), ("signal", "只记录")],
    ),
];
const BOOL_FIELDS: [(&str, &str); 2] = [
    ("delete_branch", "合并后删除分支"),
    ("draft_pr", "先开成草稿 PR"),
];

fn yes_no(b: bool) -> &'static str {
    if b { "是" } else { "否" }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "是" | "开" | "启用" | "开启" | "删除" | "要" => Some(true),
        "false" | "否" | "关" | "停用" | "关闭" | "不删除" | "不要" => Some(false),
        _ => None,
    }
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn enum_label(values: &[(&str, &str)], code: &str) -> String {
    values
        .iter()
        .find(|(c, _)| *c == code)
        .map_or_else(|| code.to_string(), |(_, l)| l.to_string())
}

fn enum_code(values: Choices, said: &str) -> Option<&'static str> {
    let s = said.trim();
    values
        .iter()
        .find(|(c, l)| c.eq_ignore_ascii_case(s) || *l == s)
        .map(|(c, _)| *c)
}

fn own_repo(c: &Connection, ws: &str, id: &str) -> ApiResult<RepoRow> {
    repo::repo_get(c, id)?
        .filter(|r| r.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("仓库"))
}

fn project_name(c: &Connection, ws: &str, id: Option<&str>) -> ApiResult<String> {
    Ok(match id {
        Some(p) => repo::project(c, ws, p)?.map_or_else(|| p.to_string(), |x| x.name),
        None => NO_PROJECT.into(),
    })
}

/// A project of `ws` (not archived) by id or exact name; `None` when unknown or ambiguous.
fn find_project(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<String>> {
    let mut st = c.prepare_cached(
        "SELECT id FROM projects WHERE workspace_id = ?1 AND archived = 0 AND (id = ?2 OR name = ?2)
          ORDER BY id = ?2 DESC LIMIT 2",
    )?;
    let ids: Vec<String> = st
        .query_map(params![ws, said.trim()], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match ids.as_slice() {
        [one] => Some(one.clone()),
        [first, _] if first == said.trim() => Some(first.clone()),
        _ => None,
    })
}

/// A bot of `ws` (not deleted) by id or exact name; `None` when unknown or ambiguous.
pub fn find_bot(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<String>> {
    let said = said.trim();
    let mut st = c.prepare_cached(
        "SELECT b.id FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?1
          WHERE b.deleted = 0 AND (b.id = ?2 OR b.name = ?2) ORDER BY b.id = ?2 DESC LIMIT 2",
    )?;
    let ids: Vec<String> = st
        .query_map(params![ws, said], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match ids.as_slice() {
        [one] => Some(one.clone()),
        [first, _] if first == said => Some(first.clone()),
        _ => None,
    })
}

/// A linked repo of `ws` by id, `owner/name`, `name` or the name of its project (case-insensitive
/// for GitHub names, exact for project names; a trailing “仓库” is ignored). `None` when unknown or
/// ambiguous — never a guess.
pub fn find_repo(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<RepoRow>> {
    let s = said.trim();
    let s = s.strip_suffix("仓库").unwrap_or(s).trim();
    if s.is_empty() {
        return Ok(None);
    }
    let rows = repo::repos(c, ws)?;
    if let Some(r) = rows.iter().find(|r| r.id == s) {
        return Ok(Some(r.clone()));
    }
    let tiers: [&dyn Fn(&RepoRow) -> bool; 2] = [&|r| r.full_name.eq_ignore_ascii_case(s), &|r| {
        r.name.eq_ignore_ascii_case(s)
    }];
    for t in tiers {
        let hits: Vec<&RepoRow> = rows.iter().filter(|r| t(r)).collect();
        match hits.as_slice() {
            [one] => return Ok(Some((*one).clone())),
            [] => {}
            _ => return Ok(None),
        }
    }
    let mut hits = Vec::new();
    for r in &rows {
        if let Some(p) = &r.project_id
            && repo::project(c, ws, p)?.is_some_and(|x| x.name == s)
        {
            hits.push(r);
        }
    }
    Ok(match hits.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    })
}

/// `8`, `"8"`, `"8 小时"`, `"8h"` → 8.
fn parse_hours(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    let s = s
        .trim_end_matches("个小时")
        .trim_end_matches("小时")
        .trim_end_matches(['h', 'H'])
        .trim();
    s.parse().ok()
}

/// A grantable capability as accounts-vault's `grant.add` drafting needs it: its Chinese name, and
/// how the words of a request become a checked operation. A small trait here until V6 defines
/// its own registry (the integrator adapts it).
pub trait GrantCapability: Send + Sync {
    /// `grants.capability` (`github.comment`).
    fn capability(&self) -> &'static str;
    /// Shown in drafts and the grants list (“在 GitHub 评论”).
    fn label(&self) -> &'static str;
    /// `grants.resource_type` (`repo`).
    fn resource_type(&self) -> &'static str;
    /// The resource the user named, as `(id, label)`; `None` when unknown or ambiguous.
    fn resolve_resource(
        &self,
        c: &Connection,
        ws: &str,
        said: &str,
    ) -> ApiResult<Option<(String, String)>>;
    /// The display label of a stored resource id (the grants list).
    fn resource_label(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<String>>;
    /// A raw `{bot, resource, hours}` from the model → the operation and its preview line, or only
    /// a “（没认出：X）” line.
    fn plan_grant(&self, c: &Connection, ws: &str, raw: &Value) -> ApiResult<Plan>;
}

/// `github.comment`: a bot may comment on one repo's PRs and issues without asking.
pub struct CommentGrants;

impl CommentGrants {
    fn plan_on(c: &Connection, ws: &str, raw: &Value, repo: Option<&RepoRow>) -> ApiResult<Plan> {
        let mut plan = Plan::default();
        let bot_said = text_of(&raw["bot"]).unwrap_or_default();
        let bot = find_bot(c, ws, &bot_said)?;
        let repo_said = text_of(&raw["resource"])
            .or_else(|| text_of(&raw["repo"]))
            .unwrap_or_default();
        let repo = match repo {
            Some(r) => Some(r.clone()),
            None => find_repo(c, ws, &repo_said)?,
        };
        let hours = parse_hours(&raw["hours"]).filter(|h| GRANT_HOURS.contains(h));
        let label = match &bot {
            Some(b) => format!(
                "授权 {}",
                repo::bot_name(c, b)?.unwrap_or_else(|| b.clone())
            ),
            None => "授权".to_string(),
        };
        let bad = |what: &str| Change::new(&label, "—", format!("（没认出：{}）", show(what)));
        match (bot, repo, hours) {
            (None, _, _) => plan.changes.push(bad(&bot_said)),
            (_, None, _) => plan.changes.push(bad(&repo_said)),
            (_, _, None) => plan.changes.push(Change::new(
                &label,
                "—",
                format!(
                    "（没认出：{}，只能 1、8 或 24 小时）",
                    show(&text_of(&raw["hours"]).unwrap_or_default())
                ),
            )),
            (Some(b), Some(r), Some(h)) => {
                plan.changes.push(Change::new(
                    &label,
                    "—",
                    format!("{COMMENT_LABEL}（仓库 {}），{h} 小时", r.full_name),
                ));
                plan.ops
                    .push(json!({"op": OP_GRANT, "bot": b, "repo": r.id, "hours": h}));
            }
        }
        Ok(plan)
    }
}

impl GrantCapability for CommentGrants {
    fn capability(&self) -> &'static str {
        COMMENT_CAP
    }
    fn label(&self) -> &'static str {
        COMMENT_LABEL
    }
    fn resource_type(&self) -> &'static str {
        "repo"
    }
    fn resolve_resource(
        &self,
        c: &Connection,
        ws: &str,
        said: &str,
    ) -> ApiResult<Option<(String, String)>> {
        Ok(find_repo(c, ws, said)?.map(|r| (r.id, format!("仓库 {}", r.full_name))))
    }
    fn resource_label(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<String>> {
        Ok(repo::repo_get(c, id)?
            .filter(|r| r.workspace_id == ws)
            .map(|r| format!("仓库 {}", r.full_name)))
    }
    fn plan_grant(&self, c: &Connection, ws: &str, raw: &Value) -> ApiResult<Plan> {
        Self::plan_on(c, ws, raw, None)
    }
}

struct RepoTarget;

impl DraftTarget for RepoTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let r = own_repo(
            c,
            &viewer.ws,
            id.ok_or_else(|| ApiError::not_found("仓库"))?,
        )?;
        let s = Settings::of(&r.settings).to_json();
        let mut fields = vec![FieldSpec::new(
            "project",
            "关联的项目（项目的名字；不关联写“无”）",
            "ref",
            json!(project_name(c, &viewer.ws, r.project_id.as_deref())?),
        )];
        for (key, label, values) in ENUM_FIELDS {
            let labels: Vec<&str> = values.iter().map(|v| v.1).collect();
            fields.push(
                FieldSpec::new(
                    key,
                    label,
                    "enum",
                    json!(enum_label(values, s[key].as_str().unwrap_or(""))),
                )
                .values(&labels),
            );
        }
        for (key, label) in BOOL_FIELDS {
            fields.push(
                FieldSpec::new(key, label, "enum", json!(yes_no(s[key] == true)))
                    .values(&["是", "否"]),
            );
        }
        Ok(Prepared {
            target_id: Some(r.id.clone()),
            revision: Some(r.revision),
            title: format!("仓库 {}", r.full_name),
            href: format!("#/github/{}", r.id),
            subject: format!("the GitHub repository \"{}\"", r.full_name),
            fields,
            ops: vec![OpSpec::new(
                MODEL_GRANT,
                "{\"bot\": <the bot's exact name>, \"hours\": 1|8|24} — let a bot comment on this repository's pull requests and issues without asking, for that long",
            )],
            free_text: None,
            context: Value::Null,
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        p: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let ws = viewer.ws.as_str();
        let r = own_repo(c, ws, p.target_id.as_deref().unwrap_or(""))?;
        let cur = Settings::of(&r.settings).to_json();
        let mut plan = Plan::default();
        let mut settings = Map::new();
        let mut project: Option<Option<String>> = None;
        let mut grants = Vec::new();
        for op in raw {
            let name = op["op"].as_str().unwrap_or("");
            if name == MODEL_GRANT || name == OP_GRANT {
                let g = CommentGrants::plan_on(c, ws, &op, Some(&r))?;
                plan.changes.extend(g.changes);
                grants.extend(g.ops);
                continue;
            }
            if name != "set" {
                plan.changes
                    .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                continue;
            }
            let key = op["key"].as_str().unwrap_or("");
            let said = text_of(&op["value"]).unwrap_or_default();
            let bad = |label: &str| Change::new(label, "—", format!("（没认出：{}）", show(&said)));
            if key == "project" || key == "project_id" {
                let label = "关联的项目";
                let to = if op["value"].is_null()
                    || matches!(said.as_str(), "" | "无" | "不关联" | "（无）" | "none")
                {
                    Some(None)
                } else {
                    find_project(c, ws, &said)?.map(Some)
                };
                match to {
                    Some(to) if to != r.project_id => {
                        plan.changes.push(Change::new(
                            label,
                            project_name(c, ws, r.project_id.as_deref())?,
                            project_name(c, ws, to.as_deref())?,
                        ));
                        project = Some(to);
                    }
                    Some(_) => {}
                    None => plan.changes.push(bad(label)),
                }
            } else if let Some((key, label, values)) =
                ENUM_FIELDS.iter().find(|f| f.0 == key).copied()
            {
                match enum_code(values, &said) {
                    Some(code) if cur[key] != code => {
                        plan.changes.push(Change::new(
                            label,
                            enum_label(values, cur[key].as_str().unwrap_or("")),
                            enum_label(values, code),
                        ));
                        settings.insert(key.into(), json!(code));
                    }
                    Some(_) => {}
                    None => plan.changes.push(bad(label)),
                }
            } else if let Some((key, label)) = BOOL_FIELDS.iter().find(|f| f.0 == key).copied() {
                match parse_bool(&said) {
                    Some(b) if cur[key] != b => {
                        plan.changes.push(Change::new(label, yes_no(!b), yes_no(b)));
                        settings.insert(key.into(), json!(b));
                    }
                    Some(_) => {}
                    None => plan.changes.push(bad(label)),
                }
            } else {
                plan.changes.push(Change::new(
                    UNRECOGNIZED,
                    "—",
                    show(&format!("{key}: {said}")),
                ));
            }
        }
        if project.is_some() || !settings.is_empty() {
            let mut o = json!({"op": OP_PATCH, "id": r.id});
            if let Some(p) = project {
                o["project_id"] = json!(p);
            }
            if !settings.is_empty() {
                o["settings"] = Value::Object(settings);
            }
            plan.ops.push(o);
        }
        plan.ops.extend(grants);
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(repo::repo_get(c, id)?
            .filter(|r| r.workspace_id == ws)
            .map(|r| r.revision))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(repo::repos(c, &viewer.ws)?
            .into_iter()
            .map(|r| Candidate {
                code: format!("{KIND}:{}", r.id),
                label: format!("仓库 {}", r.full_name),
            })
            .collect())
    }
}

fn now_of(slot: &Slot) -> i64 {
    slot.get().map_or_else(steve_store::now, |g| g.now())
}

fn notify(slot: &Slot, ws: &str, id: &str) {
    if let Some(g) = slot.get() {
        g.notifier().gh(ws, id);
    }
}

struct RepoPatchOp(Slot);

impl ChangeOp for RepoPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let p = RepoPatch::parse_with(op, &["op", "id"])?;
        let r = own_repo(c, cx.ws, op["id"].as_str().unwrap_or(""))?;
        if let Some(Some(project)) = &p.project_id {
            check_project(c, cx.ws, project, Some(&r.id))?;
        }
        if let Some(m) = &p.settings {
            Settings::patched(&r.settings, m)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let p = RepoPatch::parse_with(op, &["op", "id"])?;
        let id = op["id"].as_str().unwrap_or("");
        let before = repo_snapshot(&own_repo(tx, cx.ws, id)?);
        patch_repo_tx(tx, cx.ws, cx.actor, id, &p, now_of(&self.0))?;
        notify(&self.0, cx.ws, id);
        Ok(before)
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let id = op["id"].as_str().unwrap_or("");
        let mut r = own_repo(tx, cx.ws, id)?;
        let was = repo_snapshot(&r);
        let project = before["project_id"].as_str().map(String::from);
        if let Some(p) = &project {
            check_project(tx, cx.ws, p, Some(&r.id))?;
        }
        r.project_id = project;
        r.settings = before["settings"].clone();
        if !r.settings.is_object() {
            r.settings = json!({});
        }
        if let Some(e) = before["enabled"].as_bool() {
            r.enabled = e;
        }
        let after = repo_snapshot(&r);
        if after != was {
            r.revision += 1;
            r.updated = now_of(&self.0);
            repo::update_repo(tx, &r)?;
            base::revision(
                tx, cx.ws, "gh_repo", &r.id, r.revision, &was, &after, cx.actor,
            )?;
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "github.repo_patch",
                &r.id,
                &json!({"undo": true}),
            )?;
            notify(&self.0, cx.ws, id);
        }
        Ok(())
    }
}

struct GrantOp(Slot);

struct GrantArgs<'a> {
    bot: &'a str,
    repo: &'a str,
    hours: i64,
}

fn grant_args(op: &Value) -> ApiResult<GrantArgs<'_>> {
    let bot = op["bot"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid("bot"))?;
    let repo = op["repo"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::invalid("repo"))?;
    let hours = op["hours"]
        .as_i64()
        .filter(|h| GRANT_HOURS.contains(h))
        .ok_or_else(|| ApiError::invalid("hours"))?;
    Ok(GrantArgs { bot, repo, hours })
}

impl ChangeOp for GrantOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a = grant_args(op)?;
        own_repo(c, cx.ws, a.repo)?;
        if find_bot(c, cx.ws, a.bot)?.as_deref() != Some(a.bot) {
            return Err(ApiError::not_found("Bot"));
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a = grant_args(op)?;
        let now = now_of(&self.0);
        let id = uid("gr");
        repo::insert_comment_grant(
            tx,
            &id,
            cx.ws,
            a.bot,
            a.repo,
            now + a.hours * HOUR_MS,
            cx.actor,
            None,
            now,
        )?;
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            "grant.add",
            a.repo,
            &json!({"grant": id, "capability": COMMENT_CAP, "bot": a.bot, "hours": a.hours,
                    "changeset": cx.changeset_id}),
        )?;
        notify(&self.0, cx.ws, a.repo);
        Ok(json!({ "grant": id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let Some(id) = before["grant"].as_str() else {
            return Ok(());
        };
        let n = tx.execute(
            "UPDATE grants SET revoked = ? WHERE id = ? AND workspace_id = ? AND revoked IS NULL",
            params![now_of(&self.0), id, cx.ws],
        )?;
        if n > 0 {
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "grant.revoke",
                op["repo"].as_str().unwrap_or(""),
                &json!({"grant": id, "capability": COMMENT_CAP, "undo": true}),
            )?;
        }
        Ok(())
    }
}

/// The open `github.comment` grants of a workspace as `(grant, bot, repo, until)`, for tests and
/// the grants list.
pub fn comment_grants(
    c: &Connection,
    ws: &str,
    now: i64,
) -> ApiResult<Vec<(String, String, String, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT id, subject_id, resource_id, until FROM grants
          WHERE workspace_id = ? AND capability = 'github.comment' AND revoked IS NULL AND until > ?
          ORDER BY created, id",
    )?;
    let rows = st
        .query_map(params![ws, now], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
