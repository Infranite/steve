//! Reading a library's index and installing an archive from it: fetched, checked against the
//! index's sha256, and unpacked into a directory of its own.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::index::{Location, MAX_INDEX_BYTES};

/// The largest archive the Hub downloads.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// What an archive may unpack to.
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

pub fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Steve-plugins/1")
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .build()
}

async fn get(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    steve_modules::net::check_url(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    let r = client.get(url).send().await.context("not reachable")?;
    if !r.status().is_success() {
        bail!("{} answered {}", host(url), r.status().as_u16())
    }
    Ok(r)
}

fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "the server".into())
}

/// The index file's bytes.
pub async fn read_index(client: &reqwest::Client, at: &Location) -> Result<Vec<u8>> {
    match at {
        Location::File(p) => {
            let meta = std::fs::metadata(p)
                .with_context(|| format!("no {} there", super::index::INDEX_FILE))?;
            if meta.len() > MAX_INDEX_BYTES as u64 {
                bail!("the index is larger than 1 MiB")
            }
            Ok(std::fs::read(p)?)
        }
        Location::Url(url) => {
            let mut body = get(client, url).await?.bytes_stream();
            let mut out = Vec::new();
            while let Some(chunk) = body.next().await {
                out.extend_from_slice(&chunk.context("the index was cut short")?);
                if out.len() > MAX_INDEX_BYTES {
                    bail!("the index is larger than 1 MiB")
                }
            }
            Ok(out)
        }
    }
}

/// Fetches the archive to `to` and returns its sha256 (hex).
async fn fetch(client: &reqwest::Client, at: &Location, to: &Path) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut file = tokio::fs::File::create(to).await?;
    match at {
        Location::File(p) => {
            let mut src = tokio::fs::File::open(p)
                .await
                .with_context(|| format!("no archive at {}", p.display()))?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut src, &mut buf).await?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                if size > MAX_ARCHIVE_BYTES {
                    bail!("the archive is larger than 256 MiB")
                }
                hash.update(&buf[..n]);
                file.write_all(&buf[..n]).await?;
            }
        }
        Location::Url(url) => {
            let mut body = get(client, url).await?.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.context("the download was cut short")?;
                size += chunk.len() as u64;
                if size > MAX_ARCHIVE_BYTES {
                    bail!("the archive is larger than 256 MiB")
                }
                hash.update(&chunk);
                file.write_all(&chunk).await?;
            }
        }
    }
    file.flush().await?;
    Ok(hex::encode(hash.finalize()))
}

/// Unpacks a `.tar.gz` into `dest` (which exists and is empty). Only directories and regular files
/// with paths inside `dest` are accepted: an archive with a link, a device or a path that leaves
/// is refused whole. A file is executable when the archive says so; nothing else of its mode,
/// owner or times is kept.
fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut total = 0u64;
    for (n, entry) in tar.entries().context("not a .tar.gz archive")?.enumerate() {
        if n >= MAX_ENTRIES {
            bail!("the archive has more than {MAX_ENTRIES} entries")
        }
        let mut entry = entry.context("not a .tar.gz archive")?;
        let path = entry.path()?.into_owned();
        let mut rel = PathBuf::new();
        for c in path.components() {
            match c {
                Component::Normal(p) => rel.push(p),
                Component::CurDir => {}
                _ => bail!("the archive has a path that leaves it: {}", path.display()),
            }
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(dest.join(&rel))?;
            continue;
        }
        if !kind.is_file() {
            bail!(
                "the archive has something other than files and directories: {}",
                path.display()
            )
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        total += entry.header().size()?;
        if total > MAX_UNPACKED_BYTES {
            bail!("the archive unpacks to more than 1 GiB")
        }
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .with_context(|| format!("the archive has {} twice", path.display()))?;
        // the header's size is what was counted; never more than that from the stream
        let size = entry.header().size()?;
        std::io::copy(&mut (&mut entry).take(size), &mut out)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o755 } else { 0o644 };
            out.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = executable;
    }
    Ok(())
}

/// Installs the archive at `at` as `<plugin_dir>/<version>`: downloaded beside it, checked against
/// `sha256`, unpacked, and moved into place only when `entry` is an executable file in it. An
/// earlier directory of the same version is replaced; other versions are left for the caller.
pub async fn install(
    client: &reqwest::Client,
    at: &Location,
    sha256: &str,
    plugin_dir: &Path,
    version: &str,
    entry: &str,
) -> Result<PathBuf> {
    std::fs::create_dir_all(plugin_dir)?;
    let tmp = plugin_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    let done = async {
        let archive = tmp.join("archive.tar.gz");
        let got = fetch(client, at, &archive).await?;
        if !got.eq_ignore_ascii_case(sha256) {
            bail!("the archive is not the one the index names (sha256 {got})")
        }
        let root = tmp.join("root");
        std::fs::create_dir_all(&root)?;
        let (a, r) = (archive.clone(), root.clone());
        tokio::task::spawn_blocking(move || unpack(&a, &r)).await??;
        let exe = root.join(entry);
        let meta = std::fs::symlink_metadata(&exe)
            .with_context(|| format!("the archive has no {entry}"))?;
        if !meta.is_file() {
            bail!("{entry} in the archive is not a file")
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
        }
        let dest = plugin_dir.join(version);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&root, &dest)?;
        Ok(dest)
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    done
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `.tar.gz` of `(path, mode, bytes)` files; returns its sha256.
    pub(crate) fn archive(to: &Path, files: &[(&str, u32, &[u8])]) -> String {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, mode, bytes) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(*mode);
            h.set_cksum();
            tar.append_data(&mut h, path, *bytes).unwrap();
        }
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        std::fs::write(to, &bytes).unwrap();
        hex::encode(Sha256::digest(&bytes))
    }

    #[tokio::test]
    async fn an_archive_is_installed_only_when_it_is_the_one_named() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("p.tar.gz");
        let sha = archive(
            &src,
            &[("bin/run", 0o755, b"#!/bin/sh\n"), ("README", 0o644, b"hi")],
        );
        let plugin_dir = dir.path().join("installed/p");
        let c = client().unwrap();
        let at = Location::File(src);

        let wrong = install(&c, &at, &"0".repeat(64), &plugin_dir, "1.0.0", "bin/run").await;
        assert!(
            wrong
                .unwrap_err()
                .to_string()
                .contains("not the one the index names")
        );
        let missing = install(&c, &at, &sha, &plugin_dir, "1.0.0", "bin/other").await;
        assert!(missing.unwrap_err().to_string().contains("no bin/other"));
        // nothing is left behind by a refused install
        assert_eq!(std::fs::read_dir(&plugin_dir).unwrap().count(), 0);

        let dest = install(&c, &at, &sha, &plugin_dir, "1.0.0", "bin/run")
            .await
            .unwrap();
        assert_eq!(dest, plugin_dir.join("1.0.0"));
        assert_eq!(std::fs::read(dest.join("README")).unwrap(), b"hi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| {
                std::fs::metadata(dest.join(p))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!((mode("bin/run"), mode("README")), (0o755, 0o644));
        }
        // the same version again replaces it
        install(&c, &at, &sha, &plugin_dir, "1.0.0", "bin/run")
            .await
            .unwrap();
    }

    #[test]
    fn an_archive_with_a_link_or_a_path_outside_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        type Tar = tar::Builder<flate2::write::GzEncoder<Vec<u8>>>;
        let make = |f: &dyn Fn(&mut Tar)| {
            let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::fast(),
            ));
            f(&mut tar);
            let p = dir.path().join(format!("{}.tar.gz", uuid::Uuid::new_v4()));
            std::fs::write(&p, tar.into_inner().unwrap().finish().unwrap()).unwrap();
            p
        };
        let link = make(&|tar| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            tar.append_link(&mut h, "bin/run", "/bin/sh").unwrap();
        });
        let outside = make(&|tar| {
            let mut h = tar::Header::new_gnu();
            let name = b"../escape";
            h.as_old_mut().name[..name.len()].copy_from_slice(name);
            h.set_size(1);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append(&h, &b"x"[..]).unwrap();
        });
        for (archive, what) in [(link, "other than files"), (outside, "leaves it")] {
            let dest = dir.path().join(format!("out-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dest).unwrap();
            let e = unpack(&archive, &dest).unwrap_err().to_string();
            assert!(e.contains(what), "{e}");
        }
        assert!(!dir.path().join("escape").exists());
    }
}
