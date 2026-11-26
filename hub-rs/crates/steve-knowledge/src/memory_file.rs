//! File-as-single-source-of-truth Markdown parser, 3-step anchor reconciler, atomic file writer,
//! default `$STEVE_HOME/context/` bootstrap, and FTS rebuild engine.
//!
//! Hard invariants:
//! - Markdown files on disk are the sole source of truth for memory prose.
//! - `memory_entries` stores ONLY sidecar annotations, anchors, and irreversible hashes/bigram
//!   fingerprints (never a duplicate prose column).
//! - Generated Markdown is standard tool-agnostic Markdown + YAML front matter with zero
//!   `[mm_xxx]` tags or hidden HTML comments.
//! - `role = knowledge` sources NEVER write rows to `memory_entries`.
//! - `WriteMode` (`read_only` | `propose` | `direct_write`) is enforced at every file write.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde_json::json;
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::{Level, Visibility};
use steve_store::knowledge::{
    Chunks, ConfidenceTier, DocRow, Documents, EntryStatus, MemoryEntries, MemoryEntryRow,
    MemoryEvidences, MemoryKind, MemoryRevisionRow, MemoryRevisions, NewDoc, NewMemoryEntry,
    NewMemoryEvidence, NewMemoryRevision, NewRejectedPattern, OriginClass, ReconcileUpdate,
    RejectedPatterns, RejectionReason, RevisionOp, RevisionStatus, SourceRole, SourceRow,
    SourceTrustTier, WriteMode,
};
use steve_store::{Read, Result, StoreError, W, sha, uid};

use crate::chunk::Chunk;
use crate::extract_text::{self, Extracted, FrontMatter};
use crate::index::{self, ChunkMeta};
use crate::policy::{
    ANCHOR_REBIND_MIN_SIMILARITY, ConfidenceInput, SIMHASH_NEAR_MAX_DISTANCE,
    TOMBSTONE_MATCH_MIN_SIMILARITY, compute_confidence,
};
use crate::sources::folder::{self, FolderPolicy, folder_root};
use crate::state::Knowledge;

/// Standard tool-agnostic Markdown section headings (ASCII-only for clean portability).
pub const HEADING_OVERVIEW: &str = "Overview";
pub const HEADING_RULES: &str = "Rules & Preferences";
pub const HEADING_FACTS: &str = "Facts & Context";
pub const HEADING_EVENTS: &str = "Events & Commitments";

/// Maximum unique bigram hashes persisted in a sidecar `bigram_fingerprint`.
const MAX_FINGERPRINT_BIGRAMS: usize = 256;

/// One parsed logical block in a Markdown memory file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlock {
    pub block_anchor: String,
    pub obsidian_id: Option<String>,
    pub heading_path: String,
    pub line_start: i64,
    pub line_end: i64,
    /// Clean prose text (without leading bullet `- ` or trailing `^block-id`), used ONLY in-memory
    /// for FTS indexing and prompt assembly — never persisted into `memory_entries`.
    pub text: String,
    pub content_sha256: String,
    pub simhash: i64,
    pub bigram_hashes: Vec<u64>,
    pub bigram_fingerprint: String,
    pub kind: MemoryKind,
}

/// Complete parse result of one Markdown memory file.
#[derive(Debug, Clone)]
pub struct ParsedMemoryFile {
    pub front: FrontMatter,
    pub subject_type: String,
    pub subject_id: String,
    pub blocks: Vec<ParsedBlock>,
}

/// Summary returned by [`reconcile_memory_file`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub matched_obsidian: usize,
    pub matched_exact: usize,
    pub matched_fuzzy: usize,
    pub added_owner_file: usize,
    pub deleted_entries: usize,
    pub tombstones_recorded: usize,
    pub active_entries: usize,
}

// ---------------------------------------------------------------- Fingerprinting (sidecar-only)

/// Normalizes a block's prose for hashing and bigram fingerprinting:
/// strips leading bullet syntax, trailing `^block-id`, collapses whitespace, lowercases.
pub fn normalize_block_text(s: &str) -> String {
    let (without_id, _) = strip_obsidian_id(s.trim());
    let body = strip_bullet_prefix(without_id.trim());
    let mut out = String::with_capacity(body.len());
    let mut in_ws = false;
    for c in body.chars() {
        if c.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}

pub fn block_sha256(text: &str) -> String {
    sha(&normalize_block_text(text))
}

fn fnv1a_bigram(c1: char, c2: char) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    let mut buf = [0u8; 4];
    for c in [c1, c2] {
        for &b in c.encode_utf8(&mut buf).as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    }
    h
}

/// Deterministic sorted, deduplicated 64-bit FNV-1a character bigram hashes of normalized text.
/// Preserves exact Sørensen–Dice similarity without storing any plaintext prose in SQLite.
pub fn bigram_hashes(text: &str) -> Vec<u64> {
    let norm = normalize_block_text(text);
    let chars: Vec<char> = norm.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut hashes = Vec::with_capacity(chars.len());
    if chars.len() == 1 {
        hashes.push(fnv1a_bigram(chars[0], '\0'));
    } else {
        for w in chars.windows(2) {
            hashes.push(fnv1a_bigram(w[0], w[1]));
        }
    }
    hashes.sort_unstable();
    hashes.dedup();
    if hashes.len() > MAX_FINGERPRINT_BIGRAMS {
        hashes.truncate(MAX_FINGERPRINT_BIGRAMS);
    }
    hashes
}

pub fn encode_bigram_fingerprint(hashes: &[u64]) -> String {
    let mut out = String::with_capacity(hashes.len() * 17);
    for (i, h) in hashes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        use std::fmt::Write as _;
        let _ = write!(out, "{h:016x}");
    }
    out
}

pub fn decode_bigram_fingerprint(s: &str) -> Vec<u64> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<u64> = s
        .split(',')
        .filter_map(|tok| u64::from_str_radix(tok.trim(), 16).ok())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Sørensen–Dice similarity over two sorted deduplicated bigram-hash sets.
pub fn bigram_dice_similarity(a: &[u64], b: &[u64]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut inter) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    (2.0 * inter as f64) / ((a.len() + b.len()) as f64)
}

/// 64-bit SimHash computed over the bigram hash set.
pub fn compute_simhash(hashes: &[u64]) -> i64 {
    if hashes.is_empty() {
        return 0;
    }
    let mut acc = [0i32; 64];
    for &h in hashes {
        for (bit, slot) in acc.iter_mut().enumerate() {
            if (h >> bit) & 1 == 1 {
                *slot += 1;
            } else {
                *slot -= 1;
            }
        }
    }
    let mut out = 0u64;
    for (bit, &v) in acc.iter().enumerate() {
        if v >= 0 {
            out |= 1u64 << bit;
        }
    }
    out as i64
}

pub fn simhash_distance(a: i64, b: i64) -> u32 {
    ((a as u64) ^ (b as u64)).count_ones()
}

/// Checks whether `(sha, simhash, bigrams)` matches a stored pattern `(p_sha, p_simhash, p_fp)`
/// using exact SHA-256 or bigram similarity / SimHash proximity.
pub fn matches_fingerprint(
    sha256: &str,
    simhash: i64,
    bigrams: &[u64],
    other_sha256: &str,
    other_simhash: i64,
    other_fp: &str,
    min_similarity: f64,
) -> bool {
    if sha256 == other_sha256 {
        return true;
    }
    let other_bg = decode_bigram_fingerprint(other_fp);
    if bigram_dice_similarity(bigrams, &other_bg) >= min_similarity {
        return true;
    }
    if !bigrams.is_empty()
        && !other_bg.is_empty()
        && simhash_distance(simhash, other_simhash) <= SIMHASH_NEAR_MAX_DISTANCE
        && bigram_dice_similarity(bigrams, &other_bg) >= (min_similarity * 0.85)
    {
        return true;
    }
    false
}

/// Checks whether candidate text matches any permanent tombstone in `rejected_patterns`.
pub fn matches_tombstone(
    c: &mut impl Read,
    ws: &str,
    subject_type: &str,
    subject_id: &str,
    text: &str,
) -> Result<bool> {
    let sha256 = block_sha256(text);
    let bg = bigram_hashes(text);
    let sh = compute_simhash(&bg);
    // Check both subject-scoped and workspace-wide tombstones (e.g. if a block moved files).
    for row in RejectedPatterns(c).list_ws(ws)? {
        if (row.subject_type == subject_type && row.subject_id == subject_id)
            || row.content_sha256 == sha256
        {
            if matches_fingerprint(
                &sha256,
                sh,
                &bg,
                &row.content_sha256,
                row.simhash,
                &row.bigram_fingerprint,
                TOMBSTONE_MATCH_MIN_SIMILARITY,
            ) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------- Markdown Parsing

fn strip_bullet_prefix(s: &str) -> &str {
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    s
}

/// Extracts an optional trailing Obsidian block ID (` ^block-id`) from the end of a line/block.
pub fn strip_obsidian_id(s: &str) -> (&str, Option<String>) {
    let trimmed = s.trim_end();
    if let Some((head, tail)) = trimmed.rsplit_once(char::is_whitespace)
        && let Some(id) = tail.strip_prefix('^')
        && !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (head.trim_end(), Some(format!("^{id}")));
    }
    (trimmed, None)
}

pub fn heading_for_kind(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Overview => HEADING_OVERVIEW,
        MemoryKind::Rule | MemoryKind::Preference => HEADING_RULES,
        MemoryKind::Fact => HEADING_FACTS,
        MemoryKind::Event => HEADING_EVENTS,
    }
}

pub fn kind_from_heading(heading_path: &str, default_kind: MemoryKind) -> MemoryKind {
    let lower = heading_path.to_ascii_lowercase();
    if lower.contains("overview") || lower.contains("summary") {
        MemoryKind::Overview
    } else if lower.contains("rule") {
        MemoryKind::Rule
    } else if lower.contains("preference") || lower.contains("pref") {
        MemoryKind::Preference
    } else if lower.contains("event") || lower.contains("commitment") || lower.contains("schedule")
    {
        MemoryKind::Event
    } else if lower.contains("fact") || lower.contains("context") || lower.contains("profile") {
        MemoryKind::Fact
    } else {
        default_kind
    }
}

pub fn safe_slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for c in raw.trim().chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "entry".to_string()
    } else {
        out.chars().take(64).collect()
    }
}

/// Maps `(subject_type, subject_id)` to a clean, tool-agnostic relative path inside a memory dir.
pub fn target_rel_path(subject_type: &str, subject_id: &str) -> String {
    match subject_type {
        "me" => "me.md".to_string(),
        "party" => format!("people/{}.md", safe_slug(subject_id)),
        "project" => format!("projects/{}.md", safe_slug(subject_id)),
        "repo" => format!("repos/{}.md", safe_slug(subject_id)),
        other => format!("{}/{}.md", safe_slug(other), safe_slug(subject_id)),
    }
}

/// Determines `(subject_type, subject_id)` from YAML front matter, falling back to directory layout.
pub fn infer_subject(rel_path: &str, front: &FrontMatter) -> (String, String) {
    if let Some(st) = front.subject_type.as_deref().filter(|s| !s.is_empty()) {
        let sid = front
            .subject_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if st == "me" {
                    "me".to_string()
                } else {
                    stem_of(rel_path)
                }
            });
        return (st.to_string(), sid);
    }
    let norm = rel_path.replace('\\', "/");
    let lower = norm.to_ascii_lowercase();
    if lower == "me.md" || lower.ends_with("/me.md") {
        return ("me".to_string(), "me".to_string());
    }
    if let Some(rest) = norm.strip_prefix("people/") {
        return ("party".to_string(), stem_of(rest));
    }
    if let Some(rest) = norm.strip_prefix("projects/") {
        return ("project".to_string(), stem_of(rest));
    }
    if let Some(rest) = norm.strip_prefix("repos/") {
        return ("repo".to_string(), stem_of(rest));
    }
    ("topic".to_string(), stem_of(&norm))
}

fn stem_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(s, _)| s)
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string()
}

fn is_bullet_line(trimmed: &str) -> bool {
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")
}

fn heading_level_and_text(trimmed: &str) -> Option<(usize, &str)> {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            return Some((hashes, rest.trim()));
        }
    }
    None
}

/// Parses a Markdown memory file into logical blocks (`## Overview`, list bullets, and paragraphs)
/// with deterministic anchors, line ranges, SHA-256, SimHash, and bigram fingerprints.
pub fn parse_memory_file(rel_path: &str, raw_md: &str) -> ParsedMemoryFile {
    let (front, body) = extract_text::split_front_matter(raw_md);
    let prefix_bytes = raw_md.len().saturating_sub(body.len());
    let front_lines = raw_md[..prefix_bytes].matches('\n').count() as i64;
    parse_memory_body(rel_path, front, body, front_lines)
}

/// Parses an already-extracted Markdown document (used by `index::commit_loaded_batch` for
/// both local and Node-backed memory folders).
pub fn parse_memory_extracted(rel_path: &str, ex: &Extracted) -> ParsedMemoryFile {
    parse_memory_body(rel_path, ex.front.clone(), &ex.text, 0)
}

fn parse_memory_body(
    rel_path: &str,
    front: FrontMatter,
    body: &str,
    front_lines: i64,
) -> ParsedMemoryFile {
    let (subject_type, subject_id) = infer_subject(rel_path, &front);

    let lines: Vec<&str> = body.lines().collect();
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut blocks: Vec<ParsedBlock> = Vec::new();
    let mut used_anchors: HashMap<String, usize> = HashMap::new();

    let mut i = 0usize;
    let mut in_fence = false;
    let mut overview_lines: Vec<String> = Vec::new();
    let mut overview_start: Option<i64> = None;
    let mut overview_end: i64 = 0;

    let current_heading = |stack: &[(usize, String)]| -> String {
        stack
            .iter()
            .filter(|(lvl, _)| *lvl >= 2)
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" > ")
    };

    let push_block = |blocks: &mut Vec<ParsedBlock>,
                      used_anchors: &mut HashMap<String, usize>,
                      heading_path: String,
                      line_start: i64,
                      line_end: i64,
                      raw_text: &str,
                      force_kind: Option<MemoryKind>| {
        let (cleaned_ref, obsidian_id) = strip_obsidian_id(raw_text.trim());
        let text = strip_bullet_prefix(cleaned_ref.trim()).trim().to_string();
        if text.is_empty() {
            return;
        }
        let kind = force_kind.unwrap_or_else(|| kind_from_heading(&heading_path, MemoryKind::Fact));
        let content_sha256 = block_sha256(&text);
        let hashes = bigram_hashes(&text);
        let simhash = compute_simhash(&hashes);
        let bigram_fingerprint = encode_bigram_fingerprint(&hashes);
        let base_anchor = if kind == MemoryKind::Overview {
            "overview".to_string()
        } else if let Some(ref oid) = obsidian_id {
            oid.clone()
        } else {
            format!("blk_{}", &content_sha256[..16])
        };
        let seq = used_anchors.entry(base_anchor.clone()).or_insert(0);
        *seq += 1;
        let block_anchor = if *seq == 1 {
            base_anchor
        } else {
            format!("{base_anchor}_{seq}")
        };
        blocks.push(ParsedBlock {
            block_anchor,
            obsidian_id,
            heading_path,
            line_start,
            line_end,
            text,
            content_sha256,
            simhash,
            bigram_fingerprint,
            bigram_hashes: hashes,
            kind,
        });
    };

    let flush_overview = |overview_lines: &mut Vec<String>,
                          overview_start: &mut Option<i64>,
                          overview_end: i64,
                          blocks: &mut Vec<ParsedBlock>,
                          used_anchors: &mut HashMap<String, usize>| {
        if let Some(start) = overview_start.take() {
            let joined = overview_lines.join("\n").trim().to_string();
            overview_lines.clear();
            if !joined.is_empty() {
                push_block(
                    blocks,
                    used_anchors,
                    HEADING_OVERVIEW.to_string(),
                    start,
                    overview_end.max(start),
                    &joined,
                    Some(MemoryKind::Overview),
                );
            }
        } else {
            overview_lines.clear();
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        let line_no = front_lines + (i as i64) + 1;

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if kind_from_heading(&current_heading(&heading_stack), MemoryKind::Fact)
                == MemoryKind::Overview
            {
                if overview_start.is_none() {
                    overview_start = Some(line_no);
                }
                overview_lines.push(line.to_string());
                overview_end = line_no;
            }
            i += 1;
            continue;
        }

        if !in_fence && let Some((lvl, title)) = heading_level_and_text(trimmed) {
            flush_overview(
                &mut overview_lines,
                &mut overview_start,
                overview_end,
                &mut blocks,
                &mut used_anchors,
            );
            while heading_stack.last().is_some_and(|(l, _)| *l >= lvl) {
                heading_stack.pop();
            }
            heading_stack.push((lvl, title.to_string()));
            i += 1;
            continue;
        }

        let heading_path = current_heading(&heading_stack);
        if !in_fence && kind_from_heading(&heading_path, MemoryKind::Fact) == MemoryKind::Overview {
            if !trimmed.is_empty() || overview_start.is_some() {
                if overview_start.is_none() && !trimmed.is_empty() {
                    overview_start = Some(line_no);
                }
                if overview_start.is_some() {
                    overview_lines.push(line.to_string());
                    if !trimmed.is_empty() {
                        overview_end = line_no;
                    }
                }
            }
            i += 1;
            continue;
        }

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if !in_fence && is_bullet_line(trimmed) {
            let start = line_no;
            let mut end = line_no;
            let mut buf = vec![trimmed.to_string()];
            i += 1;
            while i < lines.len() {
                let next = lines[i];
                let nt = next.trim();
                if nt.is_empty() || is_bullet_line(nt) || heading_level_and_text(nt).is_some() {
                    break;
                }
                if next.starts_with("  ") || next.starts_with('\t') {
                    buf.push(nt.to_string());
                    end = front_lines + (i as i64) + 1;
                    i += 1;
                } else {
                    break;
                }
            }
            push_block(
                &mut blocks,
                &mut used_anchors,
                heading_path,
                start,
                end,
                &buf.join(" "),
                None,
            );
            continue;
        }

        // Non-bullet paragraph block.
        let start = line_no;
        let mut end = line_no;
        let mut buf = vec![trimmed.to_string()];
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            let nt = next.trim();
            if nt.is_empty() || is_bullet_line(nt) || heading_level_and_text(nt).is_some() {
                break;
            }
            buf.push(nt.to_string());
            end = front_lines + (i as i64) + 1;
            i += 1;
        }
        push_block(
            &mut blocks,
            &mut used_anchors,
            heading_path,
            start,
            end,
            &buf.join(" "),
            None,
        );
    }

    flush_overview(
        &mut overview_lines,
        &mut overview_start,
        overview_end,
        &mut blocks,
        &mut used_anchors,
    );

    ParsedMemoryFile {
        front,
        subject_type,
        subject_id,
        blocks,
    }
}

// ---------------------------------------------------------------- 3-Step Anchor Reconciliation

/// Reconciles one Markdown file of a `role = memory` source against its sidecar rows in
/// `memory_entries` and updates the rebuildable `kb_chunks` / `kb_fts` cache.
///
/// Hard invariant: if `src.role != SourceRole::Memory`, does NOTHING and writes 0 rows to
/// `memory_entries`.
pub fn reconcile_memory_file(
    tx: &mut W<'_>,
    doc: &DocRow,
    src: &SourceRow,
    raw_md: &str,
    now: i64,
) -> Result<ReconcileReport> {
    if src.role != SourceRole::Memory {
        return Ok(ReconcileReport::default());
    }
    let parsed = parse_memory_file(&doc.r#ref, raw_md);
    let doc_sha = sha(raw_md);
    reconcile_parsed(tx, doc, src, &parsed, &doc_sha, now)
}

/// Reconciles an [`Extracted`] document loaded by the index worker (`kb.index`) for a
/// `role = memory` source.
pub fn reconcile_memory_extracted(
    tx: &mut W<'_>,
    doc: &DocRow,
    src: &SourceRow,
    ex: &Extracted,
    now: i64,
) -> Result<ReconcileReport> {
    if src.role != SourceRole::Memory {
        return Ok(ReconcileReport::default());
    }
    let parsed = parse_memory_extracted(&doc.r#ref, ex);
    let doc_sha = sha(&ex.text);
    reconcile_parsed(tx, doc, src, &parsed, &doc_sha, now)
}

fn reconcile_parsed(
    tx: &mut W<'_>,
    doc: &DocRow,
    src: &SourceRow,
    parsed: &ParsedMemoryFile,
    doc_sha: &str,
    now: i64,
) -> Result<ReconcileReport> {
    let existing: Vec<MemoryEntryRow> = MemoryEntries(tx)
        .by_source_path(&src.id, &doc.r#ref)?
        .into_iter()
        .filter(|e| e.status != EntryStatus::Deleted)
        .collect();

    let mut report = ReconcileReport::default();
    let mut matched_blocks: HashMap<usize, usize> = HashMap::new(); // block_idx -> existing_idx
    let mut used_existing: HashSet<usize> = HashSet::new();

    // Step 0: Explicit Obsidian `^block-id` match (or `overview` singleton anchor).
    for (bi, blk) in parsed.blocks.iter().enumerate() {
        if let Some(ref oid) = blk.obsidian_id {
            if let Some((ei, _)) = existing.iter().enumerate().find(|(ei, e)| {
                !used_existing.contains(ei) && e.obsidian_id.as_deref() == Some(oid)
            }) {
                matched_blocks.insert(bi, ei);
                used_existing.insert(ei);
                report.matched_obsidian += 1;
            }
        } else if blk.kind == MemoryKind::Overview {
            if let Some((ei, _)) = existing
                .iter()
                .enumerate()
                .find(|(ei, e)| !used_existing.contains(ei) && e.kind == MemoryKind::Overview)
            {
                matched_blocks.insert(bi, ei);
                used_existing.insert(ei);
                report.matched_exact += 1;
            }
        }
    }

    // Step 1: Exact normalized `content_sha256` match (1-to-1, nearest line_start tiebreaker).
    for (bi, blk) in parsed.blocks.iter().enumerate() {
        if matched_blocks.contains_key(&bi) {
            continue;
        }
        let best = existing
            .iter()
            .enumerate()
            .filter(|(ei, e)| !used_existing.contains(ei) && e.content_sha256 == blk.content_sha256)
            .min_by_key(|(_, e)| (e.line_start - blk.line_start).abs())
            .map(|(ei, _)| ei);
        if let Some(ei) = best {
            matched_blocks.insert(bi, ei);
            used_existing.insert(ei);
            report.matched_exact += 1;
        }
    }

    // Step 2: Fuzzy bigram Dice similarity (>= 0.65), strictly mutual unique best match.
    let unmatched_blocks: Vec<usize> = (0..parsed.blocks.len())
        .filter(|bi| !matched_blocks.contains_key(bi))
        .collect();
    let unmatched_existing: Vec<usize> = (0..existing.len())
        .filter(|ei| !used_existing.contains(ei))
        .collect();
    if !unmatched_blocks.is_empty() && !unmatched_existing.is_empty() {
        let existing_bg: HashMap<usize, Vec<u64>> = unmatched_existing
            .iter()
            .map(|&ei| {
                (
                    ei,
                    decode_bigram_fingerprint(&existing[ei].bigram_fingerprint),
                )
            })
            .collect();
        let mut best_for_block: HashMap<usize, Option<usize>> = HashMap::new();
        let mut best_for_existing: HashMap<usize, Option<usize>> = HashMap::new();
        let mut scores: HashMap<(usize, usize), f64> = HashMap::new();

        for &bi in &unmatched_blocks {
            let blk = &parsed.blocks[bi];
            if blk.kind == MemoryKind::Overview {
                continue;
            }
            for &ei in &unmatched_existing {
                if existing[ei].kind == MemoryKind::Overview {
                    continue;
                }
                let sim = bigram_dice_similarity(&blk.bigram_hashes, &existing_bg[&ei]);
                if sim >= ANCHOR_REBIND_MIN_SIMILARITY {
                    scores.insert((bi, ei), sim);
                }
            }
        }

        for &bi in &unmatched_blocks {
            let mut candidates: Vec<(usize, f64, bool)> = unmatched_existing
                .iter()
                .filter_map(|&ei| {
                    let s = *scores.get(&(bi, ei))?;
                    let same_heading = existing[ei].heading_path == parsed.blocks[bi].heading_path;
                    Some((ei, s, same_heading))
                })
                .collect();
            candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
            let picked = match candidates.as_slice() {
                [(ei, _, _)] => Some(*ei),
                [(e1, s1, h1), (_, s2, h2), ..] if (s1 - s2).abs() > 1e-6 || (*h1 && !*h2) => {
                    Some(*e1)
                }
                _ => None,
            };
            best_for_block.insert(bi, picked);
        }

        for &ei in &unmatched_existing {
            let mut candidates: Vec<(usize, f64, bool)> = unmatched_blocks
                .iter()
                .filter_map(|&bi| {
                    let s = *scores.get(&(bi, ei))?;
                    let same_heading = existing[ei].heading_path == parsed.blocks[bi].heading_path;
                    Some((bi, s, same_heading))
                })
                .collect();
            candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
            let picked = match candidates.as_slice() {
                [(bi, _, _)] => Some(*bi),
                [(b1, s1, h1), (_, s2, h2), ..] if (s1 - s2).abs() > 1e-6 || (*h1 && !*h2) => {
                    Some(*b1)
                }
                _ => None,
            };
            best_for_existing.insert(ei, picked);
        }

        for (&bi, &opt_ei) in &best_for_block {
            if let Some(ei) = opt_ei
                && best_for_existing.get(&ei).copied().flatten() == Some(bi)
                && !used_existing.contains(&ei)
            {
                matched_blocks.insert(bi, ei);
                used_existing.insert(ei);
                report.matched_fuzzy += 1;
            }
        }
    }

    // Resolve any unique-constraint anchor collision cleanly before updating anchors:
    // if an existing unmatched entry holds a block_anchor that a newly matched/inserted block needs,
    // Step 3b marks it deleted and gives it a tombstone suffix so (source_id, rel_path, block_anchor)
    // stays collision-free.
    for (ei, old) in existing.iter().enumerate() {
        if used_existing.contains(&ei) {
            continue;
        }
        // Step 3b: Entry disappeared from disk -> mark Deleted + tombstone.
        let archived_anchor = format!("deleted_{}_{}", old.id, now);
        MemoryEntries(tx).update_reconciled(
            &old.id,
            &ReconcileUpdate {
                doc_id: old.doc_id.as_deref(),
                block_anchor: &archived_anchor,
                obsidian_id: None,
                heading_path: &old.heading_path,
                line_start: old.line_start,
                line_end: old.line_end,
                content_sha256: &old.content_sha256,
                simhash: old.simhash,
                bigram_fingerprint: &old.bigram_fingerprint,
                kind: old.kind,
                subject_type: &old.subject_type,
                subject_id: &old.subject_id,
                confidence_tier: old.confidence_tier,
                origin_class: old.origin_class,
                correction_count: old.correction_count,
                level: old.level,
                visibility: &old.visibility,
                status: EntryStatus::Deleted,
                verified_at: old.verified_at,
                now,
            },
        )?;
        index::remove(tx, "memory_entry", &old.id)?;
        report.deleted_entries += 1;

        if old.kind != MemoryKind::Overview {
            let recorded = RejectedPatterns(tx).record(&NewRejectedPattern {
                workspace_id: &src.workspace_id,
                subject_type: &old.subject_type,
                subject_id: &old.subject_id,
                content_sha256: &old.content_sha256,
                simhash: old.simhash,
                bigram_fingerprint: &old.bigram_fingerprint,
                reason: RejectionReason::OwnerDeleted,
                entry_id: Some(&old.id),
                created_by: &doc.created_by,
                now,
            })?;
            if recorded {
                report.tombstones_recorded += 1;
            }
            if old.origin_class != OriginClass::OwnerFile {
                metrics::counter!("steve_memory_reverted_total", "reason" => "owner_deleted")
                    .increment(1);
            }
        }
    }

    // Temporarily move matched existing anchors if swapping anchors between two rows in one file.
    for (&bi, &ei) in &matched_blocks {
        let old = &existing[ei];
        let blk = &parsed.blocks[bi];
        if old.block_anchor != blk.block_anchor {
            let tmp = format!("rebind_{}_{}", old.id, bi);
            MemoryEntries(tx).update_reconciled(
                &old.id,
                &ReconcileUpdate {
                    doc_id: Some(&doc.id),
                    block_anchor: &tmp,
                    obsidian_id: old.obsidian_id.as_deref(),
                    heading_path: &old.heading_path,
                    line_start: old.line_start,
                    line_end: old.line_end,
                    content_sha256: &old.content_sha256,
                    simhash: old.simhash,
                    bigram_fingerprint: &old.bigram_fingerprint,
                    kind: old.kind,
                    subject_type: &old.subject_type,
                    subject_id: &old.subject_id,
                    confidence_tier: old.confidence_tier,
                    origin_class: old.origin_class,
                    correction_count: old.correction_count,
                    level: old.level,
                    visibility: &old.visibility,
                    status: EntryStatus::Active,
                    verified_at: old.verified_at,
                    now,
                },
            )?;
        }
    }

    // Apply updates for matched blocks and inserts for new blocks (Step 3a), then refresh FTS cache.
    let eff_vis = doc.eff_visibility(src);
    let eff_level = doc.eff_level(src);
    let doc_label = index::doc_label(src, &doc.title);

    // Clear any legacy `chunk` rows for this memory doc so it only appears via `memory_entry`.
    index::remove(tx, "chunk", &doc.id)?;

    let mut active_count = 0usize;
    for (bi, blk) in parsed.blocks.iter().enumerate() {
        let entry = if let Some(&ei) = matched_blocks.get(&bi) {
            let old = &existing[ei];
            let edited = old.content_sha256 != blk.content_sha256;
            let (origin_class, correction_count, confidence_tier, verified_at) =
                if edited && blk.kind != MemoryKind::Overview {
                    let next_corr = old.correction_count + 1;
                    let conf = compute_confidence(&ConfidenceInput {
                        origin_class: OriginClass::OwnerEdited,
                        source_tier: SourceTrustTier::Tier1Owner,
                        episode_count: old.episode_count,
                        correction_count: next_corr,
                        has_conflict: false,
                        is_explicit_rule: true,
                        promote_min_episodes: crate::policy::PROMOTE_MIN_EPISODES,
                    });
                    (OriginClass::OwnerEdited, next_corr, conf, Some(now))
                } else {
                    (
                        old.origin_class,
                        old.correction_count,
                        old.confidence_tier,
                        old.verified_at,
                    )
                };
            // Preserve per-entry sidecar level/visibility/pinned unless doc front matter explicitly
            // narrows or the entry had none; sidecar overrides survive edits and moves.
            MemoryEntries(tx).update_reconciled(
                &old.id,
                &ReconcileUpdate {
                    doc_id: Some(&doc.id),
                    block_anchor: &blk.block_anchor,
                    obsidian_id: blk.obsidian_id.as_deref(),
                    heading_path: &blk.heading_path,
                    line_start: blk.line_start,
                    line_end: blk.line_end,
                    content_sha256: &blk.content_sha256,
                    simhash: blk.simhash,
                    bigram_fingerprint: &blk.bigram_fingerprint,
                    kind: blk.kind,
                    subject_type: &parsed.subject_type,
                    subject_id: &parsed.subject_id,
                    confidence_tier,
                    origin_class,
                    correction_count,
                    level: old.level,
                    visibility: &old.visibility,
                    status: EntryStatus::Active,
                    verified_at,
                    now,
                },
            )?;
            MemoryEntries(tx)
                .get(&old.id)?
                .unwrap_or_else(|| old.clone())
        } else {
            // Step 3a: New block written directly by the owner on disk -> OwnerFile / Verified.
            report.added_owner_file += 1;
            let pinned = parsed.front.pinned.unwrap_or(false);
            MemoryEntries(tx).insert(&NewMemoryEntry {
                id: None,
                workspace_id: &src.workspace_id,
                source_id: &src.id,
                doc_id: Some(&doc.id),
                rel_path: &doc.r#ref,
                block_anchor: &blk.block_anchor,
                obsidian_id: blk.obsidian_id.as_deref(),
                heading_path: &blk.heading_path,
                line_start: blk.line_start,
                line_end: blk.line_end,
                content_sha256: &blk.content_sha256,
                simhash: blk.simhash,
                bigram_fingerprint: &blk.bigram_fingerprint,
                kind: blk.kind,
                subject_type: &parsed.subject_type,
                subject_id: &parsed.subject_id,
                scope_hint: "",
                confidence_tier: ConfidenceTier::Verified,
                origin_class: OriginClass::OwnerFile,
                source_tier: SourceTrustTier::Tier1Owner,
                episode_count: 1,
                correction_count: 0,
                has_conflict: false,
                level: eff_level,
                visibility: &eff_vis,
                pinned,
                status: EntryStatus::Active,
                verified_at: Some(now),
                created_by: &doc.created_by,
                now,
            })?
        };
        index_memory_entry_chunk(tx, &entry, src, &doc.title, &doc_label, &blk.text, now)?;
        active_count += 1;
    }

    report.active_entries = active_count;
    Documents(tx).set_doc_indexed(&doc.id, doc_sha, active_count as i64, "active", None, now)?;
    metrics::counter!("steve_memory_reconcile_total", "result" => "ok").increment(1);
    Ok(report)
}

/// Marks all active entries of a deleted memory file as `Deleted`, removes their FTS cache rows,
/// and records tombstones for every non-overview block.
pub fn mark_file_deleted(
    tx: &mut W<'_>,
    src: &SourceRow,
    rel_path: &str,
    now: i64,
) -> Result<usize> {
    if src.role != SourceRole::Memory {
        return Ok(0);
    }
    let active = MemoryEntries(tx).active_by_source_path(&src.id, rel_path)?;
    let mut n = 0usize;
    for entry in active {
        MemoryEntries(tx).set_status(&entry.id, EntryStatus::Deleted, now)?;
        index::remove(tx, "memory_entry", &entry.id)?;
        if entry.kind != MemoryKind::Overview {
            RejectedPatterns(tx).record(&NewRejectedPattern {
                workspace_id: &src.workspace_id,
                subject_type: &entry.subject_type,
                subject_id: &entry.subject_id,
                content_sha256: &entry.content_sha256,
                simhash: entry.simhash,
                bigram_fingerprint: &entry.bigram_fingerprint,
                reason: RejectionReason::OwnerDeleted,
                entry_id: Some(&entry.id),
                created_by: &entry.created_by,
                now,
            })?;
            if entry.origin_class != OriginClass::OwnerFile {
                metrics::counter!("steve_memory_reverted_total", "reason" => "file_deleted")
                    .increment(1);
            }
        }
        n += 1;
    }
    Ok(n)
}

/// Writes the rebuildable FTS cache row (`kb_chunks` + `kb_fts`) for one active `memory_entries`
/// row, carrying its authoritative sidecar `level` and `visibility`.
pub fn index_memory_entry_chunk(
    tx: &mut W<'_>,
    entry: &MemoryEntryRow,
    src: &SourceRow,
    doc_title: &str,
    doc_label: &str,
    block_text: &str,
    now: i64,
) -> Result<()> {
    if entry.status != EntryStatus::Active || entry.level == Level::Never {
        index::remove(tx, "memory_entry", &entry.id)?;
        return Ok(());
    }
    let meta = ChunkMeta {
        ws: &entry.workspace_id,
        obj_type: "memory_entry",
        obj_id: &entry.id,
        title: doc_title,
        visibility: &entry.visibility,
        level: entry.level,
        created_by: &entry.created_by,
        source_kind: &src.kind,
        label: doc_label,
        updated: now,
    };
    let c = Chunk {
        seq: 0,
        heading: entry.heading_path.clone(),
        text: block_text.to_string(),
        start: entry.line_start.max(0) as usize,
        end: entry.line_end.max(0) as usize,
    };
    index::replace(tx, &meta, &[c])?;
    Ok(())
}

// ---------------------------------------------------------------- Disk I/O & Atomic Writes

fn validate_rel_path(rel_path: &str) -> ApiResult<PathBuf> {
    let p = Path::new(rel_path);
    if rel_path.trim().is_empty()
        || p.is_absolute()
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(ApiError::invalid("rel_path"));
    }
    Ok(p.to_path_buf())
}

/// Reads a memory file from disk under `src`'s root (safe path containment check).
pub fn read_memory_file_from_disk(src: &SourceRow, rel_path: &str) -> ApiResult<Option<String>> {
    let Some(root) = folder_root(src) else {
        return Ok(None);
    };
    let rel = validate_rel_path(rel_path)?;
    let full = root.join(rel);
    match std::fs::read_to_string(&full) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::Internal(format!("read memory file failed: {e}"))),
    }
}

/// Reads the live prose of one `memory_entries` row directly from its Markdown file on disk.
pub fn read_entry_text_from_disk(
    c: &mut impl Read,
    entry: &MemoryEntryRow,
) -> ApiResult<Option<String>> {
    let Some(src) = Documents(c).source(&entry.source_id)? else {
        return Ok(None);
    };
    let Some(raw) = read_memory_file_from_disk(&src, &entry.rel_path)? else {
        return Ok(None);
    };
    let parsed = parse_memory_file(&entry.rel_path, &raw);
    if let Some(blk) = parsed
        .blocks
        .iter()
        .find(|b| b.block_anchor == entry.block_anchor || b.content_sha256 == entry.content_sha256)
    {
        return Ok(Some(blk.text.clone()));
    }
    Ok(None)
}

fn atomic_write_file(root: &Path, rel_path: &str, content: &str) -> ApiResult<(i64, i64)> {
    let rel = validate_rel_path(rel_path)?;
    let full = root.join(&rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("create memory dir failed: {e}")))?;
    }
    let parent = full.parent().unwrap_or(root);
    let tmp = parent.join(format!(".steve.tmp.{}", uid("mf")));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ApiError::Internal(format!("create temp memory file failed: {e}")))?;
        f.write_all(content.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| ApiError::Internal(format!("write temp memory file failed: {e}")))?;
    }
    std::fs::rename(&tmp, &full)
        .map_err(|e| ApiError::Internal(format!("atomic rename memory file failed: {e}")))?;
    let meta = std::fs::metadata(&full)
        .map_err(|e| ApiError::Internal(format!("stat memory file failed: {e}")))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok((meta.len() as i64, mtime))
}

/// Ensures the workspace has a `role = memory` folder source. If none is configured, bootstraps
/// the default `$STEVE_HOME/context/` (or `$STEVE_HOME/context/<ws>`) with `WriteMode::DirectWrite`.
pub fn ensure_default_memory_source(
    tx: &mut W<'_>,
    policy: &FolderPolicy,
    ws: &str,
    actor: &str,
    now: i64,
) -> ApiResult<SourceRow> {
    let existing = Documents(tx).memory_folder_sources(ws)?;
    if let Some(writable) = existing
        .iter()
        .find(|s| !s.paused && s.write_mode == WriteMode::DirectWrite)
    {
        return Ok(writable.clone());
    }
    if let Some(propose) = existing
        .iter()
        .find(|s| !s.paused && s.write_mode == WriteMode::Propose)
    {
        return Ok(propose.clone());
    }
    if let Some(any) = existing.into_iter().find(|s| !s.paused) {
        return Ok(any);
    }
    let dir = policy.default_context_dir(ws);
    for sub in ["people", "projects", "repos"] {
        std::fs::create_dir_all(dir.join(sub))
            .map_err(|e| ApiError::Internal(format!("create default context dir failed: {e}")))?;
    }
    let canon = std::fs::canonicalize(&dir).unwrap_or(dir);
    let id = uid("ks");
    let config = json!({
        "path": canon.to_string_lossy(),
        "include": ["**/*.md"],
        "exclude": [".obsidian/**", ".git/**"],
    });
    let name = steve_i18n::stored(|| steve_i18n::t!("knowledge.builtin.context"));
    Documents(tx).insert_source(
        &id,
        ws,
        "folder",
        &name,
        &config,
        &Visibility::Private,
        Level::Reasoning,
        SourceRole::Memory,
        WriteMode::DirectWrite,
        actor,
        now,
    )?;
    tx.audit(
        ws,
        actor,
        "kb_source.bootstrap_context",
        &id,
        &json!({ "path": config["path"], "role": "memory", "write_mode": "direct_write" }),
    )?;
    Documents(tx)
        .source(&id)?
        .ok_or_else(|| ApiError::not_found(folder::NOUN_SOURCE))
}

// ---------------------------------------------------------------- Markdown Mutation Helpers

fn render_new_memory_doc(
    title: &str,
    subject_type: &str,
    subject_id: &str,
    level: Level,
    visibility: &Visibility,
    heading: &str,
    bullet_line: &str,
) -> String {
    format!(
        "---\nsubject_type: {subject_type}\nsubject_id: {subject_id}\nsteve:\n  level: {}\n  visibility: {}\n---\n\n# {title}\n\n## {heading}\n\n{bullet_line}\n",
        level.code(),
        visibility.code()
    )
}

/// Inserts a new clean bullet line under `## <heading>` (creating the heading at EOF if missing).
/// Never writes `[mm_xxx]` tags or hidden HTML comments.
pub fn render_append_block(
    existing_raw: Option<&str>,
    title: &str,
    subject_type: &str,
    subject_id: &str,
    level: Level,
    visibility: &Visibility,
    heading: &str,
    text: &str,
) -> String {
    let clean = text.trim().replace('\n', " ");
    let bullet = format!("- {clean}");
    let Some(raw) = existing_raw.filter(|s| !s.trim().is_empty()) else {
        return render_new_memory_doc(
            title,
            subject_type,
            subject_id,
            level,
            visibility,
            heading,
            &bullet,
        );
    };

    let target_h = format!("## {heading}");
    let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
    if let Some(pos) = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&target_h))
    {
        // Find end of this section (just before the next `## ` or `# ` heading).
        let mut insert_at = lines.len();
        for (idx, line) in lines.iter().enumerate().skip(pos + 1) {
            let t = line.trim();
            if t.starts_with("## ") || t.starts_with("# ") {
                insert_at = idx;
                break;
            }
        }
        // Trim trailing blank lines immediately before insert_at so the bullet lands cleanly
        // inside the section, with a blank line after if another heading follows.
        while insert_at > pos + 1 && lines[insert_at - 1].trim().is_empty() {
            insert_at -= 1;
        }
        if insert_at == pos + 1 {
            lines.insert(insert_at, String::new());
            insert_at += 1;
        }
        lines.insert(insert_at, bullet);
        if insert_at + 1 < lines.len() && !lines[insert_at + 1].trim().is_empty() {
            lines.insert(insert_at + 1, String::new());
        }
        let mut out = lines.join("\n");
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    } else {
        let mut out = raw.trim_end().to_string();
        out.push_str("\n\n");
        out.push_str(&target_h);
        out.push_str("\n\n");
        out.push_str(&bullet);
        out.push('\n');
        out
    }
}

/// Replaces or inserts the `## Overview` section at the top of the Markdown body (right after `# Title`).
pub fn render_replace_overview(existing_raw: &str, overview_body: &str) -> String {
    let clean_overview = overview_body.trim();
    let target_h = format!("## {HEADING_OVERVIEW}");
    let lines: Vec<&str> = existing_raw.lines().collect();
    if let Some(pos) = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&target_h))
    {
        let mut end = lines.len();
        for (idx, line) in lines.iter().enumerate().skip(pos + 1) {
            let t = line.trim();
            if t.starts_with("## ") || t.starts_with("# ") {
                end = idx;
                break;
            }
        }
        let mut out = Vec::with_capacity(lines.len() + 4);
        out.extend(lines[..=pos].iter().copied());
        out.push("");
        out.push(clean_overview);
        if end < lines.len() {
            out.push("");
            out.extend(lines[end..].iter().copied());
        }
        let mut joined = out.join("\n");
        if !joined.ends_with('\n') {
            joined.push('\n');
        }
        joined
    } else {
        // Insert after `# Title` (or right after front matter if no H1).
        let (front, body) = extract_text::split_front_matter(existing_raw);
        let _ = front;
        let prefix_len = existing_raw.len() - body.len();
        let front_part = &existing_raw[..prefix_len];
        let body_lines: Vec<&str> = body.lines().collect();
        let h1_pos = body_lines
            .iter()
            .position(|l| l.trim_start().starts_with("# "));
        let mut out = String::from(front_part);
        if let Some(idx) = h1_pos {
            let before = body_lines[..=idx].join("\n");
            let after = body_lines[idx + 1..].join("\n");
            out.push_str(before.trim_end());
            out.push_str("\n\n");
            out.push_str(&target_h);
            out.push_str("\n\n");
            out.push_str(clean_overview);
            if !after.trim().is_empty() {
                out.push_str("\n\n");
                out.push_str(after.trim_start());
            }
            out.push('\n');
        } else {
            out.push_str(&target_h);
            out.push_str("\n\n");
            out.push_str(clean_overview);
            if !body.trim().is_empty() {
                out.push_str("\n\n");
                out.push_str(body.trim_start());
            }
            out.push('\n');
        }
        out
    }
}

/// Removes the block matching `target_sha` (or exact `target_block` line) from a Markdown file.
pub fn render_remove_block(
    existing_raw: &str,
    target_sha: &str,
    target_block: &str,
) -> (String, bool) {
    let lines: Vec<&str> = existing_raw.lines().collect();
    let target_norm = normalize_block_text(target_block);
    let mut out = Vec::with_capacity(lines.len());
    let mut removed = false;
    for line in lines {
        if !removed {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                let norm = normalize_block_text(trimmed);
                if (!target_sha.is_empty() && sha(&norm) == target_sha)
                    || (!target_norm.is_empty() && norm == target_norm)
                {
                    removed = true;
                    continue;
                }
            }
        }
        out.push(line);
    }
    let mut joined = out.join("\n");
    if !joined.is_empty() && !joined.ends_with('\n') {
        joined.push('\n');
    }
    (joined, removed)
}

fn make_diff_preview(before: &str, after: &str) -> String {
    let mut out = String::new();
    if !before.trim().is_empty() {
        for l in before.lines() {
            out.push_str("- ");
            out.push_str(l);
            out.push('\n');
        }
    }
    if !after.trim().is_empty() {
        for l in after.lines() {
            out.push_str("+ ");
            out.push_str(l);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------- Write Request & Execution

#[derive(Debug, Clone)]
pub struct MemoryWriteRequest<'a> {
    pub workspace_id: &'a str,
    pub rel_path: &'a str,
    pub doc_title: &'a str,
    pub kind: MemoryKind,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub scope_hint: &'a str,
    pub text: &'a str,
    pub reason: &'a str,
    pub origin_class: OriginClass,
    pub source_tier: SourceTrustTier,
    pub confidence_tier: ConfidenceTier,
    pub episode_count: i64,
    pub level: Level,
    pub visibility: &'a Visibility,
    pub actor: &'a str,
    pub created_by: &'a str,
    pub evidence: Vec<MemoryWriteEvidence>,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct MemoryWriteEvidence {
    pub origin_kind: String,
    pub origin_id: String,
    pub origin_label: String,
    pub episode_key: String,
    pub quote_hash: String,
    pub rationale: String,
    pub source_tier: SourceTrustTier,
}

#[derive(Debug, Clone)]
pub enum WriteOutcome {
    RefusedReadOnly {
        source_id: String,
    },
    Proposed {
        revision: MemoryRevisionRow,
    },
    Applied {
        entry: MemoryEntryRow,
        revision: MemoryRevisionRow,
    },
}

/// Applies a silent memory addition to `source`, strictly enforcing `source.write_mode`:
/// - `ReadOnly`: refuses and writes nothing to disk or DB.
/// - `Propose`: persists a `memory_revisions` row (`status = proposed`), touches nothing on disk.
/// - `DirectWrite`: atomically writes the clean Markdown file, records `memory_revisions` (`applied`),
///   upserts sidecar `memory_entries` + `memory_evidence`, and updates `kb_chunks` / `kb_fts`.
pub fn apply_memory_add(
    tx: &mut W<'_>,
    source: &SourceRow,
    req: &MemoryWriteRequest<'_>,
) -> ApiResult<WriteOutcome> {
    if source.role != SourceRole::Memory {
        metrics::counter!("steve_memory_writes_total", "mode" => "non_memory", "result" => "refused")
            .increment(1);
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.memory.source_not_memory"
        )));
    }
    let heading = heading_for_kind(req.kind);
    let clean_text = req.text.trim().replace('\n', " ");
    let after_bullet = format!("- {clean_text}");
    let diff = make_diff_preview("", &after_bullet);

    match source.write_mode {
        WriteMode::ReadOnly => {
            metrics::counter!("steve_memory_writes_total", "mode" => "read_only", "result" => "refused")
                .increment(1);
            Ok(WriteOutcome::RefusedReadOnly {
                source_id: source.id.clone(),
            })
        }
        WriteMode::Propose => {
            let rev = MemoryRevisions(tx).insert(&NewMemoryRevision {
                workspace_id: req.workspace_id,
                source_id: &source.id,
                rel_path: req.rel_path,
                entry_id: None,
                op: RevisionOp::Add,
                write_mode: WriteMode::Propose,
                status: RevisionStatus::Proposed,
                heading_path: heading,
                before_block: "",
                after_block: &clean_text,
                diff_preview: &diff,
                reason: req.reason,
                origin_class: req.origin_class.code(),
                actor: req.actor,
                now: req.now,
                applied_at: None,
            })?;
            metrics::counter!("steve_memory_writes_total", "mode" => "propose", "result" => "proposed")
                .increment(1);
            crate::state::emit_change(tx, req.workspace_id, &rev.id);
            Ok(WriteOutcome::Proposed { revision: rev })
        }
        WriteMode::DirectWrite => {
            let root = folder_root(source)
                .ok_or_else(|| ApiError::Internal("memory source missing path".into()))?;
            let existing_raw = read_memory_file_from_disk(source, req.rel_path)?;
            let updated_md = render_append_block(
                existing_raw.as_deref(),
                req.doc_title,
                req.subject_type,
                req.subject_id,
                req.level,
                req.visibility,
                heading,
                &clean_text,
            );
            let (size, mtime) = atomic_write_file(&root, req.rel_path, &updated_md)?;

            // Ensure `kb_docs` row exists and matches disk metadata so scans see it cleanly.
            let doc = match Documents(tx).doc_by_ref(&source.id, req.rel_path)? {
                Some(d) => {
                    Documents(tx).set_doc_seen(&d.id, size, mtime, req.now)?;
                    Documents(tx).doc(&d.id)?.unwrap_or(d)
                }
                None => {
                    let doc_id = uid("kd");
                    Documents(tx).insert_doc(
                        req.workspace_id,
                        &NewDoc {
                            id: doc_id.clone(),
                            source_id: source.id.clone(),
                            r#ref: req.rel_path.to_string(),
                            title: req.doc_title.to_string(),
                            mime: "text/markdown".to_string(),
                            size,
                            mtime: Some(mtime),
                            created_by: req.created_by.to_string(),
                            ..Default::default()
                        },
                        req.now,
                    )?;
                    Documents(tx)
                        .doc(&doc_id)?
                        .ok_or_else(|| ApiError::Internal("inserted memory doc missing".into()))?
                }
            };

            // Parse the written file to locate the exact new block's line range & anchor.
            let parsed = parse_memory_file(req.rel_path, &updated_md);
            let target_sha = block_sha256(&clean_text);
            let blk = parsed
                .blocks
                .iter()
                .rev()
                .find(|b| b.content_sha256 == target_sha)
                .ok_or_else(|| ApiError::Internal("written memory block not found".into()))?;

            // Upsert or insert sidecar `memory_entries` row with exact provenance metadata.
            let existing_same = MemoryEntries(tx)
                .active_by_source_path(&source.id, req.rel_path)?
                .into_iter()
                .find(|e| e.block_anchor == blk.block_anchor || e.content_sha256 == target_sha);

            let entry = if let Some(old) = existing_same {
                MemoryEntries(tx).update_reconciled(
                    &old.id,
                    &ReconcileUpdate {
                        doc_id: Some(&doc.id),
                        block_anchor: &blk.block_anchor,
                        obsidian_id: blk.obsidian_id.as_deref(),
                        heading_path: &blk.heading_path,
                        line_start: blk.line_start,
                        line_end: blk.line_end,
                        content_sha256: &blk.content_sha256,
                        simhash: blk.simhash,
                        bigram_fingerprint: &blk.bigram_fingerprint,
                        kind: req.kind,
                        subject_type: req.subject_type,
                        subject_id: req.subject_id,
                        confidence_tier: req.confidence_tier,
                        origin_class: req.origin_class,
                        correction_count: old.correction_count,
                        level: req.level,
                        visibility: req.visibility,
                        status: EntryStatus::Active,
                        verified_at: (req.confidence_tier == ConfidenceTier::Verified)
                            .then_some(req.now)
                            .or(old.verified_at),
                        now: req.now,
                    },
                )?;
                MemoryEntries(tx).get(&old.id)?.unwrap_or(old)
            } else {
                MemoryEntries(tx).insert(&NewMemoryEntry {
                    id: None,
                    workspace_id: req.workspace_id,
                    source_id: &source.id,
                    doc_id: Some(&doc.id),
                    rel_path: req.rel_path,
                    block_anchor: &blk.block_anchor,
                    obsidian_id: blk.obsidian_id.as_deref(),
                    heading_path: &blk.heading_path,
                    line_start: blk.line_start,
                    line_end: blk.line_end,
                    content_sha256: &blk.content_sha256,
                    simhash: blk.simhash,
                    bigram_fingerprint: &blk.bigram_fingerprint,
                    kind: req.kind,
                    subject_type: req.subject_type,
                    subject_id: req.subject_id,
                    scope_hint: req.scope_hint,
                    confidence_tier: req.confidence_tier,
                    origin_class: req.origin_class,
                    source_tier: req.source_tier,
                    episode_count: req.episode_count.max(1),
                    correction_count: 0,
                    has_conflict: false,
                    level: req.level,
                    visibility: req.visibility,
                    pinned: false,
                    status: EntryStatus::Active,
                    verified_at: (req.confidence_tier == ConfidenceTier::Verified)
                        .then_some(req.now),
                    created_by: req.created_by,
                    now: req.now,
                })?
            };

            for ev in &req.evidence {
                MemoryEvidences(tx).add(&NewMemoryEvidence {
                    workspace_id: req.workspace_id,
                    entry_id: &entry.id,
                    origin_kind: &ev.origin_kind,
                    origin_id: &ev.origin_id,
                    origin_label: &ev.origin_label,
                    episode_key: &ev.episode_key,
                    quote_hash: &ev.quote_hash,
                    rationale: &ev.rationale,
                    source_tier: ev.source_tier,
                    now: req.now,
                })?;
            }

            let doc_label = index::doc_label(source, &doc.title);
            index_memory_entry_chunk(
                tx, &entry, source, &doc.title, &doc_label, &blk.text, req.now,
            )?;

            let doc_sha = sha(&updated_md);
            let active_n = MemoryEntries(tx)
                .active_by_source_path(&source.id, req.rel_path)?
                .len() as i64;
            Documents(tx).set_doc_indexed(&doc.id, &doc_sha, active_n, "active", None, req.now)?;

            let rev = MemoryRevisions(tx).insert(&NewMemoryRevision {
                workspace_id: req.workspace_id,
                source_id: &source.id,
                rel_path: req.rel_path,
                entry_id: Some(&entry.id),
                op: RevisionOp::Add,
                write_mode: WriteMode::DirectWrite,
                status: RevisionStatus::Applied,
                heading_path: heading,
                before_block: "",
                after_block: &clean_text,
                diff_preview: &diff,
                reason: req.reason,
                origin_class: req.origin_class.code(),
                actor: req.actor,
                now: req.now,
                applied_at: Some(req.now),
            })?;
            metrics::counter!("steve_memory_writes_total", "mode" => "direct_write", "result" => "applied")
                .increment(1);
            crate::state::emit_change(tx, req.workspace_id, &entry.id);
            Ok(WriteOutcome::Applied {
                entry,
                revision: rev,
            })
        }
    }
}

/// Applies an `## Overview` refresh to a memory file (enforcing `WriteMode`).
pub fn apply_overview_refresh(
    tx: &mut W<'_>,
    source: &SourceRow,
    rel_path: &str,
    new_overview: &str,
    actor: &str,
    now: i64,
) -> ApiResult<Option<MemoryRevisionRow>> {
    if source.role != SourceRole::Memory {
        return Ok(None);
    }
    let Some(existing_raw) = read_memory_file_from_disk(source, rel_path)? else {
        return Ok(None);
    };
    let parsed_before = parse_memory_file(rel_path, &existing_raw);
    let before_overview = parsed_before
        .blocks
        .iter()
        .find(|b| b.kind == MemoryKind::Overview)
        .map(|b| b.text.clone())
        .unwrap_or_default();
    let diff = make_diff_preview(&before_overview, new_overview.trim());

    match source.write_mode {
        WriteMode::ReadOnly => {
            metrics::counter!("steve_memory_writes_total", "mode" => "read_only", "result" => "refused")
                .increment(1);
            Ok(None)
        }
        WriteMode::Propose => {
            let rev = MemoryRevisions(tx).insert(&NewMemoryRevision {
                workspace_id: &source.workspace_id,
                source_id: &source.id,
                rel_path,
                entry_id: None,
                op: RevisionOp::OverviewRefresh,
                write_mode: WriteMode::Propose,
                status: RevisionStatus::Proposed,
                heading_path: HEADING_OVERVIEW,
                before_block: &before_overview,
                after_block: new_overview.trim(),
                diff_preview: &diff,
                reason: "event_driven_overview_refresh",
                origin_class: OriginClass::ImplicitPattern.code(),
                actor,
                now,
                applied_at: None,
            })?;
            Ok(Some(rev))
        }
        WriteMode::DirectWrite => {
            let root = folder_root(source)
                .ok_or_else(|| ApiError::Internal("memory source missing path".into()))?;
            let updated_md = render_replace_overview(&existing_raw, new_overview);
            let (size, mtime) = atomic_write_file(&root, rel_path, &updated_md)?;
            if let Some(doc) = Documents(tx).doc_by_ref(&source.id, rel_path)? {
                Documents(tx).set_doc_seen(&doc.id, size, mtime, now)?;
                let doc = Documents(tx).doc(&doc.id)?.unwrap_or(doc);
                reconcile_memory_file(tx, &doc, source, &updated_md, now)?;
            }
            let rev = MemoryRevisions(tx).insert(&NewMemoryRevision {
                workspace_id: &source.workspace_id,
                source_id: &source.id,
                rel_path,
                entry_id: None,
                op: RevisionOp::OverviewRefresh,
                write_mode: WriteMode::DirectWrite,
                status: RevisionStatus::Applied,
                heading_path: HEADING_OVERVIEW,
                before_block: &before_overview,
                after_block: new_overview.trim(),
                diff_preview: &diff,
                reason: "event_driven_overview_refresh",
                origin_class: OriginClass::ImplicitPattern.code(),
                actor,
                now,
                applied_at: Some(now),
            })?;
            metrics::counter!("steve_memory_writes_total", "mode" => "direct_write", "result" => "overview_applied")
                .increment(1);
            Ok(Some(rev))
        }
    }
}

/// 1-click Undo/Revert of an applied silent write revision:
/// removes the block from disk atomically, marks entry `Deleted`, marks revision `Reverted`,
/// records a permanent tombstone in `rejected_patterns`, and increments `steve_memory_reverted_total`.
pub fn revert_revision(
    tx: &mut W<'_>,
    ws: &str,
    revision_id: &str,
    actor: &str,
    now: i64,
) -> ApiResult<MemoryRevisionRow> {
    let rev = MemoryRevisions(tx)
        .get(revision_id)?
        .filter(|r| r.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("revision"))?;
    if rev.status != RevisionStatus::Applied {
        return Err(ApiError::conflict(steve_i18n::t!(
            "knowledge.change.not_undoable"
        )));
    }
    let src = Documents(tx)
        .source(&rev.source_id)?
        .ok_or_else(|| ApiError::not_found(folder::NOUN_SOURCE))?;
    let root =
        folder_root(&src).ok_or_else(|| ApiError::Internal("memory source missing path".into()))?;
    let entry = match rev.entry_id.as_deref() {
        Some(eid) => MemoryEntries(tx).get(eid)?,
        None => None,
    };
    let target_sha = entry
        .as_ref()
        .map(|e| e.content_sha256.clone())
        .unwrap_or_else(|| block_sha256(&rev.after_block));

    if let Some(raw) = read_memory_file_from_disk(&src, &rev.rel_path)? {
        let (updated_md, changed) = render_remove_block(&raw, &target_sha, &rev.after_block);
        if changed {
            let (size, mtime) = atomic_write_file(&root, &rev.rel_path, &updated_md)?;
            if let Some(doc) = Documents(tx).doc_by_ref(&src.id, &rev.rel_path)? {
                Documents(tx).set_doc_seen(&doc.id, size, mtime, now)?;
                let doc_sha = sha(&updated_md);
                Documents(tx).set_doc_indexed(
                    &doc.id,
                    &doc_sha,
                    (doc.chunks - 1).max(0),
                    "active",
                    None,
                    now,
                )?;
            }
        }
    }

    let (subject_type, subject_id, simhash, fp) = if let Some(ref e) = entry {
        MemoryEntries(tx).set_status(&e.id, EntryStatus::Deleted, now)?;
        index::remove(tx, "memory_entry", &e.id)?;
        (
            e.subject_type.clone(),
            e.subject_id.clone(),
            e.simhash,
            e.bigram_fingerprint.clone(),
        )
    } else {
        let bg = bigram_hashes(&rev.after_block);
        (
            "me".to_string(),
            "me".to_string(),
            compute_simhash(&bg),
            encode_bigram_fingerprint(&bg),
        )
    };

    RejectedPatterns(tx).record(&NewRejectedPattern {
        workspace_id: ws,
        subject_type: &subject_type,
        subject_id: &subject_id,
        content_sha256: &target_sha,
        simhash,
        bigram_fingerprint: &fp,
        reason: RejectionReason::OwnerReverted,
        entry_id: rev.entry_id.as_deref(),
        created_by: actor,
        now,
    })?;
    MemoryRevisions(tx).set_status(&rev.id, RevisionStatus::Reverted, None, Some(now))?;
    tx.audit(
        ws,
        actor,
        "memory_revision.revert",
        &rev.id,
        &json!({ "rel_path": rev.rel_path, "entry_id": rev.entry_id }),
    )?;
    metrics::counter!("steve_memory_reverted_total", "reason" => "owner_reverted").increment(1);
    crate::state::emit_change(tx, ws, &rev.id);
    MemoryRevisions(tx)
        .get(&rev.id)?
        .ok_or_else(|| ApiError::not_found("revision"))
}

/// Applies a `Propose`-mode revision after the owner accepts it.
pub fn apply_proposed_revision(
    tx: &mut W<'_>,
    ws: &str,
    revision_id: &str,
    actor: &str,
    now: i64,
) -> ApiResult<MemoryRevisionRow> {
    let rev = MemoryRevisions(tx)
        .get(revision_id)?
        .filter(|r| r.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("revision"))?;
    if rev.status != RevisionStatus::Proposed {
        return Err(ApiError::conflict(steve_i18n::t!(
            "knowledge.proposal.already_handled"
        )));
    }
    let src = Documents(tx)
        .source(&rev.source_id)?
        .ok_or_else(|| ApiError::not_found(folder::NOUN_SOURCE))?;
    let root =
        folder_root(&src).ok_or_else(|| ApiError::Internal("memory source missing path".into()))?;
    let existing_raw = read_memory_file_from_disk(&src, &rev.rel_path)?;
    let (subject_type, subject_id) = infer_subject(&rev.rel_path, &FrontMatter::default());
    let updated_md = if rev.op == RevisionOp::OverviewRefresh {
        render_replace_overview(existing_raw.as_deref().unwrap_or(""), &rev.after_block)
    } else {
        let title = folder::default_title(&rev.rel_path);
        render_append_block(
            existing_raw.as_deref(),
            &title,
            &subject_type,
            &subject_id,
            src.level,
            &src.visibility,
            if rev.heading_path.is_empty() {
                HEADING_FACTS
            } else {
                &rev.heading_path
            },
            &rev.after_block,
        )
    };
    let (size, mtime) = atomic_write_file(&root, &rev.rel_path, &updated_md)?;
    let doc = match Documents(tx).doc_by_ref(&src.id, &rev.rel_path)? {
        Some(d) => {
            Documents(tx).set_doc_seen(&d.id, size, mtime, now)?;
            Documents(tx).doc(&d.id)?.unwrap_or(d)
        }
        None => {
            let doc_id = uid("kd");
            Documents(tx).insert_doc(
                ws,
                &NewDoc {
                    id: doc_id.clone(),
                    source_id: src.id.clone(),
                    r#ref: rev.rel_path.clone(),
                    title: folder::default_title(&rev.rel_path),
                    mime: "text/markdown".to_string(),
                    size,
                    mtime: Some(mtime),
                    created_by: actor.to_string(),
                    ..Default::default()
                },
                now,
            )?;
            Documents(tx)
                .doc(&doc_id)?
                .ok_or_else(|| ApiError::Internal("inserted proposed doc missing".into()))?
        }
    };
    reconcile_memory_file(tx, &doc, &src, &updated_md, now)?;
    MemoryRevisions(tx).set_status(&rev.id, RevisionStatus::Applied, Some(now), None)?;
    tx.audit(
        ws,
        actor,
        "memory_revision.apply_proposed",
        &rev.id,
        &json!({ "rel_path": rev.rel_path }),
    )?;
    crate::state::emit_change(tx, ws, &rev.id);
    MemoryRevisions(tx)
        .get(&rev.id)?
        .ok_or_else(|| ApiError::not_found("revision"))
}

// ---------------------------------------------------------------- Rebuildable FTS Cache

/// Wipes `kb_chunks` / `kb_fts` / `kb_vectors` for `ws` and rebuilds the entire search index
/// solely from source files on disk (`role = knowledge` -> `chunk`, `role = memory` ->
/// `reconcile_memory_file` + `memory_entry`) plus notes/mems/bot_mems.
pub async fn rebuild_fts_index(m: &Modules, kb: &Knowledge, ws: &str) -> ApiResult<usize> {
    let ws_owned = ws.to_string();
    let doc_ids: Vec<String> = m.store.write(|tx| {
        Chunks(tx).clear_workspace(&ws_owned)?;
        let mut ids = Vec::new();
        for src in Documents(tx).sources(&ws_owned)? {
            for doc in Documents(tx).docs_of_source(&src.id)? {
                if doc.status == "active" {
                    // Reset sha256 so `index_document` always re-chunks from disk.
                    Documents(tx).set_doc_indexed(&doc.id, "", 0, "active", None, m.clock.now())?;
                    ids.push(doc.id);
                }
            }
        }
        Ok::<_, StoreError>(ids)
    })?;

    for batch in doc_ids.chunks(index::BATCH) {
        index::run_batch(m, kb, batch).await?;
    }
    let now = m.clock.now();
    m.store.write(|tx| {
        Documents(tx).delete_mark(ws, "sync:mems")?;
        Documents(tx).delete_mark(ws, "sync:bot_mems")?;
        crate::sync::sweep_mems(tx, ws, now)?;
        crate::sync::sync_bot_mems(tx, ws, "steve", now)?;
        Ok::<_, StoreError>(())
    })?;
    Ok(doc_ids.len())
}

/// Checks if any active `memory_entries` row in `ws` is a near-duplicate of `text` for the
/// same `(subject_type, subject_id)`. Returns the matching entry ID if found.
pub fn find_duplicate_active_entry(
    c: &mut impl Read,
    ws: &str,
    subject_type: &str,
    subject_id: &str,
    text: &str,
) -> Result<Option<MemoryEntryRow>> {
    let target_sha = block_sha256(text);
    let bg = bigram_hashes(text);
    let sh = compute_simhash(&bg);
    for entry in MemoryEntries(c).list_by_subject(ws, subject_type, subject_id)? {
        if entry.kind == MemoryKind::Overview {
            continue;
        }
        if matches_fingerprint(
            &target_sha,
            sh,
            &bg,
            &entry.content_sha256,
            entry.simhash,
            &entry.bigram_fingerprint,
            crate::policy::NEAR_DUPLICATE_MIN_SIMILARITY,
        ) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_agnostic_markdown_with_front_matter_and_obsidian_ids() {
        let md = "\
---
subject_type: me
subject_id: me
steve:
  level: reasoning
  visibility: private
---

# Personal Context

## Overview

Senior Rust engineer working on distributed systems.

## Rules & Preferences

- Always write commit messages in English ^eng-commits
- Prefer deterministic tests over probabilistic assertions

## Facts & Context

- Primary workstation runs macOS on Apple Silicon
";
        let parsed = parse_memory_file("me.md", md);
        assert_eq!(parsed.subject_type, "me");
        assert_eq!(parsed.subject_id, "me");
        assert_eq!(parsed.blocks.len(), 4);
        assert_eq!(parsed.blocks[0].kind, MemoryKind::Overview);
        assert_eq!(parsed.blocks[0].block_anchor, "overview");
        assert_eq!(parsed.blocks[1].kind, MemoryKind::Rule);
        assert_eq!(
            parsed.blocks[1].obsidian_id.as_deref(),
            Some("^eng-commits")
        );
        assert_eq!(parsed.blocks[1].block_anchor, "^eng-commits");
        assert_eq!(
            parsed.blocks[1].text,
            "Always write commit messages in English"
        );
        assert_eq!(parsed.blocks[2].kind, MemoryKind::Rule);
        assert_eq!(parsed.blocks[3].kind, MemoryKind::Fact);
    }

    #[test]
    fn bigram_fingerprint_roundtrip_and_similarity() {
        let a = bigram_hashes("Always respond with concise bullet points in code reviews");
        let b = bigram_hashes("Always respond with concise bullet points during code reviews");
        let c = bigram_hashes("Completely unrelated weekend travel itinerary to Kyoto");
        let fp_a = encode_bigram_fingerprint(&a);
        let decoded = decode_bigram_fingerprint(&fp_a);
        assert_eq!(a, decoded);
        assert!(bigram_dice_similarity(&a, &b) >= 0.80);
        assert!(bigram_dice_similarity(&a, &c) < 0.25);
    }
}
