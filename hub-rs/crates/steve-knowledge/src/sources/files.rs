//! Uploaded files as knowledge: an attachment from the `files` table
//! is added with `POST /kb/files` and indexed right away. Removing the
//! document (`DELETE /kb/docs/:id`) leaves the attachment itself alone.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;
use steve_modules::{ApiError, ApiResult, Caller, Check, Modules};
use steve_store::base::{self, Level, Visibility};
use steve_store::uid;

use crate::index::{BoxFuture, DocLoader, Loaded};
use crate::repo::{self, DocRow, NewDoc, SourceRow};
use crate::service;
use crate::sources::{self, folder};

pub const NOUN_FILE: &str = "附件";
/// The same attachment is already in the knowledge base, added by someone the caller cannot see.
pub const TAKEN: &str = "这个附件已经被别人加进知识库了";

/// `data/files/<workspace>/<sha256[..2]>/<id>`, as files.ts `blobPath` and steve-core's
/// `attachments::blob_path`.
pub fn blob_path(data_dir: &Path, ws: &str, sha256: &str, id: &str) -> PathBuf {
    data_dir
        .join("files")
        .join(ws)
        .join(sha256.get(..2).unwrap_or(""))
        .join(id)
}

/// Reads uploads for the index queue. `data_dir` is `STEVE_HOME/data`; the integrator passes it in
/// `Knowledge::with_loaders(guard, notify, vec![Arc::new(FilesLoader::new(data_dir))])`.
pub struct FilesLoader {
    data_dir: PathBuf,
}

impl FilesLoader {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }
}

impl DocLoader for FilesLoader {
    fn kinds(&self) -> &[&str] {
        &["files"]
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        doc: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            let Some(fid) = doc.file_id.as_deref() else {
                return Loaded::Gone;
            };
            let file = match m
                .store
                .reader(|c| repo::file_in_ws(c, &doc.workspace_id, fid))
            {
                Ok(Some(f)) => f,
                Ok(None) => return Loaded::Gone,
                Err(e) => return Loaded::Failed(format!("读取附件信息失败：{e}")),
            };
            let path = blob_path(&self.data_dir, &file.workspace_id, &file.sha256, &file.id);
            let bytes = match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
                Ok(Ok(b)) => b,
                _ => return Loaded::Failed("找不到附件的内容".into()),
            };
            let format = folder::format_of(&file.mime, &file.name, &bytes);
            folder::load_bytes(format, bytes).await
        })
    }
}

/// `POST /kb/files`.
#[derive(Debug, Deserialize)]
pub struct FileBody {
    pub file_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub level: Option<Level>,
}

/// Adds an attachment of the caller's workspace. Returns the document id and whether it is new
/// (an attachment already in the knowledge base returns its existing document).
pub fn add(m: &Modules, who: &Caller, b: &FileBody) -> ApiResult<(String, bool)> {
    let title = b.title.as_deref().map(str::trim);
    Check::new()
        .len("file_id", &b.file_id, 1, 64)
        .opt_len("title", title, 1, 120)
        .done()?;
    service::prepare(m, who)?;
    let viewer = m.viewer(who)?;
    let now = m.clock.now();
    m.store.transaction(|tx| {
        let file = repo::file_in_ws(tx, &who.ws, &b.file_id)?
            .ok_or_else(|| ApiError::not_found(NOUN_FILE))?;
        let src = sources::builtin(tx, &who.ws, "files", &who.me, now)?;
        if let Some(d) = repo::doc_by_ref(tx, &src.id, &file.id)? {
            return if viewer.sees(&d.eff_visibility(&src), &d.created_by) {
                Ok((d.id, false))
            } else {
                Err(ApiError::conflict(TAKEN))
            };
        }
        if let Some(v) = &b.visibility
            && !repo::scope_exists(tx, &who.ws, v)?
        {
            return Err(ApiError::not_found(match v {
                Visibility::Space(_) => "空间",
                _ => "团队",
            }));
        }
        let lower = file.name.to_ascii_lowercase();
        let mime = if lower.ends_with(".md") || lower.ends_with(".markdown") {
            "text/markdown".to_string()
        } else {
            file.mime.clone()
        };
        let id = uid("kd");
        repo::insert_doc(
            tx,
            &who.ws,
            &NewDoc {
                id: id.clone(),
                source_id: src.id.clone(),
                r#ref: file.id.clone(),
                title: title.map_or_else(|| file.name.clone(), str::to_string),
                mime,
                file_id: Some(file.id.clone()),
                size: file.size,
                visibility: b.visibility.clone(),
                level: b.level,
                created_by: who.me.clone(),
                ..Default::default()
            },
            now,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "kb_doc.add_file",
            &id,
            &json!({"file_id": file.id}),
        )?;
        Ok::<_, ApiError>((id, true))
    })
}
