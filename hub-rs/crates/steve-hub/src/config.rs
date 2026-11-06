//! Runtime configuration (config.ts). Everything comes from the environment with the TS defaults;
//! secrets are read from `*_FILE` paths, never from argv.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Where the Hub listens: every host in `STEVE_HOST` (comma-separated) on one port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// Never empty; IPv6 literals without brackets.
    pub hosts: Vec<String>,
    pub port: u16,
}

impl Listen {
    /// `STEVE_HOST`: a comma-separated list, default `127.0.0.1`. A wildcard (`0.0.0.0`, `::`)
    /// exposes the Hub to the whole network and needs `STEVE_ALLOW_ANY_HOST=1`.
    pub fn parse(raw: &str, port: u16, allow_any: bool) -> Result<Self, ConfigError> {
        let mut hosts: Vec<String> = Vec::new();
        for h in raw.split(',').map(str::trim).filter(|h| !h.is_empty()) {
            let h = h
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(h);
            if !hosts.iter().any(|x| x == h) {
                hosts.push(h.to_string());
            }
        }
        if hosts.is_empty() {
            hosts.push("127.0.0.1".into());
        }
        let listen = Self { hosts, port };
        if listen.any() && !allow_any {
            return Err(ConfigError::Invalid {
                var: "STEVE_HOST",
                msg: "0.0.0.0 / :: listens on every network; list the addresses instead \
                      (e.g. 127.0.0.1,100.64.0.2) or set STEVE_ALLOW_ANY_HOST=1"
                    .into(),
            });
        }
        Ok(listen)
    }

    /// Whether some host is a wildcard address.
    pub fn any(&self) -> bool {
        self.hosts.iter().any(|h| {
            h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_unspecified())
        })
    }

    /// The value to hand a child Hub as `STEVE_HOST`.
    pub fn env_value(&self) -> String {
        self.hosts.join(",")
    }

    /// The host (URL form) a process on this machine uses to reach the Hub: a loopback host if
    /// one is listed, else the first one; a wildcard is reached over loopback.
    pub fn local_host(&self) -> String {
        let loopback = |h: &str| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        };
        let h = self
            .hosts
            .iter()
            .find(|h| loopback(h))
            .unwrap_or(&self.hosts[0]);
        match h.as_str() {
            "0.0.0.0" => "127.0.0.1".into(),
            "::" => "[::1]".into(),
            h if h.contains(':') => format!("[{h}]"),
            h => h.into(),
        }
    }

    /// `http://host:port` for every host, for the startup line.
    pub fn urls(&self) -> String {
        self.hosts
            .iter()
            .map(|h| match h.contains(':') {
                true => format!("http://[{h}]:{}", self.port),
                false => format!("http://{h}:{}", self.port),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Bind every host; fails (naming the address) if any one cannot be bound.
    pub async fn bind(&self) -> std::io::Result<Vec<tokio::net::TcpListener>> {
        let mut out = Vec::with_capacity(self.hosts.len());
        for h in &self.hosts {
            let l = tokio::net::TcpListener::bind((h.as_str(), self.port))
                .await
                .map_err(|e| {
                    std::io::Error::new(e.kind(), format!("listen on {h}:{}: {e}", self.port))
                })?;
            out.push(l);
        }
        Ok(out)
    }
}

/// OIDC client settings; all unset means OIDC login is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Oidc {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

/// Model lists for Steve's own `claude -p` calls, tried in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Models {
    pub fast: Vec<String>,
    pub smart: Vec<String>,
}

/// The whole Hub configuration, read once at boot.
#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub worktrees: PathBuf,
    pub scratch: PathBuf,
    pub listen: Listen,
    /// Primary public origin, trailing `/` stripped; decides the cookie `Secure` flag.
    pub public_url: String,
    /// Every origin Steve is served on; OIDC login returns to the one the browser used.
    pub public_urls: Vec<String>,
    pub web_dir: PathBuf,
    pub oidc: Oidc,
    /// `/auth/dev` from loopback, for local development only.
    pub dev_login: bool,
    /// Don't resume interrupted turns or send held prompts at boot (set when running on a copy of a real database).
    pub no_resume: bool,
    /// `STEVE_TAKEOVER_CONTINUES`: how often Steve continues a stopped agent before the user hears of it
    /// (`None`: the core's default).
    pub takeover_continues: Option<u32>,
    /// `STEVE_DISK_FLOOR_GB`, `STEVE_DISK_LOW_GB`, `STEVE_WORKTREE_IDLE_HOURS` (task workspaces).
    pub disk: steve_core::workspace::DiskPolicy,
    /// `STEVE_GH`: the GitHub CLI that follows Release builds (`None`: looked up by the core).
    pub gh: Option<PathBuf>,
    pub claude_executable: String,
    /// `CODEX_EXECUTABLE`: `codex exec` for Steve's judgments when they run on Codex.
    pub codex_executable: String,
    /// Adapter entry point per harness id; a `.js` path runs with `node`.
    pub adapters: BTreeMap<String, String>,
    pub models: Models,
    /// Model for bots that don't pick one, per harness.
    pub default_models: BTreeMap<String, String>,
    pub idle_release_ms: u64,
    /// Read but unused, as in TS: the prefix comes from the workspace row.
    pub item_prefix: String,
    pub version: String,
    /// Steve's own repository (`STEVE_REPO`): seed project, items about Steve itself, release repo.
    pub steve_repo: PathBuf,
    pub debug_route: bool,
    pub codex_home: PathBuf,
    /// Node used to run `.js` adapters (TS used its own `process.execPath`).
    pub node: String,
    /// Where knowledge is stored and found (`STEVE_KB_STORE`): `None` = the Hub's own index only.
    pub openviking: Option<steve_knowledge::openviking::OpenVikingConfig>,
}

/// Errors that stop the Hub at boot, like the TS module-level throws.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var}: {msg}")]
    Invalid { var: &'static str, msg: String },
    #[error("cannot read {var} file {path}: {source}")]
    SecretFile {
        var: &'static str,
        path: String,
        source: std::io::Error,
    },
    #[error("cannot create {path}: {source}")]
    Dir {
        path: String,
        source: std::io::Error,
    },
}

/// The user's home directory as Node's `os.homedir()` sees it (`$HOME` first).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `path.resolve`: absolute against the cwd, `.`/`..` folded lexically.
pub fn resolve(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// JS `Number(s)` for config values: trimmed, empty → 0, otherwise a decimal (or 0x/0o/0b) number.
pub fn js_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    let radix = |p: &str, r: u32| {
        u64::from_str_radix(p, r)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN)
    };
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return radix(h, 16);
    }
    if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return radix(o, 8);
    }
    if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return radix(b, 2);
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Rust accepts "inf"/"nan" and friends; JS does not.
    if t.chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

fn npm_platform() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        a => a,
    };
    (os, arch)
}

/// `npm root -g`, or the Homebrew default when npm is missing.
fn npm_root() -> String {
    // npm's global prefix is the directory above `node`'s bin unless an npmrc moves it; when the adapter
    // is installed there, that is what `npm root -g` would print, without spawning npm at boot
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("node"))
        .find(|n| n.is_file());
    if let Some(node) = on_path
        && let Some(root) = node
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("lib/node_modules"))
        && root.join("@agentclientprotocol/claude-agent-acp").exists()
    {
        return root.to_string_lossy().into_owned();
    }
    std::process::Command::new("npm")
        .args(["root", "-g"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/opt/homebrew/lib/node_modules".into())
}

/// codex-acp's bin is a Node wrapper that spawnSyncs a native binary; killing the wrapper would orphan it,
/// so Steve runs the platform binary directly when it is installed.
fn codex_adapter(root: &str) -> String {
    let pkg = Path::new(root).join("@zed-industries/codex-acp");
    let (os, arch) = npm_platform();
    let native = pkg
        .join("node_modules/@zed-industries")
        .join(format!("codex-acp-{os}-{arch}"))
        .join("bin/codex-acp");
    if native.exists() {
        native.to_string_lossy().into_owned()
    } else {
        pkg.join("bin/codex-acp.js").to_string_lossy().into_owned()
    }
}

fn strip_slash(s: &str) -> String {
    s.strip_suffix('/').unwrap_or(s).to_string()
}

impl Config {
    /// Reads the process environment and creates `data/ worktrees/ scratch/` like config.ts does at import.
    pub fn from_env() -> Result<Self, ConfigError> {
        let cfg = Self::from_vars(|k| std::env::var(k).ok())?;
        for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).map_err(|source| ConfigError::Dir {
                path: d.display().to_string(),
                source,
            })?;
        }
        Ok(cfg)
    }

    /// Builds the config from any variable source (tests pass a map); touches no directories.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let env = |k: &str, d: &str| get(k).unwrap_or_else(|| d.to_string());
        let home_default = home_dir().join(".steve");
        let home = resolve(Path::new(
            &get("STEVE_HOME").unwrap_or_else(|| home_default.to_string_lossy().into_owned()),
        ));

        let port_raw = env("STEVE_PORT", "8440");
        let port_n = js_number(&port_raw);
        if !(0.0..=65535.0).contains(&port_n) || port_n.fract() != 0.0 {
            return Err(ConfigError::Invalid {
                var: "STEVE_PORT",
                msg: format!("not a port: {port_raw}"),
            });
        }

        let public_raw = env("STEVE_PUBLIC_URL", "http://127.0.0.1:8440");
        let public_url = strip_slash(&public_raw);
        let public_urls = get("STEVE_PUBLIC_URLS")
            .unwrap_or_else(|| public_raw.clone())
            .split(',')
            .map(|u| strip_slash(u.trim()))
            .filter(|u| !u.is_empty())
            .collect();

        let secret = match get("STEVE_OIDC_CLIENT_SECRET_FILE").filter(|p| !p.is_empty()) {
            Some(p) => Some(
                std::fs::read_to_string(&p)
                    .map_err(|source| ConfigError::SecretFile {
                        var: "STEVE_OIDC_CLIENT_SECRET_FILE",
                        path: p.clone(),
                        source,
                    })?
                    .trim()
                    .to_string(),
            ),
            None => get("STEVE_OIDC_CLIENT_SECRET"),
        };

        // `STEVE_KB_STORE=openviking`: the OpenViking server on loopback (ops/openviking), its root
        // key from `STEVE_OPENVIKING_KEY_FILE`
        let openviking = match get("STEVE_KB_STORE").as_deref() {
            None | Some("") | Some("sqlite") => None,
            Some("openviking") => {
                let path = get("STEVE_OPENVIKING_KEY_FILE")
                    .filter(|p| !p.is_empty())
                    .ok_or(ConfigError::Invalid {
                        var: "STEVE_OPENVIKING_KEY_FILE",
                        msg: "required with STEVE_KB_STORE=openviking".into(),
                    })?;
                let key = std::fs::read_to_string(&path)
                    .map_err(|source| ConfigError::SecretFile {
                        var: "STEVE_OPENVIKING_KEY_FILE",
                        path: path.clone(),
                        source,
                    })?
                    .trim()
                    .to_string();
                Some(steve_knowledge::openviking::OpenVikingConfig {
                    url: env("STEVE_OPENVIKING_URL", "http://127.0.0.1:18933"),
                    key,
                    account: env("STEVE_OPENVIKING_ACCOUNT", "steve"),
                })
            }
            Some(other) => {
                return Err(ConfigError::Invalid {
                    var: "STEVE_KB_STORE",
                    msg: format!("sqlite or openviking, not {other}"),
                });
            }
        };

        // npm root -g only when an adapter default is actually needed
        let mut root: Option<String> = None;
        let mut npm = || root.get_or_insert_with(npm_root).clone();
        let claude_adapter = get("STEVE_ADAPTER_CLAUDE").unwrap_or_else(|| {
            Path::new(&npm())
                .join("@agentclientprotocol/claude-agent-acp/dist/index.js")
                .to_string_lossy()
                .into_owned()
        });
        let codex = get("STEVE_ADAPTER_CODEX").unwrap_or_else(|| codex_adapter(&npm()));
        let adapters = BTreeMap::from([
            ("claude-code".to_string(), claude_adapter),
            ("codex".to_string(), codex),
        ]);

        let split = |s: String| s.split(',').map(str::to_string).collect::<Vec<_>>();
        let idle_raw = env("STEVE_IDLE_RELEASE_MS", "600000");
        let idle = js_number(&idle_raw);

        Ok(Config {
            data_dir: home.join("data"),
            worktrees: home.join("worktrees"),
            scratch: home.join("scratch"),
            listen: Listen::parse(
                &env("STEVE_HOST", "127.0.0.1"),
                port_n as u16,
                get("STEVE_ALLOW_ANY_HOST").as_deref() == Some("1"),
            )?,
            public_url,
            public_urls,
            web_dir: get("STEVE_WEB_DIR").map(PathBuf::from).unwrap_or_else(|| {
                // TS resolves ../../web/dist from hub/src; the Rust crate sits two levels deeper
                resolve(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/dist"))
            }),
            oidc: Oidc {
                issuer: get("STEVE_OIDC_ISSUER"),
                client_id: get("STEVE_OIDC_CLIENT_ID"),
                client_secret: secret,
            },
            dev_login: get("STEVE_DEV_LOGIN").as_deref() == Some("1"),
            no_resume: get("STEVE_NO_RESUME").as_deref() == Some("1"),
            takeover_continues: get("STEVE_TAKEOVER_CONTINUES").and_then(|v| v.parse().ok()),
            disk: steve_core::workspace::DiskPolicy::from_vars(&get),
            gh: get("STEVE_GH").filter(|v| !v.is_empty()).map(PathBuf::from),
            claude_executable: env("CLAUDE_CODE_EXECUTABLE", "/opt/homebrew/bin/claude"),
            codex_executable: get("CODEX_EXECUTABLE").unwrap_or_else(|| {
                ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
                    .into_iter()
                    .find(|p| Path::new(p).exists())
                    .unwrap_or("/opt/homebrew/bin/codex")
                    .to_string()
            }),
            adapters,
            models: Models {
                fast: split(env(
                    "STEVE_MODELS_FAST",
                    "claude-haiku-5-5,claude-sonnet-5-5",
                )),
                smart: split(env(
                    "STEVE_MODELS_SMART",
                    "claude-sonnet-5-5,claude-opus-5-5",
                )),
            },
            default_models: BTreeMap::from([(
                "claude-code".to_string(),
                env("STEVE_DEFAULT_MODEL_CLAUDE", "claude-opus-5-5"),
            )]),
            idle_release_ms: if idle.is_finite() && idle >= 0.0 {
                idle as u64
            } else {
                600_000
            },
            item_prefix: env("STEVE_ITEM_PREFIX", "ST"),
            version: crate::build_identity()
                .map(|(version, _)| version.to_owned())
                .unwrap_or_else(|| env("STEVE_VERSION", "dev")),
            steve_repo: get("STEVE_REPO")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("Agents/steve")),
            debug_route: get("STEVE_DEBUG_ROUTE").is_some(),
            codex_home: get("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join(".codex")),
            // the Node.js binary for `.js` adapters; the Steve Node switch is STEVE_NODE_MODE.
            // `local` is the 0.2.50 spelling of that switch (deprecated), never a binary
            node: get("STEVE_NODE")
                .filter(|v| v != "local")
                .unwrap_or_else(|| "node".into()),
            openviking,
            home,
        })
    }

    /// Cookies get `Secure` only when the primary public URL is https.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("steve.sqlite3")
    }

    /// The `last.json` the release script writes (ops/release.sh).
    pub fn release_file(&self) -> PathBuf {
        self.home.join("releases/last.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(vars: &[(&str, &str)]) -> Config {
        let mut m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // keep tests away from `npm root -g`
        m.entry("STEVE_ADAPTER_CLAUDE".into())
            .or_insert("/x/claude.js".into());
        m.entry("STEVE_ADAPTER_CODEX".into())
            .or_insert("/x/codex".into());
        Config::from_vars(|k| m.get(k).cloned()).expect("config")
    }

    #[test]
    fn steve_node_is_only_the_node_js_binary() {
        // the Steve Node's switch has its own name and never reaches the adapters' Node.js
        assert_eq!(cfg(&[("STEVE_NODE_MODE", "local")]).node, "node");
        // 0.2.50 configs say STEVE_NODE=local: still the switch, never a binary named `local`
        assert_eq!(cfg(&[("STEVE_NODE", "local")]).node, "node");
        assert_eq!(
            cfg(&[("STEVE_NODE", "/opt/x/bin/node")]).node,
            "/opt/x/bin/node"
        );
    }

    #[test]
    fn defaults_match_ts() {
        let c = cfg(&[("STEVE_HOME", "/tmp/sh/./a/../b")]);
        assert_eq!(c.home, PathBuf::from("/tmp/sh/b"));
        assert_eq!(c.data_dir, PathBuf::from("/tmp/sh/b/data"));
        assert_eq!(
            c.listen,
            Listen {
                hosts: vec!["127.0.0.1".into()],
                port: 8440
            }
        );
        assert_eq!(c.public_url, "http://127.0.0.1:8440");
        assert_eq!(c.public_urls, vec!["http://127.0.0.1:8440"]);
        assert_eq!(c.claude_executable, "/opt/homebrew/bin/claude");
        assert_eq!(c.models.fast, vec!["claude-haiku-5-5", "claude-sonnet-5-5"]);
        assert_eq!(c.models.smart, vec!["claude-sonnet-5-5", "claude-opus-5-5"]);
        assert_eq!(c.default_models["claude-code"], "claude-opus-5-5");
        assert_eq!(c.idle_release_ms, 600_000);
        assert_eq!(c.version, "dev");
        assert!(!c.dev_login);
        assert!(!c.secure_cookies());
    }

    #[test]
    fn steve_host_is_a_list_and_refuses_wildcards() {
        let c = cfg(&[("STEVE_HOST", " 127.0.0.1, 100.64.0.2 ,,[::1],127.0.0.1")]);
        assert_eq!(c.listen.hosts, vec!["127.0.0.1", "100.64.0.2", "::1"]);
        assert_eq!(c.listen.env_value(), "127.0.0.1,100.64.0.2,::1");
        assert_eq!(
            c.listen.urls(),
            "http://127.0.0.1:8440, http://100.64.0.2:8440, http://[::1]:8440"
        );
        assert!(!c.listen.any());
        assert_eq!(cfg(&[("STEVE_HOST", "")]).listen.hosts, vec!["127.0.0.1"]);

        // local callers prefer a loopback host over the first one
        let l = |raw: &str| Listen::parse(raw, 1, true).expect("listen");
        assert_eq!(l("100.64.0.2,127.0.0.1").local_host(), "127.0.0.1");
        assert_eq!(l("100.64.0.2,::1").local_host(), "[::1]");
        assert_eq!(l("100.64.0.2").local_host(), "100.64.0.2");
        assert_eq!(l("0.0.0.0").local_host(), "127.0.0.1");
        assert_eq!(l("::").local_host(), "[::1]");

        for any in ["0.0.0.0", "127.0.0.1,0.0.0.0", "::", "[::]"] {
            let e = Config::from_vars(|k| match k {
                "STEVE_HOST" => Some(any.into()),
                "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("/x".into()),
                _ => None,
            })
            .expect_err(any);
            assert!(e.to_string().starts_with("STEVE_HOST: "), "{e}");
        }
        let c = cfg(&[("STEVE_HOST", "0.0.0.0"), ("STEVE_ALLOW_ANY_HOST", "1")]);
        assert!(c.listen.any());
    }

    #[tokio::test]
    async fn binds_every_listed_host() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let l = Listen::parse("127.0.0.1,localhost", port, false).expect("listen");
        let bound = l.bind().await.expect("bind");
        assert!(!bound.is_empty());
        // a second Hub on the same port is refused, naming the address
        let e = Listen::parse("127.0.0.1", port, false)
            .expect("listen")
            .bind()
            .await
            .expect_err("taken");
        assert!(e.to_string().contains(&format!("127.0.0.1:{port}")), "{e}");
    }

    #[test]
    fn public_urls_and_flags() {
        let c = cfg(&[
            ("STEVE_PUBLIC_URL", "https://a.dev/"),
            ("STEVE_PUBLIC_URLS", " https://a.dev/ , ,https://a.home"),
            ("STEVE_DEV_LOGIN", "1"),
            ("STEVE_PORT", "9001"),
        ]);
        assert_eq!(c.public_url, "https://a.dev");
        assert_eq!(c.public_urls, vec!["https://a.dev", "https://a.home"]);
        assert!(c.dev_login && c.secure_cookies());
        assert_eq!(c.listen.port, 9001);
        // an empty STEVE_PUBLIC_URLS is "set" for `??`, so no origins at all
        assert!(cfg(&[("STEVE_PUBLIC_URLS", "")]).public_urls.is_empty());
    }

    #[test]
    fn secret_file_wins_and_is_trimmed() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = dir.path().join("s");
        std::fs::write(&f, "  sekrit\n").expect("write");
        let c = cfg(&[
            ("STEVE_OIDC_CLIENT_SECRET", "env"),
            (
                "STEVE_OIDC_CLIENT_SECRET_FILE",
                f.to_str().unwrap_or_default(),
            ),
        ]);
        assert_eq!(c.oidc.client_secret.as_deref(), Some("sekrit"));
        assert_eq!(
            cfg(&[("STEVE_OIDC_CLIENT_SECRET", "env")])
                .oidc
                .client_secret
                .as_deref(),
            Some("env")
        );
    }

    #[test]
    fn bad_port_is_an_error() {
        let m = HashMap::from([
            ("STEVE_PORT".to_string(), "abc".to_string()),
            ("STEVE_ADAPTER_CLAUDE".into(), "x".into()),
            ("STEVE_ADAPTER_CODEX".into(), "x".into()),
        ]);
        assert!(Config::from_vars(|k| m.get(k).cloned()).is_err());
    }

    #[test]
    fn number_like_js() {
        assert_eq!(js_number(" 12 "), 12.0);
        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_number("0x10"), 16.0);
        assert_eq!(js_number("1e3"), 1000.0);
        assert!(js_number("abc").is_nan());
        assert!(js_number("inf").is_nan());
        assert_eq!(js_number("-Infinity"), f64::NEG_INFINITY);
    }
}
