//! Conversational configuration of the knowledge base (slice K5, knowledge.md §8, contacts.md §0.8).
//!
//! Draft targets:
//! - `kb_source:<id>`: fields `name`, `paused`, `visibility`, `level` and, for folders, `include` /
//!   `exclude`; planned into one `kb.source.patch`. A built-in source's visibility or level is only
//!   changed by the workspace owner (the same rule as `PATCH /kb/sources/:id`).
//! - `kb_source:new` and `knowledge:new`: `kb_source.create` (folder path checked by §2.2 while
//!   planning: a refused path gives exactly one line with the reason and no operation), and on
//!   `knowledge` also `mem.add` (the user said it, so it is `confirmed`), `note.create`,
//!   `doc.patch` (document by exact title).
//!
//! The model is shown the spec's names (`mem.add`, `kb_source.create` …); stored operations carry a
//! `kb.` prefix (`kb.mem.add`, `kb.source.create` …) because contacts already registers `mem.add`.
//! Names resolve only by exact name or alias; anything else is a “（没认出：X）” line. Drafts never
//! carry secrets: text the guard flags is not drafted.
//!
//! Integration (integrator): `steve_knowledge::drafts::register(&mut registry, kb.clone())` with the
//! same [`Knowledge`] given to the routes and to [`crate::spawn_scans`] (which runs the first scan of
//! a folder a draft added, right after the changeset commits).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use steve_modules::changes::{
    Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared,
};
use steve_modules::contacts::repo::{self as people, ContactQuery, Party};
use steve_modules::{ApiError, ApiResult, Caller, Check, Registry};
use steve_store::base::mems::{self, NewMem};
use steve_store::base::{self, Change, Level, ObjRef, Viewer, Visibility, show};
use steve_store::{Tx, now};

use crate::index;
use crate::notes::{self, NewNote};
use crate::repo::{self, DocFilter, DocRow, SourceRow};
use crate::service::own_doc;
use crate::sources::folder::{self, OWNER_ONLY, SourceBody, SourcePatch, check_path};
use crate::sources::{self, is_builtin};
use crate::state::Knowledge;
use crate::sync;

pub const KIND_SOURCE: &str = "kb_source";
pub const KIND_KNOWLEDGE: &str = "knowledge";

pub const OP_SOURCE_PATCH: &str = "kb.source.patch";
pub const OP_SOURCE_CREATE: &str = "kb.source.create";
pub const OP_MEM_ADD: &str = "kb.mem.add";
pub const OP_NOTE_CREATE: &str = "kb.note.create";
pub const OP_DOC_PATCH: &str = "kb.doc.patch";

const UNRECOGNIZED: &str = "（没认出）";
const SECRET: &str = "这段话里像是有密码或令牌，草稿不处理机密，请在页面上填写";
const MEM_KINDS: [(&str, &str); 4] = [
    ("fact", "事实"),
    ("preference", "偏好"),
    ("rule", "规则"),
    ("event", "事件"),
];
const LEVELS: [(Level, &str); 4] = [
    (Level::Public, "公开"),
    (Level::Conclusion, "可转述结论"),
    (Level::Reasoning, "仅用于推理"),
    (Level::Never, "不可使用"),
];
const PRIVATE: &str = "只有我";
const WORKSPACE: &str = "工作空间";
const SYNCING: &str = "同步中";
const PAUSED: &str = "已暂停";
const FOLLOW: &str = "跟随来源";

/// Registers both targets and the five operations.
pub fn register(r: &mut Registry, kb: Knowledge) {
    r.target(KIND_SOURCE, Box::new(SourceTarget(kb.clone())))
        .target(KIND_KNOWLEDGE, Box::new(KnowledgeTarget(kb.clone())))
        .op(OP_SOURCE_PATCH, Box::new(SourcePatchOp(kb.clone())))
        .op(OP_SOURCE_CREATE, Box::new(SourceCreateOp(kb.clone())))
        .op(OP_MEM_ADD, Box::new(MemAddOp(kb.clone())))
        .op(OP_NOTE_CREATE, Box::new(NoteCreateOp(kb.clone())))
        .op(OP_DOC_PATCH, Box::new(DocPatchOp(kb)));
}

// ---------------------------------------------------------------- labels and parsing

pub fn level_label(l: Level) -> &'static str {
    LEVELS
        .iter()
        .find(|(x, _)| *x == l)
        .map(|(_, s)| *s)
        .unwrap_or("可转述结论")
}

fn parse_level(s: &str) -> Option<Level> {
    let s = s.trim();
    Level::parse(&s.to_ascii_lowercase()).or(match s {
        "公开" | "可以引用" | "可引用" => Some(Level::Public),
        "可转述结论" | "可转述" | "只说结论" | "结论" => Some(Level::Conclusion),
        "仅用于推理" | "只用于推理" | "仅推理" | "推理" => Some(Level::Reasoning),
        "不可使用" | "不能使用" | "不使用" => Some(Level::Never),
        _ => None,
    })
}

fn paused_label(p: bool) -> &'static str {
    if p { PAUSED } else { SYNCING }
}

fn parse_paused(s: &str) -> Option<bool> {
    match s.trim() {
        "已暂停" | "暂停" | "true" | "是" | "停" | "停止" | "paused" => Some(true),
        "同步中" | "恢复" | "继续" | "false" | "否" | "同步" | "syncing" => Some(false),
        _ => None,
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn named(p: &Party, n: &str) -> bool {
    same(&p.name, n) || p.aliases.iter().any(|a| same(a, n))
}

fn teams(c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Party>> {
    Ok(steve_modules::contacts::teams::repo::teams(c, viewer)?
        .into_iter()
        .map(|(p, _)| p)
        .collect())
}

/// “只有我”, “工作空间”, a team's or a space's name.
pub fn vis_label(c: &Connection, v: &Visibility) -> String {
    let name = |sql: &str, id: &str| -> String {
        c.query_row(sql, [id], |r| r.get::<_, String>(0))
            .unwrap_or_else(|_| id.to_string())
    };
    match v {
        Visibility::Private => PRIVATE.into(),
        Visibility::Workspace => WORKSPACE.into(),
        Visibility::Team(id) => format!(
            "团队「{}」",
            name("SELECT name FROM parties WHERE id = ?", id)
        ),
        Visibility::Space(id) => format!(
            "空间「{}」",
            name("SELECT name FROM spaces WHERE id = ?", id)
        ),
    }
}

/// A visibility by code, label or exact team name; `None` = not recognized (never guessed).
fn parse_vis(c: &Connection, viewer: &Viewer, s: &str) -> ApiResult<Option<Visibility>> {
    let s = s.trim();
    match s {
        "只有我" | "仅我" | "我" | "只我" | "私有" | "自己" | "private" => {
            return Ok(Some(Visibility::Private));
        }
        "工作空间" | "所有人" | "大家" | "全员" | "workspace" => {
            return Ok(Some(Visibility::Workspace));
        }
        _ => {}
    }
    if let Some(v) = Visibility::parse(s) {
        return Ok(repo::scope_exists(c, &viewer.ws, &v)?.then_some(v));
    }
    let n = s
        .trim_start_matches("团队")
        .trim_matches(|ch| ch == '「' || ch == '」' || ch == ' ');
    let found: Vec<Party> = teams(c, viewer)?
        .into_iter()
        .filter(|t| named(t, n))
        .collect();
    Ok(match found.as_slice() {
        [t] => Some(Visibility::Team(t.id.clone())),
        _ => None,
    })
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn list_of(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(xs) => xs.iter().map(text_of).collect(),
        Value::String(s) => Some(
            s.split([',', '，', '、', '\n'])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect(),
        ),
        _ => None,
    }
}

fn unknown(what: &str, said: &str) -> Change {
    Change::new(
        format!("（没认出：{}）", said.trim()),
        "—",
        format!("没有改{what}"),
    )
}

/// The operation name as the model wrote it, without the optional `kb.` prefix.
fn op_name(op: &Value) -> String {
    let n = op.get("op").and_then(Value::as_str).unwrap_or("").trim();
    let n = n.strip_prefix("kb.").unwrap_or(n);
    match n {
        "source.create" => "kb_source.create".into(),
        "source.patch" => "kb_source.patch".into(),
        n => n.into(),
    }
}

fn caller_of(viewer: &Viewer) -> Caller {
    Caller {
        ws: viewer.ws.clone(),
        me: viewer.user.clone(),
    }
}

fn cx_caller(cx: &OpCx) -> Caller {
    Caller {
        ws: cx.ws.into(),
        me: cx.actor.into(),
    }
}

/// Sources the viewer can see (built-ins always; folders by their visibility).
fn visible_sources(c: &Connection, viewer: &Viewer) -> ApiResult<Vec<SourceRow>> {
    Ok(repo::sources(c, &viewer.ws)?
        .into_iter()
        .filter(|s| is_builtin(&s.kind) || viewer.sees(&s.visibility, &s.created_by))
        .collect())
}

// ---------------------------------------------------------------- kb_source:<id>

struct SourceTarget(Knowledge);

fn source_href(id: &str) -> String {
    format!("#/knowledge?tab=sources&id={id}")
}

impl DraftTarget for SourceTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = match id {
            None | Some("new") => return Ok(create_prepared()),
            Some(id) => id,
        };
        let s = folder::own_source(c, &caller_of(viewer), id)?;
        let mut vis_values = vec![PRIVATE.to_string(), WORKSPACE.to_string()];
        vis_values.extend(teams(c, viewer)?.into_iter().map(|t| t.name));
        let levels: Vec<&str> = LEVELS.iter().map(|(_, l)| *l).collect();
        let mut fields = vec![
            FieldSpec::new("name", "名字", "text", json!(s.name)),
            FieldSpec::new("paused", "状态", "enum", json!(paused_label(s.paused)))
                .values(&[SYNCING, PAUSED]),
            FieldSpec {
                values: Some(vis_values),
                ..FieldSpec::new(
                    "visibility",
                    "新文档的可见范围（只有我 / 工作空间 / 团队名）",
                    "enum",
                    json!(vis_label(c, &s.visibility)),
                )
            },
            FieldSpec::new(
                "level",
                "新文档的披露级别",
                "enum",
                json!(level_label(s.level)),
            )
            .values(&levels),
        ];
        if s.kind == "folder" {
            for (key, label) in [("include", "包含（glob）"), ("exclude", "排除（glob）")] {
                let cur = s.config.get(key).cloned().unwrap_or(json!([]));
                fields.push(FieldSpec::new(key, label, "list", cur));
            }
        }
        Ok(Prepared {
            target_id: Some(s.id.clone()),
            revision: Some(s.revision),
            title: format!("知识来源「{}」", s.name),
            href: source_href(&s.id),
            subject: format!("the knowledge source \"{}\"", s.name),
            fields,
            ops: vec![],
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
        let Some(id) = &p.target_id else {
            return plan_ops(&self.0, c, viewer, raw, &["kb_source.create"]);
        };
        let s = folder::own_source(c, &caller_of(viewer), id)?;
        let mut plan = Plan::default();
        let mut patch = SourcePatch::default();
        for op in raw {
            let pairs: Vec<(String, Value)> = match op_name(&op).as_str() {
                "set" => vec![(
                    op.get("key")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    op.get("value").cloned().unwrap_or(Value::Null),
                )],
                "kb_source.patch" => op
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .filter(|(k, _)| !matches!(k.as_str(), "op" | "id"))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => {
                    plan.changes
                        .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                    continue;
                }
            };
            for (key, v) in pairs {
                if let Some(line) = patch_field(c, viewer, &s, &key, &v, &mut patch)? {
                    plan.changes.push(line);
                }
            }
        }
        if patch != SourcePatch::default() {
            plan.ops
                .push(json!({ "op": OP_SOURCE_PATCH, "id": s.id, "patch": patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(repo::source(c, id)?
            .filter(|s| s.workspace_id == ws)
            .map(|s| s.revision))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(visible_sources(c, viewer)?
            .into_iter()
            .map(|s| Candidate {
                code: format!("kb_source:{}", s.id),
                label: format!("知识来源 {}", s.name),
            })
            .collect())
    }
}

/// One field of a `kb_source` patch: records it in `patch` and returns its preview line (`None`
/// when the value is what it already is).
fn patch_field(
    c: &Connection,
    viewer: &Viewer,
    s: &SourceRow,
    key: &str,
    v: &Value,
    patch: &mut SourcePatch,
) -> ApiResult<Option<Change>> {
    let raw = text_of(v).unwrap_or_default();
    let folder = s.kind == "folder";
    let owner_only =
        |label: &str, from: String| Change::new(label, from, format!("（没有改：{OWNER_ONLY}）"));
    let may_rescope = !is_builtin(&s.kind) || folder::is_owner(c, &viewer.ws, &viewer.user)?;
    Ok(match key {
        "name" => {
            let n: String = raw.trim().to_string();
            if !(1..=40).contains(&n.chars().count()) {
                return Ok(Some(unknown("名字", &raw)));
            }
            if n == s.name {
                return Ok(None);
            }
            patch.name = Some(n.clone());
            Some(Change::new("名字", show(&s.name), show(&n)))
        }
        "paused" => {
            let Some(p) = parse_paused(&raw) else {
                return Ok(Some(unknown("状态", &raw)));
            };
            if p == s.paused {
                return Ok(None);
            }
            patch.paused = Some(p);
            Some(Change::new("状态", paused_label(s.paused), paused_label(p)))
        }
        "visibility" => {
            let Some(vis) = parse_vis(c, viewer, &raw)? else {
                return Ok(Some(unknown("可见范围", &raw)));
            };
            if vis == s.visibility {
                return Ok(None);
            }
            let from = vis_label(c, &s.visibility);
            if !may_rescope {
                return Ok(Some(owner_only("可见范围", from)));
            }
            let to = vis_label(c, &vis);
            patch.visibility = Some(vis);
            Some(Change::new("可见范围", from, to))
        }
        "level" => {
            let Some(l) = parse_level(&raw) else {
                return Ok(Some(unknown("披露级别", &raw)));
            };
            if l == s.level {
                return Ok(None);
            }
            if !may_rescope {
                return Ok(Some(owner_only("披露级别", level_label(s.level).into())));
            }
            patch.level = Some(l);
            Some(Change::new(
                "披露级别",
                level_label(s.level),
                level_label(l),
            ))
        }
        "include" | "exclude" if folder => {
            let label = if key == "include" { "包含" } else { "排除" };
            let max = if key == "include" { 20 } else { 50 };
            let list = match list_of(v) {
                Some(l) if l.len() <= max && l.iter().all(|x| crate::scan::valid_pattern(x)) => l,
                _ => return Ok(Some(unknown(label, &v.to_string()))),
            };
            let cur: Vec<String> = s.config.get(key).and_then(list_of_ref).unwrap_or_default();
            if list == cur {
                return Ok(None);
            }
            let line = Change::new(label, show(&cur.join("、")), show(&list.join("、")));
            if key == "include" {
                patch.include = Some(list);
            } else {
                patch.exclude = Some(list);
            }
            Some(line)
        }
        _ => {
            let said = text_of(v).unwrap_or_else(|| v.to_string());
            Some(Change::new(
                UNRECOGNIZED,
                "—",
                show(&format!("{key}: {said}")),
            ))
        }
    })
}

fn list_of_ref(v: &Value) -> Option<Vec<String>> {
    list_of(v)
}

struct SourcePatchOp(Knowledge);

fn patch_args(op: &Value) -> ApiResult<(String, SourcePatch)> {
    let id = op["id"].as_str().unwrap_or("").to_string();
    let patch: SourcePatch =
        serde_json::from_value(op["patch"].clone()).map_err(|_| ApiError::invalid("patch"))?;
    Ok((id, patch))
}

impl ChangeOp for SourcePatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let (id, patch) = patch_args(op)?;
        folder::check_patch(&patch)?;
        let who = cx_caller(cx);
        let old = folder::own_source(c, &who, &id)?;
        if folder::owner_refuses(c, &who, &old, &patch)? {
            return Err(ApiError::forbidden(OWNER_ONLY));
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let (id, patch) = patch_args(op)?;
        let p = folder::patch_tx(tx, &cx_caller(cx), &id, &patch, now())?;
        if p.rescan {
            self.0.request_scan(&id);
        }
        self.0.notify(cx.ws, &id);
        Ok(serde_json::to_value(SourcePatch::restoring(&p.old))?)
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let (id, _) = patch_args(op)?;
        let back: SourcePatch =
            serde_json::from_value(before.clone()).map_err(|_| ApiError::invalid("before"))?;
        let p = folder::patch_tx(tx, &cx_caller(cx), &id, &back, now())?;
        if p.rescan {
            self.0.request_scan(&id);
        }
        self.0.notify(cx.ws, &id);
        Ok(())
    }
}

// ---------------------------------------------------------------- kb_source:new, knowledge:new

fn level_codes() -> String {
    Level::ALL.map(Level::code).join("|")
}

fn create_spec() -> OpSpec {
    OpSpec::new(
        "kb_source.create",
        &format!(
            "{{\"kind\": \"folder\", \"path\": <absolute path or ~/…>, \"name\"?: <1-40 chars>, \"visibility\"?: \"private|workspace|<team name>\", \"level\"?: \"{}\", \"include\"?: [glob], \"exclude\"?: [glob]}} — add a folder on this computer to the knowledge base",
            level_codes()
        ),
    )
}

fn create_prepared() -> Prepared {
    Prepared {
        target_id: None,
        revision: None,
        title: "新增知识来源".into(),
        href: "#/knowledge?tab=sources".into(),
        subject: "the user's knowledge sources (a new folder)".into(),
        fields: vec![],
        ops: vec![create_spec()],
        free_text: None,
        context: Value::Null,
    }
}

struct KnowledgeTarget(Knowledge);

const KNOWLEDGE_OPS: [&str; 4] = ["mem.add", "note.create", "doc.patch", "kb_source.create"];

impl DraftTarget for KnowledgeTarget {
    fn prepare(&self, _: &Connection, _: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        if !matches!(id, None | Some("new")) {
            return Err(ApiError::not_found("知识库"));
        }
        let lv = level_codes();
        Ok(Prepared {
            target_id: None,
            revision: None,
            title: "知识库".into(),
            href: "#/knowledge".into(),
            subject: "the user's knowledge base (memories, notes, documents, sources)".into(),
            fields: vec![],
            ops: vec![
                OpSpec::new(
                    "mem.add",
                    &format!(
                        "{{\"kind\": \"fact|preference|rule|event\", \"text\": <the fact in one sentence, in the user's language, without the instruction words>, \"level\"?: \"{lv}\", \"visibility\"?: \"private|workspace|<team name>\", \"about\"?: [<exact names of people, organizations, teams, bots or projects>]}} — remember something the user said"
                    ),
                ),
                OpSpec::new(
                    "note.create",
                    &format!(
                        "{{\"title\": <1-120 chars>, \"body\": <Markdown>, \"level\"?: \"{lv}\", \"visibility\"?: \"private|workspace|<team name>\"}} — write a longer note"
                    ),
                ),
                OpSpec::new(
                    "doc.patch",
                    &format!(
                        "{{\"doc\": <exact document title>, \"level\"?: \"{lv}|follow\", \"visibility\"?: \"private|workspace|<team name>|follow\"}} — change a document's level or visibility (follow = back to its source's)"
                    ),
                ),
                create_spec(),
            ],
            free_text: None,
            context: Value::Null,
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        _: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        plan_ops(&self.0, c, viewer, raw, &KNOWLEDGE_OPS)
    }

    fn revision(&self, _: &Connection, _: &str, _: &str) -> ApiResult<Option<i64>> {
        Ok(None)
    }

    fn candidates(&self, _: &Connection, _: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(vec![Candidate {
            code: "knowledge:new".into(),
            label: "记一条记忆或笔记".into(),
        }])
    }
}

/// Plans the creating operations `allowed` on this target; others are “（没认出）” lines.
fn plan_ops(
    kb: &Knowledge,
    c: &Connection,
    viewer: &Viewer,
    raw: Vec<Value>,
    allowed: &[&str],
) -> ApiResult<Plan> {
    let mut plan = Plan::default();
    for op in raw {
        let name = op_name(&op);
        if !allowed.contains(&name.as_str()) {
            plan.changes
                .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
            continue;
        }
        let (stored, lines) = match name.as_str() {
            "kb_source.create" => plan_create(kb, c, viewer, &op)?,
            "mem.add" => plan_mem(kb, c, viewer, &op)?,
            "note.create" => plan_note(kb, c, viewer, &op)?,
            "doc.patch" => plan_doc(c, viewer, &op)?,
            _ => (None, vec![]),
        };
        plan.changes.extend(lines);
        plan.ops.extend(stored);
    }
    Ok(plan)
}

type Planned = (Option<Value>, Vec<Change>);
/// A visibility and a level, each optional.
type Scope = (Option<Visibility>, Option<Level>);
/// Resolved `about` objects with their names.
type Abouts = Vec<(ObjRef, String)>;

/// Optional `visibility` / `level` of a raw op: `Err(line)` when said but not recognized.
fn scope_of(c: &Connection, viewer: &Viewer, op: &Value) -> ApiResult<Result<Scope, Change>> {
    let vis = match op
        .get("visibility")
        .and_then(text_of)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(s) => match parse_vis(c, viewer, &s)? {
            Some(v) => Some(v),
            None => return Ok(Err(unknown("可见范围", &s))),
        },
    };
    let level = match op.get("level").and_then(text_of).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => match parse_level(&s) {
            Some(l) => Some(l),
            None => return Ok(Err(unknown("披露级别", &s))),
        },
    };
    Ok(Ok((vis, level)))
}

/// `~` and `~/…` are the Hub user's home.
fn expand_home(kb: &Knowledge, raw: &str) -> String {
    let home = &kb.folder_policy().home;
    match raw.trim() {
        "~" => home.to_string_lossy().into_owned(),
        r => match r.strip_prefix("~/") {
            Some(rest) => home.join(rest).to_string_lossy().into_owned(),
            None => r.to_string(),
        },
    }
}

fn folder_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().chars().take(40).collect())
        .filter(|n: &String| !n.is_empty())
        .unwrap_or_else(|| "文件夹".into())
}

/// `kb_source.create`: the path is checked now (§2.2); a refusal is the only line, with no op.
fn plan_create(kb: &Knowledge, c: &Connection, viewer: &Viewer, op: &Value) -> ApiResult<Planned> {
    let said = op.get("path").and_then(text_of).unwrap_or_default();
    if said.is_empty() {
        return Ok((
            None,
            vec![Change::new("（没说清楚）", "—", "缺少文件夹路径")],
        ));
    }
    let raw = expand_home(kb, &said);
    let refused = |why: String| {
        Ok((
            None,
            vec![Change::new(
                format!("新增知识来源 {raw}"),
                "—",
                folder::refusal(&why),
            )],
        ))
    };
    let path = match check_path(kb.folder_policy(), &raw) {
        Ok(p) => p,
        Err(why) => return refused(why),
    };
    if let Some(why) = folder::overlap_reason(c, &viewer.ws, &path)? {
        return refused(why);
    }
    let (vis, level) = match scope_of(c, viewer, op)? {
        Ok(x) => x,
        Err(line) => return Ok((None, vec![line])),
    };
    let mut lists = Map::new();
    for (key, max) in [("include", 20), ("exclude", 50)] {
        if let Some(v) = op.get(key).filter(|v| !v.is_null()) {
            match list_of(v) {
                Some(l) if l.len() <= max && l.iter().all(|x| crate::scan::valid_pattern(x)) => {
                    lists.insert(key.into(), json!(l));
                }
                _ => {
                    return Ok((
                        None,
                        vec![unknown(
                            if key == "include" { "包含" } else { "排除" },
                            &v.to_string(),
                        )],
                    ));
                }
            }
        }
    }
    let name = op
        .get("name")
        .and_then(text_of)
        .filter(|n| (1..=40).contains(&n.chars().count()))
        .unwrap_or_else(|| folder_name(&path));
    let vis = vis.unwrap_or(Visibility::Private);
    let level = level.unwrap_or(Level::Conclusion);
    let shown = path.to_string_lossy().into_owned();
    let line = Change::new(
        format!("新增知识来源 {shown}"),
        "—",
        format!(
            "「{name}」 · {} · {}",
            vis_label(c, &vis),
            level_label(level)
        ),
    );
    let mut stored = json!({
        "op": OP_SOURCE_CREATE, "kind": "folder", "path": shown, "name": name,
        "visibility": vis, "level": level, "as": "source",
    });
    if let Some(o) = stored.as_object_mut() {
        o.extend(lists);
    }
    Ok((Some(stored), vec![line]))
}

/// Resolves `about` names: “我”, contacts, organizations, teams (name or alias), bots and projects
/// (exact name). Returns the references and a line per name that did not resolve to exactly one.
fn resolve_about(
    c: &Connection,
    viewer: &Viewer,
    names: &[String],
) -> ApiResult<(Abouts, Vec<Change>)> {
    let mut refs: Vec<(ObjRef, String)> = Vec::new();
    let mut lines = Vec::new();
    for said in names {
        let n = said.trim();
        if n.is_empty() {
            continue;
        }
        let mut found: Vec<(ObjRef, String)> = Vec::new();
        if n == "我" || n.eq_ignore_ascii_case("me") {
            if let Some(me) = people::me_of(c, &viewer.ws, &viewer.user)? {
                found.push((ObjRef::new("party", &me.id), me.name));
            }
        } else {
            let q = ContactQuery {
                q: Some(n.to_string()),
                limit: 500,
                ..Default::default()
            };
            let parties = people::contacts(c, viewer, &q)?
                .into_iter()
                .chain(people::orgs(c, viewer)?)
                .chain(teams(c, viewer)?);
            for p in parties.filter(|p| named(p, n)) {
                if !found.iter().any(|(r, _)| r.id == p.id) {
                    found.push((ObjRef::new("party", &p.id), p.name));
                }
            }
            let bot: Option<(String, String)> = c
                .query_row(
                    "SELECT b.id, b.name FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ? WHERE b.deleted = 0 AND b.name = ? LIMIT 1",
                    params![viewer.ws, n],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            found.extend(bot.map(|(id, name)| (ObjRef::new("bot", &id), name)));
            let project: Option<(String, String)> = c
                .query_row(
                    "SELECT id, name FROM projects WHERE workspace_id = ? AND name = ? AND archived = 0 LIMIT 1",
                    params![viewer.ws, n],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            found.extend(project.map(|(id, name)| (ObjRef::new("project", &id), name)));
        }
        match found.len() {
            1 => {
                let f = found.remove(0);
                if !refs.iter().any(|(r, _)| *r == f.0) {
                    refs.push(f);
                }
            }
            0 => lines.push(Change::new(
                format!("（没认出：{n}）"),
                "—",
                "没有加进“关于”",
            )),
            k => lines.push(Change::new(
                format!("（没认出：{n}）"),
                "—",
                format!("有 {k} 个同名的，没有自动选"),
            )),
        }
    }
    Ok((refs, lines))
}

fn mem_kind(s: &str) -> Option<&'static str> {
    MEM_KINDS
        .iter()
        .find(|(code, label)| same(code, s) || *label == s.trim())
        .map(|(code, _)| *code)
}

fn mem_kind_label(code: &str) -> &'static str {
    MEM_KINDS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, l)| *l)
        .unwrap_or("事实")
}

/// `mem.add`: one line “新记忆 — <text> · <种类> · <级别> · <范围> · 关于 …”.
fn plan_mem(kb: &Knowledge, c: &Connection, viewer: &Viewer, op: &Value) -> ApiResult<Planned> {
    let text = op.get("text").and_then(text_of).unwrap_or_default();
    if !(1..=2000).contains(&text.chars().count()) {
        return Ok((
            None,
            vec![Change::new("（没说清楚）", "—", "缺少要记住的内容")],
        ));
    }
    if !kb.guard.scan(&text).is_empty() {
        return Ok((None, vec![Change::new("（没有记下）", "—", SECRET)]));
    }
    let kind_said = op.get("kind").and_then(text_of).unwrap_or_default();
    let kind = if kind_said.is_empty() {
        "fact"
    } else {
        match mem_kind(&kind_said) {
            Some(k) => k,
            None => return Ok((None, vec![unknown("种类", &kind_said)])),
        }
    };
    let (vis, level) = match scope_of(c, viewer, op)? {
        Ok(x) => x,
        Err(line) => return Ok((None, vec![line])),
    };
    let names = op.get("about").and_then(list_of).unwrap_or_default();
    let (about, mut lines) = resolve_about(c, viewer, &names)?;
    let about = about.into_iter().take(10).collect::<Vec<_>>();
    let vis = vis.unwrap_or(Visibility::Private);
    let level = level.unwrap_or(Level::Conclusion);
    let mut to = format!(
        "{} · {} · {} · {}",
        show(&text),
        mem_kind_label(kind),
        level_label(level),
        vis_label(c, &vis)
    );
    if !about.is_empty() {
        let names: Vec<&str> = about.iter().map(|(_, n)| n.as_str()).collect();
        to.push_str(&format!(" · 关于 {}", names.join("、")));
    }
    lines.insert(0, Change::new("新记忆", "—", to));
    let refs: Vec<&ObjRef> = about.iter().map(|(r, _)| r).collect();
    Ok((
        Some(json!({
            "op": OP_MEM_ADD, "kind": kind, "text": text, "level": level,
            "visibility": vis, "about": refs,
        })),
        lines,
    ))
}

/// `note.create`: one line “新建笔记 — <title>”.
fn plan_note(kb: &Knowledge, c: &Connection, viewer: &Viewer, op: &Value) -> ApiResult<Planned> {
    let body = op.get("body").and_then(text_of).unwrap_or_default();
    let title = op
        .get("title")
        .and_then(text_of)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            body.lines()
                .map(|l| l.trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect()
        });
    if title.is_empty() || title.chars().count() > 120 || body.len() > 200_000 {
        return Ok((
            None,
            vec![Change::new("（没说清楚）", "—", "笔记缺少标题或太长")],
        ));
    }
    if !kb.guard.scan(&title).is_empty() || !kb.guard.scan(&body).is_empty() {
        return Ok((None, vec![Change::new("（没有记下）", "—", SECRET)]));
    }
    let (vis, level) = match scope_of(c, viewer, op)? {
        Ok(x) => x,
        Err(line) => return Ok((None, vec![line])),
    };
    let mut to = show(&title);
    if let Some(l) = level {
        to.push_str(&format!(" · {}", level_label(l)));
    }
    if let Some(v) = &vis {
        to.push_str(&format!(" · {}", vis_label(c, v)));
    }
    Ok((
        Some(json!({
            "op": OP_NOTE_CREATE, "title": title, "body": body,
            "visibility": vis, "level": level,
        })),
        vec![Change::new("新建笔记", "—", to)],
    ))
}

fn is_follow(s: &str) -> bool {
    matches!(s.trim(), "follow" | "跟随来源" | "跟随" | "null" | "默认")
}

/// `doc.patch`: the document by exact title among those the viewer sees; one line per field.
fn plan_doc(c: &Connection, viewer: &Viewer, op: &Value) -> ApiResult<Planned> {
    let title = op
        .get("doc")
        .or_else(|| op.get("title"))
        .and_then(text_of)
        .unwrap_or_default();
    if title.is_empty() {
        return Ok((
            None,
            vec![Change::new("（没说清楚）", "—", "没说是哪篇文档")],
        ));
    }
    let f = DocFilter {
        q: Some(title.clone()),
        limit: 200,
        ..Default::default()
    };
    let docs: Vec<DocRow> = repo::docs_visible(c, viewer, &f)?
        .into_iter()
        .filter(|d| same(&d.title, &title))
        .collect();
    let d = match docs.as_slice() {
        [d] => d,
        [] => {
            return Ok((
                None,
                vec![Change::new(
                    format!("（没认出：{title}）"),
                    "—",
                    "没有这篇文档",
                )],
            ));
        }
        many => {
            return Ok((
                None,
                vec![Change::new(
                    format!("（没认出：{title}）"),
                    "—",
                    format!("有 {} 篇同名的，没有自动选", many.len()),
                )],
            ));
        }
    };
    let s = repo::source(c, &d.source_id)?.ok_or_else(|| ApiError::not_found("文档"))?;
    let mut set = Map::new();
    let mut lines = Vec::new();
    if let Some(said) = op
        .get("visibility")
        .and_then(text_of)
        .filter(|s| !s.is_empty())
    {
        let new = if is_follow(&said) {
            Some(None)
        } else {
            parse_vis(c, viewer, &said)?.map(Some)
        };
        match new {
            None => lines.push(unknown("可见范围", &said)),
            Some(v) if v != d.visibility => {
                let to = match &v {
                    Some(v) => vis_label(c, v),
                    None => format!("{FOLLOW}（{}）", vis_label(c, &s.visibility)),
                };
                lines.push(Change::new(
                    format!("「{}」的可见范围", d.title),
                    vis_label(c, &d.eff_visibility(&s)),
                    to,
                ));
                set.insert("visibility".into(), json!(v));
            }
            Some(_) => {}
        }
    }
    if let Some(said) = op.get("level").and_then(text_of).filter(|s| !s.is_empty()) {
        let new = if is_follow(&said) {
            Some(None)
        } else {
            parse_level(&said).map(Some)
        };
        match new {
            None => lines.push(unknown("披露级别", &said)),
            Some(l) if l != d.level => {
                let to = match l {
                    Some(l) => level_label(l).to_string(),
                    None => format!("{FOLLOW}（{}）", level_label(s.level)),
                };
                lines.push(Change::new(
                    format!("「{}」的披露级别", d.title),
                    level_label(d.eff_level(&s)),
                    to,
                ));
                set.insert("level".into(), json!(l));
            }
            Some(_) => {}
        }
    }
    if set.is_empty() {
        return Ok((None, lines));
    }
    Ok((
        Some(json!({ "op": OP_DOC_PATCH, "doc": d.id, "set": set })),
        lines,
    ))
}

// ---------------------------------------------------------------- creating operations

/// `kb.source.create {kind:"folder", path, name, visibility, level, include?, exclude?}`.
struct SourceCreateOp(Knowledge);

fn source_body(op: &Value) -> ApiResult<SourceBody> {
    serde_json::from_value(op.clone()).map_err(|_| ApiError::invalid("op"))
}

impl ChangeOp for SourceCreateOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let f = folder::check_new(self.0.folder_policy(), &source_body(op)?)?;
        if let Some(why) = folder::overlap_reason(c, cx.ws, &f.path)? {
            return Err(ApiError::rule(folder::refusal(&why)));
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let f = folder::check_new(self.0.folder_policy(), &source_body(op)?)?;
        let at = now();
        sources::ensure_builtin(tx, cx.ws, cx.actor, at)?;
        let s = folder::insert_folder(tx, cx.ws, cx.actor, &f, at)?;
        cx.bind("source", json!(s.id));
        self.0.request_scan(&s.id);
        self.0.notify(cx.ws, &s.id);
        Ok(json!({ "id": s.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        let id = before["id"].as_str().unwrap_or("");
        if let Some(s) = repo::source(tx, id)?.filter(|s| s.workspace_id == cx.ws) {
            let owner = Caller {
                ws: cx.ws.into(),
                me: s.created_by.clone(),
            };
            folder::delete_tx(tx, &owner, id)?;
            self.0.notify(cx.ws, id);
        }
        Ok(())
    }
}

fn scope_arg<T: serde::de::DeserializeOwned>(op: &Value, key: &str) -> ApiResult<Option<T>> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|_| ApiError::invalid(key)),
    }
}

/// `kb.mem.add {kind, text, level, visibility, about:[{type,id}]}`: confirmed (the user said it).
struct MemAddOp(Knowledge);

fn check_about(c: &Connection, cx: &OpCx, about: &[ObjRef]) -> ApiResult<()> {
    for a in about {
        if !repo::ref_exists(c, cx.ws, cx.actor, a)? {
            return Err(ApiError::not_found(if a.kind == "bot" {
                "Bot"
            } else if a.kind == "project" {
                "项目"
            } else {
                "联系人"
            }));
        }
    }
    Ok(())
}

struct MemArgs {
    kind: String,
    text: String,
    level: Level,
    visibility: Visibility,
    about: Vec<ObjRef>,
}

fn mem_args(op: &Value) -> ApiResult<MemArgs> {
    let kind = op["kind"].as_str().unwrap_or("fact").to_string();
    let text = op["text"].as_str().unwrap_or("").trim().to_string();
    let about: Vec<ObjRef> = scope_arg(op, "about")?.unwrap_or_default();
    Check::new()
        .len("text", &text, 1, 2000)
        .one_of("kind", &kind, &["fact", "preference", "rule", "event"])
        .that("about", about.len() <= 10)
        .done()?;
    Ok(MemArgs {
        kind,
        text,
        level: scope_arg(op, "level")?.unwrap_or(Level::Conclusion),
        visibility: scope_arg(op, "visibility")?.unwrap_or(Visibility::Private),
        about,
    })
}

impl ChangeOp for MemAddOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a = mem_args(op)?;
        if !self.0.guard.scan(&a.text).is_empty() {
            return Err(ApiError::rule(SECRET));
        }
        if !repo::scope_exists(c, cx.ws, &a.visibility)? {
            return Err(ApiError::not_found("团队"));
        }
        check_about(c, cx, &a.about)
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a = mem_args(op)?;
        let at = now();
        sources::ensure_builtin(tx, cx.ws, cx.actor, at)?;
        let m = mems::create(
            tx,
            cx.ws,
            &NewMem {
                kind: a.kind.clone(),
                text: a.text,
                source_type: "me".into(),
                source_label: "你说的".into(),
                visibility: a.visibility,
                level: a.level,
                status: "confirmed".into(),
                writer: "me".into(),
                created_by: cx.actor.into(),
                about: a.about,
                ..Default::default()
            },
        )?;
        sync::sync_mem(tx, &m.id, at)?;
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            "mem.create",
            &m.id,
            &json!({ "kind": a.kind, "changeset": cx.changeset_id }),
        )?;
        cx.bind("mem", json!(m.id));
        Ok(json!({ "id": m.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        if let Some(id) = before["id"].as_str() {
            mems::delete(tx, id)?;
            sync::sync_mem(tx, id, now())?;
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "mem.delete",
                id,
                &json!({ "undo": cx.changeset_id }),
            )?;
        }
        Ok(())
    }
}

/// `kb.note.create {title, body, visibility?, level?}`.
struct NoteCreateOp(Knowledge);

impl ChangeOp for NoteCreateOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let title = op["title"].as_str().unwrap_or("").trim();
        let body = op["body"].as_str().unwrap_or("");
        Check::new()
            .len("title", title, 1, 120)
            .len("body", body, 0, 200_000)
            .done()?;
        if let Some(v) = scope_arg::<Visibility>(op, "visibility")?
            && !repo::scope_exists(c, cx.ws, &v)?
        {
            return Err(ApiError::not_found("团队"));
        }
        scope_arg::<Level>(op, "level")?;
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = notes::create(
            tx,
            self.0.guard.as_ref(),
            cx.ws,
            cx.actor,
            &NewNote {
                title: op["title"].as_str().unwrap_or("").trim().to_string(),
                body: op["body"].as_str().unwrap_or("").to_string(),
                visibility: scope_arg(op, "visibility")?,
                level: scope_arg(op, "level")?,
                about: vec![],
            },
            now(),
        )?;
        cx.bind("note", json!(id));
        self.0.notify(cx.ws, &id);
        Ok(json!({ "id": id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        if let Some(id) = before["id"].as_str()
            && repo::doc(tx, id)?.is_some()
        {
            index::delete_doc(tx, id)?;
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "kb_note.delete",
                id,
                &json!({ "undo": cx.changeset_id }),
            )?;
            self.0.notify(cx.ws, id);
        }
        Ok(())
    }
}

/// `kb.doc.patch {doc, set:{visibility?: code|null, level?: code|null}}` (`null` = follow the source).
struct DocPatchOp(Knowledge);

/// Sets a document's own scope and rewrites its index rows; returns the scope it had.
fn set_doc_scope(tx: &Tx, cx: &OpCx, doc_id: &str, set: &Value) -> ApiResult<Value> {
    let viewer = Viewer::load(tx, cx.ws, cx.actor)?;
    let (d, s) = own_doc(tx, &viewer, doc_id, "文档")?;
    let vis: Option<Visibility> = match set.get("visibility") {
        None => d.visibility.clone(),
        Some(v) => scope_arg(&json!({ "v": v }), "v")?,
    };
    let level: Option<Level> = match set.get("level") {
        None => d.level,
        Some(v) => scope_arg(&json!({ "v": v }), "v")?,
    };
    if let Some(v) = &vis
        && !repo::scope_exists(tx, cx.ws, v)?
    {
        return Err(ApiError::not_found("团队"));
    }
    let before = json!({ "visibility": d.visibility, "level": d.level });
    if vis != d.visibility || level != d.level {
        repo::set_doc_scope(tx, &d.id, vis.as_ref(), level, now())?;
        let eff = vis.clone().unwrap_or_else(|| s.visibility.clone());
        index::rescope(tx, "chunk", &d.id, &eff, level.unwrap_or(s.level))?;
    }
    base::audit(
        tx,
        cx.ws,
        cx.actor,
        "kb_doc.patch",
        &d.id,
        &json!({ "visibility": vis, "level": level, "changeset": cx.changeset_id }),
    )?;
    Ok(before)
}

impl ChangeOp for DocPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let viewer = Viewer::load(c, cx.ws, cx.actor)?;
        own_doc(c, &viewer, op["doc"].as_str().unwrap_or(""), "文档").map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = op["doc"].as_str().unwrap_or("");
        let before = set_doc_scope(tx, cx, id, &op["set"])?;
        self.0.notify(cx.ws, id);
        Ok(before)
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let id = op["doc"].as_str().unwrap_or("");
        if repo::doc(tx, id)?.is_some() {
            set_doc_scope(tx, cx, id, before)?;
            self.0.notify(cx.ws, id);
        }
        Ok(())
    }
}
