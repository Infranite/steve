//! A blocked automatic request can propose a Bot on a verified route. Confirmation is a
//! workspace-owner command that creates the Bot and its reviewed grants in one transaction.
use steve_core::CreateItem;
use steve_node_api::HarnessChoice;
use steve_store::{Members, NewBot, Read, StoreError, bot_admin, bot_dispatch};
use steve_types::{
    BotProposal, ConfigEvent, ConfirmBotProposal, PlacementBlocked, PlacementRejection as Reason,
    ProposalTeam,
};

use super::core::RoutedRuntime;
use crate::error::ApiError;

fn owns(r: &mut impl Read, ws: &str, actor: &str) -> steve_store::Result<bool> {
    Ok(Members::new(r).role(ws, actor)?.as_deref() == Some("owner"))
}

fn storage(error: StoreError) -> ApiError {
    tracing::error!("Bot proposal: {error}");
    ApiError::internal(steve_i18n::t!("hub.error.generic"))
}

impl RoutedRuntime {
    pub(super) fn propose_bot(
        &self,
        n: &CreateItem,
        blocked: &PlacementBlocked,
    ) -> steve_store::Result<Option<BotProposal>> {
        let Some(actor) = n.actor.as_deref().or(n.requester.as_deref()) else {
            return Ok(None);
        };
        if n.bot.as_deref().is_some_and(|id| !id.is_empty())
            || !matches!(blocked.reason, Reason::NoCandidates | Reason::Specialty)
        {
            return Ok(None);
        }
        let Some(harnesses) = &self.dispatch_harnesses else {
            return Ok(None);
        };
        self.store.write(|w| {
            if !owns(w, &n.ws, actor)? {
                return Ok(None);
            }
            let node = n.node.as_deref().unwrap_or("local");
            let project_id = n.project.as_ref().and_then(|p| p.as_deref());
            let project = project_id
                .map(|p| self.store.projects().get(p))
                .transpose()?
                .flatten();
            if project_id.is_some() && project.is_none() {
                return Ok(None);
            }
            // A proposal starts in Ask mode. Only a current positive report may support it.
            let mut options = harnesses.for_node(node);
            options.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            let Some(choice) = options
                .into_iter()
                .map(|h| HarnessChoice {
                    harness: h.id,
                    model: h
                        .default_model
                        .filter(|id| h.models.iter().any(|m| &m.id == id)),
                    effort: None,
                })
                .find(|choice| self.verified_proposal_route(&n.ws, project.as_ref(), node, choice))
            else {
                return Ok(None);
            };
            let team = bot_dispatch::selected(w, &n.ws, n.team.as_deref())?.map(|t| ProposalTeam {
                id: t.id,
                name: t.name,
            });
            let draft = BotProposal {
                id: steve_store::uid("bp"),
                revision: 1,
                name: n
                    .required_specialties
                    .first()
                    .unwrap_or(&n.title)
                    .chars()
                    .take(40)
                    .collect(),
                role: n.title.chars().take(200).collect(),
                specialties: bot_dispatch::specialties(n.required_specialties.clone()),
                harness: choice.harness,
                model: choice.model,
                node: node.into(),
                team,
            };
            bot_dispatch::save_proposal(w, &n.ws, actor, project_id, &draft)?;
            Ok(Some(draft))
        })
    }

    fn verified_proposal_route(
        &self,
        ws: &str,
        project: Option<&steve_store::Project>,
        node: &str,
        choice: &HarnessChoice,
    ) -> bool {
        matches!(
            self.assess_node(ws, project, node, choice, "default"),
            Ok(None)
        ) && self.report(node, choice, Some("default")).0.is_none()
    }

    pub(super) fn confirm_bot_proposal(
        &self,
        ws: &str,
        actor: &str,
        id: &str,
        edit: ConfirmBotProposal,
    ) -> Result<String, ApiError> {
        self.store
            .write(|w| {
                if !owns(w, ws, actor)? {
                    return Err(ConfirmError::Api(ApiError::Json(
                        403,
                        steve_i18n::t!("hub.dispatch.owner_only"),
                    )));
                }
                let Some(proposal) = bot_dispatch::proposal(w, ws, actor, id)? else {
                    return Err(ConfirmError::Api(ApiError::not_found(steve_i18n::t!(
                        "hub.dispatch.proposal_missing"
                    ))));
                };
                let draft = proposal.draft;
                if edit.expected_revision != draft.revision {
                    return Err(ConfirmError::Api(ApiError::Json(
                        409,
                        steve_i18n::t!("hub.dispatch.proposal_stale"),
                    )));
                }
                if let Some(bot) = proposal.confirmed_bot {
                    return Ok(bot);
                }
                let mut team = draft
                    .team
                    .map(|t| bot_dispatch::selected(w, ws, Some(&t.id)))
                    .transpose()
                    .map_err(|error| match error {
                        StoreError::NotFound(_) => ConfirmError::Api(ApiError::Json(
                            409,
                            steve_i18n::t!("hub.dispatch.proposal_stale"),
                        )),
                        other => ConfirmError::Store(other),
                    })?
                    .flatten();
                let project = proposal
                    .project
                    .as_deref()
                    .map(|p| self.store.projects().get(p))
                    .transpose()?
                    .flatten();
                let choice = HarnessChoice {
                    harness: edit.harness,
                    model: edit.model,
                    effort: None,
                };
                let node = draft.node.as_str();
                if (proposal.project.is_some() && project.is_none())
                    || !self.verified_proposal_route(ws, project.as_ref(), node, &choice)
                {
                    return Err(ConfirmError::Api(ApiError::Json(
                        409,
                        steve_i18n::t!("hub.dispatch.not_candidate"),
                    )));
                }
                let bot = self
                    .store
                    .bots()
                    .create(
                        ws,
                        &NewBot {
                            name: edit.name.trim().to_owned(),
                            role: Some(edit.role),
                            specialties: Some(bot_dispatch::specialties(edit.specialties)),
                            harness: choice.harness,
                            model: choice.model,
                            perm_mode: Some("default".into()),
                            ..Default::default()
                        },
                        actor,
                    )?
                    .ok_or(StoreError::NotFound("Created Bot"))?;
                bot_admin::set_bindings(w, ws, &bot.id, &[node.into()])?;
                if let Some(team) = &mut team {
                    team.members.push(bot_dispatch::Member {
                        bot: bot.id.clone(),
                        nodes: None,
                    });
                    bot_admin::save(w, ws, Some(&team.id), &team.name, &team.members)?;
                    w.emit(ConfigEvent::Settings {
                        ws: ws.into(),
                        id: ws.into(),
                    });
                }
                bot_dispatch::confirm(w, id, &bot.id)?;
                Ok(bot.id)
            })
            .map_err(|e| match e {
                ConfirmError::Api(e) => e,
                ConfirmError::Store(e) => storage(e),
            })
    }
}

enum ConfirmError {
    Api(ApiError),
    Store(StoreError),
}

impl From<StoreError> for ConfirmError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
