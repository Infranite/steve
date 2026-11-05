//! The Hub's side of the Node→Hub methods: events, Steve's tools and blobs, handed to whatever the
//! Hub bound to this one authenticated Node (in the Hub, always the guarded ports).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use steve_node_api::*;

use crate::conn::{Handler, from_value as de, to_value as en, unsupported};
use crate::methods::*;

pub struct ServeHub {
    pub sink: Arc<dyn EventSink>,
    pub tools: Arc<dyn HubTools>,
    pub files: Option<Arc<dyn HubFiles>>,
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
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        match method {
            EVENTS_COMMIT => en(&self.sink.commit(de(p)?).await?),
            TOOL_CATALOG => en(&self.tools.catalog(de(p)?).await?),
            TOOL_CALL => en(&self.tools.call(de(p)?).await?),
            TOOL_INSPECT => en(&self.tools.call_status(de(p)?).await?),
            TOOL_CANCEL => en(&self.tools.cancel_call(de(p)?).await?),
            BLOB_FETCH => {
                let a: FetchArgs = de(p)?;
                en(&self.files()?.fetch(a.access, a.id, a.offset, a.max).await?)
            }
            BLOB_BEGIN => en(&self.files()?.begin_upload(de(p)?).await?),
            BLOB_CHUNK => en(&self.files()?.upload(de(p)?).await?),
            BLOB_STATUS => en(&self.files()?.upload_status(de(p)?).await?),
            BLOB_FINISH => en(&self.files()?.finish_upload(de(p)?).await?),
            BLOB_ABORT => en(&self.files()?.abort_upload(de(p)?).await?),
            other => Err(unsupported(other)),
        }
    }
}
