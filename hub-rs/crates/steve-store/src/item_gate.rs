//! Persisted item-level execution gates shared by session routing and Node turn planning.
//! Accepts a read/write handle so the check and a Node turn claim share one transaction.

use rusqlite::{Connection, OptionalExtension};

/// Structured reason an item session cannot start another turn or run fresh tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateBlocker {
    /// The item is paused until resumed.
    ItemPaused,
    /// The item has an active prerequisite wait in the current cycle.
    DependencyWait,
    /// At least one declared prerequisite item is not yet completed.
    OpenPrerequisite,
}

impl GateBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ItemPaused => "item_paused",
            Self::DependencyWait => "dependency_wait",
            Self::OpenPrerequisite => "open_prerequisite",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "item_paused" => Some(Self::ItemPaused),
            "dependency_wait" => Some(Self::DependencyWait),
            "open_prerequisite" => Some(Self::OpenPrerequisite),
            _ => None,
        }
    }
}

/// Persisted execution gate for an item session. State-only: runtime authorization and
/// execution policy are enforced separately by the Node binding layer.
pub(crate) fn session_blocker_on(
    c: &Connection,
    session: &str,
) -> crate::Result<Option<GateBlocker>> {
    let code = c
        .query_row(
            "SELECT CASE
              WHEN i.phase='paused' THEN 'item_paused'
              WHEN EXISTS(
                SELECT 1 FROM item_dependency_waits dw
                WHERE dw.item_id=i.id AND dw.cycle=i.cycle AND dw.state='waiting'
              ) THEN 'dependency_wait'
              WHEN EXISTS(
                SELECT 1 FROM item_deps d
                LEFT JOIN items dep ON dep.id=d.dep_id
                WHERE d.item_id=i.id
                  AND (dep.id IS NULL OR dep.phase<>'closed' OR dep.outcome IS NOT 'completed')
              ) THEN 'open_prerequisite'
              ELSE NULL END
             FROM sessions s
             JOIN items i ON i.id=s.item_id
             WHERE s.id=?",
            [session],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(code.as_deref().and_then(GateBlocker::from_code))
}

pub fn session_blocker(
    r: &mut impl crate::Read,
    session: &str,
) -> crate::Result<Option<GateBlocker>> {
    session_blocker_on(r.connection(), session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoPublisher, Store};

    #[test]
    fn reports_typed_blocker_codes_for_paused_wait_and_prerequisite() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store
            .write(|w| {
                w.conn.execute_batch(
                    "INSERT INTO workspaces(id,name,created) VALUES ('w','W',0);
                     INSERT INTO items(id,workspace_id,no,title,phase,cycle,created,updated)
                       VALUES ('ST-1','w',1,'Main','active',1,0,0),
                              ('ST-2','w',2,'Dep','active',1,0,0);
                     INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated)
                       VALUES ('s1','w','ST-1','fixture','/tmp','idle',0,0);",
                )?;
                assert_eq!(session_blocker(w, "s1")?, None);

                w.conn
                    .execute("UPDATE items SET phase='paused' WHERE id='ST-1'", [])?;
                assert_eq!(session_blocker(w, "s1")?, Some(GateBlocker::ItemPaused));

                w.conn
                    .execute("UPDATE items SET phase='active' WHERE id='ST-1'", [])?;
                w.conn.execute(
                    "INSERT INTO item_dependency_waits(
                       item_id,cycle,session_id,resume,reason,state,created,updated,
                       wait_owner,wait_started_at,wait_deadline,wait_on_timeout
                     )
                     VALUES ('ST-1',1,'s1','automatic','wait','waiting',0,0,?1,0,?2,?3)",
                    rusqlite::params![
                        steve_types::WaitOwner::system("dependencies").to_code(),
                        steve_types::wait::WAIT_LIMIT_DEPENDENCY_MS,
                        steve_types::WaitTimeoutAction::EscalateToOwner.as_str(),
                    ],
                )?;
                assert_eq!(session_blocker(w, "s1")?, Some(GateBlocker::DependencyWait));

                // Once a wait times out (escalated to owner), it no longer holds DependencyWait;
                // open prerequisites still block through OpenPrerequisite below.
                w.conn.execute(
                    "UPDATE item_dependency_waits SET state='timed_out' WHERE item_id='ST-1'",
                    [],
                )?;
                assert_eq!(session_blocker(w, "s1")?, None);

                w.conn.execute(
                    "UPDATE item_dependency_waits SET state='started' WHERE item_id='ST-1'",
                    [],
                )?;
                w.conn.execute(
                    "INSERT INTO item_deps(item_id,dep_id,created) VALUES ('ST-1','ST-2',0)",
                    [],
                )?;
                assert_eq!(
                    session_blocker(w, "s1")?,
                    Some(GateBlocker::OpenPrerequisite)
                );

                w.conn.execute(
                    "UPDATE item_dependency_waits SET state='timed_out' WHERE item_id='ST-1'",
                    [],
                )?;
                assert_eq!(
                    session_blocker(w, "s1")?,
                    Some(GateBlocker::OpenPrerequisite)
                );

                w.conn.execute(
                    "UPDATE items SET phase='closed',outcome='completed' WHERE id='ST-2'",
                    [],
                )?;
                assert_eq!(session_blocker(w, "s1")?, None);
                Ok::<_, crate::StoreError>(())
            })
            .unwrap();
    }
}
