//! Durable, non-Git deliverables in an item's working directory. The Git worktree may be
//! removed after a merge; `<home>/workspaces/<item>/outputs` is not part of that cleanup.

use std::io;
use std::path::{Component, Path, PathBuf};

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

/// Rejects paths inside dependency, build or embedded checkout trees that should never be copied into outputs/.
pub fn polluted_output_path(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | "node_modules" | "target" | "review-source" | ".next" | "dist")
        )
    })
}
