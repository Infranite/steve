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
}

pub struct DependencyWaits<'a>(&'a Store);

impl Store {
    pub fn dependency_waits(&self) -> DependencyWaits<'_> {
        DependencyWaits(self)
    }
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
    tx.conn.execute(
        "INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated)
         VALUES(?,?,NULL,'workspace','','waiting',?,?)
         ON CONFLICT(item_id,cycle) DO UPDATE SET state='waiting',updated=excluded.updated",
        params![item, i.cycle, at, at],
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
            "expired",
            &serde_json::Value::Null,
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

    pub fn take_workspace_wait(&self, item: &str, cycle: i64, automatic: bool) -> Result<bool> {
        self.0.write(|w| {
            Ok(w.conn.execute(
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
            )? == 1)
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
            let child:bool=tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_units WHERE item_id=?
                 UNION ALL SELECT 1 FROM workflow_attempts WHERE item_id=?)",
                [item,item],|r|r.get(0))?;
            if child || i.source.1=="workflow" {
                return Err(refuse("工作单元不能登记事项依赖，请向主事项负责人报告阻塞"));
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
                    "SELECT EXISTS(SELECT 1 FROM items p WHERE p.id=? AND p.workspace_id=? AND p.id<>? AND p.kind='task'
                       AND COALESCE(json_extract(p.source,'$[1]'),'')<>'workflow'
                       AND NOT EXISTS(SELECT 1 FROM workflow_units u WHERE u.item_id=p.id)
                       AND NOT EXISTS(SELECT 1 FROM workflow_attempts a WHERE a.item_id=p.id))",
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
            tx.execute("INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated)
                        VALUES(?,?,?,?,?,'waiting',?,?)
                        ON CONFLICT(item_id,cycle) DO UPDATE SET session_id=excluded.session_id,
                        resume=excluded.resume,reason=excluded.reason,state='waiting',updated=excluded.updated",
                       params![item,i.cycle,session,resume,reason,at,at])?;
            for id in all(tx, "SELECT id FROM decisions WHERE item_id=? AND status='open' AND kind IN ('ask','fail')", [item], |r| r.get::<_,String>(0))? {
                crate::DecisionRows(w).settle(&id, &json!({"superseded":"已登记前置等待"}), "expired", &serde_json::Value::Null)?;
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
                    options:Some(vec!["yes".into(),"no".into()]),requested_by:"steve".into(),..Default::default()
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
