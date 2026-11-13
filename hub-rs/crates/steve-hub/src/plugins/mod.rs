//! Plugins: what is not part of Steve, installed from a plugin library: programs run beside the Hub
//! as processes of their own (channels), or data the web client reads (themes). A plugin never
//! links into the Hub; it is reached through one of Steve's documented contracts, so one that
//! crashes or is removed leaves the Hub as it was.
//!
//! # A plugin library
//!
//! A library is whatever serves an index file, `steve-store.json`. Its address is one of:
//!
//! - `https://github.com/<owner>/<repo>` (or `<owner>/<repo>`): the file at the root of the
//!   repository's default branch;
//! - the `https://` address of the index file itself (any host; plain `http://` only on
//!   `127.0.0.1` and `::1`);
//! - an absolute directory on the Hub's machine with the file in it (a library in development, or
//!   one that is not published).
//!
//! The index (format 1; unknown members are ignored):
//!
//! ```json
//! {
//!   "store": 1,
//!   "name": "Example plugins",
//!   "plugins": [{
//!     "id": "example",
//!     "name": "Example",
//!     "summary": {"en": "What it does", "zh-CN": "它做什么"},
//!     "kind": "channel",
//!     "version": "0.1.0",
//!     "contract": 1,
//!     "entry": "bin/example",
//!     "homepage": "https://example.org/plugin",
//!     "config": [
//!       {"key": "address", "label": "Service address", "hint": "wss://…"},
//!       {"key": "token", "label": "Access token", "secret": true}
//!     ],
//!     "artifacts": {
//!       "aarch64-apple-darwin": {"url": "https://example.org/example-0.1.0-aarch64-apple-darwin.tar.gz", "sha256": "…"}
//!     }
//!   }]
//! }
//! ```
//!
//! - `id`, a setting's `key`: a lowercase letter or digit, then lowercase letters, digits, `-`
//!   and `_`, at most 40 characters. `name`, `summary`, `label` and `hint` are a string or a map
//!   from language to string.
//! - `version` is a semantic version. The index names one version of each plugin: the one to
//!   install now.
//! - `artifacts` is keyed by the target a Hub was built for (`steve-hub --build-info` prints it). A
//!   plugin without an artifact for the Hub's target is listed and cannot be installed there.
//!   `url` is `https://`, or a path relative to the index (the only form a directory library
//!   uses). The archive is a `.tar.gz` of regular files and directories; `entry` is the
//!   executable in it. `sha256` is of the archive, and an archive that does not match is not
//!   installed.
//! - `kind` and `contract` say how the Hub runs it (below). A plugin whose kind or contract this
//!   Hub does not know is listed as needing a newer Steve.
//!
//! The Hub trusts a library as much as the instance's owner does by adding it: installing a plugin
//! runs its program as the Hub's user. Only the owner adds libraries and installs; what is
//! installed is exactly the archive the index named.
//!
//! # Running a channel plugin (`kind: "channel"`, contract 1)
//!
//! A channel plugin is a channel service of the external channel protocol
//! ([`steve_channels::adapters::ext_ws::protocol`]): it signs in to a messaging system as one
//! person and serves that protocol. Connecting it makes one *instance*: the settings a person
//! entered, one process, and one external-channel connection of theirs. The Hub starts the
//! `entry` with these variables and nothing else of its own environment:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `STEVE_PLUGIN_LISTEN` | `127.0.0.1:<port>`: where to serve the protocol, the WebSocket at `/ws` and media under `/v1/media/` |
//! | `STEVE_PLUGIN_TOKEN_FILE` | a file with the bearer token the Hub will present |
//! | `STEVE_PLUGIN_CONFIG` | a JSON file: an object of the instance's settings, each a string under its `key` |
//! | `STEVE_PLUGIN_DATA` | a directory that is the instance's to write (also its working directory) |
//! | `STEVE_PLUGIN_SUPERVISED` | `1`: the Hub holds standard input open; exit when it reaches end of file |
//!
//! Standard output and error go to the instance's log. A plugin that exits is started again after
//! a pause that grows to a minute; while it is down its connection is as any external channel
//! whose service is unreachable. To stop it the Hub closes its standard input and sends `SIGTERM`.
//! Disconnecting the connection (in Accounts) removes the instance and its data.
//!
//! # A theme plugin (`kind: "theme"`, contract 1)
//!
//! A skin for the web client. Nothing runs: `entry` is a theme manifest (a JSON object of at most
//! 64 KiB, the format of `web/src/themes/manifest.ts`) in the archive, and its artifact is usually
//! keyed `any`, the key an archive for every machine has. Installing it is installing it for
//! everyone on the Hub: the web client reads the installed manifests (`GET /plugins/themes`),
//! checks each one and registers it, and the theme is then a choice in Settings → Appearance on
//! every device. A theme plugin has no settings and is not connected.
//!
//! # On disk
//!
//! Everything is under `<STEVE_HOME>/plugins`, readable without a running Hub: `stores.json` (the
//! libraries and the index last read from each), `installed/<library>/<plugin>/installed.json`
//! beside the unpacked version, and `instances/<id>/` (`instance.json`, `config.json`, `token`,
//! `data/`, `plugin.log`).

pub mod api;
pub mod index;
mod install;
pub mod run;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::{Caller, Modules};
use zeroize::Zeroizing;

use self::index::{Address, Field, Index, Plugin};
use self::run::{Runner, Spec, Status};

/// The library a new installation starts with.
pub const OFFICIAL_LIBRARY: &str = "https://github.com/Infranite/steve-extensions";
/// Another first library, or `off` for none. Read only until the owner changes the list.
pub const LIBRARY_VAR: &str = "STEVE_PLUGIN_LIBRARY";
/// The adapter a channel plugin's connection is made with.
const CHANNEL_ADAPTER: &str = steve_channels::adapters::ext_ws::ADAPTER;
/// How long a plugin gets to start listening when it is connected.
const READY_WITHIN: Duration = Duration::from_secs(20);
/// An instance left without a connection for this long was a connect that never finished.
const ORPHAN_AFTER_MS: u64 = 10 * 60 * 1000;
const MAX_STORES: usize = 20;
const MAX_VALUE_BYTES: usize = 8 * 1024;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A library the owner added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreRec {
    pub id: String,
    pub address: String,
    pub added: u64,
    /// When its index was last read successfully.
    #[serde(default)]
    pub checked: Option<u64>,
    /// Why the last read failed; the index before it stays.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub index: Option<Index>,
}

/// What is installed of one plugin: the index entry it was installed from, so running it does
/// not depend on the library staying reachable or unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub plugin: Plugin,
    pub sha256: String,
    pub installed: u64,
}

/// One connected plugin: whose it is, where it listens and the connection it serves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub store: String,
    pub plugin: String,
    pub ws: String,
    pub me: String,
    pub port: u16,
    /// Set once the connection is made.
    #[serde(default)]
    pub connection: Option<String>,
    pub created: u64,
}

/// Why a request was refused, as the API answers it.
#[derive(Debug)]
pub enum Refused {
    /// 404: no such library or plugin.
    NotFound(String),
    /// A status with a sentence for the person.
    Rule(u16, String),
    /// The connection was not made: as the credential form answers it.
    Connection(Box<steve_modules::vault::VaultError>),
}

impl Refused {
    fn rule(msg: impl Into<String>) -> Self {
        Refused::Rule(400, msg.into())
    }
    fn conflict(msg: impl Into<String>) -> Self {
        Refused::Rule(409, msg.into())
    }
}

impl From<anyhow::Error> for Refused {
    fn from(e: anyhow::Error) -> Self {
        Refused::Rule(400, format!("{e:#}"))
    }
}

impl From<std::io::Error> for Refused {
    fn from(e: std::io::Error) -> Self {
        Refused::Rule(500, e.to_string())
    }
}

struct Inner {
    dir: PathBuf,
    client: reqwest::Client,
    target: String,
    /// The first library of an installation whose list was never changed.
    first: Option<String>,
    /// One change to the libraries or to what is installed at a time.
    change: tokio::sync::Mutex<()>,
    runner: Runner,
}

#[derive(Clone)]
pub struct Plugins(Arc<Inner>);

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    write_private(path, &serde_json::to_vec_pretty(v)?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(
        &std::fs::read(path).with_context(|| path.display().to_string())?,
    )?)
}

fn subdirs(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

impl Plugins {
    /// `home` is `STEVE_HOME`; `first` the library a new installation starts with
    /// ([`LIBRARY_VAR`], else [`OFFICIAL_LIBRARY`]).
    pub fn new(home: &Path, first: Option<String>) -> Result<Plugins> {
        let first = match first.as_deref().map(str::trim) {
            Some("off") | Some("") => None,
            Some(a) => Some(a.to_string()),
            None => Some(OFFICIAL_LIBRARY.to_string()),
        };
        Ok(Plugins(Arc::new(Inner {
            dir: home.join("plugins"),
            client: install::client()?,
            target: env!("STEVE_BUILD_TARGET").to_string(),
            first,
            change: tokio::sync::Mutex::new(()),
            runner: Runner::default(),
        })))
    }

    /// The target this Hub installs artifacts for.
    pub fn target(&self) -> &str {
        &self.0.target
    }

    fn stores_file(&self) -> PathBuf {
        self.0.dir.join("stores.json")
    }

    fn plugin_dir(&self, store: &str, plugin: &str) -> PathBuf {
        self.0.dir.join("installed").join(store).join(plugin)
    }

    fn instance_dir(&self, id: &str) -> PathBuf {
        self.0.dir.join("instances").join(id)
    }

    /// The libraries: the stored list, or the first library of an installation that never
    /// changed it (not yet read; nothing is fetched until the owner asks).
    pub fn stores(&self) -> Result<Vec<StoreRec>> {
        let file = self.stores_file();
        if file.exists() {
            return read_json(&file);
        }
        Ok(self
            .0
            .first
            .as_deref()
            .and_then(|a| Address::parse(a).ok())
            .map(|a| StoreRec {
                id: a.slug(),
                address: a.display(),
                added: 0,
                checked: None,
                error: None,
                index: None,
            })
            .into_iter()
            .collect())
    }

    fn store(&self, id: &str) -> Result<StoreRec, Refused> {
        self.stores()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| Refused::NotFound(steve_i18n::t!("hub.plugins.no_library")))
    }

    async fn fetch_index(&self, address: &Address) -> Result<Index> {
        let bytes = install::read_index(&self.0.client, &address.index_location()).await?;
        index::parse(&bytes)
    }

    /// Adds a library: its index is read first, and an address without a readable index is not
    /// added.
    pub async fn add_store(&self, input: &str) -> Result<StoreRec, Refused> {
        let address = Address::parse(input)?;
        let _g = self.0.change.lock().await;
        let mut stores = self.stores()?;
        if stores.iter().any(|s| s.address == address.display()) {
            return Err(Refused::conflict(steve_i18n::t!(
                "hub.plugins.library_twice"
            )));
        }
        if stores.len() >= MAX_STORES {
            return Err(Refused::rule(steve_i18n::t!(
                "hub.plugins.too_many_libraries"
            )));
        }
        let index = self.fetch_index(&address).await?;
        let slug = address.slug();
        let mut id = slug.clone();
        let mut n = 2;
        while stores.iter().any(|s| s.id == id) || self.0.dir.join("installed").join(&id).exists() {
            id = format!("{slug}-{n}");
            n += 1;
        }
        let rec = StoreRec {
            id,
            address: address.display(),
            added: now(),
            checked: Some(now()),
            error: None,
            index: Some(index),
        };
        stores.push(rec.clone());
        write_json(&self.stores_file(), &stores)?;
        Ok(rec)
    }

    /// Reads a library's index again. A failed read is recorded on the library and the index it
    /// had stays, so what is installed keeps its description.
    pub async fn refresh(&self, id: &str) -> Result<StoreRec, Refused> {
        let _g = self.0.change.lock().await;
        let mut stores = self.stores()?;
        let rec = stores
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| Refused::NotFound(steve_i18n::t!("hub.plugins.no_library")))?;
        let address = Address::parse(&rec.address)?;
        match self.fetch_index(&address).await {
            Ok(index) => {
                rec.index = Some(index);
                rec.checked = Some(now());
                rec.error = None;
            }
            Err(e) => rec.error = Some(format!("{e:#}")),
        }
        let out = rec.clone();
        write_json(&self.stores_file(), &stores)?;
        Ok(out)
    }

    /// Removes a library nothing is installed from.
    pub async fn remove_store(&self, id: &str) -> Result<(), Refused> {
        let _g = self.0.change.lock().await;
        let mut stores = self.stores()?;
        if !stores.iter().any(|s| s.id == id) {
            return Err(Refused::NotFound(steve_i18n::t!("hub.plugins.no_library")));
        }
        if !self.installed_in(id).is_empty() {
            return Err(Refused::conflict(steve_i18n::t!(
                "hub.plugins.library_in_use"
            )));
        }
        stores.retain(|s| s.id != id);
        write_json(&self.stores_file(), &stores)?;
        Ok(())
    }

    pub fn installed(&self, store: &str, plugin: &str) -> Option<Installed> {
        if !index::id_ok(store) || !index::id_ok(plugin) {
            return None;
        }
        read_json(&self.plugin_dir(store, plugin).join("installed.json")).ok()
    }

    fn installed_in(&self, store: &str) -> Vec<Installed> {
        subdirs(&self.0.dir.join("installed").join(store))
            .iter()
            .filter_map(|p| self.installed(store, p))
            .collect()
    }

    /// Installs the version the library's index names now, over whatever version was installed,
    /// and starts the plugin's instances again on it.
    pub async fn install(&self, store: &str, plugin: &str) -> Result<Installed, Refused> {
        let _g = self.0.change.lock().await;
        let rec = self.store(store)?;
        let p = rec
            .index
            .as_ref()
            .and_then(|i| i.plugins.iter().find(|p| p.id == plugin))
            .ok_or_else(|| Refused::NotFound(steve_i18n::t!("hub.plugins.no_plugin")))?;
        if p.unsupported().is_some() {
            return Err(Refused::rule(steve_i18n::t!("hub.plugins.needs_newer")));
        }
        let artifact = p.artifact(self.target()).ok_or_else(|| {
            Refused::rule(steve_i18n::t!(
                "hub.plugins.no_build",
                target = self.target()
            ))
        })?;
        let address = Address::parse(&rec.address)?;
        let at = address.artifact_location(&artifact.url)?;
        let dir = self.plugin_dir(store, plugin);
        install::install(
            &self.0.client,
            &at,
            &artifact.sha256,
            &dir,
            &p.version,
            &p.entry,
            p.runs(),
            if p.runs() { no_check } else { check_theme },
        )
        .await?;
        let done = Installed {
            plugin: p.clone(),
            sha256: artifact.sha256.to_ascii_lowercase(),
            installed: now(),
        };
        write_json(&dir.join("installed.json"), &done)?;
        for old in subdirs(&dir) {
            if old != p.version {
                let _ = std::fs::remove_dir_all(dir.join(old));
            }
        }
        for i in self.instances() {
            if i.store == store && i.plugin == plugin {
                self.0.runner.restart(self.spec(&i)).await;
            }
        }
        tracing::info!(store, plugin, version = %p.version, "plugin installed");
        Ok(done)
    }

    /// Removes an installed plugin nobody is connected through.
    pub async fn uninstall(&self, store: &str, plugin: &str) -> Result<(), Refused> {
        let _g = self.0.change.lock().await;
        if self.installed(store, plugin).is_none() {
            return Err(Refused::NotFound(steve_i18n::t!(
                "hub.plugins.not_installed"
            )));
        }
        if self
            .instances()
            .iter()
            .any(|i| i.store == store && i.plugin == plugin)
        {
            return Err(Refused::conflict(steve_i18n::t!(
                "hub.plugins.plugin_in_use"
            )));
        }
        std::fs::remove_dir_all(self.plugin_dir(store, plugin))?;
        tracing::info!(store, plugin, "plugin uninstalled");
        Ok(())
    }

    pub fn instances(&self) -> Vec<Instance> {
        let dir = self.0.dir.join("instances");
        subdirs(&dir)
            .iter()
            .filter_map(|id| read_json(&dir.join(id).join("instance.json")).ok())
            .collect()
    }

    fn spec(&self, i: &Instance) -> Spec {
        let this = self.clone();
        let (store, plugin) = (i.store.clone(), i.plugin.clone());
        Spec {
            id: i.id.clone(),
            dir: self.instance_dir(&i.id),
            port: i.port,
            exe: Box::new(move || {
                let done = this
                    .installed(&store, &plugin)
                    .ok_or_else(|| anyhow!("{store}/{plugin} is not installed"))?;
                Ok(this
                    .plugin_dir(&store, &plugin)
                    .join(&done.plugin.version)
                    .join(&done.plugin.entry))
            }),
        }
    }

    async fn drop_instance(&self, id: &str) {
        self.0.runner.stop(id).await;
        if let Err(e) = std::fs::remove_dir_all(self.instance_dir(id))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(instance = id, "plugin instance not removed: {e}");
        }
    }

    /// The settings as the plugin's index entry asks for them.
    fn check_values(
        fields: &[Field],
        mut values: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, Refused> {
        let mut out = BTreeMap::new();
        let mut wrong = Vec::new();
        for f in fields {
            let v = values.remove(&f.key).unwrap_or_default();
            let v = v.trim();
            if v.len() > MAX_VALUE_BYTES || (v.is_empty() && f.required) {
                wrong.push(f.key.clone());
            } else if !v.is_empty() {
                out.insert(f.key.clone(), v.to_string());
            }
        }
        wrong.extend(values.into_keys());
        if wrong.is_empty() {
            Ok(out)
        } else {
            Err(Refused::rule(steve_i18n::t!(
                "hub.plugins.settings_wrong",
                fields = wrong.join(", ")
            )))
        }
    }

    /// Connects an installed channel plugin for `who`: an instance with their settings is
    /// started, and once it listens an external-channel connection to it is made the way the
    /// credential form makes one (the Hub says hello and asks who is signed in). Anything that
    /// fails takes the instance with it.
    pub async fn connect(
        &self,
        conns: &Connections,
        who: &Caller,
        store: &str,
        plugin: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Value, Refused> {
        let done = self
            .installed(store, plugin)
            .ok_or_else(|| Refused::NotFound(steve_i18n::t!("hub.plugins.not_installed")))?;
        if !done.plugin.runs() {
            return Err(Refused::rule(steve_i18n::t!("hub.plugins.not_connectable")));
        }
        let values = Self::check_values(&done.plugin.config, values)?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut inst = Instance {
            id: uuid::Uuid::new_v4().simple().to_string(),
            store: store.into(),
            plugin: plugin.into(),
            ws: who.ws.clone(),
            me: who.me.clone(),
            port: run::free_port()?,
            connection: None,
            created: now(),
        };
        let dir = self.instance_dir(&inst.id);
        let written = (|| {
            write_json(&dir.join("config.json"), &values)?;
            write_private(&dir.join("token"), token.as_bytes())?;
            write_json(&dir.join("instance.json"), &inst)
        })();
        if let Err(e) = written {
            self.drop_instance(&inst.id).await;
            return Err(e.into());
        }
        self.0.runner.ensure(self.spec(&inst));
        if let Err(why) = self.0.runner.ready(&inst.id, inst.port, READY_WITHIN).await {
            let tail = run::log_tail(&dir, 5);
            self.drop_instance(&inst.id).await;
            let detail = if tail.is_empty() {
                why
            } else {
                format!("{why}\n{tail}")
            };
            return Err(Refused::Rule(
                502,
                steve_i18n::t!("hub.plugins.not_started", detail = detail),
            ));
        }
        let fields: BTreeMap<String, Zeroizing<String>> = [
            (
                steve_channels::adapters::ext_ws::URL.to_string(),
                Zeroizing::new(format!("ws://127.0.0.1:{}/ws", inst.port)),
            ),
            (
                steve_channels::adapters::ext_ws::TOKEN.to_string(),
                Zeroizing::new(token),
            ),
        ]
        .into();
        let conn = match conns.create(who, CHANNEL_ADAPTER, &fields, json!({})).await {
            Ok(c) => c,
            Err(e) => {
                self.drop_instance(&inst.id).await;
                return Err(Refused::Connection(Box::new(e)));
            }
        };
        // the same account connected again is the same connection: the instance before it goes
        for other in self.instances() {
            if other.id != inst.id && other.connection.as_deref() == Some(conn.id.as_str()) {
                self.drop_instance(&other.id).await;
            }
        }
        inst.connection = Some(conn.id.clone());
        if let Err(e) = write_json(&dir.join("instance.json"), &inst) {
            tracing::error!(instance = %inst.id, "plugin instance not recorded: {e}");
        }
        tracing::info!(store, plugin, connection = %conn.id, "plugin connected");
        Ok(json!({"instance": inst.id, "connection": conn.id}))
    }

    /// Brings the processes in line with the instances on disk: every instance whose connection
    /// still exists runs, one whose connection was disconnected is removed with its data, and
    /// so is one a connect left behind unfinished.
    pub async fn reconcile(&self, modules: &Modules) {
        for i in self.instances() {
            let gone = match &i.connection {
                Some(c) => match connection_exists(modules, c) {
                    Ok(exists) => !exists,
                    Err(e) => {
                        tracing::warn!(instance = %i.id, "plugin connection not looked up: {e}");
                        false
                    }
                },
                None => now().saturating_sub(i.created) > ORPHAN_AFTER_MS,
            };
            if gone {
                tracing::info!(instance = %i.id, plugin = %i.plugin, "plugin instance removed");
                self.drop_instance(&i.id).await;
            } else if i.connection.is_some() {
                self.0.runner.ensure(self.spec(&i));
            }
        }
    }

    /// The job the Hub runs: reconciles now, whenever a connection changes (`events`, the bus's
    /// `connection` events) and every 20 seconds.
    pub async fn supervise(self, modules: Modules, mut events: tokio::sync::mpsc::Receiver<Value>) {
        let mut open = true;
        loop {
            self.reconcile(&modules).await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(20)) => {}
                ev = events.recv(), if open => open = ev.is_some(),
            }
            // one pass answers every change that is already waiting
            while events.try_recv().is_ok() {}
        }
    }

    /// `GET /plugins/themes`: the manifest of every installed theme plugin, as its archive carries
    /// it. The web client checks and registers each one; the Hub only knows it is a JSON object.
    pub fn themes(&self) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for s in subdirs(&self.0.dir.join("installed")) {
            for d in self.installed_in(&s) {
                if d.plugin.kind != index::KIND_THEME {
                    continue;
                }
                let file = self
                    .plugin_dir(&s, &d.plugin.id)
                    .join(&d.plugin.version)
                    .join(&d.plugin.entry);
                let manifest = read_theme(&file).unwrap_or_else(|e| {
                    tracing::warn!(store = %s, plugin = %d.plugin.id, "theme not read: {e:#}");
                    Value::Null
                });
                out.push(json!({
                    "store": s, "plugin": d.plugin.id, "name": d.plugin.name,
                    "version": d.plugin.version, "manifest": manifest,
                }));
            }
        }
        Ok(out)
    }

    pub fn status(&self, instance: &str) -> Status {
        self.0.runner.status(instance)
    }

    /// Stops every plugin process (tests, and a Hub that is shutting down in order).
    pub async fn stop_all(&self) {
        self.0.runner.stop_all().await;
    }

    /// `GET /plugins`: the libraries with their plugins, what is installed and the caller's own
    /// instances. Addresses and read errors are the owner's to see (a directory library's address
    /// is a path on the Hub's machine).
    pub fn list(&self, who: &Caller, owner: bool) -> Result<Value> {
        let instances = self.instances();
        let mut stores = Vec::new();
        for s in self.stores()? {
            let mut plugins: Vec<Value> = Vec::new();
            let listed: Vec<&Plugin> = s.index.iter().flat_map(|i| &i.plugins).collect();
            let installed = self.installed_in(&s.id);
            let entry = |p: &Plugin, listed: bool| {
                let done = installed.iter().find(|d| d.plugin.id == p.id);
                let mine: Vec<Value> = instances
                    .iter()
                    .filter(|i| i.store == s.id && i.plugin == p.id && i.ws == who.ws && i.me == who.me)
                    .map(|i| json!({"instance": i.id, "connection": i.connection, "status": self.status(&i.id)}))
                    .collect();
                json!({
                    "id": p.id, "name": p.name, "summary": p.summary, "kind": p.kind,
                    "version": p.version, "homepage": p.homepage, "config": p.config,
                    "listed": listed,
                    "unsupported": p.unsupported(),
                    "available": p.artifact(self.target()).is_some(),
                    "installed": done.map(|d| json!({"version": d.plugin.version, "installed": d.installed, "config": d.plugin.config})),
                    "update": done.is_some_and(|d| listed && p.semver() > d.plugin.semver()),
                    "connections": mine,
                })
            };
            for p in &listed {
                plugins.push(entry(p, true));
            }
            // installed from an index that no longer lists it: still there, still running
            for d in &installed {
                if !listed.iter().any(|p| p.id == d.plugin.id) {
                    plugins.push(entry(&d.plugin, false));
                }
            }
            let mut v = json!({
                "id": s.id,
                "name": s.index.as_ref().map(|i| &i.name),
                "checked": s.checked,
                "official": s.address == OFFICIAL_LIBRARY,
                "plugins": plugins,
            });
            if owner {
                v["address"] = json!(s.address);
                v["error"] = json!(s.error);
            } else {
                v["failed"] = json!(s.error.is_some());
            }
            stores.push(v);
        }
        Ok(json!({"target": self.target(), "owner": owner, "stores": stores}))
    }
}

/// The largest theme manifest a theme plugin carries.
const MAX_THEME_BYTES: u64 = 64 * 1024;

/// A theme manifest as the Hub reads it: a JSON object of at most 64 KiB with a string `id`. What
/// it may set is the web client's to check (`web/src/themes/manifest.ts`).
fn read_theme(file: &Path) -> Result<Value> {
    let size = std::fs::metadata(file)
        .with_context(|| file.display().to_string())?
        .len();
    if size > MAX_THEME_BYTES {
        anyhow::bail!("the theme manifest is larger than 64 KiB")
    }
    let v: Value =
        serde_json::from_slice(&std::fs::read(file)?).context("the theme manifest is not JSON")?;
    if !v.get("id").is_some_and(Value::is_string) {
        anyhow::bail!("the theme manifest is not an object with an id")
    }
    Ok(v)
}

fn no_check(_: &Path) -> Result<()> {
    Ok(())
}

fn check_theme(file: &Path) -> Result<()> {
    read_theme(file).map(|_| ())
}

fn connection_exists(modules: &Modules, id: &str) -> Result<bool, steve_store::StoreError> {
    use rusqlite::OptionalExtension;
    modules.store.reader(|c| {
        Ok(
            c.query_row("SELECT 1 FROM connections WHERE id = ?", [id], |_| Ok(()))
                .optional()?
                .is_some(),
        )
    })
}

#[cfg(test)]
mod tests;
