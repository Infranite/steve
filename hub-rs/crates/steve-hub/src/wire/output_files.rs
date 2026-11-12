//! Durable, non-Git deliverables in an item's working directory. The Git worktree may be
//! removed after a merge; `<home>/workspaces/<item>/outputs` is not part of that cleanup.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use steve_node_api::{FileAddress, FileRead, GitAnswer, GitQuery, GitRead, PathRef, RelPath};
use steve_store::Session;
use steve_store::{Store, StoreError};

use super::workspaces::{At, Subject, Workspaces};

const MAX_FILE: usize = 64 * 1024 * 1024;

fn item_component(item: &str) -> io::Result<&str> {
    let mut parts = Path::new(item).components();
    if !item.is_empty()
        && item
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && matches!(parts.next(), Some(Component::Normal(_)))
        && parts.next().is_none()
    {
        Ok(item)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid item id",
        ))
    }
}

pub fn directory(home: &Path, item: &str) -> io::Result<PathBuf> {
    Ok(home
        .join("workspaces")
        .join(item_component(item)?)
        .join("outputs"))
}

/// Save redacted Markdown using a content-derived name. Retrying a delivery yields the same
/// path; a crash leaves either the old complete file or a complete new one.
pub fn save_report(home: &Path, item: &str, text: &str) -> io::Result<PathBuf> {
    save_bytes(home, item, "report.md", text.as_bytes())
}

/// Keep a delivered file in the Hub's item directory. The source is read before this call, while
/// its Node workspace still exists. A repeated delivery of the same bytes reuses the same file.
pub fn save_bytes(home: &Path, item: &str, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let dir = directory(home, item)?;
    let suffix = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("bin");
    let root = home.join("workspaces");
    for part in [&root, dir.parent().unwrap(), &dir] {
        if !part.exists() {
            fs::create_dir(part)?;
        }
        if !fs::symlink_metadata(part)?.file_type().is_dir() {
            return Err(io::Error::other("output directory is not a real directory"));
        }
        fs::set_permissions(part, fs::Permissions::from_mode(0o700))?;
    }
    let hash = hex::encode(Sha256::digest(bytes));
    let path = dir.join(format!("output-{hash}.{suffix}"));
    if path.exists() {
        if fs::symlink_metadata(&path)?.file_type().is_file() && fs::read(&path)? == bytes {
            return Ok(path);
        }
        return Err(io::Error::other(
            "saved report does not match its content hash",
        ));
    }
    let temp = dir.join(format!(".report-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temp, &path) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&path)?.file_type().is_file()
                    && fs::read(&path)? == bytes => {}
            Err(e) => return Err(e),
        }
        fs::remove_file(&temp)?;
        File::open(&dir)?.sync_all()?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Old agents sometimes used the prose label of their delivery as a file location.
pub fn summary_reference(location: Option<&str>) -> bool {
    let Some(location) = location.map(str::trim) else {
        return false;
    };
    location.starts_with("本次交付摘要")
        || location.starts_with("交付摘要")
        || location.starts_with("本次交付正文")
        || location.contains("steve_deliver summary")
        || (location.contains("steve_workflow") && location.contains("summary"))
        || (location.starts_with("ST-") && location.contains("交付摘要"))
}

/// On upgrading an older Hub, repair prose-only output references from the saved workflow result
/// or parent timeline. This is a one-time, idempotent data repair: no image or unknown path is
/// guessed. A verified database copy is made before any artifact row changes.
pub fn recover_legacy(store: &Store, home: &Path, db_path: &Path) -> anyhow::Result<usize> {
    let candidates: Vec<(String, String, Option<String>, String)> = store.reader(|c| {
        let mut stmt = c.prepare("SELECT id,item_id,kind,title,location,created FROM artifacts ORDER BY created,id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?,
            r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, i64>(5)?)))?;
        let mut found = Vec::new();
        for row in rows {
            let (id, item, kind, title, location, created) = row?;
            let blank_prose = location.as_deref().is_none_or(|loc| loc.trim().is_empty())
                && ["方案", "设计", "报告", "报表", "分析", "评审", "结论"].iter().any(|word| kind.contains(word))
                && ["设计", "方案", "评审", "结论", "报告", "报表", "分析"].iter().any(|word| title.contains(word));
            if !summary_reference(location.as_deref()) && !blank_prose { continue; }
            let from_unit: Option<String> = c.query_row(
                "SELECT summary FROM workflow_units WHERE item_id=? AND length(summary)>80 ORDER BY length(summary) DESC LIMIT 1",
                [&item], |r| r.get(0)).optional()?;
            let body = if let Some(text) = from_unit { Some(text) } else {
                let timeline: Option<String> = c.query_row(
                    "SELECT text FROM timeline WHERE item_id=? AND kind='sys' AND text LIKE '%已交付，等待执行结束后交给下游%' AND ts<=? ORDER BY ts DESC,id DESC LIMIT 1",
                    rusqlite::params![item, created], |r| r.get(0)).optional()?;
                timeline.and_then(|text| text.split_once('\n').map(|(_, body)| body.to_owned()))
                    .filter(|body| body.chars().count() > 80)
            };
            if let Some(body) = body { found.push((id, item, location, body)); }
        }
        Ok::<_, StoreError>(found)
    })?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let backups = home.join("backups");
    if !backups.exists() {
        fs::create_dir(&backups)?;
    }
    if !fs::symlink_metadata(&backups)?.file_type().is_dir() {
        return Err(io::Error::other("backup directory is not a real directory").into());
    }
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o700))?;
    let backup = backups.join(format!(
        "pre-delivery-recovery-{}.sqlite3",
        uuid::Uuid::new_v4().simple()
    ));
    crate::upgrades::supervisor::backup(db_path, &backup)?;
    let paths: Vec<(String, Option<String>, String)> = candidates
        .into_iter()
        .map(|(id, item, old, body)| {
            save_report(home, &item, &body)
                .map(|path| (id, old, path.to_string_lossy().into_owned()))
        })
        .collect::<io::Result<_>>()?;
    store.transaction(|tx| {
        for (id, old, path) in &paths {
            let count = tx.execute(
                "UPDATE artifacts SET location=? WHERE id=? AND location IS ?",
                rusqlite::params![path, id, old],
            )?;
            if count != 1 {
                return Err(StoreError::Conflict(format!(
                    "artifact changed during recovery: {id}"
                )));
            }
        }
        Ok(())
    })?;
    tracing::info!(recovered = paths.len(), backup = %backup.display(), "legacy delivery outputs recovered");
    Ok(paths.len())
}

/// A declared file inside the disposable workspace is copied before the delivery can close it.
/// Paths already in the durable output directory stay there. Git revisions and opaque references
/// have no file extension and are left to their normal Git viewer.
pub async fn retain_location(
    home: &Path,
    item: &str,
    session: &Session,
    location: &str,
    workspaces: &Workspaces,
) -> Result<String, String> {
    let raw = Path::new(location);
    let source = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        Path::new(&session.cwd).join(raw)
    };
    let cwd = Path::new(&session.cwd);
    let durable = directory(home, item).map_err(|e| e.to_string())?;
    if source.starts_with(&durable) {
        // The file is already in the Hub-owned persistent directory.
        let actual = fs::canonicalize(&source).map_err(|e| e.to_string())?;
        if !actual.starts_with(fs::canonicalize(&durable).map_err(|e| e.to_string())?)
            || !actual.is_file()
        {
            return Err("交付文件不在事项 outputs 目录中".into());
        }
        return Ok(actual.to_string_lossy().into_owned());
    }
    if !source.starts_with(cwd)
        || (source.extension().is_none() && !source.starts_with(cwd.join("outputs")))
    {
        return Ok(location.into());
    }
    let relative = source.strip_prefix(cwd).map_err(|e| e.to_string())?;
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("交付文件名无效")?;
    let data = match workspaces.placement_of(Subject::Session(&session.id)) {
        Ok(p) if !workspaces.is_local_placement(&p) => {
            let At::Workspace(workspace) = p.at else {
                return Err("远程事项没有可读取的工作区".into());
            };
            let rel = RelPath::new(relative.to_string_lossy().replace('\\', "/"))
                .map_err(|e| e.to_string())?;
            if p.node
                .git_facts(GitQuery {
                    workspace: workspace.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: false,
                    reads: vec![GitRead::TrackedIgnored { rel: rel.clone() }],
                    heads_only: true,
                })
                .await
                .ok()
                .and_then(|facts| facts.reads.into_iter().next())
                .is_some_and(|fact| {
                    matches!(fact.answer, GitAnswer::TrackedIgnored { tracked: true, .. })
                })
            {
                return Ok(location.into());
            }
            let path = FileAddress::Workspace(PathRef { workspace, rel });
            let mut data = Vec::new();
            loop {
                let chunk = p
                    .node
                    .read_file(FileRead {
                        path: path.clone(),
                        expected: None,
                        offset: data.len() as u64,
                        max: 128 * 1024,
                    })
                    .await
                    .map_err(|e| format!("读取 Node 交付文件失败：{}", e.safe_detail))?;
                if chunk.offset != data.len() as u64 || data.len() + chunk.bytes.len() > MAX_FILE {
                    return Err("交付文件超过 64 MiB 或读取位置变化".into());
                }
                let empty = chunk.bytes.is_empty();
                data.extend(chunk.bytes);
                if chunk.eof {
                    break;
                }
                if empty {
                    return Err("Node 交付文件读取中断".into());
                }
            }
            data
        }
        Ok(_) | Err(super::workspaces::PlacementError::LocalOff) => {
            if steve_core::git::is_git(cwd) {
                let rel = relative.to_string_lossy();
                if steve_core::git::git_test(cwd, &["ls-files", "--error-unmatch", "--", &rel])
                    .unwrap_or(false)
                {
                    return Ok(location.into());
                }
            }
            let actual = fs::canonicalize(&source).map_err(|e| e.to_string())?;
            let allowed = fs::canonicalize(cwd).map_err(|e| e.to_string())?;
            if !actual.starts_with(&allowed) || !actual.is_file() {
                return Err(
                    "交付文件不存在、超出事项工作目录或是文件夹；文件夹请先打包成 ZIP".into(),
                );
            }
            let data = fs::read(actual).map_err(|e| e.to_string())?;
            if data.len() > MAX_FILE {
                return Err("交付文件超过 64 MiB".into());
            }
            data
        }
        Err(e) => return Err(format!("交付文件所在 Node 不可用：{e}")),
    };
    let saved = save_bytes(home, item, name, &data).map_err(|e| e.to_string())?;
    Ok(saved.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_private_complete_and_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let first = save_report(home.path(), "ST-8", "# 方案\n\n正文").unwrap();
        let second = save_report(home.path(), "ST-8", "# 方案\n\n正文").unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "# 方案\n\n正文");
        assert_eq!(
            fs::metadata(home.path().join("workspaces/ST-8/outputs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(save_report(home.path(), "../other", "x").is_err());
        let other = tempfile::tempdir().unwrap();
        let symlinked = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), symlinked.path().join("workspaces")).unwrap();
        assert!(save_report(symlinked.path(), "ST-8", "x").is_err());
    }

    #[test]
    fn legacy_recovery_backs_up_repairs_and_retries_safely() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("steve.sqlite3");
        let store = Store::open(&db_path, steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let body = "# 报告\n\n".to_owned() + &"已核查的内容。".repeat(30);
        store
            .transaction(|tx| {
                tx.execute("INSERT INTO items (id,workspace_id,no,title,status,stage,created,updated) VALUES ('ST-8','default',8,'报告','done','done',1,1)", [])?;
                tx.execute("INSERT INTO timeline (item_id,ts,actor,kind,text) VALUES ('ST-8',2,'sys','sys',?)",
                    [format!("已交付，等待执行结束后交给下游\n{body}")])?;
                tx.execute("INSERT INTO artifacts (id,item_id,kind,title,location,created) VALUES ('a1','ST-8','报告','核查报告','本次交付摘要',3)", [])?;
                tx.execute("INSERT INTO artifacts (id,item_id,kind,title,location,created) VALUES ('a2','ST-8','报表','统计报表',NULL,3)", [])?;
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(recover_legacy(&store, home.path(), &db_path).unwrap(), 2);
        let path: String = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT location FROM artifacts WHERE id='a1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
        let second: String = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT location FROM artifacts WHERE id='a2'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(second, path);
        let backups: Vec<_> = fs::read_dir(home.path().join("backups")).unwrap().collect();
        assert_eq!(backups.len(), 1);
        let backup_path = backups[0].as_ref().unwrap().path();
        let backup = rusqlite::Connection::open(backup_path).unwrap();
        let old: String = backup
            .query_row("SELECT location FROM artifacts WHERE id='a1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(old, "本次交付摘要");
        drop(backup);
        assert_eq!(recover_legacy(&store, home.path(), &db_path).unwrap(), 0);
        assert_eq!(
            fs::read_dir(home.path().join("backups")).unwrap().count(),
            1
        );
    }

    #[test]
    fn legacy_recovery_refuses_symlink_backup_without_changing_rows() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let db_path = home.path().join("steve.sqlite3");
        let store = Store::open(&db_path, steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store.transaction(|tx| {
            tx.execute("INSERT INTO items (id,workspace_id,no,title,status,stage,created,updated) VALUES ('ST-8','default',8,'报告','done','done',1,1)", [])?;
            tx.execute("INSERT INTO timeline (item_id,ts,actor,kind,text) VALUES ('ST-8',2,'sys','sys',?)",
                ["已交付，等待执行结束后交给下游\n".to_owned() + &"正文".repeat(50)])?;
            tx.execute("INSERT INTO artifacts (id,item_id,kind,title,location,created) VALUES ('a1','ST-8','报告','核查报告','本次交付摘要',3)", [])?;
            Ok::<(), StoreError>(())
        }).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join("backups")).unwrap();
        assert!(recover_legacy(&store, home.path(), &db_path).is_err());
        let old: String = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT location FROM artifacts WHERE id='a1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(old, "本次交付摘要");
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_local_output_survives_workspace_cleanup() {
        let home = tempfile::tempdir().unwrap();
        let cwd = home.path().join("worktrees/ST-8");
        fs::create_dir_all(cwd.join("outputs")).unwrap();
        fs::write(cwd.join("outputs/design.md"), "# 设计\n\n正文").unwrap();
        let store =
            steve_store::Store::open(home.path().join("test.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        store.ensure_workspace().unwrap();
        let session = store
            .sessions()
            .create(&steve_store::NewSession {
                workspace_id: "default".into(),
                cwd: cwd.to_string_lossy().into_owned(),
                harness: "test".into(),
                title: "test".into(),
                token: "token".into(),
                ..Default::default()
            })
            .unwrap();
        let workspaces = Workspaces::new(store, None, None, vec![]);
        let retained = retain_location(
            home.path(),
            "ST-8",
            &session,
            "outputs/design.md",
            &workspaces,
        )
        .await
        .unwrap();
        fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(fs::read_to_string(retained).unwrap(), "# 设计\n\n正文");
    }
}
