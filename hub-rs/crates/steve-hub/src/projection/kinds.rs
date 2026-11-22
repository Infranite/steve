//! What the projection knows of each kind: which views are hot, and how its owner reads them.

use std::sync::OnceLock;

use steve_store::{Store, Views};
use steve_types::{SessionView, View, ViewKind};

/// The coordinators configured on this Hub. A bound session without one is parked, and still owns
/// its execution slot. Like RoutedRuntime, a remote coordinator may exist before a link comes up.
#[derive(Clone, Default)]
pub(crate) struct RuntimeRoutes {
    pub(crate) local: Option<String>,
    pub(crate) remote: bool,
}

impl RuntimeRoutes {
    pub(crate) fn apply(&self, s: &mut SessionView) {
        if let Some(n) = &s.node_id {
            let id = n.as_str();
            s.busy |= self.local.as_deref() != Some(id)
                && (id == steve_store::runtime::HUB_NODE || !self.remote);
        }
    }

    pub(crate) fn apply_to(&self, v: &mut View) {
        if let View::Session(s) = v {
            self.apply(s);
        }
    }
}

/// The hot views of every workspace (loading, rebuilding, reconciling).
pub(super) fn hot_all(store: &Store, routes: &RuntimeRoutes) -> steve_store::Result<Vec<View>> {
    store.read(|r| {
        let mut out: Vec<View> = r
            .decision_views()
            .open()?
            .into_iter()
            .map(View::Decision)
            .collect();
        out.extend(r.item_views().hot()?.into_iter().map(View::Item));
        out.extend(r.session_views().hot()?.into_iter().map(View::Session));
        for v in &mut out {
            routes.apply_to(v);
        }
        Ok(out)
    })
}

pub(crate) fn read_by_id(
    store: &Store,
    routes: &RuntimeRoutes,
    k: ViewKind,
    ws: &str,
    ids: &[String],
) -> steve_store::Result<Vec<View>> {
    store.read(|r| of(r, routes, k, ws, ids))
}

pub(super) fn of(
    r: &mut impl Views,
    routes: &RuntimeRoutes,
    k: ViewKind,
    ws: &str,
    ids: &[String],
) -> steve_store::Result<Vec<View>> {
    let mut views: Vec<View> = match k {
        ViewKind::Decision => r
            .decision_views()
            .of(ws, ids)?
            .into_iter()
            .map(View::Decision)
            .collect(),
        ViewKind::Item => r
            .item_views()
            .of(ws, ids)?
            .into_iter()
            .map(View::Item)
            .collect(),
        ViewKind::Session => r
            .session_views()
            .of(ws, ids)?
            .into_iter()
            .map(View::Session)
            .collect(),
    };
    for v in &mut views {
        routes.apply_to(v);
    }
    Ok(views)
}

/// Whether the projection keeps `v` (the same definition as the web client's `table.ts`).
pub(super) fn is_hot(v: &View) -> bool {
    match v {
        View::Decision(d) => d.status == "open",
        View::Item(i) => !i.archived,
        View::Session(s) => s.status != "ended",
    }
}

/// The projection's metrics of one kind, registered once.
pub(super) struct Meters {
    pub delay_seconds: metrics::Histogram,
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
                    delay_seconds: metrics::histogram!("steve_projection_delay_seconds", "kind" => kind),
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
