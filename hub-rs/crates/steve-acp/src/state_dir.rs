//! Ambient environment and filesystem interpretation of harness declaration data.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use steve_harness_spec::{Login, StateDir};

/// Runtime interpretation of a pure StateDir declaration against the Node's environment.
pub trait StateDirRuntime {
    fn owner_source(&self, user_home: &Path) -> Option<PathBuf>;
    fn node_store(
        &self,
        user_home: &Path,
        node_var: impl Fn(&str) -> Option<OsString>,
    ) -> Option<(OsString, OsString, PathBuf)>;
    fn ambient_dirs(&self, user_home: &Path) -> Vec<PathBuf>;
    fn isolation_env_for(
        &self,
        dir: &Path,
        user_home: Option<&Path>,
        import_owner: bool,
    ) -> Vec<(OsString, OsString)>;
}

fn absolutize_env_path(p: PathBuf, user_home: &Path) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::path::absolute(&p).unwrap_or_else(|_| user_home.join(p))
    }
}

impl StateDirRuntime for StateDir {
    /// Where the owner's ambient configuration files live when importing into a per-principal
    /// state directory.
    fn owner_source(&self, user_home: &Path) -> Option<PathBuf> {
        if !self.env.is_empty()
            && let Some(val) = std::env::var_os(&self.env).filter(|v| !v.is_empty())
        {
            let p = absolutize_env_path(PathBuf::from(val), user_home);
            return Some(if self.home_override && !self.default.is_empty() {
                p.join(&self.default)
            } else if let Some(sub) = self.import_into.as_deref().filter(|s| !s.is_empty()) {
                p.join(sub)
            } else {
                p
            });
        }
        (!self.default.is_empty()).then(|| user_home.join(&self.default))
    }

    /// The Node's own credential store for [`Login::store_env`]: the variable, the value a session
    /// gets (the Node's own value of it; else the directory the Node's harness uses when `env`
    /// moves it there; else empty, the harness's default place) and that store's directory.
    /// `node_var` reads the Node's environment for the harness. `None` without `store_env`.
    fn node_store(
        &self,
        user_home: &Path,
        node_var: impl Fn(&str) -> Option<OsString>,
    ) -> Option<(OsString, OsString, PathBuf)> {
        let var = self.login.store_env.as_str();
        if !Login::var_ok(var) {
            return None;
        }
        let set = |name: &str| node_var(name).filter(|v| !v.is_empty());
        let value = match node_var(var) {
            Some(v) => v,
            // as the Node's harness has it: the harness names its store after this text
            None => (!self.env.is_empty())
                .then(|| set(&self.env))
                .flatten()
                .unwrap_or_default(),
        };
        let dir = if value.is_empty() {
            user_home.join(&self.default)
        } else {
            absolutize_env_path(PathBuf::from(&value), user_home)
        };
        Some((var.into(), value, dir))
    }

    /// Directories the harness writes in ambient (non-isolated) mode—used by read-only sandboxes
    /// so the adapter can write its own session/config state without ever opening bare `$HOME`.
    fn ambient_dirs(&self, user_home: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !self.env.is_empty()
            && let Some(val) = std::env::var_os(&self.env).filter(|v| !v.is_empty())
        {
            let p = absolutize_env_path(PathBuf::from(val), user_home);
            if self.home_override {
                if !self.default.is_empty() {
                    out.push(p.join(&self.default));
                }
            } else {
                out.push(p);
            }
        } else if !self.default.is_empty() {
            out.push(user_home.join(&self.default));
        }
        if self.xdg {
            for var in [
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
            ] {
                if let Some(val) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                    let base = absolutize_env_path(PathBuf::from(val), user_home);
                    if let Some(leaf) = Path::new(&self.default)
                        .file_name()
                        .filter(|n| !n.is_empty())
                    {
                        out.push(base.join(leaf));
                    }
                }
            }
        }
        for extra in &self.extra_dirs {
            if !extra.is_empty() {
                out.push(user_home.join(extra));
            }
        }
        out
    }

    /// Environment variables that move this harness's state into `dir`, preserving the owner's
    /// XDG git config when `XDG_CONFIG_HOME` is redirected for an owner session.
    fn isolation_env_for(
        &self,
        dir: &Path,
        user_home: Option<&Path>,
        import_owner: bool,
    ) -> Vec<(OsString, OsString)> {
        let mut out = Vec::new();
        if !self.env.is_empty() {
            out.push((self.env.clone().into(), dir.as_os_str().to_owned()));
        }
        if self.xdg {
            if import_owner
                && std::env::var_os("GIT_CONFIG_GLOBAL").is_none_or(|v| v.is_empty())
                && let Some(home) = user_home
                && !home.join(".gitconfig").exists()
            {
                let xdg_cfg = std::env::var_os("XDG_CONFIG_HOME")
                    .filter(|v| !v.is_empty())
                    .map(|v| absolutize_env_path(PathBuf::from(v), home))
                    .unwrap_or_else(|| home.join(".config"));
                let git_cfg = xdg_cfg.join("git/config");
                if git_cfg.is_file() {
                    out.push(("GIT_CONFIG_GLOBAL".into(), git_cfg.into_os_string()));
                }
            }
            for (var, sub) in [
                ("XDG_DATA_HOME", "xdg/data"),
                ("XDG_CONFIG_HOME", "xdg/config"),
                ("XDG_STATE_HOME", "xdg/state"),
                ("XDG_CACHE_HOME", "xdg/cache"),
            ] {
                out.push((var.into(), dir.join(sub).into_os_string()));
            }
        }
        out
    }
}
