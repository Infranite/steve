use super::*;
use futures::StreamExt;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
pub(super) struct Release {
    tag_name: String,
    pub html_url: String,
    pub body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<RemoteAsset>,
}
#[derive(Serialize, Deserialize)]
struct RemoteAsset {
    name: String,
    browser_download_url: String,
}
impl Release {
    pub fn version(&self) -> Result<semver::Version> {
        Ok(semver::Version::parse(
            self.tag_name.trim_start_matches('v'),
        )?)
    }
}
impl Updates {
    pub(super) async fn latest(&self) -> Result<Option<Release>> {
        let cache_path = self.dir().join("github-cache.json");
        let cache = read::<Value>(&cache_path).ok();
        let mut request = self.client.get(format!(
            "{}/repos/{}/releases/latest",
            self.api_base, self.repository
        ));
        if let Some(etag) = cache
            .as_ref()
            .filter(|v| v["repository"] == self.repository)
            .and_then(|v| v["etag"].as_str())
        {
            request = request.header("If-None-Match", etag)
        }
        let r = request.send().await?;
        if r.status() == 304 {
            let cached = cache.context("GitHub 304 without cache")?;
            return Ok(Some(serde_json::from_value(cached["release"].clone())?));
        }
        if r.status() == 404 {
            return Ok(None);
        }
        if r.status() == 403 || r.status() == 429 {
            bail!("GitHub rate limited; retry after reset")
        }
        let etag = r
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let r: Release = r.error_for_status()?.json().await?;
        atomic(
            &cache_path,
            &json!({"repository":self.repository,"etag":etag,"release":r}),
        )?;
        if r.draft || r.prerelease {
            return Ok(None);
        };
        Ok(Some(r))
    }
    /// Validate installability during checks; never advertise a tag alone as an executable update.
    pub(super) async fn inspect_release(&self, release: &Release) -> Result<bool> {
        let remote = release
            .assets
            .iter()
            .find(|asset| asset.name == "steve-release.json")
            .context("release has no steve-release.json")?;
        let manifest: Manifest = serde_json::from_slice(
            &self
                .fetch(&remote.browser_download_url, 1024 * 1024)
                .await?,
        )?;
        validate_manifest(&manifest, &release.version()?.to_string())?;
        Ok(manifest.assets.iter().any(|asset| {
            asset.target == platform()
                && release
                    .assets
                    .iter()
                    .any(|remote| remote.name == asset.name)
        }))
    }
    async fn fetch(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let u = reqwest::Url::parse(url)?;
        if u.scheme() != "https" && !(cfg!(test) && u.host_str() == Some("127.0.0.1")) {
            bail!("release asset requires HTTPS")
        }
        let r = self.client.get(u).send().await?.error_for_status()?;
        if r.content_length().is_some_and(|n| n > limit) {
            bail!("asset exceeds size limit")
        }
        let mut bytes = Vec::new();
        let mut stream = r.bytes_stream();
        while let Some(b) = stream.next().await {
            let b = b?;
            if bytes.len() as u64 + b.len() as u64 > limit {
                bail!("asset exceeds size limit")
            };
            bytes.extend_from_slice(&b)
        }
        Ok(bytes)
    }
    pub(super) async fn stage(&self, op: &mut Operation) -> Result<()> {
        let stage_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir().join(format!("{}.stage.lock", op.id)))?;
        if stage_lock.try_lock().is_err() {
            return Ok(());
        }
        if self.operation(&op.id)?.phase != "staging" {
            return Ok(());
        }
        // The immutable selection is resolved from this configured repository, never from caller URLs.
        let r: Release = self
            .client
            .get(format!(
                "{}/repos/{}/releases/tags/v{}",
                self.api_base, self.repository, op.version
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if r.draft || r.prerelease || r.version()?.to_string() != op.version {
            bail!("release is not a stable target")
        }
        if let Ok(current) = semver::Version::parse(self.current_hub.trim_start_matches('v'))
            && r.version()? < current
        {
            bail!("automatic downgrade is not supported")
        }
        let remote = r
            .assets
            .iter()
            .find(|a| a.name == "steve-release.json")
            .context("release has no steve-release.json")?;
        let manifest: Manifest = serde_json::from_slice(
            &self
                .fetch(&remote.browser_download_url, 1024 * 1024)
                .await?,
        )?;
        validate_manifest(&manifest, &op.version)?;
        let asset = manifest
            .assets
            .iter()
            .find(|a| a.target == platform())
            .context("unsupported platform: no release bundle")?;
        let remote = r
            .assets
            .iter()
            .find(|a| a.name == asset.name)
            .context("bundle missing from release")?;
        let bytes = self
            .fetch(
                &remote.browser_download_url,
                asset.size.min(512 * 1024 * 1024),
            )
            .await?;
        if bytes.len() as u64 != asset.size
            || hex::encode(Sha256::digest(&bytes)) != asset.sha256.to_lowercase()
        {
            bail!("bundle size or SHA256 mismatch")
        }
        let dir = self
            .home
            .join("releases")
            .join(format!("{}-{}", manifest.version, op.id));
        let stage = dir.with_extension("staging");
        fs::create_dir_all(&stage)?;
        let stage_clone = stage.clone();
        let manifest_clone = manifest.clone();
        let unpacked =
            tokio::task::spawn_blocking(move || unpack(&bytes, &stage_clone, &manifest_clone))
                .await?;
        if let Err(error) = unpacked {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
        fs::rename(stage, &dir)?;
        File::open(dir.parent().unwrap())?.sync_all()?;
        op.target = Some(dir.to_string_lossy().into());
        self.phase(op, "ready", None)?;
        Ok(())
    }
}
pub(super) fn validate_manifest(m: &Manifest, version: &str) -> Result<()> {
    if m.format_version != 1
        || m.api_protocol != 1
        || m.version != version
        || m.hub_version != version
        || m.web_version != version
    {
        bail!("incompatible or mismatched release manifest")
    }
    if m.commit.len() != 40 || !m.commit.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid release commit")
    }
    if m.schema.min_read > m.schema.target || m.schema.max_read < m.schema.target {
        bail!("invalid schema compatibility")
    }
    for a in &m.assets {
        if a.size == 0
            || a.size > 512 * 1024 * 1024
            || a.sha256.len() != 64
            || !a.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            || a.name.contains('/')
            || a.name.contains('\\')
        {
            bail!("invalid bundle asset")
        }
    }
    Ok(())
}
pub(super) fn unpack(bytes: &[u8], dest: &Path, manifest: &Manifest) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for entry in archive.entries()? {
        let mut e = entry?;
        let p = e.path()?.into_owned();
        if p.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) || p.as_os_str().is_empty()
        {
            bail!("unsafe archive path")
        }
        let clean: PathBuf = p
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();
        if clean.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(clean.clone()) {
            bail!("duplicate archive path")
        }
        let kind = e.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("archive links/devices are forbidden")
        }
        total = total.checked_add(e.size()).context("archive overflow")?;
        if total > 2 * 1024 * 1024 * 1024 {
            bail!("expanded archive too large")
        }
        if !e.unpack_in(dest)? {
            bail!("archive escaped staging")
        }
    }
    let embedded: Manifest = read(&dest.join("release.json"))?;
    validate_manifest(&embedded, &manifest.version)?;
    if embedded.commit != manifest.commit {
        bail!("bundle commit mismatch")
    }
    let web: Value = read(&dest.join("web/dist/release.json"))?;
    if web["version"].as_str() != Some(&manifest.version)
        || !dest.join("web/dist/index.html").is_file()
        || !dest.join("bin/steve-hub").is_file()
    {
        bail!("bundle missing matching Hub/Web")
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dest.join("bin/steve-hub"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    Ok(())
}
