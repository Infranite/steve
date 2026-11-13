/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { AgentTree, type AgentTreeLabels, type TreeAgent } from "@/components/affect/agent-tree-clean";

/*
 * Run Inspector (clean) – everything about one agent run on one page, built on Agent Tree.
 *   · the run's numbers sit under its title: duration, cost, tokens, model and tool calls, errors
 *   · Agent Tree shows who started whom; every card carries a small trace of its own calls, on its own clock
 *   · a click on a call opens it right under its row – what went wrong or came back, and what it was called with
 *   · a click on a card picks the agent: the others fade, the numbers sum up its part of the run
 * One `run` tree feeds all of it; Agent Tree installs with it.
 * The styles are injected below, so no extra files are needed.
 */

export type RunSpanKind = "agent" | "llm" | "tool";
export type RunSpanStatus = "queued" | "running" | "waiting" | "done" | "failed" | "stopped" | "idle" | "paused";

export type RunSpan = {
  id: string;
  name: string;
  /** The orchestrator and its sub-agents are "agent"; their model calls "llm", everything they call "tool" */
  kind: RunSpanKind;
  status: RunSpanStatus;
  /** When it started and ended, in ms from the start of the run – leave the end out while it runs */
  start: number;
  end?: number;
  model?: string;
  /** A few muted words after the name – the model, the query, the target */
  detail?: string;
  /** A small mark, e.g. the model maker's or the tool's logo */
  icon?: ReactNode;
  /** An agent: what it was asked to do */
  task?: string;
  /** A waiting agent: what it asks you */
  ask?: string;
  /** A tool call: the function's name, e.g. "web_search" – defaults to the name */
  tool?: string;
  /** A tool call: what it was called with and what it returned – objects are pretty-printed */
  args?: unknown;
  result?: unknown;
  tokensIn?: number;
  tokensOut?: number;
  /** Its own cost – an agent's children are added on top */
  cost?: number;
  /** A short preview of what went in and came out */
  input?: string;
  output?: string;
  error?: string;
  /** Steve: an agent's time after its cost, already formatted (how long it actually worked) */
  time?: ReactNode;
  children?: RunSpan[];
};

export type RunInspectorProps = {
  run: RunSpan;
  /** An agent that watches the run rather than working in it – shown beside the orchestrator */
  supervisor?: RunSpan;
  now?: number;
  startedAt?: number;
  selected?: string | null;
  onSelectedChange?: (id: string | null) => void;
  /** Controlled id of the call opened under its row */
  call?: string | null;
  onCallChange?: (id: string | null) => void;
  formatCost?: (amount: number) => string;
  className?: string;
  /** Steve: formats every duration – defaults to "1m 05s" */
  formatTook?: (ms: number) => string;
  /** Steve: the words it shows, translated (and the Agent Tree's under `tree`) */
  labels?: Partial<RunInspectorLabels>;
};

export type RunInspectorLabels = {
  facts: { duration: string; cost: string; tokens: string; modelCalls: string; toolCalls: string; errors: string };
  whole: string; callsOf: (name: string) => string; more: (n: number) => string; less: string; tool: string; model: string;
  state: { failed: string; running: string; done: string; queued: string }; stillRunning: string; noOutput: string; calledWith: string; prompt: string;
  title: string; tree?: Partial<AgentTreeLabels>;
};
const LABELS: RunInspectorLabels = {
  facts: { duration: "Duration", cost: "Cost", tokens: "Tokens", modelCalls: "Model calls", toolCalls: "Tool calls", errors: "Errors" },
  whole: "Whole run", callsOf: (name) => `Calls of ${name}`, more: (n) => `+${n} more`, less: "Show less", tool: "Tool", model: "Model",
  state: { failed: "Failed", running: "Running…", done: "Done", queued: "Queued" }, stillRunning: "Still running…", noOutput: "No output", calledWith: "Called with", prompt: "Prompt",
  title: "Agents",
};

const walk = (s: RunSpan): RunSpan[] => [s, ...(s.children ?? []).flatMap(walk)];
const ownCalls = (s: RunSpan) => (s.children ?? []).filter((c) => c.kind !== "agent").sort((a, b) => a.start - b.start);
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 2 });
const dollars = (n: number) => (n === 0 ? "$0" : n < 0.01 ? `$${n.toFixed(3)}` : money.format(n));
const tookEn = (ms: number) => {
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  // Seconds only while they still matter – past ten minutes it's "48m", past an hour "2h 15m"
  if (s < 600) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  return `${Math.floor(s / 3600)}h ${String(Math.floor(s / 60) % 60).padStart(2, "0")}m`;
};
const count = (n: number) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M` : n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n));
/** A value in one line: text as it is, anything else as compact JSON */
const gist = (v: unknown) =>
  v === undefined || v === null || v === ""
    ? undefined
    : typeof v === "string"
      ? v
      : JSON.stringify(v)
          .replace(/"([^"]+)":/g, "$1: ")
          .replace(/,/g, ", ");
/** Rows a card shows before "+N more" */
const ROWS = 5;

export function RunInspector({ run, supervisor, now: nowProp, startedAt, selected, onSelectedChange, call, onCallChange, formatCost = dollars, className, formatTook, labels: labelsIn }: RunInspectorProps) {
  const L = { ...LABELS, ...labelsIn, facts: { ...LABELS.facts, ...labelsIn?.facts }, state: { ...LABELS.state, ...labelsIn?.state } };
  const took = formatTook ?? tookEn;
  const base = useRef(startedAt ?? Date.now() - (nowProp ?? 0));
  if (startedAt !== undefined) base.current = startedAt;
  const all = useMemo(() => [...walk(run), ...(supervisor ? walk(supervisor) : [])], [run, supervisor]);
  const live = all.some((s) => s.status === "running" || s.status === "waiting");
  const [tick, setTick] = useState(() => Date.now());
  useEffect(() => {
    if (nowProp !== undefined || !live) return;
    const id = window.setInterval(() => setTick(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [nowProp, live]);
  const now = nowProp ?? tick - base.current;
  // Running spans reach up to now; one that hasn't started yet takes no time
  const endOf = (s: RunSpan) => s.end ?? (s.status === "queued" ? s.start : Math.max(now, s.start));

  const [innerSelected, setInnerSelected] = useState<string | null>(null);
  const pickedId = selected !== undefined ? selected : innerSelected;
  const picked = (pickedId && all.find((s) => s.id === pickedId && s.kind === "agent")) || run;
  const pick = (id: string | null) => {
    const next = id === run.id || id === pickedId ? null : id;
    setInnerSelected(next);
    onSelectedChange?.(next);
  };
  const [innerCall, setInnerCall] = useState<string | null>(null);
  const callId = call !== undefined ? call : innerCall;
  const pickCall = (id: string | null) => {
    setInnerCall(id);
    onCallChange?.(id);
  };
  const [more, setMore] = useState<string[]>([]);

  /** The small trace on a card: one row per own call, bars on the agent's own clock */
  const trace = (a: RunSpan) => {
    const calls = ownCalls(a);
    if (!calls.length) return undefined;
    const from = a.start;
    const span = Math.max(1, endOf(a) - from);
    const open = more.includes(a.id);
    const rows = open || calls.length <= ROWS ? calls : calls.slice(0, ROWS - 1);
    return (
      <div className="ui-rinc__trace" role="list" aria-label={L.callsOf(a.name)}>
        {rows.map((c) => {
          const left = ((c.start - from) / span) * 100;
          const width = Math.max(2.5, ((endOf(c) - c.start) / span) * 100);
          const shown = callId === c.id;
          return (
            <div key={c.id} role="listitem" className="ui-rinc__item">
              <button
                type="button"
                className="ui-rinc__row"
                data-kind={c.kind}
                data-status={c.status}
                aria-expanded={shown}
                aria-controls={shown ? `${c.id}-detail` : undefined}
                aria-label={`${c.name}, ${c.status === "running" ? L.state.running : took(endOf(c) - c.start)}${c.status === "failed" ? `, ${L.state.failed}` : ""}`}
                onClick={(e) => {
                  e.stopPropagation();
                  pickCall(callId === c.id ? null : c.id);
                }}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Escape" && shown) pickCall(null);
                }}
              >
                <span className="ui-rinc__icon">{c.icon}</span>
                <span className="ui-rinc__name" dir="auto" title={c.name}>
                  {c.name}
                </span>
                <span className="ui-rinc__track" aria-hidden="true">
                  <span className="ui-rinc__bar" style={{ left: `${Math.min(left, 100 - width)}%`, width: `${width}%` } as CSSProperties} />
                </span>
                <span className="ui-rinc__took">{c.status === "running" ? "…" : took(endOf(c) - c.start)}</span>
              </button>
              {/* The picked call opens right under its row: what it is, what went wrong or came back, and what it was called with */}
              {shown && (
                <div id={`${c.id}-detail`} className="ui-rinc__detail" data-status={c.status}>
                  <p className="ui-rinc__detail-what">
                    <span>{c.kind === "tool" ? (c.tool ?? c.detail ?? L.tool) : (c.model ?? L.model)}</span>
                    <span data-state="">{c.status === "failed" ? L.state.failed : c.status === "running" ? L.state.running : c.status === "done" ? L.state.done : L.state.queued}</span>
                  </p>
                  {c.error ? (
                    <p className="ui-rinc__detail-error" dir="auto">
                      {c.error}
                    </p>
                  ) : (
                    <p className="ui-rinc__detail-out" dir="auto">
                      {gist(c.result ?? c.output) ?? (c.status === "running" ? L.stillRunning : L.noOutput)}
                    </p>
                  )}
                  {gist(c.args ?? c.input) && (
                    <p className="ui-rinc__detail-in" dir="auto">
                      <span>{c.kind === "tool" ? L.calledWith : L.prompt}</span> {gist(c.args ?? c.input)}
                    </p>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {calls.length > ROWS && (
          <button
            type="button"
            className="ui-rinc__more"
            onClick={(e) => {
              e.stopPropagation();
              setMore((m) => (open ? m.filter((x) => x !== a.id) : [...m, a.id]));
            }}
            onKeyDown={(e) => e.stopPropagation()}
          >
            {open ? L.less : L.more(calls.length - ROWS + 1)}
          </button>
        )}
      </div>
    );
  };

  const toTree = (s: RunSpan): TreeAgent => {
    const own = ownCalls(s);
    return {
      id: s.id,
      name: s.name,
      task: s.task ?? s.input ?? "",
      model: s.model,
      avatar: s.icon,
      status: s.status,
      cost: (s.cost ?? 0) + own.reduce((sum, c) => sum + (c.cost ?? 0), 0) || undefined,
      ask: s.ask,
      error: s.error,
      started: base.current + s.start,
      ended: s.end !== undefined ? base.current + s.end : undefined,
      time: s.time,
      extra: trace(s),
      children: (s.children ?? []).filter((c) => c.kind === "agent").map(toTree),
    };
  };

  // The panel's numbers: the picked agent's part of the run, or the whole run
  const part = walk(picked);
  const calls = part.filter((s) => s.kind !== "agent");
  const facts: [string, string, boolean?][] = [
    [L.facts.duration, took(endOf(picked) - picked.start)],
    [L.facts.cost, formatCost(part.reduce((sum, s) => sum + (s.cost ?? 0), 0))],
    [L.facts.tokens, count(part.reduce((sum, s) => sum + (s.tokensIn ?? 0) + (s.tokensOut ?? 0), 0))],
    [L.facts.modelCalls, String(calls.filter((c) => c.kind === "llm").length)],
    [L.facts.toolCalls, String(calls.filter((c) => c.kind === "tool").length)],
    [L.facts.errors, String(part.filter((s) => s.status === "failed").length), part.some((s) => s.status === "failed")],
  ];

  return (
    <section className={["ui-rinc", className].filter(Boolean).join(" ")} aria-label={run.task ?? run.name}>
      <style href="ui-run-inspector-clean" precedence="default">
        {css}
      </style>
      <AgentTree
        root={toTree(run)}
        beside={supervisor ? toTree(supervisor) : undefined}
        now={base.current + now}
        selected={picked.id === run.id ? null : picked.id}
        onSelect={(a) => pick(a.id)}
        formatCost={formatCost}
        title={run.task ?? L.title}
        labels={L.tree}
        top={
          /* The numbers of the picked part, right under the title – the cards below never move for it */
          <section className="ui-rinc__card" aria-label={picked.id === run.id ? L.whole : picked.name}>
            <div className="ui-rinc__card-head">
              <dl className="ui-rinc__facts">
                {facts.map(([k, v, bad]) => (
                  <div key={k}>
                    <dt>{k}</dt>
                    <dd data-bad={bad ? "" : undefined}>{v}</dd>
                  </div>
                ))}
              </dl>
            </div>
          </section>
        }
      />
    </section>
  );
}

const css = /* css */ `
.ui-rinc {
  --ui-rinc-card: var(--aui-surface, #ffffff);
  --ui-rinc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-rinc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-rinc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-rinc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-rinc-track: rgba(0, 0, 0, 0.04);
  --ui-rinc-fill: var(--aui-subtle, rgba(0, 0, 0, 0.05));
  --ui-rinc-picked: rgba(0, 96, 226, 0.06);
  --ui-rinc-llm-bg: color-mix(in srgb, var(--aui-purple, #a651f0) 14%, transparent);
  --ui-rinc-llm-edge: color-mix(in srgb, var(--aui-purple, #a651f0) 38%, transparent);
  --ui-rinc-tool-bg: color-mix(in srgb, var(--aui-cyan, #14b3f0) 12%, transparent);
  --ui-rinc-tool-edge: color-mix(in srgb, var(--aui-cyan, #14b3f0) 34%, transparent);
  --ui-rinc-error-bg: #f52b0020;
  --ui-rinc-error-edge: color-mix(in srgb, var(--aui-red, #f04438) 42%, transparent);
  --ui-rinc-error-fg: #d72400da;
  --ui-rinc-stroke: var(--aui-shadow-card, 0 0 0 1px rgba(0, 0, 0, 0.08), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-rinc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-rinc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-rinc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  --ui-rinc-details-w: 680px;
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  container-type: inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  color: var(--ui-rinc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-rinc *, .ui-rinc *::before, .ui-rinc *::after { box-sizing: border-box; }
:where(.ui-rinc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-rinc :is(.ui-rinc__row, .ui-rinc__more):focus-visible { outline: 2px solid var(--ui-rinc-focus); outline-offset: -2px; }
.ui-rinc p, .ui-rinc dl, .ui-rinc dd { margin: 0; }
.ui-rinc svg { display: block; flex: none; }
/* Wider cards, so a trace fits on them */
.ui-rinc .ui-agtc { --ui-agtc-card-w: 292px; }
/* The run's title and totals span as wide as the numbers under them */
.ui-rinc .ui-agtc__bar { width: min(100%, var(--ui-rinc-details-w)); margin-inline: auto; }

/* ----- The trace on a card ----- */
.ui-rinc__trace { display: grid; gap: 1px; margin: 2px -6px 0; padding-top: 6px; box-shadow: inset 0 0.5px 0 var(--ui-rinc-line); }
.ui-rinc__row { display: grid; grid-template-columns: 16px minmax(0, 1fr) 60px 46px; align-items: center; gap: 6px; height: 24px; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: 12px; line-height: 16px; color: var(--ui-rinc-text); transition: background-color 140ms ease, color 140ms ease; }
.ui-rinc__row:hover { background: var(--ui-rinc-fill); color: var(--ui-rinc-ink); }
.ui-rinc__row[aria-expanded="true"] { background: var(--ui-rinc-picked); color: var(--ui-rinc-ink); }
.ui-rinc__icon { display: grid; width: 16px; height: 16px; place-items: center; }
.ui-rinc__icon > svg { width: 12px; height: 12px; }
.ui-rinc__name { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-rinc__track { position: relative; height: 8px; border-radius: 3px; background: var(--ui-rinc-track); overflow: hidden; }
/* A bar per call: a light tint and a hairline in its kind's color, red when it failed */
.ui-rinc__bar { position: absolute; top: 0; bottom: 0; border-radius: 3px; background: var(--ui-rinc-tool-bg); box-shadow: inset 0 0 0 0.5px var(--ui-rinc-tool-edge); transition: left 400ms var(--ui-rinc-ease), width 400ms var(--ui-rinc-ease); }
.ui-rinc__row[data-kind="llm"] .ui-rinc__bar { background: var(--ui-rinc-llm-bg); box-shadow: inset 0 0 0 0.5px var(--ui-rinc-llm-edge); }
.ui-rinc__row[data-status="failed"] .ui-rinc__bar { background: var(--ui-rinc-error-bg); box-shadow: inset 0 0 0 0.5px var(--ui-rinc-error-edge); }
.ui-rinc__row[data-status="failed"] .ui-rinc__took { color: var(--ui-rinc-error-fg); }
/* While it runs, a sheen sweeps across the bar and its leading edge glows */
.ui-rinc__row[data-status="running"] .ui-rinc__bar { background-image: linear-gradient(90deg, transparent, rgba(255, 255, 255, 0.7), transparent); background-size: 40px 100%; background-repeat: no-repeat; animation: ui-rinc-sweep 1.3s linear infinite; }
.ui-rinc__row[data-status="running"] .ui-rinc__bar::after { content: ""; position: absolute; top: 0; right: 0; bottom: 0; width: 6px; border-radius: 3px; background: linear-gradient(90deg, transparent, var(--ui-rinc-tool-edge)); }
.ui-rinc__row[data-kind="llm"][data-status="running"] .ui-rinc__bar::after { background: linear-gradient(90deg, transparent, var(--ui-rinc-llm-edge)); }
.ui-rinc__took { font-variant-numeric: tabular-nums; color: var(--ui-rinc-muted); text-align: right; white-space: nowrap; }
.ui-rinc__more { justify-self: start; height: 22px; margin-left: 2px; padding: 0 4px; border-radius: var(--aui-radius-sm, 5px); font-size: 11.5px; color: var(--ui-rinc-muted); }
.ui-rinc__more:hover { color: var(--ui-rinc-ink); }

/* ----- The numbers under the title ----- */
/* One fixed width – it doesn't follow the agents as they come and go – with a clear hairline all around */
.ui-rinc__card { display: grid; width: min(100%, var(--ui-rinc-details-w)); margin-inline: auto; min-width: 0; border-radius: var(--aui-radius-lg, 10px); background: var(--ui-rinc-card); box-shadow: var(--ui-rinc-stroke); }
.ui-rinc__card-head { padding: 12px 20px; }
.ui-rinc__facts { display: grid; grid-template-columns: repeat(6, minmax(0, 1fr)); gap: 12px 28px; }
.ui-rinc__facts dt { font-size: 11px; line-height: 16px; color: var(--ui-rinc-muted); }
.ui-rinc__facts dd { font-size: 13px; font-weight: 500; line-height: 18px; font-variant-numeric: tabular-nums; }
.ui-rinc__facts dd[data-bad] { color: var(--ui-rinc-error-fg); }
/* A picked call opens right under its row, in a quiet inset */
.ui-rinc__item { display: grid; min-width: 0; }
.ui-rinc__detail { display: grid; gap: 2px; margin: 2px 0 6px; padding: 8px 10px 9px; border-radius: calc(var(--aui-radius-lg, 10px) - 3px); background: var(--ui-rinc-fill); font-size: 12px; line-height: 17px; animation: ui-rinc-open 260ms var(--ui-rinc-ease) both; }
.ui-rinc__detail-what { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
.ui-rinc__detail-what > span:first-child { min-width: 0; overflow: hidden; font-family: var(--aui-font-mono, ui-monospace, "SF Mono", Menlo, monospace); font-size: 11px; color: var(--ui-rinc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-rinc__detail-what > [data-state] { flex: none; margin-left: auto; font-size: 11px; color: var(--ui-rinc-muted); }
.ui-rinc__detail[data-status="failed"] [data-state] { font-weight: 500; color: var(--ui-rinc-error-fg); }
.ui-rinc__detail-error { color: var(--ui-rinc-error-fg); overflow-wrap: anywhere; }
.ui-rinc__detail-out { color: var(--ui-rinc-ink); display: -webkit-box; overflow: hidden; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow-wrap: anywhere; }
.ui-rinc__detail-in { overflow: hidden; font-family: var(--aui-font-mono, ui-monospace, "SF Mono", Menlo, monospace); font-size: 11px; color: var(--ui-rinc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-rinc__detail-in > span { font-family: var(--aui-font, "Inter", system-ui, sans-serif); color: var(--ui-rinc-faint); }
@container (max-width: 720px) {
  .ui-rinc__facts { grid-template-columns: repeat(3, minmax(0, 1fr)); }
}
@keyframes ui-rinc-sweep { from { background-position: -40px 0; } to { background-position: calc(100% + 40px) 0; } }
@keyframes ui-rinc-open { from { opacity: 0; transform: translateY(-3px); } to { opacity: 1; transform: none; } }
@media (prefers-reduced-motion: reduce) {
  .ui-rinc *, .ui-rinc *::before, .ui-rinc *::after { animation: none !important; transition: none !important; }
}
:where(.dark, [data-theme="dark"]) .ui-rinc {
  --ui-rinc-card: var(--aui-surface, #1c1c1f);
  --ui-rinc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-rinc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-rinc-muted: var(--aui-muted, rgba(255, 255, 255, 0.45));
  --ui-rinc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.28));
  --ui-rinc-track: rgba(255, 255, 255, 0.05);
  --ui-rinc-fill: var(--aui-subtle, rgba(255, 255, 255, 0.07));
  --ui-rinc-picked: color-mix(in srgb, var(--aui-blue, #4fb0fe) 12%, transparent);
  --ui-rinc-llm-bg: color-mix(in srgb, var(--aui-purple, #b98af0) 22%, transparent);
  --ui-rinc-llm-edge: color-mix(in srgb, var(--aui-purple, #b98af0) 50%, transparent);
  --ui-rinc-tool-bg: color-mix(in srgb, var(--aui-cyan, #5cc6ee) 20%, transparent);
  --ui-rinc-tool-edge: color-mix(in srgb, var(--aui-cyan, #5cc6ee) 48%, transparent);
  --ui-rinc-error-bg: color-mix(in srgb, var(--aui-red, #ff6b5f) 20%, transparent);
  --ui-rinc-error-edge: color-mix(in srgb, var(--aui-red, #ff6b5f) 50%, transparent);
  --ui-rinc-error-fg: var(--aui-danger, #ff8a7a);
  --ui-rinc-stroke: var(--aui-shadow-card, 0 0 0 1px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-rinc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-rinc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
}
`;
