use super::fixture::{self, Env};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use steve_node_api::*;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Legacy,
    Resume,
    Corrupt,
    Modern,
    WrongPrefix,
}
struct Files {
    mode: Mode,
    begins: AtomicUsize,
    finishes: AtomicUsize,
    aborts: AtomicUsize,
    manifest: Mutex<Option<BlobManifest>>,
    bytes: Mutex<Vec<u8>>,
}
impl Files {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            begins: AtomicUsize::new(0),
            finishes: AtomicUsize::new(0),
            aborts: AtomicUsize::new(0),
            manifest: Mutex::new(None),
            bytes: Mutex::new(vec![]),
        }
    }
}
fn error() -> ApiError {
    ApiError::new(ErrorCode::Unsupported, "fixture")
}
#[async_trait]
impl HubFiles for Files {
    async fn fetch(&self, _: FileAccess, _: ArtifactId, _: u64, _: u32) -> ApiResult<ReadChunk> {
        Err(error())
    }
    async fn capture_reply(&self, r: ReplyCapture) -> ApiResult<String> {
        match self.mode {
            Mode::Modern => Ok(r.markdown.replace("pic.png", "/api/reply")),
            Mode::WrongPrefix => Ok("changed prefix".into()),
            _ => Err(error()),
        }
    }
    async fn begin_upload(&self, b: BeginUpload) -> ApiResult<Transfer> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        assert!(matches!(b.access, FileAccess::Execution(_)));
        *self.manifest.lock().unwrap() = Some(b.manifest);
        Ok(Transfer {
            id: TransferId::new("sg34-transfer").unwrap(),
            next_offset: self.bytes.lock().unwrap().len() as u64,
            workspace: Some("space one".into()),
        })
    }
    async fn upload(&self, b: BlobChunk) -> ApiResult<Transfer> {
        let mut bytes = self.bytes.lock().unwrap();
        assert_eq!(b.offset, bytes.len() as u64);
        assert_eq!(b.digest, Digest::sha256(&b.bytes));
        bytes.extend(b.bytes);
        Ok(Transfer {
            id: b.transfer,
            next_offset: bytes.len() as u64,
            workspace: Some("space one".into()),
        })
    }
    async fn upload_status(&self, _: TransferId) -> ApiResult<TransferState> {
        Err(error())
    }
    async fn abort_upload(&self, _: TransferId) -> ApiResult<TransferState> {
        self.aborts.fetch_add(1, Ordering::SeqCst);
        self.bytes.lock().unwrap().clear();
        Ok(TransferState::Aborted)
    }
    async fn finish_upload(&self, _: TransferId) -> ApiResult<ArtifactRef> {
        let n = self.finishes.fetch_add(1, Ordering::SeqCst);
        if self.mode == Mode::Resume && n == 0 {
            return Err(ApiError::new(ErrorCode::Offline, "first receipt lost"));
        }
        let m = self.manifest.lock().unwrap().clone().unwrap();
        Ok(ArtifactRef {
            id: ArtifactId::new("fil-sg34").unwrap(),
            sha256: if self.mode == Mode::Corrupt {
                Digest::sha256(b"wrong")
            } else {
                m.sha256
            },
            size: m.size,
            mime: m.mime,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg34_inline_capture_keeps_prefix_dedup_receipt_validation_and_resume() {
    let markdown = "Before ![first](pic.png) and ![again](pic.png)";
    for mode in [
        Mode::Legacy,
        Mode::Resume,
        Mode::Corrupt,
        Mode::Modern,
        Mode::WrongPrefix,
    ] {
        let e = Env::new();
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        std::fs::write(e.work.join("pic.png"), bytes).unwrap();
        let files = Arc::new(Files::new(mode));
        let n = e.node(true, Some(files.clone()));
        let w = e.workspace(&n).await;
        let s = e.session(&n, &w).await;
        fixture::turn(&n, &s, &w, "sg34-image", &format!("@say {markdown}")).await;
        let text: String = e
            .sink
            .events()
            .into_iter()
            .filter_map(|e| match e.body {
                EventBody::TextDelta { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        let expected = match mode {
            Mode::Modern => markdown.replace("pic.png", "/api/reply"),
            Mode::Legacy | Mode::Resume => {
                markdown.replace("pic.png", "/api/files/fil-sg34?ws=space%20one")
            }
            _ => markdown.into(),
        };
        assert_eq!(text, expected);
        let begins = files.begins.load(Ordering::SeqCst);
        match mode {
            Mode::Modern | Mode::WrongPrefix => assert_eq!(begins, 0),
            Mode::Resume => {
                assert_eq!(begins, 2);
                assert_eq!(files.finishes.load(Ordering::SeqCst), 2);
            }
            Mode::Corrupt => {
                assert_eq!(begins, 4);
                assert_eq!(files.finishes.load(Ordering::SeqCst), 4);
            }
            _ => assert_eq!(begins, 1),
        }
        assert_eq!(
            files.aborts.load(Ordering::SeqCst),
            if mode == Mode::Corrupt { 2 } else { 0 }
        );
        if matches!(mode, Mode::Legacy | Mode::Resume) {
            assert_eq!(*files.bytes.lock().unwrap(), bytes);
            assert_eq!(
                files.manifest.lock().unwrap().as_ref().unwrap().mime,
                "image/png"
            );
        }
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg34_inline_capture_refuses_source_links_and_retains_original_markdown() {
    use std::os::unix::fs::symlink;
    let e = Env::new();
    std::fs::write(
        e.dir.path().join("outside.png"),
        b"\x89PNG\r\n\x1a\nfixture",
    )
    .unwrap();
    symlink(e.dir.path().join("outside.png"), e.work.join("alias.png")).unwrap();
    let files = Arc::new(Files::new(Mode::Legacy));
    let n = e.node(true, Some(files.clone()));
    let w = e.workspace(&n).await;
    let s = e.session(&n, &w).await;
    let markdown = "Before ![alias](alias.png) and ![traversal](../outside.png)";
    fixture::turn(&n, &s, &w, "sg34-boundary", &format!("@say {markdown}")).await;
    let text: String = e
        .sink
        .events()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::TextDelta { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(text, markdown);
    assert_eq!(files.begins.load(Ordering::SeqCst), 0);
    n.shutdown().await;
}
