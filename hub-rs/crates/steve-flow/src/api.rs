//! REST of scheduled tasks, relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `GET /schedules` | `Schedule[]` |
//! | `POST /schedules` | create, `Schedule` |
//! | `POST /schedules/parse` | `{when, tz?}` → `{cron, tz, describe, next, assumed?, notes?, by}` |
//! | `GET /schedules/:id` | `Schedule` + `runs` (latest 10) |
//! | `PATCH /schedules/:id` | `Schedule` |
//! | `DELETE /schedules/:id` | `{ok:true}` |
//! | `POST /schedules/:id/run` | `{run_id}` |
//! | `GET /schedules/:id/runs?before=&limit=` | `Run[]` (≤50) |

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules};

use crate::FlowCx;
use crate::cron::{CronExpr, parse_tz, validate};
use crate::policy::{MissedPolicy, OverlapPolicy};
use crate::runs::{self, RunView};
use crate::scheduler;
use crate::schedules::{self, DEFAULT_BUDGET_USD, Fields, MAX_BUDGET_USD, Mode, Origin, Spec};
use crate::when::{self, MODEL_TIMEOUT_SECS, Parsed, WhenError};
use steve_modules::Tier;

type Cx = Extension<Arc<FlowCx>>;

/// The schedule, signal, source, flow, run and `/bots/:id/flows` routes. Mount on the `/api` router behind the layer that inserts [`Caller`].
pub fn routes(cx: Arc<FlowCx>) -> Router<Modules> {
    Router::new()
        .route("/schedules", get(list).post(create))
        .route("/schedules/parse", post(parse))
        .route("/schedules/{id}", get(detail).patch(patch).delete(remove))
        .route("/schedules/{id}/run", post(run_now))
        .route("/schedules/{id}/runs", get(history))
        .merge(crate::events_api::routes())
        .merge(crate::flows_api::routes())
        .merge(crate::corrections::routes())
        .merge(crate::flow_drafts::routes())
        .merge(crate::bots::routes())
        .layer(Extension(cx))
}

/// `null` stays distinguishable from absent (`project_id: null` clears it).
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Body of `POST /schedules` and `PATCH /schedules/:id`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScheduleBody {
    pub name: Option<String>,
    pub when: Option<String>,
    pub cron: Option<String>,
    pub tz: Option<String>,
    pub what: Option<String>,
    pub bot: Option<String>,
    pub mode: Option<Mode>,
    #[serde(default, deserialize_with = "nullable")]
    pub project_id: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub missed: Option<MissedPolicy>,
    pub overlap: Option<OverlapPolicy>,
    pub budget_usd: Option<f64>,
    pub enabled: Option<bool>,
}

impl ScheduleBody {
    fn check(&self, creating: bool) -> ApiResult<()> {
        // zod order: every issue, in the order of the schema's fields
        let mut c = Check::new();
        let need = |present: bool| !creating || present;
        c.that("name", need(self.name.is_some()))
            .opt_len("name", self.name.as_deref(), 1, 40)
            .that("when", need(self.when.is_some() || self.cron.is_some()))
            .opt_len("when", self.when.as_deref(), 1, 60)
            .opt_len("cron", self.cron.as_deref(), 1, 100)
            .opt_len("tz", self.tz.as_deref(), 1, 64)
            .that("what", need(self.what.is_some()))
            .opt_len("what", self.what.as_deref(), 1, 2000)
            .that("bot", need(self.bot.is_some()))
            .opt_len("bot", self.bot.as_deref(), 1, 100)
            .that("mode", need(self.mode.is_some()));
        if let Some(t) = &self.tags {
            c.list("tags", t, 10, 100);
        }
        if let Some(b) = self.budget_usd {
            c.that("budget_usd", (0.0..=MAX_BUDGET_USD).contains(&b));
        }
        if let Some(Some(p)) = &self.project_id {
            c.len("project_id", p, 1, 100);
        }
        c.done()
    }
}

fn utc(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

fn zone(name: &str) -> ApiResult<Tz> {
    parse_tz(name).map_err(|e| ApiError::rule(e.to_string()))
}

fn when_error(e: WhenError) -> ApiError {
    ApiError::rule(e.to_string())
}

/// `when` → cron: the rules, then the model (fast, 30 s), each answer validated like a cron.
pub async fn parse_phrase(cx: &FlowCx, phrase: &str, tz: Tz) -> ApiResult<Parsed> {
    let now = utc(cx.now());
    match when::parse_when(phrase, tz, now) {
        Ok(p) => Ok(p),
        Err(WhenError::NotUnderstood) => {
            let out = steve_modules::gateway::purpose(
                "draft",
                cx.modules.gateway.run(
                    Tier::Fast,
                    &when::model_prompt(phrase),
                    Duration::from_secs(MODEL_TIMEOUT_SECS),
                ),
            )
            .await
            .map_err(|e| {
                tracing::warn!("when model fallback: {e}");
                when_error(WhenError::NotUnderstood)
            })?;
            when::accept_model_output(&out.text, tz, now).map_err(when_error)
        }
        Err(e) => Err(when_error(e)),
    }
}

/// The `(when, cron)` pair of a schedule from what the request gives: `cron` wins when both are
/// there; without `when` the phrase stored is the description.
pub async fn resolve_time(
    cx: &FlowCx,
    when: Option<&str>,
    cron: Option<&str>,
    tz: Tz,
) -> ApiResult<(String, CronExpr)> {
    let expr = match (cron, when) {
        (Some(c), _) => {
            validate(c, tz, utc(cx.now())).map_err(|e| ApiError::rule(e.to_string()))?
        }
        (None, Some(w)) => {
            let p = parse_phrase(cx, w, tz).await?;
            CronExpr::parse(&p.cron).map_err(|e| ApiError::rule(e.to_string()))?
        }
        (None, None) => return Err(ApiError::invalid("when")),
    };
    let when = when
        .map(|w| w.trim().to_string())
        .unwrap_or_else(|| expr.describe_zh());
    Ok((when, expr))
}

fn view(cx: &FlowCx, ws: &str, id: &str) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let s = schedules::own(c, ws, id)?;
        Ok(Json(serde_json::to_value(schedules::view(c, &s)?)?))
    })
}

async fn list(Extension(cx): Cx, who: Caller) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let out = schedules::list(c, &who.ws)?
            .iter()
            .map(|s| schedules::view(c, s))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(Json(serde_json::to_value(out)?))
    })
}

async fn create(
    Extension(cx): Cx,
    who: Caller,
    Body(b): Body<ScheduleBody>,
) -> ApiResult<Json<Value>> {
    b.check(true)?;
    let tz_name = match &b.tz {
        Some(t) => t.trim().to_string(),
        None => cx.store().reader(|c| schedules::workspace_tz(c, &who.ws))?,
    };
    let tz = zone(&tz_name)?;
    let (when, expr) = resolve_time(&cx, b.when.as_deref(), b.cron.as_deref(), tz).await?;
    let spec = Spec {
        when,
        cron: expr.as_str().into(),
        tz: tz.name().into(),
        what: b.what.clone().unwrap_or_default(),
        bot: b.bot.clone().unwrap_or_default(),
        mode: b.mode.unwrap_or(Mode::ItemAuto),
        project_id: b.project_id.clone().flatten(),
        tags: b.tags.clone().unwrap_or_default(),
        missed: b.missed.unwrap_or_default(),
        overlap: b.overlap.unwrap_or_default(),
        budget_usd: b.budget_usd.unwrap_or(DEFAULT_BUDGET_USD),
    };
    let name = b.name.clone().unwrap_or_default();
    let now = cx.now();
    let id = cx.store().transaction(|tx| -> ApiResult<_> {
        schedules::check_refs(tx, &who.ws, &spec)?;
        schedules::insert(
            tx,
            &who.ws,
            &who.me,
            &Fields {
                name: name.trim(),
                spec: &spec,
                enabled: b.enabled.unwrap_or(true),
            },
            &Origin::default(),
            now,
        )
    })?;
    cx.wake();
    view(&cx, &who.ws, &id)
}

/// The new spec of a patched schedule (`None` fields keep their value).
pub async fn patched_spec(cx: &FlowCx, old: &Spec, b: &ScheduleBody) -> ApiResult<Spec> {
    let mut spec = old.clone();
    if let Some(t) = &b.tz {
        spec.tz = zone(t)?.name().into();
    }
    let tz = zone(&spec.tz)?;
    if b.when.is_some() || b.cron.is_some() {
        let (when, expr) = resolve_time(cx, b.when.as_deref(), b.cron.as_deref(), tz).await?;
        spec.when = if b.when.is_some() {
            when
        } else {
            expr.describe_zh()
        };
        spec.cron = expr.as_str().into();
    } else if spec.tz != old.tz {
        validate(&spec.cron, tz, utc(cx.now())).map_err(|e| ApiError::rule(e.to_string()))?;
    }
    if let Some(v) = &b.what {
        spec.what = v.clone();
    }
    if let Some(v) = &b.bot {
        spec.bot = v.clone();
    }
    if let Some(v) = b.mode {
        spec.mode = v;
    }
    if let Some(v) = &b.project_id {
        spec.project_id = v.clone();
    }
    if let Some(v) = &b.tags {
        spec.tags = v.clone();
    }
    if let Some(v) = b.missed {
        spec.missed = v;
    }
    if let Some(v) = b.overlap {
        spec.overlap = v;
    }
    if let Some(v) = b.budget_usd {
        spec.budget_usd = v;
    }
    Ok(spec)
}

async fn patch(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<ScheduleBody>,
) -> ApiResult<Json<Value>> {
    b.check(false)?;
    let old = cx.store().reader(|c| schedules::own(c, &who.ws, &id))?;
    let spec = patched_spec(&cx, &old.spec, &b).await?;
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<_> {
        // Re-read: the row may have changed while the model was thinking.
        let cur = schedules::own(tx, &who.ws, &id)?;
        schedules::check_refs(tx, &who.ws, &spec)?;
        let name = b
            .name
            .as_deref()
            .map(str::trim)
            .unwrap_or(&cur.name)
            .to_string();
        schedules::update(
            tx,
            &cur,
            &Fields {
                name: &name,
                spec: &spec,
                enabled: b.enabled.unwrap_or(cur.enabled),
            },
            &who.me,
            "",
            now,
        )
    })?;
    cx.wake();
    view(&cx, &who.ws, &id)
}

async fn detail(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let s = schedules::own(c, &who.ws, &id)?;
        let mut v = serde_json::to_value(schedules::view(c, &s)?)?;
        let recent: Vec<RunView> = runs::page(c, &s.workspace_id, &s.id, None, 10)?
            .iter()
            .map(|r| r.view())
            .collect();
        v["runs"] = serde_json::to_value(recent)?;
        Ok(Json(v))
    })
}

async fn remove(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<_> {
        let s = schedules::own(tx, &who.ws, &id)?;
        schedules::delete(tx, &s, now)
    })?;
    cx.wake();
    Ok(Json(json!({ "ok": true })))
}

async fn run_now(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| schedules::own(c, &who.ws, &id))?;
    let (run_id, started) = scheduler::start_manual(&cx, &who.ws, &id)?;
    if started {
        scheduler::spawn_drive(cx.clone(), run_id.clone());
    }
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Debug, Deserialize)]
struct PageParams {
    before: Option<i64>,
    limit: Option<i64>,
}

async fn history(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let s = schedules::own(c, &who.ws, &id)?;
        let page: Vec<RunView> =
            runs::page(c, &s.workspace_id, &s.id, q.before, q.limit.unwrap_or(20))?
                .iter()
                .map(|r| r.view())
                .collect();
        Ok(Json(serde_json::to_value(page)?))
    })
}

#[derive(Debug, Deserialize)]
struct ParseBody {
    when: String,
    tz: Option<String>,
}

async fn parse(Extension(cx): Cx, who: Caller, Body(b): Body<ParseBody>) -> ApiResult<Json<Value>> {
    Check::new().len("when", &b.when, 1, 60).done()?;
    let tz = match &b.tz {
        Some(t) => zone(t)?,
        None => zone(&cx.store().reader(|c| schedules::workspace_tz(c, &who.ws))?)?,
    };
    let p = parse_phrase(&cx, &b.when, tz).await?;
    Ok(Json(serde_json::to_value(p)?))
}
