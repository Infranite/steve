//! The first meeting's tools (`steve_core::thread::onboard`): what Steve writes once the owner said
//! yes in the conversation — their profile, the models of Steve's purposes, the first Bots — and the
//! end of the meeting. They exist for the run answering the owner in Steve's own conversation while
//! the meeting is on; every call checks that again, and every write goes through the same checks,
//! audit and revisions as the settings pages. A call that wrote also leaves one audit entry of its
//! own (`onboard.profile`, `onboard.models`, `onboard.bots`), so the trail says these changes were
//! the first meeting's, next to the entries of the writes themselves.

use serde_json::{Value, json};
use steve_core::Core;
use steve_modules::contacts::{memory, service, stewards};
use steve_store::{NewBot, ThreadRun};

use super::mcp_extra::Extra;

pub const PROFILE: &str = "steve_onboard_profile";
pub const MODELS: &str = "steve_onboard_models";
pub const BOTS: &str = "steve_onboard_bots";
pub const DONE: &str = "steve_onboard_done";
/// The answer once the meeting is over, or to any run that is not the owner's own.
pub const CLOSED: &str = "The first meeting is not on, so this tool is closed. Settings change through the change cards of the conversation: tell the user to say the change in their own words.";

/// The most Bots one call creates.
const MAX_BOTS: usize = 6;
/// The most facts and preferences one call saves.
const MAX_FACTS: usize = 12;

pub fn is(name: &str) -> bool {
    matches!(name, PROFILE | MODELS | BOTS | DONE)
}

/// The meeting is on for the run: the owner, in Steve's own conversation.
pub fn open(core: &Core, run: &ThreadRun) -> bool {
    core.onboard_allows(&run.workspace_id, &run.message_id, run.user_id.as_deref())
        .unwrap_or(false)
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": props, "required": required})
}

/// The tools, as `/mcp` lists them for a run the meeting is [`open`] for.
pub fn tools() -> Vec<Value> {
    let texts = |what: &str| json!({"type": "array", "items": {"type": "string"}, "maxItems": MAX_FACTS, "description": what});
    vec![
        json!({
            "name": PROFILE,
            "description": "First meeting only. Save what you learned about the user to their own profile, after they agreed: what to call them, a short description of who they are and what they do, and stable facts and preferences Steve should remember (one per entry, a full sentence each). Never include secrets. Entries already saved are skipped.",
            "inputSchema": schema(json!({
                "name": {"type": "string", "description": "what to call the user (at most 80 characters)"},
                "about": {"type": "string", "description": "who they are, their role and what they use Steve for (at most 2000 characters)"},
                "facts": texts("stable facts about the user"),
                "preferences": texts("how the user likes things done"),
            }), &[]),
        }),
        json!({
            "name": MODELS,
            "description": "First meeting only. Without arguments: lists Steve's purposes (what each is for and what it runs on now) and every harness with the models it offers. With `purposes`: sets the harness, models (tried in order) and effort of the purposes named, after the user agreed; only harness and model ids from the list are accepted, `null` puts a purpose back on Steve's own model. Purposes not named are left alone.",
            "inputSchema": schema(json!({
                "purposes": {
                    "type": "object",
                    "description": "purpose key -> {\"harness\": id, \"models\": [id, ...], \"effort\": optional} or null",
                    "additionalProperties": {"type": ["object", "null"]},
                },
            }), &[]),
        }),
        json!({
            "name": BOTS,
            "description": "First meeting only. Without arguments: lists the Bots there are already (name, what each is responsible for, its harness); look before proposing new ones. With `bots`: creates the Bots the user agreed to. Each needs a name; `role` says what it is responsible for, `rules` how it works, `persona` how it speaks. `harness` and `model` are optional (ids from steve_onboard_models; the default is the harness Steve itself runs on with its default model). A Bot whose name exists already is skipped.",
            "inputSchema": schema(json!({
                "bots": {
                    "type": "array", "minItems": 1, "maxItems": MAX_BOTS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "1 to 40 characters"},
                            "role": {"type": "string", "description": "at most 200 characters"},
                            "rules": {"type": "string", "description": "at most 8000 characters"},
                            "persona": {"type": "string", "description": "at most 400 characters"},
                            "harness": {"type": "string"},
                            "model": {"type": "string"},
                        },
                        "required": ["name"],
                    },
                },
            }), &[]),
        }),
        json!({
            "name": DONE,
            "description": "First meeting only. Call once the introductions are over, or as soon as the user wants to skip them. After this the first-meeting tools are closed.",
            "inputSchema": schema(json!({}), &[]),
        }),
    ]
}

/// The tools as a Node's catalog entries: they write.
pub fn descriptors() -> Vec<steve_node_api::ToolDescriptor> {
    tools()
        .into_iter()
        .map(|t| steve_node_api::ToolDescriptor {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
            read_only: false,
        })
        .collect()
}

/// One call of a first-meeting tool by `run`; `owner` is the workspace's owner, who a run without a
/// user of its own stands for.
pub async fn call(
    x: &Extra,
    run: &ThreadRun,
    name: &str,
    args: &Value,
    owner: &str,
) -> Result<String, String> {
    let (x, run, name, args, owner) = (
        x.clone(),
        run.clone(),
        name.to_string(),
        args.clone(),
        owner.to_string(),
    );
    tokio::task::spawn_blocking(steve_i18n::carry(move || {
        let core = &x.tools.core;
        if !open(core, &run) {
            return Err(CLOSED.to_string());
        }
        let ws = run.workspace_id.as_str();
        let user = run.user_id.clone().unwrap_or(owner);
        let (said, wrote) = match name.as_str() {
            PROFILE => profile(&x, ws, &user, &args)?,
            MODELS => models(core, ws, &user, &args)?,
            BOTS => bots(&x, ws, &user, &args)?,
            DONE => {
                core.onboard_end(ws, &user, steve_core::thread::onboard::ENDED)
                    .map_err(|e| e.to_string())?;
                let said =
                    "The first meeting is over. From here on the conversation works as usual.";
                (said.to_string(), None)
            }
            _ => return Err(super::research_mcp::REFUSED.into()),
        };
        /* what the call wrote, as one entry (best effort: each write has its own entry already) */
        if let Some(wrote) = wrote {
            let action = format!("onboard.{}", name.trim_start_matches("steve_onboard_"));
            if let Err(e) = x.store.audit(&user, &action, ws, &wrote, ws) {
                tracing::warn!("first meeting: audit of {action}: {e}");
            }
        }
        Ok(said)
    }))
    .await
    .map_err(|e| e.to_string())?
}

/// A text argument: trimmed, absent when empty.
fn text(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("`{key}` must be text.")),
    }
}

/// A list-of-texts argument: trimmed, empty entries dropped.
fn texts(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(|s| s.trim().to_string()))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.into_iter().filter(|s| !s.is_empty()).collect())
            .ok_or_else(|| format!("`{key}` must be a list of texts.")),
        Some(_) => Err(format!("`{key}` must be a list of texts.")),
    }
}

fn too_long(key: &str, value: Option<&str>, max: usize) -> Result<(), String> {
    match value {
        Some(v) if v.chars().count() > max => {
            Err(format!("`{key}` is longer than {max} characters."))
        }
        _ => Ok(()),
    }
}

/// What a tool answers, and what it wrote (for the meeting's audit entry) when it wrote.
type Called = Result<(String, Option<Value>), String>;

fn profile(x: &Extra, ws: &str, user: &str, args: &Value) -> Called {
    let (name, about) = (text(args, "name")?, text(args, "about")?);
    let kinds = [
        ("fact", texts(args, "facts")?),
        ("preference", texts(args, "preferences")?),
    ];
    let entries = kinds.iter().map(|(_, l)| l.len()).sum::<usize>();
    if name.is_none() && about.is_none() && entries == 0 {
        return Err("Nothing to save: give `name`, `about`, `facts` or `preferences`.".into());
    }
    too_long("name", name.as_deref(), 80)?;
    too_long("about", about.as_deref(), 2000)?;
    if entries > MAX_FACTS {
        return Err(format!(
            "At most {MAX_FACTS} facts and preferences in one call."
        ));
    }
    for t in kinds.iter().flat_map(|(_, l)| l) {
        too_long("facts", Some(t), 2000)?;
    }
    let who = steve_modules::Caller {
        ws: ws.into(),
        me: user.into(),
    };
    let shown = |e: steve_modules::ApiError| e.to_string();
    let mut said = vec![];
    let mut fields = vec![];
    if name.is_some() || about.is_some() {
        if let Some(n) = &name {
            said.push(format!("name “{n}”"));
            fields.push("name");
        }
        if about.is_some() {
            said.push("description".into());
            fields.push("note");
        }
        service::patch_contact(
            &x.modules,
            &who,
            "me",
            service::ContactPatch {
                name,
                note: about,
                ..Default::default()
            },
        )
        .map_err(shown)?;
    }
    let known: Vec<String> = memory::list(&x.modules, &who, "me")
        .map_err(shown)?
        .into_iter()
        .map(|m| m.text)
        .collect();
    let (mut saved, mut skipped) = (0, 0);
    for (kind, list) in kinds {
        for t in list {
            if known.contains(&t) {
                skipped += 1;
                continue;
            }
            memory::create(
                &x.modules,
                &who,
                "me",
                memory::NewMemBody {
                    text: t,
                    kind: Some(kind.into()),
                    level: None,
                },
            )
            .map_err(shown)?;
            saved += 1;
        }
    }
    if saved > 0 {
        said.push(format!("{saved} memories"));
    }
    if skipped > 0 {
        said.push(format!("{skipped} already known"));
    }
    let wrote = (!fields.is_empty() || saved > 0)
        .then(|| json!({"fields": fields, "memories": saved, "known": skipped}));
    Ok((
        format!(
            "Saved to the user's profile: {}. They can read and change it under 我.",
            said.join(", ")
        ),
        wrote,
    ))
}

/// The purposes and what they run on, and the harnesses with their models, for the model to choose from.
fn overview(core: &Core, ws: &str) -> Value {
    let m = core.models();
    let view = m.view(ws);
    let purposes: Vec<Value> = view["purposes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "key": p["key"], "label": p["label"], "what": p["desc"],
                "now": p["effective"],
                "follows_steve": p["follows"],
            })
        })
        .collect();
    let harnesses: Vec<Value> = m
        .harnesses()
        .into_iter()
        .map(|h| {
            json!({
                "id": h.id, "name": h.name, "ready": h.ready,
                "models": h.models, "default_model": h.default_model, "efforts": h.efforts,
            })
        })
        .collect();
    json!({
        "steve": {"harness": view["steve"]["runs_on"], "models": view["steve"]["models"]},
        "purposes": purposes,
        "harnesses": harnesses,
        "max_models_per_purpose": view["max"],
    })
}

/// A harness and a model the Hub knows: the model one of those the harness lists (any, while it lists none).
fn known_model(core: &Core, harness: &str, model: &str) -> Result<(), String> {
    let Some(h) = core.models().harness(harness) else {
        return Err(format!("There is no harness `{harness}` here."));
    };
    let ids = h.model_ids();
    if !ids.is_empty() && !ids.iter().any(|m| m == model) {
        return Err(format!(
            "`{model}` is not a model of `{harness}`. Its models: {}.",
            ids.join(", ")
        ));
    }
    Ok(())
}

fn models(core: &Core, ws: &str, user: &str, args: &Value) -> Called {
    let Some(purposes) = args.get("purposes").filter(|p| !p.is_null()) else {
        return Ok((overview(core, ws).to_string(), None));
    };
    let steve = core.models().steve().runs_on;
    for (key, choice) in purposes.as_object().into_iter().flatten() {
        let harness = choice["harness"].as_str().unwrap_or(&steve);
        for m in choice["models"].as_array().into_iter().flatten() {
            known_model(core, harness, m.as_str().unwrap_or_default())
                .map_err(|e| format!("{key}: {e}"))?;
        }
    }
    let mut patch = purposes.clone();
    /* a choice that names no harness is on the one Steve runs on, as the list showed it */
    for choice in patch.as_object_mut().into_iter().flatten().map(|(_, c)| c) {
        if let Some(c) = choice.as_object_mut()
            && !c.contains_key("harness")
        {
            c.insert("harness".into(), json!(steve));
        }
    }
    let written = core.models().apply(ws, &json!({"purposes": patch}), user)?;
    let keys: Vec<&String> = patch
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k)
        .collect();
    let said = json!({
        "saved": written.len(),
        "purposes": overview(core, ws)["purposes"],
        "note": "The user can change these under Settings · Models.",
    });
    let wrote = (!written.is_empty()).then(|| json!({"purposes": keys}));
    Ok((said.to_string(), wrote))
}

fn bots(x: &Extra, ws: &str, user: &str, args: &Value) -> Called {
    let core = &x.tools.core;
    let there = x.store.bots().list(Some(ws)).map_err(|e| e.to_string())?;
    if args.get("bots").is_none_or(Value::is_null) {
        let bots: Vec<Value> = there
            .iter()
            .map(|b| json!({"name": b.name, "role": b.role, "harness": b.harness, "main": b.main}))
            .collect();
        return Ok((json!({"bots": bots}).to_string(), None));
    }
    let list = args["bots"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("`bots` must be a list with at least one Bot.")?;
    if list.len() > MAX_BOTS {
        return Err(format!("At most {MAX_BOTS} Bots in one call."));
    }
    let steve = core.models().steve().runs_on;
    let mut new = vec![];
    for b in list {
        let name = text(b, "name")?.ok_or("Every Bot needs a `name`.")?;
        let (role, rules, persona) = (text(b, "role")?, text(b, "rules")?, text(b, "persona")?);
        too_long("name", Some(&name), 40)?;
        too_long("role", role.as_deref(), 200)?;
        too_long("rules", rules.as_deref(), 8000)?;
        too_long("persona", persona.as_deref(), 400)?;
        let harness = text(b, "harness")?.unwrap_or_else(|| steve.clone());
        let model = text(b, "model")?;
        if core.models().harness(&harness).is_none() {
            return Err(format!("{name}: there is no harness `{harness}` here."));
        }
        if let Some(m) = &model {
            known_model(core, &harness, m).map_err(|e| format!("{name}: {e}"))?;
        }
        new.push(NewBot {
            name,
            harness,
            role,
            model,
            rules,
            persona,
            ..Default::default()
        });
    }
    let have: Vec<String> = there.iter().map(|b| b.name.to_lowercase()).collect();
    let (mut created, mut skipped) = (vec![], vec![]);
    for b in new {
        if have.contains(&b.name.to_lowercase()) || created.contains(&b.name) {
            skipped.push(b.name);
            continue;
        }
        let bot = x
            .store
            .bots()
            .create(ws, &b, user)
            .map_err(|e| e.to_string())?
            .ok_or("The Bot was not created.")?;
        /* whoever creates a Bot is its 负责人, as on the Bots page (best effort: the Bot exists either way) */
        if let Err(e) = x
            .modules
            .store
            .transaction(|tx| stewards::adopt(tx, ws, "bot", &bot.id, Some(user), "seed"))
        {
            tracing::warn!("first meeting: steward of {}: {e}", bot.id);
        }
        created.push(b.name);
    }
    let mut out = vec![];
    if !created.is_empty() {
        out.push(format!("Created: {}.", created.join(", ")));
    }
    if !skipped.is_empty() {
        out.push(format!(
            "Skipped, a Bot of that name exists: {}.",
            skipped.join(", ")
        ));
    }
    out.push("The user can change them under Bots, and give one work with @name.".into());
    let wrote = (!created.is_empty()).then(|| json!({"created": created, "skipped": skipped}));
    Ok((out.join(" "), wrote))
}
