//! The secrets of one command ([`steve_node_api::ExecSpec::secrets`]): taken from the Hub
//! ([`HubSecrets::take`]) under the command's operation id right before it runs, once per secret,
//! and kept in memory only. `File` writes the value to `<home>/secrets/<op>/<VAR>` (directory
//! 0700, file 0600), removed when the command ends and, for what a crash left, when the Node
//! opens.
//!
//! A secret id is what the Hub names the value by (the vault's field label).

use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steve_node_api::*;
use zeroize::Zeroizing;

/// `<home>/secrets`.
fn root(home: &Path) -> PathBuf {
    home.join("secrets")
}

/// A variable name that is also a safe file name.
fn valid_var(v: &str) -> bool {
    let mut cs = v.chars();
    v.len() <= 64
        && cs
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What a command's secrets may be: env and file variables named well and once, one stdin at
/// most, no header (that is a push's).
pub(crate) fn check(refs: &[SecretRef], env: &[(String, String)]) -> ApiResult<()> {
    let bad = |m: &str| Err(ApiError::new(ErrorCode::Invalid, m.to_string()));
    let mut vars: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    let mut stdin = false;
    for r in refs {
        if r.id.is_empty() || r.id.len() > 256 {
            return bad("a secret without a usable id");
        }
        match &r.how {
            SecretDelivery::Env { name } | SecretDelivery::File { env: name } => {
                if !valid_var(name) {
                    return bad("a bad secret variable name");
                }
                if vars.contains(&name.as_str()) {
                    return bad("a variable given twice");
                }
                vars.push(name);
            }
            SecretDelivery::Stdin => {
                if stdin {
                    return bad("two secrets for stdin");
                }
                stdin = true;
            }
            SecretDelivery::Header { .. } => return bad("a header is for a push only"),
        }
    }
    Ok(())
}

/// Whether any of `refs` writes a file.
pub(crate) fn has_file(refs: &[SecretRef]) -> bool {
    refs.iter()
        .any(|r| matches!(r.how, SecretDelivery::File { .. }))
}

/// Removes what a crash left (at open: nothing runs yet).
pub(crate) fn cleanup(home: &Path) {
    let r = root(home);
    if r.exists()
        && let Err(e) = std::fs::remove_dir_all(&r)
    {
        tracing::warn!("node: could not remove leftover secret files: {e}");
    }
}

/// The temp directory of one command; removed on drop, whatever happens.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    fn create(home: &Path, op: &OperationId) -> std::io::Result<TempDir> {
        use sha2::Digest as _;
        let r = root(home);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&r)?;
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o700))?;
        let name = hex::encode(sha2::Sha256::digest(op.as_str().as_bytes()));
        let dir = r.join(&name[..24]);
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(TempDir(dir))
    }

    fn write(&self, var: &str, value: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;
        let p = self.0.join(var);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)?;
        f.write_all(value)?;
        f.sync_all()?;
        Ok(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("node: could not remove a secret temp dir: {e}");
        }
    }
}

/// A command's secrets, ready to hand to it.
pub(crate) struct Given {
    pub env: Vec<(String, Zeroizing<Vec<u8>>)>,
    pub stdin: Option<Zeroizing<Vec<u8>>>,
    /// Kept until the command ended.
    pub temp: Option<TempDir>,
}

/// Takes `refs` for `op` from the Hub, each distinct id once, and puts them where they go. `Err`:
/// why the command does not run (no value of it was used).
pub(crate) async fn take(
    hub: Option<Arc<dyn HubSecrets>>,
    home: &Path,
    op: &OperationId,
    refs: &[SecretRef],
) -> Result<Given, String> {
    let mut given = Given {
        env: vec![],
        stdin: None,
        temp: None,
    };
    if refs.is_empty() {
        return Ok(given);
    }
    let Some(hub) = hub else {
        return Err("no secrets from the Hub here".into());
    };
    let mut values: HashMap<String, Zeroizing<Vec<u8>>> = HashMap::new();
    for r in refs {
        if values.contains_key(&r.id) {
            continue;
        }
        let v = hub
            .take(op.clone(), r.id.clone())
            .await
            .map_err(|e| format!("secret {}: {}", r.id, e))?;
        values.insert(r.id.clone(), Zeroizing::new(v.expose().to_vec()));
    }
    if has_file(refs) {
        given.temp = Some(
            TempDir::create(home, op)
                .map_err(|e| format!("could not create the temp directory: {e}"))?,
        );
    }
    for r in refs {
        let v = &values[&r.id];
        match &r.how {
            SecretDelivery::Env { name } => given.env.push((name.clone(), v.clone())),
            SecretDelivery::File { env } => {
                let t = given.temp.as_ref().expect("made above");
                let p = t
                    .write(env, v)
                    .map_err(|e| format!("could not write the temp file: {e}"))?;
                given.env.push((
                    env.clone(),
                    Zeroizing::new(p.into_os_string().into_encoded_bytes()),
                ));
            }
            SecretDelivery::Stdin => given.stdin = Some(v.clone()),
            SecretDelivery::Header { .. } => return Err("a header is for a push only".into()),
        }
    }
    Ok(given)
}

/// A push's credential ([`SecretDelivery::Header`]), taken from the Hub for `op` once: the header
/// line `<name>: <value>` for git. `Err`: why the push does not run.
pub(crate) async fn header(
    hub: Option<Arc<dyn HubSecrets>>,
    op: &OperationId,
    r: &SecretRef,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let SecretDelivery::Header { name } = &r.how else {
        return Err("a push credential goes as a header".into());
    };
    let Some(hub) = hub else {
        return Err("no secrets from the Hub here".into());
    };
    let v = hub
        .take(op.clone(), r.id.clone())
        .await
        .map_err(|e| format!("secret {}: {}", r.id, e))?;
    let value = Zeroizing::new(v.expose().to_vec());
    drop(v);
    // one header line: a value that would start another is refused
    if value.is_empty() || value.iter().any(|b| matches!(b, b'\n' | b'\r' | 0)) {
        return Err(format!("secret {}: not a header value", r.id));
    }
    let mut line = Zeroizing::new(Vec::with_capacity(name.len() + 2 + value.len()));
    line.extend_from_slice(name.as_bytes());
    line.extend_from_slice(b": ");
    line.extend_from_slice(&value);
    Ok(line)
}
