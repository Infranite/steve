//! Coverage travels beside additive sums. `None` is unknown; a known zero remains `Some(0)`.
//! Counts refer to accounting meter/model rows plus attempts with no rows, not user messages.
use serde::Serialize;
use std::collections::BTreeMap;
use steve_store::statistics::{
    Attribution, CoverageRows, CoverageScope, DailyFilter, DailyKey, Entity,
};
use steve_store::{Read, Result, Tokens};

use crate::metrics::Query;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    #[default]
    NotReported,
    Unavailable,
    Partial,
    Complete,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Coverage {
    pub state: CoverageState,
    pub expected: i64,
    pub reported: i64,
    pub complete: i64,
    pub pending: i64,
    pub unavailable: i64,
    /// Old cost-only rows with a same-time token peer but no stable call ownership.
    pub uncertain: i64,
    /// Sums of known fields only. A partial sum is explicitly labelled by `state`.
    pub total: Option<i64>,
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub cost_reported: Option<f64>,
}

impl Coverage {
    pub(crate) fn add(&mut self, b: &Self) {
        fn sum<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            }
        }
        self.expected += b.expected;
        self.reported += b.reported;
        self.complete += b.complete;
        self.pending += b.pending;
        self.unavailable += b.unavailable;
        self.uncertain += b.uncertain;
        self.total = sum(self.total, b.total);
        self.input = sum(self.input, b.input);
        self.output = sum(self.output, b.output);
        self.cache_read = sum(self.cache_read, b.cache_read);
        self.cache_write = sum(self.cache_write, b.cache_write);
        self.cost_reported = sum(self.cost_reported, b.cost_reported);
        self.state = if self.expected > 0 && self.complete == self.expected {
            CoverageState::Complete
        } else if self.reported > 0 || self.uncertain > 0 {
            CoverageState::Partial
        } else if self.pending > 0 || self.expected == 0 {
            CoverageState::NotReported
        } else {
            CoverageState::Unavailable
        };
    }

    fn observe(&mut self, t: Tokens, cost: Option<f64>, open: bool) {
        let fields = [t.input, t.output, t.cache_read, t.cache_write];
        let reported = fields.iter().any(Option::is_some);
        self.add(&Self {
            expected: 1,
            reported: i64::from(reported),
            complete: i64::from(fields.iter().all(Option::is_some)),
            pending: i64::from(!reported && open),
            unavailable: i64::from(!reported && !open),
            total: reported.then(|| fields.into_iter().flatten().sum()),
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            cost_reported: cost,
            ..Self::default()
        });
    }
}

/// The daily cube's dimensions, including missing attempts without a usage row.
pub(crate) type Key = DailyKey;

fn load(
    c: &mut impl Read,
    q: Option<&Query>,
    scope: CoverageScope<'_>,
    hourly: bool,
) -> Result<BTreeMap<Key, Coverage>> {
    let zone = match q {
        Some(q) => crate::refresh::timezone(c, &q.workspace_id)?,
        None => None,
    };
    let mut cells = BTreeMap::<Key, Coverage>::new();
    for mut row in CoverageRows(c).meters(&scope)? {
        let k = &mut row.key;
        if hourly {
            k.0 = crate::day::hour(row.at, zone);
        }
        let model = q.and_then(|q| q.model.as_deref());
        if model.is_none_or(|model| model == k.4) {
            let cell = cells.entry(k.clone()).or_default();
            match row.attribution {
                Attribution::Meter => cell.observe(row.tokens, row.cost, row.pending),
                Attribution::CostOnly => cell.add(&Coverage {
                    cost_reported: row.cost,
                    ..Default::default()
                }),
                Attribution::Uncertain => cell.add(&Coverage {
                    cost_reported: row.cost,
                    expected: 1,
                    uncertain: 1,
                    ..Default::default()
                }),
            }
        }
        // Unused models may explicitly report zero alongside the answering model.
        for evidence in row.unspent {
            let t = evidence.tokens;
            if model.is_some_and(|m| m != evidence.model)
                || [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
                    .iter()
                    .flatten()
                    .any(|n| *n != 0)
            {
                continue;
            }
            let mut key = k.clone();
            key.4 = evidence.model;
            cells.entry(key).or_default().observe(t, None, false);
        }
    }
    let mut zones = BTreeMap::new();
    for row in CoverageRows(c).missing_turns(&scope)? {
        let tz = match q {
            Some(_) => zone,
            None => match zones.entry(row.ws.clone()) {
                std::collections::btree_map::Entry::Occupied(e) => *e.get(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    *e.insert(crate::refresh::timezone(c, &row.ws)?)
                }
            },
        };
        let day = crate::day(row.at, tz);
        if q.is_some_and(|q| day < q.from || day > q.to) {
            continue;
        }
        let bucket = if hourly {
            crate::day::hour(row.at, tz)
        } else {
            day
        };
        cells
            .entry((
                bucket,
                "agent".into(),
                "turn".into(),
                row.harness,
                row.model,
                row.bot,
                row.project,
            ))
            .or_default()
            .observe(Tokens::default(), None, row.pending);
    }
    Ok(cells)
}

fn report(c: &mut impl Read, q: &Query, hourly: bool) -> Result<BTreeMap<Key, Coverage>> {
    let tz = crate::refresh::timezone(c, &q.workspace_id)?;
    let scope = CoverageScope::Report {
        days: DailyFilter {
            ws: &q.workspace_id,
            from: &q.from,
            to: &q.to,
            project: q.project.as_deref(),
            bot: q.bot.as_deref(),
            model: q.model.as_deref(),
            source: q.source.as_deref(),
        },
        turns: crate::metrics::bounds(&q.from, &q.to, tz),
    };
    load(c, Some(q), scope, hourly)
}

pub(crate) fn hourly_cells(c: &mut impl Read, q: &Query) -> Result<BTreeMap<Key, Coverage>> {
    report(c, q, true)
}

pub(crate) fn cells(c: &mut impl Read, q: &Query) -> Result<BTreeMap<Key, Coverage>> {
    report(c, q, false)
}

fn subject<'a>(kind: &str, id: &'a str) -> Option<Entity<'a>> {
    Some(match kind {
        "item" => Entity::Item(id),
        "session" => Entity::Session(id),
        "turn" => Entity::Turn(id),
        _ => return None,
    })
}

pub fn entity(c: &mut impl Read, kind: &str, id: &str) -> Result<Coverage> {
    let Some(subject) = subject(kind, id) else {
        return Ok(Coverage::default());
    };
    let mut out = Coverage::default();
    for v in load(c, None, CoverageScope::Entity(subject), false)?.values() {
        out.add(v);
    }
    Ok(out)
}

pub(crate) fn model(c: &mut impl Read, id: &str, model: &str) -> Result<Coverage> {
    let mut out = Coverage::default();
    for (key, v) in load(c, None, CoverageScope::Entity(Entity::Item(id)), false)? {
        if key.4 == model {
            out.add(&v);
        }
    }
    Ok(out)
}
