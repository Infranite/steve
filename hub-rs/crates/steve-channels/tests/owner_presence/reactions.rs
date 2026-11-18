use super::*;
use steve_channels::{ReactionEvent, ReactionOrigin};

fn reaction(h: &H, space: &str, target: &str, from: &str) -> ReactionEvent {
    ReactionEvent {
        space_ext_id: Some(space.into()),
        message_ext_id: Some(target.into()),
        message_source_id: None,
        actor: FakeChannel::handle(from),
        actor_name: None,
        origin: ReactionOrigin::Manual,
        emoji: "👍".into(),
        at: h.ch.now() + 30_000,
        removed: false,
    }
}

fn stop(h: &H, message: &str) -> Option<presence::Stop> {
    h.env
        .store()
        .reader(|c| presence::stop(c, WS, message, h.ch.now()))
        .unwrap()
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
                generation: None,
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
