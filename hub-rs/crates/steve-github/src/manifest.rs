//! Creating and installing the GitHub App: the three endpoints of the manifest
//! flow. All three are top-level browser steps of the signed-in user; the two GETs answer with a
//! `302` to the web app (`/#/github…`), errors as `/#/github?error=<text>` (plus `&error_code=<code>`
//! where the web acts on the error: the redirect is a top-level navigation from GitHub, so its
//! text follows the browser's language, not the one picked in Steve).
//!
//! 1. `POST /github/app/manifest {owner?}` → `{action, manifest, state}` for an auto-submitted form;
//! 2. `GET /github/app/manifest/callback?code&state` → the conversion; the App's key and secrets
//!    go to one system-vault item, its metadata to `gh_apps`;
//! 3. `GET /github/app/installed?installation_id&setup_action` → checks with the App JWT that the
//!    installation is one of this workspace's Apps, then `Connections::connect` (`github_app`).

use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::ProviderError;
use steve_modules::vault::{VaultError, VaultResult};
use steve_modules::{ApiError, Body, Caller, Modules};
use steve_store::base;
use zeroize::Zeroizing;

use crate::GitHub;
use crate::adapters::github_app::{self, CLIENT_SECRET, PRIVATE_KEY, WEBHOOK_SECRET};
use crate::api::GhError;
use crate::auth;
use crate::repo::{self, AppRow};

/// Paths relative to `/api`.
pub const MANIFEST_PATH: &str = "/github/app/manifest";
pub const CALLBACK_PATH: &str = "/github/app/manifest/callback";
pub const INSTALLED_PATH: &str = "/github/app/installed";
/// The App-level webhook (G2), at the root.
pub const HOOK_PATH: &str = "/hooks/github";

/// GitHub's limit on App names.
const APP_NAME_MAX: usize = 34;

/// A manifest flow in progress (the `state` of step 1).
#[derive(Debug, Clone)]
pub struct ManifestFlow {
    pub ws: String,
    pub user: String,
    pub owner: Option<String>,
}

/// The routes, relative to `/api`, behind the `Caller` layer.
pub fn routes(gh: GitHub) -> Router<Modules> {
    Router::new()
        .route(MANIFEST_PATH, post(start))
        .route(CALLBACK_PATH, get(callback))
        .route(INSTALLED_PATH, get(installed))
        .layer(Extension(gh))
}

/// `/#/github?error=<text>`.
pub fn error_location(msg: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(msg.as_bytes()).collect();
    format!("/#/github?error={q}")
}

/// The `error_code` of a callback that stopped because the vault can't take the App's secrets
/// (the web links to the vault and renders its own text for it).
pub const VAULT_NEEDED_CODE: &str = "vault_needed";

fn redirect(loc: String) -> Response {
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, loc),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
        ],
    )
        .into_response()
}

/// A GitHub login: letters, digits and single hyphens, at most 39 characters.
fn valid_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
}

/// The manifest (verbatim, the name cut to GitHub's 34 characters).
pub fn manifest(workspace_name: &str, public_url: &str, origin: &str) -> Value {
    let name: String = format!("Steve · {workspace_name}")
        .chars()
        .take(APP_NAME_MAX)
        .collect();
    json!({
        "name": name.trim_end(),
        "url": public_url,
        "public": false,
        "hook_attributes": {"url": format!("{public_url}{HOOK_PATH}"), "active": true},
        "redirect_url": format!("{origin}/api{CALLBACK_PATH}"),
        "setup_url": format!("{origin}/api{INSTALLED_PATH}"),
        "setup_on_update": true,
        "default_permissions": {
            "metadata": "read", "contents": "write", "pull_requests": "write", "issues": "write",
            "checks": "read", "actions": "write", "statuses": "read"
        },
        "default_events": [
            "pull_request", "pull_request_review", "check_suite", "check_run", "workflow_run",
            "issues", "issue_comment", "installation_repositories"
        ]
    })
}

/// What the manifest conversion returns. No `Debug`: it holds the key.
struct Conversion {
    id: i64,
    slug: String,
    owner: String,
    client_id: String,
    html_url: String,
    pem: Zeroizing<String>,
    webhook_secret: Zeroizing<String>,
    client_secret: Zeroizing<String>,
}

fn conversion(mut v: Value) -> Option<Conversion> {
    let mut take = |k: &str| match v.get_mut(k).map(Value::take) {
        Some(Value::String(s)) if !s.is_empty() => Some(Zeroizing::new(s)),
        _ => None,
    };
    let pem = take("pem")?;
    let webhook_secret = take("webhook_secret")?;
    let client_secret = take("client_secret")?;
    let slug = v["slug"].as_str().filter(|s| valid_login(s))?.to_string();
    Some(Conversion {
        id: v["id"].as_i64().filter(|i| *i > 0)?,
        owner: v["owner"]["login"].as_str().unwrap_or_default().to_string(),
        client_id: v["client_id"].as_str()?.to_string(),
        html_url: v["html_url"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("https://github.com/apps/{slug}")),
        slug,
        pem,
        webhook_secret,
        client_secret,
    })
}

/// The App's key and secrets go to the vault: without a usable vault the one-time code must not
/// be spent (GitHub would keep an App nobody can use).
/// The zh-CN text of `github.manifest.vault_needed`.
pub const VAULT_NEEDED: &str = "先启用保险箱，再创建 GitHub App";

impl GitHub {
    /// Whether the vault of `ws` can take the App's secrets now.
    fn vault_ready(&self, ws: &str) -> Result<(), String> {
        use steve_modules::vault::Keychain;
        match self.0.conns.vault().status(ws) {
            Ok(s) if s.initialized && s.keychain == Keychain::Ok => Ok(()),
            Ok(_) => Err(steve_i18n::t!("github.manifest.vault_needed")),
            Err(e) => {
                tracing::warn!("github manifest: vault status: {e}");
                Err(steve_i18n::t!("github.manifest.vault_needed"))
            }
        }
    }

    /// Step 1: a fresh single-use state and the form for the browser. `origin`: where the browser
    /// is (the redirect and setup URLs go back there); the webhook goes to the primary public URL.
    pub fn manifest_start(
        &self,
        who: &Caller,
        origin: &str,
        owner: Option<&str>,
    ) -> VaultResult<Value> {
        let owner = owner.map(str::trim).filter(|o| !o.is_empty());
        if owner.is_some_and(|o| !valid_login(o)) {
            return Err(ApiError::invalid("owner").into());
        }
        self.vault_ready(&who.ws).map_err(ApiError::rule)?;
        let ws_name = self
            .store()
            .reader(|c| steve_modules::contacts::repo::workspace_name(c, &who.ws))
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::not_found("工作空间"))?;
        let public = self.public_url();
        let state = self
            .0
            .flows
            .insert(
                self.now(),
                ManifestFlow {
                    ws: who.ws.clone(),
                    user: who.me.clone(),
                    owner: owner.map(String::from),
                },
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let web = &self.0.env.web_base;
        let action = match owner {
            Some(o) => format!("{web}/organizations/{o}/settings/apps/new?state={state}"),
            None => format!("{web}/settings/apps/new?state={state}"),
        };
        Ok(json!({
            "action": action,
            "manifest": manifest(&ws_name, &public, origin),
            "state": state,
        }))
    }

    /// Step 2. Returns where to send the browser.
    pub async fn manifest_callback(
        &self,
        who: Option<&Caller>,
        code: Option<&str>,
        state: Option<&str>,
    ) -> String {
        match self.convert(who, code, state).await {
            Ok(()) => "/#/github?app=created".into(),
            // same locale as the text `convert` returned, so the comparison is exact
            Err(msg) if msg == steve_i18n::t!("github.manifest.vault_needed") => {
                format!("{}&error_code={VAULT_NEEDED_CODE}", error_location(&msg))
            }
            Err(msg) => error_location(&msg),
        }
    }

    async fn convert(
        &self,
        who: Option<&Caller>,
        code: Option<&str>,
        state: Option<&str>,
    ) -> Result<(), String> {
        let expired = || steve_i18n::t!("github.manifest.expired");
        let who = who.ok_or_else(|| steve_i18n::t!("github.manifest.login_to_create"))?;
        let flow = state
            .and_then(|s| self.0.flows.take(self.now(), s))
            .ok_or_else(expired)?;
        if flow.ws != who.ws || flow.user != who.me {
            return Err(expired());
        }
        let code = code
            .filter(|c| {
                !c.is_empty()
                    && c.len() <= 200
                    && c.chars()
                        .all(|x| x.is_ascii_alphanumeric() || x == '-' || x == '_')
            })
            .ok_or_else(|| steve_i18n::t!("github.manifest.no_result"))?;
        // before the conversion: it spends the code and creates the App for good
        {
            let (gh, ws) = (self.clone(), who.ws.clone());
            tokio::task::spawn_blocking(steve_i18n::carry(move || gh.vault_ready(&ws)))
                .await
                .map_err(|_| steve_i18n::t!("github.manifest.vault_needed"))??;
        }
        let r = self
            .client()
            .request(
                reqwest::Method::POST,
                &format!("/app-manifests/{code}/conversions"),
                None,
                None,
                None,
            )
            .await
            .map_err(|e| match e {
                GhError::Status { status: 404, .. } | GhError::Status { status: 422, .. } => {
                    expired()
                }
                e => steve_i18n::t!("github.manifest.create_failed_because", error = e),
            })?;
        let conv = conversion(r.body)
            .ok_or_else(|| steve_i18n::t!("github.manifest.create_failed_unreadable"))?;
        let (ws, me) = (who.ws.clone(), who.me.clone());
        let gh = self.clone();
        let res =
            tokio::task::spawn_blocking(steve_i18n::carry(move || gh.save_app(&ws, &me, conv)))
                .await
                .map_err(|_| steve_i18n::t!("github.manifest.create_failed"))?;
        res.map_err(|e| steve_i18n::t!("github.manifest.create_failed_because", error = e))
    }

    /// The vault item first, then the `gh_apps` row; the item is destroyed again if the row fails.
    fn save_app(&self, ws: &str, me: &str, c: Conversion) -> VaultResult<()> {
        let vault = self.0.conns.vault();
        let item = vault.create_credential(
            ws,
            me,
            &format!("GitHub App {}", c.slug),
            None,
            vec![
                (PRIVATE_KEY.into(), c.pem),
                (WEBHOOK_SECRET.into(), c.webhook_secret),
                (CLIENT_SECRET.into(), c.client_secret),
            ],
        )?;
        let row = AppRow {
            id: format!("ghapp_{}", c.id),
            workspace_id: ws.into(),
            app_id: c.id,
            slug: c.slug,
            owner: c.owner,
            client_id: c.client_id,
            credential_item: item.clone(),
            html_url: c.html_url,
            created: self.now(),
        };
        let r = self.store().transaction(|tx| {
            if repo::app_by_app_id(tx, row.app_id)?.is_some() {
                return Err(
                    ApiError::conflict(steve_i18n::t!("github.manifest.app_exists")).into(),
                );
            }
            repo::insert_app(tx, &row).map_err(ApiError::from)?;
            base::audit(
                tx,
                ws,
                me,
                "github.app_create",
                &row.id,
                &json!({"app_id": row.app_id, "slug": row.slug, "owner": row.owner}),
            )
            .map_err(ApiError::from)?;
            Ok::<_, VaultError>(())
        });
        if r.is_err() {
            let d = self
                .store()
                .transaction(|tx| vault.destroy_item(tx, ws, me, &item));
            if d.is_err() {
                tracing::error!("could not destroy a GitHub App credential item");
            }
        }
        r?;
        tracing::info!(app = %row.id, "github app created");
        Ok(())
    }

    /// Step 3. Returns where to send the browser.
    pub async fn installed(
        &self,
        who: Option<&Caller>,
        installation_id: Option<&str>,
        setup_action: Option<&str>,
    ) -> String {
        match self.install(who, installation_id, setup_action).await {
            Ok(()) => "/#/github".into(),
            Err(msg) => error_location(&msg),
        }
    }

    async fn install(
        &self,
        who: Option<&Caller>,
        installation_id: Option<&str>,
        setup_action: Option<&str>,
    ) -> Result<(), String> {
        let who = who.ok_or_else(|| steve_i18n::t!("github.manifest.login_to_install"))?;
        let Some(inst) = installation_id.and_then(|s| s.trim().parse::<u64>().ok()) else {
            return Err(if setup_action == Some("request") {
                steve_i18n::t!("github.manifest.install_requested")
            } else {
                steve_i18n::t!("github.manifest.no_installation_id")
            });
        };
        let ws = who.ws.clone();
        let apps = self
            .store()
            .reader(|c| repo::apps(c, &ws).map_err(ApiError::from))
            .map_err(|e| e.to_string())?;
        if apps.is_empty() {
            return Err(steve_i18n::t!("github.manifest.no_app"));
        }
        let client = self.client();
        let mut found = None;
        for app in &apps {
            let bearer = self.app_bearer(&ws, app).await.map_err(|e| e.to_string())?;
            match auth::get_installation(&client, &bearer, inst).await {
                Ok(v) if v["app_id"].as_i64() == Some(app.app_id) => {
                    found = Some((app.clone(), v));
                    break;
                }
                Ok(_) | Err(GhError::Status { status: 404, .. }) | Err(GhError::Unauthorized) => {}
                Err(e) => return Err(steve_i18n::t!("github.manifest.check_failed", error = e)),
            }
        }
        let (app, v) =
            found.ok_or_else(|| steve_i18n::t!("github.manifest.foreign_installation"))?;
        // One installation per App for now: its account is `<slug>[bot]`, one connection per account.
        let current = self
            .store()
            .reader(|c| repo::app_connection(c, &ws, app.app_id).map_err(ApiError::from))
            .map_err(|e| e.to_string())?;
        if let Some((_, old)) = current
            && old as u64 != inst
        {
            let bearer = self
                .app_bearer(&ws, &app)
                .await
                .map_err(|e| e.to_string())?;
            match auth::get_installation(&client, &bearer, old as u64).await {
                Err(GhError::Status { status: 404, .. }) => {}
                Ok(o) => {
                    let login = match o["account"]["login"].as_str() {
                        Some(l) => l.to_string(),
                        None => steve_i18n::t!("github.manifest.another_account"),
                    };
                    return Err(steve_i18n::t!(
                        "github.manifest.already_installed",
                        login = login
                    ));
                }
                Err(e) => return Err(steve_i18n::t!("github.manifest.check_failed", error = e)),
            }
        }
        let secrets = self
            .app_secrets(&ws, &app, &[PRIVATE_KEY, WEBHOOK_SECRET])
            .await
            .map_err(|e| e.to_string())?;
        let config = json!({
            "app_id": app.app_id,
            "installation_id": inst,
            "account_login": v["account"]["login"],
            "account_type": v["target_type"].as_str().or(v["account"]["type"].as_str()),
            "api_base": self.0.env.api_base,
        });
        let done = self
            .0
            .conns
            .connect(who, github_app::ID, secrets, config, Vec::new(), None, None)
            .await
            .map_err(|e| e.to_string())?;
        self.0.etags.forget(&done.connection_id);
        tracing::info!(connection = %done.connection_id, installation = inst, reconnected = done.reconnected, "github app installed");
        Ok(())
    }

    /// `Bearer <App JWT>` signed with the key in the App's vault item.
    pub async fn app_bearer(
        &self,
        ws: &str,
        app: &AppRow,
    ) -> Result<Zeroizing<String>, ProviderError> {
        let pem = self
            .app_secrets(ws, app, &[PRIVATE_KEY])
            .await?
            .pop()
            .map(|(_, v)| v)
            .ok_or_else(|| ProviderError::Config("GitHub App 的私钥不见了".into()))?;
        auth::app_bearer(&pem, app.app_id as u64, chrono::Utc::now().timestamp())
    }

    /// Fields of the App's vault item, in the order asked (all must be there). For G2: the
    /// webhook secret of `/hooks/github`.
    pub async fn app_secrets(
        &self,
        ws: &str,
        app: &AppRow,
        keys: &[&str],
    ) -> Result<Vec<(String, Zeroizing<String>)>, ProviderError> {
        let (gh, ws, item, actor) = (
            self.clone(),
            ws.to_string(),
            app.credential_item.clone(),
            app.id.clone(),
        );
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            gh.0.conns
                .vault()
                .with_credential(&ws, &item, &actor, "GitHub App", |c| {
                    keys.iter()
                        .map(|k| {
                            c.text(k)
                                .map(|v| (k.clone(), Zeroizing::new(v.to_string())))
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .map_err(|e| ProviderError::Config(e.to_string()))?
                .ok_or_else(|| ProviderError::Config("GitHub App 的凭据不全".into()))
        })
        .await
        .map_err(|_| ProviderError::Config("任务失败".into()))?
    }
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    owner: Option<String>,
}

async fn start(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<StartBody>,
) -> VaultResult<Json<Value>> {
    let origin = gh.0.conns.origin(&headers);
    Ok(Json(gh.manifest_start(
        &who,
        &origin,
        b.owner.as_deref(),
    )?))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn callback(
    Extension(gh): Extension<GitHub>,
    who: Option<Extension<Caller>>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    redirect(
        gh.manifest_callback(
            who.as_ref().map(|w| &w.0),
            q.code.as_deref(),
            q.state.as_deref(),
        )
        .await,
    )
}

#[derive(Deserialize)]
struct InstalledQuery {
    installation_id: Option<String>,
    setup_action: Option<String>,
}

async fn installed(
    Extension(gh): Extension<GitHub>,
    who: Option<Extension<Caller>>,
    Query(q): Query<InstalledQuery>,
) -> Response {
    redirect(
        gh.installed(
            who.as_ref().map(|w| &w.0),
            q.installation_id.as_deref(),
            q.setup_action.as_deref(),
        )
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins() {
        assert!(valid_login("steve-test"));
        assert!(valid_login("a1"));
        assert!(!valid_login("-a"));
        assert!(!valid_login("a--b"));
        assert!(!valid_login("a/b"));
        assert!(!valid_login(""));
        assert!(!valid_login(&"a".repeat(40)));
    }

    #[test]
    fn manifest_shape() {
        let m = manifest(
            "一个名字很长很长很长很长很长很长很长很长很长很长的工作空间",
            "https://s.example",
            "http://o.test",
        );
        assert!(m["name"].as_str().unwrap().chars().count() <= APP_NAME_MAX);
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://s.example/hooks/github"
        );
        assert_eq!(
            m["redirect_url"],
            "http://o.test/api/github/app/manifest/callback"
        );
        assert_eq!(m["setup_url"], "http://o.test/api/github/app/installed");
        assert_eq!(m["default_permissions"]["actions"], "write");
        assert_eq!(m["default_events"].as_array().unwrap().len(), 8);
        assert_eq!(manifest("Lee", "u", "o")["name"], "Steve · Lee");
    }
}
