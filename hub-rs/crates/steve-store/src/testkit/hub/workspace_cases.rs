//! Workspace placement fixtures for the Hub integration tests.

use crate::{Result, Store};
use steve_node_api::WorkspaceRef;

pub fn seed_item_session(store: &Store, c: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(&format!(
                "INSERT INTO items(id, workspace_id, no, title, phase, cwd, created, updated) VALUES ('i1','default',1,'t','active','{c}',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','{c}','active',1,1);",
            ))?;
        Ok(())
    })
}

pub fn seed_session_item_and_knowledge(store: &Store, c: &str, k: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(&format!(
                "INSERT INTO items(id, workspace_id, no, title, phase, cwd, created, updated) VALUES ('i1','default',1,'t','active','{c}',1,1),('i2','default',2,'t','active',NULL,1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','{c}','active',1,1),('s2','default','i1','claude-code','','active',2,2);
                 INSERT INTO kb_sources(id,workspace_id,kind,name,config,created_by,created,updated) VALUES ('k1','default','folder','wiki','{{\"path\":\"{k}\"}}','u',1,1),('k2','default','notes','n','{{}}','u',1,1);",
            ))?;
        Ok(())
    })
}

pub fn seed_directory_project_item(store: &Store, c: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(&format!(
                "INSERT INTO items(id, workspace_id, no, title, phase, cwd, project_id, created, updated) VALUES ('i9','default',9,'t','active','{c}','p9',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s9','default','i9','claude-code','{c}','active',1,1);",
            ))?;
        Ok(())
    })
}

pub fn seed_project(store: &Store, id: &str, device: &str, path: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated)
                    VALUES(?1,'default',?1,?2,'dir',?3,1,1)",
            [id, path, device],
        )?;
        Ok(())
    })
}

pub fn set_project_path(store: &Store, id: &str, path: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE projects SET path=? WHERE id=?", [path, id])?;
        Ok(())
    })
}

pub fn bind_placement(store: &Store, session: &str, workspace: &WorkspaceRef) -> Result<()> {
    store.write(|tx| {
        let json = serde_json::to_string(workspace)?;
        tx.conn.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
                    VALUES (?1,'local',?2,?1,'r','ready',0)", [session, &json])?;
        Ok(())
    })
}

pub fn record_existing_prepare(
    store: &Store,
    kind: &steve_node_api::PrepareKind,
    request: &str,
) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("UPDATE node_commands SET request=json_set(request,'$.body.kind',json(?)) WHERE node_id='local' AND method='prepare_workspace' AND json_extract(request,'$.body.resource')='item:i1'", [serde_json::to_string(kind)?])?;
        tx.conn.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated,intent_purpose,intent_subject,intent_revision) VALUES('prepare:s0:r0','local','prepare_workspace',?,'d','finished',1,1,'prepare','s0',0)", [request])?;
        Ok(())
    })
}

pub fn seed_remote_item(store: &Store, c: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(&format!(
                "INSERT INTO items(id, workspace_id, no, title, phase, cwd, created, updated) VALUES ('i1','default',1,'t','active','{c}',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','{c}','active',1,1);
                 INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1),('m','default','sub_m','m','member',1);
                 INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_p','u','p',zeroblob(32),'fp','paused',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_m','m','m',zeroblob(32),'fm','active',1);",
            ))?;
        Ok(())
    })
}

pub fn seed_permission_modes(store: &Store, bots: &[Option<&str>]) -> Result<()> {
    let mut sql = String::new();
    for (n, bot) in bots.iter().enumerate() {
        let bot_id = match bot {
            Some(m) => {
                sql += &format!(
                        "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,revision,updated) VALUES ('b{n}','default','b{n}','claude-code','{m}',1,1);
                         INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('b{n}','default',1);"
                    );
                format!("'b{n}'")
            }
            None => "NULL".into(),
        };
        for ro in [0, 1] {
            sql += &format!(
                    "INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated) VALUES ('i{n}_{ro}','default',{no},'t','active',{bot_id},1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,readonly,created,updated) VALUES ('s{n}_{ro}','default','i{n}_{ro}',{bot_id},'claude-code','','active',{ro},1,1);",
                    no = n * 2 + ro + 1
                );
        }
    }
    store.write(|tx| {
        tx.conn.execute_batch(&sql)?;
        Ok(())
    })
}

pub fn seed_item_and_active_session(store: &Store, item: &str, session: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,phase,created,updated)
            SELECT ?,'default',COALESCE(MAX(no)+1,1),'t','active',1,1
            FROM items WHERE workspace_id='default'", [item])?;
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES (?,'default',?,'claude-code','.','active',1,1)", [session, item])?;
        Ok(())
    })
}
