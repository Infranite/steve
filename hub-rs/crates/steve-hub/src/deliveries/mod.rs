//! Immutable versions received by the Hub. Sources are read through the Node; only objects
//! owned by this service are opened here. Database rows become visible after every object is
//! synced. A failed receive can leave unreferenced objects, never a partial published delivery.
mod bundle;
mod receive;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_store::{Store, StoreError};

pub const MAX_FILE: usize = 64 * 1024 * 1024;
const HTML_WARNING: &str = "HTML is shown in a static sandbox. Scripts, forms, frames and external connections are disabled.";
const LOCAL_WARNING: &str = "The configured STEVE_PUBLIC_URL is a loopback or local-only address. This link cannot be used from another device; configure a browser-reachable installation address.";
const REACHABILITY_WARNING: &str = "This URL uses the configured STEVE_PUBLIC_URL; reachability from other devices has not been verified.";
const SESSION_SCOPE: &str = "SELECT EXISTS(SELECT 1 FROM sessions s JOIN items i ON i.id=s.item_id WHERE s.id=? AND i.id=? AND s.workspace_id=? AND i.workspace_id=s.workspace_id)";

#[derive(Clone)]
pub struct Service {
    pub store: Store,
    home: PathBuf,
    pub base: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Object {
    sha256: String,
    size: u64,
    mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub item_id: String,
    pub title: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub created: i64,
    pub entrypoint: Option<String>,
    pub preview: Value,
    #[serde(skip)]
    pub workspace_id: String,
    #[serde(skip)]
    pub(crate) registered_url: String,
    #[serde(skip)]
    sha256: String,
    #[serde(skip)]
    manifest: BTreeMap<String, Object>,
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
        Self {
            store,
            home,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub fn url(&self, id: &str, workspace: &str) -> String {
        let mut url = reqwest::Url::parse(&format!("{}/deliveries/{id}", self.base))
            .expect("configured public URL");
        // The page must bind this selector BEFORE /api/state picks the remembered workspace.
        // Headers or query selectors on API requests still go through normal membership checks.
        url.query_pairs_mut().append_pair("ws", workspace);
        url.into()
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
        let mut content =
            reqwest::Url::parse(&format!("{api}/content/")).expect("configured public URL");
        {
            let mut segments = content.path_segments_mut().expect("HTTP URL");
            segments.pop_if_empty();
            for part in path.split('/') {
                segments.push(part);
            }
        }
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
        let mut warnings = vec![if local_only_url(&self.base) {
            LOCAL_WARNING
        } else {
            REACHABILITY_WARNING
        }];
        if d.entrypoint.is_some() || d.mime == "text/html" || d.mime == "image/svg+xml" {
            warnings.push(HTML_WARNING);
        }
        v["access_warning"] = json!(warnings.join(" "));
        v
    }

    pub fn get(&self, id: &str, ws: Option<&str>) -> Result<Option<Delivery>, StoreError> {
        self.store.reader(|c| {
            c.query_row("SELECT d.id,d.workspace_id,d.item_id,d.title,d.name,d.mime,d.size as i64,d.created,d.entrypoint,d.sha256,d.manifest,d.preview,d.url FROM deliveries d JOIN items i ON i.id=d.item_id AND i.workspace_id=d.workspace_id WHERE d.id=? AND (? IS NULL OR d.workspace_id=?)", params![id,ws,ws], |r| {
                Ok((Delivery { id:r.get(0)?,workspace_id:r.get(1)?,item_id:r.get(2)?,title:r.get(3)?,name:r.get(4)?,mime:r.get(5)?,size:r.get::<_,i64>(6)? as u64,created:r.get(7)?,entrypoint:r.get(8)?,sha256:r.get(9)?,manifest:BTreeMap::new(),preview:Value::Null,registered_url:r.get(12)? },r.get::<_,String>(10)?,r.get::<_,String>(11)?))
            }).optional()?.map(|(mut d,m,p)| { d.manifest=serde_json::from_str(&m)?;d.preview=serde_json::from_str(&p)?;Ok(d) }).transpose()
        })
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
        if bytes.len() > MAX_FILE {
            return Err("delivery exceeds 64 MiB".into());
        }
        if !bundle::plain(name) || name.contains('/') {
            return Err("invalid delivery filename".into());
        }
        let item = session
            .item_id
            .as_deref()
            .ok_or("file delivery requires an item session")?;
        // Recheck current session/item workspace before retaining any data. The MCP session token
        // binds the item; no caller-supplied item or workspace is accepted.
        let valid = self
            .store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    SESSION_SCOPE,
                    params![session.id, item, session.workspace_id],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .map_err(|e| e.to_string())?;
        if !valid {
            return Err("delivery session/item workspace changed".into());
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
        let form = crate::api::outputs::media(name).0;
        let mut preview = match form {
            Some(form) => {
                json!({"type":form,"name":name,"size":bytes.len(),"mime":mime(name),"large":crate::api::outputs::large(form,bytes.len() as u64)})
            }
            None if !archive && mime(name) != "text/html" => {
                crate::api::outputs::text_value(name, bytes)
            }
            _ => {
                json!({"type":if bundle.as_ref().and_then(|b|b.entrypoint.as_ref()).is_some() || mime(name)=="text/html" {"html"} else {"binary"},"name":name,"size":bytes.len()})
            }
        };
        preview["path"] = json!(name);
        let id = format!("dlv_{}", uuid::Uuid::new_v4().simple());
        let d = Delivery {
            registered_url: self.url(&id, &session.workspace_id),
            id,
            workspace_id: session.workspace_id.clone(),
            item_id: item.into(),
            title: if title.is_empty() {
                name.into()
            } else {
                title.into()
            },
            name: name.into(),
            mime: mime(name).into(),
            size: bytes.len() as u64,
            created: steve_store::now(),
            entrypoint: bundle.and_then(|b| b.entrypoint),
            sha256,
            manifest,
            preview,
        };
        self.store.transaction(|t| {
            // A scope change while copying or validating an archive must fail the receipt,
            // rather than return a URL for a row its item no longer owns.
            if !t.query_row(SESSION_SCOPE,params![session.id,item,session.workspace_id],|r|r.get::<_,bool>(0))? {
                return Err(StoreError::Conflict("delivery session/item workspace changed".into()));
            }
            t.execute("INSERT INTO deliveries(id,url,workspace_id,item_id,session_id,title,name,mime,size,created,entrypoint,sha256,manifest,preview) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![d.id,d.registered_url,d.workspace_id,d.item_id,session.id,d.title,d.name,d.mime,d.size as i64,d.created,d.entrypoint,d.sha256,serde_json::to_string(&d.manifest)?,serde_json::to_string(&d.preview)?])?;
            Ok::<_,StoreError>(())
        }).map_err(|e|e.to_string())?;
        Ok(d)
    }

    /// Only REST handlers with an authenticated user call this. Bots get no sharing tool.
    pub fn create_share(&self, d: &Delivery, user: &str, expires_at: i64) -> Result<Share, String> {
        let now = steve_store::now();
        if expires_at <= now {
            return Err("expires_at must be a future Unix millisecond timestamp".into());
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.store.transaction(|t| {
            t.execute("INSERT INTO delivery_shares(token,delivery_id,created_by,created,expires_at) VALUES(?,?,?,?,?)",params![token,d.id,user,now,expires_at])?;
            Ok::<_,StoreError>(())
        }).map_err(|e|e.to_string())?;
        Ok(Share {
            url: format!("{}/shares/{token}", self.base),
            token,
            expires_at,
            created: now,
            revoked_at: None,
        })
    }

    pub fn shares(&self, d: &Delivery) -> Result<Vec<Share>, StoreError> {
        self.store.reader(|c| {
            let mut q=c.prepare("SELECT token,expires_at,created,revoked_at FROM delivery_shares WHERE delivery_id=? ORDER BY created,token")?;
            let rows=q.query_map([&d.id],|r| {let token:String=r.get(0)?;Ok(Share{url:format!("{}/shares/{token}",self.base),token,expires_at:r.get(1)?,created:r.get(2)?,revoked_at:r.get(3)?})})?;
            Ok(rows.collect::<Result<_,_>>()?)
        })
    }

    pub fn revoke(&self, d: &Delivery, token: &str) -> Result<bool, StoreError> {
        self.store.transaction(|t| Ok(t.execute("UPDATE delivery_shares SET revoked_at=COALESCE(revoked_at,?) WHERE delivery_id=? AND token=?",params![steve_store::now(),d.id,token])? == 1))
    }

    /// Every metadata, original and resource request rechecks the live row and clock. No positive
    /// authorization cache; revoked/expired tokens deliberately look like unknown ones.
    pub fn shared(&self, token: &str) -> Result<Option<Delivery>, StoreError> {
        let id=self.store.reader(|c| Ok::<_,StoreError>(c.query_row("SELECT delivery_id FROM delivery_shares WHERE token=? AND revoked_at IS NULL AND expires_at>?",params![token,steve_store::now()],|r|r.get::<_,String>(0)).optional()?))?;
        match id {
            Some(id) => self.get(&id, None),
            None => Ok(None),
        }
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
                if d.name.to_ascii_lowercase().ends_with(".zip") {
                    let o = d.manifest.get(path).ok_or("resource not found")?;
                    (
                        &o.sha256,
                        o.size,
                        o.mime.clone(),
                        path.rsplit('/').next().unwrap_or(path).into(),
                    )
                } else if path == d.name {
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
        "txt" | "md" | "csv" => "text/plain",
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
