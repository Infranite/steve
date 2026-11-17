//! The agent's todo tools (`steve_todo_*`): list, add, update and complete the todos and goals of the person
//! whose session it is.
//!
//! The rules are fixed, not judged:
//! - Reading is allowed in that person's session.
//! - A session that is not a person's — the workspace's service principal, work nobody in particular asked
//!   for — is refused: todos belong to a person, and no Bot has its own.
//! - Changing that person's own todos and goals is done at once, recorded as an undoable change
//!   ([`crate::changes`]) whose id the result gives. These tools reach nobody else: nothing is sent, and
//!   another member's todos are not there for them.
//!
//! Times are explicit: a day is `YYYY-MM-DD`, a moment RFC 3339 with its offset. The result echoes what
//! was stored, in the person's time zone, so the agent can check it read the request right.

use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::StoreError;

use crate::{changes, todo};

pub const LIST: &str = "steve_todo_list";
pub const ADD: &str = "steve_todo_add";
pub const UPDATE: &str = "steve_todo_update";
pub const COMPLETE: &str = "steve_todo_complete";
/// How many todos one list returns at most.
pub const LIST_MAX: usize = 200;

pub fn handles(name: &str) -> bool {
    [LIST, ADD, UPDATE, COMPLETE].contains(&name)
}

const WHEN: &str =
    "a day YYYY-MM-DD, or a moment in RFC 3339 with its offset (2026-11-20T17:00:00+08:00)";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": properties,
        "required": required, "additionalProperties": false}})
}

fn nullable(t: &str, description: &str) -> Value {
    json!({"type": [t, "null"], "description": description})
}

pub fn tools() -> Vec<Value> {
    let fields = |nullable_ok: bool| {
        let n = |t: &str, d: &str| {
            if nullable_ok {
                nullable(t, &format!("{d}; null clears it"))
            } else {
                json!({"type": t, "description": d})
            }
        };
        json!({
            "text": {"type": "string", "minLength": 1, "maxLength": todo::TEXT_MAX},
            "kind": {"type": "string", "enum": todo::KINDS, "description": "goal: a result with a target date that todos and items are split into"},
            "goal": n("string", "the id of one of the person's goals this todo belongs to (one level)"),
            "due": n("string", &format!("deadline of a todo: {WHEN}")),
            "target_date": n("string", "a goal's date, YYYY-MM-DD"),
            "remind_at": n("string", "when to remind, RFC 3339 with offset"),
            "snooze_until": n("string", "hide until, RFC 3339 with offset"),
            "pri": {"type": "integer", "minimum": 0, "maximum": todo::PRI_MAX, "description": "0 none … 3 highest"},
            "star": {"type": "boolean"},
            "recur": n("string", "repeat: FREQ=DAILY|WEEKLY|MONTHLY|YEARLY with optional ;INTERVAL=n; needs a due"),
            "matter": n("string", "a matter (事) id Steve gave"),
        })
    };
    let mut update = fields(true);
    update["todo"] = json!({"type": "string", "description": "the todo's id"});
    vec![
        tool(
            LIST,
            "List the todos and goals of the person whose session this is (我的待办): open ones by default, deadlines first. Goals carry their progress (done of all not dropped; null when not split yet). Times are shown in the person's time zone.",
            json!({
                "state": {"type": "string", "enum": ["open", "done", "dropped", "handed", "all"]},
                "kind": {"type": "string", "enum": todo::KINDS},
                "goal": {"type": "string", "description": "only the todos under this goal"},
                "limit": {"type": "integer", "minimum": 1, "maximum": LIST_MAX},
            }),
            &[],
        ),
        tool(
            ADD,
            "Add a todo or a goal for the person whose session this is: what they themselves are to do (work for a Bot is an item, not a todo). Applied at once and undoable; it reaches nobody else. Give a deadline only when the person stated one. The result echoes what was stored; check it.",
            fields(false),
            &["text"],
        ),
        tool(
            UPDATE,
            "Change one of the person's own todos or goals: only the fields given; null clears a field. Applied at once and undoable; it reaches nobody else. The result echoes what was stored.",
            update,
            &["todo"],
        ),
        tool(
            COMPLETE,
            "Mark one of the person's own todos or goals done (default), dropped (放下), or open again. A repeating todo done makes its next occurrence. Applied at once and undoable.",
            json!({
                "todo": {"type": "string"},
                "state": {"type": "string", "enum": ["done", "dropped", "open"]},
            }),
            &["todo"],
        ),
    ]
}

fn bad(s: impl Into<String>) -> ApiError {
    ApiError::rule(s.into())
}

fn storage(e: StoreError) -> ApiError {
    match todo::message(&e) {
        Some(m) => bad(m),
        None => match e {
            StoreError::Hook(s) => bad(s),
            e => e.into(),
        },
    }
}

/// A deadline as given: a day or a moment.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Day(String),
    At(i64),
}

pub fn parse_when(s: &str) -> Result<When, String> {
    let s = s.trim();
    if todo::parse_date(s).is_some() {
        return Ok(When::Day(s.to_string()));
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| When::At(d.timestamp_millis()))
        .map_err(|_| format!("`{s}` is not {WHEN}"))
}

fn moment(s: &str) -> Result<i64, String> {
    match parse_when(s)? {
        When::At(t) => Ok(t),
        When::Day(_) => Err(format!("`{s}` needs a time and offset (RFC 3339)")),
    }
}

/// The person whose session it is; a service session is refused.
fn person(cx: &McpCx) -> ApiResult<&str> {
    if cx.user.starts_with("svc:") || cx.user.is_empty() {
        return Err(ApiError::Forbidden(
            "Todos belong to a person. This session runs for the workspace, not for one person, so it has no todos to read or change.".into(),
        ));
    }
    Ok(&cx.user)
}

/// A todo as the agent reads it: times in the person's zone.
fn view(z: todo::Zone, t: &Value) -> Value {
    let at = |k: &str| t[k].as_i64().and_then(|ms| z.show(ms));
    let due = match (t["due_date"].as_str(), at("due")) {
        (Some(d), _) => json!(d),
        (None, Some(m)) => json!(m),
        _ => Value::Null,
    };
    let mut v = json!({
        "id": t["id"], "text": t["text"], "kind": t["kind"], "state": t["state"], "due": due,
        "remind_at": at("remind_at"), "snooze_until": at("snooze_until"),
        "pri": t["pri"], "star": t["star"], "recur": t["recur"], "source": t["source"],
    });
    for (k, src) in [("goal", "parent"), ("matter", "matter")] {
        if !t[src].is_null() {
            v[k] = t[src].clone();
        }
    }
    for k in ["target_date", "item_id", "progress"] {
        if !t[k].is_null() {
            v[k] = t[k].clone();
        }
    }
    v
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    state: Option<String>,
    kind: Option<String>,
    goal: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    text: String,
    kind: Option<String>,
    goal: Option<String>,
    due: Option<String>,
    target_date: Option<String>,
    remind_at: Option<String>,
    snooze_until: Option<String>,
    #[serde(default)]
    pri: i64,
    #[serde(default)]
    star: bool,
    recur: Option<String>,
    matter: Option<String>,
}

fn opt<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    todo: String,
    text: Option<String>,
    kind: Option<String>,
    #[serde(default, deserialize_with = "opt")]
    goal: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt")]
    due: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt")]
    target_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt")]
    remind_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt")]
    snooze_until: Option<Option<String>>,
    pri: Option<i64>,
    star: Option<bool>,
    #[serde(default, deserialize_with = "opt")]
    recur: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt")]
    matter: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteArgs {
    todo: String,
    state: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<T> {
    serde_json::from_value(v.clone()).map_err(|e| bad(format!("Invalid todo arguments: {e}")))
}

/// A moment or clearing, from an optional text.
fn moment_patch(v: &Option<Option<String>>) -> ApiResult<Option<Option<i64>>> {
    v.as_ref()
        .map(|x| x.as_deref().map(moment).transpose())
        .transpose()
        .map_err(bad)
}

pub async fn call(m: &Modules, cx: &McpCx, name: &str, args: &Value) -> Result<String, String> {
    let (cx, name, args) = (cx.clone(), name.to_string(), args.clone());
    m.blocking(move |m| call_sync(m, &cx, &name, &args))
        .await
        .map_err(|e| e.to_string())
}

pub fn call_sync(m: &Modules, cx: &McpCx, name: &str, args: &Value) -> ApiResult<String> {
    let me = person(cx)?.to_string();
    let ws = cx.ws.clone();
    let now = m.clock.now();
    // the item or session that made a change, as the digest's changes name their run
    let run = cx
        .item_id
        .clone()
        .or_else(|| cx.session_id.clone())
        .unwrap_or_else(|| cx.bot_id.clone());
    let writer = cx.bot_id.clone();
    let zone = m
        .store
        .reader(|c| Ok::<_, StoreError>(todo::Zone::of(c, &ws, &me)))?;
    let mine = |c: &rusqlite::Connection, id: &str| -> ApiResult<()> {
        if todo::owner_of(c, &ws, id)?.as_deref() != Some(me.as_str()) {
            return Err(ApiError::not_found(steve_i18n::t!("digest.noun.todo")));
        }
        Ok(())
    };
    let shown = |id: &str| -> ApiResult<Value> {
        Ok(m.store
            .reader(|c| todo::get(c, &ws, id))?
            .map(|t| view(zone, &t))
            .unwrap_or(Value::Null))
    };
    let out = match name {
        LIST => {
            let a: ListArgs = parse(args)?;
            let state = a.state.unwrap_or_else(|| "open".into());
            let f = todo::Filter {
                states: if state == "all" { vec![] } else { vec![state] },
                kind: a.kind,
                parent: a.goal,
                limit: a.limit.unwrap_or(50).clamp(1, LIST_MAX),
            };
            let rows = m.store.reader(|c| todo::list_by(c, &ws, &me, &f))?;
            json!({
                "todos": rows.iter().map(|t| view(zone, t)).collect::<Vec<_>>(),
                "now": zone.show(now), "zone": zone.name(),
            })
        }
        ADD => {
            let a: AddArgs = parse(args)?;
            let (due, due_date) = match a.due.as_deref().map(parse_when).transpose().map_err(bad)? {
                Some(When::At(t)) => (Some(t), None),
                Some(When::Day(d)) => (None, Some(d)),
                None => (None, None),
            };
            let remind_at = a
                .remind_at
                .as_deref()
                .map(moment)
                .transpose()
                .map_err(bad)?;
            let snooze = a
                .snooze_until
                .as_deref()
                .map(moment)
                .transpose()
                .map_err(bad)?;
            let (id, change) = m.store.transaction(|tx| {
                let id = todo::create(
                    tx,
                    &todo::NewTodo {
                        ws: &ws,
                        owner: &me,
                        text: &a.text,
                        kind: a.kind.as_deref().unwrap_or("todo"),
                        parent: a.goal.as_deref(),
                        matter: a.matter.as_deref(),
                        due,
                        due_date: due_date.as_deref(),
                        target_date: a.target_date.as_deref(),
                        remind_at,
                        snooze_until: snooze,
                        pri: a.pri,
                        star: a.star,
                        recur: a.recur.as_deref(),
                        source: "agent",
                        writer: &writer,
                        ..Default::default()
                    },
                    now,
                )
                .map_err(storage)?;
                let updated = todo::fields(tx, &ws, &id)?.map(|x| x.2);
                let change = changes::record(
                    tx,
                    &ws,
                    &run,
                    &writer,
                    "todo",
                    &id,
                    None,
                    &json!({}),
                    &json!({"todo": id, "updated": updated}),
                    now,
                )?;
                Ok::<_, ApiError>((id, change))
            })?;
            json!({"todo": shown(&id)?, "change": change, "zone": zone.name(),
                "note": "Added to the person's own todos; they can undo it."})
        }
        UPDATE => {
            let a: UpdateArgs = parse(args)?;
            let (due, due_date) = match &a.due {
                None => (None, None),
                Some(None) => (Some(None), Some(None)),
                Some(Some(s)) => match parse_when(s).map_err(bad)? {
                    When::At(t) => (Some(Some(t)), None),
                    When::Day(d) => (None, Some(Some(d))),
                },
            };
            let p = todo::Patch {
                text: a.text,
                kind: a.kind,
                parent: a.goal,
                matter: a.matter,
                target_date: a.target_date,
                due,
                due_date,
                remind_at: moment_patch(&a.remind_at)?,
                snooze_until: moment_patch(&a.snooze_until)?,
                pri: a.pri,
                star: a.star,
                recur: a.recur,
            };
            let change = m.store.transaction(|tx| {
                mine(tx, &a.todo)?;
                let (before, after) = todo::edit(tx, &ws, &a.todo, &p, now)
                    .map_err(storage)?
                    .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
                Ok::<_, ApiError>(changes::record(
                    tx,
                    &ws,
                    &run,
                    &writer,
                    "todo_edit",
                    &a.todo,
                    None,
                    &json!(before),
                    &json!({"todo": a.todo, "fields": after, "updated": now}),
                    now,
                )?)
            })?;
            json!({"todo": shown(&a.todo)?, "change": change, "zone": zone.name()})
        }
        COMPLETE => {
            let a: CompleteArgs = parse(args)?;
            let state = a.state.unwrap_or_else(|| "done".into());
            if !["done", "dropped", "open"].contains(&state.as_str()) {
                return Err(bad("state is done, dropped or open"));
            }
            let (change, next) = m.store.transaction(|tx| {
                mine(tx, &a.todo)?;
                crate::api::set_todo_state(tx, &ws, &run, &writer, &a.todo, &state, None, now)
            })?;
            let next = next.as_deref().map(shown).transpose()?;
            json!({"todo": shown(&a.todo)?, "next": next, "change": change, "zone": zone.name()})
        }
        _ => return Err(bad(format!("MCP error -32602: Tool {name} not found"))),
    };
    Ok(out.to_string())
}
