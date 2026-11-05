//! Handing attachments to agents (files.ts `forAgent`): files are copied into `<cwd>/.steve/attachments/`
//! (ignored by git) and named in the prompt; images that fit also go inline when the adapter takes them.
//! Upload validation and storage belong to the API; this is the part the domain layer and the runner share.

use crate::error::Result;
use crate::repo::{Db as Store, FileRef, FileRow};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Image types a model takes inline; everything else only goes by path.
pub const INLINE_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// The Messages API takes base64 images up to 5 MB; larger ones are read from disk by the agent.
pub const MAX_INLINE: i64 = 3_750_000;

/// `data/files/<workspace>/<sha256[..2]>/<id>`
pub fn blob_path(data_dir: &Path, f: &FileRow) -> PathBuf {
    data_dir
        .join("files")
        .join(&f.workspace_id)
        .join(f.sha256.get(..2).unwrap_or(""))
        .join(&f.id)
}

fn kb(n: i64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{} KB", ((n as f64 / 1024.0).round() as i64).max(1))
    }
}

/// The note appended to a message, and the inline image blocks (`{type:"image", data, mimeType}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForAgent {
    pub note: String,
    pub images: Vec<Value>,
    /// the copies that are not inline images, which only the Read tool reaches
    pub readable: Vec<crate::gateway::Readable>,
}

/// Copy the attachments into `<cwd>/.steve/attachments` and say where they are.
pub fn for_agent(
    db: &Store,
    data_dir: &Path,
    cwd: &Path,
    files: &[FileRef],
    inline_images: bool,
) -> Result<ForAgent> {
    if files.is_empty() {
        return Ok(ForAgent::default());
    }
    let dir = cwd.join(".steve").join("attachments");
    std::fs::create_dir_all(&dir)?;
    let ign = dir.join(".gitignore");
    if !ign.exists() {
        std::fs::write(&ign, "*\n")?;
    }
    let mut lines = vec![];
    let mut images = vec![];
    let mut readable = vec![];
    for r in files {
        let Some(f) = db.file(&r.id)? else { continue };
        let rel = format!(
            ".steve/attachments/{}-{}",
            f.id.get(4..).unwrap_or(&f.id),
            f.name
        );
        let dst = cwd.join(&rel);
        if !dst.exists() {
            std::fs::copy(blob_path(data_dir, &f), &dst)?;
        }
        let inline =
            inline_images && INLINE_IMAGES.contains(&f.mime.as_str()) && f.size <= MAX_INLINE;
        if inline {
            let data = base64::engine::general_purpose::STANDARD
                .encode(std::fs::read(blob_path(data_dir, &f))?);
            images.push(json!({"type": "image", "data": data, "mimeType": f.mime}));
        } else {
            readable.push(crate::gateway::Readable {
                file_id: f.id.clone(),
                rel_path: rel.clone(),
            });
        }
        let mime = f.mime.split(';').next().unwrap_or("");
        lines.push(format!(
            "- {rel}（{mime}，{}{}）",
            kb(f.size),
            if inline {
                "，图片也直接附在这条消息里"
            } else {
                ""
            }
        ));
    }
    Ok(ForAgent {
        note: format!(
            "\n\n用户附带了 {} 个附件，已保存在工作目录里（绝对路径前缀 {}/），需要时直接读取：\n{}",
            lines.len(),
            cwd.display(),
            lines.join("\n")
        ),
        images,
        readable,
    })
}

/// An inline image of [`for_agent`] (ACP's `{type:"image", data, mimeType}`) as a Messages API image block,
/// for a `claude -p` user message.
pub fn message_image(v: &Value) -> Value {
    json!({"type": "image", "source": {
        "type": "base64",
        "media_type": v["mimeType"],
        "data": v["data"],
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(kb(10), "1 KB");
        assert_eq!(kb(1536), "2 KB");
        assert_eq!(kb(3 * 1024 * 1024 / 2), "1.5 MB");
    }
}
