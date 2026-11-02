//! A recording [`Backend`] for tests and the `serve` example.

use std::sync::Mutex;

use crate::{Backend, Check, Output, PlanStep, ToolError};

/// Accepts one token (`tok` → session `"s1"`) and records every call as `"<tool> <json args>"`.
#[derive(Default)]
pub struct Recorder {
    pub calls: Mutex<Vec<String>>,
    /// When set, every action fails with this message.
    pub fail: Option<String>,
}

impl Recorder {
    fn rec(&self, line: String) -> Result<(), ToolError> {
        eprintln!("{line}");
        if let Ok(mut c) = self.calls.lock() {
            c.push(line);
        }
        match &self.fail {
            Some(m) => Err(m.clone().into()),
            None => Ok(()),
        }
    }
}

impl Backend for Recorder {
    type Session = String;
    async fn session_by_token(&self, token: &str) -> Option<String> {
        (token == "tok").then(|| "s1".to_owned())
    }
    async fn update(&self, s: &String, now: String, plan: Vec<PlanStep>) -> Result<(), ToolError> {
        self.rec(format!(
            "{s} update {}",
            serde_json::json!({"now": now, "plan": plan})
        ))
    }
    async fn ask(
        &self,
        s: &String,
        question: String,
        options: Vec<String>,
        hint: crate::AskHint,
    ) -> Result<(), ToolError> {
        let mut v = serde_json::json!({"question": question, "options": options});
        if let Some(r) = hint.recommended {
            v["recommended"] = r.into();
        }
        if let Some(w) = hint.why {
            v["why"] = w.into();
        }
        self.rec(format!("{s} ask {v}"))
    }
    async fn request_approval(
        &self,
        s: &String,
        action: String,
        detail: String,
    ) -> Result<(), ToolError> {
        self.rec(format!(
            "{s} request_approval {}",
            serde_json::json!({"action": action, "detail": detail})
        ))
    }
    async fn deliver(
        &self,
        s: &String,
        summary: String,
        outputs: Vec<Output>,
        checks: Vec<Check>,
        unfinished: Option<Vec<String>>,
    ) -> Result<(), ToolError> {
        self.rec(format!(
            "{s} deliver {}",
            serde_json::json!({"summary": summary, "outputs": outputs, "checks": checks, "unfinished": unfinished})
        ))
    }
    async fn publish(&self, s: &String, branch: String, summary: String) -> Result<(), ToolError> {
        self.rec(format!(
            "{s} publish {}",
            serde_json::json!({"branch": branch, "summary": summary})
        ))
    }
}
