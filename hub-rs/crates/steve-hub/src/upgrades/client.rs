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
    pub(super) fn contains_asset(&self, name: &str) -> bool {
        self.assets
            .iter()
            .filter(|asset| asset.name == name)
            .count()
            == 1
    }
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
    pub(super) async fn fetch(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        self.fetch_progress(url, limit, None).await
    }
    async fn fetch_progress(
        &self,
        url: &str,
        limit: u64,
        mut op: Option<&mut Operation>,
    ) -> Result<Vec<u8>> {
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
        let mut reported = std::time::Instant::now();
        if let Some(op) = op.as_deref_mut() {
            self.progress(
                op,
                "downloading",
                Some(DownloadProgress {
                    received_bytes: 0,
                    total_bytes: limit,
                }),
            )?;
        }
        while let Some(b) = stream.next().await {
            let b = b?;
            if bytes.len() as u64 + b.len() as u64 > limit {
                bail!("asset exceeds size limit")
            };
            bytes.extend_from_slice(&b);
            // Each receipt is fsynced; bound writes independently of chunk size or network speed.
            if reported.elapsed() >= Duration::from_millis(500) {
                if let Some(op) = op.as_deref_mut() {
                    self.progress(
                        op,
                        "downloading",
                        Some(DownloadProgress {
                            received_bytes: bytes.len() as u64,
                            total_bytes: limit,
                        }),
                    )?;
                }
                reported = std::time::Instant::now();
            }
        }
        if let Some(op) = op {
            self.progress(
                op,
                "downloading",
                Some(DownloadProgress {
                    received_bytes: bytes.len() as u64,
                    total_bytes: limit,
                }),
            )?;
        }
        Ok(bytes)
    }
    /// Which tag `latest` is, from github.com's redirect (not the rate-limited API), with the
    /// manifest published beside it. No release notes: those only come from the API.
    pub(super) async fn latest_web(&self) -> Result<Option<Release>> {
        let client = reqwest::Client::builder()
            .user_agent("Steve-updater/1")
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let r = client
            .get(format!(
                "{}/{}/releases/latest",
                self.web_base, self.repository
            ))
            .send()
            .await?;
        if r.status() == 404 {
            return Ok(None);
        }
        if !r.status().is_redirection() {
            bail!("github.com answered {} for the latest release", r.status())
        }
        let location = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .context("no redirect target for the latest release")?;
        let tag = location
            .rsplit_once("/releases/tag/")
            .map(|(_, tag)| tag.trim_end_matches('/'))
            .context("the latest release redirect names no tag")?;
        let version = semver::Version::parse(tag.trim_start_matches('v'))?;
        Ok(Some(self.web_release(&version).await?.0))
    }
    /// A published release read from its download URLs only: the manifest names its assets.
    async fn web_release(&self, version: &semver::Version) -> Result<(Release, Manifest)> {
        let download = format!(
            "{}/{}/releases/download/v{version}",
            self.web_base, self.repository
        );
        let manifest: Manifest = serde_json::from_slice(
            &self
                .fetch(&format!("{download}/steve-release.json"), 1024 * 1024)
                .await?,
        )?;
        validate_manifest(&manifest, &version.to_string())?;
        let mut assets = vec![RemoteAsset {
            name: "steve-release.json".into(),
            browser_download_url: format!("{download}/steve-release.json"),
        }];
        assets.extend(manifest.assets.iter().map(|a| RemoteAsset {
            name: a.name.clone(),
            browser_download_url: format!("{download}/{}", a.name),
        }));
        let release = Release {
            tag_name: format!("v{version}"),
            html_url: format!(
                "{}/{}/releases/tag/v{version}",
                self.web_base, self.repository
            ),
            body: None,
            draft: false,
            prerelease: !version.pre.is_empty(),
            assets,
        };
        Ok((release, manifest))
    }
    /// The release `v{version}` of the configured repository and its validated manifest: a stable,
    /// published release, not older than this Hub. The selection is never taken from the caller.
    pub(super) async fn resolve(&self, version: &str) -> Result<(Release, Manifest)> {
        let api = async {
            let r: Release = self
                .client
                .get(format!(
                    "{}/repos/{}/releases/tags/v{}",
                    self.api_base, self.repository, version
                ))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            anyhow::Ok(r)
        };
        let (r, manifest) = match api.await {
            Ok(r) => {
                if r.draft || r.prerelease {
                    bail!("release is not a stable target")
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
                (r, manifest)
            }
            // rate limited: the download URLs exist only for a published (not draft) release
            Err(error) => match self.web_release(&semver::Version::parse(version)?).await {
                Ok(found) => found,
                Err(_) => return Err(error),
            },
        };
        if r.draft || r.prerelease || r.version()?.to_string() != version {
            bail!("release is not a stable target")
        }
        if let Ok(current) = semver::Version::parse(self.current_hub.trim_start_matches('v'))
            && r.version()? < current
        {
            bail!("automatic downgrade is not supported")
        }
        validate_manifest(&manifest, version)?;
        Ok((r, manifest))
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
        self.progress(op, "resolving", None)?;
        let (r, manifest) = self.resolve(&op.version).await?;
        if op
            .runner
            .as_ref()
            .and_then(|s| s.commit.as_deref())
            .is_some_and(|expected| expected != manifest.commit)
        {
            bail!("release commit changed since the upgrade was requested")
        }
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
            .fetch_progress(
                &remote.browser_download_url,
                asset.size.min(512 * 1024 * 1024),
                Some(op),
            )
            .await?;
        self.progress(op, "verifying", None)?;
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
        self.progress(op, "unpacking", None)?;
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
        if op
            .runner
            .as_ref()
            .is_some_and(|r| r.kind == "release_bundle")
        {
            // The local signing identity still has to be applied before the bundle is deployable.
            self.save(op)?;
        } else {
            self.phase(op, "ready", None)?;
        }
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
