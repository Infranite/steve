//! Secret injection contract types and decryption for Node-isolated execution.
//!
//! Secret-injected commands execute exclusively on an isolated Node (`UseNodes`), never inside the
//! Hub process. [`Vault::injection_values`] is one of the three decryption sites.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use super::crypto::open_field;
use super::secret::SecretBytes;
use super::{Vault, VaultError, VaultResult};
use crate::ApiError;
use steve_store::vault_items::VaultItems;

/// `400` when `cwd` leaves the session's workspace, verbatim.
pub const CWD_OUTSIDE: &str = "目录不在这个会话的工作区里";
pub const DEFAULT_TIMEOUT_S: u64 = 120;
pub const MAX_TIMEOUT_S: u64 = 600;
/// Bytes kept per stream.
pub const OUTPUT_CAP: usize = 64 * 1024;

static VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").expect("var pattern"));

/// A valid variable name (also a safe file name).
pub fn valid_var(v: &str) -> bool {
    VAR.is_match(v)
}

/// How one field reaches the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Env,
    /// The variable holds the path of a 0600 file with the value (created on the Node).
    File,
    Stdin,
}

impl How {
    /// `secret_uses.mode`.
    pub fn mode(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Stdin => "stdin",
        }
    }
}

/// One injected field. `var` is empty for stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub var: String,
    pub label: String,
    pub how: How,
}

/// What to run on the session's Node. Holds no secret value.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The `secret_uses` id.
    pub use_id: String,
    /// Display path shown in approval drafts (resolved relative to the Node workspace).
    pub cwd: PathBuf,
    pub command: String,
    pub injections: Vec<Injection>,
    pub timeout: Duration,
}

/// What came back from the Node execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` when killed by a signal (timeout) or never started.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// The command could not be started or was refused: the reason.
    pub error: Option<String>,
}

impl RunOutput {
    /// The number shown to the agent and the user: the exit code, or -1.
    pub fn code(&self) -> i32 {
        self.exit_code.unwrap_or(-1)
    }
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }
    /// stdout, then stderr under a marker, then any error/timeout note.
    pub fn combined(&self) -> String {
        let mut s = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("[stderr]\n");
            s.push_str(&self.stderr);
        }
        if let Some(e) = &self.error {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&format!("[steve] {e}"));
        }
        s
    }
}

impl Vault {
    /// The values of `labels` of an active item of `ws`, for one injection. Secret fields are
    /// decrypted here; non-secret ones are read as they are.
    pub(crate) fn injection_values(
        &self,
        ws: &str,
        item_id: &str,
        labels: &[String],
    ) -> VaultResult<Vec<(String, SecretBytes)>> {
        let (item, rows) = self.read(|r| {
            let mut repo = VaultItems(r);
            let item = repo
                .item(item_id)?
                .filter(|i| i.workspace_id == ws && i.status == "active")
                .ok_or_else(|| ApiError::not_found("条目"))?;
            Ok((item, repo.fields(item_id)?))
        })?;
        let mut dek = None;
        let mut out = Vec::new();
        for label in labels {
            if out.iter().any(|(l, _): &(String, SecretBytes)| l == label) {
                continue;
            }
            let f = rows
                .iter()
                .find(|f| &f.label == label)
                .ok_or_else(|| ApiError::not_found("字段"))?;
            let v = if f.secret {
                if dek.is_none() {
                    dek =
                        Some(self.dek_of(&item)?.ok_or_else(|| {
                            VaultError::internal("vault item without a data key")
                        })?);
                }
                let ct = f
                    .value_ct
                    .as_deref()
                    .ok_or_else(|| VaultError::internal("secret field without a value"))?;
                let dek = dek
                    .as_ref()
                    .ok_or_else(|| VaultError::internal("no data key"))?;
                open_field(dek, ws, item_id, &f.id, ct)?
            } else {
                SecretBytes::new(f.value_plain.clone().unwrap_or_default().into_bytes())
            };
            out.push((label.clone(), v));
        }
        Ok(out)
    }
}
