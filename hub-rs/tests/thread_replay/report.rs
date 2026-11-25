//! Shared output contract for the three replay drivers. Fixture errors always panic;
//! known product discrepancies are pinned in baseline mode and forbidden in strict mode.

#[derive(Default)]
pub struct Report {
    cases: std::collections::BTreeMap<String, Vec<&'static str>>,
    expected: std::collections::BTreeMap<String, Vec<&'static str>>,
}

impl Report {
    pub fn case(
        &mut self,
        id: impl Into<String>,
        checks: &[(&'static str, bool)],
        baseline: &[&'static str],
    ) {
        let id = id.into();
        let mut failures = checks
            .iter()
            .filter_map(|(name, ok)| (!ok).then_some(*name))
            .collect::<Vec<_>>();
        failures.sort_unstable();
        assert!(self.cases.insert(id.clone(), failures).is_none(), "{id}");
        if !baseline.is_empty() {
            let mut baseline = baseline.to_vec();
            baseline.sort_unstable();
            self.expected.insert(id, baseline);
        }
    }

    pub fn finish(self, count: usize) {
        assert_eq!(self.cases.len(), count, "replay inventory changed");
        let mut actual = std::collections::BTreeMap::new();
        for (id, failures) in self.cases {
            if failures.is_empty() {
                println!("W34 {id} PASS");
            } else {
                println!("W34 {id} FAIL {}", failures.join(","));
                actual.insert(id, failures);
            }
        }
        println!(
            "W34 total={count} pass={} fail={}",
            count - actual.len(),
            actual.len()
        );
        if std::env::var_os("STEVE_THREAD_REPLAY_STRICT").is_some() {
            assert!(actual.is_empty(), "W3.4 invariant violations: {actual:?}");
        } else {
            assert_eq!(
                actual, self.expected,
                "baseline changed (fix or regression)"
            );
        }
    }
}
