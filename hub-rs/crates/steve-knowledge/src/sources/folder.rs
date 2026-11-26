//! Folder sources: a Markdown / text folder on a Node (a wiki, an Obsidian
//! vault), scanned every 10 minutes and on demand, read only, never written back.
//!
//! - [`check_path_probe`] / [`check_path_probe_for_role`]: the single Node-probed whitelist path
//!   check, `400 这个目录不能加入知识库：<原因>`;
//! - [`sync`]: one scan: walk ([`crate::scan`]), reconcile the documents by `(mtime, size)`, index
//!   the changed ones through [`index::run_batch`] (which compares `sha256` after reading);
//! - [`FolderLoader`]: reads one file for the index, re-checking that it is still inside the root;
//! - [`spawn_scans`]: the `kb.folder_scan` job, started by the integrator;
//! - through a Node ([`super::folder_node`]): the Node probes, walks and reads, the rest stays here;
//! - the source writes (`POST/PATCH/DELETE /kb/sources`, `POST /kb/sources/:id/sync`).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use steve_store::{Members, Read as StorageRead, References, Revisions};

use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Check, Modules};
use steve_store::base::{self, Level, Visibility};
use steve_store::{Result, StoreError, W, uid};

use crate::extract_text::{self, ExtractError, Format, FrontMatter};
use crate::index::{self, BATCH, BoxFuture, DocLoader, Loaded};
use crate::scan;
use crate::service;
use crate::sources::folder_node::{self, NodeFolders, NodeRead};
use crate::sources::{is_builtin, source_json};
use crate::state::Knowledge;
use steve_store::knowledge::{
    DocRow, Documents, MemoryEntries, NewDoc, SourceRole, SourceRow, WriteMode,
};

/// `kb_docs.error` of a file over 2 MB.
pub const TOO_BIG: &str = "文件超过 2 MB，没有索引";
/// A file that resolves outside the folder (a link swapped after the scan).
pub const OUTSIDE: &str = "文件指向文件夹以外，没有读取";
pub const NOUN_SOURCE: &str = "知识来源";
/// How often `kb.folder_scan` runs.
pub const SCAN_EVERY: Duration = Duration::from_secs(600);

const MAX_PATH: usize = 4096;

/// Where the user's home and Steve's own data are, for [`check_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPolicy {
    pub home: PathBuf,
    pub steve_home: PathBuf,
}

/// Resolves links where possible (macOS temp dirs live under `/private/var`).
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl FolderPolicy {
    pub fn new(home: impl AsRef<Path>, steve_home: impl AsRef<Path>) -> Self {
        Self {
            home: canon(home.as_ref()),
            steve_home: canon(steve_home.as_ref()),
        }
    }

    /// `$HOME`, and `STEVE_HOME` (default `~/.steve`) as config.ts reads them.
    pub fn from_env() -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        let steve = std::env::var("STEVE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".steve"));
        let steve = std::path::absolute(&steve).unwrap_or(steve);
        Self::new(home, steve)
    }
    /// Default personal context directory when no custom memory folder is configured:
    /// `$STEVE_HOME/context` for the `default` workspace, `$STEVE_HOME/context/<ws>` for others.
    pub fn default_context_dir(&self, ws: &str) -> PathBuf {
        let base = self.steve_home.join("context");
        if ws.is_empty() || ws == "default" {
            base
        } else {
            base.join(ws)
        }
    }

    /// True when canonical path `c` is `$STEVE_HOME/context` or a direct per-workspace child of it.
    pub fn is_default_context_dir(&self, c: &Path) -> bool {
        let base = canon(&self.steve_home.join("context"));
        c == base || c.parent().is_some_and(|p| p == base)
    }
}

/// Policy check on a Node-supplied [`steve_node_api::PathCheckResponse`] without touching local disk.
pub fn check_path_probe(
    policy: &FolderPolicy,
    raw: &str,
    probe: &steve_node_api::PathCheckResponse,
) -> std::result::Result<PathBuf, String> {
    check_path_probe_for_role(policy, raw, probe, SourceRole::Knowledge)
}

/// Node-probe path rules parameterized by [`SourceRole`]: `$STEVE_HOME/context` (and its
/// per-workspace directory) is permitted ONLY for `SourceRole::Memory` sources, while
/// `$STEVE_HOME` itself and every other internal subdirectory remain strictly refused.
pub fn check_path_probe_for_role(
    policy: &FolderPolicy,
    raw: &str,
    probe: &steve_node_api::PathCheckResponse,
    role: SourceRole,
) -> std::result::Result<PathBuf, String> {
    let p = Path::new(raw.trim());
    if raw.trim().is_empty() || !p.is_absolute() {
        return Err(steve_i18n::t!("knowledge.folder.need_absolute"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(steve_i18n::t!("knowledge.folder.parent_dir"));
    }
    if !probe.exists {
        return Err(steve_i18n::t!("knowledge.folder.missing"));
    }
    if !probe.is_dir {
        return Err(steve_i18n::t!("knowledge.folder.not_dir"));
    }
    let c = probe
        .canonical
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| steve_i18n::t!("knowledge.folder.missing"))?;
    check_canonical_policy(policy, c, role)
}

fn check_canonical_policy(
    policy: &FolderPolicy,
    c: PathBuf,
    role: SourceRole,
) -> std::result::Result<PathBuf, String> {
    if c.parent().is_none() {
        return Err(steve_i18n::t!("knowledge.folder.root"));
    }
    if role == SourceRole::Memory && policy.is_default_context_dir(&c) {
        return Ok(c);
    }
    if c == policy.home {
        return Err(steve_i18n::t!("knowledge.folder.whole_home"));
    }
    if c.starts_with(&policy.steve_home) {
        return Err(steve_i18n::t!("knowledge.folder.steve_data"));
    }
    if policy.steve_home.starts_with(&c) {
        return Err(steve_i18n::t!("knowledge.folder.contains_steve_data"));
    }
    for d in CLOUD_DIRS {
        if c == policy.home.join(d) {
            return Err(steve_i18n::t!("knowledge.folder.cloud_root", dir = d));
        }
    }
    for d in [".ssh", ".gnupg", "Library"] {
        if c.starts_with(policy.home.join(d)) && !is_cloud(policy, &c) {
            return Err(steve_i18n::t!("knowledge.folder.private_dir", dir = d));
        }
    }
    if !(c.starts_with(&policy.home) || c.starts_with("/Volumes")) {
        return Err(outside_home());
    }
    Ok(c)
}

/// Probes `raw` via the wired [`folder_node::FolderPlaces`] Node boundary without touching Hub
/// local disk. Used synchronously by folder creation and draft planning/apply.
pub(crate) fn probe_path_sync(
    kb: &Knowledge,
    raw: &str,
) -> ApiResult<steve_node_api::PathCheckResponse> {
    let places = kb
        .folder_nodes()
        .places()
        .ok_or_else(|| ApiError::rule(folder_node::OFFLINE))?
        .clone();
    let path = raw.to_string();
    let fut = async move { places.check_path(&path).await };
    let probed = match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| h.block_on(fut))
        }
        Ok(_) => std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("probe runtime")
                .block_on(fut)
        })
        .join()
        .map_err(|_| ApiError::Internal("path probe thread panicked".into()))?,
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .block_on(fut),
    };
    probed
        .map_err(|u| ApiError::rule(folder_node::unplaced(&u)))?
        .ok_or_else(|| ApiError::rule(folder_node::OFFLINE))
}

/// Validates `raw` for `role` through the single Node-probed path policy.
pub(crate) fn check_path_via_node(
    kb: &Knowledge,
    raw: &str,
    role: SourceRole,
) -> ApiResult<std::result::Result<PathBuf, String>> {
    let trimmed = raw.trim();
    let p = Path::new(trimmed);
    if trimmed.is_empty() || !p.is_absolute() {
        return Ok(Err(steve_i18n::t!("knowledge.folder.need_absolute")));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Ok(Err(steve_i18n::t!("knowledge.folder.parent_dir")));
    }
    let probe = probe_path_sync(kb, trimmed)?;
    Ok(check_path_probe_for_role(
        kb.folder_policy(),
        trimmed,
        &probe,
        role,
    ))
}

/// The cloud-drive folders of `~/Library` (iCloud Drive, Dropbox, OneDrive, Google Drive …): a
/// folder inside one may be a source (an Obsidian vault in iCloud), the folder itself may not, and
/// only Markdown and text files are read from them ([`TEXT_EXTS`]).
pub const CLOUD_DIRS: [&str; 2] = ["Library/Mobile Documents", "Library/CloudStorage"];
/// The extensions an include rule of a cloud folder may name.
pub const TEXT_EXTS: [&str; 3] = ["md", "markdown", "txt"];

/// Is `c` (canonical) inside one of the [`CLOUD_DIRS`], not one of them?
pub fn is_cloud(policy: &FolderPolicy, c: &Path) -> bool {
    CLOUD_DIRS.iter().any(|d| {
        let root = policy.home.join(d);
        c != root && c.starts_with(&root)
    })
}

/// Does an include rule name only Markdown or text files (`**/*.md`, `notes/*.txt`)?
fn text_only(pattern: &str) -> bool {
    let name = pattern.rsplit('/').next().unwrap_or(pattern);
    name.rsplit_once('.')
        .is_some_and(|(_, e)| TEXT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The include rules a cloud folder may use: refused when any names other files.
fn check_cloud_include(policy: &FolderPolicy, path: &Path, include: &[String]) -> ApiResult<()> {
    if is_cloud(policy, path) && !include.iter().all(|p| text_only(p)) {
        return Err(refused(&steve_i18n::t!("knowledge.folder.cloud_text_only")));
    }
    Ok(())
}

/// A patch's include rules for the folder source `old` ([`CLOUD_DIRS`]: text files only).
pub fn check_patch_include(
    policy: &FolderPolicy,
    old: &SourceRow,
    b: &SourcePatch,
) -> ApiResult<()> {
    match (&b.include, folder_root(old)) {
        (Some(i), Some(root)) if !i.is_empty() => check_cloud_include(policy, &root, i),
        _ => Ok(()),
    }
}

/// The include rules a scan uses: for a cloud folder only the text ones (an older source may hold
/// others), the default when none is left.
fn scan_include(policy: &FolderPolicy, root: &Path, include: Vec<String>) -> Vec<String> {
    if !is_cloud(policy, root) {
        return include;
    }
    let kept: Vec<String> = include.into_iter().filter(|p| text_only(p)).collect();
    if kept.is_empty() {
        default_include()
    } else {
        kept
    }
}

/// A folder outside `$HOME` and `/Volumes` (coordinator decision, 2026-11-01).
fn outside_home() -> String {
    steve_i18n::t!("knowledge.folder.outside_home")
}

/// The whole message of a refused path: the outside-home reason stands alone, the others follow
/// “这个目录不能加入知识库：”.
pub fn refusal(reason: &str) -> String {
    if reason == outside_home() {
        reason.to_string()
    } else {
        steve_i18n::t!("knowledge.folder.refused", reason = reason)
    }
}

/// The first folder source of `others` whose path contains `path` or lies inside it.
pub fn overlap<'a>(path: &Path, others: &'a [SourceRow]) -> Option<&'a SourceRow> {
    others
        .iter()
        .find(|s| folder_root(s).is_some_and(|o| o.starts_with(path) || path.starts_with(&o)))
}

fn refused(reason: &str) -> ApiError {
    ApiError::rule(refusal(reason))
}

/// The stored (canonical) root of a folder source.
pub fn folder_root(s: &SourceRow) -> Option<PathBuf> {
    s.config
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The title of a file without front matter: its name without the extension.
pub fn default_title(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

fn ext(rel: &str) -> String {
    rel.rsplit('/')
        .next()
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// `kb_docs.mime` of a folder file, by extension.
pub fn mime_of(rel: &str) -> &'static str {
    match ext(rel).as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "text" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

/// The extractor for some bytes (`ext` decides for text; binary formats are sniffed).
pub fn format_of(mime: &str, name: &str, bytes: &[u8]) -> Format {
    match ext(name).as_str() {
        "md" | "markdown" => return Format::Markdown,
        "txt" | "text" | "log" => return Format::Plain,
        _ => {}
    }
    match extract_text::detect(mime, bytes) {
        Format::Unsupported if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() => {
            Format::Plain
        }
        f => f,
    }
}

/// Extracts on the right thread: text inline, binary formats isolated.
pub async fn load_bytes(format: Format, bytes: Vec<u8>) -> Loaded {
    let r = match format {
        // inline extraction is still parsing: off the runtime too
        Format::Markdown | Format::Plain => {
            tokio::task::spawn_blocking(move || extract_text::extract(format, &bytes))
                .await
                .unwrap_or(Err(ExtractError::Panicked))
        }
        _ => extract_text::extract_isolated(format, bytes).await,
    };
    match r {
        Ok(ex) => Loaded::Text(ex),
        Err(ExtractError::Unsupported) => Loaded::Skipped(ExtractError::Unsupported.to_string()),
        Err(e) => Loaded::Failed(e.to_string()),
    }
}

/// Reads folder files for the index queue through the folder's Node.
pub struct FolderLoader {
    pub(crate) nodes: Arc<NodeFolders>,
}

impl DocLoader for FolderLoader {
    fn kinds(&self) -> &[&str] {
        &["folder"]
    }
    fn load<'a>(
        &'a self,
        _: &'a Modules,
        doc: &'a DocRow,
        source: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        let rel = doc.r#ref.clone();
        let mime = doc.mime.clone();
        Box::pin(async move {
            match self.nodes.load(&source.id, &doc.id, &rel).await {
                NodeRead::Bytes(b) => load_bytes(format_of(&mime, &rel, &b), b).await,
                NodeRead::Gone => Loaded::Gone,
                NodeRead::TooBig => Loaded::Skipped(TOO_BIG.into()),
                NodeRead::Refused(why) => Loaded::Failed(why.into()),
                NodeRead::Changed => Loaded::Retry { now: true },
                NodeRead::Offline | NodeRead::Unsettled => Loaded::Retry { now: false },
            }
        })
    }
}

fn front_mark(doc_id: &str) -> String {
    format!("front:{doc_id}")
}

/// Does `v` show the document to no one `base` hides it from? `private` stays private, a team or
/// space may only become private, `workspace` may become anything.
pub fn narrows(base: &Visibility, v: &Visibility) -> bool {
    match base {
        Visibility::Workspace => true,
        Visibility::Private => *v == Visibility::Private,
        _ => *v == Visibility::Private || v == base,
    }
}

/// Applies a document's front matter (`title`, `tags`, `steve.level`, `steve.visibility`
/// override the document's settings) and returns the row again. Which fields came from the front
/// matter is remembered in `kb_marks front:<doc>`, so removing a field from the file hands the
/// setting back to the source. Uploaded files keep the title they were added with.
pub fn apply_front(
    tx: &mut steve_store::W<'_>,
    doc: &DocRow,
    src: &SourceRow,
    front: &FrontMatter,
    now: i64,
) -> Result<DocRow> {
    let ws = &doc.workspace_id;
    let prev: Value = steve_store::knowledge::Documents(tx)
        .mark(ws, &front_mark(&doc.id))?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}));
    let had = |k: &str| prev.get(k).and_then(Value::as_bool).unwrap_or(false);
    let folder = src.kind == "folder";

    let front_title = front
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| folder && !t.is_empty())
        .map(|t| t.chars().take(120).collect::<String>());
    let title = match &front_title {
        Some(t) => t.clone(),
        None if had("title") => default_title(&doc.r#ref),
        None => doc.title.clone(),
    };
    let front_level = front.level.as_deref().and_then(Level::parse);
    let level = match front_level {
        Some(l) => Some(l),
        None if had("level") => None,
        None => doc.level,
    };
    // what the document would have without its front matter: front matter may only narrow that
    let base_vis = match &doc.visibility {
        Some(v) if !had("visibility") => v,
        _ => &src.visibility,
    };
    let front_vis = match front.visibility.as_deref().and_then(Visibility::parse) {
        Some(v) if narrows(base_vis, &v) && steve_store::References(tx).scope_exists(ws, &v)? => {
            Some(v)
        }
        _ => None,
    };
    let visibility = match &front_vis {
        Some(v) => Some(v.clone()),
        None if had("visibility") => None,
        None => doc.visibility.clone(),
    };
    let tags = if folder || !front.tags.is_empty() {
        front.tags.clone()
    } else {
        doc.tags.clone()
    };
    if title != doc.title || tags != doc.tags || level != doc.level || visibility != doc.visibility
    {
        steve_store::knowledge::Documents(tx).set_doc_front(
            &doc.id,
            &title,
            &tags,
            visibility.as_ref(),
            level,
            now,
        )?;
    }
    let mark = json!({
        "title": front_title.is_some(),
        "level": front_level.is_some(),
        "visibility": front_vis.is_some(),
    });
    if front_title.is_some() || front_level.is_some() || front_vis.is_some() {
        steve_store::knowledge::Documents(tx).set_mark(
            ws,
            &front_mark(&doc.id),
            &mark.to_string(),
            now,
        )?;
    } else if prev.as_object().is_some_and(|o| !o.is_empty()) {
        steve_store::knowledge::Documents(tx).delete_mark(ws, &front_mark(&doc.id))?;
    }
    steve_store::knowledge::Documents(tx)
        .doc(&doc.id)?
        .ok_or(StoreError::NotFound("没有这个文档"))
}

// ---- scanning

/// What one scan did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Files found (indexable size).
    pub files: usize,
    /// New or changed files sent to the index.
    pub changed: usize,
    /// Documents whose file disappeared.
    pub deleted: usize,
    /// Over 2 MB or over 20000 files.
    pub skipped: usize,
    /// Links pointing outside the folder.
    pub links_refused: usize,
    /// Documents whose file was not found but which stayed, because the walk was incomplete
    /// (see `incomplete`) or their path could not be looked at (a refused link).
    pub kept: usize,
    /// Why the walk was incomplete ([`scan::Gap::code`]); empty when it saw everything.
    pub incomplete: Vec<&'static str>,
}

/// Marks a folder source as syncing. `Ok(None)` = already syncing, paused, or not a folder.
pub fn claim(m: &Modules, id: &str) -> ApiResult<Option<SourceRow>> {
    let now = m.clock.now();
    Ok(m.store.write(|tx| match Documents(tx).source(id)? {
        Some(s) if s.kind == "folder" && Documents(tx).claim_sync(id, now)? => {
            Ok::<_, StoreError>(Documents(tx).source(id)?)
        }
        _ => Ok(None),
    })?)
}

/// Runs a claimed scan to the end; the source ends `ok` (with stats) or `error` (with the reason).
pub async fn sync(m: &Modules, kb: &Knowledge, src: SourceRow) -> ApiResult<SyncReport> {
    let r = run(m, kb, &src).await;
    let now = m.clock.now();
    let (ws, id) = (src.workspace_id.clone(), src.id.clone());
    let (status, error, stats) = match &r {
        Ok((_, stats)) => ("ok", None, Some(stats)),
        Err(why) => {
            tracing::warn!(source = %id, "kb.folder_scan: {why}");
            ("error", Some(why.as_str()), None)
        }
    };
    m.store.write(|w| -> Result<()> {
        Documents(w).finish_sync(&id, status, error, stats, now)?;
        crate::state::emit_change(w, &ws, &id);
        Ok(())
    })?;
    r.map(|(rep, _)| rep).map_err(ApiError::Internal)
}

/// Claims and runs a scan in one go (tests, the job). `Ok(None)` if it could not be claimed.
pub async fn sync_now(m: &Modules, kb: &Knowledge, id: &str) -> ApiResult<Option<SyncReport>> {
    match claim(m, id)? {
        Some(src) => sync(m, kb, src).await.map(Some),
        None => Ok(None),
    }
}

/// Claims, then scans in the background. `false` if already syncing (or paused).
pub fn spawn_sync(m: &Modules, kb: &Knowledge, id: &str) -> ApiResult<bool> {
    let Some(src) = claim(m, id)? else {
        return Ok(false);
    };
    let (m, kb) = (m.clone(), kb.clone());
    tokio::spawn(async move {
        if let Err(e) = sync(&m, &kb, src).await {
            tracing::debug!("kb.folder_scan: {e}");
        }
    });
    Ok(true)
}

async fn run(
    m: &Modules,
    kb: &Knowledge,
    src: &SourceRow,
) -> std::result::Result<(SyncReport, Value), String> {
    let root = folder_root(src).ok_or("这个来源没有设置文件夹")?;
    let include = scan_include(kb.folder_policy(), &root, str_list(&src.config, "include"));
    let places = kb
        .folder_nodes()
        .places()
        .ok_or_else(|| folder_node::OFFLINE.to_string())?;
    let place = places
        .place(&src.id, true)
        .await
        .map_err(|u| folder_node::unplaced(&u))?
        .ok_or_else(|| folder_node::OFFLINE.to_string())?;
    let policy = kb.folder_policy().clone();
    let role = src.role;
    let root_str = root.to_string_lossy().into_owned();
    let canonical = match places
        .check_path(&root_str)
        .await
        .map_err(|u| folder_node::unplaced(&u))?
    {
        Some(probe) => {
            check_path_probe_for_role(&policy, &root_str, &probe, role).map_err(|r| refusal(&r))?
        }
        None => return Err(folder_node::OFFLINE.to_string()),
    };
    if canonical != root {
        return Err("文件夹的位置变了（路径经过的链接指向了别处），请重新添加".to_string());
    }
    let scanned = folder_node::scan(&place, include, str_list(&src.config, "exclude")).await?;
    let (walked, versions) = (scanned.walk, Some(scanned.versions));

    let now = m.clock.now();
    let mut rep = SyncReport {
        files: walked.files.len(),
        skipped: walked.too_big.len() + walked.over_limit,
        links_refused: walked.links_refused,
        incomplete: walked.gaps.iter().map(|g| g.code()).collect(),
        ..Default::default()
    };
    let bytes: u64 = walked.files.iter().map(|e| e.size).sum();
    // thousands of upserts for a large folder: on the blocking pool, not a runtime worker
    let (store, s) = (m.store.clone(), src.clone());
    let (changed, mut rep) = index::blocking(move || {
        let changed = store.write(|tx| reconcile(tx, &s, &walked, now, &mut rep))?;
        Ok((changed, rep))
    })
    .await
    .map_err(|e| format!("同步失败：{e}"))?;
    rep.changed = changed.len();
    if let Some(versions) = versions {
        kb.folder_nodes().remember(
            changed
                .iter()
                .filter_map(|(id, rel)| Some((id.clone(), versions.get(rel)?.clone()))),
        );
    }
    let changed: Vec<String> = changed.into_iter().map(|(id, _)| id).collect();
    for batch in changed.chunks(BATCH) {
        index::run_batch(m, kb, batch)
            .await
            .map_err(|e| format!("入库失败：{e}"))?;
    }
    let (store, s) = (m.store.clone(), src.clone());
    let (docs, chunks) = index::blocking(move || store.read(|c| Documents(c).source_counts(&s)))
        .await
        .map_err(|e| e.to_string())?;
    let mut stats = json!({"docs": docs, "chunks": chunks, "skipped": rep.skipped, "bytes": bytes});
    if rep.kept > 0 {
        stats["kept"] = json!(rep.kept);
    }
    if !rep.incomplete.is_empty() {
        stats["incomplete"] = json!(rep.incomplete);
        tracing::info!(
            source = %src.id,
            kept = rep.kept,
            "kb.folder_scan: incomplete walk ({}), nothing deleted",
            rep.incomplete.join(", ")
        );
    }
    Ok((rep, stats))
}

/// Brings the documents in line with the walk: new files inserted, files whose `(mtime, size)`
/// changed reactivated, vanished files `deleted` (index rows removed). Returns the ids to index,
/// with their paths.
///
/// Only a complete walk deletes: when part of the folder was not seen (an unreadable
/// directory, over the file limit), a missing file is no proof it is gone, so its document is
/// `kept`. Paths the walk saw but could not read (`held`) are never deleted: a link out of the
/// folder fails its document with [`OUTSIDE`] (chunks removed, as the loader does), one the
/// Node refuses with [`folder_node::DENIED`], an unreadable path keeps its document as it is.
fn reconcile(
    tx: &mut W<'_>,
    src: &SourceRow,
    w: &scan::Walk,
    now: i64,
    rep: &mut SyncReport,
) -> Result<Vec<(String, String)>> {
    if Documents(tx).source(&src.id)?.is_none() {
        return Ok(Vec::new()); // deleted meanwhile
    }
    let existing: HashMap<String, DocRow> = Documents(tx)
        .docs_of_source(&src.id)?
        .into_iter()
        .map(|d| (d.r#ref.clone(), d))
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut changed = Vec::new();
    let upsert = |tx: &mut W<'_>, e: &scan::Entry| -> Result<(String, bool)> {
        match existing.get(&e.rel) {
            None => {
                let id = uid("kd");
                Documents(tx).insert_doc(
                    &src.workspace_id,
                    &NewDoc {
                        id: id.clone(),
                        source_id: src.id.clone(),
                        r#ref: e.rel.clone(),
                        title: default_title(&e.rel),
                        mime: mime_of(&e.rel).into(),
                        size: e.size as i64,
                        mtime: Some(e.mtime),
                        created_by: src.created_by.clone(),
                        ..Default::default()
                    },
                    now,
                )?;
                Ok((id, true))
            }
            Some(d) => {
                let moved = d.status == "deleted"
                    || d.mtime != Some(e.mtime)
                    || d.size != e.size as i64
                    || d.indexed.is_none();
                if moved {
                    Documents(tx).set_doc_seen(&d.id, e.size as i64, e.mtime, now)?;
                }
                Ok((d.id.clone(), moved))
            }
        }
    };
    for e in &w.files {
        seen.insert(&e.rel);
        let (id, moved) = upsert(tx, e)?;
        if moved {
            changed.push((id, e.rel.clone()));
        }
    }
    for e in &w.too_big {
        seen.insert(&e.rel);
        let (id, moved) = upsert(tx, e)?;
        if moved && let Some(d) = Documents(tx).doc(&id)? {
            index::fail_doc(tx, &d, "skipped", TOO_BIG, now)?;
        }
    }
    let held: HashMap<&str, scan::Held> = w.held.iter().map(|(r, h)| (r.as_str(), *h)).collect();
    for (rel, d) in &existing {
        if !seen.contains(rel.as_str()) && d.status != "deleted" {
            let why = held.get(rel.as_str()).copied();
            let fails = match why {
                Some(scan::Held::Outside) => Some(OUTSIDE),
                Some(scan::Held::Denied) => Some(folder_node::DENIED),
                _ => None,
            };
            if let Some(error) = fails
                && (d.status != "failed" || d.error.as_deref() != Some(error))
            {
                index::fail_doc(tx, d, "failed", error, now)?;
            }
            if !w.complete() || why.is_some() {
                rep.kept += 1;
                continue;
            }
            if src.role == SourceRole::Memory {
                crate::memory_file::mark_file_deleted(tx, src, &d.r#ref, now)?;
            }
            index::remove(tx, "chunk", &d.id)?;
            Documents(tx).set_doc_indexed(&d.id, "", 0, "deleted", None, now)?;
            rep.deleted += 1;
        }
    }
    Ok(changed)
}

/// Scans every unpaused folder source once, one after another. Returns how many ran.
pub async fn scan_all(m: &Modules, kb: &Knowledge) -> usize {
    let list = match m.store.read(|c| Documents(c).folder_sources(None)) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("kb.folder_scan: {e}");
            return 0;
        }
    };
    let mut n = 0;
    for s in list.iter().filter(|s| !s.paused) {
        match sync_now(m, kb, &s.id).await {
            Ok(Some(_)) => n += 1,
            Ok(None) => {}
            Err(e) => tracing::debug!(source = %s.id, "kb.folder_scan: {e}"),
        }
    }
    n
}

/// The `kb.folder_scan` job: clears syncing states left by a previous run, then scans all
/// folder sources now and every 10 minutes. The integrator starts it once.
pub async fn scan_job(m: Modules, kb: Knowledge) {
    if let Err(e) = m.store.write(|tx| Documents(tx).reset_syncing()) {
        tracing::warn!("kb.folder_scan: {e}");
    }
    let (all_m, all_kb) = (m.clone(), kb.clone());
    let all = steve_runtime::every_after(
        "kb.folder_scan_all",
        m.clock.clone(),
        SCAN_EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let (m, kb) = (all_m.clone(), all_kb.clone());
            async move {
                scan_all(&m, &kb).await;
            }
        },
    );
    let requested = async {
        loop {
            kb.scan_wake.notified().await;
            steve_runtime::round("kb.scans", async {
                for id in kb.take_scan_requests() {
                    if let Err(e) = sync_now(&m, &kb, &id).await {
                        tracing::debug!(source = %id, "kb.folder_scan: {e}");
                    }
                }
            })
            .await;
        }
    };
    tokio::join!(all, requested);
}

// ---- REST operations

/// `own_source_in(ws, me, id)`: in the caller's workspace; a folder source also visible to them.
pub fn own_source_in(c: &mut impl StorageRead, who: &Caller, id: &str) -> ApiResult<SourceRow> {
    match Documents(c).source(id)? {
        Some(s) if s.workspace_id == who.ws => {
            if is_builtin(&s.kind) {
                return Ok(s);
            }
            let viewer = base::Viewer::bare(&who.ws, &who.me);
            if viewer.sees(&s.visibility, &s.created_by) {
                Ok(s)
            } else {
                Err(ApiError::not_found(NOUN_SOURCE))
            }
        }
        _ => Err(ApiError::not_found(NOUN_SOURCE)),
    }
}

fn check_scope(c: &mut impl StorageRead, ws: &str, v: Option<&Visibility>) -> ApiResult<()> {
    match v {
        Some(v) if !References(c).scope_exists(ws, v)? => Err(ApiError::not_found(match v {
            Visibility::Space(_) => "空间",
            _ => "团队",
        })),
        _ => Ok(()),
    }
}

fn check_patterns(ck: &mut Check, field: &str, xs: Option<&Vec<String>>, max: usize) {
    if let Some(xs) = xs {
        ck.list(field, xs, max, 200);
        for (i, x) in xs.iter().enumerate() {
            ck.that(&format!("{field}.{i}"), scan::valid_pattern(x));
        }
    }
}

/// `POST /kb/sources`.
#[derive(Debug, Deserialize)]
pub struct SourceBody {
    pub kind: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub role: Option<SourceRole>,
    #[serde(default)]
    pub write_mode: Option<WriteMode>,
}

/// A folder source that passed the request and path checks, ready to insert. Shared by
/// `POST /kb/sources` and the `kb.source.create` draft operation.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFolder {
    pub name: String,
    /// Canonical.
    pub path: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub visibility: Visibility,
    pub level: Level,
    pub role: SourceRole,
    pub write_mode: WriteMode,
}

/// Checks a `POST /kb/sources` body: fields (`400 参数不对`) and the single Node-probed path rules
/// (`400 这个目录不能加入知识库：<原因>`). Overlap and scopes are checked by [`insert_folder`].
pub fn check_new(kb: &Knowledge, b: &SourceBody) -> ApiResult<NewFolder> {
    let mut ck = Check::new();
    ck.one_of("kind", &b.kind, &["folder"])
        .len("name", b.name.trim(), 1, 40)
        .len("path", &b.path, 1, MAX_PATH);
    check_patterns(&mut ck, "include", b.include.as_ref(), 20);
    check_patterns(&mut ck, "exclude", b.exclude.as_ref(), 50);
    ck.done()?;
    let role = b.role.unwrap_or(SourceRole::Knowledge);
    let write_mode = b.write_mode.unwrap_or(WriteMode::ReadOnly);
    let path = check_path_via_node(kb, &b.path, role)?.map_err(|r| refused(&r))?;
    let include = match &b.include {
        Some(i) if !i.is_empty() => i.clone(),
        _ => default_include(),
    };
    check_cloud_include(kb.folder_policy(), &path, &include)?;
    Ok(NewFolder {
        name: b.name.trim().to_string(),
        path,
        include,
        exclude: b.exclude.clone().unwrap_or_default(),
        visibility: b.visibility.clone().unwrap_or(Visibility::Private),
        level: b.level.unwrap_or(Level::Conclusion),
        role,
        write_mode,
    })
}

fn default_include() -> Vec<String> {
    scan::DEFAULT_INCLUDE
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The reason `path` would overlap an existing folder source of `ws`, if it does.
pub fn overlap_reason_in(
    c: &mut impl StorageRead,
    ws: &str,
    path: &Path,
) -> Result<Option<String>> {
    let others = Documents(c).folder_sources(Some(ws))?;
    Ok(overlap(path, &others).map(|o| steve_i18n::t!("knowledge.folder.overlap", name = o.name)))
}

/// Inserts a checked folder source in `tx` (overlap and scope checked here, inside the write).
pub fn insert_folder(
    tx: &mut W<'_>,
    ws: &str,
    me: &str,
    f: &NewFolder,
    now: i64,
) -> ApiResult<SourceRow> {
    if let Some(why) = overlap_reason_in(tx, ws, &f.path)? {
        return Err(refused(&why));
    }
    check_scope(tx, ws, Some(&f.visibility))?;
    let config = json!({
        "path": f.path.to_string_lossy(),
        "include": f.include,
        "exclude": f.exclude,
    });
    let id = uid("ks");
    Documents(tx).insert_source(
        &id,
        ws,
        "folder",
        &f.name,
        &config,
        &f.visibility,
        f.level,
        f.role,
        f.write_mode,
        me,
        now,
    )?;
    tx.audit(
        ws,
        me,
        "kb_source.create",
        &id,
        &json!({"kind": "folder", "path": config["path"]}),
    )?;
    Documents(tx)
        .source(&id)?
        .ok_or_else(|| ApiError::not_found(NOUN_SOURCE))
}

/// Creates a folder source after the checks. The caller then starts its first scan.
pub fn create(m: &Modules, kb: &Knowledge, who: &Caller, b: SourceBody) -> ApiResult<SourceRow> {
    let f = check_new(kb, &b)?;
    service::prepare(m, who)?;
    let now = m.clock.now();
    m.store.write(|tx| {
        let s = insert_folder(tx, &who.ws, &who.me, &f, now)?;
        crate::state::emit_change(tx, &who.ws, &s.id);
        Ok(s)
    })
}

/// `403` for a visibility or level change of a built-in source by anyone but the owner.
pub fn owner_only() -> String {
    steve_i18n::t!("knowledge.folder.owner_only")
}

/// Is `user` the owner of `ws`?
pub fn is_owner_in(c: &mut impl StorageRead, ws: &str, user: &str) -> ApiResult<bool> {
    let role = Members::new(c).role(ws, user)?;
    Ok(role.as_deref() == Some("owner"))
}

/// `PATCH /kb/sources/:id`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, serde::Serialize)]
pub struct SourcePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<SourceRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mode: Option<WriteMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

impl SourcePatch {
    /// The patch that puts `s` back the way it is now (draft undo).
    pub fn restoring(s: &SourceRow) -> Self {
        let folder = s.kind == "folder";
        Self {
            name: Some(s.name.clone()),
            paused: Some(s.paused),
            visibility: Some(s.visibility.clone()),
            level: Some(s.level),
            role: Some(s.role),
            write_mode: Some(s.write_mode),
            include: folder.then(|| str_list(&s.config, "include")),
            exclude: folder.then(|| str_list(&s.config, "exclude")),
        }
    }
}

fn snapshot(s: &SourceRow) -> Value {
    json!({
        "name": s.name, "paused": s.paused, "visibility": s.visibility, "level": s.level,
        "role": s.role, "write_mode": s.write_mode,
        "include": s.config.get("include"), "exclude": s.config.get("exclude"),
    })
}

/// What [`patch_tx`] did.
#[derive(Debug, Clone)]
pub struct Patched {
    /// The source before the change.
    pub old: SourceRow,
    /// `Source` after it.
    pub source: Value,
    /// A folder needs a new scan (rules changed, or resumed).
    pub rescan: bool,
}

/// Checks a patch on its own (field lengths and patterns), before any transaction.
pub fn check_patch(b: &SourcePatch) -> ApiResult<()> {
    let mut ck = Check::new();
    ck.opt_len("name", b.name.as_deref().map(str::trim), 1, 40);
    check_patterns(&mut ck, "include", b.include.as_ref(), 20);
    check_patterns(&mut ck, "exclude", b.exclude.as_ref(), 50);
    ck.done()
}

/// Is `b` refused for `old` by the owner rule?
pub fn owner_refuses_in(
    c: &mut impl StorageRead,
    who: &Caller,
    old: &SourceRow,
    b: &SourcePatch,
) -> ApiResult<bool> {
    let rescoping = b.visibility.as_ref().is_some_and(|v| *v != old.visibility)
        || b.level.is_some_and(|l| l != old.level);
    Ok(is_builtin(&old.kind) && rescoping && !is_owner_in(c, &who.ws, &who.me)?)
}

/// The write of `PATCH /kb/sources/:id` inside a caller's transaction: ownership, the owner rule,
/// scopes; scope changes rewrite the index rows of every document that follows the source
/// ([`index::rescope_source`]); a revision and an audit entry are written. No event is sent.
pub fn patch_tx(
    tx: &mut W<'_>,
    who: &Caller,
    id: &str,
    b: &SourcePatch,
    now: i64,
) -> ApiResult<Patched> {
    check_patch(b)?;
    let old = own_source_in(tx, who, id)?;
    let folder = old.kind == "folder";
    // built-in sources are the whole workspace's: only its owner widens or narrows them
    if owner_refuses_in(tx, who, &old, b)? {
        return Err(ApiError::forbidden(owner_only()));
    }
    Check::new()
        .that("include", folder || b.include.is_none())
        .that("exclude", folder || b.exclude.is_none())
        .done()?;
    check_scope(tx, &who.ws, b.visibility.as_ref())?;
    let mut s = old.clone();
    if let Some(n) = &b.name {
        s.name = n.trim().to_string();
    }
    if let Some(p) = b.paused {
        s.paused = p;
    }
    if let Some(v) = &b.visibility {
        s.visibility = v.clone();
    }
    if let Some(l) = b.level {
        s.level = l;
    }
    if let Some(role) = b.role {
        s.role = role;
    }
    if let Some(wm) = b.write_mode {
        s.write_mode = wm;
    }
    if let Some(cfg) = s.config.as_object_mut() {
        if let Some(i) = &b.include {
            let i = if i.is_empty() {
                default_include()
            } else {
                i.clone()
            };
            cfg.insert("include".into(), json!(i));
        }
        if let Some(e) = &b.exclude {
            cfg.insert("exclude".into(), json!(e));
        }
    }
    if s == old {
        let source = source_json(tx, &old)?;
        return Ok(Patched {
            old,
            source,
            rescan: false,
        });
    }
    s.revision += 1;
    Documents(tx).update_source(&s, now)?;
    if s.visibility != old.visibility || s.level != old.level {
        index::rescope_source(tx, &s)?;
    }
    let (before, after) = (snapshot(&old), snapshot(&s));
    Revisions(tx).append(
        &who.ws,
        "kb_source",
        &s.id,
        s.revision,
        &json!({"before": before, "after": after}),
        &who.me,
    )?;
    tx.audit(
        &who.ws,
        &who.me,
        "kb_source.patch",
        &s.id,
        &json!({"before": before, "after": after}),
    )?;
    let rescan = folder
        && !s.paused
        && (old.paused
            || s.role != old.role
            || s.config.get("include") != old.config.get("include")
            || s.config.get("exclude") != old.config.get("exclude"));
    let s = Documents(tx)
        .source(id)?
        .ok_or_else(|| ApiError::not_found(NOUN_SOURCE))?;
    Ok(Patched {
        old,
        source: source_json(tx, &s)?,
        rescan,
    })
}

/// Changes a source ([`patch_tx`] in its own transaction, then the event). Returns the `Source`
/// and whether a folder needs a new scan.
pub fn patch(
    m: &Modules,
    kb: &Knowledge,
    who: &Caller,
    id: &str,
    b: SourcePatch,
) -> ApiResult<(Value, bool)> {
    check_patch(&b)?;
    let now = m.clock.now();
    let p = m.store.write(|tx| {
        check_patch_include(kb.folder_policy(), &own_source_in(tx, who, id)?, &b)?;
        let p = patch_tx(tx, who, id, &b, now)?;
        crate::state::emit_change(tx, &who.ws, id);
        Ok::<_, ApiError>(p)
    })?;
    Ok((p.source, p.rescan))
}

/// Deletes a folder source with its documents and index rows inside a caller's transaction
/// (built-in sources: `400 内置来源不能删除，可以暂停`). No event is sent.
pub fn delete_tx(tx: &mut W<'_>, who: &Caller, id: &str) -> ApiResult<()> {
    let s = own_source_in(tx, who, id)?;
    if is_builtin(&s.kind) {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.source.builtin_delete"
        )));
    }
    let docs = Documents(tx).docs_of_source(&s.id)?;
    for d in &docs {
        for entry in MemoryEntries(tx).by_source_path(&s.id, &d.r#ref)? {
            index::remove(tx, "memory_entry", &entry.id)?;
        }
        index::delete_doc(tx, &d.id)?;
        Documents(tx).delete_mark(&s.workspace_id, &front_mark(&d.id))?;
    }
    MemoryEntries(tx).delete_by_source(&s.id)?;
    Documents(tx).delete_source_row(&s.id)?;
    tx.audit(
        &who.ws,
        &who.me,
        "kb_source.delete",
        &s.id,
        &json!({"kind": s.kind, "docs": docs.len()}),
    )?;
    Ok(())
}

/// `DELETE /kb/sources/:id`: built-in sources can only be paused; a folder source takes its
/// documents and index rows with it (the files stay where they are).
pub fn delete(m: &Modules, who: &Caller, id: &str) -> ApiResult<Value> {
    m.store.write(|tx| {
        delete_tx(tx, who, id)?;
        crate::state::emit_change(tx, &who.ws, id);
        Ok::<_, ApiError>(())
    })?;
    Ok(json!({"ok": true}))
}

/// `POST /kb/sources/:id/sync`: checks and claims; the caller scans in the background.
pub fn request_sync(m: &Modules, who: &Caller, id: &str) -> ApiResult<SourceRow> {
    let s = crate::read_api(&m.store, |c| own_source_in(c, who, id))?;
    if s.kind != "folder" {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.source.sync_folder_only"
        )));
    }
    if s.paused {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.source.sync_paused"
        )));
    }
    claim(m, id)?.ok_or_else(|| ApiError::conflict(steve_i18n::t!("knowledge.source.syncing_now")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refusals_in_the_request_language() {
        let p = FolderPolicy::new(
            PathBuf::from("/srv/demo"),
            PathBuf::from("/srv/demo/.steve"),
        );
        let missing = steve_node_api::PathCheckResponse {
            exists: false,
            is_dir: false,
            writable: false,
            canonical: None,
        };
        let zh = check_path_probe(&p, "relative", &missing).map_err(|r| refusal(&r));
        assert_eq!(zh, Err("这个目录不能加入知识库：需要绝对路径".into()));
        let en = steve_i18n::scope(steve_i18n::EN, async {
            (
                check_path_probe(&p, "relative", &missing).map_err(|r| refusal(&r)),
                refusal(&outside_home()),
                owner_only(),
            )
        })
        .await;
        assert_eq!(
            en.0,
            Err(
                "This folder can't be added to the knowledge base: the path must be absolute"
                    .into()
            )
        );
        assert_eq!(
            en.1,
            "You can only add folders in your home folder or on an external disk"
        );
        assert_eq!(en.2, "Only the workspace owner can change built-in sources");
    }

    #[test]
    fn check_path_probe_for_role_permits_context_only_for_memory() {
        let p = FolderPolicy::new(
            PathBuf::from("/srv/demo"),
            PathBuf::from("/srv/demo/.steve"),
        );
        let ctx = "/srv/demo/.steve/context";
        let probe = steve_node_api::PathCheckResponse {
            exists: true,
            is_dir: true,
            writable: true,
            canonical: Some(ctx.into()),
        };
        assert_eq!(
            check_path_probe_for_role(&p, ctx, &probe, SourceRole::Memory),
            Ok(PathBuf::from(ctx))
        );
        assert!(check_path_probe_for_role(&p, ctx, &probe, SourceRole::Knowledge).is_err());
        let data = "/srv/demo/.steve/data";
        let data_probe = steve_node_api::PathCheckResponse {
            exists: true,
            is_dir: true,
            writable: true,
            canonical: Some(data.into()),
        };
        assert!(check_path_probe_for_role(&p, data, &data_probe, SourceRole::Memory).is_err());
    }
}
