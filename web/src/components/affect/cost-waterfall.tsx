/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { useT } from "@/i18n";

/*
 * Cost Waterfall – where the money and the tokens of an agent run go, in a single self-contained file.
 * A waterfall chart turned into a table: every step of the run – LLM call, tool call or sub-agent – is a row with its
 * model, tokens in and out, cost and time, and a bar that starts where the step before it ended. Read down the rows
 * and the bars step across to the run's total, so the expensive steps are the long ones. A white card on a soft gray tray.
 *   · the total cost, the total tokens and the most expensive step on top
 *   · a switch between cost and tokens – the bars glide to their new place; in tokens, each bar shows its output part
 *   · sub-agents are indented and fold; their bar spans everything they spent
 *   · click a row (or press Enter) and it opens the prompt and response, or the tool's input and output
 *   · live runs: new steps slide in, a running step grows with a soft shimmer, the axis rescales smoothly
 * Narrow containers drop the token columns, then stack the bar under its name. The styles are injected below.
 */

export type CostStepKind = "agent" | "llm" | "tool";
export type CostStepStatus = "running" | "done" | "failed";
export type CostMode = "cost" | "tokens";

export type CostStep = {
  id: string;
  name: string;
  /** Steve: the name where the step is shown out of its place (the most expensive step on top), e.g. with its sub-agent */
  fullName?: string;
  kind: CostStepKind;
  status: CostStepStatus;
  /** The model of an LLM call or a sub-agent – shown muted after the name */
  model?: string;
  /** A few muted words after the name when there is no model, e.g. the tool's target */
  detail?: string;
  /** A small mark, e.g. the model maker's or the tool's logo – defaults to a swatch in the kind's color */
  icon?: ReactNode;
  /** Its own tokens and cost – a sub-agent's children are added on top, so don't pre-sum */
  tokensIn?: number;
  tokensOut?: number;
  cost?: number;
  /** Steve: tokens with no price, left out of `cost` – the cost cell and the totals mark them */
  unpriced?: number;
  /** Steve: its tokens when the in/out split isn't known – counted in the tokens view, In and Out show "–" */
  tokens?: number;
  /** How long it took (or runs so far), in ms */
  duration?: number;
  /** The prompt of an LLM call, the input of a tool, the task of a sub-agent – an excerpt for the details */
  input?: string;
  /** The response, the tool's output, the sub-agent's result */
  output?: string;
  /** What went wrong – shown in the details of a failed step */
  error?: string;
  children?: CostStep[];
};

export type CostWaterfallProps = {
  /** The run's steps in order – sub-agents hold theirs as children */
  steps: CostStep[];
  /** What the run is about – shown muted next to the title */
  task?: string;
  /** Overrides the state in the bar – by default "Live" while a step runs, else "Done" or "Failed" */
  status?: CostStepStatus;
  /** Controlled view: the bars measure cost or tokens */
  mode?: CostMode;
  defaultMode?: CostMode;
  onModeChange?: (mode: CostMode) => void;
  /** Controlled ids of folded sub-agents */
  collapsed?: string[];
  defaultCollapsed?: string[];
  onCollapsedChange?: (collapsed: string[]) => void;
  /** Controlled id of the step whose details are open */
  openId?: string | null;
  defaultOpenId?: string | null;
  onOpenChange?: (id: string | null, step?: CostStep) => void;
  /** Formats every cost – defaults to dollars, e.g. "$0.042" */
  formatCost?: (amount: number) => string;
  /** Long runs: the rows scroll past this height (px) – the totals, the switch and the column heads stay */
  maxHeight?: number;
  title?: string;
  className?: string;
  /** Steve: inside a panel – no title bar and no gray tray; the panel's head holds the title and the switch */
  bare?: boolean;
  /** Steve: token and duration formats of the page (defaults: 1.2k, 3m 05s) */
  formatTokens?: (n: number) => string;
  formatDuration?: (ms: number) => string;
  /** Steve: what a step's details show instead of its input and output texts */
  renderDetail?: (step: CostStep) => ReactNode;
  /** Steve: the step's own row attributes for tests (`data-*`) */
  rowProps?: (step: CostStep) => Record<string, string | undefined>;
};

/* ---------- Helpers ---------- */

type T = ReturnType<typeof useT>;
/* Steve: labels from the stats locale, built at render so they follow the language */
const KIND = (t: T): Record<CostStepKind, string> => ({ agent: t("stats:cwf.kind.agent"), llm: t("stats:cwf.kind.llm"), tool: t("stats:cwf.kind.tool") });
const STATUS = (t: T): Record<CostStepStatus, string> => ({ running: t("stats:cwf.status.running"), done: t("stats:cwf.status.done"), failed: t("stats:cwf.status.failed") });
/** What the two texts of a step are called in its details */
const IO = (t: T): Record<CostStepKind, [string, string]> => ({ agent: [t("stats:cwf.io.task"), t("stats:cwf.io.result")], llm: [t("stats:cwf.io.prompt"), t("stats:cwf.io.response")], tool: [t("stats:cwf.io.input"), t("stats:cwf.io.output")] });
/** What a step's details say while its result is still coming */
const WAITING = (t: T): Record<CostStepKind, string> => ({ agent: t("stats:cwf.waiting.agent"), llm: t("stats:cwf.waiting.llm"), tool: t("stats:cwf.waiting.tool") });
/** Long texts scroll inside their box – only then are they a tab stop, so the keyboard can scroll them too */
const scrollable = (el: HTMLElement | null) => {
  if (!el) return;
  if (el.scrollHeight > el.clientHeight + 1) el.tabIndex = 0;
  else el.removeAttribute("tabindex");
};
/** Motion: the axis rescale while a run grows, and how long the bars glide when the view switches */
const RESCALE = 720;
const SWITCH = 560;
const STAGGER = 36;

const dollars = (n: number): string => (n < 0 ? `−${dollars(-n)}` : n === 0 ? "$0" : n < 0.001 ? "<$0.001" : n < 0.01 ? `$${n.toFixed(3)}` : n < 10 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString("en-US")}`);
const tokens0 = (n: number) =>
  n < 1000 ? String(Math.round(n)) : n < 100_000 ? `${(n / 1000).toFixed(1)}k` : n < 999_500 ? `${Math.round(n / 1000)}k` : `${(n / 1_000_000).toFixed(1)}M`;
const took0 = (ms: number) => {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))}ms`;
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  // Long runs: hours, then days – never "26h 00m"
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h ${String(m % 60).padStart(2, "0")}m` : `${Math.floor(h / 24)}d ${h % 24}h`;
};
/** A number the API really sent – NaN, Infinity and missing values count as not there */
const real = (v?: number) => (v !== undefined && Number.isFinite(v) ? v : undefined);
const walk = (s: CostStep): CostStep[] => [s, ...(s.children ?? []).flatMap(walk)];
const costOf = (s: CostStep): number => (real(s.cost) ?? 0) + (s.children ?? []).reduce((sum, c) => sum + costOf(c), 0);
/** In, out, and (Steve) tokens whose split isn't known */
const tokensOf = (s: CostStep): [number, number, number] =>
  (s.children ?? []).reduce<[number, number, number]>(
    (sum, c) => {
      const [i, o, x] = tokensOf(c);
      return [sum[0] + i, sum[1] + o, sum[2] + x];
    },
    [Math.max(0, real(s.tokensIn) ?? 0), Math.max(0, real(s.tokensOut) ?? 0), Math.max(0, real(s.tokens) ?? 0)],
  );
const unpricedOf = (s: CostStep): number => Math.max(0, real(s.unpriced) ?? 0) + (s.children ?? []).reduce((sum, c) => sum + unpricedOf(c), 0);
const hasCost = (s: CostStep): boolean => real(s.cost) !== undefined || (s.children ?? []).some(hasCost);
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const ease = (p: number) => (p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2);

/** A round axis end a little past the total, and round steps between its ticks */
const niceCeil = (v: number) => {
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  return ([1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10].find((m) => m * p >= v - 1e-12) ?? 10) * p;
};
const niceStep = (range: number, fit: number) => {
  const raw = range / fit;
  const p = Math.pow(10, Math.floor(Math.log10(raw)));
  return ([1, 2, 5, 10].find((m) => m * p >= raw - 1e-12) ?? 10) * p;
};

type Row = {
  step: CostStep;
  depth: number;
  parentId?: string;
  hidden: boolean;
  start: number;
  size: number;
  split: number;
  credit: boolean;
};

/* ---------- Icons ---------- */

const Spark = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
/* Chevron and warning: Hugeicons Free (MIT, © Hugeicons, hugeicons.com); the loader is a plain arc */
const Chevron = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M18 9.00005C18 9.00005 13.5811 15 12 15C10.4188 15 6 9 6 9" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </svg>
);
const Warning = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path
      d="M13.9248 21H10.0752C5.44476 21 3.12955 21 2.27636 19.4939C1.42317 17.9879 2.60736 15.9914 4.97574 11.9985L6.90057 8.75333C9.17559 4.91778 10.3131 3 12 3C13.6869 3 14.8244 4.91777 17.0994 8.75332L19.0243 11.9985C21.3926 15.9914 22.5768 17.9879 21.7236 19.4939C20.8704 21 18.5552 21 13.9248 21Z"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="2"
    />
    <path d="M12 9V13" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.2" />
    <path d="M12 16.8V17" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.6" />
  </svg>
);
/* Steve: unpriced tokens – a dashed coin */
const Unpriced = () => (
  <svg width="11" height="11" viewBox="0 0 12 12" fill="none" aria-hidden="true">
    <circle cx="6" cy="6" r="4.6" stroke="currentColor" strokeWidth="1.3" strokeDasharray="2 1.6" />
    <path d="M6 3.9v4.2" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
  </svg>
);
const Spinner = () => (
  <svg className="ui-cwf__spin" width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <circle cx="12" cy="12" r="8.5" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2.5" />
    <path d="M12 3.5A8.5 8.5 0 0 1 20.5 12" stroke="currentColor" strokeLinecap="round" strokeWidth="2.5" />
  </svg>
);

export function CostWaterfall({
  steps,
  task,
  status,
  mode: modeProp,
  defaultMode = "cost",
  onModeChange,
  collapsed,
  defaultCollapsed = [],
  onCollapsedChange,
  openId,
  defaultOpenId = null,
  onOpenChange,
  formatCost = dollars,
  maxHeight,
  title,
  className,
  bare = false,
  formatTokens = tokens0,
  formatDuration = took0,
  renderDetail,
  rowProps,
}: CostWaterfallProps) {
  const t = useT();
  const tokens = formatTokens, took = formatDuration;
  const KINDS = KIND(t), STATES = STATUS(t);
  title ??= t("stats:cwf.title");
  const rootRef = useRef<HTMLDivElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const rowEls = useRef(new Map<string, HTMLDivElement>());

  /* ---------- The view, folding and the open details – controlled or not ---------- */

  const [innerMode, setInnerMode] = useState<CostMode>(defaultMode);
  const mode = modeProp ?? innerMode;
  const setMode = (m: CostMode) => {
    if (m === mode) return;
    setInnerMode(m);
    onModeChange?.(m);
  };
  const [innerCollapsed, setInnerCollapsed] = useState<string[]>(defaultCollapsed);
  const folded = collapsed ?? innerCollapsed;
  const toggle = (id: string) => {
    const next = folded.includes(id) ? folded.filter((c) => c !== id) : [...folded, id];
    setInnerCollapsed(next);
    onCollapsedChange?.(next);
  };
  const all = steps.flatMap(walk);
  const [innerOpen, setInnerOpen] = useState<string | null>(defaultOpenId);
  const wantOpen = openId === undefined ? innerOpen : openId;
  const openStep = wantOpen ? all.find((s) => s.id === wantOpen) : undefined;
  const setOpen = (id: string | null) => {
    setInnerOpen(id);
    onOpenChange?.(id, id ? all.find((s) => s.id === id) : undefined);
  };

  /* ---------- The rows: the tree, flat, each bar starting where the one before it ended ---------- */

  const valueOf = (s: CostStep) => {
    if (mode === "cost") return real(s.cost) ?? 0;
    return Math.max(0, real(s.tokensIn) ?? 0) + Math.max(0, real(s.tokensOut) ?? 0) + Math.max(0, real(s.tokens) ?? 0);
  };
  const flat: Row[] = [];
  let sum = 0;
  // The highest the running total gets – a credit (a negative cost) steps the waterfall back below it
  let peak = 0;
  const visit = (s: CostStep, depth: number, parentId: string | undefined, hidden: boolean) => {
    const row: Row = {
      step: s,
      depth,
      parentId,
      hidden,
      start: sum,
      size: 0,
      split: 0,
      credit: false,
    };
    flat.push(row);
    // A sub-agent's own spend comes first, then its children's – its bar spans all of it
    sum += valueOf(s);
    peak = Math.max(peak, sum);
    const shut = hidden || folded.includes(s.id);
    for (const c of s.children ?? []) visit(c, depth + 1, s.id, shut);
    // A credit's bar runs backwards: from where the total ends up to where it was
    row.credit = sum < row.start;
    row.size = Math.abs(sum - row.start);
    row.start = Math.max(0, Math.min(row.start, sum));
    const [i, o, x] = tokensOf(s);
    row.split = mode === "tokens" && i + o + x > 0 ? o / (i + o + x) : 0;
  };
  for (const s of steps) visit(s, 0, undefined, false);
  const shownRows = flat.filter((r) => !r.hidden);

  /* ---------- The axis: a round end past the total, ticks to fit the width ---------- */

  const [trackW, setTrackW] = useState(0);
  const range = niceCeil(Math.max(mode === "cost" ? 0.1 : 10_000, peak * 1.08));
  const step = niceStep(range, Math.min(6, Math.max(2, Math.round((trackW || 400) / 90))));
  const ticks: number[] = [];
  for (let v = 0; v < range - step * 0.01; v += step) ticks.push(Number(v.toPrecision(6)));
  const tickLabel = (v: number) => (v === 0 ? (mode === "cost" ? formatCost(0) : "0") : mode === "cost" ? formatCost(v) : tokens(v));

  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    const ro = new ResizeObserver(() => setTrackW(layer.offsetWidth));
    ro.observe(layer);
    return () => ro.disconnect();
  }, []);

  // While the run grows the range glides to its new end, written straight into a CSS variable every frame. A switch
  // between cost and tokens jumps it instead – the bars glide there on their own transition
  const [switching, setSwitching] = useState(false);
  const shownRange = useRef(range);
  const rangeRaf = useRef(0);
  const lastMode = useRef(mode);
  const firstRange = useRef(true);
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const set = (v: number) => {
      shownRange.current = v;
      el.style.setProperty("--ui-cwf-range", String(v));
    };
    cancelAnimationFrame(rangeRaf.current);
    const switched = lastMode.current !== mode;
    lastMode.current = mode;
    if (firstRange.current || switched || reduced()) {
      firstRange.current = false;
      set(range);
      return;
    }
    const from = shownRange.current;
    if (from === range) return;
    const t0 = performance.now();
    const frame = () => {
      const p = Math.min(1, (performance.now() - t0) / RESCALE);
      set(from + (range - from) * ease(p));
      if (p < 1) rangeRaf.current = requestAnimationFrame(frame);
    };
    rangeRaf.current = requestAnimationFrame(frame);
  }, [range, mode]);
  useEffect(() => () => cancelAnimationFrame(rangeRaf.current), []);
  const firstMode = useRef(true);
  useLayoutEffect(() => {
    if (firstMode.current || reduced()) {
      firstMode.current = false;
      return;
    }
    setSwitching(true);
    const t = window.setTimeout(() => setSwitching(false), SWITCH + 40);
    return () => window.clearTimeout(t);
  }, [mode]);

  // Rows that appear together slide in one after another; the first picture comes in row by row
  const enterDelay = useRef(new Map<string, number>());
  const mounted = useRef(false);
  let newcomers = 0;
  for (const r of flat) {
    if (enterDelay.current.has(r.step.id)) continue;
    enterDelay.current.set(r.step.id, Math.min(400, (mounted.current ? 0 : 60) + newcomers++ * STAGGER));
  }
  useLayoutEffect(() => {
    mounted.current = true;
    const here = new Set(flat.map((r) => r.step.id));
    for (const id of [...enterDelay.current.keys()]) if (!here.has(id)) enterDelay.current.delete(id);
  });

  /* ---------- The details: open under their row, close the same way ---------- */

  const [closing, setClosing] = useState<string | null>(null);
  const lastOpen = useRef<string | undefined>(undefined);
  useLayoutEffect(() => {
    const was = lastOpen.current;
    lastOpen.current = openStep?.id;
    if (was && was !== openStep?.id && !reduced()) {
      setClosing(was);
      const t = window.setTimeout(() => setClosing(null), 240);
      return () => window.clearTimeout(t);
    }
  }, [openStep?.id]);

  /* ---------- Keyboard: one row in the tab order, arrows move, left and right fold ---------- */

  const [active, setActive] = useState<string | undefined>(undefined);
  // A row to focus once it's rendered – e.g. after "Most expensive" unfolded its sub-agents
  const focusAfter = useRef<string | null>(null);
  useEffect(() => {
    const id = focusAfter.current;
    if (!id) return;
    focusAfter.current = null;
    rowEls.current.get(id)?.focus({ preventScroll: true });
    rowEls.current.get(id)?.scrollIntoView({ block: "nearest", behavior: reduced() ? "auto" : "smooth" });
  });
  const findRow = (id?: string) => flat.find((r) => r.step.id === id);
  let activeId = active;
  // A row that folded away or left hands the focus to its nearest shown parent
  for (let r = findRow(activeId); r && r.hidden; r = findRow(r.parentId)) activeId = r.parentId;
  if (!shownRows.some((r) => r.step.id === activeId)) activeId = shownRows[0]?.step.id;
  const focusRow = (r?: Row) => {
    if (!r) return;
    setActive(r.step.id);
    rowEls.current.get(r.step.id)?.focus();
  };
  const onKey = (e: KeyboardEvent<HTMLDivElement>, r: Row) => {
    if (e.target !== e.currentTarget) return;
    const i = shownRows.indexOf(r);
    const kids = (r.step.children ?? []).length > 0;
    const isFolded = folded.includes(r.step.id);
    const keys: Record<string, () => void> = {
      ArrowDown: () => focusRow(shownRows[i + 1]),
      ArrowUp: () => focusRow(shownRows[i - 1]),
      Home: () => focusRow(shownRows[0]),
      End: () => focusRow(shownRows[shownRows.length - 1]),
      ArrowRight: () => (kids && isFolded ? toggle(r.step.id) : kids ? focusRow(shownRows[i + 1]) : undefined),
      ArrowLeft: () => (kids && !isFolded ? toggle(r.step.id) : focusRow(findRow(r.parentId))),
      Enter: () => setOpen(wantOpen === r.step.id ? null : r.step.id),
      " ": () => setOpen(wantOpen === r.step.id ? null : r.step.id),
      Escape: () => wantOpen && setOpen(null),
    };
    if (!keys[e.key]) return;
    e.preventDefault();
    keys[e.key]();
  };

  /* ---------- The run in numbers ---------- */

  const live = all.some((s) => s.status === "running");
  const state: CostStepStatus = status ?? (live ? "running" : all.some((s) => s.status === "failed") && steps.at(-1)?.status === "failed" ? "failed" : "done");
  const [tin, tout, tx] = steps.reduce<[number, number, number]>(
    (acc, s) => {
      const [i, o, x] = tokensOf(s);
      return [acc[0] + i, acc[1] + o, acc[2] + x];
    },
    [0, 0, 0],
  );
  const unpricedAll = steps.reduce((acc, s) => acc + unpricedOf(s), 0);
  /* Steve: a cost that leaves out unpriced tokens says so where it is shown – a mark with the count on hover */
  const unpricedMark = (n: number) =>
    n > 0 ? (
      <span className="ui-cwf__unpriced" title={t("stats:cwf.unpricedMark", { n: tokens(n) })} data-unpriced-mark="">
        <Unpriced />
      </span>
    ) : null;
  const leaves = all.filter((s) => s.kind !== "agent" && (real(s.cost) ?? 0) > 0);
  // The first of equals wins; the "Top" flag only shows when there is something to compare with
  const top = leaves.length ? leaves.reduce((a, b) => ((real(b.cost) ?? 0) > (real(a.cost) ?? 0) ? b : a)) : undefined;
  const flagTop = leaves.length > 1;
  const totalCost = steps.reduce((acc, s) => acc + costOf(s), 0);

  const details = (s: CostStep, leaving: boolean, depth: number) => {
    const [inLabel, outLabel] = IO(t)[s.kind];
    const mono = s.kind === "tool";
    return (
      <div
        className="ui-cwf__detail"
        data-closing={leaving ? "" : undefined}
        role="region"
        aria-label={t("stats:cwf.detailsAria", { name: s.name })}
        onKeyDown={(e) => {
          // Esc from inside the details closes them and goes back to the row
          if (e.key !== "Escape") return;
          e.stopPropagation();
          setOpen(null);
          rowEls.current.get(s.id)?.focus();
        }}
      >
        <div className="ui-cwf__detail-in" style={{ "--ui-cwf-level": depth } as CSSProperties}>
          {/* The tree's lines run on through the details, so a nested step doesn't cut them */}
          {depth > 0 && <span className="ui-cwf__guides" aria-hidden="true" />}
          {s.error && (
            <p className="ui-cwf__error">
              <Warning />
              <span dir="auto">{s.error}</span>
            </p>
          )}
          {renderDetail ? renderDetail(s) : <div className="ui-cwf__io">
            <div>
              <span>{inLabel}</span>
              <p dir="auto" data-mono={mono ? "" : undefined} ref={scrollable}>
                {s.input ?? "–"}
              </p>
            </div>
            <div>
              <span>{outLabel}</span>
              <p dir="auto" data-mono={mono ? "" : undefined} data-live={s.status === "running" ? "" : undefined} ref={scrollable}>
                {s.output ?? (s.status === "running" ? WAITING(t)[s.kind] : "–")}
              </p>
            </div>
          </div>}
        </div>
      </div>
    );
  };

  return (
    <div ref={rootRef} className={["ui-cwf", className].filter(Boolean).join(" ")} data-mode={mode} data-switching={switching ? "" : undefined} data-bare={bare ? "" : undefined}>
      <style href="ui-cost-waterfall" precedence="default">
        {css}
      </style>

      {!bare && <header className="ui-cwf__head">
        <span className="ui-cwf__spark">
          <Spark />
        </span>
        <span className="ui-cwf__heading">{title}</span>
        {task && (
          <span className="ui-cwf__task" dir="auto" title={task}>
            {task}
          </span>
        )}
        <span className="ui-cwf__switch" role="group" aria-label={t("stats:cwf.measureBy")}>
          {(["cost", "tokens"] as const).map((m) => (
            <button key={m} type="button" aria-pressed={mode === m} onClick={() => setMode(m)}>
              {m === "cost" ? t("stats:cwf.cost") : t("stats:cwf.tokens")}
            </button>
          ))}
        </span>
        <span className="ui-cwf__pill" data-status={state} aria-live="polite">
          {state === "running" && <span className="ui-cwf__pulse" aria-hidden="true" />}
          {state === "running" ? t("stats:cwf.live") : STATES[state]}
        </span>
      </header>}

      <div className="ui-cwf__card">
      <dl className="ui-cwf__stats">
        <div className="ui-cwf__stat">
          <dt>{t("stats:cwf.totalCost")}</dt>
          <dd>
            <span className="ui-cwf__amount" data-live={live ? "" : undefined}>
              {formatCost(totalCost)}
            </span>
            {unpricedAll > 0 && <span className="ui-cwf__sub ui-cwf__sub--unpriced" data-unpriced-total="">{t("stats:cwf.unpricedTotal", { n: tokens(unpricedAll) })}</span>}
          </dd>
        </div>
        <div className="ui-cwf__stat">
          <dt>{t("stats:cwf.totalTokens")}</dt>
          <dd>
            <span className="ui-cwf__amount" data-live={live ? "" : undefined}>
              {tokens(tin + tout + tx)}
            </span>
            <span className="ui-cwf__sub">
              {t("stats:cwf.inOut", { in: tokens(tin), out: tokens(tout) })}
            </span>
          </dd>
        </div>
        <div className="ui-cwf__stat">
          <dt>{t("stats:cwf.mostExpensive")}</dt>
          <dd>
            {top ? (
              <button
                type="button"
                className="ui-cwf__top"
                onClick={() => {
                  // Unfold its sub-agents, so the row is there to open
                  const path: string[] = [];
                  for (let r = findRow(top.id); r?.parentId; r = findRow(r.parentId)) path.push(r.parentId);
                  const shut = path.filter((id) => folded.includes(id));
                  if (shut.length) {
                    const next = folded.filter((id) => !shut.includes(id));
                    setInnerCollapsed(next);
                    onCollapsedChange?.(next);
                  }
                  setActive(top.id);
                  setOpen(top.id);
                  // Keyboard users land on the row they asked for, not back at the top
                  focusAfter.current = top.id;
                }}
              >
                <span className="ui-cwf__top-name" dir="auto" title={top.fullName ?? top.name}>
                  {top.fullName ?? top.name}
                </span>
                <span className="ui-cwf__sub">{unpricedMark(unpricedOf(top))}{formatCost(real(top.cost) ?? 0)}</span>
              </button>
            ) : (
              <span className="ui-cwf__sub">–</span>
            )}
          </dd>
        </div>
      </dl>

      <div className="ui-cwf__body">
        {/* Ticks and grid lines run behind the rows – positioned against the gliding range */}
        <div ref={layerRef} className="ui-cwf__layer" aria-hidden="true">
          {ticks.map((v) => (
            <span key={`${mode}:${v}`} className="ui-cwf__tick" data-zero={v === 0 ? "" : undefined} style={{ "--v": v } as CSSProperties}>
              <span>{tickLabel(v)}</span>
            </span>
          ))}
        </div>
        <div className="ui-cwf__cols" aria-hidden="true">
          <span>{t("stats:cwf.step")}</span>
          <span />
          <span data-col="in" title={t("stats:cwf.inDef")}>{t("stats:cwf.in")}</span>
          <span data-col="out">{t("stats:cwf.out")}</span>
          <span data-col="cost">{t("stats:cwf.cost")}</span>
          <span data-col="tok">{t("stats:cwf.tokens")}</span>
          <span data-col="time">{t("stats:cwf.time")}</span>
        </div>

        {!flat.length && <p className="ui-cwf__empty">{state === "running" ? t("stats:cwf.waitingFirst") : t("stats:cwf.empty")}</p>}
        <div className="ui-cwf__rows" role="tree" aria-label={title} hidden={!flat.length} data-scroll={maxHeight ? "" : undefined} style={maxHeight ? { maxHeight } : undefined}>
          {flat.map((r) => {
            const s = r.step;
            const kids = (s.children ?? []).length > 0;
            const isFolded = kids && folded.includes(s.id);
            const open = wantOpen === s.id;
            const [i, o, x] = tokensOf(s);
            const cost = hasCost(s) ? costOf(s) : undefined;
            const unpriced = unpricedOf(s);
            const running = s.status === "running";
            const anyRunning = walk(s).some((x) => x.status === "running");
            const hiddenCount = isFolded ? walk(s).length - 1 : 0;
            const label = [
              s.name,
              KINDS[s.kind],
              STATES[s.status],
              cost !== undefined ? formatCost(cost) : "",
              unpriced > 0 ? t("stats:cwf.unpricedMark", { n: tokens(unpriced) }) : "",
              i + o + x > 0 ? t("stats:cwf.tokensAria", { n: tokens(i + o + x) }) : "",
              real(s.duration) !== undefined ? took(real(s.duration)!) : "",
              flagTop && top?.id === s.id ? t("stats:cwf.mostExpensiveAria") : "",
            ]
              .filter(Boolean)
              .join(", ");
            const dur = real(s.duration);
            const vars = {
              "--s": r.start,
              "--d": r.size,
              "--split": r.split,
            } as CSSProperties;
            return (
              <div
                key={s.id}
                className="ui-cwf__item"
                data-hidden={r.hidden ? "" : undefined}
                style={
                  {
                    "--ui-cwf-delay": `${enterDelay.current.get(s.id) ?? 0}ms`,
                  } as CSSProperties
                }
              >
                <div className="ui-cwf__clip">
                  <div
                    ref={(el) => {
                      if (el) rowEls.current.set(s.id, el);
                      else rowEls.current.delete(s.id);
                    }}
                    className="ui-cwf__row"
                    role="treeitem"
                    aria-level={r.depth + 1}
                    aria-expanded={kids ? !isFolded : undefined}
                    aria-selected={open}
                    aria-hidden={r.hidden || undefined}
                    aria-label={label}
                    tabIndex={!r.hidden && activeId === s.id ? 0 : -1}
                    data-kind={s.kind}
                    data-status={s.status}
                    data-parent={kids ? "" : undefined}
                    data-folded={isFolded ? "" : undefined}
                    data-open={open ? "" : undefined}
                    style={vars}
                    onClick={() => {
                      setActive(s.id);
                      setOpen(open ? null : s.id);
                    }}
                    onFocus={(e) => e.target === e.currentTarget && setActive(s.id)}
                    onKeyDown={(e) => onKey(e, r)}
                    {...rowProps?.(s)}
                  >
                    <div className="ui-cwf__name">
                      {/* Deep trees stop indenting after a few levels, so the name always keeps its room */}
                      {r.depth > 0 && (
                        <span
                          className="ui-cwf__guides"
                          style={{
                            width: `calc(min(${r.depth}, var(--ui-cwf-depth)) * var(--ui-cwf-indent))`,
                          }}
                          aria-hidden="true"
                        />
                      )}
                      <span
                        className="ui-cwf__chev"
                        data-leaf={kids ? undefined : ""}
                        aria-hidden="true"
                        onClick={(e) => {
                          if (!kids) return;
                          e.stopPropagation();
                          setActive(s.id);
                          toggle(s.id);
                        }}
                      >
                        {kids && <Chevron />}
                      </span>
                      <span className="ui-cwf__icon">{s.icon ?? <span className="ui-cwf__swatch" />}</span>
                      {/* Long names end in … – the whole name and model show on hover, and in full to screen readers */}
                      <span className="ui-cwf__label" title={`${s.name}${(s.model ?? s.detail) ? ` · ${s.model ?? s.detail}` : ""}`}>
                        <span className="ui-cwf__title" dir="auto">
                          {s.name}
                        </span>
                        {(s.model ?? s.detail) && (
                          <span className="ui-cwf__model" dir="auto">
                            {s.model ?? s.detail}
                          </span>
                        )}
                      </span>
                      {hiddenCount > 0 && <span className="ui-cwf__more">+{hiddenCount}</span>}
                      {flagTop && top?.id === s.id && <span className="ui-cwf__flag">{t("stats:cwf.top")}</span>}
                      {running && !kids && (
                        <span className="ui-cwf__state" data-status="running">
                          <Spinner />
                        </span>
                      )}
                      {s.status === "failed" && (
                        <span className="ui-cwf__state" data-status="failed">
                          <Warning />
                        </span>
                      )}
                    </div>
                    <div className="ui-cwf__track">
                      <span className="ui-cwf__bar" data-kind={s.kind} data-status={anyRunning ? "running" : s.status} data-credit={r.credit ? "" : undefined}>
                        <span className="ui-cwf__out" />
                      </span>
                    </div>
                    <span className="ui-cwf__num" data-col="in">
                      <span className="ui-cwf__amount" data-live={anyRunning && i > 0 ? "" : undefined}>
                        {i > 0 ? tokens(i) : "–"}
                      </span>
                    </span>
                    <span className="ui-cwf__num" data-col="out">
                      <span className="ui-cwf__amount" data-live={anyRunning && o > 0 ? "" : undefined}>
                        {o > 0 ? tokens(o) : "–"}
                      </span>
                    </span>
                    <span className="ui-cwf__num" data-col="cost">
                      <span className="ui-cwf__amount" data-live={anyRunning && cost !== undefined ? "" : undefined}>
                        {unpricedMark(unpriced)}
                        {cost !== undefined ? formatCost(cost) : "–"}
                      </span>
                    </span>
                    {/* Narrow screens have no room for In and Out – in the tokens view this one takes the cost's place */}
                    <span className="ui-cwf__num" data-col="tok">
                      <span className="ui-cwf__amount" data-live={anyRunning && i + o + x > 0 ? "" : undefined}>
                        {i + o + x > 0 ? tokens(i + o + x) : "–"}
                      </span>
                    </span>
                    <span className="ui-cwf__num" data-col="time">
                      <span className="ui-cwf__amount" data-live={anyRunning && dur !== undefined ? "" : undefined}>
                        {dur !== undefined ? took(dur) : "–"}
                      </span>
                    </span>
                  </div>
                  {!r.hidden && (open || closing === s.id) && details(s, !open, r.depth)}
                </div>
              </div>
            );
          })}
        </div>
      </div>
      </div>
    </div>
  );
}

const css = /* css */ `
/* Colors, sizes and motion as variables – override them on .ui-cwf */
.ui-cwf {
  /* Steve: every color from the --aui-* map (affectui-theme.css), so one block serves light and dark */
  --ui-cwf-tray: var(--aui-subtle);
  --ui-cwf-card: var(--aui-surface);
  --ui-cwf-strong: var(--aui-ink);
  --ui-cwf-ink: var(--aui-ink);
  --ui-cwf-text: var(--aui-muted);
  --ui-cwf-muted: var(--aui-muted);
  --ui-cwf-faint: var(--aui-placeholder);
  --ui-cwf-line: var(--aui-border);
  --ui-cwf-grid: color-mix(in oklab, var(--aui-border) 60%, transparent);
  --ui-cwf-guide: var(--aui-border);
  --ui-cwf-hover: color-mix(in oklab, var(--aui-subtle) 60%, transparent);
  --ui-cwf-selected: var(--aui-subtle);
  --ui-cwf-fill: var(--aui-subtle);
  --ui-cwf-track: var(--aui-subtle);
  --ui-cwf-wash: var(--aui-subtle);
  --ui-cwf-accent: var(--aui-blue);
  /* Bars by kind: a tint, its edge, the stronger part for output tokens, and the swatch */
  --ui-cwf-agent-bg: color-mix(in oklab, var(--aui-ink) 10%, var(--aui-surface));
  --ui-cwf-agent-edge: color-mix(in oklab, var(--aui-ink) 30%, var(--aui-surface));
  --ui-cwf-agent-out: color-mix(in oklab, var(--aui-ink) 20%, var(--aui-surface));
  --ui-cwf-agent-mark: var(--aui-placeholder);
  --ui-cwf-llm-bg: color-mix(in oklab, var(--aui-blue) 16%, var(--aui-surface));
  --ui-cwf-llm-edge: color-mix(in oklab, var(--aui-blue) 50%, var(--aui-surface));
  --ui-cwf-llm-out: color-mix(in oklab, var(--aui-blue) 32%, var(--aui-surface));
  --ui-cwf-llm-mark: var(--aui-blue);
  --ui-cwf-tool-bg: color-mix(in oklab, var(--series-2) 16%, var(--aui-surface));
  --ui-cwf-tool-edge: color-mix(in oklab, var(--series-2) 50%, var(--aui-surface));
  --ui-cwf-tool-out: color-mix(in oklab, var(--series-2) 32%, var(--aui-surface));
  --ui-cwf-tool-mark: var(--series-2);
  --ui-cwf-error-bg: var(--aui-danger-soft);
  --ui-cwf-error-edge: color-mix(in oklab, var(--aui-danger) 55%, var(--aui-surface));
  --ui-cwf-error-out: color-mix(in oklab, var(--aui-danger) 26%, var(--aui-surface));
  --ui-cwf-error-ink: var(--aui-danger);
  --ui-cwf-error-mark: var(--aui-red);
  --ui-cwf-brand-bg: var(--aui-info-soft);
  --ui-cwf-brand-fg: var(--aui-blue);
  --ui-cwf-ok-bg: var(--aui-success-soft);
  --ui-cwf-ok-fg: var(--aui-success);
  --ui-cwf-top-bg: var(--aui-warning-soft);
  --ui-cwf-top-fg: var(--aui-warning);
  --ui-cwf-focus: var(--aui-focus);
  --ui-cwf-shine: var(--aui-placeholder);
  --ui-cwf-shadow: var(--aui-shadow-card);
  --ui-cwf-row: 32px;
  --ui-cwf-bar: 16px;
  --ui-cwf-head: 30px;
  --ui-cwf-name: 228px;
  --ui-cwf-n: 44px;
  --ui-cwf-n-cost: 62px;
  --ui-cwf-n-time: 50px;
  --ui-cwf-pad: 10px;
  --ui-cwf-gap: 12px;
  --ui-cwf-end: 16px;
  --ui-cwf-indent: 14px;
  /* Deeper levels stop indenting, so a deep tree keeps room for its names */
  --ui-cwf-depth: 6;
  --ui-cwf-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  --ui-cwf-range: 1;
  position: relative;
  box-sizing: border-box;
  width: 100%;
  padding: 4px;
  border-radius: calc(var(--aui-radius-lg, 16px) + 4px);
  background: var(--ui-cwf-tray);
  container: ui-cwf / inline-size;
  font-family: var(--aui-font, "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 14px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-cwf-text);
  text-align: left;
  letter-spacing: -0.006em;
  -webkit-font-smoothing: antialiased;
}
.ui-cwf *, .ui-cwf *::before, .ui-cwf *::after { box-sizing: border-box; }
:where(.ui-cwf) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-cwf button:focus-visible { outline: 2px solid var(--ui-cwf-focus); outline-offset: 2px; }
.ui-cwf :is(p, dl, dd) { margin: 0; }
.ui-cwf svg { display: block; flex: none; }

/* ----- On the tray: title, the run's task, the switch and its state ----- */
.ui-cwf__head { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 6px 6px 10px 12px; }
.ui-cwf__spark { display: grid; flex: none; place-items: center; width: 16px; color: var(--ui-cwf-accent); }
.ui-cwf__heading { flex: none; font-weight: 600; color: var(--ui-cwf-strong); white-space: nowrap; }
.ui-cwf__task { flex: 1; min-width: 0; font-size: 13px; overflow: hidden; color: var(--ui-cwf-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-cwf__switch { position: relative; display: grid; flex: none; grid-template-columns: 1fr 1fr; margin-left: auto; padding: 2px; border-radius: 999px; background: var(--ui-cwf-track); }
/* The white thumb slides under the chosen view */
.ui-cwf__switch::before { content: ""; position: absolute; top: 2px; bottom: 2px; left: 2px; width: calc(50% - 2px); border-radius: 999px; background: var(--ui-cwf-card); box-shadow: var(--ui-cwf-shadow); transition: transform 320ms var(--ui-cwf-ease); }
.ui-cwf[data-mode="tokens"] .ui-cwf__switch::before { transform: translateX(100%); }
.ui-cwf__switch > button { position: relative; height: 24px; padding: 0 12px; border-radius: 999px; font-size: 12px; font-weight: 500; line-height: 16px; text-align: center; color: var(--ui-cwf-text); transition: color 200ms ease; }
.ui-cwf__switch > button[aria-pressed="true"] { color: var(--ui-cwf-ink); }
.ui-cwf__pill { display: inline-flex; flex: none; align-items: center; gap: 6px; height: 28px; padding: 0 11px; border-radius: 999px; background: var(--aui-surface, var(--ui-cwf-card)); box-shadow: var(--ui-cwf-shadow); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-cwf-text); white-space: nowrap; }
/* On the tray the state is a white pill – only its text takes the state's color */
.ui-cwf__pill[data-status="running"] { color: var(--ui-cwf-brand-fg); }
.ui-cwf__pill[data-status="done"] { color: var(--ui-cwf-ok-fg); }
.ui-cwf__pill[data-status="failed"] { color: var(--ui-cwf-error-ink); }
.ui-cwf__pulse { width: 6px; height: 6px; border-radius: 50%; background: currentColor; animation: ui-cwf-pulse 1400ms ease-in-out infinite; }

/* ----- The white card: the run in numbers, then the waterfall ----- */
.ui-cwf__card { border-radius: var(--aui-radius-lg, 16px); background: var(--aui-surface, var(--ui-cwf-card)); box-shadow: var(--aui-shadow-card, var(--ui-cwf-shadow)); }
.ui-cwf__stats { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) minmax(0, 1.4fr); padding: 14px 0; border-bottom: 1px solid var(--aui-border, var(--ui-cwf-line)); }
.ui-cwf__stat { display: grid; gap: 2px; min-width: 0; padding: 0 16px; }
.ui-cwf__stat:first-child { padding-left: 22px; }
.ui-cwf__stat + .ui-cwf__stat { border-left: 1px solid var(--aui-border, var(--ui-cwf-line)); }
.ui-cwf__stat dt { overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-cwf-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-cwf__stat dd { display: flex; align-items: baseline; gap: 8px; min-width: 0; overflow: hidden; font-size: 16px; font-weight: 600; line-height: 24px; letter-spacing: -0.01em; color: var(--ui-cwf-ink); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-cwf__sub { flex: none; font-size: 12px; font-weight: 400; letter-spacing: 0; line-height: 16px; color: var(--ui-cwf-muted); font-variant-numeric: tabular-nums; }
/* The hover fill reaches 6px past the text on both sides – the value's box widens by as much, so it isn't clipped */
.ui-cwf__stat dd:has(.ui-cwf__top) { margin: 0 -6px; padding: 0 6px; }
.ui-cwf__top { display: flex; align-items: baseline; gap: 8px; min-width: 0; max-width: calc(100% + 12px); margin: 0 -6px; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); transition: background-color 140ms ease; }
.ui-cwf__top:hover { background: var(--ui-cwf-fill); }
/* The value's box clips above and below – the focus ring sits inside the button instead of around it */
.ui-cwf .ui-cwf__top:focus-visible { outline: 2px solid var(--ui-cwf-focus); outline-offset: -2px; }
.ui-cwf__top-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }

/* ----- The table: column heads, the axis behind it, the rows ----- */
.ui-cwf__body { position: relative; }
.ui-cwf__cols,
.ui-cwf__row {
  display: grid;
  grid-template-columns: var(--ui-cwf-name) minmax(0, 1fr) var(--ui-cwf-n) var(--ui-cwf-n) var(--ui-cwf-n-cost) var(--ui-cwf-n-time);
  column-gap: var(--ui-cwf-gap);
  padding: 0 var(--ui-cwf-end) 0 var(--ui-cwf-pad);
}
.ui-cwf__cols { align-items: center; height: var(--ui-cwf-head); border-bottom: 1px solid var(--aui-border, var(--ui-cwf-line)); font-size: 12px; line-height: 16px; color: var(--ui-cwf-muted); }
.ui-cwf__cols > span:first-child { padding-left: 6px; }
.ui-cwf__cols > [data-col] { text-align: right; }
/* The grid lines and tick labels sit over the bar column only */
.ui-cwf__layer {
  position: absolute;
  top: 0;
  bottom: 0;
  left: calc(var(--ui-cwf-pad) + var(--ui-cwf-name) + var(--ui-cwf-gap));
  right: calc(var(--ui-cwf-end) + 2 * var(--ui-cwf-n) + var(--ui-cwf-n-cost) + var(--ui-cwf-n-time) + 4 * var(--ui-cwf-gap));
  overflow: hidden;
  pointer-events: none;
}
.ui-cwf__tick { position: absolute; top: 0; bottom: 0; left: calc(var(--v) * 100% / var(--ui-cwf-range)); width: 1px; background: var(--ui-cwf-grid); animation: ui-cwf-fade 420ms ease both; }
.ui-cwf__tick[data-zero] { background: var(--ui-cwf-line); }
.ui-cwf__tick > span { position: absolute; top: 7px; left: 6px; font-size: 11.5px; line-height: 16px; color: var(--ui-cwf-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
/* No room above or below the rows: a selected row's fill meets the column heads and the card's edge */
.ui-cwf__rows { position: relative; }
.ui-cwf__rows[data-scroll] { overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; }
/* Nothing to measure yet: the grid lines stay in the head, so they don't cross the message */
.ui-cwf__body:has(.ui-cwf__empty) .ui-cwf__tick { bottom: auto; height: var(--ui-cwf-head); }
.ui-cwf__empty { position: relative; margin: 0; padding: 22px 16px 24px; font-size: 12px; line-height: 16px; text-align: center; color: var(--ui-cwf-muted); }
.ui-cwf__item:last-child .ui-cwf__clip { border-radius: 0 0 var(--aui-radius-lg, 16px) var(--aui-radius-lg, 16px); }

/* ----- One row ----- */
.ui-cwf__item { display: grid; animation: ui-cwf-row 420ms var(--ui-cwf-ease) var(--ui-cwf-delay, 0ms) backwards; transition: grid-template-rows 360ms var(--ui-cwf-ease), opacity 260ms ease; grid-template-rows: 1fr; }
.ui-cwf__clip { min-height: 0; overflow: hidden; }
/* Folded away: the row and its details close to no height */
.ui-cwf__item[data-hidden] { grid-template-rows: 0fr; opacity: 0; pointer-events: none; }
.ui-cwf__row { align-items: center; height: var(--ui-cwf-row); outline: none; cursor: pointer; transition: background-color 140ms ease; }
.ui-cwf__row:hover { background: var(--ui-cwf-hover); }
.ui-cwf__row[data-open] { background: var(--ui-cwf-selected); }
.ui-cwf__row:focus-visible { outline: 2px solid var(--ui-cwf-focus); outline-offset: -2px; }

.ui-cwf__name { display: flex; align-items: center; min-width: 0; height: var(--ui-cwf-row); }
.ui-cwf__guides { flex: none; align-self: stretch; background: repeating-linear-gradient(90deg, transparent 0 7.5px, var(--ui-cwf-guide) 7.5px 8.5px, transparent 8.5px var(--ui-cwf-indent)); }
.ui-cwf__chev { display: grid; flex: none; place-items: center; width: 16px; height: 20px; margin-right: 4px; border-radius: calc(var(--aui-radius-sm, 7px) - 2px); color: var(--ui-cwf-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-cwf__chev:not([data-leaf]):hover { background: var(--ui-cwf-fill); color: var(--ui-cwf-ink); }
.ui-cwf__chev > svg { transition: transform 240ms var(--ui-cwf-ease); }
.ui-cwf__row[data-folded] .ui-cwf__chev > svg { transform: rotate(-90deg); }
.ui-cwf__icon { display: grid; flex: none; place-items: center; width: 20px; height: 20px; margin-right: 8px; border-radius: 6px; background: var(--ui-cwf-card); box-shadow: 0 0 0 1px var(--ui-cwf-line), 0 1px 2px rgba(0, 0, 0, 0.06); }
.ui-cwf__icon > svg { width: 12px; height: 12px; }
.ui-cwf__swatch { width: 8px; height: 8px; border-radius: 2.5px; background: var(--k-mark); }
.ui-cwf__label { display: flex; align-items: baseline; gap: 6px; flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; }
.ui-cwf__title { flex: none; max-width: 100%; overflow: hidden; font-size: 13px; color: var(--ui-cwf-ink); text-overflow: ellipsis; }
.ui-cwf__row[data-parent] .ui-cwf__title { font-weight: 500; }
.ui-cwf__model { min-width: 0; overflow: hidden; font-size: 12px; color: var(--ui-cwf-muted); text-overflow: ellipsis; }
.ui-cwf__more { flex: none; height: 18px; margin-left: 6px; padding: 0 6px; border-radius: 999px; background: var(--ui-cwf-fill); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-cwf-text); font-variant-numeric: tabular-nums; animation: ui-cwf-pop 220ms var(--ui-cwf-ease) both; }
.ui-cwf__flag { flex: none; height: 16px; margin-left: 6px; padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 7px) - 3px); background: var(--ui-cwf-top-bg); font-size: 10.5px; font-weight: 500; line-height: 16px; color: var(--ui-cwf-top-fg); animation: ui-cwf-pop 260ms var(--ui-cwf-ease) both; }
.ui-cwf__state { display: grid; flex: none; place-items: center; width: 16px; margin-left: 4px; }
.ui-cwf__state[data-status="running"] { color: var(--ui-cwf-brand-fg); }
.ui-cwf__state[data-status="failed"] { color: var(--ui-cwf-error-ink); }
.ui-cwf__spin { animation: ui-cwf-spin 900ms linear infinite; }

.ui-cwf__num { min-width: 0; overflow: hidden; font-size: 12px; line-height: 16px; text-align: right; color: var(--ui-cwf-text); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-cwf__num[data-col="cost"] { color: var(--ui-cwf-ink); }
/* Steve: the unpriced mark before a cost, muted, so the amounts stay aligned on the right */
.ui-cwf__unpriced { display: inline-grid; place-items: center; margin-right: 3px; vertical-align: -1px; color: var(--ui-cwf-muted); cursor: help; }
/* …and under the total: on its own line when it doesn't fit beside it */
.ui-cwf__stat dd:has(.ui-cwf__sub--unpriced) { flex-wrap: wrap; column-gap: 8px; row-gap: 0; }
.ui-cwf__sub--unpriced { max-width: 100%; overflow: hidden; text-overflow: ellipsis; }
.ui-cwf :is(.ui-cwf__cols, .ui-cwf__row) > [data-col="tok"] { display: none; }
.ui-cwf__row[data-parent] .ui-cwf__num[data-col="cost"] { font-weight: 500; }
.ui-cwf__row[data-status="failed"] .ui-cwf__num[data-col="time"] { color: var(--ui-cwf-error-ink); }

/* ----- The bar: it starts where the step before it ended ----- */
.ui-cwf [data-kind] { --k-bg: var(--ui-cwf-agent-bg); --k-edge: var(--ui-cwf-agent-edge); --k-out: var(--ui-cwf-agent-out); --k-mark: var(--ui-cwf-agent-mark); }
.ui-cwf [data-kind="llm"] { --k-bg: var(--ui-cwf-llm-bg); --k-edge: var(--ui-cwf-llm-edge); --k-out: var(--ui-cwf-llm-out); --k-mark: var(--ui-cwf-llm-mark); }
.ui-cwf [data-kind="tool"] { --k-bg: var(--ui-cwf-tool-bg); --k-edge: var(--ui-cwf-tool-edge); --k-out: var(--ui-cwf-tool-out); --k-mark: var(--ui-cwf-tool-mark); }
.ui-cwf [data-kind][data-status="failed"] { --k-bg: var(--ui-cwf-error-bg); --k-edge: var(--ui-cwf-error-edge); --k-out: var(--ui-cwf-error-out); --k-mark: var(--ui-cwf-error-mark); }
.ui-cwf__track { position: relative; min-width: 0; height: var(--ui-cwf-row); overflow: hidden; }
.ui-cwf__bar {
  position: absolute;
  top: calc(50% - var(--ui-cwf-bar) / 2);
  left: calc(var(--s) * 100% / var(--ui-cwf-range));
  width: max(2px, calc(var(--d) * 100% / var(--ui-cwf-range)));
  height: var(--ui-cwf-bar);
  overflow: hidden;
  border-radius: 5px;
  background: var(--k-bg);
  box-shadow: inset 0 0 0 1px var(--k-edge);
  transition: box-shadow 160ms ease, background-color 240ms ease;
  animation: ui-cwf-grow 560ms var(--ui-cwf-ease) calc(var(--ui-cwf-delay, 0ms) + 60ms) backwards;
}
.ui-cwf__row[data-parent] .ui-cwf__bar { border-radius: 4px; }
.ui-cwf__row:is(:hover, [data-open]) .ui-cwf__bar { box-shadow: inset 0 0 0 1px var(--k-mark); }
/* Tokens: the output part of each bar, a step stronger, on its right end */
/* A credit runs the total back: hatched, so it doesn't read as spend */
.ui-cwf__bar[data-credit] { background: repeating-linear-gradient(135deg, var(--k-bg) 0 3px, transparent 3px 6px); }
.ui-cwf__out { position: absolute; top: 0; right: 0; bottom: 0; width: calc(var(--split) * 100%); background: var(--k-out); transition: width 240ms ease; }
/* A switch between cost and tokens: every bar glides to its new place and length */
.ui-cwf[data-switching] .ui-cwf__bar { transition: left ${SWITCH}ms var(--ui-cwf-ease), width ${SWITCH}ms var(--ui-cwf-ease), box-shadow 160ms ease; }
.ui-cwf[data-switching] .ui-cwf__out { transition: width ${SWITCH}ms var(--ui-cwf-ease); }
/* Still running: a sheen sweeps along and the leading edge glows */
.ui-cwf__bar[data-status="running"]::before { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent, var(--ui-cwf-card), transparent) -60px 0 / 60px 100% no-repeat; opacity: 0.7; animation: ui-cwf-sweep 1500ms linear infinite; }
.ui-cwf__bar[data-status="running"]::after { content: ""; position: absolute; top: 0; right: 0; bottom: 0; width: min(28px, 100%); background: linear-gradient(90deg, transparent, var(--k-edge)); animation: ui-cwf-glow 1100ms ease-in-out infinite alternate; }
/* Numbers that still climb: a light shimmer runs over them */
.ui-cwf__amount[data-live] {
  background: linear-gradient(90deg, currentColor 0%, currentColor 38%, var(--ui-cwf-shine) 50%, currentColor 62%, currentColor 100%) 0 0 / 300% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: ui-cwf-shine 1100ms linear infinite;
}

/* ----- The details: under their row, the two texts side by side ----- */
.ui-cwf__detail { display: grid; grid-template-rows: 1fr; animation: ui-cwf-open 300ms var(--ui-cwf-ease) both; }
.ui-cwf__detail[data-closing] { animation: ui-cwf-shut 220ms var(--ui-cwf-ease) both; }
/* Starts under the step's name – as far in as the row's lines, chevron and icon */
.ui-cwf__detail-in { --ui-cwf-in: calc(min(var(--ui-cwf-level), var(--ui-cwf-depth)) * var(--ui-cwf-indent)); position: relative; display: grid; gap: 8px; min-height: 0; overflow: hidden; padding: 4px var(--ui-cwf-end) 10px calc(var(--ui-cwf-pad) + var(--ui-cwf-in) + 48px); background: var(--ui-cwf-selected); }
.ui-cwf__detail-in > .ui-cwf__guides { position: absolute; top: 0; bottom: 0; left: var(--ui-cwf-pad); width: var(--ui-cwf-in); }
.ui-cwf__io { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 8px; }
.ui-cwf__io > div { display: grid; align-content: start; gap: 4px; min-width: 0; padding: 10px 12px; border-radius: calc(var(--aui-radius-lg, 16px) - 4px); background: var(--aui-surface, var(--ui-cwf-card)); box-shadow: var(--aui-shadow-card, var(--ui-cwf-shadow)); }
.ui-cwf__io span { font-size: 11.5px; line-height: 16px; color: var(--ui-cwf-muted); }
/* Up to six lines, then the text scrolls in its box – a pasted thread or a long tool payload stays readable */
.ui-cwf__io p { max-height: 108px; margin: 0 -4px; padding: 0 4px; overflow: auto; overscroll-behavior: contain; overflow-wrap: anywhere; font-size: 12px; line-height: 18px; color: var(--ui-cwf-ink); white-space: pre-line; border-radius: 4px; outline: none; scrollbar-width: thin; }
.ui-cwf__io p:focus-visible { outline: 2px solid var(--ui-cwf-focus); outline-offset: 2px; }
.ui-cwf__io p[data-mono] { font-family: var(--aui-font-mono, ui-monospace, "SF Mono", Menlo, Consolas, monospace); font-size: 11.5px; }
.ui-cwf__io p[data-live] { color: var(--ui-cwf-muted); }
.ui-cwf__error { display: flex; gap: 8px; padding: 8px 10px; border-radius: 10px; background: var(--ui-cwf-error-bg); font-size: 12px; line-height: 16px; color: var(--ui-cwf-error-ink); }
.ui-cwf__error > svg { margin-top: 2px; }
.ui-cwf__error > span { min-width: 0; overflow-wrap: anywhere; }

/* ----- Narrower: the token columns step aside – they stay in the totals and the screen reader label ----- */
@container ui-cwf (max-width: 720px) {
  .ui-cwf__body { --ui-cwf-name: 200px; }
  .ui-cwf__cols, .ui-cwf__row { grid-template-columns: var(--ui-cwf-name) minmax(0, 1fr) var(--ui-cwf-n-cost) var(--ui-cwf-n-time); }
  .ui-cwf :is(.ui-cwf__cols, .ui-cwf__row) > [data-col="in"],
  .ui-cwf :is(.ui-cwf__cols, .ui-cwf__row) > [data-col="out"] { display: none; }
  .ui-cwf__layer { right: calc(var(--ui-cwf-end) + var(--ui-cwf-n-cost) + var(--ui-cwf-n-time) + 2 * var(--ui-cwf-gap)); }
  /* In the tokens view the column next to the time shows each step's tokens instead of its cost */
  .ui-cwf[data-mode="tokens"] :is(.ui-cwf__cols, .ui-cwf__row) > [data-col="cost"] { display: none; }
  .ui-cwf[data-mode="tokens"] :is(.ui-cwf__cols, .ui-cwf__row) > [data-col="tok"] { display: block; }
}
/* Phones: the name on its own line with cost and time, the bar across the whole width under it */
@container ui-cwf (max-width: 520px) {
  .ui-cwf__head { flex-wrap: wrap; padding: 10px 12px; row-gap: 8px; }
  .ui-cwf__task { order: 5; flex-basis: 100%; }
  .ui-cwf__stats { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); row-gap: 10px; padding: 10px 0; }
  .ui-cwf__stat { padding: 0 12px; }
  .ui-cwf__stat:first-child { padding-left: 16px; }
  .ui-cwf__stat:last-child { grid-column: 1 / -1; padding-left: 16px; border-left: 0; }
  .ui-cwf__stat:nth-child(2) .ui-cwf__sub { display: none; }
  .ui-cwf__body { --ui-cwf-row: 46px; --ui-cwf-bar: 10px; --ui-cwf-end: 12px; --ui-cwf-depth: 4; }
  .ui-cwf__layer { left: var(--ui-cwf-pad); right: var(--ui-cwf-end); }
  .ui-cwf__tick { bottom: auto; height: var(--ui-cwf-head); }
  .ui-cwf__cols { grid-template-columns: minmax(0, 1fr) var(--ui-cwf-n-cost) var(--ui-cwf-n-time); }
  .ui-cwf__cols > span:first-child, .ui-cwf__cols > span:nth-child(2) { display: none; }
  /* The axis runs the full width here – its labels take the head; "$" and "s" say what the numbers are */
  .ui-cwf__cols > [data-col] { visibility: hidden; }
  .ui-cwf__row { grid-template-columns: minmax(0, 1fr) var(--ui-cwf-n-cost) var(--ui-cwf-n-time); grid-template-rows: 30px 12px; align-items: center; row-gap: 0; }
  .ui-cwf__name { grid-column: 1; grid-row: 1; height: 30px; }
  .ui-cwf__row > :is([data-col="cost"], [data-col="tok"]) { grid-column: 2; grid-row: 1; }
  .ui-cwf__row > [data-col="time"] { grid-column: 3; grid-row: 1; }
  .ui-cwf__track { grid-column: 1 / -1; grid-row: 2; height: 12px; }
  .ui-cwf__bar { border-radius: 3px; }
  .ui-cwf__detail-in { padding-left: calc(var(--ui-cwf-pad) + var(--ui-cwf-in) + 8px); }
  .ui-cwf__io { grid-template-columns: minmax(0, 1fr); }
  .ui-cwf__model { display: none; }
}

@keyframes ui-cwf-row { from { grid-template-rows: 0fr; opacity: 0; transform: translateY(-4px); } }
@keyframes ui-cwf-grow { from { clip-path: inset(0 100% 0 0 round 5px); } to { clip-path: inset(0 0 0 0 round 5px); } }
@keyframes ui-cwf-open { from { grid-template-rows: 0fr; opacity: 0; } }
@keyframes ui-cwf-shut { to { grid-template-rows: 0fr; opacity: 0; } }
@keyframes ui-cwf-fade { from { opacity: 0; } }
@keyframes ui-cwf-pop { from { opacity: 0; transform: scale(0.9); } }
@keyframes ui-cwf-sweep { to { background-position: calc(100% + 60px) 0; } }
@keyframes ui-cwf-glow { from { opacity: 0.45; } to { opacity: 1; } }
@keyframes ui-cwf-pulse { 50% { opacity: 0.35; } }
@keyframes ui-cwf-spin { to { transform: rotate(360deg); } }
@keyframes ui-cwf-shine { from { background-position: 100% 0; } to { background-position: 0 0; } }

@media (pointer: coarse) {
  .ui-cwf__chev { width: 24px; height: 28px; margin-left: -4px; }
  /* Fingers need more than 24px */
  .ui-cwf__switch > button { height: 32px; padding: 0 14px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-cwf *, .ui-cwf *::before, .ui-cwf *::after { animation: none !important; transition: none !important; }
}

/* Steve: inside a panel – the panel is the card; no tray, no second card, the rows run to the panel's edges */
.ui-cwf[data-bare] { padding: 0; border-radius: 0; background: none; }
.ui-cwf[data-bare] .ui-cwf__card { border-radius: 0; background: none; box-shadow: none; }
.ui-cwf[data-bare] .ui-cwf__stats { border-top: 1px solid var(--ui-cwf-line); }
.ui-cwf[data-bare] .ui-cwf__item:last-child .ui-cwf__clip { border-radius: 0; }
/* Steve's durations are words ("1 h 33 min" in Chinese), its costs "<$0.01": wider number columns, and more room for item titles */
.ui-cwf[data-bare] { --ui-cwf-pad: 14px; --ui-cwf-end: 20px; --ui-cwf-name: 300px; --ui-cwf-n: 48px; --ui-cwf-n-cost: 60px; --ui-cwf-n-time: 78px; }
.ui-cwf[data-bare] .ui-cwf__stat:first-child { padding-left: 20px; }
/* a phone: the folded count gives its room to the item's title */
@container ui-cwf (max-width: 520px) { .ui-cwf[data-bare] .ui-cwf__more { display: none; } }
`;
