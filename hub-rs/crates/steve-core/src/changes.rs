//! Conversational configuration (changes.ts, 变更集): the user says what they want, Steve drafts a field-level
//! change, the user applies it, and every applied change can be undone. Drafts and undo records live in memory.

use crate::error::{Result, msg};
use crate::gateway::{FieldSpec, TAG_CATS, Tier, draft_fields, first_json, jstr, truthy};
use crate::repo::{Bot, BotExt};
use crate::util::{js_head, js_len, now, uid};
use crate::{Core, patch};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;
use std::time::Duration;

/// `GET /api/settings/schema`, verbatim (key order preserved).
pub static SETTINGS: LazyLock<Value> = LazyLock::new(|| {
    json!({
      "general": [
        { "key": "lang", "label": "语言", "values": ["简体中文", "English"], "def": "简体中文", "live": false },
        { "key": "home", "label": "打开时进入", "values": ["对话", "待处理", "事项"], "def": "对话", "live": true },
        { "key": "awayH", "label": "离开期间汇报", "values": ["2", "4", "8", "0"], "def": "4", "live": true, "labels": { "2": "2 小时", "4": "4 小时", "8": "8 小时", "0": "不汇报" } },
        // the workspace time zone of schedules (steve-flow), as steve-hub's settings.rs serves it
        { "key": "tz", "label": "时区", "values": ["Asia/Taipei", "Asia/Shanghai", "Asia/Tokyo", "UTC", "Europe/London", "America/Los_Angeles", "America/New_York"], "def": "Asia/Taipei", "live": true },
      ],
      "items": [
        { "key": "accDefault", "label": "默认验收方式", "values": ["manual", "auto"], "def": "manual", "live": true, "labels": { "manual": "需要你验收", "auto": "自动验收" } },
        { "key": "depsAfter", "label": "前置完成后", "values": ["自动开始", "提醒我开始"], "def": "自动开始", "live": true },
        { "key": "handoff", "label": "交接阈值", "values": ["70", "80", "90"], "def": "80", "live": true, "labels": { "70": "70%", "80": "80%", "90": "90%" } },
        { "key": "archDays", "label": "完成后自动归档", "values": ["7", "30", "90", "0"], "def": "30", "live": true, "labels": { "7": "7 天", "30": "30 天", "90": "90 天", "0": "不自动归档" } },
      ],
      "notify": [
        { "key": "quiet", "label": "免打扰", "values": ["23:00 – 08:00", "22:00 – 09:00", "关闭"], "def": "23:00 – 08:00", "live": false },
        { "key": "digestAt", "label": "每天汇报", "values": ["08:30", "09:00", "不发"], "def": "08:30", "live": false },
      ],
      "look": [
        { "key": "fontSize", "label": "字号", "values": ["标准", "大"], "def": "标准", "live": true },
        { "key": "reduceMotion", "label": "减弱动态效果", "values": ["开", "关"], "def": "关", "live": true },
      ],
      "data": [
        { "key": "keepSess", "label": "会话记录保留", "values": ["90 天", "1 年", "永久"], "def": "永久", "live": false },
        // signals (S1 `steve_flow::retention::keep_raw_setting`) and mail (E1 `keepMail`), as steve-hub's settings.rs serves them
        { "key": "keepRaw", "label": "事件与消息原文保留", "values": ["7", "30", "90"], "def": "7", "live": true, "labels": { "7": "7 天", "30": "30 天", "90": "90 天" } },
        { "key": "keepMail", "label": "本地邮件正文保留", "values": ["30", "90", "365"], "def": "90", "live": true, "labels": { "30": "30 天", "90": "90 天", "365": "1 年" } },
      ],
    })
});

/// `settingDefaults()`: every key's `def`.
pub fn setting_defaults() -> Map<String, Value> {
    let mut m = Map::new();
    for spec in all_specs() {
        m.insert(
            spec["key"].as_str().unwrap_or_default().into(),
            spec["def"].clone(),
        );
    }
    m
}

fn all_specs() -> Vec<&'static Value> {
    SETTINGS
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .flat_map(|v| v.as_array().into_iter().flatten())
        .collect()
}

/// Is `v` an allowed value of setting `k`? (`PATCH /api/settings` validation)
pub fn setting_allowed(k: &str, v: &str) -> bool {
    all_specs().iter().any(|s| {
        s["key"] == k
            && s["values"]
                .as_array()
                .is_some_and(|vs| vs.iter().any(|x| x == v))
    })
}

const ACC: [(&str, &str); 3] = [
    ("inherit", "跟随全局"),
    ("manual", "需要你验收"),
    ("auto", "自动验收"),
];
/// Permission modes and their labels.
pub const PERM: [(&str, &str); 4] = [
    ("plan", "只读：只出方案，不改文件"),
    ("default", "每次改文件或运行命令都问你"),
    ("acceptEdits", "可以改项目目录里的文件，运行命令要问你"),
    ("auto", "可以改文件、运行命令；对外操作仍需你批准"),
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub label: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub id: String,
    pub ws: String,
    pub title: String,
    pub target: Target,
    pub href: String,
    pub changes: Vec<Change>,
    pub patch: Map<String, Value>,
    pub created: i64,
}

#[derive(Debug, Clone)]
pub struct Applied {
    target: Target,
    before: Map<String, Value>,
    ws: String,
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
        spec: FieldSpec {
            key: key.into(),
            label: label.into(),
            ty: "text",
            values: None,
            current: json!(current),
        },
        enc: None,
        dec: None,
    }
}

fn bot_fields(b: &Bot, projects: &[crate::repo::Project]) -> Vec<BotField> {
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
    let perm_keep = b.perm_mode.clone();
    vec![
        text_field("name", "名字", &b.name),
        text_field("role", "职责", &b.role),
        text_field("persona", "说话方式", &b.persona),
        BotField {
            spec: FieldSpec {
                key: "proactive".into(),
                label: "主动程度".into(),
                ty: "enum",
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
                ty: "enum",
                values: Some(
                    (if b.harness == "codex" {
                        vec!["默认", "none", "minimal", "low", "medium", "high", "xhigh"]
                    } else {
                        vec!["默认", "low", "medium", "high", "max"]
                    })
                    .iter()
                    .map(|v| v.to_string())
                    .collect(),
                ),
                current: json!(b.reasoning_effort.as_deref().unwrap_or("默认")),
            },
            enc: Some(Box::new(|v| {
                if jstr(v) == "默认" {
                    Value::Null
                } else {
                    v.clone()
                }
            })),
            dec: Some(Box::new(|v| {
                if v.is_null() {
                    json!("默认")
                } else {
                    v.clone()
                }
            })),
        },
        BotField {
            spec: FieldSpec {
                key: "accept_mode".into(),
                label: "交付验收".into(),
                ty: "enum",
                values: Some(ACC.iter().map(|(_, l)| l.to_string()).collect()),
                current: label_of(&ACC, &b.accept_mode),
            },
            enc: Some(Box::new(|v| {
                json!(key_of(&ACC, v).unwrap_or_else(|| "inherit".into()))
            })),
            dec: Some(Box::new(|v| label_of(&ACC, v.as_str().unwrap_or("")))),
        },
        BotField {
            spec: FieldSpec {
                key: "model".into(),
                label: "模型（opus、sonnet、haiku，留空为默认）".into(),
                ty: "text",
                values: None,
                current: json!(b.model.clone().unwrap_or_default()),
            },
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
                ty: "enum",
                values: Some(PERM.iter().map(|(_, l)| l.to_string()).collect()),
                current: label_of(&PERM, &b.perm_mode),
            },
            enc: Some(Box::new(move |v| {
                json!(key_of(&PERM, v).unwrap_or_else(|| perm_keep.clone()))
            })),
            dec: Some(Box::new(|v| label_of(&PERM, v.as_str().unwrap_or("")))),
        },
        BotField {
            spec: FieldSpec {
                key: "default_project".into(),
                label: "默认项目".into(),
                ty: "enum",
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
    ]
}

/// The choice of a purpose that has no override.
const FOLLOW: &str = "跟随 Steve";

fn cat_title(cat: &str) -> &'static str {
    match cat {
        "general" => "通用",
        "items" => "事项与验收",
        "notify" => "通知",
        "look" => "外观",
        "data" => "数据与隐私",
        _ => "undefined",
    }
}

/// The `models` draft target's fields: per purpose who runs it (`route.harness`: follow Steve or a
/// harness), its list (`route`) and effort (`route.effort`), per harness the agent default
/// (`agent.codex`). Empty means the default.
fn model_fields(m: &crate::models::Models, ws: &str) -> Vec<FieldSpec> {
    use crate::models::{CODEX_EFFORTS, EFFORTS, HARNESSES, JUDGE_HARNESSES, PURPOSES};
    let mut fs = vec![];
    let steve = m.steve();
    let effort_labels: Vec<String> = std::iter::once("默认".to_string())
        .chain(
            EFFORTS
                .iter()
                .chain(CODEX_EFFORTS.iter())
                .map(|(_, l)| l.to_string()),
        )
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
                .describe()
            ),
            ty: "enum",
            values: Some(
                std::iter::once(FOLLOW.to_string())
                    .chain(JUDGE_HARNESSES.iter().map(|(_, n)| n.to_string()))
                    .collect(),
            ),
            current: json!(
                c.as_ref()
                    .map_or(FOLLOW, |c| crate::models::harness_name(&c.harness))
            ),
        });
        let c = c.unwrap_or_default();
        fs.push(FieldSpec {
            key: p.key.into(),
            label: format!(
                "{} · 模型，按顺序逗号分隔，留空为该 harness 的默认",
                p.label
            ),
            ty: "text",
            values: None,
            current: json!(c.models.join(",")),
        });
        fs.push(FieldSpec {
            key: format!("{}.effort", p.key),
            label: format!("{} · 推理强度", p.label),
            ty: "enum",
            values: Some(effort_labels.clone()),
            current: json!(
                c.effort
                    .as_deref()
                    .and_then(crate::models::effort_label)
                    .unwrap_or("默认")
            ),
        });
    }
    for (h, name, known) in &HARNESSES {
        fs.push(FieldSpec {
            key: format!("agent.{h}"),
            label: format!(
                "{name} 新会话的默认模型（可选 {}，留空为默认）",
                known.join("、")
            ),
            ty: "text",
            values: None,
            current: json!(m.agent_model(ws, h).unwrap_or_default()),
        });
    }
    fs
}

/// What the model said, as the changes shown and the settings keys to write (checked by
/// [`crate::models::validate`]; anything it rejects is left out).
fn model_patch(
    m: &crate::models::Models,
    ws: &str,
    fs: &[FieldSpec],
    said: &[(String, Value)],
) -> (Vec<Change>, Map<String, Value>) {
    use crate::models::{HARNESSES, JUDGE_HARNESSES, PURPOSES, efforts, validate};
    let (mut changes, mut patch) = (vec![], Map::new());
    let get = |k: &str| said.iter().find(|(x, _)| x == k).map(|(_, v)| v);
    let label = |k: &str| {
        fs.iter()
            .find(|f| f.key == k)
            .map(|f| f.label.split('（').next().unwrap_or(&f.label).to_string())
            .unwrap_or_default()
    };
    for p in &PURPOSES {
        let stored = m.custom(ws, p.key);
        let cur = stored.clone().unwrap_or_default();
        let hk = format!("{}.harness", p.key);
        let ek = format!("{}.effort", p.key);
        // who runs it: unchanged, back to Steve, or a harness by name or id
        let harness: Option<String> = match get(&hk).map(jstr) {
            None => stored.as_ref().map(|c| c.harness.clone()),
            Some(h) if h.trim().is_empty() || h.trim() == FOLLOW => None,
            Some(h) => JUDGE_HARNESSES
                .iter()
                .find(|(k, n)| *k == h.trim() || *n == h.trim())
                .map(|(k, _)| k.to_string()),
        };
        let models: Vec<String> = match get(p.key) {
            Some(v) => jstr(v)
                .split([',', '，', '、'])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect(),
            None => cur.models.clone(),
        };
        let h = harness
            .clone()
            .unwrap_or_else(|| crate::models::CLAUDE_CODE.into());
        let effort = match get(&ek) {
            Some(v) => efforts(&h)
                .iter()
                .find(|(_, l)| *l == jstr(v))
                .map(|(k, _)| k.to_string()),
            None => cur.effort.clone(),
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
        let Ok(v) = validate(&body) else { continue };
        let show_choice = |c: Option<&crate::models::Choice>| match c {
            None => FOLLOW.to_string(),
            Some(c) => {
                let e = c.effort.as_deref().and_then(crate::models::effort_label);
                format!(
                    "{}{}",
                    c.describe(),
                    e.map(|e| format!(" · 推理{e}")).unwrap_or_default()
                )
            }
        };
        let to = (!follows).then(|| crate::models::Choice {
            harness: h.clone(),
            models: models.clone(),
            effort: effort.clone(),
        });
        changes.push(Change {
            label: format!("{} · 由谁来做", label(&hk)),
            from: show_choice(stored.as_ref()),
            to: show_choice(to.as_ref()),
        });
        patch.extend(v);
    }
    for (h, name, _) in &HARNESSES {
        let Some(v) = get(&format!("agent.{h}")) else {
            continue;
        };
        let to = jstr(v).trim().to_string();
        let cur = m.agent_model(ws, h).unwrap_or_default();
        if to == cur {
            continue;
        }
        let Ok(v) = validate(&json!({"agents": {*h: to}})) else {
            continue;
        };
        changes.push(Change {
            label: format!("{name} 新会话默认模型"),
            from: show(&json!(cur)).replace("（空）", "默认"),
            to: show(&json!(to)).replace("（空）", "默认"),
        });
        patch.extend(v);
    }
    (changes, patch)
}

impl Core {
    /// Draft a change of `target` from a request in plain words (`kind: global` lets the model pick the target).
    pub async fn draft(&self, ws: &str, target: Target, request: &str) -> Result<Proposal> {
        crate::stats::tagged("draft", ws, self.draft_untagged(ws, target, request)).await
    }

    async fn draft_untagged(&self, ws: &str, target: Target, request: &str) -> Result<Proposal> {
        let target = if target.kind == "global" {
            self.pick_target(ws, request).await?
        } else {
            target
        };
        let db = self.store();
        let gw = self.gw();
        let (mut title, mut href, mut changes, mut patch) =
            (String::new(), String::new(), vec![], Map::new());
        match target.kind.as_str() {
            "bot" => {
                let id = target.id.clone().unwrap_or_default();
                let b = match db.bot(&id)? {
                    Some(b) if db.bot_in_workspace(&b.id, ws)? => b,
                    _ => return Err(msg("没有这个 Bot")),
                };
                let fs = bot_fields(&b, &db.projects(ws)?);
                let specs: Vec<FieldSpec> = fs.iter().map(|f| f.spec.clone()).collect();
                let bv = serde_json::to_value(&b)?;
                for (k, value) in
                    draft_fields(gw, &format!("the bot \"{}\"", b.name), &specs, request).await?
                {
                    let Some(f) = fs.iter().find(|f| f.spec.key == k) else {
                        continue;
                    };
                    let v = f.enc.as_ref().map(|e| e(&value)).unwrap_or(value);
                    if bv.get(&k).unwrap_or(&Value::Null) == &v {
                        continue;
                    }
                    let to = f.dec.as_ref().map(|d| d(&v)).unwrap_or_else(|| v.clone());
                    changes.push(Change {
                        label: f.spec.label.clone(),
                        from: show(&f.spec.current),
                        to: show(&to),
                    });
                    patch.insert(k, v);
                }
                title = format!("{} 的设置", b.display());
                href = format!("#/bots/{}", b.id);
            }
            "project" => {
                let id = target.id.clone().unwrap_or_default();
                let p = db
                    .project(Some(&id))?
                    .filter(|p| p.workspace_id == ws)
                    .ok_or_else(|| msg("没有这个项目"))?;
                let bs = db.bots(ws)?;
                let fs = vec![
                    FieldSpec {
                        key: "name".into(),
                        label: "名称".into(),
                        ty: "text",
                        values: None,
                        current: json!(p.name),
                    },
                    FieldSpec {
                        key: "description".into(),
                        label: "说明".into(),
                        ty: "text",
                        values: None,
                        current: json!(p.description),
                    },
                    FieldSpec {
                        key: "default_bot".into(),
                        label: "默认 Bot".into(),
                        ty: "enum",
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
                        ty: "enum",
                        values: Some(vec!["开".into(), "关".into()]),
                        current: json!(if p.worktree { "开" } else { "关" }),
                    },
                ];
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
                        _ => value.clone(),
                    };
                    if pv.get(&k).unwrap_or(&Value::Null) == &v {
                        continue;
                    }
                    changes.push(Change {
                        label: f.label.clone(),
                        from: show(&f.current),
                        to: show(&value),
                    });
                    patch.insert(k, v);
                }
                title = format!("项目 {}", p.name);
                href = format!("#/projects/{}", p.id);
            }
            "settings" => {
                let cat = target.id.clone().unwrap_or_else(|| "general".into());
                let specs: Vec<Value> = SETTINGS
                    .get(&cat)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut cur = setting_defaults();
                cur.extend(db.settings_all(ws)?);
                let w = db.workspace(ws)?.ok_or_else(|| msg("没有这个工作空间"))?;
                let lab = |k: &str, v: &Value| -> Value {
                    specs
                        .iter()
                        .find(|s| s["key"] == k)
                        .and_then(|s| s.get("labels"))
                        .and_then(|l| l.get(jstr(v)))
                        .cloned()
                        .unwrap_or_else(|| json!(jstr(v)))
                };
                let mut fs: Vec<FieldSpec> = specs
                    .iter()
                    .map(|s| {
                        let labels = s.get("labels").and_then(Value::as_object).map(|l| {
                            format!(
                                "（值：{}）",
                                l.iter()
                                    .map(|(k, v)| format!("{k}={}", jstr(v)))
                                    .collect::<Vec<_>>()
                                    .join("，")
                            )
                        });
                        let key = s["key"].as_str().unwrap_or_default().to_string();
                        FieldSpec {
                            label: format!("{}{}", jstr(&s["label"]), labels.unwrap_or_default()),
                            ty: "enum",
                            values: s["values"]
                                .as_array()
                                .map(|vs| vs.iter().map(jstr).collect()),
                            current: cur.get(&key).cloned().unwrap_or(Value::Null),
                            key,
                        }
                    })
                    .collect();
                if cat == "items" {
                    fs.push(FieldSpec {
                        key: "item_prefix".into(),
                        label: "事项编号前缀（大写字母）".into(),
                        ty: "text",
                        values: None,
                        current: json!(w.item_prefix),
                    });
                }
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
                    let label = match f.label.find("（值：") {
                        Some(i) if f.label.ends_with('）') => f.label[..i].to_string(),
                        _ => f.label.clone(),
                    };
                    changes.push(Change {
                        label,
                        from: show(&lab(&k, &f.current)),
                        to: show(&lab(&k, &json!(v))),
                    });
                    patch.insert(k, json!(v));
                }
                title = format!("设置 · {}", cat_title(&cat));
                href = format!("#/settings/{cat}");
            }
            "tags" => {
                let ts = db.tags(ws)?;
                let mut lines = vec![
                    "Manage item tags in Steve. Existing tags (name · category · description):"
                        .to_string(),
                ];
                lines.extend(
                    ts.iter()
                        .map(|t| format!("  {} · {} · {}", t.name, t.cat, t.description)),
                );
                lines.push(format!("Categories: {}.", TAG_CATS.join("/")));
                lines.push("Reply with ONLY JSON: {\"create\":[{\"name\":\"\",\"cat\":\"\",\"description\":\"\"}],\"edit\":[{\"name\":\"<existing name>\",\"description\":\"<new description>\"}]}".into());
                let prompt = crate::gateway::with_static(
                    &lines.join("\n"),
                    &format!("Request: {}", serde_json::to_string(request)?),
                );
                let r = first_json(
                    &gw.print(&prompt, Tier::Smart, Duration::from_secs(90))
                        .await?,
                )?
                .unwrap_or(json!({}));
                let arr = |k: &str| {
                    r.get(k)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                };
                let create: Vec<Value> = arr("create")
                    .into_iter()
                    .filter(|t| {
                        t.get("name").is_some_and(truthy)
                            && t.get("cat")
                                .and_then(Value::as_str)
                                .is_some_and(|c| TAG_CATS.contains(&c))
                            && !ts
                                .iter()
                                .any(|x| t.get("name").and_then(Value::as_str) == Some(&x.name))
                    })
                    .collect();
                let edit: Vec<Value> = arr("edit")
                    .into_iter()
                    .filter(|e| {
                        ts.iter()
                            .any(|x| e.get("name").and_then(Value::as_str) == Some(&x.name))
                            && e.get("description").is_some_and(truthy)
                    })
                    .collect();
                for t in &create {
                    changes.push(Change {
                        label: format!("新建标签 · {}", jstr(&t["cat"])),
                        from: "—".into(),
                        to: format!(
                            "{}：{}",
                            jstr(&t["name"]),
                            t.get("description")
                                .map(jstr)
                                .unwrap_or_else(|| "undefined".into())
                        ),
                    });
                }
                for e in &edit {
                    let name = jstr(&e["name"]);
                    let old = ts
                        .iter()
                        .find(|x| x.name == name)
                        .map(|x| x.description.clone())
                        .unwrap_or_default();
                    changes.push(Change {
                        label: format!("描述 · {name}"),
                        from: show(&json!(old)),
                        to: show(&e["description"]),
                    });
                }
                patch.insert("create".into(), json!(create));
                patch.insert("edit".into(), json!(edit));
                title = "标签".into();
                href = "#/settings/tags".into();
            }
            "models" => {
                let fs = model_fields(self.models(), ws);
                let said = draft_fields(gw, "Steve settings · models", &fs, request).await?;
                (changes, patch) = model_patch(self.models(), ws, &fs, &said);
                title = "设置 · 模型".into();
                href = "#/settings/models".into();
            }
            _ => {}
        }
        let p = Proposal {
            id: uid("chg"),
            ws: ws.into(),
            title,
            target,
            href,
            changes,
            patch,
            created: now(),
        };
        self.tx().st.drafts.insert(p.id.clone(), p.clone());
        Ok(p)
    }

    async fn pick_target(&self, ws: &str, request: &str) -> Result<Target> {
        let db = self.store();
        let mut opts: Vec<String> = db
            .bots(ws)?
            .iter()
            .map(|b| format!("bot:{} — {}（{}）", b.id, b.name, b.role))
            .collect();
        opts.extend(
            db.projects(ws)?
                .iter()
                .map(|p| format!("project:{} — 项目 {}", p.id, p.name)),
        );
        opts.extend(
            [
                "settings:general — 语言、启动页、汇报",
                "settings:items — 验收、编号、前置、交接、归档",
                "settings:notify — 通知、免打扰",
                "settings:look — 字号、动态效果",
                "settings:data — 保留期限",
                "models — Steve 自己做判断用的模型、推理强度，新会话的默认模型",
                "tags — 事项标签",
            ]
            .map(String::from),
        );
        let prompt = crate::gateway::with_static(
            &format!(
                "Which Steve configuration does this request change? Options:\n{}\nReply with ONLY JSON {{\"target\":\"<option before the dash>\"}}",
                opts.join("\n")
            ),
            &format!("Request: {}", serde_json::to_string(request)?),
        );
        let raw = self
            .gw()
            .print(&prompt, Tier::Fast, Duration::from_secs(60))
            .await
            .unwrap_or_else(|_| "{}".into());
        let r = first_json(&raw)?;
        let t = match r.as_ref().and_then(|r| r.get("target")) {
            None | Some(Value::Null) => "settings:general".to_string(),
            Some(v) => jstr(v),
        };
        let t = t.trim();
        let mut parts = t.split(':');
        let kind = parts.next().unwrap_or_default().to_string();
        let id = parts.next().map(String::from);
        Ok(if kind == "tags" || kind == "models" {
            Target { kind, id: None }
        } else {
            Target { kind, id }
        })
    }

    /// Apply a draft of this workspace; returns the id to undo it with.
    pub fn apply(&self, id: &str, actor: &str, ws: &str) -> Result<Value> {
        let mut t = self.tx();
        let p = match t.st.drafts.get(id) {
            Some(p) if p.ws == ws => p.clone(),
            _ => return Err(msg("这份草稿已经失效，请重新说一次")),
        };
        t.st.drafts.remove(id);
        let db = t.db();
        let mut before = Map::new();
        let tid = p.target.id.clone().unwrap_or_default();
        match p.target.kind.as_str() {
            "bot" => {
                let b = serde_json::to_value(db.bot(&tid)?.ok_or_else(|| msg("没有这个 Bot"))?)?;
                for k in p.patch.keys() {
                    before.insert(k.clone(), b.get(k).cloned().unwrap_or(Value::Null));
                }
                db.bot_patch(&tid, &p.patch, actor)?;
            }
            "project" => {
                let x = serde_json::to_value(
                    db.project(Some(&tid))?.ok_or_else(|| msg("没有这个项目"))?,
                )?;
                for k in p.patch.keys() {
                    before.insert(k.clone(), x.get(k).cloned().unwrap_or(Value::Null));
                }
                db.project_patch(&tid, &p.patch, actor)?;
            }
            "settings" => {
                let mut cur = setting_defaults();
                cur.extend(db.settings_all(&p.ws)?);
                let mut rest = p.patch.clone();
                let prefix = rest.remove("item_prefix");
                for k in rest.keys() {
                    before.insert(k.clone(), cur.get(k).cloned().unwrap_or(Value::Null));
                }
                if !rest.is_empty() {
                    db.settings_set(&p.ws, &rest, actor)?;
                }
                if let Some(pre) = prefix.filter(truthy) {
                    let w = db
                        .workspace(&p.ws)?
                        .ok_or_else(|| msg("没有这个工作空间"))?;
                    before.insert("item_prefix".into(), json!(w.item_prefix));
                    db.workspace_patch(&p.ws, patch![item_prefix: jstr(&pre)])?;
                }
            }
            "tags" => {
                let mut made = vec![];
                for x in p
                    .patch
                    .get("create")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    made.push(
                        db.tag_create(
                            &p.ws,
                            &jstr(&x["name"]),
                            &jstr(&x["cat"]),
                            &x.get("description").map(jstr).unwrap_or_default(),
                            "Steve",
                        )?
                        .id,
                    );
                }
                let mut edits = Map::new();
                for e in p
                    .patch
                    .get("edit")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(tag) = db.tag_by_name(&p.ws, &jstr(&e["name"]))? else {
                        continue;
                    };
                    edits.insert(tag.id.clone(), json!(tag.description));
                    db.tag_set_description(&tag.id, &jstr(&e["description"]))?;
                }
                before.insert("made".into(), json!(made));
                before.insert("edits".into(), Value::Object(edits));
            }
            "models" => {
                before = self.models().stored(&p.ws, &p.patch);
                self.models()
                    .write(&p.ws, &p.patch, actor, "settings.models")?;
            }
            _ => {}
        }
        db.audit(
            actor,
            "config.apply",
            id,
            &json!({"target": p.target, "patch": p.patch}),
            &p.ws,
        )?;
        t.st.applied.insert(
            id.into(),
            Applied {
                target: p.target.clone(),
                before,
                ws: p.ws.clone(),
            },
        );
        Ok(json!({"ok": true, "undo": id}))
    }

    /// Undo an applied draft of this workspace.
    pub fn undo(&self, id: &str, actor: &str, ws: &str) -> Result<Value> {
        let mut t = self.tx();
        let a = match t.st.applied.get(id) {
            Some(a) if a.ws == ws => a.clone(),
            _ => return Err(msg("这次改动已经不能撤销了")),
        };
        t.st.applied.remove(id);
        let db = t.db();
        let tid = a.target.id.clone().unwrap_or_default();
        match a.target.kind.as_str() {
            "bot" => db.bot_patch(&tid, &a.before, actor)?,
            "project" => db.project_patch(&tid, &a.before, actor)?,
            "settings" => {
                let mut rest = a.before.clone();
                let prefix = rest.remove("item_prefix");
                if !rest.is_empty() {
                    db.settings_set(&a.ws, &rest, actor)?;
                }
                if let Some(pre) = prefix.filter(truthy) {
                    db.workspace_patch(&a.ws, patch![item_prefix: jstr(&pre)])?;
                }
            }
            "tags" => {
                for m in a
                    .before
                    .get("made")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    db.tag_remove(&jstr(m))?;
                }
                for (tag, d) in a
                    .before
                    .get("edits")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    db.tag_set_description(tag, &jstr(d))?;
                }
            }
            "models" => self
                .models()
                .write(&a.ws, &a.before, actor, "settings.models")?,
            _ => {}
        }
        db.audit(actor, "config.undo", id, &json!({}), &a.ws)?;
        Ok(json!({"ok": true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_and_defaults() {
        let d = setting_defaults();
        assert_eq!(d["accDefault"], "manual");
        assert_eq!(d["archDays"], "30");
        assert_eq!(d.len(), 15);
        assert_eq!(d["tz"], "Asia/Taipei");
        assert!(setting_allowed("tz", "UTC") && !setting_allowed("tz", "Mars/Base"));
        assert!(
            setting_allowed("handoff", "70")
                && !setting_allowed("handoff", "75")
                && !setting_allowed("nope", "x")
        );
        let keys: Vec<&String> = SETTINGS.as_object().unwrap().keys().collect();
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
