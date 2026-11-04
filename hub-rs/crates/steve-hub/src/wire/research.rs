//! The thread's research seam (`steve_core::thread::Researcher`) on the read-only executor of
//! steve-research (thread-routing R2): the prompt is filled from what the thread collected, the run's
//! events become drafts and tool rows, and its end becomes the thread's `RunEnd`.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use steve_core::thread::{ResearchOutcome, ResearchRequest, ResearchSink, RunEnd, ToolSeen};
use steve_research as r;

pub struct ResearchAdapter {
    pub researcher: r::Researcher,
    pub db: steve_core::repo::Db,
    pub data_dir: PathBuf,
}

impl ResearchAdapter {
    pub fn new(cfg: r::ResearchConfig, db: steve_core::repo::Db, data_dir: PathBuf) -> Self {
        if let Some(note) = cfg.startup_note() {
            tracing::info!("{note}");
        }
        ResearchAdapter {
            researcher: r::Researcher::new(cfg),
            db,
            data_dir,
        }
    }

    fn request(&self, q: &ResearchRequest) -> r::ResearchRequest {
        let skip = q.history.len().saturating_sub(10);
        let prompt = r::prompt::render(&r::PromptInput {
            bot: q.main.name.clone(),
            style: format!("{}\n{}", q.main.persona, q.main.rules)
                .trim()
                .to_string(),
            context: q.context.clone(),
            dirs: q
                .readable
                .iter()
                .map(|p| r::DirInfo {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    description: p.description.clone(),
                })
                .collect(),
            recent: q.history[skip..]
                .iter()
                .map(|m| {
                    let who = if m.author == "me" { "用户" } else { "Steve" };
                    (who.to_string(), m.text.clone())
                })
                .collect(),
            question: q.question.clone(),
            ui: if q.ui {
                steve_core::gateway::A2UI_HINT.to_string()
            } else {
                String::new()
            },
        });
        let cwd = q.project.as_ref().map(|p| PathBuf::from(&p.path));
        /* a follow-up continues the earlier session: the question alone is the prompt */
        let (prompt, resume) = match &q.resume {
            Some(f) => (
                q.question.clone(),
                Some(r::Resume {
                    claude_session: f.claude_session.clone(),
                    model: f.model.clone(),
                    prior_answer: f.prior_answer.clone(),
                }),
            ),
            None => (prompt, None),
        };
        let mut req = r::ResearchRequest::new(&q.ws, &q.question, prompt);
        req.run_id = Some(q.run.clone());
        req.resume = resume;
        req.mcp = q.mcp.as_ref().map(|m| r::McpAccess {
            url: m.url.clone(),
            token: m.token.clone(),
        });
        req.add_dirs = q
            .readable
            .iter()
            .map(|p| PathBuf::from(&p.path))
            .filter(|p| Some(p) != cwd.as_ref())
            .collect();
        req.cwd = cwd;
        if !q.models.models.is_empty() {
            req.models = Some(q.models.models.clone());
        }
        req.effort = q.models.effort.clone();
        req.harness = q.models.harness.clone();
        /* the question's own files, then the ones of the message it follows up on */
        let own = q.files.iter().map(|f| (f, false));
        let earlier = q.earlier_files.iter().map(|f| (f, true));
        req.attachments = own
            .chain(earlier)
            .filter_map(|(r, earlier)| {
                let f = self.db.file(&r.id).ok().flatten()?;
                Some(r::Attachment {
                    path: steve_core::attachments::blob_path(&self.data_dir, &f),
                    name: f.name.clone(),
                    mime: f.mime.clone(),
                    size: u64::try_from(f.size).unwrap_or(0),
                    earlier,
                })
            })
            .collect();
        req
    }
}

impl ResearchAdapter {
    /// Books the run's tokens as a `research` call (stats.md §2.4), on the model that ran it.
    fn record(&self, ws: &str, out: &r::ResearchOutcome) {
        let model = out.model.clone().unwrap_or_default();
        let codex = out.result.as_ref().is_some_and(|d| d["harness"] == "codex");
        let usage = match out.result.as_ref() {
            // Codex reports one usage object for the turn, input counting the cached part
            Some(d) if codex => d
                .get("usage")
                .filter(|u| u.is_object())
                .map(|u| {
                    vec![steve_core::gateway::ModelUsage {
                        model: model.clone(),
                        tokens: steve_stats::normalize(
                            "codex",
                            steve_stats::RawUsage::from_json(u),
                            true,
                        ),
                        cost_reported: None,
                        detail: serde_json::json!({"scope": "main"}),
                    }]
                })
                .unwrap_or_default(),
            Some(d) => steve_core::gateway::usage_of(d, &model),
            None => vec![],
        };
        let ok = matches!(out.status, r::RunStatus::Done);
        let a = steve_core::gateway::Attempt {
            result: if ok {
                Ok(Default::default())
            } else {
                Err(steve_core::CoreError::Msg("research".into()))
            },
            attempts: 1,
            duration_ms: out.ms,
            usage,
            model,
            failures: vec![],
            harness: if codex {
                steve_core::gateway::CODEX_HARNESS
            } else {
                ""
            },
        };
        let tag = steve_core::stats::CallTag {
            purpose: "research",
            workspace_id: ws.to_string(),
            item_id: None,
        };
        if let Err(e) = steve_core::stats::record(self.db.store(), &a, &tag) {
            tracing::warn!("stats: research usage not recorded: {e}");
        }
    }
}

#[async_trait]
impl steve_core::thread::Researcher for ResearchAdapter {
    async fn shutdown(&self, grace: Duration) -> bool {
        self.researcher.shutdown(grace).await
    }
    async fn research(&self, q: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        let mut run = match self.researcher.start(self.request(&q)) {
            Ok(run) => run,
            Err(e) => {
                return ResearchOutcome {
                    end: RunEnd::Failed,
                    error: Some(match e {
                        r::ResearchError::Busy { .. } => "这个工作空间已经有一个研究在跑".into(),
                        r::ResearchError::Closed => "服务正在关闭，研究已停止".into(),
                        r::ResearchError::Full => "同时在跑的研究太多，稍后再问".into(),
                    }),
                    ..Default::default()
                };
            }
        };
        let stopper = run.stopper();
        let mut stopped = false;
        loop {
            tokio::select! {
                ev = run.next_event() => match ev {
                    None => break,
                    Some(r::ResearchEvent::Text { text }) => sink.text(&text),
                    Some(r::ResearchEvent::ToolDone(t)) => sink.tool(&ToolSeen {
                        name: t.name,
                        brief: t.brief,
                        ok: t.ok.unwrap_or(true),
                        error: t.error.map(|e| e.code().to_string()),
                        ms: t.ms as i64,
                    }),
                    Some(_) => {}
                },
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if !stopped && q.stop.load(std::sync::atomic::Ordering::SeqCst) {
                        stopped = true;
                        stopper.stop();
                    }
                }
            }
        }
        let out = run.finish().await;
        self.record(&q.ws, &out);
        let (end, error) = match &out.status {
            r::RunStatus::Done => (RunEnd::Done, None),
            r::RunStatus::Stopped => (RunEnd::Stopped, None),
            r::RunStatus::Budget(b) => (RunEnd::Budget, Some(format!("{b:?}"))),
            r::RunStatus::Violated(v) => {
                let detail = serde_json::json!({"run": out.run_id, "violation": format!("{v:?}")});
                if let Err(e) = self.db.audit(
                    "steve",
                    "thread.research_violation",
                    &out.run_id,
                    &detail,
                    &q.ws,
                ) {
                    tracing::error!("audit research violation: {e}");
                }
                (RunEnd::Violated, Some(format!("{v:?}")))
            }
            r::RunStatus::Failed(e) => (RunEnd::Failed, Some(e.clone())),
        };
        ResearchOutcome {
            text: out.answer,
            offer: out.offer.map(|o| steve_core::gateway::Offer {
                title: o.title,
                goal: o.goal,
            }),
            end,
            model: out.model,
            claude_session: out.claude_session,
            turns: out.turns as i64,
            first_text_ms: out.first_text_ms.map(|m| m as i64),
            error,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use steve_core::thread::Researcher as _;

    #[derive(Default)]
    struct Seen {
        text: Mutex<Vec<String>>,
        tools: Mutex<Vec<ToolSeen>>,
    }
    impl ResearchSink for Seen {
        fn text(&self, t: &str) {
            self.text.lock().unwrap().push(t.into());
        }
        fn tool(&self, t: &ToolSeen) {
            self.tools.lock().unwrap().push(t.clone());
        }
    }

    fn bot() -> steve_core::repo::Bot {
        steve_core::repo::Bot {
            id: "steve".into(),
            workspace_id: "default".into(),
            name: "Steve".into(),
            role: "r".into(),
            main: true,
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: None,
            rules: "说重点。".into(),
            accept_mode: "inherit".into(),
            perm_mode: "default".into(),
            enabled: true,
            revision: 1,
            updated: 0,
            persona: String::new(),
            proactive: 1,
            avatar: Default::default(),
            default_project: None,
            prompt: None,
            device: None,
        }
    }

    /// The thread's request reaches the executor (prompt, cwd, run id), its events come back as drafts
    /// and tool rows, and its end and offer map onto the thread's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_through_the_executor() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("src/lib.rs"), "fn add() {}\n").unwrap();
        let lib = proj.join("src/lib.rs").display().to_string();
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-5-5"}"#.to_string(),
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#.to_string(),
            serde_json::json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path": lib}}]}}).to_string(),
            serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"1\tfn add() {}"}]}}).to_string(),
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}"#.to_string(),
            serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"add 什么都不做。"}}}).to_string(),
            serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"add 什么都不做。\nOFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}","session_id":"sess-1"}).to_string(),
        ];
        let args = root.join("args.txt");
        let body = lines
            .iter()
            .map(|l| format!("printf '%s\\n' '{}'", l.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join("\n");
        let exe = root.join("claude.sh");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n{body}\n",
                args.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = r::ResearchConfig::new(&exe, root.join("scratch"));
        cfg.models = vec!["claude-sonnet-5-5".into()];
        cfg.home = root.join("home");
        cfg.claude_dir = root.join("claude");
        cfg.sandbox = r::SandboxMode::Off;
        let store =
            steve_store::Store::open(root.join("db.sqlite3"), steve_store::NoPublisher).unwrap();
        let a = ResearchAdapter::new(cfg, steve_core::repo::Db::new(store), root.join("data"));
        let project = steve_core::repo::Project {
            id: "prj".into(),
            workspace_id: "default".into(),
            name: "add".into(),
            description: "加法".into(),
            device_id: "hub".into(),
            path: proj.display().to_string(),
            kind: "dir".into(),
            worktree: false,
            default_bot: None,
            release: None,
            archived: false,
            created: 0,
            updated: 0,
        };
        let seen = Seen::default();
        let req = |run: &str, models: steve_core::models::Choice| ResearchRequest {
            run: run.into(),
            ws: "default".into(),
            mid: "msg_1".into(),
            user: None,
            question: "add 怎么工作？".into(),
            history: vec![],
            main: bot(),
            context: String::new(),
            project: Some(project.clone()),
            readable: vec![project.clone()],
            files: vec![],
            earlier_files: vec![],
            resume: None,
            stop: Arc::new(AtomicBool::new(false)),
            mcp: None,
            models,
            ui: true,
        };
        let out = a.research(req("tr_1", Default::default()), &seen).await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(out.text, "add 什么都不做。");
        assert_eq!(out.offer.unwrap().title, "实现 add");
        assert_eq!(out.claude_session.as_deref(), Some("sess-1"));
        assert_eq!(out.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            seen.text.lock().unwrap().last().unwrap(),
            "add 什么都不做。"
        );
        let tools = seen.tools.lock().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Read");
        let argv = std::fs::read_to_string(&args).unwrap();
        assert!(
            argv.contains("add 怎么工作？"),
            "the prompt carries the question"
        );
        assert!(argv.contains("说重点。"), "and the bot's rules");
        let flag = |argv: &str, f: &str| {
            let l: Vec<&str> = argv.lines().collect();
            l.iter().position(|x| *x == f).map(|i| l[i + 1].to_string())
        };
        assert_eq!(flag(&argv, "--model").as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(flag(&argv, "--effort"), None);

        /* 设置 · 模型 · 就地研究: the request's models and effort reach the command line */
        let out = a
            .research(
                req(
                    "tr_2",
                    steve_core::models::Choice {
                        harness: "claude-code".into(),
                        models: vec!["claude-opus-5-5".into()],
                        effort: Some("max".into()),
                    },
                ),
                &seen,
            )
            .await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(out.model.as_deref(), Some("claude-opus-5-5"));
        let argv = std::fs::read_to_string(&args).unwrap();
        assert_eq!(flag(&argv, "--model").as_deref(), Some("claude-opus-5-5"));
        assert_eq!(flag(&argv, "--effort").as_deref(), Some("max"));

        /* both runs are in the statistics as research, on the model that ran */
        let rows: Vec<(String, String)> =
            a.db.store()
                .with_conn(|c| {
                    let mut st = c.prepare(
                    "SELECT purpose, model FROM usage_records WHERE source = 'gateway' ORDER BY id",
                )?;
                    let v = st
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(v)
                })
                .unwrap();
        assert_eq!(
            rows,
            [
                ("research".to_string(), "claude-sonnet-5-5".to_string()),
                ("research".to_string(), "claude-opus-5-5".to_string())
            ]
        );
    }
}
