//! Public one-shot job contracts, driven by a finite local ACP script with no test sleeps.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;
use steve_acp::oneshot::{self, Request};
use steve_acp::{AcpConfig, OneShotError};
use steve_node_api::{EventBody, Measurement, PermissionMode, Role, Tokens};
fn python() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("python3"))
        .find(|p| p.is_file())
        .unwrap()
}
fn alive(pid: u32) -> bool {
    pid != 0 && unsafe { libc::kill(pid as i32, 0) } == 0
}
struct Fixture {
    dir: tempfile::TempDir,
    cfg: AcpConfig,
    log: PathBuf,
}
impl Fixture {
    fn new(case: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("peer.jsonl");
        let path = dir.path().join("peer.py");
        let body = r#"import json,sys,os
case=CASE
log=LOG
def send(v): print(json.dumps({'jsonrpc':'2.0',**v}),flush=True)
def update(v,sid='session'): send({'method':'session/update','params':{'sessionId':sid,'update':v}})
def chunk(text,mid='answer',thought=False,**meta): update({'sessionUpdate':'agent_thought_chunk' if thought else 'agent_message_chunk','messageId':mid,'content':{'type':'text','text':text},**meta})
with open(log,'a') as f: f.write(json.dumps({'spawn':True})+'\n')
for line in sys.stdin:
 m=json.loads(line);method=m.get('method')
 with open(log,'a') as f: f.write(json.dumps({'method':method,'params':m.get('params')})+'\n')
 if method=='initialize': result={'agentCapabilities':{'promptCapabilities':{'image':case!='no-image'},'sessionCapabilities':{'systemPrompt':{'mode':{'append':case!='head-fallback','override':case!='head-fallback'}}}}}
 elif method in ('session/new','session/load'):
  if method=='session/load': chunk('replayed-history')
  result={'sessionId':'session','modes':{'availableModes':[{'id':'plan'}]}}
 elif method=='session/prompt':
  if case in ('hang','drop'): continue
  if case=='limit': chunk('x'*16000);continue
  if case=='tool': update({'sessionUpdate':'tool_call','toolCallId':'blocked','name':'Bash','kind':'execute','status':'in_progress','rawInput':{'command':'printf blocked'}});continue
  if case=='error':
   chunk('partial',_meta={'usage':{'inputTokens':7,'outputTokens':3}})
   send({'id':m['id'],'error':{'code':-32010,'message':'overloaded','data':'peer detail'}});continue
  update({'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'wrong-session'}},'other')
  chunk('private-thought',thought=True);chunk('earlier','old');chunk('final answer','new')
  result={'stopReason':'max_tokens','usage':{'inputTokens':11,'outputTokens':4}}
 else: result={}
 if 'id' in m: send({'id':m['id'],'result':result})
"#;
        let body = body
            .replace("CASE", &serde_json::to_string(case).unwrap())
            .replace(
                "LOG",
                &serde_json::to_string(&log.to_string_lossy()).unwrap(),
            );
        std::fs::write(&path, format!("#!{}\n{body}", python().display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = AcpConfig {
            declared: Default::default(),
            adapters: HashMap::from([("sg31-fake".into(), path)]),
            node: "unused-node".into(),
            claude_executable: "unused".into(),
            steve_home: dir.path().into(),
            codex_home: dir.path().into(),
            user_home: dir.path().into(),
            version: "sg31".into(),
        };
        Self { dir, cfg, log }
    }
    fn request<'a>(
        &'a self,
        allow: &'a (dyn Fn(&steve_acp::normalize::ToolFact) -> bool + Send + Sync),
    ) -> Request<'a> {
        Request {
            cfg: &self.cfg,
            harness: "sg31-fake",
            cwd: self.dir.path(),
            model: None,
            effort: None,
            resume: None,
            head: Some("system frame"),
            tools_off: false,
            mode: PermissionMode::Plan,
            prompt: vec![json!({"type":"text","text":"question"})],
            mcp_servers: json!([]),
            wrap: None,
            timeout: Duration::from_secs(5),
            prefix: "sg31-attempt",
            on_session: None,
            max_output_bytes: 1 << 20,
            on_activity: None,
            allow_tool: allow,
        }
    }
    fn frames(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
async fn reaped(pid: u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while alive(pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn sg31_jobs_preserve_stream_filtering_resume_head_image_and_callback_payloads() {
    for case in ["success", "resume", "head-fallback", "no-image"] {
        let f = Fixture::new(case);
        let pid = AtomicU32::new(0);
        let order = Mutex::new(Vec::new());
        let events = Mutex::new(Vec::new());
        let activity = AtomicU32::new(0);
        let allow = |_: &steve_acp::normalize::ToolFact| false;
        let session = |_: &str| order.lock().unwrap().push("session");
        let act = || {
            activity.fetch_add(1, Ordering::SeqCst);
        };
        let mut req = f.request(&allow);
        req.on_session = Some(&session);
        req.on_activity = Some(&act);
        req.resume = (case == "resume").then_some("session");
        if case == "no-image" {
            req.prompt
                .push(json!({"type":"image","data":"AA==","mimeType":"image/png"}));
        }
        let on_event = |event: EventBody| {
            if matches!(event,EventBody::Usage{ref report}if report.measurement==Measurement::Final)
            {
                order.lock().unwrap().push("usage");
            }
            events.lock().unwrap().push(event);
        };
        let result = oneshot::run_cancellable(
            req,
            &|p| {
                pid.store(p, Ordering::SeqCst);
                order.lock().unwrap().push("spawn");
                true
            },
            &on_event,
            std::future::pending::<()>(),
        )
        .await;
        reaped(pid.load(Ordering::SeqCst)).await;
        if case == "no-image" {
            assert!(matches!(result, Err(OneShotError::Unsupported("images"))));
            assert!(!f.frames().iter().any(|r| r["method"] == "session/prompt"));
            continue;
        }
        let out = result.unwrap();
        assert_eq!(
            (
                out.text.as_str(),
                out.session.as_str(),
                out.stop_reason.as_str()
            ),
            ("final answer", "session", "max_tokens")
        );
        assert!(
            out.usage
                .iter()
                .any(|u| u.tokens.input == Some(11) && u.tokens.output == Some(4))
        );
        let es = events.lock().unwrap();
        assert!(!es.iter().any(|e|matches!(e,EventBody::TextDelta{text,..}if text.contains("history")||text.contains("thought")||text.contains("wrong-session"))));
        assert!(es.iter().any(|e| matches!(
            e,
            EventBody::MessageStarted {
                role: Role::Assistant,
                ..
            }
        )));
        assert!(activity.load(Ordering::SeqCst) > 0);
        let frames = f.frames();
        let prompt = &frames
            .iter()
            .find(|r| r["method"] == "session/prompt")
            .unwrap()["params"]["prompt"];
        if case == "head-fallback" {
            assert_eq!(prompt[0]["text"], "system frame");
            assert_eq!(prompt[1]["text"], "question");
        } else {
            assert_eq!(prompt[0]["text"], "question");
        }
        let order = order.lock().unwrap();
        assert_eq!(order[0], "spawn");
        if case == "resume" {
            assert!(!order.contains(&"session"));
        } else {
            assert_eq!(order[1], "session");
        }
    }
}
#[tokio::test]
async fn sg31_job_failure_types_preserve_settlement_and_producer_stop_order() {
    for case in ["error", "limit", "tool", "hang"] {
        let f = Fixture::new(case);
        let pid = AtomicU32::new(0);
        let final_alive = Mutex::new(Vec::new());
        let events = Mutex::new(Vec::new());
        let allow = |_: &steve_acp::normalize::ToolFact| false;
        let mut req = f.request(&allow);
        req.max_output_bytes = if case == "limit" { 4096 } else { 1 << 20 };
        if case == "hang" {
            req.timeout = Duration::from_secs(2);
        }
        let error=oneshot::run_cancellable(req,&|p|{pid.store(p,Ordering::SeqCst);true},&|e|{if matches!(&e,EventBody::Usage{report}if report.measurement==Measurement::Final&&report.tokens==Tokens::default()){final_alive.lock().unwrap().push(alive(pid.load(Ordering::SeqCst)));}events.lock().unwrap().push(e);},std::future::pending::<()>()).await.err().expect(case);
        reaped(pid.load(Ordering::SeqCst)).await;
        match case {
            "error" => {
                assert!(
                    matches!(&error,OneShotError::Harness{code:Some(-32010),transient:true,text}if text.contains("overloaded")&&text.contains("peer detail"))
                );
                assert!(
                    events.lock().unwrap().iter().any(
                        |e| matches!(e,EventBody::Usage{report}if report.tokens.input==Some(7))
                    )
                );
            }
            "limit" => assert!(matches!(error, OneShotError::OutputLimit)),
            "tool" => assert!(matches!(error, OneShotError::ToolRefused)),
            _ => assert!(matches!(error, OneShotError::Timeout(_))),
        }
        assert_eq!(
            *final_alive.lock().unwrap(),
            [matches!(case, "error" | "tool")],
            "{case}"
        );
    }
}
#[tokio::test]
async fn sg31_ready_and_session_cancellation_never_enter_prompt_and_failed_registration_never_executes_peer()
 {
    for case in ["ready", "session", "registration"] {
        let f = Fixture::new("hang");
        let pid = AtomicU32::new(0);
        let allow = |_: &steve_acp::normalize::ToolFact| true;
        let cancel = tokio::sync::Notify::new();
        let cb = |_: &str| cancel.notify_one();
        let mut req = f.request(&allow);
        if case == "session" {
            req.on_session = Some(&cb);
        }
        let waiting = async {
            if case != "ready" {
                cancel.notified().await;
            }
        };
        let error = oneshot::run_cancellable(
            req,
            &|p| {
                pid.store(p, Ordering::SeqCst);
                case != "registration"
            },
            &|_| panic!("opening cancellation emits no prompt event"),
            waiting,
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error, OneShotError::Failed(_)));
        if case == "ready" {
            assert_eq!(pid.load(Ordering::SeqCst), 0);
            assert!(f.frames().is_empty());
        } else {
            reaped(pid.load(Ordering::SeqCst)).await;
        }
        assert!(!f.frames().iter().any(|r| r["method"] == "session/prompt"));
        if case == "registration" {
            assert!(f.frames().is_empty());
        }
    }
}
#[tokio::test]
async fn sg31_dropping_the_opening_job_reaps_its_owned_process() {
    let f = Arc::new(Fixture::new("drop"));
    let pid = Arc::new(AtomicU32::new(0));
    let ready = Arc::new(tokio::sync::Notify::new());
    let (ff, pp, rr) = (f.clone(), pid.clone(), ready.clone());
    let job = tokio::spawn(async move {
        let allow = |_: &steve_acp::normalize::ToolFact| true;
        let req = ff.request(&allow);
        oneshot::run_cancellable(
            req,
            &|p| {
                pp.store(p, Ordering::SeqCst);
                rr.notify_one();
                true
            },
            &|_| {},
            std::future::pending::<()>(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), ready.notified())
        .await
        .unwrap();
    job.abort();
    let _ = job.await;
    reaped(pid.load(Ordering::SeqCst)).await;
}
