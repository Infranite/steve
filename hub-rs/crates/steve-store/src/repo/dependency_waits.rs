//! Durable prerequisite waits. Registration and continuation are transactional so a restart never
//! loses the wake-up between changing the item status and submitting the next prompt.

use rusqlite::params;
use serde::Serialize;
use serde_json::json;

use super::{all, one};
use crate::{Item, NewDecision, Result, Store, StoreError, now};

#[derive(Debug, Clone, Serialize)]
pub struct DependencyWait {
    pub item_id: String,
    pub cycle: i64,
    pub session_id: Option<String>,
    pub resume: String,
    pub reason: String,
    pub state: String,
    pub updated: i64,
    pub wait: steve_types::WaitContract,
}

pub struct DependencyWaits<'a>(&'a Store);

impl Store {
    pub fn dependency_waits(&self) -> DependencyWaits<'_> {
        DependencyWaits(self)
    }
}

fn dependency_owner(item: &Item) -> steve_types::WaitOwner {
    item.bot_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(steve_types::WaitOwner::bot)
        .unwrap_or_else(|| steve_types::WaitOwner::system("dependencies"))
}

fn get(c: &rusqlite::Connection, item: &str) -> Result<Option<DependencyWait>> {
    one(
        c,
        "SELECT w.* FROM item_dependency_waits w JOIN items i ON i.id=w.item_id AND i.cycle=w.cycle WHERE w.item_id=?",
        [item],
        |r| {
            Ok(DependencyWait {
                item_id: r.get("item_id")?,
                cycle: r.get("cycle")?,
                session_id: r.get("session_id")?,
                resume: r.get("resume")?,
                reason: r.get("reason")?,
                state: r.get("state")?,
                updated: r.get("updated")?,
                wait: crate::wait_repo::decode_contract_rusqlite(r)?,
            })
        },
    )
}

fn refuse(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}

/// Ordinary UI/API dependency edits retain a cycle's explicit policy. An old completed wait must
/// not swallow a later unblock or leave a stale "start now" decision while a new prerequisite runs.
pub(super) fn rearm_in(tx: &mut crate::W<'_>, item: &str) -> Result<()> {
    let Some(i) = one(
        tx.conn,
        "SELECT * FROM items WHERE id=?",
        [item],
        Item::from_row,
    )?
    else {
        return Ok(());
    };
    let open:bool=tx.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id WHERE d.item_id=?
         AND (p.id IS NULL OR p.phase<>'closed' OR p.outcome IS NOT 'completed'))",
        [item],|r|r.get(0))?;
    if i.is_closed() || i.archived || !open {
        return Ok(());
    }
    let at = now();
    let wait = crate::wait_repo::contract_for(
        tx,
        &i.workspace_id,
        steve_types::WaitKind::Dependency,
        dependency_owner(&i),
        at,
        steve_types::WaitTimeoutAction::EscalateToOwner,
    )?;
    let wait_owner = wait.owner.to_code();
    tx.conn.execute(
        "INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated,wait_owner,wait_started_at,wait_deadline,wait_on_timeout)
         VALUES(?,?,NULL,'workspace','','waiting',?,?,?,?,?,?)
         ON CONFLICT(item_id,cycle) DO UPDATE SET state='waiting',updated=excluded.updated,
         wait_owner=excluded.wait_owner,wait_started_at=excluded.wait_started_at,
         wait_deadline=excluded.wait_deadline,wait_on_timeout=excluded.wait_on_timeout",
        params![
            item,
            i.cycle,
            at,
            at,
            wait_owner,
            wait.started_at.0,
            wait.deadline.0,
            wait.on_timeout.as_str()
        ],
    )?;
    for id in all(
        tx.conn,
        "SELECT id FROM decisions WHERE item_id=? AND status='open' AND kind='propose'
         AND requested_by='steve' AND question='前置事项都已完成，现在开始？'",
        [item],
        |r| r.get::<_, String>(0),
    )? {
        crate::DecisionRows(tx).settle(
            &id,
            &json!({"superseded":"前置事项已变化"}),
            crate::decision_repo::DecisionChange::Expire,
            None,
        )?;
    }
    Ok(())
}

impl DependencyWaits<'_> {
    pub fn get(&self, item: &str) -> Result<Option<DependencyWait>> {
        self.0.read_conn(|c| get(c, item))
    }

    pub fn arm(&self, item: &str) -> Result<()> {
        self.0.write(|w| rearm_in(w, item))
    }

    pub fn take_workspace_wait(
        &self,
        item: &str,
        cycle: i64,
        automatic: bool,
    ) -> Result<Option<i64>> {
        self.0.write(|w| {
            let Some(wait) = get(w.conn, item)?
                .filter(|wait| wait.cycle == cycle && wait.state == "waiting" && wait.session_id.is_none())
            else {
                return Ok(None);
            };
            let updated = w.conn.execute(
                "UPDATE item_dependency_waits SET state=?,updated=?
                 WHERE item_id=? AND cycle=? AND state='waiting' AND session_id IS NULL
                   AND EXISTS(SELECT 1 FROM items i WHERE i.id=item_dependency_waits.item_id
                     AND i.cycle=item_dependency_waits.cycle AND i.archived=0 AND i.phase IN ('ready','active'))
                   AND NOT EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
                     WHERE d.item_id=item_dependency_waits.item_id
                     AND (p.phase IS NOT 'closed' OR p.outcome IS NOT 'completed'))",
                params![
                    if automatic { "started" } else { "notified" },
                    now(),
                    item,
                    cycle
                ],
            )?;
            Ok((updated == 1).then_some(wait.updated))
        })
    }

    /// The caller authenticates the session and its disclosure scope. Validate the database again
    /// under the write lock: a partially valid batch must never leave behind an edge or a wait.
    pub fn register(
        &self,
        item: &str,
        session: &str,
        deps: &[String],
        resume: &str,
        reason: &str,
    ) -> Result<bool> {
        self.0.write(|w| {
            let tx = w.conn;
            let i = one(tx, "SELECT * FROM items WHERE id=?", [item], Item::from_row)?
                .ok_or_else(|| refuse("没有这个事项"))?;
            if i.phase != steve_types::ItemPhase::Active || i.archived {
                return Err(refuse("事项正在结束或已结束，不能登记等待"));
            }
            let authorized: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions s WHERE s.id=? AND s.item_id=? AND
                 s.workspace_id=? AND s.bot_id IS ? AND s.readonly=0 AND s.status<>'ended'
                 AND s.source='Steve' AND s.device_id='hub'
                 AND EXISTS(SELECT 1 FROM bots b JOIN bot_workspaces m ON m.bot_id=b.id
                   WHERE b.id=s.bot_id AND b.enabled=1 AND b.deleted=0 AND m.workspace_id=s.workspace_id)
                 AND s.id=(SELECT id FROM sessions WHERE item_id=? AND status<>'ended'
                           AND readonly=0 AND source='Steve' AND device_id='hub'
                           ORDER BY no DESC,created DESC LIMIT 1))",
                params![session,item,i.workspace_id,i.bot_id,item], |r| r.get(0))?;
            if !authorized {
                return Err(refuse("只有当前有效事项会话可以登记等待"));
            }
            if deps.is_empty() || reason.trim().is_empty()
                || !["automatic", "notify", "workspace"].contains(&resume) {
                return Err(refuse("需要有效依赖、原因与续跑方式"));
            }
            let open_acceptance: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM decisions WHERE item_id=? AND status='open' AND kind NOT IN ('ask','fail'))",
                [item], |r| r.get(0))?;
            if open_acceptance {
                return Err(refuse("事项还有验收或授权待处理，不能改为前置等待"));
            }
            let mut added = false;
            for dep in deps {
                let valid: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM items p WHERE p.id=? AND p.workspace_id=? AND p.id<>? AND p.kind='task')",
                    params![dep,i.workspace_id,item], |r| r.get(0))?;
                if !valid {
                    return Err(refuse("前置事项不存在、不可用或不属于当前工作空间"));
                }
                let cycle: bool = tx.query_row(
                    "WITH RECURSIVE reachable(id) AS (
                      SELECT dep_id FROM item_deps WHERE item_id=?
                      UNION SELECT d.dep_id FROM item_deps d JOIN reachable r ON d.item_id=r.id
                     ) SELECT EXISTS(SELECT 1 FROM reachable WHERE id=?)",
                    params![dep,item], |r| r.get(0))?;
                if cycle {
                    return Err(refuse("会形成循环依赖，没有添加"));
                }
                added |= tx.execute(
                    "INSERT OR IGNORE INTO item_deps(item_id,dep_id,created) VALUES(?,?,?)",
                    params![item,dep,now()])? > 0;
            }
            let has_open: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
                 WHERE d.item_id=? AND (p.id IS NULL OR p.phase<>'closed' OR p.outcome IS NOT 'completed'))",
                [item], |r|r.get(0))?;
            if !added && get(tx,item)?.is_some_and(|w| w.session_id.as_deref()==Some(session) && w.resume==resume && w.reason==reason
                && (w.state=="waiting" || !has_open)) {
                return Ok(false);
            }
            let at = now();
            let wait = crate::wait_repo::contract_for(
                w,
                &i.workspace_id,
                steve_types::WaitKind::Dependency,
                dependency_owner(&i),
                at,
                steve_types::WaitTimeoutAction::EscalateToOwner,
            )?;
            let wait_owner = wait.owner.to_code();
            let tx = w.conn;
            tx.execute("INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated,wait_owner,wait_started_at,wait_deadline,wait_on_timeout)
                        VALUES(?,?,?,?,?,'waiting',?,?,?,?,?,?)
                        ON CONFLICT(item_id,cycle) DO UPDATE SET session_id=excluded.session_id,
                        resume=excluded.resume,reason=excluded.reason,state='waiting',updated=excluded.updated,
                        wait_owner=excluded.wait_owner,wait_started_at=excluded.wait_started_at,
                        wait_deadline=excluded.wait_deadline,wait_on_timeout=excluded.wait_on_timeout",
                       params![item,i.cycle,session,resume,reason,at,at,wait_owner,wait.started_at.0,wait.deadline.0,wait.on_timeout.as_str()])?;
            for id in all(tx, "SELECT id FROM decisions WHERE item_id=? AND status='open' AND kind IN ('ask','fail')", [item], |r| r.get::<_,String>(0))? {
                crate::DecisionRows(w).settle(&id, &json!({"superseded":"已登记前置等待"}), crate::decision_repo::DecisionChange::Expire, None)?;
            }
            crate::item_repo::store_timeline(w,item,"steve","sys",&format!("登记前置等待（{resume}）：{reason}"),Some(session),None,crate::now())?;
            w.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: i.workspace_id.clone().into(),
                    id: item.into(),
                },
            ));
            Ok(true)
        })
    }

    /// Persist both the wake-up and the status change. `queued` is claimed by the existing runtime,
    /// whose Node command ledger already owns retries and attempt identity.
    pub fn ready(&self, item: &str, automatic: bool, session: &str, text: &str) -> Result<bool> {
        self.0.write(|w| {
            let tx = w.conn;
            let Some(wait) = get(tx,item)?.filter(|w| w.state=="waiting") else { return Ok(false); };
            let eligible: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM items i WHERE i.id=? AND i.phase IN ('ready','active')
                 AND i.archived=0 AND NOT EXISTS(SELECT 1 FROM decisions WHERE item_id=i.id AND status='open')
                 AND NOT EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
                 WHERE d.item_id=i.id AND (p.id IS NULL OR p.phase<>'closed' OR p.outcome IS NOT 'completed')))",
                [item], |r| r.get(0))?;
            if !eligible { return Ok(false); }
            let i = one(tx,"SELECT * FROM items WHERE id=?",[item],Item::from_row)?.unwrap();
            let at = now();
            let (state,note) = if automatic { ("started","前置事项都已完成，Steve 自动开始") }
                                    else { ("notified","前置事项都已完成，等你开始") };
            if automatic {
                let live: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions s WHERE s.id=? AND s.item_id=? AND s.bot_id IS ?
                     AND s.workspace_id=? AND s.readonly=0 AND s.status<>'ended' AND s.source='Steve' AND s.device_id='hub'
                     AND s.id=(SELECT id FROM sessions WHERE item_id=s.item_id AND status<>'ended'
                       AND readonly=0 AND source='Steve' AND device_id='hub' ORDER BY no DESC,created DESC LIMIT 1)
                     AND EXISTS(SELECT 1 FROM bots b JOIN bot_workspaces m ON m.bot_id=b.id
                       WHERE b.id=s.bot_id AND b.enabled=1 AND b.deleted=0 AND m.workspace_id=s.workspace_id))",
                    params![session,item,i.bot_id,i.workspace_id], |r| r.get(0))?;
                if !live { return Err(refuse("当前会话不可续跑")); }
                tx.execute("INSERT INTO queued(id,session_id,text,via,created,files) VALUES(?,?,?,'steve',?,'[]')",
                           params![crate::uid("q"),session,text,at])?;
                w.emit(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Session {
                        ws: i.workspace_id.clone().into(),
                        id: session.into(),
                        item_id: {
                            let value: Option<Option<String>> = Some(Some(item.into()));
                            value.map(|value| value.map(Into::into))
                        },
                    },
                ));
            } else {
                crate::DecisionRows(w).open(&NewDecision {
                    workspace_id:i.workspace_id.clone(),kind:"propose".into(),subject_type:"item".into(),
                    subject_id:item.into(),item_id:Some(item.into()),question:"前置事项都已完成，现在开始？".into(),
                    options:Some(vec!["yes".into(),"no".into()]),requested_by:"steve".into(),
                    source: Some(crate::DecisionSource::Revision {
                        revision: format!("deps:{}:{}", wait.cycle, wait.updated),
                    }),
                    ..Default::default()
                })?;
            }
            tx.execute("UPDATE item_dependency_waits SET state=?,updated=? WHERE item_id=? AND cycle=?",
                       params![state,at,item,wait.cycle])?;
            crate::item_repo::store_timeline(w,item,"steve","sys",note,None,None,crate::now())?;
            w.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: i.workspace_id.clone().into(),
                    id: item.into(),
                },
            ));
            Ok(true)
        })
    }
}
