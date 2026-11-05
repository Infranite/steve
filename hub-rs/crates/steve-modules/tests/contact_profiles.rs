//! Contact and organization profiles: what you set by hand
//! — a person's name, even a handle-looking one; job fields, also edits from before they were
//! marked, across a departure and return, and on a job added back — is never overwritten by the
//! directory sync or by enrichment; enrichment fills only what is empty and attaches an unclaimed
//! email; organizations say where their name comes from and why a placeholder stays one; the
//! contact page says which fields are missing and why, and lookups are bookkept per workspace.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use serde_json::{Value, json};
use steve_modules::accounts::{self, Enrichment, TenantHint};
use steve_modules::contacts::profile::{self, Gap};
use steve_modules::contacts::sync::{
    Department, DirConnection, DirUser, DirectoryAdapter, DirectorySync, FakeDirectory, JobState,
    Snapshot, TableConnections,
};
use steve_modules::contacts::{self, Ports};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::{ApiResult, Caller};

const CONN: &str = "con_fs";

fn me() -> Caller {
    caller(WS, ME)
}

fn user(id: &str, name: &str, title: &str) -> DirUser {
    DirUser {
        id: format!("on_{id}"),
        extra_ids: vec![format!("ou_{id}")],
        name: name.into(),
        emails: Vec::new(),
        title: Some(title.into()),
        leader_id: None,
        department_ids: vec!["od-eng".into()],
        departed: false,
    }
}

fn snapshot(users: Vec<DirUser>) -> Snapshot {
    Snapshot {
        departments: vec![Department {
            id: "od-eng".into(),
            name: "研发部".into(),
            parent_id: Some("0".into()),
            leader_id: None,
        }],
        users,
    }
}

fn people() -> Vec<DirUser> {
    vec![
        user("lilei", "李雷", "工程师"),
        user("wangfang", "王芳", "工程师"),
        user("hanmeimei", "韩梅梅", "工程师"),
        user("zhangwei", "张伟", "工程师"),
    ]
}

struct Fx {
    t: TestEnv,
    r: Router,
    sync: Arc<DirectorySync>,
    dir: Arc<FakeDirectory>,
}

fn fixture() -> Fx {
    let t = TestEnv::new(accounts::register);
    let dir = Arc::new(FakeDirectory::new("feishu", snapshot(people())));
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

    fn text(&self, sql: &str) -> String {
        self.t
            .store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .expect("text")
    }

    async fn connect(&self) {
        self.sync.spawn();
        let (s, _) = self.req("GET", "/me/contact", None).await;
        assert_eq!(s, 200);
        self.exec(&format!(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               SELECT 'acc_{CONN}', '{WS}', 'feishu_app', 'feishu_app', id, 'app_bot', 'cli_x', 'cli_x', 'connect', 1, 1
                 FROM parties WHERE workspace_id = '{WS}' AND org_kind = 'self';
             INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
               VALUES ('{CONN}', '{WS}', 'acc_{CONN}', 'feishu', 'credential', '[\"msg.receive\",\"directory.read\"]', 'usr_dev', 1, 1);"
        ));
    }

    async fn sync_now(&self) {
        let (s, b) = self.req("POST", "/teams/sync", None).await;
        assert_eq!(s, 202, "{b}");
        let id = b["job_id"].as_str().unwrap().to_string();
        for _ in 0..500 {
            if let Some(j) = self.sync.job(&id)
                && matches!(j.state, JobState::Done | JobState::Failed)
            {
                assert_eq!(j.state, JobState::Done, "{:?}", j.errors);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("sync did not finish");
    }

    fn contact_id(&self, id: &str) -> String {
        self.text(&format!(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.family = 'feishu_user' AND h.handle = 'on_{id}'"
        ))
    }

    fn account_id(&self, id: &str) -> String {
        self.text(&format!(
            "SELECT h.account_id FROM account_handles h WHERE h.family = 'feishu_user' AND h.handle = 'on_{id}'"
        ))
    }

    fn own_org(&self) -> String {
        self.text(&format!(
            "SELECT id FROM parties WHERE workspace_id = '{WS}' AND org_kind = 'self'"
        ))
    }

    async fn contact(&self, id: &str) -> Value {
        let (s, b) = self.req("GET", &format!("/contacts/{id}"), None).await;
        assert_eq!(s, 200, "{b}");
        b
    }

    async fn name(&self, id: &str) -> String {
        self.contact(id).await["name"].as_str().unwrap().to_string()
    }

    async fn title(&self, id: &str) -> Value {
        let c = self.contact(id).await;
        let org = self.own_org();
        c["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["org_id"] == json!(org))
            .map(|j| j["title"].clone())
            .unwrap_or(Value::Null)
    }

    fn enrich(&self, account: &str, e: &Enrichment) -> accounts::Enriched {
        self.t
            .store()
            .transaction(|tx| accounts::enrich(tx, &[], WS, ME, account, e))
            .expect("enrich")
    }

    fn tenant_org(&self, h: &TenantHint) -> String {
        self.t
            .store()
            .transaction(|tx| accounts::tenant_org(tx, WS, ME, h))
            .expect("tenant org")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_directory_sync_keeps_what_you_set() {
    let fx = fixture();
    fx.connect().await;
    fx.sync_now().await;
    let (lilei, wangfang, hanmeimei, zhangwei) = (
        fx.contact_id("lilei"),
        fx.contact_id("wangfang"),
        fx.contact_id("hanmeimei"),
        fx.contact_id("zhangwei"),
    );
    let org = fx.own_org();

    // names you set — also one that looks like a handle
    let (s, b) = fx
        .req(
            "PATCH",
            &format!("/contacts/{lilei}"),
            Some(json!({"name": "老李"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let (s, _) = fx
        .req(
            "PATCH",
            &format!("/contacts/{wangfang}"),
            Some(json!({"name": "ou_abcdef123456"})),
        )
        .await;
    assert_eq!(s, 200);
    // a title you set now, and one set before edits were marked (a revision, no `manual`)
    let (s, b) = fx
        .req(
            "PATCH",
            &format!("/contacts/{zhangwei}/jobs/{org}"),
            Some(json!({"title": "销售总监"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    fx.exec(&format!(
        "UPDATE links SET props = json_set(props, '$.title', '首席架构师') WHERE subject_id = '{hanmeimei}' AND object_id = '{org}' AND predicate = 'works_at';
         INSERT INTO revisions (workspace_id, object_type, object_id, version, patch, author, created)
           VALUES ('{WS}', 'party', '{hanmeimei}', 99,
             json('{{\"before\":{{\"job\":{{\"org_id\":\"{org}\",\"props\":{{\"title\":\"工程师\"}}}}}},\"after\":{{\"job\":{{\"org_id\":\"{org}\",\"props\":{{\"title\":\"首席架构师\"}}}}}}}}'), 'usr_dev', 1);"
    ));

    // the directory changes every name and title
    fx.dir.set(snapshot(vec![
        user("lilei", "李雷2", "高级工程师"),
        user("wangfang", "王芳2", "高级工程师"),
        user("hanmeimei", "韩梅梅2", "高级工程师"),
        user("zhangwei", "张伟2", "高级工程师"),
    ]));
    fx.sync_now().await;
    assert_eq!(fx.name(&lilei).await, "老李");
    assert_eq!(
        fx.name(&wangfang).await,
        "ou_abcdef123456",
        "a handle-looking name you set stays"
    );
    assert_eq!(
        fx.name(&hanmeimei).await,
        "韩梅梅2",
        "a name nobody set follows the directory"
    );
    assert_eq!(fx.title(&zhangwei).await, json!("销售总监"));
    assert_eq!(
        fx.title(&hanmeimei).await,
        json!("首席架构师"),
        "the older edit is protected by its revision"
    );
    assert_eq!(
        fx.title(&lilei).await,
        json!("高级工程师"),
        "a title nobody set follows"
    );

    // 张伟 leaves and comes back: his title stays his
    fx.dir.update(|s| {
        s.users
            .iter_mut()
            .find(|u| u.id == "on_zhangwei")
            .unwrap()
            .departed = true
    });
    fx.sync_now().await;
    assert_eq!(fx.title(&zhangwei).await, json!("销售总监"));
    fx.dir.update(|s| {
        let u = s.users.iter_mut().find(|u| u.id == "on_zhangwei").unwrap();
        u.departed = false;
        u.title = Some("销售".into());
    });
    fx.sync_now().await;
    assert_eq!(
        fx.title(&zhangwei).await,
        json!("销售总监"),
        "reopened with your title"
    );
    let props: Value = serde_json::from_str(&fx.text(&format!(
        "SELECT props FROM links WHERE subject_id = '{zhangwei}' AND object_id = '{org}' AND predicate = 'works_at'"
    )))
    .unwrap();
    assert_eq!(props["manual"], json!(["title"]));
    assert_eq!(props["to"], Value::Null);

    // 李雷 leaves; you add his job back by hand: what you entered is yours from then on
    fx.dir.update(|s| {
        s.users
            .iter_mut()
            .find(|u| u.id == "on_lilei")
            .unwrap()
            .departed = true
    });
    fx.sync_now().await;
    let (s, b) = fx
        .req(
            "POST",
            &format!("/contacts/{lilei}/jobs"),
            Some(json!({"org_id": org, "title": "顾问"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    fx.dir.update(|s| {
        let u = s.users.iter_mut().find(|u| u.id == "on_lilei").unwrap();
        u.departed = false;
        u.title = Some("工程师".into());
    });
    fx.sync_now().await;
    assert_eq!(fx.title(&lilei).await, json!("顾问"));

    // the profile says which are yours
    let p = &fx.contact(&zhangwei).await["profile"];
    assert_eq!(
        p["fields"]["title"],
        json!({"state": "known", "manual": true})
    );
    assert_eq!(
        fx.contact(&lilei).await["profile"]["fields"]["name"]["manual"],
        json!(true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn enrichment_fills_only_what_is_empty() {
    let fx = fixture();
    fx.connect().await;
    fx.sync_now().await;
    let org = fx.own_org();
    let (wangfang, hanmeimei) = (fx.contact_id("wangfang"), fx.contact_id("hanmeimei"));

    // a handle-looking name you set is not “raw”
    fx.req(
        "PATCH",
        &format!("/contacts/{wangfang}"),
        Some(json!({"name": "ou_abcdef123456"})),
    )
    .await;
    let r = fx.enrich(
        &fx.account_id("wangfang"),
        &Enrichment {
            name: Some("王芳".into()),
            ..Enrichment::default()
        },
    );
    assert!(!r.renamed);
    assert_eq!(fx.name(&wangfang).await, "ou_abcdef123456");

    // an empty title is filled; then you set one; a later answer does not replace it
    fx.exec(&format!(
        "UPDATE links SET props = json_set(props, '$.title', NULL) WHERE subject_id = '{hanmeimei}' AND object_id = '{org}'"
    ));
    let e = |t: &str| Enrichment {
        org_id: Some(org.clone()),
        title: Some(t.into()),
        directory: true,
        ..Enrichment::default()
    };
    assert!(
        fx.enrich(&fx.account_id("hanmeimei"), &e("架构师"))
            .job_filled
    );
    assert_eq!(fx.title(&hanmeimei).await, json!("架构师"));
    assert!(
        !fx.enrich(&fx.account_id("hanmeimei"), &e("别的"))
            .job_filled,
        "only an empty one"
    );
    fx.exec(&format!(
        "UPDATE links SET props = json_set(props, '$.title', NULL) WHERE subject_id = '{hanmeimei}' AND object_id = '{org}'"
    ));
    fx.req(
        "PATCH",
        &format!("/contacts/{hanmeimei}/jobs/{org}"),
        Some(json!({"title": null})),
    )
    .await;
    assert!(
        !fx.enrich(&fx.account_id("hanmeimei"), &e("又一个"))
            .job_filled,
        "a title you cleared stays cleared"
    );

    // an unclaimed email becomes theirs; one someone else has does not
    let mail = |m: &str| Enrichment {
        email: Some(m.into()),
        directory: true,
        ..Enrichment::default()
    };
    assert!(
        fx.enrich(&fx.account_id("lilei"), &mail("lilei@corp.example.com"))
            .email_added
    );
    assert!(
        !fx.enrich(&fx.account_id("zhangwei"), &mail("lilei@corp.example.com"))
            .email_added
    );
    let p = &fx.contact(&fx.contact_id("lilei")).await["profile"];
    assert_eq!(p["fields"]["email"]["state"], "known");
}

#[tokio::test(flavor = "multi_thread")]
async fn organizations_say_where_their_name_comes_from() {
    let fx = fixture();
    fx.connect().await;
    let ext = fx.tenant_org(&TenantHint {
        key: "tk_partner".into(),
        own: false,
        name: None,
        seen_in: Some("合作群".into()),
    });
    let (s, o) = fx.req("GET", &format!("/orgs/{ext}"), None).await;
    assert_eq!(s, 200, "{o}");
    assert_eq!(o["name"], "外部组织（合作群 里的成员）");
    assert_eq!(o["name_state"], "placeholder");
    assert_eq!(o["name_reason"], "external_unavailable");
    assert_eq!(o["tenant"], json!({"key": "tk_partner", "own": false}));

    // you name it — even “外部组织…” — and no provider name replaces yours
    let (s, o) = fx
        .req(
            "PATCH",
            &format!("/orgs/{ext}"),
            Some(json!({"name": "外部组织甲方"})),
        )
        .await;
    assert_eq!(s, 200, "{o}");
    assert_eq!(
        (o["name_state"].clone(), o["name_reason"].clone()),
        (json!("manual"), Value::Null)
    );
    fx.tenant_org(&TenantHint {
        key: "tk_partner".into(),
        own: false,
        name: Some("Partner Ltd".into()),
        seen_in: None,
    });
    let (_, o) = fx.req("GET", &format!("/orgs/{ext}"), None).await;
    assert_eq!(o["name"], "外部组织甲方");

    // a placeholder nobody named takes the provider's name
    let other = fx.tenant_org(&TenantHint {
        key: "tk_other".into(),
        own: false,
        name: None,
        seen_in: None,
    });
    fx.tenant_org(&TenantHint {
        key: "tk_other".into(),
        own: false,
        name: Some("Other Inc".into()),
        seen_in: None,
    });
    let (_, o) = fx.req("GET", &format!("/orgs/{other}"), None).await;
    assert_eq!(
        (o["name"].clone(), o["name_state"].clone()),
        (json!("Other Inc"), json!("provider"))
    );

    // the own organization: still the workspace's name; the name permission refused → said
    let own = fx.tenant_org(&TenantHint {
        key: "tk_own".into(),
        own: true,
        name: None,
        seen_in: None,
    });
    let (_, o) = fx.req("GET", &format!("/orgs/{own}"), None).await;
    assert_eq!(o["name_state"], "workspace");
    assert_eq!(o["name_reason"], Value::Null);
    assert_eq!(o["tenant"], json!({"key": "tk_own", "own": true}));
    fx.exec(&format!(
        "UPDATE connections SET config = '{{\"permissions\":[{{\"scope\":\"tenant:tenant:readonly\",\"status\":\"missing\"}}]}}' WHERE id = '{CONN}'"
    ));
    let (_, o) = fx.req("GET", &format!("/orgs/{own}"), None).await;
    assert_eq!(
        (o["name_reason"].clone(), o["name_scope"].clone()),
        (json!("permission"), json!("tenant:tenant:readonly"))
    );
    fx.tenant_org(&TenantHint {
        key: "tk_own".into(),
        own: true,
        name: Some("示例科技".into()),
        seen_in: None,
    });
    let (_, o) = fx.req("GET", &format!("/orgs/{own}"), None).await;
    assert_eq!(
        (o["name"].clone(), o["name_state"].clone()),
        (json!("示例科技"), json!("provider"))
    );

    // another workspace sees none of it
    let (s, _) = call(
        &fx.r,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/orgs/{ext}"),
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &fx.r,
        Some(&caller(WS2, ME)),
        "PATCH",
        &format!("/orgs/{ext}"),
        Some(&json!({"name": "x"})),
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_profile_says_what_is_missing_and_lookups_are_retried() {
    let fx = fixture();
    fx.connect().await;
    fx.sync_now().await;
    let lilei = fx.contact_id("lilei");
    let acc = fx.account_id("lilei");

    // the directory gave the name, the job and its title; no email, and no Feishu identity that
    // could look them up (this test's connection is not a Feishu one)
    let p = &fx.contact(&lilei).await["profile"];
    assert_eq!(p["checked"], Value::Null);
    assert_eq!(p["fields"]["name"], json!({"state": "known"}));
    assert_eq!(p["fields"]["org"]["state"], "known");
    assert_eq!(p["fields"]["title"], json!({"state": "known"}));
    assert_eq!(
        p["fields"]["email"],
        json!({"state": "missing", "reason": "no_source"})
    );
    fx.exec(&format!(
        "UPDATE connections SET adapter = 'feishu_app', status = 'ok' WHERE id = '{CONN}'"
    ));
    let p = &fx.contact(&lilei).await["profile"];
    assert_eq!(
        p["fields"]["email"],
        json!({"state": "missing", "reason": "pending"})
    );

    // a lookup that left the email out: said as such, retried 1 h, then 6 h later
    let rec = |now: i64, gaps: Vec<(&str, Gap)>| {
        fx.t.store()
            .transaction(|tx| profile::record_lookup(tx, WS, &acc, now, Some("directory"), &gaps))
    };
    rec(1_000, vec![("email", Gap::new("not_provided"))]).unwrap();
    let p = &fx.contact(&lilei).await["profile"];
    assert_eq!(
        (
            p["checked"].clone(),
            p["next_check"].clone(),
            p["source"].clone()
        ),
        (json!(1_000), json!(1_000 + 3_600_000), json!("directory"))
    );
    assert_eq!(
        p["fields"]["email"],
        json!({"state": "missing", "reason": "not_provided"})
    );
    rec(
        2_000,
        vec![(
            "email",
            Gap::scope("permission", "contact:user.base:readonly"),
        )],
    )
    .unwrap();
    let p = &fx.contact(&lilei).await["profile"];
    assert_eq!(p["next_check"], json!(2_000 + 6 * 3_600_000));
    assert_eq!(
        p["fields"]["email"],
        json!({"state": "missing", "reason": "permission", "scope": "contact:user.base:readonly"})
    );
    let due = |now: i64| {
        fx.t.store()
            .with_conn(|c| Ok(profile::due(c, &acc, now).unwrap()))
            .unwrap()
    };
    assert!(!due(3_000));
    assert!(due(2_000 + 6 * 3_600_000));
    // a connection changed: due at once
    fx.t.store()
        .transaction(|tx| profile::all_due(tx, WS, 5_000))
        .unwrap();
    assert!(due(5_000));
    // nothing a retry could change (another organization's member): no retry
    rec(6_000, vec![("email", Gap::new("external"))]).unwrap();
    assert_eq!(
        fx.contact(&lilei).await["profile"]["next_check"],
        Value::Null
    );
    assert!(!due(i64::MAX / 2));

    // only an account of the workspace is bookkept
    assert!(
        fx.t.store()
            .transaction(|tx| profile::record_lookup(tx, WS2, &acc, 1, None, &[]))
            .is_err()
    );

    // the page counts only jobs you can see
    let party =
        fx.t.store()
            .with_conn(|c| Ok(contacts::repo::get(c, &lilei).unwrap().unwrap()))
            .unwrap();
    let p =
        fx.t.store()
            .with_conn(|c| Ok(profile::contact_profile(c, WS, &party, Some(&[])).unwrap()))
            .unwrap();
    assert_eq!(p.fields.org.state, "missing");
}
