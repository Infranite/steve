//! Attachments (email.md §6): received attachments are only metadata until they are “landed” — taken
//! out of the original message and stored as a Steve attachment (`files`, through [`FileStore`]) —
//! and files an agent attaches to a draft come from its working directory. Both directions copy
//! bytes across the boundary of a session's working directory, so every path is checked here: nothing
//! is read from or written to outside `<cwd>`, symlinks included.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use steve_modules::ApiError;
use steve_store::FileRef;
use steve_store::base::{Viewer, Visibility};

use crate::error::{MailError, MailResult};
use crate::{Mail, parse, repo, store};

/// What `files.ts` accepts at most (and what can be landed).
pub const MAX_FILE: i64 = 20 * 1024 * 1024;
/// Where attachments go inside a session's working directory.
pub const WORKDIR_ATTACHMENTS: &str = ".steve/attachments";

/// A file the attachment store refused, with the status the REST route answers (`400` empty, `410`
/// original gone, `413` too large, `415` type not allowed) and the Chinese reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub status: u16,
    pub message: String,
}

impl Refused {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// steve-hub's attachment store (`files.rs` `store`): validates by content (`validate/sniff/cleanName`
/// of delta-attachments.md), dedupes per workspace + sha256 + name, writes
/// `data/files/<ws>/<sha[..2]>/<id>` (0600) and the `files` row. The integrator implements it.
pub trait FileStore: Send + Sync {
    fn store(&self, ws: &str, bytes: &[u8], name: &str, by: &str) -> Result<FileRef, Refused>;
}

/// No store wired: nothing can be landed.
pub struct NoFiles;

impl FileStore for NoFiles {
    fn store(&self, _: &str, _: &[u8], _: &str, _: &str) -> Result<FileRef, Refused> {
        Err(Refused::new(400, "附件存储还没有接上"))
    }
}

/// One `mail_attachments` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFull {
    pub id: String,
    pub workspace_id: String,
    pub message_id: String,
    pub part: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub file_id: Option<String>,
}

pub fn attachment(c: &Connection, id: &str) -> MailResult<Option<AttachmentFull>> {
    Ok(c
        .prepare_cached(
            "SELECT id, workspace_id, message_id, part, name, mime, size, file_id FROM mail_attachments WHERE id = ?",
        )?
        .query_row([id], |r| {
            Ok(AttachmentFull {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                message_id: r.get(2)?,
                part: r.get(3)?,
                name: r.get(4)?,
                mime: r.get(5)?,
                size: r.get(6)?,
                file_id: r.get(7)?,
            })
        })
        .optional()?)
}

fn set_file_id(c: &Connection, id: &str, file: &str) -> MailResult<()> {
    c.prepare_cached("UPDATE mail_attachments SET file_id = ? WHERE id = ?")?
        .execute(params![file, id])?;
    Ok(())
}

/// The `files` row of an id as a [`FileRef`], when it is in `ws` and its bytes are still there.
fn existing_file(c: &Connection, home: &Path, ws: &str, id: &str) -> MailResult<Option<FileRef>> {
    let Some((fws, sha, name, mime, size)) = repo::file_row(c, id)? else {
        return Ok(None);
    };
    let present =
        fws == ws && crate::send::files_dir(home, ws, &sha, id).is_some_and(|p| p.is_file());
    Ok(present.then(|| FileRef {
        id: id.to_string(),
        name,
        mime,
        size,
    }))
}

impl Mail {
    /// The attachment and the thread it belongs to, if `viewer` may see that thread
    /// (`404 没有这个附件` otherwise).
    pub(crate) fn own_attachment(
        &self,
        c: &Connection,
        viewer: &Viewer,
        id: &str,
    ) -> MailResult<(AttachmentFull, repo::MessageRow)> {
        let nf = || MailError::Api(ApiError::not_found("附件"));
        let a = attachment(c, id)?
            .filter(|a| a.workspace_id == viewer.ws)
            .ok_or_else(nf)?;
        let m = repo::message(c, &a.message_id)?.ok_or_else(nf)?;
        let t = repo::thread(c, &m.thread_id)?.ok_or_else(nf)?;
        if t.workspace_id != viewer.ws
            || !viewer.sees(&Visibility::from_db(&t.visibility), &t.created_by)
        {
            return Err(nf());
        }
        Ok((a, m))
    }

    /// Lands a received attachment (§6): from the original message into `files` (deduped), and
    /// remembers the file on the attachment row. `Ok(Err(_))` = it cannot be landed, with the reason.
    pub fn land_attachment(
        &self,
        viewer: &Viewer,
        id: &str,
    ) -> MailResult<Result<FileRef, Refused>> {
        let home = self.0.cfg.home().to_path_buf();
        let (a, m) = self
            .0
            .m
            .store
            .reader(|c| self.own_attachment(c, viewer, id))?;
        if let Some(f) = &a.file_id
            && let Some(r) = self
                .0
                .m
                .store
                .reader(|c| existing_file(c, &home, &viewer.ws, f))?
        {
            return Ok(Ok(r));
        }
        if a.size > MAX_FILE {
            return Ok(Err(Refused::new(413, "文件太大，最多 20 MB")));
        }
        let Some(path) = m
            .raw_path
            .as_deref()
            .and_then(|r| store::raw_abs_path(&home, r))
        else {
            return Ok(Err(Refused::new(
                410,
                "原件已经过了本地保留期，请先重新取原件",
            )));
        };
        let raw = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Err(Refused::new(
                    410,
                    "原件已经过了本地保留期，请先重新取原件",
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let Some(bytes) = parse::attachment_bytes(&raw, &a.part) else {
            return Ok(Err(Refused::new(404, "原件里找不到这个附件")));
        };
        let by = if viewer.user.is_empty() {
            "steve"
        } else {
            viewer.user.as_str()
        };
        let f = match self.0.files.store(&viewer.ws, &bytes, &a.name, by) {
            Ok(f) => f,
            Err(r) => return Ok(Err(r)),
        };
        self.0
            .m
            .store
            .transaction(|tx| set_file_id(tx, &a.id, &f.id))?;
        Ok(Ok(f))
    }

    /// The bytes of a stored attachment (`data/files/...`).
    pub(crate) fn file_path(&self, ws: &str, id: &str) -> MailResult<Option<PathBuf>> {
        let home = self.0.cfg.home().to_path_buf();
        self.0.m.store.reader(|c| -> MailResult<Option<PathBuf>> {
            Ok(repo::file_row(c, id)?
                .filter(|r| r.0 == ws)
                .and_then(|r| crate::send::files_dir(&home, ws, &r.1, id)))
        })
    }

    /// Copies a stored attachment into `<cwd>/.steve/attachments/<id without fil_>-<name>` (the same
    /// place and name as attachments in prompts; skipped when already there) and answers the path
    /// relative to `cwd`.
    pub fn copy_to_workdir(&self, ws: &str, cwd: &Path, f: &FileRef) -> Result<String, String> {
        let src = self
            .file_path(ws, &f.id)
            .map_err(|e| e.to_string())?
            .filter(|p| p.is_file())
            .ok_or("The attachment's bytes are missing.")?;
        copy_into(cwd, &src, f)
    }

    /// Reads files an agent named relative to its working directory and stores them as attachments
    /// (`validate` included). Each path must stay inside `cwd` after resolving symlinks.
    pub fn files_from_workdir(
        &self,
        ws: &str,
        by: &str,
        cwd: &Path,
        paths: &[String],
    ) -> Result<Vec<FileRef>, String> {
        let mut out: Vec<FileRef> = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();
        for p in paths {
            let (bytes, name, real) = read_inside(cwd, p)?;
            if seen.contains(&real) {
                continue;
            }
            seen.push(real);
            let f = self
                .0
                .files
                .store(ws, &bytes, &name, by)
                .map_err(|r| format!("{p}: {}", r.message))?;
            if !out.iter().any(|x| x.id == f.id) {
                out.push(f);
            }
        }
        Ok(out)
    }
}

/// The canonical working directory: absolute, existing, a directory.
pub fn workdir(cwd: &Path) -> Result<PathBuf, String> {
    if !cwd.is_absolute() {
        return Err("This session has no usable working directory.".into());
    }
    let c = cwd
        .canonicalize()
        .map_err(|_| "This session's working directory does not exist.".to_string())?;
    if !c.is_dir() {
        return Err("This session's working directory does not exist.".into());
    }
    Ok(c)
}

/// A file name that is one plain path segment.
fn safe_segment(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim_start_matches('.').trim().to_string();
    if s.is_empty() { "file".into() } else { s }
}

fn copy_into(cwd: &Path, src: &Path, f: &FileRef) -> Result<String, String> {
    let root = workdir(cwd)?;
    // a planted symlink (`.steve` → elsewhere) must not carry anything out of the working directory,
    // not even an empty directory: each level must be a real directory, created here if missing
    let mut dir = root.clone();
    for part in [".steve", "attachments"] {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{WORKDIR_ATTACHMENTS} points outside your working directory."
                ));
            }
            Err(_) => fs::create_dir(&dir)
                .map_err(|e| format!("Could not create {WORKDIR_ATTACHMENTS}: {e}"))?,
        }
    }
    let real = dir
        .canonicalize()
        .map_err(|e| format!("Could not open {WORKDIR_ATTACHMENTS}: {e}"))?;
    if !real.starts_with(&root) {
        return Err(format!(
            "{WORKDIR_ATTACHMENTS} points outside your working directory."
        ));
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(real.join(".gitignore"))
    {
        Ok(mut g) => g.write_all(b"*\n").map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let file = format!(
        "{}-{}",
        f.id.strip_prefix("fil_").unwrap_or(&f.id),
        safe_segment(&f.name)
    );
    let dest = real.join(&file);
    match fs::symlink_metadata(&dest) {
        Ok(md) if md.file_type().is_file() => {}
        Ok(_) => return Err(format!("{WORKDIR_ATTACHMENTS}/{file} is in the way.")),
        Err(_) => {
            let mut from = fs::File::open(src).map_err(|e| e.to_string())?;
            let mut to = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dest)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut from, &mut to).map_err(|e| e.to_string())?;
        }
    }
    Ok(format!("{WORKDIR_ATTACHMENTS}/{file}"))
}

/// Reads `rel` inside `cwd`: relative, no `..`, the resolved file still under `cwd`, a regular file
/// of at most 20 MB. Returns the bytes, the file name and the resolved path.
pub fn read_inside(cwd: &Path, rel: &str) -> Result<(Vec<u8>, String, PathBuf), String> {
    let root = workdir(cwd)?;
    let p = Path::new(rel.trim());
    let plain = !rel.trim().is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !plain {
        return Err(format!(
            "{rel}: give a path relative to your working directory, without \"..\"."
        ));
    }
    let real = root
        .join(p)
        .canonicalize()
        .map_err(|_| format!("{rel}: no such file in your working directory."))?;
    if !real.starts_with(&root) {
        return Err(format!(
            "{rel}: the file is outside your working directory."
        ));
    }
    let md = fs::metadata(&real).map_err(|e| format!("{rel}: {e}"))?;
    if !md.is_file() {
        return Err(format!("{rel}: not a file."));
    }
    if md.len() > MAX_FILE as u64 {
        return Err(format!("{rel}: 文件太大，最多 20 MB"));
    }
    let mut bytes = Vec::with_capacity(md.len() as usize);
    fs::File::open(&real)
        .and_then(|f| f.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("{rel}: {e}"))?;
    if bytes.len() as i64 > MAX_FILE {
        return Err(format!("{rel}: 文件太大，最多 20 MB"));
    }
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    Ok((bytes, name, real))
}

/// files.ts `kb`: `1.5 MB` / `12 KB` (at least 1 KB).
pub fn size_label(n: i64) -> String {
    const MB: i64 = 1024 * 1024;
    let n = n.max(0);
    if n >= MB {
        let tenths = (20 * n + MB) / (2 * MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else {
        format!("{} KB", ((2 * n + 1024) / 2048).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fref(id: &str, name: &str) -> FileRef {
        FileRef {
            id: id.into(),
            name: name.into(),
            mime: "application/pdf".into(),
            size: 3,
        }
    }

    #[test]
    fn copies_into_the_workdir_once() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("blob");
        fs::write(&src, b"pdf").unwrap();
        let cwd = d.path().join("work");
        fs::create_dir(&cwd).unwrap();
        let rel = copy_into(&cwd, &src, &fref("fil_ab12", "../../etc/x.pdf")).unwrap();
        assert_eq!(rel, ".steve/attachments/ab12-_.._etc_x.pdf");
        assert_eq!(fs::read(cwd.join(&rel)).unwrap(), b"pdf");
        assert_eq!(
            fs::read_to_string(cwd.join(".steve/attachments/.gitignore")).unwrap(),
            "*\n"
        );
        // again: already there
        assert_eq!(
            copy_into(&cwd, &src, &fref("fil_ab12", "../../etc/x.pdf")).unwrap(),
            rel
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_does_not_escape() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("blob");
        fs::write(&src, b"pdf").unwrap();
        let cwd = d.path().join("work");
        let outside = d.path().join("outside");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join(".steve")).unwrap();
        assert!(copy_into(&cwd, &src, &fref("fil_1", "a.pdf")).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

        // and reading: a link inside cwd that points out is refused
        fs::write(outside.join("secret.txt"), b"s").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), cwd.join("link.txt")).unwrap();
        assert!(
            read_inside(&cwd, "link.txt")
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn reads_only_inside() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("work");
        fs::create_dir_all(cwd.join("out")).unwrap();
        fs::write(cwd.join("out/report.txt"), b"hello").unwrap();
        fs::write(d.path().join("secret.txt"), b"s").unwrap();
        let (b, n, _) = read_inside(&cwd, "out/report.txt").unwrap();
        assert_eq!((b.as_slice(), n.as_str()), (&b"hello"[..], "report.txt"));
        assert!(read_inside(&cwd, "./out/report.txt").is_ok());
        for bad in [
            "../secret.txt",
            "out/../../secret.txt",
            "/etc/passwd",
            "",
            "out",
        ] {
            assert!(read_inside(&cwd, bad).is_err(), "{bad}");
        }
        assert!(read_inside(Path::new("relative"), "x").is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(size_label(10), "1 KB");
        assert_eq!(size_label(5 * 1024), "5 KB");
        assert_eq!(size_label(1536 * 1024), "1.5 MB");
    }
}
