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
    let actions = lane.apply(ev);
    let mut work = vec![];
    for a in actions {
        match a {
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
    lanes::lane_save(tx, &row.id, &serde_json::to_string(&lane)?)?;
    Ok(work)
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
}
