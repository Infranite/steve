//! The job contract: typed think and research work, their reports, and how the fake Node runs them.

use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use steve_node_api::testkit::{FakeNode, RecordingSink, build::*};
use steve_node_api::*;

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let s = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, v, "{s}");
}

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

fn connected() -> (Arc<FakeNode>, Arc<RecordingSink>) {
    let n = FakeNode::new("hub");
    let sink = Arc::new(RecordingSink::default());
    n.connect(sink.clone());
    (n, sink)
}

fn artifact() -> ArtifactRef {
    ArtifactRef {
        id: ArtifactId::new("art_1").unwrap(),
        sha256: Digest::sha256(b"x"),
        size: 1,
        mime: "text/plain".into(),
    }
}

fn root(rel: &str) -> RootPath {
    RootPath {
        root: RootId::new("r_code").unwrap(),
        rel: RelPath::new(rel).unwrap(),
    }
}

/// A think job that reads one attached file, with a cached head.
fn think_with_file() -> JobSpec {
    let mut j = job("j_1", "a_1", "总结附件");
    j.run.prompt.push(Part::File {
        artifact: artifact(),
        name: ".steve/attachments/att_1-notes.md".into(),
    });
    j.work = JobWork::Think(ThinkJob {
        instructions: Some("你是 Steve。".into()),
        tools: ThinkToolMode::ReadAttached,
    });
    j
}

fn research_in_projects() -> JobSpec {
    let mut j = research("j_r", "a_r", "这个函数在哪里被调用？");
    j.run.continuation = Some(ResumeHandle::new("claude_s1").unwrap());
    j.run.policy.sandbox = SandboxRequirement::Required {
        guarantees: vec!["fs-readonly".into()],
    };
    j.work = JobWork::Research(ResearchJob {
        cwd: Some(root("steve")),
        readable: vec![root("steve-notes")],
        budget: ResearchBudget {
            max_turns: Some(40),
            max_tools: None,
            idle_ms: 600_000,
        },
        steve_tools: true,
    });
    j
}

fn report(end: JobEnd) -> Completion {
    Completion {
        result: text("答案"),
        job: Some(JobReport {
            end,
            turns: 3,
            files_read: vec!["src/lib.rs".into()],
            sandboxed: true,
        }),
        ..done()
    }
}

// ── wire shape ──

#[test]
fn job_types_survive_the_wire() {
    roundtrip(&cmd("op_j", job("j_1", "a_1", "你好")));
    roundtrip(&cmd("op_j", think_with_file()));
    roundtrip(&cmd("op_r", research_in_projects()));
    roundtrip(&cmd("op_r", research("j_r", "a_r", "x")));
    for end in [
        JobEnd::Done,
        JobEnd::Failed,
        JobEnd::Stopped,
        JobEnd::Budget {
            hit: BudgetHit::Idle,
        },
        JobEnd::Violated {
            rule: ViolationRule::OutOfScope,
            tool: "Read".into(),
            target: Some("/etc/hosts".into()),
        },
        JobEnd::Violated {
            rule: ViolationRule::Tool,
            tool: "Bash".into(),
            target: None,
        },
    ] {
        roundtrip(&report(end.clone()));
        roundtrip(&EventBody::AttemptEnded {
            completion: report(end),
        });
    }
    roundtrip(&done());
}

#[test]
fn job_wire_names_are_steves() {
    let v = serde_json::to_value(think_with_file()).unwrap();
    assert_eq!(v["work"]["kind"], "think");
    assert_eq!(v["work"]["tools"], "read_attached");
    assert_eq!(v["work"]["instructions"], "你是 Steve。");
    assert!(v.get("kind").is_none(), "{v}");
    let v = serde_json::to_value(research_in_projects()).unwrap();
    assert_eq!(v["work"]["kind"], "research");
    assert_eq!(v["work"]["cwd"], json!({"root": "r_code", "rel": "steve"}));
    assert_eq!(
        v["work"]["budget"],
        json!({"max_turns": 40, "idle_ms": 600_000})
    );
    assert_eq!(v["run"]["continuation"], "claude_s1");
    let v = serde_json::to_value(report(JobEnd::Violated {
        rule: ViolationRule::Sensitive,
        tool: "Read".into(),
        target: Some("~/.ssh/id_ed25519".into()),
    }))
    .unwrap();
    assert_eq!(
        v["job"]["end"],
        json!({"type": "violated", "rule": "sensitive", "tool": "Read", "target": "~/.ssh/id_ed25519"})
    );
    let v = serde_json::to_value(report(JobEnd::Budget {
        hit: BudgetHit::Time,
    }))
    .unwrap();
    assert_eq!(v["job"]["end"], json!({"type": "budget", "hit": "time"}));
    let v = serde_json::to_value(report(JobEnd::Failed)).unwrap();
    assert_eq!(v["job"]["end"], json!({"type": "failed"}));
    // a completion without a report says nothing about one, and an older one still decodes
    let v = serde_json::to_value(done()).unwrap();
    assert!(v.get("job").is_none(), "{v}");
    let old: Completion =
        serde_json::from_value(json!({"outcome": "succeeded", "stop_reason": "end_turn"})).unwrap();
    assert_eq!(old.job, None);
    assert_eq!(
        JobWork::Think(ThinkJob {
            instructions: None,
            tools: ThinkToolMode::None,
        })
        .kind(),
        JobKind::Think
    );
    assert_eq!(research_in_projects().work.kind(), JobKind::Research);
}

#[test]
fn an_old_job_spec_is_refused() {
    // before N0d-4 a job was `kind` + `run`; none was ever stored or sent, so there is nothing to
    // migrate and the old shape must not decode into a job with a guessed body
    let mut v = serde_json::to_value(job("j_1", "a_1", "hi")).unwrap();
    let m = v.as_object_mut().unwrap();
    m.remove("work");
    m.insert("kind".into(), json!("think"));
    let e = serde_json::from_value::<JobSpec>(v.clone()).unwrap_err();
    assert!(e.to_string().contains("missing field `work`"), "{e}");
    let c = json!({
        "id": "op_j",
        "issued_at": 1000,
        "accept_before": 60000,
        "grant": "g_test",
        "body": v,
    });
    assert!(serde_json::from_value::<Command<JobSpec>>(c).is_err());
    // an unknown kind of work never decodes
    let mut v = serde_json::to_value(job("j_1", "a_1", "hi")).unwrap();
    v["work"]["kind"] = json!("exec");
    assert!(serde_json::from_value::<JobSpec>(v).is_err());
}

// ── shape checks ──

#[tokio::test]
async fn a_malformed_job_is_refused_before_anything_runs() {
    let (n, sink) = connected();
    let mut cases: Vec<(&str, JobSpec)> = vec![];
    let mut j = job("j_1", "a_1", "x");
    j.run.directory = RunDirectory::Workspace {
        workspace: workspace("hub", "item:x"),
    };
    cases.push(("workspace directory", j));
    let mut j = job("j_1", "a_1", "x");
    j.run.policy.readable = vec![PathRef {
        workspace: workspace("hub", "item:x"),
        rel: RelPath::new("src").unwrap(),
    }];
    cases.push(("policy readable", j));
    let mut j = job("j_1", "a_1", "x");
    j.run.prompt.push(Part::Diff {
        path: "a".into(),
        old: None,
        new: "b".into(),
        added: 1,
        removed: 0,
    });
    cases.push(("diff part", j));
    let mut j = think_with_file();
    j.work = JobWork::Think(ThinkJob {
        instructions: None,
        tools: ThinkToolMode::None,
    });
    cases.push(("file without read_attached", j));
    for bad in [
        "../secret",
        "/etc/passwd",
        "",
        // outside .steve/attachments: the harness's own configuration in its cwd
        ".claude/settings.json",
        ".mcp.json",
        "CLAUDE.md",
        "AGENTS.md",
        ".",
        // not exactly one plain segment under it
        ".steve/attachments/",
        ".steve/attachments/.",
        ".steve/attachments/..",
        ".steve/attachments/sub/x.md",
        ".steve/attachments//x.md",
        ".steve/attachments-x.md",
    ] {
        let mut j = think_with_file();
        j.run.prompt[1] = Part::File {
            artifact: artifact(),
            name: bad.into(),
        };
        cases.push(("file name", j));
    }
    let mut j = think_with_file();
    j.run.prompt.push(j.run.prompt[1].clone());
    cases.push(("duplicate file name", j));
    let mut j = job("j_1", "a_1", "x");
    j.run.continuation = Some(ResumeHandle::new("claude_s1").unwrap());
    cases.push(("think continuation", j));
    for budget in [
        ResearchBudget {
            max_turns: None,
            max_tools: None,
            idle_ms: 0,
        },
        ResearchBudget {
            max_turns: Some(0),
            max_tools: None,
            idle_ms: 1,
        },
        ResearchBudget {
            max_turns: None,
            max_tools: Some(0),
            idle_ms: 1,
        },
    ] {
        let mut j = research("j_1", "a_1", "x");
        if let JobWork::Research(r) = &mut j.work {
            r.budget = budget;
        }
        cases.push(("research budget", j));
    }
    for (i, (what, spec)) in cases.into_iter().enumerate() {
        assert_eq!(spec.check().unwrap_err().code, ErrorCode::Invalid, "{what}");
        let e = n
            .start_job(cmd(&format!("op_bad{i}"), spec))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{what}");
    }
    assert!(n.log().is_empty());
    assert!(sink.kinds().is_empty());
    // the well-formed ones pass the shape check (the fake enforces no sandbox, so the sandboxed
    // research run is refused as unsupported, not invalid)
    think_with_file().check().unwrap();
    research_in_projects().check().unwrap();
    let mut two = think_with_file();
    two.run.prompt.push(Part::File {
        artifact: artifact(),
        name: ".steve/attachments/att_2-notes.md".into(),
    });
    two.check().unwrap();
    n.start_job(cmd("op_ok", think_with_file())).await.unwrap();
    let e = n
        .start_job(cmd("op_rs", research_in_projects()))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let mut unsandboxed = research_in_projects();
    unsandboxed.run.policy.sandbox = SandboxRequirement::None;
    n.start_job(cmd("op_r", unsandboxed.clone())).await.unwrap();
    assert_eq!(n.job_spec(&unsandboxed.run.attempt), Some(unsandboxed));
}

// ── running ──

#[tokio::test]
async fn a_job_command_runs_once_and_an_attempt_never_twice() {
    let (n, sink) = connected();
    let c = cmd("op_j", job("j_1", "a_1", "hi"));
    let first = n.start_job(c.clone()).await.unwrap();
    assert_eq!(first.state, Observed::Running);
    assert_eq!(n.start_job(c.clone()).await.unwrap(), first);
    assert_eq!(n.log().iter().filter(|(m, _)| *m == "start_job").count(), 1);
    assert_eq!(sink.kinds(), vec!["attempt_started"]);
    // same id, other work: conflict
    let mut other = c.clone();
    other.body.work = research("j_1", "a_1", "hi").work;
    assert_eq!(
        n.start_job(other).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    // the same attempt under another operation: conflict, even after it ended
    let e = n
        .start_job(cmd("op_j2", job("j_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    n.finish(&exec_job("j_1", "a_1"), done()).await.unwrap();
    let e = n
        .start_job(cmd("op_j3", job("j_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    // the Hub's fallback is a new attempt of the same job
    n.start_job(cmd("op_j4", job("j_1", "a_2", "hi")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_job_streams_its_draft_and_ends_with_its_answer() {
    let (n, sink) = connected();
    n.start_job(cmd("op_r", research("j_r", "a_r", "q")))
        .await
        .unwrap();
    let exec = exec_job("j_r", "a_r");
    n.draft(&exec, "m1", &["先看", "一下"]).await.unwrap();
    // a new message replaces the draft so far
    n.draft(&exec, "m2", &["答案"]).await.unwrap();
    let tool = ToolView {
        id: CallId::new("call_1").unwrap(),
        parent: None,
        origin: ToolOrigin::Harness,
        name: "Read".into(),
        category: ToolCategory::Read,
        title: "src/lib.rs".into(),
        description: None,
        target: Some("src/lib.rs".into()),
        phase: ToolPhase::Succeeded,
        action: None,
        result: vec![],
        error: None,
        duration_ms: Some(3),
    };
    n.emit(
        Subject::Execution(exec.clone()),
        EventBody::ToolChanged { tool },
    )
    .await
    .unwrap();
    let end = report(JobEnd::Done);
    n.finish(&exec, end.clone()).await.unwrap();
    assert_eq!(
        sink.kinds(),
        vec![
            "attempt_started",
            "message_started",
            "text_delta",
            "text_delta",
            "message_started",
            "text_delta",
            "tool_changed",
            "attempt_ended",
        ]
    );
    let offsets: Vec<u64> = sink
        .events()
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TextDelta { utf8_offset, .. } => Some(*utf8_offset),
            _ => None,
        })
        .collect();
    assert_eq!(offsets, vec![0, "先看".len() as u64, 0]);
    let r = n.operation(op("op_r")).await.unwrap();
    assert_eq!(
        r.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Execution(end)),
            error: None,
        }
    );
    // nothing more after the end
    assert_eq!(
        n.draft(&exec, "m3", &["x"]).await.unwrap_err().code,
        ErrorCode::Stale
    );
}

#[tokio::test]
async fn a_failed_research_attempt_keeps_its_report() {
    let (n, _) = connected();
    n.start_job(cmd("op_r", research("j_r", "a_r", "q")))
        .await
        .unwrap();
    let exec = exec_job("j_r", "a_r");
    n.draft(&exec, "m1", &["读到一半"]).await.unwrap();
    let failed = |end: JobEnd| Completion {
        outcome: Outcome::Failed,
        stop_reason: "error".into(),
        error: Some(ApiError::new(ErrorCode::Internal, "harness error")),
        ..report(end)
    };
    // a report that says done next to a failure contradicts itself
    assert_eq!(
        n.finish(&exec, failed(JobEnd::Done))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    assert_eq!(
        n.finish(&exec, report(JobEnd::Failed))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    let end = failed(JobEnd::Failed);
    roundtrip(&end);
    n.finish(&exec, end.clone()).await.unwrap();
    match n.operation(op("op_r")).await.unwrap().state {
        Observed::Finished {
            outcome: Outcome::Failed,
            result: Some(OperationResult::Execution(c)),
            error: Some(e),
        } => {
            assert_eq!(e.safe_detail, "harness error");
            let r = c.job.unwrap();
            assert_eq!(r.end, JobEnd::Failed);
            assert_eq!((r.turns, r.files_read), (3, vec!["src/lib.rs".to_string()]));
            assert!(r.sandboxed);
        }
        s => panic!("{s:?}"),
    }
}

#[tokio::test]
async fn cancel_ends_a_job_as_cancelled_with_its_draft() {
    let (n, _) = connected();
    n.set_capacity(Capacity {
        sessions: 0,
        jobs: 2,
        execs: 0,
    });
    n.start_job(cmd("op_t", job("j_t", "a_t", "x")))
        .await
        .unwrap();
    n.start_job(cmd("op_r", research("j_r", "a_r", "x")))
        .await
        .unwrap();
    let think = exec_job("j_t", "a_t");
    let res = exec_job("j_r", "a_r");
    n.draft(&res, "m1", &["写到一半"]).await.unwrap();
    // nothing to end yet
    assert!(n.settle_jobs().await.unwrap().is_empty());
    for (c, target) in [("op_c1", "op_t"), ("op_c2", "op_r")] {
        n.cancel(cmd(
            c,
            Cancel {
                target: op(target),
                reason: "stop".into(),
            },
        ))
        .await
        .unwrap();
    }
    let mut ended = n.settle_jobs().await.unwrap();
    ended.sort_by(|a, b| a.attempt().cmp(b.attempt()));
    assert_eq!(ended, vec![res.clone(), think.clone()]);
    let completion = |o: &str| match n.receipt(&op(o)).unwrap().state {
        Observed::Finished {
            outcome,
            result: Some(OperationResult::Execution(c)),
            ..
        } => {
            assert_eq!(outcome, c.outcome);
            c
        }
        s => panic!("{s:?}"),
    };
    let t = completion("op_t");
    assert_eq!(
        (t.outcome, t.job, t.result),
        (Outcome::Cancelled, None, vec![])
    );
    let r = completion("op_r");
    assert_eq!(r.outcome, Outcome::Cancelled);
    assert_eq!(r.result, text("写到一半"));
    assert_eq!(r.job.unwrap().end, JobEnd::Stopped);
    // their capacity is free again
    n.start_job(cmd("op_t2", job("j_t", "a_t2", "x")))
        .await
        .unwrap();
    n.start_job(cmd("op_t3", job("j_t", "a_t3", "x")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_job_past_its_deadline_times_out() {
    let (n, _) = connected();
    let mut t = job("j_t", "a_t", "x");
    t.run.limits.deadline = Timestamp(5_000);
    let mut r = research("j_r", "a_r", "x");
    r.run.limits.deadline = Timestamp(5_000);
    n.start_job(cmd("op_t", t)).await.unwrap();
    n.start_job(cmd("op_r", r)).await.unwrap();
    n.start_job(cmd("op_late", job("j_l", "a_l", "x")))
        .await
        .unwrap();
    n.set_now(5_000);
    let ended = n.settle_jobs().await.unwrap();
    assert_eq!(ended.len(), 2);
    let state = |o: &str| n.receipt(&op(o)).unwrap().state;
    match state("op_t") {
        Observed::Finished {
            outcome: Outcome::TimedOut,
            result: Some(OperationResult::Execution(c)),
            error: Some(e),
        } => {
            assert_eq!(e.code, ErrorCode::Expired);
            assert_eq!(c.job, None);
        }
        s => panic!("{s:?}"),
    }
    match state("op_r") {
        Observed::Finished {
            outcome: Outcome::TimedOut,
            result: Some(OperationResult::Execution(c)),
            ..
        } => assert_eq!(
            c.job.unwrap().end,
            JobEnd::Budget {
                hit: BudgetHit::Time
            }
        ),
        s => panic!("{s:?}"),
    }
    assert_eq!(state("op_late"), Observed::Running);
    // a final state never changes
    assert!(n.settle_jobs().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_job_is_not_settled_but_abandoned() {
    let (n, _) = connected();
    let mut j = job("j_1", "a_1", "x");
    j.run.limits.deadline = Timestamp(5_000);
    n.start_job(cmd("op_j", j)).await.unwrap();
    n.lose_track(&exec_job("j_1", "a_1")).unwrap();
    n.set_now(6_000);
    assert!(n.settle_jobs().await.unwrap().is_empty());
    assert_eq!(
        n.receipt(&op("op_j")).unwrap().state.tag(),
        StateTag::Unknown
    );
    n.abandon(cmd(
        "op_ab",
        Abandon {
            target: op("op_j"),
            reason: "hub restarted".into(),
        },
    ))
    .await
    .unwrap();
    assert!(matches!(
        n.receipt(&op("op_j")).unwrap().state,
        Observed::Finished {
            outcome: Outcome::Interrupted,
            ..
        }
    ));
}

// ── the crate itself ──

#[test]
fn the_contract_has_no_store_core_or_agent_protocol_dependency() {
    let manifest = include_str!("../Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|s| s.split("\n[").next())
        .unwrap();
    for line in deps
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let name = line.split(['=', ' ']).next().unwrap();
        for banned in [
            "steve-store",
            "steve-core",
            "steve-acp",
            "rmcp",
            "steve-stats",
        ] {
            assert_ne!(name, banned, "steve-node-api must not depend on {banned}");
        }
        assert!(
            !name.starts_with("steve-"),
            "unexpected Steve dependency {name}"
        );
    }
}
