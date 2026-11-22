//! The Hub's side of the Node→Hub methods: events, Steve's tools and blobs, handed to whatever the
//! Hub bound to this one authenticated Node (in the Hub, always the guarded ports). A Hub with no
//! secrets for this Node answers `secret.take` `unsupported`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use steve_node_api::*;

use crate::conn::{Handler, from_value as de, to_value as en, unsupported};
use crate::methods::*;

/// Leave header space, then account for hex's two characters per byte. This must remain safe
/// even when the ordinary protocol's local chunk policy grows.
pub const UPGRADE_CHUNK_BYTES: u32 = ((upgrade_v1::MAX_FRAME - 4096) / 2) as u32;

pub struct ServeHub {
    pub sink: Arc<dyn EventSink>,
    pub tools: Arc<dyn HubTools>,
    pub files: Option<Arc<dyn HubFiles>>,
    pub secrets: Option<Arc<dyn HubSecrets>>,
}

impl ServeHub {
    fn files(&self) -> ApiResult<&Arc<dyn HubFiles>> {
        self.files
            .as_ref()
            .ok_or_else(|| ApiError::new(ErrorCode::Unsupported, "no files on this hub"))
    }
}

#[async_trait]
impl Handler for ServeHub {
    async fn handle_upgrade(&self, method: &str, p: Value) -> upgrade_v1::Result<Value> {
        if method != upgrade_v1::UPGRADE_FETCH {
            return Err(upgrade_v1::Error::incompatible());
        }
        let request: upgrade_v1::FetchRequest = crate::upgrade::decode(p)?;
        let (access, id, offset, max) = upgrade_bridge::fetch_request(request)?;
        let max = max.min(UPGRADE_CHUNK_BYTES);
        let chunk = self
            .files()
            .map_err(upgrade_v1::Error::from)?
            .fetch(access, id, offset, max)
            .await
            .map_err(upgrade_v1::Error::from)?;
        crate::upgrade::encode(upgrade_v1::FetchResponse::from(chunk))
    }

    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        match method {
            EVENTS_COMMIT => en(&self.sink.commit(de(p)?).await?),
            TOOL_CATALOG => en(&self.tools.catalog(de(p)?).await?),
            TOOL_SESSION_CATALOG => en(&self.tools.session_catalog(de(p)?).await?),
            TOOL_CALL => en(&self.tools.call(de(p)?).await?),
            TOOL_INSPECT => en(&self.tools.call_status(de(p)?).await?),
            TOOL_CANCEL => en(&self.tools.cancel_call(de(p)?).await?),
            REPLY_CAPTURE => en(&self.files()?.capture_reply(de(p)?).await?),
            BLOB_FETCH => {
                let a: FetchArgs = de(p)?;
                if matches!(a.access, FileAccess::Operation { .. }) {
                    return Err(ApiError::new(
                        ErrorCode::Denied,
                        "upgrade archives require upgrade v1",
                    ));
                }
                en(&self.files()?.fetch(a.access, a.id, a.offset, a.max).await?)
            }
            BLOB_BEGIN => en(&self.files()?.begin_upload(de(p)?).await?),
            BLOB_CHUNK => en(&self.files()?.upload(de(p)?).await?),
            BLOB_STATUS => en(&self.files()?.upload_status(de(p)?).await?),
            BLOB_FINISH => en(&self.files()?.finish_upload(de(p)?).await?),
            BLOB_ABORT => en(&self.files()?.abort_upload(de(p)?).await?),
            SECRET_TAKE => {
                let a: SecretArgs = de(p)?;
                let s = self.secrets.as_ref().ok_or_else(|| {
                    ApiError::new(ErrorCode::Unsupported, "no secrets on this hub")
                })?;
                en(&s.take(a.operation, a.id).await?)
            }
            other => Err(unsupported(other)),
        }
    }
}
