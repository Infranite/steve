//! The app registration endpoint (`/oauth/v1/app/registration`; Lark's under `/lark`), scripted:
//! `begin` hands out a device code (or a scripted answer), each `poll` answers the next scripted
//! reply (`authorization_pending` when the script is empty). Every call is recorded.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::Form;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

use super::{FakeFeishu, Shared};

/// Registration state of the fake.
#[derive(Debug, Default)]
pub(crate) struct RegFake {
    /// `(status, body)` for the next begins; empty = a fresh device code.
    pub begin_answers: VecDeque<(u16, Value)>,
    /// `(status, body)` for the next polls; empty = `authorization_pending`.
    pub poll_answers: VecDeque<(u16, Value)>,
    pub poll_delay: Duration,
    /// `(brand host, form)` of every call, in order.
    pub calls: Vec<(String, HashMap<String, String>)>,
    pub seq: u64,
}

pub(crate) async fn handle(
    State(sh): State<Arc<Shared>>,
    OriginalUri(uri): OriginalUri,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let brand = if uri.path().starts_with("/lark/") {
        "lark"
    } else {
        "feishu"
    };
    let action = form.get("action").cloned().unwrap_or_default();
    let (answer, delay) = {
        let mut rt = sh.rt();
        let r = &mut rt.reg;
        r.calls.push((brand.into(), form.clone()));
        match action.as_str() {
            "init" => (
                (200, json!({"supported_auth_methods": ["client_secret"]})),
                Duration::ZERO,
            ),
            "begin" => {
                r.seq += 1;
                let n = r.seq;
                let a = r.begin_answers.pop_front().unwrap_or_else(|| {
                    (
                        200,
                        json!({
                            "device_code": format!("dc-{n}"),
                            "user_code": format!("UC-{n}"),
                            "verification_uri": "/page/cli",
                            "expire_in": 600,
                            "interval": 1,
                        }),
                    )
                });
                (a, Duration::ZERO)
            }
            "poll" => (
                r.poll_answers
                    .pop_front()
                    .unwrap_or((400, json!({"error": "authorization_pending"}))),
                r.poll_delay,
            ),
            _ => ((400, json!({"error": "invalid_request"})), Duration::ZERO),
        }
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let (status, body) = answer;
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        axum::Json(body),
    )
        .into_response()
}

impl FakeFeishu {
    /// Queues answers for the next begins.
    pub fn reg_begin(&self, status: u16, body: Value) {
        self.shared.rt().reg.begin_answers.push_back((status, body));
    }
    /// Queues answers for the next polls (status 400 for an `error`, as the provider does).
    pub fn reg_polls(&self, answers: Vec<Value>) {
        let mut rt = self.shared.rt();
        for a in answers {
            let status = if a.get("error").is_some() { 400 } else { 200 };
            rt.reg.poll_answers.push_back((status, a));
        }
    }
    /// A poll answer with this fake's own app credentials (they pass the bot check).
    pub fn reg_issued(&self, tenant_brand: &str) -> Value {
        let app = self.app();
        json!({"client_id": app.app_id, "client_secret": app.app_secret,
               "user_info": {"open_id": "ou_registrant", "tenant_brand": tenant_brand}})
    }
    /// Delays every poll answer.
    pub fn reg_poll_delay(&self, d: Duration) {
        self.shared.rt().reg.poll_delay = d;
    }
    /// `(brand, action)` of every registration call so far.
    pub fn reg_calls(&self) -> Vec<(String, String)> {
        self.shared
            .rt()
            .reg
            .calls
            .iter()
            .map(|(b, f)| (b.clone(), f.get("action").cloned().unwrap_or_default()))
            .collect()
    }
    /// The forms of every call (to check what was sent).
    pub fn reg_forms(&self) -> Vec<HashMap<String, String>> {
        self.shared
            .rt()
            .reg
            .calls
            .iter()
            .map(|(_, f)| f.clone())
            .collect()
    }
}
