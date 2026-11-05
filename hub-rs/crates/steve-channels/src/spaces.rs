//! Spaces (groups, DMs, topic groups) and their settings, and the
//! per-connection channel settings the inbound pipeline needs (`mode`, default rules).
//!
//! Visibility: a space of a personal connection (an account of someone's “我”, a `mode=self`
//! connection) is seen only by that user; every other space by the whole workspace.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::base::{self, DecisionKind, Viewer};
use steve_store::{ChangeEvent, NewDecision, Tx, uid};

use crate::ChannelResult;
use crate::channel::SpaceKind;

/// `subject_type` of the “新群” decision (C6 registers its handler).
pub const SUBJECT_SPACE: &str = "space";
/// Its options: `open` jumps to the space page, `mute` silences it, `quiet` (不提醒) records it
/// without ever asking you, `block` (屏蔽) stops processing it.
pub const NEW_SPACE_OPTIONS: [&str; 4] = ["open", "mute", "quiet", "block"];

/// Your attention to a space (migration 019 `space_prefs.attention`): whether
/// its messages may disturb you. Independent of Steve's role there (静默 = `muted` = role 只记录).
/// Ordered from closest to farthest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Attention {
    /// 实时: as the role and rules say.
    Realtime,
    /// 不提醒: recorded and searchable, but no 待处理, drafts, items, notifications or prompts.
    /// Your `/bot` still works here.
    Quiet,
    /// 屏蔽: not processed; new messages are not stored (the space keeps its row and settings),
    /// the list hides it unless you ask for blocked spaces. Wins over `/bot`.
    Blocked,
}

impl Attention {
    pub const ALL: [Attention; 3] = [Self::Realtime, Self::Quiet, Self::Blocked];

    pub fn code(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::Quiet => "quiet",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.code() == s)
    }

    /// The level as the page names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Realtime => "实时",
            Self::Quiet => "不提醒",
            Self::Blocked => "屏蔽",
        }
    }
}

/// Steve's role in a space, derived: what the voice identity's effective
/// rules do, `record` when muted (静默), `observe` for an identity that is not the voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 回答
    Answer,
    /// 起草（等你批）
    Draft,
    /// 只记录
    Record,
    /// 不参与: another identity speaks here, this one only observes.
    Observe,
}

/// Writes a space's attention (no revision: the callers write theirs).
pub fn write_attention(
    tx: &Tx,
    ws: &str,
    space: &str,
    level: Attention,
    now: i64,
) -> ChannelResult<()> {
    tx.execute(
        "INSERT INTO space_prefs (space_id, workspace_id, attention, updated) VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (space_id) DO UPDATE SET attention = excluded.attention, updated = excluded.updated",
        params![space, ws, level.code(), now],
    )?;
    Ok(())
}

/// The attention of a space (`realtime` when nothing was set). Other modules read it only here.
pub fn attention(c: &Connection, space: &str) -> ChannelResult<Attention> {
    let v: Option<String> = c
        .query_row(
            "SELECT attention FROM space_prefs WHERE space_id = ?",
            [space],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.as_deref()
        .and_then(Attention::parse)
        .unwrap_or(Attention::Realtime))
}

/// The voice of a space: the one identity (connection id) that speaks for Steve there:
/// the chosen one while it still sees the space, else a bot identity in it, else your own
/// account. `None` when no connected identity sees it. Other modules read it only here.
pub fn voice(c: &Connection, space: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        &format!("SELECT {VOICE_SQL} FROM spaces s WHERE s.id = ?"),
        [space],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .filter(|v| !v.is_empty()))
}

/// The manually chosen voice (`None` = automatic).
pub fn chosen_voice(c: &Connection, space: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT voice FROM space_prefs WHERE space_id = ?",
        [space],
        |r| r.get(0),
    )
    .optional()?
    .flatten())
}

/// `400` for a voice that is not an identity in this space.
pub const VOICE_NOT_HERE: &str = "这个身份不在这个群里，不能替 Steve 在这里发言";

/// Sets (or, `None`, clears to automatic) the voice of a space. `400` unless that identity sees it.
pub fn set_voice(
    tx: &Tx,
    ws: &str,
    space: &str,
    voice: Option<&str>,
    now: i64,
) -> ChannelResult<()> {
    if let Some(v) = voice
        && !identities(tx, space)?.iter().any(|i| i.connection_id == v)
    {
        return Err(ApiError::rule(VOICE_NOT_HERE));
    }
    tx.execute(
        "INSERT INTO space_prefs (space_id, workspace_id, voice, updated) VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (space_id) DO UPDATE SET voice = excluded.voice, updated = excluded.updated",
        params![space, ws, voice, now],
    )?;
    Ok(())
}

/// Names that look like an alert or notification group.
const ALERT_WORDS: [&str; 10] = [
    "告警", "报警", "通知", "预警", "监控", "提醒", "alert", "alarm", "notif", "monitor",
];

/// Why a new space looks like a bot notification group (`不提醒` is suggested in the new-space
/// prompt, never applied by itself): a name with 告警 / 通知 / alert …, or most of its recent
/// messages (at least 3 of the last 50) from bots.
pub fn quiet_reason(c: &Connection, s: &SpaceRow) -> ChannelResult<Option<String>> {
    if SpaceKind::parse(&s.kind).is_some_and(|k| k == SpaceKind::Dm) {
        return Ok(None);
    }
    let name = s.name.to_lowercase();
    if let Some(w) = ALERT_WORDS.iter().find(|w| name.contains(*w)) {
        return Ok(Some(format!("群名里有“{w}”，像是通知群")));
    }
    let (all, bots): (i64, i64) = c.query_row(
        "SELECT count(*), coalesce(sum(CASE WHEN m.role = 'bot' OR a.kind IN ('bot', 'app_bot') OR p.type = 'bot' OR p.contact_kind = 'bot' THEN 1 ELSE 0 END), 0)
           FROM (SELECT sender_account_id, sender_party_id FROM chan_messages
                  WHERE space_id = ?1 AND direction = 'in' ORDER BY created DESC LIMIT 50) x
           LEFT JOIN space_members m ON m.space_id = ?1 AND m.account_id = x.sender_account_id
           LEFT JOIN accounts a ON a.id = x.sender_account_id
           LEFT JOIN parties p ON p.id = x.sender_party_id",
        [&s.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if all >= 3 && bots * 10 >= all * 7 {
        return Ok(Some(format!("最近 {all} 条消息里 {bots} 条来自机器人")));
    }
    Ok(None)
}

/// The default rules of a bot account: mentioned or DM'd → reply.
pub fn default_bot_rules() -> Value {
    json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
}

/// The default rules of your own account: 私聊你 → draft, 群里 @你 →
/// draft, the rest only recorded.
pub fn default_self_rules() -> Value {
    json!([
        {"when": {"kind": "dm"}, "act": "draft"},
        {"when": {"kind": "mention"}, "act": "draft"},
        {"when": {"kind": "all"}, "act": "record"}
    ])
}

/// `channel_settings.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// You (`你本人`): an account of your “我”.
    #[serde(rename = "self")]
    Own,
    /// An app bot of the own organization.
    Bot,
}

impl Mode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Own => "self",
            Self::Bot => "bot",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "self" { Self::Own } else { Self::Bot }
    }
}

/// One `channel_settings` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelSettings {
    pub connection_id: String,
    pub mode: Mode,
    pub duty: String,
    pub rules: Value,
    pub trigger: Value,
    pub revision: i64,
}

fn json_or(s: Option<String>, def: Value) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(def)
}

pub fn channel_settings(
    c: &Connection,
    ws: &str,
    conn: &str,
) -> ChannelResult<Option<ChannelSettings>> {
    Ok(c
        .query_row(
            "SELECT connection_id, mode, duty, rules, trigger, revision FROM channel_settings WHERE connection_id = ? AND workspace_id = ?",
            params![conn, ws],
            |r| {
                Ok(ChannelSettings {
                    connection_id: r.get(0)?,
                    mode: Mode::parse(&r.get::<_, String>(1)?),
                    duty: r.get(2)?,
                    rules: json_or(r.get(3)?, json!([])),
                    trigger: json_or(r.get(4)?, Value::Null),
                    revision: r.get(5)?,
                })
            },
        )
        .optional()?)
}

/// The settings of a connection, created with the mode's default rules on first use (the connect
/// flow, `POST /channels`, creates them up front; connections made elsewhere get them here).
pub fn ensure_channel_settings(
    tx: &Tx,
    ws: &str,
    conn: &str,
    mode: Mode,
    now: i64,
) -> ChannelResult<ChannelSettings> {
    if let Some(s) = channel_settings(tx, ws, conn)? {
        return Ok(s);
    }
    let rules = match mode {
        Mode::Bot => default_bot_rules(),
        Mode::Own => default_self_rules(),
    };
    tx.execute(
        "INSERT OR IGNORE INTO channel_settings (connection_id, workspace_id, mode, rules, updated) VALUES (?, ?, ?, ?, ?)",
        params![conn, ws, mode.code(), rules.to_string(), now],
    )?;
    // first use of channels in this workspace: the default persona “Steve” (C2)
    crate::personas::ensure_default(tx, ws, now)?;
    channel_settings(tx, ws, conn)?.ok_or_else(|| ApiError::not_found("连接"))
}

/// One `spaces` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceRow {
    pub id: String,
    pub workspace_id: String,
    pub system_id: String,
    pub connection_id: String,
    pub kind: String,
    pub external_id: String,
    pub name: String,
    pub member_count: i64,
    pub has_external: bool,
    pub status: String,
    pub last_message: Option<i64>,
    pub created: i64,
    pub updated: i64,
    /// The connection the space was first seen through (`spaces.connection_id`); `connection_id`
    /// above is its voice.
    pub via: String,
}

impl SpaceRow {
    /// How many people the chat has, as far as anyone can tell: a one-to-one chat has one besides
    /// Steve by definition; a group what the provider counts (0: unknown).
    pub fn people(&self) -> i64 {
        if self.kind == "dm" {
            1
        } else {
            self.member_count
        }
    }
}

/// The installation of a connection ([`installation_key`] over its `config`; the connection itself
/// when it is unknown or gone).
///
/// [`installation_key`]: steve_modules::accounts::connections::installation_key
pub fn installation_of(c: &Connection, conn: &str) -> ChannelResult<String> {
    let config: Option<String> = c
        .query_row("SELECT config FROM connections WHERE id = ?", [conn], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let config: Value = config
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(steve_modules::accounts::connections::installation_key(
        conn, &config,
    ))
}

/// The voice of space `s` as SQL (see [`voice`]): the chosen identity while it still sees the space,
/// else a bot identity, else your own account, else the connection it was first seen through.
const VOICE_SQL: &str = "coalesce(
    (SELECT pf.voice FROM space_prefs pf JOIN space_identities vi ON vi.space_id = pf.space_id AND vi.connection_id = pf.voice AND vi.left_at IS NULL
       JOIN connections vc ON vc.id = vi.connection_id
      WHERE pf.space_id = s.id),
    (SELECT vi.connection_id FROM space_identities vi JOIN connections vc ON vc.id = vi.connection_id
       JOIN accounts va ON va.id = vc.account_id LEFT JOIN parties vp ON vp.id = va.party_id
      WHERE vi.space_id = s.id AND vi.left_at IS NULL
      ORDER BY vp.user_id IS NOT NULL, vi.joined, vi.connection_id LIMIT 1),
    s.connection_id, '')";

/// `connection_id` is the space's voice ([`voice`]): the identity Steve acts through here. `''`
/// once every identity was disconnected (migration 012 nulls the column). `via` is the connection
/// it was first seen through.
fn space_cols() -> String {
    format!(
        "s.id, s.workspace_id, s.system_id, {VOICE_SQL}, s.kind, s.external_id, s.name, s.member_count, s.has_external, s.status, s.last_message, s.created, s.updated, coalesce(s.connection_id, '')"
    )
}

fn space_row(r: &rusqlite::Row) -> rusqlite::Result<SpaceRow> {
    Ok(SpaceRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        system_id: r.get(2)?,
        connection_id: r.get(3)?,
        kind: r.get(4)?,
        external_id: r.get(5)?,
        name: r.get(6)?,
        member_count: r.get(7)?,
        has_external: r.get::<_, i64>(8)? != 0,
        status: r.get(9)?,
        last_message: r.get(10)?,
        created: r.get(11)?,
        updated: r.get(12)?,
        via: r.get(13)?,
    })
}

pub fn get(c: &Connection, id: &str) -> ChannelResult<Option<SpaceRow>> {
    Ok(c.query_row(
        &format!("SELECT {} FROM spaces s WHERE s.id = ?", space_cols()),
        [id],
        space_row,
    )
    .optional()?)
}

/// The space of this provider chat as connection `conn` sees it: one space per (installation,
/// external id), whichever identity saw it first — a space of this chat first seen through, or seen
/// by, a connection of the same installation.
pub fn find(c: &Connection, ws: &str, conn: &str, ext_id: &str) -> ChannelResult<Option<SpaceRow>> {
    let inst = installation_of(c, conn)?;
    let mut st = c.prepare_cached(&format!(
        "SELECT {} FROM spaces s WHERE s.workspace_id = ?1 AND s.external_id = ?2 ORDER BY s.created, s.id",
        space_cols()
    ))?;
    let rows = st
        .query_map(params![ws, ext_id], space_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ids = c.prepare_cached(
        "SELECT connection_id FROM space_identities WHERE space_id = ? ORDER BY connection_id",
    )?;
    for s in rows {
        let mut seen: Vec<String> = ids
            .query_map([&s.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !s.via.is_empty() {
            seen.push(s.via.clone());
        }
        if seen.iter().any(|x| x == conn) {
            return Ok(Some(s));
        }
        for x in &seen {
            if installation_of(c, x)? == inst {
                return Ok(Some(s));
            }
        }
    }
    Ok(None)
}

/// Records that `conn` sees the space (again).
pub fn see(tx: &Tx, ws: &str, space: &str, conn: &str, now: i64) -> ChannelResult<bool> {
    Ok(tx.execute(
        "INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (space_id, connection_id) DO UPDATE SET left_at = NULL WHERE left_at IS NOT NULL",
        params![space, conn, ws, now],
    )? > 0)
}

/// The identities that see a space (active ones), bots first.
pub fn identities(c: &Connection, space: &str) -> ChannelResult<Vec<Identity>> {
    let mut st = c.prepare_cached(
        "SELECT i.connection_id, p.user_id IS NOT NULL, coalesce(nullif(a.display_name, ''), a.handle_display), a.id
           FROM space_identities i JOIN connections cn ON cn.id = i.connection_id
           JOIN accounts a ON a.id = cn.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE i.space_id = ? AND i.left_at IS NULL
          ORDER BY p.user_id IS NOT NULL, i.joined, i.connection_id",
    )?;
    let rows = st
        .query_map([space], |r| {
            Ok(Identity {
                connection_id: r.get(0)?,
                mode: if r.get::<_, bool>(1)? {
                    Mode::Own
                } else {
                    Mode::Bot
                },
                name: r.get(2)?,
                account_id: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One identity that sees a space.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Identity {
    pub connection_id: String,
    pub mode: Mode,
    pub name: String,
    pub account_id: String,
}

/// The SQL condition “alias `s` is visible to the user bound at `me`” (a placeholder like `?2`).
///
/// Connected: not on someone else's personal account. Disconnected (`connection_id` NULL): the
/// account remembered in `s.account_id` decides the same way; if it no longer resolves (account
/// or its party gone) it fails closed, only the workspace owner sees the space.
pub(crate) fn visible_sql(me: &str) -> String {
    format!(
        "(CASE WHEN EXISTS (SELECT 1 FROM space_identities vi JOIN connections vc ON vc.id = vi.connection_id
                             WHERE vi.space_id = s.id AND vi.left_at IS NULL) THEN
            EXISTS (SELECT 1 FROM space_identities vi JOIN connections vc ON vc.id = vi.connection_id
               JOIN accounts va ON va.id = vc.account_id LEFT JOIN parties vp ON vp.id = va.party_id
              WHERE vi.space_id = s.id AND vi.left_at IS NULL AND (vp.user_id IS NULL OR vp.user_id = {me}))
          WHEN s.connection_id IS NOT NULL THEN
            NOT EXISTS (SELECT 1 FROM connections vc JOIN accounts va ON va.id = vc.account_id
              JOIN parties vp ON vp.id = va.party_id
             WHERE vc.id = s.connection_id AND vp.user_id IS NOT NULL AND vp.user_id <> {me})
          ELSE coalesce(
            (SELECT vp.user_id IS NULL OR vp.user_id = {me} FROM accounts va JOIN parties vp ON vp.id = va.party_id
              WHERE va.id = s.account_id),
            EXISTS (SELECT 1 FROM workspace_members wm
              WHERE wm.workspace_id = s.workspace_id AND wm.user_id = {me} AND wm.role = 'owner'))
          END)"
    )
}

/// Refused writes (`PATCH`, drafts) to a space whose account was disconnected.
pub const DISCONNECTED: &str = "这个空间的接入账号已经断开，只能查看";

/// `409` unless the space still has its connection: a disconnected space is kept for its history only.
pub fn writable(s: &SpaceRow) -> ChannelResult<()> {
    if s.connection_id.is_empty() {
        return Err(ApiError::conflict(DISCONNECTED));
    }
    Ok(())
}

/// Can `viewer` see this space (`own_space`)?
pub fn visible(c: &Connection, viewer: &Viewer, id: &str) -> ChannelResult<bool> {
    Ok(c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM spaces s WHERE s.id = ?1 AND s.workspace_id = ?2 AND {})",
            visible_sql("?3")
        ),
        params![id, viewer.ws, viewer.user],
        |r| r.get(0),
    )?)
}

/// `own_space(ws, me, id)`: `404 没有这个空间` unless it is the viewer's to see.
pub fn own(c: &Connection, viewer: &Viewer, id: &str) -> ChannelResult<SpaceRow> {
    if !visible(c, viewer, id)? {
        return Err(ApiError::not_found("空间"));
    }
    get(c, id)?.ok_or_else(|| ApiError::not_found("空间"))
}

/// Ids of the spaces `viewer` sees, for `space:<id>` visibility scopes and the timeline.
pub fn visible_ids(c: &Connection, viewer: &Viewer) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT s.id FROM spaces s WHERE s.workspace_id = ?1 AND {} ORDER BY s.id",
        visible_sql("?2")
    ))?;
    let rows = st
        .query_map(params![viewer.ws, viewer.user], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `space:<id>` scopes of the spaces a viewer sees, for [`steve_modules::Modules::viewer`]
/// (register with `Registry::viewer_scopes`): events and messages with a `space:` visibility.
pub struct SpaceScopes;

impl steve_modules::ViewerScopes for SpaceScopes {
    fn scopes(&self, c: &Connection, viewer: &Viewer) -> ChannelResult<Vec<String>> {
        Ok(visible_ids(c, viewer)?
            .into_iter()
            .map(|id| format!("space:{id}"))
            .collect())
    }
}

/// `GET /spaces?connection=&q=`: newest activity first.
pub fn list(
    c: &Connection,
    viewer: &Viewer,
    conn: Option<&str>,
    q: Option<&str>,
) -> ChannelResult<Vec<SpaceRow>> {
    let like = q.map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let mut st = c.prepare_cached(&format!(
        "SELECT {} FROM spaces s WHERE s.workspace_id = ?1 AND {} AND (?3 IS NULL OR s.connection_id = ?3
              OR EXISTS (SELECT 1 FROM space_identities li WHERE li.space_id = s.id AND li.connection_id = ?3 AND li.left_at IS NULL))
            AND (?4 IS NULL OR s.name LIKE ?4 ESCAPE '\\')
          ORDER BY coalesce(s.last_message, s.created) DESC, s.id",
        space_cols(),
        visible_sql("?2")
    ))?;
    let rows = st
        .query_map(params![viewer.ws, viewer.user, conn, like], space_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Default disclosure of a new space's content: customer groups (external members), DMs and
/// mail `reasoning`; internal groups `conclusion`.
pub fn default_disclosure(kind: SpaceKind, has_external: bool) -> &'static str {
    if has_external || matches!(kind, SpaceKind::Dm | SpaceKind::MailThread) {
        "reasoning"
    } else {
        "conclusion"
    }
}

/// A space to create.
#[derive(Debug, Clone)]
pub struct NewSpace<'a> {
    pub ws: &'a str,
    pub system_id: &'a str,
    pub connection_id: &'a str,
    pub kind: SpaceKind,
    pub external_id: &'a str,
    pub name: &'a str,
    pub member_count: i64,
    pub has_external: bool,
    pub now: i64,
}

/// Finds or creates the space and, when new, its settings (`configured=0`). Returns the row and
/// whether it was created now. Two deliveries racing for the same space create it once.
pub fn locate_or_create(tx: &Tx, n: &NewSpace) -> ChannelResult<(SpaceRow, bool)> {
    if let Some(s) = find(tx, n.ws, n.connection_id, n.external_id)? {
        if see(tx, n.ws, &s.id, n.connection_id, n.now)? {
            event(tx, n.ws, &s.id);
        }
        return Ok((s, false));
    }
    let id = uid("spc");
    let created = tx.execute(
        "INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, member_count, has_external, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (workspace_id, connection_id, external_id) DO NOTHING",
        params![
            id,
            n.ws,
            n.system_id,
            n.connection_id,
            n.kind.code(),
            n.external_id,
            n.name,
            n.member_count,
            n.has_external as i64,
            n.now,
            n.now
        ],
    )? == 1;
    if created {
        see(tx, n.ws, &id, n.connection_id, n.now)?;
        tx.execute(
            "INSERT INTO space_settings (space_id, workspace_id, disclosure, item_defaults, updated) VALUES (?, ?, ?, ?, ?)",
            params![
                id,
                n.ws,
                default_disclosure(n.kind, n.has_external),
                json!({"tags": [], "project_id": null, "bot_id": null}).to_string(),
                n.now
            ],
        )?;
        event(tx, n.ws, &id);
    }
    let s = find(tx, n.ws, n.connection_id, n.external_id)?
        .ok_or_else(|| ApiError::not_found("空间"))?;
    Ok((s, created))
}

/// Opens the “新群” decision for a space a bot account just found itself in.
pub fn open_new_space_decision(
    tx: &Tx,
    ws: &str,
    space: &SpaceRow,
    account_name: &str,
) -> ChannelResult<String> {
    if let Some(d) = base::decisions::open_for(tx, ws, SUBJECT_SPACE, &space.id)? {
        return Ok(d.id);
    }
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: ws.into(),
            kind: DecisionKind::Ask.code().into(),
            subject_type: SUBJECT_SPACE.into(),
            subject_id: space.id.clone(),
            item_id: None,
            session_id: None,
            question: format!(
                "{account_name}被拉进了「{}」，要设置它在这里做什么吗？",
                space.name
            ),
            draft: None,
            options: Some(NEW_SPACE_OPTIONS.map(String::from).to_vec()),
            requested_by: "steve".into(),
        },
    )?;
    Ok(d.id)
}

/// Adds a member (kept if already there; a known role is not downgraded).
pub fn add_member(
    tx: &Tx,
    ws: &str,
    space: &str,
    account: &str,
    role: &str,
    now: i64,
) -> ChannelResult<bool> {
    Ok(tx.execute(
        "INSERT OR IGNORE INTO space_members (workspace_id, space_id, account_id, role, joined) VALUES (?, ?, ?, ?, ?)",
        params![ws, space, account, role, now],
    )? == 1)
}

pub fn is_member(c: &Connection, space: &str, account: &str) -> ChannelResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ? AND account_id = ?)",
        params![space, account],
        |r| r.get(0),
    )?)
}

/// After a message: `last_message`, and `has_external` once an external sender shows up.
pub fn touch(tx: &Tx, id: &str, at: i64, external: bool) -> ChannelResult<()> {
    tx.execute(
        "UPDATE spaces SET last_message = max(coalesce(last_message, 0), ?1),
            has_external = CASE WHEN ?2 THEN 1 ELSE has_external END WHERE id = ?3",
        params![at, external, id],
    )?;
    Ok(())
}

/// SSE `{type:"space", id, ws}`.
pub fn event(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Space {
        id: id.into(),
        ws: ws.into(),
    });
}

// ---------------------------------------------------------------- settings

/// One `space_settings` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceSettings {
    pub duty: String,
    /// `None` = follow the account.
    pub rules: Option<Value>,
    pub flow_id: Option<String>,
    pub muted: bool,
    pub collab: bool,
    pub memory_scope: String,
    pub disclosure: String,
    pub item_defaults: Value,
    pub configured: bool,
    pub revision: i64,
    /// Your attention (019), independent of `muted` (Steve's role 只记录).
    pub attention: Attention,
}

pub fn settings(c: &Connection, space: &str) -> ChannelResult<SpaceSettings> {
    c.query_row(
        "SELECT s.duty, s.rules, s.flow_id, s.muted, s.collab, s.memory_scope, s.disclosure, s.item_defaults, s.configured, s.revision, a.attention
           FROM space_settings s LEFT JOIN space_prefs a ON a.space_id = s.space_id WHERE s.space_id = ?",
        [space],
        |r| {
            let muted = r.get::<_, i64>(3)? != 0;
            Ok(SpaceSettings {
                duty: r.get(0)?,
                rules: r
                    .get::<_, Option<String>>(1)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                flow_id: r.get(2)?,
                muted,
                collab: r.get::<_, i64>(4)? != 0,
                memory_scope: r.get(5)?,
                disclosure: r.get(6)?,
                item_defaults: json_or(r.get(7)?, json!({})),
                configured: r.get::<_, i64>(8)? != 0,
                revision: r.get(9)?,
                attention: r
                    .get::<_, Option<String>>(10)?
                    .as_deref()
                    .and_then(Attention::parse)
                    .unwrap_or(Attention::Realtime),
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("空间"))
}

/// [`settings`], or for a space whose account was disconnected (its settings are deleted)
/// what a new space of its kind starts with, so it can still be shown.
pub fn settings_or_defaults(c: &Connection, s: &SpaceRow) -> ChannelResult<SpaceSettings> {
    match settings(c, &s.id) {
        Err(ApiError::NotFound(_)) if s.connection_id.is_empty() => Ok(SpaceSettings {
            duty: String::new(),
            rules: None,
            flow_id: None,
            muted: false,
            collab: false,
            memory_scope: "space".into(),
            disclosure: default_disclosure(
                SpaceKind::parse(&s.kind).unwrap_or(SpaceKind::Group),
                s.has_external,
            )
            .into(),
            item_defaults: json!({}),
            configured: true,
            revision: 0,
            attention: Attention::Realtime,
        }),
        r => r,
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

/// The `Space` shape. `persona` is `{id, name, inherited}`, `null` for your own accounts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpaceOut {
    pub id: String,
    pub connection_id: String,
    pub kind: String,
    pub name: String,
    pub member_count: i64,
    pub has_external: bool,
    pub status: String,
    pub persona: Option<crate::bindings::PersonaRef>,
    pub duty: String,
    pub rules: Option<Value>,
    pub effective_rules: Value,
    pub flow_id: Option<String>,
    pub muted: bool,
    pub collab: bool,
    pub memory_scope: String,
    pub disclosure: String,
    pub item_defaults: Value,
    pub configured: bool,
    pub teams: Vec<String>,
    pub revision: i64,
    pub last_message: Option<i64>,
    /// Your attention: `realtime|quiet|blocked` (实时 / 不提醒 / 屏蔽).
    pub attention: Attention,
    /// Steve's role here (derived from the voice's rules and `muted`).
    pub role: Role,
    /// The identity that speaks for Steve here, `null` when none sees it any more.
    pub voice: Option<VoiceOut>,
    /// The identities that see the space (bots first); all but the voice only observe.
    pub identities: Vec<Identity>,
    /// What lists and headers call it: a DM is named after the other party, your chat with yourself
    /// “你自己”, a group by its name.
    pub title: String,
    /// The other party of a DM (`null` for groups and your chat with yourself).
    pub counterpart: Option<Counterpart>,
    /// Your chat with yourself (`kind` is then `self_dm`).
    pub self_chat: bool,
    /// Only for a space not set up yet: why it looks like a notification group (suggest 不提醒).
    pub suggest_quiet: Option<String>,
    /// Settings that are on but do not apply here, and why (屏蔽 > `/bot` >
    /// 不提醒 > role rules), so the page shows it next to the setting.
    pub conflicts: Vec<Conflict>,
}

/// [`SpaceOut::voice`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoiceOut {
    pub connection_id: String,
    pub name: String,
    pub mode: Mode,
    /// Chosen on the space page (else automatic).
    pub chosen: bool,
}

/// One setting that does not apply here, and why (`key`: `rules` | `flow` | `trigger` | `voice`;
/// `level`: `conflict` = it does not apply, `info` = it still does, or what applies instead).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub key: &'static str,
    pub level: &'static str,
    pub text: String,
}

/// The acts of rules, as codes.
fn acts(rules: &Value) -> Vec<String> {
    rules
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r["act"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Steve's role from the voice's effective rules (`muted`, 不提醒 and 屏蔽 = 只记录).
pub fn role_of(set: &SpaceSettings, rules: &Value, voice: Option<Mode>) -> Role {
    let acts = acts(rules);
    let has = |a: &str| acts.iter().any(|x| x == a);
    match voice {
        None => Role::Observe,
        // 只记录, and under 不提醒 / 屏蔽 the rules do not apply either
        Some(_) if set.muted || set.attention >= Attention::Quiet => Role::Record,
        Some(Mode::Bot) if has("reply") => Role::Answer,
        Some(_) if has("reply") || has("draft") || has("create_item") || has("handoff") => {
            Role::Draft
        }
        Some(_) => Role::Record,
    }
}

/// Your `/bot` settings (your own account's trigger words), wherever they were set.
pub fn your_trigger(c: &Connection, ws: &str) -> ChannelResult<Option<crate::rules::Trigger>> {
    let v: Option<String> = c
        .query_row(
            "SELECT trigger FROM channel_settings WHERE workspace_id = ? AND mode = 'self' ORDER BY updated DESC LIMIT 1",
            [ws],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| crate::rules::Trigger::from_value(&v)))
}

/// What [`SpaceOut::conflicts`] says for a space with these settings.
pub fn conflicts(
    s: &SpaceRow,
    set: &SpaceSettings,
    rules: &Value,
    trigger: Option<&crate::rules::Trigger>,
    ids: &[Identity],
    chosen: Option<&str>,
) -> Vec<Conflict> {
    let mut out = Vec::new();
    let c = |key, level, text: String| Conflict { key, level, text };
    let speaking = acts(rules)
        .iter()
        .any(|a| matches!(a.as_str(), "reply" | "draft" | "create_item"));
    match set.attention {
        Attention::Realtime => {
            if set.muted && set.flow_id.is_none() && speaking {
                out.push(c(
                    "rules",
                    "conflict",
                    "只记录：回复、起草和建事项的规则暂不生效；转给你的仍进待处理".into(),
                ));
            }
        }
        Attention::Quiet => {
            out.push(c(
                "rules",
                "conflict",
                "不提醒：响应规则都不生效，只记录；不进待处理，也不起草".into(),
            ));
            if set.flow_id.is_some() {
                out.push(c(
                    "flow",
                    "conflict",
                    "不提醒：这里的消息不会触发流程".into(),
                ));
            }
        }
        Attention::Blocked => {
            out.push(c(
                "rules",
                "conflict",
                "已屏蔽：新消息不处理也不保存，响应规则不生效".into(),
            ));
            if set.flow_id.is_some() {
                out.push(c(
                    "flow",
                    "conflict",
                    "已屏蔽：这里的消息不会触发流程".into(),
                ));
            }
        }
    }
    if let Some(t) = trigger {
        let dm = SpaceKind::parse(&s.kind) == Some(SpaceKind::Dm);
        if t.on && t.applies(&s.id, dm) {
            let word = t.words.first().map_or("/bot", String::as_str);
            match set.attention {
                Attention::Blocked => out.push(c(
                    "trigger",
                    "conflict",
                    format!("这里已屏蔽，{word} 在这里不生效"),
                )),
                Attention::Quiet => out.push(c(
                    "trigger",
                    "info",
                    format!("{word} 在这里仍然生效：它是你的明确指令"),
                )),
                Attention::Realtime if set.muted => out.push(c(
                    "trigger",
                    "info",
                    format!("{word} 在这里仍然生效：它是你的明确指令"),
                )),
                Attention::Realtime => {}
            }
        }
    }
    if let Some(v) = chosen
        && !ids.iter().any(|i| i.connection_id == v)
    {
        out.push(c(
            "voice",
            "conflict",
            match ids.first() {
                Some(i) => format!("指定的发言身份已经不在这里，改由「{}」发言", i.name),
                None => "指定的发言身份已经不在这里".into(),
            },
        ));
    }
    if ids.len() > 1
        && let Some(v) = ids.iter().find(|i| i.connection_id == s.connection_id)
    {
        let others: Vec<&str> = ids
            .iter()
            .filter(|i| i.connection_id != v.connection_id)
            .map(|i| i.name.as_str())
            .collect();
        out.push(c(
            "voice",
            "info",
            format!(
                "由「{}」替 Steve 发言；「{}」在这里只观察，不起草也不回复",
                v.name,
                others.join("」「")
            ),
        ));
    }
    out
}

/// The other party of a DM.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Counterpart {
    pub account_id: String,
    pub party_id: Option<String>,
    pub name: String,
    pub is_bot: bool,
}

/// What the page calls a chat with yourself.
pub const SELF_CHAT_TITLE: &str = "你自己";
/// A DM nobody could be named for yet (the old stored default).
pub const DM_FALLBACK: &str = "私聊";

/// `(counterpart, self_chat)` of a DM: another member, else another sender; none at all on a
/// personal account whose chat carries your own name (or none) = your chat with yourself.
pub fn dm_party(c: &Connection, s: &SpaceRow) -> ChannelResult<(Option<Counterpart>, bool)> {
    let mine: Vec<String> = identities(c, &s.id)?
        .into_iter()
        .map(|i| i.account_id)
        .collect();
    let own: Option<(String, Option<String>, String, Option<String>)> = c
        .query_row(
            "SELECT a.id, p.user_id, coalesce(nullif(a.display_name, ''), a.handle_display), p.name
               FROM spaces s JOIN accounts a ON a.id = coalesce((SELECT account_id FROM connections WHERE id = s.connection_id), s.account_id)
               LEFT JOIN parties p ON p.id = a.party_id WHERE s.id = ?",
            [&s.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let own_id = own.as_ref().map(|o| o.0.clone()).unwrap_or_default();
    // every identity's account is “us”, not the other party
    let mut us = mine;
    us.push(own_id);
    let us = serde_json::to_string(&us)?;
    let other = c
        .query_row(
            "SELECT a.id, a.party_id, coalesce(nullif(p.name, ''), nullif(a.display_name, ''), a.handle_display),
                    a.kind IN ('bot', 'app_bot') OR coalesce(p.type, '') = 'bot' OR coalesce(p.contact_kind, '') = 'bot'
               FROM accounts a LEFT JOIN parties p ON p.id = a.party_id
              WHERE a.id = coalesce(
                (SELECT account_id FROM space_members WHERE space_id = ?1 AND account_id NOT IN (SELECT value FROM json_each(?2))
                  ORDER BY role = 'bot', joined LIMIT 1),
                (SELECT sender_account_id FROM chan_messages WHERE space_id = ?1 AND direction = 'in'
                    AND sender_account_id IS NOT NULL AND sender_account_id NOT IN (SELECT value FROM json_each(?2)) ORDER BY created LIMIT 1))",
            params![s.id, us],
            |r| {
                Ok(Counterpart {
                    account_id: r.get(0)?,
                    party_id: r.get(1)?,
                    name: r.get(2)?,
                    is_bot: r.get(3)?,
                })
            },
        )
        .optional()?;
    if other.is_some() {
        return Ok((other, false));
    }
    let self_chat = own.is_some_and(|(_, user, acc_name, party_name)| {
        let n = s.name.trim();
        user.is_some()
            && (n.is_empty()
                || n == DM_FALLBACK
                || n == acc_name.trim()
                || party_name.is_some_and(|p| p.trim() == n))
    });
    Ok((None, self_chat))
}

pub fn out(c: &Connection, s: &SpaceRow) -> ChannelResult<SpaceOut> {
    out_with(c, s, &your_trigger(c, &s.workspace_id)?)
}

/// [`out`] with the workspace's [`your_trigger`] already read (a list of spaces reads it once).
pub fn out_with(
    c: &Connection,
    s: &SpaceRow,
    trigger: &Option<crate::rules::Trigger>,
) -> ChannelResult<SpaceOut> {
    let set = settings_or_defaults(c, s)?;
    let account = channel_settings(c, &s.workspace_id, &s.connection_id)?;
    let account_rules = account
        .as_ref()
        .map(|x| x.rules.clone())
        .unwrap_or_else(default_bot_rules);
    let ids = identities(c, &s.id)?;
    let chosen = chosen_voice(c, &s.id)?;
    let voice = ids
        .iter()
        .find(|i| i.connection_id == s.connection_id)
        .map(|i| VoiceOut {
            connection_id: i.connection_id.clone(),
            name: i.name.clone(),
            mode: i.mode,
            chosen: chosen.as_deref() == Some(i.connection_id.as_str()),
        });
    let effective = set.rules.clone().unwrap_or_else(|| account_rules.clone());
    let role = role_of(&set, &effective, voice.as_ref().map(|v| v.mode));
    let mut conflicts = conflicts(
        s,
        &set,
        &effective,
        trigger.as_ref(),
        &ids,
        chosen.as_deref(),
    );
    if let Some(v) = voice.as_ref().filter(|v| v.mode == Mode::Own)
        && acts(&effective)
            .iter()
            .any(|a| matches!(a.as_str(), "reply" | "draft"))
        && !can_send_as_you(c, &v.connection_id)?
    {
        conflicts.push(Conflict {
            key: "rules",
            level: "conflict",
            text: "没有“以你本人身份发送”的授权：这里不替你起草或回复，只记录".into(),
        });
    }
    let mut st = c.prepare_cached(
        "SELECT party_id FROM team_details WHERE workspace_id = ? AND space_id = ? ORDER BY party_id",
    )?;
    let teams = st
        .query_map(params![s.workspace_id, s.id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let kind = SpaceKind::parse(&s.kind);
    let (counterpart, self_chat) = if kind == Some(SpaceKind::Dm) {
        dm_party(c, s)?
    } else {
        (None, false)
    };
    let title = if self_chat {
        SELF_CHAT_TITLE.to_string()
    } else if let Some(p) = &counterpart {
        p.name.clone()
    } else if s.name.trim().is_empty() {
        kind.map_or("群", |k| {
            if k == SpaceKind::Dm {
                DM_FALLBACK
            } else {
                "群"
            }
        })
        .to_string()
    } else {
        s.name.clone()
    };
    let member_count = if s.member_count > 0 {
        s.member_count
    } else {
        c.query_row(
            "SELECT count(*) FROM space_members WHERE space_id = ?",
            [&s.id],
            |r| r.get(0),
        )?
    };
    let suggest_quiet = if !set.configured && set.attention == Attention::Realtime {
        quiet_reason(c, s)?
    } else {
        None
    };
    let kind_out = match kind {
        Some(SpaceKind::Dm) if self_chat => "self_dm".to_string(),
        Some(SpaceKind::Channel) => "topic_group".to_string(),
        _ => s.kind.clone(),
    };
    Ok(SpaceOut {
        id: s.id.clone(),
        connection_id: s.connection_id.clone(),
        kind: kind_out,
        name: s.name.clone(),
        member_count,
        has_external: s.has_external,
        status: s.status.clone(),
        persona: crate::bindings::space_persona(c, s)?,
        duty: set.duty,
        effective_rules: set.rules.clone().unwrap_or(account_rules),
        rules: set.rules,
        flow_id: set.flow_id,
        muted: set.muted,
        collab: set.collab,
        memory_scope: set.memory_scope,
        disclosure: set.disclosure,
        item_defaults: item_defaults(&set.item_defaults),
        configured: set.configured,
        teams,
        revision: set.revision,
        last_message: s.last_message,
        attention: set.attention,
        role,
        voice,
        identities: ids,
        title,
        counterpart,
        self_chat,
        suggest_quiet,
        conflicts,
    })
}

/// `PATCH /spaces/:id` fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacePatch {
    /// Absent = unchanged; `null` = follow the account; a persona id = this space's own persona.
    #[serde(default, deserialize_with = "some_nullable")]
    pub persona_id: Option<Option<String>>,
    pub duty: Option<String>,
    /// Absent = unchanged; `null` = follow the account; a list = this space's own rules.
    #[serde(default, deserialize_with = "some_nullable_value")]
    pub rules: Option<Option<Value>>,
    pub muted: Option<bool>,
    /// `realtime|quiet|blocked` (your attention; independent of `muted`, Steve's role 只记录).
    pub attention: Option<String>,
    /// Absent = unchanged; `null` = automatic; a connection id = that identity speaks here (it must
    /// see the space, else `400`).
    #[serde(default, deserialize_with = "some_nullable")]
    pub voice: Option<Option<String>>,
    pub collab: Option<bool>,
    pub memory_scope: Option<String>,
    pub disclosure: Option<String>,
    pub item_defaults: Option<ItemDefaults>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ItemDefaults {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

/// Keeps `null` apart from an absent field (`Some(None)` vs `None`).
fn some_nullable<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

fn some_nullable_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<Value>>, D::Error> {
    Option::<Value>::deserialize(d).map(|v| Some(v.filter(|v| !v.is_null())))
}

pub const MEMORY_SCOPES: [&str; 2] = ["space", "persona"];
pub const LEVELS: [&str; 4] = ["public", "conclusion", "reasoning", "never"];

fn exists(c: &Connection, sql: &str, a: &str, b: &str) -> ChannelResult<bool> {
    Ok(c.query_row(sql, params![a, b], |r| r.get(0))?)
}

impl SpacePatch {
    pub fn check(&self) -> ChannelResult<()> {
        let mut c = steve_modules::Check::new();
        c.opt_len("duty", self.duty.as_deref(), 0, 4000);
        if let Some(m) = &self.memory_scope {
            c.one_of("memory_scope", m, &MEMORY_SCOPES);
        }
        if let Some(d) = &self.disclosure {
            c.one_of("disclosure", d, &LEVELS);
        }
        if let Some(a) = &self.attention {
            c.that("attention", Attention::parse(a).is_some());
        }
        if let Some(d) = &self.item_defaults {
            c.that("item_defaults.tags", d.tags.len() <= 20);
        }
        c.done()
    }
}

/// Applies a patch: `configured=1`, a revision, audit `space.patch`, SSE `space`.
pub fn patch(
    tx: &Tx,
    viewer: &Viewer,
    id: &str,
    p: &SpacePatch,
    now: i64,
) -> ChannelResult<SpaceRow> {
    p.check()?;
    let s = own(tx, viewer, id)?;
    writable(&s)?;
    let ws = &viewer.ws;
    if let Some(d) = &p.item_defaults {
        if let Some(b) = &d.bot_id
            && !exists(
                tx,
                "SELECT EXISTS (SELECT 1 FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?1 WHERE b.id = ?2 AND b.deleted = 0)",
                ws,
                b,
            )?
        {
            return Err(ApiError::not_found("Bot"));
        }
        if let Some(pr) = &d.project_id
            && !exists(
                tx,
                "SELECT EXISTS (SELECT 1 FROM projects WHERE workspace_id = ?1 AND id = ?2)",
                ws,
                pr,
            )?
        {
            return Err(ApiError::not_found("项目"));
        }
        for t in &d.tags {
            if !exists(
                tx,
                "SELECT EXISTS (SELECT 1 FROM tags WHERE workspace_id = ?1 AND id = ?2)",
                ws,
                t,
            )? {
                return Err(ApiError::not_found("标签"));
            }
        }
    }
    let before = settings(tx, id)?;
    let rules = match &p.rules {
        Some(Some(v)) => {
            if let Some(f) = &before.flow_id {
                return Err(ApiError::conflict(format!(
                    "这个空间已经转成流程「{}」，先改回规则",
                    steve_flow::flows::name_of(tx, f)?
                )));
            }
            let mode = crate::bindings::connection_account(tx, ws, &s.connection_id)?.1;
            let parsed = crate::rules::parse(v, mode)?;
            crate::rules::check_refs(tx, ws, &parsed)?;
            Some(Some(crate::rules::to_value(&parsed)))
        }
        Some(None) => Some(None),
        None => None,
    };
    let persona_before = crate::bindings::space_binding(tx, &s)?;
    if let Some(pid) = &p.persona_id {
        crate::bindings::set_space(tx, &s, pid.as_deref(), now)?;
    }
    let persona_after = crate::bindings::space_binding(tx, &s)?;
    let mut after = before.clone();
    if let Some(r) = rules {
        after.rules = r;
    }
    if let Some(v) = &p.duty {
        after.duty = v.trim().to_string();
    }
    if let Some(v) = p.muted {
        after.muted = v;
    }
    if let Some(a) = p.attention.as_deref().and_then(Attention::parse) {
        after.attention = a;
    }
    if let Some(v) = p.collab {
        after.collab = v;
    }
    if let Some(v) = &p.memory_scope {
        after.memory_scope = v.clone();
    }
    if let Some(v) = &p.disclosure {
        after.disclosure = v.clone();
    }
    if let Some(v) = &p.item_defaults {
        after.item_defaults = serde_json::to_value(v)?;
    }
    after.configured = true;
    after.revision = before.revision + 1;
    tx.execute(
        "UPDATE space_settings SET duty = ?, rules = ?, muted = ?, collab = ?, memory_scope = ?, disclosure = ?, item_defaults = ?,
            configured = 1, revision = ?, updated = ? WHERE space_id = ?",
        params![
            after.duty,
            after.rules.as_ref().map(Value::to_string),
            after.muted as i64,
            after.collab as i64,
            after.memory_scope,
            after.disclosure,
            after.item_defaults.to_string(),
            after.revision,
            now,
            id
        ],
    )?;
    let voice_before = chosen_voice(tx, id)?;
    if let Some(v) = &p.voice
        && *v != voice_before
    {
        set_voice(tx, ws, id, v.as_deref(), now)?;
    }
    if after.attention != before.attention {
        write_attention(tx, ws, id, after.attention, now)?;
        if after.attention >= Attention::Quiet {
            // 不提醒 / 屏蔽: no “新群” prompt left waiting for this space
            crate::space_decisions::expire_for_space(
                tx,
                ws,
                id,
                &format!("已设为{}", after.attention.label()),
            )?;
        }
    }
    let voice_after = chosen_voice(tx, id)?;
    let snap = |x: &SpaceSettings, persona: &Option<String>, voice: &Option<String>| {
        json!({
            "persona_id": persona, "duty": x.duty, "rules": x.rules, "muted": x.muted, "attention": x.attention.code(), "voice": voice, "collab": x.collab, "memory_scope": x.memory_scope,
            "disclosure": x.disclosure, "item_defaults": item_defaults(&x.item_defaults),
        })
    };
    base::revision(
        tx,
        ws,
        "space",
        id,
        after.revision,
        &snap(&before, &persona_before, &voice_before),
        &snap(&after, &persona_after, &voice_after),
        &viewer.user,
    )?;
    base::audit(
        tx,
        ws,
        &viewer.user,
        "space.patch",
        id,
        &json!({ "fields": patched_fields(p) }),
    )?;
    event(tx, ws, id);
    Ok(s)
}

fn patched_fields(p: &SpacePatch) -> Vec<&'static str> {
    [
        ("persona_id", p.persona_id.is_some()),
        ("duty", p.duty.is_some()),
        ("rules", p.rules.is_some()),
        ("muted", p.muted.is_some()),
        ("attention", p.attention.is_some()),
        ("voice", p.voice.is_some()),
        ("collab", p.collab.is_some()),
        ("memory_scope", p.memory_scope.is_some()),
        ("disclosure", p.disclosure.is_some()),
        ("item_defaults", p.item_defaults.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, on)| on.then_some(k))
    .collect()
}

/// Sets the default rules of an account (`PATCH /channels/:connection_id` `rules`): validated
/// (your own account cannot `reply`), a revision of `channel`, audit `channel.patch`. A
/// personal connection of someone else is `404 没有这个连接`.
pub fn set_account_rules(
    tx: &Tx,
    viewer: &Viewer,
    conn: &str,
    rules: &Value,
    now: i64,
) -> ChannelResult<ChannelSettings> {
    let ws = &viewer.ws;
    let owner: Option<Option<String>> = tx
        .query_row(
            "SELECT p.user_id FROM connections c JOIN accounts a ON a.id = c.account_id LEFT JOIN parties p ON p.id = a.party_id
              WHERE c.id = ? AND c.workspace_id = ?",
            params![conn, ws],
            |r| r.get(0),
        )
        .optional()?;
    match owner {
        None => return Err(ApiError::not_found("连接")),
        Some(Some(u)) if u != viewer.user => return Err(ApiError::not_found("连接")),
        _ => {}
    }
    let (_, mode) = crate::bindings::connection_account(tx, ws, conn)?;
    let before = ensure_channel_settings(tx, ws, conn, mode, now)?;
    let parsed = crate::rules::parse(rules, before.mode)?;
    crate::rules::check_refs(tx, ws, &parsed)?;
    let after = crate::rules::to_value(&parsed);
    let version = before.revision + 1;
    tx.execute(
        "UPDATE channel_settings SET rules = ?, revision = ?, updated = ? WHERE connection_id = ?",
        params![after.to_string(), version, now, conn],
    )?;
    base::revision(
        tx,
        ws,
        "channel",
        conn,
        version,
        &json!({ "rules": before.rules }),
        &json!({ "rules": after }),
        &viewer.user,
    )?;
    base::audit(
        tx,
        ws,
        &viewer.user,
        "channel.patch",
        conn,
        &json!({ "fields": ["rules"] }),
    )?;
    tx.publish(ChangeEvent::Channel {
        id: conn.into(),
        ws: ws.clone(),
    });
    channel_settings(tx, ws, conn)?.ok_or_else(|| ApiError::not_found("连接"))
}

/// Silences a space (the loop guard's automatic mute).
pub fn mute(tx: &Tx, ws: &str, id: &str, now: i64) -> ChannelResult<bool> {
    let n = tx.execute(
        "UPDATE space_settings SET muted = 1, updated = ? WHERE space_id = ? AND muted = 0",
        params![now, id],
    )?;
    if n > 0 {
        event(tx, ws, id);
    }
    Ok(n > 0)
}

/// [`steve_modules::contacts::SpacePort`]: a team's bound group must be a space the viewer sees.
pub struct Spaces;

impl steve_modules::contacts::SpacePort for Spaces {
    fn visible(&self, c: &Connection, viewer: &Viewer, space_id: &str) -> ChannelResult<bool> {
        visible(c, viewer, space_id)
    }

    fn seen(
        &self,
        c: &Connection,
        viewer: &Viewer,
        parties: &[String],
    ) -> ChannelResult<Vec<(String, steve_modules::contacts::Seen)>> {
        crate::seen::seen(c, viewer, parties)
    }
}

/// The capability for sending as you.
pub const SEND_AS_USER_CAP: &str = "msg.send_as_user";

/// Whether `connection` may send as you: its enabled caps include `msg.send_as_user` (the grant
/// allows it; feishu `available_caps`). Not a provider call.
pub fn can_send_as_you(c: &rusqlite::Connection, connection: &str) -> ChannelResult<bool> {
    let caps: Option<String> = c
        .query_row(
            "SELECT caps FROM connections WHERE id = ? AND status != 'revoked'",
            [connection],
            |r| r.get(0),
        )
        .optional()?;
    Ok(caps
        .and_then(|c| serde_json::from_str::<Vec<String>>(&c).ok())
        .is_some_and(|c| c.iter().any(|x| x == SEND_AS_USER_CAP)))
}
