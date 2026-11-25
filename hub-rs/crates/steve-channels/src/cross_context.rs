//! Cross-context assembly for owner-voice replies (#275).
//!
//! Reuses #573's [`steve_knowledge::context::owner_personal_mems`] for personal `me` memories
//! (never duplicating memory loading) and supplements it with bounded topic-relevant snippets from:
//! - other visible IM conversations in the same workspace,
//! - the workspace's main Steve thread (`messages.space = 'steve'`), and
//! - active/open work items matching the conversation's topic.
//!
//! Every item assembled here is tagged as `Level::Reasoning` ("仅用于推理") and returned both as a
//! prompt block and as [`Hit`] entries so the outbound disclosure audit prevents leaking cross-chat
//! details, personal notes, or internal item identifiers into the reply.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use steve_knowledge::Hit;
use steve_knowledge::context::{level_label, owner_personal_mems};
use steve_store::R;
use steve_store::base::{Level, Viewer, Visibility};
use steve_store::channel_message_repo::cross_context as repo;

use crate::ChannelResult;

/// Max distinct other IM spaces sampled for cross-conversation context.
pub const CROSS_CONV_MAX_SPACES: usize = 3;
/// Max total cross-conversation message snippets included.
pub const CROSS_CONV_MAX_SNIPPETS: usize = 6;
/// Max age (ms) of cross-conversation messages considered (14 days).
pub const CROSS_CONV_HORIZON_MS: i64 = 14 * 86_400_000;
/// Max relevant open items included as background reasoning context.
pub const CROSS_ITEMS_MAX: usize = 5;
/// Total character budget for the rendered cross-context section.
pub const CROSS_CONTEXT_MAX_CHARS: usize = 2_400;
/// Max distinct topic query terms extracted from the message and recent conversation.
pub const CROSS_QUERY_TERMS_MAX: usize = 6;
/// Character cap per individual snippet.
const SNIPPET_CHARS: usize = 160;
/// Over-fetch factor before applying per-space capping.
const OVERFETCH_MULTIPLIER: usize = 3;

/// Common ASCII filler tokens excluded from cross-context keyword queries.
const ASCII_STOP_TERMS: [&str; 12] = [
    "the", "and", "for", "with", "that", "this", "from", "hello", "thanks", "please", "about",
    "there",
];

#[derive(Debug, Default)]
pub struct CrossContext {
    /// Rendered anchor blocks (ready to append to composer layer 6).
    pub blocks: Vec<String>,
    /// Anchor/memory ids already injected (for deduplication against layer 7).
    pub ids: Vec<String>,
    /// Background reasoning hits fed into the disclosure audit so private/cross-chat details
    /// cannot leak verbatim into the outbound message.
    pub reasoning_hits: Vec<Hit>,
}

fn cut(s: &str, max_chars: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max_chars) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// Deterministically extracts up to [`CROSS_QUERY_TERMS_MAX`] topic query terms from the target
/// message and recent human conversation lines, omitting raw handles, mentions, and stop words.
pub fn extract_terms(message: &str, recent: &[String]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |raw: &str| {
        if terms.len() >= CROSS_QUERY_TERMS_MAX {
            return;
        }
        let t = raw.trim();
        let lower = t.to_ascii_lowercase();
        if t.chars().count() < 2
            || crate::context::raw_name(t)
            || ASCII_STOP_TERMS.contains(&lower.as_str())
            || seen.contains(&lower)
        {
            return;
        }
        seen.insert(lower);
        terms.push(t.to_string());
    };

    for src in std::iter::once(message).chain(recent.iter().rev().take(4).map(String::as_str)) {
        // Strip @mentions before tokenizing so names of chat participants don't drown topic terms.
        let mut ascii = String::new();
        let mut cjk = String::new();
        let mut skipping_mention = false;
        let flush_ascii = |buf: &mut String, push: &mut dyn FnMut(&str)| {
            if buf.len() >= 3 && !buf.chars().all(|c| c.is_ascii_digit()) {
                push(buf);
            }
            buf.clear();
        };
        let flush_cjk = |buf: &mut String, push: &mut dyn FnMut(&str)| {
            let chars: Vec<char> = buf.chars().collect();
            if (2..=6).contains(&chars.len()) {
                push(buf);
            } else if chars.len() > 6 {
                // Take sliding 4-char windows (stride 2) plus tail so mid-clause topic phrases
                // are preserved without needing language-specific stop-word lists.
                let mut i = 0;
                while i + 4 <= chars.len() {
                    let chunk: String = chars[i..i + 4].iter().collect();
                    push(&chunk);
                    i += 2;
                }
                let tail: String = chars[chars.len() - 4..].iter().collect();
                push(&tail);
            }
            buf.clear();
        };
        for c in src.chars() {
            if c == '@' {
                flush_ascii(&mut ascii, &mut push);
                flush_cjk(&mut cjk, &mut push);
                skipping_mention = true;
                continue;
            }
            if skipping_mention {
                if c.is_whitespace() || "，。！？、,.;:：()（）".contains(c) {
                    skipping_mention = false;
                }
                continue;
            }
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                flush_cjk(&mut cjk, &mut push);
                ascii.push(c);
            } else if is_cjk(c) {
                flush_ascii(&mut ascii, &mut push);
                cjk.push(c);
            } else {
                flush_ascii(&mut ascii, &mut push);
                flush_cjk(&mut cjk, &mut push);
            }
        }
        flush_ascii(&mut ascii, &mut push);
        flush_cjk(&mut cjk, &mut push);
    }
    terms
}

/// Assembles owner-voice cross-context (#275):
/// 1. Owner's `me` personal memories via #573's [`owner_personal_mems`].
/// 2. Relevant recent snippets from other visible IM spaces and the main Steve conversation.
/// 3. Relevant active/open items in the workspace.
pub fn gather(
    r: &mut R<'_>,
    viewer: &Viewer,
    current_space: &str,
    message: &str,
    recent: &[String],
    already_seen_ids: &[String],
    now: i64,
) -> ChannelResult<CrossContext> {
    let t0 = Instant::now();
    let mut out = CrossContext::default();
    let mut used_chars = 0usize;

    // 1. Reuse #573 owner `me` personal memories (zero duplicate memory subsystem).
    let (me_hits, _) = owner_personal_mems(r, viewer, now)
        .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?;
    let mut me_lines = Vec::new();
    for h in me_hits {
        let key = format!("{}:{}", h.obj_type, h.obj_id);
        if already_seen_ids.contains(&key) || out.ids.contains(&key) {
            continue;
        }
        let line = format!(
            "- {}（{}）",
            cut(&h.text, SNIPPET_CHARS),
            level_label(h.level)
        );
        if used_chars + line.chars().count() > CROSS_CONTEXT_MAX_CHARS {
            break;
        }
        used_chars += line.chars().count();
        me_lines.push(line);
        out.ids.push(key);
        out.reasoning_hits.push(h);
    }
    if !me_lines.is_empty() {
        metrics::counter!("steve_channel_cross_context_hits_total", "source" => "me_mem")
            .increment(me_lines.len() as u64);
        out.blocks.push(format!(
            "关于主人本人的记忆（按各自披露级别使用）：\n{}",
            me_lines.join("\n")
        ));
    }

    let terms = extract_terms(message, recent);
    if !terms.is_empty() {
        let since = now.saturating_sub(CROSS_CONV_HORIZON_MS);

        // 2. Other visible IM spaces + main Steve thread (strictly Level::Reasoning).
        let other_rows = repo::other_space_snippets(
            r,
            &viewer.ws,
            &viewer.user,
            current_space,
            &terms,
            since,
            CROSS_CONV_MAX_SNIPPETS * OVERFETCH_MULTIPLIER,
        )?;
        let main_rows =
            repo::main_thread_snippets(r, &viewer.ws, &terms, since, CROSS_CONV_MAX_SNIPPETS)?;

        let mut spaces_used: HashSet<String> = HashSet::new();
        let mut per_space_count: HashMap<String, usize> = HashMap::new();
        let mut other_lines = Vec::new();
        let mut other_hits = 0u64;
        let mut main_hits = 0u64;

        let mut combined: Vec<repo::OtherConversationSnippet> = Vec::new();
        combined.extend(other_rows);
        combined.extend(main_rows);
        combined.sort_by(|a, b| b.at.cmp(&a.at));

        for row in combined {
            if other_lines.len() >= CROSS_CONV_MAX_SNIPPETS {
                break;
            }
            if row.source == "other_chat" {
                if !spaces_used.contains(&row.space_id)
                    && spaces_used.len() >= CROSS_CONV_MAX_SPACES
                {
                    continue;
                }
                let count = per_space_count.entry(row.space_id.clone()).or_insert(0);
                if *count >= 2 {
                    continue;
                }
                *count += 1;
                spaces_used.insert(row.space_id.clone());
            }
            let who = if row.sender_name.is_empty() {
                "某人"
            } else {
                row.sender_name.as_str()
            };
            let snippet = cut(&row.text, SNIPPET_CHARS);
            let line = format!(
                "- [来自其他会话「{}」] {}：{}（仅用于推理）",
                cut(&row.space_name, 30),
                who,
                snippet
            );
            if used_chars + line.chars().count() > CROSS_CONTEXT_MAX_CHARS {
                break;
            }
            used_chars += line.chars().count();
            if row.source == "main_thread" {
                main_hits += 1;
            } else {
                other_hits += 1;
            }
            other_lines.push(line);
            out.reasoning_hits.push(Hit {
                n: 0,
                obj_type: "cross_conv".into(),
                obj_id: format!("{}:{}", row.space_id, row.at),
                seq: 0,
                title: row.space_name,
                heading: String::new(),
                text: snippet,
                label: "其他会话".into(),
                source_kind: row.source,
                level: Level::Reasoning,
                visibility: Visibility::Private,
                score: 0.0,
                href: String::new(),
                about: Vec::new(),
            });
        }
        if other_hits > 0 {
            metrics::counter!("steve_channel_cross_context_hits_total", "source" => "other_chat")
                .increment(other_hits);
        }
        if main_hits > 0 {
            metrics::counter!("steve_channel_cross_context_hits_total", "source" => "main_thread")
                .increment(main_hits);
        }
        if !other_lines.is_empty() {
            out.blocks.push(format!(
                "其他会话里的相关上下文（仅用于推理，不要透露来源群名、人名或未经允许的细节）：\n{}",
                other_lines.join("\n")
            ));
        }

        // 3. Active/open work items matching the topic (strictly Level::Reasoning on Mode::Own:
        // helps the owner answer accurately without exposing Steve's internal item numbers).
        let items = repo::relevant_open_items(r, &viewer.ws, &terms, CROSS_ITEMS_MAX)?;
        let mut item_lines = Vec::new();
        for item in items {
            let mut text = format!("{}（状态：{}）", cut(&item.title, 80), item.status);
            if !item.current.trim().is_empty() {
                text.push_str(&format!("，当前进展：{}", cut(&item.current, 100)));
            }
            let line = format!("- {text}（仅用于推理）");
            if used_chars + line.chars().count() > CROSS_CONTEXT_MAX_CHARS {
                break;
            }
            used_chars += line.chars().count();
            item_lines.push(line);
            out.reasoning_hits.push(Hit {
                n: 0,
                obj_type: "item".into(),
                obj_id: item.id,
                seq: 0,
                title: item.title,
                heading: String::new(),
                text,
                label: "进行中的事项".into(),
                source_kind: "item".into(),
                level: Level::Reasoning,
                visibility: Visibility::Workspace,
                score: 0.0,
                href: String::new(),
                about: Vec::new(),
            });
        }
        if !item_lines.is_empty() {
            metrics::counter!("steve_channel_cross_context_hits_total", "source" => "item")
                .increment(item_lines.len() as u64);
            out.blocks.push(format!(
                "主人正在推进的相关事项（仅用于推理，不要提及 Steve 或内部事项编号）：\n{}",
                item_lines.join("\n")
            ));
        }
    }

    metrics::histogram!("steve_channel_cross_context_seconds").record(t0.elapsed().as_secs_f64());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_cjk_and_ascii_terms_without_mentions_or_stop_words() {
        let terms = extract_terms("@张三 hello，请问发版计划和 deploy-v2 进度如何？", &[]);
        assert!(terms.iter().any(|t| t.contains("发版计划")), "{terms:?}");
        assert!(terms.iter().any(|t| t == "deploy-v2"), "{terms:?}");
        assert!(!terms.iter().any(|t| t.contains("张三")), "{terms:?}");
        assert!(!terms.iter().any(|t| t == "hello"), "{terms:?}");
    }
}
