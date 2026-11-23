//! Workspace membership and path-scope fixtures.

use crate::{Result, Store};

fn workspaces(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO workspaces(id,name,created) VALUES ('solo','solo',1),('team','team',1),('other','other',1);
             INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
               ('u1','team','u1','u1','owner',1),('u2','team','u2','u2','member',1),('u3','other','u3','u3','owner',1);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES
               ('solo','u1','owner',1),('team','u1','owner',1),('team','u2','member',2),('other','u3','owner',1);",
        )?;
        Ok(())
    })
}

pub fn principals(store: &Store) -> Result<()> {
    workspaces(store)?;
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO items(id,workspace_id,requested_by,no,title,status,stage,created,updated) VALUES
               ('i1','solo',NULL,1,'i1','run','define',1,1),('i2','team','u2',2,'i2','run','define',1,1),
               ('i3','team',NULL,3,'i3','run','define',1,1),('i4','team','u9',4,'i4','run','define',1,1),
               ('i6','solo','u1',6,'i6','run','define',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,cwd,harness,status,created,updated) VALUES
               ('s1','solo','i1','.','fixture','ready',1,1),('s2','team','i2','.','fixture','ready',1,1),
               ('s3','team','i3','.','fixture','ready',1,1),('s4','team','i4','.','fixture','ready',1,1),
               ('s5','team',NULL,'.','fixture','ready',1,1),('s6','solo','i6','.','fixture','ready',1,1);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
               SELECT id,'local','{}',id,'r1','ready',1 FROM sessions;",
        )?;
        Ok(())
    })
}

pub fn views(store: &Store) -> Result<()> {
    workspaces(store)?;
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO projects(id,workspace_id,name,path,worktree,created,updated) VALUES
               ('pg','team','pg','/w/git-proj',1,1,1),('pd','team','pd','/w/dir-proj',0,1,1),
               ('po','other','po','/w/other-proj',1,1,1);
             INSERT INTO items(id,workspace_id,requested_by,cwd,project_id,no,title,status,stage,created,updated) VALUES
               ('i1','team','u2','/h/worktrees/ST-1','pg',1,'i1','run','define',1,1),
               ('i2','team',NULL,'/w/dir-proj','pd',2,'i2','run','define',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,cwd,harness,status,created,updated) VALUES
               ('s1','team','i1','/h/worktrees/ST-1','fixture','ready',1,1),
               ('s2','team','i2','/w/dir-proj/','fixture','ready',1,1);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
               SELECT id,'local','{}',id,'r1','ready',1 FROM sessions;
             INSERT INTO kb_sources(id,workspace_id,kind,name,config,visibility,created_by,created,updated) VALUES
               ('k1','team','folder','shared','{\"path\":\"/docs/shared\"}','workspace','u1',1,1),
               ('k2','team','folder','u1','{\"path\":\"/docs/u1-private\"}','private','u1',1,1),
               ('k3','team','folder','u2','{\"path\":\"/docs/u2-private\"}','private','u2',1,1),
               ('k4','other','folder','elsewhere','{\"path\":\"/docs/elsewhere\"}','workspace','u3',1,1),
               ('k5','team','folder','team','{\"path\":\"/docs/team-t1\"}','team:t1','u3',1,1),
               ('k6','team','folder','invalid','not json','workspace','u1',1,1);",
        )?;
        Ok(())
    })
}

pub fn remove_member(store: &Store, ws: &str, user: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?",
            [ws, user],
        )?;
        Ok(())
    })
}

pub fn set_requester(store: &Store, item: &str, requester: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE items SET requested_by=? WHERE id=?",
            [requester, item],
        )?;
        Ok(())
    })
}

pub struct Shadow {
    pub extra_count: Option<i64>,
    pub extra: Option<String>,
    pub missing_count: Option<i64>,
    pub missing: Option<String>,
    pub digest: String,
    pub session_digest: String,
}

pub fn shadow(store: &Store, session: &str) -> Result<Shadow> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT v.extra_count,v.extra,v.missing_count,v.missing,v.digest,r.view_digest
             FROM view_shadow v JOIN runtime_sessions r USING(session_id) WHERE v.session_id=?",
            [session],
            |row| {
                Ok(Shadow {
                    extra_count: row.get(0)?,
                    extra: row.get(1)?,
                    missing_count: row.get(2)?,
                    missing: row.get(3)?,
                    digest: row.get(4)?,
                    session_digest: row.get(5)?,
                })
            },
        )?)
    })
}

pub fn bind_session(w: &mut crate::W<'_>, session: &str, node: &str) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("runtime_sessions")
            .columns([
                "session_id",
                "node_id",
                "workspace_ref",
                "node_session_id",
                "config_revision",
                "phase",
                "updated",
            ])
            .values_panic([
                session.into(),
                node.into(),
                "{}".into(),
                session.into(),
                "r".into(),
                "ready".into(),
                1_i64.into(),
            ]),
    )?;
    Ok(())
}
