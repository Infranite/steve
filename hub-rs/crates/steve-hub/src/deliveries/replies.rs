//! Before committing a completed assistant block, the Node asks the Hub for immutable
//! replacements. The request is not a tool or a browser path resolver. Reads go back to the
//! exact Node/workspace that accepted the turn, and only Hub-owned objects are served later.
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Component, Path};
use std::time::Duration;

use markdown::mdast::Node as Md;
use steve_node_api::*;
use steve_store::{Session, StoreError};

use super::{
    Object, Publisher, Service, bundle, mime,
    receive::{CHUNK, Receive},
};
use crate::wire::node_tools::{authorize, internal};

const MAX_READS: usize = 16;
const MAX_REFS: usize = 128;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_MARKDOWN: usize = 512 * 1024;
const HINT: &str = "#steve-file-unavailable-";

#[derive(Clone)]
struct Origin {
    session: Session,
    workspace: WorkspaceRef,
    binding: String,
    execution: ExecutionRef,
}

#[derive(Clone, Copy)]
enum Unavailable {
    Scope,
    Missing,
    Offline,
    Limit,
    Type,
}
impl Unavailable {
    fn marker(self) -> String {
        let reason = match self {
            Self::Scope => "scope",
            Self::Missing => "missing",
            Self::Offline => "offline",
            Self::Limit => "limit",
            Self::Type => "type",
        };
        format!("{HINT}{reason}")
    }
    fn from_node(e: ApiError) -> Self {
        match e.code {
            ErrorCode::Denied => Self::Scope,
            ErrorCode::Offline | ErrorCode::Expired | ErrorCode::Stale | ErrorCode::Unsupported => {
                Self::Offline
            }
            ErrorCode::Exhausted => Self::Limit,
            _ => Self::Missing,
        }
    }
}

struct Reference {
    /// Starts after the label for links, so even a linked image can be rewritten independently.
    replace: Range<usize>,
    destination: String,
    image: bool,
    label: Option<String>,
    title: Option<String>,
}

impl Reference {
    fn render(&self, target: &str) -> String {
        let title = self.title.as_ref().map_or_else(String::new, |t| {
            let t = t
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('&', "\\&");
            format!(" \"{t}\"")
        });
        self.label.as_ref().map_or_else(
            || format!("]({target}{title})"),
            |alt| {
                let label: String = alt
                    .chars()
                    .flat_map(|c| {
                        let c = if c.is_control() { ' ' } else { c };
                        c.is_ascii_punctuation()
                            .then_some('\\')
                            .into_iter()
                            .chain([c])
                    })
                    .collect();
                format!("![{label}]({target}{title})")
            },
        )
    }
}

fn references(text: &str) -> Vec<Reference> {
    // Match the production renderer's GFM (not CommonMark alone: e.g. a bare https URL
    // containing ![...](...) must not turn into a read). Code and raw HTML are not resources.
    let Ok(tree) = markdown::to_mdast(text, &markdown::ParseOptions::gfm()) else {
        return vec![];
    };
    let mut defs = HashMap::new();
    let mut candidates = vec![];
    let mut pending = vec![&tree];
    while let Some(n) = pending.pop() {
        match n {
            Md::Definition(d) => {
                defs.entry(&d.identifier).or_insert(d);
            }
            Md::Link(_) | Md::LinkReference(_) | Md::Image(_) | Md::ImageReference(_) => {
                candidates.push(n)
            }
            _ => {}
        }
        if let Some(children) = n.children() {
            pending.extend(children.iter().rev());
        }
    }
    let mut refs = vec![];
    for n in candidates {
        let Some(p) = n.position() else { continue };
        let start = p.start.offset;
        let end = p.end.offset;
        let Some(raw) = text.get(start..end) else {
            continue;
        };
        let (dest, title, label) = match n {
            Md::Link(l) if raw.starts_with('[') => (&l.url, &l.title, None),
            Md::LinkReference(l) if raw.starts_with('[') => {
                let Some(d) = defs.get(&l.identifier) else {
                    continue;
                };
                (&d.url, &d.title, None)
            }
            Md::Image(i) => (&i.url, &i.title, Some(i.alt.clone())),
            Md::ImageReference(i) => {
                let Some(d) = defs.get(&i.identifier) else {
                    continue;
                };
                (&d.url, &d.title, Some(i.alt.clone()))
            }
            _ => continue,
        };
        let from = if label.is_some() {
            start
        } else {
            // The AST's last child ends before the closing bracket; that includes formatted
            // label text, escaped brackets, inline code and nested image syntax.
            let last = n
                .children()
                .and_then(|c| c.last())
                .and_then(Md::position)
                .map_or(start + 1, |pos| pos.end.offset);
            let Some(tail) = text.get(last..end) else {
                continue;
            };
            let Some(close) = tail.find(']') else {
                continue;
            };
            last + close
        };
        refs.push(Reference {
            replace: from..end,
            destination: dest.clone(),
            image: label.is_some(),
            label,
            title: title.clone(),
        });
    }
    refs.sort_by_key(|r| r.replace.start);
    refs
}

fn site_route(s: &str) -> bool {
    ["/api/", "/deliveries/", "/shares/", "/auth/", "/assets/"]
        .iter()
        .any(|p| s.starts_with(p))
}
fn extension(name: &str) -> Option<&str> {
    Path::new(name).extension()?.to_str()
}
fn supported(name: &str) -> bool {
    extension(name).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "md" | "markdown"
                | "pdf"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "txt"
                | "log"
                | "csv"
                | "tsv"
                | "json"
                | "jsonl"
                | "yaml"
                | "yml"
                | "xml"
                | "toml"
                | "docx"
                | "xlsx"
                | "pptx"
                | "mp3"
                | "mp4"
                | "webm"
                | "wav"
        )
    })
}

fn candidate(dest: &str, image: bool) -> bool {
    if dest
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("file:"))
    {
        return true;
    }
    if dest.is_empty()
        || dest.starts_with("//")
        || dest.starts_with('#')
        || site_route(dest)
        || dest.contains(':')
    {
        return false;
    }
    // Avoid interpreting an ordinary in-app relative link as a source file. Explicit absolute
    // and ./ paths (and any local image) are unambiguous; bare names need a recognized suffix.
    image
        || dest.starts_with('/')
        || dest.starts_with("./")
        || dest.starts_with("../")
        || supported(dest.split(['?', '#']).next().unwrap_or_default())
}

fn location(dest: &str) -> Result<String, Unavailable> {
    let raw = if dest
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
    {
        let s = &dest[7..];
        if !s.starts_with('/') {
            return Err(Unavailable::Scope);
        }
        s
    } else {
        dest
    };
    let mut bytes = Vec::with_capacity(raw.len());
    let mut it = raw.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [
                it.next().ok_or(Unavailable::Scope)?,
                it.next().ok_or(Unavailable::Scope)?,
            ];
            let pair = std::str::from_utf8(&hex).map_err(|_| Unavailable::Scope)?;
            bytes.push(u8::from_str_radix(pair, 16).map_err(|_| Unavailable::Scope)?);
        } else {
            bytes.push(b);
        }
    }
    let s = String::from_utf8(bytes).map_err(|_| Unavailable::Scope)?;
    if s.is_empty()
        || s.len() > 4096
        || s.starts_with("//")
        || s.contains(['?', '#', '\\', ':'])
        || s.chars().any(char::is_control)
        || Path::new(&s)
            .components()
            .any(|c| c == Component::ParentDir)
    {
        return Err(Unavailable::Scope);
    }
    Ok(s.strip_prefix("./").unwrap_or(&s).to_string())
}

struct Capture<'a> {
    service: &'a Service,
    source: &'a dyn Node,
    origin: Origin,
    /// Shared across direct references and report dependencies.
    reads: usize,
    bytes: usize,
    deadline: tokio::time::Instant,
}

impl Service {
    pub(crate) async fn capture_reply(
        &self,
        caller: &NodeId,
        source: &dyn Node,
        q: ReplyCapture,
    ) -> ApiResult<String> {
        if q.markdown.len() > MAX_REPLY_CAPTURE || source.id() != caller {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "invalid reply capture scope",
            ));
        }
        let (session, _) = authorize(&self.store, caller, &q.execution, true)?;
        let binding = self
            .store
            .reader(|c| steve_store::runtime::workspace_ref_of(c, &session.id))
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(ErrorCode::Denied, "reply has no workspace binding"))?;
        let workspace: WorkspaceRef = serde_json::from_str(&binding).map_err(internal)?;
        if workspace.node != *caller {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "reply Node binding changed",
            ));
        }
        let mut c = Capture {
            service: self,
            source,
            origin: Origin {
                session,
                workspace,
                binding,
                execution: q.execution,
            },
            reads: 0,
            bytes: 0,
            deadline: tokio::time::Instant::now() + Duration::from_secs(6),
        };
        Ok(c.rewrite(q.markdown).await)
    }
}

impl Capture<'_> {
    async fn read(&mut self, path: &str) -> Result<Vec<u8>, Unavailable> {
        let remaining = MAX_BYTES.saturating_sub(self.bytes);
        if self.reads >= MAX_READS || remaining == 0 {
            return Err(Unavailable::Limit);
        }
        self.reads += 1;
        let mut q = ExecutionDeliveryRead {
            execution: self.origin.execution.clone(),
            workspace: self.origin.workspace.clone(),
            location: path.into(),
            reply: true,
            expected: None,
            offset: 0,
            max: CHUNK,
        };
        let mut recv = Receive::default();
        // Slow or absent files cannot stall the answer or consume the entire turn's deadline.
        let deadline = self
            .deadline
            .min(tokio::time::Instant::now() + Duration::from_secs(2));
        tokio::time::timeout_at(deadline, async {
            loop {
                let r = self
                    .source
                    .read_execution_delivery(q.clone())
                    .await
                    .map_err(Unavailable::from_node)?;
                let size = usize::try_from(r.size).map_err(|_| Unavailable::Limit)?;
                if size > remaining {
                    return Err(Unavailable::Limit);
                }
                q.expected = Some(r.version.clone());
                let done = recv.push(r).map_err(|_| Unavailable::Missing)?;
                if done {
                    return Ok(());
                }
                q.offset = recv.bytes.len() as u64;
            }
        })
        .await
        .map_err(|_| Unavailable::Offline)??;
        self.bytes += recv.bytes.len();
        Ok(recv.bytes)
    }

    async fn rewrite(&mut self, mut markdown: String) -> String {
        let mut captures = HashMap::new();
        let mut changes = vec![];
        for r in references(&markdown)
            .into_iter()
            .filter(|r| candidate(&r.destination, r.image))
            .take(MAX_REFS)
        {
            let target = match location(&r.destination) {
                Err(e) => e.marker(),
                Ok(path) => {
                    let key = (path, r.image);
                    if !captures.contains_key(&key) {
                        let target = self
                            .snapshot(&key.0, key.1)
                            .await
                            .unwrap_or_else(Unavailable::marker);
                        captures.insert(key.clone(), target);
                    }
                    captures[&key].clone()
                }
            };
            changes.push((r.replace.clone(), r.render(&target)));
        }
        replace(&mut markdown, changes);
        markdown
    }

    async fn snapshot(&mut self, path: &str, image: bool) -> Result<String, Unavailable> {
        let name = name(path)?;
        if !supported(name) || (image && !crate::files::is_inline_image(mime(name))) {
            return Err(Unavailable::Type);
        }
        let bytes = self.read(path).await?;
        let mut assets = BTreeMap::new();
        let mut preview = None;
        if mime(name) == "text/markdown" {
            let mut text = std::str::from_utf8(&bytes)
                .map_err(|_| Unavailable::Type)?
                .to_owned();
            if text.len() > MAX_MARKDOWN {
                return Err(Unavailable::Limit);
            }
            let mut cache = HashMap::new();
            let mut changes = vec![];
            for r in references(&text)
                .into_iter()
                .filter(|r| candidate(&r.destination, r.image))
                .take(MAX_REFS)
            {
                let target = match location(&r.destination) {
                    Err(e) => e.marker(),
                    Ok(location) => {
                        let path = if Path::new(&location).is_absolute() {
                            location
                        } else {
                            Path::new(path)
                                .with_file_name(location)
                                .to_string_lossy()
                                .into_owned()
                        };
                        if !cache.contains_key(&path) {
                            let target = match self.asset(&path).await {
                                Ok((ext, bytes)) => {
                                    let key = format!(
                                        "assets/{}.{}",
                                        hex::encode(Sha256::digest(&bytes)),
                                        ext
                                    );
                                    assets.insert(key.clone(), bytes);
                                    key
                                }
                                Err(e) => e.marker(),
                            };
                            cache.insert(path.clone(), target);
                        }
                        cache[&path].clone()
                    }
                };
                changes.push((r.replace.clone(), r.render(&target)));
            }
            replace(&mut text, changes);
            preview = Some(text);
        } else if crate::files::is_inline_image(mime(name)) {
            validate_image(&bytes, mime(name)).await?;
        } else if mime(name) == "application/pdf" && !bytes.starts_with(b"%PDF-") {
            return Err(Unavailable::Type);
        }
        let (svc, name, origin) = (self.service.clone(), name.to_string(), self.origin.clone());
        let url = tokio::task::spawn_blocking(move || {
            svc.keep_reply(&origin, &name, &bytes, preview.as_deref(), assets, image)
        })
        .await
        .map_err(|_| Unavailable::Missing)??;
        Ok(url)
    }

    async fn asset(&mut self, path: &str) -> Result<(String, Vec<u8>), Unavailable> {
        let name = name(path)?;
        let mime = mime(name);
        if !crate::files::is_inline_image(mime) {
            return Err(Unavailable::Type);
        }
        let bytes = self.read(path).await?;
        validate_image(&bytes, mime).await?;
        Ok((
            extension(name)
                .ok_or(Unavailable::Type)?
                .to_ascii_lowercase(),
            bytes,
        ))
    }
}

async fn validate_image(bytes: &[u8], mime: &'static str) -> Result<(), Unavailable> {
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        crate::wire::node_files::uploads::validate_image(&bytes, mime)
    })
    .await
    .map_err(|_| Unavailable::Type)?
    .map_err(|_| Unavailable::Type)
}

fn name(path: &str) -> Result<&str, Unavailable> {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| bundle::plain(n) && n.len() <= 255)
        .ok_or(Unavailable::Type)
}

fn replace(text: &mut String, mut changes: Vec<(Range<usize>, String)>) {
    changes.sort_by_key(|(r, _)| r.start);
    let mut end = text.len();
    for (range, change) in changes.into_iter().rev() {
        if range.end <= end {
            end = range.start;
            text.replace_range(range, &change);
        }
    }
}

use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};

impl Service {
    fn keep_reply(
        &self,
        origin: &Origin,
        name: &str,
        bytes: &[u8],
        rewritten: Option<&str>,
        assets: BTreeMap<String, Vec<u8>>,
        image: bool,
    ) -> Result<String, Unavailable> {
        let ExecutionRef::Turn { attempt, turn, .. } = &origin.execution else {
            return Err(Unavailable::Scope);
        };
        let session = &origin.session;
        let publisher = Publisher::Session(session);
        let mut manifest = BTreeMap::new();
        for (path, bytes) in assets {
            let sha256 = self.put(&bytes).map_err(|_| Unavailable::Missing)?;
            manifest.insert(
                path.clone(),
                Object {
                    sha256,
                    size: bytes.len() as u64,
                    mime: mime(&path).into(),
                },
            );
        }
        let sha = self.put(bytes).map_err(|_| Unavailable::Missing)?;
        let format = crate::api::outputs::media(name).0;
        let preview = match format {
            Some(form) => {
                json!({"type":form,"name":name,"size":bytes.len(),"mime":mime(name),"large":crate::api::outputs::large(form,bytes.len() as u64)})
            }
            None => crate::api::outputs::text_value(name, rewritten.map_or(bytes, str::as_bytes)),
        };
        let manifest = serde_json::to_string(&manifest).map_err(|_| Unavailable::Missing)?;
        let preview = serde_json::to_string(&preview).map_err(|_| Unavailable::Missing)?;
        // Retry safely, but never reuse a version across sessions/attempts (even when bytes match).
        let id = format!(
            "dlv_{}",
            &hex::encode(Sha256::digest(
                serde_json::to_vec(&(
                    session.id.as_str(),
                    attempt.as_str(),
                    name,
                    &sha,
                    &manifest,
                    &preview
                ))
                .map_err(|_| Unavailable::Missing)?
            ))[..32]
        );
        let url = self.url(&id, &session.workspace_id);
        self.store.transaction(|tx| {
            if !Self::in_scope(tx, publisher)? || !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_sessions r JOIN runtime_turns t ON t.session_id=r.session_id
                 JOIN runtime_attempts a ON a.turn_id=t.turn_id AND a.attempt_id=t.active_attempt
                 JOIN sessions s ON s.id=r.session_id AND s.status!='ended' AND s.readonly=0
                 WHERE r.session_id=?1 AND r.workspace_ref=?2 AND t.turn_id=?3 AND a.attempt_id=?4
                   AND r.active_attempt=a.attempt_id AND r.node_id=a.node_id
                   AND a.cancel_requested=0 AND a.state NOT IN ('finished','rejected','unknown'))",
                params![session.id,origin.binding,turn.as_str(),attempt.as_str()], |r| r.get::<_,bool>(0),
            )? {
                return Err(StoreError::Conflict("reply execution changed".into()));
            }
            tx.execute(
                "INSERT INTO deliveries(id,url,workspace_id,item_id,session_id,title,name,mime,size,created,sha256,manifest,preview,reply)
                 VALUES(?1,?2,?3,?4,?5,?6,?6,?7,?8,?9,?10,?11,?12,1) ON CONFLICT(id) DO NOTHING",
                params![id,url,session.workspace_id,session.item_id,session.id,name,mime(name),bytes.len() as i64,steve_store::now(),sha,manifest,preview],
            )?;
            Ok(())
        }).map_err(|e| match e {
            StoreError::Conflict(_) => Unavailable::Scope,
            _ => Unavailable::Missing,
        })?;
        if image {
            let mut url = Service::resource(
                &format!("{}/api/file-deliveries/{id}", self.base),
                "content",
                name,
            );
            url.query_pairs_mut()
                .append_pair("ws", &session.workspace_id);
            Ok(format!(
                "{}?{}",
                url.path(),
                url.query().unwrap_or_default()
            ))
        } else {
            // Installations can change addresses. Keep a relative route in the transcript.
            let url = reqwest::Url::parse(&url).map_err(|_| Unavailable::Missing)?;
            Ok(format!(
                "{}?{}",
                url.path(),
                url.query().unwrap_or_default()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_targets_only_visible_gfm_nodes_and_preserves_nested_label() {
        let mut markdown = r#"External https://host.test/![not an image](outside.png)
www.example.test/![neither](outside.png)

`[example](hidden.md)`

[**report** ![inside](image.png)][r]

![by ref][i]

[r]: <file:///tmp/report%20page.md> "title"
[i]: photo.png "caption"
"#
        .to_string();
        let changes: Vec<_> = references(&markdown)
            .into_iter()
            .filter(|r| candidate(&r.destination, r.image))
            .map(|r| (r.replace.clone(), r.render("/captured")))
            .collect();
        assert_eq!(changes.len(), 3);
        replace(&mut markdown, changes);
        assert!(
            markdown.contains(r#"[**report** ![inside](/captured)](/captured "title")"#),
            "{markdown}"
        );
        assert!(markdown.contains(r#"![by ref](/captured "caption")"#));
        assert!(markdown.contains("`[example](hidden.md)`"));
        assert!(markdown.contains("https://host.test/![not an image](outside.png)"));
    }

    #[test]
    fn no_scheme_host_traversal_or_ambiguous_filename() {
        for url in [
            "file://host/unsafe.md",
            "/view/../escape.md",
            "%2e%2e/report.md",
            "/root%00/x.md",
            "a.md?read=yes",
            "%2F%2Fhost/secret.md",
            "bad%ZZ.md",
        ] {
            assert!(location(url).is_err(), "{url}");
        }
        assert_eq!(
            location("file:///tmp/an%20item.md").ok().as_deref(),
            Some("/tmp/an item.md")
        );
        assert_eq!(location("./report.md").ok().as_deref(), Some("report.md"));
        for url in [
            "https://example.test/file.md",
            "/api/files/x",
            "/deliveries/dlv_123?ws=x",
            "#/items/ST-1",
            "//example.test/test",
        ] {
            assert!(!candidate(url, false), "{url}");
            assert!(!candidate(url, true), "{url}");
        }
    }
}
