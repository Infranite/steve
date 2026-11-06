//! A lane's window, kept: [`batch::Lane`] loaded from its snapshot, an event applied, the snapshot and
//! the request and unit rows its actions mean written back, all in one transaction. What has to be done
//! outside the store (merging a unit, releasing one) comes back as [`Work`] for the executor.

use steve_store::lanes::{self, LaneRequest, LaneRow, NewRequest};
use steve_store::{Store, StoreError};

use super::batch::{Action, Event, Lane, Policy};

/// What the executor has to do now, for `lane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    /// Merge unit `unit` of `lane`: its requests, in order.
    Merge {
        lane: String,
        unit: u64,
        requests: Vec<String>,
    },
    /// Release unit `unit` of `lane`: its own requests and the ones put off into it.
    Release {
        lane: String,
        unit: u64,
        requests: Vec<String>,
    },
}

/// `subject_id` of a batch's approval (and failed-release) card.
pub(crate) fn subject(lane: &str, unit: u64) -> String {
    format!("lane:{lane}:{unit}")
}

/// How a request is named on its batch's card and in merge commits.
pub(crate) fn label(r: &LaneRequest) -> String {
    match &r.item_id {
        Some(i) => format!("{i}（{}）", r.branch),
        None => r.branch.clone(),
    }
}

/// The question and text of the approval card of unit `unit`, listing `reqs`.
pub(crate) fn card_text(unit: u64, reqs: &[LaneRequest]) -> (String, String) {
    (
        format!("合并 MQ-{unit}（{} 项）", reqs.len()),
        format!(
            "这一批一起合并、一起发布：\n{}",
            reqs.iter()
                .map(|r| format!("- {}", label(r)))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
}

/// The requests unit `unit` of `lane` is about to merge.
pub(crate) fn merging(
    c: &rusqlite::Connection,
    lane: &str,
    unit: u64,
) -> Result<Vec<LaneRequest>, StoreError> {
    Ok(lanes::requests_of(c, lane, true)?
        .into_iter()
        .filter(|r| r.unit == Some(unit as i64) && r.state == "merging")
        .collect())
}

fn policy(l: &LaneRow) -> Policy {
    let d = Policy::default();
    Policy {
        window_ms: l.window_ms.filter(|w| *w > 0).unwrap_or(d.window_ms),
        max_members: l
            .max_members
            .and_then(|m| usize::try_from(m).ok())
            .filter(|m| *m > 0)
            .unwrap_or(d.max_members),
    }
}

/// The lane's state machine, with the lane's current limits (changed settings apply from now on).
fn load(l: &LaneRow) -> Result<Lane, StoreError> {
    let mut lane: Lane = if l.state.trim() == "{}" || l.state.trim().is_empty() {
        Lane::default()
    } else {
        serde_json::from_str(&l.state)?
    };
    lane.policy = policy(l);
    Ok(lane)
}

/// Apply `ev` to lane `id` inside `tx`; the work it leaves for the executor.
fn apply_in(tx: &steve_store::Tx, id: &str, ev: Event) -> Result<Vec<Work>, StoreError> {
    let row = lanes::lane_get(tx, id)?.ok_or(StoreError::NotFound("合并通道"))?;
    let mut lane = load(&row)?;
    // a card answered since (the answer's own step may not have reached the lane yet): the unit is
    // under way, nothing joins it any more
    if matches!(ev, Event::Arrive { .. })
        && let Some(r) = lane.running.as_ref().filter(|r| !r.approved && !r.merged)
        && lanes::batch_card(tx, &subject(&row.id, r.id), "approve")?
            .is_some_and(|(status, _)| status != "open")
    {
        let unit = r.id;
        lane.apply(Event::Approved { unit });
    }
    let actions = lane.apply(ev);
    let mut work = vec![];
    let mut added = vec![];
    for a in actions {
        match a {
            Action::Add { unit, id: r } => {
                lanes::request_set(tx, &r, "merging", Some(unit as i64), None, None)?;
                if !added.contains(&unit) {
                    added.push(unit);
                }
            }
            Action::Join { unit, id: r } => {
                let opened = lane
                    .collecting
                    .as_ref()
                    .filter(|c| c.id == unit)
                    .map_or_else(steve_store::now, |c| c.opened_at);
                lanes::unit_open(tx, &row.id, unit as i64, opened)?;
                lanes::request_set(tx, &r, "collecting", Some(unit as i64), None, None)?;
            }
            Action::Queue { id: r } => lanes::request_set(tx, &r, "queued", None, None, None)?,
            Action::Close { .. } => {}
            Action::Merge { unit, members } => {
                lanes::unit_open(tx, &row.id, unit as i64, steve_store::now())?;
                lanes::unit_set(tx, &row.id, unit as i64, "merging", None, None, None)?;
                for r in &members {
                    lanes::request_set(tx, r, "merging", Some(unit as i64), None, None)?;
                }
                work.push(Work::Merge {
                    lane: row.id.clone(),
                    unit,
                    requests: members,
                });
            }
            Action::PutOff {
                from,
                into,
                members,
            } => {
                lanes::unit_set(
                    tx,
                    &row.id,
                    from as i64,
                    "done",
                    None,
                    None,
                    Some("put_off"),
                )?;
                for r in &members {
                    lanes::request_set(tx, r, "merged", None, Some(into as i64), None)?;
                }
            }
            Action::Release { unit, members } => {
                lanes::unit_set(tx, &row.id, unit as i64, "releasing", None, None, None)?;
                for r in &members {
                    lanes::request_set(tx, r, "merged", None, Some(unit as i64), None)?;
                }
                work.push(Work::Release {
                    lane: row.id.clone(),
                    unit,
                    requests: members,
                });
            }
        }
    }
    // the one card of a unit that took more requests lists them: a new card in place of the open one
    // (none yet: it is made with them, [`ask`])
    for unit in added {
        let (q, d) = card_text(unit, &merging(tx, &row.id, unit)?);
        lanes::batch_card_renew(tx, &subject(&row.id, unit), &q, &d)?;
    }
    lanes::lane_save(tx, &row.id, &serde_json::to_string(&lane)?)?;
    Ok(work)
}

/// The approval card of unit `unit` of `lane`, made listing the requests it holds, read in the same
/// transaction (a request joining meanwhile either is listed or finds the card and renews it). Those
/// requests; empty when nothing is to merge or its card is already open.
pub fn ask(store: &Store, lane: &str, unit: u64) -> Result<Vec<LaneRequest>, StoreError> {
    store.transaction(|tx| {
        let reqs = merging(tx, lane, unit)?;
        let open = lanes::batch_card(tx, &subject(lane, unit), "approve")?
            .is_some_and(|(status, _)| status == "open");
        if reqs.is_empty() || open {
            return Ok(vec![]);
        }
        let ws = reqs
            .iter()
            .find_map(|r| r.workspace_id.clone())
            .unwrap_or_else(|| steve_store::WS.into());
        let (question, draft) = card_text(unit, &reqs);
        steve_store::decision_create_in(
            tx,
            &steve_store::NewDecision {
                workspace_id: ws,
                kind: "approve".into(),
                subject_type: "delivery_batch".into(),
                subject_id: subject(lane, unit),
                question,
                draft: Some(draft),
                options: Some(vec!["yes".into(), "no".into()]),
                requested_by: "steve".into(),
                ..Default::default()
            },
        )?;
        Ok(reqs)
    })
}

/// A merge request into target (`upstream`, `branch`): recorded and put into its lane.
pub fn request(
    store: &Store,
    upstream: &str,
    branch: &str,
    r: &NewRequest<'_>,
) -> Result<(LaneRequest, Vec<Work>), StoreError> {
    store.transaction(|tx| {
        let lane = lanes::lane_for(tx, upstream, branch)?;
        let req = lanes::request_add(
            tx,
            &NewRequest {
                lane_id: &lane.id,
                ..*r
            },
        )?;
        let work = apply_in(
            tx,
            &lane.id,
            Event::Arrive {
                id: req.id.clone(),
                at: req.arrived,
            },
        )?;
        let req = lanes::request_get(tx, &req.id)?.expect("just added");
        Ok((req, work))
    })
}

/// Unit `unit` of `lane` merged.
pub fn merged(store: &Store, lane: &str, unit: u64) -> Result<Vec<Work>, StoreError> {
    store.transaction(|tx| {
        apply_in(
            tx,
            lane,
            Event::Merged {
                unit,
                at: steve_store::now(),
            },
        )
    })
}

/// Unit `unit` of `lane` released (`outcome`: ok, failed, …; `version` when one was cut).
pub fn released(
    store: &Store,
    lane: &str,
    unit: u64,
    outcome: &str,
    version: Option<&str>,
) -> Result<Vec<Work>, StoreError> {
    store.transaction(|tx| {
        let members: Vec<LaneRequest> = lanes::requests_of(tx, lane, true)?
            .into_iter()
            .filter(|r| r.released_in == Some(unit as i64))
            .collect();
        for r in &members {
            let state = if outcome == "ok" {
                "released"
            } else {
                "merged"
            };
            lanes::request_set(tx, &r.id, state, None, None, None)?;
        }
        lanes::unit_set(tx, lane, unit as i64, "done", None, version, Some(outcome))?;
        apply_in(
            tx,
            lane,
            Event::Released {
                unit,
                at: steve_store::now(),
            },
        )
    })
}

/// Request `request` left unit `unit` of `lane` before it merged (`why`: a conflict, a failed check, …).
pub fn left(
    store: &Store,
    lane: &str,
    unit: u64,
    request: &str,
    why: &str,
) -> Result<Vec<Work>, StoreError> {
    store.transaction(|tx| {
        lanes::request_set(tx, request, "left", None, None, Some(why))?;
        let work = apply_in(
            tx,
            lane,
            Event::Left {
                unit,
                id: request.into(),
                at: steve_store::now(),
            },
        )?;
        // a unit the lane no longer holds (every member left, nothing to release) is over
        let row = lanes::lane_get(tx, lane)?.ok_or(StoreError::NotFound("合并通道"))?;
        let l = load(&row)?;
        let held = |u: &Option<super::batch::Unit>| u.as_ref().is_some_and(|u| u.id == unit);
        if !held(&l.running) && !held(&l.collecting) {
            lanes::unit_set(tx, lane, unit as i64, "done", None, None, Some("emptied"))?;
        }
        Ok(work)
    })
}

/// A unit's release failed: its requests stay with it (merged, waiting), the unit says so; nothing
/// moves on until you retry or give up ([`gave_up`]).
pub fn release_failed(store: &Store, lane: &str, unit: u64, why: &str) -> Result<(), StoreError> {
    store.transaction(|tx| {
        lanes::unit_set(
            tx,
            lane,
            unit as i64,
            "release_failed",
            None,
            None,
            Some(why),
        )?;
        Ok(())
    })
}

/// You gave up on a unit's failed release: its requests leave (merged, not released), the lane moves on.
pub fn gave_up(store: &Store, lane: &str, unit: u64) -> Result<Vec<Work>, StoreError> {
    store.transaction(|tx| {
        for r in lanes::requests_of(tx, lane, true)?
            .into_iter()
            .filter(|r| r.released_in == Some(unit as i64))
        {
            lanes::request_set(
                tx,
                &r.id,
                "left",
                None,
                None,
                Some("发布失败，你放弃了这次发布"),
            )?;
        }
        lanes::unit_set(tx, lane, unit as i64, "done", None, None, Some("failed"))?;
        apply_in(
            tx,
            lane,
            Event::Released {
                unit,
                at: steve_store::now(),
            },
        )
    })
}

/// You approved the merge of unit `unit` of `lane`: it is under way.
pub fn approved(store: &Store, lane: &str, unit: u64) -> Result<(), StoreError> {
    store.transaction(|tx| apply_in(tx, lane, Event::Approved { unit }).map(|_| ()))
}

/// "Merge and release now" on `lane`: its collecting batch closes at once. Whether there was one
/// to close, and the work that leaves.
pub fn now(store: &Store, lane: &str) -> Result<(bool, Vec<Work>), StoreError> {
    store.transaction(|tx| {
        let row = lanes::lane_get(tx, lane)?.ok_or(StoreError::NotFound("合并通道"))?;
        let open = load(&row)?.collecting.is_some_and(|c| !c.closed);
        let work = apply_in(
            tx,
            lane,
            Event::Now {
                at: steve_store::now(),
            },
        )?;
        Ok((open, work))
    })
}

/// A lane's limits, as you set them (`None`: the default); the lane is made when it isn't there yet.
pub fn set_policy(
    store: &Store,
    upstream: &str,
    branch: &str,
    window_ms: Option<i64>,
    max_members: Option<i64>,
) -> Result<LaneRow, StoreError> {
    store.transaction(|tx| {
        let l = lanes::lane_for(tx, upstream, branch)?;
        lanes::lane_policy_set(tx, &l.id, window_ms, max_members)?;
        Ok(lanes::lane_get(tx, &l.id)?.expect("just set"))
    })
}

/// What the project page shows of the lane of (`upstream`, `branch`): its limits (yours and the
/// defaults), the unit in flight and the batch collecting, each with its requests. `None`: no
/// request ever went into it and no limit was set.
pub fn view(
    store: &Store,
    upstream: &str,
    branch: &str,
) -> Result<Option<serde_json::Value>, StoreError> {
    use serde_json::json;
    store.reader(|c| {
        let Some(row) = lanes::lanes_all(c)?
            .into_iter()
            .find(|l| l.upstream == upstream && l.branch == branch)
        else {
            return Ok(None);
        };
        let lane = load(&row)?;
        let reqs = lanes::requests_of(c, &row.id, true)?;
        let named = |ids: &[String]| -> Vec<serde_json::Value> {
            ids.iter()
                .filter_map(|id| reqs.iter().find(|r| &r.id == id))
                .map(|r| json!({"id": r.id, "item_id": r.item_id, "branch": r.branch, "label": label(r)}))
                .collect()
        };
        let unit = |u: &super::batch::Unit| {
            json!({
                "no": u.id, "members": named(&u.members), "deferred": named(&u.deferred),
                "opened_at": u.opened_at, "closed": u.closed, "merged": u.merged, "approved": u.approved,
            })
        };
        let d = Policy::default();
        Ok(Some(json!({
            "id": row.id,
            "policy": {"window_ms": lane.policy.window_ms, "max_members": lane.policy.max_members},
            "set": {"window_ms": row.window_ms, "max_members": row.max_members},
            "defaults": {"window_ms": d.window_ms, "max_members": d.max_members},
            "running": lane.running.as_ref().map(unit),
            "collecting": lane.collecting.as_ref().map(|u| {
                let mut v = unit(u);
                v["closes_at"] = json!(u.opened_at + lane.policy.window_ms);
                v
            }),
            "queued": named(&lane.queue.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()),
        })))
    })
}

/// Time passed: every lane whose collecting batch is due closes it (and starts it when it may).
pub fn tick(store: &Store) -> Result<Vec<Work>, StoreError> {
    store.transaction(|tx| {
        let mut work = vec![];
        for l in lanes::lanes_all(tx)? {
            work.extend(apply_in(
                tx,
                &l.id,
                Event::Tick {
                    at: steve_store::now(),
                },
            )?);
        }
        Ok(work)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().join("s.sqlite3"), steve_store::NoPublisher).unwrap();
        (d, s)
    }

    fn req<'a>(id: &'a str) -> NewRequest<'a> {
        NewRequest {
            lane_id: "",
            workspace_id: Some("default"),
            item_id: Some(id),
            source: "item",
            dir: "/tmp/x",
            branch: id,
            head: "abc",
            repo: None,
        }
    }

    fn state(s: &Store, id: &str) -> (String, Option<i64>, Option<i64>) {
        let r = s.reader(|c| lanes::request_get(c, id)).unwrap().unwrap();
        (r.state, r.unit, r.released_in)
    }

    #[test]
    fn the_window_is_kept_across_calls_and_its_rows_follow() {
        let (_d, s) = store();
        let (a, w) = request(&s, "github.com/x/y", "main", &req("ST-1")).unwrap();
        assert!(matches!(&w[..], [Work::Merge { unit: 1, .. }]));
        assert_eq!(state(&s, &a.id), ("merging".into(), Some(1), None));
        approved(&s, &a.lane_id, 1).unwrap();
        let (b, w) = request(&s, "github.com/x/y", "main", &req("ST-2")).unwrap();
        assert!(w.is_empty(), "B collects");
        assert_eq!(state(&s, &b.id), ("collecting".into(), Some(2), None));
        // another target: a lane of its own, straight through
        let (_, w) = request(&s, "github.com/x/z", "main", &req("ST-3")).unwrap();
        assert!(matches!(&w[..], [Work::Merge { unit: 1, .. }]));
        // A merges: its release is put off into B's batch
        let w = merged(&s, &a.lane_id, 1).unwrap();
        assert!(w.is_empty());
        assert_eq!(state(&s, &a.id), ("merged".into(), Some(1), Some(2)));
        // B's window ends: B merges, then the batch releases A and B
        s.with_conn(|c| {
            c.execute("UPDATE lanes SET window_ms = 1 WHERE id = ?", [&a.lane_id])?;
            Ok(())
        })
        .unwrap();
        // the clock counts milliseconds: the tick must come at least one after the batch opened
        let opened_by = steve_store::now();
        while steve_store::now() <= opened_by {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let w = tick(&s).unwrap();
        assert_eq!(
            w,
            [Work::Merge {
                lane: a.lane_id.clone(),
                unit: 2,
                requests: vec![b.id.clone()]
            }]
        );
        let w = merged(&s, &a.lane_id, 2).unwrap();
        assert_eq!(
            w,
            [Work::Release {
                lane: a.lane_id.clone(),
                unit: 2,
                requests: vec![a.id.clone(), b.id.clone()]
            }]
        );
        released(&s, &a.lane_id, 2, "ok", Some("0.9.1")).unwrap();
        assert_eq!(state(&s, &a.id).0, "released");
        assert_eq!(state(&s, &b.id).0, "released");
        let units = s.reader(|c| lanes::units_of(c, &a.lane_id)).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|u| (
                    u.no,
                    u.state.as_str(),
                    u.outcome.as_deref(),
                    u.version.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                (1, "done", Some("put_off"), None),
                (2, "done", Some("ok"), Some("0.9.1"))
            ]
        );
    }

    #[test]
    fn a_request_that_leaves_is_recorded_with_why() {
        let (_d, s) = store();
        let (a, _) = request(&s, "u", "main", &req("ST-1")).unwrap();
        let w = left(&s, &a.lane_id, 1, &a.id, "冲突：src/x.rs").unwrap();
        assert!(w.is_empty());
        let r = s.reader(|c| lanes::request_get(c, &a.id)).unwrap().unwrap();
        assert_eq!(
            (r.state.as_str(), r.reason.as_deref()),
            ("left", Some("冲突：src/x.rs"))
        );
        // the lane is free again: the next request goes straight through
        let (_, w) = request(&s, "u", "main", &req("ST-2")).unwrap();
        assert!(matches!(&w[..], [Work::Merge { unit: 2, .. }]));
    }

    fn card(s: &Store, lane: &str, unit: u64) -> steve_store::Decision {
        s.decisions()
            .create(&steve_store::NewDecision {
                workspace_id: "default".into(),
                kind: "approve".into(),
                subject_type: "delivery_batch".into(),
                subject_id: subject(lane, unit),
                question: "合并 MQ-1（1 项）".into(),
                requested_by: "steve".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap()
    }

    fn open_card(s: &Store, lane: &str, unit: u64) -> Option<steve_store::Decision> {
        s.decisions()
            .open("default")
            .unwrap()
            .into_iter()
            .find(|d| d.subject_type == "delivery_batch" && d.subject_id == subject(lane, unit))
    }

    #[test]
    fn a_request_while_the_card_is_open_joins_the_unit_and_a_new_card_lists_it() {
        let (_d, s) = store();
        let (a, _) = request(&s, "u", "main", &req("ST-1")).unwrap();
        let d = card(&s, &a.lane_id, 1);
        let (b, w) = request(&s, "u", "main", &req("ST-2")).unwrap();
        assert!(w.is_empty(), "the card is already there");
        assert_eq!(state(&s, &b.id), ("merging".into(), Some(1), None));
        // the card you had loaded is no longer open: no answer to it merges ST-2 unseen
        let old = s.decisions().get(&d.id).unwrap().unwrap();
        assert_eq!(old.status, "expired");
        let d = open_card(&s, &a.lane_id, 1).expect("a new card");
        assert_eq!(d.question, "合并 MQ-1（2 项）");
        assert!(
            d.draft.contains("ST-1") && d.draft.contains("ST-2"),
            "{}",
            d.draft
        );
        assert_eq!(d.options, vec!["yes".to_string(), "no".to_string()]);
        // answered (its step not run yet): under way, the next one collects
        s.decisions()
            .resolve(
                &d.id,
                &serde_json::json!({"approved": true}),
                "resolved",
                &serde_json::Value::Null,
            )
            .unwrap();
        let (c, _) = request(&s, "u", "main", &req("ST-3")).unwrap();
        assert_eq!(state(&s, &c.id), ("collecting".into(), Some(2), None));
        let d = s.decisions().get(&d.id).unwrap().unwrap();
        assert_eq!(
            (d.status.as_str(), d.question.as_str()),
            ("resolved", "合并 MQ-1（2 项）"),
            "an answered card is not replaced"
        );
    }

    #[test]
    fn a_request_that_joins_before_the_card_is_made_is_listed_on_it() {
        let (_d, s) = store();
        let (a, w) = request(&s, "u", "main", &req("ST-1")).unwrap();
        assert!(matches!(w.as_slice(), [Work::Merge { unit: 1, .. }]));
        // between the unit starting and its card: ST-2 goes in with it
        let (b, _) = request(&s, "u", "main", &req("ST-2")).unwrap();
        assert_eq!(state(&s, &b.id), ("merging".into(), Some(1), None));
        let listed = ask(&s, &a.lane_id, 1).unwrap();
        assert_eq!(listed.len(), 2);
        let d = open_card(&s, &a.lane_id, 1).expect("the card");
        assert_eq!(d.question, "合并 MQ-1（2 项）");
        assert!(d.draft.contains("ST-1") && d.draft.contains("ST-2"));
        // asked again (recovery, a second trigger): the one card stays
        assert!(ask(&s, &a.lane_id, 1).unwrap().is_empty());
        let n = s
            .decisions()
            .open("default")
            .unwrap()
            .iter()
            .filter(|d| d.subject_type == "delivery_batch")
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn now_closes_the_batch_and_the_limits_are_yours_to_set() {
        let (_d, s) = store();
        let l = set_policy(&s, "u", "main", Some(60_000), Some(3)).unwrap();
        assert_eq!((l.window_ms, l.max_members), (Some(60_000), Some(3)));
        let (a, _) = request(&s, "u", "main", &req("ST-1")).unwrap();
        assert_eq!(a.lane_id, l.id, "the same lane");
        approved(&s, &a.lane_id, 1).unwrap();
        let (b, _) = request(&s, "u", "main", &req("ST-2")).unwrap();
        let v = view(&s, "u", "main").unwrap().unwrap();
        assert_eq!(v["policy"]["window_ms"], 60_000);
        assert_eq!(v["defaults"]["window_ms"], 2 * 3_600_000);
        assert_eq!(
            v["collecting"]["closes_at"],
            v["collecting"]["opened_at"].as_i64().unwrap() + 60_000
        );
        assert_eq!(v["collecting"]["members"][0]["id"], b.id.as_str());
        assert_eq!(v["running"]["approved"], true);
        let (closed, w) = now(&s, &a.lane_id).unwrap();
        assert!(
            closed && w.is_empty(),
            "A is still merging: B waits for it, not for its window"
        );
        let w = merged(&s, &a.lane_id, 1).unwrap();
        assert_eq!(
            w,
            [Work::Merge {
                lane: a.lane_id.clone(),
                unit: 2,
                requests: vec![b.id.clone()]
            }]
        );
        assert_eq!(
            now(&s, &a.lane_id).unwrap(),
            (false, vec![]),
            "nothing left to close"
        );
        assert!(view(&s, "u", "other").unwrap().is_none());
    }
}
