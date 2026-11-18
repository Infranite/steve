use serde::{Deserialize, Serialize};
use steve_types::*;

#[test]
fn ids_are_transparent_labels() {
    macro_rules! check_ids {
        ($($id:ty),+ $(,)?) => { $(
            let id = <$id>::from("legacy-any-prefix");
            assert_eq!(id.as_str(), "legacy-any-prefix");
            assert_eq!(id.to_string(), "legacy-any-prefix");
            assert_eq!(serde_json::to_string(&id).unwrap(), r#""legacy-any-prefix""#);
            assert_eq!(serde_json::from_str::<$id>(r#""legacy-any-prefix""#).unwrap(), id);
            assert_eq!(<$id>::from(String::from("legacy-any-prefix")), id);
        )+ };
    }
    check_ids!(
        WorkspaceId,
        UserId,
        ItemId,
        SessionId,
        DecisionId,
        MessageId,
        ThreadRunId,
        BotId,
        ProjectId,
        TagId,
        DeviceId,
        NodeId,
        ClientId,
        PartyId,
        MemoryId,
        ChangesetId,
        AccountId,
        ConnectionId,
        VaultItemId,
        SignalId,
        FlowId,
        FlowRunId,
        SpaceId,
        ChanMessageId,
        PersonaId,
        CalendarId,
        YoloActionId,
        KbSourceId,
        KbDocId,
        MailThreadId,
        NotificationId,
        Epoch
    );
    assert_eq!(serde_json::to_string(&Rev(47)).unwrap(), "47");
    assert_eq!(serde_json::to_string(&Millis(-1)).unwrap(), "-1");
    assert_eq!(serde_json::to_string(&Seq(-2)).unwrap(), "-2");
}

#[test]
fn code_table_matches_the_contract() {
    use Code::*;
    use Retry::{Later, Never, Now};
    let table = [
        (Invalid, "invalid", 400, Never),
        (Unauthenticated, "unauthenticated", 401, Never),
        (Forbidden, "forbidden", 403, Never),
        (NotFound, "not_found", 404, Never),
        (Conflict, "conflict", 409, Never),
        (Stale, "stale", 409, Never),
        (Gap, "gap", 409, Never),
        (TooLarge, "too_large", 413, Never),
        (NotConfigured, "not_configured", 409, Never),
        (Unsupported, "unsupported", 501, Never),
        (Busy, "busy", 409, Later { after_ms: None }),
        (RateLimited, "rate_limited", 429, Later { after_ms: None }),
        (Exhausted, "exhausted", 507, Later { after_ms: None }),
        (Unavailable, "unavailable", 503, Later { after_ms: None }),
        (Timeout, "timeout", 504, Later { after_ms: None }),
        (OutcomeUnknown, "outcome_unknown", 502, Now),
        (Incompatible, "incompatible", 500, Never),
        (Internal, "internal", 500, Never),
    ];
    for (code, wire, status, retry) in table {
        assert_eq!(code.http_status(), status, "{wire}");
        assert_eq!(code.default_retry(), retry, "{wire}");
        let text = format!("\"{wire}\"");
        assert_eq!(serde_json::to_string(&code).unwrap(), text);
        assert_eq!(serde_json::from_str::<Code>(&text).unwrap(), code);
    }
    assert_eq!(NOW_MAX_ATTEMPTS, 3);
}

#[test]
fn http_errors_keep_the_safe_message_and_all_retry_shapes() {
    let cases = [
        (Retry::Never, r#"{"kind":"never"}"#),
        (
            Retry::Later { after_ms: None },
            r#"{"kind":"later","after_ms":null}"#,
        ),
        (
            Retry::Later {
                after_ms: Some(250),
            },
            r#"{"kind":"later","after_ms":250}"#,
        ),
        (Retry::Now, r#"{"kind":"now"}"#),
    ];
    for (retry, expected_retry) in cases {
        let error = Error {
            code: Code::Invalid,
            retry,
            reason: None,
            params: Params::default(),
            safe_detail: "safe detail".into(),
        };
        let body = error.to_http();
        let expected = format!(
            r#"{{"error":"safe detail","code":"invalid","retry":{expected_retry},"reason":null,"params":{{}}}}"#
        );
        assert_eq!(serde_json::to_string(&body).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<HttpErrorBody>(&expected).unwrap(),
            body
        );
        assert_eq!(error.to_string(), "safe detail");
        let _: &dyn std::error::Error = &error;
    }
    let params = Params(
        [
            ("z".into(), Param::Int(-7)),
            ("a".into(), Param::Str("value".into())),
        ]
        .into(),
    );
    assert_eq!(
        serde_json::to_string(&params).unwrap(),
        r#"{"a":"value","z":-7}"#
    );
    assert_eq!(
        serde_json::from_str::<Params>(r#"{"z":-7,"a":"value"}"#).unwrap(),
        params
    );
}

#[test]
fn raw_json_preserves_text_and_rejects_invalid_input() {
    let text = r#"{"b": [1,  2], "a":1e+02, "escaped":"\u0061"}"#;
    let raw = RawJson::from_str(text).unwrap();
    assert_eq!(serde_json::to_string(&raw).unwrap(), text);
    assert_eq!(raw.clone(), raw);
    assert_eq!(serde_json::from_str::<RawJson>(text).unwrap(), raw);
    assert_ne!(
        RawJson::from_str("[1,2]").unwrap(),
        RawJson::from_str("[1, 2]").unwrap()
    );
    assert!(RawJson::from_str("{broken").is_err());
    assert!(RawJson::from_str("null true").is_err());
    for text in ["null", "true", "42", r#""hello""#, "[]"] {
        assert_eq!(
            serde_json::to_string(&RawJson::from_str(text).unwrap()).unwrap(),
            text
        );
    }
}

#[test]
fn envelope_flattens_an_event_and_preserves_its_scope() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type")]
    enum Example {
        #[serde(rename = "item")]
        Item { id: ItemId, ws: WorkspaceId },
    }
    impl Scoped for Example {
        fn scope(&self) -> Scope {
            match self {
                Self::Item { ws, .. } => Scope::Ws(ws.clone()),
            }
        }
    }
    let envelope = Envelope {
        event: Example::Item {
            id: "itm_1".into(),
            ws: "ws_1".into(),
        },
        rev: Rev(47),
    };
    let text = r#"{"type":"item","id":"itm_1","ws":"ws_1","rev":47}"#;
    assert_eq!(serde_json::to_string(&envelope).unwrap(), text);
    assert_eq!(
        serde_json::from_str::<Envelope<Example>>(text).unwrap(),
        envelope
    );
    assert_eq!(envelope.scope(), Scope::Ws("ws_1".into()));
}

#[test]
fn stream_controls_match_the_existing_sse_bytes() {
    // Golden bytes copied from steve-hub/src/api/stream.rs gap_frame/reset_frame.
    for (control, text, frame) in [
        (
            StreamControl::Gap,
            r#"{"type":"stream.gap"}"#,
            "data: {\"type\":\"stream.gap\"}\n\n",
        ),
        (
            StreamControl::Reset,
            r#"{"type":"stream.reset"}"#,
            "data: {\"type\":\"stream.reset\"}\n\n",
        ),
    ] {
        assert_eq!(serde_json::to_string(&control).unwrap(), text);
        assert_eq!(
            format!("data: {}\n\n", serde_json::to_string(&control).unwrap()).as_bytes(),
            frame.as_bytes()
        );
        assert_eq!(
            serde_json::from_str::<StreamControl>(text).unwrap(),
            control
        );
    }
    assert!(serde_json::from_str::<StreamControl>(r#"{"type":"ping"}"#).is_err());
}
