//! `/api/stats/*`: the summary, trends, breakdowns, the per-item table and
//! drill-down, a session's totals, the price table, the Models.dev price sync and time zone, and the rebuild. Reads go to the
//! store's read pool; summaries are cached per query until the next statistics change (at most 5 s).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Extension, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_stats::metrics::{self, By, Query as StatsQuery};
use steve_stats::views::{self, Bucket, Metric};
use steve_stats::{Price, catalog, default_prices};
use steve_store::{Store, StoreError, now, stats_changed};

use super::{ApiResult, Ctx, Noun, blocking, body, ok};
use crate::bus::Bus;
use crate::error::{ApiError, json_response};

/// How long a cached answer may be served, even without a statistics change (WIP and "now" move).
const FRESH: Duration = Duration::from_secs(5);
const CACHE_MAX: usize = 256;
const SOURCES: &[&str] = &["agent", "gateway", "backfill"];
const GROUPS: &[&str] = &[
    "none",
    "model",
    "bot",
    "project",
    "source",
    "purpose",
    "token_type",
];
const COUNT_BYS: &[&str] = &["tool", "decision_kind", "fail_reason"];

#[derive(Clone)]
pub struct StatsApi(Arc<Inner>);

struct Inner {
    store: Store,
    bus: Bus,
    /// The Models.dev catalog URL, `None` when the sync is turned off.
    price_url: Option<String>,
    /// key → (stats generation, computed at, answer)
    cache: Mutex<HashMap<String, (u64, Instant, Value)>>,
}

impl StatsApi {
    pub fn new(store: Store, bus: Bus, price_url: Option<String>) -> Self {
        StatsApi(Arc::new(Inner {
            store,
            bus,
            price_url,
            cache: Mutex::default(),
        }))
    }

    async fn read<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, ApiError> + Send + 'static,
    ) -> Result<R, ApiError> {
        let s = self.0.store.clone();
        blocking(move || s.reader(|c| Ok::<_, StoreError>(f(c))).map_err(serr)?).await
    }

    /// `f`'s answer for `key`, from the cache while no statistics changed and it is under 5 s old.
    async fn cached(
        &self,
        key: String,
        f: impl FnOnce(&Connection) -> Result<Value, ApiError> + Send + 'static,
    ) -> ApiResult {
        let generation = self.0.bus.stats_generation();
        {
            let c = self.0.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((g, at, v)) = c.get(&key)
                && *g == generation
                && at.elapsed() < FRESH
            {
                return ok(v.clone());
            }
        }
        let v = self.read(f).await?;
        let mut c = self.0.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.len() >= CACHE_MAX {
            c.retain(|_, (g, at, _)| *g == generation && at.elapsed() < FRESH);
            if c.len() >= CACHE_MAX {
                c.clear();
            }
        }
        c.insert(key, (generation, Instant::now(), v.clone()));
        ok(v)
    }

    fn forget(&self) {
        self.0
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// The routes, behind `require_user` (merged with the module routes).
pub fn routes(api: StatsApi) -> Router {
    Router::new()
        .route("/stats/summary", get(summary))
        .route("/stats/series", get(series))
        .route("/stats/breakdown", get(breakdown))
        .route("/stats/items", get(items))
        .route("/stats/items/{id}", get(item))
        .route("/stats/sessions/{id}", get(session))
        .route("/stats/prices", get(prices_get).put(prices_put))
        .route("/stats/prices/sync", post(prices_sync))
        .route("/stats/tz", put(tz_put))
        .route("/stats/rebuild", post(rebuild))
        .with_state(api)
}

type Params = HashMap<String, String>;

fn invalid(field: &str) -> ApiError {
    ApiError::Invalid(vec![field.to_string()])
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn serr(e: StoreError) -> ApiError {
    ApiError::internal(e.to_string())
}

/// The common parameters: range, then the filters, each reference checked against the workspace.
fn query(c: &Connection, ws: &str, p: &Params) -> Result<StatsQuery, ApiError> {
    let get = |k: &str| p.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let today = views::today(c, ws, now()).map_err(serr)?;
    let (from, to) = views::range(get("range").unwrap_or("7d"), get("from"), get("to"), &today)
        .map_err(invalid)?;
    if let Some(id) = get("project") {
        let w: Option<String> = c
            .query_row(
                "SELECT workspace_id FROM projects WHERE id = ?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if w.as_deref() != Some(ws) {
            return Err(ApiError::not_found(Noun::Project.missing()));
        }
    }
    if let Some(id) = get("bot") {
        let member: bool = c
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?)",
                [id, ws],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !member {
            return Err(ApiError::not_found(Noun::Bot.missing()));
        }
    }
    if let Some(s) = get("source")
        && !SOURCES.contains(&s)
    {
        return Err(invalid("source"));
    }
    if get("model").is_some_and(|m| m.chars().count() > 200) {
        return Err(invalid("model"));
    }
    Ok(StatsQuery {
        workspace_id: ws.to_string(),
        from,
        to,
        project: get("project").map(str::to_string),
        bot: get("bot").map(str::to_string),
        model: get("model").map(str::to_string),
        source: get("source").map(str::to_string),
    })
}

/// `hour=0..23`: one local hour of a one-day range (ST-43), the bucket of an hourly series.
fn hour_of(p: &Params, q: &StatsQuery) -> Result<Option<u32>, ApiError> {
    match p.get("hour").filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(h) => h
            .parse::<u32>()
            .ok()
            .filter(|h| *h < 24 && q.from == q.to)
            .map(Some)
            .ok_or_else(|| invalid("hour")),
    }
}

/// The zone days are cut in, by name: `stats.tz`, else the Hub's own, else `local`.
fn zone_name(c: &Connection, ws: &str) -> Result<String, ApiError> {
    let tz = steve_stats::refresh::tz_of(c, ws).map_err(serr)?;
    Ok(tz
        .map(|z| z.name().to_string())
        .or_else(hub_tz)
        .unwrap_or_else(|| "local".into()))
}

/// The cache key: the cached bodies hold translated labels, so the language is part of it.
fn key(ws: &str, what: &str, p: &Params) -> String {
    let sorted: BTreeMap<&String, &String> = p.iter().collect();
    let lang = steve_i18n::current();
    format!("{ws}\u{1}{what}\u{1}{lang}\u{1}{sorted:?}")
}

/// A summary as JSON with its range in `range`.
fn summary_json(c: &Connection, q: &StatsQuery) -> Result<Value, ApiError> {
    let s = metrics::summary(c, q)
        .map_err(serr)?
        .ok_or_else(|| invalid("from"))?;
    let mut v = serde_json::to_value(&s).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(o) = v.as_object_mut() {
        let range = json!({ "from": o.remove("from"), "to": o.remove("to"), "tz": zone_name(c, &q.workspace_id)? });
        o.remove("tz");
        o.insert("range".into(), range);
    }
    Ok(v)
}

async fn summary(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let k = key(&c.ws, "summary", &p);
    let store = api.0.store.clone();
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        // the previous range on a second pooled connection, at the same time
        let (v, prev) = std::thread::scope(|sc| {
            let prev = views::prev(&q.from, &q.to).map(|(from, to)| {
                let pq = StatsQuery {
                    from,
                    to,
                    ..q.clone()
                };
                let store = &store;
                sc.spawn(move || {
                    store
                        .reader(|c| Ok::<_, StoreError>(summary_json(c, &pq)))
                        .map_err(serr)?
                })
            });
            let v = summary_json(db, &q);
            let prev = match prev {
                Some(h) => h
                    .join()
                    .unwrap_or_else(|_| Err(ApiError::internal("summary thread failed")))?,
                None => Value::Null,
            };
            Ok::<_, ApiError>((v?, prev))
        })?;
        let mut v = v;
        if let Some(o) = v.as_object_mut() {
            o.insert("prev".into(), prev);
        }
        Ok(v)
    })
    .await
}

async fn series(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let metric = p
        .get("metric")
        .and_then(|m| Metric::parse(m))
        .ok_or_else(|| invalid("metric"))?;
    let group = p.get("group").cloned().unwrap_or_else(|| "none".into());
    if !GROUPS.contains(&group.as_str()) || !metric.allows(&group) {
        return Err(invalid("group"));
    }
    let hourly = match p.get("bucket").map(String::as_str) {
        None | Some("" | "day") => false,
        Some("hour") => true,
        Some(_) => return Err(invalid("bucket")),
    };
    let k = key(&c.ws, "series", &p);
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        // hours: only a one-day range (今天, or one picked day)
        if hourly && q.from != q.to {
            return Err(invalid("bucket"));
        }
        let bucket = if hourly {
            Bucket::Hour { now: now() }
        } else {
            Bucket::Day
        };
        let s = views::series(db, &q, metric, &group, bucket).map_err(serr)?;
        let mut v = serde_json::to_value(s).map_err(|e| ApiError::internal(e.to_string()))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("tz".into(), json!(zone_name(db, &c.ws)?));
        }
        Ok(v)
    })
    .await
}

async fn breakdown(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let by = p.get("by").cloned().unwrap_or_default();
    let tokens = By::parse(&by).filter(|b| *b != By::Day);
    if tokens.is_none() && !COUNT_BYS.contains(&by.as_str()) {
        return Err(invalid("by"));
    }
    let k = key(&c.ws, "breakdown", &p);
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        let hour = hour_of(&p, &q)?;
        let rows = match tokens {
            Some(b) => {
                let names = views::Names::load(db, &c.ws).map_err(serr)?;
                let prices = metrics::book_of(db, &c.ws).map_err(serr)?;
                let daily = match hour {
                    // one hour of the day: the hourly cells of that bucket, as the hourly series has them
                    Some(h) => {
                        let b = format!("{}T{h:02}:00", q.from);
                        let mut v = metrics::hourly(db, &q).map_err(serr)?;
                        v.retain(|d| d.day == b);
                        v
                    }
                    None => metrics::daily(db, &q).map_err(serr)?,
                };
                /* by purpose: also the models each purpose actually ran on */
                let used = (b == By::Purpose).then(|| metrics::models_by_purpose(&daily));
                metrics::breakdown(&daily, b, &prices)
                    .into_iter()
                    .map(|r| {
                        let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
                        if let Some(o) = v.as_object_mut() {
                            o.insert("label".into(), json!(names.label(&by, &r.key)));
                            if b == By::Model {
                                let quote = prices.quote(&r.key, prices.today());
                                o.insert(
                                    "priced".into(),
                                    json!(!r.key.is_empty() && r.usage.unpriced_tokens == 0),
                                );
                                o.insert("price_source".into(), json!(quote.map(|q| q.source)));
                            }
                            if let Some(u) = &used {
                                let ms = u.get(&r.key).map(Vec::as_slice).unwrap_or_default();
                                o.insert(
                                    "models".into(),
                                    json!(
                                        ms.iter()
                                            .map(|(h, m, n)| json!({"harness": h, "model": m, "calls": n}))
                                            .collect::<Vec<_>>()
                                    ),
                                );
                            }
                        }
                        v
                    })
                    .collect()
            }
            // the counts come from the summary, which has no hours
            None if hour.is_some() => return Err(invalid("hour")),
            None => {
                let s = metrics::summary(db, &q)
                    .map_err(serr)?
                    .ok_or_else(|| invalid("from"))?;
                views::count_rows(&s, &by)
            }
        };
        Ok(json!({ "by": by, "rows": rows }))
    })
    .await
}

async fn items(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let sort = p.get("sort").cloned().unwrap_or_else(|| "est".into());
    if !views::ITEM_SORTS.contains(&sort.as_str()) {
        return Err(invalid("sort"));
    }
    let limit = match p.get("limit") {
        None => 50,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| invalid("limit"))?,
    };
    let cursor = match p.get("cursor").filter(|v| !v.is_empty()) {
        None => 0,
        Some(v) => v.parse::<usize>().map_err(|_| invalid("cursor"))?,
    };
    let only = p.get("only").filter(|v| !v.is_empty()).cloned();
    if only
        .as_deref()
        .is_some_and(|v| !views::ITEM_ONLY.contains(&v))
    {
        return Err(invalid("only"));
    }
    let k = key(&c.ws, "items", &p);
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        let hour = hour_of(&p, &q)?;
        let page =
            views::items(db, &q, &sort, only.as_deref(), hour, limit, cursor).map_err(serr)?;
        serde_json::to_value(page).map_err(|e| ApiError::internal(e.to_string()))
    })
    .await
}

async fn item(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(p): Query<Params>,
) -> ApiResult {
    let brief = p.get("brief").is_some_and(|v| v == "1" || v == "true");
    api.read(
        move |db| match views::item_detail(db, &id, now(), brief).map_err(serr)? {
            Some((ws, d)) if ws == c.ws => {
                ok(serde_json::to_value(d).map_err(|e| ApiError::internal(e.to_string()))?)
            }
            _ => Err(ApiError::not_found(Noun::Item.missing())),
        },
    )
    .await
}

async fn session(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    api.read(move |db| match views::session(db, &id).map_err(serr)? {
        Some((ws, s)) if ws == c.ws => {
            ok(serde_json::to_value(s).map_err(|e| ApiError::internal(e.to_string()))?)
        }
        _ => Err(ApiError::not_found(Noun::Session.missing())),
    })
    .await
}

/// The stored table, `None` when the workspace uses the defaults.
fn stored_prices(c: &Connection, ws: &str) -> Result<Option<Vec<Price>>, ApiError> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'stats.prices'",
            [ws],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
}

fn stored_tz(c: &Connection, ws: &str) -> Result<Option<String>, ApiError> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'stats.tz'",
            [ws],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
}

/// The Hub's own zone name (from `TZ` or `/etc/localtime`), `null` if it can't tell.
fn hub_tz() -> Option<String> {
    if let Ok(z) = std::env::var("TZ")
        && steve_stats::tz(Some(&z)).is_some()
    {
        return Some(z);
    }
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let s = link.to_string_lossy();
    let name = s.split("zoneinfo/").nth(1)?;
    steve_stats::tz(Some(name)).map(|_| name.to_string())
}

/// Where a model's price comes from today, as the settings page lists it.
fn quote_json(q: &steve_stats::Quote) -> Value {
    json!({
        "source": q.source, "match": q.price.pattern, "via": q.via, "since": q.since,
        "input": q.price.input, "output": q.price.output,
        "cache_read": q.price.cache_read, "cache_write": q.price.cache_write, "note": q.price.note,
    })
}

async fn prices_get(State(api): State<StatsApi>, Extension(c): Extension<Ctx>) -> ApiResult {
    let enabled = api.0.price_url.is_some();
    api.read(move |db| {
        let custom = stored_prices(db, &c.ws)?;
        let prices = custom.clone().unwrap_or_else(default_prices);
        let book = metrics::book_of(db, &c.ws).map_err(serr)?;
        // tokens of models no row prices, over everything recorded
        let mut unpriced: BTreeMap<String, i64> = BTreeMap::new();
        let mut st = db
            .prepare_cached(
                "SELECT model, SUM(input + output + cache_read + cache_write) FROM stat_daily WHERE workspace_id = ? GROUP BY model",
            )
            .map_err(sql)?;
        let rows = st
            .query_map([&c.ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(sql)?;
        let mut models = vec![];
        for r in rows {
            let (m, n) = r.map_err(sql)?;
            let quote = book.quote(&m, book.today());
            if quote.is_none() && n > 0 {
                *unpriced.entry(m.clone()).or_default() += n;
            }
            models.push(json!({
                "model": m, "tokens": n,
                "match": quote.as_ref().map(|q| q.price.pattern.clone()), "via": quote.as_ref().and_then(|q| q.via),
                "price": quote.as_ref().map(quote_json),
            }));
        }
        models.sort_by_key(|m| std::cmp::Reverse(m["tokens"].as_i64().unwrap_or(0)));
        let sync = catalog::state(db, catalog::SOURCE).map_err(serr)?;
        ok(json!({
            "prices": prices,
            "custom": custom.is_some(),
            "defaults": default_prices(),
            "unpriced": unpriced.into_iter().map(|(model, tokens)| json!({"model": model, "tokens": tokens})).collect::<Vec<_>>(),
            "models": models,
            "catalog": {
                "source": "Models.dev",
                "url": catalog::URL,
                "enabled": enabled,
                "synced": sync.synced, "tried": sync.tried, "error": sync.error, "models": sync.models,
            },
            "tz": stored_tz(db, &c.ws)?,
            "hub_tz": hub_tz(),
        }))
    })
    .await
}

/// Reads `prices` from a body, naming the first bad field (`prices[i].<field>`).
fn parse_prices(v: &Value) -> Result<Option<Vec<Price>>, ApiError> {
    let Some(o) = v.as_object() else {
        return Err(invalid("prices"));
    };
    let list = match o.get("prices") {
        Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        _ => return Err(invalid("prices")),
    };
    if list.len() > 200 {
        return Err(invalid("prices"));
    }
    let mut out = vec![];
    for (i, p) in list.iter().enumerate() {
        let f = |name: &str| format!("prices[{i}].{name}");
        let Some(p) = p.as_object() else {
            return Err(invalid(&format!("prices[{i}]")));
        };
        let pattern = p
            .get("match")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.chars().count() <= 200)
            .ok_or_else(|| invalid(&f("match")))?;
        let num = |k: &str| -> Result<f64, ApiError> {
            p.get(k)
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite() && *x >= 0.0)
                .ok_or_else(|| invalid(&f(k)))
        };
        let note = match p.get("note") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) if s.chars().count() <= 500 => s.clone(),
            _ => return Err(invalid(&f("note"))),
        };
        out.push(Price {
            pattern: pattern.to_string(),
            input: num("input")?,
            output: num("output")?,
            cache_read: num("cache_read")?,
            cache_write: num("cache_write")?,
            note,
        });
    }
    steve_stats::validate(&out).map_err(|f| invalid(&f))?;
    Ok(Some(out))
}

/// Writes (or with `None` deletes) a workspace setting, with an audit row and a revision, in one transaction.
fn save_setting(
    store: &Store,
    ws: &str,
    me: &str,
    key: &str,
    object_type: &str,
    value: Option<&Value>,
) -> Result<(), ApiError> {
    let (ws, me, key, object_type) = (
        ws.to_string(),
        me.to_string(),
        key.to_string(),
        object_type.to_string(),
    );
    let value = value.cloned();
    store
        .transaction(|c| {
            let t = now();
            match &value {
                Some(v) => c.execute(
                    "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)
                     ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                    params![ws, key, v.to_string(), t],
                )?,
                None => c.execute(
                    "DELETE FROM settings WHERE workspace_id = ? AND key = ?",
                    params![ws, key],
                )?,
            };
            let detail = json!({ "value": value });
            c.execute(
                "INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)",
                params![ws, t, me, key, ws, detail.to_string()],
            )?;
            let version: i64 = c.query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM revisions WHERE workspace_id = ? AND object_type = ? AND object_id = ?",
                params![ws, object_type, ws],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO revisions (workspace_id, object_type, object_id, version, patch, author, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![ws, object_type, ws, version, json!({ key.as_str(): value }).to_string(), me, t],
            )?;
            c.publish(steve_store::ChangeEvent::Settings { id: ws.clone() });
            c.publish(stats_changed(&ws, None));
            Ok::<_, StoreError>(())
        })
        .map_err(serr)
}

async fn prices_put(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let prices = parse_prices(&v)?;
    let value = prices
        .as_ref()
        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null));
    let s = api.0.store.clone();
    blocking(move || {
        save_setting(
            &s,
            &c.ws,
            &c.me,
            "stats.prices",
            "stats_prices",
            value.as_ref(),
        )
    })
    .await?;
    api.forget();
    ok(json!({ "ok": true, "prices": prices.unwrap_or_else(default_prices) }))
}

/// `POST /stats/prices/sync`: syncs the Models.dev prices now and answers with the outcome (`409` while
/// a sync is running). A failure keeps the prices of the last sync.
async fn prices_sync(State(api): State<StatsApi>) -> ApiResult {
    static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let Some(url) = api.0.price_url.clone() else {
        return Ok(json_response(
            409,
            &json!({ "error": steve_i18n::t!("hub.prices.sync_off") }),
        ));
    };
    let Ok(_running) = RUNNING.try_lock() else {
        return Ok(json_response(
            409,
            &json!({ "error": steve_i18n::t!("hub.prices.syncing") }),
        ));
    };
    let r = crate::price_sync::sync_once(&api.0.store, &crate::price_sync::client(), &url).await;
    api.forget();
    match r {
        Ok(crate::price_sync::Synced::Fetched { models, changed }) => {
            ok(json!({ "ok": true, "models": models, "changed": changed }))
        }
        Ok(crate::price_sync::Synced::Unchanged) => ok(json!({ "ok": true, "unchanged": true })),
        Err(e) => Ok(json_response(
            502,
            &json!({ "error": steve_i18n::t!("hub.prices.sync_failed", error = e) }),
        )),
    }
}

/// `PUT /stats/tz {tz}`: the zone days are cut in (an IANA name, `null` = the Hub's); the cube is
/// rebuilt in the new zone in the background.
async fn tz_put(State(api): State<StatsApi>, Extension(c): Extension<Ctx>, b: Bytes) -> ApiResult {
    let v = body(&b)?;
    let tz = match v.get("tz") {
        Some(Value::Null) => None,
        Some(Value::String(s)) if steve_stats::tz(Some(s)).is_some() => Some(s.trim().to_string()),
        _ => return Err(invalid("tz")),
    };
    let s = api.0.store.clone();
    let ws = c.ws.clone();
    let value = tz.as_ref().map(|t| json!(t));
    blocking(move || save_setting(&s, &c.ws, &c.me, "stats.tz", "stats_tz", value.as_ref()))
        .await?;
    api.forget();
    spawn_rebuild(api.0.store.clone(), ws);
    ok(json!({ "ok": true, "tz": tz }))
}

fn spawn_rebuild(store: Store, ws: String) {
    tokio::task::spawn_blocking(move || match steve_stats::rebuild(&store, &ws) {
        Ok(n) => tracing::info!("stats rebuild {ws}: {n} daily rows"),
        Err(e) => tracing::error!("stats rebuild {ws}: {e}"),
    });
}

/// `POST /stats/rebuild`: owner only; `202`, a `stats` event follows when it is done.
async fn rebuild(State(api): State<StatsApi>, Extension(c): Extension<Ctx>) -> ApiResult {
    let (ws, me) = (c.ws.clone(), c.me.clone());
    let owner = api
        .read(move |db| {
            let role: Option<String> = db
                .query_row(
                    "SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                    [&ws, &me],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?;
            Ok(role.as_deref() == Some("owner"))
        })
        .await?;
    if !owner {
        return Ok(json_response(
            403,
            &json!({ "error": steve_i18n::t!("hub.stats.owner_only") }),
        ));
    }
    api.forget();
    spawn_rebuild(api.0.store.clone(), c.ws.clone());
    Ok(json_response(202, &json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_cache_key_has_the_language() {
        let p: Params = [("from".to_string(), "2026-11-01".to_string())].into();
        let zh = key("ws_1", "summary", &p);
        let en = steve_i18n::scope(steve_i18n::EN, async { key("ws_1", "summary", &p) }).await;
        assert_ne!(zh, en);
        assert_eq!(zh, key("ws_1", "summary", &p));
    }

    #[test]
    fn prices_name_the_bad_field() {
        let p = |v: Value| parse_prices(&v).map_err(|e| e.to_string());
        assert_eq!(p(json!({"prices": null})), Ok(None));
        let good = json!({"prices": [{"match": "kiro/*", "input": 1, "output": 2, "cache_read": 0.1, "cache_write": 1.25}]});
        assert_eq!(p(good).map(|x| x.map(|v| v.len())), Ok(Some(1)));
        assert_eq!(
            p(
                json!({"prices": [{"match": "a", "input": -1, "output": 2, "cache_read": 0, "cache_write": 0}]})
            ),
            Err("参数不对：prices[0].input".into())
        );
        assert_eq!(
            p(
                json!({"prices": [{"match": "a", "input": 1, "output": 2, "cache_read": 0, "cache_write": 0},
                                {"match": " a ", "input": 1, "output": 2, "cache_read": 0, "cache_write": 0}]})
            ),
            Err("参数不对：prices[1].match".into())
        );
        assert_eq!(
            p(json!({"prices": [{"match": "", "input": 1}]})),
            Err("参数不对：prices[0].match".into())
        );
        assert_eq!(
            p(
                json!({"prices": [{"match": "a", "input": 1, "output": "x", "cache_read": 0, "cache_write": 0}]})
            ),
            Err("参数不对：prices[0].output".into())
        );
        assert_eq!(p(json!({})), Err("参数不对：prices".into()));
    }
}
