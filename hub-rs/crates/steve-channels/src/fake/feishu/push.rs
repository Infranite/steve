//! Event pusher: wraps events in the schema-2.0 envelope, encrypts them with the Encrypt Key,
//! signs them and POSTs them to the Hub's `/hooks/<connection_id>`, the way Feishu's "send
//! events to developer server" does.
//!
//! The encryption and signing here are written independently of `adapters::feishu::crypto`
//! so the round-trip test checks one implementation against the other.

use std::sync::Arc;
use std::time::Duration;

use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::model::{Body, Mention, Message, Sender};
use super::{Shared, now_ms};

/// Feishu's redelivery schedule after a push that didn't get HTTP 200 in time: 15 s, 5 min,
/// 1 h, 6 h (four retries).
pub const FEISHU_RETRY_SCHEDULE: [Duration; 4] = [
    Duration::from_secs(15),
    Duration::from_secs(5 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
];

/// AES-256-CBC with key = sha256(encrypt_key), random IV prepended, PKCS#7, base64 — the
/// `encrypt` field value.
pub fn encrypt(encrypt_key: &str, plaintext: &[u8]) -> String {
    let key: [u8; 32] = Sha256::digest(encrypt_key.as_bytes()).into();
    let iv: [u8; 16] = rand::random();
    let enc = cbc::Encryptor::<aes::Aes256>::new(&key.into(), &iv.into());
    let ct = enc.encrypt_padded_vec::<Pkcs7>(plaintext);
    let mut raw = iv.to_vec();
    raw.extend_from_slice(&ct);
    base64::engine::general_purpose::STANDARD.encode(raw)
}

fn sign(timestamp: &str, nonce: &str, encrypt_key: &str, body: &[u8]) -> String {
    let mut buf =
        Vec::with_capacity(timestamp.len() + nonce.len() + encrypt_key.len() + body.len());
    buf.extend_from_slice(timestamp.as_bytes());
    buf.extend_from_slice(nonce.as_bytes());
    buf.extend_from_slice(encrypt_key.as_bytes());
    buf.extend_from_slice(body);
    let digest = Sha256::digest(&buf);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A request ready to POST: headers and the exact body bytes that were signed.
#[derive(Debug, Clone)]
pub struct SignedRequest {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// What was encrypted, for assertions.
    pub plaintext: Value,
}

impl SignedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The Hub's answer to one push.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub event_id: Option<String>,
    pub status: u16,
    pub body: String,
}

impl Delivery {
    pub fn ok(&self) -> bool {
        self.status == 200
    }
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("event {0} was never pushed")]
    UnknownEvent(String),
}

/// Pushes events to one hook URL. Cheap to create; history of pushed events lives in the fake.
pub struct EventPusher {
    shared: Arc<Shared>,
    url: String,
}

impl EventPusher {
    pub(crate) fn new(shared: Arc<Shared>, url: String) -> Self {
        Self { shared, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Schema-2.0 envelope with a fresh `event_id` and the app's token, app id and tenant key.
    pub fn envelope(&self, event_type: &str, event: Value) -> Value {
        envelope(&self.shared, event_type, event)
    }

    /// Encrypts and signs `plaintext` with the current time and a random nonce.
    pub fn sign(&self, plaintext: &Value) -> SignedRequest {
        let ts = (now_ms() / 1000).to_string();
        self.sign_at(plaintext, &ts)
    }

    /// Like [`Self::sign`] with a chosen `X-Lark-Request-Timestamp` (stale-timestamp tests).
    pub fn sign_at(&self, plaintext: &Value, timestamp: &str) -> SignedRequest {
        let key = self.shared.world().app.encrypt_key.clone();
        let nonce = format!("{:x}", rand::random::<u64>());
        let body = serde_json::to_vec(
            &json!({"encrypt": encrypt(&key, plaintext.to_string().as_bytes())}),
        )
        .unwrap_or_default();
        let signature = sign(timestamp, &nonce, &key, &body);
        SignedRequest {
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/json; charset=utf-8".into(),
                ),
                ("X-Lark-Request-Timestamp".into(), timestamp.to_string()),
                ("X-Lark-Request-Nonce".into(), nonce),
                ("X-Lark-Signature".into(), signature),
            ],
            body,
            plaintext: plaintext.clone(),
        }
    }

    /// Wraps `event` in a fresh envelope and pushes it.
    pub async fn push(&self, event_type: &str, event: Value) -> Result<Delivery, PushError> {
        let env = self.envelope(event_type, event);
        self.push_envelope(env).await
    }

    /// Pushes a complete envelope as-is (e.g. a fixture). Remembers it for [`Self::redeliver`];
    /// `im.message.receive_v1` messages are added to the fake's chat history.
    pub async fn push_envelope(&self, envelope: Value) -> Result<Delivery, PushError> {
        let event_id = envelope
            .pointer("/header/event_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(id) = &event_id {
            self.shared.rt().pushed.insert(id.clone(), envelope.clone());
        }
        if envelope
            .pointer("/header/event_type")
            .and_then(Value::as_str)
            == Some("im.message.receive_v1")
        {
            record_history(&self.shared, &envelope);
        }
        let req = self.sign(&envelope);
        let mut d = self.post(&req).await?;
        d.event_id = event_id;
        Ok(d)
    }

    /// Pushes the same event again (Feishu redelivers with the same `event_id`, re-signed).
    pub async fn redeliver(&self, event_id: &str) -> Result<Delivery, PushError> {
        let env = self
            .shared
            .rt()
            .pushed
            .get(event_id)
            .cloned()
            .ok_or_else(|| PushError::UnknownEvent(event_id.to_string()))?;
        let req = self.sign(&env);
        let mut d = self.post(&req).await?;
        d.event_id = Some(event_id.to_string());
        Ok(d)
    }

    /// Pushes, then redelivers on every non-200 answer after each delay in `schedule`, like
    /// Feishu does. Pass [`FEISHU_RETRY_SCHEDULE`] scaled down in tests. Returns all attempts.
    pub async fn push_with_retries(
        &self,
        event_type: &str,
        event: Value,
        schedule: &[Duration],
    ) -> Result<Vec<Delivery>, PushError> {
        let first = self.push(event_type, event).await?;
        let id = first.event_id.clone().unwrap_or_default();
        let mut all = vec![first];
        for delay in schedule {
            if all.last().is_some_and(Delivery::ok) {
                break;
            }
            tokio::time::sleep(*delay).await;
            all.push(self.redeliver(&id).await?);
        }
        Ok(all)
    }

    /// Encrypted `url_verification` challenge as the console sends it when you save the request
    /// URL: the docs exclude it from signing, so no `X-Lark-*` headers. Returns the challenge.
    pub async fn push_url_verification(&self) -> Result<(String, Delivery), PushError> {
        let (challenge, plain) = self.url_verification_plaintext();
        let mut req = self.sign(&plain);
        req.headers.retain(|(k, _)| !k.starts_with("X-Lark-"));
        let d = self.post(&req).await?;
        Ok((challenge, d))
    }

    /// Same challenge but signed like an event (in case Feishu does sign it; to be checked in F7).
    pub async fn push_url_verification_signed(&self) -> Result<(String, Delivery), PushError> {
        let (challenge, plain) = self.url_verification_plaintext();
        let d = self.post(&self.sign(&plain)).await?;
        Ok((challenge, d))
    }

    fn url_verification_plaintext(&self) -> (String, Value) {
        let token = self.shared.world().app.verification_token.clone();
        let challenge = format!("{:x}-{:x}", rand::random::<u64>(), rand::random::<u32>());
        let plain = json!({"challenge": challenge, "token": token, "type": "url_verification"});
        (challenge, plain)
    }

    /// POSTs arbitrary headers and body (bad signatures, corrupt ciphertext, plaintext bodies).
    pub async fn push_raw(
        &self,
        headers: &[(&str, &str)],
        body: impl Into<Vec<u8>>,
    ) -> Result<Delivery, PushError> {
        let req = SignedRequest {
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.into(),
            plaintext: Value::Null,
        };
        self.post(&req).await
    }

    /// POSTs a prepared request.
    pub async fn post(&self, req: &SignedRequest) -> Result<Delivery, PushError> {
        let mut rb = self.shared.http.post(&self.url);
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        let resp = rb.body(req.body.clone()).send().await?;
        let status = resp.status().as_u16();
        let body = resp.text().await?;
        Ok(Delivery {
            event_id: None,
            status,
            body,
        })
    }
}

/// Adds a received message to the chat history so `GET im/v1/messages` sees it.
pub(crate) fn record_history(shared: &Shared, env: &Value) {
    let (Some(m), Some(s)) = (env.pointer("/event/message"), env.pointer("/event/sender")) else {
        return;
    };
    let st = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let Some(message_id) = st(m, "message_id") else {
        return;
    };
    let mut w = shared.world();
    if w.messages.iter().any(|x| x.message_id == message_id) {
        return;
    }
    let sender_type = st(s, "sender_type").unwrap_or_else(|| "user".into());
    let sender_id = s
        .pointer("/sender_id/open_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mentions = m
        .get("mentions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| Mention {
                    key: st(x, "key").unwrap_or_default(),
                    id: x
                        .pointer("/id/open_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    id_type: "open_id".into(),
                    name: st(x, "name").unwrap_or_default(),
                    tenant_key: st(x, "tenant_key").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let ct = st(m, "create_time").unwrap_or_else(|| now_ms().to_string());
    w.messages.push(Message {
        message_id,
        root_id: st(m, "root_id"),
        parent_id: st(m, "parent_id"),
        thread_id: st(m, "thread_id"),
        msg_type: st(m, "message_type").unwrap_or_else(|| "text".into()),
        create_time: ct.clone(),
        update_time: st(m, "update_time").unwrap_or(ct),
        deleted: false,
        updated: false,
        chat_id: st(m, "chat_id").unwrap_or_default(),
        sender: Sender {
            id: sender_id,
            id_type: "open_id".into(),
            sender_type,
            tenant_key: st(s, "tenant_key").unwrap_or_default(),
        },
        body: Body {
            content: st(m, "content").unwrap_or_else(|| "{}".into()),
        },
        mentions,
        upper_message_id: None,
    });
}

/// Schema-2.0 envelope with a fresh `event_id`, shared by the webhook pusher and the long
/// connection.
pub(crate) fn envelope(shared: &Shared, event_type: &str, event: Value) -> Value {
    let app = shared.world().app.clone();
    let event_id = format!("{:032x}", rand::random::<u128>());
    json!({
        "schema": "2.0",
        "header": {
            "event_id": event_id,
            "event_type": event_type,
            "create_time": now_ms().to_string(),
            "token": app.verification_token,
            "app_id": app.app_id,
            "tenant_key": app.tenant_key,
        },
        "event": event,
    })
}
