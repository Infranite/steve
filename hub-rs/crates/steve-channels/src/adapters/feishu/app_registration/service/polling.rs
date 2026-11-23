use super::*;

pub(super) enum PollReply {
    Answer(Value),
    Retry,
    Finished,
}
pub(super) enum PollControl {
    Wait,
    Switched,
    Finished,
}

impl Registrations {
    pub(super) async fn request_poll(
        &self,
        id: &str,
        r: &Row,
        code: &str,
        resumed: bool,
    ) -> VaultResult<PollReply> {
        // never poll past the deadline: the request may take at most what is left
        let left = r.expires_at - self.now();
        if left <= 0 {
            let kind = if resumed {
                Kind::CredentialsLost
            } else {
                Kind::Expired
            };
            return self
                .end(id, state::EXPIRED, kind, resumed)
                .map(|()| PollReply::Finished);
        }
        let url = format!("{}{PATH}", self.bases.accounts(r.brand()));
        let timeout = POLL_TIMEOUT.min(Duration::from_millis(left as u64));
        // a credential issued by a poll sent in time is kept even if it arrives late: the app
        // exists at the provider either way, dropping it would only orphan it
        match self
            .post(&url, &[("action", "poll"), ("device_code", code)], timeout)
            .await
        {
            Ok((_, v)) => Ok(PollReply::Answer(v)),
            Err(_) => {
                self.set_interval(id, (r.interval_s + 1).min(MAX_INTERVAL_S))?;
                Ok(PollReply::Retry)
            }
        }
    }

    pub(super) async fn apply_poll_response(
        &self,
        id: &str,
        r: &Row,
        v: &Value,
        resumed: bool,
    ) -> VaultResult<PollControl> {
        match classify(v, r.brand(), r.switched) {
            Poll::Pending => Ok(PollControl::Wait),
            Poll::SlowDown => self
                .set_interval(id, (r.interval_s + 5).min(MAX_INTERVAL_S))
                .map(|()| PollControl::Wait),
            Poll::Switch(b) => {
                let now = self.now();
                self.write(|w| Ok(repo::switch_brand(w, id, b.code(), now)?))?;
                tracing::info!(registration = %id, brand = b.code(), "feishu app registration: tenant on another brand");
                Ok(PollControl::Switched)
            }
            Poll::Denied => {
                return self
                    .end(id, state::DENIED, Kind::Denied, false)
                    .map(|()| PollControl::Finished);
            }
            Poll::Expired => {
                let kind = if resumed {
                    Kind::CredentialsLost
                } else {
                    Kind::Expired
                };
                return self
                    .end(id, state::EXPIRED, kind, resumed)
                    .map(|()| PollControl::Finished);
            }
            Poll::Failed => {
                tracing::warn!(registration = %id, "feishu app registration failed (provider error)");
                return self
                    .end(id, state::FAILED, Kind::Provider, false)
                    .map(|()| PollControl::Finished);
            }
            Poll::Issued {
                client_id,
                client_secret,
                tenant_brand,
            } => {
                // the issuing host is authoritative: a contradicting final report is a violation
                if tenant_brand.is_some_and(|t| t != r.brand()) {
                    tracing::warn!(registration = %id, "feishu app registration: contradictory tenant brand");
                    return self
                        .end(id, state::FAILED, Kind::Provider, true)
                        .map(|()| PollControl::Finished);
                }
                self.save(&r, &client_id, client_secret).await?;
                self.spawn_verify(id.to_string());
                return Ok(PollControl::Finished);
            }
        }
    }
}
