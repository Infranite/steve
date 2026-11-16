use super::*;
use serde_json::json;
use std::io::Write;

fn path(home: &Path) -> PathBuf {
    let p = home.join("projects/project/s.jsonl");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    p
}
fn row(id: &str, ms: u64, model: &str, input: u64) -> Value {
    json!({"type":"assistant","sessionId":"s","timestamp":format!("1970-01-01T00:00:00.{ms:03}Z"),"message":{"id":id,"model":model,"usage":{"input_tokens":input,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":5,"output_tokens_details":{"thinking_tokens":2}}}})
}
fn append(p: &Path, v: &Value) {
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap(),
        "{v}"
    )
    .unwrap();
}
fn tokens(r: &Reader, model: &str, scope: UsageScope) -> Tokens {
    r.readings()
        .into_iter()
        .find(|r| r.model.as_deref() == Some(model) && r.scope == scope)
        .expect("reading")
        .tokens
}

#[test]
fn baseline_time_session_message_identity_and_synthetic_are_independent_fences() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    append(&p, &row("old", 10, "m", 9000));
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    append(&p, &row("old", 30, "m", 9000)); // old ID with a new timestamp
    append(&p, &row("before", 19, "m", 9000));
    let a = row("a", 30, "m", 10);
    append(&p, &a);
    append(&p, &a);
    let mut wrong = row("other", 31, "m", 9000);
    wrong["sessionId"] = json!("other");
    append(&p, &wrong);
    append(&p, &row("synthetic", 32, "<synthetic>", 0));
    r.poll();
    let expected = Tokens {
        input: Some(10),
        output: Some(5),
        cache_read: Some(0),
        cache_write: Some(0),
        reasoning: Some(2),
    };
    assert_eq!(tokens(&r, "m", UsageScope::Main), expected);
    fs::write(&p, format!("{a}\n{a}\n")).unwrap();
    r.poll();
    assert_eq!(
        tokens(&r, "m", UsageScope::Main),
        expected,
        "truncation/replay is not new consumption"
    );
    let replacement = p.with_extension("new");
    fs::write(&replacement, format!("{a}\n")).unwrap();
    fs::rename(replacement, &p).unwrap();
    r.poll();
    assert_eq!(tokens(&r, "m", UsageScope::Main), expected);
    append(&p, &row("b", 40, "m", 20));
    r.poll();
    assert_eq!(tokens(&r, "m", UsageScope::Main).input, Some(30));
}

#[test]
fn main_children_models_and_explicit_time_end_do_not_mix() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.end_fence.store(40, Ordering::Relaxed);
    r.prime();
    append(&p, &row("a", 30, "m", 10));
    append(&p, &row("padding", 41, "m", 9999));
    let sub = p.with_extension("").join("subagents");
    fs::create_dir_all(&sub).unwrap();
    let mut child = row("child", 35, "m", 20);
    child["isSidechain"] = json!(true);
    append(&sub.join("agent-one.jsonl"), &child);
    append(
        &sub.join("agent-two.jsonl"),
        &row("child2", 36, "other", 30),
    );
    let mut foreign = row("foreign", 36, "m", 9999);
    foreign["sessionId"] = json!("unrelated");
    append(&sub.join("agent-three.jsonl"), &foreign);
    r.poll();
    assert_eq!(tokens(&r, "m", UsageScope::Main).input, Some(10));
    assert_eq!(tokens(&r, "m", UsageScope::All).input, Some(30));
    assert_eq!(tokens(&r, "other", UsageScope::All).input, Some(30));
    assert!(
        !r.readings()
            .iter()
            .any(|r| r.model.as_deref() == Some("other") && r.scope == UsageScope::Main)
    );
}

#[test]
fn next_prompt_seals_old_attempt_but_tool_results_do_not() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    let user = |id: &str, content: Value| json!({"type":"user","sessionId":"s","timestamp":"1970-01-01T00:00:00.030Z","uuid":id,"message":{"content":content}});
    append(&p, &user("prompt", json!("do work")));
    append(&p, &row("a", 31, "m", 10));
    append(&p, &user("tool", json!([{"type":"tool_result"}])));
    append(&p, &row("b", 32, "m", 20));
    append(&p, &user("next", json!("next turn")));
    append(&p, &row("next-reply", 40, "m", 9999));
    r.poll();
    assert_eq!(tokens(&r, "m", UsageScope::Main).input, Some(30));
}

#[test]
fn partial_message_refinement_and_missing_fields_are_not_fabricated_zero() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    let mut a = row("a", 30, "m", 10);
    a["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_creation_input_tokens");
    append(&p, &a);
    r.poll();
    assert_eq!(tokens(&r, "m", UsageScope::Main).cache_write, None);
    a["message"]["usage"]["cache_creation_input_tokens"] = json!(0);
    a["message"]["usage"]["output_tokens"] = json!(7);
    append(&p, &a);
    r.poll();
    let t = tokens(&r, "m", UsageScope::Main);
    assert_eq!(t.cache_write, Some(0));
    assert_eq!(t.output, Some(7));
}

#[test]
fn ambiguous_roots_and_corruption_are_not_complete_consumption() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    append(&p, &row("a", 30, "m", 10));
    let other = d.path().join("projects/other/s.jsonl");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    append(&other, &row("b", 30, "m", 20));
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.poll();
    assert!(r.incomplete);
    assert!(r.readings().is_empty());
    fs::remove_file(other).unwrap();
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    writeln!(
        fs::OpenOptions::new().append(true).open(p).unwrap(),
        "{{broken"
    )
    .unwrap();
    r.poll();
    assert!(r.incomplete);
}

#[test]
fn an_end_fence_rechecks_already_read_source_timestamps() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    append(&p, &row("inside", 30, "m", 10));
    append(&p, &row("padding", 41, "m", 9999));
    r.poll();
    r.end_fence.store(40, Ordering::Relaxed);
    assert_eq!(tokens(&r, "m", UsageScope::Main).input, Some(10));
}

#[test]
fn a_message_mirrored_across_main_and_child_history_is_one_response() {
    let d = tempfile::tempdir().unwrap();
    let p = path(d.path());
    let mut r = Reader::new(d.path().into(), "s".into(), 20);
    r.prime();
    let a = row("same-message", 30, "m", 10);
    append(&p, &a);
    let sub = p.with_extension("").join("subagents");
    fs::create_dir_all(&sub).unwrap();
    let mut mirror = a;
    mirror["isSidechain"] = json!(true);
    append(&sub.join("agent-copy.jsonl"), &mirror);
    r.poll();
    assert!(!r.incomplete);
    assert_eq!(tokens(&r, "m", UsageScope::Main).input, Some(10));
    assert_eq!(tokens(&r, "m", UsageScope::All).input, Some(10));
}
