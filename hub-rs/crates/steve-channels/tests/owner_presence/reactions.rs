use super::*;
use steve_channels::{ReactionEvent, ReactionOrigin};

fn reaction(h: &H, space: &str, target: &str, from: &str) -> ReactionEvent {
    ReactionEvent {
        space_ext_id: Some(space.into()),
        message_ext_id: Some(target.into()),
        message_source_id: None,
        actor: FakeChannel::handle(from),
        actor_name: None,
        actor_is_bot: false,
        origin: ReactionOrigin::Manual,
        emoji: "👍".into(),
        at: h.ch.now() + 30_000,
        time_source: steve_channels::ReactionTimeSource::Provider,
        revision: None,
        removed: false,
    }
}

fn stop(h: &H, message: &str) -> Option<presence::Stop> {
    h.env
        .store()
        .reader(|c| presence::stop(c, WS, message, h.ch.now()))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn source_id_only_reactions_close_drafts_and_hold_yolo_at_send() {
    use steve_channels::Outbound;
    use steve_channels::deliver::{Auth, Delivered, OutRequest, OutTarget, Purpose};
    for pending in [false, true] {
        let h = setup_ids(true).await;
        let mut message = msg(&h, "D_LI", "", "ulilei", "请确认", 0).source_id("source-only");
        message.thread_key = None;
        let got = h.ch.on_inbound(WS, &h.me, message).await.unwrap();
        let plan = h
            .env
            .store()
            .reader(|c| steve_channels::flow_actions::plan_of(c, WS, &got.message_id))
            .unwrap();
        let mut target = OutTarget::reply(&plan);
        // A service without a platform id can reply using its connection-scoped delivery id.
        target.reply_to = None;
        let delivered = h
            .outbox
            .deliver(
                &h.ch,
                &OutRequest {
                    target,
                    text: "回复草稿".into(),
                    purpose: Purpose::Reply,
                    approve: true,
                    generation: Some(Default::default()),
                    auth: Auth::None,
                    operation_id: None,
                },
            )
            .await
            .unwrap();
        let Delivered::Drafted {
            message_id: draft,
            decision_id: card,
        } = delivered
        else {
            panic!("{delivered:?}")
        };
        checked(&h).await;
        if pending {
            yolo(&h, &card, "yes").unwrap();
            assert_eq!(status(&h, &draft), "pending");
        }
        let mut ev = reaction(&h, "D_LI", "unused", "upx");
        ev.message_ext_id = None;
        ev.message_source_id = Some("source-only".into());
        h.ch.on_reaction(WS, &h.me, ev).unwrap();
        assert!(stop(&h, &got.message_id).is_some());
        if !pending {
            assert_eq!(status(&h, &draft), "discarded");
            assert_eq!(decision(&h, &card).0, "expired");
        }
        h.env.clock.advance(11_000);
        assert_eq!(h.outbox.send_due(&h.ch).await, 0);
        assert_eq!(status(&h, &draft), "discarded");
        assert!(h.fake.sent.lock().unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_source_in_another_chat_cannot_bind_or_persist_a_draft() {
    use steve_channels::Outbound;
    use steve_channels::deliver::{Auth, OutRequest, OutTarget, Purpose};
    let h = setup().await;
    let source =
        h.ch.on_inbound(
            WS,
            &h.me,
            msg(&h, "D_LI", "private", "ulilei", "Private question", 0),
        )
        .await
        .unwrap();
    let target =
        h.ch.on_inbound(
            WS,
            &h.me,
            msg(&h, "C_DEV", "group", "ulilei", "Group question", 0),
        )
        .await
        .unwrap();
    let plan = h
        .env
        .store()
        .reader(|c| steve_channels::flow_actions::plan_of(c, WS, &target.message_id))
        .unwrap();
    let mut target = OutTarget::reply(&plan);
    target.source_message_id = Some(source.message_id);
    let result = steve_channels::DraftOnly
        .deliver(
            &h.ch,
            &OutRequest {
                target,
                text: "Draft".into(),
                purpose: Purpose::Reply,
                approve: true,
                generation: Some(Default::default()),
                auth: Auth::None,
                operation_id: None,
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(drafts(&h), 0);
    assert_eq!(count(&h, "SELECT count(*) FROM chan_reply_sources", &[]), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn producer_revision_orders_same_millisecond_removal_readd_and_replay() {
    let h = setup().await;
    let source =
        h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请确认", 0))
            .await
            .unwrap();
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    ev.time_source = steve_channels::ReactionTimeSource::Observed;
    ev.revision = Some(10);
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    assert!(stop(&h, &source.message_id).is_some());
    ev.removed = true;
    ev.revision = Some(11);
    let deleted = ev.clone();
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    assert_eq!(stop(&h, &source.message_id), None);
    ev.removed = false;
    ev.revision = Some(12);
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    h.ch.on_reaction(WS, &h.me, deleted).unwrap();
    assert!(
        stop(&h, &source.message_id).is_some(),
        "newer revision re-adds even at the same millisecond"
    );
    let got = h
        .env
        .store()
        .reader(|c| steve_channels::incoming_reactions::list(c, &source.message_id, -1))
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].time_source, "observed");
    assert_eq!(got[0].revision, Some(12));
    // An unversioned redelivery cannot replace a versioned state just by carrying a later clock.
    ev.revision = None;
    ev.removed = true;
    ev.at += 50_000;
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    assert!(stop(&h, &source.message_id).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_revision_wins_when_the_producer_clock_moves_backwards() {
    let h = setup().await;
    let source =
        h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请确认", 0))
            .await
            .unwrap();
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    ev.revision = Some(1);
    let stale = ev.clone();
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    ev.revision = Some(2);
    ev.removed = true;
    ev.at -= 1000;
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    h.ch.on_reaction(WS, &h.me, stale).unwrap();
    assert_eq!(stop(&h, &source.message_id), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn revisions_follow_the_same_target_when_its_provider_reference_shape_changes() {
    let h = setup().await;
    let m = msg(&h, "D_LI", "l1", "ulilei", "请确认", 0).source_id("source_l1");
    let source = h.ch.on_inbound(WS, &h.me, m).await.unwrap();
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    ev.revision = Some(11);
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    assert!(stop(&h, &source.message_id).is_some());
    ev.message_ext_id = None;
    ev.message_source_id = Some("source_l1".into());
    ev.revision = Some(12);
    ev.at -= 1000;
    ev.removed = true;
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    assert_eq!(
        stop(&h, &source.message_id),
        None,
        "the delivery-id alias is the same message"
    );
    ev.message_ext_id = Some("l1".into());
    ev.revision = None;
    ev.at += 60_000;
    ev.removed = false;
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    assert_eq!(
        stop(&h, &source.message_id),
        None,
        "unversioned alias cannot bypass version ordering"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_automated_action_does_not_turn_its_human_account_into_a_bot() {
    let h = setup().await;
    let source =
        h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请确认", 0))
            .await
            .unwrap();
    let mut ev = reaction(&h, "D_LI", "l1", "u_new");
    ev.origin = ReactionOrigin::Automated;
    assert!(!ev.actor_is_bot);
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    let got = h
        .env
        .store()
        .reader(|c| steve_channels::incoming_reactions::list(c, &source.message_id, -1))
        .unwrap();
    let account = got[0].actor.account_id.as_deref().unwrap();
    assert_eq!(
        row(&h, "SELECT kind FROM accounts WHERE id=?", &[account])[0],
        "personal"
    );
    let followup =
        h.ch.on_inbound(
            WS,
            &h.me,
            msg(&h, "D_LI", "l2", "u_new", "now speaking myself", 1),
        )
        .await
        .unwrap();
    assert_ne!(
        followup.next,
        steve_channels::Next::Record(steve_channels::inbound::LABEL_BOT)
    );
    assert_eq!(stop(&h, &source.message_id), None);
}

/// A durable event landed through another worker, without its proactive supersede pass. This
/// isolates the last-moment approval/retry gates from event-time card closure.
fn stored_elsewhere(h: &H, draft: &str) {
    h.env.store().with_conn(|c| {
        c.execute("INSERT INTO chan_message_reactions (workspace_id,connection_id,message_external_id,message_id,
            actor_account_id,emoji,origin,removed,action_at)
            SELECT ?1,?2,src.external_id,src.id,?3,'👍','manual',0,?4 FROM chan_messages src JOIN chan_messages out
              ON src.space_id=out.space_id AND src.external_id=out.reply_to WHERE out.id=?5",
            rusqlite::params![WS,h.me,h.my_account,h.ch.now()+1,draft])?;
        Ok(())
    }).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_chats_and_unscoped_unknown_targets_do_not_collect_reactions() {
    let h = setup().await;
    let source =
        h.ch.on_inbound(
            WS,
            &h.me,
            msg(&h, "D_LI", "seed", "ulilei", "known chat", 0),
        )
        .await
        .unwrap();
    h.env
        .store()
        .transaction(|tx| {
            spaces::write_attention(
                tx,
                WS,
                &source.space_id,
                spaces::Attention::Blocked,
                h.ch.now(),
            )
        })
        .unwrap();
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "not-stored", "upx"))
        .unwrap();
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "seed", "upx"))
        .unwrap();
    let mut unscoped = reaction(&h, "D_LI", "unknown", "upx");
    unscoped.space_ext_id = None;
    h.ch.on_reaction(WS, &h.me, unscoped).unwrap();
    assert_eq!(
        count(&h, "SELECT count(*) FROM chan_message_reactions", &[]),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn yolo_rechecks_durable_reactions_at_approval_and_failure_retry() {
    for retry in [false, true] {
        let h = setup().await;
        let (draft, mut card) = drafted(&h, "l1").await;
        if retry {
            yolo(&h, &card, "yes").unwrap();
            *h.fake.fail_send.lock().unwrap() = Some("isolated failure".into());
            h.env.clock.advance(11_000);
            h.outbox.send_due(&h.ch).await;
            assert_eq!(status(&h, &draft), "failed");
            card = row(
                &h,
                "SELECT decision_id FROM chan_messages WHERE id=?",
                &[&draft],
            )[0]
            .as_str()
            .unwrap()
            .into();
        }
        stored_elsewhere(&h, &draft);
        let d = yolo(&h, &card, if retry { "retry" } else { "yes" }).unwrap();
        assert_eq!(d.status, "expired");
        assert_eq!(d.result["superseded"], presence::BY_REACTION);
        assert_eq!(status(&h, &draft), "discarded");
        assert_eq!(h.outbox.send_due(&h.ch).await, 0);
        assert!(h.fake.sent.lock().unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_human_approval_keeps_its_existing_precedence() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    you_decide(&h, &card, "yes").unwrap();
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(status(&h, &draft), "sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_reply_and_handoff_persistence_recheck_reactions_in_the_write_transaction() {
    use steve_channels::Outbound;
    use steve_channels::deliver::{Auth, Delivered, OutRequest, OutTarget, Purpose};
    let h = setup().await;
    let got =
        h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请确认", 0))
            .await
            .unwrap();
    let plan = h
        .env
        .store()
        .reader(|c| steve_channels::flow_actions::plan_of(c, WS, &got.message_id))
        .unwrap();
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    let delivery = h
        .outbox
        .deliver(
            &h.ch,
            &OutRequest {
                target: OutTarget::reply(&plan),
                text: "延迟返回的生成结果".into(),
                purpose: Purpose::Reply,
                approve: true,
                generation: Some(Default::default()),
                auth: Auth::None,
                operation_id: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(delivery, Delivered::Skipped));
    assert_eq!(drafts(&h), 0);
    let d = h
        .env
        .store()
        .transaction(|tx| steve_channels::respond::open_handoff(tx, &plan))
        .unwrap();
    assert_eq!(decision(&h, &d).0, "expired");
    assert_eq!(decision(&h, &d).1["superseded"], presence::BY_REACTION);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type='chan_message' AND status='open'",
            &[]
        ),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_reaction_closes_an_own_account_handoff_and_respects_identity_visibility() {
    let h = setup().await;
    let seed =
        h.ch.on_inbound(
            WS,
            &h.me,
            msg(&h, "D_LI", "seed", "ulilei", "earlier", -120),
        )
        .await
        .unwrap();
    handoff_here(&h, &seed.space_id);
    let got = push(&h, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请本人确认", 0)).await;
    let card = got.response.unwrap().decision_id.unwrap();
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    assert_eq!(decision(&h, &card).0, "expired");
    assert_eq!(decision(&h, &card).1["superseded"], presence::BY_REACTION);
    let mask = h
        .env
        .store()
        .reader(|c| steve_channels::spaces::bit(c, &got.space_id, &h.me))
        .unwrap();
    assert_eq!(
        h.env
            .store()
            .reader(|c| steve_channels::incoming_reactions::list(c, &got.message_id, mask))
            .unwrap()
            .len(),
        1
    );
    assert!(
        h.env
            .store()
            .reader(|c| steve_channels::incoming_reactions::list(c, &got.message_id, 0))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_one_emoji_does_not_remove_another_and_text_answers_still_count() {
    let h = setup().await;
    let got =
        h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", "l1", "ulilei", "请确认", 0))
            .await
            .unwrap();
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    ev.emoji = "✅".into();
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    ev.removed = true;
    ev.at += 1;
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    assert!(stop(&h, &got.message_id).is_some());
    you_said_elsewhere(&h, "D_LI", 1);
    ev.emoji = "👍".into();
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    assert!(matches!(
        stop(&h, &got.message_id),
        Some(presence::Stop::You {
            engaged: presence::Engaged::Replied { .. }
        })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_before_message_stops_before_any_composer_and_is_read_back() {
    let h = setup().await;
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    let got = push(&h, &h.me, msg(&h, "D_LI", "l1", "ulilei", "今晚能看吗", 0)).await;
    assert!(h.model.prompts.lock().unwrap().is_empty());
    assert_eq!(drafts(&h), 0);
    assert_eq!(got.response.unwrap().label, presence::LABEL_REACTION);
    let r = h
        .env
        .store()
        .reader(|c| steve_channels::messages::get(c, &got.message_id))
        .unwrap()
        .unwrap();
    assert_eq!(r.reactions.len(), 1);
    assert_eq!(r.reactions[0].origin, "manual");
    assert_eq!(
        r.reactions[0].actor.account_id.as_deref(),
        Some(h.my_account.as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_while_composing_drops_the_generated_draft() {
    let h = setup().await;
    let (ch, conn, event) = (
        h.ch.clone(),
        h.me.clone(),
        reaction(&h, "D_LI", "l1", "upx"),
    );
    *h.model.during.lock().unwrap() =
        Some(Box::new(move || ch.on_reaction(WS, &conn, event).unwrap()));
    let got = push(&h, &h.me, msg(&h, "D_LI", "l1", "ulilei", "今晚能看吗", 0)).await;
    assert_eq!(h.model.prompts.lock().unwrap().len(), 1);
    assert_eq!(got.response.unwrap().label, presence::LABEL_REACTION);
    assert_eq!(drafts(&h), 0);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type='chan_message' AND status='open'",
            &[]
        ),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reacting_to_the_last_group_message_while_composing_does_not_close_earlier_cards() {
    let h = setup().await;
    let first = push(
        &h,
        &h.me,
        at_you(msg(&h, "C_DEV", "early", "ulilei", "第一件事请确认", 0)),
    )
    .await;
    checked(&h).await;
    let first_card = first.response.unwrap().decision_id.unwrap();
    h.env.clock.advance(10_000);
    let (ch, conn, event) = (
        h.ch.clone(),
        h.me.clone(),
        reaction(&h, "C_DEV", "last", "upx"),
    );
    *h.model.during.lock().unwrap() =
        Some(Box::new(move || ch.on_reaction(WS, &conn, event).unwrap()));
    let last = push(
        &h,
        &h.me,
        at_you(msg(&h, "C_DEV", "last", "ulilei", "第二件事请确认", 0)),
    )
    .await;
    assert_eq!(last.response.unwrap().label, presence::LABEL_REACTION);
    assert_eq!(decision(&h, &first_card).0, "open");
    assert_eq!(stop(&h, &first.message_id), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_after_draft_closes_it_and_yolo_cannot_send() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    assert_eq!(status(&h, &draft), "discarded");
    let (s, result) = decision(&h, &card);
    assert_eq!(s, "expired");
    assert_eq!(result["superseded"], presence::BY_REACTION);
    assert!(yolo(&h, &card, "yes").is_err());
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_yolo_approved_pending_reply_is_stopped_at_send() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    yolo(&h, &card, "yes").unwrap();
    assert_eq!(status(&h, &draft), "pending");
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "l1", "upx"))
        .unwrap();
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert_eq!(status(&h, &draft), "discarded");
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_on_an_old_message_never_silences_a_new_dm_or_group_message() {
    for space in ["D_LI", "C_DEV"] {
        let h = setup().await;
        let old =
            h.ch.on_inbound(WS, &h.me, msg(&h, space, "old", "ulilei", "第一件事", 0))
                .await
                .unwrap();
        h.ch.on_reaction(WS, &h.me, reaction(&h, space, "old", "upx"))
            .unwrap();
        h.env.clock.advance(10_000);
        let new = push(
            &h,
            &h.me,
            at_you(msg(&h, space, "new", "ulilei", "另一件事请确认", 0)),
        )
        .await;
        checked(&h).await;
        assert!(stop(&h, &old.message_id).is_some());
        assert_eq!(stop(&h, &new.message_id), None);
        assert!(new.response.unwrap().out_message_id.is_some());
        assert_eq!(drafts(&h), 1);
        assert_eq!(
            row(
                &h,
                "SELECT turn_of FROM chan_messages WHERE id=?",
                &[&old.message_id]
            ),
            vec![Value::Null]
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn equal_message_timestamps_do_not_let_an_earlier_reaction_handle_later_arrivals() {
    for space in ["D_LI", "C_DEV"] {
        for reaction_first in [false, true] {
            let h = setup().await;
            let old =
                h.ch.on_inbound(WS, &h.me, msg(&h, space, "old", "ulilei", "第一件事", 0))
                    .await
                    .unwrap();
            if reaction_first {
                h.ch.on_reaction(WS, &h.me, reaction(&h, space, "old", "upx"))
                    .unwrap();
            }
            let new = push(
                &h,
                &h.me,
                at_you(msg(&h, space, "new", "ulilei", "第二件事", 0)),
            )
            .await;
            let response = new.response.unwrap();
            let draft = response.out_message_id.expect("later message still drafts");
            let card = response.decision_id.unwrap();
            if !reaction_first {
                h.ch.on_reaction(WS, &h.me, reaction(&h, space, "old", "upx"))
                    .unwrap();
            }
            assert!(stop(&h, &old.message_id).is_some());
            assert_eq!(
                stop(&h, &new.message_id),
                None,
                "{space}, reaction_first={reaction_first}"
            );
            assert_eq!(status(&h, &draft), "draft");
            assert_eq!(decision(&h, &card).0, "open");
            let turn = h
                .env
                .store()
                .reader(|c| presence::turn(c, WS, &old.message_id))
                .unwrap();
            assert_eq!(turn, vec![old.message_id]);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn equal_timestamp_dm_bursts_stop_at_the_nearest_other_sender() {
    let h = setup().await;
    let mut ids = Vec::new();
    for (id, sender) in [
        ("unrelated", "ulilei"),
        ("boundary", "uhan"),
        ("early", "ulilei"),
        ("reacted", "ulilei"),
        ("later", "ulilei"),
    ] {
        ids.push(
            h.ch.on_inbound(WS, &h.me, msg(&h, "D_LI", id, sender, id, 0))
                .await
                .unwrap()
                .message_id,
        );
    }
    h.ch.on_reaction(WS, &h.me, reaction(&h, "D_LI", "reacted", "upx"))
        .unwrap();
    assert_eq!(stop(&h, &ids[0]), None);
    assert_eq!(stop(&h, &ids[1]), None);
    assert!(stop(&h, &ids[2]).is_some());
    assert!(stop(&h, &ids[3]).is_some());
    assert_eq!(stop(&h, &ids[4]), None);
    let turn = h
        .env
        .store()
        .reader(|c| presence::turn(c, WS, &ids[3]))
        .unwrap();
    assert_eq!(turn, ids[2..4]);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_reacted_message_in_groups_but_earlier_unanswered_burst_in_dms() {
    for (space, prior_handled) in [("D_LI", true), ("C_DEV", false)] {
        let h = setup().await;
        let mut ids = Vec::new();
        for (id, sender, secs) in [
            ("separate", "ulilei", -100),
            ("other", "uhan", -10),
            ("early", "ulilei", 0),
            ("reacted", "ulilei", 10),
            ("later", "ulilei", 20),
        ] {
            ids.push(
                h.ch.on_inbound(WS, &h.me, msg(&h, space, id, sender, id, secs))
                    .await
                    .unwrap()
                    .message_id,
            );
        }
        h.ch.on_reaction(WS, &h.me, reaction(&h, space, "reacted", "upx"))
            .unwrap();
        assert_eq!(stop(&h, &ids[0]), None, "outside burst");
        assert_eq!(stop(&h, &ids[1]), None, "another sender");
        assert_eq!(stop(&h, &ids[2]).is_some(), prior_handled);
        assert!(matches!(
            stop(&h, &ids[3]),
            Some(presence::Stop::You {
                engaged: presence::Engaged::Reacted { .. }
            })
        ));
        assert_eq!(stop(&h, &ids[4]), None, "later message");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn others_automation_and_unknown_are_only_composer_context() {
    for (from, origin) in [
        ("uhan", ReactionOrigin::Manual),
        ("upx", ReactionOrigin::Automated),
        ("upx", ReactionOrigin::Unknown),
    ] {
        let h = setup().await;
        let mut ev = reaction(&h, "D_LI", "l1", from);
        ev.origin = origin;
        h.ch.on_reaction(WS, &h.me, ev).unwrap();
        let got = push(&h, &h.me, msg(&h, "D_LI", "l1", "ulilei", "今晚能看吗", 0)).await;
        assert_eq!(stop(&h, &got.message_id), None);
        assert!(got.response.unwrap().out_message_id.is_some());
        let prompts = h.model.prompts.lock().unwrap();
        assert!(prompts.iter().any(|p| p.contains("对这条消息回应了 👍")));
        if origin != ReactionOrigin::Manual {
            assert!(prompts.iter().any(|p| p.contains("仅作上下文")));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn removal_tombstones_survive_stale_creates_and_do_not_reopen_expired_cards() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    let original = reaction(&h, "D_LI", "l1", "upx");
    h.ch.on_reaction(WS, &h.me, original.clone()).unwrap();
    let mut deleted = original.clone();
    deleted.removed = true;
    deleted.at += 1;
    h.ch.on_reaction(WS, &h.me, deleted.clone()).unwrap();
    h.ch.on_reaction(WS, &h.me, original.clone()).unwrap();
    let source = row(
        &h,
        "SELECT id FROM chan_messages WHERE external_id='l1'",
        &[],
    )[0]
    .as_str()
    .unwrap()
    .to_string();
    assert_eq!(stop(&h, &source), None);
    assert_eq!(status(&h, &draft), "discarded");
    assert_eq!(decision(&h, &card).0, "expired");
    assert!(
        h.env
            .store()
            .reader(|c| steve_channels::incoming_reactions::list(c, &source, -1))
            .unwrap()
            .is_empty()
    );
    // Same-time delete also wins, including after a connection replay / process restart.
    let mut readd = original;
    readd.at = deleted.at;
    h.ch.on_reaction(WS, &h.me, readd.clone()).unwrap();
    assert_eq!(stop(&h, &source), None);
    readd.at += 1;
    h.ch.on_reaction(WS, &h.me, readd).unwrap();
    assert!(stop(&h, &source).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reaction_removed_before_the_message_arrives_does_not_suppress_it() {
    let h = setup().await;
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    ev.removed = true;
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    ev.removed = false;
    ev.at -= 1;
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    let got = push(&h, &h.me, msg(&h, "D_LI", "l1", "ulilei", "新问题", 0)).await;
    assert_eq!(stop(&h, &got.message_id), None);
    assert!(got.response.unwrap().out_message_id.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_reaction_never_resends_an_already_sent_reply() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    you_decide(&h, &card, "yes").unwrap();
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    let mut ev = reaction(&h, "D_LI", "l1", "upx");
    h.ch.on_reaction(WS, &h.me, ev.clone()).unwrap();
    ev.removed = true;
    ev.at += 1;
    h.ch.on_reaction(WS, &h.me, ev).unwrap();
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert_eq!(status(&h, &draft), "sent");
    assert_eq!(h.fake.sent.lock().unwrap().len(), 1);
    let duplicate = push(
        &h,
        &h.me,
        msg(&h, "D_LI", "l1", "ulilei", "今晚能看下吗", 0),
    )
    .await;
    assert!(duplicate.duplicate);
    assert_eq!(h.fake.sent.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn own_reactions_do_not_close_a_bots_handoff() {
    let h = setup().await;
    let seed =
        h.ch.on_inbound(
            WS,
            &h.oncall,
            msg(&h, "C_BOT", "seed", "ulilei", "earlier", -120),
        )
        .await
        .unwrap();
    handoff_here(&h, &seed.space_id);
    let got = push(
        &h,
        &h.oncall,
        at_you(msg(&h, "C_BOT", "b1", "ulilei", "要老板确认", 0)),
    )
    .await;
    let card = got.response.unwrap().decision_id.unwrap();
    h.ch.on_reaction(WS, &h.oncall, reaction(&h, "C_BOT", "b1", "upx"))
        .unwrap();
    assert_eq!(decision(&h, &card).0, "open");
}
