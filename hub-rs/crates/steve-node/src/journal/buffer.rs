//! One window for the whole journal. Sequence numbers belong to committed events, so replacing
//! an unnumbered snapshot or joining adjacent text cannot leave holes in a stream.
use super::*;

pub(super) const WINDOW: Duration = Duration::from_millis(200);
const MAX_EVENTS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;
const TEXT_BYTES: usize = 16 * 1024;

pub(crate) struct Prepared {
    pub subject: Subject,
    pub body: EventBody,
    pub subject_json: String,
    pub body_json: String,
}

pub(crate) fn needs_budget(body: &EventBody) -> bool {
    match body {
        EventBody::OperationChanged { .. }
        | EventBody::AttemptEnded { .. }
        | EventBody::PermissionExpired { .. }
        | EventBody::PermissionResolved { .. }
        | EventBody::SessionChanged { .. }
        | EventBody::MessageCompleted { .. } => false,
        EventBody::Usage { report } => report.measurement != Measurement::Final,
        _ => true,
    }
}

impl Prepared {
    pub fn bytes(&self) -> usize {
        self.subject_json.len() + self.body_json.len() + 512
    }

    pub fn data(&self) -> bool {
        needs_budget(&self.body)
    }

    pub fn boundary(&self) -> bool {
        match &self.body {
            EventBody::TextDelta { .. } | EventBody::Plan { .. } => false,
            EventBody::ToolChanged { tool } => {
                matches!(tool.phase, ToolPhase::Succeeded | ToolPhase::Failed)
            }
            EventBody::Usage { report } => report.measurement == Measurement::Final,
            _ => true,
        }
    }

    fn replaces(&self, old: &Self) -> bool {
        if self.subject != old.subject {
            return false;
        }
        match (&self.body, &old.body) {
            (EventBody::ToolChanged { tool: a }, EventBody::ToolChanged { tool: b }) => {
                a.id == b.id
            }
            (EventBody::Plan { .. }, EventBody::Plan { .. }) => true,
            (EventBody::Usage { report: a }, EventBody::Usage { report: b }) => {
                a.measurement != Measurement::Final && a.measurement == b.measurement
                    && a.scope == b.scope && a.meter_epoch == b.meter_epoch && a.model == b.model
                    && usage_fields(a) == usage_fields(b)
                    // Delta reports add, rather than replace. Only a duplicate report id can
                    // replace a delta; cumulative snapshots can replace any previous snapshot.
                    && (a.measurement != Measurement::Delta || a.report_id == b.report_id)
            }
            _ => false,
        }
    }

    fn join(&mut self, next: &Self) -> bool {
        if self.subject != next.subject {
            return false;
        }
        let (
            EventBody::TextDelta {
                message: a,
                block: ab,
                utf8_offset: ao,
                text: at,
            },
            EventBody::TextDelta {
                message: b,
                block: bb,
                utf8_offset: bo,
                text: bt,
            },
        ) = (&mut self.body, &next.body)
        else {
            return false;
        };
        if a != b
            || ab != bb
            || ao.checked_add(at.len() as u64) != Some(*bo)
            || at.len() + bt.len() > TEXT_BYTES
        {
            return false;
        }
        // Text is the last field of this fixed wire variant. Reuse its already escaped JSON
        // contents: neither the old text nor the new text is serialized a second time.
        let start = next.body_json.len() - 2 - escaped_len(bt);
        self.body_json.truncate(self.body_json.len() - 2);
        self.body_json.push_str(&next.body_json[start..]);
        at.push_str(bt);
        true
    }
}

/// Token consumption, context size and cost are independent cumulative observations on the
/// same meter. Replacing a context-only report must not erase its last token observation.
fn usage_fields(report: &UsageReport) -> [bool; 8] {
    [
        report.tokens.input.is_some(),
        report.tokens.output.is_some(),
        report.tokens.cache_read.is_some(),
        report.tokens.cache_write.is_some(),
        report.tokens.reasoning.is_some(),
        report.context_used.is_some(),
        report.context_limit.is_some(),
        report.cost.is_some(),
    ]
}

fn escaped_len(text: &str) -> usize {
    text.bytes()
        .map(|b| match b {
            b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum()
}

pub(crate) fn prepare(events: Vec<(Subject, EventBody)>) -> ApiResult<Vec<Prepared>> {
    let mut out = vec![];
    for (subject, body) in events {
        let subject_json = serde_json::to_string(&subject).map_err(internal)?;
        for (body, body_json) in crate::fit_encoded(&subject_json, body) {
            out.push(Prepared {
                subject: subject.clone(),
                subject_json: subject_json.clone(),
                body,
                body_json,
            });
        }
    }
    Ok(out)
}

#[derive(Default)]
pub(super) struct Buffer {
    pub events: Vec<Prepared>,
    pub since: Option<Instant>,
    count: usize,
    bytes: usize,
    merge_from: usize,
}

impl Buffer {
    pub fn push(&mut self, event: Prepared) {
        self.since.get_or_insert_with(Instant::now);
        self.count += 1;
        self.bytes += event.bytes();
        // A terminal tool view closes the current causal interval. Keep the last pre-terminal
        // view before it and commit both; only unfinished views replace each other.
        if event.boundary() {
            self.events.push(event);
            self.merge_from = self.events.len();
            return;
        }
        if self.events.last_mut().is_some_and(|last| last.join(&event)) {
            return;
        }
        // Remove the superseded view and append the replacement at its *new* position. Updating
        // it in place would move new information before intervening events from another tool.
        if let Some(at) = self.events[self.merge_from..]
            .iter()
            .position(|old| event.replaces(old))
        {
            self.events.remove(self.merge_from + at);
        }
        self.events.push(event);
    }

    pub fn over_budget(&self, used: u64, limit: u64) -> bool {
        let has_data = self.events.iter().any(|event| event.data());
        let bytes = self.events.iter().fold(0u64, |bytes, event| {
            bytes.saturating_add(event.bytes() as u64)
        });
        has_data && !data_fits(used, bytes, limit)
    }

    pub fn ready(&self) -> bool {
        self.count >= MAX_EVENTS || self.bytes >= MAX_BYTES
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|since| since + WINDOW)
    }
    pub fn take(&mut self) -> Self {
        std::mem::take(self)
    }
    pub fn bytes(&self) -> u64 {
        self.bytes as u64
    }
}
