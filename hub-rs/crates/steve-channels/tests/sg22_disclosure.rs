//! SG22: disclosure phases and retries, fixed on the original implementation.
#[path = "support/digest_env.rs"]
mod digest_env;
use async_trait::async_trait;
use digest_env::TestEnv;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_channels::disclosure::{self, Disclosure};
use steve_modules::gateway::{Gateway, GatewayOut, Tier};
use steve_modules::outbound::{Audience, CheckCx, KnowledgeUse, OutboundCheck, Verdict};
use steve_modules::testkit::WS;
use steve_store::base::Level;

const PASS: &str = r#"{"verdict":"pass","spans":[]}"#;
const LEAK: &str = r#"{"verdict":"leak","spans":[{"text":"内部安排","reason":"内部细节","basis":"internal_detail"}]}"#;
struct Script {
    answers: Mutex<VecDeque<Result<&'static str, &'static str>>>,
    calls: Mutex<Vec<(Tier, Duration, String)>>,
}
#[async_trait]
impl Gateway for Script {
    async fn run(&self, tier: Tier, prompt: &str, timeout: Duration) -> Result<GatewayOut, String> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((tier, timeout, prompt.into()));
            calls.len()
        };
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model call")
            .map(|text| GatewayOut {
                text: text.into(),
                model: format!("model-{n}"),
                ..Default::default()
            })
            .map_err(str::to_owned)
    }
}
fn setup(answers: Vec<Result<&'static str, &'static str>>) -> (TestEnv, Disclosure, Arc<Script>) {
    let env = TestEnv::new(|_| {});
    let script = Arc::new(Script {
        answers: Mutex::new(answers.into()),
        calls: Mutex::new(vec![]),
    });
    let check = Disclosure::with_models(vec!["draft-model".into(), "check-a".into()]);
    check.bind(env.store().clone(), script.clone());
    (env, check, script)
}
fn cx() -> CheckCx {
    CheckCx {
        ws: WS.into(),
        actor: "steve".into(),
        draft_model: Some("draft-model".into()),
        ..Default::default()
    }
}
fn knowledge(level: Level, text: &str) -> KnowledgeUse {
    KnowledgeUse {
        source_type: "memory".into(),
        source_id: "m_sg22".into(),
        source_label: "保密薪资".into(),
        level,
        visibility: "private".into(),
        text: text.into(),
        seq: Some(3),
    }
}

#[tokio::test]
async fn sg22_unbound_returns_unchecked_without_entries() {
    let check = Disclosure::with_models(vec![]);
    let r = check
        .check(
            &cx(),
            &Audience::default(),
            &"字".repeat(50),
            &[knowledge(Level::Never, "字")],
        )
        .await;
    assert_eq!(r.verdict, Verdict::Unchecked);
    assert!(r.entries.is_empty() && r.check_model.is_none());
    assert_eq!(r.spans.len(), 1);
    assert_eq!(r.spans[0].text, "字".repeat(40));
    assert_eq!(r.spans[0].reason, disclosure::R_UNBOUND);
}

#[tokio::test]
async fn sg22_private_checks_only_never_even_with_patient_and_copied_style() {
    for (level, used, expected) in [
        (Level::Reasoning, true, Verdict::Pass),
        (Level::Never, false, Verdict::Pass),
        (Level::Never, true, Verdict::Leak),
    ] {
        let (_env, check, model) = setup(vec![]);
        let mut context = cx();
        context.patient = true;
        context.style_samples = vec!["月薪五万元已经确认".into()];
        let audience = Audience {
            scopes: vec!["private".into()],
            ..Default::default()
        };
        let k = knowledge(
            level,
            if used {
                "月薪五万元已经确认"
            } else {
                "完全无关的另一个秘密"
            },
        );
        let r = check
            .check(&context, &audience, "月薪五万元已经确认", &[k])
            .await;
        assert_eq!(r.verdict, expected, "{level:?} used={used}: {r:?}");
        assert!(model.calls.lock().unwrap().is_empty());
        assert!(r.check_model.is_none());
        assert_eq!(r.entries.len(), 1);
        if expected == Verdict::Leak {
            assert_eq!(r.spans.len(), 1);
            assert_eq!(r.spans[0].text, "保密薪资");
            assert_eq!(r.spans[0].reason, "用到了不可使用的知识（保密薪资）");
            assert!(r.entries[0].used);
        } else {
            assert!(r.spans.is_empty());
        }
    }
}

#[tokio::test]
async fn sg22_local_terms_and_never_remain_leaks_when_model_is_skipped() {
    for (level, text) in [
        (Level::Reasoning, "预算为30万"),
        (Level::Never, "月薪五万元已经确认"),
    ] {
        let (_env, check, model) = setup(vec![]);
        let mut context = cx();
        context.skip_model = true;
        let r = check
            .check(
                &context,
                &Audience::default(),
                text,
                &[knowledge(level, text)],
            )
            .await;
        assert_eq!(r.verdict, Verdict::Leak, "{r:?}");
        assert!(!r.spans.is_empty());
        assert!(r.entries[0].used);
        assert!(r.check_model.is_none());
        assert!(model.calls.lock().unwrap().is_empty());
        if level == Level::Never {
            assert!(
                r.spans
                    .iter()
                    .any(|s| s.reason == "用到了不可使用的知识（保密薪资）")
            );
        }
    }
}

#[tokio::test]
async fn sg22_style_samples_are_flagged_unless_visible_in_said_or_seen() {
    for (said, seen, expected) in [
        (None, None, Verdict::Leak),
        (Some("秘密发布计划"), None, Verdict::Pass),
        (None, Some("秘密发布计划"), Verdict::Pass),
    ] {
        let (_env, check, model) = setup(vec![]);
        let mut context = cx();
        context.skip_model = true;
        context.style_samples = vec!["秘密发布计划".into()];
        context.audience_said = said.map(str::to_owned);
        context.audience_seen = seen.map(str::to_owned);
        let r = check
            .check(&context, &Audience::default(), "秘密发布计划", &[])
            .await;
        assert_eq!(r.verdict, expected, "{r:?}");
        assert_eq!(r.spans.len(), usize::from(expected == Verdict::Leak));
        if expected == Verdict::Leak {
            assert_eq!(r.spans[0].reason, disclosure::R_STYLE_COPY);
        }
        assert!(model.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn sg22_internal_skip_requires_complete_public_knowledge_and_internal_audience() {
    for (internal_skip, external, workspace, tracked, level, visibility, called) in [
        (true, false, true, true, Level::Public, "private", false),
        (
            true,
            false,
            true,
            true,
            Level::Reasoning,
            "workspace",
            false,
        ),
        (true, false, true, false, Level::Public, "workspace", true),
        (true, true, true, true, Level::Public, "workspace", true),
        (true, false, true, true, Level::Reasoning, "private", true),
        (false, false, true, true, Level::Public, "workspace", true),
        (true, false, false, true, Level::Public, "workspace", true),
    ] {
        let (_env, check, model) = setup(vec![Ok(PASS)]);
        let mut context = cx();
        context.internal_skip = internal_skip;
        let audience = Audience {
            external,
            scopes: if workspace {
                vec!["workspace".into()]
            } else {
                vec![]
            },
            ..Default::default()
        };
        let mut k = knowledge(level, "源资料");
        k.visibility = visibility.into();
        let ks = if tracked { vec![k] } else { vec![] };
        let r = check.check(&context, &audience, "确认收到", &ks).await;
        assert_eq!(r.verdict, Verdict::Pass, "{r:?}");
        assert_eq!(model.calls.lock().unwrap().len(), usize::from(called));
        assert_eq!(r.check_model.is_some(), called);
    }
}

#[tokio::test]
async fn sg22_model_verdict_failures_patient_retries_and_attempt_names_table() {
    for (name, patient, answers, verdict, models, tiers) in [
        (
            "pass",
            false,
            vec![Ok(PASS)],
            Verdict::Pass,
            "model-1",
            vec![Tier::Fast],
        ),
        (
            "leak",
            false,
            vec![Ok(LEAK)],
            Verdict::Leak,
            "model-1",
            vec![Tier::Fast],
        ),
        (
            "unreadable",
            false,
            vec![Ok("not json")],
            Verdict::Unchecked,
            "model-1",
            vec![Tier::Fast],
        ),
        (
            "failed",
            false,
            vec![Err("unavailable")],
            Verdict::Unchecked,
            "check-a",
            vec![Tier::Fast],
        ),
        (
            "patient-fail-pass",
            true,
            vec![Err("unavailable"), Ok(PASS)],
            Verdict::Pass,
            "check-a → model-2",
            vec![Tier::Fast, Tier::Smart],
        ),
        (
            "patient-invalid-pass",
            true,
            vec![Ok("not json"), Ok(PASS)],
            Verdict::Pass,
            "model-1 → model-2",
            vec![Tier::Fast, Tier::Smart],
        ),
        (
            "patient-fail-fail",
            true,
            vec![Err("first"), Err("last")],
            Verdict::Unchecked,
            "check-a → tier:smart",
            vec![Tier::Fast, Tier::Smart],
        ),
        (
            "patient-first-pass",
            true,
            vec![Ok(PASS)],
            Verdict::Pass,
            "model-1",
            vec![Tier::Fast],
        ),
        (
            "patient-invalid-fail",
            true,
            vec![Ok("not json"), Err("last")],
            Verdict::Unchecked,
            "model-1 → tier:smart",
            vec![Tier::Fast, Tier::Smart],
        ),
    ] {
        let (_env, check, model) = setup(answers);
        let mut context = cx();
        context.patient = patient;
        let r = check
            .check(&context, &Audience::default(), "确认收到", &[])
            .await;
        assert_eq!(r.verdict, verdict, "{name}: {r:?}");
        assert_eq!(r.check_model.as_deref(), Some(models), "{name}");
        let calls = model.calls.lock().unwrap();
        assert_eq!(
            calls.iter().map(|c| c.0).collect::<Vec<_>>(),
            tiers,
            "{name}"
        );
        for (_, timeout, prompt) in calls.iter() {
            assert_eq!(
                *timeout,
                if patient {
                    disclosure::PATIENT_TIMEOUT
                } else {
                    disclosure::TIMEOUT
                }
            );
            assert!(prompt.starts_with(disclosure::CHECK_HEAD));
        }
        if verdict == Verdict::Unchecked {
            assert!(disclosure::not_run(&r));
            assert_eq!(r.spans.len(), 1);
            assert!(r.spans[0].reason.starts_with(disclosure::R_MODEL_FAILED));
        } else if verdict == Verdict::Pass {
            assert!(r.spans.is_empty());
        } else {
            assert_eq!(r.spans[0].reason, "内部细节（依据：内部细节）");
        }
    }
}

#[tokio::test]
async fn sg22_model_never_sees_never_knowledge_but_basis_still_records_it() {
    let (_env, check, model) = setup(vec![Ok(PASS)]);
    let k = knowledge(Level::Never, "UNSHAREABLE_NEVER_PAYLOAD");
    let r = check
        .check(&cx(), &Audience::default(), "确认收到", &[k])
        .await;
    assert_eq!(r.verdict, Verdict::Pass);
    assert_eq!(r.entries.len(), 1);
    assert_eq!(r.entries[0].level, Level::Never);
    assert_eq!(r.entries[0].seq, Some(3));
    let calls = model.calls.lock().unwrap();
    assert!(!calls[0].2.contains("UNSHAREABLE_NEVER_PAYLOAD"));
    assert!(!calls[0].2.contains("保密薪资"));
}

#[tokio::test]
async fn sg22_local_leak_survives_model_pass_failure_and_duplicate_spans() {
    for (answer, unchecked) in [
        (Ok(PASS), false),
        (
            Ok(
                r#"{"verdict":"leak","spans":[{"text":"秘密发布计划","reason":"model duplicate","basis":"internal_detail"}]}"#,
            ),
            false,
        ),
        (Ok("not json"), true),
        (Err("unavailable"), true),
    ] {
        let (_env, check, model) = setup(vec![answer]);
        let mut context = cx();
        context.style_samples = vec!["秘密发布计划".into()];
        let r = check
            .check(&context, &Audience::default(), "秘密发布计划", &[])
            .await;
        assert_eq!(r.verdict, Verdict::Leak, "{r:?}");
        assert_eq!(
            r.spans.iter().filter(|s| s.text == "秘密发布计划").count(),
            1
        );
        assert_eq!(r.spans[0].reason, disclosure::R_STYLE_COPY);
        assert_eq!(r.spans.len(), if unchecked { 2 } else { 1 });
        assert_eq!(disclosure::not_run(&r), unchecked);
        assert_eq!(model.calls.lock().unwrap().len(), 1);
    }
}
