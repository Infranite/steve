use serde_json::{Value, json};
use steve_channels::deliver::{Auth, OutRequest, OutTarget, Outbound, Purpose};
use steve_channels::{By, Mention, arbiter, conversations};
use steve_modules::testkit::{ME, WS};

#[path = "../examples/support/personal_triggers.rs"]
mod support;
use support::Fixture;

fn trigger(i: &steve_channels::Inbound) -> bool {
    matches!(i.response.as_ref().map(|r| &r.by), Some(By::Trigger { .. }))
}
fn handling(f: &Fixture, i: &steve_channels::Inbound) -> Value {
    f.record(&i.space_id)["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == i.message_id)
        .unwrap()["handling"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thread_does_not_borrow_another_receiving_connections_command() {
    let f = Fixture::new().await;
    f.configure(&f.second, json!({"on":true,"words":["/bot"],"who":["me"]}));
    let root = f
        .receive(
            &f.me,
            f.message("scope-root", "owner-one", "/bot start here"),
        )
        .await;
    assert!(trigger(&root));
    let copy = f
        .receive(
            &f.second,
            f.message("scope-root", "owner-one", "/bot start here"),
        )
        .await;
    assert!(!trigger(&copy));
    let mut next = f.message(
        "scope-follow",
        "owner-two",
        "a follow-up through another account",
    );
    next.thread_key = Some("scope-root".into());
    assert!(!trigger(&f.receive(&f.second, next).await));
    assert_eq!(f.sends(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn receiving_connection_and_its_author_define_personal_authority() {
    let f = Fixture::new().await;
    // A later setting of another personal connection must not replace the receiver's setting.
    f.configure(&f.second, json!({"on":false,"words":["/else"]}));
    f.configure(&f.other, json!({"on":true,"words":["/other"],"who":["*"]}));
    let a = f
        .receive(&f.me, f.message("a", "owner-one", "/bot answer"))
        .await;
    assert!(trigger(&a));
    assert_eq!(f.sends(), 1);
    assert_ne!(
        handling(&f, &a)["label"],
        steve_channels::inbound::LABEL_OWN
    );
    for (id, conn, from) in [
        ("b", &f.second, "owner-two"),
        ("c", &f.me, "other-member"),
        ("d", &f.bot, "owner-one"),
    ] {
        assert!(!trigger(
            &f.receive(conn, f.message(id, from, "/bot answer")).await
        ));
    }
    assert_eq!(f.sends(), 1);
    // Adapter-confirmed own aliases work at both admission and send time, without merging people.
    let a = f
        .receive(
            &f.me,
            f.message("alias", "new-provider-alias", "/bot alias")
                .mine(true),
        )
        .await;
    assert!(trigger(&a));
    assert_eq!(handling(&f, &a)["state"], "answered");
    assert_eq!(f.sends(), 2);
    // Even a bot claiming mine cannot acquire the personal trigger privilege, with collab enabled.
    f.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE space_settings SET collab=1 WHERE space_id=?",
                [&a.space_id],
            )?;
            Ok(())
        })
        .unwrap();
    let mut bot = f
        .message("bot-author", "external-bot", "/bot loop")
        .mine(true);
    bot.sender.is_bot = true;
    assert!(!trigger(&f.receive(&f.me, bot).await));
    assert_eq!(f.sends(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_filter_covers_mentions_and_thread_followups_for_every_allowed_author() {
    let f = Fixture::new().await;
    f.configure(
        &f.me,
        json!({"on":true,"words":["/bot"],"who":["*"],"ignore_prefixes":["AUTO:","机器人："]}),
    );
    let root = f
        .receive(&f.me, f.message("root", "owner-one", "/bot start"))
        .await;
    assert!(trigger(&root));
    for (n, text) in [
        "[AI] /bot loop",
        "AUTO: /bot loop",
        "机器人：自动消息",
        "🤖 AI 代答，仅供参考",
        "好的（由 Steve 代发）",
    ]
    .iter()
    .enumerate()
    {
        for (which, from) in ["owner-one", "human"].iter().enumerate() {
            let mut m = f.message(
                &format!("auto-{n}-{which}"),
                from,
                &format!("@Fixture Person {text}"),
            );
            m.mentions.push(Mention {
                handle: None,
                name: "Fixture Person".into(),
                is_connection: true,
            });
            m.thread_key = Some("root".into());
            let a = f.receive(&f.me, m).await;
            assert!(!trigger(&a));
            assert_eq!(handling(&f, &a)["state"], "recorded");
            assert_eq!(handling(&f, &a)["label"], arbiter::LABEL_AUTOMATED);
        }
    }
    assert_eq!(f.sends(), 1);
    let mut follow = f.message("follow", "human", "a real follow-up");
    follow.thread_key = Some("root".into());
    assert!(trigger(&f.receive(&f.me, follow).await));
    assert_eq!(f.sends(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_first_duplicates_are_marked_once_but_history_never_replays() {
    let f = Fixture::new().await;
    let b = f
        .receive(&f.bot, f.message("shared", "owner-one", "/bot answer"))
        .await;
    assert!(!trigger(&b));
    let a = f
        .receive(&f.me, f.message("shared", "owner-one", "/bot answer"))
        .await;
    assert!(a.duplicate && trigger(&a));
    assert_eq!(a.message_id, b.message_id);
    assert_eq!(handling(&f, &a)["state"], "answered");
    assert!(!trigger(
        &f.receive(&f.me, f.message("shared", "owner-one", "/bot answer"))
            .await
    ));
    assert_eq!(f.sends(), 1);
    let mut old = f.message("old", "owner-one", "/bot old request");
    old.created = 1;
    let h = f.receive(&f.bot, old.clone()).await;
    let d = f.receive(&f.me, old).await;
    assert!(h.history.is_some());
    assert!(!trigger(&d));
    assert_eq!(f.sends(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_outbound_rows_override_generation_and_reactions_in_both_read_apis() {
    for (scenario, expected) in [
        ("sent", "answered"),
        ("draft", "drafted"),
        ("blocked", "blocked"),
        ("failed", "failed"),
        ("unknown", "unknown"),
    ] {
        let f = Fixture::new().await;
        match scenario {
            "draft" => f.reply(true),
            "blocked" => {
                *f.fake.block_target_send.lock().unwrap() =
                    Some("target_forbidden: fixture policy".into())
            }
            "failed" => *f.fake.fail_send.lock().unwrap() = Some("fixture transport failed".into()),
            "unknown" => *f.fake.lose_answer.lock().unwrap() = true,
            _ => {}
        }
        let a = f
            .receive(&f.me, f.message("result", "owner-one", "/bot answer"))
            .await;
        let out = a
            .response
            .as_ref()
            .unwrap()
            .out_message_id
            .as_ref()
            .unwrap();
        // Remove reaction's answer link: the conversation's durable outbound association suffices.
        f.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE chan_reactions SET state='done',answer_id=NULL WHERE message_id=?",
                    [&a.message_id],
                )?;
                Ok(())
            })
            .unwrap();
        let h = handling(&f, &a);
        assert_eq!(h["state"], expected, "{scenario}: {h}");
        assert_eq!(h["out_message_id"], *out);
        let msgs = conversations::messages_of(
            &f.ch,
            WS,
            &steve_store::base::Viewer::bare(WS, ME),
            &conversations::conv_space(&a.space_id),
        )
        .unwrap();
        let reply = msgs["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["meta"]["chan_out"]["message"] == *out)
            .unwrap();
        assert_eq!(
            reply["meta"]["chan_out"]["status"], scenario,
            "{scenario}: {reply}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_time_rechecks_receiver_configuration_and_requires_its_receipt() {
    let f = Fixture::new().await;
    let a =
        f.ch.on_inbound(WS, &f.me, f.message("held", "owner-one", "/bot answer"))
            .await
            .unwrap();
    f.configure(
        &f.me,
        json!({"on":true,"words":["/bot"],"ignore_prefixes":["/bot"]}),
    );
    let plan = f
        .env
        .store()
        .reader(|c| steve_channels::flow_actions::plan_of(c, WS, &a.message_id))
        .unwrap();
    let d = f
        .out
        .deliver(
            &f.ch,
            &OutRequest {
                target: OutTarget::reply(&plan),
                text: "Synthetic late answer.".into(),
                purpose: Purpose::Trigger,
                approve: false,
                generation: Some(Default::default()),
                auth: Auth::Trigger {
                    source: a.message_id,
                },
                operation_id: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        d,
        steve_channels::deliver::Delivered::Drafted { .. }
    ));
    assert_eq!(f.sends(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_becomes_sent_only_after_human_approval_and_the_undo_window() {
    let f = Fixture::new().await;
    f.reply(true);
    let a = f
        .receive(&f.me, f.message("approve", "owner-one", "/bot draft this"))
        .await;
    assert_eq!(handling(&f, &a)["state"], "drafted");
    assert_eq!(f.sends(), 0);
    let decision = a.response.as_ref().unwrap().decision_id.as_ref().unwrap();
    // Disclosure is asynchronous on drafts. Wait for its actual stored verdict before approval.
    steve_runtime::testkit::eventually("the personal draft disclosure verdict is stored", || async {
        let checking=f.env.store().reader(|c| -> steve_modules::ApiResult<bool> {
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM chan_messages m JOIN reply_basis b ON b.id=m.basis_id WHERE m.decision_id=? AND b.verdict='checking')",[decision],|r|r.get(0))?)
        }).unwrap();
        if !checking { true } else { f.out.send_due(&f.ch).await; false }
    }).await;
    steve_modules::decisions::dispatch(
        &f.env.modules,
        &steve_modules::testkit::caller(WS, ME),
        decision,
        "yes",
        "",
    )
    .unwrap()
    .unwrap();
    assert_eq!(handling(&f, &a)["state"], "working");
    assert_eq!(f.sends(), 0);
    f.env.clock.advance(60_000);
    f.out.send_due(&f.ch).await;
    assert_eq!(handling(&f, &a)["state"], "answered");
    assert_eq!(f.sends(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_generation_without_an_outbound_receipt_is_unconfirmed() {
    let f = Fixture::new().await;
    let a = f
        .receive(&f.me, f.message("legacy", "owner-one", "ordinary words"))
        .await;
    let conv = conversations::conv_space(&a.space_id);
    let msgs = f.env.store().messages();
    let origin = msgs
        .add(
            WS,
            "me",
            "ordinary words",
            None,
            &json!({"chan":{"message":a.message_id}}),
            &conv,
        )
        .unwrap();
    let reply = msgs
        .add(
            WS,
            "steve",
            "old generated answer",
            None,
            &json!({"origin":origin,"chan_out":{"message":null,"status":"sent"}}),
            &conv,
        )
        .unwrap();
    let read =
        conversations::messages_of(&f.ch, WS, &steve_store::base::Viewer::bare(WS, ME), &conv)
            .unwrap();
    let answer = read["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == reply)
        .unwrap();
    assert_eq!(answer["meta"]["chan_out"]["status"], "unknown");
    assert_ne!(handling(&f, &a)["state"], "answered");
    assert_eq!(f.sends(), 0);
}

#[path = "../../steve-store-sqlite/tests/digest_audit.rs"]
mod digest_audit;
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            digest_audit::check(self.env.store());
        }
    }
}
