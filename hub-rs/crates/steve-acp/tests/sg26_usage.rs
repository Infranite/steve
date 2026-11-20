//! Public sampler settlement with isolated source files, notifications and no test sleeps.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;
use steve_acp::normalize::Fact;
use steve_acp::usage::{Sampler, Source};
use steve_harness_spec::UsageFallback;
use steve_node_api::{Epoch, EventBody, Measurement, Tokens, UsageReport, UsageScope};
const SID: &str = "sg26";
fn stamp() -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    )
    .unwrap()
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn epoch() -> Epoch {
    Epoch::new("sg26-meter").unwrap()
}
fn native(tokens: Tokens, model: Option<&str>, scope: UsageScope) -> Fact {
    Fact::Event(EventBody::Usage {
        report: UsageReport {
            report_id: "before-normalization".into(),
            model: model.map(String::from),
            meter_epoch: epoch(),
            scope,
            measurement: Measurement::Final,
            tokens,
            context_used: None,
            context_limit: None,
            cost: None,
        },
    })
}
fn reports(facts: &[Fact]) -> Vec<&UsageReport> {
    facts
        .iter()
        .filter_map(|f| match f {
            Fact::Event(EventBody::Usage { report }) => Some(report),
            _ => None,
        })
        .collect()
}
fn codes(facts: &[Fact]) -> Vec<&str> {
    facts
        .iter()
        .filter_map(|f| match f {
            Fact::Event(EventBody::Diagnostic { code, .. }) => Some(code.as_str()),
            _ => None,
        })
        .collect()
}
async fn source(format: UsageFallback) -> (tempfile::TempDir, PathBuf, Sampler) {
    let d = tempfile::tempdir().unwrap();
    let p = match format {
        UsageFallback::CodexRollout => d.path().join("sessions/2026/11/21/rollout-test-sg26.jsonl"),
        _ => d.path().join("projects/project/sg26.jsonl"),
    };
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "").unwrap();
    let s = Sampler::start_source(
        Some(Source {
            home: d.path().into(),
            format,
        }),
        SID,
        "sg26-attempt",
        epoch(),
    )
    .await;
    (d, p, s)
}
fn append(p: &Path, v: Value) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    writeln!(f, "{v}").unwrap();
}
fn rollout(input: u64, cache: u64, output: u64, reason: u64) -> Value {
    let c = json!({"input_tokens":input,"cached_input_tokens":cache,"output_tokens":output,"reasoning_output_tokens":reason,"cache_write_input_tokens":0});
    json!({"type":"event_msg","timestamp":stamp(),"payload":{"type":"token_count","info":{"total_token_usage":c,"last_token_usage":c}}})
}
fn transcript(id: &str, model: &str, input: u64, output: u64) -> Value {
    json!({"type":"assistant","sessionId":SID,"timestamp":stamp(),"message":{"id":id,"model":model,"usage":{"input_tokens":input,"output_tokens":output,"cache_read_input_tokens":3,"cache_creation_input_tokens":2,"output_tokens_details":{"thinking_tokens":2}}}})
}
#[tokio::test]
async fn sg26_finish_without_source_keeps_native_zero_unknown_foreign_and_measurement_boundaries() {
    for case in ["unknown", "zero", "foreign", "cumulative", "reasoning"] {
        let mut s = Sampler::start(None, SID, "sg26-attempt", epoch()).await;
        let mut rows = match case {
            "zero" => vec![native(
                Tokens {
                    input: Some(0),
                    output: Some(0),
                    ..Default::default()
                },
                None,
                UsageScope::Main,
            )],
            "foreign" => {
                let mut f = native(
                    Tokens {
                        input: Some(9),
                        ..Default::default()
                    },
                    None,
                    UsageScope::Main,
                );
                if let Fact::Event(EventBody::Usage { report }) = &mut f {
                    report.meter_epoch = Epoch::new("foreign").unwrap();
                    report.report_id = "foreign-id".into();
                }
                vec![f]
            }
            "cumulative" => {
                let mut f = native(
                    Tokens {
                        input: Some(9),
                        ..Default::default()
                    },
                    None,
                    UsageScope::Main,
                );
                if let Fact::Event(EventBody::Usage { report }) = &mut f {
                    report.measurement = Measurement::Cumulative;
                }
                vec![f]
            }
            "reasoning" => vec![native(
                Tokens {
                    output: Some(2),
                    reasoning: Some(9),
                    ..Default::default()
                },
                None,
                UsageScope::Main,
            )],
            _ => vec![],
        };
        rows.insert(0, Fact::Model("unrelated fact".into()));
        let out = tokio::time::timeout(
            Duration::from_secs(2),
            s.finish(rows, Duration::from_secs(3600)),
        )
        .await
        .unwrap();
        assert_eq!(out[0], Fact::Model("unrelated fact".into()));
        let own: Vec<_> = reports(&out)
            .into_iter()
            .filter(|r| r.meter_epoch == epoch() && r.measurement == Measurement::Final)
            .collect();
        assert_eq!(own.len(), 1, "{case}");
        assert!(own[0].report_id.starts_with("usage:"));
        assert_eq!(own[0].tokens.reasoning, None);
        if matches!(case, "unknown" | "foreign" | "cumulative") {
            assert_eq!(own[0].tokens, Tokens::default());
            assert_eq!(codes(&out), ["usage_unavailable"]);
        } else {
            assert!(codes(&out).is_empty());
        }
        if case == "foreign" {
            assert_eq!(reports(&out)[0].report_id, "foreign-id");
        }
    }
}
#[tokio::test]
async fn sg26_finish_native_fields_win_and_missing_fields_use_the_persisted_meter() {
    for case in [
        "partial",
        "all-scope",
        "explicit-model",
        "claude-zero",
        "multiple-models",
    ] {
        let (_d, p, mut s) = source(UsageFallback::ClaudeTranscript).await;
        append(&p, transcript("m1", "model-a", 100, 8));
        if case == "multiple-models" {
            append(&p, transcript("m2", "model-b", 20, 4));
        }
        let tokens = if case == "claude-zero" {
            Tokens {
                input: Some(0),
                output: Some(0),
                cache_read: Some(0),
                cache_write: Some(0),
                ..Default::default()
            }
        } else {
            Tokens {
                input: Some(7),
                reasoning: Some(99),
                ..Default::default()
            }
        };
        let out = s
            .finish(
                vec![native(
                    tokens,
                    (case == "explicit-model").then_some("model-b"),
                    if case == "all-scope" {
                        UsageScope::All
                    } else {
                        UsageScope::Main
                    },
                )],
                Duration::ZERO,
            )
            .await;
        assert!(codes(&out).is_empty(), "{case}: {:?}", codes(&out));
        let rs = reports(&out);
        assert_eq!(
            rs.iter()
                .map(|r| &r.report_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            rs.len()
        );
        let source_all = rs
            .iter()
            .find(|r| r.model.as_deref() == Some("model-a") && r.scope == UsageScope::All)
            .unwrap();
        assert_eq!(source_all.tokens.input, Some(100));
        let a = rs
            .iter()
            .find(|r| r.model.as_deref() == Some("model-a") && r.scope == UsageScope::Main)
            .unwrap();
        if case == "partial" {
            assert_eq!(a.tokens.input, Some(7));
            assert_eq!(a.tokens.output, Some(8));
            assert_eq!(a.tokens.cache_read, Some(3));
            assert_eq!(a.tokens.reasoning, None);
            assert_eq!(rs.len(), 2);
        }
        if case == "claude-zero" {
            assert_eq!(a.tokens.input, Some(100));
            assert_eq!(a.tokens.output, Some(8));
            assert_eq!(a.tokens.reasoning, Some(2));
        }
        if case == "all-scope" {
            assert_eq!(rs.len(), 3);
            assert_eq!(rs[0].scope, UsageScope::All);
            assert_eq!(rs[0].tokens.input, Some(7));
            assert_eq!(a.tokens.input, Some(100));
        }
        if case == "explicit-model" {
            assert_eq!(rs.len(), 3);
            assert_eq!(rs[0].model.as_deref(), Some("model-b"));
            assert_eq!(rs[0].tokens.input, Some(7));
        }
        if case == "multiple-models" {
            assert_eq!(rs.len(), 5);
            assert!(rs[0].model.is_none());
            assert_eq!(rs[0].tokens, Tokens::default());
            assert_eq!(a.tokens.input, Some(100));
        }
    }
}
#[tokio::test]
async fn sg26_finish_retains_live_consumption_when_source_disappears() {
    let (_d, p, mut s) = source(UsageFallback::CodexRollout).await;
    append(&p, rollout(100, 20, 8, 2));
    let live = s.poll().await;
    assert_eq!(reports(&live)[0].tokens.input, Some(80));
    std::fs::remove_file(&p).unwrap();
    let out = s.finish(vec![], Duration::ZERO).await;
    assert_eq!(reports(&out)[0].tokens, reports(&live)[0].tokens);
    assert_eq!(codes(&out), ["usage_incomplete"]);
    assert_ne!(reports(&out)[0].report_id, reports(&live)[0].report_id);
}
#[tokio::test]
async fn sg26_finish_unknown_or_native_lower_bound_distinguishes_unavailable_from_incomplete() {
    for present in [false, true] {
        let (_d, p, mut s) = source(UsageFallback::CodexRollout).await;
        std::fs::write(
            &p,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":",
        )
        .unwrap();
        let out = s
            .finish(
                if present {
                    vec![native(
                        Tokens {
                            input: Some(9),
                            ..Default::default()
                        },
                        None,
                        UsageScope::Main,
                    )]
                } else {
                    vec![]
                },
                Duration::ZERO,
            )
            .await;
        assert_eq!(
            reports(&out)[0].tokens.input,
            if present { Some(9) } else { None }
        );
        assert_eq!(
            codes(&out),
            [if present {
                "usage_incomplete"
            } else {
                "usage_unavailable"
            }]
        );
    }
}
struct NotifiedWake(tokio::sync::Notify);
impl Wake for NotifiedWake {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
}
fn poll_once<F: std::future::Future>(f: std::pin::Pin<&mut F>, w: &Waker) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(w))
}
#[tokio::test]
async fn sg26_finish_reads_a_late_completed_record_and_honors_the_entire_tail_window() {
    use std::io::Write;
    let (_d, p, mut s) = source(UsageFallback::CodexRollout).await;
    let line = format!("{}\n", rollout(100, 20, 20, 4));
    let cut = line.len() - 3;
    std::fs::write(&p, &line[..cut]).unwrap();
    assert!(s.poll().await.is_empty());
    let gate = Arc::new(NotifiedWake(tokio::sync::Notify::new()));
    let waker = Waker::from(gate.clone());
    let wait = Duration::from_secs(1);
    let start = tokio::time::Instant::now();
    let mut finish = Box::pin(s.finish(vec![], wait));
    assert!(poll_once(finish.as_mut(), &waker).is_pending());
    tokio::time::timeout(Duration::from_secs(3), gate.0.notified())
        .await
        .unwrap();
    assert!(poll_once(finish.as_mut(), &waker).is_pending());
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(line[cut..].as_bytes()).unwrap();
    writeln!(f, "{}", rollout(160, 30, 30, 5)).unwrap();
    drop(f);
    let out = tokio::time::timeout(Duration::from_secs(4), finish)
        .await
        .unwrap();
    assert!(start.elapsed() >= wait);
    let r = reports(&out);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].tokens.input, Some(130));
    assert_eq!(r[0].tokens.output, Some(30));
    assert_eq!(codes(&out), Vec::<&str>::new());
}
