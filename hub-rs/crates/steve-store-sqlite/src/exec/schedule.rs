//! Deterministic soft-deadline/deficit scheduler. Time is supplied by the caller.
use std::time::Duration;

pub use steve_storage_api::lane::Lane;
pub const LANES: [Lane; 4] = [
    Lane::Interactive,
    Lane::Admin,
    Lane::Ingest,
    Lane::Background,
];
const WEIGHTS: [i128; 4] = [4, 4, 2, 1];
const QUANTUM: i128 = 1_000_000; // one millisecond of actual connection ownership

pub fn index(lane: Lane) -> usize {
    match lane {
        Lane::Interactive => 0,
        Lane::Admin => 1,
        Lane::Ingest => 2,
        Lane::Background => 3,
    }
}
pub fn name(lane: Lane) -> &'static str {
    match lane {
        Lane::Interactive => "interactive",
        Lane::Admin => "admin",
        Lane::Ingest => "ingest",
        Lane::Background => "background",
    }
}
pub fn target(lane: Lane) -> Duration {
    Duration::from_millis(match lane {
        Lane::Interactive => 50,
        Lane::Admin => 200,
        Lane::Ingest => 20,
        Lane::Background => 1000,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Head {
    pub enqueued: Duration,
    pub ready_at: Duration,
}

#[derive(Default, Debug)]
pub struct Scheduler {
    deficit: [i128; 4],
    cursor: usize,
}
impl Scheduler {
    /// Overdue heads take priority; otherwise spend DRR credits, retaining unused service time.
    /// An unready ingest head cannot bypass its coalescing window. Empty lanes bank no credits.
    pub fn choose(&mut self, now: Duration, heads: [Option<Head>; 4]) -> Option<Lane> {
        for (i, h) in heads.iter().enumerate() {
            if h.is_none() {
                self.deficit[i] = 0;
            }
        }
        let eligible = |i: usize| heads[i].is_some_and(|h| h.ready_at <= now);
        let mut overdue = None;
        for i in 0..4 {
            if !eligible(i) {
                continue;
            }
            let h = heads[i].unwrap();
            let deadline = h.enqueued.saturating_add(target(LANES[i]));
            if now > deadline {
                let late = now - deadline;
                if overdue.is_none_or(|(_, old_late)| late > old_late) {
                    overdue = Some((i, late));
                }
            }
        }
        if let Some((i, _)) = overdue {
            return Some(LANES[i]);
        }
        let candidates: Vec<_> = (0..4)
            .map(|n| (self.cursor + n) % 4)
            .filter(|&i| eligible(i))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        if !candidates.iter().any(|&i| self.deficit[i] > 0) {
            // Jump whole rounds after a long holder, instead of looping once per elapsed quantum.
            let rounds = candidates
                .iter()
                .map(|&i| (-self.deficit[i]) / (QUANTUM * WEIGHTS[i]) + 1)
                .min()
                .unwrap();
            for &i in &candidates {
                self.deficit[i] += rounds * QUANTUM * WEIGHTS[i];
            }
        }
        let i = candidates
            .into_iter()
            .find(|&i| self.deficit[i] > 0)
            .unwrap();
        self.cursor = i;
        Some(LANES[i])
    }
    /// Called exactly once at release, including service granted by the deadline override.
    pub fn charge(&mut self, lane: Lane, held: Duration) {
        // A zero-resolution clock still makes progress; real elapsed time is otherwise unrounded.
        self.deficit[index(lane)] -= held.as_nanos().max(1) as i128;
    }
}
