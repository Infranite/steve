//! Contract tables exercise the old production entry points before their extraction.
mod fixture;
use super::{Finish, REACTION_TIMEOUT};
use crate::respond::{self, Acted, By};
use crate::rules::{Act, ItemMode};
use crate::{ChannelResult, messages, presence};
use fixture::Fixture;
use rusqlite::params;
use std::sync::Arc;
use steve_modules::testkit::{ME, WS};

#[tokio::test]
async fn sg19_execute_actions_and_responder_fallbacks_keep_receipts_and_side_effects() {
    for case in [
        "record",
        "handoff",
        "muted-handoff",
        "ack-refused",
        "reply",
        "draft",
        "start-item",
        "propose-item",
        "taken",
        "empty-label",
        "skip",
        "responder-failed",
    ] {
        let h = Fixture::new(false, false);
        let act = match case {
            "record" => Act::Record,
            "handoff" | "muted-handoff" | "ack-refused" => Act::Handoff,
            "draft" => Act::Draft,
            "start-item" => Act::CreateItem(ItemMode::Start),
            "propose-item" => Act::CreateItem(ItemMode::Propose),
            _ => Act::Reply,
        };
        let mut p = h.plan(act, "Please inspect the deployment");
        p.muted = case == "muted-handoff";
        *h.script.ack_error.lock().unwrap() = case == "ack-refused";
        if case == "responder-failed" {
            *h.script.act_error.lock().unwrap() = Some("scripted responder failure".into());
        }
        let taken = matches!(case, "taken" | "empty-label" | "skip");
        let supplied = if taken {
            let a = Acted {
                label: if case == "empty-label" {
                    String::new()
                } else {
                    "scripted response".into()
                },
                composer_skip: (case == "skip").then(|| "no answer needed".into()),
                ..Default::default()
            };
            *h.script.acted.lock().unwrap() = Some(a.clone());
            Some(a)
        } else {
            None
        };
        let event = h.attach_event(&p.message_id);
        let r = h.ch.execute(&p).await.unwrap();
        h.ch.react_settled(&p.message_id).await;
        assert_eq!(
            (r.by.clone(), r.rule, r.muted),
            (By::Rules, Some(2), false),
            "{case}"
        );
        let expected = match case {
            "record" => Act::Record,
            "reply" | "draft" | "responder-failed" => Act::Draft,
            "start-item" | "propose-item" => Act::Handoff,
            _ => act,
        };
        assert_eq!(r.act, expected, "{case}");
        assert_eq!(
            r.composer_skip,
            supplied.as_ref().and_then(|a| a.composer_skip.clone())
        );
        let expected_label = match case {
            "reply" | "draft" | "responder-failed" => "等你确认回复",
            "start-item" | "propose-item" => "转给你，可以转成事项",
            "taken" | "skip" => "scripted response",
            _ => act.label(),
        };
        assert_eq!(r.label, expected_label, "{case}");
        let count: i64 = h
            .env
            .store()
            .reader(|c| -> ChannelResult<_> {
                Ok(c.query_row("SELECT count(*) FROM decisions", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, i64::from(r.decision_id.is_some()), "{case}");
        if matches!(expected, Act::Handoff)
            || matches!(case, "reply" | "draft" | "responder-failed")
        {
            let d = r.decision_id.as_ref().unwrap();
            let (kind, subject): (String, String) = h
                .env
                .store()
                .reader(|c| -> ChannelResult<_> {
                    Ok(c.query_row(
                        "SELECT kind,subject_id FROM decisions WHERE id=?",
                        [d],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?)
                })
                .unwrap();
            assert_eq!(
                kind,
                if expected == Act::Handoff {
                    "ask"
                } else {
                    "approve"
                }
            );
            if let Some(out) = &r.out_message_id {
                let m = h
                    .env
                    .store()
                    .reader(|c| -> ChannelResult<_> { Ok(messages::get(c, out)?) })
                    .unwrap()
                    .unwrap();
                assert_eq!((m.status.as_str(), m.text.as_deref()), ("draft", Some("")));
                assert_eq!(subject, *out);
                assert_eq!(
                    h.row(&p.message_id).unwrap().answer_id.as_deref(),
                    Some(out.as_str())
                );
            } else {
                assert_eq!(subject, p.message_id);
            }
        }
        let calls = h.calls();
        if act == Act::Handoff {
            assert_eq!(calls.len(), usize::from(!p.muted), "{case}: {calls:?}");
            if !p.muted {
                assert!(calls[0].starts_with("handoff:1:我请 "), "{calls:?}");
            }
        } else if act == Act::Record {
            assert!(calls.is_empty());
            assert!(h.row(&p.message_id).is_none());
        } else {
            assert!(
                calls.iter().any(|s| s == "act:ack"),
                "ack is recorded before responder: {case}: {calls:?}"
            );
        }
        if matches!(case, "start-item" | "propose-item") {
            assert_eq!(h.row(&p.message_id).unwrap().state, "cleared");
        }
        h.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(respond::mark_message_event(tx, &p.message_id, &r)?)
            })
            .unwrap();
        let (outcome, label, reason, decision, item) = h.event(&event);
        if matches!(case, "taken" | "empty-label") {
            // An acted reply without a card, item or composer skip leaves the signal untouched.
            assert_eq!(
                (
                    outcome.as_str(),
                    label.as_str(),
                    reason.as_str(),
                    decision,
                    item
                ),
                ("recorded", "", "", None, None),
                "{case}"
            );
            continue;
        }
        assert_eq!(label, r.label, "{case}");
        assert_eq!(decision, r.decision_id);
        assert_eq!(item, r.item_id);
        assert_eq!(
            outcome,
            if r.decision_id.is_some() {
                "need"
            } else {
                "recorded"
            }
        );
        assert_eq!(
            reason,
            if case == "skip" {
                "起草器：no answer needed"
            } else if r.act == Act::Record {
                "规则：响应规则第 3 条"
            } else {
                "响应规则第 3 条"
            }
        );
    }
}

#[tokio::test]
async fn sg19_taken_response_copies_ids_and_records_the_actual_outbound_text() {
    let h = Fixture::new(false, false);
    let p = h.plan(Act::Reply, "Need a reply");
    let (out, d) = h
        .env
        .store()
        .transaction(|tx| -> ChannelResult<_> {
            Ok(respond::open_draft(
                tx,
                &p,
                "actual outbound words",
                h.ch.now(),
            )?)
        })
        .unwrap();
    *h.script.acted.lock().unwrap() = Some(Acted {
        label: "draft prepared".into(),
        decision_id: Some(d.clone()),
        out_message_id: Some(out.clone()),
        ..Default::default()
    });
    let r = h.ch.execute(&p).await.unwrap();
    h.ch.react_settled(&p.message_id).await;
    assert_eq!(
        (r.decision_id, r.out_message_id, r.label),
        (Some(d), Some(out.clone()), "draft prepared".into())
    );
    let (text, meta): (String, String) = h
        .env
        .store()
        .reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT text,meta FROM messages WHERE author='steve' ORDER BY rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(text, "actual outbound words");
    let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(meta["chan_out"]["message"], out);
    assert_eq!(meta["chan_out"]["status"], "draft");
    // The responder port's item id is opaque here; no item persistence is claimed by this branch.
    let item = h.plan(Act::CreateItem(ItemMode::Start), "Make an item");
    *h.script.acted.lock().unwrap() = Some(Acted {
        item_id: Some("ST-19".into()),
        ..Default::default()
    });
    let r = h.ch.execute(&item).await.unwrap();
    h.ch.react_settled(&item.message_id).await;
    assert_eq!(
        (r.act, r.item_id),
        (Act::CreateItem(ItemMode::Start), Some("ST-19".into()))
    );
}

#[tokio::test]
async fn sg19_owner_grant_presence_content_and_nudge_gates_precede_any_responder() {
    for case in [
        "no-grant",
        "grant-before-presence",
        "owner-replied",
        "merged",
        "empty",
        "nudge",
    ] {
        let h = Fixture::new(true, false);
        let first = if case == "nudge" {
            let older = h.plan(Act::Handoff, "Can you decide?");
            Some(
                h.env
                    .store()
                    .transaction(|tx| -> ChannelResult<_> {
                        Ok(respond::open_handoff(tx, &older)?)
                    })
                    .unwrap(),
            )
        } else {
            None
        };
        if case == "nudge" {
            h.env.clock.advance(presence::BURST_GAP_MS + 1);
        }
        let text = match case {
            "empty" => "😀",
            "nudge" => "???",
            _ => "Should we deploy?",
        };
        let p = h.plan(Act::Draft, text);
        if matches!(case, "no-grant" | "grant-before-presence") {
            h.grant(false);
        }
        if matches!(case, "owner-replied" | "grant-before-presence") {
            h.env.clock.advance(1_000);
            h.inbound("I will handle this", Some(&h.account), None);
        }
        if case == "merged" {
            let next = h.plan(Act::Draft, "More deployment context");
            h.env
                .store()
                .transaction(|tx| -> ChannelResult<_> {
                    Ok(presence::take_turn(tx, WS, &next.message_id)?)
                })
                .unwrap();
        }
        let event = h.attach_event(&p.message_id);
        let r = h.ch.execute(&p).await.unwrap();
        assert_eq!(r.act, Act::Record, "{case}");
        let label = match case {
            "no-grant" | "grant-before-presence" => respond::NO_SEND_AS_YOU,
            "owner-replied" => presence::LABEL_REPLIED,
            "merged" => presence::BY_LATER,
            "empty" => crate::decide::LABEL_NO_CONTENT,
            _ => crate::decide::LABEL_NUDGE,
        };
        assert_eq!(r.label, label, "{case}");
        assert_eq!(r.decision_id, first);
        assert!(h.calls().is_empty(), "{case}: {:?}", h.calls());
        assert!(h.row(&p.message_id).is_none());
        h.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(respond::mark_message_event(tx, &p.message_id, &r)?)
            })
            .unwrap();
        assert_eq!(h.event(&event).0, "recorded");
        if let Some(d) = r.decision_id {
            let question: String = h
                .env
                .store()
                .reader(|c| -> ChannelResult<_> {
                    Ok(
                        c.query_row("SELECT question FROM decisions WHERE id=?", [d], |r| {
                            r.get(0)
                        })?,
                    )
                })
                .unwrap();
            assert!(question.ends_with("???"), "{question}");
        }
    }
}

#[tokio::test]
async fn sg19_conversation_and_trigger_failures_keep_routing_and_reasons() {
    for case in [
        "bot-conversation",
        "trigger-conversation",
        "conversation-fallback",
        "trigger-no-port",
        "trigger-port-failed",
        "trigger-unclaimed",
        "web-command",
        "web-refused",
    ] {
        let own = matches!(case, "web-command" | "web-refused");
        let with_thread = case != "trigger-no-port";
        let h = Fixture::new(own, with_thread);
        let mut p = h.plan(
            if case == "trigger-unclaimed" {
                Act::CreateItem(ItemMode::Start)
            } else {
                Act::Reply
            },
            "/bot inspect deployment",
        );
        if case != "bot-conversation" && case != "conversation-fallback" {
            p.trigger = Some("inspect deployment".into());
        }
        if matches!(
            case,
            "conversation-fallback" | "trigger-port-failed" | "web-refused"
        ) {
            *h.script.thread_error.lock().unwrap() = Some("scripted thread failure".into());
        }
        if case == "trigger-port-failed" {
            *h.script.act_error.lock().unwrap() = Some("scripted responder failure".into());
        }
        if own {
            h.grant(false);
            let owner = h
                .env
                .store()
                .reader(|c| -> ChannelResult<_> {
                    Ok(steve_modules::contacts::repo::me_of(c, WS, ME)?)
                })
                .unwrap()
                .unwrap();
            p.message_id = h.inbound(&p.text, Some(&h.account), Some(&owner.id));
            p.external_id = h.provider_id(&p.message_id);
            p.sender_party_id = Some(owner.id);
            p.sender_account_id = Some(h.account.clone());
        }
        let r = h.ch.execute(&p).await.unwrap();
        h.ch.react_settled(&p.message_id).await;
        assert_eq!(
            r.by,
            match &p.trigger {
                Some(i) => By::Trigger {
                    instruction: i.clone()
                },
                None => By::Rules,
            }
        );
        match case {
            "bot-conversation" | "trigger-conversation" => {
                assert_eq!(
                    r.label,
                    if p.trigger.is_some() {
                        "你的指令，交给对话回答"
                    } else {
                        "交给对话回答"
                    }
                );
                assert!(!h.calls().iter().any(|s| s.starts_with("act:")));
                let turns = h.script.turns.lock().unwrap();
                assert_eq!(turns.len(), 1);
                assert_eq!(turns[0].space, crate::conversations::conv_space(&h.space));
                assert_eq!(turns[0].meta["chan"]["command"], p.trigger.is_some());
                assert_eq!(
                    turns[0].text,
                    if p.trigger.is_some() {
                        "inspect deployment"
                    } else {
                        p.text.as_str()
                    }
                );
            }
            "conversation-fallback" => {
                assert_eq!(r.act, Act::Draft);
                assert!(r.decision_id.is_some());
                assert!(h.calls().iter().any(|s| s == "act:ack"));
            }
            "web-command" => {
                assert_eq!(r.label, respond::NO_SEND_AS_YOU_COMMAND);
                assert_eq!(r.act, Act::Record);
                assert_eq!(
                    h.script.turns.lock().unwrap()[0].space,
                    crate::conversations::MAIN_CONV
                );
            }
            _ => {
                assert_eq!(r.act, Act::Record);
                assert_eq!(
                    r.label,
                    if own {
                        respond::NO_SEND_AS_YOU
                    } else {
                        "触发语未接入，只记录"
                    }
                );
                let why:String=h.env.store().reader(|c|-> ChannelResult<_> { Ok(c.query_row("SELECT text FROM messages WHERE author='steve' ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0))?) }).unwrap();
                if case == "trigger-no-port" {
                    assert!(why.contains("渠道模块没有接上 Steve 的对话"), "{why}");
                } else if case == "trigger-unclaimed" {
                    assert!(why.contains("渠道的回复器没有接手这条触发词"), "{why}");
                } else {
                    assert!(why.contains("scripted thread failure"), "{why}");
                }
                if case == "trigger-port-failed" {
                    assert!(why.contains("scripted responder failure"), "{why}");
                    assert!(why.contains('；'));
                }
                assert_eq!(h.row(&p.message_id).unwrap().state, "failed");
            }
        }
    }
}

#[tokio::test]
async fn sg19_event_outcomes_trails_and_protected_events_keep_precedence() {
    let h = Fixture::new(false, false);
    let p = h.plan(Act::Record, "Record this");
    let base = h.ch.execute(&p).await.unwrap();
    for case in [
        "skip",
        "record",
        "draft",
        "item",
        "thread",
        "trigger-draft",
        "trigger-record",
        "composer",
        "flow",
        "handled",
        "run",
        "flow-id",
    ] {
        let event = h.attach_event(&p.message_id);
        let mut r = base.clone();
        r.label = "contract outcome".into();
        match case {
            "skip" => r.by = By::Skipped,
            "draft" => {
                r.act = Act::Draft;
                r.decision_id = Some("decision-contract".into());
            }
            "item" => {
                r.act = Act::CreateItem(ItemMode::Start);
                r.item_id = Some("ST-19".into());
            }
            "thread" => {
                r.by = By::Thread {
                    item: "ST-19".into(),
                }
            }
            "trigger-draft" => {
                r.by = By::Trigger {
                    instruction: "inspect".into(),
                };
                r.decision_id = Some("decision-contract".into());
            }
            "trigger-record" => {
                r.by = By::Trigger {
                    instruction: "inspect".into(),
                }
            }
            "composer" => r.composer_skip = Some("nothing to say".into()),
            "flow" => {
                r.by = By::Flow {
                    flow_id: "flow-contract".into(),
                }
            }
            "handled" | "run" | "flow-id" => {
                h.env
                    .store()
                    .transaction(|tx| -> ChannelResult<_> {
                        Ok(tx.execute(
                            "UPDATE ext_events SET outcome=?,run_id=?,flow_id=? WHERE id=?",
                            params![
                                if case == "handled" {
                                    "item"
                                } else {
                                    "recorded"
                                },
                                (case == "run").then_some("run-contract"),
                                (case == "flow-id").then_some("flow-contract"),
                                event
                            ],
                        )?)
                    })
                    .unwrap();
            }
            _ => {}
        }
        let before = h.event(&event);
        h.env
            .store()
            .transaction(|tx| -> ChannelResult<_> { Ok(respond::mark_event(tx, &event, &r)?) })
            .unwrap();
        let after = h.event(&event);
        if matches!(case, "flow" | "handled" | "run" | "flow-id") {
            assert_eq!(after, before, "{case}");
            continue;
        }
        let outcome = match case {
            "draft" | "trigger-draft" => "need",
            "item" | "thread" => "item",
            _ => "recorded",
        };
        let reason = match case {
            "trigger-draft" | "trigger-record" => "",
            "composer" => "起草器：nothing to say",
            "skip" | "record" => "规则：响应规则第 3 条",
            _ => "响应规则第 3 条",
        };
        assert_eq!(
            (after.0.as_str(), after.1.as_str(), after.2.as_str()),
            (outcome, "contract outcome", reason),
            "{case}"
        );
        assert_eq!((after.3, after.4), (r.decision_id, r.item_id));
    }
    let event = h.attach_event(&p.message_id);
    h.env
        .store()
        .transaction(|tx| -> ChannelResult<()> {
            respond::append_label(tx, &p.message_id, "later").unwrap();
            respond::append_label(tx, &p.message_id, "later")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h.event(&event).1, "later");
}

#[tokio::test]
async fn sg19_finish_transition_matrix_retains_finality_and_provider_order() {
    for state in ["ack", "failed", "cleared", "done"] {
        for how in [Finish::Done, Finish::Failed, Finish::Clear] {
            let h = Fixture::new(false, false);
            let p = h.plan(Act::Record, "reaction target");
            h.seed_reaction(
                &p.message_id,
                state,
                Some("old-reaction"),
                Some("old error"),
            );
            let before = h.row(&p.message_id).unwrap();
            h.ch.react_finish_inner(&p.message_id, how).await.unwrap();
            let row = h.row(&p.message_id).unwrap();
            let allowed = match state {
                "ack" => true,
                "failed" => how == Finish::Done,
                "cleared" => how != Finish::Clear,
                _ => false,
            };
            if !allowed {
                assert_eq!(row, before, "{state}/{how:?}");
                assert!(h.calls().is_empty());
                continue;
            }
            let target = match how {
                Finish::Done => "done",
                Finish::Failed => "failed",
                Finish::Clear => "cleared",
            };
            assert_eq!(row.state, target);
            assert_eq!(row.error, None);
            assert_eq!(row.updated, h.ch.now());
            let mut calls = vec![format!(
                "remove:old-reaction:true:{}",
                h.provider_id(&p.message_id)
            )];
            if how != Finish::Clear {
                calls.push(format!("add:{how:?}:{}", h.provider_id(&p.message_id)));
            }
            assert_eq!(h.calls(), calls, "{state}/{how:?}");
            assert_eq!(
                row.reaction_id,
                if how == Finish::Clear {
                    None
                } else {
                    Some(format!("reaction-{how:?}"))
                }
            );
            assert_eq!(
                row.emoji,
                if how == Finish::Clear {
                    None
                } else {
                    Some(format!("emoji-{how:?}"))
                }
            );
            assert!(row.as_user);
            let before = row;
            h.clear_calls();
            h.ch.react_finish_inner(&p.message_id, how).await.unwrap();
            assert_eq!(h.row(&p.message_id).unwrap(), before);
            assert!(h.calls().is_empty());
        }
    }
}

#[tokio::test]
async fn sg19_finish_provider_failures_preserve_the_right_error_and_answer_link() {
    for case in [
        "remove-failed",
        "add-failed",
        "both-failed",
        "clear-refused-ack",
        "clear-remove-failed",
    ] {
        let h = Fixture::new(false, false);
        let p = h.plan(Act::Record, "finish despite provider failure");
        let rid = (case != "clear-refused-ack").then_some("old-reaction");
        h.seed_reaction(&p.message_id, "ack", rid, Some("ack refusal"));
        let (out, _) = h
            .env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(respond::open_draft(tx, &p, "answer", h.ch.now())?)
            })
            .unwrap();
        h.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(super::expect_answer(tx, &p.message_id, &out)?)
            })
            .unwrap();
        if matches!(
            case,
            "remove-failed" | "both-failed" | "clear-remove-failed"
        ) {
            *h.script.remove_error.lock().unwrap() = Some("remove refusal".into());
        }
        if matches!(case, "add-failed" | "both-failed") {
            *h.script.add_error.lock().unwrap() = Some("add refusal".into());
        }
        let clear = case.starts_with("clear");
        h.ch.react_finish_inner(
            &p.message_id,
            if clear { Finish::Clear } else { Finish::Failed },
        )
        .await
        .unwrap();
        let row = h.row(&p.message_id).unwrap();
        assert_eq!(row.state, if clear { "cleared" } else { "failed" });
        assert_eq!(row.answer_id, Some(out));
        assert_eq!(
            row.error.as_deref(),
            Some(match case {
                "add-failed" | "both-failed" => "add refusal",
                "clear-refused-ack" => "ack refusal",
                _ => "remove refusal",
            })
        );
        assert_eq!(
            row.reaction_id,
            if case == "remove-failed" {
                Some("reaction-Failed".into())
            } else {
                None
            }
        );
        assert_eq!(
            h.calls().len(),
            if case == "clear-refused-ack" {
                0
            } else if clear {
                1
            } else {
                2
            }
        );
    }
}

#[tokio::test]
async fn sg19_finish_no_row_no_target_and_disconnected_spaces_do_not_call_provider() {
    let h = Fixture::new(false, false);
    let p = h.plan(Act::Record, "not acknowledged");
    h.ch.react_finish_inner("absent", Finish::Done)
        .await
        .unwrap();
    h.ch.react_finish_inner(&p.message_id, Finish::Done)
        .await
        .unwrap();
    assert!(h.row(&p.message_id).is_none());
    assert!(h.calls().is_empty());
    for how in [Finish::Done, Finish::Failed, Finish::Clear] {
        let h = Fixture::new(false, false);
        let p = h.plan(Act::Record, "disconnect after ack");
        h.seed_reaction(&p.message_id, "ack", Some("old-reaction"), None);
        h.env
            .store()
            .transaction(|tx| -> ChannelResult<_> {
                Ok(tx.execute("DELETE FROM connections WHERE id=?", [&h.conn])?)
            })
            .unwrap();
        h.ch.react_finish_inner(&p.message_id, how).await.unwrap();
        let row = h.row(&p.message_id).unwrap();
        assert_eq!(
            row.state,
            match how {
                Finish::Done => "done",
                Finish::Failed => "failed",
                Finish::Clear => "cleared",
            }
        );
        assert_eq!(
            (row.reaction_id, row.emoji, row.as_user, row.error),
            (None, None, true, None)
        );
        assert!(h.calls().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn sg19_finish_timeout_records_failure_without_unbounded_wait() {
    for point in ["add", "remove", "both"] {
        let h = Fixture::new(false, false);
        let p = h.plan(Act::Record, "provider never returns");
        h.seed_reaction(
            &p.message_id,
            "ack",
            (point != "add").then_some("old-reaction"),
            None,
        );
        *h.script.hang.lock().unwrap() = point != "remove";
        *h.script.remove_hang.lock().unwrap() = point != "add";
        let start = tokio::time::Instant::now();
        h.ch.react_finish_inner(&p.message_id, Finish::Done)
            .await
            .unwrap();
        assert_eq!(
            tokio::time::Instant::now() - start,
            REACTION_TIMEOUT * if point == "both" { 2 } else { 1 }
        );
        let row = h.row(&p.message_id).unwrap();
        assert_eq!(row.state, "done");
        assert_eq!(row.error.as_deref(), Some("表情回复超时"));
        assert_eq!(
            row.reaction_id,
            (point == "remove").then(|| "reaction-Done".into())
        );
    }
}

#[tokio::test]
async fn sg19_finish_waits_for_its_ack_lock_without_locking_other_messages() {
    let h = Fixture::new(false, false);
    let p = h.plan(Act::Record, "first target");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *h.script.pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    h.ch.react_ack(&p.message_id).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let finish = h.ch.react_finish_inner(&p.message_id, Finish::Done);
    tokio::pin!(finish);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(finish.as_mut(), cx).is_pending()
        ))
        .await
    );
    let other = h.plan(Act::Record, "independent target");
    h.seed_reaction(&other.message_id, "ack", None, None);
    h.ch.react_finish_inner(&other.message_id, Finish::Clear)
        .await
        .unwrap();
    assert_eq!(h.row(&other.message_id).unwrap().state, "cleared");
    assert_eq!(h.row(&p.message_id).unwrap().state, "ack");
    release.notify_one();
    finish.await.unwrap();
    assert_eq!(h.row(&p.message_id).unwrap().state, "done");
    assert_eq!(
        h.calls(),
        vec![
            format!("add:Ack:{}", h.provider_id(&p.message_id)),
            format!("remove:reaction-Ack:true:{}", h.provider_id(&p.message_id)),
            format!("add:Done:{}", h.provider_id(&p.message_id))
        ]
    );
}

#[tokio::test]
async fn sg19_finish_database_refusal_is_returned_after_provider_work() {
    let h = Fixture::new(false, false);
    let p = h.plan(Act::Record, "db failure");
    h.seed_reaction(&p.message_id, "ack", Some("old-reaction"), None);
    let before = h.row(&p.message_id).unwrap();
    h.env.store().transaction(|tx|-> ChannelResult<_> { Ok(tx.execute_batch("CREATE TRIGGER sg19_refuse_finish BEFORE UPDATE ON chan_reactions BEGIN SELECT RAISE(FAIL,'sg19 storage refusal'); END;")?) }).unwrap();
    let error =
        h.ch.react_finish_inner(&p.message_id, Finish::Done)
            .await
            .unwrap_err();
    assert!(
        error.to_string().contains("sg19 storage refusal"),
        "{error}"
    );
    assert_eq!(h.row(&p.message_id).unwrap(), before);
    assert_eq!(h.calls().len(), 2);
}

#[tokio::test]
async fn sg19_failed_owner_trigger_sends_only_the_fixed_source_note_and_tolerates_sink_errors() {
    for mode in ["skip", "refuse", "mark-refuse"] {
        let h = Fixture::new(true, false);
        let owner = h
            .env
            .store()
            .reader(|c| steve_modules::contacts::repo::me_of(c, WS, ME))
            .unwrap()
            .unwrap();
        let mut p = h.plan(Act::Reply, "/bot inspect deployment");
        p.message_id = h.inbound(&p.text, Some(&h.account), Some(&owner.id));
        p.external_id = h.provider_id(&p.message_id);
        p.sender_account_id = Some(h.account.clone());
        p.sender_party_id = Some(owner.id);
        p.trigger = Some("inspect deployment".into());
        *h.script.sink.lock().unwrap() = Some(mode);
        let r = h.ch.execute(&p).await.unwrap();
        h.ch.react_settled(&p.message_id).await;
        assert_eq!(
            (r.act, r.label.as_str()),
            (Act::Record, "触发语未接入，只记录")
        );
        assert_eq!(h.row(&p.message_id).unwrap().state, "failed");
        let requests = h.script.deliveries.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.text, crate::conversations::TRIGGER_FAILED_NOTE);
        assert_eq!(req.purpose, crate::deliver::Purpose::Source);
        assert!(!req.approve);
        assert!(req.generation.is_none());
        assert!(
            matches!(&req.auth, crate::deliver::Auth::Trigger {source} if source == &p.message_id)
        );
        assert_eq!(req.target.mode, crate::spaces::Mode::Own);
        let (id, meta): (String, String) = h
            .env
            .store()
            .reader(|c| -> ChannelResult<_> {
                Ok(c.query_row(
                    "SELECT id,meta FROM messages WHERE author='steve' ORDER BY rowid DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta["error"], true);
        assert_eq!(
            req.operation_id.as_deref(),
            Some(
                format!(
                    "conv:{id}:{}:trigger_failed",
                    meta["origin"].as_str().unwrap()
                )
                .as_str()
            )
        );
        assert_eq!(
            meta["chan_out"]["status"].as_str(),
            (mode == "skip").then_some("skipped")
        );
    }
}

#[tokio::test]
async fn sg19_partial_sender_preserves_the_existing_owner_gate_decode_error() {
    // This optional sender shape currently makes the ownership SQL NULL. Preserve the old refusal;
    // repairing it is a behavior change outside this extraction, recorded as a delivery finding.
    let h = Fixture::new(true, false);
    let mut p = h.plan(Act::Draft, "A message with only a party");
    let empty = serde_json::json!([]);
    p.message_id = h
        .env
        .store()
        .transaction(|tx| {
            messages::insert_inbound(
                tx,
                &messages::NewInbound {
                    ws: WS,
                    space_id: &h.space,
                    external_id: "partial-sender",
                    thread_key: None,
                    reply_to: None,
                    sender_account_id: None,
                    sender_party_id: Some(&h.guest),
                    text: &p.text,
                    attachments: &empty,
                    mentions: &empty,
                    created: h.ch.now(),
                    content_key: None,
                    fallback_from: None,
                    history: None,
                    seen_by: 1,
                },
            )
        })
        .unwrap()
        .unwrap();
    p.external_id = "partial-sender".into();
    p.sender_account_id = None;
    let error = h.ch.execute(&p).await.unwrap_err();
    assert!(
        error.to_string().contains("Invalid column type Null"),
        "{error}"
    );
    assert!(h.calls().is_empty());
    assert!(h.row(&p.message_id).is_none());
    let count: i64 = h
        .env
        .store()
        .reader(|c| -> ChannelResult<_> {
            Ok(c.query_row("SELECT count(*) FROM decisions", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(count, 0);
}
