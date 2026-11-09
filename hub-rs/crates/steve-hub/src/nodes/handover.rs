//! B5: work waiting for a device that stays away goes on elsewhere. A session bound to a remote
//! Node that has been offline for `STEVE_NODE_HANDOFF_MIN` minutes (default 30; `0`: never), with
//! messages waiting for it, is ended and its item continued in a new session, placed as any new
//! session is, with its conversation and the waiting messages ([`steve_core::Core::
//! hand_over_from_offline`]). Steve posts a notice. A device counts as away from when its link
//! closed, and never longer than this Hub has been up.

use std::sync::Arc;
use std::time::Duration;

use super::NodeService;

pub fn minutes(var: impl Fn(&str) -> Option<String>) -> i64 {
    var("STEVE_NODE_HANDOFF_MIN")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|m| *m >= 0)
        .unwrap_or(30)
}

/// Sessions to hand over now: (session, device name, minutes away).
pub fn due(
    store: &steve_store::Store,
    svc: &NodeService,
    minutes: i64,
    up_since: i64,
    now: i64,
) -> Vec<(String, String, i64)> {
    let rows: Vec<(String, String, String, i64)> = store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT r.session_id, n.node_id, n.name, COALESCE(n.last_seen, n.created)
                   FROM runtime_sessions r JOIN nodes n ON n.node_id = r.node_id
                   JOIN sessions s ON s.id = r.session_id
                  WHERE n.local = 0 AND n.state IN ('active','paused') AND s.status <> 'ended'
                    AND EXISTS (SELECT 1 FROM queued q WHERE q.session_id = r.session_id)",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .unwrap_or_default();
    rows.into_iter()
        .filter(|(_, node, _, _)| svc.registry.current(node).is_none())
        .filter_map(|(sid, _, name, seen)| {
            let away = (now - seen.max(up_since)) / 60_000;
            (away >= minutes).then_some((sid, name, away))
        })
        .collect()
}

pub async fn run(svc: Arc<NodeService>, core: steve_core::Core, minutes: i64) {
    if minutes == 0 {
        return;
    }
    let up_since = super::now();
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let (store, svc2, core2) = (svc.store.clone(), svc.clone(), core.clone());
        let moved = tokio::task::spawn_blocking(move || {
            let mut moved = vec![];
            for (sid, name, away) in due(&store, &svc2, minutes, up_since, super::now()) {
                match core2.hand_over_from_offline(&sid, &name, away) {
                    Ok(Some(new)) => {
                        tracing::info!(session = %sid, new = %new, "device {name} away {away} min: handed over");
                        moved.push((sid, name, away));
                    }
                    Ok(None) => {}
                    Err(e) => tracing::error!(session = %sid, "handing over from {name}: {e}"),
                }
            }
            moved
        })
        .await
        .unwrap_or_default();
        if moved.is_empty() {
            continue;
        }
        let store = svc.store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let db = steve_core::repo::Db::new(store);
            for (_, name, away) in moved {
                let text = steve_i18n::t!("hub.node.handed_over", name = name, minutes = away);
                if let Err(e) =
                    steve_core::notify::notify(&db, steve_store::WS, "system", "info", &text, None)
                {
                    tracing::error!("hand-over notice: {e}");
                }
            }
        })
        .await;
    }
}
