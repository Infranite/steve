#![cfg(feature = "conformance")]
use steve_storage_api::verify::{MemHarness, Status, run};
fn verify(loopback: bool) {
    let report = run(&MemHarness::new(loopback));
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(report.items.len(), 25);
    let skipped: Vec<_> = report
        .items
        .iter()
        .filter(|item| item.status == Status::Skipped)
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(
        skipped,
        vec![
            "durability",
            "crash_atomicity",
            "idempotency_after_crash",
            "lease_interruption"
        ]
    );
    assert!(
        report
            .items
            .iter()
            .filter(|item| item.status == Status::Skipped)
            .all(|item| item
                .detail
                .as_ref()
                .is_some_and(|reason| !reason.is_empty()))
    );
    assert_eq!(
        report.failed(),
        0,
        "{:#?}",
        report
            .items
            .iter()
            .filter(|item| item.status == Status::Failed)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.passed(), 21);
    assert_eq!(report.skipped(), 4);
    assert!(!report.all_passed());
    assert_eq!(report.measurements.len(), 2);
    let roundtrip: steve_storage_api::verify::Report =
        serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    assert_eq!(
        roundtrip
            .items
            .iter()
            .map(|item| &item.name)
            .collect::<Vec<_>>(),
        report
            .items
            .iter()
            .map(|item| &item.name)
            .collect::<Vec<_>>()
    );
}
#[test]
fn memory_conformance() {
    verify(false);
}
#[test]
fn loopback_memory_conformance() {
    verify(true);
}
