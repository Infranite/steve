//! Session authorization and binding states exercised by Hub tests.

use crate::{Result, Store};

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
        tx.conn.execute(
            "INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,session_id,question,status,requested_by,created)
             VALUES('d1','w','permission','session','s1','s1','?','open','agent',0)",
            [],
        )?;
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
                request,
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
        tx.conn.execute(
            "UPDATE runtime_sessions SET phase='ready' WHERE session_id='s1'",
            [],
        )?;
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
