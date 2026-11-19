//! Inventory ratchet: a new eligible SQL writer needs explicit maintenance and semantic coverage.
//! Text-only/body/association writes intentionally do not count as digest membership changes.
fn statements(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        i += 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
            } else if bytes[i] == b'"' {
                out.push(&source[start..i]);
                i += 1;
                break;
            } else {
                i += 1;
            }
        }
    }
    out
}
fn is_writer(sql: &str) -> bool {
    let s = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if s.starts_with("insert into digest_receipts") {
        return true;
    }
    for table in ["chan_messages", "mail_messages"] {
        if s.starts_with(&format!("insert into {table}"))
            || s.starts_with(&format!("insert or ignore into {table}"))
            || s.starts_with(&format!("delete from {table}"))
        {
            return true;
        }
        if let Some(set) = s.strip_prefix(&format!("update {table} set ")) {
            let fields = set
                .split(" where ")
                .next()
                .unwrap()
                .split(',')
                .filter_map(|v| v.split('=').next())
                .map(str::trim);
            if fields.into_iter().any(|f| {
                [
                    "status",
                    "workspace_id",
                    "space_id",
                    "thread_id",
                    "created",
                    "date",
                ]
                .contains(&f)
            }) {
                return true;
            }
        }
    }
    false
}
#[test]
fn known_source_writers_have_explicit_calls_and_new_writers_require_review() {
    let files = [
        (
            "outbound",
            include_str!("../../steve-channels/src/outbound.rs"),
            19,
            "status_changed",
        ),
        (
            "messages",
            include_str!("../../steve-channels/src/messages.rs"),
            1,
            "enqueue_digestible",
        ),
        (
            "respond",
            include_str!("../../steve-channels/src/respond.rs"),
            1,
            "enqueue_digestible",
        ),
        (
            "presence",
            include_str!("../../steve-channels/src/presence.rs"),
            1,
            "status_changed",
        ),
        (
            "cleanup",
            include_str!("../../steve-channels/src/cleanup.rs"),
            1,
            "remove_message",
        ),
        (
            "mail",
            include_str!("../../steve-mail/src/repo.rs"),
            2,
            "enqueue_digestible",
        ),
        (
            "window",
            include_str!("../../steve-digest/src/window.rs"),
            1,
            "dequeue_on_receipt",
        ),
    ];
    for (name, source, count, call) in files {
        // Unit-test-only SQL below cfg(test) is fixture data, maintained by its own helpers.
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert_eq!(
            statements(production)
                .into_iter()
                .filter(|s| is_writer(s))
                .count(),
            count,
            "{name}: re-review the digest write inventory"
        );
        assert!(
            production.contains(call),
            "{name}: missing explicit digest maintenance"
        );
    }
}
