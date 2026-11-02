//! An in-memory Host and helpers shared by the integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use steve_acp::{
    AcpConfig, AutonomousUsage, FileRef, Host, McpEndpoint, Message, PermissionRequest,
    SessionInfo, StoredFile, TurnBegin, TurnEnd, TurnFinish, Usage,
};

#[derive(Default)]
pub struct Rec {
    pub reject_queue: bool,
    pub reject_receipt: bool,
    pub sessions: HashMap<String, SessionInfo>,
    pub statuses: Vec<String>,
    pub events: Vec<(String, Value)>,
    pub queue: HashMap<String, Vec<Message>>,
    pub usage: Option<Usage>,
    pub decisions: Vec<(String, PermissionRequest)>,
    pub turn_ends: Vec<TurnEnd>,
    pub files: HashMap<String, StoredFile>,
    pub begun: Vec<TurnBegin>,
    pub finished: Vec<TurnFinish>,
    pub autonomous: Vec<AutonomousUsage>,
}

#[derive(Default)]
pub struct MemHost(pub Mutex<Rec>);

impl MemHost {
    pub fn with(&self, s: SessionInfo) {
        self.0.lock().unwrap().sessions.insert(s.id.clone(), s);
    }
    pub fn rec<T>(&self, f: impl FnOnce(&Rec) -> T) -> T {
        f(&self.0.lock().unwrap())
    }
    pub fn kinds(&self) -> Vec<String> {
        self.rec(|r| r.events.iter().map(|(k, _)| k.clone()).collect())
    }
    /// texts of `system` events
    pub fn system(&self) -> Vec<String> {
        self.rec(|r| {
            r.events
                .iter()
                .filter(|(k, _)| k == "system")
                .map(|(_, d)| d["text"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
    }
    /// concatenated agent text
    pub fn said(&self) -> String {
        self.rec(|r| {
            r.events
                .iter()
                .filter(|(k, _)| k == "agent_message_chunk")
                .map(|(_, d)| d["content"]["text"].as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
                .join("|")
        })
    }
}

impl MemHost {
    /// store an attachment blob in `dir` and return its ref
    pub fn add_file(&self, dir: &Path, id: &str, name: &str, mime: &str, bytes: &[u8]) -> FileRef {
        let path = dir.join(format!("blob-{id}"));
        std::fs::write(&path, bytes).unwrap();
        let size = bytes.len() as u64;
        let f = StoredFile {
            id: id.into(),
            name: name.into(),
            mime: mime.into(),
            size,
            path,
        };
        self.0.lock().unwrap().files.insert(id.into(), f);
        FileRef {
            id: id.into(),
            name: name.into(),
            mime: mime.into(),
            size,
        }
    }
}

impl Host for MemHost {
    fn session(&self, id: &str) -> Option<SessionInfo> {
        self.0.lock().unwrap().sessions.get(id).cloned()
    }
    fn set_status(&self, id: &str, status: &str) {
        let mut r = self.0.lock().unwrap();
        r.statuses.push(status.into());
        if let Some(s) = r.sessions.get_mut(id) {
            s.status = status.into();
        }
    }
    fn set_acp_session_id(&self, id: &str, acp: &str) {
        if let Some(s) = self.0.lock().unwrap().sessions.get_mut(id) {
            s.acp_session_id = Some(acp.into());
        }
    }
    fn set_model(&self, id: &str, model: &str) {
        if let Some(s) = self.0.lock().unwrap().sessions.get_mut(id) {
            s.model = Some(model.into());
        }
    }
    fn set_reasoning_effort(&self, id: &str, effort: &str) {
        if let Some(s) = self.0.lock().unwrap().sessions.get_mut(id) {
            s.reasoning_effort = Some(effort.into());
        }
    }
    fn set_usage(&self, _: &str, u: Usage) {
        self.0.lock().unwrap().usage = Some(u);
    }
    fn add_event(&self, _: &str, kind: &str, data: Value) {
        self.0.lock().unwrap().events.push((kind.into(), data));
    }
    fn file(&self, id: &str) -> Option<StoredFile> {
        self.0.lock().unwrap().files.get(id).cloned()
    }
    fn queue_add(
        &self,
        id: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<bool, String> {
        let mut r = self.0.lock().unwrap();
        if r.reject_queue {
            return Err("simulated disk write failure".into());
        }
        let q = r.queue.entry(id.into()).or_default();
        if q.iter().any(|m| m.text == text && m.files == files) {
            return Ok(false);
        }
        q.push(Message {
            text: text.into(),
            via: via.into(),
            files: files.to_vec(),
        });
        Ok(true)
    }
    fn dispatch_submitted(&self, _: &str) -> bool {
        !self.0.lock().unwrap().reject_receipt
    }
    fn queue_take(&self, id: &str) -> Vec<Message> {
        self.0.lock().unwrap().queue.remove(id).unwrap_or_default()
    }
    fn queue_len(&self, id: &str) -> usize {
        self.0.lock().unwrap().queue.get(id).map_or(0, Vec::len)
    }
    fn mcp_endpoint(&self, _: &SessionInfo) -> McpEndpoint {
        McpEndpoint {
            url: "http://127.0.0.1:9/mcp".into(),
            token: "tok-123".into(),
        }
    }
    fn permission_decision(&self, _: &str, req: PermissionRequest) -> Option<String> {
        let mut r = self.0.lock().unwrap();
        let id = format!("d{}", r.decisions.len() + 1);
        r.decisions.push((id.clone(), req));
        Some(id)
    }
    fn turn_end(&self, t: TurnEnd) {
        self.0.lock().unwrap().turn_ends.push(t);
    }
    fn turn_begin(&self, b: TurnBegin) -> Option<String> {
        let mut r = self.0.lock().unwrap();
        r.begun.push(b);
        Some(format!("tn{}", r.begun.len()))
    }
    fn turn_finish(&self, f: TurnFinish) {
        self.0.lock().unwrap().finished.push(f);
    }
    fn autonomous_usage(&self, u: AutonomousUsage) {
        self.0.lock().unwrap().autonomous.push(u);
    }
}

pub async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A temp dir with a wrapper script that runs the fake adapter with per-test env (no global set_var).
pub struct Fake {
    pub dir: tempfile::TempDir,
    pub log: PathBuf,
    pub adapter: PathBuf,
}

impl Fake {
    pub fn new(extra_env: &[(&str, &str)]) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.ndjson");
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-acp.js");
        let mut sh = format!("#!/bin/sh\nexport FAKE_ACP_LOG='{}'\n", log.display());
        for (k, v) in extra_env {
            sh += &format!("export {k}='{v}'\n");
        }
        sh += &format!("exec node '{}' \"$@\"\n", fake.display());
        let adapter = dir.path().join("adapter.sh");
        std::fs::write(&adapter, sh).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir, log, adapter }
    }

    pub fn config(&self) -> AcpConfig {
        let p = self.dir.path();
        AcpConfig {
            adapters: HashMap::from([
                ("claude-code".into(), self.adapter.clone()),
                ("codex".into(), self.adapter.clone()),
            ]),
            node: "node".into(),
            claude_executable: "/fake/claude".into(),
            steve_home: p.join("steve"),
            codex_home: p.join("codex"),
            user_home: p.into(),
            version: "test".into(),
            idle_release: Duration::from_secs(600),
            retry_delays: vec![Duration::from_millis(200), Duration::from_millis(400)],
        }
    }

    pub fn session(&self, id: &str, harness: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            harness: harness.into(),
            cwd: self.dir.path().display().to_string(),
            ..Default::default()
        }
    }

    /// every message the fake received, in order
    pub fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    pub fn methods(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter(|m| m["method"] == "session/prompt")
            .map(|m| {
                m["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }
}

pub fn host() -> Arc<MemHost> {
    Arc::new(MemHost::default())
}

pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence
    unsafe { libc_kill(pid as i32, 0) == 0 }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
