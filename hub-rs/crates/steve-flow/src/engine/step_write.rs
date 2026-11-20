//! One completed node's write, applied within the driver's existing transaction.
use super::*;
use crate::nodes::Effects;

pub(super) struct StepWrite {
    pub cursor: String,
    pub input: Value,
    pub started: i64,
    pub ms: f64,
    pub step: Step,
    pub effects: Effects,
    pub gate_mark: bool,
    pub unrouted: Option<String>,
    pub node_name: String,
    pub now: i64,
}
impl StepWrite {
    pub(super) fn commit(&self, tx: &Tx, run_id: &str) -> FlowResult<RunStatus> {
        let mut r = runs::get(tx, run_id)?.ok_or_else(|| ApiError::not_found("运行"))?;
        let ours = r.status == "running";
        self.effects(&mut r);
        self.apply(tx, &mut r, ours)?;
        runs::save(tx, &r)?;
        Ok(RunStatus::parse(&r.status))
    }
    fn effects(&self, r: &mut RunRow) {
        let cursor = &self.cursor;
        let effects = &self.effects;
        let gate_mark = self.gate_mark;
        if let Some(o) = r.context.as_object_mut() {
            o.remove("resume");
        }
        if gate_mark {
            r.context["approved"][cursor] = json!(true);
        }
        r.cost += effects.cost;
        if let Some(o) = &effects.outcome {
            r.outcome = Some(o.clone());
        }
        if let Some(l) = &effects.label {
            r.outcome_label = label_with_prefix(&r, l);
        }
        if effects.item_id.is_some() {
            r.work_item_id = effects.item_id.clone();
        }
        for (k, v) in [
            ("decision_id", &effects.decision_id),
            ("reason", &effects.reason),
            ("handled_by", &effects.handled_by),
        ] {
            if let Some(v) = v {
                r.context[k] = json!(v);
            }
        }
    }
    fn add(
        &self,
        tx: &Tx,
        r: &RunRow,
        status: &str,
        out: &Value,
        branch: Option<&str>,
        err: Option<&str>,
    ) -> FlowResult<()> {
        let seq = runs::add_step(
            tx,
            &r.workspace_id,
            &r.id,
            &NewStep {
                node_id: &self.cursor,
                status,
                input: &self.input,
                output: out,
                ms: self.ms,
                error: err,
                started: self.started,
            },
        )?;
        if let Some(b) = branch {
            tx.execute(
                "UPDATE flow_run_steps SET branch = ? WHERE run_id = ? AND seq = ?",
                params![b, r.id, seq],
            )?;
        }
        Ok(())
    }
    fn apply(&self, tx: &Tx, r: &mut RunRow, ours: bool) -> FlowResult<()> {
        let now = self.now;
        match &self.step {
            Step::Advance {
                output,
                branch,
                to,
                status,
            } => self.advance(tx, r, ours, (output, branch.as_deref(), to, *status))?,
            Step::Wait(w) => self.wait(tx, r, ours, w)?,
            Step::Retry(e) => {
                self.add(tx, r, "failed", &Value::Null, None, Some(e))?;
                if ours {
                    if r.attempts < MAX_RETRIES {
                        r.status = "waiting".into();
                        r.retry_at = Some(now + RETRY_AFTER_MS);
                        r.attempts += 1;
                    } else {
                        r.status = "failed".into();
                        r.outcome_label = label_with_prefix(&r, e);
                        r.ended = Some(now);
                    }
                    r.error = Some(e.clone());
                }
            }
            Step::Fail(e) => {
                self.add(tx, r, "failed", &Value::Null, None, Some(e))?;
                if ours {
                    r.status = "failed".into();
                    r.outcome_label = label_with_prefix(&r, e);
                    r.error = Some(e.clone());
                    r.ended = Some(now);
                }
            }
            Step::Refused => {
                self.add(
                    tx,
                    r,
                    "skipped",
                    &json!({ "refused": true }),
                    Some(GATE_NO),
                    None,
                )?;
                if ours {
                    r.status = "done".into();
                    r.cursor = None;
                    r.outcome = None;
                    r.outcome_label = format!("你没有批准「{}」，没有执行", self.node_name);
                    r.ended = Some(now);
                }
            }
        }
        Ok(())
    }
    fn advance(
        &self,
        tx: &Tx,
        r: &mut RunRow,
        ours: bool,
        next: (&Value, Option<&str>, &Option<String>, &str),
    ) -> FlowResult<()> {
        let (output, branch, to, status) = next;
        let cursor = &self.cursor;
        let effects = &self.effects;
        let unrouted = &self.unrouted;
        let node_name = &self.node_name;
        let now = self.now;
        let err = output["error"].as_str().filter(|_| status == "failed");
        self.add(tx, r, status, output, branch, err)?;
        if let Some(b) = &unrouted
            && r.outcome_label.is_empty()
            && effects.label.is_none()
        {
            r.outcome_label =
                label_with_prefix(&r, &format!("分支「{b}」没有连线，到「{node_name}」为止"));
        }
        r.context["steps"][cursor] = output.clone();
        r.context["input"] = output.clone();
        if ours {
            r.cursor = to.clone();
            r.attempts = 0;
            r.retry_at = None;
            if to.is_none() {
                r.status = "done".into();
                r.ended = Some(now);
                r.error = None;
            }
        }
        Ok(())
    }
    fn wait(&self, tx: &Tx, r: &mut RunRow, ours: bool, w: &WaitSpec) -> FlowResult<()> {
        if !ours {
            self.add(tx, r, "waiting", &w.output, None, None)?;
            return Ok(());
        }
        let run_id = &r.id;
        let cursor = &self.cursor;
        let effects = &self.effects;
        let now = self.now;
        let mut out = w.output.clone();
        let mut decision_id: Option<String> = None;
        if let Some(d) = &w.decision {
            let mut d = d.clone();
            d.subject_id = format!("{run_id}:{cursor}");
            let opened = steve_store::base::decisions::open(tx, &d)?;
            out["decision_id"] = json!(opened.id);
            decision_id = Some(opened.id.clone());
            r.context["decision_id"] = json!(opened.id);
            if effects.outcome.is_some() {
                r.context["held_outcome"] = json!(true);
            }
        }
        self.add(tx, r, "waiting", &out, None, None)?;
        tx.execute(
            "INSERT OR REPLACE INTO flow_waits (run_id, node_id, workspace_id, mode, until, match, decision_id, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![run_id, cursor, r.workspace_id, w.mode, w.until, w.matcher.to_string(), decision_id, now],
        )?;
        r.status = "waiting".into();
        Ok(())
    }
}
