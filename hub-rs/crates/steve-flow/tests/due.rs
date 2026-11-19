//! The four real table-backed sources share the runtime contract. Terminal invalid state maps to
//! the domain's existing disabled trigger, failed run, or cancelled alarm, rather than a new queue.
use async_trait::async_trait;
use rusqlite::{Connection, params};
use serde_json::json;
use std::sync::{Arc, Mutex};
use steve_flow::alarms::{AlarmClock, AlarmLine, AlarmSource, Want};
use steve_flow::exec::{RunExecutor, RunStatus};
use steve_flow::scheduler::{BATCH, RunSource};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowCx, FlowResult, FlowSetup};
use steve_modules::{
    ApiError,
    testkit::{ME, TestEnv, WS},
};
use steve_runtime::Clock;
use steve_runtime::due::{
    DueSource,
    contract::{Fixture, check},
    due_loop,
};

const NOW: i64 = 1_794_963_600_000;
const DAY: i64 = 86_400_000;

/// Claiming/dispatch is this source's effect. Executor behavior has its own existing engine tests.
struct Claimed;
#[async_trait]
impl RunExecutor for Claimed {
    async fn drive(&self, _: &FlowCx, _: &str) -> FlowResult<RunStatus> {
        Ok(RunStatus::Done)
    }
}
struct AlarmLines;
impl AlarmSource for AlarmLines {
    fn family(&self) -> &'static str {
        "contract"
    }
    fn plan(&self, _: &Connection, _: &str, id: &str, _: i64) -> FlowResult<Vec<Want>> {
        if id == "source_error" {
            return Err(ApiError::Internal("planner unavailable".into()));
        }
        Ok(vec![])
    }
    fn line(&self, _: &Connection, _: &str, id: &str, _: &str) -> FlowResult<Option<AlarmLine>> {
        if id.starts_with("poison") {
            return Err(ApiError::rule("invalid subject"));
        }
        Ok(Some(AlarmLine {
            subject: format!("contract:{id}"),
            text: id.into(),
            detail: None,
            link: None,
            planned_for: None,
        }))
    }
}
fn environment() -> (TestEnv, Arc<FlowCx>) {
    let core = Arc::new(FakeCore::default());
    let setup = FlowSetup::new(core.clone()).alarm_source(Arc::new(AlarmLines));
    let env = TestEnv::new(|r| setup.register(r));
    env.clock.set(NOW);
    *core.store.lock().unwrap() = Some(env.store().clone());
    let cx = setup.build(env.modules.clone(), Arc::new(Claimed));
    (env, cx)
}

#[derive(Clone, Copy)]
enum Kind {
    Trigger,
    Wait,
    Retry,
}
struct Runs {
    env: TestEnv,
    source: RunSource,
    kind: Kind,
    cx: Arc<FlowCx>,
    rows: Mutex<Vec<(String, bool)>>,
}
impl Runs {
    fn new(kind: Kind) -> Self {
        let (env, cx) = environment();
        let source = match kind {
            Kind::Trigger => RunSource::triggers(cx.clone()),
            Kind::Wait => RunSource::waits(cx.clone()),
            Kind::Retry => RunSource::retries(cx.clone()),
        };
        Self {
            env,
            source,
            kind,
            cx,
            rows: Mutex::new(vec![]),
        }
    }
    fn state(&self, id: &str, done: bool) -> bool {
        self.env
            .store()
            .reader(|c| {
                let sql = match (self.kind, done) {
                    (Kind::Trigger, true) => {
                        "SELECT EXISTS(SELECT 1 FROM flow_runs WHERE flow_id=?1)"
                    }
                    (Kind::Trigger, false) => "SELECT enabled=0 FROM flow_triggers WHERE id=?1",
                    (_, true) => {
                        "SELECT status='running' AND retry_at IS NULL FROM flow_runs WHERE id=?1"
                    }
                    (_, false) => "SELECT status='failed' FROM flow_runs WHERE id=?1",
                };
                Ok::<_, steve_store::StoreError>(c.query_row(sql, [id], |r| r.get(0))?)
            })
            .unwrap()
    }
    fn ids(&self, done: bool) -> Vec<String> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| self.state(id, done))
            .map(|(id, _)| id.clone())
            .collect()
    }
}
impl Fixture for Runs {
    type Source = RunSource;
    fn source(&self) -> &RunSource {
        &self.source
    }
    fn batch(&self) -> usize {
        BATCH
    }
    fn add(&self, at: i64, poison: bool) -> String {
        let mut rows = self.rows.lock().unwrap();
        let id = format!("{}{}", if poison { "poison" } else { "ok" }, rows.len());
        self.env.store().transaction(|tx| -> FlowResult<()> {
            let graph = json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"time","cron":"0 0 * * *","tz":"UTC"}}],"edges":[]});
            tx.execute("INSERT INTO flows(id,workspace_id,name,kind,enabled,version,graph,origin,schedule,created_by,created,updated) VALUES(?1,?2,?1,'schedule',1,1,?3,'me','{\"overlap\":\"run\"}',?4,1,1)", params![id, WS, graph.to_string(), ME])?;
            match self.kind {
                Kind::Trigger => {
                    let cron = if poison { "invalid" } else { "0 0 * * *" };
                    tx.execute("INSERT INTO flow_triggers(id,workspace_id,flow_id,node_id,kind,cron,tz,next_at,missed,enabled) VALUES(?1,?2,?1,'t','time',?3,'UTC',?4,'catch_up_once',1)", params![id, WS, cron, at])?;
                }
                Kind::Wait | Kind::Retry => {
                    tx.execute("INSERT INTO flow_runs(id,workspace_id,flow_id,flow_version,trigger_kind,status,cursor,context,retry_at,attempts,started) VALUES(?1,?2,?1,1,'manual','waiting',?3,'{}',?4,?5,1)", params![id, WS, if poison { "wrong" } else { "w" }, if matches!(self.kind,Kind::Retry) { Some(at) } else { None }, if poison { 2 } else { 1 }])?;
                    if matches!(self.kind,Kind::Wait) {
                        tx.execute("INSERT INTO flow_waits(run_id,node_id,workspace_id,mode,until,created) VALUES(?1,'w',?2,'delay',?3,1)", params![id, WS, at])?;
                    }
                }
            }
            Ok(())
        }).unwrap();
        rows.push((id.clone(), poison));
        id
    }
    fn done(&self) -> Vec<String> {
        self.ids(true)
    }
    fn dead(&self) -> Vec<String> {
        self.ids(false)
    }
    fn remaining_due(&self) -> Option<i64> {
        if !matches!(self.kind, Kind::Trigger) {
            return None;
        }
        // Healthy time triggers recur. A disabled poison trigger must not mask their next run.
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, poison)| !poison)
            .filter_map(|(id, _)| {
                self.env
                    .store()
                    .reader(|c| {
                        Ok::<_, steve_store::StoreError>(c.query_row(
                            "SELECT next_at FROM flow_triggers WHERE id=?",
                            [id],
                            |r| r.get(0),
                        )?)
                    })
                    .unwrap()
            })
            .min()
    }
}
struct Alarms {
    env: TestEnv,
    source: AlarmClock,
    cx: Arc<FlowCx>,
    rows: Mutex<Vec<String>>,
}
impl Alarms {
    fn new() -> Self {
        let (env, cx) = environment();
        Self {
            env,
            source: AlarmClock::new(cx.clone()),
            cx,
            rows: Mutex::new(vec![]),
        }
    }
    fn ids(&self, state: &str) -> Vec<String> {
        self.env
            .store()
            .reader(|c| {
                let mut q = c.prepare("SELECT id FROM time_alarms WHERE state=? ORDER BY id")?;
                Ok::<_, steve_store::StoreError>(
                    q.query_map([state], |r| r.get(0))?
                        .collect::<Result<Vec<_>, _>>()?,
                )
            })
            .unwrap()
    }
}
impl Fixture for Alarms {
    type Source = AlarmClock;
    fn source(&self) -> &AlarmClock {
        &self.source
    }
    fn batch(&self) -> usize {
        steve_flow::alarms::BATCH
    }
    fn add(&self, at: i64, poison: bool) -> String {
        let mut rows = self.rows.lock().unwrap();
        let id = format!("{}{}", if poison { "poison" } else { "ok" }, rows.len());
        self.env.store().transaction(|tx|->FlowResult<()> {
            tx.execute("INSERT INTO time_alarms(id,workspace_id,owner_user,kind,subject,planned_for,fire_at,created,updated) VALUES(?1,?2,?3,'todo',?4,?5,?5,1,1)",params![id,WS,ME,format!("contract:{id}"),at])?;Ok(())
        }).unwrap();
        rows.push(id.clone());
        id
    }
    fn done(&self) -> Vec<String> {
        self.ids("fired")
    }
    fn dead(&self) -> Vec<String> {
        self.ids("cancelled")
    }
}

#[tokio::test]
async fn triggers_meet_the_due_contract_with_disabled_invalid_cron_as_dead_letters() {
    check(|| Runs::new(Kind::Trigger), NOW, 3).await;
}
#[tokio::test]
async fn waits_meet_the_due_contract_with_failed_unresumable_runs_as_dead_letters() {
    check(|| Runs::new(Kind::Wait), NOW, 3).await;
}
#[tokio::test]
async fn retries_meet_the_due_contract_with_exhausted_runs_as_dead_letters() {
    check(|| Runs::new(Kind::Retry), NOW, 3).await;
}
#[tokio::test]
async fn alarms_meet_the_due_contract_with_cancelled_invalid_subjects_as_dead_letters() {
    check(Alarms::new, NOW, 3).await;
}

#[tokio::test]
async fn held_alarms_do_not_take_the_due_batch_limit_and_staleness_still_expires() {
    let f = Alarms::new();
    for _ in 0..steve_flow::alarms::BATCH + 10 {
        let id = f.add(NOW - 1, false);
        f.env
            .store()
            .with_conn(|c| {
                Ok(c.execute(
                    "UPDATE time_alarms SET hold_until=?2 WHERE id=?1",
                    params![id, NOW + DAY],
                )?)
            })
            .unwrap();
    }
    let due = f.add(NOW, false);
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(f.done(), vec![due]);
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + DAY));
    let stale = f.rows.lock().unwrap()[0].clone();
    f.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE time_alarms SET stale_at=?2 WHERE id=?1",
                params![stale, NOW + 1],
            )?)
        })
        .unwrap();
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + 1));
    f.source.run_due(NOW + 1).await.unwrap();
    assert_eq!(f.ids("stale"), vec![stale]);
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("work did not finish without advancing real time");
}
fn backoff() -> steve_runtime::policy::Backoff {
    steve_runtime::policy::Backoff {
        base: std::time::Duration::from_secs(1),
        max: std::time::Duration::from_secs(60),
        factor: 2,
        jitter: steve_runtime::policy::Jitter::None,
        attempts: None,
        reset_after: None,
    }
}
#[tokio::test]
async fn waits_and_retries_wake_at_their_own_deadline_without_a_trigger_or_fifteen_second_tick() {
    for kind in [Kind::Wait, Kind::Retry] {
        let f = Runs::new(kind);
        let id = f.add(NOW + 37, false);
        let source = Arc::new(match kind {
            Kind::Wait => RunSource::waits(f.cx.clone()),
            Kind::Retry => RunSource::retries(f.cx.clone()),
            _ => unreachable!(),
        });
        let clock = f.env.clock.clone();
        let task = steve_runtime::spawn(
            "deadline",
            due_loop(
                source,
                clock.clone(),
                Arc::new(tokio::sync::Notify::new()),
                backoff(),
            ),
        );
        until(|| clock.observed(NOW)).await;
        clock.advance(36);
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(!f.state(&id, true), "no claim before its exact deadline");
        clock.advance(1);
        until(|| f.state(&id, true)).await;
        assert_eq!(
            f.env.clock.now(),
            NOW + 37,
            "the work did not wait for a 15-second tick"
        );
        task.stop();
        task.await;
    }
}

/// A small real-todo source. The production digest source uses the same AlarmSource contract;
/// this fixture keeps the flow tests independent of digest, which depends on flow itself.
struct TodoSource;
impl AlarmSource for TodoSource {
    fn family(&self) -> &'static str {
        "todo"
    }
    fn plan(&self, c: &Connection, ws: &str, id: &str, _: i64) -> FlowResult<Vec<Want>> {
        use rusqlite::OptionalExtension;
        let row:Option<(String,i64)>=c.query_row("SELECT owner,remind_at FROM todos WHERE workspace_id=?1 AND id=?2 AND state='open' AND remind_at IS NOT NULL",params![ws,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        Ok(row
            .into_iter()
            .map(|(owner, at)| Want {
                owner,
                kind: "todo",
                planned_for: at,
                fire_at: at,
                stale_at: None,
            })
            .collect())
    }
    fn line(&self, _: &Connection, _: &str, id: &str, _: &str) -> FlowResult<Option<AlarmLine>> {
        Ok(Some(AlarmLine {
            subject: format!("todo:{id}"),
            text: id.into(),
            detail: None,
            link: None,
            planned_for: None,
        }))
    }
}
async fn committed_changes(
    env: &TestEnv,
    tx: &tokio::sync::mpsc::Sender<steve_types::Notification>,
) {
    for event in env.take_events() {
        tx.send(Arc::new(steve_types::Envelope {
            event,
            rev: steve_types::Rev(1),
        }))
        .await
        .unwrap();
    }
}
fn planned(env: &TestEnv, at: i64) -> bool {
    env.store().reader(|c| Ok::<_,steve_store::StoreError>(c.query_row("SELECT EXISTS(SELECT 1 FROM time_alarms WHERE subject='todo:change' AND state='planned' AND fire_at=?)",[at],|r|r.get(0))?)).unwrap()
}
#[tokio::test]
async fn committed_todo_changes_replan_within_one_second_without_advancing_the_clock() {
    let core = Arc::new(FakeCore::default());
    let setup = FlowSetup::new(core).alarm_source(Arc::new(TodoSource));
    let env = TestEnv::new(|r| setup.register(r));
    env.clock.set(NOW);
    let cx = setup.build(env.modules.clone(), Arc::new(Claimed));
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(cx).with_changes(rx).run());
    until(|| env.clock.observed(NOW)).await;
    for at in [NOW + DAY, NOW + DAY + 37] {
        env.store().transaction(|t|->FlowResult<()> {
            t.execute("INSERT INTO todos(id,workspace_id,owner,text,state,source,writer,created,updated,remind_at) VALUES('change',?1,?2,'remind','open','you',?2,?3,?3,?4) ON CONFLICT(id) DO UPDATE SET remind_at=?4,updated=?3",params![WS,ME,NOW,at])?;
            t.publish(steve_types::DomainEvent::Channels(steve_types::ChannelsEvent::Digest{id:"change".into(),ws:WS.into()}));
            Ok(())
        }).unwrap();
        committed_changes(&env, &tx).await;
        until(|| planned(&env, at)).await;
        assert!(
            env.clock.now() - NOW < 1_000,
            "a committed change must not wait for the fallback timer"
        );
    }
    assert!(
        !planned(&env, NOW + DAY),
        "the previous reminder was cancelled"
    );
    task.stop();
    task.await;
}

#[tokio::test]
async fn quiet_policy_wake_rechecks_an_existing_hold_immediately() {
    let f = Alarms::new();
    let id = f.add(NOW, false);
    f.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE time_alarms SET hold_until=? WHERE id=?",
                params![NOW + DAY, id],
            )?)
        })
        .unwrap();
    // The quiet-hours writer outside flow calls this interface after committing its policy.
    let cx = f.cx.clone();
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(cx.clone()).run());
    until(|| f.env.clock.observed(NOW)).await;
    assert!(f.done().is_empty());
    cx.wake_alarms();
    until(|| f.done().contains(&id)).await;
    assert_eq!(f.env.clock.now(), NOW);
    task.stop();
    task.await;
}

#[tokio::test]
async fn a_missed_policy_wake_is_found_by_the_sixty_second_fallback() {
    let f = Alarms::new();
    let id = f.add(NOW, false);
    f.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE time_alarms SET hold_until=? WHERE id=?",
                params![NOW + DAY, id],
            )?)
        })
        .unwrap();
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(f.cx.clone()).run());
    until(|| f.env.clock.observed(NOW)).await;
    // A writer committed a new policy but omitted its notify. The old hold is a day away.
    f.env.store().with_conn(|c|Ok(c.execute(
        "INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES(?1,?2,0,'now','UTC',1,0,?3)",params![WS,ME,NOW+1])?)).unwrap();
    f.env
        .clock
        .advance(steve_runtime::due::FALLBACK.as_millis() as i64);
    until(|| f.done().contains(&id)).await;
    assert_eq!(f.env.clock.now(), NOW + 60_000);
    task.stop();
    task.await;
}

#[tokio::test]
async fn one_planning_source_error_does_not_rollback_healthy_fired_alarms() {
    let f = Alarms::new();
    let id = f.add(NOW, false);
    f.env.store().with_conn(|c|Ok(c.execute(
        "INSERT INTO time_alarm_dirty(workspace_id,subject) VALUES(?,'contract:source_error')",[WS])?)).unwrap();
    assert!(
        f.source.run_due(NOW).await.is_err(),
        "the source error reaches due_loop for backoff"
    );
    assert!(
        f.done().contains(&id),
        "the healthy event and fired state committed"
    );
    f.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "DELETE FROM time_alarm_dirty WHERE subject='contract:source_error'",
                [],
            )?)
        })
        .unwrap();
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(
        f.source.next_due().await.unwrap(),
        None,
        "a repaired source recovers without replaying the fired alarm"
    );
}
