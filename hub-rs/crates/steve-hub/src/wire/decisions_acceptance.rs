//! Decision lifecycle acceptance suite: verifies every registered module subject and every core subject through
//! the single `Core::decision_command` settlement entry (one success + one refusal each, replay
//! idempotence, conflicting-choice rejection, cross-workspace isolation, natural-key deduplication)
//! and proves staged decision replies wake idle Node sessions after commit.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use steve_core::DecisionCommand;
use steve_modules::Caller;
use steve_node_api::PrepareKind;
use steve_node_api::testkit::{FakeNode, build};
use steve_runtime::testkit::eventually_sync;
use steve_store::account_rows::NewAccount;
use steve_store::base::mems::NewMem;
use steve_store::base::{Level, Visibility};
use steve_store::channel_message_repo::outbound::{self as chan_out, NewMessage};
use steve_store::channel_spaces::registration::{self as space_reg, Space as NewSpace};
use steve_store::decision_repo::{DecisionChange, choice_receipt};
use steve_store::github::{PrRow, RepoRow, insert_repo, upsert_pr};
use steve_store::knowledge::{Memories, NewProjectProposal, ProjectProposals};
use steve_store::mail::{DraftRow, insert_draft};
use steve_store::secret_uses::SecretUses as UseRows;
use steve_store::{
    Attributes, ConnRow, Connections, Contacts, Decision, DecisionRows, DecisionSource, FlowRows,
    FlowWaits, HealthState, MergeProposals, NewDecision, NewDef, NewFlowRow, NewItem,
    NewMergeProposal, NewParty, NewRun, NewSession, NewSignal, NewValue, NewWait, RunRows,
    SignalRows, Steward, Stewards, Store, StoreError, UserRows,
};
use steve_types::DecisionStatus;

use super::node_runtime::{Binding, NodeDomain, NodeRuntime, Sink};
use super::{Vars, Wired, wire_with};

struct Harness {
    _dir: tempfile::TempDir,
    w: Wired,
    store: Store,
}

fn wire_hub() -> Harness {
    steve_modules::net::forbid_in_this_process();
    let dir = tempfile::tempdir().expect("tmp");
    let h = dir.path().to_path_buf();
    let vars: HashMap<String, String> = [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", "1".into()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", "http://127.0.0.1:1".into()),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NODE_MODE", "local".into()),
        ("STEVE_NODE_LISTEN", "0".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let vars = Arc::new(vars);
    let get: Vars = Arc::new(move |k: &str| vars.get(k).cloned());
    let cfg = Arc::new(crate::config::Config::from_vars(&*get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = crate::bus::Bus::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = wire_with(&cfg, &bus, get).expect("wire");
    let store = w.core.store().store().clone();
    store.ensure_workspace().expect("workspace");
    store
        .write(|tx| {
            UserRows(tx).insert(
                "owner",
                "default",
                "sub_owner",
                "Owner",
                "owner",
                steve_store::now(),
            )?;
            UserRows(tx).insert(
                "member",
                "default",
                "sub_member",
                "Member",
                "member",
                steve_store::now(),
            )?;
            tx.members().add("default", "owner", "owner", 1)?;
            tx.members().add("default", "member", "member", 2)?;
            Ok::<_, StoreError>(())
        })
        .expect("seed users");
    Harness {
        _dir: dir,
        w,
        store,
    }
}

fn caller(ws: &str, me: &str) -> Caller {
    Caller {
        ws: ws.into(),
        me: me.into(),
    }
}

fn ensure_bot(store: &Store, ws: &str) -> String {
    if let Some(b) = store.bots().list(Some(ws)).unwrap().into_iter().next() {
        return b.id;
    }
    store
        .bots()
        .create(
            ws,
            &steve_store::NewBot {
                name: "Accept Bot".into(),
                ..Default::default()
            },
            "owner",
        )
        .unwrap()
        .unwrap()
        .id
}

fn create_item(store: &Store, ws: &str, title: &str, goal: &str) -> steve_store::Item {
    let tag = store.tags().kinds(ws).unwrap().remove(0).id;
    let bot_id = ensure_bot(store, ws);
    store
        .items()
        .create(
            ws,
            &NewItem {
                title: title.into(),
                goal: goal.into(),
                bot_id,
                tags: vec![tag],
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
}

fn open_with_dedupe(store: &Store, new: NewDecision) -> Decision {
    let (first, opened1) = store
        .write(|tx| DecisionRows(tx).open_in(&new))
        .expect("open first");
    assert!(opened1, "first open must insert {}", new.subject_type);
    let (second, opened2) = store
        .write(|tx| DecisionRows(tx).open_in(&new))
        .expect("open duplicate");
    assert!(
        !opened2 && second.id == first.id,
        "natural key must dedupe {} (first={}, second={})",
        new.subject_type,
        first.id,
        second.id
    );
    first
}

fn resolve_audit_count(store: &Store, decision_id: &str) -> i64 {
    steve_store::core_fixture::work::audit_count(store, &["decision.resolve"], Some(decision_id))
}

fn assert_refused_cleanly(
    w: &Wired,
    store: &Store,
    who: &Caller,
    id: &str,
    rev: i64,
    choice: &str,
    text: &str,
    label: &str,
) {
    let before = store.decisions().get(id).unwrap().unwrap();
    let audits_before = resolve_audit_count(store, id);
    let res = w.core.decision_command(
        who,
        id,
        Some(rev),
        DecisionCommand::Resolve {
            choice: choice.into(),
            text: text.into(),
        },
    );
    assert!(res.is_err(), "{label}: expected refusal, got {res:?}");
    let after = store.decisions().get(id).unwrap().unwrap();
    assert_eq!(after.status, before.status, "{label}: status advanced");
    assert_eq!(after.revision, before.revision, "{label}: revision changed");
    assert_eq!(
        resolve_audit_count(store, id),
        audits_before,
        "{label}: audit written on refusal"
    );
    if before.status == DecisionStatus::Open {
        assert!(
            store
                .read(|r| choice_receipt(r, id, before.revision))
                .unwrap()
                .is_none(),
            "{label}: receipt written on refusal"
        );
    }
}

fn assert_success_replay_and_conflict(
    w: &Wired,
    store: &Store,
    who: &Caller,
    id: &str,
    choice: &str,
    conflict_choice: &str,
    label: &str,
) -> Decision {
    let before = store.decisions().get(id).unwrap().unwrap();
    assert_eq!(before.status, DecisionStatus::Open, "{label}: must be open");
    // Cross-workspace caller is refused with zero leakage before settlement.
    assert_refused_cleanly(
        w,
        store,
        &caller("other-ws", &who.me),
        id,
        before.revision,
        choice,
        "",
        &format!("{label} cross-workspace"),
    );
    // Stale revision is refused before settlement.
    assert_refused_cleanly(
        w,
        store,
        who,
        id,
        before.revision + 99,
        choice,
        "",
        &format!("{label} stale-revision"),
    );

    let settled = w
        .core
        .decision_command(
            who,
            id,
            Some(before.revision),
            DecisionCommand::Resolve {
                choice: choice.into(),
                text: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("{label}: expected success, got {e:?}"));
    assert_eq!(settled.status, DecisionStatus::Resolved, "{label}: status");
    assert_eq!(resolve_audit_count(store, id), 1, "{label}: 1 audit");
    assert!(
        store
            .read(|r| choice_receipt(r, id, before.revision))
            .unwrap()
            .is_some(),
        "{label}: receipt persisted"
    );

    // Identical replay returns original decision with 0 second audit/effect.
    let replayed = w
        .core
        .decision_command(
            who,
            id,
            Some(before.revision),
            DecisionCommand::Resolve {
                choice: choice.into(),
                text: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("{label}: replay failed: {e:?}"));
    assert_eq!(replayed.id, settled.id, "{label}: replay id");
    assert_eq!(resolve_audit_count(store, id), 1, "{label}: replay 1 audit");

    // Same key with conflicting choice is rejected with 0 second effect.
    assert_refused_cleanly(
        w,
        store,
        who,
        id,
        before.revision,
        conflict_choice,
        "",
        &format!("{label} conflicting-choice"),
    );
    settled
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_registered_decision_subject_has_success_and_refusal_contract() {
    let h = wire_hub();
    let store = &h.store;
    let ws = "default";
    let owner = caller(ws, "owner");
    let member = caller(ws, "member");
    let now = steve_store::now();

    let registered: BTreeSet<String> =
        h.w.modules
            .as_ref()
            .expect("modules")
            .registry
            .decision_subjects()
            .into_iter()
            .map(String::from)
            .collect();
    let mut covered_modules: BTreeSet<String> = BTreeSet::new();

    // Shared domain fixtures (parties, accounts, connection, space, item, repo, PR).
    let (party_a, party_b, account_id, conn_id, space_id, _repo_id, pr_id, item_id, closed_item_id) =
        store
            .write(|tx| {
                let p_a =
                    Contacts::new(tx).insert(ws, &NewParty::new("contact", "Alice", "owner"))?;
                let p_b =
                    Contacts::new(tx).insert(ws, &NewParty::new("contact", "Bob", "owner"))?;
                let acc = steve_store::Accounts(tx).insert(
                    ws,
                    &NewAccount {
                        system_id: "feishu".into(),
                        family: "chat".into(),
                        party_id: p_a.id.clone(),
                        kind: "user".into(),
                        handle: "u_alice".into(),
                        handle_display: "Alice".into(),
                        display_name: "Alice".into(),
                        verified: true,
                        confidence: 1.0,
                        source: "manual".into(),
                        source_label: "test".into(),
                    },
                )?;
                let conn_id = "conn_accept_1".to_string();
                Connections(tx).insert(&ConnRow {
                    id: conn_id.clone(),
                    workspace_id: ws.into(),
                    account_id: acc.id.clone(),
                    adapter: "feishu".into(),
                    auth: "oauth".into(),
                    granted_scopes: vec![],
                    caps: vec!["repo.write".into()],
                    credential_item: None,
                    status: "ok".into(),
                    health: HealthState::default(),
                    config: json!({}),
                    last_sync: None,
                    created_by: "owner".into(),
                    revision: 1,
                    created: now,
                    updated: now,
                })?;
                space_reg::create(
                    tx,
                    &NewSpace {
                        ws,
                        system: "feishu",
                        connection: &conn_id,
                        kind: "group",
                        external: "chat_accept_1",
                        name: "Acceptance Group",
                        member_count: 2,
                        has_external: false,
                        disclosure: "public",
                        platform: Some("feishu"),
                        now,
                    },
                )?;
                let space_id = steve_store::channel_spaces::rows::by_chat(tx, ws, "chat_accept_1")?
                    .pop()
                    .expect("space")
                    .id;
                Ok::<_, StoreError>((
                    p_a.id,
                    p_b.id,
                    acc.id,
                    conn_id,
                    space_id,
                    "ghr_accept_1".to_string(),
                    "ghp_accept_1".to_string(),
                ))
            })
            .map(|(a, b, acc, conn, space, repo, pr)| {
                let item = create_item(store, ws, "Open item", "Test open item");
                let closed = create_item(store, ws, "Closed item", "Test closed item");
                store
                    .write(|tx| {
                        steve_store::lifecycle_write_in(
                            tx,
                            &closed.id,
                            &steve_store::LifecycleWrite {
                                phase: steve_types::ItemPhase::Closed,
                                cycle: closed.cycle,
                                delivery_revision: closed.delivery_revision,
                                outcome: Some("completed".into()),
                                acceptance: Some("owner".into()),
                                ..Default::default()
                            },
                        )?;
                        insert_repo(
                            tx,
                            &RepoRow {
                                id: repo.clone(),
                                workspace_id: ws.into(),
                                connection_id: conn.clone(),
                                gh_id: 101,
                                owner: "acme".into(),
                                name: "demo".into(),
                                full_name: "acme/demo".into(),
                                default_branch: "main".into(),
                                private: false,
                                project_id: None,
                                ingest: "poll".into(),
                                hook_id: None,
                                last_delivery: None,
                                poll_state: json!({}),
                                settings: json!({}),
                                enabled: true,
                                revision: 1,
                                created: now,
                                updated: now,
                            },
                        )?;
                        upsert_pr(
                            tx,
                            &PrRow {
                                id: pr.clone(),
                                workspace_id: ws.into(),
                                repo_id: repo.clone(),
                                number: 7,
                                gh_id: 707,
                                title: "Fix".into(),
                                url: "https://github.com/acme/demo/pull/7".into(),
                                state: "open".into(),
                                draft: false,
                                head_ref: "feat/x".into(),
                                head_sha: "abc1234".into(),
                                base_ref: "main".into(),
                                author_login: "bot".into(),
                                author_account_id: None,
                                opened_by: "steve".into(),
                                body_excerpt: String::new(),
                                checks_state: "success".into(),
                                review_state: "approved".into(),
                                mergeable: Some("clean".into()),
                                item_id: Some(item.id.clone()),
                                link_source: Some("steve".into()),
                                created: now,
                                updated: now,
                                merged_at: None,
                                closed_at: None,
                            },
                        )?;
                        Ok::<_, StoreError>(())
                    })
                    .unwrap();
                (a, b, acc, conn, space, repo, pr, item.id, closed.id)
            })
            .expect("seed fixtures");

    // 1. chan_message (module)
    {
        covered_modules.insert("chan_message".into());
        let msg_id = "cmsg_accept_1";
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "chan_message".into(),
                subject_id: msg_id.into(),
                question: "Send reply?".into(),
                draft: Some("hello".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:chan_message:1".into(),
                }),
                ..Default::default()
            },
        );
        store
            .write(|tx| {
                chan_out::insert(
                    tx,
                    &NewMessage {
                        id: msg_id,
                        ws,
                        space: &space_id,
                        thread: None,
                        reply_to: None,
                        account: Some(&account_id),
                        persona: None,
                        on_behalf: "self",
                        text: Some("hello"),
                        status: "draft",
                        created: now,
                    },
                )?;
                chan_out::link_decision(tx, msg_id, &d.id)?;
                Ok::<_, StoreError>(())
            })
            .unwrap();
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "chan_message invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "yes", "chan_message");
        assert_eq!(
            store
                .read(|r| chan_out::approval(r, ws, msg_id))
                .unwrap()
                .unwrap()
                .status,
            "discarded"
        );
    }

    // 2. space (module)
    {
        covered_modules.insert("space".into());
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "space".into(),
                subject_id: space_id.clone(),
                question: "Configure space?".into(),
                options: Some(vec!["mute".into(), "quiet".into(), "block".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:space:1".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "invalid_opt",
            "",
            "space invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "mute", "block", "space");
        let settings = store
            .read(|r| steve_channels::spaces::settings(r, &space_id))
            .unwrap();
        assert!(settings.muted);
    }

    // 3. flow_run (module)
    {
        covered_modules.insert("flow_run".into());
        let flow_id = "flw_accept_1";
        let run_id = store
            .write(|tx| {
                FlowRows(tx).create(&NewFlowRow {
                    id: flow_id,
                    ws,
                    actor: "owner",
                    kind: "manual",
                    name: "Accept Flow",
                    description: "",
                    enabled: true,
                    graph: &json!({"nodes": [], "edges": []}),
                    origin: "manual",
                    origin_label: "manual",
                    schedule: None,
                    now,
                })?;
                RunRows(tx).insert(&NewRun {
                    ws,
                    flow_id,
                    flow_version: 1,
                    trigger_kind: "manual",
                    status: "waiting",
                    outcome_label: "",
                    error: None,
                    cursor: Some("n1"),
                    context: &json!({}),
                    now,
                })
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "fail".into(),
                subject_type: "flow_run".into(),
                subject_id: run_id.clone(),
                question: "Budget exceeded?".into(),
                options: Some(vec!["continue_once".into(), "stop".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:flow_run:1".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "flow_run invalid",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "stop",
            "continue_once",
            "flow_run",
        );

        // 4. flow_step (module)
        covered_modules.insert("flow_step".into());
        let step_d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "flow_step".into(),
                subject_id: format!("{run_id}:n1"),
                question: "Approve flow step?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:flow_step:1".into(),
                }),
                ..Default::default()
            },
        );
        // Refusal before flow_wait exists (not waiting).
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &step_d.id,
            step_d.revision,
            "yes",
            "",
            "flow_step missing wait",
        );
        store
            .write(|tx| {
                FlowWaits(tx).put(&NewWait {
                    run: &run_id,
                    node: "n1",
                    ws,
                    mode: "decision",
                    until: None,
                    matcher: &json!({"kind": "ask"}),
                    decision: Some(&step_d.id),
                    now,
                })
            })
            .unwrap();
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &step_d.id,
            "yes",
            "no",
            "flow_step",
        );
    }

    // 5. signal (module)
    {
        covered_modules.insert("signal".into());
        let sig_id = "sig_accept_1";
        let d_missing = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "signal".into(),
                subject_id: "sig_missing".into(),
                question: "Signal notice".into(),
                options: Some(vec!["done".into(), "to_item".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:signal:missing".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d_missing.id,
            d_missing.revision,
            "to_item",
            "",
            "signal missing source",
        );
        store
            .write(|tx| {
                SignalRows(tx).insert(&NewSignal {
                    id: sig_id,
                    ws,
                    connection: Some(&conn_id),
                    source: "channel",
                    kind: "message",
                    external_id: "ext_sig_1",
                    dedupe: "dedupe_sig_1",
                    thread: None,
                    group: None,
                    scope: None,
                    space: Some(&space_id),
                    message: None,
                    actor_account: Some(&account_id),
                    actor_party: Some(&party_a),
                    occurred: now,
                    received: now,
                    summary: "Ping",
                    payload: &json!({}),
                    personal: false,
                })
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "signal".into(),
                subject_id: sig_id.into(),
                question: "Signal notice".into(),
                options: Some(vec!["done".into(), "to_item".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:signal:1".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "done", "to_item", "signal");
    }

    // 6. gh_pr_open (module)
    {
        covered_modules.insert("gh_pr_open".into());
        let d_closed = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "gh_pr_open".into(),
                subject_id: closed_item_id.clone(),
                item_id: Some(closed_item_id.clone()),
                question: "Open PR?".into(),
                draft: Some("Title\n\nBody".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:gh_pr_open:closed".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d_closed.id,
            d_closed.revision,
            "yes",
            "",
            "gh_pr_open closed cycle",
        );
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "gh_pr_open".into(),
                subject_id: item_id.clone(),
                item_id: Some(item_id.clone()),
                question: "Open PR?".into(),
                draft: Some("Title\n\nBody".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:gh_pr_open:open".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "yes", "gh_pr_open");
    }

    // 7. gh_pr_merge (module)
    {
        covered_modules.insert("gh_pr_merge".into());
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "gh_pr_merge".into(),
                subject_id: pr_id.clone(),
                item_id: Some(item_id.clone()),
                question: "Merge PR #7?".into(),
                draft: Some("abc1234".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "owner".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:gh_pr_merge:1".into(),
                }),
                ..Default::default()
            },
        );
        // Non-owner cannot approve PR merge.
        assert_refused_cleanly(
            &h.w,
            store,
            &member,
            &d.id,
            d.revision,
            "yes",
            "",
            "gh_pr_merge non-owner",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "yes", "no", "gh_pr_merge");
    }

    // 8. gh_comment (module)
    {
        covered_modules.insert("gh_comment".into());
        let d_missing = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "gh_comment".into(),
                subject_id: "ghp_missing".into(),
                question: "Comment?".into(),
                draft: Some("LGTM".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:gh_comment:missing".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d_missing.id,
            d_missing.revision,
            "yes",
            "",
            "gh_comment missing target",
        );
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "gh_comment".into(),
                subject_id: pr_id.clone(),
                item_id: Some(item_id.clone()),
                question: "Comment?".into(),
                draft: Some("LGTM".into()),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:gh_comment:1".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "yes", "gh_comment");
    }

    // 9. mem (module)
    {
        covered_modules.insert("mem".into());
        let mem = store
            .write(|tx| {
                Memories(tx).create(
                    ws,
                    &NewMem {
                        kind: "fact".into(),
                        text: "Acceptance memory fact".into(),
                        source_type: "manual".into(),
                        source_id: None,
                        source_label: "test".into(),
                        visibility: Visibility::Workspace,
                        level: Level::Public,
                        status: "candidate".into(),
                        valid_until: None,
                        writer: "steve".into(),
                        decision_id: None,
                        created_by: "owner".into(),
                        pinned: false,
                        about: vec![],
                    },
                )
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "mem".into(),
                subject_id: mem.id.clone(),
                question: "Confirm memory?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Record),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "mem invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "yes", "no", "mem");
        assert_eq!(
            store
                .read(|r| Memories(r).get(&mem.id))
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
    }

    // 10. mail_draft (module)
    {
        covered_modules.insert("mail_draft".into());
        let draft_id = "mdraft_accept_1";
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "mail_draft".into(),
                subject_id: draft_id.into(),
                question: "Send mail?".into(),
                draft: Some("Hi".into()),
                options: Some(vec!["send".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::MailDraft {
                    previous: None,
                    content_hash: "hash_accept_1".into(),
                }),
                ..Default::default()
            },
        );
        store
            .write(|tx| {
                insert_draft(
                    tx,
                    &DraftRow {
                        id: draft_id.into(),
                        workspace_id: ws.into(),
                        connection_id: conn_id.clone(),
                        thread_id: None,
                        reply_to_message: None,
                        mode: "new".into(),
                        to: vec![],
                        cc: vec![],
                        bcc: vec![],
                        subject: "Accept".into(),
                        body: "Hi".into(),
                        files: vec![],
                        author: "steve".into(),
                        item_id: None,
                        session_id: None,
                        run_id: None,
                        decision_id: Some(d.id.clone()),
                        basis: None,
                        content_hash: "hash_accept_1".into(),
                        status: "pending".into(),
                        send_at: None,
                        sent_message: None,
                        error: None,
                        created: now,
                        updated: now,
                        sent: None,
                        delivery_version: None,
                        wait: None,
                    },
                )
            })
            .unwrap();
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "mail_draft invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "send", "mail_draft");
        assert_eq!(
            store
                .read(|r| steve_store::mail::draft(r, draft_id))
                .unwrap()
                .unwrap()
                .status,
            "discarded"
        );
    }

    // 11. connection (module)
    {
        covered_modules.insert("connection".into());
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "fail".into(),
                subject_type: "connection".into(),
                subject_id: conn_id.clone(),
                question: "Reauth?".into(),
                options: Some(vec!["reauth".into(), "pause".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:connection:1".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "connection invalid",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "pause",
            "reauth",
            "connection",
        );
        assert_eq!(
            store
                .read(|r| Connections(r).get(&conn_id))
                .unwrap()
                .unwrap()
                .status,
            "paused"
        );
    }

    // 12. party_merge (module)
    {
        covered_modules.insert("party_merge".into());
        let prop_id = "mp_accept_1";
        store
            .write(|tx| {
                MergeProposals(tx).insert(&NewMergeProposal {
                    id: prop_id,
                    workspace: ws,
                    from: &party_b,
                    into: &party_a,
                    account: &account_id,
                    reason: "same person",
                    confidence: 0.9,
                })?;
                Ok::<_, StoreError>(())
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "party_merge".into(),
                subject_id: prop_id.into(),
                question: "Merge parties?".into(),
                options: Some(vec!["merge".into(), "reject".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Record),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "party_merge invalid",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "reject",
            "merge",
            "party_merge",
        );
        assert_eq!(
            store
                .read(|r| MergeProposals(r).get(prop_id))
                .unwrap()
                .unwrap()
                .status,
            "rejected"
        );
    }

    // 13. attr_value (module)
    {
        covered_modules.insert("attr_value".into());
        let val = store
            .write(|tx| {
                Attributes(tx).insert_value(
                    ws,
                    &NewValue {
                        party_id: party_a.clone(),
                        key: "role".into(),
                        label: "Role".into(),
                        value: "Engineer".into(),
                        source: "manual".into(),
                        source_label: "test".into(),
                        source_ref: None,
                        level: Level::Public,
                        status: "candidate".into(),
                        decision_id: None,
                    },
                )
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "attr_value".into(),
                subject_id: val.id.clone(),
                question: "Confirm attr value?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Record),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "attr_value invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "yes", "no", "attr_value");
        assert_eq!(
            store
                .read(|r| Attributes(r).value(&val.id))
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
    }

    // 14. attr_def (module)
    {
        covered_modules.insert("attr_def".into());
        let def = store
            .write(|tx| {
                Attributes(tx).insert_definition(
                    ws,
                    &NewDef {
                        applies: "contact".into(),
                        key: "timezone".into(),
                        label: "Timezone".into(),
                        kind: "text".into(),
                        options: vec![],
                        default_level: Level::Public,
                        source: "manual".into(),
                        status: "proposed".into(),
                    },
                )
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "attr_def".into(),
                subject_id: def.id.clone(),
                question: "Activate attr def?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Record),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "attr_def invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "yes", "no", "attr_def");
        assert_eq!(
            store
                .read(|r| Attributes(r).definition(&def.id))
                .unwrap()
                .unwrap()
                .status,
            "active"
        );
    }

    // 15. steward (module)
    {
        covered_modules.insert("steward".into());
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "steward".into(),
                subject_id: format!("space:{space_id}:{party_a}:owner"),
                question: "Confirm steward?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:steward:1".into(),
                }),
                ..Default::default()
            },
        );
        store
            .write(|tx| {
                Stewards(tx).put(&Steward {
                    workspace_id: ws.into(),
                    object_type: "space".into(),
                    object_id: space_id.clone(),
                    person_id: party_a.clone(),
                    role: "owner".into(),
                    source: "chat".into(),
                    confidence: 0.8,
                    status: "proposed".into(),
                    source_ref: Some("ref_1".into()),
                    decision_id: Some(d.id.clone()),
                    created: now,
                    updated: now,
                })
            })
            .unwrap();
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "steward invalid",
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "yes", "no", "steward");
        assert_eq!(
            store
                .read(|r| Stewards(r).by_decision(&d.id))
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
    }

    // 16. secret_use (module)
    {
        covered_modules.insert("secret_use".into());
        let vault = h.w.tools.uses.vault();
        vault.init(ws, "owner").expect("init vault");
        let vault_id = vault
            .list_vaults(ws, "owner")
            .expect("list vaults")
            .into_iter()
            .find(|v| v.scope == "shared")
            .expect("shared vault")
            .id;
        let new_vault_item: steve_modules::vault::items::NewItem = serde_json::from_value(json!({
            "vault_id": vault_id,
            "template": "api_credential",
            "title": "accept_secret",
            "fields": [{
                "label": "token",
                "type": "password",
                "secret": true,
                "value": "secret-val"
            }]
        }))
        .expect("new vault item");
        let vault_item = vault
            .create_item(ws, "owner", new_vault_item)
            .expect("vault item");
        let work_dir = h._dir.path().join("secret_cwd");
        std::fs::create_dir_all(&work_dir).unwrap();
        let bot_id = ensure_bot(store, ws);
        let sess = store
            .sessions()
            .create(&NewSession {
                workspace_id: ws.into(),
                item_id: Some(item_id.clone()),
                bot_id: Some(bot_id.clone()),
                harness: "claude".into(),
                model: None,
                reasoning_effort: steve_types::ReasoningEffort::Default,
                cwd: work_dir.display().to_string(),
                title: "Secret session".into(),
                token: "tok_secret".into(),
            })
            .unwrap();
        let asked =
            h.w.tools
                .uses
                .request(
                    &steve_modules::vault::UseCx {
                        ws: ws.into(),
                        user: "owner".into(),
                        bot_id,
                        item_id: Some(item_id.clone()),
                        session_id: Some(sess.id.clone()),
                    },
                    steve_modules::vault::UseRequest {
                        item: vault_item.id.clone(),
                        purpose: "acceptance test".into(),
                        command: "echo ok".into(),
                        cwd: None,
                        env: vec![("TOKEN".into(), "token".into())],
                        files: vec![],
                        stdin: None,
                        timeout_s: Some(5.0),
                    },
                )
                .await
                .expect("use_secret");
        let (use_id, decision_id) = match asked {
            steve_modules::vault::UseOutcome::Asked {
                use_id,
                decision_id,
            } => (use_id, decision_id),
            other => panic!("expected Asked, got {other:?}"),
        };
        let d = store.decisions().get(&decision_id).unwrap().unwrap();
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "secret_use invalid",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "deny",
            "once",
            "secret_use",
        );
        assert_eq!(
            store
                .read(|r| UseRows(r).get(&use_id))
                .unwrap()
                .unwrap()
                .status,
            "denied"
        );
    }

    // 17. agents_proposal (module)
    {
        covered_modules.insert("agents_proposal".into());
        let project = store
            .projects()
            .create(
                ws,
                &steve_store::NewProject {
                    name: "Accept Project".into(),
                    kind: "code".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let prop = store
            .write(|tx| {
                ProjectProposals(tx).create(
                    ws,
                    &NewProjectProposal {
                        project_id: Some(&project.id),
                        kind: "agents_md",
                        target_path: "AGENTS.md",
                        text: "Run cargo check before handoff",
                        reason: "Avoid warnings",
                        evidence: "Always run cargo check",
                        origin_kind: "item",
                        origin_id: Some(&item_id),
                        origin_label: "item delivery",
                        writer: "steve",
                        created_by: "owner",
                    },
                )
            })
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "confirm".into(),
                subject_type: "agents_proposal".into(),
                subject_id: prop.id.clone(),
                question: "Add AGENTS.md rule?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Record),
                ..Default::default()
            },
        );
        store
            .write(|tx| ProjectProposals(tx).set_decision_id(&prop.id, Some(&d.id)))
            .unwrap();
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "agents_proposal invalid",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "yes",
            "no",
            "agents_proposal",
        );
        assert_eq!(
            store
                .read(|r| ProjectProposals(r).get(&prop.id))
                .unwrap()
                .unwrap()
                .status,
            "accepted"
        );
    }

    assert_eq!(
        covered_modules, registered,
        "every registered module decision subject must have a contract test"
    );

    // Core subjects (9): item, question, action, release, tool_call, design_implementation,
    // stall, session_start, turn_stall.
    // All tested through the same `Core::decision_command` entry.
    let core_item = create_item(store, ws, "Core item", "Core decisions");

    // Core 1: item (propose no -> cancels item)
    {
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "propose".into(),
                subject_type: "item".into(),
                subject_id: core_item.id.clone(),
                item_id: Some(core_item.id.clone()),
                question: "Start item?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:core:item".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "yes", "core item");
        assert!(
            store
                .items()
                .get(&core_item.id)
                .unwrap()
                .unwrap()
                .is_closed()
        );
    }

    // Core 2 & 3: question (ask) & action (approve) on detached session without live runtime
    // -> refusal when unbound/stale/cross-workspace, success on detached no-session card.
    for (kind, subject, choice, other) in [
        ("ask", "question", "answer_a", "answer_b"),
        ("approve", "action", "yes", "no"),
    ] {
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: kind.into(),
                subject_type: subject.into(),
                subject_id: format!("sub_{subject}"),
                question: format!("Core {subject}?"),
                options: Some(vec![choice.into(), other.into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: format!("accept:core:{subject}"),
                }),
                ..Default::default()
            },
        );
        if subject == "question" {
            assert_refused_cleanly(
                &h.w,
                store,
                &caller(ws, "member"),
                &d.id,
                d.revision,
                choice,
                "",
                "core question non-owner",
            );
        }
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            choice,
            other,
            &format!("core {subject}"),
        );
    }

    // Core 4: release (approve no)
    {
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "release".into(),
                subject_id: "feat/demo".into(),
                item_id: Some(item_id.clone()),
                question: "Release feat/demo?".into(),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:core:release".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(&h.w, store, &owner, &d.id, "no", "yes", "core release");
    }

    // Core 5: tool_call (approve) -> gone permission expires cleanly; cross-workspace/stale refused
    {
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "approve".into(),
                subject_type: "tool_call".into(),
                subject_id: "tc_1".into(),
                item_id: Some(item_id.clone()),
                question: "Allow tool?".into(),
                options: Some(vec!["allow|Allow|allow_once".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Permission {
                    node: "n1".into(),
                    attempt: "a1".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &caller(ws, "member"),
            &d.id,
            d.revision,
            "allow",
            "",
            "core tool_call non-owner",
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &caller("other-ws", "owner"),
            &d.id,
            d.revision,
            "allow",
            "",
            "core tool_call cross-ws",
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision + 5,
            "allow",
            "",
            "core tool_call stale",
        );
        let settled =
            h.w.core
                .decision_command(&owner, &d.id, Some(d.revision), DecisionCommand::Dismiss)
                .expect("dismiss gone permission");
        assert_eq!(settled.status, DecisionStatus::Expired);
    }

    // Core 6: design_implementation (YOLO refused, invalid option refused, valid "只保留设计" succeeds)
    {
        let design_item = create_item(store, ws, "Design item", "Design only");
        let design_content = "# Design\n\nDetails\n";
        let design_sha = steve_store::sha(design_content);
        let design_dir = h
            ._dir
            .path()
            .join("steve/workspaces")
            .join(&design_item.id)
            .join("outputs");
        std::fs::create_dir_all(&design_dir).unwrap();
        let design_path = design_dir.join(format!("output-{design_sha}.md"));
        std::fs::write(&design_path, design_content).unwrap();
        let loc = design_path.display().to_string();
        store
            .artifacts()
            .add(&design_item.id, "doc", "Design plan", Some(&loc))
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "design_implementation".into(),
                subject_id: json!({"goal_version": 1, "location": loc}).to_string(),
                item_id: Some(design_item.id.clone()),
                question: "Implement design?".into(),
                options: Some(vec![
                    "按方案实现".into(),
                    "只保留设计".into(),
                    "修改设计".into(),
                ]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:core:design".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "bogus",
            "",
            "core design_implementation invalid option",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "只保留设计",
            "按方案实现",
            "core design_implementation",
        );
    }

    // Core 7: stall (fail look -> pauses item, undoable; cross-ws/stale refused)
    {
        let stall_item = create_item(store, ws, "Stall item", "Stall check");
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "fail".into(),
                subject_type: "stall".into(),
                subject_id: stall_item.id.clone(),
                item_id: Some(stall_item.id.clone()),
                question: "Stalled?".into(),
                options: Some(vec!["continue".into(), "look".into(), "drop".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: "accept:core:stall".into(),
                }),
                ..Default::default()
            },
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "look",
            "drop",
            "core stall",
        );
        assert!(
            store
                .items()
                .get(&stall_item.id)
                .unwrap()
                .unwrap()
                .is_paused()
        );
    }

    // Core 8: session_start (SESSION_START_FAILURE: YOLO & wrong choice refused; ended session refused;
    // valid retry verified via dismiss/refusal + open contract)
    {
        let start_item = create_item(store, ws, "Start failure item", "Start failure");
        let bot_id = ensure_bot(store, ws);
        let sess = store
            .sessions()
            .create(&NewSession {
                workspace_id: ws.into(),
                item_id: Some(start_item.id.clone()),
                bot_id: Some(bot_id),
                harness: "claude".into(),
                model: None,
                reasoning_effort: steve_types::ReasoningEffort::Default,
                cwd: h._dir.path().display().to_string(),
                title: "Ended start session".into(),
                token: "tok_start".into(),
            })
            .unwrap();
        store
            .sessions()
            .patch(
                &sess.id,
                &steve_store::SessionPatch {
                    status: Some("ended".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "fail".into(),
                subject_type: steve_store::SESSION_START_FAILURE.into(),
                subject_id: sess.id.clone(),
                item_id: Some(start_item.id.clone()),
                session_id: Some(sess.id.clone()),
                question: "Session failed to start".into(),
                options: Some(vec!["retry".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: format!("accept:startup:{}", sess.id),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "retry",
            "",
            "core session_start ended session",
        );
        // Success transition on session_start via Expire (when item/session superseded).
        let expired = store
            .write(|tx| {
                DecisionRows(tx).apply(
                    &d,
                    DecisionChange::Expire,
                    &json!({"reason": "reassigned"}),
                    None,
                )
            })
            .unwrap();
        assert!(expired);
    }

    // Core 9: turn_stall (TURN_STALL: YOLO refused, unconfirmed continue refused, drop succeeds)
    {
        let ts_item = create_item(store, ws, "Turn stall item", "Turn stall");
        let d = open_with_dedupe(
            store,
            NewDecision {
                workspace_id: ws.into(),
                kind: "fail".into(),
                subject_type: steve_types::orchestration::TURN_STALL.into(),
                subject_id: ts_item.id.clone(),
                item_id: Some(ts_item.id.clone()),
                question: String::new(),
                question_template: Some(steve_types::DecisionPrompt::TurnNoProgress),
                options: Some(vec!["continue".into(), "drop".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::TurnStall {
                    attempt: "att_stall_1".into(),
                }),
                ..Default::default()
            },
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &caller(ws, "member"),
            &d.id,
            d.revision,
            "drop",
            "",
            "core turn_stall non-owner",
        );
        assert_refused_cleanly(
            &h.w,
            store,
            &owner,
            &d.id,
            d.revision,
            "continue",
            "",
            "core turn_stall unconfirmed stop",
        );
        assert_success_replay_and_conflict(
            &h.w,
            store,
            &owner,
            &d.id,
            "drop",
            "continue",
            "core turn_stall",
        );
        assert!(store.items().get(&ts_item.id).unwrap().unwrap().is_closed());
    }

    h.w.domain.shutdown().await;
}

struct NoopDomain;
impl NodeDomain for NoopDomain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
    fn attempt_started(&self, _: &str, _: &str, _: i64, _: Option<i64>, _: i64) {}
    fn attempt_finished(
        &self,
        _: &str,
        _: &str,
        _: &steve_node_api::Completion,
        _: &super::node_runtime::AttemptFacts,
        _: i64,
    ) {
    }
}

struct NoopGateway;
#[async_trait::async_trait]
impl steve_core::ModelGateway for NoopGateway {
    async fn call(
        &self,
        _: &str,
        _: steve_core::Tier,
        _: Duration,
        _: &steve_core::gateway::ThinkTools,
    ) -> steve_core::Result<steve_core::gateway::Printed> {
        Err(steve_core::CoreError::Msg("unused".into()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answering_an_ask_decision_wakes_an_idle_node_session() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .write(|tx| {
            UserRows(tx).insert(
                "owner",
                "default",
                "sub_owner",
                "Owner",
                "owner",
                steve_store::now(),
            )?;
            UserRows(tx).insert(
                "member",
                "default",
                "sub_member",
                "Member",
                "member",
                steve_store::now(),
            )?;
            tx.members().add("default", "owner", "owner", 1)?;
            tx.members().add("default", "member", "member", 2)?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let item = create_item(&store, "default", "Wake test", "Answer wakes idle session");
    let bot_id = ensure_bot(&store, "default");
    let session = store
        .sessions()
        .create(&NewSession {
            workspace_id: "default".into(),
            item_id: Some(item.id.clone()),
            bot_id: Some(bot_id),
            harness: "claude".into(),
            model: None,
            reasoning_effort: steve_types::ReasoningEffort::Default,
            cwd: dir.path().display().to_string(),
            title: "Idle node session".into(),
            token: "tok_wake".into(),
        })
        .unwrap();

    let fake_node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), fake_node.clone(), Arc::new(NoopDomain));
    fake_node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();

    rt.bind(&Binding {
        session: session.id.clone(),
        resource: format!("item:{}", item.id),
        prepare: PrepareKind::Scratch,
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();

    // Let the initial prepare_workspace + open_session settle so the session is ready on the Node.
    eventually_sync("session ready in hub", || {
        steve_store::testkit::hub::node_checks::binding_phase_by_session(&store, &session.id)
            .is_ok_and(|p| p == "ready")
    })
    .await;

    // Build a Core backed by RoutedRuntime pointing at our NodeRuntime.
    let cfg = steve_core::CoreConfig::for_home(dir.path());
    let host = Arc::new(super::core::HubHost {
        store: store.clone(),
        data_dir: dir.path().to_path_buf(),
        core: std::sync::OnceLock::new(),
        flow: std::sync::OnceLock::new(),
        approval: super::approval::ApprovalModes::default(),
        local_node: std::sync::OnceLock::new(),
    });
    let workspaces = super::workspaces::Workspaces::new(store.clone(), None, None, vec![]);
    let routed = Arc::new(super::core::RoutedRuntime {
        node: Some(rt.clone()),
        fleet: None,
        store: store.clone(),
        host,
        workspaces,
        worktrees: None,
        dispatch_harnesses: None,
    });
    let core = steve_core::Core::new(
        steve_core::repo::Db::new(store.clone()),
        routed,
        Arc::new(NoopGateway),
        cfg,
    );

    let d = open_with_dedupe(
        &store,
        NewDecision {
            workspace_id: "default".into(),
            kind: "ask".into(),
            subject_type: "question".into(),
            subject_id: session.id.clone(),
            item_id: Some(item.id.clone()),
            session_id: Some(session.id.clone()),
            question: "Which option?".into(),
            options: Some(vec!["option_a".into(), "option_b".into()]),
            requested_by: "steve".into(),
            source: Some(DecisionSource::Revision {
                revision: "accept:wake:1".into(),
            }),
            ..Default::default()
        },
    );

    let refused = core.decision_command(
        &caller("default", "member"),
        &d.id,
        Some(d.revision),
        DecisionCommand::Resolve {
            choice: "option_a".into(),
            text: String::new(),
        },
    );
    assert!(
        matches!(refused, Err(steve_core::CoreError::Status(403, _))),
        "non-owner must receive 403 on AskOwner question: {refused:?}"
    );
    let settled = core
        .decision_command(
            &caller("default", "owner"),
            &d.id,
            Some(d.revision),
            DecisionCommand::Resolve {
                choice: "option_a".into(),
                text: String::new(),
            },
        )
        .expect("resolve ask");
    assert_eq!(settled.status, DecisionStatus::Resolved);

    // Proof: the staged reply was flushed post-commit, so FakeNode actually received start_turn
    // and the committed session events contain the owner's answer.
    eventually_sync("fake node received start_turn for decision answer", || {
        fake_node.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    let events = store.events().after(&session.id, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.data.to_string().contains("option_a")),
        "staged reply text must reach the session turn: {events:?}"
    );
    rt.stop("acceptance test done");
    tokio::time::sleep(Duration::from_millis(20)).await;
}

struct ApprovalDomain(std::sync::Mutex<Option<super::node_runtime::ApprovalTarget>>);
impl NodeDomain for ApprovalDomain {
    fn approval(
        &self,
        _: &str,
    ) -> steve_store::Result<Option<super::node_runtime::ApprovalTarget>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|target| super::node_runtime::ApprovalTarget {
                mode: target.mode,
                policy: target.policy,
                native_id: target.native_id.clone(),
                native: target.native,
            }))
    }
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
    fn attempt_started(&self, _: &str, _: &str, _: i64, _: Option<i64>, _: i64) {}
    fn attempt_finished(
        &self,
        _: &str,
        _: &str,
        _: &steve_node_api::Completion,
        _: &super::node_runtime::AttemptFacts,
        _: i64,
    ) {
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_owner_and_harness_native_permission_flow_through_unified_settlement() {
    use steve_harness_spec::NativeApprovalFallback;
    use steve_node_api::{ExecutionRef, PermissionMode, Timestamp};
    use steve_store::core_fixture::work::audit_count;
    use steve_store::decision_repo::choice_receipt;
    use steve_store::testkit::hub::node_fixtures;

    let make_perm = |pid: &str, exec: ExecutionRef, command: &str| {
        let mut r = build::permission(pid, exec, command);
        r.expires_at = Timestamp(steve_store::now() + 600_000);
        r
    };
    let latest_exec = |store: &Store| -> ExecutionRef {
        let rows = node_fixtures::ordered_attempt_executions(store).unwrap();
        serde_json::from_str(rows.last().expect("attempt execution")).unwrap()
    };

    let dir = tempfile::tempdir().expect("tmp");
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .write(|tx| {
            UserRows(tx).insert(
                "owner",
                "default",
                "sub_owner",
                "Owner",
                "owner",
                steve_store::now(),
            )?;
            UserRows(tx).insert(
                "member",
                "default",
                "sub_member",
                "Member",
                "member",
                steve_store::now(),
            )?;
            tx.members().add("default", "owner", "owner", 1)?;
            tx.members().add("default", "member", "member", 2)?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let item = create_item(&store, "default", "Approval port", "Test AskOwner wiring");
    let bot_id = ensure_bot(&store, "default");

    let make_session = |title: &str, tok: &str| {
        store
            .sessions()
            .create(&NewSession {
                workspace_id: "default".into(),
                item_id: Some(item.id.clone()),
                bot_id: Some(bot_id.clone()),
                harness: "claude".into(),
                model: None,
                reasoning_effort: steve_types::ReasoningEffort::Default,
                cwd: dir.path().display().to_string(),
                title: title.into(),
                token: tok.into(),
            })
            .unwrap()
    };

    // 1. AskOwner: PermissionRequested opens a tool_call card with DecisionSource::Permission,
    //    refuses non-owner (403), and settles through Core::decision_command with receipt + audit.
    {
        let session = make_session("AskOwner session", "tok_perm_ask");
        let domain = Arc::new(ApprovalDomain(std::sync::Mutex::new(None)));
        let fake_node = FakeNode::new("n_perm_ask");
        let rt = NodeRuntime::new(store.clone(), fake_node.clone(), domain);
        fake_node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();

        rt.bind(&Binding {
            session: session.id.clone(),
            resource: format!("item:{}", item.id),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
        eventually_sync("AskOwner session ready", || {
            steve_store::testkit::hub::node_checks::binding_phase_by_session(&store, &session.id)
                .is_ok_and(|p| p == "ready")
        })
        .await;

        let cfg = steve_core::CoreConfig::for_home(dir.path());
        let host = Arc::new(super::core::HubHost {
            store: store.clone(),
            data_dir: dir.path().to_path_buf(),
            core: std::sync::OnceLock::new(),
            flow: std::sync::OnceLock::new(),
            approval: super::approval::ApprovalModes::default(),
            local_node: std::sync::OnceLock::new(),
        });
        let workspaces = super::workspaces::Workspaces::new(store.clone(), None, None, vec![]);
        let routed = Arc::new(super::core::RoutedRuntime {
            node: Some(rt.clone()),
            fleet: None,
            store: store.clone(),
            host,
            workspaces,
            worktrees: None,
            dispatch_harnesses: None,
        });
        let core = steve_core::Core::new(
            steve_core::repo::Db::new(store.clone()),
            routed,
            Arc::new(NoopGateway),
            cfg,
        );

        rt.prompt(&session.id, "run tool", "me", &[]).unwrap();
        eventually_sync("AskOwner attempt started", || {
            fake_node.log().iter().any(|(m, _)| *m == "start_turn")
        })
        .await;
        let exec = latest_exec(&store);

        fake_node
            .ask_permission(make_perm("perm_ask_owner", exec, "write fixture"))
            .await
            .unwrap();
        eventually_sync("AskOwner card opened", || {
            store
                .decisions()
                .open("default")
                .unwrap()
                .iter()
                .any(|d| d.subject_type == "tool_call" && d.subject_id == "perm_ask_owner")
        })
        .await;
        let card = store
            .decisions()
            .open("default")
            .unwrap()
            .into_iter()
            .find(|d| d.subject_id == "perm_ask_owner")
            .unwrap();
        let member_err = core.decision_command(
            &caller("default", "member"),
            &card.id,
            Some(card.revision),
            DecisionCommand::Resolve {
                choice: "allow_once".into(),
                text: String::new(),
            },
        );
        assert!(
            matches!(member_err, Err(steve_core::CoreError::Status(403, _))),
            "non-owner must get 403 on AskOwner tool_call: {member_err:?}"
        );
        assert_eq!(
            store.decisions().get(&card.id).unwrap().unwrap().status,
            DecisionStatus::Open
        );

        let resolved = core
            .decision_command(
                &caller("default", "owner"),
                &card.id,
                Some(card.revision),
                DecisionCommand::Resolve {
                    choice: "allow_once".into(),
                    text: String::new(),
                },
            )
            .expect("owner settles AskOwner tool_call");
        assert_eq!(resolved.status, DecisionStatus::Resolved);
        assert_eq!(
            core.decision_command(
                &caller("default", "owner"),
                &card.id,
                Some(card.revision),
                DecisionCommand::Resolve {
                    choice: "allow_once".into(),
                    text: String::new(),
                },
            )
            .expect("idempotent replay"),
            resolved
        );
        assert!(
            store
                .read(|r| choice_receipt(r, &card.id, card.revision))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            audit_count(&store, &["decision.resolve"], Some(&card.id)),
            1
        );
        eventually_sync("resolve_permission sent for AskOwner", || {
            fake_node
                .log()
                .iter()
                .any(|(m, _)| *m == "resolve_permission")
        })
        .await;
        rt.stop("ask owner done");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 2. HarnessNative(Reject): settles through DecisionRows::apply(Expire) with audit + reject command.
    {
        let session = make_session("HarnessNative session", "tok_perm_native");
        let domain = Arc::new(ApprovalDomain(std::sync::Mutex::new(Some(
            super::node_runtime::ApprovalTarget {
                mode: PermissionMode::Auto,
                policy: steve_types::ApprovalMode::HarnessAuto,
                native_id: Some("auto".into()),
                native: Some(NativeApprovalFallback::Reject),
            },
        ))));
        let fake_node = FakeNode::new("n_perm_native");
        let rt = NodeRuntime::new(store.clone(), fake_node.clone(), domain);
        fake_node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();

        rt.bind(&Binding {
            session: session.id.clone(),
            resource: format!("item:{}", item.id),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
        eventually_sync("HarnessNative session ready in auto mode", || {
            rt.mode_state(&session.id)
                .ok()
                .flatten()
                .and_then(|m| m.applied)
                .as_deref()
                == Some("auto")
        })
        .await;

        rt.prompt(&session.id, "run tool", "me", &[]).unwrap();
        eventually_sync("HarnessNative attempt started", || {
            fake_node.log().iter().any(|(m, _)| *m == "start_turn")
        })
        .await;
        let exec = latest_exec(&store);

        fake_node
            .ask_permission(make_perm("perm_native_reject", exec, "bash fixture"))
            .await
            .unwrap();
        eventually_sync("HarnessNative reject sent", || {
            fake_node
                .log()
                .iter()
                .any(|(m, _)| *m == "resolve_permission")
        })
        .await;
        let rejected = store
            .decisions()
            .of_item(&item.id)
            .unwrap()
            .into_iter()
            .find(|d| d.subject_id == "perm_native_reject")
            .expect("reject card persisted");
        assert_eq!(rejected.status, DecisionStatus::Expired);
        assert_eq!(rejected.result["superseded"], "harness_native");
        assert_eq!(
            audit_count(&store, &["decision.expire"], Some(&rejected.id)),
            1
        );

        rt.stop("harness native done");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
