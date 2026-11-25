//! Conversational configuration (变更集): the user says what they want, Steve drafts a field-level
//! change, stores it in SQLite `changesets` via `steve-modules::changes`, applies it in one
//! transaction, and supports 24-hour undo across restarts.

use crate::error::{Result, msg};
use crate::gateway::{FieldSpec, TAG_CATS, Tier, draft_fields, first_json, jstr, truthy};
use crate::repo::{Bot, BotExt};
use crate::util::{js_head, js_len};
use crate::{Core, patch};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::time::Duration;
use steve_modules::changes::{ChangeOp, OpCx, Origin, Plan, Prepared};
use steve_modules::{ApiError, ApiResult};
use steve_types::settings::{self as ws_settings, SettingSpec};

pub use steve_modules::changes::Proposal;
pub use steve_store::base::Change;

/// Core operation names registered on [`steve_modules::Registry`].
const OP_BOT_CREATE: &str = "core.bot.create";
const OP_BOT_PATCH: &str = "core.bot.patch";
const OP_PROJECT_PATCH: &str = "core.project.patch";
const OP_SETTINGS_PATCH: &str = "core.settings.patch";
const OP_TAGS_APPLY: &str = "core.tags.apply";
const OP_MODELS_PATCH: &str = "core.models.patch";

/// Workspace setting defaults (`{key: def}`) from the shared schema in `steve-types`.
pub fn setting_defaults() -> Map<String, Value> {
    ws_settings::defaults()
        .map(|(k, v)| (k.into(), json!(v)))
        .collect()
}

const ACC: [(&str, &str); 3] = [
    ("inherit", "跟随全局"),
    ("manual", "需要你验收"),
    ("auto", "自动验收"),
];
/// Permission modes and their labels.
pub const PERM: [(&str, &str); 3] = [
    ("plan", "只读：只出方案，不改文件"),
    ("default", "每次改文件或运行命令都问你"),
    (
        "bypassPermissions",
        "不再询问：工具直接运行，由系统沙箱兜底",
    ),
];
const PRO: [&str; 3] = ["只在必要时", "适中", "积极"];

fn label_of(table: &[(&str, &str)], k: &str) -> Value {
    table
        .iter()
        .find(|(a, _)| *a == k)
        .map(|(_, b)| json!(b))
        .unwrap_or(Value::Null)
}
fn key_of(table: &[(&str, &str)], label: &Value) -> Option<String> {
    table
        .iter()
        .find(|(_, b)| label.as_str() == Some(b))
        .map(|(a, _)| a.to_string())
}

/// What a draft changes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `show()`: empty → （空）; long values cut at 40 with an ellipsis.
fn show(v: &Value) -> String {
    let s = match v {
        Value::Null => return "（空）".into(),
        v => jstr(v),
    };
    if s.is_empty() {
        "（空）".into()
    } else if js_len(&s) > 40 {
        format!("{}…", js_head(&s, 40))
    } else {
        s
    }
}

type Codec = Box<dyn Fn(&Value) -> Value + Send + Sync>;

struct BotField {
    spec: FieldSpec,
    enc: Option<Codec>,
    dec: Option<Codec>,
}

fn text_field(key: &str, label: &str, current: &str) -> BotField {
    BotField {
        spec: FieldSpec::new(key, label, "text", json!(current)),
        enc: None,
        dec: None,
    }
}

const STANDARD_EFFORTS: &[&str] = &[
    "none", "off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

fn encode_effort_value(s: &str, offered: &[(String, String)]) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s == "默认" {
        return None;
    }
    if let Some((k, _)) = offered.iter().find(|(k, l)| l == s || k == s) {
        return Some(k.clone());
    }
    if let Some(&k) = STANDARD_EFFORTS
        .iter()
        .find(|&&k| crate::models::effort_label(k) == Some(s) || k == s)
    {
        return Some(k.to_string());
    }
    // Never persist localized CJK labels into `bots.reasoning_effort`.
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
        .then(|| s.to_string())
}

fn sanitize_bot_for_harness(
    patch: &mut Map<String, Value>,
    cur: Option<&Bot>,
    harnesses: &[crate::harnesses::Harness],
) -> Vec<String> {
    let mut dropped = Vec::new();
    let target_id = patch
        .get("harness")
        .and_then(Value::as_str)
        .or_else(|| cur.map(|b| b.harness.as_str()))
        .unwrap_or_default();
    let Some(target) = harnesses.iter().find(|h| h.id == target_id) else {
        return dropped;
    };
    let switched = patch
        .get("harness")
        .and_then(Value::as_str)
        .is_some_and(|next_id| cur.is_none_or(|b| b.harness.as_str() != next_id));

    if let Some(pm) = patch.get("perm_mode").and_then(Value::as_str) {
        if steve_node_api::PermissionMode::from_bot_key(pm)
            .is_some_and(|m| !target.offers_bot_mode(m))
        {
            if switched {
                patch.insert("perm_mode".into(), json!(target.default_perm_mode()));
            } else {
                patch.remove("perm_mode");
                dropped.push("perm_mode".into());
            }
        }
    } else if switched
        && let Some(b) = cur
        && steve_node_api::PermissionMode::from_bot_key(&b.perm_mode)
            .is_some_and(|m| !target.offers_bot_mode(m))
    {
        patch.insert("perm_mode".into(), json!(target.default_perm_mode()));
    }

    if let Some(val) = patch.get("reasoning_effort") {
        if let Some(eff) = val.as_str()
            && !target.takes_effort(eff)
        {
            if switched && cur.and_then(|b| b.reasoning_effort.as_ref()).is_some() {
                patch.insert("reasoning_effort".into(), Value::Null);
            } else {
                patch.remove("reasoning_effort");
                dropped.push("reasoning_effort".into());
            }
        }
    } else if switched
        && let Some(eff) = cur.and_then(|b| b.reasoning_effort.as_deref())
        && !target.takes_effort(eff)
    {
        patch.insert("reasoning_effort".into(), Value::Null);
    }

    if switched && !patch.contains_key("model") && cur.and_then(|b| b.model.as_ref()).is_some() {
        patch.insert("model".into(), Value::Null);
    }
    dropped
}

/// `harnesses`: the Hub's adapters, so `harness`, `reasoning_effort` and `perm_mode` reflect what
/// the Hub's adapters offer even when the same draft switches `harness`.
fn bot_fields(
    b: &Bot,
    projects: &[crate::repo::Project],
    harnesses: &[crate::harnesses::Harness],
) -> Vec<BotField> {
    let pro_at = |v: &Value| {
        v.as_i64()
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| PRO.get(i))
            .map(|s| json!(s))
            .unwrap_or(Value::Null)
    };
    let ps: Vec<(String, String)> = projects
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    let ps2 = ps.clone();
    let ps3 = ps.clone();
    let cur_h = harnesses.iter().find(|h| h.id == b.harness);
    let mut efforts: Vec<(String, String)> = Vec::new();
    for h in cur_h.into_iter().chain(harnesses.iter()) {
        for e in &h.efforts {
            let lbl = crate::models::effort_label(e)
                .map(str::to_string)
                .unwrap_or_else(|| e.clone());
            if !efforts.iter().any(|(k, l)| k == e || l == &lbl) {
                efforts.push((e.clone(), lbl));
            }
        }
    }
    let efforts_enc = efforts.clone();
    let efforts_dec = efforts.clone();
    let cur_effort = b.reasoning_effort.as_deref().map_or_else(
        || "默认".to_string(),
        |e| {
            efforts
                .iter()
                .find(|(k, _)| k == e)
                .map(|(_, l)| l.clone())
                .or_else(|| crate::models::effort_label(e).map(str::to_string))
                .unwrap_or_else(|| e.to_string())
        },
    );
    let perms: Vec<(&'static str, &'static str)> = PERM
        .iter()
        .copied()
        .filter(|(k, _)| {
            *k == b.perm_mode
                || harnesses.is_empty()
                || harnesses.iter().any(|h| {
                    steve_node_api::PermissionMode::from_bot_key(k)
                        .is_none_or(|m| h.offers_bot_mode(m))
                })
        })
        .collect();
    let perms_enc = perms.clone();
    let perm_keep = b.perm_mode.clone();
    let hs: Vec<(String, String)> = harnesses
        .iter()
        .map(|h| (h.id.clone(), h.name.clone()))
        .collect();
    let hs_enc = hs.clone();
    let hs_dec = hs.clone();
    let harness_keep = b.harness.clone();
    let cur_harness_name = crate::harnesses::name_of(harnesses, &b.harness);
    let mut fs = vec![
        text_field("name", "名字", &b.name),
        BotField {
            spec: FieldSpec {
                key: "harness".into(),
                label: "Harness".into(),
                ty: "enum".into(),
                values: Some(
                    hs.iter()
                        .map(|(_, n)| n.clone())
                        .chain(
                            (!hs.iter().any(|(id, _)| id == &b.harness))
                                .then(|| cur_harness_name.clone()),
                        )
                        .collect(),
                ),
                current: json!(cur_harness_name),
            },
            enc: Some(Box::new(move |v| {
                let s = jstr(v);
                let s = s.trim();
                hs_enc
                    .iter()
                    .find(|(id, n)| n == s || id == s)
                    .map(|(id, _)| json!(id))
                    .unwrap_or_else(|| json!(harness_keep))
            })),
            dec: Some(Box::new(move |v| {
                let s = v.as_str().unwrap_or("");
                hs_dec
                    .iter()
                    .find(|(id, _)| id == s)
                    .map(|(_, n)| json!(n))
                    .unwrap_or_else(|| json!(crate::models::harness_name(s)))
            })),
        },
        text_field("role", "职责", &b.role),
        text_field("persona", "说话方式", &b.persona),
        BotField {
            spec: FieldSpec {
                key: "proactive".into(),
                label: "主动程度".into(),
                ty: "enum".into(),
                values: Some(PRO.iter().map(|s| s.to_string()).collect()),
                current: pro_at(&json!(b.proactive)),
            },
            enc: Some(Box::new(|v| {
                json!(PRO.iter().position(|p| *p == jstr(v)).unwrap_or(0))
            })),
            dec: Some(Box::new(pro_at)),
        },
        text_field("rules", "工作规范", &b.rules),
        BotField {
            spec: FieldSpec {
                key: "reasoning_effort".into(),
                label: "思考强度".into(),
                ty: "enum".into(),
                values: Some(
                    std::iter::once("默认".to_string())
                        .chain(efforts.iter().map(|(_, l)| l.clone()))
                        .collect(),
                ),
                current: json!(cur_effort),
            },
            enc: Some(Box::new(move |v| {
                let s = jstr(v);
                match encode_effort_value(&s, &efforts_enc) {
                    Some(k) => json!(k),
                    None => Value::Null,
                }
            })),
            dec: Some(Box::new(move |v| {
                if v.is_null() {
                    json!("默认")
                } else {
                    let s = v.as_str().unwrap_or("");
                    efforts_dec
                        .iter()
                        .find(|(k, _)| k == s)
                        .map(|(_, l)| json!(l))
                        .or_else(|| crate::models::effort_label(s).map(|l| json!(l)))
                        .unwrap_or_else(|| v.clone())
                }
            })),
        },
        BotField {
            spec: FieldSpec {
                key: "accept_mode".into(),
                label: "交付验收".into(),
                ty: "enum".into(),
                values: Some(ACC.iter().map(|(_, l)| l.to_string()).collect()),
                current: label_of(&ACC, &b.accept_mode),
            },
            enc: Some(Box::new(|v| {
                json!(key_of(&ACC, v).unwrap_or_else(|| "inherit".into()))
            })),
            dec: Some(Box::new(|v| label_of(&ACC, v.as_str().unwrap_or("")))),
        },
        BotField {
            spec: FieldSpec::new(
                "model",
                "模型（留空为默认）",
                "text",
                json!(b.model.clone().unwrap_or_default()),
            ),
            enc: Some(Box::new(|v| {
                if truthy(v) {
                    json!(jstr(v))
                } else {
                    Value::Null
                }
            })),
            dec: None,
        },
        BotField {
            spec: FieldSpec {
                key: "perm_mode".into(),
                label: "权限".into(),
                ty: "enum".into(),
                values: Some(perms.iter().map(|(_, l)| l.to_string()).collect()),
                current: label_of(&PERM, &b.perm_mode),
            },
            enc: Some(Box::new(move |v| {
                json!(key_of(&perms_enc, v).unwrap_or_else(|| perm_keep.clone()))
            })),
            dec: Some(Box::new(|v| label_of(&PERM, v.as_str().unwrap_or("")))),
        },
        BotField {
            spec: FieldSpec {
                key: "default_project".into(),
                label: "默认项目".into(),
                ty: "enum".into(),
                values: Some(
                    std::iter::once("无".to_string())
                        .chain(ps.iter().map(|(_, n)| n.clone()))
                        .collect(),
                ),
                current: json!(
                    ps.iter()
                        .find(|(id, _)| Some(id) == b.default_project.as_ref())
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("无")
                ),
            },
            enc: Some(Box::new(move |v| {
                ps2.iter()
                    .find(|(_, n)| v.as_str() == Some(n))
                    .map(|(id, _)| json!(id))
                    .unwrap_or(Value::Null)
            })),
            dec: Some(Box::new(move |v| {
                json!(
                    ps3.iter()
                        .find(|(id, _)| v.as_str() == Some(id))
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("无")
                )
            })),
        },
    ];
    /* the main bot is always on */
    if !b.main {
        fs.push(BotField {
            spec: FieldSpec {
                key: "enabled".into(),
                label: "启用状态".into(),
                ty: "enum".into(),
                values: Some(vec!["启用".into(), "停用".into()]),
                current: json!(if b.enabled { "启用" } else { "停用" }),
            },
            enc: Some(Box::new(|v| json!(jstr(v) != "停用"))),
            dec: Some(Box::new(|v| {
                json!(if v.as_bool() == Some(false) {
                    "停用"
                } else {
                    "启用"
                })
            })),
        });
    }
    fs
}

/// The name a bot target that makes a new bot carries: `new`, or `new:<name>` when one request makes
/// several; `None` for an existing bot.
fn new_bot_name(id: &str) -> Option<&str> {
    match id.split_once(':') {
        Some(("new", n)) => Some(n.trim()),
        None if id == "new" => Some(""),
        _ => None,
    }
}

/// A bot that does not exist yet, with the workspace's default harness and permission mode: the
/// draft of a new bot starts from it.
fn blank_bot(ws: &str, name: &str, m: &crate::models::Models) -> Bot {
    let (harness, perm_mode) = m.default_bot();
    Bot {
        id: String::new(),
        workspace_id: ws.into(),
        name: name.into(),
        role: String::new(),
        specialties: vec![],
        main: false,
        harness,
        model: None,
        rules: String::new(),
        accept_mode: "inherit".into(),
        perm_mode,
        enabled: true,
        revision: 0,
        updated: 0,
        persona: String::new(),
        proactive: 1,
        avatar: Default::default(),
        default_project: None,
        prompt: None,
        device: None,
        reasoning_effort: None,
    }
}

/// A field's label as a card shows it, in the request's language. The Chinese labels of the fields are what the
/// model reads; `full` is the label with its note (“模型（opus、sonnet…）”), else the name only. `fallback` is
/// the model's label for a field without a key of its own.
fn field_shown(
    kind: &str,
    key: &str,
    full: bool,
    fallback: &str,
    harnesses: &[crate::harnesses::Harness],
) -> String {
    let shown = match (kind, key) {
        ("bot", "model") if full => Some(steve_i18n::t!("core.draft.bot.model_hint")),
        ("bot", k) => bot_field_shown(k),
        ("project", k) => project_field_shown(k),
        ("settings", "item_prefix") if full => {
            Some(steve_i18n::t!("core.draft.setting.item_prefix_hint"))
        }
        ("settings", "item_prefix") => Some(steve_i18n::t!("core.draft.setting.item_prefix")),
        ("settings", k) => setting_label_shown(k),
        ("models", k) => models_field_shown(k, harnesses),
        _ => None,
    };
    shown.unwrap_or_else(|| fallback.to_string())
}

fn bot_field_shown(key: &str) -> Option<String> {
    Some(match key {
        "name" => steve_i18n::t!("core.draft.bot.name"),
        "harness" => steve_i18n::t!("core.draft.bot.harness"),
        "role" => steve_i18n::t!("core.draft.bot.role"),
        "persona" => steve_i18n::t!("core.draft.bot.persona"),
        "proactive" => steve_i18n::t!("core.draft.bot.proactive"),
        "rules" => steve_i18n::t!("core.draft.bot.rules"),
        "reasoning_effort" => steve_i18n::t!("core.draft.bot.reasoning_effort"),
        "accept_mode" => steve_i18n::t!("core.draft.bot.accept_mode"),
        "model" => steve_i18n::t!("core.draft.bot.model"),
        "perm_mode" => steve_i18n::t!("core.draft.bot.perm_mode"),
        "default_project" => steve_i18n::t!("core.draft.bot.default_project"),
        "enabled" => steve_i18n::t!("core.draft.bot.enabled"),
        "prompt" => steve_i18n::t!("core.draft.bot.prompt"),
        _ => return None,
    })
}

fn project_field_shown(key: &str) -> Option<String> {
    Some(match key {
        "name" => steve_i18n::t!("core.draft.project.name"),
        "description" => steve_i18n::t!("core.draft.project.description"),
        "default_bot" => steve_i18n::t!("core.draft.project.default_bot"),
        "worktree" => steve_i18n::t!("core.draft.project.worktree"),
        "archived" => steve_i18n::t!("core.draft.project.archived"),
        _ => return None,
    })
}

/// A setting's name as the UI shows it, in the request's language.
fn setting_label_shown(key: &str) -> Option<String> {
    Some(match key {
        "lang" => steve_i18n::t!("core.draft.setting.lang"),
        "home" => steve_i18n::t!("core.draft.setting.home"),
        "awayH" => steve_i18n::t!("core.draft.setting.awayH"),
        "tz" => steve_i18n::t!("core.draft.setting.tz"),
        "accDefault" => steve_i18n::t!("core.draft.setting.accDefault"),
        "depsAfter" => steve_i18n::t!("core.draft.setting.depsAfter"),
        "handoff" => steve_i18n::t!("core.draft.setting.handoff"),
        "archDays" => steve_i18n::t!("core.draft.setting.archDays"),
        "quiet" => steve_i18n::t!("core.draft.setting.quiet"),
        "fontSize" => steve_i18n::t!("core.draft.setting.fontSize"),
        "reduceMotion" => steve_i18n::t!("core.draft.setting.reduceMotion"),
        "keepSess" => steve_i18n::t!("core.draft.setting.keepSess"),
        "keepRaw" => steve_i18n::t!("core.draft.setting.keepRaw"),
        "keepMail" => steve_i18n::t!("core.draft.setting.keepMail"),
        _ => return None,
    })
}

/// The `models` target's field names as a card lists them.
fn models_field_shown(key: &str, harnesses: &[crate::harnesses::Harness]) -> Option<String> {
    use crate::models::{PURPOSES, purpose_label};
    if let Some(h) = key.strip_prefix("agent.") {
        return Some(steve_i18n::t!(
            "core.draft.models.agent_default_listed",
            harness = crate::harnesses::name_of(harnesses, h)
        ));
    }
    if let Some(p) = key.strip_suffix(".effort") {
        return Some(steve_i18n::t!(
            "core.draft.models.effort",
            purpose = purpose_label(p)
        ));
    }
    let p = key.strip_suffix(".harness").unwrap_or(key);
    PURPOSES
        .iter()
        .any(|x| x.key == p)
        .then(|| purpose_label(p))
}

/// A field's value as a card shows it, in the request's language: the fixed choices (and （空）) are
/// translated; names, free text and model ids stay as they are. `s` is [`show`]'s output.
fn value_shown(kind: &str, key: &str, s: String) -> String {
    if s == "（空）" {
        return steve_i18n::t!("core.draft.value.empty");
    }
    match (kind, key) {
        ("bot", "default_project") | ("project", "default_bot") if s == "无" => {
            steve_i18n::t!("core.draft.value.none")
        }
        ("bot", "proactive" | "reasoning_effort" | "accept_mode" | "perm_mode" | "enabled")
        | ("project", "worktree" | "archived")
        | ("settings", _) => choice_shown(s),
        ("models", k) if k.ends_with(".harness") || k.ends_with(".effort") => choice_shown(s),
        _ => s,
    }
}

/// A fixed choice (a setting value, an enum label) in the request's language; anything else as it is.
fn choice_shown(s: String) -> String {
    let n = |suffix: &str| {
        s.strip_suffix(suffix)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(str::to_string)
    };
    if let Some(n) = n(" 天") {
        return steve_i18n::t!("core.draft.value.days", n = n);
    }
    if let Some(n) = n(" 小时") {
        return steve_i18n::t!("core.draft.value.hours", n = n);
    }
    match s.as_str() {
        "（空）" => steve_i18n::t!("core.draft.value.empty"),
        "默认" => steve_i18n::t!("core.draft.value.default"),
        "无" => steve_i18n::t!("core.draft.value.none"),
        "启用" => steve_i18n::t!("core.draft.value.enabled"),
        "停用" => steve_i18n::t!("core.draft.value.disabled"),
        "开" => steve_i18n::t!("core.draft.value.on"),
        "关" => steve_i18n::t!("core.draft.value.off"),
        "使用中" => steve_i18n::t!("core.draft.value.in_use"),
        "已归档" => steve_i18n::t!("core.draft.value.archived"),
        "只在必要时" => steve_i18n::t!("core.draft.value.pro_low"),
        "适中" => steve_i18n::t!("core.draft.value.pro_mid"),
        "积极" => steve_i18n::t!("core.draft.value.pro_high"),
        "跟随全局" => steve_i18n::t!("core.draft.value.acc_inherit"),
        "需要你验收" => steve_i18n::t!("core.draft.value.acc_manual"),
        "自动验收" => steve_i18n::t!("core.draft.value.acc_auto"),
        "只读：只出方案，不改文件" => steve_i18n::t!("core.draft.value.perm_plan"),
        "每次改文件或运行命令都问你" => {
            steve_i18n::t!("core.draft.value.perm_default")
        }
        "可以改项目目录里的文件，运行命令要问你" => {
            steve_i18n::t!("core.draft.value.perm_accept_edits")
        }
        "可以改文件、运行命令；对外操作仍需你批准" => {
            steve_i18n::t!("core.draft.value.perm_auto")
        }
        "不再询问：工具直接运行，由系统沙箱兜底" => {
            steve_i18n::t!("core.draft.value.perm_bypass")
        }
        "跟随 Steve" => steve_i18n::t!("core.draft.value.follow"),
        "对话" => steve_i18n::t!("core.draft.value.home_chat"),
        "待处理" => steve_i18n::t!("core.draft.value.home_inbox"),
        "事项" => steve_i18n::t!("core.draft.value.home_items"),
        "自动开始" => steve_i18n::t!("core.draft.value.deps_auto"),
        "提醒我开始" => steve_i18n::t!("core.draft.value.deps_remind"),
        "关闭" => steve_i18n::t!("core.draft.value.closed"),
        "不发" => steve_i18n::t!("core.draft.value.no_send"),
        "标准" => steve_i18n::t!("core.draft.value.font_standard"),
        "大" => steve_i18n::t!("core.draft.value.font_large"),
        "永久" => steve_i18n::t!("core.draft.value.forever"),
        "不汇报" => steve_i18n::t!("core.draft.value.no_report"),
        "不自动归档" => steve_i18n::t!("core.draft.value.no_archive"),
        "不换会话" => steve_i18n::t!("core.draft.value.no_handoff"),
        "加载" => steve_i18n::t!("core.draft.value.load"),
        "不加载" => steve_i18n::t!("core.draft.value.no_load"),
        "1 年" => steve_i18n::t!("core.draft.value.one_year"),
        "最低" => steve_i18n::t!("core.draft.value.effort_minimal"),
        "低" => steve_i18n::t!("core.draft.value.effort_low"),
        "中" => steve_i18n::t!("core.draft.value.effort_medium"),
        "高" => steve_i18n::t!("core.draft.value.effort_high"),
        "很高" => steve_i18n::t!("core.draft.value.effort_xhigh"),
        "最高" => steve_i18n::t!("core.draft.value.effort_max"),
        "极致" => steve_i18n::t!("core.draft.value.effort_ultra"),
        _ => s,
    }
}

fn new_bot_title(name: &str) -> String {
    if name.is_empty() {
        steve_i18n::t!("core.draft.title.new_bot")
    } else {
        steve_i18n::t!("core.draft.title.new_bot_named", name = name)
    }
}

/// The choice of a purpose that has no override.
const FOLLOW: &str = "跟随 Steve";

/// A settings category's name, in the request's language.
fn cat_title(cat: &str) -> String {
    match cat {
        "general" => steve_i18n::t!("core.settings_cat.general"),
        "items" => steve_i18n::t!("core.settings_cat.items"),
        "notify" => steve_i18n::t!("core.settings_cat.notify"),
        "look" => steve_i18n::t!("core.settings_cat.look"),
        "data" => steve_i18n::t!("core.settings_cat.data"),
        _ => "undefined".into(),
    }
}

/// The `models` draft target's fields: per purpose who runs it (`route.harness`: follow Steve or a
/// harness), its list (`route`) and effort (`route.effort`), per harness the agent default
/// (`agent.codex`). Empty means the default.
fn model_fields(m: &crate::models::Models, ws: &str) -> Vec<FieldSpec> {
    use crate::models::PURPOSES;
    let mut fs = vec![];
    let steve = m.steve();
    let harnesses = m.harnesses();
    let label = |e: &String| crate::models::effort_label(e).map_or_else(|| e.clone(), String::from);
    let effort_labels: Vec<String> = std::iter::once("默认".to_string())
        .chain(harnesses.iter().flat_map(|h| h.efforts.iter().map(label)))
        .fold(vec![], |mut v, l| {
            if !v.contains(&l) {
                v.push(l);
            }
            v
        });
    for p in &PURPOSES {
        let c = m.custom(ws, p.key);
        fs.push(FieldSpec {
            key: format!("{}.harness", p.key),
            label: format!(
                "{}（{}）· 由谁来做：跟随 Steve（现在是 {}）或指定 harness",
                p.label,
                p.desc,
                crate::models::Choice {
                    harness: steve.runs_on.clone(),
                    models: steve.models.clone(),
                    effort: None,
                }
                .describe_with(&harnesses)
            ),
            ty: "enum".into(),
            values: Some(
                std::iter::once(FOLLOW.to_string())
                    .chain(harnesses.iter().map(|h| h.name.clone()))
                    .collect(),
            ),
            current: json!(
                c.as_ref()
                    .map_or(FOLLOW.to_string(), |c| m.harness_name(&c.harness))
            ),
        });
        let c = c.unwrap_or_default();
        fs.push(FieldSpec {
            key: p.key.into(),
            label: format!(
                "{} · 模型，按顺序逗号分隔，留空为该 harness 的默认",
                p.label
            ),
            ty: "text".into(),
            values: None,
            current: json!(c.models.join(",")),
        });
        fs.push(FieldSpec {
            key: format!("{}.effort", p.key),
            label: format!("{} · 推理强度", p.label),
            ty: "enum".into(),
            values: Some(effort_labels.clone()),
            current: json!(c.effort.as_ref().map_or_else(|| "默认".to_string(), label)),
        });
    }
    for h in &harnesses {
        fs.push(FieldSpec {
            key: format!("agent.{}", h.id),
            label: format!(
                "{} 新会话的默认模型（可选 {}，留空为默认）",
                h.name,
                h.model_ids().join("、")
            ),
            ty: "text".into(),
            values: None,
            current: json!(m.agent_model(ws, &h.id).unwrap_or_default()),
        });
    }
    fs
}

/// What the model said, as the changes shown and the settings keys to write (checked by
/// [`crate::models::validate`]; anything it rejects is left out).
fn model_patch(
    m: &crate::models::Models,
    ws: &str,
    said: &[(String, Value)],
) -> (Vec<Change>, Map<String, Value>) {
    use crate::models::{PURPOSES, validate};
    let (mut changes, mut patch) = (vec![], Map::new());
    let harnesses = m.harnesses();
    let get = |k: &str| said.iter().find(|(x, _)| x == k).map(|(_, v)| v);
    for p in &PURPOSES {
        let stored = m.custom(ws, p.key);
        let cur = stored.clone().unwrap_or_default();
        let hk = format!("{}.harness", p.key);
        let ek = format!("{}.effort", p.key);
        // who runs it: unchanged, back to Steve, or a harness by name or id
        let (harness, explicit_follow) = match get(&hk).map(jstr) {
            None => (stored.as_ref().map(|c| c.harness.clone()), false),
            Some(h) if h.trim().is_empty() || h.trim() == FOLLOW => (None, true),
            Some(h) => {
                let Some(found) = harnesses
                    .iter()
                    .find(|x| x.id == h.trim() || x.name == h.trim())
                    .map(|x| x.id.clone())
                else {
                    continue;
                };
                (Some(found), false)
            }
        };
        let switched_harness = harness.as_ref().is_some_and(|next_id| {
            stored
                .as_ref()
                .is_none_or(|c| c.harness.as_str() != next_id)
        });
        let models: Vec<String> = match get(p.key) {
            Some(v) => jstr(v)
                .split([',', '，', '、'])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect(),
            None if explicit_follow || switched_harness => Vec::new(),
            None => cur.models.clone(),
        };
        let h = harness.clone().unwrap_or_else(|| m.steve().runs_on);
        let target_h = harnesses.iter().find(|x| x.id == h);
        let effort = match get(&ek) {
            Some(v) => {
                let s = jstr(v);
                if s.trim().is_empty() || s == "默认" {
                    None
                } else {
                    target_h
                        .and_then(|x| {
                            x.efforts.iter().find(|e| {
                                crate::models::effort_label(e).unwrap_or(e.as_str()) == s
                                    || **e == s
                            })
                        })
                        .cloned()
                }
            }
            None if explicit_follow => None,
            None => cur
                .effort
                .clone()
                .filter(|e| target_h.is_none_or(|x| x.takes_effort(e))),
        };
        let follows = harness.is_none() && models.is_empty() && effort.is_none();
        let was = stored.is_none();
        if follows && was
            || !follows
                && stored.as_ref().is_some_and(|c| {
                    Some(&c.harness) == harness.as_ref() && c.models == models && c.effort == effort
                })
        {
            continue;
        }
        let body = if follows {
            json!({"purposes": {p.key: null}})
        } else {
            json!({"purposes": {p.key: {"harness": h, "models": models, "effort": effort}}})
        };
        let Ok(v) = validate(&body, &harnesses) else {
            continue;
        };
        let show_choice = |c: Option<&crate::models::Choice>| match c {
            None => steve_i18n::t!("core.draft.value.follow"),
            Some(c) => {
                let desc = c.describe_shown_with(&harnesses);
                match c
                    .effort
                    .as_deref()
                    .map(|e| crate::models::effort_shown(e).unwrap_or_else(|| e.to_string()))
                {
                    Some(e) => {
                        steve_i18n::t!("core.draft.models.choice_effort", choice = desc, effort = e)
                    }
                    None => desc,
                }
            }
        };
        let to = (!follows).then(|| crate::models::Choice {
            harness: h.clone(),
            models: models.clone(),
            effort: effort.clone(),
        });
        changes.push(Change {
            label: steve_i18n::t!(
                "core.draft.models.runner",
                purpose = crate::models::purpose_label(p.key)
            ),
            from: show_choice(stored.as_ref()),
            to: show_choice(to.as_ref()),
        });
        patch.extend(v);
    }
    for x in &harnesses {
        let (h, name) = (&x.id, &x.name);
        let Some(v) = get(&format!("agent.{h}")) else {
            continue;
        };
        let to = jstr(v).trim().to_string();
        let cur = m.agent_model(ws, h).unwrap_or_default();
        if to == cur {
            continue;
        }
        let Ok(v) = validate(&json!({"agents": {h.as_str(): to}}), &harnesses) else {
            continue;
        };
        let shown = |s: &str| {
            if s.is_empty() {
                steve_i18n::t!("core.draft.value.default")
            } else {
                show(&json!(s))
            }
        };
        changes.push(Change {
            label: steve_i18n::t!("core.draft.models.agent_default", harness = name),
            from: shown(&cur),
            to: shown(&to),
        });
        patch.extend(v);
    }
    (changes, patch)
}

/// A line of how a configuration is set now: the field and its value as the user sees it.
pub type Current = (String, String);

/// A field label without its notes (“免打扰（值：…）”, “模型（opus、sonnet…）” → the name only).
fn short_label(l: &str) -> String {
    l.split('（')
        .next()
        .unwrap_or(l)
        .split(" · 模型，")
        .next()
        .unwrap_or(l)
        .trim()
        .to_string()
}

/// A change line's label without the value table the model sees (“免打扰（值：…）” → “免打扰”).
fn plain_label(l: &str) -> String {
    match l.find("（值：") {
        Some(i) if l.ends_with('）') => l[..i].to_string(),
        _ => l.to_string(),
    }
}

/// The project fields a draft may change.
fn project_fields(p: &crate::repo::Project, bs: &[Bot]) -> Vec<FieldSpec> {
    vec![
        FieldSpec::new("name", "名称", "text", json!(p.name)),
        FieldSpec::new("description", "说明", "text", json!(p.description)),
        FieldSpec {
            key: "default_bot".into(),
            label: "默认 Bot".into(),
            ty: "enum".into(),
            values: Some(
                std::iter::once("无".to_string())
                    .chain(bs.iter().map(|b| b.name.clone()))
                    .collect(),
            ),
            current: json!(
                bs.iter()
                    .find(|b| Some(&b.id) == p.default_bot.as_ref())
                    .map(|b| b.name.as_str())
                    .unwrap_or("无")
            ),
        },
        FieldSpec {
            key: "worktree".into(),
            label: "每个事项单独开 worktree 分支".into(),
            ty: "enum".into(),
            values: Some(vec!["开".into(), "关".into()]),
            current: json!(if p.worktree { "开" } else { "关" }),
        },
        FieldSpec {
            key: "archived".into(),
            label: "状态（归档后不再出现在项目列表和分派选项里）".into(),
            ty: "enum".into(),
            values: Some(vec!["使用中".into(), "已归档".into()]),
            current: json!(if p.archived { "已归档" } else { "使用中" }),
        },
    ]
}

/// A setting's value as the user sees it (`archDays` “30” → “30 天”).
fn setting_shown(specs: &[SettingSpec], k: &str, v: &Value) -> Value {
    let s = jstr(v);
    let shown = specs
        .iter()
        .find(|spec| spec.key == k)
        .map(|spec| spec.label_of(&s))
        .unwrap_or(s.as_str());
    json!(shown)
}

/// What a configuration request in the conversation is about (see [`Core::pick_targets`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picked {
    /// at most [`MAX_TARGETS`], in the order the request names them
    pub targets: Vec<Target>,
    /// the request only asks how they are set now
    pub ask: bool,
}

/// One configuration a conversation request touched (see [`Core::config_request`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Drafted {
    pub title: String,
    pub href: String,
    /// The draft to apply, as `meta.proposal` carries it; `None` when there is nothing to change.
    pub proposal: Option<Value>,
    /// How it is set now.
    pub current: Vec<Current>,
}

/// Settings a request in the conversation changes at once, with an undo on its card: local, and put
/// back in one click. Retention (data is deleted) and the default acceptance still show a preview.
const AUTO_SETTINGS: [&str; 11] = [
    "lang",
    "home",
    "awayH",
    "tz",
    "depsAfter",
    "handoff",
    "archDays",
    "quiet",
    "fontSize",
    "reduceMotion",
    "item_prefix",
];
/// The module operations a request in the conversation applies at once: field edits of a contact,
/// an organization or a team. Creating, closing, adding and anything of GitHub, channels or
/// mail stays a preview.
const AUTO_MODULE_OPS: [&str; 3] = ["party.patch", "org.patch", "team.patch"];

/// A draft whose lines are all clear: nothing unrecognised, and not only text appended to a free
/// field (that is better read before it goes in).
fn clear_lines(p: &Value) -> bool {
    let lines: Vec<&Value> = p["changes"].as_array().into_iter().flatten().collect();
    !lines.is_empty()
        && !lines.iter().any(|l| {
            ["label", "from", "to"]
                .iter()
                .any(|k| l[*k].as_str().is_some_and(|s| s.contains("没认出")))
        })
        && !lines
            .iter()
            .all(|l| l["label"].as_str().is_some_and(|s| s.ends_with("（追加）")))
}

/// How many configurations one request drafts at most (“按这个方案补齐 Bot” can touch every bot).
const MAX_TARGETS: usize = 6;

/// The option line of a bot to make; `bot:new:<名字>` once per new bot.
const NEW_BOT_OPTION: &str = "bot:new:<名字> — 新建一个 Bot，名字写在冒号后，每个新 Bot 一项；已有职责相近的 Bot 就改它，不新建";

/// The draft targets steve-core owns itself; the rest belong to the modules.
const CORE_KINDS: [&str; 5] = ["bot", "project", "settings", "models", "tags"];

/// The option lines of the core targets after the bots and projects.
const CORE_OPTIONS: [&str; 7] = [
    "settings:general — 语言、启动页、离开期间汇报、时区",
    "settings:items — 默认验收方式（自动验收）、事项编号前缀、前置完成后、交接阈值、完成后自动归档",
    "settings:notify — 免打扰时段",
    "settings:look — 字号、动态效果",
    "settings:data — 会话记录、事件原文、邮件正文的保留期限",
    "models — Steve 自己做判断用的模型和推理强度，新会话的默认模型",
    "tags — 事项标签：新建、改名、改分类、改描述、删除",
];

impl Core {
    /// Draft a change of `target` from a request in plain words (`kind: global` lets the model pick the target).
    pub async fn draft(&self, ws: &str, target: Target, request: &str) -> Result<Proposal> {
        crate::stats::tagged("draft", ws, self.draft_untagged(ws, target, request)).await
    }

    /// The settings of category `cat` as draft fields, and their schema entries (for value labels).
    fn settings_fields(
        &self,
        ws: &str,
        cat: &str,
    ) -> Result<(&'static [SettingSpec], Vec<FieldSpec>)> {
        let db = self.store();
        let specs = ws_settings::category(cat)
            .ok_or_else(|| msg(steve_i18n::t!("core.settings.no_category")))?;
        let mut cur = setting_defaults();
        cur.extend(db.settings_all(ws)?);
        let w = db
            .workspace(ws)?
            .ok_or_else(|| msg(steve_i18n::t!("core.space.not_found")))?;
        let mut fs: Vec<FieldSpec> = specs
            .iter()
            .map(|s| {
                let labels = s.labels.as_ref().map(|l| {
                    format!(
                        "（值：{}）",
                        l.entries()
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("，")
                    )
                });
                FieldSpec {
                    label: format!("{}{}", s.label, labels.unwrap_or_default()),
                    ty: "enum".into(),
                    values: Some(s.values.clone()),
                    current: cur.get(&s.key).cloned().unwrap_or(Value::Null),
                    key: s.key.clone(),
                }
            })
            .collect();
        if cat == "items" {
            fs.push(FieldSpec::new(
                "item_prefix",
                "事项编号前缀（大写字母）",
                "text",
                json!(w.item_prefix),
            ));
        }
        Ok((specs, fs))
    }

    /// How a core `target` is set now, without a model call: its card title, link and current values.
    pub fn describe(&self, ws: &str, target: &Target) -> Result<(String, String, Vec<Current>)> {
        let db = self.store();
        let harnesses = self.models().harnesses();
        let tid = target.id.clone().unwrap_or_default();
        let listed = |kind: &str, fs: &[FieldSpec]| -> Vec<Current> {
            fs.iter()
                .map(|f| {
                    (
                        field_shown(kind, &f.key, false, &short_label(&f.label), &harnesses),
                        value_shown(kind, &f.key, show(&f.current)),
                    )
                })
                .collect()
        };
        Ok(match target.kind.as_str() {
            "bot" if new_bot_name(&tid).is_some() => (
                new_bot_title(new_bot_name(&tid).unwrap_or_default()),
                "#/bots".into(),
                vec![],
            ),
            "bot" => {
                let b = match db.bot(&tid)? {
                    Some(b) if db.bot_in_workspace(&b.id, ws)? => b,
                    _ => return Err(msg(steve_i18n::t!("core.bot.not_found"))),
                };
                let fs: Vec<FieldSpec> = bot_fields(&b, &db.projects(ws)?, &harnesses)
                    .into_iter()
                    .map(|f| f.spec)
                    .collect();
                (
                    steve_i18n::t!("core.draft.title.bot", bot = b.display()),
                    format!("#/bots/{}", b.id),
                    listed("bot", &fs),
                )
            }
            "project" => {
                let p = db
                    .project(Some(&tid))?
                    .filter(|p| p.workspace_id == ws)
                    .ok_or_else(|| msg(steve_i18n::t!("core.project.not_found")))?;
                let fs = project_fields(&p, &db.bots(ws)?);
                (
                    steve_i18n::t!("core.draft.title.project", name = p.name),
                    format!("#/projects/{}", p.id),
                    listed("project", &fs),
                )
            }
            "settings" => {
                let cat = target.id.clone().unwrap_or_else(|| "general".into());
                let (specs, fs) = self.settings_fields(ws, &cat)?;
                let cur = fs
                    .iter()
                    .map(|f| {
                        (
                            field_shown(
                                "settings",
                                &f.key,
                                false,
                                &short_label(&f.label),
                                &harnesses,
                            ),
                            value_shown(
                                "settings",
                                &f.key,
                                show(&setting_shown(specs, &f.key, &f.current)),
                            ),
                        )
                    })
                    .collect();
                (
                    steve_i18n::t!("core.draft.title.settings", cat = cat_title(&cat)),
                    format!("#/settings/{cat}"),
                    cur,
                )
            }
            "tags" => (
                steve_i18n::t!("core.draft.title.tags"),
                "#/settings/tags".into(),
                db.tags(ws)?
                    .into_iter()
                    .map(|t| {
                        let d = if t.description.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", js_head(t.description.trim(), 40))
                        };
                        (t.name, format!("{}{d}", t.cat))
                    })
                    .collect(),
            ),
            "models" => (
                steve_i18n::t!("core.draft.title.models"),
                "#/settings/models".into(),
                listed(
                    "models",
                    &model_fields(self.models(), ws)
                        .into_iter()
                        .filter(|f| !f.current.as_str().is_some_and(str::is_empty))
                        .collect::<Vec<_>>(),
                ),
            ),
            _ => return Err(msg(steve_i18n::t!("core.settings.not_found"))),
        })
    }

    /// Draft a new version of one of a bot's long texts (`field`: `rules`, its working rules, or
    /// `prompt`, a system prompt of its own) from a request in plain words. `base` is the text the
    /// request is about (what the editor shows, saved or not; empty to write one from nothing),
    /// else the saved one. The proposal carries the whole new text in its patch.
    pub async fn draft_bot_text(
        &self,
        ws: &str,
        id: &str,
        field: &str,
        base: Option<&str>,
        request: &str,
    ) -> Result<Proposal> {
        crate::stats::tagged("draft", ws, async {
            let db = self.store();
            let b = match db.bot(id)? {
                Some(b) if db.bot_in_workspace(&b.id, ws)? => b,
                _ => return Err(msg(steve_i18n::t!("core.bot.not_found"))),
            };
            let (saved, label) = match field {
                "rules" => (b.rules.clone(), "工作规范（附在自动生成的系统提示词里）"),
                "prompt" => (
                    b.prompt.clone().unwrap_or_default(),
                    "系统提示词（自己写的全文，替换自动生成的那份；记忆和安全规则另外附上）",
                ),
                _ => return Err(msg(steve_i18n::t!("core.settings.not_found"))),
            };
            let cur = base.map(str::to_string).unwrap_or(saved);
            let spec = FieldSpec::new(field, label, "text", json!(cur));
            let what = format!(
                "the bot \"{}\" (role: {}; way of speaking: {})",
                b.name,
                if b.role.trim().is_empty() {
                    "-"
                } else {
                    b.role.trim()
                },
                if b.persona.trim().is_empty() {
                    "-"
                } else {
                    b.persona.trim()
                },
            );
            let said = draft_fields(self.gw(), &what, std::slice::from_ref(&spec), request).await?;
            let new = said
                .into_iter()
                .find(|(k, _)| k == field)
                .map(|(_, v)| jstr(&v).trim().to_string())
                .unwrap_or_default();
            let (mut changes, mut patch) = (vec![], Map::new());
            if !new.is_empty() && new != cur.trim() {
                changes.push(Change {
                    label: field_shown("bot", field, true, label, &[]),
                    from: value_shown("bot", field, show(&json!(cur))),
                    to: value_shown("bot", field, show(&json!(new))),
                });
                patch.insert(field.into(), json!(new));
            }
            let target = Target {
                kind: "bot".into(),
                id: Some(b.id.clone()),
            };
            self.store_core_draft(
                ws,
                &target,
                steve_i18n::t!("core.draft.title.bot", bot = b.display()),
                format!("#/bots/{}", b.id),
                changes,
                patch,
                request,
            )
        })
        .await
    }

    async fn draft_untagged(&self, ws: &str, target: Target, request: &str) -> Result<Proposal> {
        let target = if target.kind == "global" {
            self.pick_targets(ws, request, None)
                .await?
                .targets
                .into_iter()
                .next()
                .unwrap_or(Target {
                    kind: "settings".into(),
                    id: Some("general".into()),
                })
        } else {
            target
        };
        let (title, href, _) = self.describe(ws, &target)?;
        let db = self.store();
        let gw = self.gw();
        let harnesses = self.models().harnesses();
        let (mut changes, mut patch) = (vec![], Map::new());
        match target.kind.as_str() {
            "bot" => {
                let id = target.id.clone().unwrap_or_default();
                let fresh = new_bot_name(&id);
                let b = match fresh {
                    Some(n) => blank_bot(ws, n, self.models()),
                    None => db
                        .bot(&id)?
                        .ok_or_else(|| msg(steve_i18n::t!("core.bot.not_found")))?,
                };
                let mut fs = bot_fields(&b, &db.projects(ws)?, &harnesses);
                /* a new bot starts enabled */
                if fresh.is_some() {
                    fs.retain(|f| f.spec.key != "enabled");
                }
                let specs: Vec<FieldSpec> = fs.iter().map(|f| f.spec.clone()).collect();
                let bv = serde_json::to_value(&b)?;
                let what = match fresh {
                    Some("") => "a new bot (it does not exist yet; fill in only what the request says about this one bot)".to_string(),
                    Some(n) => format!("a new bot \"{n}\" (it does not exist yet; fill in only what the request says about this one bot)"),
                    None => format!("the bot \"{}\"", b.name),
                };
                let mut raw_patch = Map::new();
                for (k, value) in draft_fields(gw, &what, &specs, request).await? {
                    let Some(f) = fs.iter().find(|f| f.spec.key == k) else {
                        continue;
                    };
                    let v = f.enc.as_ref().map(|e| e(&value)).unwrap_or(value);
                    if bv.get(&k).unwrap_or(&Value::Null) == &v {
                        continue;
                    }
                    raw_patch.insert(k, v);
                }
                sanitize_bot_for_harness(&mut raw_patch, fresh.is_none().then_some(&b), &harnesses);
                for (k, v) in raw_patch {
                    if bv.get(&k).unwrap_or(&Value::Null) == &v {
                        continue;
                    }
                    let Some(f) = fs.iter().find(|f| f.spec.key == k) else {
                        patch.insert(k, v);
                        continue;
                    };
                    let to = f.dec.as_ref().map(|d| d(&v)).unwrap_or_else(|| v.clone());
                    changes.push(Change {
                        label: field_shown("bot", &k, true, &f.spec.label, &harnesses),
                        from: if fresh.is_some() {
                            "—".into()
                        } else {
                            value_shown("bot", &k, show(&f.spec.current))
                        },
                        to: value_shown("bot", &k, show(&to)),
                    });
                    patch.insert(k, v);
                }
                /* a new bot has a name of its own: the one drafted, else the one its target carries */
                if let Some(n) = fresh {
                    let name = patch
                        .get("name")
                        .map(jstr)
                        .unwrap_or_else(|| n.to_string())
                        .trim()
                        .to_string();
                    if name.is_empty() {
                        return Err(msg(steve_i18n::t!("core.bot.new_needs_name")));
                    }
                    if db.bots(ws)?.iter().any(|x| x.name == name) {
                        return Err(msg(steve_i18n::t!("core.bot.name_taken", name = name)));
                    }
                    if !patch.contains_key("name") {
                        changes.insert(
                            0,
                            Change {
                                label: field_shown("bot", "name", true, "名字", &harnesses),
                                from: "—".into(),
                                to: name.clone(),
                            },
                        );
                    }
                    patch.insert("name".into(), json!(name));
                }
            }
            "project" => {
                let id = target.id.clone().unwrap_or_default();
                let p = db
                    .project(Some(&id))?
                    .ok_or_else(|| msg(steve_i18n::t!("core.project.not_found")))?;
                let bs = db.bots(ws)?;
                let fs = project_fields(&p, &bs);
                let pv = serde_json::to_value(&p)?;
                for (k, value) in
                    draft_fields(gw, &format!("the project \"{}\"", p.name), &fs, request).await?
                {
                    let Some(f) = fs.iter().find(|f| f.key == k) else {
                        continue;
                    };
                    let v = match k.as_str() {
                        "default_bot" => bs
                            .iter()
                            .find(|b| value.as_str() == Some(&b.name))
                            .map(|b| json!(b.id))
                            .unwrap_or(Value::Null),
                        "worktree" => json!(value.as_str() == Some("开")),
                        "archived" => match value.as_str() {
                            Some("已归档") => json!(true),
                            Some("使用中") => json!(false),
                            _ => continue,
                        },
                        _ => value.clone(),
                    };
                    if pv.get(&k).unwrap_or(&Value::Null) == &v {
                        continue;
                    }
                    changes.push(Change {
                        label: field_shown(
                            "project",
                            &k,
                            false,
                            &short_label(&f.label),
                            &harnesses,
                        ),
                        from: value_shown("project", &k, show(&f.current)),
                        to: value_shown("project", &k, show(&value)),
                    });
                    patch.insert(k, v);
                }
            }
            "settings" => {
                let cat = target.id.clone().unwrap_or_else(|| "general".into());
                let (specs, fs) = self.settings_fields(ws, &cat)?;
                for (k, value) in
                    draft_fields(gw, &format!("Steve settings · {cat}"), &fs, request).await?
                {
                    let Some(f) = fs.iter().find(|f| f.key == k) else {
                        continue;
                    };
                    let v = if k == "item_prefix" {
                        let up: String = jstr(&value)
                            .to_uppercase()
                            .chars()
                            .filter(|c| c.is_ascii_uppercase())
                            .collect();
                        js_head(&up, 6).to_string()
                    } else {
                        jstr(&value)
                    };
                    if f.values.as_ref().is_some_and(|vs| !vs.contains(&v)) {
                        continue;
                    }
                    if v.is_empty() || v == jstr(&f.current) {
                        continue;
                    }
                    changes.push(Change {
                        label: field_shown(
                            "settings",
                            &k,
                            true,
                            &plain_label(&f.label),
                            &harnesses,
                        ),
                        from: value_shown(
                            "settings",
                            &k,
                            show(&setting_shown(specs, &k, &f.current)),
                        ),
                        to: value_shown("settings", &k, show(&setting_shown(specs, &k, &json!(v)))),
                    });
                    patch.insert(k, json!(v));
                }
            }
            "tags" => (changes, patch) = self.draft_tags(ws, request).await?,
            "models" => {
                let fs = model_fields(self.models(), ws);
                let said = draft_fields(gw, "Steve settings · models", &fs, request).await?;
                (changes, patch) = model_patch(self.models(), ws, &said);
            }
            _ => {}
        }
        self.store_core_draft(ws, &target, title, href, changes, patch, request)
    }

    /// The tags draft: new tags, and existing ones renamed, moved to another category, described anew or
    /// removed. Only a tag nothing uses is removed (its undo makes it again); one in use stays, and the
    /// preview says why.
    async fn draft_tags(
        &self,
        ws: &str,
        request: &str,
    ) -> Result<(Vec<Change>, Map<String, Value>)> {
        let db = self.store();
        let ts = db.tags(ws)?;
        let mut lines = vec![
            "Manage item tags in Steve. Existing tags (name · category · description):".to_string(),
        ];
        lines.extend(
            ts.iter()
                .map(|t| format!("  {} · {} · {}", t.name, t.cat, t.description)),
        );
        lines.push(format!("Categories: {}.", TAG_CATS.join("/")));
        lines.push("Change only what the request asks for. In \"edit\", give only the keys that change: newName renames, cat moves it to another category, description replaces it. \"remove\" lists existing tag names to delete.".into());
        lines.push("Reply with ONLY JSON: {\"create\":[{\"name\":\"\",\"cat\":\"\",\"description\":\"\"}],\"edit\":[{\"name\":\"<existing name>\",\"newName\":\"\",\"cat\":\"\",\"description\":\"\"}],\"remove\":[\"<existing name>\"]}".into());
        let prompt = crate::gateway::with_static(
            &lines.join("\n"),
            &format!("Request: {}", serde_json::to_string(request)?),
        );
        let r = first_json(
            &self
                .gw()
                .print(&prompt, Tier::Smart, Duration::from_secs(90))
                .await?,
        )?
        .unwrap_or(json!({}));
        let arr = |k: &str| {
            r.get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let named = |v: &Value| -> Option<&crate::repo::Tag> {
            let n = v.as_str()?;
            ts.iter().find(|x| x.name == n)
        };
        let (mut changes, mut patch) = (vec![], Map::new());
        let create: Vec<Value> = arr("create")
            .into_iter()
            .filter(|t| {
                t.get("name").is_some_and(truthy)
                    && t.get("cat")
                        .and_then(Value::as_str)
                        .is_some_and(|c| TAG_CATS.contains(&c))
                    && t.get("name").and_then(named).is_none()
            })
            .collect();
        for t in &create {
            changes.push(Change {
                label: steve_i18n::t!("core.draft.tag.create", cat = jstr(&t["cat"])),
                from: "—".into(),
                to: steve_i18n::t!(
                    "core.draft.tag.create_to",
                    name = jstr(&t["name"]),
                    description = t
                        .get("description")
                        .map(jstr)
                        .unwrap_or_else(|| "undefined".into())
                ),
            });
        }
        let mut edit: Vec<Value> = vec![];
        for e in arr("edit") {
            let Some(t) = e.get("name").and_then(named) else {
                continue;
            };
            let mut keep = Map::new();
            keep.insert("name".into(), json!(t.name));
            let new_name = e
                .get("newName")
                .filter(|v| truthy(v))
                .map(|v| jstr(v).trim().to_string())
                .filter(|n| *n != t.name && !ts.iter().any(|x| x.name == *n));
            if let Some(n) = new_name {
                changes.push(Change {
                    label: steve_i18n::t!("core.draft.tag.rename", name = t.name),
                    from: t.name.clone(),
                    to: n.clone(),
                });
                keep.insert("newName".into(), json!(n));
            }
            let cat = e
                .get("cat")
                .and_then(Value::as_str)
                .filter(|c| TAG_CATS.contains(c) && *c != t.cat);
            if let Some(c) = cat {
                changes.push(Change {
                    label: steve_i18n::t!("core.draft.tag.cat", name = t.name),
                    from: t.cat.clone(),
                    to: c.into(),
                });
                keep.insert("cat".into(), json!(c));
            }
            let d = e
                .get("description")
                .filter(|v| truthy(v))
                .map(jstr)
                .filter(|d| *d != t.description);
            if let Some(d) = d {
                changes.push(Change {
                    label: steve_i18n::t!("core.draft.tag.description", name = t.name),
                    from: value_shown("tags", "description", show(&json!(t.description))),
                    to: value_shown("tags", "description", show(&json!(d))),
                });
                keep.insert("description".into(), json!(d));
            }
            if keep.len() > 1 {
                edit.push(Value::Object(keep));
            }
        }
        let mut remove: Vec<Value> = vec![];
        for n in arr("remove") {
            let Some(t) = named(&n) else { continue };
            if remove.contains(&json!(t.name)) {
                continue;
            }
            let used = db.tag_used(&t.id)?;
            if used > 0 {
                changes.push(Change {
                    label: steve_i18n::t!("core.draft.tag.remove_named", name = t.name),
                    from: t.name.clone(),
                    to: steve_i18n::t!("core.draft.tag.in_use", used = used),
                });
                continue;
            }
            changes.push(Change {
                label: steve_i18n::t!("core.draft.tag.remove"),
                from: t.name.clone(),
                to: "—".into(),
            });
            remove.push(json!(t.name));
        }
        /* a line that changes nothing (a tag in use) is not a change to apply */
        if create.is_empty() && edit.is_empty() && remove.is_empty() {
            changes.clear();
        }
        patch.insert("create".into(), json!(create));
        patch.insert("edit".into(), json!(edit));
        patch.insert("remove".into(), json!(remove));
        Ok((changes, patch))
    }

    /// Which configurations a request is about (at most three) and whether it only asks how they are
    /// set. `who` (the user who said it) adds the modules' targets — schedules, flows, knowledge
    /// sources, mailboxes, repositories, channels… — to the options.
    pub async fn pick_targets(&self, ws: &str, request: &str, who: Option<&str>) -> Result<Picked> {
        let db = self.store();
        let mut opts: Vec<String> = db
            .bots(ws)?
            .iter()
            .map(|b| {
                format!(
                    "bot:{} — {}（{}）{}",
                    b.id,
                    b.name,
                    b.role,
                    if b.enabled { "" } else { "·已停用" }
                )
            })
            .collect();
        opts.push(NEW_BOT_OPTION.into());
        opts.extend(db.store().projects().list(ws, true)?.iter().map(|p| {
            format!(
                "project:{} — 项目 {}{}",
                p.id,
                p.name,
                if p.archived { "（已归档）" } else { "" }
            )
        }));
        opts.extend(CORE_OPTIONS.map(String::from));
        let modules = self.modules().zip(who);
        let mut module_kinds: Vec<String> = vec![];
        if let Some((m, me)) = modules {
            let caller = steve_modules::Caller {
                ws: ws.into(),
                me: me.into(),
            };
            module_kinds = m
                .registry
                .target_kinds()
                .iter()
                .map(|k| k.to_string())
                .collect();
            match steve_modules::changes::candidates(m, &caller) {
                Ok(cs) => opts.extend(
                    cs.iter()
                        /* people only when the request names them: there can be hundreds */
                        .filter(|c| {
                            !c.code.starts_with("contact:")
                                || request.contains(c.label.split('（').next().unwrap_or(&c.label))
                        })
                        .take(80)
                        .map(|c| c.line()),
                ),
                Err(e) => tracing::warn!("config candidates of {ws}: {e:?}"),
            }
        }
        let prompt = crate::gateway::with_static(
            &format!(
                "Which Steve configurations does this request change or ask about? Options:\n{}\nReply with ONLY JSON {{\"targets\":[\"<option before the dash>\"],\"ask\":false}}: every option the request changes, in its order (usually one, at most {MAX_TARGETS}). \"ask\": true when the request only asks how they are set now and changes nothing.",
                opts.join("\n")
            ),
            &format!("Request: {}", serde_json::to_string(request)?),
        );
        let raw = self
            .gw()
            .print(&prompt, Tier::Fast, Duration::from_secs(60))
            .await
            .unwrap_or_else(|_| "{}".into());
        let r = first_json(&raw)?.unwrap_or(json!({}));
        let named: Vec<String> = match (r.get("targets"), r.get("target")) {
            (Some(Value::Array(a)), _) => a.iter().map(jstr).collect(),
            (_, Some(v)) if !v.is_null() => vec![jstr(v)],
            _ => vec![],
        };
        let mut targets: Vec<Target> = vec![];
        for t in named {
            let t = t.trim();
            let (kind, id) = match t.split_once(':') {
                Some((k, i)) => (k.trim().to_string(), Some(i.trim().to_string())),
                None => (t.to_string(), None),
            };
            let target = match kind.as_str() {
                "tags" | "models" => Target { kind, id: None },
                "settings" => Target {
                    id: Some(id.unwrap_or_else(|| "general".into())),
                    kind,
                },
                k if CORE_KINDS.contains(&k) || module_kinds.iter().any(|m| m == k) => {
                    Target { kind, id }
                }
                _ => continue,
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        targets.truncate(MAX_TARGETS);
        if targets.is_empty() {
            targets.push(Target {
                kind: "settings".into(),
                id: Some("general".into()),
            });
        }
        Ok(Picked {
            targets,
            ask: r.get("ask").is_some_and(truthy),
        })
    }

    /// A configuration request said in the conversation (router kind `config`): every configuration it
    /// touches is drafted — the core's and, with `who`, the modules' — or, when it only asks, shown as it
    /// is now. A target that fails is left out; when all fail, the first error.
    pub async fn config_request(
        &self,
        ws: &str,
        request: &str,
        who: Option<&str>,
    ) -> Result<(bool, Vec<Drafted>)> {
        let picked = self.pick_targets(ws, request, who).await?;
        let mut out = vec![];
        let mut first_err = None;
        for t in &picked.targets {
            let r = if CORE_KINDS.contains(&t.kind.as_str()) {
                self.core_drafted(ws, t, request, picked.ask).await
            } else {
                self.module_drafted(ws, t, request, who, picked.ask).await
            };
            match r {
                Ok(d) => out.push(d),
                Err(e) => {
                    tracing::warn!("config draft {}:{:?} in {ws}: {e}", t.kind, t.id);
                    first_err.get_or_insert(e);
                }
            }
        }
        match (out.is_empty(), first_err) {
            (true, Some(e)) => Err(e),
            _ => Ok((picked.ask, out)),
        }
    }

    async fn core_drafted(
        &self,
        ws: &str,
        t: &Target,
        request: &str,
        ask: bool,
    ) -> Result<Drafted> {
        if ask {
            let (title, href, current) = self.describe(ws, t)?;
            return Ok(Drafted {
                title,
                href,
                proposal: None,
                current,
            });
        }
        let p = self.draft(ws, t.clone(), request).await?;
        if p.changes.is_empty() {
            let _ = self.drop_draft(&p.id, ws);
            let (title, href, current) = self.describe(ws, t)?;
            return Ok(Drafted {
                title,
                href,
                proposal: None,
                current,
            });
        }
        Ok(Drafted {
            title: p.title.clone(),
            href: p.href.clone(),
            proposal: Some(serde_json::to_value(&p)?),
            current: vec![],
        })
    }

    async fn module_drafted(
        &self,
        ws: &str,
        t: &Target,
        request: &str,
        who: Option<&str>,
        ask: bool,
    ) -> Result<Drafted> {
        let (Some(m), Some(me)) = (self.modules(), who) else {
            return Err(msg(steve_i18n::t!("core.settings.not_found")));
        };
        let caller = steve_modules::Caller {
            ws: ws.into(),
            me: me.into(),
        };
        let module = |e: steve_modules::ApiError| msg(format!("{e:?}"));
        let now_set = |p: &steve_modules::changes::Prepared| -> Vec<Current> {
            p.fields
                .iter()
                .map(|f| (short_label(&f.label), show(&f.current)))
                .collect()
        };
        if !ask {
            let p = steve_modules::changes::draft(
                m,
                &caller,
                &t.kind,
                t.id.as_deref(),
                request,
                Default::default(),
            )
            .await
            .map_err(module)?;
            if !p.changes.is_empty() {
                return Ok(Drafted {
                    title: p.title.clone(),
                    href: p.href.clone(),
                    proposal: Some(serde_json::to_value(&p)?),
                    current: vec![],
                });
            }
            /* nothing to change: no draft left behind */
            let _ = steve_modules::changes::drop_draft(m, &caller, &p.id);
        }
        let p = steve_modules::changes::prepared(m, &caller, &t.kind, t.id.as_deref())
            .await
            .map_err(module)?;
        Ok(Drafted {
            current: now_set(&p),
            title: p.title,
            href: p.href,
            proposal: None,
        })
    }

    /// Drafts of a request said in the conversation that touch only L0 configuration — the settings
    /// in [`AUTO_SETTINGS`], field edits of contacts, organizations and teams — apply at once; their
    /// card says so and offers the undo. Returns the ids applied; the rest stay cards to apply.
    pub async fn config_auto_apply(
        &self,
        ws: &str,
        who: Option<&str>,
        ds: &[Drafted],
    ) -> Vec<String> {
        let m = self.changes_modules();
        let mut done = vec![];
        for p in ds.iter().filter_map(|d| d.proposal.as_ref()) {
            let (Some(id), Some(kind)) = (p["id"].as_str(), p["target"]["kind"].as_str()) else {
                continue;
            };
            if !clear_lines(p) {
                continue;
            }
            if !CORE_KINDS.contains(&kind) && who.is_none() {
                continue;
            }
            let caller = steve_modules::Caller {
                ws: ws.into(),
                me: who.unwrap_or("me").into(),
            };
            let cid = id.to_string();
            let ok = m
                .blocking(move |m| {
                    let Some(cs) = m
                        .store
                        .read(|r| steve_store::base::changesets::ChangeSets(r).get(&cid))?
                    else {
                        return Ok(false);
                    };
                    if cs.ops.is_empty() {
                        return Ok(false);
                    }
                    let auto_settings = cs.ops.iter().all(|op| {
                        op.get("op").and_then(Value::as_str) == Some(OP_SETTINGS_PATCH)
                            && op.get("patch").and_then(Value::as_object).is_some_and(|m| {
                                !m.is_empty()
                                    && m.keys().all(|k| AUTO_SETTINGS.contains(&k.as_str()))
                            })
                    });
                    let auto_module = cs.ops.iter().all(|op| {
                        op.get("op")
                            .and_then(Value::as_str)
                            .is_some_and(|n| AUTO_MODULE_OPS.contains(&n))
                    });
                    if !auto_settings && !auto_module {
                        return Ok(false);
                    }
                    steve_modules::changes::apply(m, &caller, &cid).map(|_| true)
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("config auto-apply {kind}: {e}");
                    false
                });
            if ok {
                done.push(id.to_string());
            }
        }
        done
    }

    /// Registers core's conversational configuration operations (`core.*`) into `steve-modules`'s
    /// single changeset registry so every target (core and modules alike) shares one persisted
    /// SQLite `changesets` engine.
    pub fn register_change_ops(&self, reg: &mut steve_modules::Registry) {
        reg.op(OP_BOT_CREATE, Box::new(CoreBotCreate(self.clone())))
            .op(OP_BOT_PATCH, Box::new(CoreBotPatch(self.clone())))
            .op(OP_PROJECT_PATCH, Box::new(CoreProjectPatch(self.clone())))
            .op(OP_SETTINGS_PATCH, Box::new(CoreSettingsPatch(self.clone())))
            .op(OP_TAGS_APPLY, Box::new(CoreTagsApply(self.clone())))
            .op(OP_MODELS_PATCH, Box::new(CoreModelsPatch(self.clone())));
    }

    fn changes_modules(&self) -> &steve_modules::Modules {
        self.0.modules.get_or_init(|| {
            let mut reg = steve_modules::Registry::default();
            self.register_change_ops(&mut reg);
            steve_modules::Modules::new(self.store().store().clone(), self.module_gateway(), reg)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn store_core_draft(
        &self,
        ws: &str,
        target: &Target,
        title: String,
        href: String,
        changes: Vec<Change>,
        patch: Map<String, Value>,
        request: &str,
    ) -> Result<Proposal> {
        let m = self.changes_modules();
        let caller = steve_modules::Caller {
            ws: ws.into(),
            me: "me".into(),
        };
        let tid = target.id.clone().unwrap_or_default();
        let ops = if changes.is_empty() {
            vec![]
        } else {
            match target.kind.as_str() {
                "bot" if new_bot_name(&tid).is_some() => {
                    vec![json!({ "op": OP_BOT_CREATE, "patch": Value::Object(patch.clone()) })]
                }
                "bot" => vec![
                    json!({ "op": OP_BOT_PATCH, "id": tid, "patch": Value::Object(patch.clone()) }),
                ],
                "project" => vec![
                    json!({ "op": OP_PROJECT_PATCH, "id": tid, "patch": Value::Object(patch.clone()) }),
                ],
                "settings" => {
                    vec![json!({ "op": OP_SETTINGS_PATCH, "patch": Value::Object(patch.clone()) })]
                }
                "tags" => {
                    vec![json!({ "op": OP_TAGS_APPLY, "patch": Value::Object(patch.clone()) })]
                }
                "models" => {
                    vec![json!({ "op": OP_MODELS_PATCH, "patch": Value::Object(patch.clone()) })]
                }
                _ => vec![],
            }
        };
        let prepared = Prepared {
            target_id: target.id.clone(),
            revision: None,
            title,
            href,
            subject: String::new(),
            fields: vec![],
            ops: vec![],
            free_text: None,
            context: Value::Null,
        };
        let p = steve_modules::changes::store_plan_with_request(
            m,
            &caller,
            &target.kind,
            &prepared,
            Plan { ops, changes },
            Origin::default(),
            request,
        )
        .map_err(|e| msg(e.to_string()))?;
        Ok(p.with_patch(patch))
    }

    /// Apply a stored draft of this workspace; returns `{ok: true, undo: id, ...}`.
    pub fn apply(&self, id: &str, actor: &str, ws: &str) -> Result<Value> {
        let m = self.changes_modules();
        let caller = steve_modules::Caller {
            ws: ws.into(),
            me: actor.into(),
        };
        let r = steve_modules::changes::apply(m, &caller, id).map_err(|e| msg(e.to_string()))?;
        self.mark_proposal(ws, id, Some("applied"));
        Ok(r)
    }

    /// Throws a stored draft of this workspace away; its conversation card says so.
    pub fn drop_draft(&self, id: &str, ws: &str) -> Result<Value> {
        let m = self.changes_modules();
        let caller = steve_modules::Caller {
            ws: ws.into(),
            me: "me".into(),
        };
        let r =
            steve_modules::changes::drop_draft(m, &caller, id).map_err(|e| msg(e.to_string()))?;
        self.mark_proposal(ws, id, Some("dropped"));
        Ok(r)
    }

    /// The conversation card of draft `id` is now `state` (`applied`, `dropped`; `None`: a draft again
    /// after an undo), kept in its message's `meta.cfg_state` so it shows so after a reload. Draft ids
    /// are persisted changeset ids (`chs_…`); a draft made outside the conversation has no card.
    pub fn mark_proposal(&self, ws: &str, id: &str, state: Option<&str>) {
        let db = self.store();
        let holds = |meta: &Value| {
            meta.get("proposal").and_then(|p| p.get("id")) == Some(&json!(id))
                || meta
                    .get("proposals")
                    .and_then(Value::as_array)
                    .is_some_and(|ps| ps.iter().any(|p| p.get("id") == Some(&json!(id))))
        };
        let found = match db.messages(ws) {
            Ok(ms) => ms.into_iter().rev().find(|m| holds(&m.meta)),
            Err(e) => {
                tracing::warn!("proposal {id}: {e}");
                return;
            }
        };
        let Some(m) = found else { return };
        let mut meta = m.meta.clone();
        let Some(o) = meta.as_object_mut() else {
            return;
        };
        let states = o.entry("cfg_state").or_insert_with(|| json!({}));
        if let Some(s) = states.as_object_mut() {
            match state {
                Some(st) => {
                    s.insert(id.into(), json!(st));
                }
                None => {
                    s.remove(id);
                }
            }
        }
        if let Err(e) = db.message_patch_meta(&m.id, &meta) {
            tracing::warn!("proposal {id} on {}: {e}", m.id);
        }
    }

    /// Undo an applied draft of this workspace.
    pub fn undo(&self, id: &str, actor: &str, ws: &str) -> Result<Value> {
        let m = self.changes_modules();
        let caller = steve_modules::Caller {
            ws: ws.into(),
            me: actor.into(),
        };
        let r = steve_modules::changes::undo(m, &caller, id).map_err(|e| msg(e.to_string()))?;
        self.mark_proposal(ws, id, None);
        Ok(r)
    }
}

fn core_err(e: impl std::fmt::Display) -> ApiError {
    ApiError::rule(e.to_string())
}

fn patch_map(op: &Value) -> ApiResult<&Map<String, Value>> {
    op.get("patch")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::invalid("patch"))
}

fn op_id(op: &Value) -> ApiResult<&str> {
    op.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("id"))
}

fn before_after(before: &Value) -> (&Map<String, Value>, &Map<String, Value>) {
    static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
    (
        before
            .get("before")
            .and_then(Value::as_object)
            .unwrap_or(&EMPTY),
        before
            .get("after")
            .and_then(Value::as_object)
            .unwrap_or(&EMPTY),
    )
}

struct CoreBotCreate(Core);
impl ChangeOp for CoreBotCreate {
    fn apply(&self, tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let mut nb = patch_map(op)?.clone();
        let (def_harness, def_perm) = self.0.models().default_bot();
        let h = nb
            .get("harness")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| def_harness.clone());
        nb.insert("harness".into(), json!(h));
        if !nb.contains_key("perm_mode") {
            let pm = if h == def_harness {
                def_perm
            } else {
                self.0.models().default_perm_mode_for(&h)
            };
            nb.insert("perm_mode".into(), json!(pm));
        }
        self.0
            .models()
            .validate_bot_patch(None, &nb)
            .map_err(ApiError::rule)?;
        let new_bot: steve_store::NewBot =
            serde_json::from_value(Value::Object(nb)).map_err(core_err)?;
        let b = steve_modules::contacts::stewards::create_bot(tx, cx.ws, &new_bot, cx.actor)?;
        cx.set_result("bot", json!(b.id));
        Ok(json!({ "created": b.id }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let made = before.get("created").map(jstr).unwrap_or_default();
        if made.is_empty() {
            return Ok(());
        }
        let main = self.0.store().main_bot().map_err(core_err)?.id;
        self.0
            .delete_bot(&made, &main, cx.actor)
            .map_err(core_err)?;
        Ok(())
    }
}

struct CoreBotPatch(Core);
impl ChangeOp for CoreBotPatch {
    fn apply(&self, _tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let tid = op_id(op)?;
        let patch = patch_map(op)?;
        let db = self.0.store();
        let cur_bot = db
            .bot(tid)
            .map_err(core_err)?
            .ok_or_else(|| ApiError::rule(steve_i18n::t!("core.bot.not_found")))?;
        let b = serde_json::to_value(&cur_bot).map_err(core_err)?;
        let hs = self.0.models().harnesses();
        self.0
            .models()
            .validate_bot_patch(b.as_object(), patch)
            .map_err(ApiError::rule)?;
        let mut bp = patch.clone();
        crate::models::reconcile_bot_perm_mode(b.as_object(), &mut bp, &hs)
            .map_err(ApiError::invalid)?;
        let mut before = Map::new();
        for k in bp.keys() {
            before.insert(k.clone(), b.get(k).cloned().unwrap_or(Value::Null));
        }
        db.bot_patch(tid, &bp, cx.actor).map_err(core_err)?;
        let mut after = Map::new();
        if let Some(b) = db.bot(tid).map_err(core_err)? {
            let b = serde_json::to_value(b).map_err(core_err)?;
            for k in bp.keys() {
                after.insert(k.clone(), b.get(k).cloned().unwrap_or(Value::Null));
            }
        }
        Ok(json!({ "before": before, "after": after }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let tid = op_id(op)?;
        let (before_map, after_map) = before_after(before);
        let db = self.0.store();
        let now = db
            .bot(tid)
            .map_err(core_err)?
            .map(serde_json::to_value)
            .transpose()
            .map_err(core_err)?
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let harnesses = self.0.models().harnesses();
        let back = steve_modules::changes::still_ours(cx, before_map, after_map, &now, |k| {
            field_shown("bot", k, false, k, &harnesses)
        });
        if !back.is_empty() {
            db.bot_patch(tid, &back, cx.actor).map_err(core_err)?;
        }
        Ok(())
    }
}

struct CoreProjectPatch(Core);
impl ChangeOp for CoreProjectPatch {
    fn apply(&self, _tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let tid = op_id(op)?;
        let patch = patch_map(op)?;
        let db = self.0.store();
        let x = serde_json::to_value(
            db.project(Some(tid))
                .map_err(core_err)?
                .ok_or_else(|| ApiError::rule(steve_i18n::t!("core.project.not_found")))?,
        )
        .map_err(core_err)?;
        let mut before = Map::new();
        for k in patch.keys() {
            before.insert(k.clone(), x.get(k).cloned().unwrap_or(Value::Null));
        }
        db.project_patch(tid, patch, cx.actor).map_err(core_err)?;
        let mut after = Map::new();
        if let Some(x) = db.project(Some(tid)).map_err(core_err)? {
            let x = serde_json::to_value(x).map_err(core_err)?;
            for k in patch.keys() {
                after.insert(k.clone(), x.get(k).cloned().unwrap_or(Value::Null));
            }
        }
        Ok(json!({ "before": before, "after": after }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let tid = op_id(op)?;
        let (before_map, after_map) = before_after(before);
        let db = self.0.store();
        let now = db
            .project(Some(tid))
            .map_err(core_err)?
            .map(serde_json::to_value)
            .transpose()
            .map_err(core_err)?
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let harnesses = self.0.models().harnesses();
        let back = steve_modules::changes::still_ours(cx, before_map, after_map, &now, |k| {
            field_shown("project", k, false, k, &harnesses)
        });
        if !back.is_empty() {
            db.project_patch(tid, &back, cx.actor).map_err(core_err)?;
        }
        Ok(())
    }
}

struct CoreSettingsPatch(Core);
impl ChangeOp for CoreSettingsPatch {
    fn apply(&self, _tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let patch = patch_map(op)?;
        let db = self.0.store();
        let mut cur = setting_defaults();
        cur.extend(db.settings_all(cx.ws).map_err(core_err)?);
        let mut rest = patch.clone();
        let prefix = rest.remove("item_prefix");
        let mut before = Map::new();
        let mut after = Map::new();
        for k in rest.keys() {
            before.insert(k.clone(), cur.get(k).cloned().unwrap_or(Value::Null));
        }
        if !rest.is_empty() {
            db.settings_set(cx.ws, &rest, cx.actor).map_err(core_err)?;
            let mut now = setting_defaults();
            now.extend(db.settings_all(cx.ws).map_err(core_err)?);
            for k in rest.keys() {
                after.insert(k.clone(), now.get(k).cloned().unwrap_or(Value::Null));
            }
        }
        if let Some(pre) = prefix.filter(truthy) {
            let w = db
                .workspace(cx.ws)
                .map_err(core_err)?
                .ok_or_else(|| ApiError::rule(steve_i18n::t!("core.space.not_found")))?;
            before.insert("item_prefix".into(), json!(w.item_prefix));
            db.workspace_patch(cx.ws, patch![item_prefix: jstr(&pre)])
                .map_err(core_err)?;
            if let Some(w) = db.workspace(cx.ws).map_err(core_err)? {
                after.insert("item_prefix".into(), json!(w.item_prefix));
            }
        }
        Ok(json!({ "before": before, "after": after }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let (before_map, after_map) = before_after(before);
        let db = self.0.store();
        let mut now = setting_defaults();
        now.extend(db.settings_all(cx.ws).map_err(core_err)?);
        if let Some(w) = db.workspace(cx.ws).map_err(core_err)? {
            now.insert("item_prefix".into(), json!(w.item_prefix));
        }
        let harnesses = self.0.models().harnesses();
        let mut back = steve_modules::changes::still_ours(cx, before_map, after_map, &now, |k| {
            field_shown("settings", k, false, k, &harnesses)
        });
        let prefix = back.remove("item_prefix");
        if !back.is_empty() {
            db.settings_set(cx.ws, &back, cx.actor).map_err(core_err)?;
        }
        if let Some(pre) = prefix.filter(truthy) {
            db.workspace_patch(cx.ws, patch![item_prefix: jstr(&pre)])
                .map_err(core_err)?;
        }
        Ok(())
    }
}

struct CoreTagsApply(Core);
impl ChangeOp for CoreTagsApply {
    fn apply(&self, _tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let patch = patch_map(op)?;
        let db = self.0.store();
        let mut made = vec![];
        for x in patch
            .get("create")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            made.push(
                db.tag_create(
                    cx.ws,
                    &jstr(&x["name"]),
                    &jstr(&x["cat"]),
                    &x.get("description").map(jstr).unwrap_or_default(),
                    "Steve",
                )
                .map_err(core_err)?
                .id,
            );
        }
        let mut edits = Map::new();
        for e in patch
            .get("edit")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(tag) = db.tag_by_name(cx.ws, &jstr(&e["name"])).map_err(core_err)? else {
                continue;
            };
            let s = |k: &str| e.get(k).filter(|v| truthy(v)).map(jstr);
            let to = steve_store::TagPatch {
                name: s("newName"),
                cat: s("cat"),
                description: s("description"),
            };
            edits.insert(
                tag.id.clone(),
                json!({"name": tag.name, "cat": tag.cat, "description": tag.description}),
            );
            db.tag_patch(&tag.id, &to).map_err(core_err)?;
        }
        let mut removed = vec![];
        for n in patch
            .get("remove")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            /* used since the draft: kept */
            let Some(tag) = db.tag_by_name(cx.ws, &jstr(n)).map_err(core_err)? else {
                continue;
            };
            if db.tag_used(&tag.id).map_err(core_err)? > 0 {
                continue;
            }
            removed.push(json!({"name": tag.name, "cat": tag.cat, "description": tag.description}));
            db.tag_remove(&tag.id).map_err(core_err)?;
        }
        Ok(json!({ "made": made, "edits": edits, "removed": removed }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let db = self.0.store();
        for m in before
            .get("made")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            db.tag_remove(&jstr(m)).map_err(core_err)?;
        }
        for (tag, was) in before
            .get("edits")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let back = steve_store::TagPatch {
                name: was.get("name").map(jstr),
                cat: was.get("cat").map(jstr),
                description: was.get("description").map(jstr),
            };
            db.tag_patch(tag, &back).map_err(core_err)?;
        }
        for t in before
            .get("removed")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            db.tag_create(
                cx.ws,
                &jstr(&t["name"]),
                &jstr(&t["cat"]),
                &jstr(&t["description"]),
                cx.actor,
            )
            .map_err(core_err)?;
        }
        Ok(())
    }
}

struct CoreModelsPatch(Core);
impl ChangeOp for CoreModelsPatch {
    fn apply(&self, _tx: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let patch = patch_map(op)?;
        let before = self.0.models().stored(cx.ws, patch);
        self.0
            .models()
            .write(cx.ws, patch, cx.actor, "settings.models")
            .map_err(core_err)?;
        let after = self.0.models().stored(cx.ws, patch);
        Ok(json!({ "before": before, "after": after }))
    }

    fn undo(
        &self,
        _tx: &mut steve_store::W<'_>,
        cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let (before_map, after_map) = before_after(before);
        let now = self.0.models().stored(cx.ws, after_map);
        let harnesses = self.0.models().harnesses();
        let back = steve_modules::changes::still_ours(cx, before_map, after_map, &now, |k| {
            field_shown("models", k, false, k, &harnesses)
        });
        if !back.is_empty() {
            self.0
                .models()
                .write(cx.ws, &back, cx.actor, "settings.models")
                .map_err(core_err)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_and_defaults() {
        let d = setting_defaults();
        assert_eq!(d["accDefault"], "auto");
        assert_eq!(d["archDays"], "30");
        assert_eq!(d.len(), 14);
        assert_eq!(d["tz"], "system");
        assert!(ws_settings::allowed("tz", "UTC") && !ws_settings::allowed("tz", "Mars/Base"));
        assert!(
            ws_settings::allowed("handoff", "70")
                && !ws_settings::allowed("handoff", "75")
                && !ws_settings::allowed("nope", "x")
        );
        let keys: Vec<&str> = ws_settings::schema()
            .categories()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["general", "items", "notify", "look", "data"]);
    }

    #[test]
    fn show_cuts_and_marks_empty() {
        assert_eq!(show(&Value::Null), "（空）");
        assert_eq!(show(&json!("")), "（空）");
        assert_eq!(show(&json!("a".repeat(41))), format!("{}…", "a".repeat(40)));
        assert_eq!(show(&json!(true)), "true");
    }
}
