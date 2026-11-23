//! What a run would be given under the path model (S2, in shadow): one `view()` for every kind of
//! run, made from its principal ([`super::principal`]) and its work. For an item's session: the
//! session's and the item's directories read-write (a directory project's root included), and the
//! knowledge folders of its workspace that its principal may see read-only. Nothing else of the
//! machine; Steve's own state, credentials and caches come with later steps.
//!
//! It decides nothing yet. When a session is bound it is recorded with how it differs from what the
//! local Node lets every run reach today (every workspace's projects, sessions and folders): the
//! difference is what the deny-by-default sandbox will take away, and the counts say how much.

use std::path::Path;

use serde::Serialize;
use sha2::Digest as _;
use steve_store::{Read, StoreError, W, node_sessions::authority};

use steve_store::base::{Viewer, Visibility};

use super::principal;

/// How many paths of a difference are kept as a sample (the count is of all of them).
const SAMPLE: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub principal: String,
    pub audience: Vec<String>,
    /// Sorted, deduplicated, absolute.
    pub rw: Vec<String>,
    pub ro: Vec<String>,
}

impl View {
    /// A digest of exactly this view (its JSON; the lists are sorted).
    pub fn digest(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(sha2::Sha256::digest(json))
    }
}

fn clean(mut v: Vec<String>) -> Vec<String> {
    v.retain(|p| Path::new(p).is_absolute());
    for p in &mut v {
        while p.len() > 1 && p.ends_with('/') {
            p.pop();
        }
    }
    v.sort();
    v.dedup();
    v
}

/// The view of `session` (an item's or a plain session's); `None`: no such session.
pub fn of_session(r: &mut impl Read, session: &str) -> Result<Option<View>, StoreError> {
    let Some(s) = authority::session(r, session)? else {
        return Ok(None);
    };
    let Some((_, p)) = principal::of_session(r, session)? else {
        return Ok(None);
    };
    let mut rw = vec![s.cwd];
    if let Some(item) = s.item
        && let Some(item) = authority::item(r, &item)?
    {
        rw.extend(item.cwd);
        if item.worktree == Some(false) {
            rw.extend(item.project_path);
        }
    }
    let viewer = Viewer::bare(&s.workspace, p.user().unwrap_or(""));
    let ro = authority::knowledge_folders(r, &s.workspace)?
        .into_iter()
        .filter(|folder| viewer.sees(&Visibility::from_db(&folder.visibility), &folder.creator))
        .map(|folder| folder.path)
        .collect();
    let audience = match s.audience {
        Some(a) => a,
        None => principal::audience(r, &s.workspace, &p, None)?,
    };
    let rw = clean(rw);
    let mut ro = clean(ro);
    ro.retain(|r| !rw.contains(r));
    Ok(Some(View {
        principal: p.key(),
        audience,
        rw,
        ro,
    }))
}

/// `path` lies in one of `dirs` (or is one).
fn within(path: &str, dirs: &[String]) -> bool {
    dirs.iter()
        .any(|d| Path::new(path).starts_with(Path::new(d)))
}

/// How `view` differs from `today` (every directory the Node lets a run reach now): today's outside
/// the view, and the view's outside today's. Each sorted; the second only counts paths, not
/// whether they exist.
pub fn diff(view: &View, today: &[String]) -> (Vec<String>, Vec<String>) {
    let mine: Vec<String> = view.rw.iter().chain(&view.ro).cloned().collect();
    let today = clean(today.to_vec());
    let extra = today
        .iter()
        .filter(|t| !within(t, &mine))
        .cloned()
        .collect();
    let missing = mine
        .iter()
        .filter(|m| !within(m, &today))
        .cloned()
        .collect();
    (extra, missing)
}

/// Records `session`'s view on `node`, with its difference from `today` (`None`: today's roots are
/// not known here, a remote Node's are its own).
pub fn record_in(
    w: &mut W<'_>,
    session: &str,
    node: &str,
    today: Option<&[String]>,
) -> Result<(), StoreError> {
    let Some(view) = of_session(w, session)? else {
        return Ok(());
    };
    let diff = today.map(|today| diff(&view, today));
    let sample = |paths: &[String]| paths.len().min(SAMPLE);
    let extra = diff.as_ref().map(|(extra, _)| authority::PathDifference {
        total: extra.len(),
        sample: &extra[..sample(extra)],
    });
    let missing = diff.as_ref().map(|(_, missing)| authority::PathDifference {
        total: missing.len(),
        sample: &missing[..sample(missing)],
    });
    authority::record_view(
        w,
        authority::ViewRecord {
            session,
            node,
            at: steve_store::now(),
            digest: &view.digest(),
            view: &view,
            extra,
            missing,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use steve_store::{NoPublisher, Store, testkit::hub::authority as fixture};

    fn db() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
        fixture::views(&store).unwrap();
        (dir, store)
    }

    #[test]
    fn an_items_session_sees_its_work_and_what_its_principal_may_read() {
        let (_dir, store) = db();
        let v = store.read(|r| of_session(r, "s1")).unwrap().unwrap();
        assert_eq!(v.principal, "u2");
        assert_eq!(v.audience, ["workspace:team"]);
        // a git project's item works in its own worktree, not the project
        assert_eq!(v.rw, ["/h/worktrees/ST-1"]);
        // Single-user reads include other creators' private/team folders, only in this workspace.
        let folders = [
            "/docs/shared",
            "/docs/team-t1",
            "/docs/u1-private",
            "/docs/u2-private",
        ];
        assert_eq!(v.ro, folders);
        let v = store.read(|r| of_session(r, "s2")).unwrap().unwrap();
        assert_eq!(v.principal, "svc:team");
        assert_eq!(v.rw, ["/w/dir-proj"], "a directory project's root, once");
        assert_eq!(
            v.ro,
            ["/docs/shared"],
            "service audiences cannot read private or team folders"
        );
        // Changing the requester changes identity, not member read visibility.
        fixture::set_requester(&store, "i2", "u1").unwrap();
        let v = store.read(|r| of_session(r, "s2")).unwrap().unwrap();
        assert_eq!(v.principal, "u1");
        assert_eq!(v.ro, folders);
        assert_ne!(
            v.digest(),
            store
                .read(|r| of_session(r, "s1"))
                .unwrap()
                .unwrap()
                .digest()
        );
    }

    #[test]
    fn the_shadow_log_names_what_the_view_would_take_away() {
        let (_dir, store) = db();
        let today: Vec<String> = [
            "/w/git-proj",
            "/w/other-proj",
            "/h/worktrees/ST-1",
            "/h/worktrees",
            "/docs/shared",
            "/docs/elsewhere",
        ]
        .map(String::from)
        .to_vec();
        store
            .write(|w| record_in(w, "s1", "local", Some(&today)))
            .unwrap();
        let row = fixture::shadow(&store, "s1").unwrap();
        assert_eq!(row.extra_count, Some(4));
        let extra = row.extra.unwrap();
        assert_eq!(
            extra,
            r#"["/docs/elsewhere","/h/worktrees","/w/git-proj","/w/other-proj"]"#
        );
        // All same-workspace private/team folders are readable but absent from today's roots.
        assert_eq!(
            (row.missing_count.unwrap(), row.missing.as_deref().unwrap()),
            (
                3,
                r#"["/docs/team-t1","/docs/u1-private","/docs/u2-private"]"#
            )
        );
        assert_eq!(row.session_digest, row.digest);
        store
            .write(|w| record_in(w, "s2", "n_remote", None))
            .unwrap();
        assert_eq!(fixture::shadow(&store, "s2").unwrap().extra_count, None);
    }
}
