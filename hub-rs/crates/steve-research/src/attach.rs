//! The question's attachments in a research run: every file is copied into a
//! per-run directory outside `~/.steve` (whose reads stay denied) that the run may read, and is named in
//! the prompt; images small enough also go into the message itself as image blocks, so the model sees
//! them without a tool call. Read-only either way: the copy is made by the Hub before the harness starts,
//! the sandbox does not let the harness write there, and the directory is removed when the run ends.

use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Image types the model takes inline.
pub const INLINE_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// Base64 images go up to 5 MB; larger ones are read from disk.
pub const MAX_INLINE: u64 = 3_750_000;

/// One attachment of the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// where the Hub keeps it (never handed to the harness)
    pub path: PathBuf,
    /// the name the user gave it
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// attached to an earlier message the question follows up on, not to the question itself
    pub earlier: bool,
}

/// The attachments as the run sees them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prepared {
    /// the directory the copies are in (readable by the run), when there are any
    pub dir: Option<PathBuf>,
    /// appended to the prompt
    pub note: String,
    /// `{"type":"image","source":{…}}` blocks for the user message
    pub images: Vec<Value>,
    /// the copies of those images (Codex takes them as `-i <file>`)
    pub image_files: Vec<PathBuf>,
}

fn kb(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{} KB", ((n as f64 / 1024.0).round() as u64).max(1))
    }
}

/// A file name that stays inside `dir`: path separators and leading dots go.
fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() { "attachment".into() } else { s }
}

/// Copies `files` into `dir` (created `0700`) and builds the note and the image blocks.
pub async fn prepare(dir: &Path, files: &[Attachment]) -> std::io::Result<Prepared> {
    if files.is_empty() {
        return Ok(Prepared::default());
    }
    tokio::fs::create_dir_all(dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let dir = crate::guard::resolve(dir);
    let mut lines = vec![];
    let mut images = vec![];
    let mut image_files = vec![];
    let mut used: Vec<String> = vec![];
    for (n, f) in files.iter().enumerate() {
        let mut name = safe_name(&f.name);
        if used.contains(&name) {
            name = format!("{}-{name}", n + 1);
        }
        used.push(name.clone());
        let dst = dir.join(&name);
        let mime = f.mime.split(';').next().unwrap_or("").trim().to_string();
        let whose = if f.earlier {
            "（前面消息里的）"
        } else {
            ""
        };
        if let Err(e) = tokio::fs::copy(&f.path, &dst).await {
            tracing::warn!(file = %f.name, "research attachment not copied: {e}");
            lines.push(format!("- {whose}{}：没能读取", f.name));
            continue;
        }
        let inline = INLINE_IMAGES.contains(&mime.as_str()) && f.size <= MAX_INLINE;
        if inline {
            image_files.push(dst.clone());
            let data = tokio::fs::read(&dst).await?;
            images.push(json!({"type": "image", "source": {
                "type": "base64",
                "media_type": mime,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }}));
        }
        lines.push(format!(
            "- {whose}{}（{mime}，{}{}）",
            dst.display(),
            kb(f.size),
            if inline {
                "，图片已直接附在这条消息里"
            } else {
                ""
            }
        ));
    }
    Ok(Prepared {
        note: format!(
            "\n\n用户附带了 {} 个附件（只读副本），回答前先看它们；图片直接看消息里附的图，其他文件用 Read 读取：\n{}",
            lines.len(),
            lines.join("\n")
        ),
        images,
        image_files,
        dir: Some(dir),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copies_names_and_inlines_images() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("blob");
        std::fs::write(&src, b"\x89PNG").unwrap();
        let txt = t.path().join("blob2");
        std::fs::write(&txt, "hello").unwrap();
        let dir = t.path().join("files/tr_1");
        let p = prepare(
            &dir,
            &[
                Attachment {
                    path: src.clone(),
                    name: "截图.png".into(),
                    mime: "image/png".into(),
                    size: 4,
                    earlier: false,
                },
                Attachment {
                    path: txt,
                    name: "../notes.txt".into(),
                    mime: "text/plain; charset=utf-8".into(),
                    size: 5,
                    earlier: true,
                },
                Attachment {
                    path: t.path().join("gone"),
                    name: "gone.pdf".into(),
                    mime: "application/pdf".into(),
                    size: 1,
                    earlier: false,
                },
            ],
        )
        .await
        .unwrap();
        let d = p.dir.clone().unwrap();
        assert_eq!(std::fs::read(d.join("截图.png")).unwrap(), b"\x89PNG");
        assert_eq!(
            std::fs::read_to_string(d.join("_notes.txt")).unwrap(),
            "hello"
        );
        assert_eq!(p.images.len(), 1);
        assert_eq!(p.images[0]["source"]["media_type"], "image/png");
        assert!(
            p.note
                .contains("截图.png（image/png，1 KB，图片已直接附在这条消息里）")
        );
        assert!(p.note.contains("（前面消息里的）"));
        assert!(p.note.contains("text/plain，1 KB）"));
        assert!(p.note.contains("gone.pdf：没能读取"));
        assert!(prepare(&dir, &[]).await.unwrap().dir.is_none());
    }
}
