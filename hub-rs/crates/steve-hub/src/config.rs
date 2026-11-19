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
    pub async fn bind_port(&self, port: u16) -> std::io::Result<Vec<tokio::net::TcpListener>> {
        let mut out = Vec::with_capacity(self.hosts.len());
        for h in &self.hosts {
            let l = tokio::net::TcpListener::bind((h.as_str(), port))
                .await
                .map_err(|e| std::io::Error::new(e.kind(), format!("listen on {h}:{port}: {e}")))?;
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
    pub cache: steve_runtime::cache::CacheConfig,
    pub caches: steve_runtime::cache::Caches,
    pub data_dir: PathBuf,
    pub worktrees: PathBuf,
    pub scratch: PathBuf,
    pub listen: Listen,
    pub metrics: Option<crate::metrics::Listen>,
    /// Primary public origin, trailing `/` stripped; decides the cookie `Secure` flag.
    pub public_url: String,
    /// Every origin Steve is served on; OIDC login returns to the one the browser used.
    pub public_urls: Vec<String>,
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
    /// Don't resume interrupted turns or send held prompts at boot (set when running on a copy of a real database).
    pub no_resume: bool,
    /// Opt in to dynamic collaboration (`STEVE_DYNAMIC_WORKFLOWS=1`, default off).
    pub dynamic_workflows: bool,
    /// The daily copy under `<home>/backups` (`STEVE_BACKUP=off` turns it off): see `daily_backup`.
    pub backup: bool,
    /// `STEVE_TAKEOVER_CONTINUES`: how often Steve continues a stopped agent before the user hears of it
    /// (`None`: the core's default).
    pub takeover_continues: Option<u32>,
    /// `STEVE_TAKEOVER_SAME_REFUSALS`: how many refusals of a delivery in a row for the same reason
    /// hand the item to the user instead of another continue; 0: never (`None`: the core's default).
    pub takeover_same_refusals: Option<u32>,
    /// `STEVE_DISK_FLOOR_GB`, `STEVE_DISK_LOW_GB`, `STEVE_WORKTREE_IDLE_HOURS` (task workspaces).
    pub disk: steve_core::workspace::DiskPolicy,
    /// `STEVE_GH`: the GitHub CLI that follows Release builds (`None`: looked up by the core).
    pub gh: Option<PathBuf>,
    pub claude_executable: String,
    /// `CODEX_EXECUTABLE`: `codex exec` for Steve's judgments when they run on Codex.
    pub codex_executable: String,
    /// Adapter entry point per harness id; a `.js` path runs with `node`.
    pub adapters: BTreeMap<String, String>,
    /// What the Hub's harness manifest declares about adapters beyond their command
    /// (`STEVE_HARNESS_MANIFEST`, default `<STEVE_HOME>/harness.json`; [`steve_acp::adapters`]).
    pub declared_adapters: BTreeMap<String, steve_acp::Declaration>,
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

/// `npm root -g`, or the Homebrew default when npm is missing.
fn npm_root() -> String {
    // npm's global prefix is the directory above `node`'s bin unless an npmrc moves it; when that
    // directory is there, that is what `npm root -g` would print, without spawning npm at boot
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
        && root.is_dir()
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
/// The adapters a harness manifest (`{"adapters": {…}}`, as a Node's `harness.json`) lists. A file
/// that is missing lists none; one that does not read is logged and ignored, never a reason not to
/// start.
fn harness_manifest(
    path: &Path,
) -> (
    std::collections::HashMap<String, PathBuf>,
    BTreeMap<String, steve_acp::Declaration>,
) {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        adapters: std::collections::HashMap<String, steve_acp::AdapterEntry>,
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(e) => {
            tracing::warn!("harness manifest {}: {e}; ignored", path.display());
            return Default::default();
        }
    };
    match serde_json::from_str::<Manifest>(&text) {
        Ok(m) => {
            let (commands, declared) = steve_acp::adapters::split_entries(m.adapters);
            (commands, declared.into_iter().collect())
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

impl Config {
    /// Reads the process environment and creates the Hub's data and working directories.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut cfg = Self::from_vars(|k| std::env::var(k).ok())?;
        cfg.caches = steve_runtime::cache::Caches::configure_process(cfg.cache);
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

        let port_raw = env("STEVE_PORT", "8440");
        let port_n = port_raw
            .parse::<u16>()
            .ok()
            .filter(|_| !port_raw.is_empty() && port_raw.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(ConfigError::Invalid {
                var: "STEVE_PORT",
                msg: "expected a decimal port from 0 to 65535".into(),
            })?;

        // unset: this machine, on the port the Hub listens on
        let public_raw =
            get("STEVE_PUBLIC_URL").unwrap_or_else(|| format!("http://127.0.0.1:{port_n}"));
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
        let public_urls: Vec<String> = get("STEVE_PUBLIC_URLS")
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
        let client_secret = steve_auth::AuthConfig::secret(
            &get,
            "STEVE_OIDC_CLIENT_SECRET",
            steve_auth::SecretSource::FileOrDirect,
        )?;
        let issuer = get("STEVE_OIDC_ISSUER");
        if let Some(url) = &issuer {
            validate_url("STEVE_OIDC_ISSUER", url)?;
        }
        // Validate sources even when the optional backend is disabled.
        let openviking_key = steve_auth::AuthConfig::secret(
            &get,
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

        // the harness manifest: adapters of one's own, and commands for the built-in ones where no
        // STEVE_ADAPTER_* names one
        let manifest = get("STEVE_HARNESS_MANIFEST")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("harness.json"));
        let (commands, declared_adapters) = harness_manifest(&manifest);
        let manifest_adapters: std::collections::BTreeSet<String> =
            commands.keys().cloned().collect();
        // npm root -g only when an npm-managed adapter default is actually needed
        let mut root: Option<String> = None;
        let path_var = get("PATH");
        let adapters: BTreeMap<String, String> = steve_acp::resolve_builtin_adapters(
            &home,
            &commands,
            |var| get(var).filter(|v| !v.is_empty()).map(PathBuf::from),
            path_var.as_deref().map(std::ffi::OsStr::new),
            || PathBuf::from(root.get_or_insert_with(npm_root).as_str()),
        )
        .into_iter()
        .map(|(id, c)| (id, c.to_string_lossy().into_owned()))
        .collect();

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

        let cache_mode = match get("STEVE_CACHE").as_deref() {
            None | Some("memory") => steve_runtime::cache::CacheMode::Memory,
            Some("off") => steve_runtime::cache::CacheMode::Off,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "STEVE_CACHE",
                    msg: "this version does not support this backend; expected memory or off"
                        .into(),
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
        let cache = steve_runtime::cache::CacheConfig {
            mode: cache_mode,
            memory_mb,
        };
        Ok(Config {
            caches: steve_runtime::cache::Caches::new(cache),
            cache: steve_runtime::cache::CacheConfig {
                mode: cache_mode,
                memory_mb,
            },
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
            web_port,
            client_origins,
            web_dir: get("STEVE_WEB_DIR").map(PathBuf::from).unwrap_or_else(|| {
                // TS resolves ../../web/dist from hub/src; the Rust crate sits two levels deeper
                resolve(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/dist"))
            }),
            oidc: Oidc {
                issuer,
                client_id: get("STEVE_OIDC_CLIENT_ID"),
                client_secret,
            },
            dev_login: steve_auth::flag(&get, "STEVE_DEV_LOGIN")?,
            no_resume: steve_auth::flag(&get, "STEVE_NO_RESUME")?,
            dynamic_workflows: steve_auth::flag(&get, "STEVE_DYNAMIC_WORKFLOWS")?,
            backup,
            takeover_continues: count(&get, "STEVE_TAKEOVER_CONTINUES")?,
            takeover_same_refusals: count(&get, "STEVE_TAKEOVER_SAME_REFUSALS")?,
            disk,
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
            declared_adapters,
            manifest_adapters,
            harness_manifest: manifest,
            models: Models {
                // none by default: every call follows the harness's own default model
                fast: split(env("STEVE_MODELS_FAST", "")),
                smart: split(env("STEVE_MODELS_SMART", "")),
            },
            // a new agent session's model only when set; else the harness's own default
            default_models: steve_acp::adapters::builtin_commands(Path::new(""))
                .filter_map(|(id, var, _)| {
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
            debug_route: steve_auth::flag(&get, "STEVE_DEBUG_ROUTE")?,
            codex_home: get("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join(".codex")),
            // the Node.js binary for `.js` adapters; the Steve Node switch is STEVE_NODE_MODE
            node: get("STEVE_NODE").unwrap_or_else(|| "node".into()),
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

    /// The harness adapters the Hub knows (built-in, and its manifest's), sorted: what a bot may
    /// run on.
    pub fn harness_ids(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
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
        for (_, var, _) in steve_acp::adapters::builtin_commands(Path::new("")) {
            m.entry(var.into()).or_insert("/x/adapter".into());
        }
        Config::from_vars(|k| m.get(k).cloned()).expect("config")
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
    fn dynamic_workflows_require_explicit_one() {
        assert!(!cfg(&[]).dynamic_workflows);
        assert!(!cfg(&[("STEVE_DYNAMIC_WORKFLOWS", "0")]).dynamic_workflows);
        assert!(cfg(&[("STEVE_DYNAMIC_WORKFLOWS", "1")]).dynamic_workflows);
    }

    #[test]
    fn steve_node_is_only_the_node_js_binary() {
        // the Steve Node's switch has its own name and never reaches the adapters' Node.js
        assert_eq!(cfg(&[("STEVE_NODE_MODE", "local")]).node, "node");
        assert_eq!(
            cfg(&[("STEVE_NODE", "/opt/x/bin/node")]).node,
            "/opt/x/bin/node"
        );
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
    fn defaults_match_ts() {
        let c = cfg(&[("STEVE_HOME", "/tmp/sh/./a/../b")]);
        assert_eq!(c.home, PathBuf::from("/tmp/sh/b"));
        assert_eq!(c.data_dir, PathBuf::from("/tmp/sh/b/data"));
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
        assert_eq!(c.claude_executable, "/opt/homebrew/bin/claude");
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
        assert_eq!(c.adapters["mine"], "/m/mine");
        assert_eq!(c.declared_adapters["mine"].name.as_deref(), Some("Mine"));
        // the test's explicit STEVE_ADAPTER_* stay
        assert_eq!(c.adapters["claude-code"], "/x/claude.js");
        assert_eq!(c.adapters["codex"], "/x/codex");
        std::fs::write(dir.path().join("harness.json"), "not json").unwrap();
        let c = cfg(&[("STEVE_HOME", home)]);
        assert!(!c.adapters.contains_key("mine"));
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
}
