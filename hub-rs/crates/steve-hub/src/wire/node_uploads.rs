//! Output images use the existing reverse blob RPCs. Partial uploads are bounded, expiring memory,
//! not publicly readable files. Reconnecting can begin again; completed content is deduplicated on
//! disk by workspace and hash, even across Hub restarts. No path from a Node is opened on the Hub.
use super::{NodeFiles, denied, internal};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use steve_node_api::error as rpc_error;
use steve_node_api::*;

#[derive(Default)]
pub(super) struct Uploads(HashMap<String, Upload>);
struct Upload {
    execution: ExecutionRef,
    workspace: String,
    manifest: BlobManifest,
    bytes: Vec<u8>,
    state: TransferState,
    touched: Instant,
}
const TTL: Duration = Duration::from_secs(600);
const CHUNK: usize = INLINE_IMAGE_CHUNK;
const MEMORY: usize = 2 * MAX_INLINE_IMAGE as usize;

fn invalid(s: &str) -> Error {
    rpc_error(Code::Invalid, s)
}
fn conflict() -> Error {
    rpc_error(Code::Conflict, "image transfer does not match")
}
impl Upload {
    fn abort(&mut self) {
        if !matches!(self.state, TransferState::Done { .. }) {
            // Drop the allocation, not just its length: failed decodes can otherwise exhaust
            // both the Node's four Open slots and its byte budget until the next TTL sweep.
            self.bytes = vec![];
            self.state = TransferState::Aborted;
            self.touched = Instant::now();
        }
    }
    fn transfer(&self, id: TransferId) -> Transfer {
        Transfer {
            id,
            next_offset: match self.state {
                TransferState::Done { .. } => self.manifest.size,
                _ => self.bytes.len() as u64,
            },
            workspace: Some(self.workspace.clone()),
        }
    }
}
impl NodeFiles {
    pub(super) fn begin_image(&self, b: BeginUpload) -> ApiResult<Transfer> {
        let mut uploads = self.uploads.lock().unwrap();
        uploads.0.retain(|_, u| u.touched.elapsed() < TTL);
        for u in uploads
            .0
            .values_mut()
            .filter(|u| matches!(u.state, TransferState::Open { .. }))
        {
            // An attempt can end or be cancelled while its Node is disconnected. Reclaim its
            // partial bytes before admitting another execution; no restart or TTL wait needed.
            let _ = self.check_upload(u);
        }
        let exec = self.execution(b.access)?;
        let (workspace, _) = self.grant(&exec)?;
        if b.manifest.size == 0
            || b.manifest.size > MAX_INLINE_IMAGE
            || b.manifest.name.len() > 256
            || !crate::files::is_inline_image(&b.manifest.mime)
        {
            return Err(invalid("invalid image manifest"));
        }
        // The key belongs to this authenticated Node, and can never be rebound to another execution.
        let digest = Digest::sha256(format!("{}\0{}", self.node, b.key).as_bytes());
        let id = TransferId::new(format!("img_{}", &digest.as_str()[7..])).map_err(internal)?;
        if let Some(u) = uploads.0.get_mut(id.as_str()) {
            if u.execution != exec || u.workspace != workspace || u.manifest != b.manifest {
                return Err(conflict());
            }
            if matches!(u.state, TransferState::Aborted) {
                return Err(conflict());
            }
            u.touched = Instant::now();
            return Ok(u.transfer(id));
        }
        if uploads.0.len() >= 256 {
            // Bounded tombstones preserve immediate abort/status replay, but never deny a new
            // upload solely because earlier content was invalid. Done receipts keep their TTL.
            uploads
                .0
                .retain(|_, u| !matches!(u.state, TransferState::Aborted));
        }
        if uploads.0.len() >= 256
            || uploads
                .0
                .values()
                .filter(|u| u.execution == exec && !matches!(u.state, TransferState::Aborted))
                .count()
                >= MAX_INLINE_IMAGES
            || uploads
                .0
                .values()
                .filter(|u| matches!(u.state, TransferState::Open { .. }))
                .count()
                >= 4
        {
            return Err(rpc_error(Code::Exhausted, "image transfer quota"));
        }
        let u = Upload {
            execution: exec,
            workspace,
            manifest: b.manifest,
            bytes: vec![],
            state: TransferState::Open { next_offset: 0 },
            touched: Instant::now(),
        };
        let transfer = u.transfer(id.clone());
        uploads.0.insert(id.to_string(), u);
        Ok(transfer)
    }

    fn check_upload(&self, u: &mut Upload) -> ApiResult<()> {
        if u.touched.elapsed() >= TTL {
            u.abort();
            return Err(denied());
        }
        match self.grant(&u.execution) {
            Ok((workspace, _)) if workspace == u.workspace => Ok(()),
            result => {
                u.abort();
                Err(result.err().unwrap_or_else(denied))
            }
        }
    }

    pub(super) fn image_chunk(&self, b: BlobChunk) -> ApiResult<Transfer> {
        if b.bytes.is_empty() || b.bytes.len() > CHUNK || Digest::sha256(&b.bytes) != b.digest {
            return Err(invalid("invalid image chunk"));
        }
        let mut uploads = self.uploads.lock().unwrap();
        let used: usize = uploads.0.values().map(|u| u.bytes.len()).sum();
        let u = uploads.0.get_mut(b.transfer.as_str()).ok_or_else(denied)?;
        self.check_upload(u)?;
        if !matches!(u.state, TransferState::Open { .. }) {
            return Err(conflict());
        }
        let at = usize::try_from(b.offset).map_err(|_| conflict())?;
        let end = at.checked_add(b.bytes.len()).ok_or_else(conflict)?;
        if end as u64 > u.manifest.size || at > u.bytes.len() {
            return Err(conflict());
        }
        if at < u.bytes.len() {
            // A lost reply may resend a complete chunk; overlapping different bytes never replace it.
            if u.bytes.get(at..end) != Some(&b.bytes) {
                return Err(conflict());
            }
        } else {
            if used + b.bytes.len() > MEMORY {
                return Err(rpc_error(Code::Exhausted, "image transfer memory quota"));
            }
            u.bytes.extend(b.bytes);
        }
        u.state = TransferState::Open {
            next_offset: u.bytes.len() as u64,
        };
        u.touched = Instant::now();
        Ok(u.transfer(b.transfer))
    }
    pub(super) fn image_status(&self, id: TransferId) -> ApiResult<TransferState> {
        let mut uploads = self.uploads.lock().unwrap();
        let u = uploads.0.get_mut(id.as_str()).ok_or_else(denied)?;
        self.check_upload(u)?;
        Ok(u.state.clone())
    }
    pub(super) fn finish_image(&self, id: TransferId) -> ApiResult<ArtifactRef> {
        let mut uploads = self.uploads.lock().unwrap();
        let u = uploads.0.get_mut(id.as_str()).ok_or_else(denied)?;
        self.check_upload(u)?;
        match &u.state {
            TransferState::Done { artifact } => return Ok(artifact.clone()),
            TransferState::Aborted => return Err(conflict()),
            _ => {}
        }
        // An early finish is recoverable by sending the missing suffix. Complete but corrupt
        // content cannot be repaired by the immutable chunk protocol and is terminal.
        if (u.bytes.len() as u64) < u.manifest.size {
            return Err(conflict());
        }
        if u.bytes.len() as u64 != u.manifest.size || Digest::sha256(&u.bytes) != u.manifest.sha256
        {
            u.abort();
            return Err(conflict());
        }
        if let Err(error) = validate_image(&u.bytes, &u.manifest.mime) {
            u.abort();
            return Err(error);
        }
        self.check_upload(u)?;
        let artifact = super::job_input(
            &self.store,
            &self.data_dir,
            &u.workspace,
            &u.manifest.mime,
            &u.bytes,
        )?;
        u.bytes = vec![];
        u.state = TransferState::Done {
            artifact: artifact.clone(),
        };
        u.touched = Instant::now();
        Ok(artifact)
    }
    pub(super) fn abort_image(&self, id: TransferId) -> ApiResult<TransferState> {
        let mut uploads = self.uploads.lock().unwrap();
        let u = uploads.0.get_mut(id.as_str()).ok_or_else(denied)?;
        // This is cleanup of this authenticated Node's own registry, not permission to read or
        // publish. Allow it after cancellation/revocation too. Never discard a Done receipt/blob.
        u.abort();
        Ok(u.state.clone())
    }
}

pub(crate) fn validate_image(bytes: &[u8], claimed: &str) -> ApiResult<()> {
    use image::{AnimationDecoder, ImageDecoder};
    if !crate::files::sniff(bytes).is_some_and(|kind| kind.mime == claimed) {
        return Err(invalid("image content type mismatch"));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid("invalid image"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits.clone());
    reader
        .decode()
        .map_err(|_| invalid("invalid or excessive image"))?;
    // Decoders can stop after the first frame or ignore trailing data. Require the complete
    // container, and validate all animation frames with a bounded total decoded budget too.
    let bad = || invalid("incomplete image container");
    match claimed {
        "image/png" => {
            let mut at = 8usize;
            loop {
                let head = bytes.get(at..at + 8).ok_or_else(bad)?;
                let len = u32::from_be_bytes(head[..4].try_into().map_err(|_| bad())?) as usize;
                let end = at
                    .checked_add(12)
                    .and_then(|n| n.checked_add(len))
                    .ok_or_else(bad)?;
                if end > bytes.len() {
                    return Err(bad());
                }
                if &head[4..] == b"IEND" {
                    if len != 0 || end != bytes.len() {
                        return Err(bad());
                    }
                    break;
                }
                at = end;
            }
            let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes))
                .map_err(|_| bad())?;
            decoder.set_limits(limits).map_err(|_| bad())?;
            if decoder.is_apng().map_err(|_| bad())? {
                validate_frames(decoder.apng().map_err(|_| bad())?.into_frames())?;
            }
        }
        "image/jpeg" if !bytes.ends_with(&[0xff, 0xd9]) => return Err(bad()),
        "image/gif" => {
            if !bytes.ends_with(&[0x3b]) {
                return Err(bad());
            }
            let mut decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
                .map_err(|_| bad())?;
            decoder.set_limits(limits).map_err(|_| bad())?;
            validate_frames(decoder.into_frames())?;
        }
        "image/webp" => {
            let len = u32::from_le_bytes(
                bytes
                    .get(4..8)
                    .ok_or_else(bad)?
                    .try_into()
                    .map_err(|_| bad())?,
            ) as u64;
            if len + 8 != bytes.len() as u64 {
                return Err(bad());
            }
            let mut decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))
                .map_err(|_| bad())?;
            decoder.set_limits(limits).map_err(|_| bad())?;
            if decoder.has_animation() {
                validate_frames(decoder.into_frames())?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_frames(frames: image::Frames<'_>) -> ApiResult<()> {
    let mut decoded = 0usize;
    let mut count = 0;
    for frame in frames {
        count += 1;
        let frame = frame.map_err(|_| invalid("invalid image frame"))?;
        decoded += frame.buffer().len();
        if count > 100 || decoded > 64 * 1024 * 1024 {
            return Err(invalid("excessive image animation"));
        }
    }
    if count == 0 {
        return Err(invalid("empty image animation"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::node_tools::tests::fixture;

    fn png() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }
    #[test]
    fn raster_types_are_decoded_and_containers_must_be_complete() {
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::Gif, "image/gif"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut encoded = std::io::Cursor::new(vec![]);
            image::DynamicImage::new_rgb8(2, 2)
                .write_to(&mut encoded, format)
                .unwrap();
            let bytes = encoded.into_inner();
            validate_image(&bytes, mime).unwrap_or_else(|e| panic!("{mime}: {e:?}"));
            assert!(
                validate_image(&bytes[..bytes.len() / 2], mime).is_err(),
                "{mime}"
            );
            let mut trailing = bytes.clone();
            trailing.extend(b"hidden payload");
            assert!(validate_image(&trailing, mime).is_err(), "{mime}");
        }
    }
    fn request(exec: &ExecutionRef, bytes: &[u8], key: &str) -> BeginUpload {
        BeginUpload {
            key: OperationId::new(key).unwrap(),
            access: FileAccess::Execution(exec.clone()),
            manifest: BlobManifest {
                sha256: Digest::sha256(bytes),
                size: bytes.len() as u64,
                mime: "image/png".into(),
                name: "qr.png".into(),
            },
        }
    }
    fn chunk(id: &TransferId, offset: u64, bytes: &[u8]) -> BlobChunk {
        BlobChunk {
            transfer: id.clone(),
            offset,
            bytes: bytes.into(),
            digest: Digest::sha256(bytes),
        }
    }
    #[tokio::test]
    async fn resumes_verifies_and_deduplicates_images_across_reconnect() {
        let h = fixture(vec![]).await;
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let bytes = png();
        let b = request(&h.exec, &bytes, "image-one");
        let t = files.begin_upload(b.clone()).await.unwrap();
        assert_eq!(t.workspace.as_deref(), Some("default"));
        files.upload(chunk(&t.id, 0, &bytes[..20])).await.unwrap();
        assert_eq!(files.begin_upload(b.clone()).await.unwrap().next_offset, 20);
        assert_eq!(
            files
                .upload(chunk(&t.id, 0, &bytes[..20]))
                .await
                .unwrap()
                .next_offset,
            20
        );
        assert_eq!(
            files.finish_upload(t.id.clone()).await.unwrap_err().code,
            Code::Conflict
        );
        assert!(files.upload(chunk(&t.id, 0, b"changed")).await.is_err());
        assert!(files.upload(chunk(&t.id, 50, &bytes[50..])).await.is_err());
        let mut bad = chunk(&t.id, 20, &bytes[20..]);
        bad.digest = Digest::sha256(b"wrong");
        assert!(files.upload(bad).await.is_err());
        files.upload(chunk(&t.id, 20, &bytes[20..])).await.unwrap();
        let artifact = files.finish_upload(t.id.clone()).await.unwrap();
        assert_eq!(artifact, files.finish_upload(t.id.clone()).await.unwrap());
        assert!(matches!(
            files.upload_status(t.id.clone()).await.unwrap(),
            TransferState::Done { .. }
        ));
        assert!(matches!(
            files.abort_upload(t.id).await.unwrap(),
            TransferState::Done { .. }
        ));
        let row = h.store.files().get(artifact.id.as_str()).unwrap().unwrap();
        let path = h
            .dir
            .path()
            .join("files/default")
            .join(&row.sha256[..2])
            .join(&row.id);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A new link/Hub transfer registry restarts from 0, but the durable artifact is unchanged.
        let again = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let t = again.begin_upload(b).await.unwrap();
        assert_eq!(t.next_offset, 0);
        again.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
        std::fs::write(&path, vec![0; bytes.len()]).unwrap();
        assert_eq!(again.finish_upload(t.id).await.unwrap(), artifact);
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "same-length corrupt content repaired"
        );
    }
    #[tokio::test]
    async fn malformed_images_release_slots_without_caller_abort() {
        let h = fixture(vec![]).await;
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        for i in 0..4 {
            let mut bytes = b"\x89PNG\r\n\x1a\ninvalid-".to_vec();
            bytes.extend(i.to_string().as_bytes());
            let t = files
                .begin_upload(request(&h.exec, &bytes, &format!("bad-{i}")))
                .await
                .unwrap();
            files.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
            assert!(files.finish_upload(t.id.clone()).await.is_err());
            assert_eq!(
                files.upload_status(t.id.clone()).await.unwrap(),
                TransferState::Aborted
            );
            assert!(
                files.uploads.lock().unwrap().0[t.id.as_str()]
                    .bytes
                    .is_empty()
            );
        }
        let bytes = png();
        let t = files
            .begin_upload(request(&h.exec, &bytes, "valid-after-bad"))
            .await
            .unwrap();
        files.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
        files.finish_upload(t.id).await.unwrap();
        let uploads = files.uploads.lock().unwrap();
        assert!(uploads.0.values().all(|u| u.bytes.is_empty()));
        assert!(
            !uploads
                .0
                .values()
                .any(|u| matches!(u.state, TransferState::Open { .. }))
        );
    }
    #[tokio::test]
    async fn aborted_transfers_do_not_exhaust_active_or_metadata_quota() {
        let h = fixture(vec![]).await;
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let bad = b"\x89PNG\r\n\x1a\ntruncated";
        for i in 0..260 {
            let request = request(&h.exec, bad, &format!("invalid-batch-{i}"));
            let t = files.begin_upload(request.clone()).await.unwrap();
            files.upload(chunk(&t.id, 0, bad)).await.unwrap();
            assert!(files.finish_upload(t.id.clone()).await.is_err());
            assert!(
                files.begin_upload(request).await.is_err(),
                "a terminal key must not reopen"
            );
            let uploads = files.uploads.lock().unwrap();
            assert!(uploads.0.len() <= 256);
            assert!(uploads.0.values().all(|u| u.bytes.capacity() == 0));
        }
        let bytes = png();
        let t = files
            .begin_upload(request(&h.exec, &bytes, "valid-after-many"))
            .await
            .unwrap();
        files.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
        let artifact = files.finish_upload(t.id.clone()).await.unwrap();
        assert_eq!(
            files.abort_upload(t.id.clone()).await.unwrap(),
            TransferState::Done {
                artifact: artifact.clone()
            }
        );
        assert_eq!(files.finish_upload(t.id).await.unwrap(), artifact);
    }
    #[tokio::test]
    async fn invalid_content_size_hash_abort_and_expiry_never_create_files() {
        let h = fixture(vec![]).await;
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let mut huge = request(&h.exec, b"x", "huge");
        huge.manifest.size = MAX_INLINE_IMAGE + 1;
        assert!(files.begin_upload(huge).await.is_err());
        for (i, bytes) in [
            b"<svg onload=x>".as_slice(),
            b"\x89PNG\r\n\x1a\ntruncated",
            b"",
            b"plain text",
        ]
        .into_iter()
        .enumerate()
        {
            let b = request(&h.exec, bytes, &format!("invalid{i}"));
            if bytes.is_empty() {
                assert!(files.begin_upload(b).await.is_err());
                continue;
            }
            let t = files.begin_upload(b).await.unwrap();
            files.upload(chunk(&t.id, 0, bytes)).await.unwrap();
            assert_eq!(
                files.finish_upload(t.id.clone()).await.unwrap_err().code,
                Code::Invalid
            );
            assert_eq!(
                files.upload_status(t.id.clone()).await.unwrap(),
                TransferState::Aborted
            );
            assert!(
                files.uploads.lock().unwrap().0[t.id.as_str()]
                    .bytes
                    .is_empty()
            );
            assert!(files.finish_upload(t.id).await.is_err());
        }
        let bytes = png();
        let mut b = request(&h.exec, &bytes, "bad-total");
        b.manifest.sha256 = Digest::sha256(b"wrong");
        let t = files.begin_upload(b).await.unwrap();
        files.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
        assert_eq!(
            files.finish_upload(t.id.clone()).await.unwrap_err().code,
            Code::Conflict
        );
        assert_eq!(
            files.upload_status(t.id.clone()).await.unwrap(),
            TransferState::Aborted
        );
        assert!(
            files.uploads.lock().unwrap().0[t.id.as_str()]
                .bytes
                .is_empty()
        );
        files
            .uploads
            .lock()
            .unwrap()
            .0
            .get_mut(t.id.as_str())
            .unwrap()
            .touched = Instant::now() - TTL;
        assert!(files.upload_status(t.id).await.is_err());
        assert_eq!(
            h.store
                .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT count(*) FROM files",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            0
        );
        assert!(!h.dir.path().join("files").exists());
    }
    #[tokio::test]
    async fn cancellation_and_expiry_reclaim_partial_bytes_and_allow_cleanup() {
        for cancel in [true, false] {
            let h = fixture(vec![]).await;
            let files = NodeFiles::new(
                h.store.clone(),
                NodeId::new("local").unwrap(),
                h.dir.path().into(),
            );
            let bytes = png();
            let mut ids = vec![];
            for i in 0..4 {
                let t = files
                    .begin_upload(request(&h.exec, &bytes, &format!("partial-{i}")))
                    .await
                    .unwrap();
                files.upload(chunk(&t.id, 0, &bytes[..20])).await.unwrap();
                ids.push(t.id);
            }
            if cancel {
                h.store
                    .transaction::<_, steve_store::StoreError>(|tx| {
                        tx.execute("UPDATE runtime_attempts SET cancel_requested=1", [])?;
                        Ok(())
                    })
                    .unwrap();
            } else {
                for u in files.uploads.lock().unwrap().0.values_mut() {
                    u.touched = Instant::now() - TTL;
                }
            }
            assert!(files.upload_status(ids[0].clone()).await.is_err());
            assert!(
                files
                    .upload(chunk(&ids[1], 20, &bytes[20..]))
                    .await
                    .is_err()
            );
            assert!(files.finish_upload(ids[2].clone()).await.is_err());
            // Cleanup must work after execution eligibility or the upload deadline is gone.
            assert_eq!(
                files.abort_upload(ids[3].clone()).await.unwrap(),
                TransferState::Aborted
            );
            assert!(
                files
                    .uploads
                    .lock()
                    .unwrap()
                    .0
                    .values()
                    .all(|u| u.bytes.is_empty() && matches!(u.state, TransferState::Aborted))
            );
            if !cancel {
                let t = files
                    .begin_upload(request(&h.exec, &bytes, "after-expiry"))
                    .await
                    .unwrap();
                files.upload(chunk(&t.id, 0, &bytes)).await.unwrap();
                files.finish_upload(t.id).await.unwrap();
            }
        }
    }
    #[tokio::test]
    async fn new_begin_reclaims_ineligible_executions_without_old_node_cleanup() {
        let h = fixture(vec![]).await;
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let bytes = png();
        for i in 0..4 {
            let t = files
                .begin_upload(request(&h.exec, &bytes, &format!("lost-{i}")))
                .await
                .unwrap();
            files.upload(chunk(&t.id, 0, &bytes[..20])).await.unwrap();
        }
        h.store
            .transaction::<_, steve_store::StoreError>(|tx| {
                tx.execute("UPDATE runtime_attempts SET state='finished'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(
            files
                .begin_upload(request(&h.exec, &bytes, "new-begin"))
                .await
                .is_err()
        );
        assert!(
            files
                .uploads
                .lock()
                .unwrap()
                .0
                .values()
                .all(|u| u.bytes.is_empty() && matches!(u.state, TransferState::Aborted))
        );
    }
    #[tokio::test]
    async fn node_execution_and_workspace_cannot_be_rebound() {
        let h = fixture(vec![]).await;
        let bytes = png();
        let b = request(&h.exec, &bytes, "scoped");
        let foreign = NodeFiles::new(
            h.store.clone(),
            NodeId::new("other").unwrap(),
            h.dir.path().into(),
        );
        assert_eq!(
            foreign.begin_upload(b.clone()).await.unwrap_err().code,
            Code::Forbidden
        );
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let t = files.begin_upload(b.clone()).await.unwrap();
        let mut changed = b;
        changed.manifest.sha256 = Digest::sha256(b"different");
        assert_eq!(
            files.begin_upload(changed).await.unwrap_err().code,
            Code::Conflict
        );
        h.store
            .transaction::<_, steve_store::StoreError>(|tx| {
                tx.execute("UPDATE sessions SET workspace_id='foreign'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(files.upload(chunk(&t.id, 0, &bytes)).await.is_err());
        assert!(files.finish_upload(t.id).await.is_err());
    }
}
