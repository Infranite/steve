/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";

/*
 * Agent Trail (Clean) – the way an agent works through a task, in a single self-contained file.
 * A small header names the task and counts the steps ("3/5"). Under it one quiet trail: done steps get a tinted check
 * and how long they took, the one the agent is on spins and says what it does with a live timer, and the work inside
 * that step branches off to the side as small sub-steps with a detail each. A step that needs your approval before it
 * runs has a dashed ring and an "Approval" badge. While it works, a short fading light runs down the trail from the
 * step into the sub-step it is on.
 * Clean style: one white card with hairline shadows.
 * The styles are injected below, so no extra files are needed.
 */

export type TrailStepState = "done" | "running" | "pending" | "waiting" | "failed" | "declined";
export type TrailSubstepState = "done" | "running" | "pending";

export type TrailSubstep = {
  id?: string;
  title: string;
  /** done by default */
  state?: TrailSubstepState;
  /** A short fact on the right – a file, "+18 −3", "182/214" */
  detail?: string;
};

export type TrailStep = {
  id: string;
  title: string;
  state: TrailStepState;
  /** How long it took – ms as a number ("12s", "1m 4s"), or your own text */
  duration?: number | string;
  /** What it is doing right now, on the right of a running step – "Testing" */
  status?: string;
  /** When a running step started (ms, Date.now()) – shows a live timer next to its status */
  startedAt?: number;
  /** Needs a person's approval before it runs – a dashed ring and "Approval" while it waits its turn */
  gate?: boolean;
  /** What it does inside the step – branches off the trail under it */
  substeps?: TrailSubstep[];
  /** The tool or system the step works in – its logo sits before the title (GitHub, Linear, Notion …) */
  tool?: { logo: ReactNode; name: string };
  /** What a waiting step asks you to approve – shown when you open "Needs you" */
  ask?: string;
};

export type AgentTrailLabels = {
  title: string;
  running: string;
  approval: string;
  waiting: string;
  failed: string;
  /** A step you declined at "Needs you" */
  declined: string;
  done: string;
  pending: string;
  /** No steps at all */
  empty: string;
  /** A step or sub-step without a title */
  untitled: string;
  /** "+35 earlier" / "+3 more" over and under a long list of sub-steps */
  earlier: string;
  more: string;
  fewer: string;
  approve: string;
  decline: string;
};

export type AgentTrailProps = {
  steps: TrailStep[];
  /** The task, in the header – leave it out for no header */
  title?: string;
  /** Who works on it, on the right of the header – the agent or model's name and logo */
  agent?: { name: string; logo?: ReactNode };
  /** Which steps show their sub-steps – by default only the running one */
  expanded?: string[];
  onExpandedChange?: (ids: string[]) => void;
  /** A waiting step was approved – "Needs you" opens with Approve and Decline when either is set */
  onApprove?: (step: TrailStep) => void;
  onDecline?: (step: TrailStep) => void;
  labels?: Partial<AgentTrailLabels>;
  className?: string;
};

const defaults: AgentTrailLabels = {
  title: "Progress",
  running: "Working",
  approval: "Approval",
  waiting: "Needs you",
  failed: "Failed",
  declined: "Declined",
  done: "Done",
  pending: "Up next",
  empty: "No steps yet",
  untitled: "Untitled step",
  earlier: "earlier",
  more: "more",
  fewer: "Show fewer",
  approve: "Approve",
  decline: "Decline",
};

/* ---------- Helpers ---------- */

/** 12s, 1m 4s, 1h 2m – a string is shown as it is, anything broken as nothing */
export function formatDuration(d: number | string | undefined) {
  if (typeof d === "string") return d;
  if (typeof d !== "number" || !Number.isFinite(d) || d < 0) return "";
  const s = Math.round(d / 1000);
  if (s < 1) return "<1s";
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const days = Math.floor(h / 24);
  return h % 24 ? `${days}d ${h % 24}h` : `${days}d`;
}

/** Long details keep their end – a path's file and line, a hash's tail: "src/…/PaymentElementWrapper.tsx:1284" */
function shorten(text: string, max = 22) {
  const chars = [...text];
  if (chars.length <= max) return text;
  const tail = Math.ceil(max * 0.62);
  return `${chars.slice(0, max - tail - 1).join("")}…${chars.slice(-tail).join("")}`;
}

const STATES: TrailStepState[] = ["done", "running", "pending", "waiting", "failed", "declined"];
const cleanSteps = (steps: unknown) =>
  (Array.isArray(steps) ? steps : [])
    .filter((s): s is TrailStep => !!s && typeof s === "object" && !!(s as TrailStep).id)
    .map((s) => ({ ...s, title: String(s.title ?? ""), state: STATES.includes(s.state) ? s.state : "pending" }) as TrailStep);
const subsOf = (s: TrailStep) => (Array.isArray(s.substeps) ? s.substeps.filter((x) => !!x && typeof x === "object") : []);

/** The time now, once a second – only while something needs it */
function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

/** Touch screens get taller rows – the same media query the styles use */
const TOUCH = "(hover: none) and (pointer: coarse)";
function useTouch() {
  const [touch, setTouch] = useState(false);
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia(TOUCH);
    const on = () => setTouch(mq.matches);
    on();
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, []);
  return touch;
}

/* Geometry – every row has a fixed height, so the trail can be drawn from counts alone (taller on touch screens) */
const ROW = 32;
const SUB = 24;
const ROW_TOUCH = 40;
const SUB_TOUCH = 28;
const PAD = 2;
/** Sub-steps shown before the rest folds into "+N earlier" */
const VISIBLE = 5;
/** The marker's center, and where the sub-steps' line runs */
const X = 9;
const XS = 21;
/** The light: this many slices of this length, from 0 at the back to full at the front */
const SLICES = 14;
const SLICE = 2;

/* ---------- Component ---------- */

export function AgentTrail({ steps, title, agent, expanded: expandedProp, onExpandedChange, onApprove, onDecline, labels: labelsProp, className }: AgentTrailProps) {
  const labels: AgentTrailLabels = { ...defaults, ...labelsProp };
  const list = cleanSteps(steps);
  const uid = useId();
  const now = useNow(list.some((s) => s.state === "running" && typeof s.startedAt === "number"));
  const touch = useTouch();
  const geo = { row: touch ? ROW_TOUCH : ROW, sub: touch ? SUB_TOUCH : SUB };

  // Which steps are open: the running one by default, until someone opens or closes one by hand
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const isOpen = (s: TrailStep) => {
    if (!subsOf(s).length) return false;
    if (expandedProp) return expandedProp.includes(s.id);
    return toggled[s.id] ?? s.state === "running";
  };
  const toggle = (s: TrailStep) => {
    const next = !isOpen(s);
    if (!expandedProp) setToggled((t) => ({ ...t, [s.id]: next }));
    onExpandedChange?.(list.filter((x) => (x.id === s.id ? next : isOpen(x))).map((x) => x.id));
  };

  const seen = useRef<Map<string, TrailStepState> | null>(null);
  const [announce, setAnnounce] = useState("");
  useEffect(() => {
    const before = seen.current;
    seen.current = new Map(list.map((s) => [s.id, s.state]));
    if (!before) return;
    const said: string[] = [];
    // Steps that start or stop running go back to their default: open while running, closed after
    const reset: string[] = [];
    for (const s of list) {
      const was = before.get(s.id);
      if (was === undefined || was === s.state) continue;
      if (s.state === "running" || was === "running") reset.push(s.id);
      if (s.state === "done") said.push(`${s.title}: ${labels.done.toLowerCase()}`);
      if (s.state === "waiting") said.push(`${s.title}: ${labels.waiting.toLowerCase()}`);
      if (s.state === "failed") said.push(`${s.title}: ${labels.failed.toLowerCase()}`);
      if (s.state === "declined") said.push(`${s.title}: ${labels.declined.toLowerCase()}`);
    }
    if (reset.some((id) => toggled[id] !== undefined)) setToggled((t) => Object.fromEntries(Object.entries(t).filter(([k]) => !reset.includes(k))));
    if (said.length) setAnnounce(said.join(". "));
  });

  const reached = (s?: TrailStep) => !!s && s.state !== "pending";
  const doneCount = list.filter((s) => s.state === "done").length;

  return (
    <>
      <style href="ui-agent-trail-clean" precedence="default">
        {css}
      </style>
      <div className={["ui-atrc", className].filter(Boolean).join(" ")}>
        {title ? (
          <div className="ui-atrc__head">
            <span className="ui-atrc__task" dir="auto" title={title}>
              {title}
            </span>
            {agent ? (
              <span className="ui-atrc__agent" title={agent.name}>
                {agent.logo ? <span className="ui-atrc__logo" aria-hidden="true">{agent.logo}</span> : null}
                <span className="ui-atrc__agent-name" dir="auto">
                  {agent.name}
                </span>
              </span>
            ) : null}
            <span className="ui-atrc__count" aria-label={`${doneCount} of ${list.length} steps done`}>
              {doneCount}/{list.length}
            </span>
          </div>
        ) : null}
        {list.length ? null : <p className="ui-atrc__empty">{labels.empty}</p>}
        <ol className="ui-atrc__list" aria-label={title || labels.title} hidden={!list.length}>
          {list.map((s, i) => {
            const prev = list[i - 1];
            const next = list[i + 1];
            // The line between two steps fills once the agent has walked it
            const linkIn = !prev ? undefined : prev.state === "done" && reached(s) ? "on" : "off";
            const linkOut = !next ? undefined : s.state === "done" && reached(next) ? "on" : "off";
            return (
              <Step
                key={s.id}
                step={s}
                subs={subsOf(s)}
                open={isOpen(s)}
                linkIn={linkIn}
                linkOut={linkOut}
                afterBranch={!!prev && isOpen(prev)}
                labels={labels}
                now={now}
                geo={geo}
                panelId={`${uid}-${i}`}
                onToggle={() => toggle(s)}
                onApprove={onApprove ? () => onApprove(s) : undefined}
                onDecline={onDecline ? () => onDecline(s) : undefined}
              />
            );
          })}
        </ol>
        <span className="ui-atrc__sr" role="status" aria-live="polite">
          {announce}
        </span>
      </div>
    </>
  );
}

function Step({
  step: s,
  subs,
  open,
  linkIn,
  linkOut,
  afterBranch,
  labels,
  now,
  geo,
  panelId,
  onToggle,
  onApprove,
  onDecline,
}: {
  step: TrailStep;
  subs: TrailSubstep[];
  open: boolean;
  linkIn?: "on" | "off";
  linkOut?: "on" | "off";
  afterBranch: boolean;
  labels: AgentTrailLabels;
  now: number;
  geo: { row: number; sub: number };
  panelId: string;
  onToggle: () => void;
  onApprove?: () => void;
  onDecline?: () => void;
}) {
  const time = formatDuration(s.duration);
  const gated = !!s.gate && (s.state === "pending" || s.state === "waiting");
  const status = s.status || labels.running;
  const elapsed = s.state === "running" && typeof s.startedAt === "number" && Number.isFinite(s.startedAt) ? formatDuration(Math.max(0, now - s.startedAt)) : "";
  const stateWord =
    s.state === "running" ? status : s.state === "waiting" ? labels.waiting : s.state === "failed" ? labels.failed : s.state === "declined" ? labels.declined : s.state === "done" ? labels.done : gated ? labels.approval : labels.pending;

  // The sub-step the agent is on – the light runs down to it
  const current = s.state === "running" ? subs.findIndex((x) => x.state === "running") : -1;
  // A long list shows five around where the agent is; the rest folds into "+N earlier" / "+N more"
  const [all, setAll] = useState(false);
  const long = subs.length > VISIBLE + 1;
  const anchor = current >= 0 ? current : subs.length - 1;
  const end = long && !all ? Math.max(Math.min(subs.length, anchor + 2), VISIBLE) : subs.length;
  const start = long && !all ? end - VISIBLE : 0;
  const shown = subs.slice(start, end);
  const later = subs.length - end;
  // The light counts the fold row on top as one row
  const target = current >= start && current < end ? current - start + (long ? 1 : 0) : -1;
  const name = s.title.trim() || labels.untitled;

  // A step that loses its sub-steps while focused turns from a button into a plain row – focus stays on it
  const rowRef = useRef<HTMLElement | null>(null);
  const focused = useRef(false);
  const expandable = subs.length > 0;
  useLayoutEffect(() => {
    if (focused.current && (document.activeElement === document.body || !document.activeElement)) rowRef.current?.focus();
  }, [expandable]);
  const focusProps = {
    onFocus: () => (focused.current = true),
    onBlur: () => (focused.current = false),
  };

  // "Needs you" with a decision: a small panel under the row asks, Approve or Decline answers
  const decides = s.state === "waiting" && !!(onApprove || onDecline) && !subs.length;
  const [asking, setAsking] = useState(false);
  const askRef = useRef<HTMLDivElement>(null);
  const needsRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!decides) setAsking(false);
  }, [decides]);
  useEffect(() => {
    if (asking) askRef.current?.querySelector<HTMLButtonElement>("[data-approve]")?.focus({ preventScroll: true });
  }, [asking]);
  const answer = (fn?: () => void) => {
    setAsking(false);
    fn?.();
  };

  const meta =
    s.state === "running" ? (
      <span className="ui-atrc__live">
        <span className="ui-atrc__status" dir="auto" title={status}>
          {status}
        </span>
        {elapsed ? (
          <span className="ui-atrc__clock" dir="auto">
            {elapsed}
          </span>
        ) : null}
      </span>
    ) : s.state === "waiting" && decides ? (
      <button
        ref={needsRef}
        type="button"
        className="ui-atrc__badge ui-atrc__needs"
        aria-expanded={asking}
        aria-controls={`${panelId}-ask`}
        onClick={() => setAsking((v) => !v)}
      >
        {labels.waiting}
        <svg viewBox="0 0 24 24" width="10" height="10" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>
    ) : s.state === "waiting" ? (
      <span className="ui-atrc__badge" dir="auto" title={labels.waiting}>
        {labels.waiting}
      </span>
    ) : gated ? (
      <span className="ui-atrc__badge" dir="auto" title={labels.approval}>
        {labels.approval}
      </span>
    ) : s.state === "failed" ? (
      <span className="ui-atrc__fail" dir="auto">
        {labels.failed}
      </span>
    ) : s.state === "declined" ? (
      <span className="ui-atrc__declined" dir="auto">
        {labels.declined}
      </span>
    ) : s.state === "done" && time ? (
      <span className="ui-atrc__time" dir="auto">
        {time}
      </span>
    ) : null;

  const inner = (
    <>
      <Marker state={s.state} gated={gated} />
      {s.tool ? (
        <span className="ui-atrc__tool ui-atrc__logo" title={s.tool.name} aria-hidden="true">
          {s.tool.logo}
        </span>
      ) : null}
      <span className="ui-atrc__title" dir="auto" title={name} data-untitled={s.title.trim() ? undefined : ""}>
        {name}
      </span>
      {subs.length ? <Chevron /> : null}
      <span className="ui-atrc__meta" key={`${s.state}-${gated}`}>
        {meta}
      </span>
    </>
  );
  const sr = `${name}${s.tool ? ` (${s.tool.name})` : ""} – ${stateWord}${s.state === "done" && time ? `, ${time}` : ""}`;

  return (
    <li
      className="ui-atrc__step"
      data-state={s.state}
      data-gate={gated || undefined}
      data-open={open || undefined}
      data-in={linkIn}
      data-out={linkOut}
      data-after-branch={afterBranch || undefined}
      aria-current={s.state === "running" ? "step" : undefined}
    >
      {subs.length ? (
        <button ref={(el) => void (rowRef.current = el)} type="button" className="ui-atrc__row" aria-expanded={open} aria-controls={panelId} aria-label={sr} onClick={onToggle} {...focusProps}>
          {inner}
        </button>
      ) : (
        <div ref={(el) => void (rowRef.current = el)} className="ui-atrc__row" role="group" aria-label={sr} tabIndex={-1} {...focusProps}>
          {inner}
        </div>
      )}
      {decides ? (
        <div
          ref={askRef}
          id={`${panelId}-ask`}
          className="ui-atrc__ask"
          data-open={asking || undefined}
          inert={!asking || undefined}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setAsking(false);
              needsRef.current?.focus();
            }
          }}
        >
          <div className="ui-atrc__ask-in">
            {s.ask ? (
              <p className="ui-atrc__ask-text" dir="auto">
                {s.ask}
              </p>
            ) : null}
            <div className="ui-atrc__ask-actions">
              {onDecline ? (
                <button type="button" className="ui-atrc__ask-btn" onClick={() => answer(onDecline)}>
                  {labels.decline}
                </button>
              ) : null}
              {onApprove ? (
                <button type="button" className="ui-atrc__ask-btn" data-approve="" onClick={() => answer(onApprove)}>
                  {labels.approve}
                </button>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}
      {subs.length ? (
        <>
          {/* The branch: off the marker, down past the sub-steps and back onto the trail */}
          <span className="ui-atrc__wire" aria-hidden="true">
            <svg className="ui-atrc__bend" viewBox="0 0 28 14" width="28" height="14">
              <path d={`M${X} 0 C${X} 8 ${XS} 6 ${XS} 14`} />
            </svg>
            <span className="ui-atrc__spine" />
            <svg className="ui-atrc__bend ui-atrc__bend--out" viewBox="0 0 28 14" width="28" height="14">
              <path d={`M${XS} 0 C${XS} 8 ${X} 6 ${X} 14`} />
            </svg>
          </span>
          {open && target >= 0 ? <Spark index={target} geo={geo} /> : null}
          <div className="ui-atrc__subs" id={panelId} data-open={open || undefined} inert={!open || undefined}>
            <ul className="ui-atrc__sublist">
              {long ? (
                <li className="ui-atrc__sub ui-atrc__sub--fold">
                  <button type="button" className="ui-atrc__fold" aria-expanded={all} onClick={() => setAll((v) => !v)}>
                    <span className="ui-atrc__dot" aria-hidden="true" />
                    {all ? labels.fewer : `+${start} ${labels.earlier}`}
                  </button>
                </li>
              ) : null}
              {shown.map((x, i) => {
                const k = start + i;
                const st = x.state ?? "done";
                const subName = String(x.title ?? "").replace(/\s+/g, " ").trim();
                return (
                  <li key={x.id ?? `${k}-${x.title}`} className="ui-atrc__sub" data-state={st} aria-current={st === "running" ? "step" : undefined}>
                    <span className="ui-atrc__dot" aria-hidden="true" />
                    <span className="ui-atrc__subtitle" dir="auto" title={subName || labels.untitled} data-untitled={subName ? undefined : ""}>
                      {subName || labels.untitled}
                    </span>
                    {x.detail ? (
                      <span className="ui-atrc__detail" dir="auto" title={x.detail}>
                        {shorten(String(x.detail))}
                      </span>
                    ) : null}
                  </li>
                );
              })}
              {long && later > 0 && !all ? (
                <li className="ui-atrc__sub ui-atrc__sub--fold">
                  <button type="button" className="ui-atrc__fold" aria-expanded={false} onClick={() => setAll(true)}>
                    <span className="ui-atrc__dot" aria-hidden="true" />
                    {`+${later} ${labels.more}`}
                  </button>
                </li>
              ) : null}
            </ul>
          </div>
        </>
      ) : null}
    </li>
  );
}

/**
 * The light that runs from the step's marker down the branch into the sub-step it is on.
 * One line that fades from full at the front to nothing at the back: thin slices along the same path, each a little
 * fainter than the one ahead, all moving together. Past the path's end they are simply not drawn, so the light drains
 * into the dot.
 */
function Spark({ index, geo }: { index: number; geo: { row: number; sub: number } }) {
  // From the marker's bottom edge (marker 16, centered in the row): the bend ends 14 lower, then straight down to the dot
  const end = geo.row / 2 - 8 + PAD + geo.sub / 2 + geo.sub * index;
  const d = `M${X} 0 C${X} 8 ${XS} 6 ${XS} 14 L${XS} ${end}`;
  const ref = useRef<SVGPathElement>(null);
  // The path's real length, so the whole light leaves it before the loop starts over
  const [len, setLen] = useState(() => Math.round(19 + (end - 14)));
  useLayoutEffect(() => {
    const l = ref.current?.getTotalLength?.();
    if (l && Number.isFinite(l)) setLen(Math.ceil(l));
  }, [d]);
  return (
    <svg
      className="ui-atrc__spark"
      viewBox={`0 0 28 ${end + 2}`}
      width="28"
      height={end + 2}
      aria-hidden="true"
      style={{ "--len": len, "--tail": SLICES * SLICE } as CSSProperties}
    >
      <path ref={ref} d={d} className="ui-atrc__probe" />
      {Array.from({ length: SLICES }, (_, i) => (
        <path
          key={i}
          d={d}
          className="ui-atrc__slice"
          style={{ "--k": (SLICES - i) * SLICE, opacity: (i + 1) / SLICES, strokeDasharray: `${SLICE} 9999` } as CSSProperties}
        />
      ))}
    </svg>
  );
}

function Marker({ state, gated }: { state: TrailStepState; gated: boolean }) {
  if (state === "done")
    return (
      <span className="ui-atrc__mark" data-kind="done" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <path d="M5 8.3L7 10.2L11 6" pathLength={1} />
        </svg>
      </span>
    );
  if (state === "declined")
    return (
      <span className="ui-atrc__mark" data-kind="declined" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <path d="M5.5 8h5" />
        </svg>
      </span>
    );
  if (state === "failed")
    return (
      <span className="ui-atrc__mark" data-kind="failed" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <path d="M6 6L10 10M10 6L6 10" />
        </svg>
      </span>
    );
  if (state === "running")
    return (
      <span className="ui-atrc__mark" data-kind="running" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <circle className="ui-atrc__track" cx="8" cy="8" r="6.25" />
          <circle className="ui-atrc__arc" cx="8" cy="8" r="6.25" pathLength={40} />
        </svg>
      </span>
    );
  if (gated && state === "waiting")
    return (
      <span className="ui-atrc__mark" data-kind="waiting" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <path d="M6.4 5.6v4.8M9.6 5.6v4.8" />
        </svg>
      </span>
    );
  if (gated)
    return (
      <span className="ui-atrc__mark" data-kind="gate" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <circle cx="8" cy="8" r="6.25" />
        </svg>
      </span>
    );
  return <span className="ui-atrc__mark" data-kind={state} aria-hidden="true" />;
}

const Chevron = () => (
  <svg className="ui-atrc__chev" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M18 9C18 9 13.58 15 12 15C10.42 15 6 9 6 9" />
  </svg>
);

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-atrc (e.g. --ui-atrc-accent: #ee3e9e for pink) */
.ui-atrc {
  --ui-atrc-card: var(--aui-surface, #ffffff);
  --ui-atrc-strong: var(--aui-ink, #000000);
  --ui-atrc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-atrc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-atrc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-atrc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-atrc-line: var(--aui-border-strong, rgba(0, 0, 0, 0.1));
  --ui-atrc-divider: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-atrc-hover: var(--aui-subtle, rgba(0, 0, 0, 0.035));
  --ui-atrc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-atrc-logo-edge: var(--aui-border, rgba(0, 0, 0, 0.08));
  --ui-atrc-logo-shadow: rgba(0, 0, 0, 0.08);
  --ui-atrc-accent: var(--aui-blue, #0190fe);
  --ui-atrc-wash: color-mix(in srgb, var(--aui-blue, #0190fe) 12%, transparent);
  --ui-atrc-orange: var(--aui-orange, #f28a2e);
  --ui-atrc-badge: var(--aui-warning, #b25e00);
  --ui-atrc-badge-wash: var(--aui-warning-soft, rgba(242, 138, 46, 0.12));
  --ui-atrc-red: var(--aui-red, #f04438);
  --ui-atrc-red-wash: color-mix(in srgb, var(--aui-red, #f04438) 12%, transparent);
  --ui-atrc-shadow: 0 0 0 0.5px var(--aui-border, rgba(0, 0, 0, 0.06)), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-atrc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  /* One loop of the light down the trail */
  --ui-atrc-loop: 2400ms;
  /* Row heights – the trail is drawn from them, so change them only together with ROW / SUB in the code */
  --ui-atrc-row: ${ROW}px;
  --ui-atrc-sub: ${SUB}px;
  display: block;
  box-sizing: border-box;
  width: 100%;
  max-width: 380px;
  padding: 4px 8px 10px;
  border-radius: calc(var(--aui-radius-lg, 10px) + 2px);
  background: var(--ui-atrc-card);
  box-shadow: var(--aui-shadow-card, var(--ui-atrc-shadow));
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-atrc-ink);
  text-align: start;
  -webkit-font-smoothing: antialiased;
}
.ui-atrc *, .ui-atrc *::before, .ui-atrc *::after { box-sizing: border-box; }
.ui-atrc :is(ol, ul) { margin: 0; padding: 0; list-style: none; }
.ui-atrc svg { display: block; flex: none; }
.ui-atrc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.ui-atrc__list { padding-top: 6px; }

/* ----- Header: the task and how many steps are done ----- */
.ui-atrc__head { display: flex; align-items: center; gap: 12px; height: 36px; margin: 0 -8px; padding: 0 16px; }
.ui-atrc__task { flex: 1; min-width: 0; overflow: hidden; font-weight: 500; color: var(--ui-atrc-strong); white-space: nowrap; text-overflow: ellipsis; }
/* Who works on it: the agent's logo and name, quiet, before the count */
.ui-atrc__agent { display: inline-flex; flex: 0 1 auto; align-items: center; gap: 6px; min-width: 0; height: 22px; padding: 0 8px 0 7px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-atrc-hover); font-size: 12px; line-height: 16px; color: var(--ui-atrc-text); }
.ui-atrc__agent-name { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
/* A logo: small, in its own colors, on a white disc with a fine edge and a soft shadow – single-color marks take the ink */
.ui-atrc__logo { display: grid; flex: none; place-items: center; width: 20px; height: 20px; border-radius: 50%; background: var(--ui-atrc-card); box-shadow: 0 0 0 0.5px var(--ui-atrc-logo-edge), 0 1px 2px var(--ui-atrc-logo-shadow); color: var(--ui-atrc-strong); }
.ui-atrc__logo > svg, .ui-atrc__logo > img { display: block; width: 11px; height: 11px; }
/* In the header chip the logo stands on its own – no disc, no edge, no shadow */
.ui-atrc__agent .ui-atrc__logo { width: 12px; height: 12px; border-radius: 0; background: none; box-shadow: none; }
.ui-atrc__agent .ui-atrc__logo > :is(svg, img) { width: 12px; height: 12px; }
/* The step's tool, between the marker and the title */
.ui-atrc__tool { margin-inline-end: 6px; transition: opacity 300ms ease; }
.ui-atrc__step[data-state="pending"] .ui-atrc__tool { opacity: 0.55; }
.ui-atrc__count { flex: none; font-size: 12px; line-height: 16px; color: var(--ui-atrc-muted); font-variant-numeric: tabular-nums; }

/* ----- One step: a 32px row, the sub-steps under it ----- */
.ui-atrc__step { position: relative; }
.ui-atrc__row {
  position: relative;
  display: flex;
  align-items: center;
  width: 100%;
  height: var(--ui-atrc-row);
  margin: 0;
  padding: 0 8px;
  border: 0;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  background: none;
  font: inherit;
  color: inherit;
  text-align: inherit;
}
button.ui-atrc__row { cursor: pointer; -webkit-tap-highlight-color: transparent; transition: background-color 140ms ease; }
button.ui-atrc__row:hover { background: var(--ui-atrc-hover); }
.ui-atrc__row:focus-visible { outline: 2px solid var(--ui-atrc-focus); outline-offset: -2px; }
div.ui-atrc__row:focus:not(:focus-visible) { outline: none; }

/* The trail between markers: half a line above the marker, half below – they meet between two rows */
.ui-atrc__row::before, .ui-atrc__row::after {
  content: "";
  position: absolute;
  inset-inline-start: calc(8px + ${X}px - 0.75px);
  width: 1.5px;
  height: calc((var(--ui-atrc-row) - 16px) / 2);
  background: var(--ui-atrc-line);
  transition: background-color 400ms ease, opacity 420ms var(--ui-atrc-ease);
}
.ui-atrc__row::before { top: 0; }
.ui-atrc__row::after { bottom: 0; }
.ui-atrc__step:not([data-in]) > .ui-atrc__row::before,
.ui-atrc__step:not([data-out]) > .ui-atrc__row::after { visibility: hidden; }
/* Opening a step: the straight line fades out while the branch fades in and grows with the sub-steps – in step, so the
   trail never breaks or jumps; closing runs the same way back */
.ui-atrc__step[data-open] > .ui-atrc__row::after,
.ui-atrc__step[data-after-branch] > .ui-atrc__row::before { opacity: 0; }
.ui-atrc__step[data-in="on"] > .ui-atrc__row::before,
.ui-atrc__step[data-out="on"] > .ui-atrc__row::after { background: var(--ui-atrc-accent); }

/* ----- Markers, 16px ----- */
.ui-atrc__mark { position: relative; display: grid; flex: none; place-items: center; width: 18px; height: 16px; margin-inline-end: 14px; }
.ui-atrc__mark::before { content: ""; position: absolute; inset-inline-start: ${X - 8}px; width: 16px; height: 16px; border-radius: 50%; transition: background-color 300ms ease, box-shadow 300ms ease; }
.ui-atrc__mark > svg { position: relative; }
.ui-atrc__mark :is(path, circle) { fill: none; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
/* Up next: an empty ring */
.ui-atrc__mark[data-kind="pending"]::before { background: var(--ui-atrc-card); box-shadow: inset 0 0 0 1.5px var(--ui-atrc-line); }
/* Done: a tinted disc, the check draws itself */
.ui-atrc__mark[data-kind="done"]::before { background: var(--ui-atrc-wash); animation: ui-atrc-pop 380ms var(--ui-atrc-ease) both; }
.ui-atrc__mark[data-kind="done"] path { stroke: var(--ui-atrc-accent); stroke-width: 1.7; stroke-dasharray: 1; animation: ui-atrc-draw 360ms 120ms var(--ui-atrc-ease) both; }
/* Running: a faint ring with an arc going round */
.ui-atrc__mark[data-kind="running"]::before { background: var(--ui-atrc-card); }
.ui-atrc__track { stroke: var(--ui-atrc-wash); }
.ui-atrc__arc { stroke: var(--ui-atrc-accent); stroke-dasharray: 12 28; transform-origin: 8px 8px; animation: ui-atrc-spin 900ms linear infinite; }
/* Needs approval before it runs: a dashed ring – a stop ahead */
.ui-atrc__mark[data-kind="gate"]::before { background: var(--ui-atrc-card); }
.ui-atrc__mark[data-kind="gate"] circle { stroke: var(--ui-atrc-orange); stroke-dasharray: 2.4 2.5; }
/* Waiting for you: a tinted orange disc with a pause – like done and failed, a disc with its sign */
.ui-atrc__mark[data-kind="waiting"]::before { background: var(--ui-atrc-badge-wash); animation: ui-atrc-pop 380ms var(--ui-atrc-ease) both; }
.ui-atrc__mark[data-kind="waiting"] path { stroke: var(--ui-atrc-badge); stroke-width: 1.7; }
/* Failed */
.ui-atrc__mark[data-kind="failed"]::before { background: var(--ui-atrc-red-wash); animation: ui-atrc-pop 380ms var(--ui-atrc-ease) both; }
.ui-atrc__mark[data-kind="failed"] path { stroke: var(--ui-atrc-red); stroke-width: 1.7; }
/* Declined by you: a quiet gray disc with a dash – a choice, not an error */
.ui-atrc__mark[data-kind="declined"]::before { background: var(--ui-atrc-hover); box-shadow: inset 0 0 0 1px var(--ui-atrc-divider); animation: ui-atrc-pop 380ms var(--ui-atrc-ease) both; }
.ui-atrc__mark[data-kind="declined"] path { stroke: var(--ui-atrc-muted); stroke-width: 1.7; }

/* ----- Title and what's on the right ----- */
.ui-atrc__title { flex: 0 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; color: var(--ui-atrc-text); transition: color 300ms ease; }
.ui-atrc__step:is([data-state="running"], [data-state="failed"], [data-state="waiting"]) .ui-atrc__title { color: var(--ui-atrc-ink); }
.ui-atrc__step[data-state="pending"] .ui-atrc__title { color: var(--ui-atrc-muted); }
.ui-atrc__title[data-untitled], .ui-atrc__subtitle[data-untitled] { color: var(--ui-atrc-faint) !important; font-weight: 400 !important; }
.ui-atrc__empty { margin: 0; padding: 8px 8px 4px; font-size: 12px; line-height: 16px; color: var(--ui-atrc-muted); }
.ui-atrc__list[hidden] { display: none; }
.ui-atrc__chev { flex: none; margin-inline-start: 4px; color: var(--ui-atrc-faint); opacity: 0; rotate: -90deg; transition: rotate 200ms var(--ui-atrc-ease), opacity 140ms ease; }
.ui-atrc:dir(rtl) .ui-atrc__chev { rotate: 90deg; }
.ui-atrc__step[data-open] .ui-atrc__chev { rotate: 0deg; }
.ui-atrc__row:is(:hover, :focus-visible) .ui-atrc__chev { opacity: 1; }
.ui-atrc__meta { display: flex; flex: none; justify-content: flex-end; max-width: 40%; min-width: 0; margin-inline-start: auto; padding-inline-start: 12px; animation: ui-atrc-in 260ms var(--ui-atrc-ease) both; }
.ui-atrc__time { font-size: 12px; line-height: 16px; color: var(--ui-atrc-faint); font-variant-numeric: tabular-nums; white-space: nowrap; }
.ui-atrc__declined { font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-atrc-muted); white-space: nowrap; }
.ui-atrc__step[data-state="declined"] .ui-atrc__title { color: var(--ui-atrc-muted); text-decoration: line-through; text-decoration-color: var(--ui-atrc-faint); }
.ui-atrc__fail { font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-atrc-red); white-space: nowrap; }
/* Running: what it does, shimmering, and how long it has been at it */
.ui-atrc__live { display: flex; align-items: center; gap: 6px; min-width: 0; }
.ui-atrc__status {
  min-width: 0;
  overflow: hidden;
  font-size: 12px;
  font-weight: 500;
  line-height: 16px;
  white-space: nowrap;
  text-overflow: ellipsis;
  background: linear-gradient(90deg, var(--ui-atrc-accent) 0 40%, color-mix(in srgb, var(--ui-atrc-accent) 35%, var(--ui-atrc-card)) 50%, var(--ui-atrc-accent) 60% 100%) 0 0 / 250% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  animation: ui-atrc-shine 1300ms linear infinite;
}
.ui-atrc__clock { flex: none; min-width: 3ch; font-size: 12px; line-height: 16px; color: var(--ui-atrc-faint); font-variant-numeric: tabular-nums; text-align: end; }
/* "Needs you" you can answer: the badge opens a small panel under the row */
.ui-atrc__needs { display: inline-flex; align-items: center; gap: 4px; margin: 0; border: 0; font-family: inherit; cursor: pointer; transition: background-color 140ms ease, box-shadow 140ms ease; }
.ui-atrc__needs:hover, .ui-atrc__needs[aria-expanded="true"] { box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--ui-atrc-badge) 30%, transparent); }
.ui-atrc__needs > svg { flex: none; transition: rotate 200ms var(--ui-atrc-ease); }
.ui-atrc__needs[aria-expanded="true"] > svg { rotate: 180deg; }
.ui-atrc__needs:focus-visible { outline: 2px solid var(--ui-atrc-focus); outline-offset: 2px; }
.ui-atrc__ask { position: relative; display: grid; grid-template-rows: 0fr; transition: grid-template-rows 360ms var(--ui-atrc-ease); }
.ui-atrc__ask[data-open] { grid-template-rows: 1fr; }
/* The trail runs on through the panel when a step follows */
.ui-atrc__step[data-out] > .ui-atrc__ask::before { content: ""; position: absolute; top: 0; bottom: 0; inset-inline-start: calc(8px + ${X}px - 0.75px); width: 1.5px; background: var(--ui-atrc-line); }
.ui-atrc__ask-in { min-height: 0; overflow: hidden; padding-inline: 40px 8px; opacity: 0; translate: 0 -4px; transition: opacity 220ms ease, translate 320ms var(--ui-atrc-ease); }
.ui-atrc__ask[data-open] .ui-atrc__ask-in { opacity: 1; translate: 0 0; }
.ui-atrc__ask-text { margin: 2px 0 0; font-size: 12px; line-height: 17px; color: var(--ui-atrc-text); overflow-wrap: anywhere; }
.ui-atrc__ask-actions { display: flex; justify-content: flex-end; gap: 6px; padding: 8px 0 8px; }
.ui-atrc__ask-btn { height: 26px; margin: 0; padding: 0 10px; border: 0; border-radius: var(--aui-radius, 7px); background: var(--ui-atrc-card); box-shadow: 0 0 0 0.5px var(--ui-atrc-divider), 0 1px 2px rgba(0, 0, 0, 0.06); font: inherit; font-size: 12px; font-weight: 500; color: var(--ui-atrc-ink); cursor: pointer; transition: box-shadow 140ms ease, opacity 140ms ease; }
.ui-atrc__ask-btn:hover { box-shadow: 0 0 0 0.5px var(--ui-atrc-line), 0 1px 3px rgba(0, 0, 0, 0.1); }
.ui-atrc__ask-btn[data-approve] { background: var(--aui-primary, var(--ui-atrc-ink)); box-shadow: none; color: var(--aui-on-primary, var(--ui-atrc-card)); }
.ui-atrc__ask-btn[data-approve]:hover { opacity: 0.86; }
.ui-atrc__ask-btn:focus-visible { outline: 2px solid var(--ui-atrc-focus); outline-offset: 2px; }
.ui-atrc__badge { overflow: hidden; height: 20px; padding: 0 7px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-atrc-badge-wash); font-size: 12px; font-weight: 500; line-height: 20px; color: var(--ui-atrc-badge); white-space: nowrap; text-overflow: ellipsis; }

/* ----- The branch: sub-steps under a step, on their own line a little to the side ----- */
.ui-atrc__subs { display: grid; grid-template-rows: 0fr; transition: grid-template-rows 420ms var(--ui-atrc-ease); }
.ui-atrc__subs[data-open] { grid-template-rows: 1fr; }
.ui-atrc__sublist { min-height: 0; overflow: hidden; }
.ui-atrc__sublist::before, .ui-atrc__sublist::after { content: ""; display: block; height: ${PAD}px; }
.ui-atrc__sub { display: flex; align-items: center; gap: 12px; height: var(--ui-atrc-sub); padding-inline: calc(8px + ${XS}px - 2.5px) 8px; opacity: 0; translate: 0 -4px; transition: opacity 260ms ease, translate 360ms var(--ui-atrc-ease); }
.ui-atrc__subs[data-open] .ui-atrc__sub { opacity: 1; translate: 0 0; }
.ui-atrc__dot { position: relative; flex: none; width: 5px; height: 5px; margin-inline-end: ${32 - XS - 2.5 - 12 + 6}px; border-radius: 50%; background: linear-gradient(var(--ui-atrc-faint), var(--ui-atrc-faint)), var(--ui-atrc-card); transition: background-color 300ms ease; }
.ui-atrc__sub[data-state="pending"] .ui-atrc__dot { background: var(--ui-atrc-card); box-shadow: inset 0 0 0 1px var(--ui-atrc-faint); }
.ui-atrc__sub[data-state="running"] .ui-atrc__dot { background: var(--ui-atrc-accent); }
/* Once the whole step is done, its sub-steps turn blue too, when you open it again; while it runs they stay gray */
.ui-atrc__step[data-state="done"] .ui-atrc__sub[data-state="done"] .ui-atrc__dot { background: var(--ui-atrc-accent); }
/* When the light arrives, the dot rings once */
.ui-atrc__sub[data-state="running"] .ui-atrc__dot::after { content: ""; position: absolute; inset: -3px; border-radius: 50%; box-shadow: 0 0 0 1.5px var(--ui-atrc-accent); opacity: 0; animation: ui-atrc-ring var(--ui-atrc-loop) ease-out infinite; }
.ui-atrc__subtitle { flex: 0 1 auto; min-width: 0; overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-atrc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-atrc__sub[data-state="running"] .ui-atrc__subtitle { color: var(--ui-atrc-text); }
.ui-atrc__sub[data-state="pending"] .ui-atrc__subtitle { color: var(--ui-atrc-faint); }
.ui-atrc__detail { flex: none; max-width: 45%; margin-inline-start: auto; overflow: hidden; font-size: 11px; line-height: 16px; color: var(--ui-atrc-faint); font-variant-numeric: tabular-nums; white-space: nowrap; text-overflow: ellipsis; }

/* "+35 earlier" / "+3 more": a quiet row on the branch that opens the rest */
.ui-atrc__sub--fold { padding-inline-end: 0; }
.ui-atrc__fold { display: flex; flex: 1; align-items: center; gap: 12px; min-width: 0; height: 100%; margin: 0; margin-inline-start: -6px; padding: 0; padding-inline: 6px 8px; border: 0; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: none; font: inherit; font-size: 12px; line-height: 16px; color: var(--ui-atrc-muted); text-align: start; cursor: pointer; -webkit-tap-highlight-color: transparent; transition: background-color 140ms ease, color 140ms ease; }
.ui-atrc__fold:hover { background: var(--ui-atrc-hover); color: var(--ui-atrc-text); }
.ui-atrc__fold:focus-visible { outline: 2px solid var(--ui-atrc-focus); outline-offset: -2px; }
.ui-atrc__fold .ui-atrc__dot { background: var(--ui-atrc-card); box-shadow: inset 0 0 0 1px var(--ui-atrc-faint); }

/* The branch line: a bend out of the marker, straight down, a bend back onto the trail */
.ui-atrc__wire { position: absolute; top: calc(var(--ui-atrc-row) / 2 + 8px); bottom: calc((16px - var(--ui-atrc-row)) / 2); inset-inline-start: 8px; width: 28px; pointer-events: none; opacity: 0; transition: opacity 420ms var(--ui-atrc-ease); }
.ui-atrc__step[data-open] > .ui-atrc__wire { opacity: 1; }
.ui-atrc__step:not([data-out]) > .ui-atrc__wire > .ui-atrc__bend--out { display: none; }
.ui-atrc__bend { position: absolute; top: 0; left: 0; }
.ui-atrc__bend--out { top: auto; bottom: 0; }
.ui-atrc__bend path { fill: none; stroke: var(--ui-atrc-line); stroke-width: 1.5; }
/* A finished step opened by hand: its branch is part of the walked trail */
.ui-atrc__step[data-out="on"] > .ui-atrc__wire .ui-atrc__bend path { stroke: var(--ui-atrc-accent); }
.ui-atrc__step[data-out="on"] > .ui-atrc__wire .ui-atrc__spine { background: var(--ui-atrc-accent); }
.ui-atrc__spine { position: absolute; top: 14px; bottom: 14px; left: calc(${XS}px - 0.75px); width: 1.5px; background: var(--ui-atrc-line); }
.ui-atrc__step:not([data-out]) > .ui-atrc__wire > .ui-atrc__spine { bottom: calc((var(--ui-atrc-row) - 16px) / 2 + ${PAD}px + var(--ui-atrc-sub) / 2); }
.ui-atrc:dir(rtl) :is(.ui-atrc__wire, .ui-atrc__spark) { scale: -1 1; }

/* The light: one line, full at the front and fading to nothing at the back */
.ui-atrc__spark { position: absolute; top: calc(var(--ui-atrc-row) / 2 + 8px); inset-inline-start: 8px; overflow: visible; pointer-events: none; }
.ui-atrc__probe { fill: none; stroke: none; }
.ui-atrc__slice { fill: none; stroke: var(--ui-atrc-accent); stroke-width: 1.5; stroke-linecap: butt; animation: ui-atrc-run var(--ui-atrc-loop) linear infinite; }

/* An even glide down – most of the loop – then a short rest before the next one */
@keyframes ui-atrc-run {
  0% { stroke-dashoffset: calc(var(--k) * 1px); animation-timing-function: cubic-bezier(0.45, 0, 0.3, 1); }
  62%, 100% { stroke-dashoffset: calc((var(--k) - var(--len) - var(--tail)) * 1px); }
}
@keyframes ui-atrc-ring {
  0%, 50% { opacity: 0; transform: scale(0.5); }
  56% { opacity: 0.6; }
  85%, 100% { opacity: 0; transform: scale(1.4); }
}
@keyframes ui-atrc-spin { to { transform: rotate(360deg); } }
@keyframes ui-atrc-pop { from { transform: scale(0.5); } }
@keyframes ui-atrc-draw { from { stroke-dashoffset: 1; } to { stroke-dashoffset: 0; } }
@keyframes ui-atrc-in { from { opacity: 0; transform: translateY(3px); } }
@keyframes ui-atrc-shine { from { background-position: 100% 0; } to { background-position: 0 0; } }

/* Touch screens: taller rows to tap, and the chevron shows without hover */
@media (hover: none) and (pointer: coarse) {
  .ui-atrc { --ui-atrc-row: ${ROW_TOUCH}px; --ui-atrc-sub: ${SUB_TOUCH}px; }
  .ui-atrc__chev { opacity: 1; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-atrc *, .ui-atrc *::before, .ui-atrc *::after { animation: none !important; transition: none !important; }
  .ui-atrc__spark { display: none; }
  .ui-atrc__status { color: var(--ui-atrc-accent); background: none; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-atrc {
  --ui-atrc-card: var(--aui-surface, #1c1c1e);
  --ui-atrc-strong: var(--aui-ink, #ffffff);
  --ui-atrc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-atrc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-atrc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-atrc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-atrc-line: var(--aui-border-strong, rgba(255, 255, 255, 0.12));
  --ui-atrc-divider: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-atrc-hover: var(--aui-subtle, rgba(255, 255, 255, 0.05));
  --ui-atrc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-atrc-logo-edge: var(--aui-border, rgba(255, 255, 255, 0.12));
  --ui-atrc-logo-shadow: rgba(0, 0, 0, 0.4);
  --ui-atrc-accent: var(--aui-blue, #4fb0fe);
  --ui-atrc-wash: color-mix(in srgb, var(--aui-blue, #4fb0fe) 18%, transparent);
  --ui-atrc-orange: var(--aui-orange, #f2a15c);
  --ui-atrc-badge: var(--aui-warning, #ffb35c);
  --ui-atrc-badge-wash: var(--aui-warning-soft, rgba(242, 161, 92, 0.14));
  --ui-atrc-red: var(--aui-red, #ff6b5f);
  --ui-atrc-red-wash: color-mix(in srgb, var(--aui-red, #ff6b5f) 16%, transparent);
  --ui-atrc-shadow: 0 0 0 0.5px var(--aui-border, rgba(255, 255, 255, 0.1)), 0 1px 2px rgba(0, 0, 0, 0.4);
}
`;
