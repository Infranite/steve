//! Test fixtures for script-workflow ledger integration tests (backend-agnostic `sea-query`).

use sea_query::{Expr, ExprTrait, Query};

use crate::{Result, Store};

/// Overwrites `wf_calls.options_canonical_json` to simulate a corrupted ledger row.
pub fn corrupt_call_options(store: &Store, call_id: &str, raw: &str) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("wf_calls")
                .value("options_canonical_json", raw)
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        Ok(())
    })
}

/// Overwrites `wf_calls.(git_head, git_tree, git_cwd)` to simulate a completed remote-Node output.
pub fn set_call_git_fingerprint(
    store: &Store,
    call_id: &str,
    head: &str,
    tree: &str,
    cwd: &str,
) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("wf_calls")
                .values([
                    ("git_head", head.into()),
                    ("git_tree", tree.into()),
                    ("git_cwd", cwd.into()),
                ])
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        Ok(())
    })
}
