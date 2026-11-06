//! Attachments (files.ts): validation by content (not by the name or the claimed type), storage under
//! `data/files`. The Hub never copies them into a working directory: the Node fetches what a turn
//! was granted (`wire/node_files.rs`) and places it there itself.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::backend::{BResult, BackendError, Obj, Repo};
use crate::js;

pub const MAX_FILE: usize = 20 * 1024 * 1024;
pub const MAX_FILES: usize = 10;
/// Image types a model takes inline; everything else only goes by path.
pub const INLINE_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A file kind Steve accepts: its MIME type and the extensions that name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub mime: &'static str,
    pub ext: &'static [&'static str],
}

const PNG: Kind = Kind {
    mime: "image/png",
    ext: &["png"],
};
const JPEG: Kind = Kind {
    mime: "image/jpeg",
    ext: &["jpg", "jpeg"],
};
const GIF: Kind = Kind {
    mime: "image/gif",
    ext: &["gif"],
};
const WEBP: Kind = Kind {
    mime: "image/webp",
    ext: &["webp"],
};
const PDF: Kind = Kind {
    mime: "application/pdf",
    ext: &["pdf"],
};
const ZIP: Kind = Kind {
    mime: "application/zip",
    ext: &["zip", "docx", "xlsx", "pptx", "jar", "apk", "ipa", "epub"],
};
pub const TEXT: Kind = Kind {
    mime: "text/plain; charset=utf-8",
    ext: &[],
};

/// The type of the bytes, or `None` when Steve doesn't accept it (executables, other archives; svg/html are text at most).
pub fn sniff(b: &[u8]) -> Option<Kind> {
    let at = |off: usize, sig: &[u8]| b.len() >= off + sig.len() && &b[off..off + sig.len()] == sig;
    if at(0, &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(PNG);
    }
    if at(0, &[0xff, 0xd8, 0xff]) {
        return Some(JPEG);
    }
    if at(0, b"GIF87a") || at(0, b"GIF89a") {
        return Some(GIF);
    }
    if at(0, b"RIFF") && at(8, b"WEBP") {
        return Some(WEBP);
    }
    if at(0, b"%PDF-") {
        return Some(PDF);
    }
    if at(0, &[0x50, 0x4b, 0x03, 0x04]) || at(0, &[0x50, 0x4b, 0x05, 0x06]) {
        return Some(ZIP);
    }
    // text: valid UTF-8 without NUL bytes (logs, code, csv, markdown…); always served as a download
    if !b.contains(&0) && std::str::from_utf8(b).is_ok() {
        return Some(TEXT);
    }
    None
}

fn invisible(c: char) -> bool {
    matches!(c as u32, 0x00..=0x1f | 0x7f..=0x9f | 0x200b..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069 | 0xfeff)
}

/// JS `lastIndexOf(".")` in UTF-16 units (or -1).
fn last_dot(s: &str) -> isize {
    match s.rfind('.') {
        Some(i) => js::utf16_len(&s[..i]) as isize,
        None => -1,
    }
}

/// `s.slice(from)` in UTF-16 units (negative from the end).
fn slice_from(s: &str, from: isize) -> String {
    let n = js::utf16_len(s) as isize;
    let start = if from < 0 {
        (n + from).max(0)
    } else {
        from.min(n)
    } as usize;
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        if used >= start {
            out.push(c);
        }
        used += c.len_utf16();
    }
    out
}

/// A file name that is safe on disk, in a header and in a prompt: no directories, control or bidi characters,
/// reserved punctuation or leading dots; the extension says what the bytes are.
pub fn clean_name(raw: Option<&str>, kind: Kind) -> String {
    let n: String = raw.unwrap_or("").nfc().filter(|c| !invisible(*c)).collect();
    let last = n.rsplit(['\\', '/']).next().unwrap_or("").to_string();
    let replaced: String = last
        .chars()
        .map(|c| if "<>:\"|?*`$".contains(c) { '_' } else { c })
        .collect();
    let mut collapsed = String::new();
    let mut in_ws = false;
    for c in replaced.chars() {
        if c.is_whitespace() {
            if !in_ws {
                collapsed.push(' ');
            }
            in_ws = true;
        } else {
            collapsed.push(c);
            in_ws = false;
        }
    }
    let mut n = collapsed
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim()
        .to_string();
    let dot = last_dot(&n);
    let mut ext = if dot > 0 {
        slice_from(&n, dot + 1).to_lowercase()
    } else {
        String::new()
    };
    let or_file = |n: &str| {
        if n.is_empty() {
            "file".to_string()
        } else {
            n.to_string()
        }
    };
    if !kind.ext.is_empty() && !kind.ext.contains(&ext.as_str()) {
        n = format!("{}.{}", or_file(&n), kind.ext[0]);
        ext = kind.ext[0].to_string();
    }
    if kind.ext.is_empty() && ext.is_empty() {
        n = format!("{}.txt", or_file(&n));
        ext = "txt".into();
    }
    if n.is_empty() || n.starts_with('.') {
        n = format!(
            "file{}",
            if ext.is_empty() {
                String::new()
            } else {
                format!(".{ext}")
            }
        );
    }
    if js::utf16_len(&n) > 120 {
        let tail = slice_from(&n, last_dot(&n));
        n = format!("{}{}", js::slice(&n, 120 - js::utf16_len(&tail)), tail);
    }
    n
}

/// Checks an upload: 400 empty, 413 too large, 415 not an accepted type. Returns (mime, clean name).
pub fn validate(b: &[u8], name: Option<&str>) -> BResult<(&'static str, String)> {
    if b.is_empty() {
        return Err(BackendError {
            status: 400,
            message: steve_i18n::t!("hub.file.empty"),
        });
    }
    if b.len() > MAX_FILE {
        return Err(BackendError {
            status: 413,
            message: steve_i18n::t!("hub.file.too_large", mb = MAX_FILE / 1024 / 1024),
        });
    }
    let Some(kind) = sniff(b) else {
        return Err(BackendError {
            status: 415,
            message: steve_i18n::t!("hub.file.unsupported"),
        });
    };
    Ok((kind.mime, clean_name(name, kind)))
}

/// `data/files/<workspace>/<sha256[..2]>/<id>`.
pub fn blob_path(data_dir: &Path, f: &Obj) -> PathBuf {
    let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or_default();
    let sha = s("sha256");
    data_dir
        .join("files")
        .join(s("workspace_id"))
        .join(sha.get(..2).unwrap_or(sha))
        .join(s("id"))
}

/// A row to insert into `files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub id: String,
    pub workspace_id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub created_by: String,
}

/// Validates and stores (under the write lock); the same bytes under the same name in a workspace are stored once.
/// A stored copy whose length differs from the row (cut short by a power loss before blobs were written
/// atomically) is written again from these bytes, which have the same hash.
pub fn store(
    r: &dyn Repo,
    data_dir: &Path,
    ws: &str,
    b: &[u8],
    name: Option<&str>,
    by: &str,
) -> BResult<Obj> {
    let (mime, name) = validate(b, name)?;
    let sha256 = hex::encode(Sha256::digest(b));
    if let Some(dup) = r.file_same(ws, &sha256, &name)? {
        let p = blob_path(data_dir, &dup);
        if std::fs::metadata(&p).map(|m| m.len()).ok() != Some(b.len() as u64) {
            write_blob(&p, b).map_err(|e| BackendError::msg(e.to_string()))?;
        }
        return Ok(dup);
    }
    let row = NewFile {
        id: crate::system::uid("fil"),
        workspace_id: ws.into(),
        sha256,
        name,
        mime: mime.into(),
        size: b.len() as u64,
        created_by: by.into(),
    };
    let mut probe = Obj::new();
    for (k, v) in [
        ("id", &row.id),
        ("workspace_id", &row.workspace_id),
        ("sha256", &row.sha256),
    ] {
        probe.insert(k.into(), json!(v));
    }
    let p = blob_path(data_dir, &probe);
    write_blob(&p, b).map_err(|e| BackendError::msg(e.to_string()))?;
    r.file_create(&row)
}

/// Atomically: a temporary file next to it, synced, then renamed over `p`, so a crash or power loss
/// leaves either no blob or the whole one, never a truncated one.
fn write_blob(p: &Path, b: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = p
        .parent()
        .ok_or_else(|| std::io::Error::other("a blob path has a parent"))?;
    std::fs::create_dir_all(dir)?;
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(b)?;
        f.sync_all()?;
        std::fs::rename(&tmp, p)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    // the rename itself survives a power loss once the directory is synced
    std::fs::File::open(dir).and_then(|d| d.sync_all())
}

/// `FileRef` = `{id, name, mime, size}`, the copy every other place stores.
pub fn file_ref(f: &Obj) -> Value {
    json!({ "id": f.get("id"), "name": f.get("name"), "mime": f.get("mime"), "size": f.get("size") })
}

/// Attachment ids from a request → FileRefs; every id must be a file of this workspace (404 `没有这个附件`).
pub fn refs(r: &dyn Repo, ws: &str, ids: Option<&[String]>) -> BResult<Vec<Value>> {
    let mut seen: Vec<&String> = vec![];
    for id in ids.unwrap_or_default() {
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    seen.into_iter()
        .map(|id| match r.file_get(id)? {
            Some(f) if f.get("workspace_id").and_then(Value::as_str) == Some(ws) => {
                Ok(file_ref(&f))
            }
            _ => Err(BackendError::not_found(crate::api::Noun::File.missing())),
        })
        .collect()
}

pub fn is_inline_image(mime: &str) -> bool {
    INLINE_IMAGES.contains(&mime)
}

/// `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `decodeURIComponent`: `None` where JS throws a URIError.
pub fn decode_uri_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())?;
            out.push(h);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `Content-Disposition` of a download: inline only for the four image types without `?download=1`.
pub fn disposition(name: &str, inline: bool) -> String {
    // [^\x20-\x7e] per UTF-16 unit, like the JS regex
    let ascii: String = name
        .chars()
        .flat_map(|c| {
            let keep = (' '..='~').contains(&c) && c != '"' && c != '\\';
            std::iter::repeat_n(
                if keep { c } else { '_' },
                if keep { 1 } else { c.len_utf16() },
            )
        })
        .collect();
    format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        encode_uri_component(name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

    fn png() -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.extend_from_slice(b"rest");
        v
    }

    #[test]
    fn types_come_from_the_bytes() {
        assert_eq!(
            validate(&png(), Some("shot.png")).map(|x| x.0),
            Ok("image/png")
        );
        assert_eq!(
            validate(b"%PDF-1.7\n...", Some("a.pdf")).map(|x| x.0),
            Ok("application/pdf")
        );
        assert_eq!(
            validate(&[0x50, 0x4b, 0x03, 0x04, 1, 2, 3], Some("a.zip")).map(|x| x.0),
            Ok("application/zip")
        );
        assert_eq!(
            validate("2026-10-31 ERROR 出错了\n".as_bytes(), Some("hub.log")).map(|x| x.0),
            Ok("text/plain; charset=utf-8")
        );
        assert_eq!(
            validate(b"<svg onload=alert(1)>", Some("x.svg")).map(|x| x.0),
            Ok("text/plain; charset=utf-8")
        );
        assert_eq!(
            validate(&[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 7], Some("tool")).map_err(|e| e.status),
            Err(415)
        );
        assert_eq!(
            validate(&[0xff, 0xfe, 0x00, 0x41], Some("a.txt")).map_err(|e| e.status),
            Err(415)
        );
        assert_eq!(validate(&[], Some("a.txt")).map_err(|e| e.status), Err(400));
        assert_eq!(
            validate(&vec![0x61; MAX_FILE + 1], Some("big.txt")).map_err(|e| e.status),
            Err(413)
        );
        assert!(validate(&vec![0x61; MAX_FILE], Some("ok.txt")).is_ok());
    }

    #[test]
    fn names_are_sanitized() {
        let n = |raw: &str| clean_name(Some(raw), PNG);
        assert_eq!(n("../../etc/passwd.png"), "passwd.png");
        assert_eq!(n("C:\\Users\\me\\截图 2026.png"), "截图 2026.png");
        assert_eq!(n(".bashrc.png"), "bashrc.png");
        assert_eq!(n("evil.exe"), "evil.exe.png");
        assert_eq!(n("a\u{202e}gnp.exe"), "agnp.exe.png");
        assert_eq!(n("x\"; filename=y.png"), "x_; filename=y.png");
        assert_eq!(n(""), "file.png");
        assert_eq!(clean_name(Some("notes"), TEXT), "notes.txt");
        assert_eq!(clean_name(Some("report.html"), TEXT), "report.html");
        let long = n(&format!("{}.png", "a".repeat(300)));
        assert!(long.len() <= 120 && long.ends_with(".png"));
        assert_eq!(n("a  \t b.PNG"), "a b.PNG");
    }

    #[test]
    fn sizes_and_headers() {
        assert_eq!(
            encode_uri_component("截图 a.png"),
            "%E6%88%AA%E5%9B%BE%20a.png"
        );
        assert_eq!(decode_uri_component("%E6%88%AA").as_deref(), Some("截"));
        assert_eq!(decode_uri_component("%"), None);
        assert_eq!(
            disposition("截图\"x.png", true),
            "inline; filename=\"___x.png\"; filename*=UTF-8''%E6%88%AA%E5%9B%BE%22x.png"
        );
    }
}
