//! Validate an entire ZIP before publishing any delivery. Names are logical manifest keys;
//! nothing from an archive is ever extracted to a pathname.
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use super::MAX_FILE;

const MAX_ENTRIES: usize = 1024;
const MAX_EXPANDED: u64 = 128 * 1024 * 1024;
const MAX_ENTRY: u64 = 32 * 1024 * 1024;

pub(super) struct Bundle {
    pub entrypoint: Option<String>,
    pub files: BTreeMap<String, Vec<u8>>,
}

/// Strict on both platforms, including ZIPs made on a different OS. No normalization which
/// could silently merge two entries, and no percent-encoded aliases in resource paths.
pub(super) fn plain(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '%' | '?' | '#'))
}

pub(super) fn unpack(bytes: &[u8], entrypoint: Option<&str>) -> Result<Bundle, String> {
    if bytes.len() > MAX_FILE {
        return Err("delivery exceeds 64 MiB".into());
    }
    preflight(bytes)?;
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("invalid ZIP: {e}"))?;
    if zip.len() > MAX_ENTRIES || zip.has_duplicate_names() {
        return Err("ZIP has too many or duplicate entries".into());
    }
    if zip
        .has_overlapping_entries()
        .map_err(|e| format!("invalid ZIP structure: {e}"))?
    {
        return Err("ZIP contains overlapping entries".into());
    }
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    let mut files = BTreeMap::new();
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| format!("unreadable ZIP entry: {e}"))?;
        let name = std::str::from_utf8(f.name_raw())
            .map_err(|_| "ZIP names must be UTF-8")?
            .to_string();
        let dir = f.is_dir();
        let key = if dir {
            name.strip_suffix('/').unwrap_or(&name)
        } else {
            &name
        };
        if !plain(key) || !names.insert(key.to_string()) {
            return Err("ZIP contains an unsafe or duplicate path".into());
        }
        if let Some(mode) = f.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != if dir { 0o040000 } else { 0o100000 } {
                return Err("ZIP links and special files are forbidden".into());
            }
        }
        let size = f.size();
        total = total.checked_add(size).ok_or("ZIP size overflow")?;
        if size > MAX_ENTRY
            || total > MAX_EXPANDED
            || size
                > f.compressed_size()
                    .saturating_mul(200)
                    .saturating_add(1024 * 1024)
        {
            return Err("ZIP expansion limit exceeded".into());
        }
        if dir {
            if size != 0 {
                return Err("ZIP directory contains data".into());
            }
            continue;
        }
        let mut data = Vec::new();
        (&mut f)
            .take(size + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("incomplete ZIP entry: {e}"))?;
        if data.len() as u64 != size {
            return Err("incomplete ZIP entry".into());
        }
        files.insert(name, data);
    }
    // A file cannot simultaneously be a parent directory of another entry.
    for path in names.iter() {
        let mut parent = path.as_str();
        while let Some((p, _)) = parent.rsplit_once('/') {
            if files.contains_key(p) {
                return Err("ZIP file/directory collision".into());
            }
            parent = p;
        }
    }
    let html = |p: &str| {
        p.to_ascii_lowercase().ends_with(".html") || p.to_ascii_lowercase().ends_with(".htm")
    };
    let markdown = |p: &str| {
        p.to_ascii_lowercase().ends_with(".md") || p.to_ascii_lowercase().ends_with(".markdown")
    };
    // An explicit entry may be any file of the package (an HTML page, a Markdown document, a PDF…);
    // without one a page is preferred, then a README or index document.
    let entrypoint = match entrypoint {
        Some(p) if plain(p) && files.contains_key(p) => Some(p.to_string()),
        Some(_) => return Err("entrypoint must name a file in the ZIP".into()),
        None if files.contains_key("index.html") => Some("index.html".into()),
        None => files
            .keys()
            .find(|p| html(p))
            .or_else(|| {
                ["README.md", "readme.md", "index.md"]
                    .iter()
                    .find_map(|n| files.get_key_value(*n).map(|(k, _)| k))
            })
            .or_else(|| files.keys().find(|p| markdown(p)))
            .cloned(),
    };
    Ok(Bundle { entrypoint, files })
}

/// Bound central-directory allocation before the archive library parses it. Our small-file
/// limits do not need ZIP64 or split archives; accepting them could let a tiny header claim an
/// unbounded number of entries before `ZipArchive::len` can be inspected.
fn preflight(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 22 {
        return Err("incomplete ZIP".into());
    }
    let end = (bytes.len().saturating_sub(22 + 65535)..=bytes.len() - 22)
        .rev()
        .find(|&i| {
            &bytes[i..i + 4] == b"PK\x05\x06"
                && i + 22 + u16::from_le_bytes([bytes[i + 20], bytes[i + 21]]) as usize
                    == bytes.len()
        })
        .ok_or("incomplete ZIP end record")?;
    let n = |i| u16::from_le_bytes([bytes[end + i], bytes[end + i + 1]]);
    if n(4) != 0
        || n(6) != 0
        || n(8) != n(10)
        || n(10) as usize > MAX_ENTRIES
        || (end >= 20 && &bytes[end - 20..end - 16] == b"PK\x06\x07")
    {
        return Err("ZIP64, split archives and more than 1024 entries are unsupported".into());
    }
    Ok(())
}
