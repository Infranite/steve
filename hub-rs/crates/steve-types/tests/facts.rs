use steve_types::*;
use strum::EnumCount;

#[test]
fn durable_topics_keep_snapshot_fields_and_roundtrip() {
    let raw = || RawJson::from_str(r#"{"text": "snapshot", "n":1e+02}"#).unwrap();
    let alarm = || AlarmFired {
        owner: "subject-owner".into(),
        kind: "todo".into(),
        alarms: vec!["alarm-1".into()],
        lines: vec![AlarmLine {
            subject: "todo-1".into(),
            text: "due".into(),
            detail: None,
            link: Some("#/todos/todo-1".into()),
            planned_for: Some(Millis(7)),
        }],
        late: false,
        at: Some(Millis(10)),
    };
    let facts = [
        (
            "work.item.lifecycle",
            DurableFact::WorkItemLifecycle {
                item_id: "item-1".into(),
                cycle: 1,
                revision: 2,
                transition: "closed".into(),
                phase: "closed".into(),
                outcome: Some("done".into()),
                acceptance: None,
                reason: "accepted".into(),
            },
        ),
        (
            "work.item.done",
            DurableFact::WorkItemDone {
                item_id: "item-1".into(),
                title: "title".into(),
                status: "done".into(),
                bot_id: None,
                project_id: None,
                source: raw(),
                cycle: None,
                revision: None,
                acceptance: None,
            },
        ),
        (
            "mail.event.ready",
            DurableFact::MailEventReady {
                message_id: "mail-message-1".into(),
                connection_id: "connection-1".into(),
                event: raw(),
                how: MailDelivery::Fixed {
                    outcome: "ignored".into(),
                    label: "muted".into(),
                },
            },
        ),
        (
            "github.after",
            DurableFact::GithubAfter {
                connection: "connection-1".into(),
                signals: vec![raw()],
                supplements: vec![PrSupplement {
                    item: "item-1".into(),
                    external_id: "comment:1".into(),
                    text: "comment".into(),
                }],
            },
        ),
        (
            "kb.object.changed",
            DurableFact::KnowledgeObjectChanged {
                obj_type: "doc".into(),
                obj_id: "doc-1".into(),
            },
        ),
        ("todo.due", DurableFact::TodoDue(alarm())),
        ("snooze.wake", DurableFact::SnoozeWake(alarm())),
        ("briefing.ready", DurableFact::BriefingReady(alarm())),
    ];
    assert_eq!(facts.len(), DurableFact::COUNT);
    for (topic, fact) in facts {
        let bytes = serde_json::to_string(&fact).unwrap();
        assert!(bytes.starts_with(&format!(r#"{{"topic":"{topic}","payload":"#)));
        assert_eq!(serde_json::from_str::<DurableFact>(&bytes).unwrap(), fact);
        // Database JSON may put payload before topic; raw snapshots must survive either key order.
        let payload =
            &bytes[bytes.find(",\"payload\":").unwrap() + ",\"payload\":".len()..bytes.len() - 1];
        let reversed = format!(r#"{{"payload":{payload},"topic":"{topic}"}}"#);
        assert_eq!(
            serde_json::from_str::<DurableFact>(&reversed).unwrap(),
            fact
        );

        if bytes.contains("snapshot") {
            assert!(bytes.contains(r#"{"text": "snapshot", "n":1e+02}"#));
        }
    }
    // Old facts omitted these fields; retain defaults rather than inventing current entity data.
    let old = r#"{"topic":"github.after","payload":{"connection":"c","signals":[]}}"#;
    assert!(
        matches!(serde_json::from_str::<DurableFact>(old).unwrap(),DurableFact::GithubAfter { supplements,.. } if supplements.is_empty())
    );
    let old = r#"{"topic":"work.item.done","payload":{"item_id":"i","title":"t","status":"done","bot_id":null,"project_id":null,"source":[]}}"#;
    assert!(matches!(
        serde_json::from_str::<DurableFact>(old).unwrap(),
        DurableFact::WorkItemDone {
            cycle: None,
            revision: None,
            acceptance: None,
            ..
        }
    ));
}
