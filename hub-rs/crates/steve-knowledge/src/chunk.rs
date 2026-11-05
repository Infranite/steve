//! Splitting normalized document text into retrieval chunks.
//!
//! Markdown is first cut into sections at headings (found with pulldown-cmark, so `#` inside
//! code blocks is not a heading), then each section is split with text-splitter into 600–1000
//! character pieces overlapping by up to 100 characters. Chunking is deterministic: the same
//! normalized text always yields the same chunks, so `(doc id, seq)` is a stable chunk identity
//! across re-indexing, and `start..end` (character offsets into the normalized text) lets a
//! citation jump to and highlight the exact passage.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use text_splitter::{ChunkCharIndex, ChunkConfig, MarkdownSplitter, TextSplitter};

/// Desired lower bound of a chunk, in characters.
pub const MIN_CHARS: usize = 600;
/// Upper bound of a chunk, in characters.
pub const MAX_CHARS: usize = 1000;
/// Maximum overlap between consecutive chunks of one section, in characters.
pub const OVERLAP: usize = 100;
/// Chunks kept per document; the rest is dropped and the doc gets [`TRUNCATED_ERROR`].
pub const MAX_CHUNKS: usize = 2000;
/// `kb_docs.error` for a document cut at [`MAX_CHUNKS`].
pub const TRUNCATED_ERROR: &str = "太长，只索引了前 2000 段";
/// Separator of the heading path: “标题 > 小节”.
pub const HEADING_SEP: &str = " > ";

/// How to interpret a document's normalized text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Markdown,
    Plain,
}

/// One retrieval unit; maps onto a `kb_chunks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// 0-based position in the document (`kb_chunks.seq`).
    pub seq: usize,
    /// Heading path of the enclosing section, “标题 > 小节”; empty before the first heading.
    pub heading: String,
    /// Chunk text, exactly `normalized[start..end]` (in characters).
    pub text: String,
    /// Character offset of the first character in the normalized text (`kb_chunks.start_at`).
    pub start: usize,
    /// Character offset one past the last character (`kb_chunks.end_at`).
    pub end: usize,
}

impl Chunk {
    /// What goes into the `seg` column of `kb_fts` (before [`crate::seg::seg`]): the heading
    /// path is indexed with the text so a section title helps find its body, but it is shown
    /// separately.
    pub fn index_text(&self) -> String {
        if self.heading.is_empty() {
            self.text.clone()
        } else {
            format!("{}\n{}", self.heading, self.text)
        }
    }
}

/// Result of chunking one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chunked {
    pub chunks: Vec<Chunk>,
    /// More than [`MAX_CHUNKS`] chunks were produced and the tail was dropped.
    pub truncated: bool,
}

fn config() -> ChunkConfig<text_splitter::Characters> {
    ChunkConfig::new(MIN_CHARS..MAX_CHARS)
        .with_overlap(OVERLAP)
        .expect("overlap is smaller than the chunk capacity")
}

/// Byte offset → character offset, for any char boundary in `text`.
struct CharIndex {
    starts: Vec<usize>,
}

impl CharIndex {
    fn new(text: &str) -> Self {
        let mut starts: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        starts.push(text.len());
        Self { starts }
    }
    fn chars(&self, byte: usize) -> usize {
        self.starts.partition_point(|&b| b < byte)
    }
}

/// A heading-delimited part of a Markdown document, as byte ranges into the text.
struct Section {
    heading: String,
    body_start: usize,
    body_end: usize,
}

fn sections(text: &str) -> Vec<Section> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    // (heading start, heading end, level, title)
    let mut found: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut current: Option<(usize, usize, String)> = None;
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((range.start, level as usize, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((start, level, title)) = current.take() {
                    found.push((start, range.end, level, title.trim().to_string()));
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, title)) = current.as_mut() {
                    title.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, title)) = current.as_mut() {
                    title.push(' ');
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(found.len() + 1);
    let first = found.first().map_or(text.len(), |h| h.0);
    out.push(Section {
        heading: String::new(),
        body_start: 0,
        body_end: first,
    });
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (i, (_, end, level, title)) in found.iter().enumerate() {
        while stack.last().is_some_and(|(l, _)| *l >= *level) {
            stack.pop();
        }
        stack.push((*level, title.clone()));
        let path: Vec<&str> = stack
            .iter()
            .map(|(_, t)| t.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        out.push(Section {
            heading: path.join(HEADING_SEP),
            body_start: *end,
            body_end: found.get(i + 1).map_or(text.len(), |h| h.0),
        });
    }
    out
}

fn collect<'t>(
    out: &mut Chunked,
    idx: &CharIndex,
    base_byte: usize,
    heading: &str,
    pieces: impl Iterator<Item = ChunkCharIndex<'t>>,
) {
    for piece in pieces {
        if piece.chunk.trim().is_empty() {
            continue;
        }
        if out.chunks.len() == MAX_CHUNKS {
            out.truncated = true;
            return;
        }
        let start = idx.chars(base_byte + piece.byte_offset);
        let end = start + piece.chunk.chars().count();
        out.chunks.push(Chunk {
            seq: out.chunks.len(),
            heading: heading.to_string(),
            text: piece.chunk.to_string(),
            start,
            end,
        });
    }
}

/// Chunks Markdown by section, then by size.
pub fn chunk_markdown(text: &str) -> Chunked {
    let idx = CharIndex::new(text);
    let splitter = MarkdownSplitter::new(config());
    let mut out = Chunked::default();
    for s in sections(text) {
        if out.truncated {
            break;
        }
        let body = &text[s.body_start..s.body_end];
        collect(
            &mut out,
            &idx,
            s.body_start,
            &s.heading,
            splitter.chunk_char_indices(body),
        );
    }
    out
}

/// Chunks plain text by size only (paragraphs and sentences are preferred split points).
pub fn chunk_plain(text: &str) -> Chunked {
    let idx = CharIndex::new(text);
    let splitter = TextSplitter::new(config());
    let mut out = Chunked::default();
    collect(&mut out, &idx, 0, "", splitter.chunk_char_indices(text));
    out
}

/// Chunks normalized text according to its kind.
pub fn chunk(text: &str, kind: TextKind) -> Chunked {
    match kind {
        TextKind::Markdown => chunk_markdown(text),
        TextKind::Plain => chunk_plain(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(text: &str, start: usize, end: usize) -> String {
        text.chars().skip(start).take(end - start).collect()
    }

    /// Distinct, numbered sentences so overlaps and offsets are unambiguous.
    fn para(tag: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("{tag}第{i}句话讲部署和续签的细节。"))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn heading_paths_and_offsets() {
        let doc = format!(
            "前言一句。\n\n# Acme 合同说明\n\n概要。\n\n## 续签\n\n{}\n\n```\n# 不是标题\n```\n\n### 价格 `v2`\n\n价格条款。\n\n## 联系人\n\nSarah Chen 负责。\n\n# 附录\n\n附录内容。\n",
            para("续", 120)
        );
        let out = chunk_markdown(&doc);
        assert!(!out.truncated);
        for c in &out.chunks {
            assert_eq!(
                slice(&doc, c.start, c.end),
                c.text,
                "offsets of seq {}",
                c.seq
            );
            assert!(c.text.chars().count() <= MAX_CHARS);
        }
        let heads: Vec<&str> = out.chunks.iter().map(|c| c.heading.as_str()).collect();
        assert_eq!(heads.first(), Some(&""));
        assert!(heads.contains(&"Acme 合同说明"));
        assert!(heads.contains(&"Acme 合同说明 > 续签"));
        assert!(heads.contains(&"Acme 合同说明 > 续签 > 价格 v2"));
        assert!(heads.contains(&"Acme 合同说明 > 联系人"));
        assert_eq!(heads.last(), Some(&"附录"));
        // The fenced `# 不是标题` stays inside the 续签 section.
        assert!(
            out.chunks
                .iter()
                .any(|c| c.heading == "Acme 合同说明 > 续签" && c.text.contains("# 不是标题"))
        );
        // Heading lines themselves are not part of chunk text.
        assert!(!out.chunks.iter().any(|c| c.text.contains("## 续签")));
        // seq is dense and ordered.
        for (i, c) in out.chunks.iter().enumerate() {
            assert_eq!(c.seq, i);
        }
    }

    #[test]
    fn long_section_splits_with_overlap() {
        let doc = format!("# 续签\n\n{}", para("续", 200));
        let out = chunk_markdown(&doc);
        let cs = &out.chunks;
        assert!(cs.len() >= 3, "got {} chunks", cs.len());
        for pair in cs.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(a.text.chars().count() >= MIN_CHARS - OVERLAP);
            assert!(
                b.start < a.end,
                "chunks {} and {} must overlap",
                a.seq,
                b.seq
            );
            let overlap = a.end - b.start;
            assert!(
                overlap <= OVERLAP,
                "overlap {overlap} between {} and {}",
                a.seq,
                b.seq
            );
            // The overlapping text is the same on both sides.
            assert_eq!(slice(&doc, b.start, a.end), slice(&b.text, 0, overlap));
        }
        // Average overlap is close to the target (sentences are ~20 chars).
        let total: usize = cs.windows(2).map(|p| p[0].end - p[1].start).sum();
        assert!(total / (cs.len() - 1) >= OVERLAP - 25);
    }

    #[test]
    fn plain_text_and_determinism() {
        let doc = (0..30)
            .map(|i| para(&format!("段{i}"), 3))
            .collect::<Vec<_>>()
            .join("\n\n");
        let a = chunk_plain(&doc);
        let b = chunk(&doc, TextKind::Plain);
        assert_eq!(a, b);
        assert!(a.chunks.len() > 1);
        for c in &a.chunks {
            assert_eq!(c.heading, "");
            assert_eq!(slice(&doc, c.start, c.end), c.text);
        }
    }

    #[test]
    fn empty_and_heading_only_docs() {
        assert!(chunk_markdown("").chunks.is_empty());
        assert!(
            chunk_markdown("# 只有标题\n\n## 空小节\n")
                .chunks
                .is_empty()
        );
        let short = chunk_markdown("# T\n\n一句话。");
        assert_eq!(short.chunks.len(), 1);
        assert_eq!(short.chunks[0].heading, "T");
        assert_eq!(short.chunks[0].text, "一句话。");
        assert_eq!(short.chunks[0].index_text(), "T\n一句话。");
    }

    #[test]
    fn caps_at_max_chunks() {
        let doc = (0..MAX_CHUNKS + 5)
            .map(|i| format!("## h{i}\n\nx{i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let out = chunk_markdown(&doc);
        assert!(out.truncated);
        assert_eq!(out.chunks.len(), MAX_CHUNKS);
    }
}
