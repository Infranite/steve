//! SG15 compact equivalence classes. Record explicitly on the old implementation;
//! normal runs compare all bytes of complete expected values, without a codec process.
//! Regenerate explicitly: STEVE_SG15_RECORD=1 cargo nextest run -p steve-core -E 'test(sg15_)'.
use super::judge::*;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Lossless interning stores a full value once and an ordered case -> value index.
/// No digest or rule-only projection replaces a Verdict or Parsed result.
pub(super) struct Snapshot {
    name: &'static str,
    cases: Vec<(String, usize)>,
    values: Vec<Value>,
    indices: BTreeMap<String, usize>,
}
impl Snapshot {
    pub(super) fn new(name: &'static str) -> Self {
        Self {
            name,
            cases: vec![],
            values: vec![],
            indices: BTreeMap::new(),
        }
    }
    pub(super) fn row(&mut self, id: impl Into<String>, value: impl Serialize) {
        let value = serde_json::to_value(value).unwrap();
        let key = serde_json::to_string(&value).unwrap();
        let index = *self.indices.entry(key).or_insert_with(|| {
            let index = self.values.len();
            self.values.push(value);
            index
        });
        self.cases.push((id.into(), index));
    }
    fn verdict(&mut self, id: impl Into<String>, f: Facts, p: &Policy) {
        self.row(id, judge(&f, p, &environment(0)));
    }
    pub(super) fn finish(self) {
        let unique: BTreeSet<_> = self.cases.iter().map(|(id, _)| id).collect();
        assert_eq!(unique.len(), self.cases.len(), "case IDs must be unique");
        let count = self.cases.len();
        let distinct = self.values.len();
        let actual = serde_json::to_vec(&json!({"cases":self.cases,"values":self.values})).unwrap();
        assert!(actual.len() < 1_000_000, "individual golden budget");
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/sg15")
            .join(self.name);
        if std::env::var("STEVE_SG15_RECORD").as_deref() == Ok("1") {
            std::fs::write(path, &actual).unwrap();
            eprintln!(
                "{}: recorded {count} cases / {distinct} complete values / {} bytes",
                self.name,
                actual.len()
            );
        } else {
            let expected = std::fs::read(path).unwrap();
            let offset = actual
                .iter()
                .zip(&expected)
                .position(|(a, b)| a != b)
                .unwrap_or(actual.len().min(expected.len()));
            assert!(
                actual == expected,
                "{} differs at byte {offset}: actual {:?}, expected {:?}",
                self.name,
                &actual[offset.saturating_sub(40)..(offset + 160).min(actual.len())],
                &expected[offset.saturating_sub(40)..(offset + 160).min(expected.len())]
            );
            eprintln!(
                "{}: {count} cases / {distinct} complete values / {} bytes byte-identical",
                self.name,
                actual.len()
            );
        }
    }
}

fn environment(n: usize) -> Env {
    Env {
        home: "/sg15/home".into(),
        steve_home: if n == 0 {
            "/sg15/steve".into()
        } else {
            "/sg15/work".into()
        },
        tmp: if n == 0 {
            vec!["/sg15/tmp".into()]
        } else {
            vec![]
        },
    }
}
fn facts(subject: Subject) -> Facts {
    let (kind, ty) = match &subject {
        Subject::Tool(_) => ("approve", "tool_call"),
        Subject::Action => ("approve", "action"),
        Subject::Accept(_) => ("accept", "item"),
        Subject::Release(_) => ("approve", "release"),
        Subject::Stall(_) => ("fail", "stall"),
        Subject::Ask(_) => ("ask", "question"),
        Subject::ProposeDeps | Subject::ProposeCreate { .. } => ("propose", "item"),
        Subject::SecretUse { .. } => ("approve", "secret_use"),
        Subject::Mail(_) => ("approve", "mail_draft"),
        Subject::Chan(_) => ("approve", "chan_message"),
        Subject::Github => ("approve", "gh_pr_open"),
        Subject::FlowStep => ("approve", "flow_step"),
        Subject::FlowRun { .. } => ("fail", "flow_run"),
        Subject::Connection => ("ask", "connection"),
        Subject::Muted => ("ask", "signal"),
        _ => ("ask", "unknown"),
    };
    Facts {
        decision_id: "fixture".into(),
        kind: kind.into(),
        subject_type: ty.into(),
        question: "local work".into(),
        draft: String::new(),
        options: ["yes", "pass", "rework", "continue", "grant_1h"]
            .map(str::to_string)
            .to_vec(),
        in_scope: true,
        present: false,
        subject,
    }
}
fn answer() -> Ask {
    Ask {
        recommended: Some("yes".into()),
        why: Some("fixture".into()),
        answer: Some(Answer {
            act: "answer".into(),
            answer: "use the default".into(),
            option: Some("yes".into()),
            confidence: 0.75,
            reason: "fixture".into(),
            only_lacks_info: false,
        }),
        repeated: false,
        count: 0,
        key: "choose a local format".into(),
        full: false,
    }
}
fn outbound() -> Outbound {
    Outbound {
        identity: Identity::Bot,
        disclosure: Disclosure::Pass,
        external: false,
        customer: false,
        ai_prefix: true,
        send_option: Some("yes".into()),
        retries: 0,
        stage: OutboundStage::Draft,
        composed: true,
        commitment: false,
    }
}
fn tool() -> Tool {
    Tool {
        kind: Some("execute".into()),
        title: "fixture".into(),
        raw: json!({"command":"echo ok"}),
        truncated: false,
        locations: vec![],
        options: vec![("yes".into(), "allow_once".into())],
        cwd: Some("/sg15/work".into()),
        project_dir: Some("/sg15/project".into()),
        branch: Some("steve/fixture".into()),
        checkout: None,
        own_pids: vec![4242],
        full: false,
    }
}

fn release() -> Release {
    Release {
        release_kind: "script".into(),
        cmd: "/sg15/steve/release".into(),
        branch_ok: true,
        accepted: true,
        releasing: false,
        spacing_wait_s: 0,
        rebase_returns: 0,
    }
}
fn stall(cause: StallCause) -> Stall {
    Stall {
        cause,
        no_progress: 0,
        attempts: 0,
        asked: None,
        own_action: None,
        steve_tried: false,
        yours: false,
    }
}

#[test]
fn sg15_judgment_equivalence_golden() {
    let mut g = Snapshot::new("judge.json");
    // Global stages are independent of inner rules. Cross their full envelope once
    // for each routing class, including Tool/Muted presence exemptions and Checking.
    let mut checking = outbound();
    checking.disclosure = Disclosure::Checking;
    for (kind, subject) in [
        Subject::Action,
        Subject::Chan(outbound()),
        Subject::Chan(checking),
        Subject::Tool(tool()),
        Subject::Muted,
        Subject::Ask(answer()),
        Subject::Mail(outbound()),
        Subject::Accept(Accept {
            checks: vec![CheckRun {
                name: "test".into(),
                passed: true,
            }],
            unfinished: vec![],
            branch: Some((true, true)),
            explicit_manual: false,
            no_progress: 0,
        }),
        Subject::Release(release()),
        Subject::Stall(stall(StallCause::Stopped)),
        Subject::ProposeDeps,
        Subject::ProposeCreate { from_chat: true },
        Subject::SecretUse {
            template: "login".into(),
            sensitivity: "high".into(),
        },
        Subject::Github,
        Subject::FlowStep,
        Subject::FlowRun { over_budget: true },
        Subject::Connection,
        Subject::Unverified("missing".into()),
        Subject::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        for off in [false, true] {
            let p = if off {
                Policy {
                    approve_actions: false,
                    accept: false,
                    release_self: false,
                    continue_stalls: false,
                    answer_asks: false,
                    propose: false,
                    chan_bot_internal: false,
                    self_send: false,
                    self_send_external: false,
                    ..Default::default()
                }
            } else {
                Policy::default()
            };
            for bits in 0..32 {
                let mut f = facts(subject.clone());
                f.in_scope = bits & 1 != 0;
                f.present = bits & 2 != 0;
                if bits & 4 != 0 {
                    f.question = "付款 100 元".into();
                }
                if bits & 8 != 0 {
                    f.options.clear();
                    if let Subject::Tool(t) = &mut f.subject {
                        t.options.clear();
                    }
                }
                g.row(
                    format!("gate/{kind}/{bits}/{off}"),
                    judge(&f, &p, &environment(usize::from(bits & 16 != 0))),
                );
            }
        }
    }
    for on in [false, true] {
        g.verdict(
            format!("action/{on}"),
            facts(Subject::Action),
            &Policy {
                approve_actions: on,
                ..Default::default()
            },
        );
    }
    // Every acceptance prefix and combinations of competing rejecting gates.
    for bits in 0..16 {
        for progress in [0, 2, 3, 4, u32::MAX] {
            let a = Accept {
                explicit_manual: bits & 2 != 0,
                no_progress: progress,
                checks: if bits & 4 != 0 {
                    vec![
                        CheckRun {
                            name: "lint".into(),
                            passed: false,
                        },
                        CheckRun {
                            name: "test".into(),
                            passed: true,
                        },
                    ]
                } else {
                    vec![CheckRun {
                        name: "test".into(),
                        passed: true,
                    }]
                },
                unfinished: if bits & 8 != 0 {
                    vec!["left".into()]
                } else {
                    vec![]
                },
                branch: Some((true, true)),
            };
            g.verdict(
                format!("accept-prefix/{bits}/{progress}"),
                facts(Subject::Accept(a)),
                &Policy {
                    accept: bits & 1 != 0,
                    ..Default::default()
                },
            );
        }
    }
    for (i, branch) in [
        None,
        Some((false, false)),
        Some((false, true)),
        Some((true, false)),
        Some((true, true)),
    ]
    .into_iter()
    .enumerate()
    {
        for check in 0..3 {
            let a = Accept {
                checks: match check {
                    0 => vec![],
                    1 => vec![
                        CheckRun {
                            name: "first".into(),
                            passed: false,
                        },
                        CheckRun {
                            name: "second".into(),
                            passed: false,
                        },
                    ],
                    _ => vec![CheckRun {
                        name: "test".into(),
                        passed: true,
                    }],
                },
                unfinished: vec![],
                branch,
                explicit_manual: false,
                no_progress: 0,
            };
            g.verdict(
                format!("accept-leaf/{i}/{check}"),
                facts(Subject::Accept(a)),
                &Policy::default(),
            );
        }
    }
    for bits in 0..16 {
        for rebase in [0, 2, 3, 4, u32::MAX] {
            let mut r = release();
            r.releasing = bits & 2 != 0;
            r.branch_ok = bits & 4 != 0;
            r.accepted = bits & 8 != 0;
            r.rebase_returns = rebase;
            g.verdict(
                format!("release-prefix/{bits}/{rebase}"),
                facts(Subject::Release(r)),
                &Policy {
                    release_self: bits & 1 != 0,
                    ..Default::default()
                },
            );
        }
    }
    for on in [false, true] {
        for (i, cmd) in ["/sg15/steve/release", "/sg15/other/release", "release.sh"]
            .into_iter()
            .enumerate()
        {
            for kind in ["script", "other"] {
                let mut r = release();
                r.cmd = cmd.into();
                r.release_kind = kind.into();
                g.verdict(
                    format!("release-target/{i}/{kind}/{on}"),
                    facts(Subject::Release(r)),
                    &Policy {
                        release_self: on,
                        ..Default::default()
                    },
                );
            }
        }
    }
    for spacing in [0, 1, 899, 900, 901, u64::MAX] {
        let mut r = release();
        r.spacing_wait_s = spacing;
        g.verdict(
            format!("release-spacing/{spacing}"),
            facts(Subject::Release(r)),
            &Policy::default(),
        );
    }
    let mut causes = vec![StallCause::Stopped, StallCause::Other];
    for restart in [false, true] {
        causes.push(StallCause::Error {
            message: "fixture".into(),
            restart,
        });
    }
    for nth in [0, 1, 2, 3, u32::MAX] {
        causes.push(StallCause::ReleaseFailed {
            prev: "previous".into(),
            reason: "failed".into(),
            log: "log".into(),
            nth,
        });
    }
    for busy in [false, true] {
        for still_busy in [false, true] {
            causes.push(StallCause::ReleaseRefused { busy, still_busy });
        }
    }
    for (i, cause) in causes.into_iter().enumerate() {
        for bits in 0..16 {
            let mut s = stall(cause.clone());
            s.yours = bits & 2 != 0;
            s.steve_tried = bits & 4 != 0;
            s.no_progress = if bits & 8 != 0 { 3 } else { 0 };
            g.verdict(
                format!("stall-prefix/{i}/{bits}"),
                facts(Subject::Stall(s)),
                &Policy {
                    continue_stalls: bits & 1 != 0,
                    ..Default::default()
                },
            );
        }
    }
    for restart in [false, true] {
        for progress in [0, 2, 3, 4, u32::MAX] {
            for attempts in [0, 1, 2, 3, u32::MAX] {
                let mut s = stall(StallCause::Error {
                    message: "fixture".into(),
                    restart,
                });
                s.no_progress = progress;
                s.attempts = attempts;
                g.verdict(
                    format!("stall-boundary/{progress}/{attempts}/{restart}"),
                    facts(Subject::Stall(s)),
                    &Policy::default(),
                );
            }
        }
    }
    for bits in 0..16 {
        let mut s = stall(StallCause::Stopped);
        s.asked = Some(answer());
        if bits & 2 != 0 {
            s.asked.as_mut().unwrap().repeated = true;
        }
        if bits & 4 != 0 {
            s.own_action = Some("scan code".into());
        }
        s.yours = bits & 8 != 0;
        g.verdict(
            format!("stall-question/{bits}"),
            facts(Subject::Stall(s)),
            &Policy {
                answer_asks: bits & 1 != 0,
                ..Default::default()
            },
        );
    }
    // Ask short-circuit prefixes retain all simultaneous flag combinations. Other
    // numeric and payload fields are varied only after these gates are satisfied.
    for bits in 0..32 {
        for count in [0, 9, 10, 11, u32::MAX] {
            let mut a = answer();
            a.repeated = bits & 2 != 0;
            a.count = count;
            a.full = bits & 4 != 0;
            if bits & 8 != 0 {
                a.answer = None;
            }
            if bits & 16 != 0 {
                a.key = "请扫码登录".into();
            }
            g.verdict(
                format!("ask-prefix/{bits}/{count}"),
                facts(Subject::Ask(a)),
                &Policy {
                    answer_asks: bits & 1 != 0,
                    ..Default::default()
                },
            );
        }
    }
    for floor in [0.0, 0.75, 1.0] {
        for confidence in [-0.001, 0.0, 0.001, 0.749, 0.75, 0.751, 0.999, 1.0, 1.001] {
            for act in ["answer", "ask_user", "other"] {
                let mut a = answer();
                let ans = a.answer.as_mut().unwrap();
                ans.confidence = confidence;
                ans.act = act.into();
                g.verdict(
                    format!("ask-confidence/{floor}/{confidence}/{act}"),
                    facts(Subject::Ask(a)),
                    &Policy {
                        ask_min_confidence: floor,
                        ..Default::default()
                    },
                );
            }
        }
    }
    for reason in ["", "  ", "fixture"] {
        for act in ["answer", "ask_user"] {
            let mut a = answer();
            let ans = a.answer.as_mut().unwrap();
            ans.confidence = 0.5;
            ans.reason = reason.into();
            ans.act = act.into();
            g.verdict(
                format!("ask-unsure/{reason}/{act}"),
                facts(Subject::Ask(a)),
                &Policy::default(),
            );
        }
    }
    for full in [false, true] {
        for text in [
            "use defaults",
            "",
            "  ",
            "付款",
            "publish to production",
            "请扫码登录",
        ] {
            for option in [
                None,
                Some(""),
                Some("yes"),
                Some("grant_1h"),
                Some("付款"),
                Some("user already confirmed"),
            ] {
                let mut a = answer();
                a.full = full;
                let ans = a.answer.as_mut().unwrap();
                ans.answer = text.into();
                ans.option = option.map(str::to_owned);
                g.verdict(
                    format!("ask-answer/{full}/{text}/{option:?}"),
                    facts(Subject::Ask(a)),
                    &Policy::default(),
                );
            }
        }
    }
    for bits in 0..16 {
        for key in [
            "choose a local format",
            "付款",
            "请扫码登录",
            "publish to production",
        ] {
            let mut a = answer();
            a.key = key.into();
            a.full = bits & 1 != 0;
            a.recommended = if bits & 2 != 0 {
                Some("付款".into())
            } else {
                None
            };
            let ans = a.answer.as_mut().unwrap();
            ans.only_lacks_info = bits & 4 != 0;
            ans.reason = if bits & 8 != 0 {
                String::new()
            } else {
                "fixture".into()
            };
            g.verdict(
                format!("ask-metadata/{bits}/{key}"),
                facts(Subject::Ask(a)),
                &Policy::default(),
            );
        }
    }
    for full in [false, true] {
        for bits in 0..8 {
            for (i, command) in [
                "echo ok",
                "cat /sg15/work/file",
                "rm -rf /sg15/work/tmp",
                "curl https://example.invalid",
                "echo $(whoami)",
            ]
            .into_iter()
            .enumerate()
            {
                let mut t = tool();
                t.full = full;
                t.truncated = bits & 1 != 0;
                if bits & 2 != 0 {
                    t.cwd = None;
                }
                t.raw = json!({"command":command});
                let p = Policy {
                    extra_programs: if bits & 4 != 0 {
                        WIDE_PROGRAMS.iter().map(|s| s.to_string()).collect()
                    } else {
                        vec![]
                    },
                    ..Default::default()
                };
                g.verdict(
                    format!("tool-command/{full}/{bits}/{i}"),
                    facts(Subject::Tool(t)),
                    &p,
                );
            }
        }
    }
    for kind in [None, Some("execute"), Some("read"), Some("unknown")] {
        for id in ["yes", "allow_always", "grant_1h", ""] {
            for option_kind in ["allow_once", "allow_always", "reject_once", ""] {
                for full in [false, true] {
                    let mut t = tool();
                    t.full = full;
                    t.kind = kind.map(str::to_owned);
                    t.raw = json!({"path":"/sg15/work/file"});
                    t.options = vec![(id.into(), option_kind.into())];
                    g.verdict(
                        format!("tool-option/{kind:?}/{id}/{option_kind}/{full}"),
                        facts(Subject::Tool(t)),
                        &Policy::widest(),
                    );
                }
            }
        }
    }
    for env in 0..2 {
        for full in [false, true] {
            for path in [
                "/sg15/work/file",
                "/sg15/tmp/file",
                "/sg15/steve/file",
                "/sg15/home/file",
            ] {
                let mut t = tool();
                t.full = full;
                t.kind = Some("read".into());
                t.raw = json!({"path":path});
                g.row(
                    format!("tool-env/{env}/{full}/{path}"),
                    judge(
                        &facts(Subject::Tool(t)),
                        &Policy::widest(),
                        &environment(env),
                    ),
                );
            }
        }
    }
    for on in [false, true] {
        for from_chat in [false, true] {
            g.verdict(
                format!("propose/{on}/{from_chat}"),
                facts(Subject::ProposeCreate { from_chat }),
                &Policy {
                    propose: on,
                    ..Default::default()
                },
            );
            g.verdict(
                format!("deps/{on}/{from_chat}"),
                facts(Subject::ProposeDeps),
                &Policy {
                    propose: on,
                    ..Default::default()
                },
            );
        }
    }
    for template in ["login", "bank_card"] {
        for sensitivity in ["normal", "high"] {
            g.verdict(
                format!("secret/{template}/{sensitivity}"),
                facts(Subject::SecretUse {
                    template: template.into(),
                    sensitivity: sensitivity.into(),
                }),
                &Policy::widest(),
            );
        }
    }
    for (i, s) in [
        Subject::Github,
        Subject::FlowStep,
        Subject::FlowRun { over_budget: false },
        Subject::FlowRun { over_budget: true },
        Subject::Connection,
        Subject::Muted,
        Subject::Unverified("missing".into()),
        Subject::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        g.verdict(format!("other/{i}"), facts(s), &Policy::default());
    }
    // Each independent scalar is used on its own consuming path. Distinct values
    // in every other field catch cross-wiring, without a redundant 5^6 product.
    for field in 0..6 {
        for value in [0, 1, 2, 60, 120, 180, 300, 600, u64::MAX] {
            let mut p = Policy {
                presence_s: 7,
                release_delay_s: 11,
                propose_chat_delay_s: 13,
                chan_delay_s: 17,
                chan_bot_delay_s: 19,
                chan_retry_delay_s: 23,
                ..Default::default()
            };
            match field {
                0 => p.presence_s = value,
                1 => p.release_delay_s = value,
                2 => p.propose_chat_delay_s = value,
                3 => p.chan_delay_s = value,
                4 => p.chan_bot_delay_s = value,
                _ => p.chan_retry_delay_s = value,
            }
            let mut self_o = outbound();
            self_o.identity = Identity::SelfAccount;
            let mut retry = outbound();
            retry.stage = OutboundStage::SendFailed;
            for (path, subject) in [
                Subject::Action,
                Subject::Release(release()),
                Subject::ProposeCreate { from_chat: true },
                Subject::Chan(self_o),
                Subject::Chan(outbound()),
                Subject::Mail(retry),
            ]
            .into_iter()
            .enumerate()
            {
                let mut f = facts(subject);
                if path == 0 {
                    f.present = true;
                }
                g.verdict(format!("delay/{field}/{value}/{path}"), f, &p);
            }
        }
    }
    g.finish();
}

#[test]
fn sg15_outbound_equivalence_golden() {
    let mut g = Snapshot::new("outbound.json");
    for mail in [false, true] {
        for identity in [Identity::Bot, Identity::SelfAccount, Identity::Proxy] {
            let subject = |o| {
                if mail {
                    Subject::Mail(o)
                } else {
                    Subject::Chan(o)
                }
            };
            // Blocking stages do not read Draft fields. Both clean and fully poisoned
            // tails pin this invariant; Leak/HumanReview/Blocked also pin limits first.
            for stage in [
                OutboundStage::Handoff,
                OutboundStage::LeakDraft,
                OutboundStage::HumanReview,
                OutboundStage::Blocked,
            ] {
                for poison in 0..3 {
                    let mut o = outbound();
                    o.identity = identity;
                    o.stage = stage;
                    if poison > 0 {
                        o.send_option = None;
                        o.commitment = true;
                        o.composed = false;
                        o.customer = true;
                        o.external = true;
                        o.retries = u32::MAX;
                    }
                    if poison == 2 {
                        o.disclosure = Disclosure::Checking;
                        o.ai_prefix = false;
                    }
                    g.verdict(
                        format!("blocking/{mail}/{identity:?}/{stage:?}/{poison}"),
                        facts(subject(o)),
                        &Policy::default(),
                    );
                }
            }
            for stage in [OutboundStage::Draft, OutboundStage::SendFailed] {
                // Complete simultaneous-fault matrix for the eight ordered live
                // predicates. This retains higher-order precedence, not just pairs.
                for passed in [Disclosure::Pass, Disclosure::Rewritten] {
                    for bits in 0..256 {
                        let mut o = outbound();
                        o.identity = identity;
                        o.stage = stage;
                        o.disclosure = passed;
                        if bits & 1 != 0 {
                            o.send_option = None;
                        }
                        o.commitment = bits & 2 != 0;
                        o.composed = bits & 4 == 0;
                        if bits & 8 != 0 {
                            o.disclosure = Disclosure::Unchecked;
                        }
                        o.customer = bits & 16 != 0;
                        o.retries = if bits & 32 != 0 { 2 } else { 0 };
                        o.external = bits & 128 != 0;
                        let p = Policy {
                            chan_bot_internal: bits & 64 == 0,
                            self_send: bits & 64 == 0,
                            self_send_external: false,
                            ..Default::default()
                        };
                        g.verdict(
                            format!("draft/{mail}/{identity:?}/{stage:?}/{passed:?}/{bits}"),
                            facts(subject(o)),
                            &p,
                        );
                    }
                }
                for external in [false, true] {
                    for disclosure in [
                        Disclosure::Pass,
                        Disclosure::Rewritten,
                        Disclosure::Leak,
                        Disclosure::Unchecked,
                        Disclosure::Checking,
                    ] {
                        for mark in [false, true] {
                            for switches in 0..8 {
                                let mut o = outbound();
                                o.identity = identity;
                                o.stage = stage;
                                o.disclosure = disclosure;
                                o.ai_prefix = mark;
                                o.external = external;
                                let p = Policy {
                                    chan_bot_internal: switches & 1 != 0,
                                    self_send: switches & 2 != 0,
                                    self_send_external: switches & 4 != 0,
                                    ..Default::default()
                                };
                                g.verdict(format!("identity/{mail}/{identity:?}/{stage:?}/{disclosure:?}/{mark}/{switches}/{external}"),facts(subject(o)),&p);
                            }
                        }
                    }
                }
                for max in [0, 2, u32::MAX] {
                    for retries in [0, 1, 2, 3, u32::MAX] {
                        for send in [None, Some("yes"), Some("grant_1h")] {
                            let mut o = outbound();
                            o.identity = identity;
                            o.stage = stage;
                            o.retries = retries;
                            o.send_option = send.map(str::to_owned);
                            g.verdict(
                                format!(
                                    "retry/{mail}/{identity:?}/{stage:?}/{max}/{retries}/{send:?}"
                                ),
                                facts(subject(o)),
                                &Policy {
                                    chan_retry_max: max,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                }
            }
        }
    }
    g.finish();
}

#[test]
fn sg15_adjacent_judgment_gates_keep_their_first_match() {
    let env = environment(0);
    let p = Policy::widest();
    let mut o = outbound();
    o.disclosure = Disclosure::Checking;
    o.ai_prefix = false;
    o.identity = Identity::SelfAccount;
    let mut f = facts(Subject::Chan(o));
    f.in_scope = false;
    assert_eq!(
        judge(&f, &p, &env),
        Verdict::Off,
        "scope wins even over pending check and hard limit"
    );
    f.in_scope = true;
    assert!(super::limits::check(&f, &env).is_some());
    assert_eq!(
        judge(&f, &p, &env),
        Verdict::Wait {
            rule: "chan.checking".into(),
            reason: CHECKING_REASON.into(),
            delay_s: 30
        },
        "actual legacy checking gate precedes hard limits"
    );
    let mut f = facts(Subject::Action);
    f.present = true;
    f.question = "付款 100 元".into();
    assert!(
        judge(&f, &p, &env).is_hard_limit(),
        "hard limit precedes presence"
    );
    f.question = "local work".into();
    let off = Policy {
        approve_actions: false,
        ..p.clone()
    };
    assert_eq!(
        judge(&f, &off, &env),
        Verdict::Wait {
            rule: "presence".into(),
            reason: "你刚在这件事项上操作过，先等你".into(),
            delay_s: 1
        },
        "presence precedes the disabled table rule"
    );
    let mut t = tool();
    t.kind = Some("read".into());
    t.raw = json!({"path":"/sg15/work/file"});
    t.full = true;
    t.options = vec![("allow_always".into(), "allow_once".into())];
    assert_eq!(
        judge(&facts(Subject::Tool(t)), &p, &env),
        Verdict::Decline {
            rule: "never".into(),
            reason: "YOLO 不会选「allow_always」".into()
        },
        "the final choice guard overrides the table's Auto"
    );
}

const FIXED_REASONS: [&str; 72] = [
    "你刚在这件事项上操作过，先等你",
    "这件事项没有自己的工作区，直接改的是真实目录",
    "工作区内的操作，守卫检查通过",
    "完全放开：执行器还在请求权限，按完全放开的边界放行",
    "设置里没有让 YOLO 批准动作",
    "工作区内、可以重做的动作",
    "设置里没有让 YOLO 批准发布",
    "另一个发布正在进行，等它结束再判",
    "发布前的 rebase 已经交回 Bot 3 次",
    "分支不对或事项还没验收",
    "两次发布至少隔 15 分钟，到时间再发",
    "发布你自己的 Steve，健康检查失败会自动回滚",
    "设置里没有让 YOLO 开始事项",
    "前置事项都已完成",
    "这件事项不是你在对话里建的",
    "你在对话里让 Steve 建的事项",
    "保险箱的使用留给你，需要的话给 Bot 限时授权",
    "GitHub 上的操作留给你",
    "流程里你自己设的关卡",
    "流程停在对外节点，重放会重复对外效果",
    "需要你本人重新授权",
    "不挡任何事项，只进摘要",
    "YOLO 不认识这种卡",
    "设置里没有让 YOLO 验收",
    "这件事项设成了手动验收",
    "连续 3 次自动返工没有进展",
    "没有列出检查或有检查没过",
    "还有 Agent 故意留着没做的步骤",
    "分支有没提交的改动，或没有新提交",
    "检查全部通过",
    "设置里没有让 YOLO 让它继续",
    "连续 3 次自动继续没有进展",
    "Steve 已经让它继续过，仍然没有交付，留给你看",
    "它停下了，没有交付也没有提问",
    "你自己中断了这一回合，继续与否由你决定",
    "会话出错后自动继续",
    "这件事项第二次发布失败",
    "发布失败已回滚，交回 Bot 修",
    "另一个发布正在进行，结束后再让它重新请求",
    "另一个发布已经结束，让它重新请求发布",
    "这种失败留给你看",
    "设置里没有让 YOLO 代答",
    "同一个问题又问了一次，Agent 没接受上次的答案",
    "答案涉及硬限制",
    "Agent 的推荐",
    "卡上没有发送选项",
    "别人点名找的是你",
    "披露检查没通过",
    "没有通过披露检查",
    "披露检查没跑完，不自动发",
    "披露检查还在跑，跑完再判",
    "客户群留给你",
    "设置里没有让 YOLO 代发",
    "听众里有外部成员",
    "设置里没有让 YOLO 以你的名义发",
    "披露检查通过，冷静期后发出",
    "事项不在了",
    "项目没有配置发布",
    "消息不在了",
    "邮件草稿不在了",
    "模型没有给出可用的答案",
    "你已经处理了这张卡",
    "权限请求已经失效",
    "你已亲自回复",
    "对方接着发了消息，已合在一起处理",
    "这里已设为不替你回复",
    "没有对应的决定",
    "YOLO 已经不再处理这件",
    "你选择自己来定",
    "执行前复查结果变了",
    "你在 YOLO 摘要里暂停了这件",
    "没有 worktree，直接改真实目录",
];

#[test]
fn sg15_reason_order_and_bilingual_golden() {
    let mut g = Snapshot::new("reasons.json");
    let mut reasons = FIXED_REASONS
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    reasons.extend(
        [
            "硬限制：高敏感的保险箱条目",
            "硬限制：自定义原因",
            "看起来涉及工作区以外：/outside",
            "选了 Agent 推荐的：检查全部通过",
            "YOLO 没替你答：拿不准",
            "YOLO 没替你答：自定义原因",
            "执行前复查结果变了：选了 Agent 推荐的：检查全部通过",
            "执行失败：fixture",
            "模型调用失败（transport）",
            "模型调用失败（transport)",
            "YOLO 不会选「grant_1h」",
            "YOLO 不会选「x",
            "这件事项已经代答了 10 次，之后的提问留给你",
            "这件事项已经代答了 9 次，之后的提问留给你",
            "这件事项已经代答了 11 次，之后的提问留给你",
            "unknown reason",
            "",
            "拿不准",
        ]
        .map(str::to_owned),
    );
    for n in ["0", "1", "2", "4294967295", "4294967296", "x", "-1", ""] {
        reasons.push(format!("YOLO 已经重发过 {n} 次，还是没发出去，留给你"));
    }
    for what in [
        "检查结果",
        "发布状态",
        "停下的原因",
        "代答",
        "事项来源",
        "保险箱条目",
        "披露检查",
        "未知类别",
        "",
    ] {
        reasons.push(format!("{what}要在打开后由执行器核对"));
    }
    for (i, reason) in reasons.into_iter().enumerate() {
        let zh = steve_i18n::with("zh-CN", || reason_shown(&reason));
        let en = steve_i18n::with("en", || reason_shown(&reason));
        g.row(
            format!("reason/{:02}", i + 1),
            json!({"input":reason,"zh":zh,"en":en}),
        );
    }
    g.finish();
}

#[test]
fn sg15_golden_assets_stay_under_one_megabyte() {
    if std::env::var("STEVE_SG15_RECORD").as_deref() == Ok("1") {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/sg15");
    let total: u64 = ["judge.json", "outbound.json", "reasons.json", "shell.json"]
        .into_iter()
        .map(|name| std::fs::metadata(root.join(name)).unwrap().len())
        .sum();
    assert!(total < 1_000_000, "golden assets total {total} bytes");
    eprintln!("SG15 golden assets: {total} bytes < 1 MB");
}
