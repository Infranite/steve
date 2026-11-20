mod common;
use common::*;
use rusqlite::params;
use serde_json::json;
use steve_store::{Result, WS, attention, feed};

fn seed(t: &T) {
    t.store.with_conn(|c| {
        c.execute_batch("INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u','default','u','User','owner',1),('other','default','other','Other','member',1);
          INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u','owner',1),('default','other','member',1);")?;
        Ok(())
    }).unwrap();
}

#[test]
fn pending_cards_exact_parity_with_web_selector_fixture() {
    let t = fresh();
    seed(&t);
    // Exact selector fixture: status alone never creates a card; wait makes ANY status eligible;
    // all waits per item (not one/item); orphan waits; effective snooze label; expired snooze;
    // archive; and ONLY confirm/mem excluded (ask/mem remains actionable).
    t.store.with_conn(|c| {
        for (n,id,status,arch,snooze) in [
            (1,"wait","wait",false,None), (2,"prop","prop",false,None),
            (3,"run","run",false,None), (4,"done","done",false,None),
            (5,"no_wait","wait",false,None), (6,"arch","wait",true,None),
            (7,"snooze","wait",false,Some(json!({"label":"Later","until":101}))),
            (8,"expired","wait",false,Some(json!({"label":"Later","until":100}))),
            (9,"empty_label","wait",false,Some(json!({"label":"","until":101}))),
        ] {
            c.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,archived,snooze,created,updated) VALUES(?,'default',?,? ,?,'',?,?,1,1)",params![id,n,id,status,arch,snooze.map(|s|s.to_string())])?;
        }
        for (id,item,kind,subject,status) in [
            ("wait_a",Some("wait"),"ask","item","open"),
            ("wait_b",Some("wait"),"approve","item","open"),
            ("prop",Some("prop"),"accept","item","open"),
            ("run",Some("run"),"ask","item","open"),
            ("done",Some("done"),"fail","item","open"),
            ("arch",Some("arch"),"ask","item","open"),
            ("snooze",Some("snooze"),"ask","item","open"),
            ("expired",Some("expired"),"ask","item","open"),
            ("empty_label",Some("empty_label"),"ask","item","open"),
            ("orphan",None,"ask","item","open"),
            ("missing_item",Some("gone"),"ask","item","open"),
            ("mem",None,"confirm","mem","open"),
            ("mem_on_item",Some("wait"),"confirm","mem","open"),
            ("ask_mem",None,"ask","mem","open"),
            ("settled",None,"ask","item","resolved"),
        ] {
            c.execute("INSERT INTO decisions(id,workspace_id,item_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES(?,'default',?,?,?,'subject','Question',?,'steve',1)",params![id,item,kind,subject,status])?;
        }
        Ok(())
    }).unwrap();
    let got = t
        .store
        .reader(|c| attention::pending(c, WS, "u", 100))
        .unwrap();
    let mut ids = got.into_iter().map(|e| e.id).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        [
            "ask_mem",
            "done",
            "empty_label",
            "expired",
            "missing_item",
            "orphan",
            "prop",
            "run",
            "wait_a",
            "wait_b"
        ]
    );
    assert!(
        t.store
            .reader(|c| attention::pending(c, WS, "stranger", 100))
            .is_err()
    );
}

#[test]
fn feed_projection_reuses_personal_audience_reads_and_snapshot_pages() {
    let t = fresh();
    seed(&t);
    t.store.transaction(|tx| -> Result<()> {
        for n in 0..205 {
            feed::publish_report(tx,WS,"flow",&format!("run{n}"),"node",&feed::Content{title:"Report".into(),..Default::default()},&json!({}),10)?;
        }
        tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created) VALUES('private','default','u','event','key','log','Private body','held',10)",[])?;
        Ok(())
    }).unwrap();
    let first = t
        .store
        .transaction(|tx| attention::feed_page(tx, WS, "u", None, None, 100, 100))
        .unwrap();
    assert_eq!(first.count, 206);
    assert!(first.snapshot.is_none());
    assert_eq!(first.entries[0].group_key.as_deref(), Some("event"));
    let partial = first.next_cursor.as_ref().unwrap().clone();
    assert!(
        t.store
            .transaction(|tx| attention::feed_page(tx, WS, "u", None, Some(&partial), 100, 100))
            .is_err()
    );
    let mut cursor = first.next_cursor;
    let mut seen = first.entries;
    let snapshot;
    loop {
        let p = t
            .store
            .transaction(|tx| attention::feed_page(tx, WS, "u", cursor.as_deref(), None, 100, 100))
            .unwrap();
        assert_eq!(p.count, 206);
        seen.extend(p.entries);
        cursor = p.next_cursor;
        if cursor.is_none() {
            snapshot = p.snapshot.unwrap();
            break;
        }
        assert!(p.snapshot.is_none());
    }
    assert_eq!(seen.len(), 206);
    assert!(seen.windows(2).all(|w| w[0].order > w[1].order));
    assert_eq!(
        seen.iter()
            .map(|e| &e.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        206
    );
    let u = t
        .store
        .transaction(|tx| attention::feed_page(tx, WS, "u", None, Some(&snapshot), 100, 100))
        .unwrap();
    assert!(u.unchanged);
    assert!(u.entries.is_empty());
    assert_eq!(
        t.store
            .transaction(|tx| attention::feed_page(tx, WS, "other", None, None, 100, 100))
            .unwrap()
            .count,
        205
    );
    for (user, token, time) in [
        ("other", snapshot.as_str(), 101),
        ("u", "forged", 101),
        ("u", snapshot.as_str(), 86_400_101),
    ] {
        assert!(
            t.store
                .transaction(|tx| attention::feed_page(tx, WS, user, None, Some(token), 100, time))
                .is_err()
        );
    }
    assert!(
        t.store
            .transaction(|tx| attention::page(
                tx,
                WS,
                "u",
                "pending",
                vec![],
                0,
                None,
                Some(&snapshot),
                100,
                101
            ))
            .is_err()
    );
    t.store
        .transaction(|tx| feed::read_ids(tx, WS, "u", &["feed_private".into()], 200))
        .unwrap();
    // Read changes invalidate an in-flight keyset traversal, never silently skip membership.
    assert!(
        t.store
            .transaction(|tx| attention::feed_page(tx, WS, "u", Some(&partial), None, 100, 201))
            .is_err()
    );
    let p = t
        .store
        .transaction(|tx| attention::feed_page(tx, WS, "u", None, Some(&snapshot), 100, 201))
        .unwrap();
    assert!(!p.unchanged);
    assert_eq!(p.count, 205);
    assert!(p.snapshot.is_none());
    // Existing Feed reads are authoritative; update/delete/membership transitions maintain index.
    t.store
        .with_conn(|c| {
            c.execute("DELETE FROM feed_reads WHERE entry_id='feed_private'", [])?;
            c.execute(
                "UPDATE feed_entries SET source_id='changed' WHERE id='feed_private'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let p = t
        .store
        .transaction(|tx| attention::feed_page(tx, WS, "u", None, None, 100, 202))
        .unwrap();
    assert_eq!(p.count, 206);
    assert_eq!(p.entries[0].source_id, "changed");
    t.store
        .with_conn(|c| {
            c.execute("DELETE FROM feed_entries WHERE id='feed_private'", [])?;
            c.execute("DELETE FROM workspace_members WHERE user_id='u'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        t.store
            .transaction(|tx| attention::feed_page(tx, WS, "u", None, None, 100, 202))
            .is_err()
    );
    t.store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO workspace_members VALUES('default','u','owner',1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        t.store
            .transaction(|tx| attention::feed_page(tx, WS, "u", None, None, 100, 202))
            .unwrap()
            .count,
        205
    );
}

#[test]
fn feed_pages_and_unchanged_are_bounded_after_large_read_history() {
    let t = fresh();
    seed(&t);
    t.store.with_conn(|c| {
        c.execute_batch("BEGIN;
          WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000)
          INSERT INTO feed_entries(id,workspace_id,audience,kind,source_type,source_key,source_id,run_id,node_id,content,result,published_at)
          SELECT 'history_'||x,'default','workspace','report','flow',json_array('run_'||x,'node'),'flow','run_'||x,'node','{}','{}',1 FROM n;
          INSERT INTO feed_reads SELECT 'default','u',id,2 FROM feed_entries WHERE seq<=19749;
          COMMIT;")?;
        Ok(())
    }).unwrap();
    let started = std::time::Instant::now();
    let mut cursor = None;
    let mut total = 0;
    let snapshot;
    loop {
        let p = t
            .store
            .transaction(|tx| attention::feed_page(tx, WS, "u", cursor.as_deref(), None, 100, 100))
            .unwrap();
        assert_eq!(p.count, 251);
        assert!(p.entries.len() <= 100);
        total += p.entries.len();
        cursor = p.next_cursor;
        if cursor.is_none() {
            snapshot = p.snapshot.unwrap();
            break;
        }
    }
    assert_eq!(total, 251);
    let baseline_elapsed = started.elapsed();
    let started = std::time::Instant::now();
    for _ in 0..100 {
        let p = t
            .store
            .transaction(|tx| attention::feed_page(tx, WS, "u", None, Some(&snapshot), 100, 100))
            .unwrap();
        assert!(p.unchanged);
        assert!(p.entries.is_empty());
        assert_eq!(p.count, 251);
    }
    eprintln!(
        "Feed scaling: 20,000 historical rows / 19,749 reads / 251 unread; 3 pages {:?}; 100 unchanged polls {:?}",
        baseline_elapsed,
        started.elapsed()
    );
    let started = std::time::Instant::now();
    let mut cursor = None;
    let mut total = 0;
    let mut pages = 0;
    loop {
        let p = t
            .store
            .transaction(|tx| {
                attention::feed_page(tx, WS, "other", cursor.as_deref(), None, 100, 100)
            })
            .unwrap();
        assert_eq!(p.count, 20000);
        assert_eq!(p.entries.len(), 100);
        total += p.entries.len();
        pages += 1;
        cursor = p.next_cursor;
        if cursor.is_none() {
            assert!(p.snapshot.is_some());
            break;
        }
        assert!(p.snapshot.is_none());
    }
    assert_eq!((total, pages), (20000, 200));
    eprintln!(
        "Feed scaling: 20,000 unread complete 200-page traversal {:?}",
        started.elapsed()
    );
    t.store.reader(|c| -> Result<()> {
        let sql="SELECT e.id FROM attention_feed_unread u JOIN feed_entries e ON e.id=u.entry_id WHERE u.workspace_id=?1 AND u.user_id=?2 AND u.seq<?3 ORDER BY u.seq DESC LIMIT ?4";
        let mut st=c.prepare(sql)?;
        let ids=st.query_map(params![WS,"u",20001,100],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(ids.len(),100);
        assert_eq!(st.get_status(rusqlite::StatementStatus::FullscanStep),0);
        assert_eq!(st.get_status(rusqlite::StatementStatus::Sort),0);
        assert!(st.get_status(rusqlite::StatementStatus::VmStep)<3000);
        let mut st=c.prepare("SELECT revision,unread_count FROM attention_feed_scopes WHERE workspace_id=? AND user_id=?")?;
        st.query_row(params![WS,"u"],|r|r.get::<_,String>(0))?;
        assert_eq!(st.get_status(rusqlite::StatementStatus::FullscanStep),0);
        assert!(st.get_status(rusqlite::StatementStatus::VmStep)<30);
        let plan:String=c.query_row("EXPLAIN QUERY PLAN DELETE FROM attention_windows WHERE expires<=100",[],|r|r.get(3))?;
        assert!(plan.contains("attention_windows_expiry"),"{plan}");
        Ok(())
    }).unwrap();
}

#[test]
fn final_reply_commits_follow_completion_instead_of_timestamp_order() {
    let t = fresh();
    seed(&t);
    t.store
        .transaction(|tx| -> Result<()> {
            for n in 0..2 {
                let (entry, _) = feed::publish_report(
                    tx,
                    WS,
                    "flow",
                    &format!("old_run{n}"),
                    "node",
                    &feed::Content::default(),
                    &json!({}),
                    10,
                )?;
                if n == 0 {
                    feed::read_ids(
                        tx,
                        WS,
                        "u",
                        &[entry["feed_entry_id"].as_str().unwrap().into()],
                        11,
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();
    t.store.with_conn(|c| {
    assert_eq!(count(c,"SELECT unread_count FROM attention_feed_scopes WHERE user_id='u'"),1);
    assert_eq!(count(c,"SELECT unread_count FROM attention_feed_scopes WHERE user_id='other'"),2);
    c.execute_batch("INSERT INTO messages(id,workspace_id,space,author,text,created) VALUES('origin','default','steve','me','Question',1);
       INSERT INTO thread_runs(id,workspace_id,message_id,user_id,status,created) VALUES('new','default','origin','u','running',1);").unwrap();
    c.execute_batch("INSERT INTO messages(id,workspace_id,space,author,text,meta,created) VALUES('new_reply','default','steve','steve','Final','{\"run\":\"new\",\"origin\":\"origin\",\"run_receipt\":true}',1);
      UPDATE thread_runs SET status='done',reply_id='new_reply' WHERE id='new';
      UPDATE thread_runs SET ended=2 WHERE id='new';").unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM thread_reply_commits"),
        1,
        "metadata is idempotent"
    );
    assert_eq!(
        count(
            &c,
            "SELECT seq FROM thread_reply_commits WHERE message_id='new_reply'"
        ),
        1
    );
    Ok(())
    }).unwrap();
}
