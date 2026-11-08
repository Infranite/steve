//! Bounded diagnostic metadata, independent of business state and transcript contents.
//! Producers only enqueue; the worker resolves ownership and writes after business locks are free.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Inner, Result, Store, StoreError, now};

pub const RETENTION_DAYS: i64 = 7;
pub const MAX_ITEM_BYTES: i64 = 10 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: i64 = 100 * 1024 * 1024;
pub const DEFAULT_CAPTURE_MINUTES: u32 = 30;
pub const MAX_CAPTURE_MINUTES: u32 = 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub after: Option<i64>,
    /// Stable upper cursor, returned as `snapshot_cursor` on the first page.
    pub through: Option<i64>,
    pub limit: u32,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub event_id: Option<String>,
    pub session_id: Option<String>,
    pub attempt_id: Option<String>,
    pub kind: Option<String>,
    pub kinds: Vec<String>,
    pub reason_code: Option<String>,
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Info,
    Warning,
    Error,
}

/// Whitelisted scalars only. No prompt, tool arguments/results, environment, or free text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Details {
    /// Captured from the business object by the producer; never inferred during persistence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_cycle: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_decisions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// Remote self-reported occurrence time; the event's `at` remains Hub recording time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_epoch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_errors: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain_timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reader_failed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_ordinal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub workspace_id: String,
    /// May be resolved from `session_id` by the worker, never by the calling thread.
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub attempt_id: Option<String>,
    pub operation_id: Option<String>,
    pub decision_id: Option<String>,
    pub queued_id: Option<String>,
    pub cause_id: Option<String>,
    pub kind: String,
    pub reason_code: String,
    pub severity: Severity,
    /// Detailed evaluations require an active capture. Low-frequency facts remain available.
    pub detail: bool,
    pub details: Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    /// Accepted into memory only, not a durability receipt.
    Queued,
    DroppedFull,
    Unavailable,
    Invalid,
    /// Detailed observations are disabled; no queue or database work was performed.
    Skipped,
}

pub(crate) fn decision_event(d: &crate::Decision, operation: &str, from: Option<&str>) -> Record {
    Record {
        workspace_id: d.workspace_id.clone(),
        item_id: d.item_id.clone(),
        session_id: d.session_id.clone(),
        decision_id: Some(d.id.clone()),
        kind: format!("decision.{operation}"),
        reason_code: format!("decision_{operation}"),
        details: Details {
            from_state: from.map(str::to_owned),
            to_state: Some(d.status.clone()),
            ..Default::default()
        },
        ..Default::default()
    }
}

const QUEUE_CAPACITY: usize = 512;
const MAX_EVENT_BYTES: usize = 8192;
const KEEP_MS: i64 = RETENTION_DAYS * 86_400_000;

struct Pending {
    at: i64,
    record: Record,
}

/// Started when Store opens, so producers never create threads or acquire the writer lock.
pub(crate) struct Recorder {
    sender: mpsc::SyncSender<Pending>,
    lost: Arc<AtomicU64>,
    pending: Arc<AtomicU64>,
    /// Odd during a worker transaction and its transfer of in-memory loss counts.
    loss_epoch: Arc<AtomicU64>,
    active_until: AtomicI64,
}

impl Recorder {
    pub(crate) fn start(inner: Weak<Inner>, initial_until: i64) -> Option<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Pending>(QUEUE_CAPACITY);
        let lost = Arc::new(AtomicU64::new(0));
        let losses = lost.clone();
        let pending = Arc::new(AtomicU64::new(0));
        let queued = pending.clone();
        let loss_epoch = Arc::new(AtomicU64::new(0));
        let epoch = loss_epoch.clone();
        std::thread::Builder::new()
            .name("steve-diagnostics".into())
            .spawn(move || {
                loop {
                    let first = match receiver.recv_timeout(Duration::from_secs(30)) {
                        Ok(p) => Some(p),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let Some(inner) = inner.upgrade() else { break };
                    let store = Store { inner };
                    let mut batch: Vec<_> = first.into_iter().collect();
                    batch.extend(receiver.try_iter().take(31));
                    let count = batch.len() as u64;
                    epoch.fetch_add(1, Ordering::AcqRel);
                    let result: Result<u64> = store.write(|w| {
                        let tx = w.tx()?;
                        prune_expired(&tx, now())?;
                        for event in &batch {
                            persist(&tx, event)?;
                        }
                        let pending = losses.load(Ordering::Relaxed);
                        tx.execute(
                            "UPDATE diagnostic_stats SET dropped=dropped+? WHERE id=1",
                            [pending.min(i64::MAX as u64) as i64],
                        )?;
                        let until = active_until(&tx)?;
                        tx.commit()?;
                        // Keep the cache ordered with concurrent capture writes.
                        if let Some(recorder) = store.inner.diagnostics.get() {
                            recorder.active_until.store(until, Ordering::Relaxed);
                        }
                        Ok(pending)
                    });
                    // Do not log through the instrumented service, or turn a diagnostic failure
                    // into a business retry. Failed writes remain visible as a coverage loss.
                    match result {
                        Err(_) => {
                            losses.fetch_add(count, Ordering::Relaxed);
                        }
                        Ok(persisted) => {
                            losses.fetch_sub(persisted, Ordering::Relaxed);
                        }
                    }
                    epoch.fetch_add(1, Ordering::Release);
                    // A zero observed with Acquire precedes a new SQLite snapshot,
                    // so it cannot acknowledge a commit absent from that snapshot.
                    queued.fetch_sub(count, Ordering::Release);
                }
            })
            .ok()?;
        Some(Self {
            sender,
            lost,
            pending,
            loss_epoch,
            active_until: AtomicI64::new(initial_until),
        })
    }
}

pub struct Diagnostics<'a>(&'a Store);

#[derive(Clone, Copy, Default)]
struct Snapshot {
    queued: u64,
    loss_epoch: u64,
}

impl Store {
    pub fn diagnostics(&self) -> Diagnostics<'_> {
        Diagnostics(self)
    }
}

fn token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-/".contains(&b))
}

fn valid(r: &Record) -> bool {
    (r.workspace_id.is_empty() || token(&r.workspace_id, 160))
        && token(&r.kind, 80)
        && token(&r.reason_code, 80)
        && (r.item_id.is_some() || r.session_id.is_some())
        && [
            &r.item_id,
            &r.session_id,
            &r.attempt_id,
            &r.operation_id,
            &r.decision_id,
            &r.queued_id,
            &r.cause_id,
        ]
        .into_iter()
        .all(|s| s.as_ref().is_none_or(|s| token(s, 160)))
        && [
            &r.details.from_state,
            &r.details.to_state,
            &r.details.rpc_method,
            &r.details.code,
        ]
        .into_iter()
        .all(|s| s.as_ref().is_none_or(|s| token(s, 80)))
        && [
            &r.details.rpc_id,
            &r.details.source_node,
            &r.details.source_epoch,
        ]
        .into_iter()
        .all(|s| s.as_ref().is_none_or(|s| token(s, 160)))
}

fn own(c: &Connection, ws: &str, item: &str) -> Result<()> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE id=? AND workspace_id=?)",
        params![item, ws],
        |r| r.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound("item not found"))
    }
}

impl Diagnostics<'_> {
    /// Called under the writer lock, after the transaction that created the capture committed.
    /// The cache only avoids pointless enqueueing; persistence still checks each item's window.
    pub(crate) fn activate_until(&self, until: i64) {
        if let Some(recorder) = self.0.inner.diagnostics.get() {
            recorder.active_until.fetch_max(until, Ordering::Relaxed);
        }
    }

    fn snapshot<T>(&self, read: impl FnOnce(&Connection, Snapshot) -> Result<T>) -> Result<T> {
        self.0.reader(|c| {
            // Sample before the first SELECT fixes the SQLite snapshot. Sampling
            // at the end can pair old rows with a worker's newly drained queue.
            let counters = self
                .0
                .inner
                .diagnostics
                .get()
                .map_or_else(Snapshot::default, |r| Snapshot {
                    queued: r.pending.load(Ordering::Acquire),
                    loss_epoch: r.loss_epoch.load(Ordering::Acquire),
                });
            let tx = c.unchecked_transaction()?;
            read(&tx, counters)
        })
    }

    /// Only whitelisted identifiers, codes and scalars can be queued. Never reads the Store.
    pub fn record(&self, record: Record) -> RecordStatus {
        let Some(recorder) = self.0.inner.diagnostics.get() else {
            return RecordStatus::Unavailable;
        };
        let at = now();
        if record.detail && at >= recorder.active_until.load(Ordering::Relaxed) {
            return RecordStatus::Skipped;
        }
        if !valid(&record) {
            recorder.lost.fetch_add(1, Ordering::Relaxed);
            return RecordStatus::Invalid;
        }
        recorder.pending.fetch_add(1, Ordering::Relaxed);
        match recorder.sender.try_send(Pending { at, record }) {
            Ok(()) => RecordStatus::Queued,
            Err(mpsc::TrySendError::Full(_)) => {
                recorder.pending.fetch_sub(1, Ordering::Relaxed);
                recorder.lost.fetch_add(1, Ordering::Relaxed);
                RecordStatus::DroppedFull
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                recorder.pending.fetch_sub(1, Ordering::Relaxed);
                recorder.lost.fetch_add(1, Ordering::Relaxed);
                RecordStatus::Unavailable
            }
        }
    }

    /// `None`: 30 minutes. `Some(0)`: stop without deleting history. Maximum: 60 minutes.
    pub fn capture(
        &self,
        ws: &str,
        item: &str,
        actor: &str,
        minutes: Option<u32>,
    ) -> Result<Value> {
        let minutes = minutes.unwrap_or(DEFAULT_CAPTURE_MINUTES);
        if minutes > MAX_CAPTURE_MINUTES || actor.is_empty() || actor.len() > 160 {
            return Err(StoreError::Conflict(
                "invalid diagnostic capture window or actor".into(),
            ));
        }
        if minutes > 0 && self.0.inner.diagnostics.get().is_none() {
            return Err(StoreError::Conflict(
                "diagnostic recorder unavailable".into(),
            ));
        }
        self.0.write(|w| {
            let tx = w.tx()?;
            own(&tx, ws, item)?;
            let at = now();
            tx.execute("INSERT OR IGNORE INTO diagnostic_item_stats(item_id) VALUES(?)", [item])?;
            if minutes == 0 {
                tx.execute("UPDATE diagnostic_captures SET stop_reason='manual',stopped_at=? WHERE item_id=? AND workspace_id=? AND stop_reason IS NULL", params![at,item, ws])?;
            } else {
                tx.execute("INSERT INTO diagnostic_captures(item_id,workspace_id,actor,started_at,expires_at,bytes,dropped_at_start)
                    VALUES(?,?,?,?,?,0,(SELECT dropped FROM diagnostic_item_stats WHERE item_id=?))
                    ON CONFLICT(item_id) DO UPDATE SET actor=excluded.actor,started_at=excluded.started_at,
                    expires_at=excluded.expires_at,stop_reason=NULL,stopped_at=NULL,bytes=0,dropped_at_start=excluded.dropped_at_start",
                    params![item,ws,actor,at,at+i64::from(minutes)*60_000,item])?;
            }
            let result=json!({"observed_at":at,"capture":capture_view(&tx,item,at)?});
            let until=active_until(&tx)?;
            tx.commit()?;
            if let Some(recorder)=self.0.inner.diagnostics.get(){recorder.active_until.store(until,Ordering::Relaxed);}
            Ok(result)
        })
    }

    pub fn explain(&self, ws: &str, item: &str) -> Result<Value> {
        self.snapshot(|tx, counters| {
            own(tx, ws, item)?;
            let at = now();
            let item_view = tx.query_row("SELECT id,status,phase,cycle,revision FROM items WHERE id=?",[item],|r| {
                Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"phase":r.get::<_,String>(2)?,"cycle":r.get::<_,i64>(3)?,"revision":r.get::<_,i64>(4)?}))
            })?;
            let mut st = tx.prepare("SELECT id,status,harness,created,updated FROM sessions WHERE item_id=? AND workspace_id=? ORDER BY no DESC,created DESC LIMIT 100")?;
            let sessions = st.query_map(params![item,ws],|r| Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"harness":r.get::<_,String>(2)?,"created":r.get::<_,i64>(3)?,"updated":r.get::<_,i64>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(json!({"observed_at":at,"item":item_view,"capture":capture_view(tx,item,at)?,"sessions":sessions,"coverage":self.coverage(tx,item,at,counters)?}))
        })
    }

    pub fn events(&self, ws: &str, item: &str, filter: Filter) -> Result<Value> {
        if filter.after.is_some_and(|x| x < 0)
            || filter.through.is_some_and(|x| x < 0)
            || filter.kinds.len() > 32
            || filter.since.zip(filter.until).is_some_and(|(a, b)| a > b)
            || filter
                .severity
                .as_deref()
                .is_some_and(|s| !matches!(s, "info" | "warning" | "error"))
            || [
                &filter.session_id,
                &filter.attempt_id,
                &filter.kind,
                &filter.reason_code,
            ]
            .into_iter()
            .any(|v| v.as_ref().is_some_and(|s| !token(s, 160)))
            || filter.kinds.iter().any(|s| !token(s, 80))
        {
            return Err(StoreError::Conflict("invalid diagnostic filter".into()));
        }
        self.snapshot(|tx, counters| {
            own(tx,ws,item)?;
            let at=now();
            let head: i64=tx.query_row("SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='diagnostic_events'),0)",[],|r|r.get(0))?;
            let through=filter.through.unwrap_or(head).min(head);
            let after=filter.after.unwrap_or(0);
            let limit=if filter.limit==0 {100} else {filter.limit.min(200)} as usize;
            let mut sql=String::from("SELECT seq,at,kind,reason_code,severity,data FROM diagnostic_events WHERE workspace_id=? AND item_id=? AND seq>? AND seq<=? AND at>=?");
            let mut args:Vec<rusqlite::types::Value>=vec![ws.to_owned().into(),item.to_owned().into(),after.into(),through.into(),(at-KEEP_MS).into()];
            for (column,value) in [("session_id",filter.session_id.as_ref()),("attempt_id",filter.attempt_id.as_ref()),("kind",filter.kind.as_ref()),("reason_code",filter.reason_code.as_ref()),("severity",filter.severity.as_ref())] {
                if let Some(v)=value {sql.push_str(&format!(" AND {column}=?"));args.push(v.clone().into());}
            }
            if let Some(since)=filter.since {sql.push_str(" AND at>=?");args.push(since.into());}
            if let Some(until)=filter.until {sql.push_str(" AND at<=?");args.push(until.into());}
            if let Some(id)=&filter.event_id {
                let seq=id.strip_prefix("diag_").and_then(|x|x.parse::<i64>().ok()).ok_or_else(||StoreError::Conflict("invalid diagnostic event id".into()))?;
                sql.push_str(" AND seq=?");args.push(seq.into());
            }
            if !filter.kinds.is_empty() {
                sql.push_str(" AND kind IN (");
                sql.push_str(&vec!["?";filter.kinds.len()].join(","));sql.push(')');
                args.extend(filter.kinds.iter().cloned().map(Into::into));
            }
            sql.push_str(" ORDER BY seq LIMIT ?");args.push(((limit+1)as i64).into());
            let mut st=tx.prepare(&sql)?;
            let mut events=st.query_map(params_from_iter(args),|r|{
                let seq:i64=r.get(0)?;
                let data:String=r.get(5)?;
                let mut v:Value=serde_json::from_str(&data).unwrap_or_else(|_|json!({}));
                v["event_id"]=json!(format!("diag_{seq}"));v["cursor"]=json!(seq);v["at"]=json!(r.get::<_,i64>(1)?);
                v["kind"]=json!(r.get::<_,String>(2)?);v["reason_code"]=json!(r.get::<_,String>(3)?);v["severity"]=json!(r.get::<_,String>(4)?);
                Ok(v)
            })?.collect::<std::result::Result<Vec<_>,_>>()?;
            let has_more=events.len()>limit;events.truncate(limit);
            let next=events.last().and_then(|e|e["cursor"].as_i64());
            let coverage=self.coverage(tx,item,at,counters)?;
            let removed=coverage["removed_through"].as_i64().unwrap_or(0);
            let mut gaps=vec![];
            if removed.min(through)>after {gaps.push(json!({"reason":"retention_or_capacity","after":after,"through":removed.min(through)}));}
            if coverage["instance_dropped"].as_u64().unwrap_or(0)>0 {gaps.push(json!({"reason":"diagnostic_queue_or_write_loss","scope":"instance","count":coverage["instance_dropped"]}));}
            if coverage["item_dropped"].as_u64().unwrap_or(0)>0 {gaps.push(json!({"reason":"diagnostic_item_loss","scope":"item","count":coverage["item_dropped"]}));}
            Ok(json!({"events":events,"next_cursor":next,"snapshot_cursor":through,"has_more":has_more,"coverage":coverage,"truncated":!gaps.is_empty(),"gaps":gaps}))
        })
    }

    fn coverage(&self, c: &Connection, item: &str, at: i64, counters: Snapshot) -> Result<Value> {
        let (first, last): (Option<i64>, Option<i64>) = c.query_row(
            "SELECT min(seq),max(seq) FROM diagnostic_events WHERE item_id=? AND at>=?",
            params![item, at - KEEP_MS],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let removed:i64=c.query_row("SELECT max(COALESCE((SELECT removed_through FROM diagnostic_item_stats WHERE item_id=?1),0),COALESCE((SELECT max(seq) FROM diagnostic_events WHERE item_id=?1 AND at<?2),0))",params![item,at-KEEP_MS],|r|r.get(0))?;
        let persisted: i64 =
            c.query_row("SELECT dropped FROM diagnostic_stats WHERE id=1", [], |r| {
                r.get(0)
            })?;
        let pending = self
            .0
            .inner
            .diagnostics
            .get()
            .map_or(0, |r| r.lost.load(Ordering::Relaxed));
        let item_dropped: i64 = c.query_row(
            "SELECT COALESCE((SELECT dropped FROM diagnostic_item_stats WHERE item_id=?),0)",
            [item],
            |r| r.get(0),
        )?;
        // The DB count and in-memory remainder are exact only when no worker
        // transfer crossed the snapshot. Do not hide a transient zero or double
        // count behind a false claim of stable coverage; callers should refresh.
        std::sync::atomic::fence(Ordering::Acquire);
        let ending_epoch = self
            .0
            .inner
            .diagnostics
            .get()
            .map_or(0, |r| r.loss_epoch.load(Ordering::Relaxed));
        let loss_count_stable =
            counters.loss_epoch.is_multiple_of(2) && counters.loss_epoch == ending_epoch;
        let queued = counters.queued;
        // A process crash can discard queued observations or interrupt remote-event
        // projection before the recorder sees them. Zero known losses or pending
        // observations therefore does not prove complete diagnostic coverage.
        Ok(
            json!({"earliest_cursor":first,"latest_cursor":last,"removed_through":removed,"retention_days":RETENTION_DAYS,"max_item_bytes":MAX_ITEM_BYTES,"max_total_bytes":MAX_TOTAL_BYTES,"instance_dropped":(persisted.max(0)as u64).saturating_add(pending),"loss_count_stable":loss_count_stable,"refresh_required":!loss_count_stable,"item_dropped":item_dropped,"instance_pending":queued,"pending_count":queued,"pending_scope":"instance","pending_basis":"before_snapshot","recorder_available":self.0.inner.diagnostics.get().is_some(),"async":true,"delivery":"best_effort","unobserved_gaps":"unknown"}),
        )
    }
}

/// A new item gets one bounded window, anchored to creation rather than its first read or run.
/// Never replace an existing row: a stopped or expired capture is an owner's explicit choice
/// until they start another window. Diagnostic failure must not fail the business creation.
pub(crate) fn capture_created(c: &Connection, ws: &str, item: &str, created: i64) -> Option<i64> {
    let until = created.saturating_add(i64::from(DEFAULT_CAPTURE_MINUTES) * 60_000);
    match c.execute(
        "INSERT INTO diagnostic_captures(item_id,workspace_id,actor,started_at,expires_at)
         VALUES(?,?,'system:item-created',?,?) ON CONFLICT(item_id) DO NOTHING",
        params![item, ws, created, until],
    ) {
        Ok(1) => Some(until),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(item, %error, "could not start the new item's diagnostic capture");
            None
        }
    }
}

fn capture_view(c: &Connection, item: &str, at: i64) -> Result<Value> {
    let row=c.query_row("SELECT started_at,expires_at,stop_reason,bytes,max(0,COALESCE((SELECT dropped FROM diagnostic_item_stats WHERE item_id=?1),0)-dropped_at_start) FROM diagnostic_captures WHERE item_id=?1",[item],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?))).optional()?;
    Ok(match row {
        Some((start, end, reason, bytes, dropped)) => {
            json!({"active":reason.is_none()&&end>at,"started_at":start,"expires_at":end,"stop_reason":reason.or_else(||(end<=at).then(||"expired".to_owned())),"bytes":bytes,"dropped":dropped})
        }
        None => {
            json!({"active":false,"started_at":null,"expires_at":null,"stop_reason":null,"bytes":0,"dropped":0})
        }
    })
}

fn persist(c: &Connection, p: &Pending) -> Result<()> {
    let r = &p.record;
    let session = match &r.session_id {
        Some(s) => c
            .query_row(
                "SELECT workspace_id,item_id FROM sessions WHERE id=?",
                [s],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?,
        None => None,
    };
    // Generic session observers also see ordinary chats. A real session with no
    // item is outside item diagnostics, unless the caller claims a conflicting scope.
    if let Some((ws, None)) = &session
        && r.item_id.is_none()
        && (r.workspace_id.is_empty() || r.workspace_id == *ws)
    {
        return Ok(());
    }
    let identity = if let Some((ws, Some(item))) = session {
        if (!r.workspace_id.is_empty() && r.workspace_id != ws)
            || r.item_id.as_ref().is_some_and(|i| *i != item)
        {
            None
        } else {
            Some((ws, item))
        }
    } else if r.session_id.is_none() {
        match &r.item_id {
            Some(item) => c
                .query_row(
                    "SELECT workspace_id,id FROM items WHERE id=?",
                    [item],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
                .filter(|(ws, _)| r.workspace_id.is_empty() || *ws == r.workspace_id),
            None => None,
        }
    } else {
        None
    };
    let Some((ws, item)) = identity else {
        c.execute(
            "UPDATE diagnostic_stats SET dropped=dropped+1 WHERE id=1",
            [],
        )?;
        return Ok(());
    };
    if own(c, &ws, &item).is_err() {
        c.execute(
            "UPDATE diagnostic_stats SET dropped=dropped+1 WHERE id=1",
            [],
        )?;
        return Ok(());
    }
    c.execute(
        "INSERT OR IGNORE INTO diagnostic_item_stats(item_id) VALUES(?)",
        [&item],
    )?;
    if r.detail {
        let enabled:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM diagnostic_captures WHERE item_id=?1 AND (stop_reason IS NULL OR (stop_reason='manual' AND stopped_at>=?2)) AND started_at<=?2 AND expires_at>?2)",params![item,p.at],|r|r.get(0))?;
        if !enabled {
            // A replacement window must not silently hide observations still in the queue.
            c.execute("UPDATE diagnostic_item_stats SET dropped=dropped+1 WHERE item_id=?1 AND EXISTS(SELECT 1 FROM diagnostic_captures WHERE item_id=?1 AND started_at>?2)",params![item,p.at])?;
            return Ok(());
        }
    }
    let data = serde_json::to_string(
        &json!({"session_id":r.session_id,"attempt_id":r.attempt_id,"operation_id":r.operation_id,"decision_id":r.decision_id,"queued_id":r.queued_id,"cause_id":r.cause_id,"detail":r.detail,"details":r.details}),
    )?;
    let bytes = (data.len() + r.kind.len() + r.reason_code.len() + 160) as i64;
    if bytes > MAX_EVENT_BYTES as i64 {
        c.execute(
            "UPDATE diagnostic_item_stats SET dropped=dropped+1 WHERE item_id=?",
            [&item],
        )?;
        return Ok(());
    }
    let item_bytes: i64 = c.query_row(
        "SELECT bytes FROM diagnostic_item_stats WHERE item_id=?",
        [&item],
        |r| r.get(0),
    )?;
    let total: i64 = c.query_row(
        "SELECT COALESCE(sum(bytes),0) FROM diagnostic_item_stats",
        [],
        |r| r.get(0),
    )?;
    if r.detail && (item_bytes + bytes > MAX_ITEM_BYTES || total + bytes > MAX_TOTAL_BYTES) {
        let reason = if item_bytes + bytes > MAX_ITEM_BYTES {
            "item_byte_limit"
        } else {
            "store_byte_limit"
        };
        c.execute(
            "UPDATE diagnostic_captures SET stop_reason=? WHERE item_id=? AND stop_reason IS NULL",
            params![reason, item],
        )?;
        c.execute(
            "UPDATE diagnostic_item_stats SET dropped=dropped+1 WHERE item_id=?",
            [&item],
        )?;
        return Ok(());
    }
    let severity = match r.severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    };
    c.execute("INSERT INTO diagnostic_events(workspace_id,item_id,at,session_id,attempt_id,kind,reason_code,severity,detail,data,bytes) VALUES(?,?,?,?,?,?,?,?,?,?,?)",params![ws,item,p.at,r.session_id,r.attempt_id,r.kind,r.reason_code,severity,r.detail,data,bytes])?;
    c.execute(
        "UPDATE diagnostic_item_stats SET bytes=bytes+? WHERE item_id=?",
        params![bytes, item],
    )?;
    if r.detail {
        c.execute(
            "UPDATE diagnostic_captures SET bytes=bytes+? WHERE item_id=?",
            params![bytes, item],
        )?;
    }
    prune_capacity(c, Some(&item), MAX_ITEM_BYTES)?;
    prune_capacity(c, None, MAX_TOTAL_BYTES)?;
    Ok(())
}

pub(crate) fn active_until(c: &Connection) -> Result<i64> {
    Ok(c.query_row(
        "SELECT COALESCE(max(expires_at),0) FROM diagnostic_captures WHERE stop_reason IS NULL",
        [],
        |r| r.get(0),
    )?)
}

fn remove(c: &Connection, rows: &[(i64, String, i64)]) -> Result<()> {
    for (seq, item, bytes) in rows {
        c.execute("DELETE FROM diagnostic_events WHERE seq=?", [seq])?;
        c.execute("UPDATE diagnostic_item_stats SET bytes=max(0,bytes-?),removed_through=max(removed_through,?) WHERE item_id=?",params![bytes,seq,item])?;
    }
    Ok(())
}

fn prune_expired(c: &Connection, at: i64) -> Result<()> {
    let mut st = c.prepare(
        "SELECT seq,item_id,bytes FROM diagnostic_events WHERE at<? ORDER BY seq LIMIT 500",
    )?;
    let rows = st
        .query_map([at - KEEP_MS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    remove(c, &rows)
}

fn prune_capacity(c: &Connection, item: Option<&str>, max: i64) -> Result<()> {
    let total: i64 = match item {
        Some(item) => c.query_row(
            "SELECT bytes FROM diagnostic_item_stats WHERE item_id=?",
            [item],
            |r| r.get(0),
        )?,
        None => c.query_row(
            "SELECT COALESCE(sum(bytes),0) FROM diagnostic_item_stats",
            [],
            |r| r.get(0),
        )?,
    };
    if total <= max {
        return Ok(());
    }
    let mut st=c.prepare("SELECT seq,item_id,bytes FROM diagnostic_events WHERE (?1 IS NULL OR item_id=?1) ORDER BY seq")?;
    let mut query = st.query([item])?;
    let mut rows = vec![];
    let mut removed = 0;
    while let Some(r) = query.next()? {
        let bytes: i64 = r.get(2)?;
        rows.push((r.get(0)?, r.get(1)?, bytes));
        removed += bytes;
        if total - removed <= max {
            break;
        }
    }
    drop(query);
    drop(st);
    remove(c, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_precommit_snapshot_keeps_pending_and_marks_crossed_loss_transfers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("diagnostic.sqlite3"), crate::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .bots()
            .seed(
                "worker",
                false,
                &crate::NewBot {
                    name: "Worker".into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                crate::WS,
            )
            .unwrap();
        let item = store
            .items()
            .create(
                crate::WS,
                &crate::NewItem {
                    title: "Concurrent diagnostic snapshot".into(),
                    bot_id: "worker".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let event = Record {
            workspace_id: crate::WS.into(),
            item_id: Some(item.id.clone()),
            kind: "turn.ended".into(),
            reason_code: "fixture_condition".into(),
            ..Default::default()
        };
        let (snapshot_ready, snapshot_started) = mpsc::sync_channel(0);
        let (worker_done, resume_reader) = mpsc::sync_channel(0);
        let reader_store = store.clone();
        let reader_item = item.id.clone();
        let reader = store
            .with_conn(|_| {
                // The worker cannot commit before the reader has fixed its old snapshot.
                let mut invalid = event.clone();
                invalid.kind = "invalid free text".into();
                assert_eq!(store.diagnostics().record(invalid), RecordStatus::Invalid);
                assert_eq!(store.diagnostics().record(event), RecordStatus::Queued);
                let reader = std::thread::spawn(move || {
                    reader_store
                        .diagnostics()
                        .snapshot(|tx, counters| {
                            own(tx, crate::WS, &reader_item)?;
                            snapshot_ready.send(()).unwrap();
                            resume_reader.recv_timeout(Duration::from_secs(5)).unwrap();
                            reader_store
                                .diagnostics()
                                .coverage(tx, &reader_item, now(), counters)
                        })
                        .unwrap()
                });
                snapshot_started
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                Ok(reader)
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while store
            .inner
            .diagnostics
            .get()
            .unwrap()
            .pending
            .load(Ordering::Acquire)
            != 0
        {
            assert!(Instant::now() < deadline, "worker did not commit");
            std::thread::sleep(Duration::from_millis(1));
        }
        worker_done.send(()).unwrap();
        let old = reader.join().unwrap();
        assert!(old["latest_cursor"].is_null());
        assert_eq!(old["instance_pending"], 1);
        // Old persisted zero + newly flushed memory zero is provisional, never
        // a stable assertion that this instance has suffered no diagnostic loss.
        assert_eq!(old["instance_dropped"], 0);
        assert_eq!(old["loss_count_stable"], false);
        assert_eq!(old["refresh_required"], true);
        let fresh = store.diagnostics().explain(crate::WS, &item.id).unwrap();
        assert_eq!(fresh["coverage"]["latest_cursor"], 1);
        assert_eq!(fresh["coverage"]["instance_pending"], 0);
        assert_eq!(fresh["coverage"]["instance_dropped"], 1);
        assert_eq!(fresh["coverage"]["loss_count_stable"], true);
        assert_eq!(fresh["coverage"]["refresh_required"], false);
    }
}
