/* ── outputs read through the Node that has the item's files (N0e-E5b) ──
With the local Node on (`STEVE_NODE_MODE=local`), or for an item on another device, a document,
file or folder is read with the Node's files face (`list_root`/`list_dir`/`read_file`) and a
branch or commit with its git face, so the Hub opens nothing in the item's places. What is shown
is the same as the Hub's own reads give (the JSON, the bytes, a Range).

Where it is looked for, in order: the workspace of the item's bound session, then its own
directory and its project (as the Hub did: on the Hub's device each path under the local Node's
root that holds it; on another device the places the store names there). An absolute location is
read only on the Hub's own device, and only inside the item's places; one in the temp directory
is the Hub's own and still read where it is (no other device has it). Git: a bound workspace's
through its Node; else the Hub's own checkout on the Hub's device, and on another device none. On
the Hub's device a workspace that has no such repository, branch or commit (a plain directory under
a git project) leaves it to the Hub's own checkout, as the Hub looked there first. */

use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use serde_json::{Value, json};
use steve_node_api as na;

use super::{
    DIFF_MAX, Found, LIST_MAX, Out, TEXT_MAX, branch, commit, first_word, git_doc, home_tilde,
    is_hash, media, plain, text_value,
};
use crate::api::blocking;
use crate::api::projects::{normalize, offline, placement_error};
use crate::error::ApiError;
use crate::wire::workspaces::{At, PlacementError, Subject, Workspaces};

/// A place on a Node and how it is shown.
#[derive(Clone)]
pub(crate) struct Addr {
    node: Arc<dyn na::Node>,
    at: Place,
    shown: String,
}

#[derive(Clone, PartialEq)]
enum Place {
    Root(na::RootPath),
    /// In a workspace, at this path inside it (`""`: its top).
    Ws(na::WorkspaceRef, String),
}

/// Where the branch and commit views read git.
enum Git {
    /// A bound workspace, through its Node; on the Hub's device (`true`) what the workspace's git
    /// does not have is looked for in the Hub's own checkout.
    Node(Arc<dyn na::Node>, na::WorkspaceRef, bool),
    /// The Hub's own checkout (the project or the item's directory) on the Hub's device.
    Hub,
    /// On another device with no workspace to ask: not shown.
    Elsewhere,
}

/// The Hub's own device.
struct Local {
    ws: Arc<Workspaces>,
    /// The item's places as the Hub has them (its directory, project, worktree and scratch dir).
    places: Vec<PathBuf>,
    /// The temp directories: the Hub's own, read where they are.
    tmp: Vec<PathBuf>,
}

pub(crate) struct View {
    bases: Vec<Addr>,
    local: Option<Local>,
    git: Git,
}

/// What a file or directory is, as its Node lists it.
pub(crate) struct Entry {
    is_dir: bool,
    size: u64,
    mtime: Option<i64>,
    version: Option<na::Revision>,
}

impl Entry {
    fn dir() -> Entry {
        Entry {
            is_dir: true,
            size: 0,
            mtime: None,
            version: None,
        }
    }
    pub(crate) fn is_dir(&self) -> bool {
        self.is_dir
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

enum Seen {
    Here(Entry),
    Gone,
    /// Refused: outside its root, through a link out.
    Outside,
}

/// Why an output cannot be shown: said in the preview, or the request failed (device offline).
pub(crate) enum Fail {
    Missing(String),
    Api(ApiError),
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Fail::Api(e)
    }
}

pub(crate) enum Located {
    Node(Addr, Entry),
    /// Read by the Hub (its temp directory).
    Disk(Found),
    /// Gone from disk, kept by git: rev, path, content.
    Git(String, String, String),
}

fn node_error(e: na::ApiError) -> ApiError {
    match e.code {
        na::ErrorCode::Offline => offline(),
        _ => ApiError::internal(e.safe_detail),
    }
}

/// `rel` below `base` (both relative, `/`-separated): `.` dropped, `..` only within `rel`.
fn joined(base: &str, rel: &str) -> Option<(String, String)> {
    let mut parts: Vec<&str> = vec![];
    for c in FsPath::new(rel).components() {
        match c {
            Component::Normal(n) => parts.push(n.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    let tail = parts.join("/");
    let all = match (base, tail.as_str()) {
        ("", t) => t.to_string(),
        (b, "") => b.to_string(),
        (b, t) => format!("{b}/{t}"),
    };
    na::RelPath::new(all.clone()).ok()?;
    Some((all, tail))
}

impl Addr {
    fn rel(&self) -> &str {
        match &self.at {
            Place::Root(at) => at.rel.as_str(),
            Place::Ws(_, rel) => rel,
        }
    }

    fn at_rel(&self, rel: String, shown: String) -> Option<Addr> {
        let at = match &self.at {
            Place::Root(at) => Place::Root(na::RootPath {
                root: at.root.clone(),
                rel: na::RelPath::new(rel).ok()?,
            }),
            Place::Ws(w, _) => Place::Ws(w.clone(), rel),
        };
        Some(Addr {
            node: self.node.clone(),
            at,
            shown,
        })
    }

    /// `rel` below this place, shown below it.
    fn join(&self, rel: &str) -> Option<Addr> {
        let (all, tail) = joined(self.rel(), rel)?;
        let shown = match tail.as_str() {
            "" => self.shown.clone(),
            t => format!("{}/{t}", self.shown.trim_end_matches('/')),
        };
        self.at_rel(all, shown)
    }

    pub(crate) fn name(&self) -> String {
        self.rel()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn file(&self) -> na::FileAddress {
        match &self.at {
            Place::Root(at) => na::FileAddress::Root(at.clone()),
            Place::Ws(w, rel) => na::FileAddress::Workspace(na::PathRef {
                workspace: w.clone(),
                rel: na::RelPath::new(rel.clone()).expect("checked when joined"),
            }),
        }
    }

    async fn page(
        &self,
        cursor: Option<na::Cursor>,
        limit: u32,
    ) -> Result<na::Page<na::FileInfo>, na::ApiError> {
        let page = na::PageRequest { cursor, limit };
        match &self.at {
            Place::Root(at) => self.node.list_root(at.clone(), page).await,
            Place::Ws(w, rel) => {
                let rel = na::RelPath::new(rel.clone()).expect("checked when joined");
                let p = na::PathRef {
                    workspace: w.clone(),
                    rel,
                };
                self.node.list_dir(p, page).await
            }
        }
    }

    /// What is there: the place itself is a directory when it lists; anything below is found in
    /// its directory's listing.
    async fn stat(&self) -> Result<Seen, ApiError> {
        let seen = |e: na::ApiError| match e.code {
            na::ErrorCode::Invalid => Ok(Seen::Gone),
            na::ErrorCode::Denied => Ok(Seen::Outside),
            _ => Err(node_error(e)),
        };
        let rel = self.rel().to_string();
        let parent = match rel.rsplit_once('/') {
            _ if rel.is_empty() => {
                return match self.page(None, 1).await {
                    Ok(_) => Ok(Seen::Here(Entry::dir())),
                    Err(e) => seen(e),
                };
            }
            Some((p, _)) => p.to_string(),
            None => String::new(),
        };
        let Some(up) = self.at_rel(parent, String::new()) else {
            return Ok(Seen::Gone);
        };
        let mut cursor = None;
        loop {
            let page = match up.page(cursor, 1000).await {
                Ok(p) => p,
                Err(e) => return seen(e),
            };
            if let Some(f) = page.items.into_iter().find(|f| f.path.as_str() == rel) {
                return Ok(Seen::Here(Entry {
                    is_dir: f.is_dir,
                    size: f.size,
                    mtime: f.mtime.map(|t| t.0),
                    version: Some(f.version),
                }));
            }
            cursor = page.next;
            if cursor.is_none() {
                return Ok(Seen::Gone);
            }
        }
    }

    /// Everything right below, as far as a person could look through (an unreadable directory
    /// lists nothing, as on the Hub's disk).
    async fn list(&self) -> Result<Vec<na::FileInfo>, ApiError> {
        const MAX: usize = 50_000;
        let mut out = vec![];
        let mut cursor = None;
        loop {
            let page = match self.page(cursor, 1000).await {
                Ok(p) => p,
                Err(e) if matches!(e.code, na::ErrorCode::Invalid | na::ErrorCode::Denied) => {
                    return Ok(out);
                }
                Err(e) => return Err(node_error(e)),
            };
            out.extend(page.items);
            cursor = page.next;
            if cursor.is_none() || out.len() >= MAX {
                return Ok(out);
            }
        }
    }

    /// `len` bytes from `offset` (fewer at the end), of the file at `expected` when given.
    async fn read(
        &self,
        expected: Option<na::Revision>,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, na::ApiError> {
        let mut out = vec![];
        while (out.len() as u64) < len {
            let want = (len - out.len() as u64).min(u32::MAX as u64) as u32;
            let c = self
                .node
                .read_file(na::FileRead {
                    path: self.file(),
                    expected: expected.clone(),
                    offset: offset + out.len() as u64,
                    max: want,
                })
                .await?;
            let none = c.bytes.is_empty();
            out.extend(c.bytes);
            if c.eof || none {
                break;
            }
        }
        out.truncate(len as usize);
        Ok(out)
    }
}

/// Where the outputs of `item` are read, or `None` when the Hub reads its own disk as always (the
/// local Node is off and the item is on the Hub's device).
pub(super) async fn view(
    ws: Arc<Workspaces>,
    item: String,
    project: Option<String>,
    o: &Out,
    tmp: Vec<PathBuf>,
) -> Result<Option<View>, ApiError> {
    let (cwd, proj) = (o.cwd.clone(), o.project.clone());
    let places: Vec<PathBuf> = o
        .roots
        .iter()
        .filter(|r| !tmp.contains(r))
        .cloned()
        .collect();
    let w = ws.clone();
    enum Pre {
        Local(Option<(Arc<dyn na::Node>, na::WorkspaceRef)>, Vec<Addr>),
        Remote(Vec<crate::wire::workspaces::Placement>),
    }
    let pre = blocking(move || {
        let fail = |e: PlacementError| match e {
            PlacementError::LocalOff => Ok(None),
            e => Err(placement_error(e)),
        };
        let item_p = match w.placement_of(Subject::Item(&item)) {
            Ok(p) => Some(p),
            Err(PlacementError::NoWorkspace | PlacementError::NotFound(_)) => None,
            Err(e) => return fail(e),
        };
        if item_p.as_ref().is_none_or(|p| w.is_local_placement(p)) {
            if let Err(e) = w.local_roots() {
                return fail(e);
            }
            let bound = item_p.and_then(|p| match p.at {
                At::Workspace(r) => Some((p.node, r)),
                At::Root(_) => None,
            });
            let mut bases = vec![];
            for d in [cwd, proj].into_iter().flatten() {
                match w.hub_path(&PathBuf::from(normalize(&d.to_string_lossy()))) {
                    Ok(p) => {
                        if let At::Root(at) = p.at {
                            bases.push(Addr {
                                node: p.node,
                                at: Place::Root(at),
                                shown: home_tilde(&d),
                            });
                        }
                    }
                    // not a root (an archived project): nothing is read there
                    Err(PlacementError::Outside) => {}
                    Err(e) => return fail(e),
                }
            }
            return Ok(Some(Pre::Local(bound, bases)));
        }
        let mut found = vec![item_p.expect("not on the Hub's device")];
        if let Some(pid) = project {
            match w.placement_of(Subject::Project(&pid)) {
                Ok(p) if p.at != found[0].at => found.push(p),
                Ok(_) | Err(PlacementError::NoWorkspace | PlacementError::NotFound(_)) => {}
                Err(e) => return fail(e),
            }
        }
        Ok(Some(Pre::Remote(found)))
    })
    .await?;
    let Some(pre) = pre else { return Ok(None) };
    Ok(Some(match pre {
        Pre::Local(bound, rest) => {
            let mut bases = vec![];
            let git = match bound {
                Some((node, w)) => match workspace_base(&node, &w).await? {
                    Some(a) => {
                        bases.push(a);
                        Git::Node(node, w, true)
                    }
                    // the workspace is gone there: the project's checkout, as the Hub had it
                    None => Git::Hub,
                },
                None => Git::Hub,
            };
            bases.extend(rest);
            View {
                bases,
                local: Some(Local { ws, places, tmp }),
                git,
            }
        }
        Pre::Remote(found) => {
            let mut bases = vec![];
            let mut git = Git::Elsewhere;
            for p in found {
                match p.at {
                    At::Workspace(w) => {
                        if let Some(a) = workspace_base(&p.node, &w).await? {
                            bases.push(a);
                            if matches!(git, Git::Elsewhere) {
                                git = Git::Node(p.node.clone(), w, false);
                            }
                        }
                    }
                    At::Root(at) => bases.push(Addr {
                        node: p.node,
                        shown: match at.rel.as_str() {
                            "" => at.root.as_str().to_string(),
                            rel => format!("{}/{rel}", at.root.as_str()),
                        },
                        at: Place::Root(at),
                    }),
                }
            }
            View {
                bases,
                local: None,
                git,
            }
        }
    }))
}

/// The top of a bound workspace, shown as its Node shows it; `None` when it is gone there.
async fn workspace_base(
    node: &Arc<dyn na::Node>,
    w: &na::WorkspaceRef,
) -> Result<Option<Addr>, ApiError> {
    match node.workspace(w.clone()).await {
        Ok(v) => Ok(Some(Addr {
            node: node.clone(),
            at: Place::Ws(w.clone(), String::new()),
            shown: v.display,
        })),
        Err(e) if e.code == na::ErrorCode::Offline => Err(offline()),
        Err(_) => Ok(None),
    }
}

/// [`super::locate`] through the Node.
pub(crate) async fn locate(o: &Out, v: &View, sub: Option<&str>) -> Result<Located, Fail> {
    let Some(loc) = o.loc.as_deref().map(str::trim).filter(|l| !l.is_empty()) else {
        return Err(Fail::Missing(steve_i18n::t!("hub.output.no_location")));
    };
    let loc = super::super::projects::expand(loc);
    let outside = || Fail::Missing(steve_i18n::t!("hub.output.outside_roots"));
    let abs = FsPath::new(&loc).is_absolute();
    let base = if abs {
        let Some(l) = &v.local else {
            return Err(outside());
        };
        let p = PathBuf::from(normalize(&loc));
        let mine = l
            .places
            .iter()
            .any(|r| p.starts_with(normalize(&r.to_string_lossy())));
        if !mine {
            // the temp directory is the Hub's own: read where it is, as always
            let hub = Out {
                cwd: None,
                project: None,
                roots: l.tmp.clone(),
                ..o.clone()
            };
            let sub = sub.map(str::to_string);
            return blocking(move || Ok(super::locate(&hub, sub.as_deref())))
                .await?
                .map(Located::Disk)
                .map_err(Fail::Missing);
        }
        let ws = l.ws.clone();
        let q = p.clone();
        let placed = blocking(move || Ok(ws.hub_path(&q))).await?;
        let a = match placed {
            Ok(pl) => match pl.at {
                At::Root(at) => Addr {
                    node: pl.node,
                    at: Place::Root(at),
                    shown: home_tilde(&p),
                },
                At::Workspace(_) => return Err(outside()),
            },
            Err(PlacementError::Outside) => return Err(outside()),
            Err(e) => return Err(Fail::Api(placement_error(e))),
        };
        match a.stat().await? {
            Seen::Here(e) => Some((a, e)),
            Seen::Outside => return Err(outside()),
            Seen::Gone => None,
        }
    } else {
        let mut found = None;
        for b in &v.bases {
            let Some(a) = b.join(&loc) else { continue };
            match a.stat().await? {
                Seen::Here(e) => {
                    found = Some((a, e));
                    break;
                }
                Seen::Outside => return Err(outside()),
                Seen::Gone => {}
            }
        }
        found
    };
    let Some((base, entry)) = base else {
        // a document of a finished item: its worktree is gone, the branch or main still has it
        if !abs
            && sub.is_none()
            && plain(&loc).is_some()
            && let Some(g) = node_doc(o, v, &loc).await?
        {
            return Ok(g);
        }
        return Err(Fail::Missing(steve_i18n::t!(
            "hub.output.cleaned_up",
            loc = loc
        )));
    };
    match sub.filter(|s| !s.is_empty()) {
        None => Ok(Located::Node(base, entry)),
        Some(s) => {
            let bad = || Fail::Missing(steve_i18n::t!("hub.output.bad_path"));
            plain(s).ok_or_else(bad)?;
            let missing = || Fail::Missing(steve_i18n::t!("hub.output.path_missing", path = s));
            if !entry.is_dir {
                return Err(missing());
            }
            let a = base.join(s).ok_or_else(bad)?;
            let a = Addr {
                shown: format!("{}/{}", base.shown.trim_end_matches('/'), s),
                ..a
            };
            match a.stat().await? {
                Seen::Here(e) => Ok(Located::Node(a, e)),
                Seen::Gone => Err(missing()),
                Seen::Outside => Err(bad()),
            }
        }
    }
}

/// The document `loc` as the item's branch, else HEAD, has it.
async fn node_doc(o: &Out, v: &View, loc: &str) -> Result<Option<Located>, Fail> {
    let hub = || {
        let (o, loc) = (o.clone(), loc.to_string());
        async move {
            Ok(blocking(move || Ok(git_doc(&o, &loc)))
                .await?
                .map(|f| match f {
                    Found::Git(rev, rel, text) => Located::Git(rev, rel, text),
                    f => Located::Disk(f),
                }))
        }
    };
    match &v.git {
        Git::Elsewhere => Ok(None),
        Git::Hub => hub().await,
        Git::Node(node, w, on_hub) => {
            let Ok(path) = na::RelPath::new(loc) else {
                return Ok(None);
            };
            let revs = o.branch.iter().cloned().chain(["HEAD".to_string()]);
            for rev in revs {
                let read = na::GitRead::Show {
                    rev: rev.clone(),
                    path: Some(path.clone()),
                    // past what is shown, so the cut is made where the Hub's own read made it
                    max: (TEXT_MAX + 4096) as u32,
                };
                let f = match facts(node, w, vec![], vec![read]).await {
                    Ok(f) => f,
                    Err(e) => match e.code {
                        na::ErrorCode::Offline => return Err(Fail::Api(offline())),
                        // not there (or no checkout): the next revision, then the Hub's
                        na::ErrorCode::Invalid | na::ErrorCode::Unsupported => continue,
                        _ => return Err(Fail::Missing(steve_i18n::t!("hub.output.git_failed"))),
                    },
                };
                if let Some(na::GitAnswer::Blob {
                    found: true, bytes, ..
                }) = f.reads.into_iter().next().map(|r| r.answer)
                {
                    let text = String::from_utf8_lossy(&bytes).trim().to_string();
                    return Ok(Some(Located::Git(rev, loc.to_string(), text)));
                }
            }
            match on_hub {
                true => hub().await,
                false => Ok(None),
            }
        }
    }
}

async fn facts(
    node: &Arc<dyn na::Node>,
    w: &na::WorkspaceRef,
    refs: Vec<String>,
    reads: Vec<na::GitRead>,
) -> Result<na::GitFacts, na::ApiError> {
    node.git_facts(na::GitQuery {
        workspace: w.clone(),
        refs,
        ranges: vec![],
        fetch: false,
        reads,
        // a preview reads no working tree: no untracked-file scan for it
        heads_only: true,
    })
    .await
}

fn missing(reason: String) -> Value {
    json!({ "type": "missing", "reason": reason })
}

/// [`super::describe`] through the Node.
pub(crate) async fn describe(o: Out, v: View, sub: Option<String>) -> Result<Value, ApiError> {
    if let Some(u) = o
        .loc
        .as_deref()
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
    {
        return Ok(json!({ "type": "link", "url": u }));
    }
    let k = o.kind.to_lowercase();
    if sub.is_none() {
        let is_commit = k.contains("提交") || k == "commit";
        if is_commit || k.contains("分支") || k == "branch" {
            let hub =
                |o: Out| blocking(move || Ok(if is_commit { commit(&o) } else { branch(&o) }));
            return match &v.git {
                Git::Hub => hub(o).await,
                Git::Elsewhere => Ok(missing(steve_i18n::t!("hub.output.git_elsewhere"))),
                Git::Node(node, w, on_hub) => {
                    let shown = match is_commit {
                        true => node_commit(&o, node, w).await?,
                        false => node_branch(&o, &v, node, w).await?,
                    };
                    match shown {
                        Ok(shown) => Ok(shown),
                        // what the workspace's git does not have, the Hub's checkout may
                        Err(_) if *on_hub => hub(o).await,
                        Err(reason) => Ok(missing(reason)),
                    }
                }
            };
        }
    }
    match locate(&o, &v, sub.as_deref()).await {
        Ok(Located::Node(a, e)) if e.is_dir => node_dir(&a).await,
        Ok(Located::Node(a, e)) => node_file(&a, &e).await,
        Ok(Located::Disk(f)) => blocking(move || Ok(super::shown(f))).await,
        Ok(Located::Git(rev, rel, text)) => Ok(super::from_git(&rev, &rel, &text)),
        Err(Fail::Missing(r)) => Ok(missing(r)),
        Err(Fail::Api(e)) => Err(e),
    }
}

async fn node_dir(a: &Addr) -> Result<Value, ApiError> {
    let mut ents: Vec<(bool, String, u64, Value)> = a
        .list()
        .await?
        .into_iter()
        .filter_map(|f| {
            let name = f.path.as_str().rsplit('/').next()?.to_string();
            let m = f.mtime.map(|t| json!(t.0 as u64)).unwrap_or(Value::Null);
            (!name.starts_with('.')).then_some((!f.is_dir, name, f.size, m))
        })
        .collect();
    ents.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let more = ents.len() > LIST_MAX;
    let entries: Vec<Value> = ents
        .into_iter()
        .take(LIST_MAX)
        .map(|(file, name, size, m)| {
            let form = if file { media(&name).0 } else { None };
            json!({ "name": name, "dir": !file, "size": if file { json!(size) } else { Value::Null }, "modified": m, "form": form })
        })
        .collect();
    Ok(json!({ "type": "dir", "path": a.shown, "entries": entries, "truncated": more }))
}

async fn node_file(a: &Addr, e: &Entry) -> Result<Value, ApiError> {
    let name = a.name();
    let mut v = match media(&name) {
        (Some(form), mime) => {
            json!({ "type": form, "name": name, "mime": mime, "large": super::large(form, e.size) })
        }
        (None, _) => {
            // a file that went or changed meanwhile reads as empty, as on the Hub's disk
            let buf = match a.read(e.version.clone(), 0, TEXT_MAX as u64 + 1).await {
                Ok(b) => b,
                Err(err) if err.code == na::ErrorCode::Offline => return Err(offline()),
                Err(_) => vec![],
            };
            text_value(&name, &buf)
        }
    };
    v["path"] = json!(a.shown);
    v["size"] = json!(e.size);
    v["modified"] = e.mtime.map(|t| json!(t as u64)).unwrap_or(Value::Null);
    Ok(v)
}

/// What a git view shows, or why the workspace's git does not have it (`Err`: the Hub's own
/// checkout may, on its device).
type Shown = Result<Value, String>;

/// A git failure: offline fails the request; a Node with no git face, or git not finishing (its
/// deadline), says so; git refusing (no checkout, no such revision) is `otherwise`.
fn git_miss(e: na::ApiError, otherwise: String) -> Result<Shown, ApiError> {
    match e.code {
        na::ErrorCode::Offline => Err(offline()),
        na::ErrorCode::Unsupported => Ok(Ok(missing(steve_i18n::t!("hub.output.git_elsewhere")))),
        na::ErrorCode::Invalid => Ok(Err(otherwise)),
        _ => Ok(Ok(missing(steve_i18n::t!("hub.output.git_failed")))),
    }
}

/// A log's hash as git abbreviates it there (`%h`); the first 7 from a Node that does not say.
fn short(c: &na::CommitInfo) -> String {
    c.abbrev
        .clone()
        .unwrap_or_else(|| c.sha[..c.sha.len().min(7)].to_string())
}

fn at_ms(t: na::Timestamp) -> Value {
    json!(t.0 as u64)
}

async fn node_commit(
    o: &Out,
    node: &Arc<dyn na::Node>,
    w: &na::WorkspaceRef,
) -> Result<Shown, ApiError> {
    let hash = [
        first_word(&o.title),
        o.loc.as_deref().map(first_word).unwrap_or_default(),
    ]
    .into_iter()
    .find(|h| is_hash(h));
    let Some(hash) = hash else {
        return Ok(Ok(missing(steve_i18n::t!("hub.output.no_commit_hash"))));
    };
    let read = na::GitRead::Show {
        rev: hash.to_string(),
        path: None,
        max: DIFF_MAX as u32,
    };
    // one round: a refusal is no checkout there, an answer of no commit is a commit gone
    let f = match facts(node, w, vec![], vec![read]).await {
        Ok(f) => f,
        Err(e) => return git_miss(e, steve_i18n::t!("hub.output.no_repo")),
    };
    let Some(na::GitAnswer::Commit {
        commit,
        stat,
        patch,
        truncated,
    }) = f.reads.into_iter().next().map(|r| r.answer)
    else {
        return Ok(Err(steve_i18n::t!("hub.output.commit_gone", hash = hash)));
    };
    Ok(Ok(json!({
        "type": "commit",
        "hash": commit.sha,
        "author": commit.author,
        "at": at_ms(commit.time),
        "message": commit.body.as_deref().unwrap_or_default().trim(),
        "stat": stat,
        "diff": patch,
        "truncated": truncated,
    })))
}

async fn node_branch(
    o: &Out,
    v: &View,
    node: &Arc<dyn na::Node>,
    w: &na::WorkspaceRef,
) -> Result<Shown, ApiError> {
    let named = first_word(&o.title);
    let wanted: Vec<&str> = [
        Some(named),
        o.branch.as_deref(),
        Some("main"),
        Some("master"),
    ]
    .into_iter()
    .flatten()
    .filter(|b| !b.is_empty() && !b.starts_with('-'))
    .collect();
    let refs = wanted.iter().map(|b| format!("refs/heads/{b}")).collect();
    let f = match facts(node, w, refs, vec![]).await {
        Ok(f) => f,
        Err(e) => return git_miss(e, steve_i18n::t!("hub.output.no_repo")),
    };
    let has = |b: &str| {
        f.refs
            .iter()
            .any(|(r, sha)| sha.is_some() && r.strip_prefix("refs/heads/") == Some(b))
    };
    let name = [Some(named), o.branch.as_deref()]
        .into_iter()
        .flatten()
        .find(|b| !b.is_empty() && !b.starts_with('-') && has(b));
    let Some(name) = name else {
        let shown = if named.is_empty() {
            o.branch.as_deref().unwrap_or_default()
        } else {
            named
        };
        return Ok(Err(steve_i18n::t!(
            "hub.output.branch_gone",
            branch = shown
        )));
    };
    let base = ["main", "master"]
        .into_iter()
        .find(|b| *b != name && has(b));
    let range = |from: Option<&str>| na::RevRange {
        from: from.map(str::to_string),
        to: name.to_string(),
    };
    let mut reads = vec![na::GitRead::Log {
        range: range(base),
        max: 200,
    }];
    if let Some(b) = base {
        reads.push(na::GitRead::Diff {
            range: range(Some(b)),
            max: DIFF_MAX as u32,
        });
    }
    let answers = match facts(node, w, vec![], reads).await {
        Ok(f) => f.reads.into_iter().map(|r| r.answer).collect(),
        Err(e) => match e.code {
            na::ErrorCode::Offline => return Err(offline()),
            // what git would not say is left empty, as on the Hub's disk
            na::ErrorCode::Invalid | na::ErrorCode::Unsupported => vec![],
            // git not finishing is said, never shown as an empty log
            _ => return Ok(Ok(missing(steve_i18n::t!("hub.output.git_failed")))),
        },
    };
    let mut commits = vec![];
    let (mut stat, mut diff, mut cut) = (String::new(), String::new(), false);
    for a in answers {
        match a {
            na::GitAnswer::Log { commits: cs, .. } => {
                commits = cs
                    .into_iter()
                    .map(|c| {
                        json!({
                            "hash": short(&c),
                            "subject": c.subject,
                            "author": c.author,
                            "at": at_ms(c.time),
                        })
                    })
                    .collect();
            }
            na::GitAnswer::Diff {
                stat: s,
                patch,
                truncated,
            } => (stat, diff, cut) = (s, patch, truncated),
            _ => {}
        }
    }
    Ok(Ok(json!({
        "type": "branch",
        "branch": name,
        "base": base,
        "worktree": worktree(o, v).await?,
        "commits": commits,
        "stat": stat,
        "diff": diff,
        "truncated": cut,
    })))
}

/// The branch's worktree when it is still there (on the Hub's device, under a local root).
async fn worktree(o: &Out, v: &View) -> Result<Value, ApiError> {
    let (Some(l), Some(loc)) = (&v.local, o.loc.as_deref()) else {
        return Ok(Value::Null);
    };
    let p = PathBuf::from(loc);
    if !p.is_absolute() {
        return Ok(Value::Null);
    }
    let (ws, q) = (l.ws.clone(), PathBuf::from(normalize(loc)));
    let Ok(pl) = blocking(move || Ok(ws.hub_path(&q))).await? else {
        return Ok(Value::Null);
    };
    let At::Root(at) = pl.at else {
        return Ok(Value::Null);
    };
    let a = Addr {
        node: pl.node,
        at: Place::Root(at),
        shown: String::new(),
    };
    Ok(match a.stat().await? {
        Seen::Here(_) => json!(home_tilde(&p)),
        _ => Value::Null,
    })
}

/// `len` bytes of the file at `a` from `start`, read from its Node as they are sent. A file that
/// changed meanwhile ends the body early (the client sees a short read, never mixed content).
pub(crate) fn stream(a: Addr, e: &Entry, start: u64, len: u64) -> Body {
    /// A read at a time: what the local Node gives in one go.
    const CHUNK: u64 = 96 * 1024;
    let expected = e.version.clone();
    let s = futures::stream::unfold((a, start, len), move |(a, at, left)| {
        let expected = expected.clone();
        async move {
            if left == 0 {
                return None;
            }
            match a.read(expected, at, left.min(CHUNK)).await {
                Ok(b) if !b.is_empty() => {
                    let n = b.len() as u64;
                    Some((Ok(b), (a, at + n, left - n)))
                }
                Ok(_) => Some((
                    Err(std::io::Error::other("the file ended early")),
                    (a, at, 0),
                )),
                Err(err) => Some((Err(std::io::Error::other(err.safe_detail)), (a, at, 0))),
            }
        }
    });
    Body::from_stream(s)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{g, out, repo};
    use super::super::{describe as on_disk, locate as locate_disk};
    use super::*;
    use std::time::Duration;
    use steve_node_api::WorkspaceExecutor as _;
    use steve_node_api::testkit::FakeNode;

    /// A local Node with one root, `dir` (canonical, so what the Hub shows of it is the same).
    fn local_node(dir: &FsPath) -> Arc<steve_node::LocalNode> {
        Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: na::NodeId::new("local").unwrap(),
                home: dir.join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: dir.join("steve"),
                    codex_home: dir.join("codex"),
                    user_home: dir.to_path_buf(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: vec![steve_node::Root {
                    id: na::RootId::new("r").unwrap(),
                    path: dir.to_path_buf(),
                    display: "local workspace".into(),
                    access: na::RootAccess::ReadWrite,
                }],
                capacity: na::Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 0,
                },
                storage: Default::default(),
                jobs: Default::default(),
            })
            .unwrap(),
        )
    }

    fn root_at(node: Arc<dyn na::Node>, dir: &FsPath, rel: &str) -> Addr {
        Addr {
            node,
            at: Place::Root(na::RootPath {
                root: na::RootId::new("r").unwrap(),
                rel: na::RelPath::new(rel).unwrap(),
            }),
            shown: home_tilde(&dir.join(rel)),
        }
    }

    /// A workspace of `rel` prepared on the local Node (as a bound session's).
    async fn workspace_of(n: &Arc<steve_node::LocalNode>, rel: &str) -> na::WorkspaceRef {
        let mut c = na::testkit::build::cmd(
            "op_prep",
            na::Prepare {
                resource: "item:i1".into(),
                kind: na::PrepareKind::Existing {
                    dir: na::RootPath {
                        root: na::RootId::new("r").unwrap(),
                        rel: na::RelPath::new(rel).unwrap(),
                    },
                },
            },
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        c.issued_at = na::Timestamp(now);
        c.accept_before = na::Timestamp(now + 60_000);
        n.prepare_workspace(c).await.unwrap();
        let w = na::WorkspaceRef {
            node: na::NodeId::new("local").unwrap(),
            id: na::WorkspaceId::new(na::derived_id("w", "item:i1")).unwrap(),
        };
        for _ in 0..500 {
            if n.workspace(w.clone()).await.is_ok() {
                return w;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the workspace was not prepared")
    }

    fn view(bases: Vec<Addr>, git: Git) -> View {
        View {
            bases,
            local: None,
            git,
        }
    }

    async fn both(o: &Out, v: &View, sub: Option<&str>) -> (Value, Value) {
        let disk = {
            let (o, sub) = (o.clone(), sub.map(str::to_string));
            tokio::task::spawn_blocking(move || on_disk(&o, sub.as_deref()))
                .await
                .unwrap()
        };
        let v = View {
            bases: v.bases.clone(),
            local: None,
            git: match &v.git {
                Git::Node(n, w, h) => Git::Node(n.clone(), w.clone(), *h),
                Git::Hub => Git::Hub,
                Git::Elsewhere => Git::Elsewhere,
            },
        };
        let node = describe(o.clone(), v, sub.map(str::to_string))
            .await
            .unwrap();
        (disk, node)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn documents_folders_and_media_read_the_same_through_the_node() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().canonicalize().unwrap();
        let w = dir.join("wt");
        std::fs::create_dir_all(w.join("docs/shots")).unwrap();
        std::fs::write(w.join("docs/说明.md"), "# 标题\n\n正文").unwrap();
        std::fs::write(w.join("docs/shots/a.png"), [0x89, b'P', b'N', b'G', 0, 0]).unwrap();
        std::fs::write(w.join("docs/shots/b.bin"), [1, 0, 2]).unwrap();
        std::fs::write(w.join("docs/shots/.hidden"), "x").unwrap();
        std::fs::write(w.join("docs/报告.docx"), [b'P', b'K', 3, 4, 0]).unwrap();
        std::fs::write(w.join("long.txt"), "中".repeat(TEXT_MAX / 3 + 10)).unwrap();
        let n = local_node(&dir);
        let ws = workspace_of(&n, "wt").await;
        let node: Arc<dyn na::Node> = n.clone();
        // the item's directory as a root path, and as the bound session's workspace
        let as_root = view(vec![root_at(node.clone(), &dir, "wt")], Git::Hub);
        let mut top = Addr {
            node: node.clone(),
            at: Place::Ws(ws.clone(), String::new()),
            shown: home_tilde(&w),
        };
        let as_ws = view(vec![top.clone()], Git::Node(node.clone(), ws.clone(), true));
        for v in [&as_root, &as_ws] {
            for (loc, sub) in [
                ("docs/说明.md", None),
                ("docs/shots/", None),
                ("docs/shots", Some("a.png")),
                ("docs/shots", Some("b.bin")),
                ("docs/shots", Some(".hidden")),
                ("docs/shots", Some("../说明.md")),
                ("docs/shots", Some("/etc/hosts")),
                ("docs/shots", Some("nope.png")),
                ("docs/报告.docx", None),
                ("long.txt", None),
                ("nope.md", None),
                ("https://a.example/x", None),
            ] {
                let o = out("文档", "x", Some(loc), &w, None);
                let (disk, node) = both(&o, v, sub).await;
                assert_eq!(node, disk, "{loc} {sub:?}");
            }
        }
        assert_eq!(
            both(
                &out("文档", "x", Some("docs/说明.md"), &w, None),
                &as_ws,
                None
            )
            .await
            .1["text"],
            "# 标题\n\n正文"
        );

        // raw: the same bytes, a span of them too
        let o = out("文件", "x", Some("docs/shots"), &w, None);
        let Ok(Located::Node(a, e)) = locate(&o, &as_ws, Some("a.png")).await else {
            panic!("located")
        };
        assert!(matches!(
            locate_disk(&o, Some("a.png")),
            Ok(Found::Disk(..))
        ));
        let read = |a: Addr, start, len| {
            let body = stream(a, &e, start, len);
            async move { axum::body::to_bytes(body, usize::MAX).await.unwrap() }
        };
        assert_eq!(
            &read(a.clone(), 0, 6).await[..],
            [0x89, b'P', b'N', b'G', 0, 0]
        );
        assert_eq!(&read(a.clone(), 1, 3).await[..], b"PNG");
        // a file changed since it was listed ends the body: never mixed content
        std::fs::write(w.join("docs/shots/a.png"), b"changed!").unwrap();
        let body = stream(a, &e, 0, 6);
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());

        // a link out of the item's place is refused, as the Hub refused it
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("key"), "s").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), w.join("link")).unwrap();
        let o = out("文件", "x", Some("link/key"), &w, None);
        for v in [&as_root, &as_ws] {
            let (disk, node) = both(&o, v, None).await;
            assert_eq!(disk["type"], "missing");
            assert_eq!(node, disk);
        }
        // and nothing above the place it is in
        top.shown = "x".into();
        assert!(top.join("../wt").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn branches_commits_and_documents_from_git_through_the_node() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().canonicalize().unwrap();
        let r = repo(&dir);
        // a hash length other than 7, as in a repository of real size (`%h` follows it)
        g(&r, &["config", "core.abbrev", "10"]);
        g(&r, &["checkout", "-q", "-b", "steve/st-1"]);
        std::fs::create_dir_all(r.join("docs")).unwrap();
        std::fs::write(r.join("docs/方案.md"), "# 方案\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "加方案"]);
        let hash = g(&r, &["rev-parse", "--short", "HEAD"]);
        g(&r, &["checkout", "-q", "main"]);
        let gone = dir.join("gone-worktree");
        let n = local_node(&dir);
        let ws = workspace_of(&n, "repo").await;
        let node: Arc<dyn na::Node> = n.clone();
        let v = view(
            vec![Addr {
                node: node.clone(),
                at: Place::Ws(ws.clone(), String::new()),
                shown: home_tilde(&r),
            }],
            // the Node's own answers, never the Hub's checkout behind them
            Git::Node(node.clone(), ws.clone(), false),
        );

        let mut b = out(
            "分支",
            "steve/st-1（已 rebase 到 0.2.19）",
            Some(&gone.to_string_lossy()),
            &gone,
            Some(&r),
        );
        let (disk, through) = both(&b, &v, None).await;
        assert_eq!(disk["type"], "branch", "{disk}");
        assert_eq!(disk["commits"][0]["hash"].as_str().unwrap().len(), 10);
        assert_eq!(through, disk);
        for title in [
            format!("{hash} 加方案"),
            "1234567 不存在".into(),
            "无".into(),
        ] {
            let o = out("提交", &title, None, &gone, Some(&r));
            let (disk, through) = both(&o, &v, None).await;
            assert_eq!(through, disk, "{title}");
        }
        // the worktree is gone and main does not have it: the branch still does
        b.kind = "文档".into();
        b.loc = Some("docs/方案.md".into());
        b.branch = Some("steve/st-1".into());
        let (disk, through) = both(&b, &v, None).await;
        assert_eq!(disk["from"], "git steve/st-1", "{disk}");
        assert_eq!(through, disk);
        b.kind = "分支".into();
        b.title = "steve/st-9".into();
        b.branch = None;
        let (disk, through) = both(&b, &v, None).await;
        assert_eq!(disk["type"], "missing");
        assert_eq!(through, disk);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_workspace_that_is_no_checkout_leaves_git_to_the_hubs_checkout() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().canonicalize().unwrap();
        let r = repo(&dir);
        g(&r, &["checkout", "-q", "-b", "steve/st-1"]);
        std::fs::write(r.join("方案.md"), "# 方案\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "加方案"]);
        let hash = g(&r, &["rev-parse", "--short", "HEAD"]);
        g(&r, &["checkout", "-q", "main"]);
        // the session ran in a plain directory, not under the project's checkout
        let plain = dir.join("scratch");
        std::fs::create_dir_all(&plain).unwrap();
        let n = local_node(&dir);
        let ws = workspace_of(&n, "scratch").await;
        let node: Arc<dyn na::Node> = n.clone();
        let top = Addr {
            node: node.clone(),
            at: Place::Ws(ws.clone(), String::new()),
            shown: home_tilde(&plain),
        };
        let on_hub = view(vec![top.clone()], Git::Node(node.clone(), ws.clone(), true));
        let mut doc = out("文档", "x", Some("方案.md"), &plain, Some(&r));
        doc.branch = Some("steve/st-1".into());
        for (o, shown) in [
            (out("分支", "steve/st-1", None, &plain, Some(&r)), "branch"),
            (
                out("提交", &format!("{hash} 加方案"), None, &plain, Some(&r)),
                "commit",
            ),
            (
                out("提交", "1234567 不存在", None, &plain, Some(&r)),
                "missing",
            ),
            (doc, "text"),
        ] {
            let (disk, through) = both(&o, &on_hub, None).await;
            assert_eq!(disk["type"], shown, "{disk}");
            assert_eq!(through, disk, "{}", o.title);
        }
        // on another device there is no Hub checkout to fall back on: the workspace's answer
        let far = view(vec![top], Git::Node(node.clone(), ws, false));
        let o = out("分支", "steve/st-1", None, &plain, Some(&r));
        let shown = describe(o, far, None).await.unwrap();
        assert_eq!(
            shown,
            json!({"type": "missing", "reason": steve_i18n::t!("hub.output.no_repo")})
        );
    }

    #[test]
    fn git_not_finishing_is_said_not_shown_as_a_rewritten_commit() {
        let failed = |code| git_miss(na::ApiError::new(code, "x"), "gone".into());
        for code in [na::ErrorCode::Internal, na::ErrorCode::Expired] {
            assert_eq!(
                failed(code).unwrap(),
                Ok(missing(steve_i18n::t!("hub.output.git_failed")))
            );
        }
        assert_eq!(failed(na::ErrorCode::Invalid).unwrap(), Err("gone".into()));
        assert_eq!(failed(na::ErrorCode::Offline).unwrap_err(), offline());
        assert_eq!(
            short(&na::CommitInfo {
                sha: "65c3f544aa".into(),
                abbrev: None,
                subject: String::new(),
                author: String::new(),
                time: na::Timestamp(0),
                body: None,
            }),
            "65c3f54"
        );
    }

    #[tokio::test]
    async fn on_another_device_files_come_from_its_node_and_git_is_not_shown() {
        let n = FakeNode::new("n1");
        n.set_roots(vec![na::RootInfo {
            id: na::RootId::new("work").unwrap(),
            display: "~/work".into(),
            access: na::RootAccess::ReadWrite,
        }]);
        n.put_file("work", "far/docs/a.md", b"# A");
        n.put_file("work", "far/docs/b.png", b"png");
        let node: Arc<dyn na::Node> = n.clone();
        let v = View {
            bases: vec![Addr {
                node,
                at: Place::Root(na::RootPath {
                    root: na::RootId::new("work").unwrap(),
                    rel: na::RelPath::new("far").unwrap(),
                }),
                shown: "work/far".into(),
            }],
            local: None,
            git: Git::Elsewhere,
        };
        let o = |kind: &str, loc: &str| Out {
            kind: kind.into(),
            title: "steve/st-1".into(),
            loc: Some(loc.into()),
            cwd: None,
            project: Some(PathBuf::from("work/far")),
            branch: None,
            roots: vec![],
        };
        let d = |o: Out, sub: Option<&str>| {
            let v = View {
                bases: v.bases.clone(),
                local: None,
                git: Git::Elsewhere,
            };
            describe(o, v, sub.map(str::to_string))
        };
        let text = d(o("文档", "docs/a.md"), None).await.unwrap();
        assert_eq!(
            (&text["type"], &text["text"], &text["path"]),
            (&json!("text"), &json!("# A"), &json!("work/far/docs/a.md"))
        );
        let dir = d(o("文件", "docs"), None).await.unwrap();
        let names: Vec<&str> = dir["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["a.md", "b.png"]);
        assert_eq!(
            d(o("文件", "docs"), Some("b.png")).await.unwrap()["type"],
            "image"
        );
        // an absolute path means nothing on another device
        let abs = d(o("文件", "/etc/hosts"), None).await.unwrap();
        assert_eq!(abs["reason"], steve_i18n::t!("hub.output.outside_roots"));
        // a root the Node does not allow is refused, not read
        let mut far = v.bases[0].clone();
        far.at = Place::Root(na::RootPath {
            root: na::RootId::new("elsewhere").unwrap(),
            rel: na::RelPath::new("").unwrap(),
        });
        assert!(matches!(far.stat().await.unwrap(), Seen::Outside));
        let cleaned = d(o("文档", "docs/gone.md"), None).await.unwrap();
        assert_eq!(cleaned["type"], "missing");
        for kind in ["分支", "提交"] {
            let v = d(o(kind, "x"), None).await.unwrap();
            assert_eq!(
                v,
                json!({"type": "missing", "reason": "这个事项的 git 记录在另一台设备上，暂时不能在这里查看"})
            );
        }
    }

    #[test]
    fn an_offline_node_fails_the_request_with_device_offline() {
        let e = node_error(na::ApiError::new(na::ErrorCode::Offline, "link down"));
        assert_eq!(e, ApiError::Json(503, "设备离线".into()));
        assert_eq!(
            node_error(na::ApiError::new(na::ErrorCode::Internal, "boom")),
            ApiError::internal("boom")
        );
    }

    #[test]
    fn a_location_stays_below_its_place() {
        assert_eq!(
            joined("app", "docs/./a.md"),
            Some(("app/docs/a.md".into(), "docs/a.md".into()))
        );
        assert_eq!(joined("", "a/../b"), Some(("b".into(), "b".into())));
        assert_eq!(joined("app", ""), Some(("app".into(), String::new())));
        assert_eq!(joined("app", "../x"), None);
        assert_eq!(joined("app", "/etc"), None);
    }
}
