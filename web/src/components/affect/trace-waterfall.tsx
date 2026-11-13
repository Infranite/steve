/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { memo, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
/* Steve: words and numbers in the reader's language (Steve's i18n and formats) */
import { t, useT } from "@/i18n";
import { clockDur, msDur, num } from "@/i18n/format";
import { fmtN } from "@/stats/format";

/*
 * Trace Waterfall – where the time and the money of an agent run go, in a single self-contained file.
 * Like the performance panel of an APM tool, but for agents: on the left the run as a tree of spans – the agent,
 * its LLM calls, tool calls and sub-agents – and on the right a timeline where every span is a bar that starts
 * when it started and is as long as it took. One quiet white card with hairlines, the whole run in numbers on top.
 *   · bars are tinted by kind: agents a quiet gray, LLM calls indigo, tool calls teal, errors red
 *   · a running span grows live with a soft shimmer at its leading edge, and a "now" line moves across
 *   · when the run outgrows the timeline, the axis rescales smoothly – ticks glide and fade, bars follow
 *   · every row shows how long it took and, where it costs something, what it cost; the slowest call is marked
 *   · fold a span and its rows close up – their bars collapse into the parent bar as small marks
 *   · click a row (or press Enter) for a small window with its input, output, tokens, cost and error
 * Narrow containers stack the name above its bar. The styles are injected below, so no extra files are needed.
 */

export type TraceSpanKind = "agent" | "subagent" | "llm" | "tool";
/* Steve: "cancelled" – the run was stopped before it finished; neither a success nor a failure */
export type TraceSpanStatus = "running" | "done" | "failed" | "cancelled";

export type TraceSpan = {
  id: string;
  name: string;
  kind: TraceSpanKind;
  status: TraceSpanStatus;
  /** When it started, in ms from the start of the run */
  start: number;
  /** When it ended, in ms from the start of the run – leave it out while it runs */
  end?: number;
  /** A few muted words after the name – the model, the query, the target */
  detail?: string;
  /** A small mark, e.g. the model maker's or the tool's logo – defaults to a swatch in the kind's color */
  icon?: ReactNode;
  /** The model of an LLM call or an agent – shown in the details */
  model?: string;
  tokensIn?: number;
  tokensOut?: number;
  /** Its own cost – an agent's children are added on top */
  cost?: number;
  /** What went in and what came out – a short preview, shown in the details */
  input?: string;
  output?: string;
  /** What went wrong – shown in the details of a failed span */
  error?: string;
  /** Steve: left out of the run's numbers – e.g. a note the model wrote, which is not a thinking stretch */
  uncounted?: boolean;
  children?: TraceSpan[];
};

export type TraceWaterfallProps = {
  root: TraceSpan;
  /** The clock, in ms from the start of the run – e.g. a faster one for a demo */
  now?: number;
  /** When the run started (epoch ms) – without `now`, the component keeps its own clock from here while it runs */
  startedAt?: number;
  /** Controlled ids of folded spans */
  collapsed?: string[];
  defaultCollapsed?: string[];
  onCollapsedChange?: (collapsed: string[]) => void;
  /** Controlled id of the span whose details are open */
  openId?: string | null;
  defaultOpenId?: string | null;
  onOpenChange?: (id: string | null, span?: TraceSpan) => void;
  /** Marks the slowest finished LLM or tool call (default true) */
  markSlowest?: boolean;
  /** The shortest time the axis shows, in ms (default 10s) */
  minRange?: number;
  /** Formats every cost – defaults to dollars, e.g. "$0.042" */
  formatCost?: (amount: number) => string;
  title?: string;
  className?: string;
};

/* ---------- Helpers ---------- */

const KIND = (k: TraceSpanKind) => t(`core:trace.kind.${k}`);
const STATUS = (s: TraceSpanStatus) => t(`core:trace.status.${s}`);
const SEC = 1000;
/** Ranges the axis snaps to, and the steps between its ticks */
const RANGES = [5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 450, 600, 900, 1200, 1800, 2700, 3600, 5400, 7200].map((s) => s * SEC);
const STEPS = [1, 2, 3, 5, 10, 15, 20, 30, 60, 120, 300, 600, 900, 1800, 3600].map((s) => s * SEC);
/** Motion: the axis rescale, rows that leave, and the beat between rows that come or go together */
const RESCALE = 720;
const EXIT = 280;
const STAGGER = 36;

const dollars = (n: number) => (n === 0 ? "$0" : n < 0.001 ? "<$0.001" : n < 0.01 ? `$${n.toFixed(3)}` : n < 10 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString("en-US")}`);
/* Steve: "240 ms", "4.2 秒", "1 分 5 秒" – the same words as the rest of the transcript */
const took = (ms: number) => (ms < 10 * SEC ? msDur(Math.max(0, ms)) : clockDur(ms));
/* Steve: the same length in the fewest characters, for the narrow totals: "1m 15s", "1分15秒" */
const short = (ms: number) => {
  const s = Math.max(0, Math.round(ms / SEC));
  if (ms < 10 * SEC) return took(ms);
  if (s < 60) return t("core:trace.short.s", { s });
  if (s < 3600) return t("core:trace.short.ms", { m: Math.floor(s / 60), s: s % 60 });
  return t("core:trace.short.hm", { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) });
};
const tickLabel = (v: number) => {
  const s = Math.round(v / SEC);
  if (s < 60) return t("common:dur.seconds", { count: s });
  if (s >= 3600) return s % 3600 ? t("common:dur.hourMin", { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) }) : t("common:dur.hours", { count: s / 3600 });
  return s % 60 ? t("common:dur.minSec", { m: Math.floor(s / 60), s: s % 60 }) : t("common:dur.minutes", { count: s / 60 });
};
const tokens = (n: number) => fmtN(n);
const count = (n: number) => num(n);
const walk = (s: TraceSpan): TraceSpan[] => [s, ...(s.children ?? []).flatMap(walk)];
const costOf = (s: TraceSpan): number => (s.cost ?? 0) + (s.children ?? []).reduce((sum, c) => sum + costOf(c), 0);
const tokensOf = (s: TraceSpan): [number, number] =>
  (s.children ?? []).reduce<[number, number]>(
    (sum, c) => {
      const [i, o] = tokensOf(c);
      return [sum[0] + i, sum[1] + o];
    },
    [s.tokensIn ?? 0, s.tokensOut ?? 0],
  );
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const ease = (p: number) => (p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2);

type Row = { span: TraceSpan; depth: number; parentId?: string; hidden: boolean; leaving?: number };

/* ---------- Icons ---------- */

const Spark = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
/* Chevron, warning and cancel: Hugeicons Free (MIT, © Hugeicons, hugeicons.com); the loader is a plain arc */
const Chevron = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M18 9.00005C18 9.00005 13.5811 15 12 15C10.4188 15 6 9 6 9" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </svg>
);
const Warning = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M13.9248 21H10.0752C5.44476 21 3.12955 21 2.27636 19.4939C1.42317 17.9879 2.60736 15.9914 4.97574 11.9985L6.90057 8.75333C9.17559 4.91778 10.3131 3 12 3C13.6869 3 14.8244 4.91777 17.0994 8.75332L19.0243 11.9985C21.3926 15.9914 22.5768 17.9879 21.7236 19.4939C20.8704 21 18.5552 21 13.9248 21Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
    <path d="M12 9V13" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.2" />
    <path d="M12 16.8V17" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.6" />
  </svg>
);
const Spinner = () => (
  <svg className="ui-trwc__spin" width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <circle cx="12" cy="12" r="8.5" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2.5" />
    <path d="M12 3.5A8.5 8.5 0 0 1 20.5 12" stroke="currentColor" strokeLinecap="round" strokeWidth="2.5" />
  </svg>
);
const Cancel = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M19 5L5 19M5 5L19 19" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </svg>
);

/* ---------- One row – memoized, so a live trace re-renders only the rows whose clock runs ---------- */

type RowHandlers = {
  click: (id: string) => void;
  fold: (id: string) => void;
  focus: (id: string) => void;
  key: (e: KeyboardEvent<HTMLDivElement>, id: string) => void;
  el: (id: string, el: HTMLDivElement | null) => void;
};
type RowProps = {
  span: TraceSpan;
  depth: number;
  hidden: boolean;
  /** Set while the row closes up – the delay before it goes */
  leaving?: number;
  folded: boolean;
  open: boolean;
  focusable: boolean;
  slowest: boolean;
  /** The clock – only passed to rows that run or hold something running, so the others skip the re-render */
  now?: number;
  range: number;
  trackW: number;
  delay: number;
  formatCost: (amount: number) => string;
  on: { current: RowHandlers | null };
};

/** A child's mark on its parent's bar – memoized too: a live root holds one for every span below it */
const Mark = memo(function Mark({ kind, status, start, dur }: { kind: TraceSpanKind; status: TraceSpanStatus; start: number; dur: number }) {
  return <span className="ui-trwc__mark" data-kind={kind} data-status={status} style={{ "--s": start, "--d": dur } as CSSProperties} aria-hidden="true" />;
});

const SpanRow = memo(function SpanRow({ span: s, depth, hidden, leaving: leaveDelay, folded: isFolded, open, focusable, slowest: isSlowest, now, range, trackW, delay, formatCost, on }: RowProps) {
  useT(); // Steve: a row follows a change of language too
  const kids = s.children ?? [];
  const leaving = leaveDelay !== undefined;
  const endOf = (x: TraceSpan) => x.end ?? (x.status === "running" ? Math.max(now ?? x.start, x.start) : x.start);
  const durOf = (x: TraceSpan) => Math.max(0, endOf(x) - x.start);
  const dur = durOf(s);
  const cost = kids.length ? costOf(s) : s.cost;
  const showCost = cost !== undefined && cost > 0 && (s.kind !== "tool" || s.cost !== undefined);
  const label = `${took(dur)}${showCost ? ` · ${formatCost(cost!)}` : ""}`;
  // Where the duration goes: after the bar, inside its end, or before it – whichever fits the track
  const px = (v: number) => (v / range) * trackW;
  // Steve: CJK characters are about twice as wide as Latin ones at this size
  const textW = (x: string) => [...x].reduce((w, c) => w + (c.charCodeAt(0) > 0x2e80 ? 11.5 : 6.4), 0);
  const need = textW(label) + (isSlowest ? textW(t("core:trace.slowest")) + 22 : 0) + 8;
  const side = !trackW || px(s.start + dur) + 6 + need <= trackW ? "after" : px(dur) >= need + 10 ? "inside" : px(s.start) >= need + 6 ? "before" : "inside";
  const hiddenCount = isFolded ? walk(s).length - 1 : 0;
  const marks = isFolded || kids.length ? walk(s).slice(1).filter((c) => c.kind === "llm" || c.kind === "tool") : [];
  const vars = { "--s": s.start, "--d": dur } as CSSProperties;
  return (
    <div
      ref={leaving ? undefined : (el) => on.current?.el(s.id, el)}
      className="ui-trwc__row"
      role="treeitem"
      aria-level={depth + 1}
      aria-expanded={kids.length ? !isFolded : undefined}
      aria-selected={open}
      aria-hidden={hidden || leaving || undefined}
      aria-label={`${s.name}, ${KIND(s.kind)}, ${STATUS(s.status)}, ${label}`}
      tabIndex={focusable ? 0 : -1}
      data-kind={s.kind}
      data-status={s.status}
      data-parent={kids.length ? "" : undefined}
      data-hidden={hidden ? "" : undefined}
      data-folded={isFolded ? "" : undefined}
      data-leaving={leaving ? "" : undefined}
      data-open={open ? "" : undefined}
      style={{ animationDelay: `${leaving ? leaveDelay : delay}ms` }}
      onClick={() => !leaving && on.current?.click(s.id)}
      onFocus={(e) => e.target === e.currentTarget && on.current?.focus(s.id)}
      onKeyDown={(e) => on.current?.key(e, s.id)}
    >
      <div className="ui-trwc__name">
        {/* Deep trees stop indenting after a few levels, so the name always keeps its room */}
        {depth > 0 && <span className="ui-trwc__guides" style={{ width: `calc(min(${depth}, var(--ui-trwc-depth)) * var(--ui-trwc-indent))` }} aria-hidden="true" />}
        <span
          className="ui-trwc__chev"
          data-leaf={kids.length ? undefined : ""}
          aria-hidden="true"
          onClick={(e) => {
            if (!kids.length || leaving) return;
            e.stopPropagation();
            on.current?.fold(s.id);
          }}
        >
          {kids.length > 0 && <Chevron />}
        </span>
        <span className="ui-trwc__icon" data-logo={s.icon ? "" : undefined}>
          {s.icon ?? <span className="ui-trwc__swatch" />}
        </span>
        <span className="ui-trwc__label">
          <span className="ui-trwc__title" dir="auto">
            {s.name}
          </span>
          {s.detail && (
            <span className="ui-trwc__detail" dir="auto">
              {s.detail}
            </span>
          )}
        </span>
        {hiddenCount > 0 && <span className="ui-trwc__more">+{count(hiddenCount)}</span>}
        {s.status === "running" && !kids.length && (
          <span className="ui-trwc__state" data-status="running">
            <Spinner />
          </span>
        )}
        {s.status === "failed" && (
          <span className="ui-trwc__state" data-status="failed">
            <Warning />
          </span>
        )}
        {/* Narrow: the duration sits at the end of the name line */}
        <span className="ui-trwc__meta">
          {isSlowest && (
            <span className="ui-trwc__slow" title={t("core:trace.slowest")}>
              {t("core:trace.slowest")}
            </span>
          )}
          <span className="ui-trwc__amount" data-live={s.status === "running" ? "" : undefined}>
            {took(dur)}
            {/* The cost steps aside on the narrowest screens – it stays in the details and the totals */}
            {showCost && <span className="ui-trwc__meta-cost"> · {formatCost(cost!)}</span>}
          </span>
        </span>
      </div>
      <div className="ui-trwc__track">
        <span className="ui-trwc__bar" data-bar="" data-kind={s.kind} data-status={s.status} style={{ ...vars, animationDelay: `${delay + 60}ms` }} />
        {marks.map((m) => (
          <Mark key={m.id} kind={m.kind} status={m.status} start={m.start} dur={durOf(m)} />
        ))}
        <span className="ui-trwc__tag" data-side={side} style={vars}>
          <span className="ui-trwc__amount" data-live={s.status === "running" ? "" : undefined}>
            {label}
          </span>
          {isSlowest && <span className="ui-trwc__slow">{t("core:trace.slowest")}</span>}
        </span>
      </div>
    </div>
  );
});

export function TraceWaterfall({
  root,
  now: nowProp,
  startedAt,
  collapsed,
  defaultCollapsed = [],
  onCollapsedChange,
  openId,
  defaultOpenId = null,
  onOpenChange,
  markSlowest = true,
  minRange = 10 * SEC,
  formatCost = dollars,
  title = t("core:trace.title"),
  className,
}: TraceWaterfallProps) {
  useT();
  const rootRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const rowsRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const rowEls = useRef(new Map<string, HTMLDivElement>());

  /* ---------- Folding and the open details, controlled or not ---------- */

  const [innerCollapsed, setInnerCollapsed] = useState<string[]>(defaultCollapsed);
  const folded = collapsed ?? innerCollapsed;
  const toggle = (id: string) => {
    const next = folded.includes(id) ? folded.filter((c) => c !== id) : [...folded, id];
    setInnerCollapsed(next);
    onCollapsedChange?.(next);
  };
  const [innerOpen, setInnerOpen] = useState<string | null>(defaultOpenId);
  const all = walk(root);
  const wantOpen = openId === undefined ? innerOpen : openId;
  const openSpan = wantOpen ? all.find((s) => s.id === wantOpen) : undefined;
  const setOpen = (id: string | null) => {
    setInnerOpen(id);
    onOpenChange?.(id, id ? all.find((s) => s.id === id) : undefined);
  };

  /* ---------- The clock ---------- */

  const live = all.some((s) => s.status === "running");
  const [tick, setTick] = useState(() => Date.now());
  useEffect(() => {
    if (nowProp !== undefined || startedAt === undefined || !live) return;
    const t = window.setInterval(() => setTick(Date.now()), 100);
    return () => window.clearInterval(t);
  }, [live, nowProp, startedAt]);
  const lastEnd = Math.max(0, ...all.map((s) => s.end ?? s.start));
  const now = nowProp ?? (startedAt !== undefined && live ? tick - startedAt : lastEnd);
  const endOf = (s: TraceSpan) => s.end ?? (s.status === "running" ? Math.max(now, s.start) : s.start);
  const durOf = (s: TraceSpan) => Math.max(0, endOf(s) - s.start);

  /* ---------- The axis: a range that snaps to round numbers, ticks to fit the width ---------- */

  const [trackW, setTrackW] = useState(0);
  const latest = Math.max(live ? now : 0, ...all.map(endOf));
  const want = Math.max(minRange, latest * 1.2);
  const range = RANGES.find((r) => r >= want) ?? Math.ceil(want / 3_600_000) * 3_600_000;
  const fit = Math.min(8, Math.max(3, Math.round((trackW || 600) / 96)));
  const step = STEPS.find((s) => range / s <= fit) ?? range / fit;
  const ticks: number[] = [];
  for (let v = 0; v < range - 1; v += step) ticks.push(v);

  // Ticks that are no longer on the axis fade out where they are while the range glides
  const prevTicks = useRef<number[]>([]);
  const [fading, setFading] = useState<number[]>([]);
  const tickKey = ticks.join(",");
  useLayoutEffect(() => {
    const gone = prevTicks.current.filter((v) => !ticks.includes(v));
    prevTicks.current = ticks;
    if (!gone.length || reduced()) return;
    setFading((f) => [...f.filter((v) => !ticks.includes(v) && !gone.includes(v)), ...gone]);
    window.setTimeout(() => setFading((f) => f.filter((v) => !gone.includes(v) || prevTicks.current.includes(v))), 520);
  }, [tickKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // The range glides to its new value – written straight into a CSS variable, so bars and ticks move together every frame
  const shownRange = useRef(range);
  const rangeRaf = useRef(0);
  const firstRange = useRef(true);
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const set = (v: number) => {
      shownRange.current = v;
      el.style.setProperty("--ui-trwc-range", String(v));
    };
    cancelAnimationFrame(rangeRaf.current);
    if (firstRange.current || reduced()) {
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
  }, [range]);
  useEffect(() => () => cancelAnimationFrame(rangeRaf.current), []);

  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    const ro = new ResizeObserver(() => setTrackW(layer.offsetWidth));
    ro.observe(layer);
    return () => ro.disconnect();
  }, []);

  /* ---------- The rows: the tree, flat – folded children stay, closed to no height ---------- */

  const flat: Row[] = [];
  const visit = (s: TraceSpan, depth: number, parentId: string | undefined, hidden: boolean) => {
    flat.push({ span: s, depth, parentId, hidden });
    const shut = hidden || folded.includes(s.id);
    for (const c of s.children ?? []) visit(c, depth + 1, s.id, shut);
  };
  visit(root, 0, undefined, false);
  const ids = new Set(flat.map((r) => r.span.id));

  // Rows that leave (a new run, removed data) close up a beat after each other, bottom first
  const [ghosts, setGhosts] = useState<{ row: Row; index: number }[]>([]);
  const prevFlat = useRef<Row[]>([]);
  useLayoutEffect(() => {
    const prev = prevFlat.current;
    prevFlat.current = flat;
    if (reduced()) return;
    const gone = prev.filter((r) => !ids.has(r.span.id));
    if (!gone.length) return;
    const batch = gone.map((r, i) => ({ row: { ...r, leaving: Math.min(240, (gone.length - 1 - i) * 22) }, index: prev.indexOf(r) }));
    const out = new Set(batch.map((g) => g.row.span.id));
    setGhosts((g) => [...g.filter((x) => !out.has(x.row.span.id)), ...batch]);
    window.setTimeout(() => setGhosts((g) => g.filter((x) => !out.has(x.row.span.id))), EXIT + 300);
  });
  const rows = [...flat];
  const staying = ghosts.filter((g) => !ids.has(g.row.span.id)).sort((a, b) => a.index - b.index);
  // A whole new run (another root) comes in on top; the old one closes up below it
  const newRun = staying.some((g) => g.row.depth === 0);
  for (const g of staying) {
    if (newRun) rows.push(g.row);
    else rows.splice(Math.min(g.index, rows.length), 0, g.row);
  }
  const shownRows = rows.filter((r) => !r.hidden && r.leaving === undefined);

  // Rows that appear together slide in one after another; the first picture comes in row by row
  const enterDelay = useRef(new Map<string, number>());
  const mounted = useRef(false);
  let newcomers = 0;
  for (const r of rows) {
    if (r.leaving !== undefined || enterDelay.current.has(r.span.id)) continue;
    enterDelay.current.set(r.span.id, Math.min(400, (mounted.current ? 0 : 60) + newcomers++ * STAGGER));
  }
  useLayoutEffect(() => {
    mounted.current = true;
    const here = new Set(rows.filter((r) => r.leaving === undefined).map((r) => r.span.id));
    for (const id of [...enterDelay.current.keys()]) if (!here.has(id)) enterDelay.current.delete(id);
  });

  /* ---------- Keyboard: one row in the tab order, arrows move, left and right fold ---------- */

  const [active, setActive] = useState(root.id);
  const findRow = (id?: string) => rows.find((r) => r.span.id === id);
  let activeId = active;
  // A row that folded away or left hands the focus to its nearest shown parent
  for (let r = findRow(activeId); r && (r.hidden || r.leaving !== undefined); r = findRow(r.parentId)) activeId = r.parentId ?? root.id;
  if (!shownRows.some((r) => r.span.id === activeId)) activeId = shownRows[0]?.span.id ?? root.id;
  const focusRow = (r?: Row) => {
    if (!r) return;
    setActive(r.span.id);
    rowEls.current.get(r.span.id)?.focus();
  };
  const onKey = (e: KeyboardEvent<HTMLDivElement>, r: Row) => {
    if (e.target !== e.currentTarget) return;
    const i = shownRows.findIndex((x) => x.span.id === r.span.id);
    const kids = (r.span.children ?? []).length > 0;
    const isFolded = folded.includes(r.span.id);
    const keys: Record<string, () => void> = {
      ArrowDown: () => focusRow(shownRows[i + 1]),
      ArrowUp: () => focusRow(shownRows[i - 1]),
      Home: () => focusRow(shownRows[0]),
      End: () => focusRow(shownRows[shownRows.length - 1]),
      ArrowRight: () => (kids && isFolded ? toggle(r.span.id) : kids ? focusRow(shownRows[i + 1]) : undefined),
      ArrowLeft: () => (kids && !isFolded ? toggle(r.span.id) : focusRow(findRow(r.parentId))),
      Enter: () => setOpen(wantOpen === r.span.id ? null : r.span.id),
      " ": () => setOpen(wantOpen === r.span.id ? null : r.span.id),
      Escape: () => wantOpen && setOpen(null),
    };
    if (!keys[e.key]) return;
    e.preventDefault();
    keys[e.key]();
  };

  /* ---------- The details window: opens with a soft scale, closes the same way ---------- */

  const [closing, setClosing] = useState<TraceSpan | null>(null);
  const lastOpen = useRef<TraceSpan | undefined>(undefined);
  useLayoutEffect(() => {
    const was = lastOpen.current;
    lastOpen.current = openSpan;
    if (was && !openSpan && !reduced()) {
      setClosing(was);
      const t = window.setTimeout(() => setClosing(null), 170);
      return () => window.clearTimeout(t);
    }
    if (openSpan) setClosing(null);
  }, [openSpan?.id]); // eslint-disable-line react-hooks/exhaustive-deps
  const popSpan = openSpan ?? closing ?? undefined;

  useEffect(() => {
    if (!openSpan) return;
    const away = (e: PointerEvent) => {
      const t = e.target as Node;
      if (popRef.current?.contains(t) || rowsRef.current?.contains(t)) return;
      setOpen(null);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }); // eslint-disable-line react-hooks/exhaustive-deps

  // Under its row, starting where its bar starts – above the row when there is no room below
  useLayoutEffect(() => {
    const pop = popRef.current;
    const body = bodyRef.current;
    const row = popSpan ? rowEls.current.get(popSpan.id) : undefined;
    if (!pop || !body || !row) return;
    const b = body.getBoundingClientRect();
    const scale = body.offsetWidth ? b.width / body.offsetWidth : 1;
    const r = row.getBoundingClientRect();
    const bar = row.querySelector<HTMLElement>("[data-bar]")?.getBoundingClientRect();
    const w = pop.offsetWidth;
    const h = pop.offsetHeight;
    const rowTop = (r.top - b.top) / scale;
    const rowBottom = (r.bottom - b.top) / scale;
    const below = rowBottom + 6;
    // Above: its top edge snaps to a row boundary, so it never cuts through a row
    const rowH = row.offsetHeight || 1;
    const first = rowsRef.current ? (rowsRef.current.getBoundingClientRect().top - b.top) / scale + 4 : 0;
    const raw = rowTop - h - 6;
    const snapped = first + Math.floor((raw - first) / rowH) * rowH;
    const up = snapped >= first ? snapped : raw;
    const top = below + h > body.offsetHeight + 24 && up > 0 ? up : below;
    const start = bar ? (bar.left - b.left) / scale : 8;
    pop.style.left = `${Math.max(8, Math.min(start, body.offsetWidth - w - 8))}px`;
    pop.style.top = `${top}px`;
    pop.dataset.side = top === up ? "above" : "below";
  });

  /* ---------- The run in numbers ---------- */

  const leaves = all.filter((s) => s.kind === "llm" || s.kind === "tool");
  const [tin, tout] = tokensOf(root);
  const errors = all.filter((s) => s.status === "failed").length;
  const finished = leaves.filter((s) => s.status !== "running" && s.end !== undefined);
  const slowest = markSlowest && finished.length > 1 ? finished.reduce((a, b) => (durOf(b) > durOf(a) ? b : a)) : undefined;
  const state: TraceSpanStatus = root.status === "running" || live ? "running" : root.status === "failed" || root.status === "cancelled" ? root.status : "done";

  // Rows that run, or hold something that runs – only they get the clock
  const liveIds = new Set<string>();
  const markLive = (s: TraceSpan): boolean => {
    let running = s.status === "running";
    for (const c of s.children ?? []) if (markLive(c)) running = true;
    if (running) liveIds.add(s.id);
    return running;
  };
  markLive(root);
  // The rows call back through one stable object, so a memoized row doesn't re-render for a new handler
  const on = useRef<RowHandlers | null>(null);
  on.current = {
    click: (id) => {
      setActive(id);
      setOpen(wantOpen === id ? null : id);
    },
    fold: (id) => {
      setActive(id);
      toggle(id);
    },
    focus: (id) => setActive(id),
    key: (e, id) => {
      const r = findRow(id);
      if (r && r.leaving === undefined) onKey(e, r);
    },
    el: (id, el) => {
      if (el) rowEls.current.set(id, el);
      else rowEls.current.delete(id);
    },
  };

  /* ---------- The details ---------- */

  const details = (s: TraceSpan) => {
    const kids = (s.children ?? []).length > 0;
    const [i, o] = tokensOf(s);
    const cost = kids ? costOf(s) : s.cost;
    const stats: [string, string][] = [
      [t("core:trace.stat.duration"), took(durOf(s))],
      [t("core:trace.started"), `+${took(s.start)}`],
      ...(cost !== undefined && cost > 0 ? [[t("core:trace.stat.cost"), formatCost(cost)] as [string, string]] : []),
      ...(i > 0 ? [[t("core:trace.tokensIn"), tokens(i)] as [string, string]] : []),
      ...(o > 0 ? [[t("core:trace.tokensOut"), tokens(o)] as [string, string]] : []),
      ...(kids ? [[t("core:trace.spans"), count(walk(s).length - 1)] as [string, string]] : []),
    ];
    return (
      <div
        ref={popRef}
        className="ui-trwc__pop"
        role="dialog"
        aria-label={t("core:trace.details", { name: s.name })}
        data-closing={openSpan ? undefined : ""}
        data-kind={s.kind}
        data-status={s.status}
        onKeyDown={(e) => {
          if (e.key !== "Escape") return;
          e.stopPropagation();
          setOpen(null);
          rowEls.current.get(s.id)?.focus();
        }}
      >
        <div className="ui-trwc__pop-head">
          <span className="ui-trwc__icon" data-logo={s.icon ? "" : undefined}>
            {s.icon ?? <span className="ui-trwc__swatch" />}
          </span>
          <span className="ui-trwc__pop-who">
            <span className="ui-trwc__pop-name" dir="auto">
              {s.name}
            </span>
            <span className="ui-trwc__pop-kind">
              {KIND(s.kind)}
              {s.model ? ` · ${s.model}` : s.detail ? ` · ${s.detail}` : ""}
            </span>
          </span>
          <span className="ui-trwc__pill" data-status={s.status}>
            {STATUS(s.status)}
          </span>
          <button
            type="button"
            className="ui-trwc__close"
            aria-label={t("core:trace.close")}
            onClick={() => {
              setOpen(null);
              rowEls.current.get(s.id)?.focus();
            }}
          >
            <Cancel />
          </button>
        </div>
        <dl className="ui-trwc__pop-stats">
          {stats.map(([k, v]) => (
            <div key={k}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
        {s.error && (
          <p className="ui-trwc__pop-error">
            <Warning />
            <span className="ui-trwc__pop-error-text" dir="auto">
              {s.error}
            </span>
          </p>
        )}
        {s.input && (
          <div className="ui-trwc__pop-io">
            <span>{t("core:trace.input")}</span>
            <p dir="auto">{s.input}</p>
          </div>
        )}
        {s.output && (
          <div className="ui-trwc__pop-io">
            <span>{t("core:trace.output")}</span>
            <p dir="auto">{s.output}</p>
          </div>
        )}
      </div>
    );
  };

  /* Steve: cost and tokens only where the Hub knows them – an unknown is left out, never shown as $0 */
  const knownCost = all.some((s) => s.cost !== undefined), knownTokens = tin + tout > 0;
  const stats: { label: string; value: string; full?: string; live?: boolean; kind?: string }[] = [
    { label: t("core:trace.stat.duration"), value: short(durOf(root)), full: took(durOf(root)), live: state === "running" },
    ...(knownCost ? [{ label: t("core:trace.stat.cost"), value: formatCost(costOf(root)), live: state === "running" }] : []),
    ...(knownTokens ? [{ label: t("core:trace.stat.tokens"), value: tokens(tin + tout), live: state === "running" }] : []),
    { label: t("core:trace.stat.llm"), value: count(all.filter((s) => s.kind === "llm" && !s.uncounted).length) },
    { label: t("core:trace.stat.tools"), value: count(all.filter((s) => s.kind === "tool" || s.kind === "subagent").length) },
    { label: t("core:trace.stat.errors"), value: count(errors), kind: errors ? "failed" : undefined },
  ];

  const tickList = [...ticks.map((v) => ({ v, leaving: false })), ...fading.filter((v) => !ticks.includes(v)).map((v) => ({ v, leaving: true }))];

  return (
    <div ref={rootRef} className={["ui-trwc", className].filter(Boolean).join(" ")} data-live={state === "running" ? "" : undefined} style={{ "--ui-trwc-at": now } as CSSProperties}>
      <style href="ui-trace-waterfall-clean" precedence="default">
        {css}
      </style>

      <header className="ui-trwc__bar-head">
        <span className="ui-trwc__spark">
          <Spark />
        </span>
        <span className="ui-trwc__heading" title={title}>
          {title}
        </span>
        <span className="ui-trwc__task" dir="auto">
          {root.input ?? root.name}
        </span>
        <span className="ui-trwc__pill" data-status={state} aria-live="polite">
          {state === "running" && <span className="ui-trwc__pulse" aria-hidden="true" />}
          {state === "running" ? t("core:trace.live") : STATUS(state)}
        </span>
      </header>

      <div className="ui-trwc__card">
        <dl className="ui-trwc__stats" data-cols={stats.length} style={{ "--ui-trwc-cols": stats.length } as CSSProperties}>
          {stats.map((s) => (
            <div key={s.label} className="ui-trwc__stat" data-kind={s.kind}>
              <dt>{s.label}</dt>
              <dd title={s.full && s.full !== s.value ? s.full : undefined}>
                <span className="ui-trwc__amount" data-live={s.live ? "" : undefined}>
                  {s.value}
                </span>
              </dd>
            </div>
          ))}
        </dl>

        <div ref={bodyRef} className="ui-trwc__body">
          {/* Ticks, grid lines and the "now" line – positioned against the gliding range */}
          <div ref={layerRef} className="ui-trwc__layer" aria-hidden="true">
            {tickList.map(({ v, leaving }) => (
              <span key={v} className="ui-trwc__tick" data-zero={v === 0 ? "" : undefined} data-edge={v > range * 0.84 ? "" : undefined} data-leaving={leaving ? "" : undefined} style={{ "--v": v } as CSSProperties}>
                <span>{tickLabel(v)}</span>
              </span>
            ))}
            {state === "running" && <span className="ui-trwc__now" />}
          </div>
          <div className="ui-trwc__axis" aria-hidden="true">
            <span>{t("core:trace.span")}</span>
          </div>
          <div ref={rowsRef} className="ui-trwc__rows" role="tree" aria-label={title}>
            {rows.map((r) => (
              <SpanRow
                key={r.leaving !== undefined ? `${r.span.id}~out` : r.span.id}
                span={r.span}
                depth={r.depth}
                hidden={r.hidden}
                leaving={r.leaving}
                folded={(r.span.children ?? []).length > 0 && folded.includes(r.span.id)}
                open={wantOpen === r.span.id}
                focusable={!r.hidden && r.leaving === undefined && activeId === r.span.id}
                slowest={slowest?.id === r.span.id}
                now={liveIds.has(r.span.id) ? now : undefined}
                range={range}
                trackW={trackW}
                delay={enterDelay.current.get(r.span.id) ?? 0}
                formatCost={formatCost}
                on={on}
              />
            ))}
          </div>
          {popSpan && details(popSpan)}
        </div>
      </div>
    </div>
  );
}


const css = /* css */ `
/* Colors, sizes and motion as variables – override them on .ui-trwc */
.ui-trwc {
  /* Steve: every color comes from Steve's tokens, which already switch between light and dark. Thinking is the one
   * indigo kind, tool calls a stronger slate, the turn itself a quiet slate; status colors only for status. */
  --ui-trwc-card: var(--aui-surface);
  --ui-trwc-strong: var(--aui-ink);
  --ui-trwc-ink: var(--aui-ink);
  --ui-trwc-text: var(--aui-muted);
  --ui-trwc-muted: var(--aui-placeholder);
  --ui-trwc-faint: var(--aui-placeholder);
  --ui-trwc-line: var(--aui-border);
  --ui-trwc-grid: color-mix(in srgb, var(--aui-border) 55%, transparent);
  --ui-trwc-guide: var(--aui-border);
  --ui-trwc-hover: color-mix(in srgb, var(--aui-ink) 3%, transparent);
  --ui-trwc-selected: color-mix(in srgb, var(--aui-primary) 6%, transparent);
  --ui-trwc-chip: var(--aui-subtle);
  --ui-trwc-fill: var(--aui-subtle);
  --ui-trwc-wash: var(--aui-subtle);
  --ui-trwc-accent: var(--aui-primary);
  --ui-trwc-now: color-mix(in srgb, var(--aui-primary) 55%, transparent);
  --ui-trwc-agent-bg: color-mix(in srgb, var(--aui-ink) 5%, transparent);
  --ui-trwc-agent-edge: color-mix(in srgb, var(--aui-ink) 12%, transparent);
  --ui-trwc-agent-ink: var(--aui-muted);
  --ui-trwc-agent-mark: var(--aui-placeholder);
  --ui-trwc-llm-bg: color-mix(in srgb, var(--aui-info) 14%, transparent);
  --ui-trwc-llm-edge: color-mix(in srgb, var(--aui-info) 38%, transparent);
  --ui-trwc-llm-ink: var(--aui-info);
  --ui-trwc-llm-mark: var(--aui-info);
  --ui-trwc-tool-bg: color-mix(in srgb, var(--aui-ink) 11%, transparent);
  --ui-trwc-tool-edge: color-mix(in srgb, var(--aui-ink) 24%, transparent);
  --ui-trwc-tool-ink: var(--aui-ink);
  --ui-trwc-tool-mark: var(--aui-muted);
  --ui-trwc-error-bg: var(--aui-danger-soft);
  --ui-trwc-error-edge: color-mix(in srgb, var(--aui-danger) 45%, transparent);
  --ui-trwc-error-ink: var(--aui-danger);
  --ui-trwc-error-mark: var(--aui-danger);
  --ui-trwc-brand-bg: var(--aui-info-soft);
  --ui-trwc-brand-fg: var(--aui-info);
  --ui-trwc-ok-bg: var(--aui-success-soft);
  --ui-trwc-ok-fg: var(--aui-success);
  --ui-trwc-slow-bg: var(--aui-warning-soft);
  --ui-trwc-slow-fg: var(--aui-warning);
  --ui-trwc-focus: var(--aui-focus);
  --ui-trwc-shine: color-mix(in srgb, var(--aui-ink) 35%, transparent);
  --ui-trwc-shadow: var(--aui-shadow-card);
  --ui-trwc-pop: var(--aui-shadow-popover);
  --ui-trwc-row: 30px;
  --ui-trwc-bar: 14px;
  --ui-trwc-axis: 30px;
  --ui-trwc-name: 232px;
  --ui-trwc-pad: 10px;
  --ui-trwc-gap: 14px;
  --ui-trwc-end: 16px;
  --ui-trwc-indent: 14px;
  /* Deeper levels stop indenting, so a deep tree keeps room for its names */
  --ui-trwc-depth: 8;
  --ui-trwc-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  --ui-trwc-range: 10000;
  position: relative;
  box-sizing: border-box;
  width: 100%;
  border-radius: calc(var(--aui-radius-lg, 10px) + 2px);
  background: var(--aui-surface, var(--ui-trwc-card));
  box-shadow: var(--aui-shadow-card, var(--ui-trwc-shadow));
  container: ui-trwc / inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-trwc-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-trwc *, .ui-trwc *::before, .ui-trwc *::after { box-sizing: border-box; }
:where(.ui-trwc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-trwc button:focus-visible { outline: 2px solid var(--ui-trwc-focus); outline-offset: 2px; }
.ui-trwc :is(p, dl, dd) { margin: 0; }
.ui-trwc svg { display: block; flex: none; }

/* ----- The bar: what ran, and whether it still runs ----- */
.ui-trwc__bar-head { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 12px 14px 12px 16px; border-bottom: 0.5px solid var(--ui-trwc-line); }
.ui-trwc__spark { display: grid; flex: none; place-items: center; width: 16px; color: var(--ui-trwc-accent); }
.ui-trwc__heading { flex: 0 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-weight: 500; color: var(--ui-trwc-strong); }
.ui-trwc__task { flex: 1; min-width: 0; overflow: hidden; color: var(--ui-trwc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-trwc__pill { display: inline-flex; flex: none; align-items: center; gap: 5px; height: 20px; padding: 0 7px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-trwc-fill); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-trwc-text); white-space: nowrap; }
.ui-trwc__pill[data-status="running"] { background: var(--ui-trwc-brand-bg); color: var(--ui-trwc-brand-fg); }
.ui-trwc__pill[data-status="done"] { background: var(--ui-trwc-ok-bg); color: var(--ui-trwc-ok-fg); }
.ui-trwc__pill[data-status="failed"] { background: var(--ui-trwc-error-bg); color: var(--ui-trwc-error-ink); }
.ui-trwc__pill[data-status="cancelled"] { color: var(--ui-trwc-muted); }
.ui-trwc__pulse { width: 6px; height: 6px; border-radius: 50%; background: currentColor; animation: ui-trwc-pulse 1400ms ease-in-out infinite; }

/* ----- The run in numbers, then the waterfall – all on the one card ----- */
.ui-trwc__stats { display: grid; grid-template-columns: repeat(var(--ui-trwc-cols, 6), minmax(0, 1fr)); padding: 12px 0; border-bottom: 0.5px solid var(--ui-trwc-line); }
.ui-trwc__stat { display: grid; gap: 2px; min-width: 0; padding: 0 16px; }
/* The first value a little further from the edge, the last one a little closer – so the row looks even */
.ui-trwc__stat:first-child { padding-left: 22px; }
.ui-trwc__stat:last-child { padding-right: 10px; }
.ui-trwc__stat + .ui-trwc__stat { border-left: 0.5px solid var(--ui-trwc-line); }
.ui-trwc__stat dt { overflow: hidden; font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-trwc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-trwc__stat dd { overflow: hidden; font-size: 15px; font-weight: 500; line-height: 24px; color: var(--ui-trwc-ink); white-space: nowrap; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
.ui-trwc__stat[data-kind="failed"] dd { color: var(--ui-trwc-error-ink); }

.ui-trwc__body { position: relative; padding-bottom: 18px; }
/* The axis and its grid lines run behind the rows; the range glides, so everything is placed in calc() */
.ui-trwc__layer { position: absolute; top: 0; bottom: 0; left: calc(var(--ui-trwc-pad) + var(--ui-trwc-name) + var(--ui-trwc-gap)); right: var(--ui-trwc-end); overflow: hidden; pointer-events: none; }
.ui-trwc__tick { position: absolute; top: 0; bottom: 0; left: calc(var(--v) * 100% / var(--ui-trwc-range)); width: 0.5px; background: var(--ui-trwc-grid); animation: ui-trwc-fade 420ms ease both; transition: opacity 420ms ease; }
.ui-trwc__tick[data-zero] { width: 0.5px; background: rgba(0, 0, 0, 0.1); }
.ui-trwc__tick[data-leaving] { opacity: 0; }
.ui-trwc__tick > span { position: absolute; top: 7px; left: 6px; font-size: 12px; line-height: 16px; color: var(--ui-trwc-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
/* Steve: a label too near the end of the axis would be cut off – its line stays, the label goes */
.ui-trwc__tick[data-edge] > span { display: none; }
.ui-trwc__now { position: absolute; top: 0; bottom: 0; left: calc(var(--ui-trwc-at) * 100% / var(--ui-trwc-range)); width: 1px; background: var(--ui-trwc-now); }
.ui-trwc__now::before { content: ""; position: absolute; top: calc(var(--ui-trwc-axis) - 2.5px); left: -2px; width: 5px; height: 5px; border-radius: 50%; background: var(--ui-trwc-accent); box-shadow: 0 0 0 3px var(--aui-surface, var(--ui-trwc-card)); }
.ui-trwc__axis { display: flex; align-items: center; height: var(--ui-trwc-axis); padding-left: calc(var(--ui-trwc-pad) + 6px); border-bottom: 0.5px solid var(--ui-trwc-line); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-trwc-muted); }
.ui-trwc__rows { position: relative; padding-top: 4px; }

/* ----- One row: its name on the left, its bar on the right ----- */
.ui-trwc__row {
  display: grid;
  grid-template-columns: var(--ui-trwc-name) minmax(0, 1fr);
  column-gap: var(--ui-trwc-gap);
  height: var(--ui-trwc-row);
  padding: 0 var(--ui-trwc-end) 0 var(--ui-trwc-pad);
  overflow: hidden;
  outline: none;
  cursor: pointer;
  transition: height 360ms var(--ui-trwc-ease), opacity 260ms ease, background-color 140ms ease;
  animation: ui-trwc-row 420ms var(--ui-trwc-ease) backwards;
}
.ui-trwc__row:hover { background: var(--ui-trwc-hover); }
.ui-trwc__row[data-open] { background: var(--ui-trwc-selected); }
.ui-trwc__row:focus-visible { outline: 2px solid var(--ui-trwc-focus); outline-offset: -2px; }
/* Folded away or leaving: the row closes to no height */
.ui-trwc__row:is([data-hidden], [data-leaving]) { height: 0; opacity: 0; pointer-events: none; }
.ui-trwc__row[data-leaving] { animation: ui-trwc-leave ${EXIT}ms var(--ui-trwc-ease) both; }
.ui-trwc__row[data-leaving][data-hidden] { animation: none; }
.ui-trwc__row[data-leaving] :is(.ui-trwc__bar, .ui-trwc__tag, .ui-trwc__more, .ui-trwc__slow) { animation: none; }

.ui-trwc__name { display: flex; align-items: center; min-width: 0; height: var(--ui-trwc-row); }
/* One faint line per level, under the chevron of each parent */
.ui-trwc__guides { flex: none; align-self: stretch; background: repeating-linear-gradient(90deg, transparent 0 7.75px, var(--ui-trwc-guide) 7.75px 8.25px, transparent 8.25px var(--ui-trwc-indent)); }
.ui-trwc__chev { display: grid; flex: none; place-items: center; width: 16px; height: 20px; margin-right: 4px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); color: var(--ui-trwc-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-trwc__chev:not([data-leaf]):hover { background: var(--ui-trwc-fill); color: var(--ui-trwc-ink); }
.ui-trwc__chev > svg { transition: transform 240ms var(--ui-trwc-ease); }
.ui-trwc__row[data-folded] .ui-trwc__chev > svg { transform: rotate(-90deg); }
.ui-trwc__icon { display: grid; flex: none; place-items: center; width: 20px; height: 20px; margin-right: 8px; border-radius: 5px; background: var(--ui-trwc-card); box-shadow: var(--ui-trwc-shadow); }
.ui-trwc__icon > svg { width: 12px; height: 12px; }
.ui-trwc__swatch { width: 8px; height: 8px; border-radius: 2px; background: var(--k-mark); }
.ui-trwc__label { display: flex; align-items: baseline; gap: 6px; flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; }
.ui-trwc__title { flex: none; max-width: 100%; overflow: hidden; color: var(--ui-trwc-ink); text-overflow: ellipsis; }
.ui-trwc__row[data-parent] .ui-trwc__title { font-weight: 500; color: var(--ui-trwc-strong); }
.ui-trwc__detail { min-width: 0; overflow: hidden; font-size: 12px; color: var(--ui-trwc-muted); text-overflow: ellipsis; }
.ui-trwc__more { flex: none; height: 18px; margin-left: 6px; padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-trwc-fill); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-trwc-text); font-variant-numeric: tabular-nums; animation: ui-trwc-pop 220ms var(--ui-trwc-ease) both; }
.ui-trwc__state { display: grid; flex: none; place-items: center; width: 16px; margin-left: 4px; }
.ui-trwc__state[data-status="running"] { color: var(--ui-trwc-brand-fg); }
.ui-trwc__state[data-status="failed"] { color: var(--ui-trwc-error-ink); }
.ui-trwc__spin { animation: ui-trwc-spin 900ms linear infinite; }
.ui-trwc__meta { display: none; }

/* ----- The bar ----- */
.ui-trwc [data-kind] { --k-bg: var(--ui-trwc-agent-bg); --k-edge: var(--ui-trwc-agent-edge); --k-ink: var(--ui-trwc-agent-ink); --k-mark: var(--ui-trwc-agent-mark); }
.ui-trwc [data-kind="llm"] { --k-bg: var(--ui-trwc-llm-bg); --k-edge: var(--ui-trwc-llm-edge); --k-ink: var(--ui-trwc-llm-ink); --k-mark: var(--ui-trwc-llm-mark); }
.ui-trwc [data-kind="tool"] { --k-bg: var(--ui-trwc-tool-bg); --k-edge: var(--ui-trwc-tool-edge); --k-ink: var(--ui-trwc-tool-ink); --k-mark: var(--ui-trwc-tool-mark); }
.ui-trwc [data-kind][data-status="failed"] { --k-bg: var(--ui-trwc-error-bg); --k-edge: var(--ui-trwc-error-edge); --k-ink: var(--ui-trwc-error-ink); --k-mark: var(--ui-trwc-error-mark); }
.ui-trwc__track { position: relative; min-width: 0; height: var(--ui-trwc-row); overflow: hidden; }
.ui-trwc__bar {
  position: absolute;
  top: calc(50% - var(--ui-trwc-bar) / 2);
  left: calc(var(--s) * 100% / var(--ui-trwc-range));
  width: max(3px, calc(var(--d) * 100% / var(--ui-trwc-range)));
  height: var(--ui-trwc-bar);
  overflow: hidden;
  border-radius: 4px;
  background: var(--k-bg);
  box-shadow: inset 0 0 0 0.5px var(--k-edge);
  transition: box-shadow 160ms ease, background-color 240ms ease, opacity 240ms ease;
  animation: ui-trwc-grow 560ms var(--ui-trwc-ease) backwards;
}
.ui-trwc__row:is(:hover, [data-open]) .ui-trwc__bar { box-shadow: inset 0 0 0 1px var(--k-mark); }
/* Folded: the parent bar steps back, its children show on it as small marks */
.ui-trwc__row[data-folded] .ui-trwc__bar { opacity: 0.75; }
.ui-trwc__mark { position: absolute; top: calc(50% - 2.5px); left: calc(var(--s) * 100% / var(--ui-trwc-range)); width: max(2px, calc(var(--d) * 100% / var(--ui-trwc-range))); height: 5px; border-radius: 1.5px; background: var(--k-mark); opacity: 0; transform: scaleY(0.2); transition: opacity 200ms ease, transform 200ms var(--ui-trwc-ease); pointer-events: none; }
.ui-trwc__row[data-folded] .ui-trwc__mark { opacity: 0.9; transform: none; transition-delay: 140ms; }
/* Still running: a sheen sweeps along and the leading edge glows */
.ui-trwc__bar[data-status="running"]::before { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent, var(--ui-trwc-card), transparent) -60px 0 / 60px 100% no-repeat; opacity: 0.7; animation: ui-trwc-sweep 1500ms linear infinite; }
.ui-trwc__bar[data-status="running"]::after { content: ""; position: absolute; top: 0; right: 0; bottom: 0; width: min(28px, 100%); background: linear-gradient(90deg, transparent, var(--k-edge)); animation: ui-trwc-glow 1100ms ease-in-out infinite alternate; }

/* How long it took – on the card's color, so grid lines and the "now" line pass behind it */
.ui-trwc__tag { position: absolute; top: calc(50% - 9px); display: flex; align-items: center; gap: 6px; height: 18px; padding: 0 4px; border-radius: 4px; background: var(--aui-surface, var(--ui-trwc-card)); font-size: 12px; line-height: 16px; color: var(--ui-trwc-text); white-space: nowrap; font-variant-numeric: tabular-nums; pointer-events: none; }
.ui-trwc__tag[data-side="after"] { left: calc((var(--s) + var(--d)) * 100% / var(--ui-trwc-range) + 2px); }
.ui-trwc__tag[data-side="inside"] { right: calc(100% - (var(--s) + var(--d)) * 100% / var(--ui-trwc-range) + 2px); flex-direction: row-reverse; background: none; color: var(--k-ink); }
.ui-trwc__tag[data-side="before"] { right: calc(100% - var(--s) * 100% / var(--ui-trwc-range) + 2px); flex-direction: row-reverse; }
.ui-trwc__row[data-open] .ui-trwc__tag { background: none; }
.ui-trwc__row[data-status="failed"] .ui-trwc__tag { color: var(--ui-trwc-error-ink); }
.ui-trwc__slow { flex: none; height: 16px; padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-trwc-slow-bg); font-size: 10px; font-weight: 500; line-height: 16px; letter-spacing: 0.3px; text-transform: uppercase; color: var(--ui-trwc-slow-fg); animation: ui-trwc-pop 260ms var(--ui-trwc-ease) both; }
/* Numbers that still climb: a light shimmer runs over them */
.ui-trwc__amount[data-live] {
  background: linear-gradient(90deg, currentColor 0%, currentColor 38%, var(--ui-trwc-shine) 50%, currentColor 62%, currentColor 100%) 0 0 / 300% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: ui-trwc-shine 1100ms linear infinite;
}

/* ----- The details window ----- */
.ui-trwc__pop { position: absolute; top: 0; left: 0; z-index: 5; display: grid; gap: 10px; width: min(320px, calc(100% - 16px)); padding: 12px; border-radius: var(--aui-radius-lg, 10px); background: var(--aui-overlay, var(--ui-trwc-card)); box-shadow: var(--aui-shadow-popover, var(--ui-trwc-pop)); cursor: auto; transform-origin: 24px top; animation: ui-trwc-open 220ms var(--ui-trwc-ease) both; }
.ui-trwc__pop[data-side="above"] { transform-origin: 24px bottom; }
.ui-trwc__pop[data-closing] { animation: ui-trwc-close 160ms ease forwards; pointer-events: none; }
.ui-trwc__pop-head { display: flex; align-items: center; min-width: 0; }
.ui-trwc__pop-head .ui-trwc__icon { width: 28px; height: 28px; margin-right: 10px; border-radius: 6px; }
.ui-trwc__pop-head .ui-trwc__icon > svg { width: 15px; height: 15px; }
.ui-trwc__pop-who { display: grid; flex: 1; min-width: 0; }
.ui-trwc__pop-name { overflow: hidden; font-weight: 500; line-height: 18px; color: var(--ui-trwc-strong); overflow-wrap: anywhere; }
.ui-trwc__pop-kind { overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-trwc-muted); overflow-wrap: anywhere; }
.ui-trwc__pop-head .ui-trwc__pill { height: 18px; margin-left: 8px; padding: 0 6px; font-size: 11px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); }
.ui-trwc__close { display: grid; flex: none; place-items: center; width: 28px; height: 28px; margin: -2px -4px 0 4px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-trwc-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-trwc__close:hover { background: var(--ui-trwc-fill); color: var(--ui-trwc-ink); }
.ui-trwc__pop-stats { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; padding: 8px 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); background: var(--ui-trwc-wash); box-shadow: inset 0 0 0 0.5px var(--ui-trwc-line); }
.ui-trwc__pop-stats dt { font-size: 10px; font-weight: 500; line-height: 16px; letter-spacing: 0.5px; text-transform: uppercase; color: var(--ui-trwc-muted); }
.ui-trwc__pop-stats dd { font-weight: 500; line-height: 20px; color: var(--ui-trwc-ink); font-variant-numeric: tabular-nums; }
.ui-trwc__pop-error { display: flex; gap: 8px; padding: 8px 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); background: var(--ui-trwc-error-bg); font-size: 12px; line-height: 16px; color: var(--ui-trwc-error-ink); }
.ui-trwc__pop-error > svg { margin-top: 2px; }
.ui-trwc__pop-error-text { min-width: 0; overflow-wrap: anywhere; }
.ui-trwc__pop-io { display: grid; gap: 2px; }
.ui-trwc__pop-io > span { font-size: 10px; font-weight: 500; line-height: 16px; letter-spacing: 0.5px; text-transform: uppercase; color: var(--ui-trwc-muted); }
.ui-trwc__pop-io > p { display: -webkit-box; overflow: hidden; overflow-wrap: anywhere; font-size: 12px; line-height: 16px; color: var(--ui-trwc-text); -webkit-line-clamp: 3; -webkit-box-orient: vertical; }

/* ----- Narrow: the name on its own line, its bar under it across the whole width ----- */
@container ui-trwc (max-width: 560px) {
  .ui-trwc__bar-head { padding: 10px 12px; }
  .ui-trwc__stats { grid-template-columns: repeat(3, minmax(0, 1fr)); row-gap: 10px; padding: 10px 0; }
  .ui-trwc__stat { padding: 0 12px; }
  .ui-trwc__stat:is(:first-child, :nth-child(4)) { padding-left: 16px; }
  .ui-trwc__stat:is(:nth-child(3), :last-child) { padding-right: 8px; }
  .ui-trwc__stat:nth-child(4) { border-left: 0; }
  /* Steve: four numbers (no cost or tokens known) stay on one line */
  .ui-trwc__stats[data-cols="4"] { grid-template-columns: repeat(4, minmax(0, 1fr)); }
  .ui-trwc__stats[data-cols="4"] .ui-trwc__stat:nth-child(4) { padding-left: 12px; border-left: 0.5px solid var(--ui-trwc-line); }
  /* A container query can't restyle its own container – the sizes change on the body */
  .ui-trwc__body { --ui-trwc-row: 46px; --ui-trwc-depth: 6; --ui-trwc-bar: 10px; --ui-trwc-end: 12px; }
  .ui-trwc__layer { left: var(--ui-trwc-pad); right: var(--ui-trwc-end); }
  /* The grid lines would cross the names – here they only mark the axis, and "now" is a dot on it */
  .ui-trwc__tick { bottom: auto; height: var(--ui-trwc-axis); }
  .ui-trwc__now { bottom: auto; height: var(--ui-trwc-axis); }
  .ui-trwc__axis > span { display: none; }
  .ui-trwc__row { grid-template-columns: minmax(0, 1fr); grid-template-rows: 28px 12px; column-gap: 0; }
  .ui-trwc__name { height: 28px; }
  .ui-trwc__track { height: 12px; }
  .ui-trwc__bar { border-radius: 3px; }
  .ui-trwc__mark { top: calc(50% - 2px); height: 4px; }
  .ui-trwc__tag { display: none; }
  .ui-trwc__meta { display: flex; flex: none; align-items: center; gap: 6px; margin-left: 8px; font-size: 12px; line-height: 16px; color: var(--ui-trwc-text); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .ui-trwc__row[data-status="failed"] .ui-trwc__meta { color: var(--ui-trwc-error-ink); }
  .ui-trwc__pop-stats { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
/* Phones: the name gets the room – only the duration beside it, "Slowest" becomes a small dot */
@container ui-trwc (max-width: 400px) {
  .ui-trwc__stat { padding-inline: 10px; }
  .ui-trwc__stat:is(:first-child, :nth-child(4)) { padding-left: 14px; }
  .ui-trwc__stat dt { letter-spacing: 0.2px; }
  .ui-trwc__meta-cost { display: none; }
  .ui-trwc__meta .ui-trwc__slow { width: 6px; height: 6px; padding: 0; border-radius: 50%; background: var(--ui-trwc-slow-fg); font-size: 0; }
}
@container ui-trwc (max-width: 360px) {
  .ui-trwc__detail { display: none; }
}

@keyframes ui-trwc-row { from { height: 0; opacity: 0; transform: translateY(-4px); } }
@keyframes ui-trwc-leave { from { height: var(--ui-trwc-row); opacity: 1; } to { height: 0; opacity: 0; } }
@keyframes ui-trwc-grow { from { clip-path: inset(0 100% 0 0 round 4px); } to { clip-path: inset(0 0 0 0 round 4px); } }
@keyframes ui-trwc-fade { from { opacity: 0; } }
@keyframes ui-trwc-pop { from { opacity: 0; transform: scale(0.9); } }
@keyframes ui-trwc-open { from { opacity: 0; transform: translateY(-4px) scale(0.96); } }
@keyframes ui-trwc-close { to { opacity: 0; transform: translateY(-3px) scale(0.97); } }
@keyframes ui-trwc-sweep { to { background-position: calc(100% + 60px) 0; } }
@keyframes ui-trwc-glow { from { opacity: 0.45; } to { opacity: 1; } }
@keyframes ui-trwc-pulse { 50% { opacity: 0.35; } }
@keyframes ui-trwc-spin { to { transform: rotate(360deg); } }
@keyframes ui-trwc-shine { from { background-position: 100% 0; } to { background-position: 0 0; } }

@media (pointer: coarse) {
  .ui-trwc__chev { width: 24px; height: 28px; margin-left: -4px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-trwc *, .ui-trwc *::before, .ui-trwc *::after { animation: none !important; transition: none !important; }
}

/* Steve: dark mode comes with the tokens above */
`;
