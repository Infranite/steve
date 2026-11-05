//! Slice P6 acceptance: a fake directory synced twice (the second time one
//! department is gone and one person moved) → the team is dissolved, member rows follow, no contact is
//! deleted — plus renames, departures, returns, matching people to existing contacts by email and by
//! handle, scheduling (POST /teams/sync, daily 06:00, debounced events) and failure paths.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use serde_json::{Value, json};
use steve_modules::contacts::sync::{
    CATCH_UP_FIRST_MS, CATCH_UP_MS, DAILY_HOUR, DEBOUNCE_MS, Department, DirConnection, DirUser,
    DirectoryAdapter, DirectoryError, DirectorySync, EMPTY_SNAPSHOT, FakeDirectory, Job, JobState,
    Snapshot, TableConnections, next_daily,
};
use steve_modules::contacts::{self, Ports};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::{ApiResult, Caller, Clock, accounts};

const CONN: &str = "con_fs";

fn me() -> Caller {
    caller(WS, ME)
}

fn dept(id: &str, name: &str, parent: Option<&str>, leader: Option<&str>) -> Department {
    Department {
        id: id.into(),
        name: name.into(),
        parent_id: parent.map(String::from),
        leader_id: leader.map(String::from),
    }
}

fn user(id: &str, name: &str, depts: &[&str], leader: Option<&str>) -> DirUser {
    DirUser {
        id: format!("on_{id}"),
        extra_ids: vec![format!("ou_{id}")],
        name: name.into(),
        emails: Vec::new(),
        title: Some("工程师".into()),
        leader_id: leader.map(|l| format!("on_{l}")),
        department_ids: depts.iter().map(|d| d.to_string()).collect(),
        departed: false,
    }
}

/// 研发部 (李雷 leads) ⊃ 平台组 (王芳 leads, 韩梅梅); 销售部 (张伟).
fn first() -> Snapshot {
    Snapshot {
        departments: vec![
            dept("od-eng", "研发部", Some("0"), Some("on_lilei")),
            dept("od-plat", "平台组", Some("od-eng"), Some("on_wangfang")),
            dept("od-sales", "销售部", Some("0"), None),
        ],
        users: vec![
            user("lilei", "李雷", &["od-eng"], None),
            user("wangfang", "王芳", &["od-plat"], Some("lilei")),
            user("hanmeimei", "韩梅梅", &["od-plat"], Some("wangfang")),
            user("zhangwei", "张伟", &["od-sales"], None),
        ],
    }
}

struct Fx {
    t: TestEnv,
    r: Router,
    sync: Arc<DirectorySync>,
    dir: Arc<FakeDirectory>,
}

fn fixture(snapshot: Snapshot) -> Fx {
    let t = TestEnv::new(accounts::register);
    let dir = Arc::new(FakeDirectory::new("feishu", snapshot));
    let d = dir.clone();
    let conns = TableConnections::new().with(
        "feishu",
        "飞书",
        Arc::new(
            move |_: &DirConnection| -> ApiResult<Arc<dyn DirectoryAdapter>> { Ok(d.clone()) },
        ),
    );
    let sync = DirectorySync::new(t.modules.clone(), Arc::new(conns));
    let mut ports: Ports = accounts::ports();
    ports.sync = Some(sync.clone());
    let r = contacts::routes(ports)
        .merge(accounts::routes())
        .with_state(t.modules.clone());
    Fx { t, r, sync, dir }
}

impl Fx {
    async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(&self.r, Some(&me()), method, path, body.as_ref()).await
    }

    fn exec(&self, sql: &str) {
        self.t
            .store()
            .with_conn(|c| Ok(c.execute_batch(sql)?))
            .expect("exec");
    }

    fn count(&self, sql: &str) -> i64 {
        self.t
            .store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .expect("count")
    }

    fn text(&self, sql: &str) -> String {
        self.t
            .store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .expect("text")
    }

    /// A Feishu app connection of the own organization in `ws`.
    async fn connect(&self, ws: &str, id: &str, caps: &str) {
        let (s, _) = call(&self.r, Some(&caller(ws, ME)), "GET", "/me/contact", None).await;
        assert_eq!(s, 200);
        self.exec(&format!(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               SELECT 'acc_{id}', '{ws}', 'feishu_app', 'feishu_app', id, 'app_bot', 'cli_{id}', 'cli_{id}', 'connect', 1, 1
                 FROM parties WHERE workspace_id = '{ws}' AND org_kind = 'self';
             INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
               VALUES ('{id}', '{ws}', 'acc_{id}', 'feishu', 'credential', '{caps}', 'usr_dev', 1, 1);"
        ));
    }

    /// `POST /teams/sync`, then waits for the job.
    async fn sync_now(&self) -> Job {
        let (s, b) = self.req("POST", "/teams/sync", None).await;
        assert_eq!(s, 202, "{b}");
        self.wait(b["job_id"].as_str().expect("job id")).await
    }

    async fn wait(&self, id: &str) -> Job {
        for _ in 0..500 {
            if let Some(j) = self.sync.job(id)
                && matches!(j.state, JobState::Done | JobState::Failed)
            {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job {id} did not finish");
    }

    fn team_id(&self, ext: &str) -> String {
        self.text(&format!(
            "SELECT party_id FROM team_details WHERE external_id = '{ext}'"
        ))
    }

    fn contact_id(&self, union_id: &str) -> String {
        self.text(&format!(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.family = 'feishu_user' AND h.handle = '{union_id}'"
        ))
    }

    async fn team(&self, ext: &str) -> Value {
        let (s, b) = self
            .req("GET", &format!("/teams/{}", self.team_id(ext)), None)
            .await;
        assert_eq!(s, 200, "{b}");
        b
    }

    async fn member_names(&self, ext: &str) -> Vec<(String, String)> {
        self.team(ext).await["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| {
                (
                    m["name"].as_str().unwrap_or("").to_string(),
                    m["role"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }
}

fn pair(n: &str, r: &str) -> (String, String) {
    (n.into(), r.into())
}

#[tokio::test(flavor = "multi_thread")]
async fn two_snapshots_dissolve_a_department_and_move_a_person() {
    let fx = fixture(first());
    fx.sync.spawn();
    fx.connect(WS, CONN, r#"["msg.receive","directory.read"]"#)
        .await;

    let job = fx.sync_now().await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.errors);
    let r = &job.reports[0].1;
    assert_eq!((r.teams, r.contacts, r.added, r.removed), (3, 4, 4, 0));

    // the org chart in tree order, owned by the own organization, with the sync label
    let (_, list) = fx.req("GET", "/teams", None).await;
    let chart = list["chart"].as_array().expect("chart");
    let names: Vec<(&str, u64)> = chart
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["depth"].as_u64().unwrap()))
        .collect();
    assert_eq!(names, vec![("研发部", 0), ("平台组", 1), ("销售部", 0)]);
    assert_eq!(chart[0]["headcount"], 3, "研发部 counts 平台组 too");
    assert_eq!(list["sync"]["connection_id"], CONN);
    assert_eq!(list["sync"]["label"], "飞书通讯录");
    let own =
        fx.text("SELECT id FROM parties WHERE org_kind = 'self' AND workspace_id = 'default'");
    assert!(
        chart
            .iter()
            .all(|t| t["org_id"] == own.as_str() && t["kind"] == "chart")
    );
    assert_eq!(
        fx.member_names("od-plat").await,
        vec![pair("王芳", "lead"), pair("韩梅梅", "member")]
    );
    let plat = fx.team("od-plat").await;
    assert!(
        plat["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["source"] == "sync")
    );
    assert!(
        plat["source_label"]
            .as_str()
            .unwrap()
            .starts_with("飞书通讯录 · ")
    );

    // contacts: real, source sync, with the Feishu account (union_id) and the app's open_id
    let wang = fx.contact_id("on_wangfang");
    let (_, c) = fx.req("GET", &format!("/contacts/{wang}"), None).await;
    assert_eq!(c["kind"], "real");
    assert_eq!(c["source"], "sync");
    assert_eq!(c["org"]["id"], own.as_str());
    assert_eq!(c["org"]["title"], "工程师");
    assert_eq!(c["jobs"][0]["dept"], "研发部 / 平台组");
    assert_eq!(c["jobs"][0]["source"], "sync");
    assert_eq!(c["reports_to"][0]["name"], "李雷");
    assert_eq!(
        fx.text("SELECT connection_id FROM account_handles WHERE handle = 'ou_wangfang'"),
        CONN
    );
    assert_eq!(
        fx.text(&format!(
            "SELECT source FROM accounts WHERE party_id = '{wang}'"
        )),
        "sync"
    );
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM decisions"),
        0,
        "no proposals"
    );
    assert!(fx.count("SELECT last_sync FROM connections WHERE id = 'con_fs'") > 0);

    // second snapshot: 销售部 gone, 张伟 moved to 研发部
    let zhang = fx.contact_id("on_zhangwei");
    let sales = fx.team_id("od-sales");
    fx.dir.update(|s| {
        s.departments.retain(|d| d.id != "od-sales");
        s.users[3].department_ids = vec!["od-eng".into()];
    });
    let contacts_before = fx.count("SELECT COUNT(*) FROM parties WHERE type = 'contact'");
    let job = fx.sync_now().await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.errors);
    let r = &job.reports[0].1;
    assert_eq!((r.dissolved, r.added, r.removed, r.teams), (1, 1, 1, 0));

    let (_, t) = fx.req("GET", &format!("/teams/{sales}"), None).await;
    assert_eq!(t["status"], "dissolved");
    let (_, list) = fx.req("GET", "/teams", None).await;
    assert_eq!(list["chart"].as_array().unwrap().len(), 2);
    assert_eq!(
        fx.member_names("od-eng").await,
        vec![pair("李雷", "lead"), pair("张伟", "member")]
    );
    assert_eq!(fx.team("od-eng").await["headcount"], 4);
    assert_eq!(
        fx.count(&format!(
            "SELECT COUNT(*) FROM team_members WHERE contact_id = '{zhang}' AND team_id = '{sales}'"
        )),
        0
    );
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM parties WHERE type = 'contact'"),
        contacts_before,
        "no contact deleted"
    );
    let (s, _) = fx.req("GET", &format!("/contacts/{zhang}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM audit WHERE action = 'team.sync'"),
        2
    );
    let detail: Value = serde_json::from_str(
        &fx.text("SELECT detail FROM audit WHERE action = 'team.sync' ORDER BY rowid DESC LIMIT 1"),
    )
    .unwrap();
    assert_eq!(detail["dissolved"], 1);
    assert_eq!(detail["added"], 1);
    assert_eq!(detail["removed"], 1);

    // the same snapshot again changes nothing
    let job = fx.sync_now().await;
    let r = &job.reports[0].1;
    assert_eq!(
        (r.added, r.removed, r.dissolved, r.renamed, r.contacts),
        (0, 0, 0, 0, 0)
    );

    // 销售部 comes back: the same team is reactivated, with 张伟 again
    fx.dir.update(|s| {
        s.departments
            .push(dept("od-sales", "销售部", Some("0"), Some("on_zhangwei")));
        s.users[3].department_ids = vec!["od-sales".into()];
    });
    let job = fx.sync_now().await;
    assert_eq!(job.reports[0].1.restored, 1);
    assert_eq!(fx.team_id("od-sales"), sales);
    assert_eq!(
        fx.member_names("od-sales").await,
        vec![pair("张伟", "lead")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_departures_and_returns() {
    let fx = fixture(first());
    fx.sync.spawn();
    fx.connect(WS, CONN, r#"["directory.read"]"#).await;
    fx.sync_now().await;
    let plat = fx.team_id("od-plat");
    let han = fx.contact_id("on_hanmeimei");
    let li = fx.contact_id("on_lilei");
    // a name you set on a contact you created is not overwritten; a synced one follows the directory
    fx.exec(&format!(
        "UPDATE parties SET source = 'manual' WHERE id = '{li}'"
    ));
    fx.dir.update(|s| {
        s.departments[1].name = "基础平台组".into();
        s.users[0].name = "李雷雷".into();
        s.users[1].name = "王芳芳".into();
        s.users[2].departed = true; // 韩梅梅 leaves
    });
    let job = fx.sync_now().await;
    let r = &job.reports[0].1;
    assert_eq!((r.renamed, r.departed, r.removed), (2, 1, 1), "{r:?}");

    let t = fx.team("od-plat").await;
    assert_eq!(t["name"], "基础平台组");
    assert_eq!(t["revision"], 2, "rename writes a revision");
    assert_eq!(
        fx.count(&format!(
            "SELECT COUNT(*) FROM revisions WHERE object_id = '{plat}'"
        )),
        1
    );
    assert_eq!(
        fx.member_names("od-plat").await,
        vec![pair("王芳芳", "lead")]
    );
    let (_, li_c) = fx.req("GET", &format!("/contacts/{li}"), None).await;
    assert_eq!(li_c["name"], "李雷");

    // the person who left: still a contact, job closed, out of every team, no reports_to
    let (s, c) = fx.req("GET", &format!("/contacts/{han}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(c["status"], "active");
    assert!(c["jobs"][0]["to"].is_string(), "{c}");
    assert_eq!(c["teams"], json!([]));
    assert_eq!(c["reports_to"], json!([]));
    assert_eq!(
        fx.count(&format!(
            "SELECT COUNT(*) FROM accounts WHERE party_id = '{han}'"
        )),
        1
    );

    // and comes back: the job reopens with the old interval in history
    fx.dir.update(|s| s.users[2].departed = false);
    fx.sync_now().await;
    let (_, c) = fx.req("GET", &format!("/contacts/{han}"), None).await;
    assert!(c["jobs"][0]["to"].is_null(), "{c}");
    let props: Value = serde_json::from_str(&fx.text(&format!(
        "SELECT props FROM links WHERE subject_id = '{han}' AND predicate = 'works_at'"
    )))
    .unwrap();
    assert_eq!(props["history"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        fx.member_names("od-plat").await,
        vec![pair("王芳芳", "lead"), pair("韩梅梅", "member")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn people_join_contacts_you_already_have() {
    let mut snap = first();
    snap.users[0].emails = vec!["Dev@Corp.example.com".into()]; // 李雷 is “我”
    snap.users[1].emails = vec!["wang.fang@corp.example.com".into()];
    let fx = fixture(snap);
    fx.sync.spawn();
    fx.connect(WS, CONN, r#"["directory.read"]"#).await;
    let (_, me_c) = fx.req("GET", "/me/contact", None).await;
    let me_id = me_c["id"].as_str().unwrap().to_string();
    let (s, b) = fx
        .req(
            "POST",
            "/accounts",
            Some(json!({ "system_id": "mail", "party_id": "me", "kind": "work_email", "handle": "dev@corp.example.com" })),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    // 王芳 is already known through chat (an unconfirmed contact from identify)
    let wang = fx.t.store().transaction(|tx| {
        accounts::identify(
            tx,
            WS,
            ME,
            "feishu",
            "on_wangfang",
            &accounts::Hints {
                display_name: Some("王芳".into()),
                ..Default::default()
            },
        )
    });
    let wang = wang.expect("identify").party_id;

    let job = fx.sync_now().await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.errors);
    assert_eq!(job.reports[0].1.contacts, 2, "only 韩梅梅 and 张伟 are new");
    assert_eq!(fx.contact_id("on_lilei"), me_id, "matched by email");
    assert_eq!(fx.contact_id("on_wangfang"), wang, "matched by handle");
    let (_, me_c) = fx.req("GET", "/me/contact", None).await;
    assert_eq!(me_c["name"], "Dev", "“我” keeps its name");
    assert_eq!(fx.member_names("od-eng").await, vec![pair("Dev", "lead")]);
    let (_, w) = fx.req("GET", &format!("/contacts/{wang}"), None).await;
    assert_eq!(
        w["kind"], "real",
        "the directory confirms an unconfirmed contact"
    );
    // the email became a mail account of the same contact
    assert_eq!(
        fx.text("SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.handle = 'wang.fang@corp.example.com'"),
        wang
    );
    // “我” leads 研发部: the team scope reaches me
    let viewer = fx.t.modules.viewer(&me()).unwrap();
    assert!(
        viewer
            .scopes
            .contains(&format!("team:{}", fx.team_id("od-eng")))
    );
    // the own organization's app bot account is not a person
    assert_eq!(
        fx.count(
            "SELECT COUNT(*) FROM parties WHERE type = 'contact' AND workspace_id = 'default'"
        ),
        4
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_change_nothing() {
    let fx = fixture(first());
    fx.sync.spawn();

    // no directory connection yet
    let (s, b) = fx.req("POST", "/teams/sync", None).await;
    assert_eq!(
        (s, b),
        (400, json!({ "error": "还没有接入能读通讯录的账号" }))
    );
    fx.connect(WS, "con_chat", r#"["msg.receive"]"#).await;
    let (s, _) = fx.req("POST", "/teams/sync", None).await;
    assert_eq!(s, 400, "a connection without directory.read does not count");
    let (s, b) = fx
        .req(
            "POST",
            "/teams/sync",
            Some(json!({ "connection_id": "con_chat" })),
        )
        .await;
    assert_eq!(
        (s, b),
        (400, json!({ "error": "还没有接入能读通讯录的账号" }))
    );
    let (s, b) = fx
        .req(
            "POST",
            "/teams/sync",
            Some(json!({ "connection_id": "con_nope" })),
        )
        .await;
    assert_eq!((s, b), (404, json!({ "error": "没有这个连接" })));

    fx.connect(WS, CONN, r#"["directory.read"]"#).await;
    let (s, b) = fx
        .req(
            "POST",
            "/teams/sync",
            Some(json!({ "connection_id": CONN })),
        )
        .await;
    assert_eq!(s, 202);
    fx.wait(b["job_id"].as_str().unwrap()).await;
    let teams = fx.count("SELECT COUNT(*) FROM parties WHERE type = 'team' AND status = 'active'");
    assert_eq!(teams, 3);

    // another workspace's connection is not ours
    let (s, _) = call(
        &fx.r,
        Some(&caller(WS2, ME)),
        "POST",
        "/teams/sync",
        Some(&json!({ "connection_id": CONN })),
    )
    .await;
    assert_eq!(s, 404);

    // a read error fails the job
    fx.dir.fail(DirectoryError::Auth("99991672".into()));
    let job = fx.sync_now().await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.errors[0].1, "通讯录没有授权：99991672");
    // an empty directory is refused instead of dissolving everything
    fx.dir.set(Snapshot::default());
    let job = fx.sync_now().await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.errors[0].1, EMPTY_SNAPSHOT);
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM parties WHERE type = 'team' AND status = 'active'"),
        teams
    );
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM audit WHERE action = 'team.sync'"),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn daily_run_and_debounced_events() {
    let fx = fixture(first());
    fx.connect(WS, CONN, r#"["directory.read"]"#).await;
    fx.connect(WS2, "con_w2", r#"["directory.read"]"#).await;
    let now = fx.t.clock.now();
    let at = next_daily(now, DAILY_HOUR);
    assert!(at > now && at - now <= 25 * 3_600_000);
    // connections that never synced are caught up soon after start, without waiting for 06:00
    assert!(fx.sync.tick(now).unwrap().is_empty(), "not at once");
    let caught = fx.sync.tick(now + CATCH_UP_FIRST_MS).unwrap();
    assert_eq!(caught.len(), 2, "one catch-up job per workspace");
    assert_eq!(fx.sync.job(&caught[0]).unwrap().reason, "catch_up");
    for id in &caught {
        fx.sync.run_job(id).await;
    }
    assert_eq!(fx.dir.reads(), 2);
    // synced now: the next catch-up finds nothing
    assert!(
        fx.sync
            .tick(now + CATCH_UP_FIRST_MS + CATCH_UP_MS)
            .unwrap()
            .is_empty()
    );
    assert!(fx.sync.tick(at - 1).unwrap().is_empty());
    let jobs = fx.sync.tick(at).unwrap();
    assert_eq!(jobs.len(), 2, "one job per workspace");
    assert!(fx.sync.tick(at + 1).unwrap().is_empty(), "once a day");
    let j = fx.sync.job(&jobs[0]).unwrap();
    assert_eq!((j.state, j.reason.as_str()), (JobState::Queued, "daily"));
    // queued jobs are reused rather than piled up
    let (_, b) = fx.req("POST", "/teams/sync", None).await;
    assert_eq!(
        b["job_id"],
        jobs.iter()
            .find(|id| fx.sync.job(id).unwrap().ws == WS)
            .unwrap()
            .as_str()
    );

    // the worker runs what was queued
    fx.sync.spawn();
    for id in &jobs {
        assert_eq!(fx.wait(id).await.state, JobState::Done);
    }
    assert_eq!(fx.dir.reads(), 4);
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM parties WHERE type = 'team' AND workspace_id = 'w2'"),
        3
    );

    // directory events: one sync after a minute of quiet
    fx.sync.changed(WS, CONN);
    fx.t.clock.advance(DEBOUNCE_MS / 2);
    fx.sync.changed(WS, CONN);
    let t = fx.t.clock.now();
    assert!(fx.sync.tick(t + DEBOUNCE_MS - 1).unwrap().is_empty());
    let jobs = fx.sync.tick(t + DEBOUNCE_MS).unwrap();
    assert_eq!(jobs.len(), 1);
    let j = fx.wait(&jobs[0]).await;
    assert_eq!(
        (j.reason.as_str(), j.connections.clone()),
        ("event", vec![CONN.to_string()])
    );
    assert!(fx.sync.tick(t + 2 * DEBOUNCE_MS).unwrap().is_empty());
}

#[test]
fn next_daily_is_the_next_six_oclock() {
    use chrono::{Local, TimeZone, Timelike};
    let noon = Local
        .with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    let next = Local.timestamp_millis_opt(next_daily(noon, 6)).unwrap();
    assert_eq!((next.hour(), next.minute()), (6, 0));
    assert_eq!(next.date_naive().to_string(), "2026-11-02");
    let five = noon - 7 * 3_600_000;
    let next = Local.timestamp_millis_opt(next_daily(five, 6)).unwrap();
    assert_eq!(next.date_naive().to_string(), "2026-11-01");
    let six = noon - 6 * 3_600_000;
    assert!(next_daily(six, 6) > six, "strictly after");
}
