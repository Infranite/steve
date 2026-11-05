//! Text extraction and normalization.
//!
//! Every format ends as normalized UTF-8 text plus a [`TextKind`] telling the chunker whether
//! to look for Markdown headings. Parsers of untrusted binary formats (PDF, docx) run under
//! `catch_unwind`; [`extract_isolated`] additionally moves them off the async runtime and gives
//! up after 30 s. The secret scan (`steve_vault::guard::scan`) runs on the result at index
//! time and is not part of this module.

use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use quick_xml::events::Event as XmlEvent;
use unicode_normalization::UnicodeNormalization;

use crate::chunk::TextKind;

/// Upper bound of extracted text, in bytes (cut at a character boundary).
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound of `word/document.xml` we are willing to inflate (zip-bomb guard).
const MAX_DOCX_XML_BYTES: u64 = 64 * 1024 * 1024;
/// Time limit of [`extract_isolated`].
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
/// How many isolated extractions run at once across the Hub.
const EXTRACT_PARALLEL: usize = 3;
/// How many timed-out extraction threads (still running, unkillable) we tolerate before refusing
/// new extractions until some of them end.
const MAX_ABANDONED: usize = 16;
/// Disclosure levels a front matter may set (`steve.level`).
pub const LEVELS: [&str; 4] = ["public", "conclusion", "reasoning", "never"];

/// Formats with full-text extraction; everything else is [`Format::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Plain,
    Html,
    Pdf,
    Docx,
    Unsupported,
}

/// Why a document has no text. `Display` is the Chinese `kb_docs.error` shown to the user.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractError {
    /// Images, spreadsheets, plain zips: indexed by title only (`status=skipped`).
    #[error("这种文件暂不支持全文检索")]
    Unsupported,
    #[error("文件无法解析：{0}")]
    Corrupt(String),
    #[error("解析超时（30 秒）")]
    Timeout,
    #[error("解析文件时出错")]
    Panicked,
}

/// Fields a Markdown front matter may set; they override the document's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub tags: Vec<String>,
    /// `steve.level`, only if it is one of [`LEVELS`].
    pub level: Option<String>,
    /// `steve.visibility`, raw (validated by the caller against the workspace).
    pub visibility: Option<String>,
}

/// Normalized text of one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub text: String,
    pub kind: TextKind,
    pub front: FrontMatter,
}

/// Picks the extractor from the sniffed mime type; docx is recognised by content
/// (a zip with `word/document.xml`) because uploads often arrive as `application/zip` or
/// `application/octet-stream`.
pub fn detect(mime: &str, bytes: &[u8]) -> Format {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "text/markdown" | "text/x-markdown" => Format::Markdown,
        "text/plain" => Format::Plain,
        "text/html" | "application/xhtml+xml" => Format::Html,
        "application/pdf" => Format::Pdf,
        _ if is_docx(bytes) => Format::Docx,
        _ => Format::Unsupported,
    }
}

fn is_docx(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK")
        && zip::ZipArchive::new(Cursor::new(bytes))
            .map(|z| z.index_for_name("word/document.xml").is_some())
            .unwrap_or(false)
}

/// Extracts and normalizes synchronously. Parser panics become [`ExtractError::Panicked`].
pub fn extract(format: Format, bytes: &[u8]) -> Result<Extracted, ExtractError> {
    let plain = |text: String| Extracted {
        text: normalize(&text),
        kind: TextKind::Plain,
        front: FrontMatter::default(),
    };
    match format {
        Format::Markdown => Ok(markdown(&String::from_utf8_lossy(bytes))),
        Format::Plain => Ok(plain(String::from_utf8_lossy(bytes).into_owned())),
        Format::Html => Ok(Extracted {
            kind: TextKind::Markdown,
            ..plain(guarded(|| html(bytes))?)
        }),
        Format::Pdf => Ok(plain(guarded(|| pdf(bytes))?)),
        Format::Docx => Ok(plain(guarded(|| docx(bytes))?)),
        Format::Unsupported => Err(ExtractError::Unsupported),
    }
}

/// [`extract`] on the blocking pool with a 30 s limit, for PDFs and other heavy files.
/// On timeout the parser thread is abandoned (it cannot be killed) but the caller moves on.
pub async fn extract_isolated(format: Format, bytes: Vec<u8>) -> Result<Extracted, ExtractError> {
    extract_isolated_with(format, bytes, EXTRACT_TIMEOUT).await
}

async fn extract_isolated_with(
    format: Format,
    bytes: Vec<u8>,
    limit: Duration,
) -> Result<Extracted, ExtractError> {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(EXTRACT_PARALLEL);
    static ABANDONED: AtomicUsize = AtomicUsize::new(0);
    run_isolated(&SLOTS, &ABANDONED, limit, move || extract(format, &bytes))
        .await
        .inspect_err(|e| {
            if *e == ExtractError::Timeout {
                tracing::warn!(?format, "text extraction timed out");
            }
        })
}

/// Runs `job` on the blocking pool, at most `slots` at a time, for at most `limit`.
///
/// The permit stays with the caller, not the thread: on timeout it is released and the next
/// extraction runs, so stuck parsers cannot take every slot for good (and the wait for a slot is
/// bounded, since every holder lets go within `limit`). A timed-out thread cannot be killed; it
/// is counted in `abandoned` until it ends, and once [`MAX_ABANDONED`] are stuck, new jobs fail
/// at once with [`ExtractError::Timeout`] instead of piling more threads onto the pool.
async fn run_isolated<F>(
    slots: &tokio::sync::Semaphore,
    abandoned: &'static AtomicUsize,
    limit: Duration,
    job: F,
) -> Result<Extracted, ExtractError>
where
    F: FnOnce() -> Result<Extracted, ExtractError> + Send + 'static,
{
    if abandoned.load(Ordering::Acquire) >= MAX_ABANDONED {
        tracing::warn!("too many stuck text extractions; refusing a new one");
        return Err(ExtractError::Timeout);
    }
    let _permit = slots.acquire().await.map_err(|_| ExtractError::Panicked)?;
    // Set by whichever side finishes first: the thread when it ends, the caller when it gives up.
    let done = Arc::new(AtomicBool::new(false));
    let on_thread = done.clone();
    let task = tokio::task::spawn_blocking(move || {
        // A guard, so the count also drops if the job unwinds.
        struct Ended(Arc<AtomicBool>, &'static AtomicUsize);
        impl Drop for Ended {
            fn drop(&mut self) {
                if self.0.swap(true, Ordering::AcqRel) {
                    self.1.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
        let _ended = Ended(on_thread, abandoned);
        job()
    });
    match tokio::time::timeout(limit, task).await {
        Err(_) => {
            // Count first, then mark: if the thread already ended, take the count back.
            abandoned.fetch_add(1, Ordering::AcqRel);
            if done.swap(true, Ordering::AcqRel) {
                abandoned.fetch_sub(1, Ordering::AcqRel);
            }
            Err(ExtractError::Timeout)
        }
        Ok(Err(_join)) => Err(ExtractError::Panicked),
        Ok(Ok(r)) => r,
    }
}

fn guarded(f: impl FnOnce() -> Result<String, ExtractError>) -> Result<String, ExtractError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        tracing::warn!("text extractor panicked");
        Err(ExtractError::Panicked)
    })
}

fn html(bytes: &[u8]) -> Result<String, ExtractError> {
    // html2text only renders text: scripts, styles and attributes never reach the output.
    html2text::config::plain_no_decorate()
        .link_footnotes(false)
        .string_from_read(bytes, 100_000)
        .map_err(|e| ExtractError::Corrupt(e.to_string()))
}

fn pdf(bytes: &[u8]) -> Result<String, ExtractError> {
    pdf_extract::extract_text_from_mem(bytes).map_err(|e| ExtractError::Corrupt(e.to_string()))
}

fn docx(bytes: &[u8]) -> Result<String, ExtractError> {
    let corrupt = |e: &dyn std::fmt::Display| ExtractError::Corrupt(e.to_string());
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| corrupt(&e))?;
    let entry = zip.by_name("word/document.xml").map_err(|e| corrupt(&e))?;
    let mut xml = Vec::new();
    entry
        .take(MAX_DOCX_XML_BYTES)
        .read_to_end(&mut xml)
        .map_err(|e| corrupt(&e))?;

    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    let mut out = String::new();
    let mut in_text = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| corrupt(&e))? {
            XmlEvent::Start(e) if e.name().as_ref() == "w:t" => in_text = true,
            XmlEvent::End(e) => match e.name().as_ref() {
                "w:t" => in_text = false,
                "w:p" => out.push('\n'),
                _ => {}
            },
            XmlEvent::Empty(e) => match e.name().as_ref() {
                "w:tab" => out.push('\t'),
                "w:br" | "w:cr" => out.push('\n'),
                "w:p" => out.push('\n'),
                _ => {}
            },
            XmlEvent::Text(t) if in_text => {
                out.push_str(&t.xml10_content());
            }
            XmlEvent::GeneralRef(r) if in_text => {
                match quick_xml::escape::resolve_predefined_entity(r.as_ref()) {
                    Some(s) => out.push_str(s),
                    None => {
                        if let Ok(Some(c)) = r.resolve_char_ref() {
                            out.push(c);
                        }
                    }
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
        buf.clear();
    }
    Ok(out)
}

/// Markdown: front matter out (its fields returned), wiki links resolved, then normalized.
pub fn markdown(src: &str) -> Extracted {
    let (front, body) = split_front_matter(src);
    Extracted {
        text: normalize(&wiki_links(body)),
        kind: TextKind::Markdown,
        front,
    }
}

/// `[[页|别名]]` → 别名, `[[页]]` → 页, `![[嵌入]]` removed.
fn wiki_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[[") {
        let embed = rest[..i].ends_with('!');
        let after = &rest[i + 2..];
        let Some(j) = after.find("]]").filter(|&j| !after[..j].contains('\n')) else {
            out.push_str(&rest[..i + 2]);
            rest = after;
            continue;
        };
        let inner = &after[..j];
        if embed {
            out.push_str(&rest[..i - 1]);
        } else {
            out.push_str(&rest[..i]);
            out.push_str(inner.rsplit('|').next().unwrap_or(inner).trim());
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

/// Splits a leading YAML front matter (`---` … `---`/`...`) from the body.
///
/// Only the fields Steve uses are read, with a small line parser instead of a YAML library:
/// `title`, `tags` (`[a, b]`, `- a` list, or a single value), `steve.level` /
/// `steve.visibility` (nested under `steve:` or dotted). Anything else is ignored.
pub fn split_front_matter(src: &str) -> (FrontMatter, &str) {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let Some(after) = src
        .strip_prefix("---\n")
        .or_else(|| src.strip_prefix("---\r\n"))
    else {
        return (FrontMatter::default(), src);
    };
    let mut end = None;
    let mut pos = 0;
    for line in after.split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            end = Some((pos, pos + line.len()));
            break;
        }
        pos += line.len();
    }
    let Some((yaml_end, body_start)) = end else {
        return (FrontMatter::default(), src);
    };
    (parse_front(&after[..yaml_end]), &after[body_start..])
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .unwrap_or(v);
    v.trim().to_string()
}

fn parse_front(yaml: &str) -> FrontMatter {
    let mut fm = FrontMatter::default();
    // Which block key the indented lines belong to.
    let mut block = "";
    for line in yaml.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let t = line.trim();
        if indented {
            match block {
                "tags" => {
                    if let Some(v) = t.strip_prefix("- ").or_else(|| t.strip_prefix('-')) {
                        push_tag(&mut fm, v);
                    }
                }
                "steve" => {
                    if let Some((k, v)) = t.split_once(':') {
                        set_steve(&mut fm, k.trim(), v);
                    }
                }
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = t.split_once(':') else {
            block = "";
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        block = "";
        match key {
            "title" if !value.is_empty() => fm.title = Some(unquote(value)),
            "tags" => {
                if let Some(list) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                    list.split(',').for_each(|v| push_tag(&mut fm, v));
                } else if value.is_empty() {
                    block = "tags";
                } else {
                    push_tag(&mut fm, value);
                }
            }
            "steve" if value.is_empty() => block = "steve",
            _ => {
                if let Some(sub) = key.strip_prefix("steve.") {
                    set_steve(&mut fm, sub, value);
                }
            }
        }
    }
    fm
}

fn push_tag(fm: &mut FrontMatter, v: &str) {
    let tag = unquote(v);
    if !tag.is_empty() && !fm.tags.contains(&tag) {
        fm.tags.push(tag);
    }
}

fn set_steve(fm: &mut FrontMatter, key: &str, v: &str) {
    let v = unquote(v);
    match key {
        "level" if LEVELS.contains(&v.as_str()) => fm.level = Some(v),
        "visibility" if !v.is_empty() => fm.visibility = Some(v),
        _ => {}
    }
}

/// Zero-width characters and bidi controls: invisible, and used to hide or reorder text.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE … RLO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // LRI … PDI
        | '\u{061C}'              // Arabic letter mark
        | '\u{FEFF}'              // BOM / ZWNBSP
        | '\u{00AD}'              // soft hyphen
    )
}

/// NFC, invisible characters removed, CRLF → LF, runs of spaces inside a line collapsed to
/// one, trailing spaces dropped, more than one blank line collapsed to one, ≤ 2 MB.
///
/// Leading indentation is kept: it carries Markdown structure (nested lists, code blocks).
pub fn normalize(text: &str) -> String {
    let nfc: String = text
        .nfc()
        .filter(|c| !is_invisible(*c))
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = String::with_capacity(nfc.len());
    let mut blank_run = 0usize;
    for line in nfc.split('\n') {
        let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
        let (indent, rest) = line.split_at(indent_len);
        let mut collapsed = String::with_capacity(rest.len());
        let mut in_space = false;
        for c in rest.chars() {
            if c.is_whitespace() {
                in_space = true;
            } else {
                if in_space && !collapsed.is_empty() {
                    collapsed.push(' ');
                }
                in_space = false;
                collapsed.push(c);
            }
        }
        if collapsed.is_empty() {
            blank_run += 1;
            if blank_run == 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;
        out.push_str(indent);
        out.push_str(&collapsed);
        out.push('\n');
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    if out.len() > MAX_TEXT_BYTES {
        let mut cut = MAX_TEXT_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn docx_bytes(document_xml: &str) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            z.start_file("[Content_Types].xml", opts).unwrap();
            z.write_all(b"<Types/>").unwrap();
            z.start_file("word/document.xml", opts).unwrap();
            z.write_all(document_xml.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn front_matter_overrides() {
        let src = "---\ntitle: \"Acme 合同说明\"\ntags: [客户, 合同]\nsteve:\n  level: never\n  visibility: private\nother: x\n---\n# 正文\n\n内容\n";
        let e = markdown(src);
        assert_eq!(e.front.title.as_deref(), Some("Acme 合同说明"));
        assert_eq!(e.front.tags, vec!["客户", "合同"]);
        assert_eq!(e.front.level.as_deref(), Some("never"));
        assert_eq!(e.front.visibility.as_deref(), Some("private"));
        assert_eq!(e.text, "# 正文\n\n内容");
        assert_eq!(e.kind, TextKind::Markdown);

        let dotted = "---\ntags:\n  - a\n  - 'b'\nsteve.level: reasoning\n---\nbody";
        let (fm, body) = split_front_matter(dotted);
        assert_eq!(fm.tags, vec!["a", "b"]);
        assert_eq!(fm.level.as_deref(), Some("reasoning"));
        assert_eq!(body, "body");

        // Unknown level ignored; unterminated front matter is just text.
        assert_eq!(
            split_front_matter("---\nsteve.level: secret\n---\n")
                .0
                .level,
            None
        );
        let (fm, body) = split_front_matter("---\ntitle: x\nno end");
        assert_eq!(fm, FrontMatter::default());
        assert!(body.starts_with("---"));
    }

    #[test]
    fn wiki_link_rewrite() {
        assert_eq!(
            wiki_links(
                "见 [[30_项目/Steve终态领域设计|Steve 终态领域设计]] 和 [[部署]]。![[图.png]]结束"
            ),
            "见 Steve 终态领域设计 和 部署。结束"
        );
        assert_eq!(wiki_links("[[未闭合\n]] [x]"), "[[未闭合\n]] [x]");
    }

    #[test]
    fn normalization() {
        // NFC: e + combining acute → é.
        assert_eq!(normalize("cafe\u{301}"), "café");
        assert_eq!(normalize("部\u{200B}署\u{202E}脚本\u{FEFF}"), "部署脚本");
        assert_eq!(
            normalize("a  b\t\tc   \r\n\r\n\r\n\r\n  - 缩进\u{3000}\u{3000}保留\n"),
            "a b c\n\n  - 缩进 保留"
        );
        let big = "字".repeat(MAX_TEXT_BYTES);
        let n = normalize(&big);
        assert!(n.len() <= MAX_TEXT_BYTES);
    }

    #[test]
    fn detect_formats() {
        assert_eq!(
            detect("text/markdown; charset=utf-8", b""),
            Format::Markdown
        );
        assert_eq!(detect("text/plain", b""), Format::Plain);
        assert_eq!(detect("TEXT/HTML", b""), Format::Html);
        assert_eq!(detect("application/pdf", b""), Format::Pdf);
        assert_eq!(
            detect("application/octet-stream", &docx_bytes("<w:document/>")),
            Format::Docx
        );
        assert_eq!(detect("image/png", b"\x89PNG"), Format::Unsupported);
        assert_eq!(
            extract(Format::Unsupported, b"x"),
            Err(ExtractError::Unsupported)
        );
        assert_eq!(
            ExtractError::Unsupported.to_string(),
            "这种文件暂不支持全文检索"
        );
    }

    #[test]
    fn html_is_stripped() {
        let page = r#"<html><head><title>T</title><style>body{color:red}</style>
            <script>alert('x')</script></head><body><h1>部署说明</h1>
            <p onclick="evil()">先跑 <b>测试</b>，再<a href="javascript:x()">发布</a>。</p>
            <ul><li>第一步</li><li>第二步</li></ul></body></html>"#;
        let e = extract(Format::Html, page.as_bytes()).unwrap();
        assert!(e.text.contains("部署说明"), "{}", e.text);
        assert!(e.text.contains("先跑 测试"), "{}", e.text);
        assert!(e.text.contains("第二步"));
        for bad in ["alert", "color:red", "onclick", "javascript", "<"] {
            assert!(!e.text.contains(bad), "{bad} leaked: {}", e.text);
        }
    }

    #[test]
    fn docx_paragraph_order() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>第一段：</w:t></w:r><w:r><w:t xml:space="preserve"> Acme &amp; 合同</w:t></w:r></w:p>
<w:p><w:r><w:t>第二段</w:t><w:tab/><w:t>带制表</w:t></w:r></w:p>
<w:p/>
<w:p><w:r><w:t>第三段</w:t><w:br/><w:t>换行后</w:t></w:r></w:p>
</w:body></w:document>"#;
        let e = extract(Format::Docx, &docx_bytes(xml)).unwrap();
        assert_eq!(
            e.text,
            "第一段： Acme & 合同\n第二段 带制表\n\n第三段\n换行后"
        );
        assert_eq!(e.kind, TextKind::Plain);
    }

    #[test]
    fn broken_docx_is_an_error() {
        assert!(matches!(
            extract(Format::Docx, b"PK\x03\x04garbage"),
            Err(ExtractError::Corrupt(_))
        ));
        let no_doc = {
            let mut buf = Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("a.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.finish().unwrap();
            buf.into_inner()
        };
        assert!(matches!(
            extract(Format::Docx, &no_doc),
            Err(ExtractError::Corrupt(_))
        ));
        let bad_xml = docx_bytes("<w:document><w:p><w:t>x</w:p></w:document>");
        assert!(matches!(
            extract(Format::Docx, &bad_xml),
            Err(ExtractError::Corrupt(_))
        ));
    }

    #[test]
    fn broken_pdf_is_an_error_not_a_panic() {
        for bytes in [
            &b""[..],
            b"not a pdf",
            b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF",
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\nxref\n0 1\n0000000000 65535 f \ntrailer<</Size 1>>\nstartxref\n9\n%%EOF",
        ] {
            let r = extract(Format::Pdf, bytes);
            assert!(r.is_err(), "{:?}", r);
        }
    }

    #[test]
    fn minimal_pdf_text() {
        let pdf = crate::testdata::minimal_pdf("Steve knowledge base");
        let e = extract(Format::Pdf, &pdf).unwrap();
        assert!(e.text.contains("Steve knowledge base"), "{:?}", e.text);
    }

    #[tokio::test]
    async fn isolated_extraction() {
        let e = extract_isolated(Format::Plain, "一行\u{200B}文本".as_bytes().to_vec())
            .await
            .unwrap();
        assert_eq!(e.text, "一行文本");
        let r = extract_isolated(Format::Pdf, b"%PDF-garbage".to_vec()).await;
        assert!(r.is_err());
        let r = extract_isolated_with(
            Format::Pdf,
            crate::testdata::minimal_pdf("x"),
            Duration::ZERO,
        )
        .await;
        assert!(matches!(r, Err(ExtractError::Timeout) | Ok(_)));
    }

    fn plain(text: &str) -> Extracted {
        Extracted {
            text: text.into(),
            kind: TextKind::Plain,
            front: FrontMatter::default(),
        }
    }

    /// Stuck parsers holding every slot must not block later extractions (regression).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stuck_extractions_free_their_slots() {
        static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(EXTRACT_PARALLEL);
        static ABANDONED: AtomicUsize = AtomicUsize::new(0);
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Arc::new(std::sync::Mutex::new(gate));
        let limit = Duration::from_millis(50);

        for _ in 0..EXTRACT_PARALLEL {
            let gate = gate.clone();
            let r = run_isolated(&SLOTS, &ABANDONED, limit, move || {
                let _ = gate.lock().unwrap().recv();
                Ok(plain("late"))
            })
            .await;
            assert_eq!(r, Err(ExtractError::Timeout));
        }
        assert_eq!(ABANDONED.load(Ordering::Acquire), EXTRACT_PARALLEL);

        // All "real" slots are held by stuck threads; a new extraction still runs.
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            run_isolated(&SLOTS, &ABANDONED, limit, || Ok(plain("ok"))),
        )
        .await
        .expect("extraction waited for a stuck thread's slot");
        assert_eq!(r.unwrap().text, "ok");

        // Stuck threads that finally end are no longer counted.
        drop(release);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ABANDONED.load(Ordering::Acquire) != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "abandoned count never dropped"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Past the cap on stuck threads, new extractions fail at once instead of adding threads.
    #[tokio::test]
    async fn too_many_stuck_extractions_refuse_new_ones() {
        static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(EXTRACT_PARALLEL);
        static ABANDONED: AtomicUsize = AtomicUsize::new(MAX_ABANDONED);
        let r = run_isolated(&SLOTS, &ABANDONED, Duration::from_secs(5), || {
            panic!("must not run")
        })
        .await;
        assert_eq!(r, Err(ExtractError::Timeout));
        ABANDONED.store(MAX_ABANDONED - 1, Ordering::Release);
        let r = run_isolated(&SLOTS, &ABANDONED, Duration::from_secs(5), || {
            Ok(plain("ok"))
        })
        .await;
        assert_eq!(r.unwrap().text, "ok");
    }
}
