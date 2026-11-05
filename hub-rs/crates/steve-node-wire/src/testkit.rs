//! In-memory links for tests, with faults: cut a side after N frames written, or swallow every
//! response it would receive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::envelope::{Frame, FrameSink, FrameStream};

/// Faults of one direction (what one side writes).
#[derive(Default)]
pub struct Faults {
    /// Frames still allowed before the link is cut (negative: unlimited).
    budget: AtomicI64,
    cut: AtomicBool,
    /// Response frames written by this side vanish (the peer never hears an answer).
    pub drop_responses: AtomicBool,
    /// Writes hang (a peer that stopped reading) until unstalled.
    pub stalled: AtomicBool,
}

impl Faults {
    fn new() -> Arc<Faults> {
        let f = Faults::default();
        f.budget.store(-1, Ordering::SeqCst);
        Arc::new(f)
    }
    /// After `n` more frames written, everything breaks (both directions).
    pub fn cut_after(&self, n: i64) {
        self.budget.store(n, Ordering::SeqCst);
    }
    pub fn cut_now(&self) {
        self.cut.store(true, Ordering::SeqCst);
    }
    pub fn stall(&self, on: bool) {
        self.stalled.store(on, Ordering::SeqCst);
    }
    pub fn is_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }
}

pub struct End {
    pub sink: FrameSink,
    pub stream: FrameStream,
    /// Faults of what this end writes.
    pub faults: Arc<Faults>,
}

fn end(
    tx: mpsc::UnboundedSender<Frame>,
    rx: mpsc::UnboundedReceiver<Frame>,
    mine: Arc<Faults>,
    theirs: Arc<Faults>,
) -> End {
    let f = mine.clone();
    let sink = futures_util::sink::unfold(tx, move |tx, frame: Frame| {
        let f = f.clone();
        async move {
            while f.stalled.load(Ordering::SeqCst) && !f.is_cut() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            if f.is_cut() {
                return Err(std::io::Error::other("cut"));
            }
            let b = f.budget.load(Ordering::SeqCst);
            if b == 0 {
                f.cut_now();
                return Err(std::io::Error::other("cut"));
            }
            if b > 0 {
                f.budget.fetch_sub(1, Ordering::SeqCst);
            }
            let is_res = matches!(&frame, Frame::Text(t) if t.contains("\"t\":\"res\""));
            if !(is_res && f.drop_responses.load(Ordering::SeqCst)) {
                let _ = tx.send(frame);
            }
            Ok(tx)
        }
    });
    let (m, t) = (mine.clone(), theirs.clone());
    let stream = futures_util::stream::unfold(rx, move |mut rx| {
        let (m, t) = (m.clone(), t.clone());
        async move {
            if m.is_cut() || t.is_cut() {
                return None;
            }
            // a cut on either side ends this stream too
            loop {
                tokio::select! {
                    f = rx.recv() => return f.map(|f| (Ok(f), rx)),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                        if m.is_cut() || t.is_cut() { return None; }
                    }
                }
            }
        }
    });
    End {
        sink: Box::pin(sink.sink_map_err(|e| e)),
        stream: stream.boxed(),
        faults: mine,
    }
}

/// Two connected ends: what `a` writes `b` reads, and the other way round.
pub fn pair() -> (End, End) {
    let (atx, brx) = mpsc::unbounded_channel();
    let (btx, arx) = mpsc::unbounded_channel();
    let (fa, fb) = (Faults::new(), Faults::new());
    (end(atx, arx, fa.clone(), fb.clone()), end(btx, brx, fb, fa))
}
