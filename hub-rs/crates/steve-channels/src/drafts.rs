//! Conversational configuration of channels (channels-personas.md §7, contacts.md §0.8): the draft
//! targets `space`, `persona` and `channel` (the channel part of a connection; `connection` itself
//! is the accounts module's target), their `global` candidates, and one operation per target:
//! `chan.space.patch` (= `PATCH /spaces/:id`), `chan.persona.patch` (= `PATCH /personas/:id`) and
//! `chan.channel.patch` (the channel fields of `PATCH /channels/:connection_id`). Operations carry
//! the patch exactly as those routes take it, so apply validates the same way; undo puts back the
//! values the patched keys had.
//!
//! Planning resolves names only by exact name (personas, Bots, tags, spaces) or name/alias
//! (contacts and teams in `member` rules); anything else is a “（没认出：X）” line and no change.
//! A request the domain refuses (a persona on your own account, `reply` on your own account, rules
//! of a space that became a flow) is one “（不能改：<原因>）” line and no operation.
//!
//! Integration (integrator):
//! ```ignore
//! steve_channels::drafts::register(&mut registry, clock.clone());
//! // pickTarget: candidates come with `changes::candidates`; for a request naming a group,
//! // `drafts::space_hints(c, &viewer, request)` orders the spaces by the prototype's `bestSpace`.
//! ```

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use steve_modules::changes::{Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, Plan, Prepared};
use steve_modules::{ApiError, ApiResult, Clock, Registry};
use steve_store::base::{self, Change, Viewer, show};
use steve_store::{ChangeEvent, Tx};

use crate::bindings::{self, SELF_NO_PERSONA};
use crate::personas::{self, PersonaPatch, PersonaRow, Reveal};
use crate::rules::{self, Rule, When};
use crate::spaces::{self, Mode, SpacePatch, SpaceRow};

pub const KIND_SPACE: &str = "space";
pub const KIND_PERSONA: &str = "persona";
pub const KIND_CHANNEL: &str = "channel";
pub const OP_SPACE: &str = "chan.space.patch";
pub const OP_PERSONA: &str = "chan.persona.patch";
pub const OP_CHANNEL: &str = "chan.channel.patch";

/// A line for something the model said that fits no field.
pub const UNRECOGNIZED: &str = "（没认出）";
/// What a space without its own persona / rules shows.
pub const FOLLOW: &str = "跟随账号";
/// Your own account speaks as you.
pub const YOU: &str = "你本人";
pub const DISCLOSURE_LABEL: &str = "本群内容在别处能说到什么程度";
/// Trigger words belong to your own account only (§2.7).
pub const TRIGGER_SELF_ONLY: &str = "触发语只用于以你本人接入的账号";
pub const TRIGGER_WORDS_MAX: usize = 10;
pub const DUTY_MAX: usize = 4000;

const WORKING: &str = "工作中";
const MUTED: &str = "静默";
const ON: &str = "开";
const OFF: &str = "关";
const REVEAL: &str = "透露";
const HIDE: &str = "隐藏";
const ALL_DMS: &str = "所有私聊";
const EVERYWHERE: &str = "所有群和私聊";
const LEVELS: [(&str, &str); 4] = [
    ("public", "公开"),
    ("conclusion", "可转述结论"),
    ("reasoning", "仅用于推理"),
    ("never", "不可使用"),
];
const SCOPES: [(&str, &str); 2] = [("space", "仅本群"), ("persona", "与身份共享")];
const REVEALS: [(&str, &str); 5] = [
    ("name", "名字"),
    ("role", "职责"),
    ("ai", "是 AI 助手"),
    ("steve", "由 Steve 驱动"),
    ("owner", "归谁管理"),
];

/// How the prompt explains the rule JSON (§2.4), so the model can write a whole new list.
const RULES_FORMAT: &str = "JSON 列表，整体替换。每条 {\"when\":{\"kind\":\"mention_or_dm|mention|dm|reply_to_me|keyword|intent|member|external|hours|all|other_group\", 参数: keyword 的 words、intent 的 text、member 的 parties/teams（名字）、hours 的 outside/start/end/days},\"act\":\"reply|draft|record|create_item|handoff\", create_item 可带 \"mode\":\"start|propose\"}；按顺序第一条命中的生效";

/// Registers the three targets and their operations.
pub fn register(r: &mut Registry, clock: Arc<dyn Clock>) {
    r.target(KIND_SPACE, Box::new(SpaceTarget))
        .target(KIND_PERSONA, Box::new(PersonaTarget))
        .target(KIND_CHANNEL, Box::new(ChannelTarget))
        .op(OP_SPACE, Box::new(SpaceOp(clock.clone())))
        .op(OP_PERSONA, Box::new(PersonaOp(clock.clone())))
        .op(OP_CHANNEL, Box::new(ChannelOp(clock)));
}

// ---------------------------------------------------------------- helpers

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A list value: an array of strings, or one string split at “、，, ”.
fn list_of(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(a) => a.iter().map(text_of).collect(),
        Value::String(s) => Some(
            s.split(['、', '，', ',', ' '])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect(),
        ),
        Value::Null => Some(Vec::new()),
        _ => None,
    }
}

/// A yes/no value: JSON booleans, the codes, or the given Chinese words.
fn flag(v: &Value, yes: &[&str], no: &[&str]) -> Option<bool> {
    if let Value::Bool(b) = v {
        return Some(*b);
    }
    let s = text_of(v)?;
    if s == "true" || yes.contains(&s.as_str()) {
        Some(true)
    } else if s == "false" || no.contains(&s.as_str()) {
        Some(false)
    } else {
        None
    }
}

fn on_off(b: bool) -> &'static str {
    if b { ON } else { OFF }
}

fn on_off_flag(v: &Value) -> Option<bool> {
    flag(
        v,
        &[ON, "开启", "打开", "启用", "是"],
        &[OFF, "关闭", "停用", "否"],
    )
}

/// The code of an enum value given as code or Chinese label.
fn code_of(table: &[(&'static str, &'static str)], s: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(c, l)| *c == s || *l == s)
        .map(|(c, _)| *c)
}

fn label_of(table: &[(&'static str, &'static str)], code: &str) -> String {
    table
        .iter()
        .find(|(c, _)| *c == code)
        .map_or_else(|| code.to_string(), |(_, l)| (*l).to_string())
}

fn labels(table: &[(&'static str, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, l)| *l).collect()
}

/// changes.ts `appended`: the model sends the full new text; if it does not start with the old one,
/// it is what to add.
fn appended(old: &str, new: &str) -> String {
    let new = new.trim();
    if old.is_empty() || new.starts_with(old) {
        new.to_string()
    } else {
        format!("{old}\n{new}")
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn not_recognized(label: &str, said: &str) -> Change {
    Change::new(label, "—", format!("（没认出：{}）", show(said)))
}

fn refused(label: &str, from: impl Into<String>, why: &str) -> Change {
    Change::new(label, from, format!("（不能改：{why}）"))
}

/// A domain error as a refusal reason; storage errors stay errors.
fn reason(e: ApiError) -> ApiResult<String> {
    match e {
        ApiError::Internal(_) | ApiError::Unauthenticated => Err(e),
        e => Ok(e.to_string()),
    }
}

/// Preview lines and the patch being built for one target. A key set twice keeps the last value
/// and the last line.
#[derive(Default)]
struct Lines {
    changes: Vec<Change>,
    patch: Map<String, Value>,
}

impl Lines {
    fn line(&mut self, c: Change) {
        self.changes.retain(|x| x.label != c.label);
        self.changes.push(c);
    }
    fn set(&mut self, key: &str, v: Value, c: Change) {
        self.patch.insert(key.into(), v);
        self.line(c);
    }
    fn other(&mut self, op: &Value) {
        self.changes
            .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
    }
}

/// `(key, value)` of a `set` op; `None` for anything else.
fn set_of(op: &Value) -> Option<(&str, &Value)> {
    (op["op"] == "set").then(|| (op["key"].as_str().unwrap_or(""), &op["value"]))
}

fn viewer(c: &Connection, cx: &OpCx) -> ApiResult<Viewer> {
    Ok(Viewer::load(c, cx.ws, cx.actor)?)
}

/// A Bot of the workspace by exact name or id (never a guess).
fn find_bot(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<String>> {
    Ok(c.query_row(
        "SELECT b.id FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ? WHERE b.deleted = 0 AND (b.id = ?2 OR b.name = ?2) ORDER BY b.id = ?2 DESC LIMIT 1",
        params![ws, said.trim()],
        |r| r.get(0),
    )
    .optional()?)
}

fn bot_name(c: &Connection, id: &str) -> String {
    c.query_row("SELECT name FROM bots WHERE id = ?", [id], |r| r.get(0))
        .unwrap_or_else(|_| id.to_string())
}

/// A live persona of the workspace by exact name or id.
fn find_persona(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<PersonaRow>> {
    let said = said.trim();
    Ok(personas::list(c, ws)?
        .into_iter()
        .find(|p| p.id == said || p.name == said))
}

/// Tag ids by exact name or id; `Err(name)` for the first one not found.
fn find_tags(c: &Connection, ws: &str, said: &[String]) -> ApiResult<Result<Vec<String>, String>> {
    let mut out = Vec::new();
    for s in said {
        let id: Option<String> = c
            .query_row(
                "SELECT id FROM tags WHERE workspace_id = ? AND (id = ?2 OR name = ?2) ORDER BY id = ?2 DESC LIMIT 1",
                params![ws, s],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) if !out.contains(&id) => out.push(id),
            Some(_) => {}
            None => return Ok(Err(s.clone())),
        }
    }
    Ok(Ok(out))
}

fn tag_names(c: &Connection, ids: &[String]) -> String {
    let names: Vec<String> = ids
        .iter()
        .map(|id| {
            c.query_row("SELECT name FROM tags WHERE id = ?", [id], |r| r.get(0))
                .unwrap_or_else(|_| id.clone())
        })
        .collect();
    if names.is_empty() {
        "无".into()
    } else {
        names.join("、")
    }
}

// ---------------------------------------------------------------- rules in words

fn hhmm(m: u32) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn party_name(c: &Connection, id: &str) -> String {
    c.query_row("SELECT name FROM parties WHERE id = ?", [id], |r| r.get(0))
        .unwrap_or_else(|_| id.to_string())
}

fn when_text(c: &Connection, w: &When, mode: Mode) -> String {
    let own = mode == Mode::Own;
    match w {
        When::MentionOrDm if own => "@你 或私聊你".into(),
        When::MentionOrDm => "被 @ 或私聊".into(),
        When::Mention if own => "@你".into(),
        When::Mention => "被 @".into(),
        When::Dm if own => "私聊你".into(),
        When::Dm => "私聊".into(),
        When::ReplyToMe => "回复了它的消息".into(),
        When::Keyword { words } => format!(
            "消息包含{}",
            words.iter().map(|w| format!("「{w}」")).collect::<String>()
        ),
        When::Intent { text } => format!("AI 判断是「{text}」"),
        When::Member { parties, teams } => {
            let names: Vec<String> = parties
                .iter()
                .chain(teams)
                .map(|id| party_name(c, id))
                .collect();
            format!("{} 发言", names.join("、"))
        }
        When::External => "外部成员发言".into(),
        When::Hours {
            outside,
            start,
            end,
            ..
        } => format!(
            "{}（{}–{}）",
            if *outside {
                "工作时间外"
            } else {
                "工作时间内"
            },
            hhmm(*start),
            hhmm(*end)
        ),
        When::All => "全部消息".into(),
        When::OtherGroup => "其他群消息".into(),
    }
}

/// A rule list in words, the way the change lines show it: “被 @ 或私聊时直接回复；其他只记录”.
pub fn describe_rules(c: &Connection, rules: &[Rule], mode: Mode) -> String {
    if rules.is_empty() {
        return "没有规则（全部只记录）".into();
    }
    rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let act = r.act.label();
            match &r.when {
                When::All if i > 0 => format!("其他{act}"),
                When::All | When::OtherGroup => format!("{}{act}", when_text(c, &r.when, mode)),
                w => format!("{}时{act}", when_text(c, w, mode)),
            }
        })
        .collect::<Vec<_>>()
        .join("；")
}

/// A contact or team by id, or by exact name or alias (case-insensitive); `None` when there is
/// none or more than one.
fn find_party(c: &Connection, ws: &str, ty: &str, said: &str) -> ApiResult<Option<String>> {
    let mut st = c.prepare_cached(
        "SELECT id, name, aliases FROM parties WHERE workspace_id = ? AND type = ? AND merged_into IS NULL",
    )?;
    let rows = st
        .query_map(params![ws, ty], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some((id, _, _)) = rows.iter().find(|(id, _, _)| id == said) {
        return Ok(Some(id.clone()));
    }
    let want = said.trim().to_lowercase();
    let hits: Vec<&String> = rows
        .iter()
        .filter(|(_, name, aliases)| {
            name.to_lowercase() == want
                || serde_json::from_str::<Vec<String>>(aliases)
                    .unwrap_or_default()
                    .iter()
                    .any(|a| a.to_lowercase() == want)
        })
        .map(|(id, _, _)| id)
        .collect();
    Ok((hits.len() == 1).then(|| hits[0].clone()))
}

enum Checked {
    /// The normalized rules to store, and in words.
    Ok(Value, String),
    /// A name (or the whole value) that was not recognized.
    Unknown(String),
    /// The domain refused it (the same checks as `PATCH /spaces`).
    Refused(String),
}

/// The model's rules → names in `member` resolved → validated like `PATCH /spaces/:id`
/// (`rules::parse` for the account's mode, then `rules::check_refs`).
fn check_rules(c: &Connection, ws: &str, mode: Mode, raw: &Value) -> ApiResult<Checked> {
    let mut v = match raw {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => return Ok(Checked::Unknown(s.clone())),
        },
        v => v.clone(),
    };
    let Some(items) = v.as_array_mut() else {
        return Ok(Checked::Unknown(raw.to_string()));
    };
    for r in items.iter_mut() {
        if r["when"]["kind"] != "member" {
            continue;
        }
        for (key, ty) in [("parties", "contact"), ("teams", "team")] {
            let Some(xs) = r["when"][key].as_array().cloned() else {
                continue;
            };
            let mut ids = Vec::new();
            for x in xs {
                let said = text_of(&x).unwrap_or_default();
                match find_party(c, ws, ty, &said)? {
                    Some(id) => ids.push(Value::String(id)),
                    None => return Ok(Checked::Unknown(said)),
                }
            }
            r["when"][key] = Value::Array(ids);
        }
    }
    let parsed = match rules::parse(&v, mode) {
        Ok(p) => p,
        Err(e) => return Ok(Checked::Refused(reason(e)?)),
    };
    if let Err(e) = rules::check_refs(c, ws, &parsed) {
        return Ok(Checked::Refused(reason(e)?));
    }
    Ok(Checked::Ok(
        rules::to_value(&parsed),
        describe_rules(c, &parsed, mode),
    ))
}

/// Stored rules in words (leniently loaded, like the pipeline reads them).
fn rules_text(c: &Connection, v: &Value, mode: Mode) -> String {
    describe_rules(c, &rules::load(v, mode), mode)
}

// ---------------------------------------------------------------- space

/// What an account is called (display name, else its handle).
fn account_name(c: &Connection, ws: &str, conn: &str) -> ApiResult<String> {
    Ok(c
        .query_row(
            "SELECT coalesce(nullif(a.display_name, ''), a.handle_display) FROM connections c JOIN accounts a ON a.id = c.account_id
              WHERE c.id = ? AND c.workspace_id = ?",
            params![conn, ws],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| conn.to_string()))
}

fn space_name(s: &SpaceRow) -> String {
    if s.name.is_empty() {
        "私聊".into()
    } else {
        s.name.clone()
    }
}

/// `item_defaults` with every key present.
fn item_defaults(v: &Value) -> Value {
    json!({
        "tags": v.get("tags").cloned().filter(Value::is_array).unwrap_or_else(|| json!([])),
        "project_id": v.get("project_id").cloned().unwrap_or(Value::Null),
        "bot_id": v.get("bot_id").cloned().unwrap_or(Value::Null),
    })
}

/// A space as the draft sees it.
struct SpaceNow {
    row: SpaceRow,
    set: spaces::SpaceSettings,
    mode: Mode,
    account: String,
    /// The persona bound to the space itself.
    binding: Option<String>,
    persona: Option<bindings::PersonaRef>,
    account_rules: Value,
}

impl SpaceNow {
    fn load(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Self> {
        let row = spaces::own(c, viewer, id)?;
        spaces::writable(&row)?;
        let set = spaces::settings(c, id)?;
        let mode = bindings::connection_account(c, &viewer.ws, &row.connection_id)?.1;
        let account_rules = spaces::channel_settings(c, &viewer.ws, &row.connection_id)?
            .map(|x| x.rules)
            .unwrap_or_else(|| match mode {
                Mode::Bot => spaces::default_bot_rules(),
                Mode::Own => spaces::default_self_rules(),
            });
        Ok(Self {
            account: account_name(c, &viewer.ws, &row.connection_id)?,
            binding: bindings::space_binding(c, &row)?,
            persona: bindings::space_persona(c, &row)?,
            row,
            set,
            mode,
            account_rules,
        })
    }

    fn persona_text(&self) -> String {
        match (&self.persona, self.mode) {
            (_, Mode::Own) => YOU.into(),
            (Some(p), _) if p.inherited == Some(true) => format!("{FOLLOW}（{}）", p.name),
            (Some(p), _) => p.name.clone(),
            (None, _) => FOLLOW.into(),
        }
    }

    fn rules_text(&self, c: &Connection) -> String {
        match &self.set.rules {
            Some(v) => rules_text(c, v, self.mode),
            None => format!(
                "{FOLLOW}（{}）",
                rules_text(c, &self.account_rules, self.mode)
            ),
        }
    }
}

fn space_revision(c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
    Ok(c.query_row(
        "SELECT ss.revision FROM space_settings ss JOIN spaces s ON s.id = ss.space_id WHERE s.id = ? AND s.workspace_id = ?",
        params![id, ws],
        |r| r.get(0),
    )
    .optional()?)
}

const L_PERSONA: &str = "对外身份";
const L_MUTED: &str = "工作状态";
const L_RULES: &str = "响应规则";
const L_MEMORY: &str = "记忆";
const L_COLLAB: &str = "和其他机器人协作";
const L_TAGS: &str = "事项默认标签";
const L_DUTY: &str = "职责（agents）";

/// `target.kind = "space"`.
struct SpaceTarget;

impl DraftTarget for SpaceTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
        let s = SpaceNow::load(c, viewer, id)?;
        let name = space_name(&s.row);
        let mut fields = Vec::new();
        if s.mode == Mode::Bot {
            fields.push(FieldSpec::new(
                "persona",
                "在这里说话的对外身份（身份的名字；跟随账号写“跟随账号”）",
                "ref",
                json!(s.persona_text()),
            ));
        }
        fields.extend([
            FieldSpec::new(
                "muted",
                L_MUTED,
                "enum",
                json!(if s.set.muted { MUTED } else { WORKING }),
            )
            .values(&[WORKING, MUTED]),
            FieldSpec::new(
                "rules",
                &format!("{L_RULES}（{RULES_FORMAT}；跟随账号写 null）"),
                "list",
                json!({ "本群": s.set.rules, "生效": s.set.rules.clone().unwrap_or_else(|| s.account_rules.clone()), "说明": s.rules_text(c) }),
            ),
            FieldSpec::new(
                "disclosure",
                DISCLOSURE_LABEL,
                "enum",
                json!(label_of(&LEVELS, &s.set.disclosure)),
            )
            .values(&labels(&LEVELS)),
            FieldSpec::new(
                "memory_scope",
                L_MEMORY,
                "enum",
                json!(label_of(&SCOPES, &s.set.memory_scope)),
            )
            .values(&labels(&SCOPES)),
            FieldSpec::new("collab", L_COLLAB, "enum", json!(on_off(s.set.collab)))
                .values(&[ON, OFF]),
            FieldSpec::new(
                "item_defaults.tags",
                "从这里建的事项默认带的标签（标签名）",
                "list",
                json!(tag_names(c, &tags_of(&s.set.item_defaults))),
            ),
            FieldSpec::new("duty", "这个群的职责（自由文本）", "text", json!(s.set.duty)),
        ]);
        Ok(Prepared {
            target_id: Some(id.to_string()),
            revision: Some(s.set.revision),
            title: format!("{name} 的设置"),
            href: format!("#/channels/space/{id}"),
            subject: format!("the chat space \"{name}\" of the account \"{}\"", s.account),
            fields,
            ops: vec![],
            free_text: Some("duty".into()),
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
        let id = p.target_id.as_deref().unwrap_or("");
        let s = SpaceNow::load(c, viewer, id)?;
        let ws = &viewer.ws;
        let mut d = Lines::default();
        for op in raw {
            let Some((key, v)) = set_of(&op) else {
                d.other(&op);
                continue;
            };
            let said = text_of(v).unwrap_or_default();
            match key {
                "persona" if s.mode == Mode::Own => {
                    d.line(refused(L_PERSONA, YOU, SELF_NO_PERSONA));
                }
                "persona" if v.is_null() || said.is_empty() || said == FOLLOW => {
                    if s.binding.is_some() {
                        d.set(
                            "persona_id",
                            Value::Null,
                            Change::new(L_PERSONA, s.persona_text(), FOLLOW),
                        );
                    }
                }
                "persona" => match find_persona(c, ws, &said)? {
                    Some(per) if s.binding.as_deref() != Some(per.id.as_str()) => d.set(
                        "persona_id",
                        json!(per.id),
                        Change::new(L_PERSONA, s.persona_text(), per.name),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_PERSONA, &said)),
                },
                "muted" => match flag(v, &[MUTED, "是", "mute"], &[WORKING, "否"]) {
                    Some(m) if m != s.set.muted => d.set(
                        "muted",
                        json!(m),
                        Change::new(
                            L_MUTED,
                            if s.set.muted { MUTED } else { WORKING },
                            if m { MUTED } else { WORKING },
                        ),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_MUTED, &said)),
                },
                "rules" => {
                    if let Some(f) = &s.set.flow_id {
                        let why = format!(
                            "这个空间已经转成流程「{}」，先改回规则",
                            steve_flow::flows::name_of(c, f)?
                        );
                        d.line(refused(L_RULES, s.rules_text(c), &why));
                        continue;
                    }
                    if v.is_null() || said == FOLLOW {
                        if s.set.rules.is_some() {
                            let to =
                                format!("{FOLLOW}（{}）", rules_text(c, &s.account_rules, s.mode));
                            d.set(
                                "rules",
                                Value::Null,
                                Change::new(L_RULES, s.rules_text(c), to),
                            );
                        }
                        continue;
                    }
                    match check_rules(c, ws, s.mode, v)? {
                        Checked::Ok(rules, to) => {
                            if s.set.rules.as_ref() != Some(&rules) {
                                d.set("rules", rules, Change::new(L_RULES, s.rules_text(c), to));
                            }
                        }
                        Checked::Unknown(x) => d.line(not_recognized(L_RULES, &x)),
                        Checked::Refused(why) => d.line(refused(L_RULES, s.rules_text(c), &why)),
                    }
                }
                "disclosure" => match code_of(&LEVELS, &said) {
                    Some(l) if l != s.set.disclosure => d.set(
                        "disclosure",
                        json!(l),
                        Change::new(
                            DISCLOSURE_LABEL,
                            label_of(&LEVELS, &s.set.disclosure),
                            label_of(&LEVELS, l),
                        ),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(DISCLOSURE_LABEL, &said)),
                },
                "memory_scope" => match code_of(&SCOPES, &said) {
                    Some(m) if m != s.set.memory_scope => d.set(
                        "memory_scope",
                        json!(m),
                        Change::new(
                            L_MEMORY,
                            label_of(&SCOPES, &s.set.memory_scope),
                            label_of(&SCOPES, m),
                        ),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_MEMORY, &said)),
                },
                "collab" => match on_off_flag(v) {
                    Some(b) if b != s.set.collab => d.set(
                        "collab",
                        json!(b),
                        Change::new(L_COLLAB, on_off(s.set.collab), on_off(b)),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_COLLAB, &said)),
                },
                "item_defaults.tags" => {
                    let Some(names) = list_of(v) else {
                        d.line(not_recognized(L_TAGS, &v.to_string()));
                        continue;
                    };
                    let old = tags_of(&s.set.item_defaults);
                    match find_tags(c, ws, &names)? {
                        Ok(ids) if ids.len() > 20 => d.line(refused(
                            L_TAGS,
                            tag_names(c, &old),
                            "参数不对：item_defaults.tags",
                        )),
                        Ok(ids) if ids != old => {
                            let mut defaults = item_defaults(&s.set.item_defaults);
                            defaults["tags"] = json!(ids);
                            d.set(
                                "item_defaults",
                                defaults,
                                Change::new(L_TAGS, tag_names(c, &old), tag_names(c, &ids)),
                            );
                        }
                        Ok(_) => {}
                        Err(name) => d.line(not_recognized(L_TAGS, &name)),
                    }
                }
                "duty" => {
                    let new = appended(&s.set.duty, &said);
                    if said.is_empty() || new == s.set.duty {
                        continue;
                    }
                    if utf16_len(&new) > DUTY_MAX {
                        d.line(refused(L_DUTY, show(&s.set.duty), "参数不对：duty"));
                        continue;
                    }
                    let added = new.strip_prefix(s.set.duty.as_str()).unwrap_or(&new).trim();
                    let line = Change::new(
                        L_DUTY,
                        show(&s.set.duty),
                        format!("{}（追加）", show(added)),
                    );
                    d.set("duty", json!(new), line);
                }
                _ => d.other(&op),
            }
        }
        let mut plan = Plan {
            ops: vec![],
            changes: d.changes,
        };
        if !d.patch.is_empty() {
            plan.ops
                .push(json!({ "op": OP_SPACE, "space_id": id, "patch": d.patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        space_revision(c, ws, id)
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let mut out = Vec::new();
        for s in spaces::list(c, viewer, None, None)? {
            if s.status != "active" {
                continue;
            }
            out.push(Candidate {
                code: format!("space:{}", s.id),
                label: format!(
                    "{}（{}）",
                    space_name(&s),
                    account_name(c, &viewer.ws, &s.connection_id)?
                ),
            });
        }
        Ok(out)
    }
}

fn tags_of(defaults: &Value) -> Vec<String> {
    defaults["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The prototype's `bestSpace` as an ordering hint for `pickTarget`: spaces whose name (without a
/// trailing “群”) shares a piece of at least 2 characters with the request, longest piece first.
/// The model still picks; `prepare` checks the pick.
pub fn space_hints(c: &Connection, viewer: &Viewer, request: &str) -> ApiResult<Vec<Candidate>> {
    let score = |name: &str| -> usize {
        let core: Vec<char> = name.strip_suffix('群').unwrap_or(name).chars().collect();
        let mut best = 0;
        for i in 0..core.len() {
            for j in (i + 2)..=core.len() {
                let piece: String = core[i..j].iter().collect();
                if request.contains(&piece) {
                    best = best.max(j - i);
                }
            }
        }
        best
    };
    let mut scored: Vec<(usize, Candidate)> = SpaceTarget
        .candidates(c, viewer)?
        .into_iter()
        .filter_map(|cand| {
            let id = cand.code.strip_prefix("space:")?;
            let name = spaces::get(c, id).ok()??.name;
            let n = score(&name);
            (n >= 2).then_some((n, cand))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    Ok(scored.into_iter().map(|(_, c)| c).collect())
}

fn parse_space_patch(v: &Value) -> ApiResult<SpacePatch> {
    serde_json::from_value(v.clone()).map_err(|_| ApiError::invalid("patch"))
}

/// `chan.space.patch {space_id, patch}`: `PATCH /spaces/:id` in the changeset's transaction.
struct SpaceOp(Arc<dyn Clock>);

impl ChangeOp for SpaceOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        parse_space_patch(&op["patch"])?.check()?;
        let s = spaces::own(c, &viewer(c, cx)?, op["space_id"].as_str().unwrap_or(""))?;
        spaces::writable(&s)?;
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = op["space_id"].as_str().unwrap_or("");
        let v = viewer(tx, cx)?;
        let s = spaces::own(tx, &v, id)?;
        spaces::writable(&s)?;
        let set = spaces::settings(tx, id)?;
        let mut before = Map::new();
        for key in op["patch"].as_object().into_iter().flat_map(|m| m.keys()) {
            let old = match key.as_str() {
                "persona_id" => json!(bindings::space_binding(tx, &s)?),
                "duty" => json!(set.duty),
                "rules" => json!(set.rules),
                "muted" => json!(set.muted),
                "collab" => json!(set.collab),
                "memory_scope" => json!(set.memory_scope),
                "disclosure" => json!(set.disclosure),
                "item_defaults" => item_defaults(&set.item_defaults),
                _ => continue,
            };
            before.insert(key.clone(), old);
        }
        spaces::patch(tx, &v, id, &parse_space_patch(&op["patch"])?, self.0.now())?;
        Ok(json!({ "fields": before, "configured": set.configured }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let id = op["space_id"].as_str().unwrap_or("");
        let v = viewer(tx, cx)?;
        let now = self.0.now();
        spaces::patch(tx, &v, id, &parse_space_patch(&before["fields"])?, now)?;
        if before["configured"] == false {
            tx.execute(
                "UPDATE space_settings SET configured = 0, updated = ? WHERE space_id = ?",
                params![now, id],
            )?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- persona

const L_NAME: &str = "名字";
const L_BOT: &str = "背后的 Bot";
const L_INTRO: &str = "自我介绍";
const L_SOUL: &str = "口吻（soul）";
const L_SIGNATURE: &str = "签名";

fn reveal_of(r: &Reveal, key: &str) -> bool {
    match key {
        "name" => r.name,
        "role" => r.role,
        "ai" => r.ai,
        "steve" => r.steve,
        _ => r.owner,
    }
}

fn reveal_text(b: bool) -> &'static str {
    if b { REVEAL } else { HIDE }
}

fn or_none(s: &str) -> String {
    if s.is_empty() { "无".into() } else { show(s) }
}

/// `target.kind = "persona"`.
struct PersonaTarget;

impl DraftTarget for PersonaTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
        let p = personas::own(c, &viewer.ws, id)?;
        let mut fields = vec![
            FieldSpec::new("name", "名字（1–20 字）", "text", json!(p.name)),
            FieldSpec::new(
                "bot",
                "背后的 Bot（Bot 的名字）",
                "ref",
                json!(bot_name(c, &p.bot_id)),
            ),
            FieldSpec::new("intro", "自我介绍（≤400 字）", "text", json!(p.intro)),
            FieldSpec::new(
                "soul",
                "口吻、性格、说话方式（自由文本）",
                "text",
                json!(p.soul),
            ),
        ];
        for (k, l) in REVEALS {
            fields.push(
                FieldSpec::new(
                    &format!("reveal.{k}"),
                    &format!("被问到时是否透露：{l}"),
                    "enum",
                    json!(reveal_text(reveal_of(&p.reveal, k))),
                )
                .values(&[REVEAL, HIDE]),
            );
        }
        fields.push(FieldSpec::new(
            "signature",
            "签名（追加在每条消息末尾，≤60 字）",
            "text",
            json!(p.signature),
        ));
        Ok(Prepared {
            target_id: Some(p.id.clone()),
            revision: Some(p.revision),
            title: format!("对外身份「{}」", p.name),
            href: format!("#/channels/personas/{}", p.id),
            subject: format!(
                "the outward persona \"{}\" (how Steve presents itself in chats)",
                p.name
            ),
            fields,
            ops: vec![],
            free_text: Some("soul".into()),
            context: Value::Null,
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        prep: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let p = personas::own(c, &viewer.ws, prep.target_id.as_deref().unwrap_or(""))?;
        let mut d = Lines::default();
        let mut reveal = Map::new();
        for op in raw {
            let Some((key, v)) = set_of(&op) else {
                d.other(&op);
                continue;
            };
            let said = text_of(v).unwrap_or_default();
            if let Some((k, l)) = key
                .strip_prefix("reveal.")
                .and_then(|k| REVEALS.iter().find(|(x, _)| *x == k))
            {
                let label = format!("透露：{l}");
                match flag(v, &[REVEAL, "是", "说"], &[HIDE, "否", "不说", "不透露"]) {
                    Some(b) if b != reveal_of(&p.reveal, k) => {
                        reveal.insert((*k).into(), json!(b));
                        d.line(Change::new(
                            label,
                            reveal_text(reveal_of(&p.reveal, k)),
                            reveal_text(b),
                        ));
                    }
                    Some(_) => {
                        reveal.remove(*k);
                        d.changes.retain(|x| x.label != label);
                    }
                    None => d.line(not_recognized(&label, &said)),
                }
                continue;
            }
            match key {
                "name" => {
                    if !(1..=20).contains(&utf16_len(&said)) {
                        d.line(not_recognized(L_NAME, &said));
                    } else if said != p.name {
                        d.set(
                            "name",
                            json!(said),
                            Change::new(L_NAME, &p.name, said.clone()),
                        );
                    }
                }
                "bot" => match find_bot(c, &viewer.ws, &said)? {
                    Some(b) if b != p.bot_id => d.set(
                        "bot_id",
                        json!(b),
                        Change::new(L_BOT, bot_name(c, &p.bot_id), bot_name(c, &b)),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_BOT, &said)),
                },
                "intro" => {
                    if utf16_len(&said) > 400 {
                        d.line(refused(L_INTRO, show(&p.intro), "参数不对：intro"));
                    } else if said != p.intro {
                        d.set(
                            "intro",
                            json!(said),
                            Change::new(L_INTRO, show(&p.intro), show(&said)),
                        );
                    }
                }
                "soul" => {
                    let new = appended(&p.soul, &said);
                    if said.is_empty() || new == p.soul {
                        continue;
                    }
                    if utf16_len(&new) > 2000 {
                        d.line(refused(L_SOUL, show(&p.soul), "参数不对：soul"));
                        continue;
                    }
                    let added = new.strip_prefix(p.soul.as_str()).unwrap_or(&new).trim();
                    let line =
                        Change::new(L_SOUL, show(&p.soul), format!("{}（追加）", show(added)));
                    d.set("soul", json!(new), line);
                }
                "signature" => {
                    if utf16_len(&said) > 60 {
                        d.line(refused(
                            L_SIGNATURE,
                            or_none(&p.signature),
                            "参数不对：signature",
                        ));
                    } else if said != p.signature {
                        d.set(
                            "signature",
                            json!(said),
                            Change::new(L_SIGNATURE, or_none(&p.signature), or_none(&said)),
                        );
                    }
                }
                _ => d.other(&op),
            }
        }
        if !reveal.is_empty() {
            d.patch.insert("reveal".into(), Value::Object(reveal));
        }
        let mut plan = Plan {
            ops: vec![],
            changes: d.changes,
        };
        if !d.patch.is_empty() {
            plan.ops
                .push(json!({ "op": OP_PERSONA, "persona_id": p.id, "patch": d.patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(c.query_row(
            "SELECT revision FROM personas WHERE id = ? AND workspace_id = ? AND deleted = 0",
            params![id, ws],
            |r| r.get(0),
        )
        .optional()?)
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        Ok(personas::list(c, &viewer.ws)?
            .into_iter()
            .map(|p| Candidate {
                code: format!("persona:{}", p.id),
                label: format!("对外身份 {}", p.name),
            })
            .collect())
    }
}

fn parse_persona_patch(v: &Value) -> ApiResult<PersonaPatch> {
    serde_json::from_value(v.clone()).map_err(|_| ApiError::invalid("patch"))
}

/// `chan.persona.patch {persona_id, patch}`: `PATCH /personas/:id` in the changeset's transaction.
struct PersonaOp(Arc<dyn Clock>);

impl ChangeOp for PersonaOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        parse_persona_patch(&op["patch"])?;
        personas::own(c, cx.ws, op["persona_id"].as_str().unwrap_or(""))?;
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = op["persona_id"].as_str().unwrap_or("");
        let p = personas::own(tx, cx.ws, id)?;
        let mut before = Map::new();
        for key in op["patch"].as_object().into_iter().flat_map(|m| m.keys()) {
            let old = match key.as_str() {
                "name" => json!(p.name),
                "bot_id" => json!(p.bot_id),
                "intro" => json!(p.intro),
                "soul" => json!(p.soul),
                "signature" => json!(p.signature),
                "reveal" => serde_json::to_value(p.reveal)?,
                _ => continue,
            };
            before.insert(key.clone(), old);
        }
        personas::patch(
            tx,
            &viewer(tx, cx)?,
            id,
            &parse_persona_patch(&op["patch"])?,
            self.0.now(),
        )?;
        Ok(Value::Object(before))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        personas::patch(
            tx,
            &viewer(tx, cx)?,
            op["persona_id"].as_str().unwrap_or(""),
            &parse_persona_patch(before)?,
            self.0.now(),
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------- channel (account)

const L_DEFAULT_PERSONA: &str = "默认对外身份";
const L_DEFAULT_RULES: &str = "默认响应规则";
const L_ACCOUNT_DUTY: &str = "账号默认职责";
const L_TRIGGER_ON: &str = "触发语";
const L_TRIGGER_WORDS: &str = "触发语词";
const L_TRIGGER_WHERE: &str = "触发语生效范围";
const L_TRIGGER_MARK: &str = "代发标记“（由 Steve 代发）”";

/// The connections of channels `viewer` may configure: in the workspace, receiving messages, not
/// someone else's personal account (`own` like `set_account_rules`).
const CHANNEL_SQL: &str = "SELECT c.id FROM connections c JOIN accounts a ON a.id = c.account_id LEFT JOIN parties p ON p.id = a.party_id
    WHERE c.workspace_id = ?1 AND c.status <> 'revoked' AND (p.user_id IS NULL OR p.user_id = ?2)
      AND (EXISTS (SELECT 1 FROM channel_settings s WHERE s.connection_id = c.id)
           OR EXISTS (SELECT 1 FROM json_each(c.caps) WHERE json_each.value = 'msg.receive'))";

/// A channel account as the draft sees it.
struct ChannelNow {
    conn: String,
    mode: Mode,
    name: String,
    duty: String,
    rules: Value,
    trigger: Value,
    revision: i64,
    /// The persona bound as the account default.
    binding: Option<String>,
    persona: Option<bindings::PersonaRef>,
}

fn default_trigger() -> Value {
    json!({"on": false, "words": ["/bot"], "where": [], "mark": true})
}

/// `trigger` with every key present.
fn full_trigger(v: &Value) -> Value {
    let t = rules::Trigger::from_value(v);
    json!({"on": t.on, "words": t.words, "where": t.r#where, "mark": t.mark})
}

fn account_binding(c: &Connection, ws: &str, account: &str) -> ApiResult<Option<String>> {
    Ok(c.query_row(
        "SELECT persona_id FROM persona_bindings WHERE workspace_id = ? AND account_id = ? AND space_id = ''",
        params![ws, account],
        |r| r.get(0),
    )
    .optional()?)
}

impl ChannelNow {
    fn load(c: &Connection, viewer: &Viewer, conn: &str) -> ApiResult<Self> {
        let ok: bool = c.query_row(
            &format!("SELECT EXISTS ({CHANNEL_SQL} AND c.id = ?3)"),
            params![viewer.ws, viewer.user, conn],
            |r| r.get(0),
        )?;
        if !ok {
            return Err(ApiError::not_found("连接"));
        }
        let (account, mode) = bindings::connection_account(c, &viewer.ws, conn)?;
        let set = spaces::channel_settings(c, &viewer.ws, conn)?;
        let (duty, rules, trigger, revision) = match set {
            Some(s) => (s.duty, s.rules, full_trigger(&s.trigger), s.revision),
            None => (
                String::new(),
                match mode {
                    Mode::Bot => spaces::default_bot_rules(),
                    Mode::Own => spaces::default_self_rules(),
                },
                default_trigger(),
                1,
            ),
        };
        Ok(Self {
            conn: conn.to_string(),
            mode,
            name: account_name(c, &viewer.ws, conn)?,
            duty,
            rules,
            trigger,
            revision,
            binding: account_binding(c, &viewer.ws, &account)?,
            persona: bindings::account_persona(c, &viewer.ws, conn)?,
        })
    }

    fn persona_text(&self) -> String {
        match (&self.persona, self.mode) {
            (_, Mode::Own) => YOU.into(),
            (Some(p), _) => p.name.clone(),
            (None, _) => "无".into(),
        }
    }

    /// The account's spaces `(id, name)`, for `trigger.where`.
    fn spaces(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<(String, String)>> {
        Ok(spaces::list(c, viewer, Some(&self.conn), None)?
            .into_iter()
            .map(|s| (s.id, s.name))
            .collect())
    }
}

fn where_names(places: &[(String, String)], ids: &[String]) -> String {
    let names: Vec<String> = ids
        .iter()
        .map(|id| {
            if id == rules::WHERE_ALL_DMS {
                ALL_DMS.to_string()
            } else if id == rules::WHERE_EVERYWHERE {
                EVERYWHERE.to_string()
            } else {
                places
                    .iter()
                    .find(|(x, _)| x == id)
                    .map_or_else(|| id.clone(), |(_, n)| n.clone())
            }
        })
        .collect();
    if names.is_empty() {
        "无".into()
    } else {
        names.join("、")
    }
}

/// `trigger.words` entries: `/^\/[A-Za-z0-9_]{1,20}$/`.
fn word_ok(w: &str) -> bool {
    let Some(rest) = w.strip_prefix('/') else {
        return false;
    };
    (1..=20).contains(&rest.len())
        && rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// `PATCH /channels/:connection_id` `trigger` (§5.2): `{on?, words?: str[≤10], where?: str[], mark?}`.
fn check_trigger(v: &Value) -> ApiResult<()> {
    let mut c = steve_modules::Check::new();
    c.that("trigger", v.is_object());
    for k in ["on", "mark"] {
        c.that(&format!("trigger.{k}"), v[k].is_null() || v[k].is_boolean());
    }
    match &v["words"] {
        Value::Null => {}
        Value::Array(a) => {
            c.that("trigger.words", a.len() <= TRIGGER_WORDS_MAX);
            for (i, w) in a.iter().enumerate() {
                c.that(
                    &format!("trigger.words.{i}"),
                    w.as_str().is_some_and(word_ok),
                );
            }
        }
        _ => {
            c.that("trigger.words", false);
        }
    }
    match &v["where"] {
        Value::Null => {}
        Value::Array(a) => {
            for (i, w) in a.iter().enumerate() {
                c.that(&format!("trigger.where.{i}"), w.is_string());
            }
        }
        _ => {
            c.that("trigger.where", false);
        }
    }
    c.done()
}

/// `target.kind = "channel"` (`channel:<connection_id>`).
struct ChannelTarget;

impl DraftTarget for ChannelTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
        let a = ChannelNow::load(c, viewer, id)?;
        let mut fields = Vec::new();
        if a.mode == Mode::Bot {
            fields.push(FieldSpec::new(
                "persona",
                "默认对外身份（身份的名字）",
                "ref",
                json!(a.persona_text()),
            ));
        }
        fields.push(FieldSpec::new(
            "rules",
            &format!("{L_DEFAULT_RULES}（{RULES_FORMAT}）"),
            "list",
            json!({ "规则": a.rules, "说明": rules_text(c, &a.rules, a.mode) }),
        ));
        fields.push(FieldSpec::new(
            "duty",
            "账号默认职责（自由文本）",
            "text",
            json!(a.duty),
        ));
        if a.mode == Mode::Own {
            let t = rules::Trigger::from_value(&a.trigger);
            let places = a.spaces(c, viewer)?;
            fields.extend([
                FieldSpec::new(
                    "trigger.on",
                    "触发语（你本人发以触发语开头的消息时，Steve 以你的名义代发）",
                    "enum",
                    json!(on_off(t.on)),
                )
                .values(&[ON, OFF]),
                FieldSpec::new(
                    "trigger.words",
                    "触发语词（以 / 开头的英文，最多 10 个）",
                    "list",
                    json!(t.words),
                ),
                FieldSpec::new(
                    "trigger.where",
                    &format!(
                        "触发语生效的群（群名；所有私聊写“{ALL_DMS}”，到处都生效写“{EVERYWHERE}”）"
                    ),
                    "list",
                    json!(where_names(&places, &t.r#where)),
                ),
                FieldSpec::new(
                    "trigger.mark",
                    "代发的消息末尾加“（由 Steve 代发）”",
                    "enum",
                    json!(on_off(t.mark)),
                )
                .values(&[ON, OFF]),
            ]);
        }
        Ok(Prepared {
            target_id: Some(id.to_string()),
            revision: Some(a.revision),
            title: format!("账号「{}」", a.name),
            href: format!("#/channels/acc/{id}"),
            subject: format!(
                "the chat account \"{}\" ({})",
                a.name,
                if a.mode == Mode::Own {
                    "the user's own account"
                } else {
                    "an app bot"
                }
            ),
            fields,
            ops: vec![],
            free_text: Some("duty".into()),
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
        let a = ChannelNow::load(c, viewer, p.target_id.as_deref().unwrap_or(""))?;
        let ws = &viewer.ws;
        let places = if a.mode == Mode::Own {
            a.spaces(c, viewer)?
        } else {
            Vec::new()
        };
        let mut d = Lines::default();
        let mut trigger = a.trigger.clone();
        for op in raw {
            let Some((key, v)) = set_of(&op) else {
                d.other(&op);
                continue;
            };
            let said = text_of(v).unwrap_or_default();
            if let Some(tk) = key.strip_prefix("trigger.") {
                let label = match tk {
                    "on" => L_TRIGGER_ON,
                    "words" => L_TRIGGER_WORDS,
                    "where" => L_TRIGGER_WHERE,
                    "mark" => L_TRIGGER_MARK,
                    _ => {
                        d.other(&op);
                        continue;
                    }
                };
                if a.mode != Mode::Own {
                    d.line(refused(label, "—", TRIGGER_SELF_ONLY));
                    continue;
                }
                let t = rules::Trigger::from_value(&a.trigger);
                match tk {
                    "on" | "mark" => {
                        let old = if tk == "on" { t.on } else { t.mark };
                        match on_off_flag(v) {
                            Some(b) => {
                                trigger[tk] = json!(b);
                                d.line(Change::new(label, on_off(old), on_off(b)));
                            }
                            None => d.line(not_recognized(label, &said)),
                        }
                    }
                    "words" => match list_of(v) {
                        Some(ws) if ws.len() > TRIGGER_WORDS_MAX => {
                            d.line(refused(label, t.words.join(" "), "参数不对：trigger.words"))
                        }
                        Some(ws) => match ws.iter().find(|w| !word_ok(w)) {
                            Some(bad) => d.line(refused(
                                label,
                                t.words.join(" "),
                                &format!("触发语要以 / 开头、只用英文字母数字和下划线：{bad}"),
                            )),
                            None => {
                                let mut uniq: Vec<String> = Vec::new();
                                for w in ws {
                                    if !uniq.contains(&w) {
                                        uniq.push(w);
                                    }
                                }
                                d.line(Change::new(label, t.words.join(" "), uniq.join(" ")));
                                trigger["words"] = json!(uniq);
                            }
                        },
                        None => d.line(not_recognized(label, &said)),
                    },
                    _ => {
                        let Some(names) = list_of(v) else {
                            d.line(not_recognized(label, &v.to_string()));
                            continue;
                        };
                        let mut ids: Vec<String> = Vec::new();
                        let mut unknown = None;
                        for n in names {
                            let id = if n == rules::WHERE_ALL_DMS || n == ALL_DMS || n == "私聊" {
                                Some(rules::WHERE_ALL_DMS.to_string())
                            } else if n == rules::WHERE_EVERYWHERE
                                || n == EVERYWHERE
                                || n == "所有地方"
                                || n == "到处"
                            {
                                Some(rules::WHERE_EVERYWHERE.to_string())
                            } else if places.iter().any(|(id, _)| *id == n) {
                                Some(n.clone())
                            } else {
                                let hits: Vec<&String> = places
                                    .iter()
                                    .filter(|(_, name)| *name == n)
                                    .map(|(id, _)| id)
                                    .collect();
                                (hits.len() == 1).then(|| hits[0].clone())
                            };
                            match id {
                                Some(id) if !ids.contains(&id) => ids.push(id),
                                Some(_) => {}
                                None => {
                                    unknown = Some(n);
                                    break;
                                }
                            }
                        }
                        match unknown {
                            Some(n) => d.line(not_recognized(label, &n)),
                            None => {
                                let ids = rules::normal_where(ids);
                                d.line(Change::new(
                                    label,
                                    where_names(&places, &t.r#where),
                                    where_names(&places, &ids),
                                ));
                                trigger["where"] = json!(ids);
                            }
                        }
                    }
                }
                continue;
            }
            match key {
                "persona" if a.mode == Mode::Own => {
                    d.line(refused(L_DEFAULT_PERSONA, YOU, SELF_NO_PERSONA));
                }
                "persona" => match find_persona(c, ws, &said)? {
                    Some(per) if a.binding.as_deref() != Some(per.id.as_str()) => d.set(
                        "persona_id",
                        json!(per.id),
                        Change::new(L_DEFAULT_PERSONA, a.persona_text(), per.name),
                    ),
                    Some(_) => {}
                    None => d.line(not_recognized(L_DEFAULT_PERSONA, &said)),
                },
                "rules" => match check_rules(c, ws, a.mode, v)? {
                    Checked::Ok(rules, to) => {
                        if rules != a.rules {
                            let from = rules_text(c, &a.rules, a.mode);
                            d.set("rules", rules, Change::new(L_DEFAULT_RULES, from, to));
                        }
                    }
                    Checked::Unknown(x) => d.line(not_recognized(L_DEFAULT_RULES, &x)),
                    Checked::Refused(why) => d.line(refused(
                        L_DEFAULT_RULES,
                        rules_text(c, &a.rules, a.mode),
                        &why,
                    )),
                },
                "duty" => {
                    let new = appended(&a.duty, &said);
                    if said.is_empty() || new == a.duty {
                        continue;
                    }
                    if utf16_len(&new) > DUTY_MAX {
                        d.line(refused(L_ACCOUNT_DUTY, show(&a.duty), "参数不对：duty"));
                        continue;
                    }
                    let added = new.strip_prefix(a.duty.as_str()).unwrap_or(&new).trim();
                    let line = Change::new(
                        L_ACCOUNT_DUTY,
                        show(&a.duty),
                        format!("{}（追加）", show(added)),
                    );
                    d.set("duty", json!(new), line);
                }
                _ => d.other(&op),
            }
        }
        d.changes.retain(|x| x.from != x.to);
        if trigger != a.trigger {
            d.patch.insert("trigger".into(), trigger);
        }
        let mut plan = Plan {
            ops: vec![],
            changes: d.changes,
        };
        if !d.patch.is_empty() {
            plan.ops
                .push(json!({ "op": OP_CHANNEL, "connection_id": a.conn, "patch": d.patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        if let Some(s) = spaces::channel_settings(c, ws, id)? {
            return Ok(Some(s.revision));
        }
        let exists: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM connections WHERE id = ? AND workspace_id = ?)",
            params![id, ws],
            |r| r.get(0),
        )?;
        Ok(exists.then_some(1))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let mut st = c.prepare_cached(&format!("{CHANNEL_SQL} ORDER BY c.created, c.id"))?;
        let ids = st
            .query_map(params![viewer.ws, viewer.user], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                Ok(Candidate {
                    label: format!("接入账号 {}", account_name(c, &viewer.ws, &id)?),
                    code: format!("channel:{id}"),
                })
            })
            .collect()
    }
}

/// `chan.channel.patch {connection_id, patch: {persona_id?, rules?, duty?, trigger?}}`: the channel
/// fields of `PATCH /channels/:connection_id`. Rules go through [`spaces::set_account_rules`]; the
/// rest is written here with its own revision of `channel`.
struct ChannelOp(Arc<dyn Clock>);

impl ChannelOp {
    /// Writes `fields` as they are (already checked, or a `before` being put back): one revision
    /// of `channel`, audit `channel.patch`, SSE `channel`.
    fn write(
        &self,
        tx: &Tx,
        cx: &OpCx,
        conn: &str,
        fields: &Map<String, Value>,
        undo: bool,
    ) -> ApiResult<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let ws = cx.ws;
        let now = self.0.now();
        let (account, mode) = bindings::connection_account(tx, ws, conn)?;
        let set = spaces::ensure_channel_settings(tx, ws, conn, mode, now)?;
        let mut before = Map::new();
        let mut after = Map::new();
        let (mut rules, mut duty, mut trigger) =
            (set.rules.clone(), set.duty.clone(), set.trigger.clone());
        for (k, v) in fields {
            match k.as_str() {
                "rules" => {
                    before.insert(k.clone(), set.rules.clone());
                    rules = v.clone();
                }
                "duty" => {
                    before.insert(k.clone(), json!(set.duty));
                    duty = v.as_str().unwrap_or("").trim().to_string();
                }
                "trigger" => {
                    before.insert(k.clone(), set.trigger.clone());
                    trigger = v.clone();
                }
                "persona_id" => {
                    before.insert(k.clone(), json!(account_binding(tx, ws, &account)?));
                    match v.as_str() {
                        Some(p) => {
                            bindings::set_account_default(tx, &viewer(tx, cx)?, conn, p, now)?
                        }
                        None => {
                            tx.execute(
                                "DELETE FROM persona_bindings WHERE workspace_id = ? AND account_id = ? AND space_id = ''",
                                params![ws, account],
                            )?;
                        }
                    }
                }
                _ => continue,
            }
            after.insert(k.clone(), v.clone());
        }
        let version = set.revision + 1;
        tx.execute(
            "UPDATE channel_settings SET rules = ?, duty = ?, trigger = ?, revision = ?, updated = ? WHERE connection_id = ?",
            params![rules.to_string(), duty, trigger.to_string(), version, now, conn],
        )?;
        base::revision(
            tx,
            ws,
            "channel",
            conn,
            version,
            &Value::Object(before),
            &Value::Object(after),
            cx.actor,
        )?;
        let keys: Vec<&String> = fields.keys().collect();
        base::audit(
            tx,
            ws,
            cx.actor,
            "channel.patch",
            conn,
            &json!({ "fields": keys, "changeset": cx.changeset_id, "undo": undo }),
        )?;
        tx.publish(ChangeEvent::Channel {
            id: conn.into(),
            ws: ws.into(),
        });
        Ok(())
    }
}

impl ChangeOp for ChannelOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let conn = op["connection_id"].as_str().unwrap_or("");
        let a = ChannelNow::load(c, &viewer(c, cx)?, conn)?;
        let p = &op["patch"];
        if !p.is_object() {
            return Err(ApiError::invalid("patch"));
        }
        if let Some(d) = p.get("duty") {
            let ok = d.as_str().is_some_and(|d| utf16_len(d) <= DUTY_MAX);
            if !ok {
                return Err(ApiError::invalid("duty"));
            }
        }
        if let Some(t) = p.get("trigger") {
            if a.mode != Mode::Own {
                return Err(ApiError::rule(TRIGGER_SELF_ONLY));
            }
            check_trigger(t)?;
        }
        if let Some(per) = p.get("persona_id") {
            if a.mode == Mode::Own {
                return Err(ApiError::rule(SELF_NO_PERSONA));
            }
            personas::own(c, cx.ws, per.as_str().unwrap_or(""))?;
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let conn = op["connection_id"].as_str().unwrap_or("");
        let v = viewer(tx, cx)?;
        let a = ChannelNow::load(tx, &v, conn)?;
        let mut fields = op["patch"].as_object().cloned().unwrap_or_default();
        let mut before = Map::new();
        for k in fields.keys() {
            let old = match k.as_str() {
                "rules" => a.rules.clone(),
                "duty" => json!(a.duty),
                "trigger" => a.trigger.clone(),
                "persona_id" => json!(a.binding),
                _ => continue,
            };
            before.insert(k.clone(), old);
        }
        if let Some(r) = fields.remove("rules") {
            spaces::set_account_rules(tx, &v, conn, &r, self.0.now())?;
        }
        self.write(tx, cx, conn, &fields, false)?;
        Ok(Value::Object(before))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let fields = before.as_object().cloned().unwrap_or_default();
        self.write(
            tx,
            cx,
            op["connection_id"].as_str().unwrap_or(""),
            &fields,
            true,
        )
    }
}
