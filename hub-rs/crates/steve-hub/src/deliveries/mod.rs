//! Immutable versions received by the Hub. Sources are read through the Node; only objects
//! owned by this service are opened here. Database rows become visible after every object is
//! synced. A failed receive can leave unreferenced objects, never a partial published delivery.
mod bundle;
mod receive;
mod replies;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use steve_store::delivery_repo::Delivery;
use steve_store::delivery_repo::Object;
use steve_store::{Read, Store, StoreError, delivery_repo as repo};

pub const MAX_FILE: usize = 64 * 1024 * 1024;
const HTML_WARNING: &str = "HTML is shown in a static sandbox: scripts, forms, frames and external connections are disabled. A user who manages the delivery can turn on its interactive preview, which runs the package's scripts in an isolated sandbox with no access to Steve and no network.";
const INTERACTIVE_WARNING: &str = "HTML runs in the interactive preview: the package's scripts run in an isolated sandbox with no access to Steve, its cookies or storage, and no network beyond the package's own files.";
const LOCAL_WARNING: &str = "The configured STEVE_PUBLIC_URL is a loopback or local-only address. This link cannot be used from another device; configure a browser-reachable installation address.";
const REACHABILITY_WARNING: &str = "This URL uses the configured STEVE_PUBLIC_URL; reachability from other devices has not been verified.";
/// A frame grant lives this long; the page asks for a new one each time it loads the metadata.
const FRAME_TTL_MS: i64 = 12 * 3600 * 1000;
const FRAME_GRANTS_MAX: usize = 10_000;
/// How much of an HTML entry is scanned for scripts.
const SCRIPT_SCAN: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct Service {
    pub store: Store,
    home: PathBuf,
    pub base: String,
    /// Origins allowed to embed an interactive frame (`frame-ancestors`): this installation's
    /// addresses and its configured web client origins.
    ancestors: Vec<String>,
    /// Unguessable, short-lived capabilities for one user's interactive frame of one delivery.
    /// They live in memory only: a restart makes the page fetch new ones.
    frames: Arc<Mutex<HashMap<String, FrameGrant>>>,
}

#[derive(Debug, Clone)]
struct FrameGrant {
    delivery: String,
    user: String,
    expires: i64,
}

/// Who receives a delivery: a Bot's session (inside or outside an item), or a signed-in user.
#[derive(Clone, Copy)]
pub enum Publisher<'a> {
    Session(&'a steve_store::Session),
    User { id: &'a str, workspace: &'a str },
}

fn html_entry(d: &Delivery) -> bool {
    match &d.entrypoint {
        Some(e) => html_name(e),
        None => d.mime == "text/html",
    }
}

fn html_name(p: &str) -> bool {
    let p = p.to_ascii_lowercase();
    p.ends_with(".html") || p.ends_with(".htm")
}

/// Lower-cased HTML that asks for scripts: a script element or an inline event handler.
fn scripts_in(lower: &str) -> bool {
    lower.contains("<script") || inline_handler(lower)
}

/// ` onclick=`, `\tonload =`: an attribute that runs code when the page is used.
fn inline_handler(lower: &str) -> bool {
    let b = lower.as_bytes();
    let mut i = 0;
    while let Some(at) = lower[i..].find("on") {
        let start = i + at;
        i = start + 2;
        if start == 0 || !b[start - 1].is_ascii_whitespace() {
            continue;
        }
        let mut j = i;
        while j < b.len() && b[j].is_ascii_lowercase() {
            j += 1;
        }
        if j - i < 3 {
            continue;
        }
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && b[j] == b'=' {
            return true;
        }
    }
    false
}

/// `scheme://host[:port]` of an http(s) URL.
fn origin(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}

#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub token: String,
    pub url: String,
    pub expires_at: i64,
    pub created: i64,
    pub revoked_at: Option<i64>,
}

impl Service {
    pub fn new(store: Store, home: PathBuf, base: String) -> Self {
        let base = base.trim_end_matches('/').to_string();
        Self {
            ancestors: origin(&base).into_iter().collect(),
            store,
            home,
            base,
            frames: Arc::default(),
        }
    }

    /// Adds origins that may embed interactive frames (other public addresses, web client origins).
    pub fn with_ancestors<'a>(mut self, urls: impl IntoIterator<Item = &'a str>) -> Self {
        for o in urls.into_iter().filter_map(origin) {
            if !self.ancestors.contains(&o) {
                self.ancestors.push(o);
            }
        }
        self
    }

    pub fn url(&self, id: &str, workspace: &str) -> String {
        let mut url = reqwest::Url::parse(&format!("{}/deliveries/{id}", self.base))
            .expect("configured public URL");
        // The page must bind this selector BEFORE the Web bootstrap picks the remembered workspace.
        // Headers or query selectors on API requests still go through normal membership checks.
        url.query_pairs_mut().append_pair("ws", workspace);
        url.into()
    }

    /// A resource URL below `api` (`…/content/` or `…/frame/`), one escaped segment per path part.
    fn resource(api: &str, kind: &str, path: &str) -> reqwest::Url {
        let mut u = reqwest::Url::parse(&format!("{api}/{kind}/")).expect("configured public URL");
        {
            let mut segments = u.path_segments_mut().expect("HTTP URL");
            segments.pop_if_empty();
            for part in path.split('/') {
                segments.push(part);
            }
        }
        u
    }

    pub fn metadata(&self, d: &Delivery, share: Option<&str>) -> Value {
        let (url, api) = match share {
            Some(t) => (
                format!("{}/shares/{t}", self.base),
                format!("{}/api/delivery-shares/{t}", self.base),
            ),
            None => (
                self.url(&d.id, &d.workspace_id),
                format!("{}/api/file-deliveries/{}", self.base, d.id),
            ),
        };
        let path = d.entrypoint.as_deref().unwrap_or(&d.name);
        let mut content = Self::resource(&api, "content", path);
        // Every authenticated resource URL carries its workspace, also when it cannot set a
        // header (PDF viewers, images and iframe navigations). Cookie workspace changes cannot
        // silently redirect it into another workspace.
        let suffix = if share.is_none() {
            let mut u = reqwest::Url::parse(&api).expect("configured public URL");
            u.query_pairs_mut().append_pair("ws", &d.workspace_id);
            content.query_pairs_mut().append_pair("ws", &d.workspace_id);
            format!("?{}", u.query().unwrap_or_default())
        } else {
            String::new()
        };
        let mut v = serde_json::to_value(d).expect("delivery JSON");
        if share.is_none() {
            v["workspace_id"] = json!(d.workspace_id);
        }
        v["url"] = json!(url);
        v["download_url"] = json!(format!("{api}/raw{suffix}"));
        v["content_url"] = if d.entrypoint.is_none() && d.mime == "application/zip" {
            json!(format!("{api}/raw{suffix}"))
        } else {
            json!(content.as_str())
        };
        let html = html_entry(d);
        // Recorded when published; computed (bounded) only for versions published before that.
        let scripts = html
            && d.preview["scripts"]
                .as_bool()
                .unwrap_or_else(|| self.has_scripts(d));
        v["scripts"] = json!(scripts);
        // A share serves its own frame, checked against the live grant on every request. A signed-in
        // page gets a per-user frame grant from the API handler (`frame_url`).
        v["frame_url"] = match share {
            Some(_) if html && d.interactive => json!(Self::resource(&api, "frame", path).as_str()),
            _ => Value::Null,
        };
        let local = local_only_url(&self.base);
        v["reachability"] = json!(if local { "local" } else { "unverified" });
        let mut warnings = vec![if local {
            LOCAL_WARNING
        } else {
            REACHABILITY_WARNING
        }];
        if html && d.interactive {
            warnings.push(INTERACTIVE_WARNING);
        } else if html || d.mime == "image/svg+xml" {
            warnings.push(HTML_WARNING);
        }
        v["access_warning"] = json!(warnings.join(" "));
        v
    }

    /// The interactive frame of a signed-in user's page: a fresh grant for this user and delivery.
    pub fn frame_url(&self, d: &Delivery, user: &str) -> Option<String> {
        if !d.interactive || !html_entry(d) {
            return None;
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let now = steve_store::now();
        {
            let mut grants = self.frames.lock().unwrap_or_else(|e| e.into_inner());
            grants.retain(|_, g| g.expires > now);
            if grants.len() >= FRAME_GRANTS_MAX {
                // Oldest first; a page whose grant went can load the metadata again.
                let mut by_age: Vec<_> =
                    grants.iter().map(|(k, g)| (g.expires, k.clone())).collect();
                by_age.sort();
                for (_, k) in by_age.into_iter().take(grants.len() + 1 - FRAME_GRANTS_MAX) {
                    grants.remove(&k);
                }
            }
            grants.insert(
                token.clone(),
                FrameGrant {
                    delivery: d.id.clone(),
                    user: user.into(),
                    expires: now + FRAME_TTL_MS,
                },
            );
        }
        let api = format!("{}/api/delivery-frames", self.base);
        let path = d.entrypoint.as_deref().unwrap_or(&d.name);
        Some(Self::resource(&api, &token, path).into())
    }

    /// The delivery of a live frame grant, rechecked on every request: unexpired, its user still a
    /// member of the delivery's workspace, and the delivery still interactive. No cookie is read.
    pub fn framed(&self, token: &str) -> Result<Option<Delivery>, StoreError> {
        let grant = {
            let grants = self.frames.lock().unwrap_or_else(|e| e.into_inner());
            grants.get(token).cloned()
        };
        let Some(g) = grant.filter(|g| g.expires > steve_store::now()) else {
            return Ok(None);
        };
        self.store.read(|r| {
            let Some(d) = repo::get(r, &g.delivery, None)? else {
                return Ok(None);
            };
            let member = repo::member(r, &d.workspace_id, &g.user)?;
            Ok((member && d.interactive).then_some(d))
        })
    }

    /// The share's delivery, for its frame: only while the delivery is interactive.
    pub fn shared_frame(&self, token: &str) -> Result<Option<Delivery>, StoreError> {
        Ok(self
            .shared(token)?
            .filter(|d| d.interactive && html_entry(d)))
    }

    /// The `frame-ancestors` and resource sources of an interactive frame below `prefix`.
    pub fn frame_policy(&self, prefix: &str) -> String {
        let ancestors = if self.ancestors.is_empty() {
            "'self'".to_string()
        } else {
            format!("'self' {}", self.ancestors.join(" "))
        };
        // `sandbox allow-scripts` makes the document an opaque origin on every load, including a
        // direct navigation: no cookies, storage or same-origin access to Steve. Only the package
        // (below `prefix`) and inline code run; no fetch, XHR, WebSocket, beacon, form, worker,
        // nested frame or plugin, and only the embedding pages listed may frame it.
        format!(
            "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' {prefix}; style-src 'unsafe-inline' {prefix}; img-src {prefix} data: blob:; font-src {prefix} data:; media-src {prefix} data: blob:; connect-src 'none'; form-action 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; manifest-src 'none'; base-uri 'none'; frame-ancestors {ancestors}"
        )
    }

    /// Whether the HTML entry asks for scripts: a script element or an inline event handler. A
    /// static page that needs them would otherwise look empty.
    fn has_scripts(&self, d: &Delivery) -> bool {
        let path = d.entrypoint.as_deref();
        let Ok((f, _, _, _)) = self.open(d, path.or(Some(&d.name))) else {
            return false;
        };
        let mut head = Vec::new();
        use std::io::Read;
        if f.take(SCRIPT_SCAN).read_to_end(&mut head).is_err() {
            return false;
        }
        scripts_in(&String::from_utf8_lossy(&head).to_ascii_lowercase())
    }

    /// Who may manage a delivery (turn its interactive preview on or off): its owner; for an item
    /// delivery, the member who asked for the item or an owner of the workspace.
    pub fn can_manage(&self, d: &Delivery, user: &str) -> Result<bool, StoreError> {
        if d.reply {
            return Ok(false);
        }
        if d.owner_id.as_deref() == Some(user) {
            return Ok(true);
        }
        if d.owner_id.is_some() {
            return Ok(false);
        }
        self.store
            .read(|r| repo::item_manager(r, &d.workspace_id, d.item_id.as_deref(), user))
    }

    pub fn set_interactive(&self, d: &Delivery, on: bool) -> Result<(), StoreError> {
        self.store.write(|w| repo::set_interactive(w, &d.id, on))
    }

    /// The deliveries `user` manages in `workspace`, newest first.
    pub fn owned(&self, workspace: &str, user: &str) -> Result<Vec<Delivery>, StoreError> {
        self.store.read(|r| repo::owned(r, workspace, user))
    }

    pub fn get(&self, id: &str, ws: Option<&str>) -> Result<Option<Delivery>, StoreError> {
        self.store.read(|r| repo::get(r, id, ws))
    }

    /// No update method exists: a new receive always creates a new version, even for the same name.
    pub fn accept(
        &self,
        session: &steve_store::Session,
        title: &str,
        name: &str,
        bytes: &[u8],
        entrypoint: Option<&str>,
    ) -> Result<Delivery, String> {
        self.publish(
            Publisher::Session(session),
            title,
            name,
            bytes,
            entrypoint,
            false,
        )
    }

    /// Recheck the publisher's current scope: a Bot's session (and its item, when it has one) in its
    /// workspace, or a user's membership. No caller-supplied item or workspace is accepted.
    fn in_scope(r: &mut impl Read, p: Publisher) -> Result<bool, StoreError> {
        match p {
            Publisher::Session(s) => repo::session_in_scope(r, s),
            Publisher::User { id, workspace } => repo::member(r, workspace, id),
        }
    }

    /// Validate and keep one immutable version. `interactive` is the publisher's choice at publish
    /// time; static is the default.
    pub fn publish(
        &self,
        publisher: Publisher,
        title: &str,
        name: &str,
        bytes: &[u8],
        entrypoint: Option<&str>,
        interactive: bool,
    ) -> Result<Delivery, String> {
        if bytes.len() > MAX_FILE {
            return Err("delivery exceeds 64 MiB".into());
        }
        if !bundle::plain(name) || name.contains('/') {
            return Err("invalid delivery filename".into());
        }
        let (workspace, item, session) = match publisher {
            Publisher::Session(s) => (
                s.workspace_id.as_str(),
                s.item_id.as_deref(),
                Some(s.id.as_str()),
            ),
            Publisher::User { workspace, .. } => (workspace, None, None),
        };
        let changed = "delivery session/item workspace changed";
        if !self
            .store
            .read(|r| Self::in_scope(r, publisher))
            .map_err(|e| e.to_string())?
        {
            return Err(changed.into());
        }
        let archive = name.to_ascii_lowercase().ends_with(".zip");
        let bundle = if archive {
            Some(bundle::unpack(bytes, entrypoint)?)
        } else {
            if entrypoint.is_some() {
                return Err("entrypoint requires a ZIP delivery".into());
            }
            None
        };
        let mut manifest = BTreeMap::new();
        if let Some(b) = &bundle {
            for (path, data) in &b.files {
                manifest.insert(
                    path.clone(),
                    Object {
                        sha256: self.put(data).map_err(|e| e.to_string())?,
                        size: data.len() as u64,
                        mime: mime(path).into(),
                    },
                );
            }
        }
        let sha256 = self.put(bytes).map_err(|e| e.to_string())?;
        // A package previews as its entry: an HTML page, a Markdown document, a PDF, media…
        let entry = bundle.as_ref().and_then(|b| b.entrypoint.clone());
        let (shown, shown_bytes): (&str, &[u8]) = match (&entry, &bundle) {
            (Some(e), Some(b)) => (e.as_str(), b.files[e].as_slice()),
            _ => (name, bytes),
        };
        let preview = Self::published_preview(shown, shown_bytes, entry.is_some(), archive);
        let id = format!("dlv_{}", uuid::Uuid::new_v4().simple());
        let mut d = Delivery {
            registered_url: self.url(&id, workspace),
            id,
            workspace_id: workspace.into(),
            item_id: item.map(str::to_string),
            title: if title.is_empty() {
                name.into()
            } else {
                title.into()
            },
            name: name.into(),
            mime: mime(name).into(),
            size: bytes.len() as u64,
            created: steve_store::now(),
            entrypoint: entry,
            sha256,
            manifest,
            preview,
            interactive,
            owner_id: None,
            reply: false,
        };
        self.store
            .write(|w| {
                // Recheck the scope and recipient after copying or unpacking the objects.
                if !Self::in_scope(w, publisher)? {
                    return Err(StoreError::Conflict(changed.into()));
                }
                d.owner_id = match publisher {
                    Publisher::User { id, .. } => Some(id.into()),
                    Publisher::Session(s) if s.item_id.is_none() => repo::session_owner(w, s)?,
                    Publisher::Session(_) => None,
                };
                repo::insert(w, &d, session)
            })
            .map_err(|e| e.to_string())?;
        Ok(d)
    }

    /// Preview the selected immutable entry, including the bounded HTML script marker.
    fn published_preview(
        shown: &str,
        shown_bytes: &[u8],
        with_entry: bool,
        archive: bool,
    ) -> Value {
        let form = crate::api::outputs::media(shown).0;
        let mut preview = match form {
            Some(form) => {
                json!({"type":form,"name":shown,"size":shown_bytes.len(),"mime":mime(shown),"large":crate::api::outputs::large(form,shown_bytes.len() as u64)})
            }
            None if !html_name(shown) && (with_entry || !archive) => {
                crate::api::outputs::text_value(shown, shown_bytes)
            }
            _ => {
                json!({"type":if html_name(shown) {"html"} else {"binary"},"name":shown,"size":shown_bytes.len()})
            }
        };
        preview["path"] = json!(shown);
        if html_name(shown) {
            let head = &shown_bytes[..shown_bytes.len().min(SCRIPT_SCAN as usize)];
            preview["scripts"] = json!(scripts_in(
                &String::from_utf8_lossy(head).to_ascii_lowercase()
            ));
        }
        preview
    }

    /// Only REST handlers with an authenticated user call this. Bots get no sharing tool.
    pub fn create_share(&self, d: &Delivery, user: &str, expires_at: i64) -> Result<Share, String> {
        if d.reply {
            return Err("reply resources cannot be shared".into());
        }
        let now = steve_store::now();
        if expires_at <= now {
            return Err("expires_at must be a future Unix millisecond timestamp".into());
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let row = repo::ShareRow {
            token,
            expires_at,
            created: now,
            revoked_at: None,
        };
        self.store
            .write(|w| repo::create_share(w, &d.id, user, &row))
            .map_err(|e| e.to_string())?;
        Ok(self.share(row))
    }

    fn share(&self, row: repo::ShareRow) -> Share {
        Share {
            url: format!("{}/shares/{}", self.base, row.token),
            token: row.token,
            expires_at: row.expires_at,
            created: row.created,
            revoked_at: row.revoked_at,
        }
    }

    pub fn shares(&self, d: &Delivery) -> Result<Vec<Share>, StoreError> {
        self.store
            .read(|r| repo::shares(r, &d.id))
            .map(|rows| rows.into_iter().map(|row| self.share(row)).collect())
    }

    pub fn revoke(&self, d: &Delivery, token: &str) -> Result<bool, StoreError> {
        self.store
            .write(|w| repo::revoke(w, &d.id, token, steve_store::now()))
    }

    /// Every metadata, original and resource request rechecks the live row and clock.
    pub fn shared(&self, token: &str) -> Result<Option<Delivery>, StoreError> {
        self.store
            .read(|r| repo::shared(r, token, steve_store::now()))
    }

    fn objects(&self) -> PathBuf {
        self.home.join("deliveries").join("objects")
    }

    fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        let hash = hex::encode(Sha256::digest(bytes));
        let dir = self.objects();
        for p in [self.home.join("deliveries"), dir.clone()] {
            match fs::create_dir(&p) {
                Ok(()) => {
                    File::open(p.parent().expect("storage parent"))?.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            if !fs::symlink_metadata(&p)?.is_dir() {
                return Err(std::io::Error::other(
                    "delivery storage must be a real directory",
                ));
            }
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
        }
        let target = dir.join(&hash);
        let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o400)
                .open(&temp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            match fs::hard_link(&temp, &target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let mut old = self.object_file(&hash)?;
                    use std::io::Read;
                    let mut content = Vec::new();
                    std::io::Read::by_ref(&mut old)
                        .take(MAX_FILE as u64 + 1)
                        .read_to_end(&mut content)?;
                    if content != bytes {
                        return Err(std::io::Error::other("delivery object hash mismatch"));
                    }
                }
                Err(e) => return Err(e),
            }
            File::open(&dir)?.sync_all()?;
            Ok(hash)
        })();
        let _ = fs::remove_file(&temp);
        result
    }

    fn object_file(&self, hash: &str) -> std::io::Result<File> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(std::io::Error::other("invalid delivery object"));
        }
        for p in [self.home.join("deliveries"), self.objects()] {
            if !fs::symlink_metadata(p)?.is_dir() {
                return Err(std::io::Error::other("invalid delivery storage"));
            }
        }
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(self.objects().join(hash))?;
        if !f.metadata()?.is_file() {
            return Err(std::io::Error::other("invalid delivery object"));
        }
        Ok(f)
    }

    pub fn open(
        &self,
        d: &Delivery,
        content: Option<&str>,
    ) -> Result<(File, u64, String, String), String> {
        let (hash, size, mime, name) = match content {
            None => (&d.sha256, d.size, d.mime.clone(), d.name.clone()),
            Some(path) if bundle::plain(path) => {
                if let Some(o) = d.manifest.get(path) {
                    (
                        &o.sha256,
                        o.size,
                        o.mime.clone(),
                        path.rsplit('/').next().unwrap_or(path).into(),
                    )
                } else if d.mime != "application/zip" && path == d.name {
                    (&d.sha256, d.size, d.mime.clone(), d.name.clone())
                } else {
                    return Err("resource not found".into());
                }
            }
            Some(_) => return Err("invalid resource path".into()),
        };
        let f = self
            .object_file(hash)
            .map_err(|_| "delivery object unavailable")?;
        if f.metadata()
            .map_err(|_| "delivery object unavailable")?
            .len()
            != size
        {
            return Err("incomplete delivery object".into());
        }
        Ok((f, size, mime, name))
    }
}

/// Recognize only addresses whose configured spelling is known to be local. DNS, routing,
/// reverse proxies and firewall reachability cannot be inferred from a non-loopback hostname.
fn local_only_url(base: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base) else {
        return false;
    };
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| ip.is_loopback() || ip.is_unspecified())
        }
        Err(_) => false,
    }
}

pub(crate) fn mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "txt" | "csv" | "log" => "text/plain",
        "zip" => "application/zip",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => crate::api::outputs::media(name).1,
    }
}

/// Git references and external links keep their existing viewers. Actual file locations use
/// the same receiver as the explicit file tool, including tracked documents (versioned bytes).
pub(crate) fn file_location(kind: &str, location: &str) -> bool {
    if location.starts_with("https://") || location.starts_with("http://") {
        return false;
    }
    if matches!(
        kind.to_ascii_lowercase().as_str(),
        "branch" | "commit" | "pr" | "分支" | "提交"
    ) {
        return false;
    }
    Path::new(location).is_absolute()
        || Path::new(location).extension().is_some()
        || matches!(
            kind.to_ascii_lowercase().as_str(),
            "file" | "document" | "文件" | "文档" | "报告" | "设计"
        )
}
