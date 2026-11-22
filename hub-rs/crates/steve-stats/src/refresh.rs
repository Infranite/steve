//! Refreshes derived item facts and rebuilds the daily cube when the workspace's timezone changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use chrono_tz::Tz;
use steve_store::statistics::{
    DailyKey, DailyRows, DecisionReads, GatewayCalls, ItemHistory, ItemStats, Measures,
    StatsSettings, TurnFact, TurnFacts, UsageFact, UsageFacts,
};
use steve_store::{Read, Result, Store, W, now};

use crate::items;

/// Items recomputed per pass at most.
pub const BATCH: usize = 200;
const EVERY: Duration = Duration::from_secs(2);

/// The workspace's `stats.tz`, with the Hub's own zone as the default.
pub fn timezone(c: &mut impl Read, ws: &str) -> Result<Option<Tz>> {
    Ok(crate::day::tz(StatsSettings(c).timezone(ws)?.as_deref()))
}

#[derive(Debug, Default)]
pub struct Refresher {
    since: i64,
}

impl Refresher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self, store: &Store, limit: usize) -> Result<usize> {
        let t = now();
        let since = self.since;
        // Stay off the single-writer queue during idle polls; recheck under the write lock.
        if !store.read(|r| {
            Ok(ItemStats(r).has_dirty()?
                || (since > 0 && DecisionReads(r).has_item_activity(since)?))
        })? {
            self.since = t - 1_000;
            return Ok(0);
        }
        let n = store.write(|w| {
            if since > 0 {
                let mut changed = BTreeMap::<String, Vec<(String, i64)>>::new();
                for id in DecisionReads(w).items_changed(since)? {
                    if let Some((ws, created)) = ItemHistory(w).task(&id)? {
                        changed.entry(ws).or_default().push((id, created));
                    }
                }
                for (ws, items) in changed {
                    ItemStats(w).flag(&ws, &items, t)?;
                }
            }
            let dirty = ItemStats(w).dirty(limit)?;
            for ws in dirty.iter().map(|(_, ws)| ws).collect::<BTreeSet<_>>() {
                w.emit(steve_store::stats_changed(ws, None));
            }
            update_items(w, &dirty, t)
        })?;
        self.since = t - 1_000;
        Ok(n)
    }

    pub async fn run(mut self, store: Store) {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            let s = store.clone();
            let mut me = std::mem::take(&mut self);
            match tokio::task::spawn_blocking(move || {
                let r = me.tick(&s, BATCH);
                (me, r)
            })
            .await
            {
                Ok((me, r)) => {
                    self = me;
                    if let Err(e) = r {
                        tracing::error!("stats refresh {e}");
                    }
                }
                Err(e) => tracing::error!("stats refresh task {e}"),
            }
        }
    }
}

fn update_items(w: &mut W<'_>, rows: &[(String, String)], at: i64) -> Result<usize> {
    for (id, _) in rows {
        items::refresh_in(w, id, at)?;
    }
    Ok(rows.len())
}

pub fn refresh_dirty_in(w: &mut W<'_>, limit: usize, at: i64) -> Result<usize> {
    let dirty = ItemStats(w).dirty(limit)?;
    update_items(w, &dirty, at)
}

pub fn refresh_all(store: &Store) -> Result<usize> {
    let mut total = 0;
    loop {
        let n = store.write(|w| refresh_dirty_in(w, 1_000, now()))?;
        total += n;
        if n == 0 {
            return Ok(total);
        }
    }
}

pub(crate) fn add_usage(m: &mut Measures, u: &UsageFact) {
    let t = u.tokens;
    m.input += t.input.unwrap_or(0);
    m.output += t.output.unwrap_or(0);
    m.cache_read += t.cache_read.unwrap_or(0);
    m.cache_write += t.cache_write.unwrap_or(0);
    m.reasoning += t.reasoning.unwrap_or(0);
    if let (Some(input), Some(read)) = (t.input, t.cache_read) {
        m.hit_base += input + read + t.cache_write.unwrap_or(0);
    }
    m.cost_reported += u.cost_reported;
    m.calls += 1;
    m.failed_calls += i64::from(!u.ok);
}

pub(crate) fn add_turn(m: &mut Measures, t: &TurnFact) {
    m.turns += i64::from(t.attempt == 0);
    m.turn_ms += (t.ended - t.started).max(0);
    m.turn_errors += i64::from(t.reason.as_deref() == Some("error") && !t.transient);
    m.retries += i64::from(t.transient);
    m.interrupted += i64::from(t.reason.as_deref() == Some("interrupted"));
    m.tools += t.tools;
    m.tool_errors += t.tool_errors;
    for (kind, n) in &t.tool_kinds {
        *m.tool_kinds.entry(kind.clone()).or_default() += n;
    }
}

/// Re-cuts the source days and replaces a workspace's daily cube atomically.
pub fn rebuild_in(w: &mut W<'_>, ws: &str) -> Result<usize> {
    let tz = timezone(w, ws)?;
    let mut cells = HashMap::<DailyKey, Measures>::new();
    let mut first = HashMap::<String, String>::new();
    let mut recut = Vec::new();
    UsageFacts(w).scan(ws, None, None, None, |u| {
        let day = crate::day::day(u.at, tz);
        if day != u.day {
            recut.push((u.id, day.clone()));
        }
        if let Some(id) = &u.turn {
            first.entry(id.clone()).or_insert_with(|| u.model.clone());
        }
        let key = (
            day,
            u.source.clone(),
            u.purpose.clone(),
            u.harness.clone(),
            u.model.clone(),
            u.bot.clone(),
            u.project.clone(),
        );
        add_usage(cells.entry(key).or_default(), &u);
        Ok(())
    })?;
    TurnFacts(w).scan_ended(ws, None, None, None, |t| {
        let model = t
            .model
            .clone()
            .or_else(|| first.get(&t.id).cloned())
            .unwrap_or_else(|| t.fallback_model.clone());
        let key = (
            crate::day::day(t.ended, tz),
            "agent".into(),
            "turn".into(),
            t.harness.clone(),
            model,
            t.bot.clone(),
            t.project.clone(),
        );
        add_turn(cells.entry(key).or_default(), &t);
        Ok(())
    })?;
    UsageFacts(w).recut(ws, recut)?;
    // A dispatched call without a projection still needs the new day for a late final report.
    for (call, started) in GatewayCalls(w).starts(ws)? {
        GatewayCalls(w).recut(ws, &call, &crate::day::day(started, tz))?;
    }
    let n = DailyRows(w).replace(ws, cells)?;
    flag_workspace(w, ws)?;
    Ok(n)
}

pub(crate) fn flag_workspace(w: &mut W<'_>, ws: &str) -> Result<()> {
    let tasks = ItemHistory(w).tasks(ws)?;
    ItemStats(w).flag(ws, &tasks, now())
}

pub fn rebuild(store: &Store, ws: &str) -> Result<usize> {
    let n = store.write(|w| rebuild_in(w, ws))?;
    refresh_all(store)?;
    store.write(|w| {
        w.emit(steve_store::stats_changed(ws, None));
        Ok::<_, steve_store::StoreError>(())
    })?;
    Ok(n)
}
