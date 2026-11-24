//! Database setup and fault injection for Hub test scenarios.

use crate::contract::Q;
use crate::{Result, Store};

macro_rules! mutation {
    ($name:ident, $sql:literal) => {
        pub fn $name(store: &Store) -> Result<()> {
            store.write(|w| w.db().batch($sql))
        }
    };
}

mutation!(
    clear_workspace_members_user_id_m,
    "DELETE FROM workspace_members WHERE user_id='m'"
);

mutation!(
    clear_workspace_members_user_id_requester,
    "DELETE FROM workspace_members WHERE user_id='requester'"
);

mutation!(
    create_fail_results,
    "CREATE TRIGGER fail_results BEFORE INSERT ON usage_records BEGIN SELECT RAISE(ABORT,'projection outage'); END"
);

mutation!(
    create_refuse,
    "CREATE TRIGGER refuse BEFORE INSERT ON events WHEN NEW.data LIKE '%conflicting%' BEGIN SELECT RAISE(ABORT,'refused'); END;"
);

mutation!(
    create_refuse_exit_apply,
    "CREATE TRIGGER refuse_exit_apply BEFORE UPDATE OF applied ON node_streams
            WHEN NEW.stream='exit-evidence' AND NEW.applied>OLD.applied
            BEGIN SELECT RAISE(ABORT, 'test apply rollback'); END;"
);

mutation!(
    create_refuse_for_an_idle_session_on_a_faulted_stream_plans_no_turn,
    "CREATE TRIGGER refuse BEFORE UPDATE OF node_revision ON runtime_sessions BEGIN SELECT RAISE(ABORT,'refused'); END;"
);

mutation!(
    create_refuse_push,
    "CREATE TRIGGER refuse_push BEFORE INSERT ON node_commands
                     BEGIN SELECT RAISE(ABORT, 'test submit failure'); END"
);

mutation!(
    create_reject_failed_update,
    "CREATE TRIGGER reject_failed_update BEFORE UPDATE ON flow_runs WHEN NEW.status = 'failed' BEGIN SELECT RAISE(ABORT, 'cannot update failed start'); END"
);

mutation!(
    create_reject_running_start,
    "CREATE TRIGGER reject_running_start BEFORE INSERT ON flow_runs WHEN NEW.status='running' BEGIN SELECT RAISE(ABORT,'running insert unavailable'); END"
);

mutation!(remove_decisions, "DROP TABLE decisions");

mutation!(
    remove_fail_results,
    "DROP TRIGGER fail_results; CREATE TRIGGER fail_results BEFORE INSERT ON usage_records WHEN json_extract(NEW.detail,'$.call_id') != 'result-128' BEGIN SELECT RAISE(ABORT,'isolated bad calls'); END"
);

mutation!(remove_refuse_exit_apply, "DROP TRIGGER refuse_exit_apply");

mutation!(remove_refuse_push, "DROP TRIGGER refuse_push");

mutation!(remove_runtime_sessions, "DROP TABLE runtime_sessions");

mutation!(
    seed_bots,
    "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('writer', 'default', '写作', 'claude-code', 1);"
);

mutation!(seed_bots_for_the_steve_dev_bot_binds_in_auto, "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,revision,updated) VALUES ('dev','default','Steve 开发','claude-code','auto',3,1);
                     INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('dev','default',1);
                     INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated) VALUES ('i1','default',1,'t','active','dev',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','dev','claude-code','','active',1,1);");

mutation!(seed_items, "INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('i1','default',1,'t','active',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,acp_session_id,created,updated) VALUES ('s1','default','i1','claude-code','','idle',NULL,1,1);");

mutation!(
    seed_items_for_a_remote_node_gets_only_work_of_a_workspace_its_pairer_owns,
    "INSERT INTO items(id, workspace_id, no, title, phase, project_id, created, updated) VALUES ('iu','default',1,'t','active','pu',1,1),('im','default',2,'t','active','pm',1,1);"
);

mutation!(seed_items_for_observation_preserves_results_partial_effects_and_cancellation_without_retry, "INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('i1','default',1,'Test','active',1,1);
                INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES('s1','default','i1','fixture','','active',1,1);");

mutation!(seed_items_for_retry_fixture, "INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('i1','default',1,'retry','active',1,1);
            INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','.','active',1,1);");

mutation!(seed_items_for_workflow_pause_holds_node_fact_followups_until_explicit_resume, "INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('parent','default',2,'主事项','active',1,1);
             INSERT INTO item_goals(item_id,version,text,created)
             VALUES ('parent',1,'验证暂停和恢复',1);
             INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
             VALUES ('parent',1,2,3,1,1);
             INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
             VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作\",\"goal\":\"测试\",\"bot_id\":\"dev\"}','running','i1','s1');");

mutation!(seed_items_for_workflow_sessions_without_a_node_never_fall_back, "INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('parent','default',1,'主事项','active',1,1),('child','default',2,'工作单元','active',1,1);
                 INSERT INTO item_goals(item_id,version,text,created)
                 VALUES ('parent',1,'验证运行隔离',1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated)
                 VALUES ('work','default','child','unused-acp','','active',1,1);
                 INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
                 VALUES ('parent',1,2,3,1,1);
                 INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
                 VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作单元\",\"goal\":\"验证隔离\",\"bot_id\":\"test\"}','running','child','work');");

mutation!(seed_items_item_id_child_id_s1, "INSERT INTO items(id, workspace_id, no, title, phase, source, created, updated) VALUES ('parent','default',1,'主事项','active','[\"测试\",\"test\"]',1,1),('child','default',2,'工作','active','[\"协作工作\",\"workflow\"]',1,1);
                 INSERT INTO item_goals(item_id,version,text,created)
                 VALUES ('parent',1,'验证旧授权失效',1);
                 UPDATE sessions SET item_id='child' WHERE id='s1';
                 INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
                 VALUES ('parent',1,2,3,1,1);
                 INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
                 VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作\",\"goal\":\"测试\",\"bot_id\":\"dev\"}','running','child','s1');");

mutation!(seed_users, "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
             INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_far','u','far',zeroblob(32),'ff','active',1);");

mutation!(seed_users_for_a_remote_node_gets_only_work_of_a_workspace_its_pairer_owns, "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1),('m','default','sub_m','m','member',1);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
             INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_u','u','u',zeroblob(32),'fu','active',1),('n_m','m','m',zeroblob(32),'fm','active',1);
");

mutation!(seed_users_for_new, "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('requester','default','requester','Requester','member',1),('other','default','other','Other','member',1);
            INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1),('default','other','member',1);");

mutation!(seed_users_item_id_i1_id_s1, "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','u','u','owner',1);
                 INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n1','u','n1',zeroblob(32),'f1','active',1);
                 INSERT INTO items(id, workspace_id, no, title, phase, created, updated, requested_by) VALUES ('i1','default',1,'t','active',1,1,'u');
                 UPDATE sessions SET item_id='i1' WHERE id='s1';
                 INSERT INTO node_env(node_id,principal,key,value,updated_by,updated) VALUES
                   ('n1','u','NPM_TOKEN','tok','u',1),
                   ('n2','u','ELSEWHERE','x','u',1),
                   ('n1','someone','THEIRS','y','u',1);");

mutation!(
    seed_workspace_members,
    "INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','m','member',2);"
);

mutation!(seed_workspace_members_for_admission_and_queued_followups_refuse_unknown_changed_principals_bots_and_nodes, "INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1)");

mutation!(seed_workspaces, "INSERT INTO workspaces(id,name,created) VALUES ('home','Home',1);
                     INSERT INTO bots(id,workspace_id,name,harness,revision,updated) VALUES ('shared','home','Shared','fixture',2,1);
                     INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('shared','home',1),('shared','default',1);
                     INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated) VALUES ('i1','default',1,'t','active','shared',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','shared','fixture','','active',1,1);");

mutation!(
    update_items_bot_id_dev_id_i1,
    "UPDATE items SET bot_id='dev' WHERE id='i1'; UPDATE sessions SET bot_id='dev' WHERE id='s1';"
);

mutation!(
    update_items_bot_id_test_id_child,
    "UPDATE items SET bot_id='test' WHERE id='child'; UPDATE sessions SET bot_id='test' WHERE id='work'"
);

mutation!(
    update_items_requested_by_requester,
    "UPDATE items SET requested_by='requester'"
);

mutation!(
    update_items_status_pause_id_i1,
    "UPDATE items SET phase='paused' WHERE id='i1'"
);

mutation!(
    update_items_status_run_id_i1,
    "UPDATE items SET phase='active' WHERE id='i1'"
);

mutation!(
    update_messages_id_reply_unbound_status_failed,
    "UPDATE messages SET meta='{}' WHERE id='reply_unbound';
              UPDATE thread_runs SET status='failed' WHERE id='run_unbound';"
);

mutation!(
    update_nodes_state_paused_node_id_n_u,
    "UPDATE nodes SET state='paused' WHERE node_id='n_u'"
);

mutation!(
    update_projects_id_p3,
    "UPDATE projects SET archived=1 WHERE id='p3'"
);

mutation!(
    update_projects_id_p9,
    "UPDATE projects SET archived=1 WHERE id='p9'"
);

mutation!(
    update_runtime_attempts_state_finished,
    "UPDATE runtime_attempts SET state='finished';
                     UPDATE runtime_turns SET active_attempt=NULL;
                     UPDATE runtime_sessions SET active_attempt=NULL;"
);

mutation!(update_sessions_status_ended_id_s1, "UPDATE sessions SET status='ended' WHERE id='s1';
        INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project','/project','git',1,1,1);
        UPDATE items SET project_id='p1',branch='steve/i1' WHERE id='i1';
        INSERT INTO sessions(id,workspace_id,item_id,no,harness,cwd,status,created,updated) VALUES('s2','default','i1',2,'claude-code','','starting',2,2);");
