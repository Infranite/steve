//! Agent prerequisite management, scoped to the authenticated current item session.

use serde_json::{Value, json};

use crate::cycle::{ACTIVE, conflict};
use crate::lifecycle::deps_open;
use crate::repo::Session;
use crate::{Core, Result, Tx};

impl Core {
    pub fn tool_item_dependencies(&self, session: &Session, args: Value) -> Result<Value> {
        let mut t = self.tx();
        let s = t
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
        let item = t.item(id)?;
        let bot = s
            .bot_id
            .as_deref()
            .map(|id| t.db().bot(id))
            .transpose()?
            .flatten();
        if item.workspace_id != s.workspace_id
            || item.bot_id != s.bot_id
            || bot.is_none_or(|b| !b.enabled)
            || !t
                .db()
                .bot_in_workspace(s.bot_id.as_deref().unwrap_or(""), &s.workspace_id)?
            || t.dependency_session(id)?
                .is_none_or(|latest| latest.id != s.id)
        {
            return Err(conflict("只有事项当前负责人和有效会话可以操作前置等待"));
        }
        if t.workflow_child(&item)? {
            return Err(conflict(
                "工作单元不能修改事项依赖，请向主事项负责人报告阻塞",
            ));
        }
        let action = args["action"].as_str().unwrap_or("get");
        match action {
            "get" => t.dependency_view(id),
            "wait" => {
                if item.phase != ACTIVE || item.archived {
                    return Err(conflict("事项已结束或正在结束，不能登记前置等待"));
                }
                if let Some(graph) = t.db().store().workflows().get(id)? {
                    for unit in &graph.units {
                        if t.workflow_busy(unit)? {
                            return Err(conflict(
                                "协作工作仍在执行或停止尚未确认，请先暂停工作图并确认停止后再登记前置等待",
                            ));
                        }
                    }
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
                t.db()
                    .store()
                    .dependency_waits()
                    .register(id, &s.id, &deps, resume, reason)?;
                // If this call arrives outside a busy turn, no turn-end callback is needed to wake.
                t.check_blocked(id)?;
                let mut view = t.dependency_view(id)?;
                view["end_turn"] = json!(view["waiting"] == true);
                Ok(view)
            }
            _ => Err(conflict("只支持 get 或 wait")),
        }
    }
}

impl Tx<'_> {
    fn dependency_session(&self, id: &str) -> Result<Option<Session>> {
        Ok(self.db().sessions_of_item(id)?.into_iter().rfind(|s| {
            s.status != "ended" && s.readonly == 0 && s.source == "Steve" && s.device_id == "hub"
        }))
    }
    pub(crate) fn dependency_view(&self, id: &str) -> Result<Value> {
        let item = self.item(id)?;
        let wait = self.db().store().dependency_waits().get(id)?;
        let historical_wait =
            item.status == "block" && wait.as_ref().is_some_and(|w| w.state != "waiting");
        let configured = if historical_wait {
            "workspace"
        } else {
            wait.as_ref()
                .map(|w| w.resume.as_str())
                .unwrap_or("workspace")
        };
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
            dependencies.push(
                json!({"id":dep,"status":p.as_ref().map(|i|i.status.as_str()),
                "phase":p.as_ref().map(|i|i.phase.as_str())}),
            );
        }
        let busy = match self.active_session(id)? {
            Some(s) => self.busy(&s.id) || self.rt().uncertain(&s.id),
            None => false,
        };
        Ok(json!({
            "item_id":id,"status":item.status,"phase":item.phase,"cycle":item.cycle,
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
                if matches!(i.phase.as_str(), ACTIVE | crate::cycle::READY)
                    && !i.archived
                    && i.status != "pause"
                    && self.dependency_waiting(&i.id)?
                    && i.status != "block"
                {
                    self.db().item_patch(&i.id, crate::patch![status:"block"])?;
                }
                if matches!(i.phase.as_str(), ACTIVE | crate::cycle::READY)
                    && (i.status == "block"
                        || (i.status != "pause" && self.dependency_waiting(&i.id)?))
                {
                    self.check_blocked(&i.id)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn dependency_resume(&mut self, id: &str) -> Result<bool> {
        let Some(wait) = self.db().store().dependency_waits().get(id)? else {
            return Ok(false);
        };
        if wait.state != "waiting" {
            return Ok(false);
        }
        if self.holding() || self.cfg().no_resume {
            return Ok(true);
        }
        // A dependency finishing during the registering turn must not queue a competing turn.
        for s in self.db().sessions_of_item(id)? {
            if self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
            {
                return Ok(true);
            }
        }
        if self
            .db()
            .decisions_of_item(id)?
            .iter()
            .any(|d| d.status == "open")
        {
            return Ok(true);
        }
        let item = self.item(id)?;
        let automatic = match wait.resume.as_str() {
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
        let text = format!(
            "前置事项都已完成：{}\n等待原因：{}\n请先回读事项及交付记录，再继续未完成工作；不要重复已经完成的外部操作。",
            self.db().deps_of(id)?.join("、"),
            wait.reason
        );
        let sid = session.as_ref().map(|s| s.id.as_str()).unwrap_or("");
        if self
            .db()
            .store()
            .dependency_waits()
            .ready(id, automatic, sid, &text)?
            && automatic
        {
            self.rt().flush(sid, None);
        }
        Ok(true)
    }
}
