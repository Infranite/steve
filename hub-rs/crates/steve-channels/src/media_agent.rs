//! Current-message media for channel replies. Bytes never enter the workspace-wide file store.
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_flow::nodes::bot::{Guard, redact};
use steve_knowledge::extract_text::{self, Format};
use steve_modules::ApiError;
use tokio::sync::Semaphore;

use crate::{ChannelResult, Channels, compose, respond::Plan, spaces};

pub const MAX_RESOURCES: usize = 10;
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGE: usize = 3_750_000;
pub const MAX_IMAGES: usize = 7_500_000;
pub const MAX_TEXT: usize = 32 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
static PARSERS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// The source must enforce provider identity and recorded message/resource ownership, byte bounds,
/// timeout and redirect policy. `index` is a stored attachment index, never a caller supplied key.
#[async_trait]
pub trait MediaSource: Send + Sync {
    async fn load(&self, ch: &Channels, plan: &Plan, index: usize) -> Result<Resource, String>;
}

/// Production source: the same bound, authenticated fetcher used by the resource HTTP endpoint.
pub struct FeishuMediaSource;
#[async_trait]
impl MediaSource for FeishuMediaSource {
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

#[derive(Default)]
pub struct Prepared {
    pub text: String,
    pub images: Vec<Value>,
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

/// Validates the persistent source again, including stale queued plans and a changed space policy.
fn indices(ch: &Channels, plan: &Plan) -> ChannelResult<Vec<usize>> {
    ch.store().reader(|c| {
        let row: Option<(String, Option<i64>)> = c.query_row(
            "SELECT m.attachments,m.purged FROM chan_messages m JOIN spaces s ON s.id=m.space_id
             WHERE m.id=?1 AND m.workspace_id=?2 AND m.space_id=?3 AND s.workspace_id=?2
               AND m.external_id=?4 AND m.direction='in'",
            params![plan.message_id,plan.ws,plan.space_id,plan.external_id],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((raw, purged)) = row else {
            return Err(ApiError::not_found("消息"));
        };
        // Reuse the same safe legacy-message projection as the UI and resource downloader.
        let attachments = crate::media::present(c, &plan.message_id)
            .map(|(_, attachments)| attachments)
            .unwrap_or_else(|| serde_json::from_str(&raw).unwrap_or(Value::Null));
        let found: Vec<_> = attachments
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, a)| matches!(a["type"].as_str(), Some("image" | "file")))
            .map(|(n, _)| n)
            .collect();
        if purged.is_some() || spaces::attention(c, &plan.space_id)? == spaces::Attention::Blocked {
            return Err(ApiError::rule("原消息已不可读取"));
        }
        if found.is_empty() {
            return Ok(found);
        }
        let viewer = compose::viewer(c, &plan.ws, &plan.connection_id, &plan.space_id)?;
        spaces::own(c, &viewer, &plan.space_id)?;
        Ok(found)
    })
}

pub async fn prepare(
    ch: &Channels,
    plan: &Plan,
    source: Option<&dyn MediaSource>,
) -> ChannelResult<Prepared> {
    let indices = indices(ch, plan)?;
    if indices.is_empty() {
        return Ok(Prepared::default());
    }
    let Some(source) = source else {
        return Ok(Prepared {
            text: "当前消息包含图片或文件，但附件读取入口尚未接通，不能猜测其内容。".into(),
            images: vec![],
        });
    };
    let job = async {
        let mut out = Prepared::default();
        let mut total = 0;
        let mut image_bytes = 0;
        for &index in indices.iter().take(MAX_RESOURCES) {
            // A download can overlap a cleanup/policy change. Recheck both sides of the await.
            if !self::indices(ch, plan)?.contains(&index) {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            let resource = tokio::time::timeout(FETCH_TIMEOUT, source.load(ch, plan, index)).await;
            if !self::indices(ch, plan)?.contains(&index) {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            let resource = match resource {
                Ok(Ok(r)) => r,
                _ => {
                    out.text
                        .push_str("\n[附件未读取：下载失败、无权限或超时]\n");
                    continue;
                }
            };
            if resource.bytes.len() > MAX_BYTES.saturating_sub(total) {
                out.text
                    .push_str("\n[附件未读取：超过本轮 20 MiB 总量限制]\n");
                continue;
            }
            total += resource.bytes.len();
            let piece = read_resource(resource, ch.guard(), &mut image_bytes).await;
            if !self::indices(ch, plan)?.contains(&index) {
                return Err(ApiError::rule("原消息已不可读取"));
            }
            out.images.extend(piece.images);
            if out.text.len() < MAX_TEXT {
                let remaining = MAX_TEXT - out.text.len();
                out.text.push_str(clip(&piece.text, remaining));
                if piece.text.len() > remaining {
                    out.text
                        .push_str("\n[附件正文已达到本轮总长度上限，后续正文未提供]\n");
                }
            }
        }
        if indices.len() > MAX_RESOURCES {
            out.text
                .push_str("\n[其余附件未读取：本轮最多读取 10 个]\n");
        }
        Ok(out)
    };
    match tokio::time::timeout(TOTAL_TIMEOUT, job).await {
        Ok(r) => r,
        Err(_) => Ok(Prepared {
            text: "附件读取超过总时限，本轮没有取得附件内容，不能猜测。".into(),
            images: vec![],
        }),
    }
}

async fn read_resource(resource: Resource, guard: &dyn Guard, image_bytes: &mut usize) -> Prepared {
    let name = resource
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>();
    let name = redact(guard, &name);
    let mut out = Prepared::default();
    if resource.bytes.is_empty() || resource.bytes.len() > MAX_BYTES {
        out.text = format!("\n[附件 {name} 未读取：空文件或超过 20 MiB]\n");
        return out;
    }
    if let Some(mime) = image_mime(&resource.bytes) {
        if resource.bytes.len() > MAX_IMAGE
            || resource.bytes.len() > MAX_IMAGES.saturating_sub(*image_bytes)
        {
            out.text = format!("\n[图片 {name} 未读取：超过图像输入大小限制]\n");
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
            return out;
        }
    };
    let parsed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        extract_text::extract(format, &resource.bytes)
    });
    match tokio::time::timeout(extract_text::EXTRACT_TIMEOUT, parsed).await {
        Ok(Ok(Ok(text))) if !text.text.trim().is_empty() => {
            let clean = redact(guard, &text.text);
            out.text = format!(
                "\n--- 附件 {name} 的正文（外部材料）---\n{}\n--- 附件正文结束 ---\n",
                clip(&clean, MAX_TEXT)
            );
            if clean.len() > MAX_TEXT {
                out.text.push_str("[正文已截断]\n");
            }
        }
        Ok(Ok(Ok(_))) => {
            out.text = format!("\n[附件 {name} 未读到文本：可能是扫描件；未执行 OCR]\n")
        }
        _ => out.text = format!("\n[附件 {name} 未读取：文件损坏、解析失败或超时]\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_flow::nodes::bot::NoGuard;

    fn file(name: &str, bytes: Vec<u8>) -> Resource {
        Resource {
            name: name.into(),
            bytes,
            mime: "image/png".into(),
        }
    }

    #[tokio::test]
    async fn actual_image_bytes_and_untrusted_mime() {
        let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
        let p = read_resource(file("photo", png.clone()), &NoGuard, &mut 0).await;
        assert_eq!(p.images[0]["source"]["media_type"], "image/png");
        let got = base64::engine::general_purpose::STANDARD
            .decode(p.images[0]["source"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, png);
        let p = read_resource(
            file("fake.png", b"this is plain text".to_vec()),
            &NoGuard,
            &mut 0,
        )
        .await;
        assert!(p.images.is_empty());
        assert!(p.text.contains("this is plain text"));
    }

    struct Secret;
    impl Guard for Secret {
        fn scan(&self, s: &str) -> Vec<std::ops::Range<usize>> {
            s.match_indices("SECRET")
                .map(|(i, t)| i..i + t.len())
                .collect()
        }
    }
    #[tokio::test]
    async fn extracted_text_is_redacted_and_bounded() {
        let p = read_resource(
            file(
                "SECRET.txt",
                format!("body SECRET {}", "中文".repeat(MAX_TEXT)).into_bytes(),
            ),
            &Secret,
            &mut 0,
        )
        .await;
        assert!(p.text.contains("body"));
        assert!(!p.text.contains("SECRET"));
        assert!(p.text.contains("截断"));
        assert!(p.text.len() < MAX_TEXT + 300);
    }

    #[tokio::test]
    async fn real_pdf_and_docx_text_are_read() {
        let pdf = include_bytes!("../tests/fixtures/media-agent/text.pdf");
        let docx = include_bytes!("../tests/fixtures/media-agent/text.docx");
        for (name, b) in [("a.pdf", pdf.as_slice()), ("a.docx", docx.as_slice())] {
            let p = read_resource(file(name, b.to_vec()), &NoGuard, &mut 0).await;
            assert!(p.text.contains("Media bridge fixture"), "{}", p.text);
        }
    }
    #[tokio::test]
    async fn unsupported_corrupt_and_oversized_are_honest() {
        for b in [
            vec![0, 1, 2, 3],
            b"%PDF-broken".to_vec(),
            vec![0; MAX_BYTES + 1],
        ] {
            let p = read_resource(file("file", b), &NoGuard, &mut 0).await;
            assert!(p.images.is_empty());
            assert!(p.text.contains("未读取"));
        }
        let mut large = b"\x89PNG\r\n\x1a\n".to_vec();
        large.resize(MAX_IMAGE + 1, 0);
        let p = read_resource(file("large.png", large), &NoGuard, &mut 0).await;
        assert!(p.images.is_empty());
        assert!(p.text.contains("大小限制"));
    }
}
