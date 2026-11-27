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
    /// Listening sockets the Hub was started with ([`Handed::take`]), served instead of binding.
    pub handed: Handed,
}

/// Listening sockets handed to the Hub when it starts, the way systemd socket activation does:
/// `LISTEN_PID` is the Hub's process id and `LISTEN_FDS` sockets start at descriptor 3, named by
/// the colon-separated `LISTEN_FDNAMES`. One named `local-node` is the service Node's listener
/// (`STEVE_LOCAL_NODE_PORT`); every other one is the Hub's own port, one per `STEVE_HOST` address
/// on `STEVE_PORT`.
///
/// Whoever starts the Hub keeps the port while the Hub restarts, so nothing else can take it in
/// between. The Hub's children do not inherit the sockets.
#[derive(Debug, Clone, Default)]
pub struct Handed(std::sync::Arc<Sockets>);

#[derive(Debug, Default)]
struct Sockets {
    hub: Vec<std::net::TcpListener>,
    local_node: Option<std::net::TcpListener>,
}

impl PartialEq for Handed {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || (self.is_empty() && other.is_empty())
    }
}

impl Eq for Handed {}

impl Handed {
    /// The sockets this process was started with (none unless `LISTEN_PID` is this process).
    /// Takes the descriptors over: call it once, before the Hub starts any process of its own.
    pub fn take() -> std::io::Result<Handed> {
        Self::take_from(|k| std::env::var(k).ok())
    }

    fn take_from(get: impl Fn(&str) -> Option<String>) -> std::io::Result<Handed> {
        use std::os::fd::FromRawFd;
        let invalid = |msg: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg);
        if get("LISTEN_PID").and_then(|p| p.trim().parse::<u32>().ok()) != Some(std::process::id())
        {
            return Ok(Handed::default());
        }
        let n: usize = get("LISTEN_FDS")
            .unwrap_or_default()
            .trim()
            .parse()
            .map_err(|_| invalid("LISTEN_FDS is not a number".into()))?;
        let names: Vec<String> = match get("LISTEN_FDNAMES") {
            Some(v) => v.split(':').map(str::to_string).collect(),
            None => vec![String::new(); n],
        };
        if names.len() != n {
            return Err(invalid(format!(
                "LISTEN_FDNAMES names {} sockets, LISTEN_FDS {n}",
                names.len()
            )));
        }
        let mut sockets = Sockets::default();
        for (i, name) in names.into_iter().enumerate() {
            let fd = 3 + i as i32;
            // SAFETY: fcntl on a descriptor number only; a closed one fails with EBADF
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: LISTEN_PID names this process: the descriptor was handed to it, and is
            // owned from here on by this listener alone
            let l = unsafe { std::net::TcpListener::from_raw_fd(fd) };
            l.local_addr()
                .map_err(|e| invalid(format!("handed socket {fd} ({name}): {e}")))?;
            match name.as_str() {
                "local-node" if sockets.local_node.is_none() => sockets.local_node = Some(l),
                "local-node" => return Err(invalid("two handed sockets named local-node".into())),
                _ => sockets.hub.push(l),
            }
        }
        Ok(Handed(std::sync::Arc::new(sockets)))
    }

    pub fn is_empty(&self) -> bool {
        self.0.hub.is_empty() && self.0.local_node.is_none()
    }

    /// The service Node's listener, if one was handed.
    pub fn local_node(&self) -> Option<std::io::Result<tokio::net::TcpListener>> {
        self.0.local_node.as_ref().map(tokio_listener)
    }
}

/// A listener of the runtime's on the same socket (each server gets its own descriptor).
fn tokio_listener(l: &std::net::TcpListener) -> std::io::Result<tokio::net::TcpListener> {
    let l = l.try_clone()?;
    l.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(l)
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
        let listen = Self {
            hosts,
            port,
            handed: Handed::default(),
        };
        if listen.any() && !allow_any {
            return Err(ConfigError::Invalid {
                var: "STEVE_HOST",
                msg: "0.0.0.0 / :: listens on every network, so the Hub refuses it. In steve.env \
                      either set STEVE_ALLOW_ANY_HOST=1 to keep listening everywhere, or narrow \
                      STEVE_HOST to 127.0.0.1 plus the addresses that must reach the Hub \
                      (e.g. 127.0.0.1,100.64.0.2)"
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

    /// Bind every host; fails (naming the address) if any one cannot be bound. Handed sockets
    /// ([`Handed`]) are served as they are instead.
    pub async fn bind(&self) -> std::io::Result<Vec<tokio::net::TcpListener>> {
        if !self.handed.0.hub.is_empty() {
            return self.handed.0.hub.iter().map(tokio_listener).collect();
        }
        self.bind_port(self.port).await
    }

    /// Serve `handed` instead of binding: one socket per host, each on this port.
    pub fn adopt(&mut self, handed: Handed) -> std::io::Result<()> {
        let hub = &handed.0.hub;
        if !hub.is_empty() {
            let invalid = |msg: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg);
            if hub.len() != self.hosts.len() {
                return Err(invalid(format!(
                    "{} handed sockets for the Hub's port, STEVE_HOST names {} addresses",
                    hub.len(),
                    self.hosts.len()
                )));
            }
            for l in hub {
                let addr = l.local_addr()?;
                if addr.port() != self.port {
                    return Err(invalid(format!(
                        "a handed socket listens on {addr}, not on STEVE_PORT ({})",
                        self.port
                    )));
                }
            }
        }
        self.handed = handed;
        Ok(())
    }

    /// Every host on `port` (the web client's own port, [`Config::web_port`]).
    pub async fn bind_port(&self, mut port: u16) -> std::io::Result<Vec<tokio::net::TcpListener>> {
        let mut out = Vec::with_capacity(self.hosts.len());
        for h in &self.hosts {
            let l = tokio::net::TcpListener::bind((h.as_str(), port))
                .await
                .map_err(|e| std::io::Error::new(e.kind(), format!("listen on {h}:{port}: {e}")))?;
            if port == 0 {
                port = l.local_addr()?.port();
            }
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

/// A diagnostic Hub opens a copied installation without running work or accepting Nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Normal,
    Diagnostic,
}

/// The whole Hub configuration, read once at boot.
#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub cache: steve_runtime::cache::CacheConfig,
    pub data_dir: PathBuf,
    pub worktrees: PathBuf,
    pub scratch: PathBuf,
    pub listen: Listen,
    pub metrics: Option<crate::metrics::Listen>,
    /// Primary public origin, trailing `/` stripped; decides the cookie `Secure` flag.
    pub public_url: String,
    /// Every origin Steve is served on; OIDC login returns to the one the browser used.
    pub public_urls: Vec<String>,
    public_url_defaulted: bool,
    public_urls_defaulted: bool,
    /// Web origins a client may call this Hub from besides those of signed-in web clients
    /// (`STEVE_CLIENT_ORIGINS`, comma-separated), e.g. a web client served on its own port.
    pub client_origins: Vec<String>,
    /// The web client's own port (`STEVE_WEB_PORT`), on the same hosts as the Hub: set, the web pages are
    /// served there and the Hub's port answers the API only (A3); unset, both on the Hub's port.
    pub web_port: Option<u16>,
    pub web_dir: PathBuf,
    pub oidc: Oidc,
    /// `/auth/dev` from loopback, for local development only.
    pub dev_login: bool,
    /// Normal operation or a diagnostic copy (`STEVE_NO_RESUME=1`).
    pub mode: RuntimeMode,
    /// The daily snapshot in `<home>/backups/restic` (`STEVE_BACKUP=off` turns it off): see `daily_backup`.
    pub backup: bool,
    /// `STEVE_TAKEOVER_CONTINUES`: how often Steve continues a stopped agent before the user hears of it
    /// (`None`: the core's default).
    pub takeover_continues: Option<u32>,
    /// `STEVE_TAKEOVER_SAME_REFUSALS`: how many refusals of a delivery in a row for the same reason
    /// hand the item to the user instead of another continue; 0: never (`None`: the core's default).
    pub takeover_same_refusals: Option<u32>,
    /// `STEVE_PROVIDER_ERROR_HANDOFF_THRESHOLD`: consecutive errors on one surface before switching fallback (`None`: core default).
    pub provider_error_handoff_threshold: Option<u32>,
    /// `STEVE_DISK_FLOOR_GB`, `STEVE_DISK_LOW_GB`, `STEVE_WORKTREE_IDLE_HOURS` (task workspaces).
    pub disk: steve_core::workspace::DiskPolicy,
    /// `STEVE_GH`: the GitHub CLI that follows Release builds (`None`: looked up by the core).
    pub gh: Option<PathBuf>,
    /// What the Hub's harness manifest declares about adapters beyond their command
    /// (`STEVE_HARNESS_MANIFEST`, default `<STEVE_HOME>/harness.json`; [`steve_harness_spec`]).
    pub declared_adapters: BTreeMap<String, steve_harness_spec::Declaration>,
    /// Adapter ids explicitly listed in the Hub's harness manifest (`harness.json`).
    pub manifest_adapters: std::collections::BTreeSet<String>,
    /// Where that manifest is (the runtimes page adds adapters to it).
    pub harness_manifest: PathBuf,
    pub models: Models,
    /// Model for bots that don't pick one, per harness.
    pub default_models: BTreeMap<String, String>,
    pub idle_release_ms: u64,
    pub version: String,
    /// Steve's own repository (`STEVE_REPO`): seed project, items about Steve itself, release repo.
    pub steve_repo: PathBuf,
    /// `STEVE_MAINTAINER_WORKFLOW=1`: opt-in maintainer release cutting (`ops/cut-release.py`).
    pub maintainer_workflow: bool,
    pub debug_route: bool,
    /// Where knowledge is stored and found (`STEVE_KB_STORE`): `None` = the Hub's own index only.
    pub openviking: Option<steve_knowledge::openviking::OpenVikingConfig>,
}

/// Errors that stop the Hub at boot, like the TS module-level throws.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Input(#[from] std::io::Error),
    #[error("{var}: {msg}")]
    Invalid { var: &'static str, msg: String },
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
/// Normalize `p` to an absolute canonical path: resolves symlinks on the deepest existing ancestor
/// (for example macOS `/var` -> `/private/var` or `/tmp` -> `/private/tmp`) and re-appends any
/// not-yet-created trailing components lexically.
pub fn resolve(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    };
    let mut normalized = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut tail = Vec::new();
    let mut cur = normalized.as_path();
    loop {
        if let Ok(canon) = std::fs::canonicalize(cur) {
            let mut out = canon;
            for part in tail.into_iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = cur.file_name() else {
            return normalized;
        };
        tail.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            return normalized;
        };
        cur = parent;
    }
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

/// Reads a Hub-owned configuration text file (harness manifest or `steve.env`).
fn read_config_text(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// Resolves `<STEVE_HOME>/steve.env` (defaulting `STEVE_HOME` to `$HOME/.steve`) and returns only
/// variables not already set in `existing`. Missing `steve.env` is normal and returns an empty map;
/// symbolic links and non-regular files are refused.
fn read_env_file_vars(
    existing: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let steve_home = existing
        .get("STEVE_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            existing
                .get("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".steve"))
        });
    let Some(home) = steve_home else {
        return Ok(BTreeMap::new());
    };
    let home = resolve(&home);
    let path = home.join("steve.env");
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(ConfigError::Input(e)),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(ConfigError::Invalid {
            var: "STEVE_HOME",
            msg: "steve.env must be a regular file, not a symbolic link".into(),
        });
    }
    let text = read_config_text(&path)?;
    let mut base = existing.clone();
    base.insert("STEVE_HOME".into(), home.to_string_lossy().into_owned());
    let parsed =
        steve_install::env_file::parse_runtime_env(text.as_bytes(), &base).map_err(|e| {
            ConfigError::Invalid {
                var: "STEVE_HOME",
                msg: format!("steve.env: {e}"),
            }
        })?;
    Ok(parsed
        .into_iter()
        .filter(|(k, _)| k == "PATH" || !existing.contains_key(k))
        .collect())
}

/// Loads `<STEVE_HOME>/steve.env` into the process environment at startup without overriding
/// variables already supplied by the caller or service manager — except `PATH`, where launchd
/// always supplies a minimal default and an explicit `PATH` in `steve.env` must take effect.
///
/// Must be called once in `main()` before spawning worker threads or building the Tokio runtime.
pub fn load_env_file() -> Result<(), ConfigError> {
    let existing: BTreeMap<String, String> = std::env::vars().collect();
    let to_set = read_env_file_vars(&existing)?;
    for (key, value) in to_set {
        if key == "PATH" || std::env::var_os(&key).is_none() {
            // SAFETY: invoked at process startup in main() while single-threaded, before the
            // Tokio runtime or any background threads exist.
            unsafe {
                std::env::set_var(&key, &value);
            }
        }
    }
    Ok(())
}

/// Default static web directory when `STEVE_WEB_DIR` is unset: prefer an installed `<home>/web/current`,
/// then a sibling `web/dist` bundled next to the running executable, and finally the source checkout.
fn default_web_dir(home: &Path) -> PathBuf {
    let installed = home.join("web/current");
    if installed.join("index.html").is_file() {
        return installed;
    }
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::canonicalize(exe).ok())
        .and_then(|exe| {
            exe.parent()
                .and_then(Path::parent)
                .map(|root| root.join("web/dist"))
        })
        .filter(|dir| dir.join("index.html").is_file())
    {
        return bundled;
    }
    // TS resolves ../../web/dist from hub/src; the Rust crate sits two levels deeper
    resolve(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/dist"))
}

/// The adapters a harness manifest (`{"adapters": {…}}`, as a Node's `harness.json`) lists. A file
/// that is missing lists none; one that does not read is logged and ignored, never a reason not to
/// start.
fn harness_manifest(
    path: &Path,
) -> (
    BTreeMap<String, PathBuf>,
    BTreeMap<String, steve_harness_spec::Declaration>,
    Option<PathBuf>,
) {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        adapters: std::collections::HashMap<String, steve_harness_spec::AdapterEntry>,
        #[serde(default)]
        node: Option<PathBuf>,
    }
    let text = match read_config_text(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(e) => {
            tracing::warn!("harness manifest {}: {e}; ignored", path.display());
            return Default::default();
        }
    };
    match serde_json::from_str::<Manifest>(&text) {
        Ok(m) => {
            let (commands, declared) = steve_harness_spec::split_entries(m.adapters);
            (
                commands.into_iter().collect(),
                declared.into_iter().collect(),
                m.node.filter(|p| !p.as_os_str().is_empty()),
            )
        }
        Err(e) => {
            tracing::warn!("harness manifest {}: {e}; ignored", path.display());
            Default::default()
        }
    }
}

fn strip_slash(s: &str) -> String {
    s.strip_suffix('/').unwrap_or(s).to_string()
}

// Removed inputs are rejected even when set to an empty value.
const REMOVED: &[&str] = &["STEVE_ITEM_PREFIX"];

fn count(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<u32>, ConfigError> {
    get(var)
        .map(|s| {
            s.trim().parse().map_err(|_| ConfigError::Invalid {
                var,
                msg: "expected an unsigned 32-bit integer".into(),
            })
        })
        .transpose()
}

fn validate_url(var: &'static str, value: &str) -> Result<(), ConfigError> {
    if reqwest::Url::parse(value)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
    {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            var,
            msg: "expected an absolute http(s) URL".into(),
        })
    }
}

struct Endpoints {
    port: u16,
    public_url: String,
    public_urls: Vec<String>,
    public_url_defaulted: bool,
    public_urls_defaulted: bool,
    web_port: Option<u16>,
    client_origins: Vec<String>,
}

fn default_public_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn endpoints(get: &impl Fn(&str) -> Option<String>) -> Result<Endpoints, ConfigError> {
    let port_raw = get("STEVE_PORT").unwrap_or_else(|| "8440".into());
    let port_n = port_raw
        .parse::<u16>()
        .ok()
        .filter(|_| !port_raw.is_empty() && port_raw.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(ConfigError::Invalid {
            var: "STEVE_PORT",
            msg: "expected a decimal port from 0 to 65535".into(),
        })?;

    // unset: this machine, on the port the Hub listens on
    let public_raw = get("STEVE_PUBLIC_URL");
    let public_url_defaulted = public_raw.is_none();
    let public_raw = public_raw.unwrap_or_else(|| default_public_url(port_n));
    validate_url("STEVE_PUBLIC_URL", &public_raw)?;
    let public_url = strip_slash(&public_raw);
    let web_port = match get("STEVE_WEB_PORT").filter(|v| !v.trim().is_empty()) {
        None => None,
        Some(v) => match v.trim().parse::<u16>() {
            Ok(p) if p != 0 && p != port_n => Some(p),
            _ => {
                return Err(ConfigError::Invalid {
                    var: "STEVE_WEB_PORT",
                    msg: format!("a port other than STEVE_PORT ({port_n}), not {v}"),
                });
            }
        },
    };
    let public_urls = get("STEVE_PUBLIC_URLS");
    let public_urls_defaulted = public_urls.is_none();
    let public_urls: Vec<String> = public_urls
        .unwrap_or_else(|| public_raw.clone())
        .split(',')
        .map(|u| strip_slash(u.trim()))
        .filter(|u| !u.is_empty())
        .collect();

    for url in &public_urls {
        validate_url("STEVE_PUBLIC_URLS", url)?;
    }
    let client_origins: Vec<String> = get("STEVE_CLIENT_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(|u| strip_slash(u.trim()))
        .filter(|u| !u.is_empty())
        .collect();
    for url in &client_origins {
        validate_url("STEVE_CLIENT_ORIGINS", url)?;
    }
    Ok(Endpoints {
        port: port_n,
        public_url,
        public_urls,
        public_url_defaulted,
        public_urls_defaulted,
        web_port,
        client_origins,
    })
}

fn knowledge_backend(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<steve_knowledge::openviking::OpenVikingConfig>, ConfigError> {
    // Validate sources even when the optional backend is disabled.
    let openviking_key = steve_auth::AuthConfig::secret(
        get,
        "STEVE_OPENVIKING_KEY",
        steve_auth::SecretSource::FileOnly,
    )?;
    let openviking_url = get("STEVE_OPENVIKING_URL");
    if let Some(url) = &openviking_url {
        validate_url("STEVE_OPENVIKING_URL", url)?;
    }

    // `STEVE_KB_STORE=openviking`: the OpenViking server on loopback (ops/openviking), its root
    // key from `STEVE_OPENVIKING_KEY_FILE`
    let openviking = match get("STEVE_KB_STORE").as_deref() {
        None | Some("") | Some("sqlite") => None,
        Some("openviking") => {
            let key = openviking_key
                .filter(|k| !k.is_empty())
                .ok_or(ConfigError::Invalid {
                    var: "STEVE_OPENVIKING_KEY_FILE",
                    msg: "required with STEVE_KB_STORE=openviking".into(),
                })?;
            Some(steve_knowledge::openviking::OpenVikingConfig {
                url: openviking_url.unwrap_or_else(|| "http://127.0.0.1:18933".into()),
                key,
                account: get("STEVE_OPENVIKING_ACCOUNT").unwrap_or_else(|| "steve".into()),
            })
        }
        Some(other) => {
            return Err(ConfigError::Invalid {
                var: "STEVE_KB_STORE",
                msg: format!("sqlite or openviking, not {other}"),
            });
        }
    };

    Ok(openviking)
}

fn cache_config(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<steve_runtime::cache::CacheConfig, ConfigError> {
    let cache_mode = match get("STEVE_CACHE").as_deref() {
        None | Some("memory") => steve_runtime::cache::CacheMode::Memory,
        Some("off") => steve_runtime::cache::CacheMode::Off,
        Some(_) => {
            return Err(ConfigError::Invalid {
                var: "STEVE_CACHE",
                msg: "this version does not support this backend; expected memory or off".into(),
            });
        }
    };
    let memory_mb = get("STEVE_CACHE_MEMORY_MB")
        .map(|s| s.parse::<u32>())
        .transpose()
        .map_err(|_| ConfigError::Invalid {
            var: "STEVE_CACHE_MEMORY_MB",
            msg: "expected a positive integer".into(),
        })?
        .unwrap_or(256);
    if memory_mb == 0 {
        return Err(ConfigError::Invalid {
            var: "STEVE_CACHE_MEMORY_MB",
            msg: "expected a positive integer".into(),
        });
    }
    crate::cache_tags::validate_spaces(crate::cache_tags::SPACES)?;
    let cache = steve_runtime::cache::CacheConfig {
        mode: cache_mode,
        memory_mb,
    };
    Ok(cache)
}

impl Config {
    /// Reads the process environment and creates the Hub's data and working directories.
    pub fn from_env() -> Result<Self, ConfigError> {
        let cfg = Self::from_vars(|k| std::env::var(k).ok())?;
        for d in [
            &cfg.data_dir,
            &cfg.worktrees,
            &cfg.scratch,
            &cfg.home.join("workspaces"),
        ] {
            std::fs::create_dir_all(d).map_err(|source| ConfigError::Dir {
                path: d.display().to_string(),
                source,
            })?;
        }
        Ok(cfg)
    }

    /// Builds the config from any variable source (tests pass a map); touches no directories.
    /// The source must provide STEVE_HOME or HOME; the process cannot supply a missing Hub home.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        if let Some(&var) = REMOVED.iter().find(|&&var| get(var).is_some()) {
            return Err(ConfigError::Invalid {
                var,
                msg: "unknown variable; see the deployment configuration reference".into(),
            });
        }
        let env = |k: &str, d: &str| get(k).unwrap_or_else(|| d.to_string());
        let home = get("STEVE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                get("HOME")
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join(".steve"))
            })
            .ok_or_else(|| ConfigError::Invalid {
                var: "STEVE_HOME",
                msg: "set STEVE_HOME or HOME in the configuration variables".into(),
            })?;
        let home = resolve(&home);

        let Endpoints {
            port: port_n,
            public_url,
            public_urls,
            public_url_defaulted,
            public_urls_defaulted,
            web_port,
            client_origins,
        } = endpoints(&get)?;
        let client_secret = steve_auth::AuthConfig::secret(
            &get,
            "STEVE_OIDC_CLIENT_SECRET",
            steve_auth::SecretSource::FileOrDirect,
        )?;
        let issuer = get("STEVE_OIDC_ISSUER");
        if let Some(url) = &issuer {
            validate_url("STEVE_OIDC_ISSUER", url)?;
        }
        let openviking = knowledge_backend(&get)?;

        // the harness manifest: adapters of one's own declared for UI/routing metadata
        let manifest = get("STEVE_HARNESS_MANIFEST")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("harness.json"));
        let (manifest_commands, declared_adapters, _manifest_node) = harness_manifest(&manifest);
        let manifest_adapters: std::collections::BTreeSet<String> =
            manifest_commands.keys().cloned().collect();

        let split = |s: String| {
            s.split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect::<Vec<_>>()
        };
        let idle_raw = env("STEVE_IDLE_RELEASE_MS", "600000");
        let idle = idle_raw
            .trim()
            .parse::<u64>()
            .map_err(|_| ConfigError::Invalid {
                var: "STEVE_IDLE_RELEASE_MS",
                msg: "expected a nonnegative integer number of milliseconds".into(),
            })?;
        if idle > i64::MAX as u64 {
            return Err(ConfigError::Invalid {
                var: "STEVE_IDLE_RELEASE_MS",
                msg: "expected a nonnegative integer number of milliseconds within i64 range"
                    .into(),
            });
        }
        let backup = match get("STEVE_BACKUP") {
            None => true,
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "" | "on" => true,
                "off" => false,
                _ => {
                    return Err(ConfigError::Invalid {
                        var: "STEVE_BACKUP",
                        msg: "expected on or off".into(),
                    });
                }
            },
        };
        let disk = steve_core::workspace::DiskPolicy::from_vars(&get).map_err(|e| {
            ConfigError::Invalid {
                var: e.var,
                msg: e.message.into(),
            }
        })?;

        let cache = cache_config(&get)?;
        Ok(Config {
            cache,
            data_dir: home.join("data"),
            worktrees: home.join("worktrees"),
            scratch: home.join("scratch"),
            listen: Listen::parse(
                &env("STEVE_HOST", "127.0.0.1"),
                port_n,
                steve_auth::flag(&get, "STEVE_ALLOW_ANY_HOST")?,
            )?,
            metrics: {
                let listen =
                    crate::metrics::Listen::parse(&env("STEVE_METRICS_LISTEN", "127.0.0.1:9464"))?;
                match env("STEVE_METRICS", "off").as_str() {
                    "" | "off" => None,
                    "prometheus" => Some(listen),
                    _ => {
                        return Err(ConfigError::Invalid {
                            var: "STEVE_METRICS",
                            msg: "expected off or prometheus".into(),
                        });
                    }
                }
            },
            public_url,
            public_urls,
            public_url_defaulted,
            public_urls_defaulted,
            web_port,
            client_origins,
            web_dir: get("STEVE_WEB_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| default_web_dir(&home)),
            oidc: Oidc {
                issuer,
                client_id: get("STEVE_OIDC_CLIENT_ID"),
                client_secret,
            },
            dev_login: steve_auth::flag(&get, "STEVE_DEV_LOGIN")?,
            mode: if steve_auth::flag(&get, "STEVE_NO_RESUME")? {
                RuntimeMode::Diagnostic
            } else {
                RuntimeMode::Normal
            },
            backup,
            takeover_continues: count(&get, "STEVE_TAKEOVER_CONTINUES")?,
            takeover_same_refusals: count(&get, "STEVE_TAKEOVER_SAME_REFUSALS")?,
            provider_error_handoff_threshold: count(
                &get,
                "STEVE_PROVIDER_ERROR_HANDOFF_THRESHOLD",
            )?,
            disk,
            gh: get("STEVE_GH").filter(|v| !v.is_empty()).map(PathBuf::from),
            declared_adapters,
            manifest_adapters,
            harness_manifest: manifest,
            models: Models {
                // none by default: every call follows the harness's own default model
                fast: split(env("STEVE_MODELS_FAST", "")),
                smart: split(env("STEVE_MODELS_SMART", "")),
            },
            // a new agent session's model only when set; else the harness's own default
            default_models: steve_harness_spec::builtin_command_vars()
                .filter_map(|(id, var)| {
                    let suffix = var.strip_prefix("STEVE_ADAPTER_")?;
                    let model = get(&format!("STEVE_DEFAULT_MODEL_{suffix}"))?
                        .trim()
                        .to_string();
                    (!model.is_empty()).then(|| (id.to_string(), model))
                })
                .collect(),
            idle_release_ms: idle,
            version: crate::build_identity()
                .map(|(version, _)| version.to_owned())
                .unwrap_or_else(|| env("STEVE_VERSION", "dev")),
            /* set, or the checkout this program was built in; nobody's directory by default */
            steve_repo: get("STEVE_REPO")
                .map(PathBuf::from)
                .or_else(steve_core::config::steve_checkout_of_exe)
                .unwrap_or_default(),
            maintainer_workflow: steve_auth::flag(&get, "STEVE_MAINTAINER_WORKFLOW")?,
            debug_route: steve_auth::flag(&get, "STEVE_DEBUG_ROUTE")?,
            openviking,
            home,
        })
    }

    /// Cookies get `Secure` only when the primary public URL is https.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    /// Defaults follow the bound HTTP port. Explicitly configured public origins are fixed.
    pub fn set_http_port(&mut self, port: u16) {
        self.listen.port = port;
        if self.public_url_defaulted {
            self.public_url = default_public_url(port);
        }
        if self.public_urls_defaulted {
            self.public_urls = vec![self.public_url.clone()];
        }
    }

    pub fn diagnostic(&self) -> bool {
        self.mode == RuntimeMode::Diagnostic
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("steve.sqlite3")
    }

    /// The harness adapters the Hub knows (built-in, and its manifest's), sorted: what a bot may
    /// run on.
    pub fn harness_ids(&self) -> Vec<String> {
        let mut ids: std::collections::BTreeSet<String> = steve_harness_spec::builtin_ids()
            .map(str::to_string)
            .collect();
        ids.extend(self.manifest_adapters.iter().cloned());
        ids.extend(self.declared_adapters.keys().cloned());
        ids.into_iter().collect()
    }

    /// Whether `id` is a built-in harness or declared in the Hub's harness manifest.
    pub fn knows_harness(&self, id: &str) -> bool {
        steve_harness_spec::builtin(id).is_some()
            || self.manifest_adapters.contains(id)
            || self.declared_adapters.contains_key(id)
    }

    /// The `last.json` metadata written by release deployments.
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
        let dir = tempfile::tempdir().unwrap();
        if !m.contains_key("HOME") {
            m.entry("STEVE_HOME".into())
                .or_insert_with(|| dir.path().display().to_string());
        }
        // keep tests away from `npm root -g`
        m.entry("STEVE_ADAPTER_CLAUDE".into())
            .or_insert("/x/claude.js".into());
        m.entry("STEVE_ADAPTER_CODEX".into())
            .or_insert("/x/codex".into());
        for (_, var) in steve_harness_spec::builtin_command_vars() {
            m.entry(var.into()).or_insert("/x/adapter".into());
        }
        Config::from_vars(|k| m.get(k).cloned()).expect("config")
    }

    #[test]
    fn configuration_errors_keep_source_order() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            (
                "STEVE_PORT",
                &[("STEVE_PORT", "invalid"), ("STEVE_PUBLIC_URL", "invalid")],
            ),
            (
                "STEVE_PUBLIC_URL",
                &[
                    ("STEVE_PUBLIC_URL", "invalid"),
                    ("STEVE_OIDC_ISSUER", "invalid"),
                ],
            ),
            (
                "STEVE_OPENVIKING_URL",
                &[
                    ("STEVE_OPENVIKING_URL", "invalid"),
                    ("STEVE_CACHE", "invalid"),
                ],
            ),
        ];
        for (expected, values) in cases {
            let error = Config::from_vars(|key| {
                if key == "STEVE_HOME" {
                    return Some(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("tests/config-home")
                            .display()
                            .to_string(),
                    );
                }
                values
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_owned())
            })
            .expect_err("invalid configuration");
            assert!(
                matches!(error, ConfigError::Invalid { var, .. } if var == *expected),
                "{error}"
            );
        }
    }

    #[test]
    fn home_comes_only_from_the_variable_source() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().display().to_string();
        assert_eq!(
            cfg(&[("HOME", &home)]).home,
            resolve(&dir.path().join(".steve"))
        );
        let explicit = dir.path().join("hub").display().to_string();
        assert_eq!(
            cfg(&[("HOME", &home), ("STEVE_HOME", &explicit)]).home,
            resolve(&dir.path().join("hub")),
        );
        assert_eq!(
            cfg(&[("STEVE_HOME", &explicit)]).home,
            resolve(&dir.path().join("hub"))
        );
        for home in [None, Some("")] {
            let error =
                Config::from_vars(|k| (k == "HOME").then_some(home).flatten().map(str::to_string))
                    .unwrap_err();
            assert!(matches!(
                error,
                ConfigError::Invalid {
                    var: "STEVE_HOME",
                    ..
                }
            ));
        }
    }

    #[test]
    fn handed_sockets_are_only_this_processs() {
        // another process's sockets (a child inherits its parent's LISTEN_*): none taken
        let other = (std::process::id() + 1).to_string();
        let h = Handed::take_from(|k| match k {
            "LISTEN_PID" => Some(other.clone()),
            "LISTEN_FDS" => Some("1".into()),
            _ => None,
        })
        .expect("ignored");
        assert!(h.is_empty());
        assert!(Handed::take_from(|_| None).expect("none").is_empty());
    }

    #[tokio::test]
    async fn handed_sockets_must_be_the_hubs_port() {
        let socket = || std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let handed = |hub: Vec<std::net::TcpListener>| {
            Handed(std::sync::Arc::new(Sockets {
                hub,
                local_node: None,
            }))
        };
        let s = socket();
        let port = s.local_addr().expect("addr").port();
        let mut l = Listen::parse("127.0.0.1", port, false).expect("listen");
        l.adopt(handed(vec![s])).expect("on STEVE_PORT");
        // served as it is, as often as asked (the upgrade gate, then the Hub)
        for _ in 0..2 {
            let bound = l.bind().await.expect("handed");
            assert_eq!(bound[0].local_addr().expect("addr").port(), port);
        }
        let mut other = Listen::parse("127.0.0.1", port, false).expect("listen");
        // a socket of its own: never on the port `s` holds
        let e = other
            .adopt(handed(vec![socket()]))
            .expect_err("another port");
        assert!(e.to_string().contains("not on STEVE_PORT"), "{e}");
        let mut two = Listen::parse("127.0.0.1,localhost", port, false).expect("listen");
        let e = two
            .adopt(l.handed.clone())
            .expect_err("one socket, two hosts");
        assert!(e.to_string().contains("2 addresses"), "{e}");
    }

    #[test]
    fn resolve_canonicalizes_symlink_ancestors_and_missing_tails() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(unix)]
        {
            let expected = std::fs::canonicalize(&real).unwrap().join("worktrees/ST-1");
            assert_eq!(resolve(&link.join("a/../worktrees/ST-1")), expected);
        }
    }

    #[test]
    fn defaults_match_ts() {
        let c = cfg(&[("STEVE_HOME", "/tmp/sh/./a/../b")]);
        assert_eq!(c.home, resolve(Path::new("/tmp/sh/b")));
        assert_eq!(c.data_dir, resolve(Path::new("/tmp/sh/b/data")));
        assert_eq!(
            c.listen,
            Listen {
                hosts: vec!["127.0.0.1".into()],
                port: 8440,
                handed: Handed::default(),
            }
        );
        assert_eq!(c.public_url, "http://127.0.0.1:8440");
        assert_eq!(c.public_urls, vec!["http://127.0.0.1:8440"]);
        // no model named anywhere: the harness's own default
        assert!(c.models.fast.is_empty() && c.models.smart.is_empty());
        assert!(c.default_models.is_empty());
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

        let dir = tempfile::tempdir().unwrap();
        for any in ["0.0.0.0", "127.0.0.1,0.0.0.0", "::", "[::]"] {
            let e = Config::from_vars(|k| match k {
                "STEVE_HOME" => Some(dir.path().display().to_string()),
                "STEVE_HOST" => Some(any.into()),
                v if v.starts_with("STEVE_ADAPTER_") => Some("/x".into()),
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
        // a port that was free when asked for can be another test's a moment later: ask again then
        let mut tries = 0;
        let (port, bound) = loop {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .expect("free port")
                .port();
            let l = Listen::parse("127.0.0.1,localhost", port, false).expect("listen");
            match l.bind().await {
                Ok(bound) => break (port, bound),
                Err(e) if tries < 20 && e.kind() == std::io::ErrorKind::AddrInUse => tries += 1,
                Err(e) => panic!("bind: {e:?}"),
            }
        };
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
    fn assigning_a_bound_port_updates_only_default_origins() {
        let mut defaulted = cfg(&[("STEVE_PORT", "0")]);
        defaulted.set_http_port(18700);
        assert_eq!(defaulted.public_url, "http://127.0.0.1:18700");
        assert_eq!(defaulted.public_urls, ["http://127.0.0.1:18700"]);

        let mut explicit = cfg(&[
            ("STEVE_PORT", "0"),
            ("STEVE_PUBLIC_URL", "http://127.0.0.1:0"),
            ("STEVE_PUBLIC_URLS", "https://example.com"),
        ]);
        explicit.set_http_port(18700);
        assert_eq!(explicit.public_url, "http://127.0.0.1:0");
        assert_eq!(explicit.public_urls, ["https://example.com"]);
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
        // no public URL: the Hub's own port, not the default one
        let c = cfg(&[("STEVE_PORT", "9001")]);
        assert_eq!(c.public_url, "http://127.0.0.1:9001");
        assert_eq!(c.public_urls, vec!["http://127.0.0.1:9001"]);
    }

    /// Harness parity S1: the Hub's own manifest adds adapters and declarations; an explicit
    /// STEVE_ADAPTER_* still wins, and a manifest that does not read is ignored, not fatal.
    #[test]
    fn the_harness_manifest_adds_adapters() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        std::fs::write(
            dir.path().join("harness.json"),
            r#"{"adapters": {"claude-code": "/m/claude.js", "codex": "/m/codex",
                "mine": {"command": "/m/mine", "name": "Mine"}}}"#,
        )
        .unwrap();
        let c = cfg(&[("STEVE_HOME", home)]);
        assert!(c.knows_harness("mine"));
        assert!(c.harness_ids().iter().any(|id| id == "mine"));
        assert_eq!(c.declared_adapters["mine"].name.as_deref(), Some("Mine"));
        assert!(c.knows_harness("claude-code"));
        assert!(c.knows_harness("codex"));
        std::fs::write(dir.path().join("harness.json"), "not json").unwrap();
        let c = cfg(&[("STEVE_HOME", home)]);
        assert!(!c.knows_harness("mine"));
        assert!(c.declared_adapters.is_empty());
    }

    #[test]
    fn bad_port_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Config::from_vars(|k| match k {
                "STEVE_HOME" => Some(dir.path().display().to_string()),
                "STEVE_PORT" => Some("abc".into()),
                v if v.starts_with("STEVE_ADAPTER_") => Some("x".into()),
                _ => None,
            })
            .is_err()
        );
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

    #[test]
    fn env_file_fills_missing_vars_without_overriding_process_env_and_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("hub");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("steve.env"),
            concat!(
                "# hub config\n",
                "STEVE_PORT=9123\n",
                "STEVE_HOST=127.0.0.1\n",
                "STEVE_WEB_DIR=$STEVE_HOME/web/current\n",
                "STEVE_OIDC_CLIENT_SECRET_FILE=$HOME/secret\n",
            ),
        )
        .unwrap();

        let existing = BTreeMap::from([
            ("HOME".into(), dir.path().display().to_string()),
            ("STEVE_HOME".into(), home.display().to_string()),
            ("STEVE_PORT".into(), "18888".into()),
        ]);
        let loaded = read_env_file_vars(&existing).unwrap();
        assert!(
            !loaded.contains_key("STEVE_PORT"),
            "explicit process env must win over steve.env"
        );
        assert_eq!(loaded["STEVE_HOST"], "127.0.0.1");
        assert_eq!(
            loaded["STEVE_WEB_DIR"],
            resolve(&home).join("web/current").display().to_string()
        );
        assert_eq!(
            loaded["STEVE_OIDC_CLIENT_SECRET_FILE"],
            dir.path().join("secret").display().to_string()
        );

        // Symlinked steve.env must be refused.
        let target = dir.path().join("other.env");
        std::fs::write(&target, "STEVE_PORT=9999\n").unwrap();
        let sym_home = dir.path().join("sym-hub");
        std::fs::create_dir_all(&sym_home).unwrap();
        std::os::unix::fs::symlink(&target, sym_home.join("steve.env")).unwrap();
        let err = read_env_file_vars(&BTreeMap::from([(
            "STEVE_HOME".into(),
            sym_home.display().to_string(),
        )]))
        .unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
    }

    #[test]
    fn default_web_dir_prefers_installed_web_current() {
        let dir = tempfile::tempdir().unwrap();
        let web_current = dir.path().join("web/current");
        std::fs::create_dir_all(&web_current).unwrap();
        std::fs::write(web_current.join("index.html"), "<!doctype html>").unwrap();
        let c = cfg(&[("STEVE_HOME", dir.path().to_str().unwrap())]);
        assert_eq!(c.web_dir, resolve(&web_current));
    }

    #[test]
    fn review593_4_steve_env_path_overrides_minimal_launchd_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("hub");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("steve.env"),
            "PATH=/opt/homebrew/bin:/custom/tools:$PATH\nSTEVE_PORT=9123\n",
        )
        .unwrap();
        // Simulate launchd passing a minimal PATH while steve.env declares Homebrew + custom tools.
        let existing = BTreeMap::from([
            ("STEVE_HOME".into(), home.display().to_string()),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("STEVE_PORT".into(), "18888".into()),
        ]);
        let loaded = read_env_file_vars(&existing).unwrap();
        assert_eq!(
            loaded.get("PATH").map(String::as_str),
            Some("/opt/homebrew/bin:/custom/tools:/usr/bin:/bin:/usr/sbin:/sbin"),
            "PATH in steve.env must override launchd's minimal inherited PATH"
        );
        assert!(
            !loaded.contains_key("STEVE_PORT"),
            "non-PATH explicit caller overrides must still win over steve.env"
        );
    }
}
