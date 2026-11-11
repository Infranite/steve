//! The first meeting: after a fresh installation Steve and the instance's owner get to know each
//! other in the main conversation, and Steve offers to fill in the owner's profile, pick the models
//! and create Bots for what they do. The talking is the research run's; what does not depend on a
//! model is here: the welcome, the state, and the end.

use super::MAIN;
use crate::Core;
use crate::error::Result;
use serde_json::json;

/// Workspace setting: empty (never met), [`PENDING`] or [`DONE`]. The web reads it from `/api/state`.
pub const STATE: &str = "onboard";
/// Workspace setting: the welcome message the meeting began with.
pub const FROM: &str = "onboardFrom";
pub const PENDING: &str = "pending";
pub const DONE: &str = "done";
/// How many of the owner's messages a meeting lasts at most, so one nobody finished does not keep
/// the router skipped.
pub const TURNS: usize = 12;
/// How a meeting ended, as its audit entry (`onboard.end`) says it: Steve called it done, the owner
/// skipped it, or its turns ran out.
pub const ENDED: &str = "done";
pub const SKIPPED: &str = "skip";
pub const RAN_OUT: &str = "turns";
/// What the research run is told while the meeting is on.
pub const BRIEF: &str = include_str!("onboard.txt");

impl Core {
    /// The first meeting is on.
    pub fn onboard_pending(&self, ws: &str) -> Result<bool> {
        Ok(self.store().setting(ws, STATE, "")? == PENDING)
    }

    /// Begin the first meeting: Steve says hello in the main conversation, in the language of the
    /// request. Returns the welcome message; one that is already on is left as it is.
    pub fn onboard_start(&self, ws: &str, actor: &str) -> Result<String> {
        let db = self.store();
        if self.meeting(ws)? {
            return db.setting(ws, FROM, "");
        }
        let id = self.post(
            ws,
            MAIN,
            &steve_i18n::t!("core.onboard.welcome"),
            None,
            Some(&json!({"onboard": true})),
        )?;
        self.onboard_set(ws, PENDING, &id, actor)?;
        self.store()
            .audit(actor, "onboard.start", ws, &json!({}), ws)?;
        Ok(id)
    }

    /// End the first meeting; `how` is [`ENDED`], [`SKIPPED`] or [`RAN_OUT`]. Ending one that is
    /// over changes nothing, and skipping one that never began leaves no entry of a meeting.
    pub fn onboard_end(&self, ws: &str, actor: &str, how: &str) -> Result<()> {
        let was = self.store().setting(ws, STATE, "")?;
        if was == DONE {
            return Ok(());
        }
        self.onboard_set(ws, DONE, "", actor)?;
        let action = if was == PENDING {
            "onboard.end"
        } else {
            "onboard.skip"
        };
        self.store()
            .audit(actor, action, ws, &json!({"how": how}), ws)
    }

    fn onboard_set(&self, ws: &str, state: &str, from: &str, actor: &str) -> Result<()> {
        let p = json!({STATE: state, FROM: from});
        self.store()
            .settings_set(ws, p.as_object().expect("an object"), actor)
    }

    /// The first meeting is on and has turns left; one that ran out ([`TURNS`]) ends here.
    pub(crate) fn meeting(&self, ws: &str) -> Result<bool> {
        if !self.onboard_pending(ws)? {
            return Ok(false);
        }
        let from = self.store().setting(ws, FROM, "")?;
        let msgs = self.conv_messages(ws, MAIN)?;
        let said = msgs
            .iter()
            .position(|m| m.id == from)
            .map(|at| msgs[at..].iter().filter(|m| m.author == "me").count());
        if said.is_some_and(|n| n <= TURNS) {
            return Ok(true);
        }
        self.onboard_end(ws, "steve", RAN_OUT)?;
        Ok(false)
    }

    /// Whether the run answering message `mid` (asked by `user`) may use the first meeting's tools:
    /// the owner's own main conversation, while the meeting is on. A message that is not there (or
    /// is another workspace's) opens nothing.
    pub fn onboard_allows(&self, ws: &str, mid: &str, user: Option<&str>) -> Result<bool> {
        let here = self
            .store()
            .message(mid)?
            .is_some_and(|m| m.workspace_id == ws && m.space == MAIN);
        Ok(here
            && super::research::asked_by_owner(self.store(), user)
            && self.onboard_pending(ws)?)
    }
}
