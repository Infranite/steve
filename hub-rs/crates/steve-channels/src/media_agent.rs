//! Current-message and burst media for channel replies (#278).
//! Bytes never enter the workspace-wide file store.
//!
//! Improvements for #278:
//! - Prepares media across every message in the current burst (`plan.burst_message_ids`) plus an
//!   unread quoted parent message (`reply_to`) when referenced.
//! - Reuses cached image descriptions (`context::DESCRIBED`) instead of re-downloading.
//! - Reports structured [`MediaIssue`] values so unsupported formats, unavailable sources,
//!   oversized files, or timeouts are communicated explicitly instead of silently skipped.
//! - Emits `steve_channel_media_prepare_total` and `steve_channel_media_prepare_seconds`.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};
use steve_knowledge::extract_text::{self, Format};
use steve_modules::ApiError;
use tokio::sync::Semaphore;

use crate::{ChannelResult, Channels, compose, respond::Plan, spaces};

pub const MAX_RESOURCES: usize = 10;
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGE: usize = 3_750_000;
pub const MAX_IMAGES: usize = 7_500_000;
pub const MAX_TEXT: usize = 32 * 1024;
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
static PARSERS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// The source must enforce provider identity and recorded message/resource ownership, byte bounds,
/// timeout and redirect policy. `index` is a stored attachment index, never a caller supplied key.
#[async_trait]
pub trait MediaSource: Send + Sync {
    async fn load(&self, ch: &Channels, plan: &Plan, index: usize) -> Result<Resource, String>;
}

/// Production source: the same bound, authenticated fetcher used by the resource HTTP endpoint.
pub struct ChannelMediaSource;
#[async_trait]
impl MediaSource for ChannelMediaSource {
    async fn load(&self, ch: &Channels, plan: &Plan, index: usize) -> Result<Resource, String> {
        let fetched = crate::media::fetch_bound(
            ch,
            &plan.ws,
            &plan.space_id,
            &plan.message_id,
            index,
            crate::media::Limits {
                max_bytes: MAX_BYTES as u64,
                timeout: FETCH_TIMEOUT,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(Resource {
            bytes: fetched.bytes,
            name: fetched.name,
            mime: fetched.mime.into(),
        })
    }
}

pub struct Resource {
    pub bytes: Vec<u8>,
    pub name: String,
    /// Claimed MIME is not trusted: content is sniffed again before entering a model.
    pub mime: String,
}

/// Typed classification of why an attachment could not be read (#278).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaIssueKind {
    UnsupportedFormat,
    UnsupportedModel,
    FetchFailed,
    Oversized,
    EmptyOrScanned,
    Truncated,
    SourceUnavailable,
    TimedOut,
}

impl MediaIssueKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "unsupported_format",
            Self::UnsupportedModel => "unsupported_model",
            Self::FetchFailed => "fetch_failed",
            Self::Oversized => "oversized",
            Self::EmptyOrScanned => "empty_or_scanned",
            Self::Truncated => "truncated",
            Self::SourceUnavailable => "source_unavailable",
            Self::TimedOut => "timed_out",
        }
    }
}

/// One structured failure/limitation encountered while preparing media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaIssue {
    pub message_id: String,
    pub name: String,
    pub kind: MediaIssueKind,
    pub detail: String,
}

#[derive(Default, Clone)]
pub struct Prepared {
    pub text: String,
    pub images: Vec<Value>,
    pub issues: Vec<MediaIssue>,
    /// Message IDs that contributed at least one image (used to persist `described` back).
    pub source_message_ids: Vec<String>,
    /// Whether any message in scope carried an image or file attachment.
    pub has_attachments: bool,
    /// Whether readable text was extracted from at least one file attachment.
    pub extracted_file_text: bool,
    /// Number of images satisfied from cached `DESCRIBED` notes without re-downloading.
    pub cached_descriptions: usize,
}

impl Prepared {
    /// Whether every attachment in scope failed or was unsupported (no images, no extracted
    /// readable file text, no cached descriptions).
    pub fn all_unreadable(&self) -> bool {
        self.has_attachments
            && self.images.is_empty()
            && self.cached_descriptions == 0
            && !self.issues.is_empty()
            && !self.extracted_file_text
    }

    /// Concise human-readable summary of all issues for explicit owner reporting (#278).
    pub fn issue_summary(&self) -> String {
        let mut seen = HashSet::new();
        let parts: Vec<&str> = self
            .issues
            .iter()
            .map(|i| i.detail.as_str())
            .filter(|d| !d.is_empty() && seen.insert(*d))
            .take(3)
            .collect();
        if parts.is_empty() {
            "消息包含图片或文件，但未能读取内容，需要你亲自查看。".into()
        } else {
            parts.join("；")
        }
    }

    pub fn has_unsupported_issue(&self) -> bool {
        self.issues.iter().any(|i| {
            matches!(
                i.kind,
                MediaIssueKind::UnsupportedFormat
                    | MediaIssueKind::UnsupportedModel
                    | MediaIssueKind::SourceUnavailable
            )
        })
    }
}

fn clip(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn image_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

struct AttachmentTarget {
    message_id: String,
    external_id: String,
    index: usize,
    name: String,
    cached_described: Option<String>,
}

/// Validates a persistent message source and lists its readable image/file attachments.
fn message_targets(
    db: &mut impl steve_store::Read,
    plan: &Plan,
    message_id: &str,
    expected_external_id: Option<&str>,
    strict: bool,
) -> ChannelResult<Vec<AttachmentTarget>> {
    let row = steve_store::channel_message_repo::media::get(db, message_id)?;
    let Some(row) = row.filter(|r| {
        r.ws == plan.ws
            && r.space_id == plan.space_id
            && r.direction == "in"
            && expected_external_id.is_none_or(|ext| r.external_id.as_deref() == Some(ext))
    }) else {
        return if strict {
            Err(ApiError::not_found("消息"))
        } else {
            Ok(Vec::new())
        };
    };
    let chat = steve_store::channel_spaces::platform_chat(db, &row.space_id)?;
    if !chat.is_some_and(|chat| chat.ws == plan.ws) {
        return if strict {
            Err(ApiError::not_found("消息"))
        } else {
            Ok(Vec::new())
        };
    }
    if row.purged.is_some() || spaces::attention(db, &plan.space_id)? == spaces::Attention::Blocked
    {
        return if strict {
            Err(ApiError::rule("原消息已不可读取"))
        } else {
            Ok(Vec::new())
        };
    }
    let (_, attachments) = crate::media::present_source(db, &row);
    let mut found = Vec::new();
    for (n, a) in attachments.as_array().into_iter().flatten().enumerate() {
        let Some(ty) = a["type"].as_str() else {
            continue;
        };
        if !matches!(ty, "image" | "file") {
            continue;
        }
        // Stickers are wordless non-document attachments.
        if ty == "file" && a["kind"].as_str() == Some("sticker") {
            continue;
        }
        let cached_described = (ty == "image")
            .then(|| {
                a[crate::context::DESCRIBED]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
            .flatten();
        let name = a["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(if ty == "image" { "图片" } else { "文件" })
            .to_string();
        found.push(AttachmentTarget {
            message_id: row.id.clone(),
            external_id: row.external_id.clone().unwrap_or_default(),
            index: n,
            name,
            cached_described,
        });
    }
    if !found.is_empty() {
        let viewer = compose::viewer(db, &plan.ws, &plan.connection_id)?;
        spaces::checked_space(db, &viewer, &plan.space_id)?;
    }
    Ok(found)
}

fn collect_targets(ch: &Channels, plan: &Plan) -> ChannelResult<Vec<AttachmentTarget>> {
    ch.store().read(|db| {
        let mut message_ids: Vec<(String, Option<String>, bool)> = Vec::new();
        let mut seen = HashSet::new();
        // 1. Primary message first (strict validation).
        seen.insert(plan.message_id.clone());
        message_ids.push((
            plan.message_id.clone(),
            Some(plan.external_id.clone()),
            true,
        ));
        // 2. Earlier messages in the same burst (#268 / #278).
        for mid in &plan.burst_message_ids {
            if seen.insert(mid.clone()) {
                message_ids.push((mid.clone(), None, false));
            }
        }
        // 3. Quoted parent message (`reply_to`) if it carries attachments (#258 / #278).
        if let Some(row) = steve_store::channel_message_repo::projection::get(db, &plan.message_id)?
            && let Some(parent_ext) = row.reply_to.as_deref().filter(|s| !s.is_empty())
            && let Some(parent) = steve_store::channel_message_repo::dialogue::by_external(
                db,
                &plan.space_id,
                parent_ext,
            )?
            && seen.insert(parent.id.clone())
        {
            message_ids.push((parent.id, None, false));
        }

        let mut targets = Vec::new();
        for (mid, ext, strict) in message_ids {
            targets.extend(message_targets(db, plan, &mid, ext.as_deref(), strict)?);
        }
        Ok(targets)
    })
}

fn still_readable(ch: &Channels, sub_plan: &Plan, index: usize) -> ChannelResult<bool> {
    ch.store().read(|db| {
        let targets = message_targets(
            db,
            sub_plan,
            &sub_plan.message_id,
            Some(&sub_plan.external_id),
            true,
        )?;
        Ok(targets.iter().any(|t| t.index == index))
    })
}

pub async fn prepare(
    ch: &Channels,
    plan: &Plan,
    source: Option<&dyn MediaSource>,
) -> ChannelResult<Prepared> {
    let t0 = Instant::now();
    let targets = collect_targets(ch, plan)?;
    if targets.is_empty() {
        return Ok(Prepared::default());
    }

    let mut out = Prepared {
        has_attachments: true,
        ..Prepared::default()
    };
    let mut to_fetch = Vec::new();
    for target in targets {
        if let Some(desc) = target.cached_described {
            out.cached_descriptions += 1;
            out.text
                .push_str(&format!("\n[图片（之前读过）：{desc}]\n"));
        } else {
            to_fetch.push(target);
        }
    }
    if to_fetch.is_empty() {
        metrics::counter!("steve_channel_media_prepare_total", "outcome" => "cached").increment(1);
        metrics::histogram!("steve_channel_media_prepare_seconds")
            .record(t0.elapsed().as_secs_f64());
        return Ok(out);
    }

    let Some(source) = source else {
        let detail = "当前消息包含图片或文件，但附件读取入口尚未接通，不能猜测其内容。";
        out.text.push_str(detail);
        out.issues.push(MediaIssue {
            message_id: plan.message_id.clone(),
            name: to_fetch[0].name.clone(),
            kind: MediaIssueKind::SourceUnavailable,
            detail: detail.into(),
        });
        metrics::counter!("steve_channel_media_prepare_total", "outcome" => "unsupported")
            .increment(1);
        metrics::histogram!("steve_channel_media_prepare_seconds")
            .record(t0.elapsed().as_secs_f64());
        return Ok(out);
    };

    let job = async {
        let mut total = 0usize;
        let mut image_bytes = 0usize;
        for target in to_fetch.iter().take(MAX_RESOURCES) {
            let mut sub_plan = plan.clone();
            sub_plan.message_id = target.message_id.clone();
            sub_plan.external_id = target.external_id.clone();
            // A download can overlap a cleanup/policy change. Recheck both sides of the await.
            if !still_readable(ch, &sub_plan, target.index)? {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            let resource =
                tokio::time::timeout(FETCH_TIMEOUT, source.load(ch, &sub_plan, target.index)).await;
            if !still_readable(ch, &sub_plan, target.index)? {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            let resource = match resource {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    let detail = format!("附件「{}」下载失败或无权限：{e}", target.name);
                    out.text
                        .push_str("\n[附件未读取：下载失败、无权限或超时]\n");
                    out.issues.push(MediaIssue {
                        message_id: target.message_id.clone(),
                        name: target.name.clone(),
                        kind: MediaIssueKind::FetchFailed,
                        detail,
                    });
                    continue;
                }
                Err(_) => {
                    let detail = format!("附件「{}」下载超时", target.name);
                    out.text
                        .push_str("\n[附件未读取：下载失败、无权限或超时]\n");
                    out.issues.push(MediaIssue {
                        message_id: target.message_id.clone(),
                        name: target.name.clone(),
                        kind: MediaIssueKind::TimedOut,
                        detail,
                    });
                    continue;
                }
            };
            if resource.bytes.len() > MAX_BYTES.saturating_sub(total) {
                let detail = format!("附件「{}」超过本轮 20 MiB 总量限制", target.name);
                out.text
                    .push_str("\n[附件未读取：超过本轮 20 MiB 总量限制]\n");
                out.issues.push(MediaIssue {
                    message_id: target.message_id.clone(),
                    name: target.name.clone(),
                    kind: MediaIssueKind::Oversized,
                    detail,
                });
                continue;
            }
            total += resource.bytes.len();
            let mut piece = read_resource(resource, &mut image_bytes).await;
            if !still_readable(ch, &sub_plan, target.index)? {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            if !piece.images.is_empty() && !out.source_message_ids.contains(&target.message_id) {
                out.source_message_ids.push(target.message_id.clone());
            }
            for issue in &mut piece.issues {
                issue.message_id = target.message_id.clone();
            }
            out.issues.extend(piece.issues);
            out.images.extend(piece.images);
            out.extracted_file_text |= piece.extracted_file_text;
            if out.text.len() < MAX_TEXT {
                let remaining = MAX_TEXT - out.text.len();
                out.text.push_str(clip(&piece.text, remaining));
                if piece.text.len() > remaining {
                    out.text
                        .push_str("\n[附件正文已达到本轮总长度上限，后续正文未提供]\n");
                }
            }
        }
        if to_fetch.len() > MAX_RESOURCES {
            out.text
                .push_str("\n[其余附件未读取：本轮最多读取 10 个]\n");
        }
        Ok(out)
    };
    let result = match tokio::time::timeout(TOTAL_TIMEOUT, job).await {
        Ok(r) => r,
        Err(_) => {
            let detail = "附件读取超过总时限，本轮没有取得附件内容，不能猜测。";
            Ok(Prepared {
                text: detail.into(),
                images: vec![],
                issues: vec![MediaIssue {
                    message_id: plan.message_id.clone(),
                    name: String::new(),
                    kind: MediaIssueKind::TimedOut,
                    detail: detail.into(),
                }],
                source_message_ids: vec![],
                has_attachments: true,
                extracted_file_text: false,
                cached_descriptions: 0,
            })
        }
    };
    let outcome = match &result {
        Ok(p) if p.issues.is_empty() => "ok",
        Ok(p) if p.issues.iter().any(|i| i.kind == MediaIssueKind::TimedOut) => "timeout",
        Ok(p) if p.has_unsupported_issue() => "unsupported",
        Ok(_) | Err(_) => "failed",
    };
    metrics::counter!("steve_channel_media_prepare_total", "outcome" => outcome).increment(1);
    metrics::histogram!("steve_channel_media_prepare_seconds").record(t0.elapsed().as_secs_f64());
    result
}

pub(crate) async fn read_resource(resource: Resource, image_bytes: &mut usize) -> Prepared {
    let name = resource
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>();
    let mut out = Prepared {
        has_attachments: true,
        ..Prepared::default()
    };
    if resource.bytes.is_empty() || resource.bytes.len() > MAX_BYTES {
        out.text = format!("\n[附件 {name} 未读取：空文件或超过 20 MiB]\n");
        out.issues.push(MediaIssue {
            message_id: String::new(),
            name: name.clone(),
            kind: MediaIssueKind::Oversized,
            detail: format!("附件「{name}」为空或超过 20 MiB 限制"),
        });
        return out;
    }
    if let Some(mime) = image_mime(&resource.bytes) {
        if resource.bytes.len() > MAX_IMAGE
            || resource.bytes.len() > MAX_IMAGES.saturating_sub(*image_bytes)
        {
            out.text = format!("\n[图片 {name} 未读取：超过图像输入大小限制]\n");
            out.issues.push(MediaIssue {
                message_id: String::new(),
                name: name.clone(),
                kind: MediaIssueKind::Oversized,
                detail: format!("图片「{name}」超过图像输入大小限制"),
            });
            return out;
        }
        *image_bytes += resource.bytes.len();
        out.images.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":base64::engine::general_purpose::STANDARD.encode(&resource.bytes)}}));
        out.text = format!("\n[图片 {name} 已附在当前模型输入中，请直接读取图片]\n");
        return out;
    }
    let mime = if resource.bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if !resource.bytes.contains(&0) && std::str::from_utf8(&resource.bytes).is_ok() {
        "text/plain"
    } else {
        "application/octet-stream"
    };
    let format = extract_text::detect(mime, &resource.bytes);
    if format == Format::Unsupported {
        out.text = format!("\n[附件 {name} 未读取：暂不支持这种格式的内容解析]\n");
        out.issues.push(MediaIssue {
            message_id: String::new(),
            name: name.clone(),
            kind: MediaIssueKind::UnsupportedFormat,
            detail: format!("附件「{name}」的格式暂不支持解析"),
        });
        return out;
    }
    // Keep the permit inside the parser task: timing out cannot free a slot while parsing continues.
    let permit = match tokio::time::timeout(
        Duration::from_secs(5),
        PARSERS
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .acquire_owned(),
    )
    .await
    {
        Ok(Ok(p)) => p,
        _ => {
            out.text = format!("\n[附件 {name} 未读取：解析繁忙]\n");
            out.issues.push(MediaIssue {
                message_id: String::new(),
                name: name.clone(),
                kind: MediaIssueKind::TimedOut,
                detail: format!("附件「{name}」解析繁忙"),
            });
            return out;
        }
    };
    let parsed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        extract_text::extract(format, &resource.bytes)
    });
    match tokio::time::timeout(extract_text::EXTRACT_TIMEOUT, parsed).await {
        Ok(Ok(Ok(text))) if !text.text.trim().is_empty() => {
            out.extracted_file_text = true;
            out.text = format!(
                "\n--- 附件 {name} 的正文（外部材料）---\n{}\n--- 附件正文结束 ---\n",
                clip(&text.text, MAX_TEXT)
            );
            if text.text.len() > MAX_TEXT {
                out.text.push_str("[正文已截断]\n");
                out.issues.push(MediaIssue {
                    message_id: String::new(),
                    name: name.clone(),
                    kind: MediaIssueKind::Truncated,
                    detail: format!("附件「{name}」正文已截断"),
                });
            }
        }
        Ok(Ok(Ok(_))) => {
            out.text = format!("\n[附件 {name} 未读到文本：可能是扫描件；未执行 OCR]\n");
            out.issues.push(MediaIssue {
                message_id: String::new(),
                name: name.clone(),
                kind: MediaIssueKind::EmptyOrScanned,
                detail: format!("附件「{name}」未提取到正文（可能是扫描件或纯图片 PDF）"),
            });
        }
        _ => {
            out.text = format!("\n[附件 {name} 未读取：文件损坏、解析失败或超时]\n");
            out.issues.push(MediaIssue {
                message_id: String::new(),
                name: name.clone(),
                kind: MediaIssueKind::FetchFailed,
                detail: format!("附件「{name}」损坏、解析失败或超时"),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, bytes: Vec<u8>) -> Resource {
        Resource {
            name: name.into(),
            bytes,
            mime: "image/png".into(),
        }
    }

    #[tokio::test]
    async fn actual_image_bytes_and_untrusted_mime() {
        let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
        let p = read_resource(file("photo", png.clone()), &mut 0).await;
        assert_eq!(p.images[0]["source"]["media_type"], "image/png");
        let got = base64::engine::general_purpose::STANDARD
            .decode(p.images[0]["source"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, png);
        assert!(p.issues.is_empty());
        let p = read_resource(file("fake.png", b"this is plain text".to_vec()), &mut 0).await;
        assert!(p.images.is_empty());
        assert!(p.text.contains("this is plain text"));
    }

    #[tokio::test]
    async fn extracted_text_is_bounded() {
        let p = read_resource(
            file(
                "SECRET.txt",
                format!("body SECRET {}", "中文".repeat(MAX_TEXT)).into_bytes(),
            ),
            &mut 0,
        )
        .await;
        assert!(p.text.contains("body SECRET"));
        assert!(p.text.contains("截断"));
        assert!(p.text.len() < MAX_TEXT + 300);
        assert!(p.issues.iter().any(|i| i.kind == MediaIssueKind::Truncated));
    }

    #[tokio::test]
    async fn real_pdf_and_docx_text_are_read() {
        let pdf = include_bytes!("../tests/fixtures/media-agent/text.pdf");
        let docx = include_bytes!("../tests/fixtures/media-agent/text.docx");
        for (name, b) in [("a.pdf", pdf.as_slice()), ("a.docx", docx.as_slice())] {
            let p = read_resource(file(name, b.to_vec()), &mut 0).await;
            assert!(p.text.contains("Media bridge fixture"), "{}", p.text);
        }
    }

    #[tokio::test]
    async fn unsupported_corrupt_and_oversized_are_honest() {
        for (b, expected) in [
            (vec![0, 1, 2, 3], MediaIssueKind::UnsupportedFormat),
            (b"%PDF-broken".to_vec(), MediaIssueKind::FetchFailed),
            (vec![0; MAX_BYTES + 1], MediaIssueKind::Oversized),
        ] {
            let p = read_resource(file("file", b), &mut 0).await;
            assert!(p.images.is_empty());
            assert!(p.text.contains("未读取"));
            assert!(p.issues.iter().any(|i| i.kind == expected));
        }
        let mut large = b"\x89PNG\r\n\x1a\n".to_vec();
        large.resize(MAX_IMAGE + 1, 0);
        let p = read_resource(file("large.png", large), &mut 0).await;
        assert!(p.images.is_empty());
        assert!(p.text.contains("大小限制"));
        assert!(p.issues.iter().any(|i| i.kind == MediaIssueKind::Oversized));
    }
}
