//! The registration protocol itself: hosts per brand, the begin answer, a poll's classification.
//! No database, no tasks (see the module docs for the rules and their sources).

use serde_json::Value;
use zeroize::Zeroizing;

/// The registration endpoint, on an accounts host.
pub const PATH: &str = "/oauth/v1/app/registration";
/// The app kind created (both official tools use it).
pub const ARCHETYPE: &str = "PersonalAgent";
pub(crate) const DEFAULT_INTERVAL_S: i64 = 5;
pub(crate) const DEFAULT_EXPIRE_S: i64 = 600;
pub(crate) const MAX_INTERVAL_S: i64 = 60;

/// The hosts. [`Bases::real`] in the Hub; tests put both brands on the fake (Lark under `/lark`).
#[derive(Debug, Clone)]
pub struct Bases {
    pub feishu_accounts: String,
    pub lark_accounts: String,
    pub feishu_open: String,
    pub lark_open: String,
    /// Tests only: the bot connection's `api_base` (the fake's open API).
    pub api_base: Option<String>,
}

impl Bases {
    pub fn real() -> Self {
        Self {
            feishu_accounts: "https://accounts.feishu.cn".into(),
            lark_accounts: "https://accounts.larksuite.com".into(),
            feishu_open: "https://open.feishu.cn".into(),
            lark_open: "https://open.larksuite.com".into(),
            api_base: None,
        }
    }

    /// Everything on one fake: Feishu at `base`, Lark at `base/lark`.
    pub fn fake(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            feishu_accounts: b.into(),
            lark_accounts: format!("{b}/lark"),
            feishu_open: b.into(),
            lark_open: format!("{b}/lark"),
            api_base: Some(b.into()),
        }
    }

    pub(crate) fn accounts(&self, brand: Brand) -> &str {
        match brand {
            Brand::Feishu => &self.feishu_accounts,
            Brand::Lark => &self.lark_accounts,
        }
    }

    pub(crate) fn open(&self, brand: Brand) -> &str {
        match brand {
            Brand::Feishu => &self.feishu_open,
            Brand::Lark => &self.lark_open,
        }
    }

    /// A confirmation page the provider gave is used only on one of our hosts.
    fn ours(&self, url: &str) -> bool {
        [
            &self.feishu_accounts,
            &self.lark_accounts,
            &self.feishu_open,
            &self.lark_open,
        ]
        .iter()
        .any(|b| url.starts_with(&format!("{b}/")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brand {
    Feishu,
    Lark,
}

impl Brand {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "feishu" => Some(Self::Feishu),
            "lark" => Some(Self::Lark),
            _ => None,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Feishu => "feishu",
            Self::Lark => "lark",
        }
    }
}

/// What a poll answered, classified ([`classify`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Pending,
    SlowDown,
    /// The tenant is on another brand: poll there.
    Switch(Brand),
    Issued {
        client_id: String,
        /// Zeroized when dropped; never formatted.
        client_secret: Secret,
        tenant_brand: Option<Brand>,
    },
    Denied,
    Expired,
    Failed,
}

/// A secret that never prints.
#[derive(Clone, PartialEq)]
pub struct Secret(pub(crate) Zeroizing<String>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

fn int(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// One poll's answer, by the official client's rules. `switched`: a brand switch already happened
/// (a second report is ignored); `current`: the brand polled.
pub fn classify(v: &Value, current: Brand, switched: bool) -> Poll {
    let tenant = v
        .get("user_info")
        .and_then(|u| u.get("tenant_brand"))
        .and_then(Value::as_str)
        .and_then(Brand::parse);
    if !switched
        && let Some(t) = tenant
        && t != current
    {
        return Poll::Switch(t);
    }
    let err = s(v, "error");
    if err.is_empty() {
        let (id, secret) = (s(v, "client_id"), s(v, "client_secret"));
        if !id.is_empty() && !secret.is_empty() {
            return Poll::Issued {
                client_id: id,
                client_secret: Secret(Zeroizing::new(secret)),
                tenant_brand: tenant,
            };
        }
        // no error and no complete credential: keep polling
        return Poll::Pending;
    }
    match err.as_str() {
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "access_denied" => Poll::Denied,
        "expired_token" | "invalid_grant" => Poll::Expired,
        _ => Poll::Failed,
    }
}

/// The begin answer we keep. No `Debug`: it holds the bearer ticket.
#[derive(Clone, PartialEq)]
pub struct Begun {
    pub device_code: Zeroizing<String>,
    pub user_code: String,
    pub verification_uri_complete: String,
    pub expire_s: i64,
    pub interval_s: i64,
}

/// Reads a begin answer; `page` is the fallback confirmation page (`{open}/page/cli?user_code=`).
pub fn parse_begin(v: &Value, bases: &Bases, brand: Brand) -> Option<Begun> {
    if v.get("error").is_some() {
        return None;
    }
    let device_code = s(v, "device_code");
    if device_code.is_empty() {
        return None;
    }
    let user_code = s(v, "user_code");
    let given = s(v, "verification_uri_complete");
    let verification_uri_complete = if !given.is_empty() && bases.ours(&given) {
        given
    } else {
        let mut u = url::Url::parse(&format!("{}/page/cli", bases.open(brand))).ok()?;
        u.query_pairs_mut().append_pair("user_code", &user_code);
        u.to_string()
    };
    let expire = match int(v, "expire_in") {
        n if n > 0 => n,
        _ => int(v, "expires_in"),
    };
    Some(Begun {
        device_code: Zeroizing::new(device_code),
        user_code,
        verification_uri_complete,
        expire_s: if expire > 0 { expire } else { DEFAULT_EXPIRE_S },
        interval_s: clamp_interval(int(v, "interval")),
    })
}

fn clamp_interval(n: i64) -> i64 {
    if n <= 0 {
        DEFAULT_INTERVAL_S
    } else {
        n.min(MAX_INTERVAL_S)
    }
}

pub(crate) fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_follows_the_official_rules() {
        let f = Brand::Feishu;
        assert_eq!(
            classify(&json!({"error": "authorization_pending"}), f, false),
            Poll::Pending
        );
        assert_eq!(
            classify(&json!({}), f, false),
            Poll::Pending,
            "incomplete without error"
        );
        assert_eq!(
            classify(&json!({"client_id": "cli_x"}), f, false),
            Poll::Pending
        );
        assert_eq!(
            classify(&json!({"error": "slow_down"}), f, false),
            Poll::SlowDown
        );
        assert_eq!(
            classify(&json!({"error": "access_denied"}), f, false),
            Poll::Denied
        );
        assert_eq!(
            classify(&json!({"error": "expired_token"}), f, false),
            Poll::Expired
        );
        assert_eq!(
            classify(&json!({"error": "invalid_grant"}), f, false),
            Poll::Expired
        );
        assert_eq!(classify(&json!({"error": "boom"}), f, false), Poll::Failed);
        // the tenant report switches once, even alongside pending
        let lark = json!({"error": "authorization_pending", "user_info": {"tenant_brand": "lark"}});
        assert_eq!(classify(&lark, f, false), Poll::Switch(Brand::Lark));
        assert_eq!(classify(&lark, f, true), Poll::Pending, "only once");
        assert_eq!(
            classify(
                &json!({"user_info": {"tenant_brand": "feishu"}, "error": "authorization_pending"}),
                f,
                false
            ),
            Poll::Pending
        );
        match classify(
            &json!({"client_id": "cli_x", "client_secret": "sec", "user_info": {"tenant_brand": "feishu"}}),
            f,
            false,
        ) {
            Poll::Issued {
                client_id,
                tenant_brand,
                client_secret,
            } => {
                assert_eq!(
                    (client_id.as_str(), tenant_brand),
                    ("cli_x", Some(Brand::Feishu))
                );
                assert_eq!(format!("{client_secret:?}"), "Secret(…)", "never printed");
            }
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn begin_answers_are_normalized() {
        let b = Bases::fake("http://127.0.0.1:9");
        let v = json!({"device_code": "d", "user_code": "U-1", "expires_in": 120, "interval": 0});
        let x = parse_begin(&v, &b, Brand::Feishu).expect("begun");
        assert_eq!((x.expire_s, x.interval_s), (120, DEFAULT_INTERVAL_S));
        assert_eq!(
            x.verification_uri_complete,
            "http://127.0.0.1:9/page/cli?user_code=U-1"
        );
        let v = json!({"device_code": "d", "user_code": "U-1", "expire_in": 0, "interval": 999,
            "verification_uri_complete": "http://127.0.0.1:9/lark/page/cli?user_code=U-1&x=1"});
        let x = parse_begin(&v, &b, Brand::Lark).expect("begun");
        assert_eq!(
            (x.expire_s, x.interval_s),
            (DEFAULT_EXPIRE_S, MAX_INTERVAL_S)
        );
        assert_eq!(
            x.verification_uri_complete,
            "http://127.0.0.1:9/lark/page/cli?user_code=U-1&x=1"
        );
        // a page elsewhere is not shown: ours instead
        let v = json!({"device_code": "d", "user_code": "U 2", "verification_uri_complete": "https://evil.example/p"});
        let x = parse_begin(&v, &b, Brand::Lark).expect("begun");
        assert_eq!(
            x.verification_uri_complete,
            "http://127.0.0.1:9/lark/page/cli?user_code=U+2"
        );
        assert!(
            parse_begin(
                &json!({"error": "x", "device_code": "d"}),
                &b,
                Brand::Feishu
            )
            .is_none()
        );
        assert!(parse_begin(&json!({"user_code": "u"}), &b, Brand::Feishu).is_none());
    }
}
