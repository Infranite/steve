//! Maps stored release evidence into the read-side closeout view.
use serde_json::Value;
use steve_types::{Closeout, PullSnapshot};

/// The caller selects the release operation of the current item cycle/revision only.
pub fn closeout_from_evidence(
    ev: &Value,
    completed: bool,
    awaiting_approval: bool,
) -> Option<Self> {
    if !ev["pr_slug"].as_str().is_some_and(|s| !s.is_empty())
        || (completed && (ev["pr_merged"] != true || ev["pr_merged_void"] == true))
    {
        return None;
    }
    let s = |key: &str| ev[key].as_str().map(str::to_owned);
    let merged = ev["pr_merged"] == true && ev["pr_merged_void"] != true;
    let mut pr: Option<PullSnapshot> = if ev.get("pr_snapshot").is_some() {
        serde_json::from_value(ev["pr_snapshot"].clone()).ok()
    } else {
        // Pre-upgrade evidence had no check rollup; never show its loose pr_failing field.
        ev["pr"].as_u64().map(|number| PullSnapshot {
            number,
            url: s("pr_url").unwrap_or_default(),
            head: s("pr_head").unwrap_or_default(),
            state: s("pr_state").unwrap_or_default(),
            checks: None,
        })
    };
    if merged && let Some(pr) = &mut pr {
        pr.state = "MERGED".into();
    }
    let sync_error = s("pr_sync_error");
    let (status, next_owner) = if completed {
        ("merged", "none")
    } else if merged {
        ("merged", "steve")
    } else if sync_error.is_some() {
        ("sync_failed", "steve")
    } else if ev["pr_merge_declined"] == true {
        ("manual_merge", "user")
    } else if awaiting_approval {
        ("awaiting_approval", "user")
    } else {
        match ev["pr_progress"].as_str() {
            Some("no_pr") => ("no_pr", "agent"),
            Some("checks_failed") => ("checks_failed", "agent"),
            Some("waiting_checks") => ("waiting_checks", "steve"),
            Some("conflict") => ("conflict", "agent"),
            Some("behind") => ("behind", "agent"),
            Some("draft") => ("draft", "agent"),
            Some("head") => ("head", "agent"),
            Some("uncommitted") => ("uncommitted", "agent"),
            Some("accepted_unknown") => ("accepted_unknown", "user"),
            Some("closed") => ("closed", "user"),
            Some("waiting_github") => ("waiting_github", "steve"),
            Some("waiting_result") => ("waiting_result", "steve"),
            Some("ready") => ("ready", "steve"),
            Some("merge_retry") => ("merge_retry", "steve"),
            Some("merge_method") => ("merge_method", "user"),
            _ => ("syncing", "steve"),
        }
    };
    let merge_error = matches!(status, "merge_retry" | "merge_method")
        .then(|| s("pr_merge_error"))
        .flatten();
    Some(Closeout {
        status,
        next_owner,
        pr,
        last_success_at: ev["pr_looked"].as_i64(),
        sync_error,
        merge_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pre_upgrade_evidence_does_not_claim_to_have_a_check_snapshot() {
        let ev = json!({
            "pr_slug": "acme/app", "pr": 7, "pr_head": "new-head",
            "pr_state": "OPEN", "pr_looked": 42, "pr_failing": ["from another head"]
        });
        let view = closeout_from_evidence(&ev, false, false).unwrap();
        assert_eq!(view.last_success_at, Some(42));
        assert_eq!(view.pr.unwrap().checks, None);
        assert_eq!(view.status, "syncing");
        assert!(closeout_from_evidence(&ev, true, false).is_none());
    }
}
