//! The conversation's read-only research on a Node. The Hub owns the prompt, model fallback,
//! audience and statistics; only the Node starts an adapter and reads project files.

use super::node_jobs::{News, Waiting};
use super::*;
use std::path::PathBuf;
use steve_core::thread::{ResearchOutcome, ResearchRequest, ResearchSink, RunEnd, ToolSeen};
use steve_store::jobs;

pub struct NodeResearch {
    pub runtime: Arc<NodeRuntime>,
    pub node: Arc<steve_node::LocalNode>,
    /// Roots configured for this local Node, refreshed before every research request.
    pub roots: Vec<PathBuf>,
}

fn failed(error: impl ToString) -> ResearchOutcome {
    ResearchOutcome {
        end: RunEnd::Failed,
        error: Some(error.to_string()),
        ..Default::default()
    }
}

fn prompt(q: &ResearchRequest) -> String {
    steve_research::prompt::render(&steve_research::PromptInput {
        bot: q.main.name.clone(),
        style: format!("{}\n{}", q.main.persona, q.main.rules),
        context: q.context.clone(),
        dirs: q
            .readable
            .iter()
            .map(|p| steve_research::DirInfo {
                name: p.name.clone(),
                path: p.path.clone(),
                description: p.description.clone(),
            })
            .collect(),
        recent: q
            .history
            .iter()
            .rev()
            .take(10)
            .rev()
            .map(|m| {
                (
                    if m.author == "me" { "用户" } else { "Steve" }.into(),
                    m.text.clone(),
                )
            })
            .collect(),
        question: q.question.clone(),
        ui: if q.ui {
            steve_core::gateway::A2UI_HINT.into()
        } else {
            String::new()
        },
    })
}

fn project_root(p: &steve_core::repo::Project) -> Result<RootPath, StoreError> {
    if p.device_id != "hub" {
        return Err(conflict("research project is on another Node"));
    }
    Ok(RootPath {
        root: crate::wire::node_binding::root_id(std::path::Path::new(&p.path))
            .ok_or_else(|| conflict("research project has no absolute root"))?,
        rel: idv("")?,
    })
}

impl NodeResearch {
    fn unavailable(&self, attempt: &str) -> Result<bool, StoreError> {
        if self.runtime.is_stopped() {
            return Ok(true);
        }
        self.runtime.store.reader(|conn| {
            Ok(conn.query_row(
                "SELECT state='unknown' OR (state<>'finished' AND EXISTS(
                   SELECT 1 FROM node_commands WHERE target_job_attempt=?1
                   AND method IN ('cancel','abandon') AND state='rejected'
                 )) FROM runtime_job_attempts WHERE attempt_id=?1",
                [attempt],
                |r| r.get(0),
            )?)
        })
    }

    /// Resume only a session created by this workspace on this Node with the same adapter/model.
    /// Legacy CLI session ids and sessions of another adapter start fresh with the prior answer.
    fn continuation(
        &self,
        q: &ResearchRequest,
        model: &str,
    ) -> Result<Option<ResumeHandle>, StoreError> {
        let Some(previous) = q.resume.as_ref().filter(|r| r.model == model) else {
            return Ok(None);
        };
        self.runtime.store.reader(|conn| {
            let spec: Option<String> = conn.query_row(
                "SELECT a.spec FROM runtime_jobs j JOIN runtime_job_attempts a ON a.job_id=j.job_id
                 WHERE j.thread_run_id=?1 AND j.workspace_id=?2 AND j.node_id=?3 AND a.state='finished'
                   AND json_extract(a.completion,'$.continuation')=?4
                 ORDER BY a.updated DESC LIMIT 1",
                rusqlite::params![previous.run, q.ws, self.runtime.node_id.as_str(), previous.claude_session],
                |r| r.get(0),
            ).optional()?;
            let Some(spec) = spec else { return Ok(None) };
            let spec: JobSpec = serde_json::from_str(&spec).map_err(conflict)?;
            if spec.run.choice.harness != q.models.harness
                || spec.run.choice.model.as_deref().unwrap_or("") != model {
                return Ok(None);
            }
            Ok(Some(idv(&previous.claude_session)?))
        })
    }

    fn start(
        &self,
        q: &ResearchRequest,
        attempt: &str,
        model: &str,
        budget: steve_research::Budget,
        deadline: i64,
        continuation: Option<ResumeHandle>,
    ) -> Result<(), StoreError> {
        self.runtime.running()?;
        let mut text = prompt(q);
        if continuation.is_none()
            && let Some(previous) = &q.resume
        {
            text.push_str("\n\nPrevious answer:\n");
            text.push_str(&previous.prior_answer);
        }
        let mut parts = vec![Part::Text { text }];
        let mut files = std::collections::HashSet::new();
        for f in q.files.iter().chain(&q.earlier_files) {
            if !files.insert(&f.id) {
                continue;
            }
            let row = self
                .runtime
                .store
                .files()
                .get(&f.id)?
                .ok_or_else(|| conflict("research attachment is gone"))?;
            if row.workspace_id != q.ws || row.size < 0 {
                return Err(conflict("research attachment is in another workspace"));
            }
            let artifact = ArtifactRef {
                id: idv(row.id.clone())?,
                sha256: idv(format!("sha256:{}", row.sha256))?,
                size: row.size as u64,
                mime: row.mime.clone(),
            };
            if row.mime.starts_with("image/") {
                parts.push(Part::Image { artifact });
            } else {
                let name = row.name.replace(['/', '\\'], "_");
                parts.push(Part::File {
                    artifact,
                    name: format!("{ATTACHMENTS_DIR}{}-{name}", row.id),
                });
            }
        }
        let job = uid("job");
        let op = format!("op_{attempt}");
        let spec = JobSpec {
            job: idv(&job)?,
            run: RunSpec {
                attempt: idv(attempt)?,
                choice: HarnessChoice {
                    harness: q.models.harness.clone(),
                    model: (!model.is_empty()).then(|| model.into()),
                    effort: q.models.effort.clone(),
                },
                directory: RunDirectory::Scratch,
                prompt: parts,
                policy: ExecutionPolicy {
                    revision: idv("research")?,
                    permissions: PermissionMode::Plan,
                    sandbox: SandboxRequirement::AllowFallback {
                        preferred: vec!["fs-readonly".into()],
                        acceptable: vec![],
                    },
                    readable: vec![],
                    network: NetworkScope::Unrestricted,
                    tools: idv("hub")?,
                },
                limits: Limits {
                    deadline: Timestamp(deadline),
                    offline_until: Timestamp(deadline),
                    max_output_bytes: 256 << 20,
                    max_tokens: None,
                    max_cost: None,
                },
                continuation,
            },
            work: JobWork::Research(ResearchJob {
                cwd: q.project.as_ref().map(project_root).transpose()?,
                readable: q
                    .readable
                    .iter()
                    .map(project_root)
                    .collect::<Result<_, _>>()?,
                budget: ResearchBudget {
                    max_turns: budget.max_turns,
                    max_tools: budget.max_tools,
                    idle_ms: u64::try_from(budget.idle.as_millis()).unwrap_or(u64::MAX),
                },
                steve_tools: q.mcp.is_some(),
            }),
        };
        let exec = ExecutionRef::Job {
            job: spec.job.clone(),
            attempt: spec.run.attempt.clone(),
        };
        let (req, digest) = command(&op, spec.clone())?;
        self.runtime.tx(|tx| {
            jobs::create_job_in(
                tx,
                &jobs::NewJob {
                    id: &job,
                    kind: jobs::JobKind::Research,
                    purpose: "research",
                    workspace: &q.ws,
                    node: self.runtime.node_id.as_str(),
                    thread_run: Some(&q.run),
                },
                &jobs::NewJobAttempt {
                    id: attempt,
                    ordinal: 0,
                    model,
                    operation: &op,
                    execution: &serde_json::to_value(exec).map_err(conflict)?,
                    spec: &serde_json::to_value(spec).map_err(conflict)?,
                    digest: &digest,
                },
                &jobs::StartJob {
                    request: &req,
                    digest: &digest,
                },
                now(),
            )
        })?;
        if !self.runtime.jobs.has(attempt) {
            self.runtime.stop_job(attempt);
        }
        self.runtime.wake_send.notify_one();
        Ok(())
    }
}

#[async_trait]
impl steve_core::thread::Researcher for NodeResearch {
    async fn research(&self, mut q: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        // A workspace can contain remote projects too. They cannot become roots of this Node,
        // but their presence must not prevent reading a local project.
        q.readable.retain(|p| p.device_id == "hub");
        let roots = match crate::wire::node_binding::roots(&self.runtime.store, &self.roots) {
            Ok(roots) => roots,
            Err(e) => return failed(e),
        };
        self.node.set_roots(roots);
        let setting = self
            .runtime
            .store
            .settings()
            .get(&q.ws, "thread.research", Value::Null)
            .unwrap_or(Value::Null);
        let budget = steve_research::Budget::from_setting(&setting);
        let started = now();
        let deadline = budget
            .hard
            .map(|d| started.saturating_add(d.as_millis() as i64))
            .unwrap_or(i64::MAX / 2);
        let models = if q.models.models.is_empty() {
            vec![String::new()]
        } else {
            q.models.models.clone()
        };
        let mut attempts = vec![];
        if let Some(previous) = &q.resume {
            match self.continuation(&q, &previous.model) {
                Ok(Some(session)) => attempts.push((previous.model.clone(), Some(session))),
                Ok(None) => {}
                Err(e) => return failed(e),
            }
        }
        attempts.extend(models.into_iter().map(|model| (model, None)));
        let mut last = failed("Node 没有返回研究结果");
        for (model, continuation) in attempts {
            if q.stop.load(std::sync::atomic::Ordering::SeqCst) {
                return ResearchOutcome {
                    end: RunEnd::Stopped,
                    ..last
                };
            }
            let attempt = uid("ja");
            let mut news = self.runtime.jobs.add(&attempt);
            let mut waiting = Waiting {
                runtime: self.runtime.clone(),
                attempt: attempt.clone(),
                armed: true,
            };
            if let Err(e) = self.start(&q, &attempt, &model, budget, deadline, continuation) {
                waiting.armed = false;
                return failed(e);
            }
            let mut text = String::new();
            let mut first = None;
            let mut usage = vec![];
            let mut tools = std::collections::HashSet::new();
            let mut used_tool = false;
            let mut stopped = false;
            let mut unavailable = false;
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            let mut health = tokio::time::interval(Duration::from_secs(1));
            let c = loop {
                tokio::select! {
                    n = news.recv() => match n {
                        Some(News::Text(t)) => {
                            first.get_or_insert_with(|| now() - started);
                            text = t; sink.text(&text);
                        }
                        Some(News::Tool(t)) => {
                            used_tool = true;
                            if matches!(t.phase, ToolPhase::Succeeded | ToolPhase::Failed) && tools.insert(t.id.to_string()) {
                                sink.tool(&ToolSeen {
                                    name: t.name, brief: t.target.unwrap_or(t.title),
                                    ok: t.phase == ToolPhase::Succeeded,
                                    error: t.error, ms: t.duration_ms.unwrap_or(0) as i64,
                                });
                            }
                        }
                        Some(News::Usage(r)) => { usage.push(r); }
                        Some(News::End(c)) => break c,
                        None => return failed("Node 的研究事件流已关闭"),
                    },
                    _ = tick.tick() => {
                        if !stopped && (q.stop.load(std::sync::atomic::Ordering::SeqCst) || now() >= deadline) {
                            stopped = true; self.runtime.stop_job(&attempt);
                        }
                    }
                    _ = health.tick() => {
                        if self.unavailable(&attempt).unwrap_or(true) {
                            unavailable = true;
                            break Completion {
                                outcome: Outcome::Interrupted, stop_reason: "connection_lost".into(),
                                error: Some(ApiError::new(ErrorCode::Offline,
                                    "Node 连接中断，已保留当前内容并请求停止；尚未确认执行结束")),
                                result: vec![], continuation: None, transient: false, acted: used_tool, job: None,
                            };
                        }
                    }
                }
            };
            // An unavailable Node has not confirmed its end. Keep the durable cancellation
            // active for reconciliation while allowing the conversation to leave its busy state.
            waiting.armed = unavailable;
            let raw = c
                .result
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            let (answer, offer) =
                steve_core::gateway::split_offer(if raw.is_empty() { &text } else { &raw });
            let end = match c.job.as_ref().map(|r| &r.end) {
                _ if c.outcome == Outcome::Interrupted => RunEnd::Failed,
                Some(JobEnd::Done) => RunEnd::Done,
                Some(JobEnd::Stopped) => RunEnd::Stopped,
                Some(JobEnd::Budget { .. }) => RunEnd::Budget,
                Some(JobEnd::Violated { .. }) => RunEnd::Violated,
                _ if c.outcome == Outcome::Succeeded => RunEnd::Done,
                _ if c.outcome == Outcome::Cancelled => RunEnd::Stopped,
                _ => RunEnd::Failed,
            };
            let ok = end == RunEnd::Done;
            let record = steve_core::gateway::Attempt {
                result: if ok {
                    Ok(Default::default())
                } else {
                    Err(steve_core::CoreError::Msg("research".into()))
                },
                attempts: 1,
                duration_ms: (now() - started).max(0) as u64,
                usage: super::node_jobs::normalized_usage(usage, &model)
                    .into_iter()
                    .map(steve_core::gateway::model_usage)
                    .collect(),
                model: model.clone(),
                failures: vec![],
                harness: [
                    ("claude-code", "claude-cli"),
                    ("codex", steve_core::gateway::CODEX_HARNESS),
                ]
                .into_iter()
                .find(|(name, _)| *name == q.models.harness)
                .map_or("acp", |(_, label)| label),
            };
            if let Err(e) = steve_core::stats::record(
                &self.runtime.store,
                &record,
                &steve_core::stats::CallTag {
                    purpose: "research",
                    workspace_id: q.ws.clone(),
                    item_id: None,
                },
            ) {
                tracing::warn!("research statistics: {e}");
            }
            last = ResearchOutcome {
                text: if unavailable && !answer.is_empty() {
                    format!("{answer}\n\n（Node 连接中断，已请求停止，尚未确认执行结束。）")
                } else {
                    answer
                },
                offer,
                end,
                model: Some(model),
                claude_session: c.continuation.map(|s| s.to_string()),
                turns: c.job.as_ref().map_or(0, |r| i64::from(r.turns)),
                first_text_ms: first,
                error: c.error.map(|e| e.safe_detail),
            };
            // Retrying after text or a tool would duplicate work and replace a visible answer.
            if unavailable || end != RunEnd::Failed || !text.is_empty() || used_tool || c.acted {
                return last;
            }
        }
        last
    }
}

#[cfg(test)]
#[path = "node_research_tests.rs"]
mod tests;
