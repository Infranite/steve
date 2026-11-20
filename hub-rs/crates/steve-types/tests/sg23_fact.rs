//! SG23: durable JSON dispatch, exact raw snapshots, and error contracts.
use serde_json::json;
use steve_types::DurableFact;

#[test]
fn sg23_all_topics_accept_unknown_fields_and_either_key_order() {
    let cases = [
        (
            "work.item.lifecycle",
            r#"{"item_id":"i","cycle":1,"revision":2,"transition":"closed","phase":"closed","reason":"accepted","future":true}"#,
        ),
        (
            "work.item.done",
            r#"{"item_id":"i","title":"title","status":"done","source":[],"future":true}"#,
        ),
        (
            "mail.event.ready",
            r#"{"message_id":"mail-1","connection_id":"c","event":{},"how":{"mode":"ingest"},"future":true}"#,
        ),
        (
            "github.after",
            r#"{"connection":"c","finish":[["i","done"]],"signals":[],"future":true}"#,
        ),
        (
            "kb.object.changed",
            r#"{"obj_type":"doc","obj_id":"d","future":true}"#,
        ),
        (
            "todo.due",
            r#"{"owner":"owner","kind":"todo","alarms":["a"],"lines":[],"late":false,"future":true}"#,
        ),
        (
            "snooze.wake",
            r#"{"owner":"owner","kind":"snooze","alarms":[],"lines":[],"late":true,"future":true}"#,
        ),
        (
            "briefing.ready",
            r#"{"owner":"owner","kind":"brief","alarms":[],"lines":[{"subject":"s","text":"t"}],"late":false,"at":7,"future":true}"#,
        ),
    ];
    let signed = r#"{"topic":"todo.due","payload":{"owner":"o","kind":"todo","alarms":[],"lines":[],"late":false,"at":-1}}"#;
    assert!(
        matches!(serde_json::from_str::<DurableFact>(signed).unwrap(), DurableFact::TodoDue(a) if a.at == Some(steve_types::Millis(-1)))
    );
    for (topic, payload) in cases {
        let mut expected: serde_json::Value = serde_json::from_str(payload).unwrap();
        expected.as_object_mut().unwrap().remove("future");
        match topic {
            "work.item.lifecycle" => {
                expected["outcome"] = json!(null);
                expected["acceptance"] = json!(null);
            }
            "work.item.done" => {
                expected["bot_id"] = json!(null);
                expected["project_id"] = json!(null);
            }
            "github.after" => expected["supplements"] = json!([]),
            _ => {}
        }
        for raw in [
            format!(r#"{{"topic":"{topic}","payload":{payload},"unknown":9}}"#),
            format!(r#"{{"payload":{payload},"unknown":9,"topic":"{topic}"}}"#),
        ] {
            let f: DurableFact = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                serde_json::to_value(&f).unwrap(),
                json!({"topic":topic,"payload":expected}),
                "{raw}"
            );
        }
    }
}

#[test]
fn sg23_opaque_payloads_keep_raw_bytes_and_optional_fields() {
    let raw = r#"{ "n": 1e+02, "s":"\u0061", "nested": [true, null] }"#;
    for (topic, payload) in [
        (
            "work.item.done",
            format!(
                r#"{{"item_id":"i","title":"t","status":"done","source":{raw},"cycle":3,"revision":4,"acceptance":"yes"}}"#
            ),
        ),
        (
            "mail.event.ready",
            format!(
                r#"{{"message_id":"m","connection_id":"c","event":{raw},"how":{{"mode":"fixed","outcome":"ignored","label":"muted"}}}}"#
            ),
        ),
        (
            "github.after",
            format!(
                r#"{{"connection":"c","finish":[],"signals":[{raw}],"supplements":[{{"item":"i","external_id":"comment:1","text":"body"}}]}}"#
            ),
        ),
    ] {
        for input in [
            format!(r#"{{"topic":"{topic}","payload":{payload}}}"#),
            format!(r#"{{"payload":{payload},"topic":"{topic}"}}"#),
        ] {
            let f: DurableFact = serde_json::from_str(&input).unwrap();
            let encoded = serde_json::to_string(&f).unwrap();
            assert!(encoded.contains(raw), "{encoded}");
            assert_eq!(serde_json::from_str::<DurableFact>(&encoded).unwrap(), f);
        }
    }
}

#[test]
fn sg23_decode_errors_keep_their_exact_text() {
    // Exact diagnostics captured on the unchanged implementation.
    let cases = [
        (r#"{}"#, "missing field `topic` at line 1 column 2"),
        (
            r#"{"topic":"future","payload":{}}"#,
            "unknown fact topic: future",
        ),
        (
            r#"{"topic":1,"payload":{}}"#,
            "invalid type: integer `1`, expected a string at line 1 column 10",
        ),
        (
            r#"{"topic":"work.item.done"}"#,
            "missing field `payload` at line 1 column 26",
        ),
        (
            r#"{"topic":"work.item.lifecycle","payload":{}}"#,
            "missing field `item_id` at line 1 column 43",
        ),
        (
            r#"{"topic":"work.item.done","payload":{"item_id":"i","title":"t","status":"done"}}"#,
            "missing field `source` at line 1 column 79",
        ),
        (
            r#"{"topic":"mail.event.ready","payload":{"message_id":"m","connection_id":"c","event":{}}}"#,
            "missing field `how` at line 1 column 87",
        ),
        (
            r#"{"topic":"mail.event.ready","payload":{"message_id":"m","connection_id":"c","event":{},"how":{"mode":"later"}}}"#,
            "unknown variant `later`, expected `ingest` or `fixed` at line 1 column 108",
        ),
        (
            r#"{"topic":"github.after","payload":{"connection":"c","finish":[],"signals":[]},"topic":"todo.due"}"#,
            "duplicate field `topic` at line 1 column 85",
        ),
        (
            r#"{"topic":"github.after","payload":{"connection":"c","finish":[["i"]],"signals":[]}}"#,
            "invalid length 1, expected a tuple of size 2 at line 1 column 67",
        ),
        (
            r#"{"topic":"kb.object.changed","payload":{"obj_type":"doc"}}"#,
            "missing field `obj_id` at line 1 column 57",
        ),
        (
            r#"{"topic":"todo.due","payload":{"owner":"o","kind":"todo","alarms":[],"lines":[],"late":"false"}}"#,
            "invalid type: string \"false\", expected a boolean at line 1 column 94",
        ),
        (
            r#"{"topic":"snooze.wake","payload":null}"#,
            "invalid type: null, expected struct AlarmFired at line 1 column 37",
        ),
        (
            r#"{"topic":"briefing.ready","payload":{"owner":"o","kind":"b","alarms":[],"lines":[],"late":false,"at":"soon"}}"#,
            "invalid type: string \"soon\", expected i64 at line 1 column 107",
        ),
    ];
    for (input, expected) in cases {
        let error = serde_json::from_str::<DurableFact>(input)
            .unwrap_err()
            .to_string();
        assert_eq!(error, expected, "{input}");
    }
}
