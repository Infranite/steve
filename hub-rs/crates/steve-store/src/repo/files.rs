//! Attachments (repo.ts `files`). The bytes live under `data/files`; the row is the descriptor everything
//! else copies as a [`FileRef`](crate::FileRef). Creating a file publishes nothing, as in TS.

use super::Cached;
use rusqlite::params;

use super::{must, one};
use crate::{FileRow, Result, Store, now};

impl Store {
    pub fn files(&self) -> Files<'_> {
        Files(self)
    }
}

/// Fields of a new `files` row; `id` is chosen by the caller (`fil_` + 16 hex) because the blob is
/// written under that name before the row exists.
#[derive(Debug, Clone, Default)]
pub struct NewFile {
    pub id: String,
    pub workspace_id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub created_by: String,
}

pub struct Files<'a>(&'a Store);

impl Files<'_> {
    pub fn get(&self, id: &str) -> Result<Option<FileRow>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM files WHERE id = ?",
                [id],
                FileRow::from_row,
            )
        })
    }

    /// The dedupe lookup: same workspace, content and (sanitized) name.
    pub fn same(&self, ws: &str, sha256: &str, name: &str) -> Result<Option<FileRow>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM files WHERE workspace_id = ? AND sha256 = ? AND name = ? LIMIT 1",
                [ws, sha256, name],
                FileRow::from_row,
            )
        })
    }

    pub fn create(&self, p: &NewFile) -> Result<FileRow> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO files (id, workspace_id, sha256, name, mime, size, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![p.id, p.workspace_id, p.sha256, p.name, p.mime, p.size, p.created_by, now()],
            )?;
            must(one(
                w.c,
                "SELECT * FROM files WHERE id = ?",
                [&p.id],
                FileRow::from_row,
            )?)
        })
    }
}
