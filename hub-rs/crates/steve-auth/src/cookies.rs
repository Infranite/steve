//! Cookies serialized exactly like Hono's `setCookie`/`deleteCookie`.

use axum::{
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};

/// `name=value; Max-Age=…; Path=…; HttpOnly[; Secure]; SameSite=Lax`
pub fn set(name: &str, value: &str, path: &str, max_age: u64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{name}={value}; Max-Age={max_age}; Path={path}; HttpOnly{secure}; SameSite=Lax")
}

/// Hono's `deleteCookie(c, name, {path})`.
pub fn clear(name: &str, path: &str) -> String {
    format!("{name}=; Max-Age=0; Path={path}")
}

pub fn append(res: &mut Response, cookie: String) {
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
}

/// First value of `name` across the request's `Cookie` headers.
pub fn get(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hono_format() {
        assert_eq!(
            set("steve_session", "abc", "/", 2592000, true),
            "steve_session=abc; Max-Age=2592000; Path=/; HttpOnly; Secure; SameSite=Lax"
        );
        assert_eq!(
            set("steve_login", "k", "/auth", 600, false),
            "steve_login=k; Max-Age=600; Path=/auth; HttpOnly; SameSite=Lax"
        );
        assert_eq!(
            clear("steve_login", "/auth"),
            "steve_login=; Max-Age=0; Path=/auth"
        );
    }

    #[test]
    fn reads_cookie_header() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            "a=1; steve_session=tok; steve_ws=w".parse().unwrap(),
        );
        assert_eq!(get(&h, "steve_session").as_deref(), Some("tok"));
        assert_eq!(get(&h, "steve_login"), None);
        h.insert(header::COOKIE, "steve_session=".parse().unwrap());
        assert_eq!(get(&h, "steve_session"), None);
    }
}
