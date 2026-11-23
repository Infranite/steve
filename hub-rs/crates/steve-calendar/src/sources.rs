//! The built-in calendar sources. Both are connection providers (they show on the 接入 page and are
//! connected with a form) and [`CalendarSource`]s, and need no outside service of a contributor:
//!
//! - [`IcsSource`] (`ics`): a calendar subscription address (`https://` or `webcal://`, kept in the
//!   vault: such addresses usually carry a secret) or an `.ics` file uploaded afterwards. Read only.
//! - [`DemoSource`] (`cal_demo`): synthetic calendars around today, for trying Steve and for tests.
//!   Registered only when the operator asks for it (`STEVE_CALENDAR_DEMO=1`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Weekday};
use chrono_tz::Tz;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Health, Integration, ProviderError,
};
use steve_store::calendar::CalendarRepo;

use crate::ics;
use crate::profile::{
    Attendee, Busy, CalendarSource, Features, Person, RemoteCalendar, RemoteEvent, SourceError,
    When, Window, feature,
};

/// The capability codes of a read-only calendar source.
const READ_CAPS: &[&str] = &["cal.read"];

fn integration(provider: &str, name: String) -> Integration {
    let mut i = Integration::from_caps(provider, &name, READ_CAPS);
    i.provider_name = name;
    i
}

/// One field of the connection's config.
fn config(cx: &ConnCx, key: &str) -> Option<String> {
    cx.config_str(key)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The zone the form named, else `def`.
fn zone_of(cx: &ConnCx, def: Tz) -> Tz {
    config(cx, "tz")
        .as_deref()
        .and_then(ics::zone)
        .unwrap_or(def)
}

fn provider_error(e: SourceError) -> ProviderError {
    match e {
        SourceError::Auth(m) => ProviderError::Auth(m),
        SourceError::Transient(m) => ProviderError::Transient(m),
        e => ProviderError::Rejected(e.to_string()),
    }
}

// ------------------------------------------------------------------------------------------- ICS

/// How long a fetched calendar is reused (ms; one sync reads it several times).
const ICS_REUSE_MS: i64 = 60_000;
/// How long a fetch may take.
const ICS_TIMEOUT: StdDuration = StdDuration::from_secs(30);

pub struct IcsSource {
    fields: Vec<FieldDef>,
    /// Connection → (when read, Unix ms, the calendar).
    cache: Mutex<HashMap<String, (i64, Arc<ics::Calendar>)>>,
}

impl Default for IcsSource {
    fn default() -> Self {
        Self::new()
    }
}

/// `webcal://` is `https://`; only http(s) addresses without a user name are fetched.
pub fn subscription_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let fixed = match raw.get(..9) {
        Some(p) if p.eq_ignore_ascii_case("webcal://") => format!("https://{}", &raw[9..]),
        _ => raw.to_string(),
    };
    let u = url::Url::parse(&fixed).map_err(|_| "订阅地址不是一个网址".to_string())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("订阅地址要以 https:// 或 webcal:// 开头".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("订阅地址里不能带用户名和密码".into());
    }
    Ok(u.to_string())
}

impl IcsSource {
    pub const ID: &'static str = "ics";

    pub fn new() -> Self {
        Self {
            fields: vec![
                FieldDef::plain("name", "名称")
                    .optional()
                    .with_hint("在 Steve 里显示的名字；不填就用日历文件里的"),
                FieldDef::secret("url", "订阅地址")
                    .optional()
                    .with_hint("以 https:// 或 webcal:// 开头的 .ics 地址；不填的话，连接后在这里上传 .ics 文件"),
                FieldDef::plain("tz", "时区")
                    .optional()
                    .with_hint("日历文件没写时区时用，例如 Asia/Shanghai"),
            ],
            cache: Mutex::default(),
        }
    }

    async fn url(cx: &ConnCx) -> Result<Option<String>, SourceError> {
        let cx2 = cx.clone();
        tokio::task::spawn_blocking(move || {
            cx2.with_credential(|c| c.text("url").map(|v| v.trim().to_string()))
        })
        .await
        .map_err(|_| SourceError::Transient("任务失败".into()))?
        .map_err(|e| match e {
            ProviderError::Auth(m) => SourceError::Auth(m),
            e => SourceError::Transient(e.to_string()),
        })
        .map(|u| u.filter(|u| !u.is_empty()))
    }

    async fn fetch(cx: &ConnCx, raw: &str) -> Result<String, SourceError> {
        let url = subscription_url(raw).map_err(SourceError::Invalid)?;
        steve_modules::net::check_url(&url).map_err(|e| SourceError::Transient(e.to_string()))?;
        let mut res = cx
            .http()
            .get(&url)
            .timeout(ICS_TIMEOUT)
            .send()
            .await
            .map_err(|e| SourceError::Transient(format!("取不到日历：{}", e.without_url())))?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(SourceError::Auth(format!("日历地址拒绝访问（{status}）")));
        }
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            return Err(SourceError::Invalid(format!(
                "日历地址不存在了（{status}）"
            )));
        }
        if !status.is_success() {
            return Err(SourceError::Transient(format!("日历地址答复 {status}")));
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|e| SourceError::Transient(format!("日历没读完：{}", e.without_url())))?
        {
            if body.len() + chunk.len() > ics::MAX_BYTES {
                return Err(SourceError::Invalid(format!(
                    "日历文件超过 {} MB",
                    ics::MAX_BYTES / 1024 / 1024
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// The connection's calendar: fetched, or the uploaded file; `None` before either exists.
    /// The connection's calendar: fetched (reused for a minute), or the uploaded file; `None` before
    /// either exists.
    async fn read(&self, cx: &ConnCx) -> Result<Option<Arc<ics::Calendar>>, SourceError> {
        let Some(u) = Self::url(cx).await? else {
            let store = cx.connections().modules().store.clone();
            let id = cx.id().to_string();
            let file = store
                .read(|c| CalendarRepo::new(c).ics_file(&id))
                .map_err(|e| SourceError::Transient(e.to_string()))?;
            return file
                .map(|(_, b)| {
                    ics::Calendar::parse(&b)
                        .map(Arc::new)
                        .map_err(SourceError::Invalid)
                })
                .transpose();
        };
        let now = cx.connections().modules().clock.now();
        if let Some((at, c)) = crate::lock(&self.cache).get(cx.id()).cloned()
            && (0..ICS_REUSE_MS).contains(&(now - at))
        {
            return Ok(Some(c));
        }
        let body = Self::fetch(cx, &u).await?;
        let cal = Arc::new(ics::Calendar::parse(&body).map_err(SourceError::Invalid)?);
        crate::lock(&self.cache).insert(cx.id().to_string(), (now, cal.clone()));
        Ok(Some(cal))
    }

    /// Whether the connection reads a subscription address (else an uploaded file).
    pub async fn subscribed_cx(cx: &ConnCx) -> bool {
        matches!(Self::url(cx).await, Ok(Some(_)))
    }
}

#[async_trait]
impl ConnectionProvider for IcsSource {
    fn id(&self) -> &str {
        Self::ID
    }
    fn system(&self) -> &str {
        Self::ID
    }
    fn name(&self) -> &str {
        "ICS 日历订阅"
    }
    fn localize(&self, text: &str) -> Option<String> {
        const KEYS: &[&str] = &[
            "calendar.ics.name",
            "calendar.ics.field_name",
            "calendar.ics.field_name_hint",
            "calendar.ics.field_url",
            "calendar.ics.field_url_hint",
            "calendar.ics.field_tz",
            "calendar.ics.field_tz_hint",
            "calendar.ics.note",
        ];
        steve_i18n::reword!(text, KEYS)
    }
    fn caps(&self) -> &[&'static str] {
        READ_CAPS
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
    fn integration(&self) -> Integration {
        let mut i = integration(Self::ID, self.name().to_string());
        for c in &mut i.capabilities {
            c.note = Some("读一个日历订阅地址或 .ics 文件里的日程，只读".into());
        }
        i
    }
    /// A subscription address is fetched once and must be a calendar; without one, the file comes later.
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let url = Self::url(cx).await.map_err(provider_error)?;
        let mut name = config(cx, "name");
        if let Some(u) = &url {
            let body = Self::fetch(cx, u).await.map_err(provider_error)?;
            let cal = ics::Calendar::parse(&body).map_err(ProviderError::Rejected)?;
            name = name.or(cal.name.clone()).or_else(|| {
                url::Url::parse(&subscription_url(u).unwrap_or_default())
                    .ok()
                    .and_then(|x| x.host_str().map(str::to_string))
            });
        }
        if let Some(z) = config(cx, "tz")
            && ics::zone(&z).is_none()
        {
            return Err(ProviderError::Rejected(format!("不认识的时区：{z}")));
        }
        let name = name.unwrap_or_else(|| self.name().to_string());
        Ok(AccountInfo::new(format!("ics:{}", cx.id()), name).kind("calendar"))
    }
    /// Health comes from the syncs.
    async fn health(&self, _cx: &ConnCx) -> Health {
        Health::Ok
    }
}

#[async_trait]
impl CalendarSource for IcsSource {
    fn adapter(&self) -> &str {
        Self::ID
    }
    async fn features(&self, _cx: &ConnCx) -> Result<Option<Features>, SourceError> {
        Ok(Some(Features::of(&[feature::INSTANCES, feature::ICAL_UID])))
    }
    async fn calendars(&self, cx: &ConnCx) -> Result<Vec<RemoteCalendar>, SourceError> {
        let Some(cal) = self.read(cx).await? else {
            return Ok(Vec::new());
        };
        Ok(vec![RemoteCalendar {
            id: "ics".into(),
            name: config(cx, "name")
                .or(cal.name.clone())
                .unwrap_or_else(|| "ICS".into()),
            kind: Some("subscribed".into()),
            role: Some("reader".into()),
            tz: cal
                .tz
                .clone()
                .or_else(|| config(cx, "tz").filter(|z| ics::zone(z).is_some()))
                .or_else(|| Some(steve_modules::zone::default_tz().name().into())),
            ..Default::default()
        }])
    }
    async fn list(
        &self,
        cx: &ConnCx,
        _calendar: &str,
        w: Window,
    ) -> Result<Vec<RemoteEvent>, SourceError> {
        let Some(cal) = self.read(cx).await? else {
            return Ok(Vec::new());
        };
        Ok(cal.occurrences(w, zone_of(cx, steve_modules::zone::default_tz())))
    }
    async fn get(
        &self,
        cx: &ConnCx,
        calendar: &str,
        event: &str,
    ) -> Result<Option<RemoteEvent>, SourceError> {
        // the file has no lookup: the occurrence is found in a wide window
        let now = cx.connections().modules().clock.now();
        let w = Window {
            from_ms: now - 400 * 86_400_000,
            to_ms: now + 400 * 86_400_000,
        };
        Ok(self
            .list(cx, calendar, w)
            .await?
            .into_iter()
            .find(|e| e.id == event))
    }
}

// ------------------------------------------------------------------------------------------ demo

/// The person whose free/busy the demo's second calendar shows.
pub const DEMO_COLLEAGUE: &str = "colleague@example.com";

pub struct DemoSource {
    fields: Vec<FieldDef>,
}

impl Default for DemoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoSource {
    pub const ID: &'static str = "cal_demo";

    pub fn new() -> Self {
        Self {
            fields: vec![
                FieldDef::plain("tz", "时区")
                    .optional()
                    .with_default("Asia/Shanghai")
                    .with_hint("示例日程排在这个时区的工作时间里"),
            ],
        }
    }

    fn tz(cx: &ConnCx) -> Tz {
        zone_of(cx, chrono_tz::Asia::Shanghai)
    }

    /// The days the demo fills: a week back, three weeks ahead of today (in the demo's zone).
    fn days(cx: &ConnCx, w: Window) -> Vec<NaiveDate> {
        let tz = Self::tz(cx);
        let now = cx.connections().modules().clock.now();
        let today = steve_runtime::tz::Zone::parse(tz.name())
            .ok()
            .and_then(|z| z.local(now).ok())
            .map(|d| d.date())
            .unwrap_or_default();
        (-7..=21)
            .map(|k| today + Duration::days(k))
            .filter(|d| {
                let s = ics::local_ms(tz, d.and_time(NaiveTime::MIN));
                s < w.to_ms && s + 86_400_000 > w.from_ms
            })
            .collect()
    }

    fn at(tz: Tz, d: NaiveDate, h: u32, m: u32) -> i64 {
        ics::local_ms(tz, d.and_hms_opt(h, m, 0).expect("time"))
    }

    fn work(cx: &ConnCx, w: Window) -> Vec<RemoteEvent> {
        let tz = Self::tz(cx);
        let now = cx.connections().modules().clock.now();
        let today = steve_runtime::tz::Zone::parse(tz.name())
            .ok()
            .and_then(|z| z.local(now).ok())
            .map(|d| d.date())
            .unwrap_or_default();
        let me = || Attendee {
            who: Person {
                me: true,
                ..Default::default()
            },
            rsvp: Some("accepted".into()),
            ..Default::default()
        };
        let guest = |email: &str, name: &str, rsvp: &str| Attendee {
            who: Person {
                email: Some(email.into()),
                name: Some(name.into()),
                ..Default::default()
            },
            rsvp: Some(rsvp.into()),
            ..Default::default()
        };
        let timed = |id: &str, d: NaiveDate, (h, m): (u32, u32), mins: i64, title: &str| {
            let s = Self::at(tz, d, h, m);
            RemoteEvent {
                id: id.into(),
                ical_uid: Some(format!("{id}@demo.invalid")),
                title: Some(title.into()),
                start: When {
                    ms: Some(s),
                    tz: Some(tz.name().into()),
                    ..Default::default()
                },
                end: When {
                    ms: Some(s + mins * 60_000),
                    tz: Some(tz.name().into()),
                    ..Default::default()
                },
                status: Some("confirmed".into()),
                my_rsvp: Some("accepted".into()),
                can_modify: true,
                ..Default::default()
            }
        };
        let mut out = Vec::new();
        for d in Self::days(cx, w) {
            let key = d.format("%Y%m%d").to_string();
            if d.weekday().number_from_monday() <= 5 {
                let mut e = timed(&format!("standup@{key}"), d, (9, 30), 15, "站会");
                e.series_id = Some("standup".into());
                e.original_start = Some(key.clone());
                e.ical_uid = Some("standup@demo.invalid".into());
                e.attendees = vec![me(), guest(DEMO_COLLEAGUE, "陈默", "accepted")];
                out.push(e);
            }
            if d.weekday() == Weekday::Tue {
                let mut e = timed(&format!("one-on-one@{key}"), d, (15, 0), 30, "一对一");
                e.series_id = Some("one-on-one".into());
                e.original_start = Some(key.clone());
                e.ical_uid = Some("one-on-one@demo.invalid".into());
                e.attendees = vec![me(), guest(DEMO_COLLEAGUE, "陈默", "accepted")];
                out.push(e);
            }
            let k = (d - today).num_days();
            if k == 1 {
                let mut e = timed("quarterly-review", d, (14, 0), 60, "季度复盘");
                e.location = Some("3 号会议室".into());
                e.meeting_url = Some("https://meet.google.com/demo-review".into());
                e.description = Some("回顾这个季度的目标完成情况，列出下季度的重点。".into());
                e.organizer = Some(Person {
                    me: true,
                    ..Default::default()
                });
                e.attendees = vec![
                    me(),
                    guest(DEMO_COLLEAGUE, "陈默", "accepted"),
                    guest("partner@partner.example", "林舟", "tentative"),
                ];
                out.push(e);
                let mut c = timed("design-review", d, (14, 30), 60, "评审会");
                c.can_modify = false;
                c.organizer = Some(Person {
                    email: Some(DEMO_COLLEAGUE.into()),
                    name: Some("陈默".into()),
                    ..Default::default()
                });
                c.attendees = vec![me(), guest(DEMO_COLLEAGUE, "陈默", "accepted")];
                out.push(c);
            }
            if k == 2 {
                let n = d + Duration::days(1);
                out.push(RemoteEvent {
                    id: "offsite".into(),
                    ical_uid: Some("offsite@demo.invalid".into()),
                    title: Some("团建".into()),
                    start: When::day(&d.format("%Y-%m-%d").to_string()),
                    end: When::day(&n.format("%Y-%m-%d").to_string()),
                    status: Some("confirmed".into()),
                    can_modify: true,
                    ..Default::default()
                });
            }
            if k == 3 {
                let mut e = timed("customer-demo", d, (10, 0), 60, "客户演示");
                e.status = Some("tentative".into());
                e.my_rsvp = Some("needs_action".into());
                e.can_modify = false;
                e.organizer = Some(Person {
                    email: Some("partner@partner.example".into()),
                    name: Some("林舟".into()),
                    ..Default::default()
                });
                let mut mine = me();
                mine.rsvp = Some("needs_action".into());
                e.attendees = vec![mine, guest("partner@partner.example", "林舟", "accepted")];
                out.push(e);
            }
        }
        out.retain(|e| {
            let s = e.start.ms.unwrap_or_else(|| {
                e.start
                    .date
                    .as_deref()
                    .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| ics::local_ms(tz, d.and_time(NaiveTime::MIN)))
                    .unwrap_or(0)
            });
            s < w.to_ms
        });
        out
    }

    fn colleague(cx: &ConnCx, w: Window) -> Vec<Busy> {
        let tz = Self::tz(cx);
        let mut out = Vec::new();
        for d in Self::days(cx, w) {
            if d.weekday().number_from_monday() > 5 {
                continue;
            }
            for (h0, h1) in [(10, 12), (14, 15)] {
                let b = Busy {
                    start_ms: Self::at(tz, d, h0, 0),
                    end_ms: Self::at(tz, d, h1, 0),
                };
                if b.start_ms < w.to_ms && b.end_ms > w.from_ms {
                    out.push(b);
                }
            }
        }
        out
    }
}

#[async_trait]
impl ConnectionProvider for DemoSource {
    fn id(&self) -> &str {
        Self::ID
    }
    fn system(&self) -> &str {
        Self::ID
    }
    fn name(&self) -> &str {
        "示例日历"
    }
    fn localize(&self, text: &str) -> Option<String> {
        const KEYS: &[&str] = &[
            "calendar.demo.name",
            "calendar.demo.field_tz",
            "calendar.demo.field_tz_hint",
            "calendar.demo.note",
        ];
        steve_i18n::reword!(text, KEYS)
    }
    fn caps(&self) -> &[&'static str] {
        READ_CAPS
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
    fn integration(&self) -> Integration {
        let mut i = integration(Self::ID, self.name().to_string());
        for c in &mut i.capabilities {
            c.note = Some("合成的示例日程，用来试用日历；不连接任何服务".into());
        }
        i
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        if let Some(z) = config(cx, "tz")
            && ics::zone(&z).is_none()
        {
            return Err(ProviderError::Rejected(format!("不认识的时区：{z}")));
        }
        Ok(AccountInfo::new(format!("demo:{}", cx.id()), self.name()).kind("calendar"))
    }
    async fn health(&self, _cx: &ConnCx) -> Health {
        Health::Ok
    }
}

#[async_trait]
impl CalendarSource for DemoSource {
    fn adapter(&self) -> &str {
        Self::ID
    }
    async fn features(&self, _cx: &ConnCx) -> Result<Option<Features>, SourceError> {
        Ok(Some(Features::of(&[
            feature::INSTANCES,
            feature::ICAL_UID,
            feature::FREEBUSY,
        ])))
    }
    async fn calendars(&self, cx: &ConnCx) -> Result<Vec<RemoteCalendar>, SourceError> {
        let tz = Some(Self::tz(cx).name().to_string());
        Ok(vec![
            RemoteCalendar {
                id: "work".into(),
                name: "示例 · 工作".into(),
                color: Some("#3b82f6".into()),
                kind: Some("primary".into()),
                role: Some("owner".into()),
                tz: tz.clone(),
                ..Default::default()
            },
            RemoteCalendar {
                id: "colleague".into(),
                name: "示例 · 同事".into(),
                color: Some("#a855f7".into()),
                kind: Some("shared".into()),
                role: Some("free_busy".into()),
                tz,
                owner: Some(Person {
                    email: Some(DEMO_COLLEAGUE.into()),
                    name: Some("陈默".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ])
    }
    async fn list(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<RemoteEvent>, SourceError> {
        Ok(match calendar {
            "work" => Self::work(cx, w),
            "colleague" => Self::colleague(cx, w)
                .iter()
                .map(|b| RemoteEvent {
                    id: format!("busy:{}", b.start_ms),
                    start: When::at(b.start_ms),
                    end: When::at(b.end_ms),
                    ..Default::default()
                })
                .collect(),
            _ => Vec::new(),
        })
    }
    async fn get(
        &self,
        cx: &ConnCx,
        calendar: &str,
        event: &str,
    ) -> Result<Option<RemoteEvent>, SourceError> {
        let now = cx.connections().modules().clock.now();
        let w = Window {
            from_ms: now - 8 * 86_400_000,
            to_ms: now + 22 * 86_400_000,
        };
        Ok(self
            .list(cx, calendar, w)
            .await?
            .into_iter()
            .find(|e| e.id == event))
    }
    async fn freebusy(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<Busy>, SourceError> {
        Ok(match calendar {
            "colleague" => Self::colleague(cx, w),
            _ => Self::work(cx, w)
                .iter()
                .filter_map(|e| {
                    Some(Busy {
                        start_ms: e.start.ms?,
                        end_ms: e.end.ms?,
                    })
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(
                DemoSource::at(
                    name.parse().unwrap(),
                    wall.date(),
                    chrono::Timelike::hour(&wall),
                    chrono::Timelike::minute(&wall)
                ),
                ms
            );
        }
    }
}
