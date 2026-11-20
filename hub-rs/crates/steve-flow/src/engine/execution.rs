//! Node dispatch preserves resume, dry-run and approval precedence; persistence is separate.
use super::step_write::StepWrite;
use super::*;

pub(super) async fn execute_node(
    cx: &FlowCx,
    run: RunRow,
    graph: &Graph,
    sender: &Value,
    node: &crate::graph::Node,
    kind: Arc<dyn crate::nodes::NodeKind>,
    cursor: String,
) -> FlowResult<RunStatus> {
    let run_id = run.id.as_str();
    let input = run
        .context
        .get("input")
        .cloned()
        .filter(|_| node.kind != "trigger")
        .unwrap_or(Value::Null);
    let started = cx.now();
    let t0 = Instant::now();
    let effectful = kind.effectful(&node.config);
    let resume = run
        .context
        .get("resume")
        .filter(|r| r["node"] == json!(cursor))
        .cloned();
    let mut rcx = RunCx {
        flow: cx,
        run: run.clone(),
        graph,
        node,
        dry_run: run.dry_run,
        sender: sender.clone(),
        effects: Default::default(),
    };
    let step = choose_step(kind.as_ref(), &mut rcx, &run, &input, effectful, resume).await?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    // a named branch with no edge while the node has others: the run ends here, and says so
    let unrouted: Option<String> = match &step {
        Step::Advance {
            branch: Some(b),
            to: None,
            ..
        } if graph.outs(&cursor).next().is_some() => {
            tracing::info!(run = %run_id, node = %cursor, branch = %b, "flow branch without edge");
            Some(b.clone())
        }
        _ => None,
    };
    let effects = std::mem::take(&mut rcx.effects);
    let gate_mark = rcx.run.context["approved"][&cursor] == json!(true);
    let now = cx.now();
    let node_name = node.name().to_string();
    let run_id_owned = run_id.to_string();
    let held = match STEP_WRITES.try_with(Arc::clone) {
        Ok(m) => Some(m.lock_owned().await),
        Err(_) => None,
    };
    // on the blocking pool: the step's write may wait for the writer
    let write = StepWrite {
        cursor,
        input,
        started,
        ms,
        step,
        effects,
        gate_mark,
        unrouted,
        node_name,
        now,
    };
    let status = cx
        .blocking(move |s| {
            s.transaction(|tx| {
                let _held = held;
                write.commit(tx, &run_id_owned)
            })
        })
        .await?;
    Ok(status)
}

async fn choose_step(
    kind: &dyn crate::nodes::NodeKind,
    rcx: &mut RunCx<'_>,
    run: &RunRow,
    input: &Value,
    effectful: bool,
    resume: Option<Value>,
) -> FlowResult<Step> {
    let (graph, node) = (rcx.graph, rcx.node);
    let cursor = &node.id;
    let gate_ok = approved(run, cursor);
    let step = match resume {
        Some(r) if r["gate"] == GATE_NO => Step::Refused,
        Some(r) if r.get("error").is_some_and(|e| !e.is_null()) => step_of(
            graph,
            node,
            NodeOutcome::Fail(r["error"].as_str().unwrap_or("").to_string()),
        ),
        Some(r) if r["gate"] == GATE_YES => {
            rcx.run.context["approved"][&cursor] = json!(true);
            step_of(graph, node, kind.run(rcx, &node.config, &input).await)
        }
        Some(r) => step_of(
            graph,
            node,
            NodeOutcome::Next {
                output: r.get("output").cloned().unwrap_or(Value::Null),
                branch: r["branch"].as_str().map(String::from),
            },
        ),
        None if run.dry_run => dry_step(kind, rcx, run, input, effectful).await,
        None if effectful && !gate_ok => unapproved_effect(kind, rcx, run, input).await?,
        None => step_of(graph, node, kind.run(rcx, &node.config, &input).await),
    };
    Ok(step)
}

async fn dry_step(
    kind: &dyn crate::nodes::NodeKind,
    rcx: &mut RunCx<'_>,
    run: &RunRow,
    input: &Value,
    effectful: bool,
) -> Step {
    let (graph, node) = (rcx.graph, rcx.node);
    let cursor = &node.id;
    let prefer: Vec<String> = run.context["prefer"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if effectful {
        step_of(graph, node, NodeOutcome::next(skipped(DRY_EFFECT)))
    } else if node.kind == "approve" || node.kind == "bot" {
        let e = graph.preferred(&cursor, &prefer);
        let mut out = skipped("试运行时按预设的分支走");
        out["branch"] = json!(e.map(|e| e.label.clone()));
        Step::Advance {
            output: out,
            branch: e.map(|e| e.label.clone()),
            to: e.map(|e| e.to.clone()),
            status: "ok",
        }
    } else if node.kind == "wait" {
        let l = wait::pass_label(graph, node);
        let to = graph
            .outs(&cursor)
            .find(|e| Some(&e.label) == l.as_ref())
            .map(|e| e.to.clone());
        Step::Advance {
            output: skipped("试运行时不等待"),
            branch: l,
            to,
            status: "ok",
        }
    } else {
        step_of(graph, node, kind.dry_run(rcx, &node.config, &input).await)
    }
}

async fn unapproved_effect(
    kind: &dyn crate::nodes::NodeKind,
    rcx: &mut RunCx<'_>,
    run: &RunRow,
    input: &Value,
) -> FlowResult<Step> {
    let (cx, graph, node) = (rcx.flow, rcx.graph, rcx.node);
    let grant = kind.grant(&node.config);
    let now = cx.now();
    let ok = match &grant {
        Some(g) => cx.store().reader(|c| granted(c, &run, g, now))?,
        None => false,
    };
    let step = if ok {
        step_of(graph, node, kind.run(rcx, &node.config, &input).await)
    } else {
        let flow_name = cx.store().reader(|c| flows::name_of(c, &run.flow_id))?;
        rcx.effects.outcome = Some("need".into());
        rcx.effects.label = Some(format!("等你批准：{}", node.name()));
        let note = kind.gate_note(&rcx, &node.config);
        let grants = kind.gate_grants(&rcx, &node.config).unwrap_or_default();
        Step::Wait(Box::new(gate_wait(
            &run,
            &flow_name,
            node,
            &rcx.render_value(&node.config, &input),
            note.as_deref(),
            &grants,
        )))
    };
    Ok(step)
}
