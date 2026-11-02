//! The directory sync driven through a thin Feishu-shaped adapter against the fake Feishu open platform
//! (steve-channels, feature `fake`): contact v3 departments (`fetch_child=true`), users by department,
//! resigned users, deleted departments. The production adapter is slice C7/F5 (`feishu::directory`);
//! this one only proves the snapshot shape fits what Feishu returns.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, World, ids};
use steve_modules::contacts::sync::{
    Department, DirConnection, DirUser, DirectoryAdapter, DirectoryError, DirectorySync, JobState,
    Snapshot, TableConnections,
};
use steve_modules::contacts::{self, Ports};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::{ApiResult, accounts};

struct FeishuDirectory {
    base: String,
    http: reqwest::Client,
}

fn unavailable(e: impl std::fmt::Display) -> DirectoryError {
    DirectoryError::Unavailable(e.to_string())
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

impl FeishuDirectory {
    async fn token(&self) -> Result<String, DirectoryError> {
        let v: Value = self
            .http
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.base
            ))
            .json(&json!({ "app_id": ids::APP_ID, "app_secret": ids::APP_SECRET }))
            .send()
            .await
            .map_err(unavailable)?
            .json()
            .await
            .map_err(unavailable)?;
        s(&v, "tenant_access_token").ok_or_else(|| DirectoryError::Auth(v["code"].to_string()))
    }

    /// Every item of a paged contact v3 list.
    async fn all(&self, token: &str, path: &str) -> Result<Vec<Value>, DirectoryError> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!("{}{path}&page_size=50", self.base);
            if let Some(p) = &page {
                url.push_str(&format!("&page_token={p}"));
            }
            let v: Value = self
                .http
                .get(url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(unavailable)?
                .json()
                .await
                .map_err(unavailable)?;
            if v["code"] != 0 {
                return Err(DirectoryError::Auth(format!("{} {}", v["code"], v["msg"])));
            }
            out.extend(v["data"]["items"].as_array().cloned().unwrap_or_default());
            if v["data"]["has_more"] != true {
                return Ok(out);
            }
            page = s(&v["data"], "page_token");
        }
    }
}

#[async_trait]
impl DirectoryAdapter for FeishuDirectory {
    fn system(&self) -> &str {
        "feishu"
    }

    async fn snapshot(&self) -> Result<Snapshot, DirectoryError> {
        let token = self.token().await?;
        let depts = self
            .all(&token, "/open-apis/contact/v3/departments/0/children?department_id_type=open_department_id&user_id_type=union_id&fetch_child=true")
            .await?;
        let departments: Vec<Department> = depts
            .iter()
            .filter(|d| d["status"]["is_deleted"] != true)
            .map(|d| Department {
                id: s(d, "open_department_id").unwrap_or_default(),
                name: s(d, "name").unwrap_or_default(),
                parent_id: s(d, "parent_department_id"),
                leader_id: s(d, "leader_user_id"),
            })
            .collect();
        let mut users: Vec<DirUser> = Vec::new();
        for dep in std::iter::once("0").chain(departments.iter().map(|d| d.id.as_str())) {
            let path = format!(
                "/open-apis/contact/v3/users/find_by_department?department_id={dep}&department_id_type=open_department_id&user_id_type=union_id"
            );
            for u in self.all(&token, &path).await? {
                let id = s(&u, "union_id").unwrap_or_default();
                if users.iter().any(|x| x.id == id) {
                    continue;
                }
                users.push(DirUser {
                    id,
                    extra_ids: s(&u, "open_id").into_iter().collect(),
                    name: s(&u, "name").unwrap_or_default(),
                    emails: [s(&u, "enterprise_email"), s(&u, "email")]
                        .into_iter()
                        .flatten()
                        .collect(),
                    title: s(&u, "job_title"),
                    leader_id: s(&u, "leader_user_id"),
                    department_ids: u["department_ids"]
                        .as_array()
                        .map(|xs| {
                            xs.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                    departed: u["status"]["is_resigned"] == true,
                });
            }
        }
        Ok(Snapshot { departments, users })
    }
}

async fn wait(sync: &DirectorySync, id: &str) -> steve_modules::contacts::sync::Job {
    for _ in 0..500 {
        if let Some(j) = sync.job(id)
            && matches!(j.state, JobState::Done | JobState::Failed)
        {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn feishu_departments_and_users_become_the_org_chart() {
    let fake = FakeFeishu::start_with(World::sample())
        .await
        .expect("fake feishu");
    let t = TestEnv::new(accounts::register);
    let base = fake.base_url().to_string();
    let conns = TableConnections::new().with(
        "feishu",
        "飞书",
        Arc::new(
            move |_: &DirConnection| -> ApiResult<Arc<dyn DirectoryAdapter>> {
                Ok(Arc::new(FeishuDirectory {
                    base: base.clone(),
                    http: reqwest::Client::new(),
                }))
            },
        ),
    );
    let sync = DirectorySync::new(t.modules.clone(), Arc::new(conns));
    sync.spawn();
    let mut ports: Ports = accounts::ports();
    ports.sync = Some(sync.clone());
    let r = contacts::routes(ports).with_state(t.modules.clone());
    let who = caller(WS, ME);
    call(&r, Some(&who), "GET", "/me/contact", None).await;
    t.store()
        .with_conn(|c| {
            Ok(c.execute_batch(&format!(
                "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
                   SELECT 'acc_bot', 'default', 'feishu_app', 'feishu_app', id, 'app_bot', '{app}', '{app}', 'connect', 1, 1
                     FROM parties WHERE workspace_id = 'default' AND org_kind = 'self';
                 INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
                   VALUES ('con_fs', 'default', 'acc_bot', 'feishu', 'credential', '[\"msg.receive\",\"directory.read\"]', 'usr_dev', 1, 1);",
                app = ids::APP_ID
            ))?)
        })
        .expect("connection");
    let q = |sql: &str| -> String {
        t.store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get::<_, String>(0))?))
            .expect(sql)
    };
    let party_of = |union_id: &str| {
        q(&format!(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.family = 'feishu_user' AND h.handle = '{union_id}'"
        ))
    };
    let members = |team_ext: &str| -> Vec<(String, String)> {
        t.store()
            .with_conn(|c| {
                let mut st = c.prepare(
                    "SELECT p.name, m.role FROM team_members m JOIN parties p ON p.id = m.contact_id
                      JOIN team_details d ON d.party_id = m.team_id WHERE d.external_id = ? ORDER BY m.role != 'lead', p.name",
                )?;
                let rows = st.query_map([team_ext], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .expect("members")
    };
    let sync_now = || async {
        let (s, b) = call(&r, Some(&who), "POST", "/teams/sync", None).await;
        assert_eq!(s, 202, "{b}");
        let j = wait(&sync, b["job_id"].as_str().unwrap()).await;
        assert_eq!(j.state, JobState::Done, "{:?}", j.errors);
        j.reports[0].1.clone()
    };

    let rep = sync_now().await;
    assert_eq!(
        (rep.teams, rep.contacts),
        (3, 3),
        "Carol has left and is never created"
    );
    let (_, list) = call(&r, Some(&who), "GET", "/teams", None).await;
    let chart: Vec<(&str, u64)> = list["chart"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["depth"].as_u64().unwrap()))
        .collect();
    assert_eq!(chart, vec![("研发部", 0), ("平台组", 1), ("销售部", 0)]);
    assert_eq!(
        members(ids::DEPT_ENG),
        vec![("李鹏翔".into(), "lead".into())]
    );
    assert_eq!(
        members(ids::DEPT_PLATFORM),
        vec![("王小丽".into(), "lead".into())]
    );
    assert_eq!(
        members(ids::DEPT_SALES),
        vec![("张伟".into(), "lead".into())]
    );
    // union_id is the account, open_id an extra handle of this connection; emails are mail accounts
    let alice = party_of(ids::ALICE.union_id);
    assert_eq!(
        q(&format!(
            "SELECT h.connection_id FROM account_handles h WHERE h.handle = '{}'",
            ids::ALICE.open_id
        )),
        "con_fs"
    );
    assert_eq!(
        q(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.handle = 'alice@corp.example.com'"
        ),
        alice
    );
    let (_, c) = call(&r, Some(&who), "GET", &format!("/contacts/{alice}"), None).await;
    assert_eq!(c["org"]["title"], "后端工程师");
    assert_eq!(c["reports_to"][0]["name"], "李鹏翔");
    let dept_reqs = fake.requests_to(Endpoint::DepartmentChildren);
    assert!(
        dept_reqs[0].query.contains("fetch_child=true"),
        "{:?}",
        dept_reqs[0]
    );

    // Feishu: 平台组 deleted, Bob moves to 研发部, Alice resigns
    fake.world(|w| {
        for d in w.departments.iter_mut() {
            if d.open_department_id == ids::DEPT_PLATFORM {
                d.is_deleted = true;
            }
        }
        for u in w.users.iter_mut() {
            if u.ids.union_id == ids::BOB.union_id {
                u.department_ids = vec![ids::DEPT_ENG.into()];
            }
            if u.ids.union_id == ids::ALICE.union_id {
                u.department_ids = vec![ids::DEPT_ENG.into()];
                u.is_resigned = true;
            }
        }
    });
    let rep = sync_now().await;
    assert_eq!(
        (rep.dissolved, rep.departed, rep.contacts),
        (1, 1, 0),
        "{rep:?}"
    );
    assert_eq!(
        q(&format!(
            "SELECT p.status FROM parties p JOIN team_details d ON d.party_id = p.id WHERE d.external_id = '{}'",
            ids::DEPT_PLATFORM
        )),
        "dissolved"
    );
    assert_eq!(
        members(ids::DEPT_ENG),
        vec![
            ("李鹏翔".into(), "lead".into()),
            ("张伟".into(), "member".into())
        ]
    );
    // still the department's leader in Feishu, so still its lead
    assert_eq!(
        members(ids::DEPT_SALES),
        vec![("张伟".into(), "lead".into())]
    );
    assert_eq!(
        party_of(ids::ALICE.union_id),
        alice,
        "Alice stays a contact"
    );
    let (s, c) = call(&r, Some(&who), "GET", &format!("/contacts/{alice}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(c["teams"], json!([]));
}
