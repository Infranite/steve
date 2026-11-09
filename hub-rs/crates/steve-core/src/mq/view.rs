//! The merge queue as the project page shows it (S8), and what you do there: the target's mode and
//! permission, its policy (every number, with the defaults), what is queued and under way (with a
//! bisection's halves), the release train with when it closes, and the last week's statistics to
//! tune the policy by. A target is shared by every workspace whose projects merge into it: another
//! workspace's entries are counted, never named.

use serde_json::{Value, json};
use steve_store::mq::{self as rows, MqEntry};

use super::drive::{SUBJECT_TARGET, parse_batch};
use super::policy::Policy;
use super::queue::Event as Qe;
use super::train::Event as Te;
use crate::Core;
use crate::error::{Result, msg};

const WEEK_MS: i64 = 7 * 24 * 3_600_000;

fn avg(v: &[i64]) -> Option<i64> {
    (!v.is_empty()).then(|| v.iter().sum::<i64>() / v.len() as i64)
}

impl Core {
    /// The queue of (`upstream`, `branch`) as workspace `ws` sees it; `None` while the queue is off.
    pub fn mq_view(&self, upstream: &str, branch: &str, ws: &str) -> Result<Option<Value>> {
        if !self.config().mq {
            return Ok(None);
        }
        let s = self.store().store();
        let defaults = serde_json::to_value(Policy::default()).unwrap_or_default();
        let Some(t) = s.reader(|c| rows::target_by(c, upstream, branch))? else {
            return Ok(Some(json!({
                "on": self.mq_on(), "target": null, "policy": defaults, "set": {}, "defaults": defaults,
            })));
        };
        let policy = Policy::of(&t.policy, None);
        let set: Value = serde_json::from_str(&t.policy).unwrap_or(json!({}));
        let entries = s.reader(|c| rows::entries_recent(c, &t.id, 60))?;
        let mine = |e: &MqEntry| e.workspace_id.as_deref() == Some(ws);
        let label = |e: &MqEntry| match (&e.item_id, &e.branch) {
            (Some(i), _) => i.clone(),
            (None, Some(b)) => b.clone(),
            _ => e.id.clone(),
        };
        let entry_json = |e: &MqEntry| {
            json!({
                "id": e.id, "item_id": e.item_id, "label": label(e), "state": e.state,
                "head": e.head.chars().take(12).collect::<String>(), "batch": e.batch_no, "train": e.train_no,
                "pr": e.pr, "reason": e.reason, "arrived": e.arrived, "landed_at": e.landed_at,
                "release_now": e.release_now,
            })
        };
        let names = |ids: &[String]| -> (Vec<Value>, usize) {
            let mut v = vec![];
            let mut others = 0;
            for id in ids {
                match entries.iter().find(|e| &e.id == id) {
                    Some(e) if mine(e) => v.push(json!({"id": e.id, "label": label(e)})),
                    Some(_) => others += 1,
                    None => match s.reader(|c| rows::entry_get(c, id)).ok().flatten() {
                        Some(e) if mine(&e) => v.push(json!({"id": e.id, "label": label(&e)})),
                        _ => others += 1,
                    },
                }
            }
            (v, others)
        };
        let queued: Vec<Value> = entries
            .iter()
            .filter(|e| e.state == "queued" && mine(e))
            .map(entry_json)
            .collect();
        let queued_others = entries
            .iter()
            .filter(|e| e.state == "queued" && !mine(e))
            .count();
        let recent: Vec<Value> = entries
            .iter()
            .filter(|e| mine(e) && e.state != "queued")
            .take(20)
            .map(entry_json)
            .collect();
        let batches: Vec<Value> = s
            .reader(|c| rows::batches_recent(c, &t.id, 20))?
            .into_iter()
            .map(|b| {
                let ids: Vec<String> = serde_json::from_str(&b.members).unwrap_or_default();
                let (members, others) = names(&ids);
                json!({
                    "no": b.no, "parent": b.parent, "root": b.root, "state": b.state, "step": b.step,
                    "members": members, "others": others, "outcome": b.outcome, "reruns": b.reruns,
                    "base": b.base.map(|x| x.chars().take(12).collect::<String>()),
                    "tip": b.tip.map(|x| x.chars().take(12).collect::<String>()),
                    "opened": b.opened, "ended": b.ended,
                })
            })
            .collect();
        let live = s.reader(|c| rows::trains_live(c, &t.id))?;
        let open_closes = live.iter().find(|r| r.state == "open").map(|r| {
            let mut at = r.opened + policy.window_ms();
            if let Some(i) = policy.release_idle_s {
                at = at.min(r.last + i as i64 * 1000);
            }
            at
        });
        let trains: Vec<Value> = s
            .reader(|c| rows::trains_recent(c, &t.id, 10))?
            .into_iter()
            .map(|r| {
                let ids: Vec<String> = s
                    .reader(|c| rows::train_members(c, &t.id, r.no))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| e.id)
                    .collect();
                let (members, others) = names(&ids);
                json!({
                    "no": r.no, "state": r.state, "now": r.now, "version": r.version, "tag": r.tag,
                    "members": members, "others": others, "opened": r.opened, "last": r.last,
                    "closes_at": (r.state == "open").then_some(open_closes).flatten(), "ended": r.ended,
                })
            })
            .collect();
        Ok(Some(json!({
            "on": self.mq_on(),
            "target": {
                "id": t.id, "upstream": t.upstream, "branch": t.branch, "mode": t.mode,
                "granted": t.granted_at.is_some(), "refused": t.granted_by.as_deref() == Some("refused"),
                "migrated": t.legacy_lane.is_some(), "node": t.node_id,
            },
            "policy": policy, "set": set, "defaults": defaults,
            "queued": queued, "queued_others": queued_others, "recent": recent,
            "batches": batches, "trains": trains,
            "stats": self.mq_stats(&t.id)?,
        })))
    }

    /// The last week of `target`, summed up for tuning its policy.
    fn mq_stats(&self, target: &str) -> Result<Value> {
        let s = self.store().store();
        let since = crate::util::now() - WEEK_MS;
        let facts = s.reader(|c| rows::stats_since(c, Some(target), since, 20_000))?;
        let count = |k: &str| facts.iter().filter(|f| f.kind == k).count();
        let ran: Vec<&rows::MqStat> = facts.iter().filter(|f| f.kind == "batch_ran").collect();
        let members: Vec<i64> = ran
            .iter()
            .filter_map(|f| f.data["members"].as_i64())
            .collect();
        let ms: Vec<i64> = ran.iter().filter_map(|f| f.data["ms"].as_i64()).collect();
        let outcome = |o: &str| ran.iter().filter(|f| f.data["outcome"] == o).count();
        let returned = |w: &str| {
            facts
                .iter()
                .filter(|f| f.kind == "returned" && f.data["why"] == w)
                .count()
        };
        let released: Vec<&rows::MqStat> = facts
            .iter()
            .filter(|f| f.kind == "train_released")
            .collect();
        let train_size: Vec<i64> = released
            .iter()
            .filter_map(|f| f.data["members"].as_i64())
            .collect();
        // waits, from the entries of the week: asked → merged, merged → released
        let entries = s.reader(|c| rows::entries_recent(c, target, 500))?;
        let to_land: Vec<i64> = entries
            .iter()
            .filter(|e| e.arrived >= since)
            .filter_map(|e| e.landed_at.map(|l| l - e.arrived))
            .collect();
        let trains = s.reader(|c| rows::trains_recent(c, target, 100))?;
        let train_time: Vec<i64> = trains
            .iter()
            .filter(|r| r.state == "released" && r.opened >= since)
            .filter_map(|r| r.ended.map(|e| e - r.opened))
            .collect();
        // the cards the queue raised, and who answered them how fast
        let cards = s.reader(|c| {
            let mut st = c.prepare(
                "SELECT subject_type, subject_id, status, created, resolved, result FROM decisions
                  WHERE subject_type IN ('mq_target', 'mq_batch', 'mq_train') AND created >= ?",
            )?;
            let v = st
                .query_map([since], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })?;
        let mine: Vec<_> = cards
            .iter()
            .filter(|c| c.1 == target || parse_batch(&c.1).is_some_and(|(t, _)| t == target))
            .collect();
        let answered: Vec<i64> = mine.iter().filter_map(|c| c.4.map(|r| r - c.3)).collect();
        let by_yolo = mine
            .iter()
            .filter(|c| c.5.as_deref().is_some_and(|r| r.contains("yolo")))
            .count();
        Ok(json!({
            "since": since,
            "batches": count("batch_started"), "avg_members": avg(&members), "avg_batch_ms": avg(&ms),
            "landed": outcome("landed"), "failed": outcome("failed"), "no_result": outcome("unknown"),
            "bisections": count("split"), "returned_conflict": returned("left"), "returned_failed": returned("failed"),
            "reruns": s
                .reader(|c| rows::batches_recent(c, target, 500))?
                .iter()
                .filter(|b| b.opened >= since)
                .map(|b| b.reruns)
                .sum::<i64>(),
            "fed": count("fed"),
            "trains": released.len(), "avg_train_size": avg(&train_size), "avg_train_ms": avg(&train_time),
            "avg_wait_to_land_ms": avg(&to_land),
            "cards": mine.len(), "cards_answered": answered.len(), "cards_by_yolo": by_yolo,
            "avg_answer_ms": avg(&answered),
        }))
    }

    /// What you did on the queue of (`upstream`, `branch`): `{"policy": {..}}` sets fields of its
    /// policy (`null` back to the default), `{"allow": bool}` answers whether Steve may merge into
    /// it, `{"release_now": true}`, `{"once": true}` lets one batch through while paused. The queue
    /// as `ws` sees it after.
    pub fn mq_act(
        &self,
        upstream: &str,
        branch: &str,
        ws: &str,
        act: &Value,
        actor: &str,
    ) -> Result<Option<Value>> {
        if !self.config().mq {
            return Err(msg(steve_i18n::t!("core.mq.off")));
        }
        let s = self.store().store();
        let t = s.transaction(|tx| rows::target_for(tx, upstream, branch))?;
        if let Some(p) = act.get("policy").and_then(Value::as_object) {
            let mut set: serde_json::Map<String, Value> =
                serde_json::from_str(&t.policy).unwrap_or_default();
            let known = serde_json::to_value(Policy::default()).unwrap_or_default();
            for (k, v) in p {
                if known.get(k).is_none() {
                    return Err(msg(steve_i18n::t!("core.mq.policy_unknown", field = k)));
                }
                if v.is_null() {
                    set.remove(k);
                } else {
                    set.insert(k.clone(), v.clone());
                }
            }
            // the whole policy must read back as one (types, signs)
            let mut whole = known.as_object().cloned().unwrap_or_default();
            whole.extend(set.clone());
            serde_json::from_value::<Policy>(Value::Object(whole))
                .map_err(|e| msg(steve_i18n::t!("core.mq.policy_bad", why = e.to_string())))?;
            let text = serde_json::to_string(&set)?;
            s.transaction(|tx| rows::target_policy_set(tx, &t.id, &text))?;
            let at = crate::util::now();
            self.mq_event(&t.id, |l, p| {
                let ta = l.trains.apply(p, Te::Tick { at });
                l.train_out.extend(ta);
                l.queue.apply(p, Qe::Tick { at })
            })?;
        }
        if let Some(allow) = act.get("allow").and_then(Value::as_bool) {
            // the open card asking the same is answered by this
            let open: Vec<String> = s.reader(|c| {
                let mut st = c.prepare(
                    "SELECT id FROM decisions WHERE subject_type = ? AND subject_id = ? AND status = 'open'",
                )?;
                let v = st
                    .query_map([SUBJECT_TARGET, t.id.as_str()], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })?;
            for d in open {
                self.store().decision_resolve(
                    &d,
                    json!({"choice": if allow { "yes" } else { "no" }, "via": "project"}),
                    "resolved",
                    None,
                )?;
            }
            self.mq_answered(
                SUBJECT_TARGET,
                &t.id,
                if allow { "yes" } else { "no" },
                actor,
            );
        }
        if act.get("release_now").and_then(Value::as_bool) == Some(true) {
            self.mq_release_now(&t.id)?;
        }
        if act.get("once").and_then(Value::as_bool) == Some(true) {
            let at = crate::util::now();
            self.mq_event(&t.id, |l, p| l.queue.apply(p, Qe::Once { at }))?;
        }
        self.mq_view(upstream, branch, ws)
    }
}
