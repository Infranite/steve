//! Feishu's address book as a [`DirectoryAdapter`].
//!
//! One snapshot is a full read of what the app can see — the organisation is small, so a full
//! snapshot is simpler than replaying change events. The read follows the contact
//! visibility scope (`contact/v3/scopes`):
//! - departments: when the scope is the whole company (`"0"`), every descendant of the root; else each
//!   scoped department and its descendants (`departments/{id}/children?fetch_child=true` only lists
//!   what is visible either way);
//! - people: the members of every department read (`users/find_by_department`, plus the root's own
//!   members when the whole company is in scope), then users listed in the scope by themselves and the
//!   members of scoped user groups, fetched one by one.
//!
//! User groups only widen who is visible: the sync port has org-chart departments, and a user group is
//! not one (it would become an org-chart team that nobody can edit). Every list is read to the last page.
//! Rate limits wait for `x-ogw-ratelimit-reset` and retry up to three times. Missing
//! scopes and departments outside the scope fail the whole read as [`DirectoryError::Auth`], so the
//! sync changes nothing rather than dissolving teams it merely can't see.
//!
//! Change events (`contact.user.*_v3`, `contact.department.*_v3`, `contact.scope.updated_v3`) don't
//! patch anything: [`on_event`] asks [`DirectorySync::changed`] for one debounced sync.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use steve_modules::ApiError;
use steve_modules::accounts::connections::{ConnCx, Connections, ProviderError};
use steve_modules::contacts::sync::{
    AdapterFactory, Department, DirConnection, DirScope, DirUser, DirectoryAdapter, DirectoryError,
    DirectorySync, Snapshot,
};

use super::client::{FeishuClient, FeishuError, TokenSource};
use super::ratelimit;

/// The accounts system of [`DirUser::id`]: Feishu `union_id`s.
pub const SYSTEM: &str = "feishu";
/// `connections.adapter` of the bot app connection, under which the integrator registers [`factory`].
pub const ADAPTER: &str = "feishu_app";
/// Display name: the sync's label becomes “飞书通讯录”.
pub const NAME: &str = "飞书";

/// Page sizes: the documented maxima (children and find_by_department 50, the rest 100).
const DEPT_PAGE: &str = "50";
const USER_PAGE: &str = "50";
const SCOPE_PAGE: &str = "100";
/// Retries after a rate-limit answer.
const RATE_RETRIES: u32 = 3;
/// The most we wait (one second when the answer has no reset header).
const RATE_WAIT_MAX: Duration = Duration::from_secs(30);
/// Guards against a server that keeps answering `has_more`.
const MAX_PAGES: usize = 10_000;

/// Codes meaning “outside the app's contact scope” (department, user, user group).
const NO_AUTHORITY_CODES: &[i64] = &[40004, 41050, 42002];

/// Directory change events: each one schedules a debounced full sync.
pub const DIRECTORY_EVENTS: &[&str] = &[
    "contact.user.created_v3",
    "contact.user.updated_v3",
    "contact.user.deleted_v3",
    "contact.department.created_v3",
    "contact.department.updated_v3",
    "contact.department.deleted_v3",
    "contact.scope.updated_v3",
];

/// Whether an event type is a directory change ([`DIRECTORY_EVENTS`]).
pub fn is_directory_event(event_type: &str) -> bool {
    DIRECTORY_EVENTS.contains(&event_type)
}

/// For the event dispatcher: a schema 2.0 envelope (`header.event_type`) of a directory change
/// queues one sync of the connection after a minute of quiet. Returns whether it was one.
pub fn on_event(sync: &DirectorySync, ws: &str, connection_id: &str, envelope: &Value) -> bool {
    let t = envelope
        .pointer("/header/event_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_directory_event(t) {
        return false;
    }
    tracing::debug!(
        connection = connection_id,
        event = t,
        "feishu: directory changed"
    );
    sync.changed(ws, connection_id);
    true
}

/// The adapter factory for `TableConnections::with(ADAPTER, NAME, factory(conns))`: each sync reads
/// through the connection's cached tenant token.
pub fn factory(conns: Connections) -> AdapterFactory {
    Arc::new(move |c: &DirConnection| {
        let cx = conns.cx(&c.workspace_id, &c.id).map_err(|e| match e {
            steve_modules::vault::VaultError::Api(a) => a,
            e => ApiError::rule(e.to_string()),
        })?;
        Ok(Arc::new(FeishuDirectory::for_cx(cx)) as Arc<dyn DirectoryAdapter>)
    })
}

/// The address book behind one bot app connection.
pub struct FeishuDirectory {
    client: FeishuClient,
    tokens: Arc<dyn TokenSource>,
    /// What the last snapshot learned about the visibility range and missing permissions.
    seen_scope: std::sync::Mutex<Option<DirScope>>,
    missing: std::sync::Mutex<Vec<(String, String)>>,
}

/// The permission a directory path needs (for “缺权限” on the 接入 page).
fn scope_of(path: &str) -> (&'static str, &'static str) {
    if path.contains("/contact/v3/departments") {
        ("contact:department.base:readonly", "通讯录同步：部门")
    } else if path.contains("/contact/v3/users") {
        ("contact:user.base:readonly", "通讯录同步：成员")
    } else if path.contains("/contact/v3/group") {
        ("contact:group:readonly", "通讯录同步：用户组")
    } else {
        ("contact:contact.base:readonly", "通讯录同步")
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FeishuDirectory {
    pub fn new(client: FeishuClient, tokens: Arc<dyn TokenSource>) -> Self {
        Self {
            client,
            tokens,
            seen_scope: std::sync::Mutex::new(None),
            missing: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Through a connection: its base URL and its tenant token.
    pub fn for_cx(cx: ConnCx) -> Self {
        Self::new(FeishuClient::for_cx(&cx), Arc::new(cx))
    }

    /// One GET, waiting out rate limits.
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, DirectoryError> {
        let what = format!("directory read {path}");
        let call = || self.client.get(self.tokens.as_ref(), path, query);
        match ratelimit::with_retries_capped(RATE_RETRIES, RATE_WAIT_MAX, &what, call).await {
            // the `data` member, taken rather than copied
            Ok((mut v, _)) => Ok(match v.get_mut("data") {
                Some(d) => d.take(),
                None => v,
            }),
            Err(e) => {
                if e.is_no_scope() {
                    let (sc, why) = scope_of(path);
                    let mut m = lock(&self.missing);
                    if !m.iter().any(|(x, _)| x == sc) {
                        m.push((sc.into(), why.into()));
                    }
                }
                Err(dir_error(e))
            }
        }
    }

    /// Every page of a list; `f` gets each page's `data`.
    async fn pages(
        &self,
        path: &str,
        query: &[(&str, &str)],
        mut f: impl FnMut(&Value),
    ) -> Result<(), DirectoryError> {
        let mut token = String::new();
        for _ in 0..MAX_PAGES {
            let d = {
                let mut q = query.to_vec();
                if !token.is_empty() {
                    q.push(("page_token", &token));
                }
                self.get(path, &q).await?
            };
            f(&d);
            let more = d.get("has_more").and_then(Value::as_bool).unwrap_or(false);
            let next = d
                .get("page_token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !more {
                return Ok(());
            }
            if next.is_empty() || next == token {
                return Err(DirectoryError::Unavailable("飞书的分页标记不对".into()));
            }
            token = next;
        }
        Err(DirectoryError::Unavailable("通讯录分页太多".into()))
    }

    async fn read_scope(&self) -> Result<Scope, DirectoryError> {
        let mut s = Scope::default();
        self.pages(
            "/open-apis/contact/v3/scopes",
            &[
                ("user_id_type", "union_id"),
                ("department_id_type", "open_department_id"),
                ("page_size", SCOPE_PAGE),
            ],
            |d| {
                s.departments.extend(strings(d, "department_ids"));
                s.users.extend(strings(d, "user_ids"));
                s.groups.extend(strings(d, "group_ids"));
            },
        )
        .await?;
        Ok(s)
    }

    /// Every visible descendant of `parent`.
    async fn descendants(&self, parent: &str, out: &mut Departments) -> Result<(), DirectoryError> {
        let path = format!("/open-apis/contact/v3/departments/{}/children", seg(parent));
        self.pages(
            &path,
            &[
                ("department_id_type", "open_department_id"),
                ("user_id_type", "union_id"),
                ("fetch_child", "true"),
                ("page_size", DEPT_PAGE),
            ],
            |d| {
                for item in items(d, "items") {
                    out.add(item);
                }
            },
        )
        .await
    }

    async fn department(&self, id: &str, out: &mut Departments) -> Result<(), DirectoryError> {
        let d = self
            .get(
                &format!("/open-apis/contact/v3/departments/{}", seg(id)),
                &[
                    ("department_id_type", "open_department_id"),
                    ("user_id_type", "union_id"),
                ],
            )
            .await?;
        if let Some(dep) = d.get("department") {
            out.add(dep);
        }
        Ok(())
    }

    async fn members(&self, department: &str, out: &mut Users) -> Result<(), DirectoryError> {
        self.pages(
            "/open-apis/contact/v3/users/find_by_department",
            &[
                ("department_id", department),
                ("department_id_type", "open_department_id"),
                ("user_id_type", "union_id"),
                ("page_size", USER_PAGE),
            ],
            |d| {
                for item in items(d, "items") {
                    out.add(item);
                }
            },
        )
        .await
    }

    async fn user(&self, union_id: &str, out: &mut Users) -> Result<(), DirectoryError> {
        let d = self
            .get(
                &format!("/open-apis/contact/v3/users/{}", seg(union_id)),
                &[
                    ("user_id_type", "union_id"),
                    ("department_id_type", "open_department_id"),
                ],
            )
            .await?;
        if let Some(u) = d.get("user") {
            out.add(u);
        }
        Ok(())
    }

    async fn group_members(&self, group: &str) -> Result<Vec<String>, DirectoryError> {
        let mut ids = Vec::new();
        self.pages(
            &format!(
                "/open-apis/contact/v3/group/{}/member/simplelist",
                seg(group)
            ),
            &[
                ("member_id_type", "union_id"),
                ("member_type", "user"),
                ("page_size", SCOPE_PAGE),
            ],
            |d| {
                for m in items(d, "memberlist") {
                    let is_user = m
                        .get("member_type")
                        .and_then(Value::as_str)
                        .is_none_or(|t| t == "user");
                    if let Some(id) = text(m, "member_id")
                        && is_user
                    {
                        ids.push(id);
                    }
                }
            },
        )
        .await?;
        Ok(ids)
    }
}

#[async_trait]
impl DirectoryAdapter for FeishuDirectory {
    fn system(&self) -> &str {
        SYSTEM
    }

    fn scope(&self) -> Option<DirScope> {
        *lock(&self.seen_scope)
    }

    fn missing_scopes(&self) -> Vec<(String, String)> {
        lock(&self.missing).clone()
    }

    async fn snapshot(&self) -> Result<Snapshot, DirectoryError> {
        lock(&self.missing).clear();
        let scope = self.read_scope().await?;
        let whole = scope.departments.iter().any(|d| d == "0");
        *lock(&self.seen_scope) = Some(DirScope {
            whole,
            departments: if whole { 0 } else { scope.departments.len() },
            users: scope.users.len() + scope.groups.len(),
        });

        let mut depts = Departments::default();
        if whole {
            self.descendants("0", &mut depts).await?;
        } else {
            for d in &scope.departments {
                if !depts.has(d) {
                    self.department(d, &mut depts).await?;
                }
                self.descendants(d, &mut depts).await?;
            }
        }

        let mut users = Users::default();
        if whole {
            self.members("0", &mut users).await?;
        }
        let ids: Vec<String> = depts.list.iter().map(|d| d.id.clone()).collect();
        for d in &ids {
            self.members(d, &mut users).await?;
        }
        let mut singles: Vec<String> = scope.users.clone();
        for g in &scope.groups {
            singles.extend(self.group_members(g).await?);
        }
        for u in singles {
            if !users.has(&u) {
                self.user(&u, &mut users).await?;
            }
        }
        Ok(Snapshot {
            departments: depts.list,
            users: users.list,
        })
    }
}

/// `contact/v3/scopes`, all pages.
#[derive(Default)]
struct Scope {
    departments: Vec<String>,
    users: Vec<String>,
    groups: Vec<String>,
}

/// Departments in first-seen order, deleted ones left out.
#[derive(Default)]
struct Departments {
    list: Vec<Department>,
    seen: BTreeSet<String>,
}

impl Departments {
    fn has(&self, id: &str) -> bool {
        self.seen.contains(id)
    }
    fn add(&mut self, v: &Value) {
        if let Some(d) = department(v)
            && self.seen.insert(d.id.clone())
        {
            self.list.push(d);
        }
    }
}

/// People by `union_id`, in first-seen order; someone in several departments appears once.
#[derive(Default)]
struct Users {
    list: Vec<DirUser>,
    at: BTreeMap<String, usize>,
}

impl Users {
    fn has(&self, id: &str) -> bool {
        self.at.contains_key(id)
    }
    fn add(&mut self, v: &Value) {
        let Some(u) = user(v) else { return };
        match self.at.get(&u.id) {
            Some(&i) => {
                if let Some(old) = self.list.get_mut(i) {
                    for d in u.department_ids {
                        if !old.department_ids.contains(&d) {
                            old.department_ids.push(d);
                        }
                    }
                }
            }
            None => {
                self.at.insert(u.id.clone(), self.list.len());
                self.list.push(u);
            }
        }
    }
}

/// A contact-v3 department (`open_department_id`), or `None` when deleted or without an id.
pub fn department(v: &Value) -> Option<Department> {
    if v.pointer("/status/is_deleted").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let id = text(v, "open_department_id")?;
    Some(Department {
        name: text(v, "name").unwrap_or_else(|| id.clone()),
        parent_id: text(v, "parent_department_id").filter(|p| p != "0"),
        leader_id: text(v, "leader_user_id"),
        id,
    })
}

/// A contact-v3 user read with `user_id_type=union_id`, or `None` without a `union_id`.
pub fn user(v: &Value) -> Option<DirUser> {
    let id = text(v, "union_id")?;
    let mut emails = Vec::new();
    for k in ["enterprise_email", "email"] {
        if let Some(e) = text(v, k)
            && !emails.iter().any(|x: &String| x.eq_ignore_ascii_case(&e))
        {
            emails.push(e);
        }
    }
    Some(DirUser {
        extra_ids: text(v, "open_id").into_iter().collect(),
        name: text(v, "name")
            .or_else(|| text(v, "en_name"))
            .unwrap_or_else(|| id.clone()),
        emails,
        title: text(v, "job_title"),
        leader_id: text(v, "leader_user_id"),
        department_ids: strings(v, "department_ids")
            .into_iter()
            .filter(|d| d != "0")
            .collect(),
        departed: v.pointer("/status/is_resigned").and_then(Value::as_bool) == Some(true),
        id,
    })
}

/// A Feishu error as the sync port's: refused credentials, missing scopes and ids outside the
/// contact scope are “no authorization”; the rest is “unavailable”.
pub fn dir_error(e: FeishuError) -> DirectoryError {
    let msg = e.to_string();
    match e {
        FeishuError::TokenInvalid { .. } | FeishuError::NoScope { .. } => DirectoryError::Auth(msg),
        FeishuError::Api { code, .. } if NO_AUTHORITY_CODES.contains(&code) => {
            DirectoryError::Auth(format!("{msg}，请在飞书开放平台的“通讯录权限范围”里检查"))
        }
        FeishuError::Token(ProviderError::Auth(m) | ProviderError::Config(m)) => {
            DirectoryError::Auth(m)
        }
        _ => DirectoryError::Unavailable(msg),
    }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn strings(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn items<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

/// An id as one path segment (ids are `[A-Za-z0-9_-]` in practice; anything else is escaped).
fn seg(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_departments() {
        let d = department(&json!({
            "name": "平台组", "open_department_id": "od-p", "parent_department_id": "od-e",
            "leader_user_id": "on_a", "status": {"is_deleted": false}
        }))
        .unwrap();
        assert_eq!(
            d,
            Department {
                id: "od-p".into(),
                name: "平台组".into(),
                parent_id: Some("od-e".into()),
                leader_id: Some("on_a".into()),
            }
        );
        let top = department(
            &json!({"name": "研发部", "open_department_id": "od-e", "parent_department_id": "0"}),
        )
        .unwrap();
        assert_eq!(top.parent_id, None);
        assert_eq!(top.leader_id, None);
        assert!(
            department(&json!({"open_department_id": "x", "status": {"is_deleted": true}}))
                .is_none()
        );
        assert!(department(&json!({"name": "无 id"})).is_none());
    }

    #[test]
    fn maps_users() {
        let u = user(&json!({
            "union_id": "on_a", "open_id": "ou_a", "name": "王小丽", "en_name": "Alice",
            "email": "Alice@corp.example.com", "enterprise_email": "alice@corp.example.com",
            "job_title": "后端工程师", "leader_user_id": "on_o", "department_ids": ["od-p", "0"],
            "status": {"is_resigned": false}
        }))
        .unwrap();
        assert_eq!(
            u,
            DirUser {
                id: "on_a".into(),
                extra_ids: vec!["ou_a".into()],
                name: "王小丽".into(),
                emails: vec!["alice@corp.example.com".into()],
                title: Some("后端工程师".into()),
                leader_id: Some("on_o".into()),
                department_ids: vec!["od-p".into()],
                departed: false,
            }
        );
        let r = user(&json!({"union_id": "on_c", "en_name": "Carol", "job_title": "", "status": {"is_resigned": true}})).unwrap();
        assert!(r.departed);
        assert_eq!((r.name.as_str(), r.title), ("Carol", None));
        assert!(user(&json!({"open_id": "ou_x"})).is_none());
    }

    #[test]
    fn same_person_in_two_departments_is_one_user() {
        let mut us = Users::default();
        us.add(&json!({"union_id": "on_a", "name": "A", "department_ids": ["d1"]}));
        us.add(&json!({"union_id": "on_a", "name": "A", "department_ids": ["d1", "d2"]}));
        assert_eq!(us.list.len(), 1);
        assert_eq!(us.list[0].department_ids, vec!["d1", "d2"]);
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(
            dir_error(FeishuError::NoScope {
                code: 99991672,
                msg: String::new(),
                scopes: Vec::new(),
                help_url: None,
            }),
            DirectoryError::Auth(_)
        ));
        assert!(matches!(
            dir_error(FeishuError::Api { code: 40004, msg: "no dept authority error".into(), status: 400 }),
            DirectoryError::Auth(m) if m.contains("通讯录权限范围")
        ));
        assert!(matches!(
            dir_error(FeishuError::RateLimited {
                code: 99991400,
                reset: None
            }),
            DirectoryError::Unavailable(_)
        ));
        assert!(matches!(
            dir_error(FeishuError::Server(502)),
            DirectoryError::Unavailable(_)
        ));
        assert!(matches!(
            dir_error(FeishuError::Token(ProviderError::Transient("x".into()))),
            DirectoryError::Unavailable(_)
        ));
    }

    #[test]
    fn directory_events() {
        assert!(is_directory_event("contact.department.deleted_v3"));
        assert!(is_directory_event("contact.scope.updated_v3"));
        assert!(!is_directory_event("im.message.receive_v1"));
        assert!(!is_directory_event(""));
    }
}
