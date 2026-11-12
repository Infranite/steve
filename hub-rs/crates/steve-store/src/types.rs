//! Row types. Field order follows the `SELECT *` column order (migration 1 columns, then the ones
//! migration 2 appended), so the JSON matches what the TS API returns key for key. Conversions follow
//! the TS row mappers (`rowItem`, `rowBot`, …): 0/1 → bool only where TS converts, JSON columns parsed
//! with the `json(s, default)` fallback. Status-like columns stay `String`: the store passes them
//! through and the TS unions are still growing.

use rusqlite::Row;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

/// `json(s, d)` from db.ts: null, empty or unparsable text gives the default.
pub(crate) fn json_or<T: DeserializeOwned>(s: Option<String>, d: T) -> T {
    match s.as_deref() {
        None | Some("") => d,
        Some(s) => serde_json::from_str(s).unwrap_or(d),
    }
}

fn json_col<T: DeserializeOwned>(r: &Row, col: &str, d: T) -> rusqlite::Result<T> {
    Ok(json_or(r.get::<_, Option<String>>(col)?, d))
}

fn bool_col(r: &Row, col: &str) -> rusqlite::Result<bool> {
    Ok(r.get::<_, Option<i64>>(col)?.unwrap_or(0) != 0)
}

/// For `Option<Option<T>>` fields: a present `null` becomes `Some(None)`, an absent key `None`.
pub(crate) fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// JS truthiness, for TS code like `meta ? JSON.stringify(meta) : null`.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step: String,
    pub done: bool,
    /// Not done when the item finished: the agent left it open on delivery, or nobody ticked it off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
}

/// An earlier plan, kept when the agent replaced it (`replaced`: its open steps were abandoned) or
/// started a new round after finishing it (`done`). Stored in `revisions` as `item_plan`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRound {
    pub round: i64,
    pub steps: Vec<PlanStep>,
    pub end: String,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snooze {
    pub label: String,
    pub until: i64,
}

/// Where a forked item came from. Every key is optional and may be null, as in TS.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Parent {
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub item: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub session: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub at: Option<Option<i64>>,
}

/// Bot avatar. `round` is a JSON number so 70 stays `70`, not `70.0`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Avatar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub skin: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<Number>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub kind: String,
    pub cmd: String,
    /// What counts as finished for an accepted result, instead of a release by `cmd` on this machine:
    /// `merge` (its commits are on the remote default branch) or `tag` (merged, and a pushed tag
    /// contains them). A build the tag triggers is followed on its own and never holds the close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<String>,
}

/// [`Release::done_when`] values.
pub const DONE_MERGE: &str = "merge";
pub const DONE_TAG: &str = "tag";

fn default_source() -> (String, String) {
    ("对话".into(), "chat".into())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created: i64,
    pub description: String,
    pub item_prefix: String,
    pub sort: i64,
}

impl Workspace {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            created: r.get("created")?,
            description: r.get("description")?,
            item_prefix: r.get("item_prefix")?,
            sort: r.get("sort")?,
        })
    }
}

/// `workspaces.ofUser`: the workspace plus the member's role.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemberWorkspace {
    #[serde(flatten)]
    pub workspace: Workspace,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct User {
    pub id: String,
    pub workspace_id: String,
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
    pub created: i64,
}

impl User {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            subject: r.get("subject")?,
            name: r.get("name")?,
            email: r.get("email")?,
            role: r.get("role")?,
            created: r.get("created")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub device_id: String,
    pub path: String,
    /// `git` or `dir`
    pub kind: String,
    pub worktree: bool,
    pub default_bot: Option<String>,
    pub release: Option<Release>,
    pub archived: bool,
    pub created: i64,
    pub updated: i64,
}

impl Project {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            name: r.get("name")?,
            description: r.get("description")?,
            device_id: r.get("device_id")?,
            path: r.get("path")?,
            kind: r.get("kind")?,
            worktree: bool_col(r, "worktree")?,
            default_bot: r.get("default_bot")?,
            release: json_col(r, "release", None)?,
            archived: bool_col(r, "archived")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

/// `rowBot`: `project_dir`, `worktree`, `deleted` are dropped; `workspace_id` is the legacy owner.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bot {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub role: String,
    pub main: bool,
    pub harness: String,
    pub model: Option<String>,
    pub rules: String,
    pub accept_mode: String,
    pub perm_mode: String,
    pub enabled: bool,
    pub revision: i64,
    pub updated: i64,
    pub persona: String,
    /// stays a number in TS (not converted to bool)
    pub proactive: i64,
    pub avatar: Avatar,
    pub default_project: Option<String>,
    pub prompt: Option<String>,
    pub device: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl Bot {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            name: r.get("name")?,
            role: r.get("role")?,
            main: bool_col(r, "main")?,
            harness: r.get("harness")?,
            model: r.get("model")?,
            reasoning_effort: r.get("reasoning_effort")?,
            rules: r.get("rules")?,
            accept_mode: r.get("accept_mode")?,
            perm_mode: r.get("perm_mode")?,
            enabled: bool_col(r, "enabled")?,
            revision: r.get("revision")?,
            updated: r.get("updated")?,
            persona: r.get("persona")?,
            proactive: r.get("proactive")?,
            avatar: json_col(r, "avatar", Avatar::default())?,
            default_project: r.get("default_project")?,
            prompt: r.get("prompt")?,
            device: r.get("device")?,
        })
    }
}

/// A row of `bot_workspaces` as `bots.workspaces` returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BotWorkspace {
    pub workspace_id: String,
    pub duty: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Memory {
    pub id: String,
    pub bot_id: String,
    pub workspace_id: String,
    pub text: String,
    pub source: String,
    pub created: i64,
}

impl Memory {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            bot_id: r.get("bot_id")?,
            workspace_id: r.get("workspace_id")?,
            text: r.get("text")?,
            source: r.get("source")?,
            created: r.get("created")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tag {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub cat: String,
    pub description: String,
    pub created_by: String,
    pub created: i64,
}

impl Tag {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            name: r.get("name")?,
            cat: r.get("cat")?,
            description: r.get("description")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
        })
    }
}

/// `rowItem`. `project_dir` (legacy, always null today) stays in the JSON like in TS.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub workspace_id: String,
    pub no: i64,
    /// `task` or `chat`
    pub kind: String,
    pub title: String,
    pub status: String,
    pub stage: String,
    pub bot_id: Option<String>,
    pub project_dir: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub plan: Vec<PlanStep>,
    pub now: String,
    pub accept_mode: Option<String>,
    pub priority: String,
    pub archived: bool,
    pub created: i64,
    pub updated: i64,
    pub project_id: Option<String>,
    pub star: bool,
    pub due: Option<String>,
    pub snooze: Option<Snooze>,
    pub source: (String, String),
    pub parent: Option<Parent>,
    /// The lifecycle (migration 027): `ready` | `active` | `closing` | `closed`. Written only by the
    /// core's lifecycle ([`crate::lifecycle_write_in`]); `status` is its legacy projection.
    pub phase: String,
    pub cycle: i64,
    /// The result revision within the cycle (0: nothing delivered yet).
    pub revision: i64,
    /// `completed` | `failed` | `cancelled` | `superseded` once closed.
    pub outcome: Option<String>,
    /// `human` | `automatic` | `waived` once closed completed (`None` for migrated history).
    pub acceptance: Option<String>,
    /// The closing intent while `phase = closing`.
    pub closing: Option<serde_json::Value>,
    pub closed_at: Option<i64>,
    pub closed_reason: Option<String>,
}

impl Item {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            no: r.get("no")?,
            kind: r.get("kind")?,
            title: r.get("title")?,
            status: r.get("status")?,
            stage: r.get("stage")?,
            bot_id: r.get("bot_id")?,
            project_dir: r.get("project_dir")?,
            cwd: r.get("cwd")?,
            branch: r.get("branch")?,
            plan: json_col(r, "plan", Vec::new())?,
            now: r.get("now")?,
            accept_mode: r.get("accept_mode")?,
            priority: r.get("priority")?,
            archived: bool_col(r, "archived")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            project_id: r.get("project_id")?,
            star: bool_col(r, "star")?,
            due: r.get("due")?,
            snooze: json_col(r, "snooze", None)?,
            source: json_col(r, "source", default_source())?,
            parent: json_col(r, "parent", None)?,
            phase: r.get("phase")?,
            cycle: r.get("cycle")?,
            revision: r.get("revision")?,
            outcome: r.get("outcome")?,
            acceptance: r.get("acceptance")?,
            closing: json_col(r, "closing", None)?,
            closed_at: r.get("closed_at")?,
            closed_reason: r.get("closed_reason")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Goal {
    pub version: i64,
    pub text: String,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub id: i64,
    pub ts: i64,
    pub actor: String,
    pub kind: String,
    pub text: String,
    pub session_id: Option<String>,
    /// Always an array in the JSON (`json(r.files, [])`).
    pub files: Vec<FileRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Artifact {
    pub id: String,
    pub item_id: String,
    pub kind: String,
    pub title: String,
    pub location: Option<String>,
    pub created: i64,
}

/// Sessions pass through unconverted in TS: `readonly` stays 0/1.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub item_id: Option<String>,
    pub no: i64,
    pub bot_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub cwd: String,
    pub acp_session_id: Option<String>,
    pub status: String,
    pub used: Option<i64>,
    pub size: Option<i64>,
    pub cost: Option<f64>,
    pub token_hash: Option<String>,
    pub title: String,
    pub created: i64,
    pub updated: i64,
    pub device_id: String,
    pub source: String,
    pub readonly: i64,
    pub import_key: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl Session {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            item_id: r.get("item_id")?,
            no: r.get("no")?,
            bot_id: r.get("bot_id")?,
            harness: r.get("harness")?,
            model: r.get("model")?,
            reasoning_effort: r.get("reasoning_effort")?,
            cwd: r.get("cwd")?,
            acp_session_id: r.get("acp_session_id")?,
            status: r.get("status")?,
            used: r.get("used")?,
            size: r.get("size")?,
            cost: r.get("cost")?,
            token_hash: r.get("token_hash")?,
            title: r.get("title")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            device_id: r.get("device_id")?,
            source: r.get("source")?,
            readonly: r.get("readonly")?,
            import_key: r.get("import_key")?,
        })
    }
}

/// One session event; `data` parsed (null if unparsable).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub seq: i64,
    pub ts: i64,
    pub kind: String,
    pub data: Value,
}

/// How a session's last turn ended, and whether you stopped it (`session_stops`, migration 62).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct SessionStop {
    /// The attempt whose end is recorded (none when the backend named none).
    pub attempt: Option<String>,
    /// `None` until a turn ends after the row was made (only a Stop was asked for).
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    /// The recorded end was the turn you stopped.
    pub yours: bool,
    /// The attempt your Stop was asked for (`""`: none named), until a turn ends.
    pub stop_asked: Option<String>,
    /// When the recorded end came.
    pub ended: Option<i64>,
    /// The session's last event then: a message sent after it began another turn.
    pub seq: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Queued {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub via: String,
    pub created: i64,
    pub files: Vec<FileRef>,
}

impl Queued {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            session_id: r.get("session_id")?,
            text: r.get("text")?,
            via: r.get("via")?,
            created: r.get("created")?,
            files: json_col(r, "files", Vec::new())?,
        })
    }
}

/// `rowDec`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    pub id: String,
    pub workspace_id: String,
    /// `accept` | `ask` | `approve` | `fail` | `propose`
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub question: String,
    pub draft: String,
    pub options: Vec<String>,
    /// `open` | `resolved` | `expired`
    pub status: String,
    pub result: Value,
    pub requested_by: String,
    pub created: i64,
    pub resolved: Option<i64>,
    pub undo: Value,
}

impl Decision {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            kind: r.get("kind")?,
            subject_type: r.get("subject_type")?,
            subject_id: r.get("subject_id")?,
            item_id: r.get("item_id")?,
            session_id: r.get("session_id")?,
            question: r.get("question")?,
            draft: r.get("draft")?,
            options: json_col(r, "options", Vec::new())?,
            status: r.get("status")?,
            result: json_col(r, "result", Value::Null)?,
            requested_by: r.get("requested_by")?,
            created: r.get("created")?,
            resolved: r.get("resolved")?,
            undo: json_col(r, "undo", Value::Null)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub id: String,
    pub workspace_id: String,
    pub space: String,
    /// `me` or `steve`
    pub author: String,
    pub text: String,
    pub item_id: Option<String>,
    pub created: i64,
    pub meta: Value,
}

impl Message {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            space: r.get("space")?,
            author: r.get("author")?,
            text: r.get("text")?,
            item_id: r.get("item_id")?,
            created: r.get("created")?,
            meta: json_col(r, "meta", Value::Null)?,
        })
    }
}

/// A semantic link. TS returns `props` as raw text; nothing reads it through the API, so it is parsed here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    pub workspace_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub predicate: String,
    pub object_type: String,
    pub object_id: String,
    pub props: Value,
    pub source: String,
    pub confidence: f64,
    pub created: i64,
}

impl Link {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: r.get("workspace_id")?,
            subject_type: r.get("subject_type")?,
            subject_id: r.get("subject_id")?,
            predicate: r.get("predicate")?,
            object_type: r.get("object_type")?,
            object_id: r.get("object_id")?,
            props: json_col(r, "props", Value::Object(Map::new()))?,
            source: r.get("source")?,
            confidence: r.get("confidence")?,
            created: r.get("created")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub roles: Vec<String>,
    pub os: String,
    pub version: String,
    pub status: String,
    pub max_sessions: i64,
    pub seen: i64,
    pub created: i64,
}

impl Device {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            roles: json_col(r, "roles", Vec::new())?,
            os: r.get("os")?,
            version: r.get("version")?,
            status: r.get("status")?,
            max_sessions: r.get("max_sessions")?,
            seen: r.get("seen")?,
            created: r.get("created")?,
        })
    }
}

/// What messages, comments, queued prompts and session events carry for each attachment (TS `FileRef`).
/// Copied, not joined, so it stays readable after the file row is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

/// A `files` row (TS `FileRow`): the descriptor of bytes stored under `data/files/<ws>/<sha[0..2]>/<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    pub id: String,
    pub workspace_id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub created_by: String,
    pub created: i64,
}

impl FileRow {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            sha256: r.get("sha256")?,
            name: r.get("name")?,
            mime: r.get("mime")?,
            size: r.get("size")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
        })
    }

    /// `fileRef(f)`.
    pub fn file_ref(&self) -> FileRef {
        FileRef {
            id: self.id.clone(),
            name: self.name.clone(),
            mime: self.mime.clone(),
            size: self.size,
        }
    }
}
