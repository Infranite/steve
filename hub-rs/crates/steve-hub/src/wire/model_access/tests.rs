//! The model access setting and the preflight (#222), on a real store with synthetic rows.

use super::*;
use steve_store::{NewBot, NoPublisher, Store};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(d.path().join("ma.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.transaction::<_, StoreError>(|tx| {
        for (id, role) in [("owner", "owner"), ("member", "member")] {
            tx.execute(
                "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES (?,?,?,?,?,1)",
                params![id, WS, id, id, role],
            )?;
            tx.execute(
                "INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES (?,?,?,1)",
                params![WS, id, role],
            )?;
        }
        // a shared item with the key, a personal one, and a high-sensitivity shared one
        for (vault, scope, owner) in [
            ("v_shared", "shared", None),
            ("v_mine", "personal", Some("owner")),
        ] {
            tx.execute(
                "INSERT INTO vaults(id,workspace_id,name,scope,owner_user,created) VALUES (?,?,?,?,?,1)",
                params![vault, WS, vault, scope, owner],
            )?;
        }
        for (item, vault, sensitivity) in [
            ("vi_gw", "v_shared", "normal"),
            ("vi_mine", "v_mine", "normal"),
            ("vi_bank", "v_shared", "high"),
        ] {
            tx.execute(
                "INSERT INTO vault_items(id,workspace_id,vault_id,template,title,sensitivity,status,created_by,created,updated)
                 VALUES (?,?,?,'api_key',?,?,'active','owner',1,1)",
                params![item, WS, vault, item, sensitivity],
            )?;
            tx.execute(
                "INSERT INTO vault_fields(id,workspace_id,item_id,label,type,secret,value_ct,updated)
                 VALUES (?,?,?,'API key','password',1,x'00',1)",
                params![format!("vf_{item}"), WS, item],
            )?;
        }
        Ok(())
    })
    .unwrap();
    for (id, harness) in [
        ("coder", "codex"),
        ("writer", "claude-code"),
        ("plain", "fixture"),
    ] {
        s.bots()
            .seed(
                id,
                false,
                &NewBot {
                    name: id.into(),
                    harness: harness.into(),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }
    (d, s)
}

fn preflight(ready: Option<bool>, principal_state: bool) -> Arc<Preflight> {
    let readiness: Readiness = Arc::new(move || {
        ["codex", "claude-code", "fixture"]
            .into_iter()
            .map(|h| (h.to_string(), h.to_string(), ready))
            .collect()
    });
    Preflight::new(BTreeMap::new(), Some(readiness), principal_state)
}

fn put(s: &Store, harness: &str, item: &str) {
    s.transaction::<_, StoreError>(|tx| {
        let mut v = load(tx)?;
        v.harnesses.insert(
            harness.into(),
            Entry {
                base_url: "https://gateway.example.test/v1".into(),
                item: item.into(),
                field: "API key".into(),
                updated_by: "owner".into(),
                updated: 1,
            },
        );
        save(tx, &v)
    })
    .unwrap();
}

fn code(r: Result<(), Blocked>) -> Option<&'static str> {
    r.err().map(|b| b.code)
}

#[test]
fn a_service_principal_needs_the_hubs_model_access_and_a_person_does_not() {
    let (_d, s) = store();
    let p = preflight(Some(true), true);
    let svc = Principal::Svc(WS.into());
    let me = Principal::User("owner".into());
    s.reader::<_, StoreError>(|c| {
        // nothing configured: the service principal's Codex Bot cannot run (Codex signs in only
        // through its directory), saying where it is fixed; Claude Code, which can be configured
        // but has other ways to sign in, is not stopped; a person runs with their own; a harness
        // Steve cannot point at a provider is not stopped
        let b = p.can_run(c, &svc, "coder").unwrap_err();
        assert_eq!(
            (b.code, b.fix),
            ("model_access_missing", "settings.model_access")
        );
        assert!(b.reason.contains("codex"), "{}", b.reason);
        assert_eq!(code(p.can_run(c, &svc, "writer")), None);
        assert_eq!(code(p.can_run(c, &me, "coder")), None);
        assert_eq!(code(p.can_run(c, &svc, "plain")), None);
        // a schedule without a requester is the service principal's; with its member, theirs
        assert!(p.can_run_schedule(c, WS, None, "coder").is_err());
        assert!(p.can_run_schedule(c, WS, Some("owner"), "coder").is_ok());
        assert!(
            p.can_run_schedule(c, WS, Some("nobody"), "coder").is_err(),
            "not a member: the service principal"
        );
        // the devices / settings list
        let all = p.blocked_all(c)?;
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0]["bot"], "coder");
        assert!(all.iter().all(|r| r["principal"] == "svc:default"));
        Ok(())
    })
    .unwrap();
    // principals without directories of their own use the Node's state: nothing is needed
    let off = preflight(Some(true), false);
    s.reader::<_, StoreError>(|c| {
        assert!(off.can_run(c, &svc, "coder").is_ok());
        Ok(())
    })
    .unwrap();
}

#[test]
fn the_key_must_be_shared_and_findable_and_the_harness_ready() {
    let (_d, s) = store();
    let p = preflight(Some(true), true);
    let svc = Principal::Svc(WS.into());
    put(&s, "codex", "vi_mine");
    put(&s, "claude-code", "vi_bank");
    s.reader::<_, StoreError>(|c| {
        let b = p.can_run(c, &svc, "coder").unwrap_err();
        assert_eq!((b.code, b.fix), ("credential_not_shared", "vault"));
        assert_eq!(
            code(p.can_run(c, &svc, "writer")),
            Some("credential_not_shared")
        );
        Ok(())
    })
    .unwrap();
    put(&s, "codex", "vi_gone");
    s.reader::<_, StoreError>(|c| {
        assert_eq!(
            code(p.can_run(c, &svc, "coder")),
            Some("credential_missing")
        );
        Ok(())
    })
    .unwrap();
    put(&s, "codex", "vi_gw");
    s.reader::<_, StoreError>(|c| {
        assert_eq!(code(p.can_run(c, &svc, "coder")), None);
        Ok(())
    })
    .unwrap();
    // the field is removed from the item: found no more
    s.transaction::<_, StoreError>(|tx| {
        tx.execute("DELETE FROM vault_fields WHERE item_id='vi_gw'", [])?;
        Ok(())
    })
    .unwrap();
    s.reader::<_, StoreError>(|c| {
        assert_eq!(
            code(p.can_run(c, &svc, "coder")),
            Some("credential_missing")
        );
        Ok(())
    })
    .unwrap();
    // a harness the Nodes say is not ready stops everyone, pointing at the devices page; one
    // not reported yet is not stopped
    let down = preflight(Some(false), true);
    let unknown = preflight(None, true);
    s.reader::<_, StoreError>(|c| {
        let me = Principal::User("owner".into());
        let b = down.can_run(c, &me, "coder").unwrap_err();
        assert_eq!((b.code, b.fix), ("harness_not_ready", "devices"));
        assert!(unknown.can_run(c, &me, "coder").is_ok());
        Ok(())
    })
    .unwrap();
}

/// A service session's `open_session` carries the endpoint and a reference only; the owner's
/// session nothing (it starts from the owner's own files). The key is given only to the Node the
/// session is bound to, for that session's harness, while the setting covers it.
#[test]
fn the_open_carries_a_reference_and_the_key_goes_only_to_its_session() {
    let (_d, s) = store();
    put(&s, "codex", "vi_gw");
    s.transaction::<_, StoreError>(|tx| {
        tx.execute(
            "INSERT INTO items(id,workspace_id,no,title,status,stage,requested_by,created,updated)
             VALUES ('i_svc',?1,1,'tidy','run','执行',NULL,1,1),
                    ('i_me',?1,2,'mine','run','执行','owner',1,1)",
            [WS],
        )?;
        for (sid, item) in [("s_svc", "i_svc"), ("s_me", "i_me")] {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated)
                 VALUES (?,?,?,'coder','codex','','idle',1,1)",
                params![sid, WS, item],
            )?;
            tx.execute(
                "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
                 VALUES (?,'local','{}',?,'r','ready',1)",
                params![sid, sid],
            )?;
            principal::record_in(tx, sid, None)?;
        }
        Ok(())
    })
    .unwrap();
    let own = |import_owner| steve_node_api::HarnessState {
        dir: "d".into(),
        import_owner,
        owner_context: true,
    };
    s.reader::<_, StoreError>(|c| {
        let a = for_session_in(c, "s_svc", "codex", Some(&own(false)))?.unwrap();
        assert_eq!(a.base_url, "https://gateway.example.test/v1");
        assert_eq!(a.secret, "codex", "a reference, never the key");
        assert!(a.valid());
        assert!(for_session_in(c, "s_svc", "codex", None)?.is_none());
        assert!(for_session_in(c, "s_me", "codex", Some(&own(true)))?.is_none());
        assert!(
            for_session_in(c, "s_me", "codex", Some(&own(false)))?.is_none(),
            "a person"
        );
        assert!(for_session_in(c, "s_svc", "claude-code", Some(&own(false)))?.is_none());
        // the key: only for the bound Node, the session's own harness, a service session
        assert_eq!(
            entry_for(c, "local", "s_svc", "codex")?.unwrap().item,
            "vi_gw"
        );
        assert!(entry_for(c, "elsewhere", "s_svc", "codex")?.is_none());
        assert!(entry_for(c, "local", "s_svc", "claude-code")?.is_none());
        assert!(entry_for(c, "local", "s_me", "codex")?.is_none());
        Ok(())
    })
    .unwrap();
    // the setting no longer covers it: the next open carries nothing and the key is not given
    s.transaction::<_, StoreError>(|tx| save(tx, &Setting::default()))
        .unwrap();
    s.reader::<_, StoreError>(|c| {
        assert!(for_session_in(c, "s_svc", "codex", Some(&own(false)))?.is_none());
        assert!(entry_for(c, "local", "s_svc", "codex")?.is_none());
        Ok(())
    })
    .unwrap();
}

#[test]
fn only_the_owner_edits_and_only_a_usable_entry_is_kept() {
    let (_d, s) = store();
    let api = Api {
        store: s.clone(),
        preflight: preflight(Some(true), true),
    };
    let ch = |url: &str, item: &str| Change {
        base_url: url.into(),
        item: item.into(),
        field: "API key".into(),
    };
    let good = ch("https://gateway.example.test/v1", "vi_gw");
    assert!(matches!(
        write(&api, "member", "codex", Some(&good)),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(
        write(&api, "owner", "codex", Some(&ch("ftp://x", "vi_gw"))),
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        write(
            &api,
            "owner",
            "codex",
            Some(&ch("https://x.example.test", "vi_mine"))
        ),
        Err(ApiError::Rule(_))
    ));
    assert!(matches!(
        write(&api, "owner", "fixture", Some(&good)),
        Err(ApiError::Rule(_))
    ));
    let v = write(&api, "owner", "codex", Some(&good)).unwrap();
    let codex = v["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "codex")
        .unwrap()
        .clone();
    assert_eq!(codex["problem"], Value::Null);
    assert_eq!(codex["item"], "vi_gw");
    // the shared item is offered, the personal and the high-sensitivity ones are not
    let items: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(items, ["vi_gw"]);
    // everything can run now; Claude Code's is optional, so not reported missing
    assert!(v["blocked"].as_array().unwrap().is_empty());
    let claude = v["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "claude-code")
        .unwrap()
        .clone();
    assert_eq!(
        (claude["required"].clone(), claude["problem"].clone()),
        (json!(false), Value::Null)
    );
    let v = write(&api, "owner", "codex", None).unwrap();
    assert_eq!(v["blocked"].as_array().unwrap().len(), 1);
}

/// #234: the Hub's own Node says Claude Code is not ready, and a second Node linked but has not
/// answered yet. The second Node may well have it, so the Bot is not refused; once every linked
/// Node has answered and none has it ready, it is.
#[test]
fn a_node_that_linked_but_has_not_answered_is_not_a_refusal() {
    let (_d, s) = store();
    let cfg = crate::config::Config::from_vars(&|_: &str| None).unwrap();
    let hs = crate::harnesses::HubHarnesses::of(&cfg);
    hs.update(vec![steve_node_api::HarnessCapability {
        name: "claude-code".into(),
        version: String::new(),
        sessions: false,
        jobs: false,
        models: vec![],
        resume: false,
        cancel: false,
        permissions: false,
        images: false,
        enforcement: vec![],
        setup: None,
        adapter: None,
    }]);
    let linked = Arc::new(std::sync::Mutex::new(vec!["n_second".to_string()]));
    let readiness: Readiness = {
        let (hs, linked) = (hs.clone(), linked.clone());
        Arc::new(move || hs.readiness(&linked.lock().unwrap()))
    };
    let p = Preflight::new(BTreeMap::new(), Some(readiness), true);
    let me = Principal::User("owner".into());
    s.reader::<_, StoreError>(|c| {
        assert_eq!(code(p.can_run(c, &me, "writer")), None);
        // the second Node went away without answering: only the Hub's own Node is left
        linked.lock().unwrap().clear();
        assert_eq!(code(p.can_run(c, &me, "writer")), Some("harness_not_ready"));
        Ok(())
    })
    .unwrap();
}
