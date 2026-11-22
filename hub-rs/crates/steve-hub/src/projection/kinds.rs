//! What the projection knows of each kind: which views are hot, and how its owner reads them.

use std::sync::OnceLock;

use steve_store::{Store, Views};
use steve_types::{View, ViewKind};

/// The hot views of every workspace (loading, rebuilding, reconciling).
pub(super) fn hot_all(store: &Store) -> steve_store::Result<Vec<View>> {
    let mut out = Vec::new();
    for k in ViewKind::ALL {
        match k {
            ViewKind::Decision => {
                let ds = store.read(|r| r.decision_views().open())?;
                out.extend(ds.into_iter().map(View::Decision));
            }
        }
    }
    Ok(out)
}

/// The views `ids` of kind `k` in `ws` that exist, hot or not: one statement per 500 ids (and the
/// facts the decisions' tiers need).
pub fn read_by_id(
    store: &Store,
    k: ViewKind,
    ws: &str,
    ids: &[String],
) -> steve_store::Result<Vec<View>> {
    Ok(match k {
        ViewKind::Decision => {
            let ds = store.read(|r| r.decision_views().of(ws, ids))?;
            ds.into_iter().map(View::Decision).collect()
        }
    })
}

/// Whether the projection keeps `v` (the same definition as the web client's `table.ts`).
pub(super) fn is_hot(v: &View) -> bool {
    match v {
        View::Decision(d) => d.status == "open",
    }
}

pub(super) fn id(v: &View) -> &str {
    match v {
        View::Decision(d) => d.id.as_str(),
    }
}

pub(super) fn ws(v: &View) -> &str {
    match v {
        View::Decision(d) => d.ws.as_str(),
    }
}

/// The projection's metrics of one kind, registered once.
pub(super) struct Meters {
    pub flush_seconds: metrics::Histogram,
    pub flush_entities: metrics::Histogram,
    pub upserts: metrics::Counter,
    pub removes: metrics::Counter,
    pub entities: metrics::Gauge,
    pub bytes: metrics::Gauge,
    pub reconcile_diff: metrics::Counter,
}

pub(super) fn meters(k: ViewKind) -> &'static Meters {
    static ALL: OnceLock<Vec<Meters>> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        ViewKind::ALL
            .iter()
            .map(|k| {
                let kind = k.as_str();
                Meters {
                    flush_seconds: metrics::histogram!("steve_projection_flush_seconds", "kind" => kind),
                    flush_entities: metrics::histogram!("steve_projection_flush_entities", "kind" => kind),
                    upserts: metrics::counter!("steve_projection_patch_ops_total", "kind" => kind, "op" => "upsert"),
                    removes: metrics::counter!("steve_projection_patch_ops_total", "kind" => kind, "op" => "remove"),
                    entities: metrics::gauge!("steve_projection_entities", "kind" => kind),
                    bytes: metrics::gauge!("steve_projection_bytes", "kind" => kind),
                    reconcile_diff: metrics::counter!("steve_projection_reconcile_diff_total", "kind" => kind),
                }
            })
            .collect()
    });
    &all[ViewKind::ALL
        .iter()
        .position(|x| *x == k)
        .expect("a listed kind")]
}
