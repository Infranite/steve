//! Capture only completed assistant Markdown, while the execution still owns its source files.
//! Source reads stay on the Node. A failed capture leaves the original text; only a verified Hub
//! receipt becomes a browser URL. No HTML, code sample or ordinary link grants a file read.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use steve_node_api::*;

#[derive(Clone)]
pub(crate) struct Scope {
    pub cwd: PathBuf,
    roots: Vec<crate::artifact::ImageRoot>,
    pub protected: Vec<PathBuf>,
}
impl Scope {
    pub fn new(cwd: PathBuf, roots: Vec<PathBuf>, protected: Vec<PathBuf>) -> Self {
        Self {
            cwd,
            roots: roots
                .iter()
                .filter_map(|p| crate::artifact::ImageRoot::capture(p).ok())
                .collect(),
            protected,
        }
    }
}

pub(crate) fn protected(hub: &Path, node: &Path) -> Vec<PathBuf> {
    steve_vault::harness::PROTECTED
        .iter()
        .map(|p| hub.join(p.trim_end_matches(['/', '*'])))
        .chain(
            steve_vault::harness::NODE_PROTECTED
                .iter()
                .map(|p| node.join(p.trim_end_matches(['/', '*']))),
        )
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}
struct Block {
    message: String,
    block: String,
    text: String,
    completed: bool,
    emitted: usize,
}
#[derive(Default)]
pub(crate) struct Images {
    blocks: Vec<Block>,
    /// Successful captures are immutable receipts, shared by repeated references in this attempt.
    urls: HashMap<String, String>,
    tried: usize,
    capture_spent: Duration,
}
impl Images {
    fn stream(&mut self, body: EventBody) -> Option<EventBody> {
        match &body {
            EventBody::MessageStarted {
                message,
                block,
                role: Role::Assistant,
            } => {
                self.blocks.push(Block {
                    message: message.clone(),
                    block: block.clone(),
                    text: String::new(),
                    completed: false,
                    emitted: 0,
                });
            }
            EventBody::TextDelta {
                message,
                block,
                utf8_offset,
                text,
            } => {
                if let Some(b) = self
                    .blocks
                    .iter_mut()
                    .find(|b| b.message == *message && b.block == *block)
                {
                    let at = *utf8_offset as usize;
                    if at <= b.text.len() && b.text.is_char_boundary(at) {
                        b.text.truncate(at);
                        b.text.push_str(text);
                    }
                    // A literal ! may become ![ in the next chunk. Everything before a candidate
                    // keeps streaming. Once one appears, the suffix waits for complete Markdown.
                    let end = b
                        .text
                        .find("![")
                        .unwrap_or_else(|| b.text.len() - usize::from(b.text.ends_with('!')));
                    if end <= b.emitted {
                        return None;
                    }
                    let delta = EventBody::TextDelta {
                        message: message.clone(),
                        block: block.clone(),
                        utf8_offset: b.emitted as u64,
                        text: b.text[b.emitted..end].to_string(),
                    };
                    b.emitted = end;
                    return Some(delta);
                }
            }
            EventBody::MessageCompleted { message } => {
                for b in &mut self.blocks {
                    if b.message == *message {
                        b.completed = true;
                    }
                }
            }
            _ => {}
        }
        Some(body)
    }

    pub async fn process(
        &mut self,
        scope: &Scope,
        execution: &ExecutionRef,
        files: Option<Arc<dyn HubFiles>>,
        bodies: Vec<EventBody>,
    ) -> Vec<EventBody> {
        let mut out = vec![];
        for body in bodies {
            let completed = matches!(body, EventBody::MessageCompleted { .. });
            if let Some(body) = self.stream(body) {
                if completed {
                    out.extend(self.capture(scope, execution, files.clone(), false).await);
                }
                out.push(body);
            }
        }
        out
    }

    /// The withheld suffix goes before MessageCompleted/AttemptEnded, once. No text already sent
    /// is replaced: older transcript consumers append text rather than applying block offsets.
    pub async fn capture(
        &mut self,
        scope: &Scope,
        execution: &ExecutionRef,
        files: Option<Arc<dyn HubFiles>>,
        all: bool,
    ) -> Vec<EventBody> {
        let mut out = vec![];
        for b in &mut self.blocks {
            if !all && !b.completed {
                continue;
            }
            let mut replacements = vec![];
            let mut captured: HashMap<String, String> = HashMap::new();
            for node in nodes(&b.text) {
                let Some(path) = local_path(&node.destination) else {
                    continue;
                };
                if !captured.contains_key(&path) {
                    self.urls.remove(&path);
                }
                let url = if let Some(url) = captured.get(&path) {
                    Some(url.clone())
                } else if self.tried < MAX_INLINE_IMAGES && files.is_some() {
                    // A later message can overwrite the same pathname (e.g. a fresh QR). Re-read
                    // it; deduplicate only within this complete block and by content on the Hub.
                    self.tried += 1;
                    let scope = scope.clone();
                    let exec = execution.clone();
                    let hub = files.clone().expect("checked");
                    let source = path.clone();
                    let started = std::time::Instant::now();
                    let remaining = Duration::from_secs(5).saturating_sub(self.capture_spent);
                    let result = tokio::time::timeout(remaining, async move {
                        let bytes = tokio::task::spawn_blocking(move || read(&scope, &source))
                            .await
                            .map_err(|_| invalid())??;
                        upload(&*hub, &exec, &bytes).await
                    })
                    .await;
                    self.capture_spent += started.elapsed();
                    match result {
                        Ok(Ok(url)) => {
                            self.urls.insert(path.clone(), url.clone());
                            captured.insert(path, url.clone());
                            Some(url)
                        }
                        _ => {
                            // Never log the local path (it may name a secret) or fail the text turn.
                            tracing::debug!("inline image unavailable; original Markdown retained");
                            None
                        }
                    }
                } else {
                    None
                };
                if let Some(url) = url {
                    let replacement = node.markdown(&url);
                    replacements.push((node.range, replacement));
                }
            }
            if b.emitted < b.text.len() {
                let mut text = b.text.clone();
                for (range, replacement) in replacements.into_iter().rev() {
                    text.replace_range(range, &replacement);
                }
                out.push(EventBody::TextDelta {
                    message: b.message.clone(),
                    block: b.block.clone(),
                    utf8_offset: b.emitted as u64,
                    text: text[b.emitted..].to_string(),
                });
            }
        }
        // These blocks have reached a stable Markdown boundary. Keep incomplete ones for the end.
        self.blocks.retain(|b| !all && !b.completed);
        out
    }

    /// The final research report uses the same successful receipts as its streamed blocks.
    pub fn rewrite(&self, text: &str) -> String {
        let mut out = text.to_string();
        for node in nodes(text).into_iter().rev() {
            if let Some(url) = local_path(&node.destination).and_then(|p| self.urls.get(&p)) {
                let replacement = node.markdown(url);
                out.replace_range(node.range, &replacement);
            }
        }
        out
    }

    /// Cancellation can end a normal turn without an async capture. Preserve its text as-is.
    pub fn drain_original(&mut self) -> Vec<EventBody> {
        self.blocks
            .drain(..)
            .filter_map(|b| {
                (b.emitted < b.text.len()).then(|| EventBody::TextDelta {
                    message: b.message,
                    block: b.block,
                    utf8_offset: b.emitted as u64,
                    text: b.text[b.emitted..].to_string(),
                })
            })
            .collect()
    }
}

struct ImageNode {
    range: std::ops::Range<usize>,
    destination: String,
    alt: String,
    title: String,
}
impl ImageNode {
    fn markdown(&self, url: &str) -> String {
        let title = if self.title.is_empty() {
            String::new()
        } else {
            format!(
                " \"{}\"",
                self.title.replace('\\', "\\\\").replace('"', "\\\"")
            )
        };
        format!("![{}]({url}{title})", self.alt)
    }
}
fn nodes(markdown: &str) -> Vec<ImageNode> {
    let mut nodes = vec![];
    let mut current: Option<ImageNode> = None;
    let mut nested = 0usize;
    for (event, range) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                // An image inside another image's alt contributes text, not another resource.
                if current.is_some() {
                    nested += 1;
                    continue;
                }
                current = Some(ImageNode {
                    range,
                    destination: dest_url.into_string(),
                    alt: String::new(),
                    title: title.into_string(),
                });
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(node) = current.as_mut() {
                    // Preserve the rendered alt, including literal brackets/backslashes.
                    for c in t.chars() {
                        if matches!(c, '\\' | '[' | ']' | '`' | '*' | '_') {
                            node.alt.push('\\');
                        }
                        node.alt.push(c);
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(node) = current.as_mut() {
                    node.alt.push('\n');
                }
            }
            Event::End(TagEnd::Image) => {
                if nested > 0 {
                    nested -= 1;
                    continue;
                }
                if let Some(node) = current.take() {
                    nodes.push(node);
                }
            }
            _ => {}
        }
    }
    nodes
}
fn local_path(destination: &str) -> Option<String> {
    // No URLs (including file://, protocol-relative, Hub URLs or data:), queries or fragments.
    if destination.is_empty()
        || destination.starts_with("//")
        || destination.starts_with("/api/")
        || destination.contains([':', '?', '#', '\\', '\0'])
    {
        return None;
    }
    let mut bytes = vec![];
    let mut it = destination.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let pair = [it.next()?, it.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&pair).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    if decoded.contains([':', '?', '#', '\\', '\0']) || decoded.chars().any(char::is_control) {
        return None;
    }
    Some(decoded)
}
fn invalid() -> ApiError {
    ApiError::new(ErrorCode::Denied, "inline image unavailable")
}

fn sensitive(rel: &Path) -> bool {
    rel.components().any(|c| {
        let Component::Normal(c) = c else {
            return false;
        };
        let c = c.to_string_lossy().to_ascii_lowercase();
        c.starts_with('.')
            || c.contains("credential")
            || c.contains("secret")
            || c == "token"
            || c.starts_with("id_rsa")
            || c.starts_with("id_ed25519")
            || c.ends_with(".pem")
            || c.ends_with(".key")
    })
}
fn read(scope: &Scope, source: &str) -> ApiResult<Vec<u8>> {
    let input = Path::new(source);
    if input
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(invalid());
    }
    let path = if input.is_absolute() {
        input.to_path_buf()
    } else {
        scope.cwd.join(input)
    };
    // Match the most specific granted root, never canonicalize a caller-selected file or link.
    let root = scope
        .roots
        .iter()
        .filter(|r| path.starts_with(&r.path) || path.starts_with(&r.recorded))
        .max_by_key(|r| r.path.components().count())
        .ok_or_else(invalid)?;
    let rel = path
        .strip_prefix(&root.path)
        .or_else(|_| path.strip_prefix(&root.recorded))
        .map_err(|_| invalid())?;
    // Translate only an alias of an already captured installation root. Never resolve a file
    // supplied by the assistant, which could be a link into another workspace or a secret.
    let path = root.path.join(rel);
    if sensitive(rel)
        || path.components().any(|c| matches!(c, Component::Normal(c) if matches!(c.to_string_lossy().to_ascii_lowercase().as_str(), ".ssh" | ".aws" | ".gnupg" | ".codex" | ".claude" | ".config" | ".kube" | ".git" | "keychains")))
        || scope
            .protected
            .iter()
            .any(|p| path.starts_with(p) || path.to_string_lossy().starts_with(p.to_string_lossy().as_ref()))
    {
        return Err(invalid());
    }
    let rel = RelPath::new(rel.to_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
    let mut bytes = vec![];
    let mut version = None;
    let mut size = None;
    loop {
        let piece = root.read(&rel, version.as_ref(), bytes.len() as u64)?;
        if piece.size == 0
            || piece.size > MAX_INLINE_IMAGE
            || size.is_some_and(|s| s != piece.size)
            || piece.chunk.offset != bytes.len() as u64
            || piece.chunk.bytes.is_empty()
        {
            return Err(invalid());
        }
        size = Some(piece.size);
        version = Some(piece.version);
        bytes.extend(piece.chunk.bytes);
        if bytes.len() as u64 > piece.size {
            return Err(invalid());
        }
        if piece.chunk.eof {
            if bytes.len() as u64 != piece.size {
                return Err(invalid());
            }
            break;
        }
    }
    Ok(bytes)
}

async fn upload(files: &dyn HubFiles, execution: &ExecutionRef, bytes: &[u8]) -> ApiResult<String> {
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[255, 216, 255]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        return Err(invalid());
    };
    let digest = Digest::sha256(bytes);
    let key = Digest::sha256(
        format!(
            "{}\0{}",
            serde_json::to_string(execution).map_err(|_| invalid())?,
            digest
        )
        .as_bytes(),
    );
    let begin = BeginUpload {
        key: OperationId::new(format!("img_{}", &key.as_str()[7..])).map_err(|_| invalid())?,
        access: FileAccess::Execution(execution.clone()),
        manifest: BlobManifest {
            sha256: digest.clone(),
            size: bytes.len() as u64,
            mime: mime.into(),
            name: format!("inline-{}", &digest.as_str()[7..19]),
        },
    };
    let mut last = invalid();
    // Retry begin too: a lost finish reply, reconnect or Hub restart is safe. Successful content
    // is addressed by workspace+hash on the Hub; it never depends on a surviving source pathname.
    for _ in 0..2 {
        let attempt = async {
            let mut transfer = files.begin_upload(begin.clone()).await?;
            let workspace = transfer
                .workspace
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(invalid)?;
            while transfer.next_offset < bytes.len() as u64 {
                let at = usize::try_from(transfer.next_offset).map_err(|_| invalid())?;
                let chunk = bytes
                    .get(at..(at + INLINE_IMAGE_CHUNK).min(bytes.len()))
                    .ok_or_else(invalid)?;
                let next = files
                    .upload(BlobChunk {
                        transfer: transfer.id.clone(),
                        offset: at as u64,
                        bytes: chunk.to_vec(),
                        digest: Digest::sha256(chunk),
                    })
                    .await?;
                if next.id != transfer.id || next.next_offset != (at + chunk.len()) as u64 {
                    return Err(invalid());
                }
                transfer = next;
            }
            let artifact = files.finish_upload(transfer.id).await?;
            if artifact.sha256 != digest
                || artifact.size != bytes.len() as u64
                || artifact.mime != mime
            {
                return Err(invalid());
            }
            let encoded: String = workspace
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            Ok(format!("/api/files/{}?ws={encoded}", artifact.id))
        }
        .await;
        match attempt {
            Ok(url) => return Ok(url),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Files {
        reads: AtomicUsize,
        bytes: Mutex<Vec<u8>>,
        manifest: Mutex<Option<BlobManifest>>,
        corrupt_receipt: bool,
    }
    #[async_trait::async_trait]
    impl HubFiles for Files {
        async fn fetch(
            &self,
            _: FileAccess,
            _: ArtifactId,
            _: u64,
            _: u32,
        ) -> ApiResult<ReadChunk> {
            Err(invalid())
        }
        async fn begin_upload(&self, b: BeginUpload) -> ApiResult<Transfer> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            *self.manifest.lock().unwrap() = Some(b.manifest);
            Ok(Transfer {
                id: TransferId::new("tr").unwrap(),
                next_offset: self.bytes.lock().unwrap().len() as u64,
                workspace: Some("space one".into()),
            })
        }
        async fn upload(&self, b: BlobChunk) -> ApiResult<Transfer> {
            self.bytes.lock().unwrap().extend(b.bytes);
            Ok(Transfer {
                id: b.transfer,
                next_offset: self.bytes.lock().unwrap().len() as u64,
                workspace: Some("space one".into()),
            })
        }
        async fn finish_upload(&self, _: TransferId) -> ApiResult<ArtifactRef> {
            let m = self.manifest.lock().unwrap().clone().unwrap();
            Ok(ArtifactRef {
                id: ArtifactId::new("fil_kept").unwrap(),
                sha256: if self.corrupt_receipt {
                    Digest::sha256(b"wrong")
                } else {
                    m.sha256
                },
                size: m.size,
                mime: m.mime,
            })
        }
        async fn upload_status(&self, _: TransferId) -> ApiResult<TransferState> {
            Err(invalid())
        }
        async fn abort_upload(&self, _: TransferId) -> ApiResult<TransferState> {
            Ok(TransferState::Aborted)
        }
    }
    fn exec() -> ExecutionRef {
        ExecutionRef::Turn {
            session: SessionId::new("s").unwrap(),
            turn: TurnId::new("t").unwrap(),
            attempt: AttemptId::new("a").unwrap(),
        }
    }
    fn start() -> EventBody {
        EventBody::MessageStarted {
            message: "m".into(),
            block: "b".into(),
            role: Role::Assistant,
        }
    }
    fn delta(offset: u64, text: &str) -> EventBody {
        EventBody::TextDelta {
            message: "m".into(),
            block: "b".into(),
            utf8_offset: offset,
            text: text.into(),
        }
    }
    fn end() -> EventBody {
        EventBody::MessageCompleted {
            message: "m".into(),
        }
    }
    fn text(events: Vec<EventBody>) -> String {
        events
            .into_iter()
            .filter_map(|e| match e {
                EventBody::TextDelta { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }
    #[tokio::test]
    async fn split_images_wait_for_complete_markdown_keep_alt_and_stream_text_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let scope = Scope::new(root.clone(), vec![root.clone()], vec![]);
        let bytes = b"\x89PNG\r\n\x1a\npayload";
        std::fs::write(root.join("qr.png"), bytes).unwrap();
        let files = Arc::new(Files::default());
        let mut images = Images::default();
        let prefix = "文字先到 !";
        let rest = "[授权二维码](qr.png) ![第二张](qr.png)";
        let first = images
            .process(
                &scope,
                &exec(),
                Some(files.clone()),
                vec![start(), delta(0, prefix)],
            )
            .await;
        assert_eq!(text(first), "文字先到 ");
        assert!(
            images
                .process(
                    &scope,
                    &exec(),
                    Some(files.clone()),
                    vec![delta(prefix.len() as u64, rest)]
                )
                .await
                .is_empty()
        );
        assert_eq!(
            files.reads.load(Ordering::SeqCst),
            0,
            "no read on ambiguous streaming Markdown"
        );
        let tail = text(
            images
                .process(&scope, &exec(), Some(files.clone()), vec![end()])
                .await,
        );
        assert_eq!(
            tail,
            "![授权二维码](/api/files/fil_kept?ws=space%20one) ![第二张](/api/files/fil_kept?ws=space%20one)"
        );
        assert_eq!(*files.bytes.lock().unwrap(), bytes);
        assert_eq!(files.reads.load(Ordering::SeqCst), 1);
        assert!(
            images
                .capture(&scope, &exec(), Some(files), true)
                .await
                .is_empty()
        );
    }
    #[tokio::test]
    async fn code_examples_never_read_and_failed_receipts_preserve_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let scope = Scope::new(root.clone(), vec![root.clone()], vec![]);
        std::fs::write(root.join("qr.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
        let files = Arc::new(Files::default());
        let mut images = Images::default();
        // Until the last chunk this looks like an unclosed backtick span. It becomes code,
        // so even a real, readable image must not reach HubFiles.
        let first = "`![example](qr.png)";
        let mut events = images
            .process(
                &scope,
                &exec(),
                Some(files.clone()),
                vec![start(), delta(0, first)],
            )
            .await;
        events.extend(
            images
                .process(
                    &scope,
                    &exec(),
                    Some(files.clone()),
                    vec![delta(first.len() as u64, "`"), end()],
                )
                .await,
        );
        assert_eq!(text(events), format!("{first}`"));
        assert_eq!(files.reads.load(Ordering::SeqCst), 0);
        let bad = Arc::new(Files {
            corrupt_receipt: true,
            ..Files::default()
        });
        let original = "普通文本 ![授权二维码](qr.png)";
        let mut images = Images::default();
        let events = images
            .process(
                &scope,
                &exec(),
                Some(bad),
                vec![start(), delta(0, original), end()],
            )
            .await;
        assert_eq!(text(events), original);
        let mut images = Images::default();
        let mut events = images
            .process(&scope, &exec(), None, vec![start(), delta(0, original)])
            .await;
        events.extend(images.drain_original());
        assert_eq!(text(events), original, "cancellation retains pending text");
    }
    #[test]
    fn markdown_images_only_and_semantic_alt() {
        let text = "`![code](secret.png)`\n\n```md\n![code](secret.png)\n```\n\n    ![code](secret.png)\n\n[link](secret.png)\n\n![授权 二维码](qr.png) ![two][q]\n\n[q]: image.png\n";
        let n = nodes(text);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].alt, "授权 二维码");
        assert_eq!(&text[n[0].range.clone()], "![授权 二维码](qr.png)");
        assert_eq!(n[1].destination, "image.png");
        let nested =
            nodes("![outer ![inner](local.png)](https://example.invalid/image.png \"title\")");
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].destination, "https://example.invalid/image.png");
        assert_eq!(nested[0].alt, "outer inner");
        assert_eq!(nested[0].title, "title");
        for url in [
            "https://host/x.png",
            "file:///x.png",
            "//host/x.png",
            "/api/files/f?ws=w",
            "data:image/png,aaa",
            "x%00.png",
            "x%3Ay.png",
        ] {
            assert!(local_path(url).is_none(), "{url}");
        }
        assert_eq!(local_path("a%20b.png").as_deref(), Some("a b.png"));
    }
    #[test]
    fn scoped_reads_refuse_links_sensitive_traversal_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let scope = Scope::new(root.clone(), vec![root.clone()], vec![]);
        std::fs::write(root.join("ok.png"), b"bytes").unwrap();
        assert_eq!(read(&scope, "ok.png").unwrap(), b"bytes");
        assert_eq!(read(&scope, "./ok.png").unwrap(), b"bytes");
        std::fs::write(root.join(".env.png"), b"bytes").unwrap();
        std::os::unix::fs::symlink("ok.png", root.join("link.png")).unwrap();
        std::fs::hard_link(root.join("ok.png"), root.join("hard.png")).unwrap();
        for path in [
            ".env.png",
            "link.png",
            "hard.png",
            "../ok.png",
            "/etc/passwd",
        ] {
            assert!(read(&scope, path).is_err(), "{path}");
        }
        let file = std::fs::File::create(root.join("big.png")).unwrap();
        file.set_len(MAX_INLINE_IMAGE + 1).unwrap();
        assert!(read(&scope, "big.png").is_err());
    }
    #[test]
    fn captured_root_replacement_and_broad_control_plane_grants_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let own = root.join("workspace");
        std::fs::create_dir(&own).unwrap();
        std::fs::write(own.join("x.png"), b"old").unwrap();
        let scope = Scope::new(own.clone(), vec![own.clone()], vec![]);
        std::fs::rename(&own, root.join("old-workspace")).unwrap();
        std::fs::create_dir(&own).unwrap();
        std::fs::write(own.join("x.png"), b"new").unwrap();
        assert!(read(&scope, "x.png").is_err());
        let control = root.join("hub");
        std::fs::create_dir_all(control.join("data/files")).unwrap();
        std::fs::write(control.join("data/files/private.png"), b"bytes").unwrap();
        let scope = Scope::new(
            control.join("data/files"),
            vec![control.join("data/files")],
            protected(&control, &root.join("node")),
        );
        assert!(
            read(&scope, "private.png").is_err(),
            "even an explicitly broad control-plane root is denied"
        );
    }
    #[test]
    fn trusted_root_aliases_work_without_following_source_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let real = root.join("real");
        let alias = root.join("alias");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        std::fs::write(real.join("image.png"), b"bytes").unwrap();
        std::os::unix::fs::symlink("image.png", real.join("link.png")).unwrap();
        let scope = Scope::new(real.clone(), vec![alias.clone()], vec![]);
        assert_eq!(
            read(&scope, alias.join("image.png").to_str().unwrap()).unwrap(),
            b"bytes"
        );
        assert!(read(&scope, alias.join("link.png").to_str().unwrap()).is_err());
    }
}
