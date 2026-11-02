//! Sends webhook deliveries the way GitHub does: JSON body signed with
//! `X-Hub-Signature-256`, plus the `X-GitHub-*` headers documented at
//! <https://docs.github.com/en/webhooks/webhook-events-and-payloads#delivery-headers>.

use std::sync::Mutex;

use rand::RngExt;
use serde_json::Value;

use super::FakeError;
use crate::signature;

/// One delivery as sent (kept so it can be redelivered byte for byte).
#[derive(Debug, Clone)]
pub struct Delivery {
    /// `X-GitHub-Delivery`; redeliveries reuse it, as GitHub does.
    pub id: String,
    pub event: String,
    pub body: Vec<u8>,
    pub signature: String,
    /// Status the receiver answered with.
    pub status: u16,
    pub response: String,
}

/// Signs and POSTs deliveries to one receiver URL.
#[derive(Debug)]
pub struct WebhookSender {
    target: String,
    secret: Vec<u8>,
    hook_id: u64,
    /// `integration` for App webhooks, `repository` for repo hooks.
    target_type: String,
    target_id: u64,
    client: reqwest::Client,
    sent: Mutex<Vec<Delivery>>,
}

impl WebhookSender {
    /// A sender for App webhooks (`X-GitHub-Hook-Installation-Target-Type: integration`).
    pub fn new(target: impl Into<String>, secret: impl Into<Vec<u8>>) -> Self {
        Self::with_target(target, secret, 4001, "integration", 1001)
    }

    /// A sender for a specific hook (repository hooks use `repository` and the repo id).
    pub fn with_target(
        target: impl Into<String>,
        secret: impl Into<Vec<u8>>,
        hook_id: u64,
        target_type: &str,
        target_id: u64,
    ) -> Self {
        WebhookSender {
            target: target.into(),
            secret: secret.into(),
            hook_id,
            target_type: target_type.to_string(),
            target_id,
            client: reqwest::Client::new(),
            sent: Mutex::new(Vec::new()),
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    /// Signs and sends `payload` as event `event` with a fresh delivery id.
    pub async fn send(&self, event: &str, payload: &Value) -> Result<Delivery, FakeError> {
        let body = serde_json::to_vec(payload).map_err(|e| FakeError::Fixture(e.to_string()))?;
        self.send_raw(event, body, new_guid(), None).await
    }

    /// Sends `body` as is, optionally with a forged signature header (for
    /// testing that the receiver rejects it).
    pub async fn send_raw(
        &self,
        event: &str,
        body: Vec<u8>,
        delivery_id: String,
        signature_override: Option<String>,
    ) -> Result<Delivery, FakeError> {
        let signature = signature_override.unwrap_or_else(|| signature::sign(&self.secret, &body));
        let resp = self
            .client
            .post(&self.target)
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                format!("GitHub-Hookshot/{:07x}", self.hook_id),
            )
            .header("X-GitHub-Event", event)
            .header("X-GitHub-Delivery", &delivery_id)
            .header("X-GitHub-Hook-ID", self.hook_id.to_string())
            .header("X-GitHub-Hook-Installation-Target-Type", &self.target_type)
            .header(
                "X-GitHub-Hook-Installation-Target-ID",
                self.target_id.to_string(),
            )
            .header(signature::SIGNATURE_HEADER, &signature)
            .body(body.clone())
            .send()
            .await
            .map_err(|e| FakeError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let response = resp.text().await.unwrap_or_default();
        let d = Delivery {
            id: delivery_id,
            event: event.to_string(),
            body,
            signature,
            status,
            response,
        };
        self.lock().push(d.clone());
        Ok(d)
    }

    /// Sends an earlier delivery again: same id, same bytes, same signature.
    pub async fn redeliver(&self, delivery_id: &str) -> Result<Delivery, FakeError> {
        let d = self
            .lock()
            .iter()
            .find(|d| d.id == delivery_id)
            .cloned()
            .ok_or_else(|| FakeError::NotFound(format!("delivery {delivery_id}")))?;
        self.send_raw(&d.event, d.body, d.id, Some(d.signature))
            .await
    }

    /// Everything sent so far, oldest first.
    pub fn sent(&self) -> Vec<Delivery> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Delivery>> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A GUID like GitHub's delivery ids (`72d3162e-cc78-11e3-81ab-4c9367dc0958`).
pub fn new_guid() -> String {
    let b: [u8; 16] = rand::rng().random();
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

macro_rules! fixtures {
    ($($name:literal => $event:literal),* $(,)?) => {
        /// Recorded payloads under `tests/fixtures/github/`: (file stem, `X-GitHub-Event`, JSON).
        pub const FIXTURES: &[(&str, &str, &str)] = &[
            $(($name, $event, include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/github/", $name, ".json")))),*
        ];
    };
}

fixtures! {
    "ping" => "ping",
    "installation.created" => "installation",
    "installation_repositories.removed" => "installation_repositories",
    "push" => "push",
    "pull_request.opened" => "pull_request",
    "pull_request.synchronize" => "pull_request",
    "pull_request.closed.merged" => "pull_request",
    "pull_request.review_requested" => "pull_request",
    "pull_request_review.submitted" => "pull_request_review",
    "check_suite.completed.success" => "check_suite",
    "check_suite.completed.failure" => "check_suite",
    "check_run.completed.success" => "check_run",
    "check_run.completed.failure" => "check_run",
    "workflow_run.completed.failure" => "workflow_run",
    "issues.opened" => "issues",
    "issue_comment.created" => "issue_comment",
}

/// A recorded payload by file stem (e.g. `pull_request.opened`): `(event, payload)`.
pub fn fixture(name: &str) -> Result<(&'static str, Value), FakeError> {
    let (_, event, raw) = FIXTURES
        .iter()
        .find(|(n, _, _)| *n == name)
        .ok_or_else(|| FakeError::NotFound(format!("fixture {name}")))?;
    let v = serde_json::from_str(raw).map_err(|e| FakeError::Fixture(format!("{name}: {e}")))?;
    Ok((event, v))
}
