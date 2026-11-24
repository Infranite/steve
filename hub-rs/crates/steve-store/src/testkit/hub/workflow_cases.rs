//! Workflow and session authorization states exercised by the Hub tests.

use crate::{Result, Store};

#[derive(Clone, Copy, Debug)]
pub enum WorkflowGate {
    CycleChanged,
    GoalChanged,
    WorkflowPaused,
    ItemPaused,
    ItemClosed,
    ItemStopping,
    UnitSucceeded,
    UnitCancelled,
    UnitUnbound,
    SessionEnded,
}

pub fn set_workflow_gate(store: &Store, gate: WorkflowGate, enabled: bool) -> Result<()> {
    let sql = match (gate, enabled) {
        (WorkflowGate::CycleChanged, true) => "UPDATE items SET cycle=2 WHERE id='parent'",
        (WorkflowGate::CycleChanged, false) => "UPDATE items SET cycle=1 WHERE id='parent'",
        (WorkflowGate::GoalChanged, true) => {
            "UPDATE item_goals SET version=2 WHERE item_id='parent'"
        }
        (WorkflowGate::GoalChanged, false) => {
            "UPDATE item_goals SET version=1 WHERE item_id='parent'"
        }
        (WorkflowGate::WorkflowPaused, true) => "UPDATE workflows SET paused=1",
        (WorkflowGate::WorkflowPaused, false) => "UPDATE workflows SET paused=0",
        (WorkflowGate::ItemPaused, true) => "UPDATE items SET phase='paused' WHERE id='parent'",
        (WorkflowGate::ItemPaused, false) => "UPDATE items SET phase='active' WHERE id='parent'",
        (WorkflowGate::ItemClosed, true) => "UPDATE items SET phase='closed' WHERE id='parent'",
        (WorkflowGate::ItemClosed, false) => "UPDATE items SET phase='active' WHERE id='parent'",
        (WorkflowGate::ItemStopping, true) => {
            "UPDATE items SET phase='closing',closing='{\"kind\":\"cancel\"}' WHERE id='parent'"
        }
        (WorkflowGate::ItemStopping, false) => {
            "UPDATE items SET phase='active',closing=NULL WHERE id='parent'"
        }
        (WorkflowGate::UnitSucceeded, true) => "UPDATE workflow_units SET status='succeeded'",
        (WorkflowGate::UnitSucceeded, false) => "UPDATE workflow_units SET status='running'",
        (WorkflowGate::UnitCancelled, true) => "UPDATE workflow_units SET status='cancelled'",
        (WorkflowGate::UnitCancelled, false) => "UPDATE workflow_units SET status='running'",
        (WorkflowGate::UnitUnbound, true) => "UPDATE workflow_units SET session_id=NULL",
        (WorkflowGate::UnitUnbound, false) => "UPDATE workflow_units SET session_id='work'",
        (WorkflowGate::SessionEnded, true) => "UPDATE sessions SET status='ended' WHERE id='work'",
        (WorkflowGate::SessionEnded, false) => {
            "UPDATE sessions SET status='active' WHERE id='work'"
        }
    };
    store.write(|tx| {
        tx.conn.execute_batch(sql)?;
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum PrincipalGate {
    Requester,
    Principal,
    DisabledBot,
    Membership,
}

pub fn set_principal_gate(
    store: &Store,
    item: &str,
    gate: PrincipalGate,
    enabled: bool,
) -> Result<()> {
    let user = if enabled { "other" } else { "requester" };
    store.write(|tx| {
        match gate {
            PrincipalGate::Requester => tx.conn.execute("UPDATE items SET requested_by=? WHERE id=?", [user, item])?,
            PrincipalGate::Principal => tx.conn.execute("UPDATE runtime_sessions SET principal=?", [user])?,
            PrincipalGate::DisabledBot => tx.conn.execute("UPDATE bots SET enabled=? WHERE id='writer'", [!enabled])?,
            PrincipalGate::Membership if enabled => tx.conn.execute("DELETE FROM workspace_members WHERE user_id='requester'", [])?,
            PrincipalGate::Membership => tx.conn.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1)", [])?,
        };
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum RefusalCase {
    UnsupportedMode,
    MissingBot,
    ArchivedProject,
    DetachedItem,
    ReadOnly,
}

pub fn seed_session_refusal(store: &Store, case: RefusalCase) -> Result<()> {
    let sql = match case {
        RefusalCase::UnsupportedMode => {
            "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,updated) VALUES ('b1','default','b','claude-code','dontAsk',1); INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('b1','default',1); UPDATE sessions SET bot_id='b1'"
        }
        RefusalCase::MissingBot => "UPDATE sessions SET bot_id='nobody'",
        RefusalCase::ArchivedProject => {
            "INSERT INTO projects(id,workspace_id,name,path,kind,archived,created,updated) VALUES ('p1','default','p','/nonexistent-steve-b5','dir',1,1,1); UPDATE items SET project_id='p1'"
        }
        RefusalCase::DetachedItem => "UPDATE sessions SET item_id=NULL",
        RefusalCase::ReadOnly => "UPDATE sessions SET readonly=1",
    };
    store.write(|tx| {
        tx.conn.execute_batch(sql)?;
        Ok(())
    })
}

pub fn seed_open_permission(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,session_id,question,status,requested_by,created)
                     VALUES('d1','w','permission','session','s1','s1','?','open','agent',0)", [])?;
        Ok(())
    })
}

pub fn seed_remote_open_session(store: &Store, request: &serde_json::Value) -> Result<()> {
    store.write::<_, crate::StoreError>(|tx| {
                tx.conn.execute(
                    "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s1','default','claude-code','.','active',1,1)",
                    [],
                )?;
                crate::runtime::bind_session_in(
                    tx,
                    &crate::runtime::SessionBinding {
                        session: "s1",
                        node: "n2",
                        workspace: &serde_json::json!({"node": "n2", "id": "w1"}),
                        node_session: "s1",
                        config_revision: "r1",
                    },
                    1,
                )?;
                crate::runtime::insert_command_in(
                    tx,
                    &crate::runtime::NewCommand {
                        operation: "open_s1",
                        node: "n2",
                        method: "open_session",
                        request: request,
                        digest: "d",
                        target_attempt: None,
                        dependency: None,
                    },
                    1,
                )?;
                crate::node_write::merge_receipt_in(
                    tx,
                    "open_s1",
                    crate::runtime::ReceiptState::Finished,
                    true,
                    &serde_json::json!({}),
                    2,
                )?;
                tx.conn.execute("UPDATE runtime_sessions SET phase='ready' WHERE session_id='s1'", [])?;
                Ok(())
            })
}

pub fn seed_local_binding(store: &Store) -> Result<()> {
    store.write::<_, crate::StoreError>(|tx| {
                tx.conn.execute(
                    "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s1','default','claude-code','.','active',1,1)",
                    [],
                )?;
                crate::runtime::bind_session_in(
                    tx,
                    &crate::runtime::SessionBinding {
                        session: "s1",
                        node: "local",
                        workspace: &serde_json::json!({"node": "local", "id": "w1"}),
                        node_session: "s1",
                        config_revision: "r1",
                    },
                    1,
                )?;
                Ok(())
            })
}
