//! The four real table-backed sources share the runtime contract. Terminal invalid state maps to
//! the domain's existing disabled trigger, failed run, or cancelled alarm, rather than a new queue.
use async_trait::async_trait;
use rusqlite::{Connection, params};
use serde_json::json;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use steve_flow::alarms::{AlarmClock, AlarmLine, AlarmSource, Want};
use steve_flow::exec::{RunExecutor, RunStatus};
use steve_flow::scheduler::{BATCH, Retries, Triggers, Waits};
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
    environment_for(Arc::new(Claimed))
}
fn environment_for(exec: Arc<dyn RunExecutor>) -> (TestEnv, Arc<FlowCx>) {
    let core = Arc::new(FakeCore::default());
    let fault_store = Arc::new(std::sync::OnceLock::new());
    let setup = FlowSetup::new(core.clone())
        .alarm_source(Arc::new(AlarmLines))
        .alarm_source(Arc::new(Faults {
            store: fault_store.clone(),
        }));
    let env = TestEnv::new(|r| setup.register(r));
    env.clock.set(NOW);
    *core.store.lock().unwrap() = Some(env.store().clone());
    env.store().with_conn(|c| {c.execute_batch("CREATE TABLE claim_faults(kind TEXT PRIMARY KEY); INSERT INTO claim_faults VALUES('plan'),('line');")?;Ok(())}).unwrap();
    fault_store.set(env.store().clone()).ok().unwrap();
    let cx = setup.build(env.modules.clone(), exec);
    (env, cx)
}

#[derive(Clone, Copy)]
enum Kind {
    Trigger,
    Wait,
    Retry,
}
enum Inner {
    Trigger(Triggers),
    Wait(Waits),
    Retry(Retries),
}
struct Source {
    inner: Inner,
    last: AtomicI64,
}
impl Source {
    fn new(kind: Kind, cx: Arc<FlowCx>) -> Self {
        let inner = match kind {
            Kind::Trigger => Inner::Trigger(Triggers::new(cx)),
            Kind::Wait => Inner::Wait(Waits::new(cx)),
            Kind::Retry => Inner::Retry(Retries::new(cx)),
        };
        Self {
            inner,
            last: AtomicI64::new(NOW),
        }
    }
}
impl DueSource for Source {
    fn name(&self) -> &'static str {
        match &self.inner {
            Inner::Trigger(s) => s.name(),
            Inner::Wait(s) => s.name(),
            Inner::Retry(s) => s.name(),
        }
    }
    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        match &self.inner {
            Inner::Trigger(s) => s.next_due().await,
            Inner::Wait(s) => s.next_due().await,
            Inner::Retry(s) => s.next_due().await,
        }
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        self.last.store(now, Ordering::SeqCst);
        match &self.inner {
            Inner::Trigger(s) => s.run_due(now).await,
            Inner::Wait(s) => s.run_due(now).await,
            Inner::Retry(s) => s.run_due(now).await,
        }
    }
}
struct Runs {
    env: TestEnv,
    source: Source,
    kind: Kind,
    cx: Arc<FlowCx>,
    rows: Mutex<Vec<(String, bool)>>,
}
impl Runs {
    fn new(kind: Kind) -> Self {
        Self::with_executor(kind, Arc::new(Claimed))
    }
    fn with_executor(kind: Kind, exec: Arc<dyn RunExecutor>) -> Self {
        let (env, cx) = environment_for(exec);
        let source = Source::new(kind, cx.clone());
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
    type Source = Source;
    fn source(&self) -> &Source {
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
        // Independent oracle: this fixture uses a daily UTC schedule. Do not read next_at
        // back from the source's own table, which could hide an incorrect calculation.
        let at = self.source.last.load(Ordering::SeqCst);
        Some((at.div_euclid(DAY) + 1) * DAY)
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
        let source = Arc::new(Source::new(kind, f.cx.clone()));
        assert_eq!(source.next_due().await.unwrap(), Some(NOW + 37));
        source.run_due(NOW + 36).await.unwrap();
        assert!(
            !f.state(&id, true),
            "completed claim pass before the deadline must do nothing"
        );
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
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(
        f.source.next_due().await.unwrap(),
        None,
        "failed requests are terminal, not source backoff"
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

struct Faults {
    store: Arc<std::sync::OnceLock<steve_store::Store>>,
}
impl Faults {
    fn fail(&self, c: &Connection, kind: &str) -> FlowResult<()> {
        c.execute("INSERT INTO claim_leaks(kind) VALUES(?)", [kind])?;
        self.store
            .get()
            .unwrap()
            .publish(steve_types::DomainEvent::from(
                steve_types::AutomationEvent::Signal {
                    id: "row-leak".into(),
                    ws: WS.into(),
                },
            ));
        Err(ApiError::Internal(format!("persistent {kind} failure")))
    }
}
impl AlarmSource for Faults {
    fn family(&self) -> &'static str {
        "todo"
    }
    fn plan(&self, c: &Connection, ws: &str, id: &str, now: i64) -> FlowResult<Vec<Want>> {
        if id == "plan" {
            if c.query_row(
                "SELECT EXISTS(SELECT 1 FROM claim_faults WHERE kind='plan')",
                [],
                |r| r.get::<_, bool>(0),
            )? {
                self.fail(c, "plan")?;
            }
            return TodoSource.plan(c, ws, id, now);
        }
        Ok(vec![])
    }
    fn line(&self, c: &Connection, _: &str, id: &str, _: &str) -> FlowResult<Option<AlarmLine>> {
        if id == "line"
            && c.query_row(
                "SELECT EXISTS(SELECT 1 FROM claim_faults WHERE kind='line')",
                [],
                |r| r.get::<_, bool>(0),
            )?
        {
            self.fail(c, "line")?;
        }
        Ok(Some(AlarmLine {
            subject: format!("todo:{id}"),
            text: id.into(),
            detail: None,
            link: None,
            planned_for: None,
        }))
    }
}
fn no_leaks(env: &TestEnv) {
    assert_eq!(
        env.store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT count(*) FROM claim_leaks",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
    assert!(!env.take_events().iter().any(|e|matches!(e.as_domain(),Some(steve_types::DomainEvent::Automation(steve_types::AutomationEvent::Signal{id,..})) if id.to_string()=="row-leak")),"rolled-back row events must not escape the Store savepoint");
}
#[tokio::test]
async fn internal_claim_failures_end_only_the_bad_row_without_delaying_other_deadlines() {
    for kind in [Kind::Trigger, Kind::Wait, Kind::Retry] {
        let f = Runs::new(kind);
        let bad = f.add(NOW, false);
        let good = f.add(NOW + 37, false);
        let trigger = match kind {
            Kind::Trigger => format!(
                "CREATE TRIGGER fail_claim BEFORE UPDATE OF next_at ON flow_triggers WHEN OLD.id='{bad}' BEGIN SELECT RAISE(ABORT,'persistent claim failure'); END;"
            ),
            _ => format!(
                "CREATE TRIGGER fail_claim BEFORE UPDATE OF status ON flow_runs WHEN OLD.id='{bad}' AND NEW.status='running' BEGIN SELECT RAISE(ABORT,'persistent claim failure'); END;"
            ),
        };
        f.env
            .store()
            .with_conn(|c| {
                c.execute_batch(&trigger)?;
                c.execute("UPDATE flow_triggers SET missed='skip'", [])?;
                Ok(())
            })
            .unwrap();
        f.source.run_due(NOW).await.unwrap();
        assert!(f.state(&bad, false));
        assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + 37));
        f.source.run_due(NOW + 36).await.unwrap();
        assert!(!f.state(&good, true));
        f.env.clock.set(NOW + 37);
        f.source.run_due(NOW + 37).await.unwrap();
        assert!(
            f.state(&good, true),
            "a failed peer cannot turn a healthy deadline into a missed run"
        );
        if matches!(kind, Kind::Trigger) {
            assert_eq!(
                f.env
                    .store()
                    .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                        "SELECT started FROM flow_runs WHERE flow_id=?",
                        [&good],
                        |r| r.get::<_, i64>(0)
                    )?))
                    .unwrap(),
                NOW + 37
            );
        }
        assert_eq!(
            f.env
                .store()
                .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT count(*) FROM audit WHERE action='flow.claim_failed' AND target=?",
                    [&bad],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            1,
            "terminal failure is durable and not attempted again"
        );
        let expected = if matches!(kind, Kind::Trigger) {
            Some((NOW.div_euclid(DAY) + 1) * DAY)
        } else {
            None
        };
        assert_eq!(f.source.next_due().await.unwrap(), expected);
    }
}

#[tokio::test]
async fn a_poison_alarm_line_rolls_back_only_its_row_and_not_the_ready_batch() {
    let f = Alarms::new();
    let bad = f.add(NOW - 1, false);
    f.env
        .store()
        .with_conn(|c| {
            c.execute_batch("CREATE TABLE claim_leaks(kind TEXT)")?;
            c.execute(
                "UPDATE time_alarms SET subject='todo:line' WHERE id=?",
                [&bad],
            )?;
            Ok(())
        })
        .unwrap();
    for _ in 0..BATCH {
        f.add(NOW, false);
    }
    f.env.take_events();
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(
        f.done().len(),
        BATCH - 1,
        "a failed row must not undo the healthy rows of its batch"
    );
    no_leaks(&f.env);
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(
        f.done().len(),
        BATCH,
        "the retrying first row must be filtered before LIMIT"
    );
    no_leaks(&f.env);
    assert!(f.dead().contains(&bad));
    assert_eq!(f.done().len(), BATCH);
    assert_eq!(f.source.next_due().await.unwrap(), None);
}

#[tokio::test]
async fn broken_dirty_planning_does_not_mask_alarm_deadlines_and_a_subject_edit_can_retry() {
    let f = Alarms::new();
    let good = f.add(NOW + 37, false);
    f.env
        .store()
        .with_conn(|c| {
            c.execute_batch("CREATE TABLE claim_leaks(kind TEXT)")?;
            c.execute("INSERT INTO todos(id,workspace_id,owner,text,state,source,writer,created,updated,remind_at) VALUES('plan',?1,?2,'retry planning','open','you',?2,?3,?3,?3)",params![WS,ME,NOW])?;
            Ok(())
        })
        .unwrap();
    f.env.take_events();
    f.source.run_due(NOW).await.unwrap();
    no_leaks(&f.env);
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + 37));
    f.env.clock.set(NOW + 37);
    f.source.run_due(NOW + 37).await.unwrap();
    assert!(f.done().contains(&good));
    no_leaks(&f.env);
    assert_eq!(f.source.next_due().await.unwrap(), None);
    assert_eq!(f.env.store().reader(|c|Ok::<_,steve_store::StoreError>(c.query_row("SELECT count(*) FROM audit WHERE action='flow.claim_failed' AND target='todo:plan'",[],|r|r.get::<_,i64>(0))?)).unwrap(),1);
    // The actual migrated subject trigger requests planning again after the fault is repaired.
    f.env
        .store()
        .with_conn(|c| {
            c.execute("DELETE FROM claim_faults WHERE kind='plan'", [])?;
            c.execute("UPDATE todos SET remind_at=? WHERE id='plan'", [NOW + DAY])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.source.next_due().await.unwrap(), Some(f.env.clock.now()));
    f.source.run_due(f.env.clock.now()).await.unwrap();
    assert_eq!(
        f.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT state FROM time_alarms WHERE subject='todo:plan'",
                [],
                |r| r.get::<_, String>(0)
            )?))
            .unwrap(),
        "planned"
    );
    f.env.clock.set(NOW + DAY);
    f.source.run_due(NOW + DAY).await.unwrap();
    assert_eq!(
        f.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT state FROM time_alarms WHERE subject='todo:plan'",
                [],
                |r| r.get::<_, String>(0)
            )?))
            .unwrap(),
        "fired"
    );
}

struct Finished(Arc<std::sync::atomic::AtomicBool>);
#[async_trait]
impl RunExecutor for Finished {
    async fn drive(&self, cx: &FlowCx, id: &str) -> FlowResult<RunStatus> {
        let result = steve_flow::GraphEngine.drive(cx, id).await;
        self.0.store(true, Ordering::SeqCst);
        result
    }
}

#[tokio::test]
async fn failed_wait_completion_settles_its_signal_once_through_the_engine() {
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = Runs::with_executor(Kind::Wait, Arc::new(Finished(finished.clone())));
    let bad = f.add(NOW, true);
    f.env.store().with_conn(|c|{
        c.execute("INSERT INTO ext_events(id,workspace_id,source,type,dedupe_key,occurred,received,summary) VALUES('settle-event',?,'manual','manual','settle-event',?2,?2,'test')",params![WS,NOW])?;
        c.execute("UPDATE flow_runs SET event_id='settle-event' WHERE id=?",[&bad])?;
        c.execute_batch("CREATE TABLE settle_counts(n INTEGER); CREATE TRIGGER count_settle AFTER UPDATE ON ext_events WHEN NEW.id='settle-event' BEGIN INSERT INTO settle_counts(n) VALUES(1); END;")?;Ok(())
    }).unwrap();
    f.source.run_due(NOW).await.unwrap();
    until(|| finished.load(Ordering::SeqCst)).await;
    assert!(
        f.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT outcome='failed' FROM ext_events WHERE id='settle-event'",
                [],
                |r| r.get::<_, bool>(0)
            )?))
            .unwrap()
    );
    assert_eq!(
        f.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT count(*) FROM settle_counts",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn source_jobs_are_spawnable_and_stop_after_the_recovery_gate() {
    let (_env, cx) = environment();
    let jobs = [
        steve_runtime::spawn("triggers", Triggers::new(cx.clone()).run()),
        steve_runtime::spawn("waits", Waits::new(cx.clone()).run()),
        steve_runtime::spawn("retries", Retries::new(cx.clone()).run()),
        steve_runtime::spawn("followups", cx.clone().followups_task()),
    ];
    cx.recovered().await;
    for job in jobs {
        job.stop();
        job.await;
    }
}
