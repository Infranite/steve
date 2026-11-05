//! Attachments in prompts (files.ts `forAgent`, acp.ts `promptBlocks`): files are copied into the
//! session's working directory and named in the text; images also go inline when the adapter takes them.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{Value, json};

/// What every other place stores for an attachment (copied, not joined): `{id, name, mime, size}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl FileRef {
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "mime": self.mime, "size": self.size })
    }
}

/// A stored attachment as the Hub knows it: the `files` row plus the blob path
/// (`$STEVE_HOME/data/files/<ws>/<sha[0..2]>/<id>`).
#[derive(Debug, Clone)]
pub struct StoredFile {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub path: PathBuf,
}

/// image types a model takes inline; everything else only goes by path
pub const INLINE_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_INLINE: u64 = 3_750_000;

/// files.ts `kb`: `toFixed(1)` and `Math.round` both round halves up; done in integers to match exactly.
pub fn kb(n: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if n >= MB {
        let tenths = (20 * n + MB) / (2 * MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else {
        format!("{} KB", ((2 * n + 1024) / 2048).max(1))
    }
}

pub struct ForAgent {
    pub note: String,
    pub images: Vec<Value>,
}

/// Copy attachments into `<cwd>/.steve/attachments` (ignored by git) and say where they are;
/// images that fit also go inline. Rows the Hub no longer knows are skipped, like the TS.
pub fn for_agent(
    cwd: &str,
    refs: &[FileRef],
    inline_images: bool,
    lookup: impl Fn(&str) -> Option<StoredFile>,
) -> Result<ForAgent, String> {
    if refs.is_empty() {
        return Ok(ForAgent {
            note: String::new(),
            images: vec![],
        });
    }
    let dir = Path::new(cwd).join(".steve").join("attachments");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ign = dir.join(".gitignore");
    if !ign.exists() {
        fs::write(&ign, "*\n").map_err(|e| e.to_string())?;
    }
    let (mut lines, mut images) = (vec![], vec![]);
    for r in refs {
        let Some(f) = lookup(&r.id) else { continue };
        let rel = format!(
            ".steve/attachments/{}-{}",
            f.id.chars().skip(4).collect::<String>(),
            f.name
        );
        let dst = Path::new(cwd).join(&rel);
        if !dst.exists() {
            fs::copy(&f.path, &dst).map_err(|e| e.to_string())?;
        }
        let inline =
            inline_images && INLINE_IMAGES.contains(&f.mime.as_str()) && f.size <= MAX_INLINE;
        if inline {
            let bytes = fs::read(&f.path).map_err(|e| e.to_string())?;
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            images.push(json!({ "type": "image", "data": data, "mimeType": f.mime }));
        }
        let mime = f.mime.split(';').next().unwrap_or_default();
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
            "\n\n用户附带了 {} 个附件，已保存在工作目录里（绝对路径前缀 {cwd}/），需要时直接读取：\n{}",
            lines.len(),
            lines.join("\n")
        ),
        images,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_like_js() {
        assert_eq!(kb(0), "1 KB");
        assert_eq!(kb(511), "1 KB");
        assert_eq!(kb(1536), "2 KB");
        assert_eq!(kb(1535), "1 KB");
        assert_eq!(kb(1024 * 1024), "1.0 MB");
        assert_eq!(kb(1_310_720), "1.3 MB"); // exactly 1.25: toFixed rounds up
        assert_eq!(kb(20 * 1024 * 1024), "20.0 MB");
    }

    #[test]
    fn copies_and_describes() {
        let t = tempfile::tempdir().unwrap();
        let blob = t.path().join("blob");
        fs::write(&blob, b"\x89PNG\r\n\x1a\nxx").unwrap();
        let cwd = t.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.display().to_string();
        let png = FileRef {
            id: "fil_0123456789abcdef".into(),
            name: "a.png".into(),
            mime: "image/png".into(),
            size: 10,
        };
        let txt = FileRef {
            id: "fil_fedcba9876543210".into(),
            name: "log.txt".into(),
            mime: "text/plain; charset=utf-8".into(),
            size: 2048,
        };
        let gone = FileRef {
            id: "fil_gone".into(),
            name: "x".into(),
            mime: "image/png".into(),
            size: 1,
        };
        let lookup = |id: &str| {
            [&png, &txt]
                .into_iter()
                .find(|f| f.id == id)
                .map(|f| StoredFile {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    mime: f.mime.clone(),
                    size: f.size,
                    path: blob.clone(),
                })
        };
        let a = for_agent(&cwd, &[png.clone(), txt.clone(), gone], true, lookup).unwrap();
        assert_eq!(
            a.note,
            format!(
                "\n\n用户附带了 2 个附件，已保存在工作目录里（绝对路径前缀 {cwd}/），需要时直接读取：\n- .steve/attachments/0123456789abcdef-a.png（image/png，1 KB，图片也直接附在这条消息里）\n- .steve/attachments/fedcba9876543210-log.txt（text/plain，2 KB）"
            )
        );
        assert_eq!(
            a.images,
            [json!({"type": "image", "data": "iVBORw0KGgp4eA==", "mimeType": "image/png"})]
        );
        assert_eq!(
            fs::read_to_string(format!("{cwd}/.steve/attachments/.gitignore")).unwrap(),
            "*\n"
        );
        assert!(Path::new(&format!("{cwd}/.steve/attachments/0123456789abcdef-a.png")).exists());
        // no inline images for adapters that do not take them
        let a = for_agent(&cwd, std::slice::from_ref(&png), false, lookup).unwrap();
        assert!(a.images.is_empty() && !a.note.contains("直接附在"));
        // a missing blob is an error the caller turns into a note
        let bad = |_: &str| {
            Some(StoredFile {
                id: "fil_zzzz".into(),
                name: "b.png".into(),
                mime: "image/png".into(),
                size: 1,
                path: "/nonexistent".into(),
            })
        };
        assert!(for_agent(&cwd, std::slice::from_ref(&png), true, bad).is_err());
    }
}
