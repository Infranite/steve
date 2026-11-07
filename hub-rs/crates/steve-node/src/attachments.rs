//! Attachments in a session prompt, placed by the Node: every artifact the Hub granted the attempt
//! is fetched through `HubFiles` (checked against its hash), then put into
//! `<workspace>/.steve/attachments` (ignored by git) and named in the prompt text; images that fit
//! also go inline when the adapter takes them. The Hub never writes into the workspace.
//!
//! The text is the one the legacy runner wrote, so a session that moved to the Node reads the same.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest as _;

/// image types a model takes inline; everything else only goes by path
pub(crate) const INLINE_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// the largest image sent inline (the model's limit, with room for base64)
const MAX_INLINE: u64 = 3_750_000;
/// Where attachments go, relative to the workspace.
pub(crate) const DIR: &str = ".steve/attachments";

/// One attachment, fetched: its bytes are at `blob` (the Node's checked cache).
#[derive(Debug, Clone)]
pub(crate) struct Fetched {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// `sha256:<hex>`, as the Hub granted it.
    pub sha256: String,
    pub blob: PathBuf,
}

/// What goes to the agent: the note appended to the prompt text, and the inline image blocks.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ForAgent {
    pub note: String,
    pub images: Vec<Value>,
}

/// `1.3 MB` (toFixed(1), halves up) or `12 KB` (rounded, at least 1), as the web shows sizes.
pub(crate) fn kb(n: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if n >= MB {
        let tenths = (20 * n + MB) / (2 * MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else {
        format!("{} KB", ((2 * n + 1024) / 2048).max(1))
    }
}

/// A name that stays one file inside the attachments directory, whatever the Hub sent: no
/// separators, no control characters, never `.`/`..` or empty. A name the Hub cleaned already
/// (every upload is) comes through unchanged.
pub(crate) fn file_name(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "file".into()
    } else {
        s
    }
}

/// The path of `f` relative to the workspace (`.steve/attachments/<id without its prefix>-<name>`).
pub(crate) fn rel_path(f: &Fetched) -> String {
    let id = file_name(&f.id.chars().skip(4).collect::<String>());
    format!("{DIR}/{id}-{}", file_name(&f.name))
}

/// A directory of the workspace, made if missing; refused when it is anything but a real
/// directory (a link could point out of the workspace).
fn real_dir(p: &Path) -> Result<(), String> {
    match fs::create_dir(p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", p.display())),
    }
    let m = fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if m.file_type().is_symlink() || !m.is_dir() {
        return Err(format!("{} is not a directory", p.display()));
    }
    Ok(())
}

/// Whether `p` is a regular file (not a link) holding exactly the bytes hashed `sha256`.
fn holds(p: &Path, size: u64, sha256: &str) -> bool {
    let Ok(m) = fs::symlink_metadata(p) else {
        return false;
    };
    if !m.file_type().is_file() || m.len() != size {
        return false;
    }
    fs::read(p).is_ok_and(|b| format!("sha256:{}", hex::encode(sha2::Sha256::digest(&b))) == sha256)
}

/// Writes `bytes` to `dst` whole: a temporary file next to it, then one rename (which replaces a
/// link at `dst` rather than writing through it).
fn publish(dst: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = dst.parent().ok_or("no directory")?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let write = || -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, dst)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", dst.display())
    })
}

/// Puts `files` into `<cwd>/.steve/attachments` and says where they are. Blocking.
///
/// The directory gets a `.gitignore` of `*` (the same one the legacy runner wrote), so nothing
/// placed here is committed. A file already there with the right bytes is left alone; one that is
/// not (cut short, changed, a link) is replaced.
pub(crate) fn place(
    cwd: &Path,
    files: &[Fetched],
    inline_images: bool,
) -> Result<ForAgent, String> {
    if files.is_empty() {
        return Ok(ForAgent::default());
    }
    real_dir(&cwd.join(".steve"))?;
    let dir = cwd.join(DIR);
    real_dir(&dir)?;
    let ign = dir.join(".gitignore");
    if !fs::symlink_metadata(&ign).is_ok_and(|m| m.file_type().is_file()) {
        publish(&ign, b"*\n")?;
    }
    let (mut lines, mut images) = (vec![], vec![]);
    for f in files {
        let rel = rel_path(f);
        let dst = cwd.join(&rel);
        if !holds(&dst, f.size, &f.sha256) {
            let bytes = fs::read(&f.blob).map_err(|e| format!("{}: {e}", f.name))?;
            publish(&dst, &bytes)?;
        }
        let inline =
            inline_images && INLINE_IMAGES.contains(&f.mime.as_str()) && f.size <= MAX_INLINE;
        if inline {
            let bytes = fs::read(&f.blob).map_err(|e| format!("{}: {e}", f.name))?;
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
            "\n\n用户附带了 {} 个附件，已保存在工作目录里（绝对路径前缀 {}/），需要时直接读取：\n{}",
            lines.len(),
            cwd.display(),
            lines.join("\n")
        ),
        images,
    })
}

/// The note for attachments the Hub could not hand over (gone or changed there): the turn goes on
/// without them and the agent is told, as the legacy runner did.
pub(crate) fn missing_note(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    format!(
        "\n\n（用户还附带了 {} 个附件，但没能取到：{}）",
        names.len(),
        names.join("、")
    )
}

/// The note when the attachments could not be put into the workspace at all.
pub(crate) fn failed_note(count: usize, e: &str) -> String {
    format!("\n\n（用户附带了 {count} 个附件，但没能放进工作目录：{e}）")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(b: &[u8]) -> String {
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)))
    }

    fn fetched(t: &Path, id: &str, name: &str, mime: &str, bytes: &[u8]) -> Fetched {
        let blob = t.join(format!("blob-{}", hex::encode(id)));
        fs::write(&blob, bytes).unwrap();
        Fetched {
            id: id.into(),
            name: name.into(),
            mime: mime.into(),
            size: bytes.len() as u64,
            sha256: sha(bytes),
            blob,
        }
    }

    #[test]
    fn sizes_like_the_web() {
        assert_eq!(kb(0), "1 KB");
        assert_eq!(kb(1535), "1 KB");
        assert_eq!(kb(1536), "2 KB");
        assert_eq!(kb(1_310_720), "1.3 MB");
    }

    /// The same text, files and ignore file the legacy runner made (steve-acp attachments.rs).
    #[test]
    fn placed_like_the_legacy_runner() {
        let t = tempfile::tempdir().unwrap();
        let cwd = t.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let png = fetched(
            t.path(),
            "fil_0123456789abcdef",
            "a.png",
            "image/png",
            b"\x89PNG\r\n\x1a\nxx",
        );
        let txt = fetched(
            t.path(),
            "fil_fedcba9876543210",
            "log.txt",
            "text/plain; charset=utf-8",
            &[b'x'; 2048],
        );
        let a = place(&cwd, &[png.clone(), txt], true).unwrap();
        assert_eq!(
            a.note,
            format!(
                "\n\n用户附带了 2 个附件，已保存在工作目录里（绝对路径前缀 {}/），需要时直接读取：\n- .steve/attachments/0123456789abcdef-a.png（image/png，1 KB，图片也直接附在这条消息里）\n- .steve/attachments/fedcba9876543210-log.txt（text/plain，2 KB）",
                cwd.display()
            )
        );
        assert_eq!(
            a.images,
            [json!({"type": "image", "data": "iVBORw0KGgp4eA==", "mimeType": "image/png"})]
        );
        assert_eq!(
            fs::read_to_string(cwd.join(".steve/attachments/.gitignore")).unwrap(),
            "*\n"
        );
        assert_eq!(
            fs::read(cwd.join(".steve/attachments/0123456789abcdef-a.png")).unwrap(),
            b"\x89PNG\r\n\x1a\nxx"
        );
        let a = place(&cwd, std::slice::from_ref(&png), false).unwrap();
        assert!(a.images.is_empty() && !a.note.contains("直接附在"));
    }

    #[test]
    fn a_short_or_linked_copy_is_replaced() {
        let t = tempfile::tempdir().unwrap();
        let cwd = t.path().join("work");
        fs::create_dir_all(cwd.join(DIR)).unwrap();
        let f = fetched(t.path(), "fil_aa", "n.txt", "text/plain", b"the whole file");
        let dst = cwd.join(rel_path(&f));
        fs::write(&dst, b"the wh").unwrap();
        place(&cwd, std::slice::from_ref(&f), false).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"the whole file");
        // a link at the destination is replaced, its target untouched
        let outside = t.path().join("outside");
        fs::write(&outside, b"keep").unwrap();
        fs::remove_file(&dst).unwrap();
        std::os::unix::fs::symlink(&outside, &dst).unwrap();
        place(&cwd, std::slice::from_ref(&f), false).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        assert!(!fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&dst).unwrap(), b"the whole file");
        // nothing temporary is left behind
        let left: Vec<_> = fs::read_dir(cwd.join(DIR))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_linked_attachments_directory_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let cwd = t.path().join("work");
        let elsewhere = t.path().join("elsewhere");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, cwd.join(".steve")).unwrap();
        let f = fetched(t.path(), "fil_bb", "n.txt", "text/plain", b"x");
        assert!(place(&cwd, &[f], false).is_err());
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "nothing written"
        );
    }

    #[test]
    fn names_stay_inside_the_directory() {
        assert_eq!(file_name("a.png"), "a.png");
        assert_eq!(file_name("截图 1.png"), "截图 1.png");
        assert_eq!(file_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(file_name(".."), "file");
        assert_eq!(file_name(""), "file");
        assert_eq!(file_name("a\nb\\c"), "a_b_c");
        let t = tempfile::tempdir().unwrap();
        let cwd = t.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let f = fetched(t.path(), "fil_cc/../..", "../x", "text/plain", b"x");
        place(&cwd, std::slice::from_ref(&f), false).unwrap();
        let names: Vec<_> = fs::read_dir(cwd.join(DIR))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"cc_.._..-.._x".to_string()), "{names:?}");
    }

    #[test]
    fn notes_for_what_could_not_be_placed() {
        assert_eq!(missing_note(&[]), "");
        assert_eq!(
            missing_note(&["a.png".into(), "b.txt".into()]),
            "\n\n（用户还附带了 2 个附件，但没能取到：a.png、b.txt）"
        );
        assert_eq!(
            failed_note(1, "disk full"),
            "\n\n（用户附带了 1 个附件，但没能放进工作目录：disk full）"
        );
    }
}
