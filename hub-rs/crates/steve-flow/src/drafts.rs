//! Conversational configuration. This file: schedules; the [`source`] and [`flow`] targets are in their own files. The
//! `schedule` draft target (`schedule:<id>` and `schedule:new`) and the `schedule.create` /
//! `schedule.patch` operations. Times are resolved by the rules of §3.3 while planning; a phrase
//! they do not understand is not put into the draft but shown as “（没看懂时间：<原话>）”.

use std::sync::Arc;

use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use steve_modules::changes::{
    Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Plan, Prepared,
};
use steve_modules::{ApiError, ApiResult, Clock, Registry};
use steve_store::Tx;
use steve_store::base::{Change, Viewer, show};
use tokio::sync::Notify;

use crate::cron::{CronExpr, parse_tz, validate};
use crate::policy::{MissedPolicy, OverlapPolicy};
use crate::schedules::{
    self, DEFAULT_BUDGET_USD, Fields, MAX_BUDGET_USD, Mode, Origin, Schedule, Spec,
};
use crate::when::rule_cron;

pub mod flow;
pub mod source;

pub const KIND: &str = "schedule";
const MODES: [Mode; 4] = [Mode::ItemAuto, Mode::Item, Mode::Propose, Mode::Report];

pub fn register(r: &mut Registry, clock: Arc<dyn Clock>, wake: Arc<Notify>) {
    r.target(KIND, Box::new(ScheduleTarget))
        .op(
            "schedule.create",
            Box::new(CreateOp {
                clock: clock.clone(),
                wake: wake.clone(),
            }),
        )
        .op("schedule.patch", Box::new(PatchOp { clock, wake }));
}

fn missed_label(m: MissedPolicy) -> &'static str {
    match m {
        MissedPolicy::CatchUpOnce => "补一次",
        MissedPolicy::Skip => "跳过",
    }
}

fn overlap_label(o: OverlapPolicy) -> &'static str {
    match o {
        OverlapPolicy::Skip => "跳过",
        OverlapPolicy::Run => "照常运行",
    }
}

fn describe(cron: &str) -> String {
    CronExpr::parse(cron)
        .map(|e| e.describe())
        .unwrap_or_else(|_| format!("按 cron「{cron}」"))
}

fn bot_name(c: &Connection, id: &str) -> String {
    c.query_row("SELECT name FROM bots WHERE id = ?", [id], |r| r.get(0))
        .unwrap_or_else(|_| id.to_string())
}

/// A bot of the workspace by exact name or id (never a guess).
fn find_bot(c: &Connection, ws: &str, said: &str) -> ApiResult<Option<String>> {
    Ok(c.query_row(
        "SELECT b.id FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ? WHERE b.deleted = 0 AND (b.id = ?2 OR b.name = ?2) ORDER BY b.id = ?2 DESC LIMIT 1",
        params![ws, said.trim()],
        |r| r.get(0),
    )
    .optional()?)
}

fn main_bot(c: &Connection) -> ApiResult<Option<String>> {
    Ok(c.query_row(
        "SELECT id FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()?)
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    Mode::parse(s).or_else(|| MODES.into_iter().find(|m| m.label() == s))
}

fn parse_missed(s: &str) -> Option<MissedPolicy> {
    match s {
        "catch_up_once" | "补一次" | "补跑" | "补跑一次" => {
            Some(MissedPolicy::CatchUpOnce)
        }
        "skip" | "跳过" | "算了" => Some(MissedPolicy::Skip),
        _ => None,
    }
}

fn parse_overlap(s: &str) -> Option<OverlapPolicy> {
    match s {
        "skip" | "跳过" => Some(OverlapPolicy::Skip),
        "run" | "照常" | "照常运行" => Some(OverlapPolicy::Run),
        _ => None,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "是" | "开" | "启用" | "开启" => Some(true),
        "false" | "否" | "关" | "停用" | "关闭" => Some(false),
        _ => None,
    }
}

fn parse_budget(s: &str) -> Option<f64> {
    let n: f64 = s
        .trim()
        .trim_start_matches('$')
        .trim_end_matches("美元")
        .trim()
        .parse()
        .ok()?;
    (0.0..=MAX_BUDGET_USD).contains(&n).then_some(n)
}

/// A `when` phrase (or a cron) → `(cron, describe)` by the rules only; `None` = not understood.
fn time_of(said: &str, tz: &str, now: i64) -> Option<(String, String)> {
    let tz = parse_tz(tz).ok()?;
    let now = DateTime::from_timestamp_millis(now)?;
    let rule = rule_cron(said).ok()??;
    let expr = validate(&rule.cron, tz, now).ok()?;
    Some((expr.as_str().to_string(), expr.describe()))
}

/// One field of a schedule as the model may set it, checked. `Err` = a “（没认出…）” line.
enum Field {
    Name(String),
    Time {
        when: String,
        cron: String,
        describe: String,
    },
    What(String),
    Bot(String),
    Mode(Mode),
    Enabled(bool),
    Missed(MissedPolicy),
    Overlap(OverlapPolicy),
    Budget(f64),
}

const UNRECOGNIZED: &str = "（没认出）";

fn field(
    c: &Connection,
    ws: &str,
    tz: &str,
    now: i64,
    key: &str,
    v: &Value,
) -> Result<Field, Change> {
    let raw = text_of(v).unwrap_or_default();
    let bad = |label: &str| Change::new(label, "—", format!("（没认出：{}）", show(&raw)));
    Ok(match key {
        "name" if (1..=40).contains(&raw.chars().count()) => Field::Name(raw),
        "name" => return Err(bad("名字")),
        "when" | "cron" => match time_of(&raw, tz, now) {
            Some((cron, describe)) => Field::Time {
                when: if key == "when" {
                    raw.clone()
                } else {
                    describe.clone()
                },
                cron,
                describe,
            },
            None => return Err(Change::new("时间", "—", format!("（没看懂时间：{raw}）"))),
        },
        "what" if (1..=2000).contains(&raw.chars().count()) => Field::What(raw),
        "what" => return Err(bad("做什么")),
        "bot" => match find_bot(c, ws, &raw).ok().flatten() {
            Some(id) => Field::Bot(id),
            None => return Err(bad("交给谁")),
        },
        "mode" => Field::Mode(parse_mode(&raw).ok_or_else(|| bad("方式"))?),
        "enabled" => Field::Enabled(parse_bool(&raw).ok_or_else(|| bad("启用"))?),
        "missed" => Field::Missed(parse_missed(&raw).ok_or_else(|| bad("错过了怎么办"))?),
        "overlap" => Field::Overlap(parse_overlap(&raw).ok_or_else(|| bad("上一次没结束"))?),
        "budget_usd" | "budget" => {
            Field::Budget(parse_budget(&raw).ok_or_else(|| bad("每次预算"))?)
        }
        _ => {
            return Err(Change::new(
                UNRECOGNIZED,
                "—",
                show(&format!("{key}: {raw}")),
            ));
        }
    })
}

/// The `set` map of a `schedule.patch` op, and its preview line, for one checked field.
fn patch_line(c: &Connection, s: &Schedule, f: &Field, set: &mut Map<String, Value>) -> Change {
    let sp = &s.spec;
    match f {
        Field::Name(n) => {
            set.insert("name".into(), json!(n));
            Change::new("名字", show(&s.name), show(n))
        }
        Field::Time {
            when,
            cron,
            describe: d,
        } => {
            set.insert("when".into(), json!(when));
            set.insert("cron".into(), json!(cron));
            Change::new("时间", describe(&sp.cron), format!("{d}（{cron}）"))
        }
        Field::What(w) => {
            set.insert("what".into(), json!(w));
            Change::new("做什么", show(&sp.what), show(w))
        }
        Field::Bot(b) => {
            set.insert("bot".into(), json!(b));
            Change::new("交给谁", bot_name(c, &sp.bot), bot_name(c, b))
        }
        Field::Mode(m) => {
            set.insert("mode".into(), json!(m.as_str()));
            Change::new("方式", sp.mode.label(), m.label())
        }
        Field::Enabled(e) => {
            set.insert("enabled".into(), json!(e));
            let l = |b: bool| if b { "启用" } else { "停用" };
            Change::new("启用", l(s.enabled), l(*e))
        }
        Field::Missed(m) => {
            set.insert("missed".into(), json!(m.as_str()));
            Change::new("错过了怎么办", missed_label(sp.missed), missed_label(*m))
        }
        Field::Overlap(o) => {
            set.insert("overlap".into(), json!(o.as_str()));
            Change::new("上一次没结束", overlap_label(sp.overlap), overlap_label(*o))
        }
        Field::Budget(b) => {
            set.insert("budget_usd".into(), json!(b));
            Change::new(
                "每次预算",
                crate::budget::usd(sp.budget_usd),
                crate::budget::usd(*b),
            )
        }
    }
}

/// Applies a `set` map to a schedule: `(name, spec, enabled)`.
fn apply_set(s: &Schedule, set: &Value) -> (String, Spec, bool) {
    let mut spec = s.spec.clone();
    let mut name = s.name.clone();
    let mut enabled = s.enabled;
    let str_of = |k: &str| set.get(k).and_then(Value::as_str).map(String::from);
    if let Some(v) = str_of("name") {
        name = v;
    }
    if let (Some(w), Some(c)) = (str_of("when"), str_of("cron")) {
        spec.when = w;
        spec.cron = c;
    }
    if let Some(v) = str_of("what") {
        spec.what = v;
    }
    if let Some(v) = str_of("bot") {
        spec.bot = v;
    }
    if let Some(m) = str_of("mode").and_then(|m| Mode::parse(&m)) {
        spec.mode = m;
    }
    if let Some(v) = set.get("enabled").and_then(Value::as_bool) {
        enabled = v;
    }
    if let Some(v) = str_of("missed") {
        spec.missed = MissedPolicy::from_db(Some(&v));
    }
    if let Some(v) = str_of("overlap") {
        spec.overlap = OverlapPolicy::from_db(Some(&v));
    }
    if let Some(v) = set.get("budget_usd").and_then(Value::as_f64) {
        spec.budget_usd = v;
    }
    (name, spec, enabled)
}

struct ScheduleTarget;

impl DraftTarget for ScheduleTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let tz = schedules::workspace_tz(c, &viewer.ws)?;
        let modes: Vec<&str> = MODES.iter().map(|m| m.label()).collect();
        let Some(id) = id else {
            return Ok(Prepared {
                target_id: None,
                revision: None,
                title: "新建定时任务".into(),
                href: "#/auto/schedules".into(),
                subject: "a new scheduled task".into(),
                fields: vec![],
                ops: vec![OpSpec::new(
                    "schedule.create",
                    &format!(
                        "{{\"name\": <1-40 chars>, \"when\": <time in Chinese, e.g. 每周五 09:00> or \"cron\": <5-field cron>, \"what\": <what to do>, \"bot\": <bot name, default Steve>, \"mode\": {}, \"missed\": 补一次|跳过, \"overlap\": 跳过|照常运行, \"budget_usd\": <0-50, USD>}}",
                        modes.join("|")
                    ),
                )],
                free_text: None,
                context: json!({ "tz": tz }),
            });
        };
        let s = schedules::own(c, &viewer.ws, id)?;
        let sp = &s.spec;
        Ok(Prepared {
            target_id: Some(s.id.clone()),
            revision: Some(s.updated),
            title: format!("定时任务「{}」", s.name),
            href: format!("#/auto/schedules?id={}", s.id),
            subject: format!("the scheduled task \"{}\"", s.name),
            fields: vec![
                FieldSpec::new("name", "名字", "text", json!(s.name)),
                FieldSpec::new(
                    "when",
                    "时间（中文说法，例如 每天 09:00）",
                    "text",
                    json!(describe(&sp.cron)),
                ),
                FieldSpec::new("what", "做什么", "text", json!(sp.what)),
                FieldSpec::new("bot", "交给谁", "ref", json!(bot_name(c, &sp.bot))),
                FieldSpec::new("mode", "方式", "enum", json!(sp.mode.label())).values(&modes),
                FieldSpec::new(
                    "enabled",
                    "启用",
                    "enum",
                    json!(if s.enabled { "是" } else { "否" }),
                )
                .values(&["是", "否"]),
                FieldSpec::new(
                    "missed",
                    "错过了怎么办",
                    "enum",
                    json!(missed_label(sp.missed)),
                )
                .values(&["补一次", "跳过"]),
                FieldSpec::new(
                    "overlap",
                    "上一次没结束",
                    "enum",
                    json!(overlap_label(sp.overlap)),
                )
                .values(&["跳过", "照常运行"]),
                FieldSpec::new(
                    "budget_usd",
                    "每次预算（美元，0 = 不限）",
                    "text",
                    json!(sp.budget_usd),
                ),
            ],
            ops: vec![],
            free_text: Some("what".into()),
            context: json!({ "tz": sp.tz }),
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        p: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let tz = p.context["tz"]
            .as_str()
            .unwrap_or(crate::cron::DEFAULT_TZ)
            .to_string();
        let now = steve_store::now();
        let mut plan = Plan::default();
        match &p.target_id {
            Some(id) => {
                let s = schedules::own(c, &viewer.ws, id)?;
                let mut set = Map::new();
                for op in raw {
                    if op["op"] != "set" {
                        plan.changes
                            .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                        continue;
                    }
                    let key = op["key"].as_str().unwrap_or("");
                    match field(c, &viewer.ws, &tz, now, key, &op["value"]) {
                        Ok(f) => plan.changes.push(patch_line(c, &s, &f, &mut set)),
                        Err(line) => plan.changes.push(line),
                    }
                }
                if !set.is_empty() {
                    plan.ops
                        .push(json!({ "op": "schedule.patch", "id": s.id, "set": set }));
                }
            }
            None => {
                for op in raw {
                    if op["op"] != "schedule.create" {
                        plan.changes
                            .push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                        continue;
                    }
                    let (o, lines) = plan_create(c, &viewer.ws, &tz, now, &op)?;
                    plan.changes.extend(lines);
                    if let Some(o) = o {
                        plan.ops.push(o);
                        break;
                    }
                }
            }
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(schedules::get(c, ws, id)?.map(|s| s.updated))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let mut out: Vec<Candidate> = schedules::list(c, &viewer.ws)?
            .into_iter()
            .map(|s| Candidate {
                code: format!("schedule:{}", s.id),
                label: format!("定时任务 {}", s.name),
            })
            .collect();
        out.push(Candidate {
            code: "schedule:new".into(),
            label: "新建定时任务".into(),
        });
        Ok(out)
    }
}

/// The fields of a `schedule.create` op checked, with their preview lines.
fn create_lines(
    c: &Connection,
    ws: &str,
    tz: &str,
    now: i64,
    op: &Value,
) -> (Vec<Field>, Vec<Change>) {
    let mut fields = Vec::new();
    let mut lines = Vec::new();
    // `cron` wins over `when` when both are there
    let time_key = if op.get("cron").is_some() {
        "cron"
    } else {
        "when"
    };
    for key in [
        "name",
        time_key,
        "what",
        "bot",
        "mode",
        "missed",
        "overlap",
        "budget_usd",
    ] {
        let Some(v) = op.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        match field(c, ws, tz, now, key, v) {
            Ok(f) => fields.push(f),
            Err(l) => lines.push(l),
        }
    }
    (fields, lines)
}

/// A checked `schedule.create` with its preview lines; no op (only the lines saying why) when
/// something is not understood or missing.
fn plan_create(
    c: &Connection,
    ws: &str,
    tz: &str,
    now: i64,
    op: &Value,
) -> ApiResult<(Option<Value>, Vec<Change>)> {
    let (fields, errors) = create_lines(c, ws, tz, now, op);
    if !errors.is_empty() {
        return Ok((None, errors));
    }
    let (mut name, mut time, mut what, mut bot) = (None, None, None, None);
    let mut spec = json!({ "mode": Mode::ItemAuto.as_str(), "missed": "catch_up_once", "overlap": "skip", "budget_usd": DEFAULT_BUDGET_USD, "tz": tz, "project_id": null, "tags": [] });
    let mut lines = Vec::new();
    for f in &fields {
        match f {
            Field::Name(n) => name = Some(n.clone()),
            Field::Time {
                when,
                cron,
                describe,
            } => time = Some((when.clone(), cron.clone(), describe.clone())),
            Field::What(w) => what = Some(w.clone()),
            Field::Bot(b) => bot = Some(b.clone()),
            Field::Mode(m) => spec["mode"] = json!(m.as_str()),
            Field::Missed(m) => spec["missed"] = json!(m.as_str()),
            Field::Overlap(o) => spec["overlap"] = json!(o.as_str()),
            Field::Budget(b) => spec["budget_usd"] = json!(b),
            Field::Enabled(_) => {}
        }
    }
    let missing = |what: &str| {
        Ok((
            None,
            vec![Change::new("（没说清楚）", "—", format!("缺少{what}"))],
        ))
    };
    let Some((when, cron, d)) = time else {
        return missing("时间");
    };
    let Some(what) = what else {
        return missing("要做什么");
    };
    let name = name.unwrap_or_else(|| what.chars().take(40).collect());
    let Some(bot) = bot.or(main_bot(c)?) else {
        return missing("交给谁");
    };
    spec["when"] = json!(when);
    spec["cron"] = json!(cron);
    spec["what"] = json!(what);
    spec["bot"] = json!(bot);
    let parsed: Spec = serde_json::from_value(spec.clone())?;
    lines.push(Change::new("名字", "—", show(&name)));
    lines.push(Change::new("时间", "—", format!("{d}（{cron}）")));
    lines.push(Change::new("做什么", "—", show(&what)));
    lines.push(Change::new("交给谁", "—", bot_name(c, &bot)));
    lines.push(Change::new("方式", "—", parsed.mode.label()));
    lines.push(Change::new(
        "错过了怎么办",
        "—",
        missed_label(parsed.missed),
    ));
    lines.push(Change::new(
        "上一次没结束",
        "—",
        overlap_label(parsed.overlap),
    ));
    lines.push(Change::new(
        "每次预算",
        "—",
        crate::budget::usd(parsed.budget_usd),
    ));
    Ok((
        Some(json!({ "op": "schedule.create", "name": name, "spec": spec, "as": "schedule" })),
        lines,
    ))
}

struct CreateOp {
    clock: Arc<dyn Clock>,
    wake: Arc<Notify>,
}

impl ChangeOp for CreateOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let spec: Spec =
            serde_json::from_value(op["spec"].clone()).map_err(|_| ApiError::invalid("spec"))?;
        schedules::check_refs(c, cx.ws, &spec)
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let spec: Spec =
            serde_json::from_value(op["spec"].clone()).map_err(|_| ApiError::invalid("spec"))?;
        let id = schedules::insert(
            tx,
            cx.ws,
            cx.actor,
            &Fields {
                name: op["name"].as_str().unwrap_or(""),
                spec: &spec,
                enabled: true,
            },
            &Origin {
                origin: "steve".into(),
                label: "Steve 起草，你确认".into(),
            },
            self.clock.now(),
        )?;
        cx.bind("schedule", json!(id));
        self.wake.notify_one();
        Ok(json!({ "id": id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        let id = before["id"].as_str().unwrap_or("");
        if let Some(s) = schedules::get(tx, cx.ws, id)? {
            schedules::delete(tx, &s, self.clock.now())?;
        }
        Ok(())
    }
}

struct PatchOp {
    clock: Arc<dyn Clock>,
    wake: Arc<Notify>,
}

impl ChangeOp for PatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let s = schedules::own(c, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let (_, spec, _) = apply_set(&s, &op["set"]);
        schedules::check_refs(c, cx.ws, &spec)
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let s = schedules::own(tx, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let (name, spec, enabled) = apply_set(&s, &op["set"]);
        let f = Fields {
            name: &name,
            spec: &spec,
            enabled,
        };
        schedules::update(tx, &s, &f, cx.actor, "对话里改的", self.clock.now())?;
        self.wake.notify_one();
        Ok(json!({ "name": s.name, "spec": s.spec, "enabled": s.enabled }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let s = schedules::own(tx, cx.ws, op["id"].as_str().unwrap_or(""))?;
        let spec: Spec = serde_json::from_value(before["spec"].clone())?;
        let name = before["name"].as_str().unwrap_or(&s.name).to_string();
        let enabled = before["enabled"].as_bool().unwrap_or(s.enabled);
        let f = Fields {
            name: &name,
            spec: &spec,
            enabled,
        };
        schedules::update(tx, &s, &f, cx.actor, "撤销对话里的改动", self.clock.now())?;
        self.wake.notify_one();
        Ok(())
    }
}
