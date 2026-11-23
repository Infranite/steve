//! Strict placement for new sessions only. The legacy router is untouched when neither a
//! workspace Bot binding nor a Team applies. Existing sessions keep their durable Node.
use serde_json::{Value, json};
use steve_core::{CoreError, Result};
use steve_node_api::{
    AdapterStatus, HarnessCapability, HarnessChoice, NodeId, PermissionMode, SetupState,
};
use steve_store::{
    Bot, Read, Store, StoreError, bot_admin,
    bot_dispatch::{self, Scope},
    node_directory,
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
        "local" => steve_i18n::t!("hub.dispatch.local"),
        _ => steve_i18n::t!("hub.dispatch.bind_failed"),
    }
}

/// Why `node` cannot be shown as a verified candidate for `choice`. `pending`: nothing says it
/// cannot run it (no current report, or one that does not say), which does not keep a session
/// from it ([`blocks`]); every other reason is a reported mismatch.
fn compatible(
    cap: Option<&HarnessCapability>,
    choice: &HarnessChoice,
    mode: Option<PermissionMode>,
) -> Option<Reason> {
    let Some(cap) = cap else {
        return Some("harness");
    };
    match cap.setup.as_ref().map(|s| s.state) {
        Some(SetupState::Unknown) => return Some("pending"),
        Some(SetupState::Installing | SetupState::NotInstalled) => return Some("harness"),
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
        // an adapter that lists no models does not say which it takes
        if offer.models.is_empty() {
            return Some("pending");
        }
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

/// Structural constraints shared by preflight and the binding transaction: authorization, the
/// project's machine, the item's tag, and the Node the item's other sessions are bound to.
pub(super) fn node_blocker(
    store: &Store,
    c: &mut impl Read,
    ws: &str,
    item: &str,
    session: Option<&str>,
    node: &str,
) -> steve_store::Result<Option<Reason>> {
    if node != "local" {
        if !node_binding::node_takes(c, node, ws)? {
            return Ok(Some("unauthorized"));
        }
        let project = bot_admin::project_attached(c, item, ws)?;
        if project {
            return Ok(Some("project"));
        }
    }
    if let Some(tag) = store.reader(|db| steve_store::nodes::item_tag(db, item))?
        && !store
            .reader(|db| steve_store::nodes::with_tag(db, &tag))?
            .iter()
            .any(|n| n == node)
    {
        return Ok(Some("tag"));
    }
    let other = bot_admin::wrong_node(c, item, session, node)?;
    if other {
        return Ok(Some("affinity"));
    }
    Ok(None)
}

/// Authorization is held through the same SQLite transaction that fixes the Node. Reject a
/// legacy decision raced by an opt-in, and reject a strict decision raced by a revocation.
pub(super) fn commit_guard(
    store: &Store,
    c: &mut impl Read,
    session: &str,
    node: &str,
) -> steve_store::Result<()> {
    let Some(row) = steve_store::node_sessions::authority::session(c, session)? else {
        return Ok(());
    };
    let Some(item) = row.item else { return Ok(()) };
    let ws = row.workspace;
    let bot = row.bot.unwrap_or_else(|| "steve".into());
    match store.reader(|db| bot_dispatch::scope(db, &ws, &item, &bot))? {
        Scope::Nodes(nodes) if nodes.iter().any(|n| n == node) => {
            let active = bot_admin::selectable(c, &ws, &bot, true)?;
            if !active {
                return Err(StoreError::Conflict(text("bot")));
            }
            if let Some(why) = node_blocker(store, c, &ws, &item, Some(session), node)? {
                return Err(StoreError::Conflict(text(why)));
            }
            Ok(())
        }
        _ => Err(StoreError::Conflict(text("bind_failed"))),
    }
}

/// What keeps a session from a Node: a reported mismatch, never a report that is missing or says
/// nothing (#234). A newly bound Node still needs a verified report ([`compatible`]).
fn blocks(reason: Option<Reason>) -> Option<Reason> {
    reason.filter(|r| *r != "pending")
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
        let (bindings, names, configured) = self.store.read(|r| {
            let bindings = self
                .store
                .reader(|db| bot_dispatch::bindings(db, ws, &bot.id))?;
            let mut names = vec![("local".to_string(), None)];
            names.extend(
                node_directory::workspace_candidates(r, ws)?
                    .into_iter()
                    .map(|n| (n.id, Some(n.name))),
            );
            // Stale bindings must remain removable, without leaking a formerly authorized Node's
            // name or capability report once its grant has disappeared.
            let visible: Vec<String> = names.iter().map(|(id, _)| id.clone()).collect();
            let configured: Vec<_> = bindings
                .iter()
                .filter(|id| visible.contains(id))
                .cloned()
                .collect();
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
        let hidden = bindings.len() - configured.len();
        Ok(json!({"bindings": configured,"unavailable_bindings": hidden,"candidates": candidates}))
    }

    /// Preflight before the Core prepares any worktree or stops a session for a handoff: some Node
    /// of the Bot's could take the item now.
    pub(super) fn check_dispatch(&self, item: &str, bot: &str) -> Result<()> {
        let row = self
            .store
            .items()
            .get(item)?
            .ok_or(StoreError::NotFound("Item not found"))?;
        let scope = self
            .store
            .reader(|c| bot_dispatch::scope(c, &row.workspace_id, item, bot))?;
        let nodes = match scope {
            Scope::Nodes(nodes) => nodes,
            Scope::OutsideTeam => return Err(CoreError::Msg(text("outside_team"))),
        };
        let available = self.store.bots().get(bot)?.filter(|b| b.enabled);
        // the main Bot works in every workspace
        let in_workspace = available.as_ref().is_some_and(|b| b.main)
            || self.store.bots().in_workspace(bot, &row.workspace_id)?;
        let mut rejected = Vec::new();
        if let Some(b) = available.filter(|_| in_workspace) {
            let choice = self.bot_choice(&row.workspace_id, &b);
            for n in nodes {
                let why = self
                    .store
                    .read(|c| node_blocker(&self.store, c, &row.workspace_id, item, None, &n))?;
                // Local mode can be widened by the item's policy at actual bind time. Do not
                // reject a mode here that may be made compatible then. The bind checks it exactly.
                let report = self.report(&n, &choice, Some(&b.perm_mode)).0;
                match why.or(blocks(report).filter(|r| *r != "permission")) {
                    None => return Ok(()),
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

    /// true: a session of an item, bound or refused; false: a session of no item.
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
            Scope::OutsideTeam => {
                self.save_dispatch(item, bot, Some(session), None, "outside_team", &[])?;
                self.refused_with_node_on();
                return Ok(true);
            }
            Scope::Nodes(nodes) => {
                let choice = HarnessChoice {
                    harness: s.harness.clone(),
                    model: s.model.clone(),
                    effort: s.reasoning_effort.clone(),
                };
                let mode = self.perm_mode(session);
                let mut rejected = Vec::new();
                for (i, node) in nodes.iter().enumerate() {
                    let why = self.store.read(|c| {
                        node_blocker(&self.store, c, &s.workspace_id, item, Some(session), node)
                    })?;
                    if let Some(why) = why {
                        rejected.push(why);
                        continue;
                    }
                    // the plan first: the local Node's own refusal says the most
                    let planned = if node == "local" {
                        match (self.node.as_ref(), self.local_plan(session)) {
                            (Some(rt), Ok(plan)) => Ok((rt.clone(), plan)),
                            // why is said from the plan when the refusal is read
                            (Some(_), Err(_)) => Err("local"),
                            (None, _) => Err("offline"),
                        }
                    } else {
                        // Authorization, tag and same-Node affinity checked above and again in
                        // bind_after_scoped. No Hub directory is translated to the remote.
                        match node_binding::scoped_remote_plan(
                            &self.store,
                            session,
                            mode.as_deref(),
                        ) {
                            None => Err("project"),
                            Some(plan) => NodeId::new(node.clone())
                                .ok()
                                .and_then(|id| self.fleet.as_ref()?.start(&id))
                                .map(|rt| (rt, plan))
                                .ok_or("offline"),
                        }
                    };
                    let (rt, plan) = match planned {
                        Ok(p) => p,
                        Err(why) => {
                            rejected.push(why);
                            continue;
                        }
                    };
                    // the plan names the same harness choice, and the Node reports no mismatch
                    if let Some(why) = (plan.choice != choice)
                        .then_some("bind_failed")
                        .or_else(|| blocks(self.report(node, &choice, mode.as_deref()).0))
                    {
                        rejected.push(why);
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
                self.refused_with_node_on();
            }
        }
        Ok(true)
    }

    /// A new session bound nowhere although the local Node is on: counted on it.
    fn refused_with_node_on(&self) {
        if let Some(n) = self.node.as_ref().filter(|n| !n.is_stopped()) {
            n.fell_back();
        }
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
        self.store.write(|w| {
            steve_store::node_write::record_placement(
                w,
                item,
                &placement(bot, session, node, reason, rejected),
            )?;
            let row = steve_store::node_sessions::authority::item(w, item)?
                .ok_or(StoreError::NotFound("Item"))?;
            w.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: row.workspace.into(),
                    id: item.into(),
                },
            ));
            Ok::<_, StoreError>(())
        })?;
        Ok(())
    }

    pub(super) fn dispatch_refusal(&self, session: &str) -> Option<String> {
        let s = self.store.sessions().get(session).ok()??;
        let item = s.item_id?;
        let reason = self
            .store
            .reader(|c| {
                let r = bot_dispatch::last_decision(c, &item)?;
                Ok::<_, StoreError>(
                    r.filter(|v| v["session"] == session && v["node"].is_null())
                        .and_then(|v| v["reason"].as_str().map(str::to_string)),
                )
            })
            .ok()??;
        Some(steve_i18n::stored(|| match reason.as_str() {
            "local" => match self
                .local_plan(session)
                .err()
                .and_then(super::core::unbound_why)
            {
                Some(why) => steve_i18n::t!("hub.node.refused", why = why),
                None => text("local"),
            },
            r => text(r),
        }))
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
        // an adapter listing no models cannot verify one, nor refuse it
        let offer = cap.adapter.as_mut().unwrap().session.as_mut().unwrap();
        offer.models.clear();
        choice.model = Some("work".into());
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), Some("pending"));
        // what is only unverified does not keep a session from a Node; a mismatch does
        assert_eq!(blocks(Some("pending")), None);
        assert_eq!(blocks(Some("model")), Some("model"));
    }
}
