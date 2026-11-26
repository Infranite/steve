//! Sidecar metadata storage for file-as-single-source-of-truth memories:
//! - [`SourceRole`] and [`WriteMode`] on `kb_sources`
//! - [`MemoryEntries`] (`memory_entries`: sidecar annotations only, never duplicate prose)
//! - [`MemoryEvidences`] (`memory_evidence`: episode provenance)
//! - [`Observations`] (`observations`: 3-gate corroboration buffer)
//! - [`RejectedPatterns`] (`rejected_patterns`: tombstones preventing recurrence)
//! - [`MemoryRevisions`] (`memory_revisions`: propose queue + direct-write audit & revert)

use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};

use crate::base::{Level, Visibility};
use crate::contract::Row;
use crate::{Read, Result, W, uid};

/// Whether a source provides reference knowledge or personal/entity memory files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    #[default]
    Knowledge,
    Memory,
}

impl SourceRole {
    pub fn code(self) -> &'static str {
        match self {
            Self::Knowledge => "knowledge",
            Self::Memory => "memory",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "knowledge" => Some(Self::Knowledge),
            "memory" => Some(Self::Memory),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Knowledge)
    }
}

/// How Steve is permitted to interact with files in a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    #[default]
    ReadOnly,
    Propose,
    DirectWrite,
}

impl WriteMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Propose => "propose",
            Self::DirectWrite => "direct_write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read_only" => Some(Self::ReadOnly),
            "propose" => Some(Self::Propose),
            "direct_write" => Some(Self::DirectWrite),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::ReadOnly)
    }
}

/// Deterministic confidence tier computed solely from typed provenance rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceTier {
    Verified,
    Corroborated,
    Provisional,
    Contested,
}

impl ConfidenceTier {
    pub fn code(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Corroborated => "corroborated",
            Self::Provisional => "provisional",
            Self::Contested => "contested",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "verified" => Some(Self::Verified),
            "corroborated" => Some(Self::Corroborated),
            "provisional" => Some(Self::Provisional),
            "contested" => Some(Self::Contested),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Provisional)
    }
}

/// Semantic kind of a file-backed memory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Rule,
    Preference,
    Fact,
    Event,
    Overview,
}

impl MemoryKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Preference => "preference",
            Self::Fact => "fact",
            Self::Event => "event",
            Self::Overview => "overview",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rule" => Some(Self::Rule),
            "preference" => Some(Self::Preference),
            "fact" => Some(Self::Fact),
            "event" => Some(Self::Event),
            "overview" => Some(Self::Overview),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Fact)
    }

    /// Lower number = higher prompt assembly priority.
    pub fn priority(self) -> u8 {
        match self {
            Self::Rule => 0,
            Self::Preference => 1,
            Self::Fact => 2,
            Self::Event => 3,
            Self::Overview => 4,
        }
    }
}

/// Origin class driving deterministic confidence and admission rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginClass {
    OwnerFile,
    OwnerExplicit,
    OwnerRework,
    OwnerDraftDiff,
    OwnerEdited,
    ImplicitPattern,
    ExternalEntity,
}

impl OriginClass {
    pub fn code(self) -> &'static str {
        match self {
            Self::OwnerFile => "owner_file",
            Self::OwnerExplicit => "owner_explicit",
            Self::OwnerRework => "owner_rework",
            Self::OwnerDraftDiff => "owner_draft_diff",
            Self::OwnerEdited => "owner_edited",
            Self::ImplicitPattern => "implicit_pattern",
            Self::ExternalEntity => "external_entity",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner_file" => Some(Self::OwnerFile),
            "owner_explicit" => Some(Self::OwnerExplicit),
            "owner_rework" => Some(Self::OwnerRework),
            "owner_draft_diff" => Some(Self::OwnerDraftDiff),
            "owner_edited" => Some(Self::OwnerEdited),
            "implicit_pattern" => Some(Self::ImplicitPattern),
            "external_entity" => Some(Self::ExternalEntity),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::ImplicitPattern)
    }
}

/// Source trust tier for external prompt-injection and rule-pollution isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTrustTier {
    /// Owner direct statements, owner-authored files, owner rework/draft diffs.
    Tier1Owner = 1,
    /// Indirect summaries of owner actions / verified internal records.
    Tier2Indirect = 2,
    /// External inbound emails, third-party IM messages, untrusted documents.
    Tier3External = 3,
}

impl SourceTrustTier {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_db(v: i64) -> Self {
        match v {
            1 => Self::Tier1Owner,
            2 => Self::Tier2Indirect,
            _ => Self::Tier3External,
        }
    }
}

/// Lifecycle state of a sidecar `memory_entries` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Active,
    Stale,
    Deleted,
}

impl EntryStatus {
    pub fn code(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Deleted => "deleted",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "stale" => Self::Stale,
            "deleted" => Self::Deleted,
            _ => Self::Deleted,
        }
    }
}

/// Lifecycle state of a buffered `observations` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Pending,
    Promoted,
    Expired,
    Rejected,
}

impl ObservationStatus {
    pub fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Promoted => "promoted",
            Self::Expired => "expired",
            Self::Rejected => "rejected",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "promoted" => Self::Promoted,
            "expired" => Self::Expired,
            _ => Self::Rejected,
        }
    }
}

/// File operation recorded in `memory_revisions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionOp {
    Add,
    Update,
    Delete,
    OverviewRefresh,
}

impl RevisionOp {
    pub fn code(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::OverviewRefresh => "overview_refresh",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "add" => Self::Add,
            "update" => Self::Update,
            "delete" => Self::Delete,
            "overview_refresh" => Self::OverviewRefresh,
            _ => Self::Update,
        }
    }
}

/// Status of a `memory_revisions` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStatus {
    Proposed,
    Applied,
    Reverted,
    Dismissed,
}

impl RevisionStatus {
    pub fn code(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Applied => "applied",
            Self::Reverted => "reverted",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "proposed" => Some(Self::Proposed),
            "applied" => Some(Self::Applied),
            "reverted" => Some(Self::Reverted),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }

    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Dismissed)
    }
}

/// Why a block pattern was recorded in `rejected_patterns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    OwnerDeleted,
    OwnerReverted,
    SupersededRejected,
}

impl RejectionReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::OwnerDeleted => "owner_deleted",
            Self::OwnerReverted => "owner_reverted",
            Self::SupersededRejected => "superseded_rejected",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "owner_deleted" => Self::OwnerDeleted,
            "owner_reverted" => Self::OwnerReverted,
            _ => Self::SupersededRejected,
        }
    }
}

// ---------------------------------------------------------------- memory_entries

pub struct MemoryEntries<'a, C>(pub &'a mut C);

/// Sidecar-only annotation row for one block in a memory Markdown file.
/// Contains NO duplicate prose text; the Markdown file on disk is the sole truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryEntryRow {
    pub id: String,
    pub workspace_id: String,
    pub source_id: String,
    pub doc_id: Option<String>,
    pub rel_path: String,
    pub block_anchor: String,
    pub obsidian_id: Option<String>,
    pub heading_path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub content_sha256: String,
    pub simhash: i64,
    pub bigram_fingerprint: String,
    pub kind: MemoryKind,
    pub subject_type: String,
    pub subject_id: String,
    pub scope_hint: String,
    pub confidence_tier: ConfidenceTier,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub episode_count: i64,
    pub correction_count: i64,
    pub has_conflict: bool,
    pub level: Level,
    pub visibility: Visibility,
    pub pinned: bool,
    pub status: EntryStatus,
    pub use_count: i64,
    pub last_used_at: Option<i64>,
    pub verified_at: Option<i64>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemoryEntry<'a> {
    pub id: Option<&'a str>,
    pub workspace_id: &'a str,
    pub source_id: &'a str,
    pub doc_id: Option<&'a str>,
    pub rel_path: &'a str,
    pub block_anchor: &'a str,
    pub obsidian_id: Option<&'a str>,
    pub heading_path: &'a str,
    pub line_start: i64,
    pub line_end: i64,
    pub content_sha256: &'a str,
    pub simhash: i64,
    pub bigram_fingerprint: &'a str,
    pub kind: MemoryKind,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub scope_hint: &'a str,
    pub confidence_tier: ConfidenceTier,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub episode_count: i64,
    pub correction_count: i64,
    pub has_conflict: bool,
    pub level: Level,
    pub visibility: &'a Visibility,
    pub pinned: bool,
    pub status: EntryStatus,
    pub verified_at: Option<i64>,
    pub created_by: &'a str,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct ReconcileUpdate<'a> {
    pub doc_id: Option<&'a str>,
    pub block_anchor: &'a str,
    pub obsidian_id: Option<&'a str>,
    pub heading_path: &'a str,
    pub line_start: i64,
    pub line_end: i64,
    pub content_sha256: &'a str,
    pub simhash: i64,
    pub bigram_fingerprint: &'a str,
    pub kind: MemoryKind,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub confidence_tier: ConfidenceTier,
    pub origin_class: OriginClass,
    pub correction_count: i64,
    pub level: Level,
    pub visibility: &'a Visibility,
    pub status: EntryStatus,
    pub verified_at: Option<i64>,
    pub now: i64,
}

const ENTRY_COLS: [&str; 33] = [
    "id",
    "workspace_id",
    "source_id",
    "doc_id",
    "rel_path",
    "block_anchor",
    "obsidian_id",
    "heading_path",
    "line_start",
    "line_end",
    "content_sha256",
    "simhash",
    "bigram_fingerprint",
    "kind",
    "subject_type",
    "subject_id",
    "scope_hint",
    "confidence_tier",
    "origin_class",
    "source_tier",
    "episode_count",
    "correction_count",
    "has_conflict",
    "level",
    "visibility",
    "pinned",
    "status",
    "use_count",
    "last_used_at",
    "verified_at",
    "created_by",
    "created_at",
    "updated_at",
];

fn entry_from_row(r: &Row<'_>) -> Result<MemoryEntryRow> {
    Ok(MemoryEntryRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        source_id: r.get(2)?,
        doc_id: r.get(3)?,
        rel_path: r.get(4)?,
        block_anchor: r.get(5)?,
        obsidian_id: r.get(6)?,
        heading_path: r.get(7)?,
        line_start: r.get(8)?,
        line_end: r.get(9)?,
        content_sha256: r.get(10)?,
        simhash: r.get(11)?,
        bigram_fingerprint: r.get(12)?,
        kind: MemoryKind::from_db(&r.get::<String>(13)?),
        subject_type: r.get(14)?,
        subject_id: r.get(15)?,
        scope_hint: r.get(16)?,
        confidence_tier: ConfidenceTier::from_db(&r.get::<String>(17)?),
        origin_class: OriginClass::from_db(&r.get::<String>(18)?),
        source_tier: SourceTrustTier::from_db(r.get::<i64>(19)?),
        episode_count: r.get(20)?,
        correction_count: r.get(21)?,
        has_conflict: r.get::<i64>(22)? != 0,
        level: Level::from_db(&r.get::<String>(23)?),
        visibility: Visibility::from_db(&r.get::<String>(24)?),
        pinned: r.get::<i64>(25)? != 0,
        status: EntryStatus::from_db(&r.get::<String>(26)?),
        use_count: r.get(27)?,
        last_used_at: r.get(28)?,
        verified_at: r.get(29)?,
        created_by: r.get(30)?,
        created_at: r.get(31)?,
        updated_at: r.get(32)?,
    })
}

fn entry_select() -> SelectStatement {
    Query::select()
        .columns(ENTRY_COLS)
        .from("memory_entries")
        .take()
}

impl<C: Read> MemoryEntries<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<MemoryEntryRow>> {
        self.0.one(
            entry_select().and_where(Expr::col("id").eq(id)),
            entry_from_row,
        )
    }

    pub fn by_source_path(
        &mut self,
        source_id: &str,
        rel_path: &str,
    ) -> Result<Vec<MemoryEntryRow>> {
        self.0.all(
            entry_select()
                .and_where(Expr::col("source_id").eq(source_id))
                .and_where(Expr::col("rel_path").eq(rel_path))
                .order_by("line_start", Order::Asc)
                .order_by("id", Order::Asc),
            entry_from_row,
        )
    }

    pub fn active_by_source_path(
        &mut self,
        source_id: &str,
        rel_path: &str,
    ) -> Result<Vec<MemoryEntryRow>> {
        self.0.all(
            entry_select()
                .and_where(Expr::col("source_id").eq(source_id))
                .and_where(Expr::col("rel_path").eq(rel_path))
                .and_where(Expr::col("status").eq(EntryStatus::Active.code()))
                .order_by("line_start", Order::Asc)
                .order_by("id", Order::Asc),
            entry_from_row,
        )
    }

    pub fn list_active_ws(&mut self, ws: &str) -> Result<Vec<MemoryEntryRow>> {
        self.0.all(
            entry_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq(EntryStatus::Active.code()))
                .order_by("pinned", Order::Desc)
                .order_by("updated_at", Order::Desc)
                .order_by("id", Order::Asc),
            entry_from_row,
        )
    }

    pub fn list_by_subject(
        &mut self,
        ws: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<MemoryEntryRow>> {
        self.0.all(
            entry_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq(subject_type))
                .and_where(Expr::col("subject_id").eq(subject_id))
                .and_where(Expr::col("status").eq(EntryStatus::Active.code()))
                .order_by("pinned", Order::Desc)
                .order_by("updated_at", Order::Desc)
                .order_by("id", Order::Asc),
            entry_from_row,
        )
    }

    pub fn find_active_by_sha(
        &mut self,
        ws: &str,
        content_sha256: &str,
    ) -> Result<Option<MemoryEntryRow>> {
        self.0.one(
            entry_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("content_sha256").eq(content_sha256))
                .and_where(Expr::col("status").eq(EntryStatus::Active.code()))
                .limit(1),
            entry_from_row,
        )
    }

    pub fn count_by_source(&mut self, source_id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("memory_entries")
                    .and_where(Expr::col("source_id").eq(source_id))
                    .and_where(Expr::col("status").eq(EntryStatus::Active.code())),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn count_dirty_atomic_since(
        &mut self,
        source_id: &str,
        rel_path: &str,
        since_ts: i64,
    ) -> Result<(i64, Option<i64>)> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .expr(Func::min(Expr::col("updated_at")))
                    .from("memory_entries")
                    .and_where(Expr::col("source_id").eq(source_id))
                    .and_where(Expr::col("rel_path").eq(rel_path))
                    .and_where(Expr::col("status").eq(EntryStatus::Active.code()))
                    .and_where(Expr::col("kind").ne(MemoryKind::Overview.code()))
                    .and_where(Expr::col("updated_at").gt(since_ts)),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .unwrap_or((0, None)))
    }
}

impl MemoryEntries<'_, W<'_>> {
    pub fn insert(&mut self, e: &NewMemoryEntry<'_>) -> Result<MemoryEntryRow> {
        let id = e.id.map(str::to_owned).unwrap_or_else(|| uid("me"));
        self.0.exec(
            Query::insert()
                .into_table("memory_entries")
                .columns(ENTRY_COLS)
                .values_panic([
                    id.as_str().into(),
                    e.workspace_id.into(),
                    e.source_id.into(),
                    e.doc_id.into(),
                    e.rel_path.into(),
                    e.block_anchor.into(),
                    e.obsidian_id.into(),
                    e.heading_path.into(),
                    e.line_start.into(),
                    e.line_end.into(),
                    e.content_sha256.into(),
                    e.simhash.into(),
                    e.bigram_fingerprint.into(),
                    e.kind.code().into(),
                    e.subject_type.into(),
                    e.subject_id.into(),
                    e.scope_hint.into(),
                    e.confidence_tier.code().into(),
                    e.origin_class.code().into(),
                    e.source_tier.as_i64().into(),
                    e.episode_count.into(),
                    e.correction_count.into(),
                    i64::from(e.has_conflict).into(),
                    e.level.code().into(),
                    e.visibility.code().into(),
                    i64::from(e.pinned).into(),
                    e.status.code().into(),
                    0_i64.into(),
                    Option::<i64>::None.into(),
                    e.verified_at.into(),
                    e.created_by.into(),
                    e.now.into(),
                    e.now.into(),
                ]),
        )?;
        crate::repo::must(self.get(&id)?)
    }

    pub fn update_reconciled(&mut self, id: &str, u: &ReconcileUpdate<'_>) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_entries")
                .value("doc_id", u.doc_id)
                .value("block_anchor", u.block_anchor)
                .value("obsidian_id", u.obsidian_id)
                .value("heading_path", u.heading_path)
                .value("line_start", u.line_start)
                .value("line_end", u.line_end)
                .value("content_sha256", u.content_sha256)
                .value("simhash", u.simhash)
                .value("bigram_fingerprint", u.bigram_fingerprint)
                .value("kind", u.kind.code())
                .value("subject_type", u.subject_type)
                .value("subject_id", u.subject_id)
                .value("confidence_tier", u.confidence_tier.code())
                .value("origin_class", u.origin_class.code())
                .value("correction_count", u.correction_count)
                .value("level", u.level.code())
                .value("visibility", u.visibility.code())
                .value("status", u.status.code())
                .value("verified_at", u.verified_at)
                .value("updated_at", u.now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_status(&mut self, id: &str, status: EntryStatus, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_entries")
                .value("status", status.code())
                .value("updated_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_metadata(
        &mut self,
        id: &str,
        level: Level,
        visibility: &Visibility,
        pinned: bool,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_entries")
                .value("level", level.code())
                .value("visibility", visibility.code())
                .value("pinned", i64::from(pinned))
                .value("updated_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn bump_episode(
        &mut self,
        id: &str,
        episode_count: i64,
        confidence_tier: ConfidenceTier,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_entries")
                .value("episode_count", episode_count)
                .value("confidence_tier", confidence_tier.code())
                .value("updated_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn record_use(&mut self, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_entries")
                .value("use_count", Expr::col("use_count").add(1_i64))
                .value("last_used_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn delete_by_source(&mut self, source_id: &str) -> Result<usize> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("memory_entries")
                .and_where(Expr::col("source_id").eq(source_id)),
        )? as usize)
    }
}

// ---------------------------------------------------------------- memory_evidence

pub struct MemoryEvidences<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryEvidenceRow {
    pub id: String,
    pub workspace_id: String,
    pub entry_id: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub origin_label: String,
    pub episode_key: String,
    pub quote_hash: String,
    pub rationale: String,
    pub source_tier: SourceTrustTier,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemoryEvidence<'a> {
    pub workspace_id: &'a str,
    pub entry_id: &'a str,
    pub origin_kind: &'a str,
    pub origin_id: &'a str,
    pub origin_label: &'a str,
    pub episode_key: &'a str,
    pub quote_hash: &'a str,
    pub rationale: &'a str,
    pub source_tier: SourceTrustTier,
    pub now: i64,
}

const EVIDENCE_COLS: [&str; 11] = [
    "id",
    "workspace_id",
    "entry_id",
    "origin_kind",
    "origin_id",
    "origin_label",
    "episode_key",
    "quote_hash",
    "rationale",
    "source_tier",
    "created_at",
];

fn evidence_from_row(r: &Row<'_>) -> Result<MemoryEvidenceRow> {
    Ok(MemoryEvidenceRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        entry_id: r.get(2)?,
        origin_kind: r.get(3)?,
        origin_id: r.get(4)?,
        origin_label: r.get(5)?,
        episode_key: r.get(6)?,
        quote_hash: r.get(7)?,
        rationale: r.get(8)?,
        source_tier: SourceTrustTier::from_db(r.get::<i64>(9)?),
        created_at: r.get(10)?,
    })
}

impl<C: Read> MemoryEvidences<'_, C> {
    pub fn list_for_entry(&mut self, entry_id: &str) -> Result<Vec<MemoryEvidenceRow>> {
        self.0.all(
            Query::select()
                .columns(EVIDENCE_COLS)
                .from("memory_evidence")
                .and_where(Expr::col("entry_id").eq(entry_id))
                .order_by("created_at", Order::Asc)
                .order_by("id", Order::Asc),
            evidence_from_row,
        )
    }

    pub fn distinct_episode_count(&mut self, entry_id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count_distinct(Expr::col("episode_key")))
                    .from("memory_evidence")
                    .and_where(Expr::col("entry_id").eq(entry_id)),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }
}

impl MemoryEvidences<'_, W<'_>> {
    /// Records one provenance link; returns `true` when this episode was newly attached.
    pub fn add(&mut self, ev: &NewMemoryEvidence<'_>) -> Result<bool> {
        let id = uid("mev");
        let n = self.0.exec(
            Query::insert()
                .into_table("memory_evidence")
                .columns(EVIDENCE_COLS)
                .values_panic([
                    id.as_str().into(),
                    ev.workspace_id.into(),
                    ev.entry_id.into(),
                    ev.origin_kind.into(),
                    ev.origin_id.into(),
                    ev.origin_label.into(),
                    ev.episode_key.into(),
                    ev.quote_hash.into(),
                    ev.rationale.into(),
                    ev.source_tier.as_i64().into(),
                    ev.now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["entry_id", "episode_key"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(n > 0)
    }
}

// ---------------------------------------------------------------- observations

pub struct Observations<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationEvidenceRef {
    pub origin_kind: String,
    pub origin_id: String,
    pub origin_label: String,
    pub episode_key: String,
    pub quote_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationRow {
    pub id: String,
    pub workspace_id: String,
    pub kind: MemoryKind,
    pub subject_type: String,
    pub subject_id: String,
    pub scope_hint: String,
    pub target_rel_path: String,
    pub proposed_text: String,
    pub why: String,
    pub content_sha256: String,
    pub simhash: i64,
    pub bigram_fingerprint: String,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub episode_keys: Vec<String>,
    pub episode_count: i64,
    pub evidence_refs: Vec<ObservationEvidenceRef>,
    pub level: Level,
    pub visibility: Visibility,
    pub status: ObservationStatus,
    pub promoted_entry_id: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewObservation<'a> {
    pub workspace_id: &'a str,
    pub kind: MemoryKind,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub scope_hint: &'a str,
    pub target_rel_path: &'a str,
    pub proposed_text: &'a str,
    pub why: &'a str,
    pub content_sha256: &'a str,
    pub simhash: i64,
    pub bigram_fingerprint: &'a str,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub episode_key: &'a str,
    pub evidence: ObservationEvidenceRef,
    pub level: Level,
    pub visibility: &'a Visibility,
    pub created_by: &'a str,
    pub now: i64,
    pub expires_at: i64,
}

const OBS_COLS: [&str; 25] = [
    "id",
    "workspace_id",
    "kind",
    "subject_type",
    "subject_id",
    "scope_hint",
    "target_rel_path",
    "proposed_text",
    "why",
    "content_sha256",
    "simhash",
    "bigram_fingerprint",
    "origin_class",
    "source_tier",
    "episode_keys",
    "episode_count",
    "evidence_refs",
    "level",
    "visibility",
    "status",
    "promoted_entry_id",
    "created_by",
    "created_at",
    "updated_at",
    "expires_at",
];

fn obs_from_row(r: &Row<'_>) -> Result<ObservationRow> {
    Ok(ObservationRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        kind: MemoryKind::from_db(&r.get::<String>(2)?),
        subject_type: r.get(3)?,
        subject_id: r.get(4)?,
        scope_hint: r.get(5)?,
        target_rel_path: r.get(6)?,
        proposed_text: r.get(7)?,
        why: r.get(8)?,
        content_sha256: r.get(9)?,
        simhash: r.get(10)?,
        bigram_fingerprint: r.get(11)?,
        origin_class: OriginClass::from_db(&r.get::<String>(12)?),
        source_tier: SourceTrustTier::from_db(r.get::<i64>(13)?),
        episode_keys: serde_json::from_str(&r.get::<String>(14)?).unwrap_or_default(),
        episode_count: r.get(15)?,
        evidence_refs: serde_json::from_str(&r.get::<String>(16)?).unwrap_or_default(),
        level: Level::from_db(&r.get::<String>(17)?),
        visibility: Visibility::from_db(&r.get::<String>(18)?),
        status: ObservationStatus::from_db(&r.get::<String>(19)?),
        promoted_entry_id: r.get(20)?,
        created_by: r.get(21)?,
        created_at: r.get(22)?,
        updated_at: r.get(23)?,
        expires_at: r.get(24)?,
    })
}

fn obs_select() -> SelectStatement {
    Query::select()
        .columns(OBS_COLS)
        .from("observations")
        .take()
}

impl<C: Read> Observations<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<ObservationRow>> {
        self.0
            .one(obs_select().and_where(Expr::col("id").eq(id)), obs_from_row)
    }

    pub fn list_pending(&mut self, ws: &str) -> Result<Vec<ObservationRow>> {
        self.0.all(
            obs_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq(ObservationStatus::Pending.code()))
                .order_by("updated_at", Order::Desc)
                .order_by("id", Order::Asc),
            obs_from_row,
        )
    }

    pub fn list_pending_for_subject(
        &mut self,
        ws: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<ObservationRow>> {
        self.0.all(
            obs_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq(subject_type))
                .and_where(Expr::col("subject_id").eq(subject_id))
                .and_where(Expr::col("status").eq(ObservationStatus::Pending.code()))
                .order_by("updated_at", Order::Desc)
                .order_by("id", Order::Asc),
            obs_from_row,
        )
    }
}

impl Observations<'_, W<'_>> {
    pub fn insert(&mut self, o: &NewObservation<'_>) -> Result<ObservationRow> {
        let id = uid("obs");
        let episodes = serde_json::to_string(&[o.episode_key])?;
        let evs = serde_json::to_string(&[&o.evidence])?;
        self.0.exec(
            Query::insert()
                .into_table("observations")
                .columns(OBS_COLS)
                .values_panic([
                    id.as_str().into(),
                    o.workspace_id.into(),
                    o.kind.code().into(),
                    o.subject_type.into(),
                    o.subject_id.into(),
                    o.scope_hint.into(),
                    o.target_rel_path.into(),
                    o.proposed_text.into(),
                    o.why.into(),
                    o.content_sha256.into(),
                    o.simhash.into(),
                    o.bigram_fingerprint.into(),
                    o.origin_class.code().into(),
                    o.source_tier.as_i64().into(),
                    episodes.into(),
                    1_i64.into(),
                    evs.into(),
                    o.level.code().into(),
                    o.visibility.code().into(),
                    ObservationStatus::Pending.code().into(),
                    Option::<String>::None.into(),
                    o.created_by.into(),
                    o.now.into(),
                    o.now.into(),
                    o.expires_at.into(),
                ]),
        )?;
        crate::repo::must(self.get(&id)?)
    }

    pub fn append_episode(
        &mut self,
        row: &ObservationRow,
        episode_key: &str,
        evidence: ObservationEvidenceRef,
        now: i64,
        expires_at: i64,
    ) -> Result<ObservationRow> {
        let mut keys = row.episode_keys.clone();
        let mut evs = row.evidence_refs.clone();
        if !keys.iter().any(|k| k == episode_key) {
            keys.push(episode_key.to_owned());
            evs.push(evidence);
        }
        let count = keys.len() as i64;
        self.0.exec(
            Query::update()
                .table("observations")
                .value("episode_keys", serde_json::to_string(&keys)?)
                .value("episode_count", count)
                .value("evidence_refs", serde_json::to_string(&evs)?)
                .value("updated_at", now)
                .value("expires_at", expires_at)
                .and_where(Expr::col("id").eq(&row.id)),
        )?;
        crate::repo::must(self.get(&row.id)?)
    }

    pub fn mark_promoted(&mut self, id: &str, entry_id: Option<&str>, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("observations")
                .value("status", ObservationStatus::Promoted.code())
                .value("promoted_entry_id", entry_id.map(str::to_string))
                .value("updated_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn mark_rejected(&mut self, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("observations")
                .value("status", ObservationStatus::Rejected.code())
                .value("updated_at", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn expire_due(&mut self, ws: &str, now: i64) -> Result<usize> {
        Ok(self.0.exec(
            Query::update()
                .table("observations")
                .value("status", ObservationStatus::Expired.code())
                .value("updated_at", now)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq(ObservationStatus::Pending.code()))
                .and_where(Expr::col("expires_at").lte(now)),
        )? as usize)
    }
}

// ---------------------------------------------------------------- rejected_patterns

pub struct RejectedPatterns<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedPatternRow {
    pub id: String,
    pub workspace_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub content_sha256: String,
    pub simhash: i64,
    pub bigram_fingerprint: String,
    pub reason: RejectionReason,
    pub entry_id: Option<String>,
    pub created_by: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewRejectedPattern<'a> {
    pub workspace_id: &'a str,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub content_sha256: &'a str,
    pub simhash: i64,
    pub bigram_fingerprint: &'a str,
    pub reason: RejectionReason,
    pub entry_id: Option<&'a str>,
    pub created_by: &'a str,
    pub now: i64,
}

const REJECT_COLS: [&str; 11] = [
    "id",
    "workspace_id",
    "subject_type",
    "subject_id",
    "content_sha256",
    "simhash",
    "bigram_fingerprint",
    "reason",
    "entry_id",
    "created_by",
    "created_at",
];

fn reject_from_row(r: &Row<'_>) -> Result<RejectedPatternRow> {
    Ok(RejectedPatternRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        subject_type: r.get(2)?,
        subject_id: r.get(3)?,
        content_sha256: r.get(4)?,
        simhash: r.get(5)?,
        bigram_fingerprint: r.get(6)?,
        reason: RejectionReason::from_db(&r.get::<String>(7)?),
        entry_id: r.get(8)?,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
    })
}

impl<C: Read> RejectedPatterns<'_, C> {
    pub fn list_ws(&mut self, ws: &str) -> Result<Vec<RejectedPatternRow>> {
        self.0.all(
            Query::select()
                .columns(REJECT_COLS)
                .from("rejected_patterns")
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("created_at", Order::Desc),
            reject_from_row,
        )
    }

    pub fn list_for_subject(
        &mut self,
        ws: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<RejectedPatternRow>> {
        self.0.all(
            Query::select()
                .columns(REJECT_COLS)
                .from("rejected_patterns")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq(subject_type))
                .and_where(Expr::col("subject_id").eq(subject_id))
                .order_by("created_at", Order::Desc),
            reject_from_row,
        )
    }
}

impl RejectedPatterns<'_, W<'_>> {
    pub fn record(&mut self, p: &NewRejectedPattern<'_>) -> Result<bool> {
        let id = uid("rej");
        let n = self.0.exec(
            Query::insert()
                .into_table("rejected_patterns")
                .columns(REJECT_COLS)
                .values_panic([
                    id.as_str().into(),
                    p.workspace_id.into(),
                    p.subject_type.into(),
                    p.subject_id.into(),
                    p.content_sha256.into(),
                    p.simhash.into(),
                    p.bigram_fingerprint.into(),
                    p.reason.code().into(),
                    p.entry_id.into(),
                    p.created_by.into(),
                    p.now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "content_sha256"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(n > 0)
    }
}

// ---------------------------------------------------------------- memory_revisions

pub struct MemoryRevisions<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryRevisionRow {
    pub id: String,
    pub workspace_id: String,
    pub source_id: String,
    pub rel_path: String,
    pub entry_id: Option<String>,
    pub op: RevisionOp,
    pub write_mode: WriteMode,
    pub status: RevisionStatus,
    pub heading_path: String,
    pub before_block: String,
    pub after_block: String,
    pub diff_preview: String,
    pub reason: String,
    pub origin_class: String,
    pub actor: String,
    pub created_at: i64,
    pub applied_at: Option<i64>,
    pub reverted_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewMemoryRevision<'a> {
    pub workspace_id: &'a str,
    pub source_id: &'a str,
    pub rel_path: &'a str,
    pub entry_id: Option<&'a str>,
    pub op: RevisionOp,
    pub write_mode: WriteMode,
    pub status: RevisionStatus,
    pub heading_path: &'a str,
    pub before_block: &'a str,
    pub after_block: &'a str,
    pub diff_preview: &'a str,
    pub reason: &'a str,
    pub origin_class: &'a str,
    pub actor: &'a str,
    pub now: i64,
    pub applied_at: Option<i64>,
}

const REV_COLS: [&str; 18] = [
    "id",
    "workspace_id",
    "source_id",
    "rel_path",
    "entry_id",
    "op",
    "write_mode",
    "status",
    "heading_path",
    "before_block",
    "after_block",
    "diff_preview",
    "reason",
    "origin_class",
    "actor",
    "created_at",
    "applied_at",
    "reverted_at",
];

fn rev_from_row(r: &Row<'_>) -> Result<MemoryRevisionRow> {
    Ok(MemoryRevisionRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        source_id: r.get(2)?,
        rel_path: r.get(3)?,
        entry_id: r.get(4)?,
        op: RevisionOp::from_db(&r.get::<String>(5)?),
        write_mode: WriteMode::from_db(&r.get::<String>(6)?),
        status: RevisionStatus::from_db(&r.get::<String>(7)?),
        heading_path: r.get(8)?,
        before_block: r.get(9)?,
        after_block: r.get(10)?,
        diff_preview: r.get(11)?,
        reason: r.get(12)?,
        origin_class: r.get(13)?,
        actor: r.get(14)?,
        created_at: r.get(15)?,
        applied_at: r.get(16)?,
        reverted_at: r.get(17)?,
    })
}

fn rev_select() -> SelectStatement {
    Query::select()
        .columns(REV_COLS)
        .from("memory_revisions")
        .take()
}

impl<C: Read> MemoryRevisions<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<MemoryRevisionRow>> {
        self.0
            .one(rev_select().and_where(Expr::col("id").eq(id)), rev_from_row)
    }

    pub fn list(
        &mut self,
        ws: &str,
        status: Option<RevisionStatus>,
        limit: usize,
    ) -> Result<Vec<MemoryRevisionRow>> {
        let mut q = rev_select();
        q.and_where(Expr::col("workspace_id").eq(ws));
        if let Some(st) = status {
            q.and_where(Expr::col("status").eq(st.code()));
        }
        q.order_by("created_at", Order::Desc)
            .order_by("id", Order::Desc)
            .limit(limit.max(1) as u64);
        self.0.all(&q, rev_from_row)
    }

    pub fn list_applied_since(
        &mut self,
        ws: &str,
        since_ts: i64,
    ) -> Result<Vec<MemoryRevisionRow>> {
        self.0.all(
            rev_select()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq(RevisionStatus::Applied.code()))
                .and_where(Expr::col("op").ne(RevisionOp::OverviewRefresh.code()))
                .and_where(Expr::col("applied_at").gte(since_ts))
                .order_by("applied_at", Order::Desc),
            rev_from_row,
        )
    }

    /// Returns `(total_writes, reverted_writes)` for non-overview revisions since `since_ts`.
    pub fn revert_window_counts(&mut self, ws: &str, since_ts: i64) -> Result<(i64, i64)> {
        let total = self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("memory_revisions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("status").is_in([
                        RevisionStatus::Applied.code(),
                        RevisionStatus::Reverted.code(),
                    ]))
                    .and_where(Expr::col("op").ne(RevisionOp::OverviewRefresh.code()))
                    .and_where(Expr::col("created_at").gte(since_ts)),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or(0);
        let reverted = self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("memory_revisions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("status").eq(RevisionStatus::Reverted.code()))
                    .and_where(Expr::col("op").ne(RevisionOp::OverviewRefresh.code()))
                    .and_where(Expr::col("created_at").gte(since_ts)),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or(0);
        Ok((total, reverted))
    }

    pub fn last_overview_refresh(
        &mut self,
        source_id: &str,
        rel_path: &str,
    ) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .expr(Func::max(Expr::col("applied_at")))
                .from("memory_revisions")
                .and_where(Expr::col("source_id").eq(source_id))
                .and_where(Expr::col("rel_path").eq(rel_path))
                .and_where(Expr::col("op").eq(RevisionOp::OverviewRefresh.code()))
                .and_where(Expr::col("status").eq(RevisionStatus::Applied.code())),
            |r| r.get(0),
        )
    }
}

impl MemoryRevisions<'_, W<'_>> {
    pub fn insert(&mut self, r: &NewMemoryRevision<'_>) -> Result<MemoryRevisionRow> {
        let id = uid("mrev");
        self.0.exec(
            Query::insert()
                .into_table("memory_revisions")
                .columns(REV_COLS)
                .values_panic([
                    id.as_str().into(),
                    r.workspace_id.into(),
                    r.source_id.into(),
                    r.rel_path.into(),
                    r.entry_id.into(),
                    r.op.code().into(),
                    r.write_mode.code().into(),
                    r.status.code().into(),
                    r.heading_path.into(),
                    r.before_block.into(),
                    r.after_block.into(),
                    r.diff_preview.into(),
                    r.reason.into(),
                    r.origin_class.into(),
                    r.actor.into(),
                    r.now.into(),
                    r.applied_at.into(),
                    Option::<i64>::None.into(),
                ]),
        )?;
        crate::repo::must(self.get(&id)?)
    }

    pub fn set_status(
        &mut self,
        id: &str,
        status: RevisionStatus,
        applied_at: Option<i64>,
        reverted_at: Option<i64>,
    ) -> Result<()> {
        let mut q = Query::update();
        q.table("memory_revisions")
            .value("status", status.code())
            .and_where(Expr::col("id").eq(id));
        if let Some(ts) = applied_at {
            q.value("applied_at", ts);
        }
        if let Some(ts) = reverted_at {
            q.value("reverted_at", ts);
        }
        self.0.exec(&q)?;
        Ok(())
    }

    pub fn set_entry_id(&mut self, id: &str, entry_id: Option<&str>) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("memory_revisions")
                .value("entry_id", entry_id)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}
