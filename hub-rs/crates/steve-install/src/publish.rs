//! `steve login` and `steve publish`: a person's own preview links. The CLI signs in to a Hub as a
//! user access client (`POST /auth/token`, kind `cli`), keeps only that client's credential, and
//! uploads a file, or a folder packed as a ZIP, to `POST /api/file-deliveries`. The Hub applies the
//! same limits and immutable versions as a Bot's delivery; nothing here reads the Hub's own files.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Hub refuses larger originals; checked here first so a folder fails before uploading.
pub const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 1024;
/// Never packed from a folder: version control and the Finder's own files.
const SKIPPED: &[&str] = &[".git", ".DS_Store"];

/// What `steve login` keeps: the Hub, who signed in and the client's own credential (not the token).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Login {
    pub hub: String,
    pub user: String,
    pub credential: String,
}

/// `$XDG_CONFIG_HOME/steve/cli.json`, else `~/.config/steve/cli.json`.
pub fn login_file() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => crate::common::user_home()?.join(".config"),
    };
    Ok(base.join("steve").join("cli.json"))
}

pub fn saved_login(file: &Path) -> Result<Option<Login>> {
    match fs::read(file) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("{} is unreadable", file.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_login(file: &Path, login: &Login) -> Result<()> {
    let dir = file.parent().context("login file has no directory")?;
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let temp = dir.join(format!(".cli.json.{}", std::process::id()));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        f.write_all(&serde_json::to_vec_pretty(login)?)?;
        f.sync_all()?;
    }
    fs::rename(&temp, file)?;
    Ok(())
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(format!(
            "steve-cli/{}",
            crate::build_info()["version"].as_str().unwrap_or("dev")
        ))
        .build()?)
}

/// The Hub's answer, or its `{"error"}` text.
fn answer(r: reqwest::blocking::Response) -> Result<Value> {
    let status = r.status();
    let body: Value = r.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let why = body["error"].as_str().unwrap_or("no details");
        bail!("the Hub answered {status}: {why}");
    }
    Ok(body)
}

pub fn normalize_hub(hub: &str) -> Result<String> {
    let url = url::Url::parse(hub.trim()).with_context(|| format!("{hub} is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("the Hub address must be an http(s) URL");
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Sign in with a user's name and one of their access tokens; returns the new client credential.
pub fn login(hub: &str, user: &str, token: &str) -> Result<Login> {
    let hub = normalize_hub(hub)?;
    let r = client(Duration::from_secs(30))?
        .post(format!("{hub}/auth/token"))
        .json(&json!({"user": user, "token": token, "client": {
            "kind": "cli", "name": "steve publish", "platform": std::env::consts::OS,
            "version": crate::build_info()["version"],
        }}))
        .send()
        .with_context(|| format!("cannot reach {hub}"))?;
    let v = answer(r)?;
    let credential = v["credential"]
        .as_str()
        .context("the Hub did not return a credential")?
        .to_string();
    Ok(Login {
        hub,
        user: v["user"]["name"].as_str().unwrap_or(user).to_string(),
        credential,
    })
}

/// A secret from the terminal without echo, else one line of standard input.
pub fn read_secret(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.is_terminal() {
        eprint!("{prompt}");
        std::io::stderr().flush().ok();
        let fd = libc::STDIN_FILENO;
        let mut old: libc::termios = unsafe { std::mem::zeroed() };
        let saved = unsafe { libc::tcgetattr(fd, &mut old) } == 0;
        if saved {
            let mut quiet = old;
            quiet.c_lflag &= !libc::ECHO;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
        }
        let read = stdin.read_line(&mut line);
        if saved {
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
        }
        eprintln!();
        read?;
    } else {
        stdin.read_line(&mut line)?;
    }
    let secret = line.trim().to_string();
    if secret.is_empty() {
        bail!("no access token given");
    }
    Ok(secret)
}

/// The Hub's rule for a path inside a package: plain UTF-8 parts, none empty, `.` or `..`, and no
/// control characters, `\`, `:`, `%`, `?` or `#`.
fn plain(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '%' | '?' | '#'))
}

/// Every regular file below `root`, as package paths in a stable order. Links and special files
/// are refused, not followed: what is published is exactly what is in the folder.
pub fn package_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e
                .file_name()
                .into_string()
                .map_err(|n| anyhow::anyhow!("{} is not a UTF-8 name", Path::new(&n).display()))?;
            if SKIPPED.contains(&name.as_str()) {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if !plain(&rel) {
                bail!(
                    "{rel}: a published path may not contain \\ : % ? # or control characters; rename it"
                );
            }
            let kind = e.file_type()?;
            if kind.is_dir() {
                stack.push((e.path(), rel));
            } else if kind.is_file() {
                out.push((rel, e.path()));
            } else {
                bail!("{rel}: links and special files are not published; copy the file in instead");
            }
            if out.len() > MAX_ENTRIES {
                bail!("a published folder holds at most {MAX_ENTRIES} files");
            }
        }
    }
    out.sort();
    if out.is_empty() {
        bail!("{} has no files to publish", root.display());
    }
    Ok(out)
}

/// The folder as a ZIP (deflated), refusing what the Hub would refuse.
pub fn zip_folder(root: &Path) -> Result<Vec<u8>> {
    let files = package_files(root)?;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut total = 0u64;
    for (rel, path) in files {
        let mut data = Vec::new();
        fs::File::open(&path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut data)?;
        total += data.len() as u64;
        if data.len() as u64 > 32 * 1024 * 1024 || total > 128 * 1024 * 1024 {
            bail!("{rel}: a published folder holds files of at most 32 MiB, 128 MiB together");
        }
        w.start_file(rel.as_str(), options)?;
        w.write_all(&data)?;
    }
    let bytes = w.finish()?.into_inner();
    if bytes.len() as u64 > MAX_FILE {
        bail!("the packed folder is larger than 64 MiB");
    }
    Ok(bytes)
}

pub struct Publish<'a> {
    pub path: &'a Path,
    pub title: Option<&'a str>,
    pub entry: Option<&'a str>,
    pub interactive: bool,
    pub workspace: Option<&'a str>,
}

/// Upload and return the Hub's delivery metadata (`url` is the preview link).
pub fn publish(login: &Login, p: &Publish) -> Result<Value> {
    let meta = fs::symlink_metadata(p.path)
        .with_context(|| format!("{} does not exist", p.path.display()))?;
    let base = p
        .path
        .canonicalize()?
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .context("the path has no usable name")?;
    let (name, bytes) = if meta.is_dir() {
        (format!("{base}.zip"), zip_folder(p.path)?)
    } else if meta.is_file() {
        if meta.len() > MAX_FILE {
            bail!("{} is larger than 64 MiB", p.path.display());
        }
        if p.entry.is_some() && !base.to_ascii_lowercase().ends_with(".zip") {
            bail!("--entry selects a file inside a folder or ZIP");
        }
        (base, fs::read(p.path)?)
    } else {
        bail!(
            "{}: only a file or a folder can be published",
            p.path.display()
        );
    };
    if !plain(&name) {
        bail!("{name}: rename it without \\ : % ? # or control characters");
    }
    let http = client(Duration::from_secs(300))?;
    let auth = format!("Bearer {}", login.credential);
    let workspace = match p.workspace {
        Some(w) => w.to_string(),
        None => {
            let me = answer(
                http.get(format!("{}/api/me", login.hub))
                    .header("authorization", &auth)
                    .send()
                    .with_context(|| format!("cannot reach {}", login.hub))?,
            )?;
            me["workspace"]
                .as_str()
                .context("the Hub named no workspace; pass --workspace")?
                .to_string()
        }
    };
    let mut url = url::Url::parse(&format!("{}/api/file-deliveries", login.hub))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("name", &name);
        if let Some(t) = p.title {
            q.append_pair("title", t);
        }
        if let Some(e) = p.entry {
            q.append_pair("entrypoint", e);
        }
        if p.interactive {
            q.append_pair("interactive", "1");
        }
    }
    answer(
        http.post(url)
            .header("authorization", &auth)
            .header("x-steve-ws", &workspace)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .with_context(|| format!("cannot reach {}", login.hub))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_pack_regular_files_and_refuse_links_and_unsafe_names() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("site");
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("index.html"), "<p>hi</p>").unwrap();
        fs::write(root.join("assets/app.js"), "1").unwrap();
        fs::write(root.join(".git/HEAD"), "ref").unwrap();
        fs::write(root.join(".DS_Store"), "x").unwrap();
        let names: Vec<_> = package_files(&root)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["assets/app.js", "index.html"]);
        let bytes = zip_folder(&root).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut html = String::new();
        z.by_name("index.html")
            .unwrap()
            .read_to_string(&mut html)
            .unwrap();
        assert_eq!(html, "<p>hi</p>");
        std::os::unix::fs::symlink("/etc/hosts", root.join("hosts")).unwrap();
        assert!(zip_folder(&root).is_err(), "links are refused");
        fs::remove_file(root.join("hosts")).unwrap();
        fs::write(root.join("50%.txt"), "x").unwrap();
        assert!(
            package_files(&root).is_err(),
            "names the Hub would refuse fail first"
        );
        assert!(package_files(d.path().join("missing").as_path()).is_err());
    }

    #[test]
    fn logins_are_kept_privately_and_hub_addresses_are_checked() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("steve/cli.json");
        assert_eq!(saved_login(&file).unwrap(), None);
        let login = Login {
            hub: "https://steve.example".into(),
            user: "me".into(),
            credential: "c.s".into(),
        };
        save_login(&file, &login).unwrap();
        assert_eq!(saved_login(&file).unwrap(), Some(login));
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            normalize_hub("https://steve.example/").unwrap(),
            "https://steve.example"
        );
        assert!(normalize_hub("file:///etc").is_err());
        assert!(normalize_hub("steve.example").is_err());
    }
}
