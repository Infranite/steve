//! Smoke tests against the real adapters, in a throwaway cwd and STEVE_HOME. They cost model calls,
//! so they are ignored by default: `cargo test -p steve-acp --test smoke -- --ignored --nocapture`.
//! The MCP URL is a dead stub; permission requests are recorded and never answered.
mod common;

use std::time::Duration;

use common::*;
use steve_acp::{Acp, AcpConfig};

fn group(pgid: u32) -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args(["-g", &pgid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn transcript_models(acp_id: &str) -> Vec<String> {
    let root = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".claude/projects");
    let mut out = vec![];
    for d in std::fs::read_dir(root).unwrap().flatten() {
        if let Ok(t) = std::fs::read_to_string(d.path().join(format!("{acp_id}.jsonl"))) {
            for l in t
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            {
                if let Some(m) = l.pointer("/message/model").and_then(|m| m.as_str()) {
                    out.push(m.to_owned());
                }
            }
        }
    }
    out
}

async fn smoke(harness: &str, model: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AcpConfig::from_env();
    cfg.steve_home = dir.path().join("steve");
    let h = host();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    h.with(steve_acp::SessionInfo {
        id: "smoke".into(),
        harness: harness.into(),
        cwd: cwd.display().to_string(),
        model: model.map(str::to_owned),
        ..Default::default()
    });
    let acp = Acp::new(cfg, h.clone());
    acp.prompt(
        "smoke",
        "Reply with exactly the word PONG and nothing else. Do not use any tools.",
        "me",
    )
    .await
    .unwrap();
    wait_for("first turn", 240, || {
        h.rec(|r| !r.turn_ends.is_empty() || !r.decisions.is_empty())
    })
    .await;
    h.rec(|r| {
        assert!(
            r.decisions.is_empty(),
            "unexpected permission request: {:?}",
            r.decisions
        );
        assert_eq!(
            r.turn_ends[0].stop_reason, "end_turn",
            "{:?} {:?}",
            r.turn_ends, r.events
        );
        let s = &r.sessions["smoke"];
        assert!(s.acp_session_id.is_some());
        println!(
            "{harness}: acp session {:?}, model {:?}, usage {:?}",
            s.acp_session_id, s.model, r.usage
        );
        assert!(s.model.is_some());
    });
    assert!(
        h.said().replace("|", "").contains("PONG"),
        "said: {}",
        h.said()
    );
    // the model the harness really used, from Claude Code's own transcript
    if harness == "claude-code" {
        let acp_id = h
            .rec(|r| r.sessions["smoke"].acp_session_id.clone())
            .unwrap();
        let used = transcript_models(&acp_id);
        println!("{harness}: transcript models {used:?}");
        assert!(
            !used.is_empty() && used.iter().all(|m| Some(m.as_str()) == model),
            "{used:?}"
        );
    }
    let kinds = h.kinds();
    println!("{harness}: events {kinds:?}");

    // release, then resume the same ACP session with session/load
    let pid = acp.adapter_pid("smoke").unwrap();
    acp.release("smoke");
    wait_for("released", 10, || !alive(pid)).await;
    acp.prompt(
        "smoke",
        "Which single word did you reply with last time? Answer with that word only.",
        "me",
    )
    .await
    .unwrap();
    wait_for("second turn", 240, || {
        h.rec(|r| r.turn_ends.len() == 2 || !r.decisions.is_empty())
    })
    .await;
    h.rec(|r| assert_eq!(r.turn_ends[1].stop_reason, "end_turn", "{:?}", r.events));
    assert!(
        h.said().replace("|", "").matches("PONG").count() >= 2,
        "said: {}",
        h.said()
    );

    if harness == "claude-code" {
        // also after session/load, which resets the adapter to its default model
        let acp_id = h
            .rec(|r| r.sessions["smoke"].acp_session_id.clone())
            .unwrap();
        let used = transcript_models(&acp_id);
        assert!(
            used.len() >= 2 && used.iter().all(|m| Some(m.as_str()) == model),
            "{used:?}"
        );
    }
    let pid = acp.adapter_pid("smoke").unwrap();
    let members = group(pid);
    println!("{harness}: process group {pid}: {members:?}");
    acp.shutdown(Duration::from_secs(3)).await;
    wait_for("group gone", 10, || group(pid).is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn claude_code() {
    // not the adapter's default (kiro/claude-opus-5-5), so the transcript proves the switch
    smoke("claude-code", Some("kiro/claude-sonnet-5-5")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn codex() {
    smoke("codex", None).await;
}
