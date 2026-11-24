//! Agent prerequisite management, scoped to the authenticated current item session.

use serde_json::{Value, json};

use crate::cycle::{ACTIVE, conflict};
use crate::lifecycle::deps_open;
use crate::repo::{Item, Session};
use crate::{Core, Result, Tx};

impl Core {
    pub fn tool_item_dependencies(&self, session: &Session, args: Value) -> Result<Value> {
        self.tx().tool_item_dependencies(session, args)
    }

    pub fn dependency_get(&self, item: &str) -> Result<Value> {
        self.tx().dependency_view(item)
    }

    /// An item-page action: resolve the current session on the server without a separate agent
    /// turn to register the policy. The same transaction checks the cycle and agent gates.
    pub fn dependency_wait(
        &self,
        item: &str,
        expected_cycle: i64,
        resume: &str,
        reason: &str,
    ) -> Result<Value> {
        let mut t = self.tx();
        let i = t.item(item)?;
        if i.cycle != expected_cycle {
            return Err(conflict("事项已进入另一轮，请刷新后再设置等待"));
        }
        let s = t
            .dependency_session(item)?
            .ok_or_else(|| conflict("没有当前有效事项会话，不能登记持久等待"))?;
        let deps = t.db().deps_of(item)?;
        if deps.is_empty() {
            return Err(conflict("请先添加前置事项"));
        }
        t.tool_item_dependencies(
            &s,
            json!({
                "action":"wait", "deps":deps, "resume":resume, "reason":reason,
            }),
        )
    }
}

impl Tx<'_> {
    pub(crate) fn dependency_summary(&self, item: &str) -> Result<String> {
        let mut summary = vec![];
        for dep in self.db().deps_of(item)? {
            let (title, now) = self
                .db()
                .item(&dep)?
                .map(|i| {
                    (
                        i.title,
                        i.outcome.unwrap_or_else(|| i.phase.as_str().into()),
                    )
                })
                .unwrap_or_default();
            summary.push(format!("{dep}「{title}」：{now}"));
        }
        Ok(summary.join("\n"))
    }

    /// A stale turn can still report after dependency admission closed. Readiness belongs to
    /// the Hub; that turn must not replace the wait with a question or a scope approval card.
    pub(crate) fn dependency_question_guard(&self, item: &str) -> Result<()> {
        if self.dependency_waiting(item)? || !deps_open(self.db(), item)?.is_empty() {
            return Err(conflict(
                "事项正在等待前置完成，由 Hub 核对进度；请结束回合等待，不要向用户提问或请求恢复读取范围",
            ));
        }
        Ok(())
    }

    fn tool_item_dependencies(&mut self, session: &Session, args: Value) -> Result<Value> {
        let s = self
            .db()
            .session(&session.id)?
            .filter(|s| {
                s.status != "ended"
                    && s.readonly == 0
                    && s.source == "Steve"
                    && s.device_id == "hub"
            })
            .ok_or_else(|| conflict("会话已结束或只读"))?;
        let id = s
            .item_id
            .as_deref()
            .ok_or_else(|| conflict("需要事项会话"))?;
        let item = self.item(id)?;
        let bot = s
            .bot_id
            .as_deref()
            .map(|id| self.db().bot(id))
            .transpose()?
            .flatten();
        if item.workspace_id != s.workspace_id
            || item.bot_id != s.bot_id
            || bot.is_none_or(|b| !b.enabled)
            || !self
                .db()
                .bot_in_workspace(s.bot_id.as_deref().unwrap_or(""), &s.workspace_id)?
            || self
                .dependency_session(id)?
                .is_none_or(|latest| latest.id != s.id)
        {
            return Err(conflict("只有事项当前负责人和有效会话可以操作前置等待"));
        }
        if self.workflow_child(&item)? {
            return Err(conflict(
                "工作单元不能修改事项依赖，请向主事项负责人报告阻塞",
            ));
        }
        let action = args["action"].as_str().unwrap_or("get");
        match action {
            "get" => self.dependency_view(id),
            "wait" => {
                if let Some(why) = self.dependency_registration_blocker(&item, Some(&s))? {
                    return Err(conflict(why));
                }
                let deps: Vec<String> = serde_json::from_value(args["deps"].clone())
                    .map_err(|_| conflict("deps 必须是前置事项编号数组"))?;
                let reason = args["reason"]
                    .as_str()
                    .filter(|r| !r.trim().is_empty())
                    .ok_or_else(|| conflict("必须说明等待原因"))?;
                let resume = args["resume"].as_str().unwrap_or("workspace");
                if !["automatic", "notify", "workspace"].contains(&resume) {
                    return Err(conflict("无效的前置完成后续跑方式"));
                }
                self.db()
                    .store()
                    .dependency_waits()
                    .register(id, &s.id, &deps, resume, reason)?;
                self.diagnostic_session(
                    &s.id,
                    None,
                    "dependency.wait_registered",
                    "prerequisites",
                    false,
                    Default::default(),
                );
                // If this call arrives outside a busy turn, no turn-end callback is needed to wake.
                self.check_blocked(id)?;
                let mut view = self.dependency_view(id)?;
                view["end_turn"] = json!(view["waiting"] == true);
                Ok(view)
            }
            _ => Err(conflict("只支持 get 或 wait")),
        }
    }

    fn dependency_session(&self, id: &str) -> Result<Option<Session>> {
        Ok(self.db().sessions_of_item(id)?.into_iter().rfind(|s| {
            s.status != "ended" && s.readonly == 0 && s.source == "Steve" && s.device_id == "hub"
        }))
    }

    /// The item page and the authenticated tool explain the same registration gate. This is
    /// separate from execution readiness: saving a policy never authorizes an isolated run.
    fn dependency_registration_blocker(
        &self,
        item: &Item,
        session: Option<&Session>,
    ) -> Result<Option<&'static str>> {
        if item.phase != ACTIVE || item.archived {
            return Ok(Some("事项已结束或正在结束，不能登记前置等待"));
        }
        if self.workflow_child(item)? {
            return Ok(Some("请由主事项负责人管理前置等待"));
        }
        let bot = item
            .bot_id
            .as_deref()
            .map(|id| self.db().bot(id))
            .transpose()?
            .flatten();
        if session.is_none_or(|s| s.bot_id != item.bot_id || s.workspace_id != item.workspace_id)
            || bot.is_none_or(|b| !b.enabled)
            || !self
                .db()
                .bot_in_workspace(item.bot_id.as_deref().unwrap_or(""), &item.workspace_id)?
        {
            return Ok(Some("没有当前负责人可续跑的有效会话"));
        }
        if let Some(graph) = self.db().store().workflows().get(&item.id)?
            && self.workflow_unsettled(&graph.units)?.is_some()
        {
            return Ok(Some(
                "协作工作仍在执行或停止尚未确认，请先暂停工作图并确认停止后再登记前置等待",
            ));
        }
        if self.db().decisions_of_item(&item.id)?.iter().any(|d| {
            d.status == steve_types::DecisionStatus::Open
                && !matches!(d.kind.as_str(), "ask" | "fail")
        }) {
            return Ok(Some("事项还有验收或授权待处理，不能改为前置等待"));
        }
        Ok(None)
    }
    pub(crate) fn dependency_view(&self, id: &str) -> Result<Value> {
        let item = self.item(id)?;
        let wait = self.db().store().dependency_waits().get(id)?;
        let configured = wait
            .as_ref()
            .map(|w| w.resume.as_str())
            .unwrap_or("workspace");
        let requested_auto = match configured {
            "automatic" => true,
            "notify" => false,
            _ => {
                self.db()
                    .setting(&item.workspace_id, "depsAfter", "自动开始")?
                    == "自动开始"
            }
        };
        let fallback = requested_auto && wait.as_ref().is_some_and(|w| w.state == "notified");
        let automatic = requested_auto && !fallback;
        let mut dependencies = vec![];
        for dep in self.db().deps_of(id)? {
            let p = self.db().item(&dep)?;
            dependencies.push(json!({"id":dep,"phase":p.as_ref().map(|i|i.phase),
                "outcome":p.as_ref().and_then(|i|i.outcome.as_deref())}));
        }
        let busy = match self.active_session(id)? {
            Some(s) => self.busy(&s.id) || self.rt().uncertain(&s.id),
            None => false,
        };
        // Registration is a management action. It does not establish that the Node can execute
        // collaboration; without registration the workspace's policy applies.
        let session = self.dependency_session(id)?;
        let register_blocker = if dependencies.is_empty() {
            Some("请先添加前置事项")
        } else {
            self.dependency_registration_blocker(&item, session.as_ref())?
        };
        Ok(json!({
            "item_id":id,"phase":item.phase,"cycle":item.cycle,
            "dependencies":dependencies,"open_dependencies":deps_open(self.db(),id)?,
            "resume_mode":if automatic {"automatic"} else {"notify"},
            "resume_source":if fallback {"fallback"} else if configured=="workspace" {"workspace"} else {"item"},
            "resume_override":configured,
            "resume_enabled":!self.cfg().no_resume && !self.holding(),
            "resume_blocker":if self.cfg().no_resume {Some("实例已禁用自动续跑")}
                else if self.holding() {Some("实例暂时暂停调度")} else {None},
            "resume_fallback_reason":if fallback {Some("本次等待已转为提醒开始，请查看待处理卡片")} else {None},
            "waiting":wait.as_ref().is_some_and(|w|w.state=="waiting"),
            "wait_state":wait.as_ref().map(|w|&w.state),
            "reason":wait.as_ref().map(|w|&w.reason),"active_turn":busy,
            "can_register":register_blocker.is_none(),"register_blocker":register_blocker,
        }))
    }

    /// Returns true for an explicit wait. Waits remain intact across process restarts and don't
    /// trigger the watchdog's "no delivery or question" continuation.
    pub(crate) fn dependency_waiting(&self, id: &str) -> Result<bool> {
        Ok(self
            .db()
            .store()
            .dependency_waits()
            .get(id)?
            .is_some_and(|w| w.state == "waiting"))
    }

    pub(crate) fn dependency_tick(&mut self) -> Result<()> {
        if self.holding() || self.cfg().no_resume {
            return Ok(());
        }
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                if !i.phase.can_run() || i.archived {
                    continue;
                }
                if self.dependency_waiting(&i.id)? {
                    self.check_blocked(&i.id)?;
                }
            }
        }
        Ok(())
    }

    fn dependency_resume_policy(
        &self,
        item: &Item,
        id: &str,
        configured: &str,
    ) -> Result<(bool, Option<Session>)> {
        let automatic = match configured {
            "automatic" => true,
            "notify" => false,
            _ => {
                self.db()
                    .setting(&item.workspace_id, "depsAfter", "自动开始")?
                    == "自动开始"
            }
        };
        let session = self.dependency_session(id)?;
        let owner = item
            .bot_id
            .as_deref()
            .map(|id| self.db().bot(id))
            .transpose()?
            .flatten();
        let owner_ok = owner.is_some_and(|b| b.enabled)
            && self
                .db()
                .bot_in_workspace(item.bot_id.as_deref().unwrap_or(""), &item.workspace_id)?;
        // If no live session survives, notify instead of creating a process from inside the Hub.
        let automatic = automatic
            && owner_ok
            && session
                .as_ref()
                .is_some_and(|s| s.bot_id == item.bot_id && s.readonly == 0);
        Ok((automatic, session))
    }

    pub(crate) fn dependency_resume(&mut self, id: &str) -> Result<bool> {
        let Some(wait) = self.db().store().dependency_waits().get(id)? else {
            return Ok(false);
        };
        let Some(registration) = wait
            .session_id
            .as_deref()
            .filter(|_| wait.state == "waiting")
        else {
            return Ok(false);
        };
        if self.holding() || self.cfg().no_resume {
            self.diagnostic_session(
                registration,
                None,
                "dependency.resume_evaluated",
                if self.cfg().no_resume {
                    "resume_disabled"
                } else {
                    "dispatch_held"
                },
                true,
                Default::default(),
            );
            return Ok(true);
        }
        // A dependency finishing during the registering turn must not queue a competing turn.
        for s in self.db().sessions_of_item(id)? {
            if self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
            {
                self.diagnostic_session(
                    &s.id,
                    None,
                    "dependency.resume_evaluated",
                    "execution_pending",
                    true,
                    steve_store::diagnostics::Details {
                        busy: Some(self.busy(&s.id)),
                        uncertain: Some(self.rt().uncertain(&s.id)),
                        ..Default::default()
                    },
                );
                return Ok(true);
            }
        }
        if self
            .db()
            .decisions_of_item(id)?
            .iter()
            .any(|d| d.status == steve_types::DecisionStatus::Open)
        {
            self.diagnostic_session(
                registration,
                None,
                "dependency.resume_evaluated",
                "open_decision",
                true,
                Default::default(),
            );
            return Ok(true);
        }
        let item = self.item(id)?;
        let (automatic, session) =
            self.dependency_resume_policy(&item, id, wait.resume.as_str())?;
        let text = format!(
            "Hub 已核对前置事项都已完成：\n{}\n等待原因：{}\n请先回读本事项及其交付记录，再继续未完成工作；不要重复已经完成的外部操作。前置状态由 Hub 管理，无需为核对进度读取其他事项或请求扩大读取范围。",
            self.dependency_summary(id)?,
            wait.reason
        );
        let sid = session.as_ref().map(|s| s.id.as_str()).unwrap_or("");
        let changed = self
            .db()
            .store()
            .dependency_waits()
            .ready(id, automatic, sid, &text)?;
        if changed {
            self.diagnostic_item(
                &item,
                "dependency.ready",
                if automatic {
                    "automatic_resume"
                } else {
                    "notify"
                },
                false,
                Default::default(),
            );
        }
        if changed && automatic {
            self.rt().flush(sid, None);
        }
        Ok(true)
    }
}
