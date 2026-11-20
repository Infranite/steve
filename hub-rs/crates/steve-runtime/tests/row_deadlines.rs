use steve_runtime::due::RowDeadlines;

#[test]
fn r3e_fences_are_ready_at_the_exact_deadline_and_replace_explicitly() {
    let rows = RowDeadlines::default();
    assert!(rows.ready("missing", 0));
    assert_eq!(rows.not_before("missing"), None);
    rows.defer("a", 1000);
    assert!(!rows.ready("a", 999));
    assert!(rows.ready("a", 1000));
    assert!(rows.ready("a", 1001));
    rows.defer("a", 2000);
    assert_eq!(rows.not_before("a"), Some(2000));
    rows.defer("a", 500);
    assert!(
        rows.ready("a", 500),
        "the caller may deliberately bring a row forward"
    );
}

#[test]
fn r3e_live_rows_are_retained_without_time_or_cache_policy() {
    let rows = RowDeadlines::default();
    rows.defer("live", 1000);
    rows.defer("gone", i64::MAX);
    rows.retain_live(["live"]);
    assert_eq!(rows.not_before("live"), Some(1000));
    assert!(rows.ready("gone", 0));
    rows.retain_live(std::iter::empty::<&str>());
    assert!(rows.ready("live", 0));
}

#[test]
fn r3e_batch_snapshot_drops_only_elapsed_fences_and_clear_lifts_all() {
    let rows = RowDeadlines::default();
    rows.defer("past", 999);
    rows.defer("at", 1000);
    rows.defer("future", 1001);
    assert_eq!(rows.pending(1000), [("future".to_owned(), 1001)].into());
    assert_eq!(rows.not_before("past"), None);
    assert_eq!(rows.not_before("at"), None);
    rows.clear();
    assert!(rows.pending(0).is_empty());
}

#[test]
fn r3e_independent_rows_can_be_deferred_from_multiple_threads() {
    let rows = std::sync::Arc::new(RowDeadlines::default());
    std::thread::scope(|scope| {
        for n in 0..8 {
            let rows = rows.clone();
            scope.spawn(move || rows.defer(n.to_string(), n + 1));
        }
    });
    assert_eq!(rows.pending(0).len(), 8);
    for n in 0..8 {
        assert_eq!(rows.not_before(&n.to_string()), Some(n + 1));
    }
}
