//! Instance-wide shared information, separate from workspace settings and client aliases.
//! v1: every authenticated workspace member can read; only the instance owner can edit.
//! The reserved settings row needs no schema change. Permission, revision and write share
//! one SQLite transaction. Identity reuses node.hub_id even with both Nodes disabled.
//!
//! `GET /api/hub-metadata` returns contract/capabilities v1, metadata, can_edit and
//! edit_reason, and the Hub's addresses (`addresses: [{url, page, kind}]`, `capabilities.addresses: 1`). `PUT` accepts only revision, display_name and purpose_note. A stale
//! revision is 409; revoked/non-owner access is 403; invalid fields are 400. No force
//! overwrite exists. Times are Unix milliseconds; updated_by is the stable users.id.
//! Names are trimmed, single-line, 1–64 Unicode scalars. Notes normalize CRLF/CR to
//! LF, retain paragraphs, allow LF/tab and at most 500 scalars (empty clears).
//! Clients treat both fields as plain text. /auth/hello declares hub_metadata: 1;
//! neither API generation nor software version implies this capability.
use super::{AppState, Ctx, blocking};
use crate::error::ApiError;
use axum::http::HeaderMap;
use axum::{Extension, Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{Read, Store, StoreError, UserRows, WS, WorkspaceSettings, now};

const KEY: &str = "hub.metadata.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Metadata {
    pub hub_id: String,
    pub display_name: String,
    pub purpose_note: String,
    pub revision: i64,
    pub updated_at: Option<i64>,
    pub updated_by: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub revision: i64,
    pub display_name: String,
    pub purpose_note: String,
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        Self::internal(e.to_string())
    }
}

fn owner(c: &mut impl Read, actor: &str) -> Result<bool, ApiError> {
    Ok(UserRows(c).role(actor)?.as_deref() == Some("owner"))
}

fn read(c: &mut impl Read, id: &str) -> Result<Metadata, ApiError> {
    let raw = WorkspaceSettings(c).get(WS, KEY)?;
    let mut m = match raw {
        Some(s) => serde_json::from_str(&s).map_err(|e| ApiError::internal(e.to_string()))?,
        None => Metadata {
            hub_id: id.into(),
            display_name: "Hub".into(),
            purpose_note: String::new(),
            revision: 0,
            updated_at: None,
            updated_by: None,
        },
    };
    // The trusted identity is authoritative, never a connection origin or Hello.name.
    m.hub_id = id.into();
    Ok(m)
}

fn normalized(b: &Change) -> Result<(String, String), ApiError> {
    let name = b.display_name.trim().to_string();
    let note = b.purpose_note.replace("\r\n", "\n").replace('\r', "\n");
    let mut bad = Vec::new();
    if !(1..=64).contains(&name.chars().count())
        || name
            .chars()
            .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        bad.push("display_name".into());
    }
    if note.chars().count() > 500
        || note
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        bad.push("purpose_note".into());
    }
    if b.revision < 0 {
        bad.push("revision".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    Ok((name, note))
}

fn snapshot(s: &Store, actor: &str) -> Result<(Metadata, bool), ApiError> {
    let id = crate::nodes::hub_id(s)?;
    s.read(|c| Ok((read(c, &id)?, owner(c, actor)?)))
}

fn update(s: &Store, actor: &str, b: &Change) -> Result<Metadata, ApiError> {
    let id = crate::nodes::hub_id(s)?;
    s.write(|w| {
        // A role observed by the request can have been revoked before this write.
        if !owner(w, actor)? {
            return Err(ApiError::Json(403, "hub_metadata_forbidden".into()));
        }
        let (name, note) = normalized(b)?;
        let old = read(w, &id)?;
        if old.revision != b.revision {
            return Err(ApiError::Json(409, "hub_metadata_conflict".into()));
        }
        let m = Metadata {
            display_name: name,
            purpose_note: note,
            revision: old
                .revision
                .checked_add(1)
                .ok_or_else(|| ApiError::internal("revision exhausted"))?,
            updated_at: Some(now()),
            updated_by: Some(actor.into()),
            ..old
        };
        let value = serde_json::to_string(&m).map_err(StoreError::from)?;
        WorkspaceSettings(w).set(WS, KEY, &value, now())?;
        Ok(m)
    })
}

fn store(st: &AppState) -> Result<Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "hub_metadata_unavailable".into()))
}

fn reply(m: Metadata, can_edit: bool) -> Value {
    json!({"contract": 1, "capabilities": {"shared_metadata": 1}, "metadata": m,
        "can_edit": can_edit, "edit_reason": if can_edit { None } else { Some("owner_required") },
        // No trustworthy device mapping is available from the instance identity alone.
        "device_id": null})
}

/// `reply` with the Hub's addresses (G12, `capabilities.addresses`): `[{url, page, kind}]`, the public one
/// first, then `lan` / `tailnet`, then `loopback`.
fn with_addresses(mut v: Value, st: &AppState, headers: &HeaderMap) -> Value {
    v["capabilities"]["addresses"] = json!(1);
    v["addresses"] = json!(crate::access::hub_addresses(&st.cfg, headers));
    v
}

pub async fn get(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let s = store(&st)?;
    let v = blocking(move || {
        let (m, edit) = snapshot(&s, &cx.me)?;
        Ok(reply(m, edit))
    })
    .await?;
    Ok(Json(with_addresses(v, &st, &headers)))
}

pub async fn put(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    headers: HeaderMap,
    Json(b): Json<Change>,
) -> Result<Json<Value>, ApiError> {
    let s = store(&st)?;
    let v = blocking(move || {
        let m = update(&s, &cx.me, &b)?;
        Ok(reply(m, true))
    })
    .await?;
    Ok(Json(with_addresses(v, &st, &headers)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        s.write::<_, StoreError>(|w| {
            steve_store::Workspaces(w).insert(WS, "Test", 0)?;
            for (id, role) in [("a", "owner"), ("b", "member")] {
                UserRows(w).insert(id, WS, id, id, role, 0)?;
            }
            Ok(())
        })
        .unwrap();
        (d, s)
    }
    fn change(revision: i64, name: &str, note: &str) -> Change {
        Change {
            revision,
            display_name: name.into(),
            purpose_note: note.into(),
        }
    }
    #[test]
    fn persistence_identity_permissions_and_clear() {
        let (d, s) = fixture();
        let (first, edit) = snapshot(&s, "a").unwrap();
        assert!(edit);
        assert_eq!(first.hub_id, crate::nodes::hub_id(&s).unwrap());
        assert!(!snapshot(&s, "b").unwrap().1);
        assert!(matches!(
            update(&s, "b", &change(0, "Bad", "")),
            Err(ApiError::Json(403, _))
        ));
        let saved = update(
            &s,
            "a",
            &change(0, "  团队😀  ", "<b>text</b>\r\nnext\rlast"),
        )
        .unwrap();
        assert_eq!(saved.display_name, "团队😀");
        assert_eq!(saved.purpose_note, "<b>text</b>\nnext\nlast");
        assert_eq!(saved.updated_by.as_deref(), Some("a"));
        assert!(saved.updated_at.unwrap() > 0);
        let reopened =
            Store::open(d.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        assert_eq!(snapshot(&reopened, "b").unwrap().0, saved);
        assert!(update(&s, "a", &change(1, "", "")).is_err());
        assert_eq!(snapshot(&s, "a").unwrap().0, saved);
        let cleared = update(&reopened, "a", &change(1, "Hub", "")).unwrap();
        assert!(cleared.purpose_note.is_empty());
        assert_eq!(cleared.hub_id, first.hub_id);
        steve_store::testkit::hub::set_user_role(&s, "a", "member").unwrap();
        assert!(matches!(
            update(&s, "a", &change(2, "Revoked", "")),
            Err(ApiError::Json(403, _))
        ));
    }
    #[test]
    fn unicode_limits_and_controls() {
        assert!(normalized(&change(0, &"😀".repeat(64), &"😀".repeat(500))).is_ok());
        for name in [" ".into(), "😀".repeat(65), "a\nb".into(), "a\0b".into()] {
            assert!(normalized(&change(0, &name, "")).is_err());
        }
        assert!(normalized(&change(0, "a", &"😀".repeat(501))).is_err());
        assert!(normalized(&change(0, "a", "\0")).is_err());
        assert_eq!(normalized(&change(0, "a", "\r\n")).unwrap().1, "\n");
    }
    #[test]
    fn concurrent_cas_across_connections_and_failed_commit() {
        let (d, s) = fixture();
        let other = Store::open(d.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [s.clone(), other]
            .into_iter()
            .map(|s| {
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    update(&s, "a", &change(0, "New", ""))
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(ApiError::Json(409, _))))
                .count(),
            1
        );
        steve_store::testkit::hub::refuse_metadata_updates(&s).unwrap();
        assert!(update(&s, "a", &change(1, "Failed", "must not persist")).is_err());
        assert_eq!(snapshot(&s, "a").unwrap().0.revision, 1);
        assert_eq!(snapshot(&s, "a").unwrap().0.display_name, "New");
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new().route(
        "/hub-metadata",
        get(hub_metadata::get).put(hub_metadata::put),
    )
}
