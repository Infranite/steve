//! Access tokens and clients (A1). A user signs a client in with their name and an access token; the
//! client gets its own secret and uses that from then on. Only sha256 of a token or secret is kept.

use rusqlite::{Row, params};
use serde::Serialize;

use super::{Cached, all, one};
use crate::{Result, Store, User, now, sha, uid};

/// The prefix of an access token as it is shown (`stt_…`), so a pasted one is recognised.
pub const TOKEN_PREFIX: &str = "stt_";

/// 256 random bits as hex (two v4 UUIDs: the OS random generator).
fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// An access token, without the token itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccessToken {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub created: i64,
    pub expires: Option<i64>,
    pub last_used: Option<i64>,
    pub revoked: Option<i64>,
}

impl AccessToken {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            user_id: r.get("user_id")?,
            name: r.get("name")?,
            created: r.get("created")?,
            expires: r.get("expires")?,
            last_used: r.get("last_used")?,
            revoked: r.get("revoked")?,
        })
    }
}

/// A signed-in client: a web page somewhere, a desktop or phone app, a command line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Client {
    pub id: String,
    pub user_id: String,
    pub token_id: Option<String>,
    /// `web`, `mac`, `ios`, `cli`, …
    pub kind: String,
    pub name: String,
    pub platform: String,
    pub version: String,
    /// The web origin it runs on, when it is a web page.
    pub origin: Option<String>,
    pub created: i64,
    pub last_seen: Option<i64>,
    /// How it last came: `loopback`, `lan`, `tailnet`, `proxy`, `public` (and `+tls`).
    pub last_route: Option<String>,
    pub last_ip: Option<String>,
    pub revoked: Option<i64>,
}

impl Client {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            user_id: r.get("user_id")?,
            token_id: r.get("token_id")?,
            kind: r.get("kind")?,
            name: r.get("name")?,
            platform: r.get("platform")?,
            version: r.get("version")?,
            origin: r.get("origin")?,
            created: r.get("created")?,
            last_seen: r.get("last_seen")?,
            last_route: r.get("last_route")?,
            last_ip: r.get("last_ip")?,
            revoked: r.get("revoked")?,
        })
    }
}

/// What a client says about itself when it signs in.
#[derive(Debug, Clone, Default)]
pub struct NewClient<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub platform: &'a str,
    pub version: &'a str,
    pub origin: Option<&'a str>,
}

/// A client's credential: `<client id>.<secret>`, what it sends as `Authorization: Bearer …`.
pub fn credential(client_id: &str, secret: &str) -> String {
    format!("{client_id}.{secret}")
}

pub struct Access<'a>(&'a Store);

impl Store {
    pub fn access(&self) -> Access<'_> {
        Access(self)
    }
}

/// A token is live when it is neither revoked nor expired.
const LIVE_TOKEN: &str = "t.revoked IS NULL AND (t.expires IS NULL OR t.expires > ?)";

impl Access<'_> {
    /// A new access token for `user_id`; the token itself is returned once and never stored.
    pub fn token_create(
        &self,
        user_id: &str,
        name: &str,
        expires: Option<i64>,
    ) -> Result<(AccessToken, String)> {
        let token = format!("{TOKEN_PREFIX}{}", secret());
        let id = uid("tok");
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO access_tokens (id, user_id, name, hash, created, expires) VALUES (?, ?, ?, ?, ?, ?)",
                params![id, user_id, name, sha(&token), now(), expires],
            )?;
            Ok(())
        })?;
        let t = self.token(&id)?.expect("just written");
        Ok((t, token))
    }

    pub fn token(&self, id: &str) -> Result<Option<AccessToken>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM access_tokens WHERE id = ?",
                [id],
                AccessToken::from_row,
            )
        })
    }

    /// The tokens of `user_id`, or of everyone; newest first.
    pub fn tokens(&self, user_id: Option<&str>) -> Result<Vec<AccessToken>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM access_tokens WHERE ?1 IS NULL OR user_id = ?1 ORDER BY created DESC, rowid DESC",
                [user_id],
                AccessToken::from_row,
            )
        })
    }

    /// Revokes a token, and the clients signed in with it; false when there was no live one.
    pub fn token_revoke(&self, id: &str) -> Result<bool> {
        self.0.write(|w| {
            let t = now();
            let n = w.c.run(
                "UPDATE access_tokens SET revoked = ? WHERE id = ? AND revoked IS NULL",
                params![t, id],
            )?;
            w.c.run(
                "UPDATE clients SET revoked = ? WHERE token_id = ? AND revoked IS NULL",
                params![t, id],
            )?;
            Ok(n > 0)
        })
    }

    /// The user called `user` (their name, sign-in subject or email; case-insensitive, and only when
    /// exactly one user answers to it).
    pub fn user_named(&self, user: &str) -> Result<Option<User>> {
        let user = user.trim();
        if user.is_empty() {
            return Ok(None);
        }
        let found = self.0.read(|c| {
            all(
                c,
                "SELECT * FROM users WHERE lower(name) = lower(?1) OR lower(subject) = lower(?1)
                   OR (email IS NOT NULL AND lower(email) = lower(?1))",
                [user],
                User::from_row,
            )
        })?;
        Ok(match found.as_slice() {
            [u] => Some(u.clone()),
            _ => None,
        })
    }

    /// The workspace owner who signed up first.
    pub fn owner(&self) -> Result<Option<User>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM users WHERE role = 'owner' ORDER BY created, rowid LIMIT 1",
                [],
                User::from_row,
            )
        })
    }

    /// Signs a client in: `user` holds the live access token `token`. The new client and its secret
    /// (returned once); `None` when the name or the token does not fit (which of the two is not said).
    pub fn sign_in(
        &self,
        user: &str,
        token: &str,
        c: &NewClient,
    ) -> Result<Option<(User, Client, String)>> {
        let Some(u) = self.user_named(user)? else {
            return Ok(None);
        };
        let t = now();
        let tok: Option<String> = self.0.read(|db| {
            one(
                db,
                &format!(
                    "SELECT t.id FROM access_tokens t WHERE t.hash = ? AND t.user_id = ? AND {LIVE_TOKEN}"
                ),
                params![sha(token.trim()), u.id, t],
                |r| r.get(0),
            )
        })?;
        let Some(tok) = tok else {
            return Ok(None);
        };
        let (id, sec) = (uid("cli"), secret());
        self.0.write(|w| {
            w.c.run(
                "UPDATE access_tokens SET last_used = ? WHERE id = ?",
                params![t, tok],
            )?;
            w.c.run(
                "INSERT INTO clients (id, user_id, token_id, secret_hash, kind, name, platform, version, origin, created, last_seen)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, u.id, tok, sha(&sec), c.kind, c.name, c.platform, c.version, c.origin, t, t],
            )?;
            Ok(())
        })?;
        let client = self.client(&id)?.expect("just written");
        Ok(Some((u, client, sec)))
    }

    pub fn client(&self, id: &str) -> Result<Option<Client>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM clients WHERE id = ?",
                [id],
                Client::from_row,
            )
        })
    }

    /// The user and client behind a credential ([`credential`]): the client not revoked, and the token
    /// it was signed in with still live.
    pub fn client_user(&self, credential: &str) -> Result<Option<(User, Client)>> {
        let Some((id, sec)) = credential.trim().split_once('.') else {
            return Ok(None);
        };
        let row = self.0.read(|c| {
            one(
                c,
                &format!(
                    "SELECT cl.id FROM clients cl LEFT JOIN access_tokens t ON t.id = cl.token_id
                      WHERE cl.id = ? AND cl.secret_hash = ? AND cl.revoked IS NULL
                        AND (cl.token_id IS NULL OR ({LIVE_TOKEN}))"
                ),
                params![id, sha(sec), now()],
                |r| r.get::<_, String>(0),
            )
        })?;
        let Some(id) = row else { return Ok(None) };
        let Some(client) = self.client(&id)? else {
            return Ok(None);
        };
        let user = self.0.users().get(&client.user_id)?;
        Ok(user.map(|u| (u, client)))
    }

    /// Whether a live web client signed in from `origin` (its page may call the Hub from there).
    pub fn origin_signed_in(&self, origin: &str) -> Result<bool> {
        self.0.read(|c| {
            Ok(one(
                c,
                &format!(
                    "SELECT 1 FROM clients cl LEFT JOIN access_tokens t ON t.id = cl.token_id
                      WHERE cl.origin = ? AND cl.revoked IS NULL AND (cl.token_id IS NULL OR ({LIVE_TOKEN}))
                      LIMIT 1"
                ),
                params![origin, now()],
                |r| r.get::<_, i64>(0),
            )?
            .is_some())
        })
    }

    /// Notes that the client came now by `route` from `ip`.
    pub fn client_seen(&self, id: &str, route: &str, ip: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE clients SET last_seen = ?, last_route = ?, last_ip = ? WHERE id = ?",
                params![now(), route, ip, id],
            )?;
            Ok(())
        })
    }

    /// The clients of `user_id`, or of everyone; latest seen first.
    pub fn clients(&self, user_id: Option<&str>) -> Result<Vec<Client>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM clients WHERE ?1 IS NULL OR user_id = ?1
                  ORDER BY coalesce(last_seen, created) DESC, rowid DESC",
                [user_id],
                Client::from_row,
            )
        })
    }

    /// Revokes one client; false when there was no live one.
    pub fn client_revoke(&self, id: &str) -> Result<bool> {
        self.0.write(|w| {
            Ok(w.c.run(
                "UPDATE clients SET revoked = ? WHERE id = ? AND revoked IS NULL",
                params![now(), id],
            )? > 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("t.sqlite3"), crate::FnPublisher(|_| {})).unwrap();
        s.ensure_workspace().unwrap();
        (dir, s)
    }

    fn web() -> NewClient<'static> {
        NewClient {
            kind: "web",
            name: "浏览器",
            platform: "macOS",
            version: "0.2.87",
            origin: Some("http://127.0.0.1:8441"),
        }
    }

    #[test]
    fn a_token_signs_a_client_in_and_revoking_it_signs_the_client_out() {
        let (_d, s) = store();
        let me = s.users().create("local:lee", "Lee", None, "owner").unwrap();
        let (t, token) = s.access().token_create(&me, "first", None).unwrap();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(s.access().tokens(Some(&me)).unwrap(), vec![t.clone()]);
        // wrong token, unknown user: nothing, and the same nothing
        assert!(
            s.access()
                .sign_in("Lee", "stt_nope", &web())
                .unwrap()
                .is_none()
        );
        assert!(
            s.access()
                .sign_in("ghost", &token, &web())
                .unwrap()
                .is_none()
        );
        let (u, c, sec) = s.access().sign_in("lee", &token, &web()).unwrap().unwrap();
        assert_eq!((u.id.as_str(), c.kind.as_str()), (me.as_str(), "web"));
        let cred = credential(&c.id, &sec);
        let (u2, c2) = s.access().client_user(&cred).unwrap().unwrap();
        assert_eq!((u2.id, c2.id.clone()), (me.clone(), c.id.clone()));
        assert!(
            s.access()
                .client_user(&credential(&c.id, "x"))
                .unwrap()
                .is_none()
        );
        s.access()
            .client_seen(&c.id, "lan", Some("192.168.1.9"))
            .unwrap();
        let seen = s.access().client(&c.id).unwrap().unwrap();
        assert_eq!(seen.last_route.as_deref(), Some("lan"));
        assert!(
            s.access()
                .token(&t.id)
                .unwrap()
                .unwrap()
                .last_used
                .is_some()
        );
        // revoking the token signs its clients out
        assert!(s.access().token_revoke(&t.id).unwrap());
        assert!(!s.access().token_revoke(&t.id).unwrap());
        assert!(s.access().client_user(&cred).unwrap().is_none());
        assert!(s.access().sign_in("Lee", &token, &web()).unwrap().is_none());
    }

    #[test]
    fn a_client_is_revoked_alone_and_an_expired_token_signs_nobody_in() {
        let (_d, s) = store();
        let me = s.users().create("local:lee", "Lee", None, "owner").unwrap();
        let (_, token) = s.access().token_create(&me, "t", None).unwrap();
        let (_, a, sa) = s.access().sign_in("Lee", &token, &web()).unwrap().unwrap();
        let (_, b, sb) = s.access().sign_in("Lee", &token, &web()).unwrap().unwrap();
        assert!(s.access().client_revoke(&a.id).unwrap());
        assert!(
            s.access()
                .client_user(&credential(&a.id, &sa))
                .unwrap()
                .is_none()
        );
        assert!(
            s.access()
                .client_user(&credential(&b.id, &sb))
                .unwrap()
                .is_some()
        );
        assert_eq!(s.access().clients(Some(&me)).unwrap().len(), 2);
        let (_, old) = s
            .access()
            .token_create(&me, "old", Some(now() - 1))
            .unwrap();
        assert!(s.access().sign_in("Lee", &old, &web()).unwrap().is_none());
    }

    #[test]
    fn an_ambiguous_name_signs_nobody_in() {
        let (_d, s) = store();
        let a = s.users().create("oidc:a", "Sam", None, "owner").unwrap();
        s.users().create("oidc:b", "Sam", None, "member").unwrap();
        let (_, token) = s.access().token_create(&a, "t", None).unwrap();
        assert!(s.access().sign_in("Sam", &token, &web()).unwrap().is_none());
        assert!(
            s.access()
                .sign_in("oidc:a", &token, &web())
                .unwrap()
                .is_some()
        );
    }
}
