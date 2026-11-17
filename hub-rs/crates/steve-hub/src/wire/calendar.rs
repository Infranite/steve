//! The calendar sources a connector plugin brings: an ext_ws connection whose connector declares
//! the `calendar.v1` profile (ext_ws §17) is read by the calendar service through the profile's
//! methods, never through its exported tools.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use steve_calendar::profile::{Features, JsonSource, PROFILE, ProfileCall, SourceError};
use steve_channels::ext::ConnectorTools;
use steve_modules::accounts::connections::ConnCx;

/// [`ProfileCall`] over a channel side's connector.
pub struct ProfileSource(pub Arc<dyn ConnectorTools>);

impl ProfileSource {
    /// A calendar source for the connections of `connector`'s adapter.
    pub fn source(connector: Arc<dyn ConnectorTools>) -> Arc<dyn steve_calendar::CalendarSource> {
        Arc::new(JsonSource(Self(connector)))
    }
}

#[async_trait]
impl ProfileCall for ProfileSource {
    fn adapter(&self) -> &str {
        self.0.adapter_id()
    }

    /// The connection's declared `calendar.v1`, when it offers reading through it.
    async fn declared(&self, cx: &ConnCx) -> Result<Option<Features>, SourceError> {
        let manifest = self.0.catalog(cx).await.map_err(SourceError::Transient)?;
        Ok(manifest
            .and_then(|m| m.profiles.into_iter().find(|p| p.id == PROFILE))
            .filter(|p| p.caps.iter().any(|c| c == "cal.read"))
            .map(|p| Features(p.features.into_iter().collect())))
    }

    async fn call(&self, cx: &ConnCx, method: &str, args: Value) -> Result<Value, SourceError> {
        self.0
            .profile_call(cx, PROFILE, method, args)
            .await
            .map_err(SourceError::Transient)
    }
}
