//! `/api/stats/*`: the summary, trends, breakdowns, the per-item table and
//! drill-down, a session's totals, the price table, the Models.dev price sync and time zone, and the rebuild. Reads go to the
//! store's read pool; summaries are cached per query until the next statistics change (at most 5 s).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Extension, Router};
use serde_json::{Value, json};
use steve_stats::metrics::{self, By, Query as StatsQuery};
use steve_stats::views::{self, Bucket, Metric};
use steve_stats::{Price, catalog, default_prices};
use steve_store::statistics::{Catalog, DailyRows, Setting, StatsSettings};
use steve_store::{ItemReads, Members, R, Store, StoreError, Workspaces, now};

use super::{ApiResult, Ctx, Noun, blocking, body, ok};
use crate::bus::Bus;
use crate::error::{ApiError, json_response};

/// How long a cached answer may be served, even without a statistics change (WIP and "now" move).
const FRESH: Duration = Duration::from_secs(5);
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct StatsKey {
    ws: String,
    what: String,
    lang: &'static str,
    params: BTreeMap<String, String>,
}
static CACHE: steve_runtime::cache::CacheSpace<StatsKey, Value> =
    steve_runtime::cache::CacheSpace {
        name: "stats",
        ttl: FRESH,
        stale_for: Duration::ZERO,
        share_pct: 15,
        tags: |key| vec![steve_runtime::cache::Tag::of("stats", &key.ws)],
        weigh: |v| serde_json::to_vec(v).map_or(1, |b| b.len().min(u32::MAX as usize) as u32),
    };
const SOURCES: &[&str] = &["agent", "gateway"];
const GROUPS: &[&str] = &[
    "none",
    "model",
    "harness",
    "bot",
    "project",
    "node",
    "session",
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
    /// Live harness adapter list (`id` → `name`) for stats labels (including runtime-discovered titles).
    harnesses: Option<steve_core::harnesses::HarnessSource>,
    /// key → (stats generation, computed at, answer)
    cache: steve_runtime::cache::Cached<StatsKey, Value>,
}

impl StatsApi {
    pub fn new(store: Store, bus: Bus, price_url: Option<String>) -> Self {
        let cache = bus.caches().space(&CACHE);
        StatsApi(Arc::new(Inner {
            store,
            bus,
            price_url,
            harnesses: None,
            cache,
        }))
    }

    pub fn with_harness_source(self, harnesses: steve_core::harnesses::HarnessSource) -> Self {
        StatsApi(Arc::new(Inner {
            store: self.0.store.clone(),
            bus: self.0.bus.clone(),
            price_url: self.0.price_url.clone(),
            harnesses: Some(harnesses),
            cache: self.0.cache.clone(),
        }))
    }

    fn harness_names(&self) -> HashMap<String, String> {
        self.0
            .harnesses
            .as_ref()
            .map(|src| src().into_iter().map(|h| (h.id, h.name)).collect())
            .unwrap_or_default()
    }

    async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut R<'_>) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let s = self.0.store.clone();
        blocking(move || s.read(|r| Ok(f(r))).map_err(serr)?).await
    }

    /// `f`'s answer for `key`, from the cache while no statistics changed and it is under 5 s old.
    async fn cached(
        &self,
        key: StatsKey,
        f: impl FnOnce(&mut R<'_>) -> Result<Value, ApiError> + Send + 'static,
    ) -> ApiResult {
        let value = self.0.cache.get_or_load(key, || self.read(f)).await?;
        ok(value.as_ref().clone())
    }

    fn forget(&self) {
        self.0
            .bus
            .caches()
            .invalidate(&[steve_runtime::cache::Tag::global("stats")]);
    }
}

/// The routes, behind `require_user` (merged with the module routes).
pub fn routes(api: StatsApi) -> Router {
    Router::new()
        .route("/stats/realtime", get(realtime))
        .route("/stats/nav", get(nav_usage))
        .route("/stats/summary", get(summary))
        .route("/stats/series", get(series))
        .route("/stats/breakdown", get(breakdown))
        .route("/stats/items", get(items))
        .route("/stats/items/{id}", get(item))
        .route("/stats/tree/{id}", get(tree))
        .route("/stats/events", get(events))
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

fn serr(e: StoreError) -> ApiError {
    ApiError::internal(e.to_string())
}

/// References have the same workspace boundary as historical statistics, without parsing dates.
fn check_scope(c: &mut R<'_>, ws: &str, p: &Params) -> Result<(), ApiError> {
    let get = |k: &str| p.get(k).map(String::as_str).filter(|v| !v.is_empty());
    if let Some(id) = get("project")
        && ItemReads(c).project_name(ws, id).map_err(serr)?.is_none()
    {
        return Err(ApiError::not_found(Noun::Project.missing()));
    }
    if let Some(id) = get("bot")
        && !Workspaces(c).has_bot(ws, id).map_err(serr)?
    {
        return Err(ApiError::not_found(Noun::Bot.missing()));
    }
    Ok(())
}

/// GET /stats/nav?range=7d|30d|all: durable menu open counts aggregated by workspace local day
/// from the unified telemetry daily series (`telemetry_daily`, metric `web.nav.open`).
async fn nav_usage(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    use steve_store::statistics::{TelemetryDaily, TelemetryMetric};
    use steve_types::stats::{NavUsage, NavUsageItem, NavUsageRange};

    let range = match p.get("range").map(String::as_str) {
        None | Some("" | "7d") => NavUsageRange::Days7,
        Some("30d") => NavUsageRange::Days30,
        Some("all") => NavUsageRange::All,
        Some(_) => return Err(invalid("range")),
    };
    let ws = c.ws;
    let usage = api
        .read(move |db| {
            let today = views::today(db, &ws, now()).map_err(serr)?;
            let bounds = match range {
                NavUsageRange::Days7 => {
                    Some(views::range("7d", None, None, &today).map_err(invalid)?)
                }
                NavUsageRange::Days30 => {
                    Some(views::range("30d", None, None, &today).map_err(invalid)?)
                }
                NavUsageRange::All => None,
            };
            let totals = TelemetryDaily(db)
                .totals(
                    &ws,
                    TelemetryMetric::WebNavOpen,
                    bounds.as_ref().map(|(f, t)| (f.as_str(), t.as_str())),
                )
                .map_err(serr)?;
            let mut items: Vec<NavUsageItem> = totals
                .rows
                .into_iter()
                .filter_map(|(item, opens)| {
                    let opens = u64::try_from(opens).ok().filter(|&n| n > 0)?;
                    crate::metrics::WEB_NAV_ITEMS
                        .contains(&item.as_str())
                        .then_some(NavUsageItem { item, opens })
                })
                .collect();
            items.sort_by(|a, b| {
                b.opens.cmp(&a.opens).then_with(|| {
                    let pos = |k: &str| {
                        crate::metrics::WEB_NAV_ITEMS
                            .iter()
                            .position(|x| *x == k)
                            .unwrap_or(usize::MAX)
                    };
                    pos(&a.item).cmp(&pos(&b.item))
                })
            });
            let total = items.iter().map(|x| x.opens).sum();
            Ok(NavUsage {
                range,
                from: bounds.map(|(f, _)| f).or(totals.since_day),
                to: today,
                total,
                items,
            })
        })
        .await?;
    let mut response = axum::response::IntoResponse::into_response(axum::Json(usage));
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

/// GET /stats/realtime: poll once per second; no historical cache or date range is consulted.
async fn realtime(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let value = api.read(move |r| realtime_in(r, &c.ws, &p)).await?;
    let mut response = json_response(200, &value);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

fn realtime_in(r: &mut steve_store::R<'_>, ws: &str, p: &Params) -> Result<Value, ApiError> {
    let get = |key: &str| p.get(key).map(String::as_str).filter(|s| !s.is_empty());
    let project = get("project");
    let bot = get("bot");
    check_scope(r, ws, p)?;
    let v = steve_stats::realtime::read(
        r,
        steve_stats::realtime::Scope {
            workspace: ws,
            project,
            bot,
        },
        now(),
    )
    .map_err(serr)?;
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// The common parameters: range, then the filters, each reference checked against the workspace.
fn query(c: &mut R<'_>, ws: &str, p: &Params) -> Result<StatsQuery, ApiError> {
    let get = |k: &str| p.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let today = views::today(c, ws, now()).map_err(serr)?;
    let (from, to) = views::range(get("range").unwrap_or("7d"), get("from"), get("to"), &today)
        .map_err(invalid)?;
    check_scope(c, ws, p)?;
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
        node: get("node").map(str::to_string),
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
fn zone_name(c: &mut R<'_>, ws: &str) -> Result<String, ApiError> {
    let tz = steve_stats::refresh::timezone(c, ws).map_err(serr)?;
    Ok(tz
        .map(|z| z.name().to_string())
        .or_else(hub_tz)
        .unwrap_or_else(|| "local".into()))
}

/// The cache key: the cached bodies hold translated labels, so the language is part of it.
fn key(ws: &str, what: &str, p: &Params) -> StatsKey {
    StatsKey {
        ws: ws.into(),
        what: what.into(),
        lang: steve_i18n::current(),
        params: p.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

/// A summary as JSON with its range in `range`.
fn summary_json(c: &mut R<'_>, q: &StatsQuery) -> Result<Value, ApiError> {
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
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        let mut v = summary_json(db, &q)?;
        let prev = match views::prev(&q.from, &q.to) {
            Some((from, to)) => summary_json(db, &StatsQuery { from, to, ..q })?,
            None => Value::Null,
        };
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
    let harnesses = api.harness_names();
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
        let s = views::series_with_harnesses(db, &q, metric, &group, bucket, &harnesses)
            .map_err(serr)?;
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
    let harnesses = api.harness_names();
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        let hour = hour_of(&p, &q)?;
        let rows = match tokens {
            Some(b) => {
                let names = views::Names::load(db, &c.ws)
                    .map_err(serr)?
                    .with_harnesses(harnesses);
                let daily = match (b, hour) {
                    (By::Session, Some(h)) => {
                        let bucket = format!("{}T{h:02}:00", q.from);
                        let mut v = metrics::by_session(db, &q, true).map_err(serr)?;
                        v.retain(|d| d.day == bucket);
                        v
                    }
                    (By::Session, None) => metrics::by_session(db, &q, false).map_err(serr)?,
                    // one hour of the day: the hourly cells of that bucket, as the hourly series has them
                    (_, Some(h)) => {
                        let bucket = format!("{}T{h:02}:00", q.from);
                        let mut v = metrics::hourly(db, &q).map_err(serr)?;
                        v.retain(|d| d.day == bucket);
                        v
                    }
                    (_, None) => metrics::daily(db, &q).map_err(serr)?,
                };
                let prices = metrics::book_of(db, &c.ws, daily.iter().map(|row| row.model.as_str())).map_err(serr)?;
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

/// Longest span the event explorer reads at once.
const EVENTS_SPAN_MAX: i64 = 400 * 86_400_000;

/// `GET /stats/events`: the event explorer. The span is the page's range (`range`, `from`, `to`), or `lo`/`hi`
/// (ms) once zoomed in; `a`/`b` (ms) the window the list covers; `kind` (turn, decision, mark) and `item` narrow
/// it, `buckets` sets the histogram's resolution (12–240, 96 by default).
async fn events(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Query(p): Query<Params>,
) -> ApiResult {
    let ms = |k: &str| -> Result<Option<i64>, ApiError> {
        p.get(k)
            .filter(|v| !v.is_empty())
            .map(|v| v.parse::<i64>().map_err(|_| invalid(k)))
            .transpose()
    };
    let (lo, hi, a, b) = (ms("lo")?, ms("hi")?, ms("a")?, ms("b")?);
    if lo.is_some() != hi.is_some()
        || lo
            .zip(hi)
            .is_some_and(|(l, h)| h <= l || h - l > EVENTS_SPAN_MAX)
    {
        return Err(invalid("lo"));
    }
    if a.is_some() != b.is_some() {
        return Err(invalid("a"));
    }
    let kind = p.get("kind").filter(|v| !v.is_empty()).cloned();
    if kind
        .as_deref()
        .is_some_and(|k| !steve_stats::events::KINDS.contains(&k))
    {
        return Err(invalid("kind"));
    }
    let buckets = match p.get("buckets").filter(|v| !v.is_empty()) {
        None => 96,
        Some(v) => v.parse::<usize>().map_err(|_| invalid("buckets"))?,
    };
    let item = p.get("item").filter(|v| !v.is_empty()).cloned();
    let k = key(&c.ws, "events", &p);
    api.cached(k, move |db| {
        let q = query(db, &c.ws, &p)?;
        let (lo, hi) = match lo.zip(hi) {
            Some(x) => x,
            None => {
                let tz = steve_stats::refresh::timezone(db, &c.ws).map_err(serr)?;
                views::window(&q, None, tz).ok_or_else(|| invalid("range"))?
            }
        };
        let scope = steve_stats::events::Scope {
            workspace_id: c.ws.clone(),
            project: q.project,
            bot: q.bot,
            kind,
            item,
        };
        let e = steve_stats::events::events(db, &scope, lo, hi, a.zip(b), buckets, now())
            .map_err(serr)?;
        serde_json::to_value(e).map_err(|e| ApiError::internal(e.to_string()))
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

/// `root` and the items started from it, each with its own usage and agent time (the agent tree).
async fn tree(
    State(api): State<StatsApi>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    api.read(move |db| match views::tree(db, &id).map_err(serr)? {
        Some((ws, t)) if ws == c.ws => {
            ok(serde_json::to_value(t).map_err(|e| ApiError::internal(e.to_string()))?)
        }
        _ => Err(ApiError::not_found(Noun::Item.missing())),
    })
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
        "cache_read": q.price.cache_read, "cache_write": q.price.cache_write,
        "note": steve_stats::prices::note_shown(&q.price.note),
    })
}

async fn prices_get(State(api): State<StatsApi>, Extension(c): Extension<Ctx>) -> ApiResult {
    let enabled = api.0.price_url.is_some();
    api.read(move |db| {
        let custom = StatsSettings(db).prices(&c.ws).map_err(serr)?.and_then(|value| serde_json::from_value::<Vec<Price>>(value).ok());
        // the default rows' notes are the Hub's and read in your language; rows you saved are yours
        let prices = custom
            .clone()
            .unwrap_or_else(|| steve_stats::prices::rows_shown(&default_prices()));
        let rows = DailyRows(db).model_totals(&c.ws).map_err(serr)?;
        let book = metrics::book_of(db, &c.ws, rows.iter().map(|(model, _)| model.as_str())).map_err(serr)?;
        let mut unpriced: BTreeMap<String, i64> = BTreeMap::new();
        let mut models = vec![];
        for (m, n) in rows {
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
        let sync = Catalog(db).state(catalog::SOURCE).map_err(serr)?;
        ok(json!({
            "prices": prices,
            "custom": custom.is_some(),
            "defaults": steve_stats::prices::rows_shown(&default_prices()),
            "unpriced": unpriced.into_iter().map(|(model, tokens)| json!({"model": model, "tokens": tokens})).collect::<Vec<_>>(),
            "models": models,
            "catalog": {
                "source": "Models.dev",
                "url": catalog::URL,
                "enabled": enabled,
                "synced": sync.synced, "tried": sync.tried, "error": sync.error, "models": sync.models,
            },
            "tz": StatsSettings(db).timezone(&c.ws).map_err(serr)?,
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

fn save_setting(
    store: &Store,
    ws: &str,
    me: &str,
    setting: Setting,
    value: Option<&Value>,
) -> Result<(), ApiError> {
    store
        .write(|w| StatsSettings(w).save(ws, me, setting, value, now()))
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
    blocking(move || save_setting(&s, &c.ws, &c.me, Setting::Prices, value.as_ref())).await?;
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
    blocking(move || save_setting(&s, &c.ws, &c.me, Setting::Timezone, value.as_ref())).await?;
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
            let role = Members::new(db).role(&ws, &me).map_err(serr)?;
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
    #[tokio::test]
    async fn a_committed_statistics_notice_invalidates_the_query_immediately() {
        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let cache = bus.caches().space(&CACHE);
        let key = key("w", "summary", &Params::new());
        cache
            .get_or_load(key.clone(), || async { Ok::<_, ()>(json!(1)) })
            .await
            .unwrap();
        bus.publish(steve_types::StatsEvent::Stats {
            ws: "w".into(),
            day: None,
        });
        assert_eq!(
            *cache
                .get_or_load(key, || async { Ok::<_, ()>(json!(2)) })
                .await
                .unwrap(),
            json!(2)
        );
    }

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

pub(crate) const CACHE_SPEC: (&str, u8) = (CACHE.name, CACHE.share_pct);

#[cfg(test)]
pub(crate) fn register_cache_spaces(caches: &steve_runtime::cache::Caches) {
    let _ = caches.space(&CACHE);
}
