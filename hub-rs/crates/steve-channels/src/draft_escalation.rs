//! Working-hour escalation for unhandled @-mention and DM drafts (#213).
//!
//! Ordinary channel reply drafts stay quietly inside their conversation (`DecisionTier::Draft`).
//! When an unhandled draft replying to a DM or @-mention sits untouched for at least
//! [`DRAFT_ESCALATE_WORK_HOURS`] working hours, it is promoted to `DecisionTier::Decide` so it
//! surfaces in Inbox (`待我决定`) for owner attention. If the owner replies or reacts meanwhile,
//! decision views automatically fold the card as `FoldReason::Replied`.

use std::collections::HashMap;

use chrono::{Datelike, Duration, NaiveTime, TimeZone, Weekday};
use chrono_tz::Tz;
use serde_json::{Value, json};
use steve_modules::zone::Zone;
use steve_store::channel_message_repo::outbound as message_repo;
use steve_store::views::Views;
use steve_store::{DecisionRows, Store};
use steve_types::{DecisionStatus, DecisionTier, DomainEvent, WorkEvent};

use crate::{ChannelResult, Channels};

/// Number of working hours an unhandled @-mention or DM draft waits before escalating to Inbox.
pub const DRAFT_ESCALATE_WORK_HOURS: u64 = 4;
/// Start of a standard working day in local workspace time (inclusive, 09:00).
pub const WORK_DAY_START_HOUR: u32 = 9;
/// End of a standard working day in local workspace time (exclusive, 18:00).
pub const WORK_DAY_END_HOUR: u32 = 18;
/// Standard working days of the week (Monday through Friday).
pub const WORK_DAYS: [Weekday; 5] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
];
/// Milliseconds in one working hour.
pub const WORK_HOUR_MS: i64 = 60 * 60 * 1000;
/// Working-hour escalation threshold in milliseconds.
pub const DRAFT_ESCALATE_WORK_MS: i64 = DRAFT_ESCALATE_WORK_HOURS as i64 * WORK_HOUR_MS;
/// Machine-readable reason code written to `decisions.result.escalated_reason`.
pub const ESCALATED_REASON_WORK_HOURS: &str = "unhandled_4_work_hours";
/// Safety cap on local calendar days scanned per draft when computing working duration
/// (14 calendar days always contain >= 10 workdays = 90 working hours >> 4 hours).
const MAX_WORK_SCAN_DAYS: i64 = 14;

/// Whether `day` is a working day under [`WORK_DAYS`].
pub fn is_workday(day: Weekday) -> bool {
    WORK_DAYS.contains(&day)
}

/// Deterministic elapsed working milliseconds in `[start_ms, end_ms]` on `tz`'s local clock,
/// counting only [`WORK_DAYS`] within `[WORK_DAY_START_HOUR:00, WORK_DAY_END_HOUR:00)`.
///
/// Caps accumulation once [`DRAFT_ESCALATE_WORK_MS`] is exceeded or [`MAX_WORK_SCAN_DAYS`] local
/// days have been examined so old drafts remain O(1) to evaluate.
pub fn working_duration_ms(start_ms: i64, end_ms: i64, tz: Tz) -> i64 {
    if end_ms <= start_ms {
        return 0;
    }
    let Some(start_dt) = tz.timestamp_millis_opt(start_ms).single() else {
        return 0;
    };
    let Some(end_dt) = tz.timestamp_millis_opt(end_ms).single() else {
        return 0;
    };
    let zone = Zone::new(tz);
    let Some(work_start_time) = NaiveTime::from_hms_opt(WORK_DAY_START_HOUR, 0, 0) else {
        return 0;
    };
    let Some(work_end_time) = NaiveTime::from_hms_opt(WORK_DAY_END_HOUR, 0, 0) else {
        return 0;
    };
    let mut day = start_dt.date_naive();
    let last_day = end_dt.date_naive();
    let mut total = 0_i64;
    let mut scanned = 0_i64;
    while day <= last_day && scanned < MAX_WORK_SCAN_DAYS {
        if is_workday(day.weekday())
            && let (Some(day_start), Some(day_end)) = (
                zone.instant(day.and_time(work_start_time)),
                zone.instant(day.and_time(work_end_time)),
            )
        {
            let lo = start_ms.max(day_start);
            let hi = end_ms.min(day_end);
            if hi > lo {
                total = total.saturating_add(hi - lo);
                if total >= DRAFT_ESCALATE_WORK_MS {
                    return total;
                }
            }
        }
        day += Duration::days(1);
        scanned += 1;
    }
    total
}

/// Whether a draft created at `created_ms` has crossed [`DRAFT_ESCALATE_WORK_HOURS`] working hours
/// by `now_ms` in `tz`.
pub fn is_escalation_due(created_ms: i64, now_ms: i64, tz: Tz) -> bool {
    working_duration_ms(created_ms, now_ms, tz) >= DRAFT_ESCALATE_WORK_MS
}

fn already_escalated(result_json: Option<&str>) -> bool {
    result_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.get("escalated").and_then(Value::as_bool))
        == Some(true)
}

/// Scans open @-mention and DM drafts using the module's injected clock and each workspace
/// owner's configured timezone ([`Zone::for_owner`]), escalating any draft that has remained
/// unhandled for at least [`DRAFT_ESCALATE_WORK_HOURS`] working hours. Returns how many drafts
/// were escalated.
pub fn escalate_due(ch: &Channels) -> ChannelResult<usize> {
    escalate_due_in(ch.store(), ch.now())
}

/// Store-level escalation pass at injected timestamp `now` (used by [`escalate_due`] and
/// deterministic unit tests). Working hours are evaluated in each workspace owner's timezone via
/// [`Zone::for_owner`].
pub fn escalate_due_in(store: &Store, now: i64) -> ChannelResult<usize> {
    let candidates = store.read(|r| -> ChannelResult<_> {
        let rows = message_repo::open_addressed_drafts(r)?;
        let mut zones: HashMap<String, Tz> = HashMap::new();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tz = match zones.get(&row.ws) {
                Some(&tz) => tz,
                None => {
                    let tz = Zone::for_owner(r, &row.ws)?.tz;
                    zones.insert(row.ws.clone(), tz);
                    tz
                }
            };
            out.push((row, tz));
        }
        Ok(out)
    })?;
    let due: Vec<_> = candidates
        .into_iter()
        .filter(|(c, _)| !already_escalated(c.result_json.as_deref()))
        .filter(|(c, tz)| is_escalation_due(c.created, now, *tz))
        .map(|(c, _)| c)
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    let mut escalated = 0;
    for c in due {
        let kind = if c.is_dm { "dm" } else { "mention" };
        let did_escalate = store.write(|w| -> ChannelResult<bool> {
            let view = w
                .decision_views()
                .of(&c.ws, &[c.decision_id.clone()])?
                .pop();
            let Some(view) = view else {
                return Ok(false);
            };
            if view.status != DecisionStatus::Open || view.tier != DecisionTier::Draft {
                return Ok(false);
            }
            let Some(d) = DecisionRows(w).get(&c.decision_id)? else {
                return Ok(false);
            };
            if d.status != DecisionStatus::Open || d.result["escalated"] == true {
                return Ok(false);
            }
            let mut metadata = match d.result {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            metadata.insert("escalated".into(), Value::Bool(true));
            metadata.insert("escalated_at".into(), json!(now));
            metadata.insert(
                "escalated_reason".into(),
                json!(ESCALATED_REASON_WORK_HOURS),
            );
            DecisionRows(w).set_result(&d.id, &Value::Object(metadata))?;
            steve_store::audit_repo::record(
                w,
                &c.ws,
                "steve",
                "chan_message.escalated",
                &c.message_id,
                &json!({
                    "decision": d.id,
                    "kind": kind,
                    "reason": ESCALATED_REASON_WORK_HOURS,
                }),
            )?;
            w.emit(DomainEvent::from(WorkEvent::Decision {
                ws: c.ws.clone().into(),
                id: d.id.into(),
                item_id: Some(d.item_id.map(steve_types::ItemId::from)),
            }));
            Ok(true)
        })?;
        if did_escalate {
            metrics::counter!("steve_channel_draft_escalated_total", "kind" => kind).increment(1);
            escalated += 1;
        }
    }
    Ok(escalated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use steve_modules::testkit::{ME, TestEnv, WS, caller};
    use steve_store::base::Viewer;
    use steve_store::views::Views;
    use steve_types::FoldReason;

    use crate::channel::{InboundMessage, Mention, SpaceKind};
    use crate::fake::channel::FakeChannel;
    use crate::outbound::{OutLink, Outbox};
    use crate::respond::SUBJECT_MESSAGE;
    use crate::{Channels, Replies, spaces};
    use std::sync::Arc;
    use steve_flow::testkit::FakeCore;
    use steve_flow::{FlowSetup, LinearExecutor};
    use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
    use steve_modules::vault::Vault;
    use steve_modules::vault::keystore::FileKeyStore;

    fn local_ms(tz: Tz, y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        let dt = NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap();
        Zone::new(tz).instant(dt).unwrap()
    }

    #[test]
    fn working_hours_respects_workday_bounds_overnight_and_weekends() {
        let tz = chrono_tz::Asia::Shanghai;
        // Thursday 2026-11-19 10:00 -> 13:59 is 3h59m (not due); 14:00 is 4h (due).
        let thu_10 = local_ms(tz, 2026, 11, 19, 10, 0);
        assert!(!is_escalation_due(
            thu_10,
            local_ms(tz, 2026, 11, 19, 13, 59),
            tz
        ));
        assert!(is_escalation_due(
            thu_10,
            local_ms(tz, 2026, 11, 19, 14, 0),
            tz
        ));

        // Overnight: Thu 16:00 -> Fri 10:59 = 2h + 1h59m = 3h59m; Fri 11:00 = 4h.
        let thu_16 = local_ms(tz, 2026, 11, 19, 16, 0);
        assert!(!is_escalation_due(
            thu_16,
            local_ms(tz, 2026, 11, 20, 10, 59),
            tz
        ));
        assert!(is_escalation_due(
            thu_16,
            local_ms(tz, 2026, 11, 20, 11, 0),
            tz
        ));

        // Weekend skipped: Fri 17:00 -> Sat/Sun = 1h; Mon 11:59 = 3h59m; Mon 12:00 = 4h.
        let fri_17 = local_ms(tz, 2026, 11, 20, 17, 0);
        assert!(!is_escalation_due(
            fri_17,
            local_ms(tz, 2026, 11, 22, 23, 0),
            tz
        ));
        assert!(!is_escalation_due(
            fri_17,
            local_ms(tz, 2026, 11, 23, 11, 59),
            tz
        ));
        assert!(is_escalation_due(
            fri_17,
            local_ms(tz, 2026, 11, 23, 12, 0),
            tz
        ));

        // Created on Sunday -> clock starts Monday 09:00, due Monday 13:00.
        let sun_14 = local_ms(tz, 2026, 11, 22, 14, 0);
        assert!(!is_escalation_due(
            sun_14,
            local_ms(tz, 2026, 11, 23, 12, 59),
            tz
        ));
        assert!(is_escalation_due(
            sun_14,
            local_ms(tz, 2026, 11, 23, 13, 0),
            tz
        ));
    }

    struct EscalationHarness {
        env: TestEnv,
        _keys: tempfile::TempDir,
        ch: Channels,
        me: String,
    }

    impl EscalationHarness {
        fn new(start: i64, ws_tz: &str, owner_tz: Option<&str>) -> Self {
            let core = Arc::new(FakeCore::default());
            let flow_setup = FlowSetup::new(core);
            let link = OutLink::new();
            let reg_link = link.clone();
            let env = TestEnv::new(|r| {
                flow_setup.register(r);
                connections::register(r);
                r.decision(SUBJECT_MESSAGE, reg_link.handler());
            });
            env.clock.set(start);
            link.bind_clock(env.clock.clone());
            env.store()
                .settings()
                .set(WS, json!({"tz": ws_tz}).as_object().unwrap(), ME)
                .unwrap();
            if let Some(tz) = owner_tz {
                steve_store::testkit::modules::notification_timezone(env.store(), WS, ME, tz)
                    .unwrap();
            }
            let keys = tempfile::tempdir().unwrap();
            let vault = Vault::new(
                env.store().clone(),
                Arc::new(FileKeyStore::open(keys.path()).unwrap()),
            );
            let fake = Arc::new(FakeChannel::new());
            let conns = Connections::builder(
                env.modules.clone(),
                vault,
                ConnectionsConfig::new(Vec::new(), true, |_| None),
            )
            .provider(fake.clone())
            .build();
            let outbox = Arc::new(Outbox::new(link));
            let replies = Arc::new(Replies::new().outbound(outbox));
            let flow = flow_setup
                .connections(conns.clone())
                .build(env.modules.clone(), Arc::new(LinearExecutor));
            let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
                .adapter(fake.clone())
                .responder(replies)
                .flow(flow)
                .build();
            let (me, _) =
                FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).unwrap();
            fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
            fake.space(
                "G_DEV",
                "开发群",
                SpaceKind::Group,
                false,
                &[("ulilei", "李雷")],
            );
            fake.space(
                "G_DIRECT",
                "公告群",
                SpaceKind::Group,
                false,
                &[("ulilei", "李雷")],
            );
            env.store()
                .write(|w| {
                    spaces::set_account_rules(
                        w,
                        &Viewer::bare(WS, ME),
                        &me,
                        &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
                        start,
                    )
                })
                .unwrap();
            env.gateway.script(
                crate::compose::H_SAFETY,
                r#"{"text": "收到，稍后看。", "needs_approval": false, "why": "回复"}"#,
            );
            Self {
                env,
                _keys: keys,
                ch,
                me,
            }
        }

        fn tier_of(&self, id: &str) -> (DecisionTier, Option<FoldReason>) {
            let v = self
                .env
                .store()
                .read(|r| r.decision_views().of(WS, &[id.to_owned()]))
                .unwrap()
                .pop()
                .unwrap();
            (v.tier, v.folded)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unhandled_dm_and_mention_drafts_escalate_after_four_working_hours_only() {
        let tz = chrono_tz::Asia::Shanghai;
        // Fixed Thursday timestamp independent of wall clock or host timezone.
        let start = local_ms(tz, 2026, 1, 15, 16, 0); // Thu 16:00 Asia/Shanghai
        let h = EscalationHarness::new(start, "Asia/Shanghai", None);

        let mut dm = FakeChannel::message("D_LI", "m_dm", "ulilei", "在吗？");
        dm.created = start;
        let dm_res = h.ch.on_message(WS, &h.me, dm).await.unwrap();
        let dm_dec = dm_res.response.unwrap().decision_id.unwrap();

        let mut mention: InboundMessage =
            FakeChannel::message("G_DEV", "m_at", "ulilei", "@你 看一下发布单");
        mention.created = start;
        mention.mentions.push(Mention {
            handle: None,
            name: "你".into(),
            is_connection: true,
        });
        let at_res = h.ch.on_message(WS, &h.me, mention).await.unwrap();
        let at_dec = at_res.response.unwrap().decision_id.unwrap();

        // A direct outbound draft in a separate group without an @-mention source must NOT escalate.
        let mut plain_in = FakeChannel::message("G_DIRECT", "m_plain", "ulilei", "同步进度");
        plain_in.created = start;
        let plain_res = h.ch.on_message(WS, &h.me, plain_in).await.unwrap();
        let non_mention_dec = h
            .env
            .store()
            .write(|w| {
                let target = crate::deliver::OutTarget {
                    ws: WS.into(),
                    connection_id: h.me.clone(),
                    mode: spaces::Mode::Own,
                    space_id: plain_res.space_id.clone(),
                    space_name: "公告群".into(),
                    persona_id: None,
                    reply_to: None,
                    thread_key: None,
                    source_message_id: None,
                    burst_id: None,
                };
                crate::respond::open_draft_to(w, &target, "主动同步一下", start)
            })
            .unwrap()
            .1;

        assert_eq!(h.tier_of(&dm_dec), (DecisionTier::Draft, None));
        assert_eq!(h.tier_of(&at_dec), (DecisionTier::Draft, None));
        assert_eq!(h.tier_of(&non_mention_dec), (DecisionTier::Draft, None));

        // Overnight Fri 10:59 (3h59m working): still quiet Draft, nothing escalated.
        h.env.clock.advance_to(local_ms(tz, 2026, 1, 16, 10, 59));
        assert_eq!(escalate_due(&h.ch).unwrap(), 0);
        assert_eq!(h.tier_of(&dm_dec), (DecisionTier::Draft, None));
        assert_eq!(h.tier_of(&at_dec), (DecisionTier::Draft, None));

        // Fri 11:00 (4h00m working): both unhandled DM and @-mention drafts escalate to Decide,
        // while the non-addressed group draft stays quiet.
        h.env.clock.advance_to(local_ms(tz, 2026, 1, 16, 11, 0));
        assert_eq!(escalate_due(&h.ch).unwrap(), 2);
        assert_eq!(h.tier_of(&dm_dec), (DecisionTier::Decide, None));
        assert_eq!(h.tier_of(&at_dec), (DecisionTier::Decide, None));
        assert_eq!(h.tier_of(&non_mention_dec), (DecisionTier::Draft, None));
        // Second sweep is idempotent.
        assert_eq!(escalate_due(&h.ch).unwrap(), 0);

        // Once the owner replies in the DM, even an escalated draft folds as Replied.
        let mut own_reply = FakeChannel::message("D_LI", "m_owner", "upx", "我直接回复了");
        own_reply.mine = true;
        own_reply.created = local_ms(tz, 2026, 1, 16, 11, 5);
        h.ch.on_message(WS, &h.me, own_reply).await.unwrap();
        let (tier, folded) = h.tier_of(&dm_dec);
        assert_eq!(tier, DecisionTier::Folded);
        assert!(matches!(
            folded,
            Some(FoldReason::Replied | FoldReason::Expired)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn escalation_uses_owner_timezone_over_workspace_and_host_clock() {
        let shanghai = chrono_tz::Asia::Shanghai;
        let new_york = chrono_tz::America::New_York;
        // Thu 2026-01-15 11:00 Asia/Shanghai == Wed 2026-01-14 22:00 America/New_York (03:00 UTC).
        let start = local_ms(shanghai, 2026, 1, 15, 11, 0);
        // Workspace tz is Asia/Shanghai, while owner's personal notification tz is America/New_York.
        let h = EscalationHarness::new(start, "Asia/Shanghai", Some("America/New_York"));

        let mut dm = FakeChannel::message("D_LI", "m_tz", "ulilei", "在吗？");
        dm.created = start;
        let dm_dec =
            h.ch.on_message(WS, &h.me, dm)
                .await
                .unwrap()
                .response
                .unwrap()
                .decision_id
                .unwrap();

        // Thu 15:00 Asia/Shanghai (4h working in Shanghai) == Thu 02:00 America/New_York (0h working).
        h.env
            .clock
            .advance_to(local_ms(shanghai, 2026, 1, 15, 15, 0));
        assert_eq!(escalate_due(&h.ch).unwrap(), 0);

        // Thu 12:59 America/New_York (3h59m working in NY): still quiet.
        h.env
            .clock
            .advance_to(local_ms(new_york, 2026, 1, 15, 12, 59));
        assert_eq!(escalate_due(&h.ch).unwrap(), 0);

        // Thu 13:00 America/New_York (4h00m working in NY): escalates to Decide.
        h.env
            .clock
            .advance_to(local_ms(new_york, 2026, 1, 15, 13, 0));
        assert_eq!(escalate_due(&h.ch).unwrap(), 1);
        assert_eq!(h.tier_of(&dm_dec), (DecisionTier::Decide, None));
    }
}
