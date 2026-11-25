//! Assess ordered Bot bindings against current authority and Node capabilities.
use steve_core::{CoreError, Result};
use steve_node_api::{
    AdapterStatus, HarnessCapability, HarnessChoice, NodeId, PermissionMode, SetupState,
};
use steve_store::{
    Bot, BotRows, Read, StoreError, bot_admin,
    bot_dispatch::{self, Scope},
    node_directory,
};

use super::{core::RoutedRuntime, node_binding};
use crate::backend::{BotNodeCandidate, BotNodeCandidates, BotNodeState};

use steve_types::{
    BotPlacement, Millis, PlacementBlocked, PlacementBlocker, PlacementExplanation,
    PlacementReason, PlacementRejection as Reason,
};

pub(crate) fn text(reason: Reason) -> String {
    match reason {
        Reason::NoCandidates => steve_i18n::t!("hub.dispatch.no_candidates"),
        Reason::OutsideTeam => steve_i18n::t!("hub.dispatch.outside_team"),
        Reason::Bot => steve_i18n::t!("hub.dispatch.bot"),
        Reason::Specialty => steve_i18n::t!("hub.dispatch.specialty"),
        Reason::NoBindings => steve_i18n::t!("hub.dispatch.no_bindings"),
        Reason::Unauthorized => steve_i18n::t!("hub.dispatch.unauthorized"),
        Reason::Project => steve_i18n::t!("hub.dispatch.project"),
        Reason::Affinity => steve_i18n::t!("hub.dispatch.affinity"),
        Reason::Tag => steve_i18n::t!("hub.dispatch.tag"),
        Reason::Offline => steve_i18n::t!("hub.dispatch.offline"),
        Reason::Harness => steve_i18n::t!("hub.dispatch.harness"),
        Reason::SignIn => steve_i18n::t!("hub.dispatch.signin"),
        Reason::SessionProbe => steve_i18n::t!("hub.dispatch.session_probe"),
        Reason::Model => steve_i18n::t!("hub.dispatch.model"),
        Reason::Effort => steve_i18n::t!("hub.dispatch.effort"),
        Reason::Permission => steve_i18n::t!("hub.dispatch.permission"),
        Reason::Pending => steve_i18n::t!("hub.dispatch.pending"),
        Reason::Local => steve_i18n::t!("hub.dispatch.local"),
        Reason::BindFailed => steve_i18n::t!("hub.dispatch.bind_failed"),
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
        return Some(Reason::Harness);
    };
    match cap.setup.as_ref().map(|s| s.state) {
        Some(SetupState::Unknown) => return Some(Reason::Pending),
        Some(SetupState::Installing | SetupState::NotInstalled) => return Some(Reason::Harness),
        _ => {}
    }
    let Some(adapter) = &cap.adapter else {
        return Some(Reason::Pending);
    };
    if adapter.status != AdapterStatus::Ready {
        return Some(Reason::Harness);
    }
    if adapter.session_auth_required == Some(true) {
        return Some(Reason::SignIn);
    }
    let Some(offer) = &adapter.session else {
        return Some(if adapter.session_error.is_some() {
            Reason::SessionProbe
        } else {
            Reason::Pending
        });
    };
    if cap
        .setup
        .as_ref()
        .is_some_and(|s| s.state != SetupState::Ready)
    {
        return Some(Reason::SessionProbe);
    }
    if let Some(model) = &choice.model {
        if offer.models.is_empty() {
            return Some(Reason::Pending);
        }
        if !offer.models.iter().any(|m| &m.id == model) {
            return Some(Reason::Model);
        }
    } else if offer.default_model.as_ref().is_none_or(|m| m.is_empty()) {
        // The actual default of an opaque offer is not known.
        return Some(Reason::Pending);
    }
    if choice
        .effort
        .as_ref()
        .is_some_and(|e| !offer.efforts.contains(e))
    {
        return Some(Reason::Effort);
    }
    let Some(modes) = &adapter.execution_modes else {
        return Some(Reason::Pending);
    };
    if mode.is_none_or(|mode| !modes.contains(&mode) || !offer.modes.contains(&mode)) {
        return Some(Reason::Permission);
    }
    None
}

/// Structural constraints shared by preflight and the binding transaction: authorization, the
/// project's machine, the item's tag, and the Node the item's other sessions are bound to.
fn node_blocker(
    c: &mut impl Read,
    ws: &str,
    item: &str,
    session: Option<&str>,
    node: &str,
) -> steve_store::Result<Option<Reason>> {
    if node != "local" {
        if !node_binding::node_takes(c, node, ws)? {
            return Ok(Some(Reason::Unauthorized));
        }
        let project = bot_admin::project_attached(c, item, ws)?;
        if project {
            return Ok(Some(Reason::Project));
        }
    }
    item_node_blocker(c, item, session, node)
}

fn item_node_blocker(
    c: &mut impl Read,
    item: &str,
    session: Option<&str>,
    node: &str,
) -> steve_store::Result<Option<Reason>> {
    if let Some(tag) = steve_store::nodes::item_tag(c, item)?
        && !steve_store::nodes::with_tag(c, &tag)?
            .iter()
            .any(|n| n == node)
    {
        return Ok(Some(Reason::Tag));
    }
    let other = bot_admin::wrong_node(c, item, session, node)?;
    if other {
        return Ok(Some(Reason::Affinity));
    }
    Ok(None)
}

/// Authorization stays current through the transaction that fixes the session Node.
pub(super) fn commit_guard(
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
    let required = bot_dispatch::initial(c, &item)?.and_then(|p| p.required_node);
    if required.as_ref().is_some_and(|id| id.as_str() != node) {
        return Err(StoreError::Conflict(text(Reason::Unauthorized)));
    }
    match bot_dispatch::scope(c, &ws, &item, &bot)? {
        Scope::Nodes(nodes) if nodes.iter().any(|n| n == node) => {
            let active = BotRows(c)
                .local(&ws, &bot)?
                .is_some_and(|(_, enabled)| enabled);
            if !active {
                return Err(StoreError::Conflict(text(Reason::Bot)));
            }
            if let Some(why) = node_blocker(c, &ws, &item, Some(session), node)? {
                return Err(StoreError::Conflict(text(why)));
            }
            Ok(())
        }
        _ => Err(StoreError::Conflict(text(Reason::BindFailed))),
    }
}

/// What keeps a session from a Node: a reported mismatch, never a report that is missing or says
/// nothing (#234). A newly bound Node still needs a verified report ([`compatible`]).
fn blocks(reason: Option<Reason>) -> Option<Reason> {
    reason.filter(|r| *r != Reason::Pending)
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

    pub(super) fn report(
        &self,
        node: &str,
        choice: &HarnessChoice,
        requested_mode: Option<&str>,
    ) -> (Option<Reason>, Option<i64>) {
        let Some(generation) = self.dispatch_probe_target(node) else {
            return (Some(Reason::Offline), None);
        };
        let Some((at, cap)) = self
            .dispatch_harnesses
            .as_ref()
            .and_then(|h| h.target_report(node, generation, &choice.harness))
        else {
            return (Some(Reason::Pending), None);
        };
        let mode = steve_node_api::PermissionMode::from_key(requested_mode.unwrap_or("default"));
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

    pub(super) fn bot_candidates(&self, ws: &str, bot: &str) -> Result<BotNodeCandidates> {
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
            let bindings = bot_dispatch::bindings(r, ws, &bot.id)?;
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
        let candidates = names
            .into_iter()
            .map(|(id, name)| {
                let (reason, checked_at) = self.report(&id, &choice, Some(&bot.perm_mode));
                let state = match reason {
                    None => BotNodeState::Candidate,
                    Some(Reason::Pending) => BotNodeState::Pending,
                    _ => BotNodeState::Unavailable,
                };
                BotNodeCandidate {
                    id,
                    name,
                    state,
                    reason,
                    checked_at,
                    provider_verified: false,
                }
            })
            .collect();
        Ok(BotNodeCandidates {
            unavailable_bindings: bindings.len() - configured.len(),
            bindings: configured,
            candidates,
        })
    }

    pub(super) fn assess_node(
        &self,
        ws: &str,
        project: Option<&steve_store::Project>,
        node: &str,
        choice: &HarnessChoice,
        mode: &str,
    ) -> Result<Option<Reason>> {
        if steve_node_api::PermissionMode::from_key(mode).is_none() {
            return Ok(Some(Reason::Permission));
        }
        if node != "local" && !self.store.read(|r| node_binding::node_takes(r, node, ws))? {
            return Ok(Some(Reason::Unauthorized));
        }
        if project.is_some_and(|p| {
            p.archived
                || p.workspace_id != ws
                || p.device_id != super::workspaces::HUB_DEVICE
                || node != "local"
                || node_binding::root_id(std::path::Path::new(&p.path)).is_none()
        }) {
            return Ok(Some(Reason::Project));
        }
        let reason = self.report(node, choice, Some(mode)).0;
        // The Hub supervises its own Node. While its coordinator catches up, a binding is
        // durable and its commands wait in the ledger until the Node can take them.
        if reason == Some(Reason::Offline)
            && node == "local"
            && self.host.local_node.get().is_some()
            && self
                .node
                .as_ref()
                .is_some_and(|n| !n.is_stopped() && !n.is_reconciled())
        {
            return Ok(None);
        }
        Ok(blocks(reason))
    }

    fn item_routes(&self, item: &steve_store::Item, bot: &str) -> Result<Routes> {
        let (scope, initial) = self.store.read(|r| {
            Ok::<_, StoreError>((
                bot_dispatch::scope(r, &item.workspace_id, &item.id, bot)?,
                bot_dispatch::initial(r, &item.id)?,
            ))
        })?;
        let Scope::Nodes(nodes) = scope else {
            return Err(blocked(bot, &[Reason::OutsideTeam]));
        };
        let active = self
            .store
            .read(|r| BotRows(r).local(&item.workspace_id, bot))?
            .is_some_and(|(_, enabled)| enabled);
        let bot = self
            .store
            .bots()
            .get(bot)?
            .filter(|_| active)
            .ok_or_else(|| blocked(bot, &[Reason::Bot]))?;
        let project = item
            .project_id
            .as_deref()
            .map(|id| self.store.projects().get(id))
            .transpose()?
            .flatten();
        let nodes = nodes
            .into_iter()
            .enumerate()
            .filter(|(_, node)| {
                initial
                    .as_ref()
                    .and_then(|p| p.required_node.as_ref())
                    .is_none_or(|id| id.as_str() == node)
            })
            .collect();
        Ok(Routes {
            bot,
            initial,
            project,
            nodes,
        })
    }

    fn route_blocker(&self, target: &Target<'_>, node: &str) -> Result<Option<Reason>> {
        if let Some(why) = self
            .store
            .read(|r| item_node_blocker(r, &target.item.id, target.session, node))?
        {
            return Ok(Some(why));
        }
        self.assess_node(
            &target.item.workspace_id,
            target.project,
            node,
            target.choice,
            target.mode,
        )
    }

    /// Admission before preparing a worktree or interrupting another session for a handoff.
    pub(super) fn check_dispatch(&self, item_id: &str, bot: &str) -> Result<()> {
        let item = self
            .store
            .items()
            .get(item_id)?
            .ok_or(StoreError::NotFound("Item"))?;
        let routes = self.item_routes(&item, bot)?;
        let choice = self.bot_choice(&item.workspace_id, &routes.bot);
        let target = Target {
            item: &item,
            session: None,
            project: routes.project.as_ref(),
            choice: &choice,
            mode: &routes.bot.perm_mode,
        };
        let mut rejected = Vec::new();
        for (_, node) in &routes.nodes {
            match self.route_blocker(&target, node)? {
                Some(why) => rejected.push(why),
                None => return Ok(()),
            }
        }
        Err(blocked(bot, &rejected))
    }

    /// Bind a new item session. The item selects its Bot only once, and a handoff names its Bot
    /// explicitly. The admitted Bot's current grants and Node constraints are checked again here.
    pub(super) fn bind_dispatch(&self, session: &str, after: Option<&str>) -> Result<bool> {
        let Some(s) = self.store.sessions().get(session)? else {
            return Ok(true);
        };
        let Some(item_id) = s.item_id.as_deref() else {
            return Ok(false);
        };
        let item = self
            .store
            .items()
            .get(item_id)?
            .ok_or(StoreError::NotFound("Item"))?;
        let error = match self.bind_item_session(&item, &s, after) {
            Ok(()) => return Ok(true),
            Err(CoreError::Placement(error)) => error,
            Err(other) => return Err(other),
        };
        self.store.write(|w| {
            bot_dispatch::record(w, &item.workspace_id, &item.id, Some(session), Err(&error))
        })?;
        if let Some(n) = self.node.as_ref().filter(|n| !n.is_stopped()) {
            n.fell_back();
        }
        Ok(true)
    }

    fn bind_item_session(
        &self,
        item: &steve_store::Item,
        s: &steve_store::Session,
        after: Option<&str>,
    ) -> Result<()> {
        let session = s.id.as_str();
        let bot = s.bot_id.as_deref().unwrap_or("steve");
        let routes = self.item_routes(item, bot)?;
        let choice = HarnessChoice {
            harness: s.harness.clone(),
            model: s.model.clone(),
            effort: s.reasoning_effort.clone(),
        };
        let mode = self.perm_mode(session);
        let target = Target {
            item,
            session: Some(session),
            project: routes.project.as_ref(),
            choice: &choice,
            mode: mode.as_deref().unwrap_or("default"),
        };
        let mut rejected = Vec::new();
        for (priority, node) in &routes.nodes {
            if let Some(why) = self.route_blocker(&target, node)? {
                rejected.push(why);
                continue;
            }
            let planned = if node == "local" {
                self.node.as_ref().ok_or(Reason::Offline).and_then(|node| {
                    self.local_plan(session)
                        .map(|plan| (node.clone(), plan))
                        .map_err(|_| Reason::Local)
                })
            } else {
                node_binding::scoped_remote_plan(&self.store, session, mode.as_deref())
                    .ok_or(Reason::Project)
                    .and_then(|plan| {
                        NodeId::new(node.clone())
                            .ok()
                            .and_then(|id| self.fleet.as_ref()?.start(&id))
                            .map(|rt| (rt, plan))
                            .ok_or(Reason::Offline)
                    })
            };
            let (rt, plan) = match planned {
                Ok(p) => p,
                Err(why) => {
                    rejected.push(why);
                    continue;
                }
            };
            if plan.choice != choice {
                rejected.push(Reason::BindFailed);
                continue;
            }
            let receipt = routes.placement(node, *priority, &choice, &rejected);
            match rt.bind_after_scoped(&plan, after, &receipt) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::info!(session, node, "scoped binding refused: {e}"),
            }
            rejected.push(Reason::BindFailed);
        }
        Err(blocked(bot, &rejected))
    }

    pub(super) fn dispatch_refusal(&self, session: &str) -> Option<String> {
        let error = self
            .store
            .read(|r| bot_dispatch::refusal(r, session))
            .ok()??;
        Some(steve_i18n::stored(|| match error.reason {
            Reason::Local => match self
                .local_plan(session)
                .err()
                .and_then(super::core::unbound_why)
            {
                Some(why) => steve_i18n::t!("hub.node.refused", why = why),
                None => text(Reason::Local),
            },
            reason => text(reason),
        }))
    }
}

struct Target<'a> {
    item: &'a steve_store::Item,
    session: Option<&'a str>,
    project: Option<&'a steve_store::Project>,
    choice: &'a HarnessChoice,
    mode: &'a str,
}

struct Routes {
    bot: Bot,
    initial: Option<BotPlacement>,
    project: Option<steve_store::Project>,
    nodes: Vec<(usize, String)>,
}

impl Routes {
    fn placement(
        &self,
        node: &str,
        priority: usize,
        choice: &HarnessChoice,
        rejected: &[Reason],
    ) -> BotPlacement {
        let mut explanation = self
            .initial
            .as_ref()
            .filter(|p| p.explanation.bot.as_str() == self.bot.id)
            .map(|p| p.explanation.clone())
            .unwrap_or_else(|| PlacementExplanation {
                bot: self.bot.id.clone().into(),
                reason: PlacementReason::ExistingAssignment,
                matched: vec![],
                rejected: vec![],
                fallback: None,
            });
        for reason in rejected {
            let blocker = PlacementBlocker {
                bot: Some(explanation.bot.clone()),
                reason: *reason,
            };
            if !explanation.rejected.contains(&blocker) {
                explanation.rejected.push(blocker);
            }
        }
        BotPlacement {
            rule_version: self.initial.as_ref().map_or(1, |p| p.rule_version),
            bot_revision: self.bot.revision,
            at: Millis(steve_store::now()),
            node: node.into(),
            required_node: self.initial.as_ref().and_then(|p| p.required_node.clone()),
            node_priority: priority as u32,
            harness: choice.harness.clone(),
            model: choice.model.clone(),
            reasoning_effort: choice.effort.clone(),
            explanation,
        }
    }
}

fn blocked(bot: &str, rejected: &[Reason]) -> CoreError {
    CoreError::Placement(PlacementBlocked {
        reason: rejected.first().copied().unwrap_or(Reason::NoBindings),
        rejected: rejected
            .iter()
            .map(|reason| PlacementBlocker {
                bot: Some(bot.into()),
                reason: *reason,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::Pending)
        );
        cap.adapter.as_mut().unwrap().session_auth_required = Some(true);
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::SignIn)
        );
        let a = cap.adapter.as_mut().unwrap();
        a.session_auth_required = None;
        a.session = Some(serde_json::from_value(json!({
            "modes":["ask","full_access"], "models":[{"id":"work"}], "default_model":"work", "efforts":["high"]
        })).unwrap());
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::Pending)
        );
        cap.adapter.as_mut().unwrap().execution_modes = Some(vec![PermissionMode::Ask]);
        assert_eq!(
            check(&cap, &choice, PermissionMode::FullAccess),
            Some(Reason::Permission)
        );
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), None);
        choice.model = Some("different-subscription".into());
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::Model)
        );
        choice.model = None;
        choice.effort = Some("max".into());
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::Effort)
        );
        choice.effort = None;
        assert_eq!(check(&cap, &choice, PermissionMode::Ask), None);
        // an adapter listing no models cannot verify one, nor refuse it
        let offer = cap.adapter.as_mut().unwrap().session.as_mut().unwrap();
        offer.models.clear();
        choice.model = Some("work".into());
        assert_eq!(
            check(&cap, &choice, PermissionMode::Ask),
            Some(Reason::Pending)
        );
        // what is only unverified does not keep a session from a Node; a mismatch does
        assert_eq!(blocks(Some(Reason::Pending)), None);
        assert_eq!(blocks(Some(Reason::Model)), Some(Reason::Model));
    }
}
