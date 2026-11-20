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
        tokio::spawn(self.sync.clone().run());
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
    // a Feishu identity that looks Feishu people up (registered as the channels would)
    profile::register_lookup("feishu_app", "feishu");
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

#[test]
fn sg8_enrichment_name_and_promotion_table() {
    for (incoming, directory, manual, active, expected_name, promoted, source) in [
        ("  Alice  ", true, false, true, "Alice", true, "sync"),
        ("Alice", false, false, true, "Alice", true, "chat"),
        (
            "ou_abcdef123456",
            true,
            false,
            true,
            "ou_sg8unknown",
            false,
            "infer",
        ),
        ("   ", true, false, true, "ou_sg8unknown", false, "infer"),
        ("Alice", true, true, true, "ou_manual123456", true, "sync"),
        ("Alice", true, false, false, "ou_sg8unknown", false, "infer"),
    ] {
        let t = TestEnv::new(accounts::register);
        let known = t
            .store()
            .transaction(|tx| {
                accounts::identify(
                    tx,
                    WS,
                    ME,
                    "feishu",
                    "ou_sg8unknown",
                    &accounts::Hints::default(),
                )
            })
            .unwrap();
        if manual {
            t.store()
                .transaction(|tx| {
                    let party = contacts::repo::get(tx, &known.party_id)?.unwrap();
                    contacts::service::revise_as(
                        tx,
                        &party,
                        &contacts::repo::PartyPatch {
                            name: Some("ou_manual123456".into()),
                            ..Default::default()
                        },
                        json!({"name": party.name}),
                        json!({"name": "ou_manual123456"}),
                        ME,
                    )
                })
                .unwrap();
        }
        if !active {
            t.store()
                .with_conn(|c| {
                    Ok(c.execute(
                        "UPDATE parties SET status='deleted' WHERE id=?",
                        [&known.party_id],
                    )?)
                })
                .unwrap();
        }
        let before_source = t
            .store()
            .reader(|c| -> steve_store::Result<_> {
                Ok(c.query_row(
                    "SELECT source FROM parties WHERE id=?",
                    [&known.party_id],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap();
        let r = t
            .store()
            .transaction(|tx| {
                accounts::enrich(
                    tx,
                    &[],
                    WS,
                    ME,
                    &known.account_id,
                    &Enrichment {
                        name: Some(incoming.into()),
                        directory,
                        avatar_url: Some("https://avatar.test/a".into()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let (account, party) = t
            .store()
            .reader(|c| -> steve_store::Result<_> {
                Ok((
                    accounts::repo::get(c, &known.account_id)?.unwrap(),
                    contacts::repo::get(c, &known.party_id)?.unwrap(),
                ))
            })
            .unwrap();
        assert_eq!(
            party.name, expected_name,
            "incoming={incoming:?} manual={manual} active={active}"
        );
        assert_eq!(r.promoted, promoted);
        assert_eq!(r.renamed, promoted && !manual);
        assert_eq!(r.account_named, promoted);
        assert_eq!(
            party.contact_kind.as_deref(),
            Some(if promoted { "real" } else { "unconfirmed" })
        );
        assert_eq!(party.source, if promoted { source } else { &before_source });
        assert_eq!(account.display_name, if promoted { "Alice" } else { "" });
        assert_eq!(
            party.avatar.get("url").and_then(Value::as_str),
            active.then_some("https://avatar.test/a")
        );
        assert_eq!(r.party_id, known.party_id);
    }
}

#[test]
fn sg8_enrichment_job_and_email_idempotence_table() {
    for directory in [false, true] {
        let t = TestEnv::new(accounts::register);
        let known = t
            .store()
            .transaction(|tx| {
                accounts::identify(
                    tx,
                    WS,
                    ME,
                    "feishu",
                    "ou_sg8unknown",
                    &accounts::Hints::default(),
                )
            })
            .unwrap();
        let org = t
            .store()
            .transaction(|tx| {
                accounts::tenant_org(
                    tx,
                    WS,
                    ME,
                    &TenantHint {
                        key: "sg8-tenant".into(),
                        own: false,
                        name: Some("SG8 org".into()),
                        seen_in: None,
                    },
                )
            })
            .unwrap();
        let e = Enrichment {
            name: Some("Alice".into()),
            org_id: Some(org.clone()),
            title: Some("  Engineer  ".into()),
            dept: Some(" Platform ".into()),
            email: Some(" Alice@Example.test ".into()),
            directory,
            ..Default::default()
        };
        let first = t
            .store()
            .transaction(|tx| accounts::enrich(tx, &[], WS, ME, &known.account_id, &e))
            .unwrap();
        assert!(first.org_linked);
        assert!(first.email_added);
        let second = t
            .store()
            .transaction(|tx| accounts::enrich(tx, &[], WS, ME, &known.account_id, &e))
            .unwrap();
        assert!(
            !second.org_linked
                && !second.job_filled
                && !second.email_added
                && !second.promoted
                && !second.renamed
        );
        let (props, source): (String,String) = t.store().reader(|c| -> steve_store::Result<_> { Ok(c.query_row("SELECT props,source FROM links WHERE subject_id=?1 AND object_id=?2 AND predicate='works_at'", rusqlite::params![known.party_id,org], |r| Ok((r.get(0)?,r.get(1)?)))?) }).unwrap();
        let props: Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["title"], "Engineer");
        assert_eq!(props["dept"], "Platform");
        assert_eq!(source, if directory { "sync" } else { "infer" });
        let mail = t.store().reader(|c| -> steve_store::Result<_> { Ok(c.query_row("SELECT a.verified,a.confidence,a.display_name FROM accounts a JOIN account_handles h ON h.account_id=a.id WHERE h.family='email' AND h.handle='alice@example.test'", [], |r| Ok((r.get::<_,bool>(0)?,r.get::<_,f64>(1)?,r.get::<_,String>(2)?)))?) }).unwrap();
        assert_eq!(
            mail,
            (directory, if directory { 1.0 } else { 0.8 }, "Alice".into())
        );
    }
}

#[test]
fn sg8_enrichment_strong_handle_evidence_merge_direction_table() {
    for (mine_real, theirs_real, expected_merge) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
        (false, false, true),
    ] {
        let t = TestEnv::new(accounts::register);
        let identify = |handle| {
            t.store()
                .transaction(|tx| {
                    accounts::identify(tx, WS, ME, "feishu", handle, &accounts::Hints::default())
                })
                .unwrap()
        };
        let mine = identify("ou_sg8mine");
        let theirs = identify("on_sg8theirs");
        for (party, real) in [(&mine.party_id, mine_real), (&theirs.party_id, theirs_real)] {
            if real {
                t.store()
                    .with_conn(|c| {
                        Ok(
                            c.execute(
                                "UPDATE parties SET contact_kind='real' WHERE id=?",
                                [party],
                            )?,
                        )
                    })
                    .unwrap();
            }
        }
        let r = t
            .store()
            .transaction(|tx| {
                accounts::enrich(
                    tx,
                    &[Arc::new(accounts::AccountHooks)],
                    WS,
                    ME,
                    &mine.account_id,
                    &Enrichment {
                        handles: vec![accounts::ExtraHandle {
                            handle: "on_sg8theirs".into(),
                            connection_id: None,
                        }],
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let owner = if expected_merge && !mine_real {
            &theirs.party_id
        } else {
            &mine.party_id
        };
        assert_eq!(r.merged_into.as_ref(), expected_merge.then_some(owner));
        assert_eq!(&r.party_id, owner);
        let mine_account = t
            .store()
            .reader(|c| accounts::repo::get(c, &mine.account_id))
            .unwrap()
            .unwrap();
        assert_eq!(&mine_account.party_id, owner);
        assert!(!r.account_named && !r.promoted && !r.org_linked && !r.email_added);
    }
}
