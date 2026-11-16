use serde::Deserialize;
use steve_node_api::{Tokens, UsageReport, merged_usage};

#[derive(Deserialize)]
struct Case {
    name: String,
    fallback_model: String,
    reports: Vec<UsageReport>,
    tokens: Tokens,
    models: Option<Vec<String>>,
}

#[test]
fn accounting_vectors_and_materialized_snapshots_agree() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/accounting.json")).unwrap();
    for c in cases {
        let rows = merged_usage(c.reports, &c.fallback_model);
        let sum = |get: fn(Tokens) -> Option<u64>| {
            rows.iter()
                .filter_map(|r| get(r.tokens))
                .reduce(u64::saturating_add)
        };
        assert_eq!(
            Tokens {
                input: sum(|t| t.input),
                output: sum(|t| t.output),
                cache_read: sum(|t| t.cache_read),
                cache_write: sum(|t| t.cache_write),
                reasoning: sum(|t| t.reasoning)
            },
            c.tokens,
            "{}",
            c.name
        );
        if let Some(models) = c.models {
            assert_eq!(
                rows.iter()
                    .map(|r| r.model.clone().unwrap_or_default())
                    .collect::<Vec<_>>(),
                models,
                "{}",
                c.name
            );
        }
        assert_eq!(
            merged_usage(rows.clone(), &c.fallback_model),
            rows,
            "snapshot {}",
            c.name
        );
    }
}
