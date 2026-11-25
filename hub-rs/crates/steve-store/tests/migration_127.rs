//! 127: adds unified WaitContract columns, CHECK constraints, due indexes, and backfills
//! across decisions, item_dependency_waits, item_operations, gh_effects, mail_drafts, and flow_waits.

use rusqlite::Connection;
use steve_store::testkit::SqliteFixtures;
use steve_store::{DecisionRows, FlowWaits, NoPublisher, Store, db, github, mail, operation_repo};
use steve_types::{Millis, WaitContract, WaitOwner, WaitTimeoutAction, wait};

mod common;
use common::{count, fresh, schema_of};

#[test]
fn backfills_wait_contracts_waives_closed_pending_ops_and_matches_fresh_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version (v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES(126);
         CREATE TABLE schema_compat (min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES(126);",
    )
    .unwrap();
    for migration in &db::MIGRATIONS[..119] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        "ALTER TABLE bots ADD COLUMN reasoning_effort TEXT;
         ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;",
    )
    .unwrap();
    for migration in &db::MIGRATIONS[119..121] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    for migration in &db::MIGRATIONS[121..126] {
        c.execute_batch(migration).unwrap();
    }
    let t0: i64 = 1_790_000_000_000;
    c.execute_batch(&format!(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',{t0});
         INSERT INTO bots(id,workspace_id,name,harness,updated)
           VALUES('dev','default','Dev','fixture',{t0});
         INSERT INTO items(id,workspace_id,no,title,bot_id,phase,outcome,source,created,updated,closed_at)
           VALUES('ST-1','default',1,'Active item','dev','active',NULL,'[\"对话\",\"chat\"]',{t0},{t0},NULL),
                 ('ST-2','default',2,'Unassigned active',NULL,'active',NULL,'[\"对话\",\"chat\"]',{t0},{t0},NULL),
                 ('ST-3','default',3,'Closed item','dev','closed','completed','[\"对话\",\"chat\"]',{t0},{t0},{t0}+500);
         INSERT INTO sessions(id,workspace_id,item_id,no,bot_id,harness,cwd,status,created,updated)
           VALUES('ses_1','default','ST-1',1,'dev','fixture','.','idle',{t0},{t0});
         INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
           VALUES('ses_1','hub','default','ses_1','1','idle',{t0});
         INSERT INTO runtime_turns(turn_id,session_id,state,created,updated)
           VALUES('turn_1','ses_1','running',{t0},{t0});
         INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated)
           VALUES('att_1','turn_1',0,'hub','op_1','{{}}','{{}}','d','running',{t0},{t0});
         INSERT INTO systems(id,workspace_id,name,kind,family,created)
           VALUES('feishu','default','Feishu','comm','im',{t0}),
                 ('mail','default','Mail','comm','email',{t0});
         INSERT INTO parties(id,workspace_id,type,name,contact_kind,created_by,created,updated)
           VALUES('pt_1','default','contact','Owner','real','usr_1',{t0},{t0});
         INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
           VALUES('acc_1','default','feishu','im','pt_1','personal','u1','u1','connect',{t0},{t0}),
                 ('acc_2','default','feishu','im','pt_1','personal','u2','u2','connect',{t0},{t0}),
                 ('acc_mail','default','mail','email','pt_1','personal','a@b.test','a@b.test','connect',{t0},{t0});
         INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated)
           VALUES('con_null','default','acc_1','feishu','credential','usr_1',{t0},{t0}),
                 ('con_12h','default','acc_2','feishu','credential','usr_1',{t0},{t0}),
                 ('con_mail','default','acc_mail','imap','credential','usr_1',{t0},{t0});
         INSERT INTO channel_settings(connection_id,workspace_id,mode,rules,updated,card_ttl_h)
           VALUES('con_null','default','self','{{}}',{t0},NULL),
                 ('con_12h','default','self','{{}}',{t0},12);
         INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated)
           VALUES('sp_null','default','feishu','con_null','group','g1','G1',{t0},{t0}),
                 ('sp_12h','default','feishu','con_12h','group','g2','G2',{t0},{t0});
         INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,text,created)
           VALUES('cm_null','default','sp_null','out','draft','hi',{t0}),
                 ('cm_12h','default','sp_12h','out','draft','hi',{t0});
         INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,item_id,question,status,requested_by,created)
           VALUES('dec_gen','default','ask','item','ST-1','ST-1','q','open','dev',{t0}),
                 ('dec_mem','default','confirm','mem','m1',NULL,'m','open','steve',{t0}),
                 ('dec_sec','default','approve','secret_use','su1','ST-1','s','open','dev',{t0}),
                 ('dec_ch_null','default','approve','chan_message','cm_null',NULL,'c1','open','steve',{t0}),
                 ('dec_ch_12h','default','approve','chan_message','cm_12h',NULL,'c2','open','steve',{t0}),
                 ('dec_perm','default','approve','tool_call','req_1','ST-1','p','open','dev',{t0});
         INSERT INTO runtime_permissions(decision_id,node_id,session_id,attempt_id,permission_id,request,expires_at,state,created,updated)
           VALUES('dec_perm','hub','ses_1','att_1','req_1','{{}}',{t0}+600000,'waiting',{t0},{t0});
         INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated)
           VALUES('ST-1',1,'ses_1','automatic','wait','waiting',{t0},{t0}),
                 ('ST-2',1,NULL,'workspace','','waiting',{t0},{t0});
         INSERT INTO item_operations(id,item_id,cycle,revision,kind,required,status,label,evidence,created,updated)
           VALUES('op_open','ST-1',1,1,'release',1,'pending','rel','{{}}',{t0},{t0}),
                 ('op_closed','ST-3',1,1,'release',1,'pending','rel','{{}}',{t0},{t0});
         INSERT INTO gh_effects(id,workspace_id,decision_id,kind,state,next_at,created,updated)
           VALUES('ghe_1','default','dec_gen','open','pending',{t0},{t0},{t0});
         INSERT INTO mail_drafts(id,workspace_id,connection_id,mode,to_addrs,subject,body,author,content_hash,status,created,updated)
           VALUES('md_pend','default','con_mail','new','[]','Sub','Body','steve','h1','pending',{t0},{t0}),
                 ('md_unk','default','con_mail','new','[]','Sub2','Body2','steve','h2','unknown',{t0},{t0});
         INSERT INTO flows(id,workspace_id,name,kind,enabled,version,graph,origin,created_by,created,updated)
           VALUES('fl_1','default','F','flow',1,1,'{{}}','me','usr_1',{t0},{t0});
         INSERT INTO flow_runs(id,workspace_id,flow_id,flow_version,trigger_kind,status,cursor,context,started)
           VALUES('fr_1','default','fl_1',1,'manual','waiting','n1','{{}}',{t0});
         INSERT INTO flow_waits(run_id,node_id,workspace_id,mode,until,match,decision_id,created)
           VALUES('fr_1','n1','default','decision',NULL,'{{}}','dec_gen',{t0}),
                 ('fr_1','n2','default','event',{t0}+120000,'{{}}',NULL,{t0});"
    ))
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_write(|c| {
            let compat = db::compat(c)?;
            assert_eq!(
                (compat.schema, compat.min_reader),
                (db::SCHEMA_VERSION, db::SCHEMA_VERSION)
            );
            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();

    // Verify decoded WaitContract across all 6 tables.
    store
        .read(|r| {
            {
                let mut dec = |id: &str| DecisionRows(r).get(id).unwrap().unwrap().wait;
                assert_eq!(
                    dec("dec_gen"),
                    WaitContract::new(
                        WaitOwner::Owner,
                        Millis(t0),
                        Millis(t0 + wait::WAIT_LIMIT_DECISION_MS),
                        WaitTimeoutAction::Close
                    )
                    .unwrap()
                );
                assert_eq!(
                    dec("dec_mem").deadline,
                    Millis(t0 + wait::WAIT_LIMIT_MEMORY_CANDIDATE_MS)
                );
                assert_eq!(
                    dec("dec_sec").deadline,
                    Millis(t0 + wait::WAIT_LIMIT_SECRET_USE_MS)
                );
                assert_eq!(
                    dec("dec_ch_null").deadline,
                    Millis(t0 + wait::WAIT_LIMIT_CHANNEL_DRAFT_MS)
                );
                assert_eq!(
                    dec("dec_ch_12h").deadline,
                    Millis(t0 + 12 * wait::MS_PER_HOUR)
                );
                assert_eq!(dec("dec_perm").deadline, Millis(t0 + 600_000));
            }

            let op_open = operation_repo::get(r, "op_open")?.unwrap();
            assert_eq!(op_open.status, "pending");
            assert_eq!(
                op_open.wait,
                WaitContract::new(
                    WaitOwner::system("release"),
                    Millis(t0),
                    Millis(t0 + wait::WAIT_LIMIT_ITEM_OPERATION_MS),
                    WaitTimeoutAction::EscalateToOwner
                )
                .unwrap()
            );
            let op_closed = operation_repo::get(r, "op_closed")?.unwrap();
            assert_eq!(op_closed.status, "waived");
            assert!(!op_closed.required);

            let gh = github::effect_of(r, "dec_gen")?.unwrap();
            assert_eq!(
                gh.wait,
                WaitContract::new(
                    WaitOwner::system("github"),
                    Millis(t0),
                    Millis(t0 + wait::WAIT_LIMIT_EXTERNAL_EFFECT_MS),
                    WaitTimeoutAction::EscalateToOwner
                )
                .unwrap()
            );

            let md_pend = mail::draft(r, "md_pend")?.unwrap();
            assert_eq!(
                md_pend.wait.unwrap(),
                WaitContract::new(
                    WaitOwner::Owner,
                    Millis(t0),
                    Millis(t0 + wait::WAIT_LIMIT_MAIL_DRAFT_MS),
                    WaitTimeoutAction::Close
                )
                .unwrap()
            );
            let md_unk = mail::draft(r, "md_unk")?.unwrap();
            assert_eq!(
                md_unk.wait.unwrap(),
                WaitContract::new(
                    WaitOwner::system("mail"),
                    Millis(t0),
                    Millis(t0 + wait::WAIT_LIMIT_MAIL_DRAFT_MS),
                    WaitTimeoutAction::EscalateToOwner
                )
                .unwrap()
            );

            let fw1 = FlowWaits(r).get("fr_1", "n1")?.unwrap();
            assert_eq!(
                fw1.wait,
                WaitContract::new(
                    WaitOwner::Owner,
                    Millis(t0),
                    Millis(t0 + wait::WAIT_LIMIT_FLOW_MS),
                    WaitTimeoutAction::Close
                )
                .unwrap()
            );
            let fw2 = FlowWaits(r).get("fr_1", "n2")?.unwrap();
            assert_eq!(fw2.wait.owner, WaitOwner::system("flow"));
            assert_eq!(fw2.wait.deadline, Millis(t0 + 120_000));
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();

    let dep1 = store.dependency_waits().get("ST-1").unwrap().unwrap();
    assert_eq!(dep1.wait.owner, WaitOwner::bot("dev"));
    assert_eq!(
        dep1.wait.deadline,
        Millis(t0 + wait::WAIT_LIMIT_DEPENDENCY_MS)
    );
    assert_eq!(dep1.wait.on_timeout, WaitTimeoutAction::EscalateToOwner);
    let dep2 = store.dependency_waits().get("ST-2").unwrap().unwrap();
    assert_eq!(dep2.wait.owner, WaitOwner::system("dependencies"));

    drop(store);
    assert_eq!(schema_of(&path), schema_of(fresh().store.path()));
}
