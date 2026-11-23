//! The preflight that says, before anything is created, whether a principal's Bot can run at all.
//!
//! A harness on a Node signs in by itself there (its own login command and settings); every
//! principal's session on that Node uses that sign-in ([`steve_harness_spec::Login`]), and Steve never
//! configures a model provider or handles a provider key. So what can stop a Bot is only its
//! harness: not installed, not ready, or not signed in on any Node that could take it. Each Node's
//! probe opens a scratch session of every harness it has; one that is not signed in cannot open it
//! and is reported not ready.
//!
//! [`Preflight::can_run`] is the deterministic check `can_run(principal, bot, harness)`. It runs
//! when a schedule is created or edited and when a Bot is given an item or a schedule (refused,
//! saying why and where to fix it), when a schedule fires (no item is made; the run records the
//! reason), and before a session is made (no session; one clear error on the item, no retry). It
//! reads only the database and what the Nodes last reported; it decrypts nothing and calls no
//! model. A Node that is linked but has not answered yet leaves its harnesses unknown, never
//! refused (#234).

use std::sync::Arc;

use serde::Serialize;
use steve_store::Read;

use super::principal::{self, Principal};

/// Why a principal's Bot cannot run now, and where that is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocked {
    /// `harness_not_ready`: not installed, not ready or not signed in on any Node.
    pub code: &'static str,
    pub bot: String,
    pub harness: String,
    /// In the request's language (built under `steve_i18n::stored` where it is stored).
    pub reason: String,
    /// Where it is fixed: `devices` (install or sign the harness in on a device).
    pub fix: &'static str,
}

/// The harness ids the Hub knows, with their names and whether a Node could take them (`ready`):
/// `Some(false)` only when every Node has answered and none has it ready, `None` while unknown
/// ([`crate::harnesses::HubHarnesses::readiness`]).
pub type Readiness = Arc<dyn Fn() -> Vec<(String, String, Option<bool>)> + Send + Sync>;

/// What [`Preflight::can_run`] decides on: the Nodes' last readiness reports.
pub struct Preflight {
    readiness: Option<Readiness>,
}

impl Preflight {
    pub fn new(readiness: Option<Readiness>) -> Arc<Preflight> {
        Arc::new(Preflight { readiness })
    }

    fn ready(&self, harness: &str) -> Option<bool> {
        let r = self.readiness.as_ref()?;
        r().into_iter()
            .find(|(id, _, _)| id.as_str().eq(harness))
            .and_then(|(_, _, ready)| ready)
    }

    /// `can_run(principal, bot, harness)` in `c`'s view. A Bot that is not there is not refused
    /// here (the callers' own checks say that).
    pub fn can_run(&self, c: &mut impl Read, p: &Principal, bot: &str) -> Result<(), Blocked> {
        let harness = steve_store::BotRows(c).harness(bot).ok().flatten();
        let Some(harness) = harness else {
            return Ok(());
        };
        self.can_run_harness(c, p, bot, &harness)
    }

    /// [`Self::can_run`] with the harness known. The principal does not change the answer: every
    /// principal's session on a Node uses the harness's sign-in there.
    pub fn can_run_harness(
        &self,
        _c: &mut impl Read,
        _p: &Principal,
        bot: &str,
        harness: &str,
    ) -> Result<(), Blocked> {
        if self.ready(harness) == Some(false) {
            return Err(Blocked {
                code: "harness_not_ready",
                bot: bot.to_string(),
                harness: harness.to_string(),
                reason: steve_i18n::t!("hub.preflight.harness_not_ready", harness = harness),
                fix: "devices",
            });
        }
        Ok(())
    }

    /// [`Self::can_run_harness`] for `bot` given `item`: its principal is the item's.
    pub fn can_run_item(
        &self,
        c: &mut impl Read,
        item: &str,
        bot: &str,
        harness: &str,
    ) -> Result<(), Blocked> {
        match principal::of_item(c, item) {
            Ok(Some((_, p))) => self.can_run_harness(c, &p, bot, harness),
            _ => Ok(()),
        }
    }

    /// For a schedule: its principal is the named requester while a member, else the service one.
    pub fn can_run_schedule(
        &self,
        c: &mut impl Read,
        ws: &str,
        requester: Option<&str>,
        bot: &str,
    ) -> Result<(), Blocked> {
        match principal::resolve(c, ws, requester) {
            Ok(p) => self.can_run(c, &p, bot),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
