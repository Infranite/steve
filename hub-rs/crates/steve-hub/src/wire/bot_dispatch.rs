//! Strict placement for new sessions only. The legacy router is untouched when neither a
//! workspace Bot binding nor a Team applies. Existing sessions keep their durable Node.
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_core::{CoreError, Result};
use steve_node_api::{
    AdapterStatus, HarnessCapability, HarnessChoice, NodeId, PermissionMode, SetupState,
};
use steve_store::{
    Bot, StoreError,
    bot_dispatch::{self, Scope},
};

use super::{core::RoutedRuntime, node_binding};

type Reason = &'static str;

fn text(reason: &str) -> String {
    match reason {
        "outside_team" => steve_i18n::t!("hub.dispatch.outside_team"),
        "no_bindings" => steve_i18n::t!("hub.dispatch.no_bindings"),
        "offline" => steve_i18n::t!("hub.dispatch.offline"),
        "permission" => steve_i18n::t!("hub.dispatch.permission"),
        "project" => steve_i18n::t!("hub.dispatch.project"),
        "affinity" => steve_i18n::t!("hub.dispatch.affinity"),
        "tag" => steve_i18n::t!("hub.dispatch.tag"),
        "model" => steve_i18n::t!("hub.dispatch.model"),
        "effort" => steve_i18n::t!("hub.dispatch.effort"),
        "harness" => steve_i18n::t!("hub.dispatch.harness"),
        "signin" => steve_i18n::t!("hub.dispatch.signin"),
        "session_probe" => steve_i18n::t!("hub.dispatch.session_probe"),
        "pending" => steve_i18n::t!("hub.dispatch.pending"),
        "unauthorized" => steve_i18n::t!("hub.dispatch.unauthorized"),
        "bot" => steve_i18n::t!("hub.dispatch.bot"),
        _ => steve_i18n::t!("hub.dispatch.bind_failed"),
    }
}

fn compatible(
    cap: Option<&HarnessCapability>,
    choice: &HarnessChoice,
    mode: Option<PermissionMode>,
) -> Option<Reason> {
    let Some(cap) = cap else {
        return Some("harness");
    };
    match cap.setup.as_ref().map(|s| s.state) {
        Some(SetupState::Installing | SetupState::Unknown) => return Some("pending"),
        Some(SetupState::NotInstalled) => return Some("harness"),
        _ => {}
    }
    let Some(adapter) = &cap.adapter else {
        return Some("pending");
    };
    if adapter.status != AdapterStatus::Ready {
        return Some("harness");
    }
    if adapter.session_auth_required == Some(true) {
        return Some("signin");
    }
    let Some(offer) = &adapter.session else {
        return Some(if adapter.session_error.is_some() {
            "session_probe"
        } else {
            "pending"
        });
    };
    if cap
        .setup
        .as_ref()
        .is_some_and(|s| s.state != SetupState::Ready)
    {
        return Some("session_probe");
    }
    if let Some(model) = &choice.model {
        if !offer.models.iter().any(|m| &m.id == model) {
            return Some("model");
        }
    } else if offer.default_model.as_ref().is_none_or(|m| m.is_empty()) {
        // The actual default of an opaque offer is not known.
        return Some("pending");
    }
    if choice
        .effort
        .as_ref()
        .is_some_and(|e| !offer.efforts.contains(e))
    {
        return Some("effort");
    }
    let Some(modes) = &adapter.execution_modes else {
        return Some("pending");
    };
    if mode.is_none_or(|mode| !modes.contains(&mode) || !offer.modes.contains(&mode)) {
        return Some("permission");
    }
    None
}

/// Structural constraints shared by preflight and the binding transaction. An ended *unbound*
/// failed attempt has no files and may be ignored. A legacy session with a real cwd still fixes
/// the work locally, even if it predates the Node runtime ledger.
pub(super) fn node_blocker(
    c: &Connection,
    ws: &str,
    item: &str,
    session: Option<&str>,
    node: &str,
) -> steve_store::Result<Option<Reason>> {
    if node != "local" {
        if !node_binding::node_takes(c, node, ws)? {
            return Ok(Some("unauthorized"));
        }
        let project: bool = c.query_row(
            "SELECT project_id IS NOT NULL FROM items WHERE id=? AND workspace_id=?",
            params![item, ws],
            |r| r.get(0),
        )?;
        if project {
            return Ok(Some("project"));
        }
    }
    if let Some(tag) = steve_store::nodes::item_tag(c, item)?
        && !steve_store::nodes::with_tag(c, &tag)?
            .iter()
            .any(|n| n == node)
    {
        return Ok(Some("tag"));
    }
    let other: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s LEFT JOIN runtime_sessions r ON r.session_id=s.id
         WHERE s.item_id=?1 AND (?2 IS NULL OR s.id<>?2) AND s.readonly=0 AND s.source='Steve' AND
           ((r.node_id IS NOT NULL AND r.node_id<>?3)
            OR (r.node_id IS NULL AND (s.status<>'ended' OR s.acp_session_id IS NOT NULL) AND ?3<>'local')))",
        params![item, session, node], |r| r.get(0),
    )?;
    if other {
        return Ok(Some("affinity"));
    }
    Ok(None)
}

/// Authorization is held through the same SQLite transaction that fixes the Node. Reject a
/// legacy decision raced by an opt-in, and reject a strict decision raced by a revocation.
pub(super) fn commit_guard(
    c: &Connection,
    session: &str,
    node: &str,
    strict: bool,
) -> steve_store::Result<()> {
    let row: Option<(String, String, String)> = c.query_row(
        "SELECT workspace_id,item_id,COALESCE(bot_id,'steve') FROM sessions WHERE id=? AND item_id IS NOT NULL",
        [session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    let Some((ws, item, bot)) = row else {
        return Ok(());
    };
    match bot_dispatch::scope(c, &ws, &item, &bot)? {
        Scope::Legacy if !strict => Ok(()),
        Scope::Restricted(nodes) if strict && nodes.iter().any(|n| n == node) => {
            let active: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM bots b
                 JOIN bot_workspaces bw ON bw.bot_id=b.id WHERE b.id=?1 AND bw.workspace_id=?2
                   AND b.enabled=1 AND b.deleted=0)",
                params![bot, ws],
                |r| r.get(0),
            )?;
            if !active {
                return Err(StoreError::Conflict(text("bot")));
            }
            if let Some(why) = node_blocker(c, &ws, &item, Some(session), node)? {
                return Err(StoreError::Conflict(text(why)));
            }
            Ok(())
        }
        _ => Err(StoreError::Conflict(text("bind_failed"))),
    }
}

impl RoutedRuntime {
    fn dispatch_probe_target(&self, node: &str) -> Option<Option<u64>> {
        if node == "local" {
            self.node.as_ref().filter(|n| !n.is_stopped())?;
            if self.host.local_node.get()?.in_process() {
                return Some(None);
            }
        }
        let id = NodeId::new(node.to_string()).ok()?;
        Some(Some(self.fleet.as_ref()?.remote(&id).ready_generation()?))
    }

    fn report(
        &self,
        node: &str,
        choice: &HarnessChoice,
        requested_mode: Option<&str>,
    ) -> (Option<Reason>, Option<i64>) {
        let Some(generation) = self.dispatch_probe_target(node) else {
            return (Some("offline"), None);
        };
        let Some((at, cap)) = self
            .dispatch_harnesses
            .as_ref()
            .and_then(|h| h.target_report(node, generation, &choice.harness))
        else {
            return (Some("pending"), None);
        };
        let mode = node_binding::permission_mode(requested_mode.unwrap_or("default"));
        let bad = compatible(cap.as_ref(), choice, mode);
        if bad.is_none()
            && generation.is_some()
            && mode == Some(PermissionMode::Auto)
            && !NodeId::new(node.to_string()).ok().is_some_and(|id| {
                self.fleet
                    .as_ref()
                    .is_some_and(|f| f.speaks(&id, steve_node_api::AUTO_MODE))
            })
        {
            return (Some("permission"), Some(at));
        }
        (bad, Some(at))
    }

    fn bot_choice(&self, ws: &str, bot: &Bot) -> HarnessChoice {
        HarnessChoice {
            harness: bot.harness.clone(),
            model: bot.model.clone().or_else(|| {
                self.host
                    .core
                    .get()
                    .and_then(|c| c.models().agent_model(ws, &bot.harness))
            }),
            effort: bot.reasoning_effort.clone(),
        }
    }

    pub(super) fn bot_candidates(&self, ws: &str, bot: &str) -> Result<Value> {
        let bot = self
            .store
            .bots()
            .get(bot)?
            .filter(|b| !b.main)
            .ok_or(StoreError::NotFound("Bot not found"))?;
        if !self.store.bots().in_workspace(&bot.id, ws)? {
            return Err(StoreError::NotFound("Bot not found").into());
        }
        let (bindings, names, configured) = self.store.reader(|c| {
            let bindings = bot_dispatch::bindings(c, ws, &bot.id)?;
            let mut names = vec![("local".to_string(), None)];
            let mut q = c.prepare("SELECT n.node_id,n.name FROM nodes n
             WHERE n.local=0 AND n.state='active' AND EXISTS
               (SELECT 1 FROM workspace_members m WHERE m.workspace_id=?1 AND m.user_id=n.owner AND m.role='owner')
             AND NOT EXISTS(SELECT 1 FROM workspace_members m WHERE m.workspace_id=?1 AND m.user_id<>n.owner)
             ORDER BY n.name,n.node_id")?;
            let rows = q.query_map([ws], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?)))?;
            names.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
            // Stale bindings must remain removable, without leaking a formerly authorized Node's
            // name or capability report once its grant has disappeared.
            let visible: Vec<String> = names.iter().map(|(id, _)| id.clone()).collect();
            let configured = bindings.as_ref().map(|ids| ids.iter().filter(|id| visible.contains(id)).cloned().collect::<Vec<_>>());
            Ok::<_, StoreError>((bindings, names, configured))
        })?;
        let choice = self.bot_choice(ws, &bot);
        let candidates: Vec<Value> = names
            .into_iter()
            .map(|(id, name)| {
                let (reason, checked_at) = self.report(&id, &choice, Some(&bot.perm_mode));
                let state = match reason {
                    None => "candidate",
                    Some("pending") => "pending",
                    _ => "unavailable",
                };
                json!({"id":id,"name":name,"state":state,"reason":reason,"checked_at":checked_at,
                   "provider_verified":false})
            })
            .collect();
        let hidden =
            bindings.as_ref().map_or(0, Vec::len) - configured.as_ref().map_or(0, Vec::len);
        Ok(json!({"bindings": configured,"unavailable_bindings": hidden,"candidates": candidates}))
    }

    /// Preflight before the Core prepares any worktree or stops a session for a handoff.
    pub(super) fn check_dispatch(&self, item: &str, bot: &str) -> Result<bool> {
        let row = self
            .store
            .items()
            .get(item)?
            .ok_or(StoreError::NotFound("Item not found"))?;
        let scope = self
            .store
            .reader(|c| bot_dispatch::scope(c, &row.workspace_id, item, bot))?;
        let nodes = match scope {
            Scope::Legacy => return Ok(false),
            Scope::Restricted(nodes) => nodes,
            Scope::OutsideTeam => return Err(CoreError::Msg(text("outside_team"))),
        };
        let available = self.store.bots().get(bot)?.filter(|b| b.enabled);
        let in_workspace = self.store.bots().in_workspace(bot, &row.workspace_id)?;
        let mut rejected = Vec::new();
        if let Some(b) = available.filter(|_| in_workspace) {
            let choice = self.bot_choice(&row.workspace_id, &b);
            for n in nodes {
                let why = self
                    .store
                    .reader(|c| node_blocker(c, &row.workspace_id, item, None, &n))?;
                // Local mode can be widened by the item's policy at actual bind time. Do not
                // reject a mode here that may be made compatible then. The bind checks it exactly.
                let report = self.report(&n, &choice, Some(&b.perm_mode)).0;
                match why.or(report.filter(|r| *r != "permission")) {
                    None => return Ok(true),
                    Some(why) => rejected.push(why),
                }
            }
        } else {
            rejected.push("bot");
        }
        let why = rejected.first().copied().unwrap_or("no_bindings");
        // An unsuccessful reassignment must not overwrite the placement of an active session.
        let active = self
            .store
            .sessions()
            .of_item(item)?
            .iter()
            .any(|s| s.status != "ended");
        if !active {
            self.save_dispatch(item, bot, None, None, why, &rejected)?;
        }
        Err(CoreError::Msg(text(why)))
    }

    /// true: handled strictly, whether bound or refused; false: serve the legacy router.
    pub(super) fn bind_dispatch(&self, session: &str, after: Option<&str>) -> Result<bool> {
        let Some(s) = self.store.sessions().get(session)? else {
            return Ok(true);
        };
        let Some(item) = s.item_id.as_deref() else {
            return Ok(false);
        };
        let bot = s.bot_id.as_deref().unwrap_or("steve");
        let scope = self
            .store
            .reader(|c| bot_dispatch::scope(c, &s.workspace_id, item, bot))?;
        match scope {
            Scope::Legacy => return Ok(false),
            Scope::OutsideTeam => {
                self.save_dispatch(item, bot, Some(session), None, "outside_team", &[])?;
                return Ok(true);
            }
            Scope::Restricted(nodes) => {
                let choice = HarnessChoice {
                    harness: s.harness.clone(),
                    model: s.model.clone(),
                    effort: s.reasoning_effort.clone(),
                };
                let mode = self.perm_mode(session);
                let mut rejected = Vec::new();
                for (i, node) in nodes.iter().enumerate() {
                    let why = self
                        .store
                        .reader(|c| node_blocker(c, &s.workspace_id, item, Some(session), node))?;
                    if let Some(why) = why.or_else(|| self.report(node, &choice, mode.as_deref()).0)
                    {
                        rejected.push(why);
                        continue;
                    }
                    let planned = if node == "local" {
                        self.node
                            .as_ref()
                            .cloned()
                            .zip(self.local_plan(session).ok())
                    } else {
                        NodeId::new(node.clone()).ok().and_then(|id| {
                            let fleet = self.fleet.as_ref()?;
                            // Authorization, tag and same-Node affinity checked above and again
                            // in bind_after_scoped. No Hub directory is translated to the remote.
                            let plan = node_binding::scoped_remote_plan(
                                &self.store,
                                session,
                                mode.as_deref(),
                            )?;
                            Some((fleet.start(&id)?, plan))
                        })
                    };
                    let Some((rt, plan)) = planned else {
                        rejected.push("project");
                        continue;
                    };
                    if plan.choice != choice
                        || self.report(node, &choice, mode.as_deref()).0.is_some()
                    {
                        rejected.push("pending");
                        continue;
                    }
                    let placement = placement(
                        bot,
                        Some(session),
                        Some(node),
                        if i == 0 { "preferred" } else { "fallback" },
                        &rejected,
                    );
                    match rt.bind_after_scoped(&plan, after, &placement) {
                        Ok(()) => return Ok(true),
                        Err(e) => tracing::info!(session, node, "scoped binding refused: {e}"),
                    }
                    rejected.push("bind_failed");
                }
                let why = rejected.first().copied().unwrap_or("no_bindings");
                self.save_dispatch(item, bot, Some(session), None, why, &rejected)?;
            }
        }
        Ok(true)
    }

    fn save_dispatch(
        &self,
        item: &str,
        bot: &str,
        session: Option<&str>,
        node: Option<&str>,
        reason: Reason,
        rejected: &[Reason],
    ) -> Result<()> {
        self.store.transaction(|tx| {
            bot_dispatch::decision(tx, item, &placement(bot, session, node, reason, rejected))?;
            tx.publish(steve_store::ChangeEvent::Item { id: item.into() });
            Ok::<_, StoreError>(())
        })?;
        Ok(())
    }

    pub(super) fn dispatch_refusal(&self, session: &str) -> Option<String> {
        let s = self.store.sessions().get(session).ok()??;
        let item = s.item_id?;
        self.store
            .reader(|c| {
                let r = bot_dispatch::last_decision(c, &item)?;
                Ok::<_, StoreError>(
                    r.filter(|v| v["session"] == session && v["node"].is_null())
                        .and_then(|v| v["reason"].as_str().map(|r| steve_i18n::stored(|| text(r)))),
                )
            })
            .ok()?
    }
}

fn placement(
    bot: &str,
    session: Option<&str>,
    node: Option<&str>,
    reason: Reason,
    skipped: &[Reason],
) -> Value {
    json!({"bot":bot,"session":session,"node":node,"reason":reason,"skipped":skipped,
           "at":steve_store::now()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_requires_a_scratch_session_and_node_policy_not_just_a_model_name() {
        let mut cap: HarnessCapability = serde_json::from_value(json!({
            "name":"fixture", "version":"1", "sessions":true, "jobs":false,
            "models":["work"], "resume":true, "cancel":true, "permissions":true,
            "images":false, "enforcement":[],
            "adapter":{"title":"Fixture","status":"ready"}
        }))
        .unwrap();
        let mut choice = HarnessChoice {
            harness: "fixture".into(),
            model: Some("work".into()),
            effort: Some("high".into()),
        };
        let check = |cap: &HarnessCapability, choice: &HarnessChoice, mode| {
            compatible(Some(cap), choice, Some(mode))
        };
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("pending"));
        cap.adapter.as_mut().unwrap().session_auth_required = Some(true);
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("signin"));
        let a = cap.adapter.as_mut().unwrap();
        a.session_auth_required = None;
        a.session = Some(serde_json::from_value(json!({
            "modes":["ask","full_access"], "models":[{"id":"work"}], "default_model":"work", "efforts":["high"]
        })).unwrap());
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("pending"));
        cap.adapter.as_mut().unwrap().execution_modes = Some(vec![PermissionMode::Ask]);
        assert_eq!(
            check(&cap, &choice, PermissionMode::FullAccess),
            Some("permission")
        );
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), None);
        choice.model = Some("different-subscription".into());
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("model"));
        choice.model = None;
        choice.effort = Some("max".into());
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("effort"));
        choice.effort = None;
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), None);
    }
}
