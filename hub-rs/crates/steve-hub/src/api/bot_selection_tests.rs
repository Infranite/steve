//! Real REST handlers and SQLite writes, with a controllable Node catalogue and no harness process.
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use axum::routing::{patch, post};
use serde_json::{Value, json};
use steve_core::harnesses::{Harness, HarnessModel};
use tower::ServiceExt;

use super::*;
use crate::backend::{BResult, Domain};
use crate::bus::Bus;
use crate::config::Config;
use crate::wire::store::StoreBackend;

struct Catalogue(RwLock<Vec<Harness>>);

#[async_trait::async_trait]
impl Domain for Catalogue {
    async fn harnesses(&self, fresh: bool) -> BResult<Vec<Harness>> {
        assert!(!fresh, "saving configuration must not start a probe");
        Ok(self.0.read().unwrap().clone())
    }
    async fn claude_print(
        &self,
        _: &'static str,
        _: &str,
        _: &str,
        _: crate::backend::Tier,
        _: u64,
    ) -> Result<String, crate::error::ApiError> {
        Ok(json!({"name":"Draft", "model":"claude-opus-5-5"}).to_string())
    }
}

struct Env {
    _dir: tempfile::TempDir,
    store: steve_store::Store,
    app: Router,
    catalogue: Arc<Catalogue>,
    me: String,
}

fn reported(id: &str, models: &[&str], efforts: &[&str]) -> Harness {
    let mut h = Harness::declared(id, None);
    h.ready = Some(true);
    h.models = models
        .iter()
        .map(|id| HarnessModel {
            id: id.to_string(),
            name: None,
            description: None,
        })
        .collect();
    h.efforts = efforts.iter().map(|s| s.to_string()).collect();
    h
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();
        let cfg = Arc::new(
            Config::from_vars(&|k: &str| match k {
                "HOME" | "STEVE_HOME" => Some(home.clone()),
                "STEVE_VAULT_KEYSTORE" => Some("file".into()),
                _ => None,
            })
            .unwrap(),
        );
        let store = steve_store::Store::open(
            dir.path().join("steve.sqlite3"),
            steve_store::FnPublisher(|_| {}),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        let me = store
            .users()
            .create("test-owner", "Owner", None, "owner")
            .unwrap();
        let catalogue = Arc::new(Catalogue(RwLock::new(vec![
            reported(
                "claude-code",
                &["claude-opus-5-5", "us.anthropic.claude-x-v1:0"],
                &["low", "high", "max"],
            ),
            reported(
                "codex",
                &["gpt-6.1-sol", "route/gpt-6.1-sol[1m]"],
                &["low", "high", "ultra"],
            ),
        ])));
        let st = AppState::new(
            cfg,
            Arc::new(StoreBackend(store.clone())),
            catalogue.clone(),
            Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
                Default::default(),
            ))),
        );
        let ctx = Ctx {
            user: Obj::new(),
            ws: "default".into(),
            me: me.clone(),
            client: None,
        };
        let app = Router::new()
            .route("/bots", post(create))
            .route("/bots/draft", post(draft))
            .route("/bots/{id}", patch(patch_one))
            .route("/bots/{id}/duplicate", post(duplicate))
            .layer(Extension(ctx))
            .with_state(st);
        Self {
            _dir: dir,
            store,
            app,
            catalogue,
            me,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status().as_u16();
        let body = to_bytes(r.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn create(&self, fields: Value) -> String {
        let (status, out) = self.call("POST", "/bots", fields).await;
        assert_eq!(status, 200, "{out}");
        out["id"].as_str().unwrap().to_string()
    }

    fn bot(&self, id: &str) -> Value {
        serde_json::to_value(self.store.bots().get(id).unwrap().unwrap()).unwrap()
    }
}

#[tokio::test]
async fn create_rejects_wrong_harness_model_and_effort_without_writing() {
    let e = Env::new();
    for bad in [
        json!({"name":"bad-model","harness":"codex","model":"claude-opus-5-5"}),
        json!({"name":"bad-effort","harness":"claude-code","reasoning_effort":"ultra"}),
        json!({"name":"made-up","harness":"codex","model":"no-such-model"}),
        json!({"name":"alias","harness":"codex","model":"gpt 6.1 sol"}),
    ] {
        let before = e.store.bots().list(None).unwrap().len();
        let (status, out) = e.call("POST", "/bots", bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {out}");
        assert_eq!(e.store.bots().list(None).unwrap().len(), before);
    }
}

#[tokio::test]
async fn real_ids_reported_efforts_and_null_defaults_can_be_saved() {
    let e = Env::new();
    for model in ["gpt-6.1-sol", "route/gpt-6.1-sol[1m]"] {
        let id = e
            .create(
                json!({"name":model,"harness":"codex","model":model,"reasoning_effort":"ultra"}),
            )
            .await;
        assert_eq!(e.bot(&id)["model"], model);
        assert_eq!(e.bot(&id)["reasoning_effort"], "ultra");
        let (status, out) = e
            .call(
                "PATCH",
                &format!("/bots/{id}"),
                json!({"model":null,"reasoning_effort":null}),
            )
            .await;
        assert_eq!(status, 200, "{out}");
        assert!(e.bot(&id)["model"].is_null());
        assert!(e.bot(&id)["reasoning_effort"].is_null());
    }
    e.create(json!({"name":"default","model":null,"reasoning_effort":null}))
        .await;
    e.create(json!({"name":"provider-id","model":"us.anthropic.claude-x-v1:0"}))
        .await;
}

#[tokio::test]
async fn harness_only_patch_rechecks_inherited_model_and_effort_atomically() {
    let e = Env::new();
    for fields in [
        json!({"name":"model","model":"claude-opus-5-5","role":"keep"}),
        json!({"name":"effort","model":null,"reasoning_effort":"max","role":"keep"}),
    ] {
        let id = e.create(fields).await;
        let before = e.bot(&id);
        let (status, out) = e
            .call(
                "PATCH",
                &format!("/bots/{id}"),
                json!({"harness":"codex","role":"must not write"}),
            )
            .await;
        assert_eq!(status, 400, "{out}");
        assert_eq!(e.bot(&id), before);
        let (status, out) = e
            .call(
                "PATCH",
                &format!("/bots/{id}"),
                json!({"harness":"codex","model":null,"reasoning_effort":null}),
            )
            .await;
        assert_eq!(status, 200, "{out}");
        let after = e.bot(&id);
        assert_eq!(after["harness"], "codex");
        assert_eq!(after["role"], "keep");
        assert!(after["model"].is_null() && after["reasoning_effort"].is_null());
    }
}

#[tokio::test]
async fn partial_edits_preserve_legacy_values_and_other_bot_fields() {
    let e = Env::new();
    let id = e.create(json!({"name":"legacy","harness":"codex","model":"gpt-6.1-sol","reasoning_effort":"low","rules":"keep"})).await;
    e.store
        .bots()
        .patch(
            &id,
            &steve_store::BotPatch {
                model: Some(Some("claude-opus-5-5".into())),
                reasoning_effort: Some(Some("max".into())),
                ..Default::default()
            },
            "me",
        )
        .unwrap();
    let before = e.bot(&id);
    let path = format!("/bots/{id}");
    let (status, out) = e.call("PATCH", &path, json!({"role":"new note"})).await;
    assert_eq!(status, 200, "{out}");
    for key in [
        "harness",
        "model",
        "reasoning_effort",
        "rules",
        "perm_mode",
        "enabled",
        "avatar",
    ] {
        assert_eq!(e.bot(&id)[key], before[key], "{key}");
    }
    // Explicitly resaving an invalid value is refused; fixing one field need not rewrite another.
    for bad in [
        json!({"model":"claude-opus-5-5"}),
        json!({"reasoning_effort":"max"}),
    ] {
        let before = e.bot(&id);
        let (status, out) = e.call("PATCH", &path, bad).await;
        assert_eq!(status, 400, "{out}");
        assert_eq!(e.bot(&id), before);
    }
    let (status, out) = e.call("PATCH", &path, json!({"model":"gpt-6.1-sol"})).await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(e.bot(&id)["reasoning_effort"], "max");
}

#[tokio::test]
async fn unknown_or_failed_probe_is_not_an_empty_supported_list() {
    let e = Env::new();
    for ready in [None, Some(false)] {
        for h in e.catalogue.0.write().unwrap().iter_mut() {
            h.ready = ready;
            h.models.clear();
            h.efforts.clear();
        }
        let (status, saved) = e.call("POST", "/bots", json!({"name":format!("unknown-{ready:?}"),"harness":"codex","model":"future-model","reasoning_effort":"future-effort"})).await;
        assert_eq!(status, 200);
        assert_eq!(saved["selection_validation"], "unknown");
        let (status, out) = e
            .call(
                "POST",
                "/bots",
                json!({"name":"bad-format","model":"not an id"}),
            )
            .await;
        assert_eq!(status, 400, "{out}");
    }
    // A successful probe with no choices is different: only defaults can be selected.
    for h in e.catalogue.0.write().unwrap().iter_mut() {
        h.ready = Some(true);
    }
    for bad in [
        json!({"name":"empty-models","model":"future-model"}),
        json!({"name":"empty-efforts","reasoning_effort":"high"}),
    ] {
        let (status, out) = e.call("POST", "/bots", bad).await;
        assert_eq!(status, 400, "{out}");
    }
    e.create(json!({"name":"empty-default","model":null,"reasoning_effort":null}))
        .await;
}

#[tokio::test]
async fn legacy_draft_and_duplicate_cannot_bypass_selection_validation() {
    let e = Env::new();
    let id = e
        .create(json!({"name":"copy","model":"claude-opus-5-5"}))
        .await;
    let (status, out) = e
        .call("POST", &format!("/bots/{id}/duplicate"), json!({}))
        .await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["model"], "claude-opus-5-5");
    assert_eq!(out["enabled"], false);
    let (status, out) = e
        .call("POST", "/bots/draft", json!({"desc":"create a helper"}))
        .await;
    assert_eq!(status, 200, "{out}");
    e.catalogue.0.write().unwrap()[0].models.clear();
    let before = e.store.bots().list(None).unwrap().len();
    for path in [format!("/bots/{id}/duplicate"), "/bots/draft".into()] {
        let (status, out) = e
            .call("POST", &path, json!({"desc":"create a helper"}))
            .await;
        assert_eq!(status, 400, "{out}");
        assert_eq!(e.store.bots().list(None).unwrap().len(), before);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_harness_and_model_edits_never_commit_an_invalid_pair() {
    let e = Env::new();
    for h in e.catalogue.0.write().unwrap().iter_mut() {
        h.models.push(HarnessModel {
            id: "shared-model".into(),
            name: None,
            description: None,
        });
    }
    for i in 0..8 {
        let id = e
            .create(
                json!({"name":format!("race-{i}"),"model":"shared-model","reasoning_effort":"low"}),
            )
            .await;
        let path = format!("/bots/{id}");
        let (model, harness) = tokio::join!(
            e.call("PATCH", &path, json!({"model":"claude-opus-5-5"})),
            e.call("PATCH", &path, json!({"harness":"codex"})),
        );
        assert!(
            (model.0 == 200 && harness.0 == 400) || (model.0 == 400 && harness.0 == 200),
            "{model:?}, {harness:?}"
        );
        let after = e.bot(&id);
        assert!(
            (after["harness"] == "codex" && after["model"] == "shared-model")
                || (after["harness"] == "claude-code" && after["model"] == "claude-opus-5-5"),
            "{after}"
        );
    }
    // A compatible inherited selection stays exactly as it was.
    let id = e.create(json!({"name":"compatible","model":"shared-model","reasoning_effort":"low","rules":"keep"})).await;
    let (status, out) = e
        .call("PATCH", &format!("/bots/{id}"), json!({"harness":"codex"}))
        .await;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["model"], "shared-model");
    assert_eq!(out["reasoning_effort"], "low");
    assert_eq!(out["rules"], "keep");
}

#[tokio::test]
async fn a_partially_unknown_target_set_does_not_certify_an_accepted_save() {
    let e = Env::new();
    e.catalogue.0.write().unwrap()[1].selection_unknown = true;
    let (status, saved) = e
        .call(
            "POST",
            "/bots",
            json!({"name":"partial-evidence","harness":"codex","model":"gpt-6.1-sol"}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(saved["selection_validation"], "unknown");
    let (status, _) = e
        .call(
            "POST",
            "/bots",
            json!({"name":"known-negative","harness":"codex","model":"not-offered"}),
        )
        .await;
    assert_eq!(
        status, 400,
        "unknown targets do not erase another target's known incompatibility"
    );
}

#[tokio::test]
async fn create_draft_and_duplicate_all_adopt_creator_as_steward() {
    let e = Env::new();
    let mut owner = steve_modules::contacts::repo::NewParty::new("contact", "Owner", &e.me);
    owner.user_id = Some(e.me.clone());
    let me = e
        .store
        .write(|w| steve_modules::contacts::repo::insert(w, "default", &owner))
        .unwrap();
    let created = e.create(json!({"name": "Created"})).await;
    let (s, dup) = e
        .call("POST", &format!("/bots/{created}/duplicate"), json!({}))
        .await;
    assert_eq!(s, 200, "{dup}");
    let duplicated = dup["id"].as_str().unwrap().to_string();
    let (s, dr) = e
        .call("POST", "/bots/draft", json!({"desc": "create a helper"}))
        .await;
    assert_eq!(s, 200, "{dr}");
    let drafted = dr["id"].as_str().unwrap().to_string();

    for (kind, id) in [
        ("create", created),
        ("duplicate", duplicated),
        ("draft", drafted),
    ] {
        let stewards = e
            .store
            .read(|r| steve_store::Stewards(r).of_object("default", "bot", &id))
            .unwrap();
        assert_eq!(
            stewards.first().map(|s| s.person_id.as_str()),
            Some(me.id.as_str()),
            "{kind} ({id}) did not adopt creator as steward: {stewards:?}"
        );
    }
}
