use super::*;
use serde_json::json;
use std::io::Write;

fn tick(ms: u64, total: Value, last: Value) -> String {
    format!(
        "{}\n",
        json!({"timestamp": format!("1970-01-01T00:00:{:02}.{:03}Z", ms / 1000, ms % 1000),
        "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": total, "last_token_usage": last}}})
    )
}
fn counts(i: u64, c: u64, o: u64, r: u64) -> Value {
    json!({"input_tokens":i,"cached_input_tokens":c,"output_tokens":o,"reasoning_output_tokens":r})
}

#[test]
fn cache_creation_presence_zero_positive_delta_reset_replay_and_exclusive_input() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let c = |i, c, o, r, w| {
        let mut v = counts(i, c, o, r);
        v["cache_write_input_tokens"] = json!(w);
        v
    };
    append(&p, &tick(10, c(1000, 400, 100, 20, 50), Value::Null));
    let mut r = reader(home.path(), 20);
    let first = tick(30, c(1107, 460, 120, 25, 57), c(107, 60, 20, 5, 7));
    append(&p, &first);
    append(&p, &first);
    r.poll();
    assert_eq!(
        r.tokens(),
        Tokens {
            input: Some(40),
            cache_read: Some(60),
            cache_write: Some(7),
            output: Some(20),
            reasoning: Some(5)
        }
    );
    let reset = tick(40, c(50, 20, 10, 2, 0), c(50, 20, 10, 2, 0));
    append(&p, &reset);
    append(&p, &reset);
    r.poll();
    assert_eq!(r.tokens().cache_write, Some(7));
    assert_eq!(r.tokens().input, Some(70));
    fs::write(&p, format!("{first}{reset}")).unwrap();
    r.poll();
    assert_eq!(r.tokens().cache_write, Some(7));
    assert_eq!(r.tokens().input, Some(70));
    append(&p, &tick(50, c(83, 30, 15, 3, 3), c(33, 10, 5, 1, 3)));
    r.poll();
    assert_eq!(r.tokens().cache_write, Some(10));
    assert_eq!(r.tokens().input, Some(90));
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    append(&p, &tick(30, c(100, 60, 20, 5, 0), c(100, 60, 20, 5, 0)));
    r.poll();
    assert_eq!(r.tokens().cache_write, Some(0));
    assert_eq!(r.tokens().input, Some(40));
}
fn path(home: &Path, sid: &str) -> PathBuf {
    let day = home.join("sessions/1970/01/01");
    fs::create_dir_all(&day).unwrap();
    day.join(format!("rollout-t-{sid}.jsonl"))
}
fn append(path: &Path, text: &str) {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}
fn reader(home: &Path, started: i64) -> Reader {
    let mut r = Reader::new(home.into(), "same-session".into(), started);
    r.prime();
    r
}

#[test]
fn baseline_live_final_replay_and_cache_reasoning_partition() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    append(&p, &tick(10, counts(5000, 3000, 200, 100), Value::Null));
    let mut r = reader(home.path(), 20);
    assert_eq!(r.tokens(), Tokens::default());
    let event = tick(30, counts(5100, 3060, 230, 110), counts(100, 60, 30, 10));
    append(&p, &event);
    r.poll();
    let live = Tokens {
        input: Some(40),
        cache_read: Some(60),
        output: Some(30),
        reasoning: Some(10),
        cache_write: None,
    };
    assert_eq!(r.tokens(), live);
    append(&p, &event);
    append(
        &p,
        &tick(31, counts(5100, 3060, 230, 110), counts(100, 60, 30, 10)),
    );
    r.poll();
    assert_eq!(r.tokens(), live);
    append(
        &p,
        &tick(40, counts(5200, 3100, 250, 115), counts(100, 40, 20, 5)),
    );
    r.poll();
    let t = r.tokens();
    assert_eq!(
        (t.input, t.cache_read, t.output, t.reasoning),
        (Some(100), Some(100), Some(50), Some(15))
    );
    assert_eq!(
        t.input.unwrap() + t.cache_read.unwrap() + t.output.unwrap(),
        250
    ); // no extra reasoning
}

#[test]
fn multiple_resets_dedupe_each_tick_instead_of_summing_repeated_last() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    append(&p, &tick(10, counts(1000, 400, 100, 20), Value::Null));
    let mut r = reader(home.path(), 20);
    let a = tick(30, counts(1100, 460, 120, 25), counts(100, 60, 20, 5));
    let b = tick(40, counts(50, 20, 10, 4), counts(50, 20, 10, 4));
    let c = tick(50, counts(80, 30, 15, 5), counts(30, 10, 5, 1));
    let d = tick(60, counts(10, 0, 3, 1), counts(10, 0, 3, 1));
    append(&p, &format!("{a}{b}{b}{a}{c}{d}{d}"));
    r.poll();
    assert_eq!(
        r.tokens(),
        Tokens {
            input: Some(100),
            cache_read: Some(90),
            output: Some(38),
            reasoning: Some(11),
            cache_write: None
        }
    );
}

#[test]
fn missing_is_unknown_and_explicit_zero_is_zero() {
    let home = tempfile::tempdir().unwrap();
    let mut r = reader(home.path(), 20);
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
    let p = path(home.path(), "same-session");
    append(&p, &tick(30, counts(0, 0, 0, 0), counts(0, 0, 0, 0)));
    r.poll();
    assert_eq!(
        r.tokens(),
        Tokens {
            input: Some(0),
            cache_read: Some(0),
            output: Some(0),
            reasoning: Some(0),
            cache_write: None
        }
    );
    let missing = json!({"input_tokens":100,"output_tokens":5});
    let mut r = reader(home.path(), 40);
    append(&p, &tick(50, missing.clone(), missing));
    r.poll();
    assert_eq!(r.tokens().input, None);
    assert_eq!(r.tokens().cache_read, None);
    assert_eq!(r.tokens().output, Some(5));
    assert_eq!(r.tokens().reasoning, None);
}

#[test]
fn reset_without_last_keeps_unknown_fields_instead_of_claiming_zero() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    append(&p, &tick(10, counts(1000, 400, 100, 20), Value::Null));
    let mut r = reader(home.path(), 20);
    append(&p, &tick(30, counts(5, 0, 2, 1), Value::Null));
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
}

#[test]
fn late_file_cannot_book_a_resumed_session_lifetime() {
    let home = tempfile::tempdir().unwrap();
    let mut r = reader(home.path(), 20);
    let p = path(home.path(), "same-session");
    append(&p, &tick(10, counts(10000, 5000, 1000, 200), Value::Null));
    append(
        &p,
        &tick(30, counts(10100, 5060, 1020, 205), counts(100, 60, 20, 5)),
    );
    r.poll();
    assert_eq!(r.tokens().input, Some(40));
    assert_eq!(r.tokens().output, Some(20));
}

#[test]
fn partial_tail_is_only_counted_after_the_record_is_complete() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    let line = tick(30, counts(100, 60, 20, 5), counts(100, 60, 20, 5));
    let cut = line.len() / 2;
    append(&p, &line[..cut]);
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
    append(&p, &line[cut..]);
    r.poll();
    assert_eq!(r.tokens().input, Some(40));
}

#[test]
fn same_id_in_two_homes_and_concurrent_attempts_are_isolated() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let pa = path(a.path(), "same-session");
    let pb = path(b.path(), "same-session");
    let mut ra = reader(a.path(), 20);
    let mut rb = reader(b.path(), 20);
    append(&pa, &tick(30, counts(10, 0, 3, 1), counts(10, 0, 3, 1)));
    append(
        &pb,
        &tick(30, counts(100, 40, 30, 10), counts(100, 40, 30, 10)),
    );
    let (ra, rb) = std::thread::scope(|s| {
        let a = s.spawn(move || {
            ra.poll();
            ra
        });
        let b = s.spawn(move || {
            rb.poll();
            rb
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(
        find(a.path(), "same-session"),
        find(b.path(), "same-session")
    );
    assert_eq!(ra.tokens().input, Some(10));
    assert_eq!(rb.tokens().input, Some(60));
}

#[test]
fn file_replacement_and_replay_do_not_rebook_the_previous_epoch() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    let a = tick(30, counts(100, 40, 20, 5), counts(100, 40, 20, 5));
    append(&p, &a);
    r.poll();
    let replacement = p.with_extension("new");
    fs::write(&replacement, &a).unwrap();
    fs::rename(replacement, &p).unwrap();
    r.poll();
    assert_eq!(r.tokens().input, Some(60));
    append(&p, &tick(40, counts(10, 0, 3, 1), counts(10, 0, 3, 1)));
    r.poll();
    assert_eq!(r.tokens().input, Some(70));
    assert_eq!(r.tokens().output, Some(23));
}

#[test]
fn task_boundary_and_wrong_session_metadata_prevent_contamination() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    let marker = |kind, id| {
        format!(
            "{}\n",
            json!({"timestamp":"1970-01-01T00:00:00.021Z","type":"event_msg","payload":{"type":kind,"turn_id":id}})
        )
    };
    append(&p, &marker("task_started", "first"));
    append(&p, &tick(30, counts(10, 0, 3, 1), counts(10, 0, 3, 1)));
    append(
        &p,
        &format!(
            "{}\n",
            json!({"timestamp":"1970-01-01T00:00:00.031Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"first"}})
        ),
    );
    append(
        &p,
        &tick(40, counts(1000, 400, 300, 10), counts(990, 400, 297, 9)),
    );
    r.poll();
    assert_eq!(r.tokens().input, Some(10));
    let other = tempfile::tempdir().unwrap();
    let p = path(other.path(), "same-session");
    fs::write(
        &p,
        format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":"different"}})
        ),
    )
    .unwrap();
    let mut r = reader(other.path(), 20);
    append(&p, &tick(30, counts(10, 0, 3, 1), counts(10, 0, 3, 1)));
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
}

#[test]
fn large_history_and_live_reads_have_fixed_byte_and_memory_bounds() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    fs::write(&p, "x".repeat((PRIME_BYTES * 2) as usize)).unwrap();
    append(&p, "\n");
    append(&p, &tick(10, counts(1000, 400, 100, 20), Value::Null));
    let mut r = reader(home.path(), 20);
    assert_eq!(
        r.previous,
        Some(Counts([Some(1000), Some(400), Some(100), Some(20), None]))
    );
    append(&p, &"x".repeat((POLL_BYTES * 3) as usize));
    append(&p, "\n");
    append(
        &p,
        &tick(30, counts(1100, 460, 120, 25), counts(100, 60, 20, 5)),
    );
    let offset = r.offset;
    r.poll();
    assert!(r.offset - offset <= POLL_BYTES);
    assert!(!r.caught_up);
    assert!(r.pending.len() <= LINE_BYTES);
    for _ in 0..4 {
        r.poll();
    }
    assert!(r.caught_up);
    assert_eq!(r.tokens().input, Some(40));
}

#[test]
fn rfc3339_and_invalid_ids() {
    assert_eq!(parse_ms("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(
        parse_ms("2026-11-01T16:59:20.548Z"),
        Some(1_793_552_360_548)
    );
    assert_eq!(parse_ms("garbage"), None);
    assert_eq!(find(Path::new("/unused"), "../../other"), None);
}

#[test]
fn losing_a_source_after_live_usage_cannot_claim_a_complete_settlement() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    append(
        &p,
        &tick(30, counts(100, 60, 20, 5), counts(100, 60, 20, 5)),
    );
    r.poll();
    assert_eq!(r.tokens().input, Some(40));
    fs::remove_file(p).unwrap();
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
    assert!(r.incomplete);
}

#[test]
fn replay_after_replacement_with_fresh_timestamps_still_deduplicates_the_last_total() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    append(
        &p,
        &tick(30, counts(100, 60, 20, 5), counts(100, 60, 20, 5)),
    );
    r.poll();
    let replacement = p.with_extension("new");
    fs::write(
        &replacement,
        tick(40, counts(100, 60, 20, 5), counts(100, 60, 20, 5)),
    )
    .unwrap();
    fs::rename(replacement, &p).unwrap();
    r.poll();
    assert_eq!(r.tokens().input, Some(40));
    // The replacement's first new counter belongs to a new source: use last, never the
    // unrelated total even when its numeric value happens to exceed the old total.
    append(
        &p,
        &tick(50, counts(1000, 600, 200, 50), counts(50, 20, 10, 2)),
    );
    r.poll();
    assert_eq!(r.tokens().input, Some(70));
    assert_eq!(r.tokens().output, Some(30));
}

#[test]
fn malformed_token_record_leaves_settlement_unknown() {
    let home = tempfile::tempdir().unwrap();
    let p = path(home.path(), "same-session");
    let mut r = reader(home.path(), 20);
    append(
        &p,
        &tick(30, counts(100, 60, 20, 5), counts(100, 60, 20, 5)),
    );
    r.poll();
    append(
        &p,
        "{\"type\":\"token_count\",broken}
",
    );
    r.poll();
    assert_eq!(r.tokens(), Tokens::default());
    assert!(r.incomplete);
}
