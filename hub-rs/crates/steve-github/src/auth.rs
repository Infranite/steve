//! GitHub App authentication: the App JWT (RS256, `iat = now-60`,
//! `exp = now+540`, `iss = app id`) and installation tokens.
//!
//! Caching: the GitHub App adapter declares `TokenKind::Minted`, so the connections framework keeps
//! the token in memory and mints a new one 5 minutes before [`Minted::expires_at`] (and after
//! `invalidate_token`). The plaintext exists only in `Zeroizing` strings: here, in that cache, and
//! for the duration of one request.

use reqwest::Method;
use serde_json::{Value, json};
use steve_modules::accounts::connections::{Minted, ProviderError};
use zeroize::Zeroizing;

use crate::api::{GhClient, GhError};

/// JWT lifetime: GitHub allows 10 minutes; 9 leaves room for clock drift.
pub const JWT_TTL_SECS: i64 = 540;
/// `iat` back-dated by a minute, as GitHub recommends against drift.
pub const JWT_BACKDATE_SECS: i64 = 60;

/// Signs an App JWT. `now_secs`: Unix seconds.
pub fn app_jwt(pem: &str, app_id: u64, now_secs: i64) -> Result<Zeroizing<String>, ProviderError> {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
        .map_err(|_| ProviderError::Auth("GitHub App 的私钥不对".into()))?;
    let claims = json!({
        "iat": now_secs - JWT_BACKDATE_SECS,
        "exp": now_secs + JWT_TTL_SECS,
        "iss": app_id,
    });
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )
    .map(Zeroizing::new)
    .map_err(|_| ProviderError::Auth("GitHub App 的私钥签不了 JWT".into()))
}

/// `Bearer <jwt>` for the App endpoints.
pub fn app_bearer(
    pem: &str,
    app_id: u64,
    now_secs: i64,
) -> Result<Zeroizing<String>, ProviderError> {
    let jwt = app_jwt(pem, app_id, now_secs)?;
    Ok(Zeroizing::new(format!("Bearer {}", jwt.as_str())))
}

/// `GET /app/installations/{id}` with the App JWT.
pub async fn get_installation(
    client: &GhClient,
    bearer: &str,
    installation_id: u64,
) -> Result<Value, GhError> {
    let r = client
        .request(
            Method::GET,
            &format!("/app/installations/{installation_id}"),
            Some(bearer),
            None,
            None,
        )
        .await?;
    Ok(r.body)
}

/// `POST /app/installations/{id}/access_tokens`. `hub_now_ms` / `real_now_ms`: the Hub clock and
/// the wall clock, so the expiry lands on the clock the cache compares with (they are the same in
/// production; tests move the Hub clock).
pub async fn installation_token(
    client: &GhClient,
    bearer: &str,
    installation_id: u64,
    hub_now_ms: i64,
    real_now_ms: i64,
) -> Result<Minted, GhError> {
    let r = client
        .request(
            Method::POST,
            &format!("/app/installations/{installation_id}/access_tokens"),
            Some(bearer),
            Some(&json!({})),
            None,
        )
        .await?;
    let token = r.body["token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or(GhError::Decode)?;
    let token = Zeroizing::new(token.to_string());
    let expires = r.body["expires_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
        // GitHub's tokens live an hour; assume the least if the field is missing.
        .unwrap_or(real_now_ms + 10 * 60_000);
    Ok(Minted {
        token,
        expires_at: hub_now_ms + (expires - real_now_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The fake's throwaway test key (also used by the fake to check signatures).
    const PEM: &str = include_str!("fake/test_app_key.pem");
    const PUB: &str = include_str!("fake/test_app_key.pub.pem");

    #[test]
    fn jwt_has_the_documented_claims() {
        let t = app_jwt(PEM, 1001, 1_700_000_000).expect("jwt");
        let key = jsonwebtoken::DecodingKey::from_rsa_pem(PUB.as_bytes()).expect("pub");
        let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        v.validate_exp = false;
        v.set_required_spec_claims(&["exp"]);
        let d = jsonwebtoken::decode::<Value>(t.as_str(), &key, &v).expect("valid signature");
        assert_eq!(d.header.alg, jsonwebtoken::Algorithm::RS256);
        assert_eq!(d.claims["iss"], json!(1001));
        assert_eq!(d.claims["iat"], json!(1_700_000_000 - 60));
        assert_eq!(d.claims["exp"], json!(1_700_000_000 + 540));
    }

    #[test]
    fn a_bad_key_is_an_auth_error() {
        assert!(matches!(
            app_jwt("not a key", 1, 0),
            Err(ProviderError::Auth(_))
        ));
    }
}
