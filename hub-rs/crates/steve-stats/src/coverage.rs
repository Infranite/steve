//! Coverage travels beside additive sums. `None` is unknown; a known zero remains `Some(0)`.
//! Counts refer to accounting meter/model rows plus attempts with no rows, not user messages.
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::BTreeMap;
use steve_store::{Result, Tokens};

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
        self.total = sum(self.total, b.total);
        self.input = sum(self.input, b.input);
        self.output = sum(self.output, b.output);
        self.cache_read = sum(self.cache_read, b.cache_read);
        self.cache_write = sum(self.cache_write, b.cache_write);
        self.cost_reported = sum(self.cost_reported, b.cost_reported);
        self.state = if self.expected > 0 && self.complete == self.expected {
            CoverageState::Complete
        } else if self.reported > 0 {
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

/// Same dimensions as the cube, including missing attempts that have no cube usage row yet.
pub(crate) type Key = (String, String, String, String, String, String, String);

#[derive(Default)]
struct Filter<'a> {
    q: Option<&'a Query>,
    item: Option<&'a str>,
    session: Option<&'a str>,
    turn: Option<&'a str>,
    hourly: bool,
}

// A billing companion does not report another attempt's tokens.
const TOKEN_ROLE: &str = "COALESCE(json_extract(u.detail,'$.accounting_tokens'),1)";

fn load(c: &Connection, f: Filter<'_>) -> Result<BTreeMap<Key, Coverage>> {
    let q = f.q;
    let p = params![
        q.map(|q| q.workspace_id.as_str()),
        q.map(|q| q.from.as_str()),
        q.map(|q| q.to.as_str()),
        q.and_then(|q| q.project.as_deref()),
        q.and_then(|q| q.bot.as_deref()),
        q.and_then(|q| q.model.as_deref()),
        q.and_then(|q| q.source.as_deref()),
        f.item,
        f.session,
        f.turn
    ];
    // A concrete leading predicate keeps entity reads on their item/session/turn indexes;
    // parameterized `IS NULL OR` filters alone would force a full usage-history scan.
    let usage_guard = if q.is_some() {
        "u.workspace_id=?1 AND u.day>=?2 AND u.day<=?3"
    } else if f.item.is_some() {
        "u.item_id=?8"
    } else if f.session.is_some() {
        "u.session_id=?9"
    } else {
        "u.turn_id=?10"
    };
    let turn_guard = if q.is_some() {
        "t.workspace_id=?1 AND t.started>=?2 AND t.started<?3"
    } else if f.item.is_some() {
        "t.item_id=?8"
    } else if f.session.is_some() {
        "t.session_id=?9"
    } else {
        "t.id=?10"
    };
    let mut cells = BTreeMap::<Key, Coverage>::new();
    let mut st=c.prepare_cached(&format!(
        "SELECT u.day,u.source,u.purpose,u.harness,u.model,COALESCE(u.bot_id,''),COALESCE(u.project_id,''),u.input,u.output,u.cache_read,u.cache_write,u.cost_reported,
          CASE WHEN (t.id IS NOT NULL AND t.ended IS NULL) OR json_extract(u.detail,'$.pending')=1 THEN 1 ELSE 0 END, u.ts, {TOKEN_ROLE}, CASE WHEN json_extract(u.detail,'$.unspent_accounting')=1 THEN json_extract(u.detail,'$.unspent_usage') END
         FROM usage_records u LEFT JOIN agent_turns t ON t.id=u.turn_id
         WHERE {usage_guard} AND (?1 IS NULL OR u.workspace_id=?1) AND (?2 IS NULL OR u.day>=?2) AND (?3 IS NULL OR u.day<=?3)
          AND (?4 IS NULL OR u.project_id=?4) AND (?5 IS NULL OR u.bot_id=?5) AND (?6 IS NULL OR u.model=?6 OR (json_extract(u.detail,'$.unspent_accounting')=1 AND EXISTS(
            SELECT 1 FROM json_each(u.detail,'$.unspent_usage') e WHERE json_extract(e.value,'$.model')=?6))) AND (?7 IS NULL OR u.source=?7)
          AND (?8 IS NULL OR u.item_id=?8) AND (?9 IS NULL OR u.session_id=?9) AND (?10 IS NULL OR u.turn_id=?10)"))?;
    let mut rows = st.query(p)?;
    while let Some(r) = rows.next()? {
        let bucket = if f.hourly {
            crate::day::hour(
                r.get(13)?,
                crate::refresh::tz_of(c, &q.unwrap().workspace_id)?,
            )
        } else {
            r.get(0)?
        };
        let k = (
            bucket,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        );
        let role: i64 = r.get(14)?;
        if q.and_then(|q| q.model.as_deref())
            .is_none_or(|model| model == k.4)
        {
            if role == 0 {
                cells.entry(k.clone()).or_default().add(&Coverage {
                    cost_reported: r.get(11)?,
                    ..Coverage::default()
                });
            } else {
                cells.entry(k.clone()).or_default().observe(
                    Tokens {
                        input: r.get(7)?,
                        output: r.get(8)?,
                        cache_read: r.get(9)?,
                        cache_write: r.get(10)?,
                        reasoning: None,
                    },
                    r.get(11)?,
                    r.get(12)?,
                );
            }
        }
        if let Some(payload) = r.get::<_, Option<String>>(15)? {
            observe_unspent_models(&mut cells, q, &k, &payload)?;
        }
    }
    observe_unreported_turns(c, &f, turn_guard, &mut cells)?;
    Ok(cells)
}

fn observe_unspent_models(
    cells: &mut BTreeMap<Key, Coverage>,
    q: Option<&Query>,
    k: &Key,
    payload: &str,
) -> Result<()> {
    // One attempt can explicitly report an unused model. Keep its zero coverage
    // beside the charged model without creating another request/cube row. Retry
    // zeros have no such marker and cannot stand in for the answering model.
    for evidence in serde_json::from_str::<Vec<serde_json::Value>>(payload)? {
        let model = evidence["model"].as_str().unwrap_or_default();
        if q.and_then(|q| q.model.as_deref())
            .is_some_and(|m| m != model)
        {
            continue;
        }
        let f = &evidence["tokens"];
        let t = Tokens {
            input: f["input"].as_i64(),
            output: f["output"].as_i64(),
            cache_read: f["cache_read"].as_i64(),
            cache_write: f["cache_write"].as_i64(),
            reasoning: f["reasoning"].as_i64(),
        };
        if [t.input, t.output, t.cache_read, t.cache_write, t.reasoning]
            .iter()
            .flatten()
            .any(|n| *n != 0)
        {
            continue;
        }
        let mut key = k.clone();
        key.4 = model.into();
        cells.entry(key).or_default().observe(t, None, false);
    }
    Ok(())
}

fn observe_unreported_turns(
    c: &Connection,
    f: &Filter<'_>,
    turn_guard: &str,
    cells: &mut BTreeMap<Key, Coverage>,
) -> Result<()> {
    let q = f.q;
    // No synthetic zero-valued usage row is needed to represent an unreported attempt.
    let bounds = match q {
        Some(q) => {
            crate::metrics::bounds(&q.from, &q.to, crate::refresh::tz_of(c, &q.workspace_id)?)
        }
        None => None,
    };
    let mut st=c.prepare_cached(&format!(
        "SELECT t.workspace_id,t.started,t.harness,COALESCE(t.model,s.model,b.model,''),COALESCE(t.bot_id,''),COALESCE(t.project_id,''),t.ended IS NULL
         FROM agent_turns t LEFT JOIN sessions s ON s.id=t.session_id LEFT JOIN bots b ON b.id=t.bot_id
         WHERE {turn_guard} AND (?1 IS NULL OR t.workspace_id=?1) AND (?4 IS NULL OR t.project_id=?4) AND (?5 IS NULL OR t.bot_id=?5)
          AND (?6 IS NULL OR COALESCE(t.model,s.model,b.model,'')=?6) AND (?7 IS NULL OR ?7='agent')
          AND (?8 IS NULL OR t.item_id=?8) AND (?9 IS NULL OR t.session_id=?9) AND (?10 IS NULL OR t.id=?10)
          AND NOT EXISTS(SELECT 1 FROM usage_records u WHERE u.turn_id=t.id AND {TOKEN_ROLE}<>0)"))?;
    let mut rows = st.query(params![
        q.map(|q| q.workspace_id.as_str()),
        bounds.map(|b| b.0),
        bounds.map(|b| b.1),
        q.and_then(|q| q.project.as_deref()),
        q.and_then(|q| q.bot.as_deref()),
        q.and_then(|q| q.model.as_deref()),
        q.and_then(|q| q.source.as_deref()),
        f.item,
        f.session,
        f.turn
    ])?;
    while let Some(r) = rows.next()? {
        let ws: String = r.get(0)?;
        let d = crate::day(r.get(1)?, crate::refresh::tz_of(c, &ws)?);
        if q.is_some_and(|q| d < q.from || d > q.to) {
            continue;
        }
        let bucket = if f.hourly {
            crate::day::hour(r.get(1)?, crate::refresh::tz_of(c, &ws)?)
        } else {
            d
        };
        let k = (
            bucket,
            "agent".into(),
            "turn".into(),
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        );
        cells
            .entry(k)
            .or_default()
            .observe(Tokens::default(), None, r.get(6)?);
    }
    Ok(())
}

pub(crate) fn hourly_cells(c: &Connection, q: &Query) -> Result<BTreeMap<Key, Coverage>> {
    load(
        c,
        Filter {
            q: Some(q),
            hourly: true,
            ..Filter::default()
        },
    )
}

pub(crate) fn cells(c: &Connection, q: &Query) -> Result<BTreeMap<Key, Coverage>> {
    load(
        c,
        Filter {
            q: Some(q),
            ..Filter::default()
        },
    )
}

/// Read coverage of an existing entity; numeric details and workspace authorization are handled by
/// the calling statistics view. Only these fixed entity columns are accepted.
pub fn entity(c: &Connection, kind: &str, id: &str) -> Result<Coverage> {
    let f = match kind {
        "item" => Filter {
            item: Some(id),
            ..Filter::default()
        },
        "session" => Filter {
            session: Some(id),
            ..Filter::default()
        },
        "turn" => Filter {
            turn: Some(id),
            ..Filter::default()
        },
        _ => return Ok(Coverage::default()),
    };
    let mut out = Coverage::default();
    for v in load(c, f)?.values() {
        out.add(v);
    }
    Ok(out)
}

pub(crate) fn model(c: &Connection, kind: &str, id: &str, model: &str) -> Result<Coverage> {
    let f = match kind {
        "item" => Filter {
            item: Some(id),
            ..Filter::default()
        },
        _ => return Ok(Coverage::default()),
    };
    let mut out = Coverage::default();
    for (key, v) in load(c, f)? {
        if key.4 == model {
            out.add(&v);
        }
    }
    Ok(out)
}
