//! steve-auth's [`UserStore`](steve_auth::UserStore) on steve-store.

use serde_json::json;
use steve_store::{Store, StoreError, User, WS};

#[derive(Clone)]
pub struct AuthStore(pub Store);

impl steve_auth::UserStore for AuthStore {
    type User = User;
    type Error = StoreError;

    fn user_id_by_subject(&self, subject: &str) -> Result<Option<String>, StoreError> {
        Ok(self.0.users().by_subject(subject)?.map(|u| u.id))
    }
    fn user_count(&self) -> Result<i64, StoreError> {
        self.0.users().count()
    }
    fn create_owner(
        &self,
        subject: &str,
        name: &str,
        email: Option<&str>,
    ) -> Result<String, StoreError> {
        let id = self.0.users().create(subject, name, email, "owner")?;
        self.0
            .audit(&id, "user.create", &id, &json!({ "role": "owner" }), WS)?;
        Ok(id)
    }
    fn create_session(&self, user_id: &str, token: &str, ttl_ms: i64) -> Result<(), StoreError> {
        self.0.auth_sessions().create(user_id, token, ttl_ms)
    }
    fn session_user(&self, token: &str) -> Result<Option<User>, StoreError> {
        self.0.auth_sessions().user(token)
    }
    fn drop_session(&self, token: &str) -> Result<(), StoreError> {
        self.0.auth_sessions().drop(token)
    }
}
