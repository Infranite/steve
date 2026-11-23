//! Successful tool receipts, committed with their business state. Transcript prose and refused
//! calls are never receipts. The Node's logical turn survives retries and Hub restarts; the input
//! sequence also prevents a receipt from a previous turn satisfying an unnamed turn end.

use serde::{Deserialize, Serialize};
use steve_store::Ingest;

use crate::repo::{Item, Session};
use crate::{Mark, Result, Running, Tx};

const RECEIPT: &str = "steve_tool_receipt";

#[derive(Serialize, Deserialize)]
struct Receipt {
    item: String,
    cycle: i64,
    scope: i64,
    input: i64,
    turn: Option<String>,
    attempt: Option<String>,
    revision: i64,
    asked: bool,
}

impl Tx<'_> {
    /// Only call after validation. Failed saves roll back both the receipt and business writes,
    /// including their notifications; their in-memory mark must not survive the rollback either.
    pub(crate) fn with_turn_receipt(
        &mut self,
        s: &Session,
        save: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let Some(item_id) = s.item_id.as_deref() else {
            return save(self);
        };
        let before = self.st.pending.get(&s.id).cloned();
        let hooks = self.st.hooks.len();
        let cancels = self.st.cancels.len();
        let store = self.db().store().clone();
        let result = store.write(|w| {
            let i = self.item(item_id)?;
            let facts = store.item_facts();
            let mut attempt = self.rt().running(&s.id).map(|r| r.attempt);
            let turn = match attempt.as_deref() {
                Some(a) => facts.turn_of_attempt(a)?,
                None => facts.latest_turn(&s.id)?.map(|t| t.turn_id),
            };
            if attempt.is_none() && turn.is_none() {
                attempt = store.sessions().stop(&s.id)?.and_then(|k| k.attempt);
            }
            let mut receipt = Receipt {
                item: i.id,
                cycle: i.cycle,
                scope: self.scope_revision(item_id)?,
                input: w.events().latest_input_seq(&s.id)?.unwrap_or_default(),
                turn,
                attempt,
                revision: i.revision,
                asked: false,
            };
            save(self)?;
            let mark = self.st.pending.get(&s.id).map(|(m, _)| *m);
            if matches!(mark, Some(Mark::Delivered | Mark::Asked)) {
                receipt.asked = mark == Some(Mark::Asked);
                receipt.revision = self.item(item_id)?.revision;
                w.events().add_local(
                    &s.id,
                    RECEIPT,
                    serde_json::to_value(&receipt)?,
                    steve_store::now(),
                )?;
            }
            Ok(())
        });
        if result.is_err() {
            // Acceptance/finish hooks and deferred stops must not escape a rolled-back save.
            self.st.hooks.truncate(hooks);
            self.st.cancels.truncate(cancels);
            match before {
                Some(m) => {
                    self.st.pending.insert(s.id.clone(), m);
                }
                None => {
                    self.st.pending.remove(&s.id);
                }
            }
        }
        result
    }

    /// A receipt of the current cycle, goal, input and logical turn, whose result still stands.
    /// `attempt` is the end being judged; `None` is used by the idle watch/continue checks.
    pub(crate) fn turn_receipt(
        &self,
        item: &Item,
        s: &Session,
        attempt: Option<&str>,
    ) -> Result<Option<Mark>> {
        let (input, data) = self.db().store().read::<_, steve_store::StoreError>(|r| {
            let mut events = r.events();
            let input = events.latest_input_seq(&s.id)?.unwrap_or_default();
            Ok((input, events.latest_payload_after(&s.id, RECEIPT, input)?))
        })?;
        let Some(r) = data.and_then(|d| serde_json::from_str::<Receipt>(&d).ok()) else {
            return Ok(None);
        };
        if r.item != item.id
            || r.cycle != item.cycle
            || r.revision != item.revision
            || r.scope != self.scope_revision(&item.id)?
            || r.input != input
        {
            return Ok(None);
        }
        let facts = self.db().store().item_facts();
        let latest = facts.latest_turn(&s.id)?;
        if latest
            .as_ref()
            .is_some_and(|t| r.turn.as_ref() != Some(&t.turn_id))
        {
            return Ok(None);
        }
        if latest.is_none()
            && let Some(now) = self.rt().running(&s.id)
            && !r.attempt.as_deref().is_some_and(|a| {
                self.rt().same_turn(
                    &Running {
                        session: s.id.clone(),
                        attempt: a.into(),
                    },
                    &now,
                )
            })
        {
            return Ok(None);
        }
        if let Some(a) = attempt {
            let same = match (r.turn.as_deref(), facts.turn_of_attempt(a)?) {
                (Some(t), Some(end)) => t == end,
                _ => r.attempt.as_deref().is_some_and(|old| {
                    self.rt().same_turn(
                        &Running {
                            session: s.id.clone(),
                            attempt: old.into(),
                        },
                        &Running {
                            session: s.id.clone(),
                            attempt: a.into(),
                        },
                    )
                }),
            };
            // An unnamed receipt made after the runtime cleared its active attempt can still be
            // attributed by the durable logical turn. Without either identity, fail closed.
            if !same {
                return Ok(None);
            }
        }
        if r.asked {
            let open = self.db().decisions_of_item(&item.id)?.iter().any(|d| {
                d.status == "open" && d.kind == "ask" && d.session_id.as_deref() == Some(&s.id)
            });
            return Ok(open.then_some(Mark::Asked));
        }
        let valid = item.phase == crate::cycle::CLOSING
            && item.closing.as_ref().is_some_and(|c| {
                c["kind"] == crate::cycle::COMPLETE && c["frozen_revision"] == r.revision
            });
        Ok(valid.then_some(Mark::Delivered))
    }

    /// Reconcile only the no-receipt fallback. Execution failures, unknown effects, approvals and
    /// release cards keep their own recovery paths. Runs after a restart as well as during work.
    pub(crate) fn receipts_tidy(&self) -> Result<usize> {
        let mut n = 0;
        for id in self.db().store().item_facts().open_task_items()? {
            let item = self.item(&id)?;
            let Some(s) = self.active_session(&id)? else {
                continue;
            };
            if self.turn_receipt(&item, &s, None)?.is_some() {
                n += crate::lifecycle::supersede_where(
                    self.db(),
                    &id,
                    |d| {
                        crate::lifecycle::stop_card(d) && (
                        d.question.starts_with("Agent 停下来了，没有交付也没有提问")
                        || d.question.starts_with("它停下了：上一回合结束后没有交付、提问")
                        || d.question.starts_with("它停下了：会话空闲：没有回合在运行、没有排队输入，也没有交付或提问")
                    )
                    },
                    "当前回合已有持久交付或提问回执",
                )?;
            }
        }
        Ok(n)
    }
}
