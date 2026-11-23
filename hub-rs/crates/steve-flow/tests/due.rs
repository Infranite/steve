//! The four real table-backed sources share the runtime contract. Terminal invalid state maps to
//! the domain's existing disabled trigger, failed run, or cancelled alarm, rather than a new queue.
use async_trait::async_trait;
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
use steve_store::testkit::flow::{due as fixtures, signal_settlement};
use steve_store::{Alarms as AlarmRows, FlowRows, RunRows};

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
    fn plan(&self, _: &mut steve_store::W<'_>, _: &str, id: &str, _: i64) -> FlowResult<Vec<Want>> {
        if id == "source_error" {
            return Err(ApiError::Internal("planner unavailable".into()));
        }
        Ok(vec![])
    }
    fn line(
        &self,
        _: &mut steve_store::W<'_>,
        _: &str,
        id: &str,
        _: &str,
    ) -> FlowResult<Option<AlarmLine>> {
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
    let setup = FlowSetup::new(core.clone())
        .alarm_source(Arc::new(AlarmLines))
        .alarm_source(Arc::new(Faults));
    let env = TestEnv::new(|r| setup.register(r));
    env.clock.set(NOW);
    *core.store.lock().unwrap() = Some(env.store().clone());
    fixtures::seed_claim_faults(env.store()).unwrap();
    let cx = setup.build(env.modules.clone(), exec);
    (env, cx)
}

use fixtures::DueKind as Kind;
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
        fixtures::due_row_state(self.env.store(), self.kind, id, done).unwrap()
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
        fixtures::seed_due_row(self.env.store(), self.kind, WS, ME, &id, at, poison).unwrap();
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
        fixtures::alarm_ids(self.env.store(), state).unwrap()
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
        fixtures::seed_due_alarm(self.env.store(), WS, ME, &id, at).unwrap();
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
        fixtures::hold_due_alarm(f.env.store(), &id, NOW + DAY).unwrap();
    }
    let due = f.add(NOW, false);
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(f.done(), vec![due]);
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + DAY));
    let stale = f.rows.lock().unwrap()[0].clone();
    fixtures::expire_due_alarm(f.env.store(), &stale, NOW + 1).unwrap();
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
    fn plan(
        &self,
        c: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
        _: i64,
    ) -> FlowResult<Vec<Want>> {
        let row = fixtures::todo_reminder(c, ws, id)?;
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
    fn line(
        &self,
        _: &mut steve_store::W<'_>,
        _: &str,
        id: &str,
        _: &str,
    ) -> FlowResult<Option<AlarmLine>> {
        Ok(Some(AlarmLine {
            subject: format!("todo:{id}"),
            text: id.into(),
            detail: None,
            link: None,
            planned_for: None,
        }))
    }
}
fn planned(env: &TestEnv, at: i64) -> bool {
    env.store()
        .read(|r| AlarmRows(r).of_subject(WS, "todo:change"))
        .unwrap()
        .into_iter()
        .any(|a| a.fire_at == at && a.state == "planned")
}
#[tokio::test]
async fn committed_todo_changes_replan_within_one_second_without_advancing_the_clock() {
    let core = Arc::new(FakeCore::default());
    let setup = FlowSetup::new(core).alarm_source(Arc::new(TodoSource));
    let env = TestEnv::new(|r| setup.register(r));
    env.clock.set(NOW);
    let cx = setup.build(env.modules.clone(), Arc::new(Claimed));
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(cx).run());
    until(|| env.clock.observed(NOW)).await;
    for at in [NOW + DAY, NOW + DAY + 37] {
        fixtures::set_todo_reminder(env.store(), WS, ME, NOW, at).unwrap();
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
async fn committed_policy_or_timezone_changes_recheck_existing_holds_immediately() {
    for table in ["notification_policies", "settings"] {
        let f = Alarms::new();
        let id = f.add(NOW, false);
        fixtures::hold_due_alarm_with_timestamp(f.env.store(), &id, NOW + DAY, NOW).unwrap();
        let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(f.cx.clone()).run());
        until(|| f.env.clock.observed(NOW)).await;
        assert!(f.done().is_empty());
        // No DomainEvent and no manual Notify; equal updated timestamps deliberately
        // cannot satisfy CHANGED_POLICY's greater-than fallback predicate.
        fixtures::change_alarm_policy_or_timezone(
            f.env.store(),
            WS,
            ME,
            NOW,
            table == "notification_policies",
        )
        .unwrap();
        until(|| f.done().contains(&id)).await;
        assert_eq!(
            f.env.clock.now(),
            NOW,
            "{table} commit woke before fallback"
        );
        task.stop();
        task.await;
    }
}

#[tokio::test]
async fn a_missed_policy_wake_is_found_by_the_sixty_second_fallback() {
    let f = Alarms::new();
    let id = f.add(NOW, false);
    fixtures::hold_due_alarm(f.env.store(), &id, NOW + DAY).unwrap();
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(f.cx.clone()).run());
    until(|| f.env.clock.observed(NOW)).await;
    // An independent Store connection has no in-process subscription. The old hold is a day away.
    fixtures::disable_notifications_outside_store(f.env.store(), WS, ME, NOW + 1).unwrap();
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
    f.env
        .store()
        .write(|w| AlarmRows(w).mark(WS, "contract:source_error"))
        .unwrap();
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
    fixtures::clear_failed_alarm_request(f.env.store()).unwrap();
    f.source.run_due(NOW).await.unwrap();
    assert_eq!(
        f.source.next_due().await.unwrap(),
        None,
        "a repaired source recovers without replaying the fired alarm"
    );
}

struct Faults;

impl Faults {
    fn fail(&self, w: &mut steve_store::W<'_>, kind: &str) -> FlowResult<()> {
        fixtures::record_claim_leak(w, kind)?;
        w.emit(steve_types::DomainEvent::from(
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
    fn plan(
        &self,
        c: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
        now: i64,
    ) -> FlowResult<Vec<Want>> {
        if id == "plan" {
            if fixtures::claim_fault(c, "plan")? {
                self.fail(c, "plan")?;
            }
            return TodoSource.plan(c, ws, id, now);
        }
        Ok(vec![])
    }
    fn line(
        &self,
        c: &mut steve_store::W<'_>,
        _: &str,
        id: &str,
        _: &str,
    ) -> FlowResult<Option<AlarmLine>> {
        if id == "line" && fixtures::claim_fault(c, "line")? {
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
    assert_eq!(fixtures::claim_leaks(env.store()).unwrap(), 0);
    assert!(!env.take_events().iter().any(|e|matches!(e.as_domain(),Some(steve_types::DomainEvent::Automation(steve_types::AutomationEvent::Signal{id,..})) if id.to_string()=="row-leak")),"rolled-back row events must not escape the Store savepoint");
}
#[tokio::test]
async fn internal_claim_failures_end_only_the_bad_row_without_delaying_other_deadlines() {
    for kind in [Kind::Trigger, Kind::Wait, Kind::Retry] {
        let f = Runs::new(kind);
        let bad = f.add(NOW, false);
        let good = f.add(NOW + 37, false);
        fixtures::reject_due_claim(f.env.store(), kind, &bad).unwrap();
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
                    .read(|r| RunRows(r).latest_start(WS, &good))
                    .unwrap()
                    .unwrap(),
                NOW + 37
            );
        }
        assert_eq!(
            fixtures::claim_failure_count(f.env.store(), &bad).unwrap(),
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
    fixtures::seed_alarm_line_failure(f.env.store(), &bad).unwrap();
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
    fixtures::seed_alarm_plan_failure(f.env.store(), WS, ME, NOW).unwrap();
    f.env.take_events();
    f.source.run_due(NOW).await.unwrap();
    no_leaks(&f.env);
    assert_eq!(f.source.next_due().await.unwrap(), Some(NOW + 37));
    f.env.clock.set(NOW + 37);
    f.source.run_due(NOW + 37).await.unwrap();
    assert!(f.done().contains(&good));
    no_leaks(&f.env);
    assert_eq!(f.source.next_due().await.unwrap(), None);
    assert_eq!(
        fixtures::claim_failure_count(f.env.store(), "todo:plan").unwrap(),
        1
    );
    // The actual migrated subject trigger requests planning again after the fault is repaired.
    fixtures::repair_alarm_plan(f.env.store(), NOW + DAY).unwrap();
    assert_eq!(f.source.next_due().await.unwrap(), Some(f.env.clock.now()));
    f.source.run_due(f.env.clock.now()).await.unwrap();
    assert_eq!(
        f.env
            .store()
            .read(|r| AlarmRows(r).of_subject(WS, "todo:plan"))
            .unwrap()
            .remove(0)
            .state,
        "planned"
    );
    f.env.clock.set(NOW + DAY);
    f.source.run_due(NOW + DAY).await.unwrap();
    assert_eq!(
        f.env
            .store()
            .read(|r| AlarmRows(r).of_subject(WS, "todo:plan"))
            .unwrap()
            .remove(0)
            .state,
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
    fixtures::track_signal_settlement(f.env.store(), WS, &bad, NOW).unwrap();
    f.source.run_due(NOW).await.unwrap();
    until(|| finished.load(Ordering::SeqCst)).await;
    assert!(signal_settlement(f.env.store(), "settle-event").unwrap().0 == "failed");
    assert_eq!(fixtures::settle_count(f.env.store()).unwrap(), 1);
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

#[tokio::test]
async fn committed_alarm_deadline_changes_wake_without_advancing_the_clock() {
    let f = Alarms::new();
    let id = f.add(NOW + DAY, false);
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(f.cx.clone()).run());
    until(|| f.env.clock.observed(NOW)).await;
    fixtures::reschedule_due_alarm(f.env.store(), &id, NOW).unwrap();
    until(|| f.done().contains(&id)).await;
    assert_eq!(
        f.env.clock.now(),
        NOW,
        "a committed direct alarm edit must not wait for fallback"
    );
    task.stop();
    task.await;
}

/// Every due query and round reads this clock; its count is an upper bound on rounds.
struct CountedAlarmClock {
    clock: Arc<steve_runtime::FakeClock>,
    reads: std::sync::atomic::AtomicUsize,
}
impl Clock for CountedAlarmClock {
    fn now(&self) -> i64 {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.clock.now()
    }
    fn sleep(
        &self,
        duration: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.clock.sleep(duration)
    }
}

#[tokio::test]
async fn alarm_hold_commits_do_not_self_wake_into_repeated_due_rounds() {
    let setup = FlowSetup::new(Arc::new(FakeCore::default())).alarm_source(Arc::new(AlarmLines));
    let env = TestEnv::new(|_| {});
    env.clock.set(NOW);
    let clock = Arc::new(CountedAlarmClock {
        clock: env.clock.clone(),
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut registry = steve_modules::Registry::new();
    setup.register(&mut registry);
    let modules = steve_modules::Modules::with_clock(
        env.store().clone(),
        env.gateway.clone(),
        registry,
        clock.clone(),
    );
    let cx = setup.build(modules, Arc::new(Claimed));
    fixtures::seed_quiet_alarm(env.store(), WS, ME, NOW).unwrap();
    let state = || fixtures::alarm_state(env.store(), "held").unwrap();
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(cx).run());
    until(|| state().1.is_some()).await;
    until(|| env.clock.observed(NOW)).await;
    let held_until = state().1.unwrap();
    for at in [NOW + 1_000, NOW + 5_000, NOW + 10_000, NOW + 20_000] {
        env.clock.set(at);
        // Handshake with the actual injected wait after each step, not advance followed
        // immediately by an absence assertion. Own hints may re-query but may not clear holds.
        until(|| env.clock.observed(at)).await;
        assert_eq!(state(), ("planned".into(), Some(held_until)));
        assert!(
            clock.reads.load(Ordering::SeqCst) <= 64,
            "alarm source keeps running on its own commits: {} clock reads",
            clock.reads.load(Ordering::SeqCst)
        );
    }
    assert!(held_until > env.clock.now());
    task.stop();
    task.await;
}

/// Observe the loop returning to a stored deadline after each real planning pass.
struct ScheduleAlarmClock {
    clock: Arc<steve_runtime::FakeClock>,
    waits: std::sync::atomic::AtomicUsize,
}
impl Clock for ScheduleAlarmClock {
    fn now(&self) -> i64 {
        self.clock.now()
    }
    fn sleep(
        &self,
        duration: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        self.clock.sleep(duration)
    }
}
struct SchedulePlans {
    clock: Arc<ScheduleAlarmClock>,
    observed: Mutex<Vec<(bool, bool, usize)>>,
}
impl AlarmSource for SchedulePlans {
    fn family(&self) -> &'static str {
        "brief"
    }
    fn plan(
        &self,
        c: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
        _: i64,
    ) -> FlowResult<Vec<Want>> {
        let row = FlowRows(c)
            .get(id)?
            .filter(|row| row.workspace_id == ws)
            .unwrap();
        let (enabled, deleted) = (row.enabled, row.deleted);
        self.observed.lock().unwrap().push((
            enabled,
            deleted,
            self.clock.waits.load(Ordering::SeqCst),
        ));
        Ok(vec![])
    }
    fn line(
        &self,
        _: &mut steve_store::W<'_>,
        _: &str,
        _: &str,
        _: &str,
    ) -> FlowResult<Option<AlarmLine>> {
        Ok(None)
    }
}

#[tokio::test]
async fn committed_schedule_patch_and_delete_wake_planning_without_clearing_unrelated_holds() {
    use steve_flow::schedules::{Fields, Mode, Origin, Spec};
    use steve_modules::testkit::{call, caller};
    let env = TestEnv::new(|_| {});
    env.clock.set(NOW);
    let clock = Arc::new(ScheduleAlarmClock {
        clock: env.clock.clone(),
        waits: std::sync::atomic::AtomicUsize::new(0),
    });
    let plans = Arc::new(SchedulePlans {
        clock: clock.clone(),
        observed: Mutex::new(vec![]),
    });
    let setup = FlowSetup::new(Arc::new(FakeCore::default()))
        .alarm_source(Arc::new(AlarmLines))
        .alarm_source(plans.clone());
    let mut registry = steve_modules::Registry::new();
    setup.register(&mut registry);
    let modules = steve_modules::Modules::with_clock(
        env.store().clone(),
        env.gateway.clone(),
        registry,
        clock.clone(),
    );
    let cx = setup.build(modules.clone(), Arc::new(Claimed));
    let spec = Spec {
        requester: Some(ME.into()),
        when: "每天 08:00".into(),
        cron: "0 8 * * *".into(),
        tz: "UTC".into(),
        what: String::new(),
        bot: String::new(),
        mode: Mode::Brief,
        project_id: None,
        tags: vec![],
        missed: Default::default(),
        overlap: Default::default(),
        budget_usd: 0.0,
    };
    let id = env
        .store()
        .write(|w| -> FlowResult<_> {
            let id = steve_flow::schedules::insert(
                w,
                WS,
                ME,
                &Fields {
                    name: "brief",
                    spec: &spec,
                    enabled: true,
                },
                &Origin::default(),
                NOW,
            )?;
            fixtures::seed_schedule_hold(w, WS, ME, NOW, NOW + DAY)?;
            Ok(id)
        })
        .unwrap();
    let task = steve_runtime::spawn("flow.alarms", AlarmClock::new(cx.clone()).run());
    until(|| !plans.observed.lock().unwrap().is_empty()).await;
    until(|| clock.waits.load(Ordering::SeqCst) > 0).await;
    plans.observed.lock().unwrap().clear();
    let router = steve_flow::routes(cx).with_state(modules);
    for (method, body, expected) in [
        ("PATCH", Some(json!({"enabled":false})), (false, false)),
        ("DELETE", None, (false, true)),
    ] {
        let (status, response) = call(
            &router,
            Some(&caller(WS, ME)),
            method,
            &format!("/schedules/{id}"),
            body.as_ref(),
        )
        .await;
        assert_eq!(status, 200, "{method}: {response}");
        until(|| {
            plans
                .observed
                .lock()
                .unwrap()
                .iter()
                .any(|&(enabled, deleted, _)| (enabled, deleted) == expected)
        })
        .await;
        let at_plan = plans
            .observed
            .lock()
            .unwrap()
            .iter()
            .find(|&&(enabled, deleted, _)| (enabled, deleted) == expected)
            .unwrap()
            .2;
        // A later sleep is prepared only after this actual replan/claim/replan round
        // finishes. This avoids asserting while a manual recheck could still fire the hold.
        until(|| clock.waits.load(Ordering::SeqCst) > at_plan).await;
        let held: (String, Option<i64>) =
            fixtures::alarm_state(env.store(), "schedule-hold").unwrap();
        assert_eq!(
            held,
            ("planned".into(), Some(NOW + DAY)),
            "{method} must not invalidate a different subject's hold"
        );
        assert_eq!(
            env.clock.now(),
            NOW,
            "{method} committed trigger woke without fallback"
        );
    }
    task.stop();
    task.await;
}
