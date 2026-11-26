//! Single unified memory ingestion pipeline (`steve_remember`, `steve_contacts_remember`,
//! `steve-digest`, item/chat extraction, mail threads, channel spaces, and human draft-edit diffs).
//!
//! Hard invariants:
//! - Single admission criterion: "Does this change what an agent does in another different task?"
//!   Default is ALWAYS `NOOP`.
//! - Zero Inbox confirmation cards (`decisions.subject_type = 'mem'` is never opened).
//! - Tier 3 external sources (inbound emails, third-party IM) are blocked at the code level from
//!   writing `me`/owner entries or any `Rule`/`Preference` entries (0% pollution).
//! - Permanent tombstones in `rejected_patterns` block recurrence after owner deletion/revert (0%).
//! - Implicit patterns buffer in `observations` and promote only after recurring across
//!   `PROMOTE_MIN_EPISODES` distinct `episode_key`s.
//! - Automatic silent writes clamp disclosure level to at least `Level::Reasoning`.
//! - Writes strictly obey the target memory source's `WriteMode` (`read_only` | `propose` | `direct_write`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult, Caller, Modules, Tier};
use steve_store::base::{Level, ObjRef, Visibility};
use steve_store::knowledge::{
    Documents, MemoryEntries, MemoryEntryRow, MemoryEvidences, MemoryKind, MemoryRevisionRow,
    NewMemoryEvidence, NewObservation, ObservationEvidenceRef, ObservationRow, Observations,
    OriginClass, SourceTrustTier,
};
use steve_store::{Contacts, Read, References, W, sha};

use crate::extract::Names;
use crate::memory_file::{
    self, MemoryWriteEvidence, MemoryWriteRequest, WriteOutcome, bigram_dice_similarity,
    bigram_hashes, block_sha256, compute_simhash, encode_bigram_fingerprint,
    find_duplicate_active_entry, matches_fingerprint, matches_tombstone, target_rel_path,
};
use crate::policy::{
    ConfidenceInput, DAY_MS, DEFAULT_SILENT_WRITE_LEVEL, DRAFT_DIFF_MAX_SIMILARITY,
    DRAFT_DIFF_MIN_CHANGED_CHARS, MemorySettings, NEAR_DUPLICATE_MIN_SIMILARITY,
    RATIONALE_MAX_CHARS, compute_confidence, enforce_disclosure_ceiling, is_external_blocked,
};
use crate::sources::folder::FolderPolicy;
use crate::state::Knowledge;

/// Unified typed memory ingestion item shared by every producer in Steve.
#[derive(Debug, Clone)]
pub struct IngestItem {
    pub kind: MemoryKind,
    pub subject_type: String,
    pub subject_id: String,
    pub subject_name: String,
    pub scope_hint: String,
    pub text: String,
    pub why: String,
    /// Hard admission criterion: must be `true` to pass Gate 1; default is `false` (NOOP).
    pub changes_future_behavior: bool,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub origin_kind: String,
    pub origin_id: String,
    pub origin_label: String,
    /// Independent episode identifier (e.g. `chat:2026-11-25`, `item:ST-12`, `mail:mt_1`).
    pub episode_key: String,
    pub evidence_quote: String,
    pub requested_level: Level,
    pub visibility: Visibility,
    pub actor: String,
    pub created_by: String,
    /// True when the owner explicitly stated a future-behavior rule/correction (fast-path).
    pub is_explicit_rule: bool,
}

/// Deterministic drop reasons recorded for telemetry and evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestDropReason {
    InvalidShape,
    NoopNoFutureChange,
    BlockedExternal,
    BlockedTombstone,
}

impl IngestDropReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidShape => "invalid_shape",
            Self::NoopNoFutureChange => "noop",
            Self::BlockedExternal => "blocked_external",
            Self::BlockedTombstone => "blocked_tombstone",
        }
    }
}

/// Strongly-typed outcome of ingesting one [`IngestItem`].
#[derive(Debug, Clone)]
pub enum IngestOutcome {
    Written {
        entry: MemoryEntryRow,
        revision: MemoryRevisionRow,
        promoted_from: Option<String>,
    },
    Proposed {
        revision: MemoryRevisionRow,
    },
    RefusedReadOnly {
        source_id: String,
    },
    Buffered {
        observation: ObservationRow,
    },
    CorroboratedExisting {
        entry_id: String,
        episode_count: i64,
    },
    Dropped(IngestDropReason),
}

impl IngestOutcome {
    pub fn is_written(&self) -> bool {
        matches!(self, Self::Written { .. })
    }

    pub fn entry_id(&self) -> Option<&str> {
        match self {
            Self::Written { entry, .. } => Some(&entry.id),
            Self::CorroboratedExisting { entry_id, .. } => Some(entry_id),
            _ => None,
        }
    }
}

fn clip_chars(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// Resolves a fallback [`FolderPolicy`] from the store's own SQLite file parent directory so
/// standalone callers without a [`Knowledge`] handle (such as `steve-digest`) still stay inside
/// the active Hub's `STEVE_HOME` (and isolated test tempdirs) without ever touching `~/.steve`.
pub fn fallback_policy_for_store(store: &steve_store::Store) -> FolderPolicy {
    let steve_home = store
        .path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| FolderPolicy::from_env().steve_home);
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| steve_home.clone());
    FolderPolicy::new(home, steve_home)
}

/// Core single-entry ingestion gate inside a write transaction.
/// Every memory producer in Steve flows through this exact function.
pub fn ingest_in(
    tx: &mut W<'_>,
    policy: &FolderPolicy,
    ws: &str,
    item: &IngestItem,
    now: i64,
) -> ApiResult<IngestOutcome> {
    let text = item.text.trim();
    let len = text.chars().count();
    if len == 0 || len > 2000 || item.kind == MemoryKind::Overview {
        metrics::counter!("steve_memory_writes_total", "mode" => "ingest", "result" => "invalid_shape")
            .increment(1);
        return Ok(IngestOutcome::Dropped(IngestDropReason::InvalidShape));
    }

    // Gate 1: Single hard admission criterion ("changes agent behavior in another different task").
    // Default is NOOP.
    let is_owner_direct = matches!(
        item.origin_class,
        OriginClass::OwnerExplicit | OriginClass::OwnerFile
    ) && item.source_tier == SourceTrustTier::Tier1Owner;
    if !item.changes_future_behavior {
        metrics::counter!("steve_memory_writes_total", "mode" => "ingest", "result" => "noop")
            .increment(1);
        return Ok(IngestOutcome::Dropped(IngestDropReason::NoopNoFutureChange));
    }
    if !is_owner_direct && (item.why.trim().is_empty() || item.scope_hint.trim().is_empty()) {
        metrics::counter!("steve_memory_writes_total", "mode" => "ingest", "result" => "noop")
            .increment(1);
        return Ok(IngestOutcome::Dropped(IngestDropReason::NoopNoFutureChange));
    }

    // Gate 2: Hard code-level external trust isolation (0% external pollution of `me`/global rules).
    if is_external_blocked(item.source_tier, &item.subject_type, item.kind) {
        metrics::counter!("steve_memory_writes_total", "mode" => "ingest", "result" => "blocked_external")
            .increment(1);
        return Ok(IngestOutcome::Dropped(IngestDropReason::BlockedExternal));
    }

    // Gate 3: Permanent tombstone check (0% recurrence after owner deletes/reverts).
    if matches_tombstone(tx, ws, &item.subject_type, &item.subject_id, text)? {
        metrics::counter!("steve_memory_writes_total", "mode" => "ingest", "result" => "blocked_tombstone")
            .increment(1);
        return Ok(IngestOutcome::Dropped(IngestDropReason::BlockedTombstone));
    }

    let settings = MemorySettings::load(tx, ws);
    let quote_hash = if item.evidence_quote.trim().is_empty() {
        String::new()
    } else {
        sha(item.evidence_quote.trim())
    };
    let rationale = clip_chars(&item.why, RATIONALE_MAX_CHARS);
    let episode_key = if item.episode_key.trim().is_empty() {
        format!("{}:{}", item.origin_kind, item.origin_id)
    } else {
        item.episode_key.clone()
    };

    // Gate 4: Active entry deduplication & episode corroboration (never duplicate lines on disk).
    if let Some(existing) =
        find_duplicate_active_entry(tx, ws, &item.subject_type, &item.subject_id, text)?
    {
        let added = MemoryEvidences(tx).add(&NewMemoryEvidence {
            workspace_id: ws,
            entry_id: &existing.id,
            origin_kind: &item.origin_kind,
            origin_id: &item.origin_id,
            origin_label: &item.origin_label,
            episode_key: &episode_key,
            quote_hash: &quote_hash,
            rationale: &rationale,
            source_tier: item.source_tier,
            now,
        })?;
        let episodes = if added {
            MemoryEvidences(tx)
                .distinct_episode_count(&existing.id)?
                .max(existing.episode_count + 1)
        } else {
            existing.episode_count
        };
        if added {
            let next_conf = compute_confidence(&ConfidenceInput {
                origin_class: existing.origin_class,
                source_tier: existing.source_tier.min(item.source_tier),
                episode_count: episodes,
                correction_count: existing.correction_count,
                has_conflict: existing.has_conflict,
                is_explicit_rule: item.is_explicit_rule,
                promote_min_episodes: settings.min_episodes_for(existing.kind),
            });
            MemoryEntries(tx).bump_episode(&existing.id, episodes, next_conf, now)?;
        }
        return Ok(IngestOutcome::CorroboratedExisting {
            entry_id: existing.id,
            episode_count: episodes,
        });
    }

    let rel_path = target_rel_path(&item.subject_type, &item.subject_id);
    let doc_title = if item.subject_name.trim().is_empty() {
        if item.subject_type == "me" {
            "Personal Context".to_string()
        } else {
            item.subject_id.clone()
        }
    } else {
        item.subject_name.trim().to_string()
    };

    // Gate 5: Fast-path (explicit owner statement / high-signal owner rework rule) vs
    // 3-gate observation buffer (must recur across independent episodes before silent write).
    let is_fast_path = item.source_tier == SourceTrustTier::Tier1Owner
        && (item.origin_class == OriginClass::OwnerExplicit
            || (item.origin_class == OriginClass::OwnerRework
                && item.is_explicit_rule
                && !item.scope_hint.trim().is_empty()
                && !item.why.trim().is_empty()));

    if !is_fast_path {
        let min_eps = settings.min_episodes_for(item.kind);
        let content_sha = block_sha256(text);
        let bg = bigram_hashes(text);
        let sh = compute_simhash(&bg);
        let fp = encode_bigram_fingerprint(&bg);
        let expires_at = now + settings.observation_ttl_days * DAY_MS;
        let ev_ref = ObservationEvidenceRef {
            origin_kind: item.origin_kind.clone(),
            origin_id: item.origin_id.clone(),
            origin_label: item.origin_label.clone(),
            episode_key: episode_key.clone(),
            quote_hash: quote_hash.clone(),
        };

        let existing_obs = Observations(tx)
            .list_pending_for_subject(ws, &item.subject_type, &item.subject_id)?
            .into_iter()
            .find(|o| {
                o.kind == item.kind
                    && matches_fingerprint(
                        &content_sha,
                        sh,
                        &bg,
                        &o.content_sha256,
                        o.simhash,
                        &o.bigram_fingerprint,
                        NEAR_DUPLICATE_MIN_SIMILARITY,
                    )
            });

        let obs = match existing_obs {
            Some(old) => {
                Observations(tx).append_episode(&old, &episode_key, ev_ref, now, expires_at)?
            }
            None => Observations(tx).insert(&NewObservation {
                workspace_id: ws,
                kind: item.kind,
                subject_type: &item.subject_type,
                subject_id: &item.subject_id,
                scope_hint: &item.scope_hint,
                target_rel_path: &rel_path,
                proposed_text: text,
                why: &rationale,
                content_sha256: &content_sha,
                simhash: sh,
                bigram_fingerprint: &fp,
                origin_class: item.origin_class,
                source_tier: item.source_tier,
                episode_key: &episode_key,
                evidence: ev_ref,
                level: enforce_disclosure_ceiling(item.requested_level, true),
                visibility: &item.visibility,
                created_by: &item.created_by,
                now,
                expires_at,
            })?,
        };

        if obs.episode_count < min_eps {
            metrics::counter!("steve_memory_writes_total", "mode" => "buffer", "result" => "buffered")
                .increment(1);
            return Ok(IngestOutcome::Buffered { observation: obs });
        }

        // Gate 3 Promotion: corroborated across >= min_eps independent episodes!
        let confidence_tier = compute_confidence(&ConfidenceInput {
            origin_class: obs.origin_class,
            source_tier: obs.source_tier,
            episode_count: obs.episode_count,
            correction_count: 0,
            has_conflict: false,
            is_explicit_rule: false,
            promote_min_episodes: min_eps,
        });
        let level = enforce_disclosure_ceiling(obs.level, true);
        let source = memory_file::ensure_default_memory_source(tx, policy, ws, &item.actor, now)?;
        let evidence = obs
            .evidence_refs
            .iter()
            .map(|r| MemoryWriteEvidence {
                origin_kind: r.origin_kind.clone(),
                origin_id: r.origin_id.clone(),
                origin_label: r.origin_label.clone(),
                episode_key: r.episode_key.clone(),
                quote_hash: r.quote_hash.clone(),
                rationale: obs.why.clone(),
                source_tier: obs.source_tier,
            })
            .collect();
        let outcome = memory_file::apply_memory_add(
            tx,
            &source,
            &MemoryWriteRequest {
                workspace_id: ws,
                rel_path: &obs.target_rel_path,
                doc_title: &doc_title,
                kind: obs.kind,
                subject_type: &obs.subject_type,
                subject_id: &obs.subject_id,
                scope_hint: &obs.scope_hint,
                text: &obs.proposed_text,
                reason: &obs.why,
                origin_class: obs.origin_class,
                source_tier: obs.source_tier,
                confidence_tier,
                episode_count: obs.episode_count,
                level,
                visibility: &obs.visibility,
                actor: &item.actor,
                created_by: &obs.created_by,
                evidence,
                now,
            },
        )?;
        return match outcome {
            WriteOutcome::Applied { entry, revision } => {
                Observations(tx).mark_promoted(&obs.id, Some(&entry.id), now)?;
                Ok(IngestOutcome::Written {
                    entry,
                    revision,
                    promoted_from: Some(obs.id),
                })
            }
            WriteOutcome::Proposed { revision } => {
                Observations(tx).mark_promoted(&obs.id, None, now)?;
                Ok(IngestOutcome::Proposed { revision })
            }
            WriteOutcome::RefusedReadOnly { source_id } => {
                Ok(IngestOutcome::RefusedReadOnly { source_id })
            }
        };
    }

    // Fast-path silent write (Tier 1 Owner explicit statement or explicit owner rework rule).
    let is_auto = item.origin_class != OriginClass::OwnerExplicit;
    let level = enforce_disclosure_ceiling(item.requested_level, is_auto);
    let confidence_tier = compute_confidence(&ConfidenceInput {
        origin_class: item.origin_class,
        source_tier: item.source_tier,
        episode_count: 1,
        correction_count: if item.origin_class == OriginClass::OwnerRework {
            1
        } else {
            0
        },
        has_conflict: false,
        is_explicit_rule: item.is_explicit_rule,
        promote_min_episodes: settings.min_episodes_for(item.kind),
    });
    let source = memory_file::ensure_default_memory_source(tx, policy, ws, &item.actor, now)?;
    let outcome = memory_file::apply_memory_add(
        tx,
        &source,
        &MemoryWriteRequest {
            workspace_id: ws,
            rel_path: &rel_path,
            doc_title: &doc_title,
            kind: item.kind,
            subject_type: &item.subject_type,
            subject_id: &item.subject_id,
            scope_hint: &item.scope_hint,
            text,
            reason: &rationale,
            origin_class: item.origin_class,
            source_tier: item.source_tier,
            confidence_tier,
            episode_count: 1,
            level,
            visibility: &item.visibility,
            actor: &item.actor,
            created_by: &item.created_by,
            evidence: vec![MemoryWriteEvidence {
                origin_kind: item.origin_kind.clone(),
                origin_id: item.origin_id.clone(),
                origin_label: item.origin_label.clone(),
                episode_key,
                quote_hash,
                rationale: rationale.clone(),
                source_tier: item.source_tier,
            }],
            now,
        },
    )?;

    match outcome {
        WriteOutcome::Applied { entry, revision } => Ok(IngestOutcome::Written {
            entry,
            revision,
            promoted_from: None,
        }),
        WriteOutcome::Proposed { revision } => Ok(IngestOutcome::Proposed { revision }),
        WriteOutcome::RefusedReadOnly { source_id } => {
            Ok(IngestOutcome::RefusedReadOnly { source_id })
        }
    }
}

/// Resolves `(subject_type, subject_id, subject_name)` from a list of `ObjRef`s, normalizing the
/// owner's own contact party to `("me", "me", "Personal Context")`.
pub fn resolve_subject(
    c: &mut impl Read,
    ws: &str,
    user: &str,
    about: &[ObjRef],
) -> ApiResult<(String, String, String)> {
    let me_party = if !user.is_empty() {
        Contacts::new(c).user_party(ws, user)?
    } else {
        None
    };
    let Some(first) = about.first() else {
        return Ok(("me".into(), "me".into(), "Personal Context".into()));
    };
    if first.kind == "party" && me_party.as_deref() == Some(first.id.as_str()) {
        return Ok(("me".into(), "me".into(), "Personal Context".into()));
    }
    let name = References(c)
        .name(first)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| first.id.clone());
    Ok((first.kind.clone(), first.id.clone(), name))
}

// ---------------------------------------------------------------- Unified MCP Tool Entry Points

#[derive(Debug, Deserialize)]
struct RememberToolArgs {
    text: String,
    kind: String,
    #[serde(default)]
    about: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    why: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContactsRememberArgs {
    who: String,
    text: String,
    kind: String,
}

fn format_tool_outcome(outcome: &IngestOutcome) -> String {
    match outcome {
        IngestOutcome::Written { entry, .. } => {
            format!(
                "Saved silently to {} (anchor: {}, confidence: {}).",
                entry.rel_path,
                entry.block_anchor,
                entry.confidence_tier.code()
            )
        }
        IngestOutcome::Proposed { revision } => {
            format!(
                "Proposed update to {} (revision {}, pending approval in Propose mode).",
                revision.rel_path, revision.id
            )
        }
        IngestOutcome::RefusedReadOnly { source_id } => {
            format!("Not saved: memory source {source_id} is configured as read_only.")
        }
        IngestOutcome::Buffered { observation } => {
            format!(
                "Buffered observation {} (episodes: {}); will promote automatically after corroboration.",
                observation.id, observation.episode_count
            )
        }
        IngestOutcome::CorroboratedExisting {
            entry_id,
            episode_count,
        } => {
            format!("Corroborated existing memory {entry_id} (episodes: {episode_count}).")
        }
        IngestOutcome::Dropped(reason) => {
            format!("Not saved ({}).", reason.code())
        }
    }
}

/// Unified handler for `steve_remember`: validates arguments strongly, resolves optional `about`,
/// and routes directly through [`ingest_in`] (zero Inbox confirmation cards).
pub fn remember_tool(
    m: &Modules,
    kb: &Knowledge,
    cx: &McpCx,
    args: &Value,
) -> Result<String, String> {
    let parsed: RememberToolArgs = serde_json::from_value(args.clone()).map_err(|e| {
        format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {}: {e}",
            crate::candidates::REMEMBER
        )
    })?;
    let Some(kind) = MemoryKind::parse(&parsed.kind).filter(|k| {
        matches!(
            k,
            MemoryKind::Fact | MemoryKind::Preference | MemoryKind::Rule
        )
    }) else {
        return Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {}: Invalid option: expected one of \"fact\"|\"preference\"|\"rule\" at kind",
            crate::candidates::REMEMBER
        ));
    };
    let text = parsed.text.trim();
    let n = text.chars().count();
    if n == 0 || n > 2000 {
        return Err("text must be 1 to 2000 characters.".into());
    }
    let about_raw = parsed
        .about
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let who = Caller {
        ws: cx.ws.clone(),
        me: cx.user.clone(),
    };
    crate::service::prepare(m, &who).map_err(|e| e.to_string())?;
    let viewer = m.viewer(&who).map_err(|e| e.to_string())?;
    let now = m.clock.now();
    let policy = kb.folder_policy().clone();

    let outcome = m
        .store
        .write(|tx| {
            let about = match about_raw {
                None => {
                    if kind == MemoryKind::Fact {
                        return Err(ApiError::rule(
                            "A fact must name the person, organization, team or project it is about (pass `about`).",
                        ));
                    }
                    Vec::new()
                }
                Some(a) => {
                    let names = Names::load(tx, &viewer, None)?;
                    match names.resolve_entity(a) {
                        Some((r, ent)) if kind != MemoryKind::Fact || ent.is_fact_subject() => {
                            vec![r]
                        }
                        Some(_) => {
                            return Err(ApiError::rule(format!(
                                "A fact must be about a person, organization, team or project, not \"{a}\"."
                            )));
                        }
                        None => {
                            return Err(ApiError::rule(format!(
                                "No person, organization, project or bot is called \"{a}\". Pass the exact name or id."
                            )));
                        }
                    }
                }
            };
            let (subject_type, subject_id, subject_name) =
                resolve_subject(tx, &cx.ws, &cx.user, &about)?;
            let origin_id = cx
                .item_id
                .clone()
                .or_else(|| cx.session_id.clone())
                .unwrap_or_else(|| "tool".into());
            let origin_label = match &cx.item_id {
                Some(i) => format!("{i} 的会话"),
                None => "Agent 会话".into(),
            };
            let item = IngestItem {
                kind,
                subject_type,
                subject_id,
                subject_name,
                scope_hint: parsed
                    .scope
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "global".into()),
                text: text.to_string(),
                why: parsed
                    .why
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "explicit_tool_remember".into()),
                changes_future_behavior: true,
                origin_class: OriginClass::OwnerExplicit,
                source_tier: SourceTrustTier::Tier1Owner,
                origin_kind: "tool".into(),
                origin_id: origin_id.clone(),
                origin_label,
                episode_key: format!("tool:{origin_id}"),
                evidence_quote: text.to_string(),
                requested_level: DEFAULT_SILENT_WRITE_LEVEL,
                visibility: Visibility::Private,
                actor: cx.bot_id.clone(),
                created_by: cx.user.clone(),
                is_explicit_rule: true,
            };
            ingest_in(tx, &policy, &cx.ws, &item, now)
        })
        .map_err(|e: ApiError| e.to_string())?;
    Ok(format_tool_outcome(&outcome))
}

/// Unified handler for `steve_contacts_remember` routed through the exact same [`ingest_in`]
/// pipeline as `steve_remember` and `steve-digest`.
pub fn remember_contact_tool(
    m: &Modules,
    kb: &Knowledge,
    cx: &McpCx,
    args: &Value,
) -> Result<String, String> {
    let parsed: ContactsRememberArgs = serde_json::from_value(args.clone()).map_err(|e| {
        format!("MCP error -32602: Input validation error: Invalid arguments for tool steve_contacts_remember: {e}")
    })?;
    let Some(kind) = MemoryKind::parse(&parsed.kind).filter(|k| {
        matches!(
            k,
            MemoryKind::Fact | MemoryKind::Preference | MemoryKind::Event
        )
    }) else {
        return Err(
            "MCP error -32602: Input validation error: Invalid arguments for tool steve_contacts_remember: Invalid option: expected one of \"fact\"|\"preference\"|\"event\" at kind"
                .into(),
        );
    };
    let text = parsed.text.trim();
    let n = text.chars().count();
    if n == 0 || n > 2000 {
        return Err("text must be 1 to 2000 characters.".into());
    }
    let caller = Caller {
        ws: cx.ws.clone(),
        me: cx.user.clone(),
    };
    let viewer = if cx.user == format!("svc:{}", cx.ws) {
        m.viewer(&caller).map_err(|e| e.to_string())?
    } else {
        steve_modules::contacts::service::start(m, &caller)
            .map(|(v, _)| v)
            .map_err(|e| e.to_string())?
    };
    let now = m.clock.now();
    let policy = kb.folder_policy().clone();

    let outcome = m
        .store
        .write(|tx| {
            let names = Names::load(tx, &viewer, None)?;
            let Some((obj, _)) = names
                .resolve_entity(&parsed.who)
                .filter(|(r, _)| r.kind == "party")
            else {
                return Err(ApiError::rule(format!(
                    "No person, organization or team is called \"{}\". Look them up with steve_contacts_find and pass the id.",
                    parsed.who.trim()
                )));
            };
            let (subject_type, subject_id, subject_name) =
                resolve_subject(tx, &cx.ws, &cx.user, &[obj])?;
            let origin_id = cx
                .item_id
                .clone()
                .or_else(|| cx.session_id.clone())
                .unwrap_or_else(|| "contacts_tool".into());
            let origin_label = match &cx.item_id {
                Some(i) => format!("{i} 的会话"),
                None => "Agent 会话".into(),
            };
            let item = IngestItem {
                kind,
                subject_type,
                subject_id,
                subject_name,
                scope_hint: "contact_context".into(),
                text: text.to_string(),
                why: "explicit_contact_remember".into(),
                changes_future_behavior: true,
                origin_class: OriginClass::OwnerExplicit,
                source_tier: SourceTrustTier::Tier1Owner,
                origin_kind: "tool".into(),
                origin_id: origin_id.clone(),
                origin_label,
                episode_key: format!("contacts_tool:{origin_id}"),
                evidence_quote: text.to_string(),
                requested_level: DEFAULT_SILENT_WRITE_LEVEL,
                visibility: Visibility::Private,
                actor: cx.bot_id.clone(),
                created_by: cx.user.clone(),
                is_explicit_rule: true,
            };
            ingest_in(tx, &policy, &cx.ws, &item, now)
        })
        .map_err(|e: ApiError| e.to_string())?;
    Ok(format_tool_outcome(&outcome))
}

/// Unified entry point for `steve-digest` conversation digest chunks inside a write transaction.
/// Uses the exact same [`ingest_in`] pipeline (zero Inbox confirmation cards, default `Level::Reasoning`).
#[allow(clippy::too_many_arguments)]
pub fn ingest_digest_memory_in(
    tx: &mut W<'_>,
    policy: &FolderPolicy,
    ws: &str,
    user: &str,
    bot_id: &str,
    packet_id: &str,
    conv_label: &str,
    kind: MemoryKind,
    text: &str,
    evidence: &str,
    party_ids: &[String],
    now: i64,
) -> ApiResult<IngestOutcome> {
    let about: Vec<ObjRef> = party_ids
        .iter()
        .take(1)
        .map(|id| ObjRef::new("party", id))
        .collect();
    let (subject_type, subject_id, subject_name) = resolve_subject(tx, ws, user, &about)?;
    let item = IngestItem {
        kind,
        subject_type,
        subject_id,
        subject_name,
        scope_hint: conv_label.to_string(),
        text: text.to_string(),
        why: "conversation_digest_signal".into(),
        changes_future_behavior: true,
        origin_class: OriginClass::ImplicitPattern,
        source_tier: SourceTrustTier::Tier2Indirect,
        origin_kind: "digest".into(),
        origin_id: packet_id.to_string(),
        origin_label: conv_label.to_string(),
        episode_key: format!("digest:{packet_id}"),
        evidence_quote: evidence.to_string(),
        requested_level: DEFAULT_SILENT_WRITE_LEVEL,
        visibility: Visibility::Private,
        actor: bot_id.to_string(),
        created_by: user.to_string(),
        is_explicit_rule: false,
    };
    ingest_in(tx, policy, ws, &item, now)
}

// ---------------------------------------------------------------- Human Draft-Edit Diff Ingestion

/// Checks whether an owner's edit to an AI-composed draft (`before` -> `after`) is substantive
/// enough to carry a preference/rule signal (filters out trivial typos and whitespace edits).
pub fn is_substantive_draft_diff(before: &str, after: &str) -> bool {
    let b = before.trim();
    let a = after.trim();
    if b.is_empty() || a.is_empty() || b == a {
        return false;
    }
    let char_delta = b.chars().count().abs_diff(a.chars().count());
    let bg_b = bigram_hashes(b);
    let bg_a = bigram_hashes(a);
    let sim = bigram_dice_similarity(&bg_b, &bg_a);
    char_delta >= DRAFT_DIFF_MIN_CHANGED_CHARS || sim <= DRAFT_DIFF_MAX_SIMILARITY
}

#[derive(Debug, Deserialize)]
struct DraftDiffModelResponse {
    #[serde(default)]
    items: Vec<DraftDiffItem>,
}

#[derive(Debug, Deserialize)]
struct DraftDiffItem {
    #[serde(default)]
    changes_future_behavior: bool,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    why: String,
}

/// High-signal human draft-edit diff ingestion (`OriginClass::OwnerDraftDiff`, `Tier1Owner`).
/// Called after the owner edits and approves an AI-composed email or channel reply draft.
/// Uses the single unified [`ingest_in`] pipeline (buffered until corroborated across independent
/// episodes unless explicit rule; zero Inbox confirmation cards).
#[allow(clippy::too_many_arguments)]
pub async fn submit_draft_diff(
    m: &Modules,
    kb: &Knowledge,
    ws: &str,
    user: &str,
    origin_kind: &str,
    draft_id: &str,
    label: &str,
    recipient_party_id: Option<&str>,
    before: &str,
    after: &str,
) -> ApiResult<Vec<IngestOutcome>> {
    submit_draft_diff_with_policy(
        m,
        kb.folder_policy(),
        ws,
        user,
        origin_kind,
        draft_id,
        label,
        recipient_party_id,
        before,
        after,
    )
    .await
}

/// [`submit_draft_diff`] parameterized directly by [`FolderPolicy`] (for callers without a
/// [`Knowledge`] handle, such as `steve-channels` outbox maintenance).
#[allow(clippy::too_many_arguments)]
pub async fn submit_draft_diff_with_policy(
    m: &Modules,
    policy: &FolderPolicy,
    ws: &str,
    user: &str,
    origin_kind: &str,
    draft_id: &str,
    label: &str,
    recipient_party_id: Option<&str>,
    before: &str,
    after: &str,
) -> ApiResult<Vec<IngestOutcome>> {
    if !is_substantive_draft_diff(before, after) {
        return Ok(Vec::new());
    }
    let prompt = format!(
        "The owner manually edited an AI-composed draft before sending ({label}).\n\
         Extract ONLY reusable communication style preferences or standing rules that would change how an agent drafts future messages in DIFFERENT situations.\n\
         If the edit only changes task-specific facts, numbers, dates, or phrasing for this one message, return {{\"items\":[]}} (default NOOP).\n\n\
         Draft BEFORE owner edit:\n<<<\n{}\n>>>\n\n\
         Draft AFTER owner edit:\n<<<\n{}\n>>>\n\n\
         Return strict JSON: {{\"items\":[{{\"changes_future_behavior\":true,\"kind\":\"preference|rule\",\"text\":\"...\",\"scope\":\"email|channel|recipient\",\"why\":\"...\"}}]}}",
        clip_chars(before, 3000),
        clip_chars(after, 3000),
    );
    let answer = steve_modules::gateway::tagged(
        "memory_draft_diff",
        ws,
        m.gateway.run(Tier::Fast, &prompt, crate::extract::TIMEOUT),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("gateway: {e}")))?;
    let Some(a) = answer.text.find('{') else {
        return Ok(Vec::new());
    };
    let Some(b) = answer.text.rfind('}') else {
        return Ok(Vec::new());
    };
    let Ok(resp) = serde_json::from_str::<DraftDiffModelResponse>(&answer.text[a..=b]) else {
        return Ok(Vec::new());
    };
    if resp.items.is_empty() {
        return Ok(Vec::new());
    }
    let policy = policy.clone();
    let now = m.clock.now();
    let ws = ws.to_string();
    let user = user.to_string();
    let origin_kind = origin_kind.to_string();
    let draft_id = draft_id.to_string();
    let label = label.to_string();
    let party = recipient_party_id.map(str::to_string);
    let after_clip = clip_chars(after, 300);

    m.blocking(move |m| {
        m.store.write(|tx| {
            let mut outcomes = Vec::new();
            for it in resp.items.into_iter().take(2) {
                let kind = MemoryKind::parse(&it.kind)
                    .filter(|k| matches!(k, MemoryKind::Preference | MemoryKind::Rule))
                    .unwrap_or(MemoryKind::Preference);
                let about = match party.as_deref() {
                    Some(pid) if it.scope.eq_ignore_ascii_case("recipient") => {
                        vec![ObjRef::new("party", pid)]
                    }
                    _ => Vec::new(),
                };
                let (subject_type, subject_id, subject_name) =
                    resolve_subject(tx, &ws, &user, &about)?;
                let item = IngestItem {
                    kind,
                    subject_type,
                    subject_id,
                    subject_name,
                    scope_hint: if it.scope.trim().is_empty() {
                        origin_kind.clone()
                    } else {
                        it.scope.trim().to_string()
                    },
                    text: it.text,
                    why: it.why,
                    changes_future_behavior: it.changes_future_behavior,
                    origin_class: OriginClass::OwnerDraftDiff,
                    source_tier: SourceTrustTier::Tier1Owner,
                    origin_kind: origin_kind.clone(),
                    origin_id: draft_id.clone(),
                    origin_label: label.clone(),
                    episode_key: format!("draft_diff:{draft_id}"),
                    evidence_quote: after_clip.clone(),
                    requested_level: DEFAULT_SILENT_WRITE_LEVEL,
                    visibility: Visibility::Private,
                    actor: "steve".into(),
                    created_by: user.clone(),
                    is_explicit_rule: false,
                };
                outcomes.push(ingest_in(tx, &policy, &ws, &item, now)?);
            }
            Ok::<_, ApiError>(outcomes)
        })
    })
    .await
}

/// Checks if `mem_entries` already exist in `ws` and have at least one active memory file source.
pub fn has_memory_source(c: &mut impl Read, ws: &str) -> steve_store::Result<bool> {
    Ok(!Documents(c).memory_folder_sources(ws)?.is_empty())
}
