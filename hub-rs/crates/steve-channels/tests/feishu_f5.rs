//! F5 acceptance on the fake Feishu: the bot connection's
//! address book read as a full snapshot (department tree, members, leaders, every page), synced into
//! org-chart teams; resigned people leave their teams but stay contacts; a narrowed visibility scope
//! (departments, single users, user groups); permission errors and rate limits; directory events
//! trigger one debounced sync.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::FeishuApp;
use steve_channels::adapters::feishu::directory::{self, ADAPTER, FeishuDirectory, NAME};
use steve_channels::fake::feishu::{
    Department as FDept, Endpoint, FakeFeishu, User as FUser, UserGroup, World, codes, fixture, ids,
};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::contacts::sync::{
    DEBOUNCE_MS, DirConnection, DirectoryAdapter, DirectoryConnections, DirectoryError,
    DirectorySync, JobState, Snapshot, TableConnections,
};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{ApiError, Clock};
use tempfile::TempDir;
use tower::ServiceExt;

struct H {
    env: TestEnv,
    _keys: TempDir,
    conns: Connections,
    fake: FakeFeishu,
    sync: Arc<DirectorySync>,
    tables: Arc<TableConnections>,
    con: String,
}

async fn setup(world: World) -> H {
    let fake = FakeFeishu::start_with(world).await.expect("fake feishu");
    let env = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let cfg = ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None);
    let conns = Connections::builder(env.modules.clone(), vault, cfg)
        .provider(Arc::new(FeishuApp::new()))
        .build();
    let api: Router = connections::routes(conns.clone()).with_state(env.modules.clone());
    let con = connect_bot(&api, fake.base_url()).await;
    // the integrator's wiring
    let tables =
        Arc::new(TableConnections::new().with(ADAPTER, NAME, directory::factory(conns.clone())));
    let sync = DirectorySync::new(env.modules.clone(), tables.clone());
    H {
        env,
        _keys: keys,
        conns,
        fake,
        sync,
        tables,
        con,
    }
}

async fn connect_bot(api: &Router, base: &str) -> String {
    let body = json!({
        "adapter": "feishu_app",
        "fields": {
            "App ID": ids::APP_ID,
            "App Secret": ids::APP_SECRET,
            "Verification Token": "vt",
            "Encrypt Key": "ek",
        },
        "config": {"api_base": base},
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/connections")
        .header(header::HOST, "hub.test:8440")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    req.extensions_mut().insert(caller(WS, ME));
    let res = api.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, 200, "{v}");
    v["id"].as_str().expect("id").to_string()
}

impl H {
    fn dir(&self) -> FeishuDirectory {
        FeishuDirectory::for_cx(self.conns.cx(WS, &self.con).expect("cx"))
    }

    async fn snapshot(&self) -> Result<Snapshot, DirectoryError> {
        self.dir().snapshot().await
    }

    /// The connection as the sync sees it (healthy, `directory.read` on, Feishu adapter).
    fn dir_conn(&self) -> DirConnection {
        self.env
            .store()
            .reader(|c| self.tables.get(c, WS, &self.con))
            .expect("get")
            .expect("a directory connection")
    }

    async fn sync(&self) -> Result<steve_modules::contacts::sync::SyncReport, ApiError> {
        self.sync.sync_connection(&self.dir_conn()).await
    }

    fn rows(&self, sql: &str) -> Vec<String> {
        let sql = sql.to_string();
        self.env
            .store()
            .reader(move |c| {
                let mut st = c.prepare(&sql)?;
                let r = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .expect("rows")
    }

    fn count(&self, sql: &str) -> i64 {
        let sql = sql.to_string();
        self.env
            .store()
            .reader(move |c| {
                Ok::<_, steve_store::StoreError>(c.query_row(&sql, [], |r| r.get::<_, i64>(0))?)
            })
            .expect("count")
    }

    /// Names of the members of the team synced from this department, sorted.
    fn members(&self, dept: &str) -> Vec<String> {
        let mut v = self.rows(&format!(
            "SELECT p.name FROM team_members m JOIN team_details d ON d.party_id = m.team_id
               JOIN parties p ON p.id = m.contact_id WHERE d.external_id = '{dept}'"
        ));
        v.sort();
        v
    }

    fn team_status(&self, dept: &str) -> String {
        self.rows(&format!(
            "SELECT p.status FROM team_details d JOIN parties p ON p.id = d.party_id WHERE d.external_id = '{dept}'"
        ))
        .pop()
        .unwrap_or_default()
    }

    fn contact_exists(&self, name: &str) -> bool {
        self.count(&format!(
            "SELECT COUNT(*) FROM parties WHERE type = 'contact' AND name = '{name}' AND workspace_id = '{WS}'"
        )) == 1
    }

    fn calls(&self, ep: Endpoint) -> usize {
        self.fake.requests_to(ep).len()
    }
}

fn names(s: &Snapshot) -> (Vec<String>, Vec<String>) {
    let mut d: Vec<String> = s.departments.iter().map(|d| d.name.clone()).collect();
    let mut u: Vec<String> = s.users.iter().map(|u| u.name.clone()).collect();
    d.sort();
    u.sort();
    (d, u)
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn sample_tree_becomes_the_snapshot_and_the_org_chart() {
    let h = setup(World::sample()).await;
    let snap = h.snapshot().await.expect("snapshot");

    let plat = snap
        .departments
        .iter()
        .find(|d| d.id == ids::DEPT_PLATFORM)
        .expect("platform");
    assert_eq!(plat.name, "平台组");
    assert_eq!(plat.parent_id.as_deref(), Some(ids::DEPT_ENG));
    assert_eq!(plat.leader_id.as_deref(), Some(ids::ALICE.union_id));
    let eng = snap
        .departments
        .iter()
        .find(|d| d.id == ids::DEPT_ENG)
        .expect("eng");
    assert_eq!(eng.parent_id, None, "the root \"0\" is not a parent");
    assert_eq!(snap.departments.len(), 3);

    let alice = snap
        .users
        .iter()
        .find(|u| u.id == ids::ALICE.union_id)
        .expect("alice");
    assert_eq!(alice.extra_ids, vec![ids::ALICE.open_id.to_string()]);
    assert_eq!(alice.name, "王小丽");
    assert_eq!(alice.title.as_deref(), Some("后端工程师"));
    assert_eq!(alice.leader_id.as_deref(), Some(ids::OWNER.union_id));
    assert_eq!(alice.department_ids, vec![ids::DEPT_PLATFORM.to_string()]);
    assert_eq!(
        alice.emails,
        s(&["alice@corp.example.com", "alice@example.com"])
    );
    assert!(!alice.departed);
    let carol = snap
        .users
        .iter()
        .find(|u| u.id == ids::CAROL.union_id)
        .expect("carol");
    assert!(carol.departed);
    assert_eq!(snap.users.len(), 4);
    // every call used union ids and open department ids
    for r in h.fake.requests_to(Endpoint::UsersByDepartment) {
        assert!(r.query.contains("user_id_type=union_id"), "{}", r.query);
        assert!(r.query.contains("department_id_type=open_department_id"));
    }

    // through the sync port: three teams, Carol (resigned before she was ever synced) not imported,
    // Alice leads 平台组
    let r = h.sync().await.expect("sync");
    assert_eq!((r.teams, r.contacts), (3, 3), "{r:?}");
    assert_eq!(h.members(ids::DEPT_PLATFORM), s(&["王小丽"]));
    assert_eq!(h.members(ids::DEPT_ENG), s(&["李鹏翔"]));
    assert_eq!(h.members(ids::DEPT_SALES), s(&["张伟"]));
    assert!(!h.contact_exists("陈静"));
    assert_eq!(
        h.rows(&format!(
            "SELECT m.role FROM team_members m JOIN team_details d ON d.party_id = m.team_id
               WHERE d.external_id = '{}'",
            ids::DEPT_PLATFORM
        )),
        s(&["lead"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resignation_and_a_deleted_department_on_the_next_sync() {
    let h = setup(World::sample()).await;
    h.sync().await.expect("first sync");

    h.fake.world(|w| {
        for u in w.users.iter_mut() {
            if u.ids.open_id == ids::ALICE.open_id {
                u.is_resigned = true;
            }
        }
        for d in w.departments.iter_mut() {
            if d.open_department_id == ids::DEPT_SALES {
                d.is_deleted = true;
            }
        }
    });
    let r = h.sync().await.expect("second sync");
    assert_eq!((r.dissolved, r.departed), (1, 1), "{r:?}");
    assert_eq!(h.team_status(ids::DEPT_SALES), "dissolved");
    assert!(h.members(ids::DEPT_PLATFORM).is_empty(), "Alice left");
    assert!(h.contact_exists("王小丽"), "contacts are never deleted");
    assert!(h.contact_exists("张伟"));
}

/// 60 departments in a chain of subtrees and 120 people in one department: more than one page each.
fn big_world() -> World {
    let mut w = World::sample();
    for i in 0..60 {
        let parent = if i % 2 == 0 {
            ids::DEPT_ENG.to_string()
        } else {
            format!("od-big{:03}", i - 1)
        };
        w.departments.push(FDept {
            open_department_id: format!("od-big{i:03}"),
            department_id: format!("B{i:03}"),
            name: format!("小组{i:03}"),
            parent_department_id: parent,
            leader_open_id: None,
            is_deleted: false,
            order: "0".into(),
        });
    }
    let template = w.users[1].clone();
    for i in 0..120 {
        let mut u = template.clone();
        u.ids.open_id = format!("ou_big{i:03}");
        u.ids.union_id = format!("on_big{i:03}");
        u.ids.user_id = format!("u_big{i:03}");
        u.name = format!("员工{i:03}");
        u.email = format!("big{i:03}@example.com");
        u.enterprise_email = String::new();
        u.department_ids = vec!["od-big000".into()];
        // a few also sit in a second department
        if i < 5 {
            u.department_ids.push(ids::DEPT_SALES.into());
        }
        w.users.push(u);
    }
    w
}

#[tokio::test(flavor = "multi_thread")]
async fn every_page_is_read() {
    let h = setup(big_world()).await;
    let snap = h.snapshot().await.expect("snapshot");
    assert_eq!(snap.departments.len(), 63);
    assert_eq!(
        snap.users.len(),
        124,
        "people in two departments appear once"
    );
    let twice = snap
        .users
        .iter()
        .find(|u| u.id == "on_big000")
        .expect("big000");
    assert_eq!(
        twice.department_ids,
        s(&["od-big000", ids::DEPT_SALES]),
        "{twice:?}"
    );
    // 63 departments at 50 a page; 120 people at 50 a page
    let children = h.fake.requests_to(Endpoint::DepartmentChildren);
    assert_eq!(children.len(), 2);
    assert!(children[1].query.contains("page_token="));
    let big = h
        .fake
        .requests_to(Endpoint::UsersByDepartment)
        .into_iter()
        .filter(|r| r.query.contains("department_id=od-big000"))
        .count();
    assert_eq!(big, 3);

    let r = h.sync().await.expect("sync");
    assert_eq!(r.teams, 63);
    assert_eq!(h.members("od-big000").len(), 120);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrow_scope_reads_departments_users_and_user_groups() {
    let mut w = World::sample();
    w.scope_department_ids = vec![ids::DEPT_PLATFORM.into()];
    w.scope_user_ids = vec![ids::BOB.open_id.into()];
    let mut dana: FUser = w.users[2].clone();
    dana.ids.open_id = "ou_dana".into();
    dana.ids.union_id = "on_dana".into();
    dana.ids.user_id = "u_dana".into();
    dana.name = "赵丹".into();
    dana.email = "dana@example.com".into();
    dana.enterprise_email = String::new();
    dana.department_ids = vec![];
    w.users.push(dana);
    w.groups = vec![UserGroup {
        group_id: "g_ops".into(),
        name: "运维值班".into(),
        member_open_ids: vec!["ou_dana".into(), ids::ALICE.open_id.into()],
    }];
    w.scope_group_ids = vec!["g_ops".into()];
    let h = setup(w).await;

    let snap = h.snapshot().await.expect("snapshot");
    let (depts, people) = names(&snap);
    assert_eq!(depts, s(&["平台组"]));
    // 平台组's members, Bob by himself, Dana through the group (Alice only once)
    assert_eq!(people, s(&["张伟", "王小丽", "赵丹", "陈静"]));
    // the scoped department was read by id (its parent is outside the scope), Bob and Dana by id
    assert_eq!(h.calls(Endpoint::GetDepartment), 1);
    assert_eq!(h.calls(Endpoint::GetUser), 2);
    assert_eq!(h.calls(Endpoint::GroupMembers), 1);
    assert!(
        h.fake
            .requests_to(Endpoint::UsersByDepartment)
            .iter()
            .all(|r| !r.query.contains("department_id=0&")),
        "the root's own members are only read with the whole company in scope"
    );

    let r = h.sync().await.expect("sync");
    assert_eq!((r.teams, r.contacts), (1, 3), "{r:?}");
    assert_eq!(h.members(ids::DEPT_PLATFORM), s(&["王小丽"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_permissions_fail_the_read_and_change_nothing() {
    let h = setup(World::sample()).await;
    h.sync().await.expect("first sync");
    let before = h.count("SELECT COUNT(*) FROM team_members");

    for ep in [
        Endpoint::ContactScopes,
        Endpoint::DepartmentChildren,
        Endpoint::UsersByDepartment,
    ] {
        h.fake.deny(ep);
        match h.snapshot().await {
            Err(DirectoryError::Auth(m)) => {
                assert!(m.contains(&codes::NO_SCOPE.to_string()), "{m}")
            }
            other => panic!("{ep:?}: {other:?}"),
        }
        let e = h.sync().await.expect_err("sync fails");
        assert!(e.to_string().starts_with("通讯录没有授权"), "{e}");
        h.fake.allow(ep);
    }
    assert_eq!(h.count("SELECT COUNT(*) FROM team_members"), before);
    assert_eq!(h.team_status(ids::DEPT_SALES), "active");

    // a scoped department that is no longer visible: 40004, with a hint about the scope settings
    h.fake.world(|w| {
        w.scope_department_ids = vec![ids::DEPT_SALES.into()];
        for d in w.departments.iter_mut() {
            if d.open_department_id == ids::DEPT_SALES {
                d.is_deleted = true;
            }
        }
    });
    match h.snapshot().await {
        Err(DirectoryError::Auth(m)) => {
            assert!(m.contains("40004") && m.contains("通讯录权限范围"), "{m}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(h.team_status(ids::DEPT_SALES), "active");
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_are_waited_out_and_expired_tokens_replaced() {
    let h = setup(World::sample()).await;
    h.fake.set_rate_limit_reset(0);
    h.fake.rate_limit_next(Endpoint::UsersByDepartment, 2);
    let snap = h.snapshot().await.expect("retried");
    assert_eq!(snap.users.len(), 4);
    let limited = h
        .fake
        .requests_to(Endpoint::UsersByDepartment)
        .iter()
        .filter(|r| r.code == codes::RATE_LIMITED)
        .count();
    assert_eq!(limited, 2);

    // more than three in a row: the read fails as unavailable
    h.fake.rate_limit_next(Endpoint::ContactScopes, 4);
    assert!(matches!(
        h.snapshot().await,
        Err(DirectoryError::Unavailable(_))
    ));

    // an expired tenant token is replaced once, transparently
    h.fake.expire_tokens();
    assert!(h.snapshot().await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_events_trigger_one_debounced_sync() {
    let h = setup(World::sample()).await;
    let user_updated = fixture("contact_user_updated").expect("fixture");
    let dept_deleted = fixture("contact_department_deleted").expect("fixture");
    let message = fixture("message_group_at_bot").expect("fixture");

    assert!(!directory::on_event(&h.sync, WS, &h.con, &message));
    assert!(
        h.sync
            .tick(h.env.clock.now() + DEBOUNCE_MS)
            .unwrap()
            .is_empty()
    );

    assert!(directory::on_event(&h.sync, WS, &h.con, &user_updated));
    h.env.clock.advance(DEBOUNCE_MS / 2);
    assert!(directory::on_event(&h.sync, WS, &h.con, &dept_deleted));
    let t = h.env.clock.now();
    assert!(
        h.sync.tick(t + DEBOUNCE_MS - 1).unwrap().is_empty(),
        "still quiet"
    );
    let jobs = h.sync.tick(t + DEBOUNCE_MS).unwrap();
    assert_eq!(jobs.len(), 1, "two events, one sync");
    assert!(h.sync.tick(t + 2 * DEBOUNCE_MS).unwrap().is_empty());

    h.sync.spawn();
    let mut job = None;
    for _ in 0..500 {
        if let Some(j) = h.sync.job(&jobs[0])
            && matches!(j.state, JobState::Done | JobState::Failed)
        {
            job = Some(j);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let job = job.expect("job finished");
    assert_eq!(job.state, JobState::Done, "{:?}", job.errors);
    assert_eq!(job.reason, "event");
    assert_eq!(job.connections, vec![h.con.clone()]);
    assert_eq!(h.members(ids::DEPT_SALES), s(&["张伟"]));
    // the label of the sync is “飞书通讯录”
    assert_eq!(h.dir_conn().label, "飞书通讯录");
}
