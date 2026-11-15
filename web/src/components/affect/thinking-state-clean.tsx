/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useRef, useState, type ReactNode } from "react";

/* Thinking State (Clean), adapted to Steve's event stream. Only authoritative incoming steps and
 * outcomes drive this panel. There is no demo playback timer and no inferred completion. */
export type ThinkingSource = { label: string; icon?: ReactNode; href?: string; tooltip?: string };
export type ThinkingStatus = "running" | "done" | "error" | "stopped";
export type ThinkingStep = {
  id: string;
  kind: "search" | "step";
  label: ReactNode;
  activeLabel?: ReactNode;
  status: ThinkingStatus;
  icon?: ReactNode;
  sources?: ThinkingSource[];
  body?: ReactNode;
  attrs?: Record<string, string | undefined>;
};
export type ThinkingStateProps = {
  steps: ThinkingStep[];
  status: ThinkingStatus;
  label: string;
  endLabel: string;
  summary?: string;
  startedAt?: number;
  durationMs?: number;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  headerAttrs?: Record<string, string>;
  formatTime: (seconds: number) => string;
  className?: string;
};

export function ThinkingState({ steps, status, label, endLabel, summary, startedAt, durationMs,
  open, defaultOpen, onOpenChange, headerAttrs, formatTime, className }: ThinkingStateProps) {
  const uid = useId(), observedAt = useRef(Date.now());
  const [ownOpen, setOwnOpen] = useState(defaultOpen ?? status === "running");
  const touched = useRef(false), previous = useRef(status);
  const opened = open ?? ownOpen, running = status === "running";
  const setOpen = (value: boolean) => { setOwnOpen(value); onOpenChange?.(value) };
  useEffect(() => {
    if (previous.current === "running" && !running && !touched.current) setOpen(false);
    previous.current = status;
  }, [status]);
  const elapsed = durationMs != null && Number.isFinite(durationMs) ? formatTime(Math.max(0, durationMs) / 1000) : null;
  return <div className={["ui-thinkc", className].filter(Boolean).join(" ")} data-status={status} data-done={!running || undefined} aria-busy={running}>
    <style href="ui-thinking-state-clean" precedence="default">{css}</style>
    <button type="button" className="ui-thinkc__head" aria-expanded={opened} aria-controls={`${uid}-body`}
      onClick={() => { touched.current = true; setOpen(!opened) }} {...headerAttrs}>
      <span className="ui-thinkc__mark" aria-hidden="true">{Array.from({length:9},(_,i)=><i key={i} style={{animationDelay:`${PIXEL_DELAYS[i]}ms`}} />)}</span>
      <span className="ui-thinkc__title" key={status}>
        <span aria-live="polite" className={running ? "ui-thinkc__shimmer" : undefined} data-text={label}>{label}</span>
        {(running || elapsed) && <span className="ui-thinkc__time" aria-hidden="true">{running ? <Elapsed since={startedAt ?? observedAt.current} formatTime={formatTime} /> : elapsed}</span>}
      </span>
      <span className="ui-thinkc__chevron" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="M4.5 6.25 8 9.75l3.5-3.5" /></svg></span>
    </button>
    {summary && <p className="ui-thinkc__summary">{summary}</p>}
    <div id={`${uid}-body`} className="ui-thinkc__body" data-open={opened} aria-hidden={!opened} inert={!opened}>
      <div className="ui-thinkc__clip"><ol className="ui-thinkc__list">
        {steps.map((step) => {
          const active = running && step.status === "running", text = active ? step.activeLabel ?? step.label : step.label;
          return <li key={step.id} className="ui-thinkc__item" data-kind={step.kind} data-active={active || undefined} data-step-status={step.status} {...step.attrs}>
            <span className="ui-thinkc__node" aria-hidden="true">{step.status === "error" ? <svg viewBox="0 0 16 16" className="ui-thinkc__outcome"><path d="m4 4 8 8m0-8-8 8" /></svg> : step.icon ?? <span className="ui-thinkc__dot" />}</span>
            <div className="ui-thinkc__content">
              <span className={active ? "ui-thinkc__label ui-thinkc__shimmer" : "ui-thinkc__label"} data-text={typeof text === "string" ? text : undefined} dir="auto">{text}</span>
              {!!step.sources?.length && <div className="ui-thinkc__sources">{step.sources.map((source,j)=><Source key={source.label+j} source={source} delay={Math.min(j,12)*70} />)}</div>}
              {step.body && <div className="ui-thinkc__detail">{step.body}</div>}
            </div>
          </li>
        })}
        {!running && <li className="ui-thinkc__item" data-kind="done" data-outcome={status}>
          <span className="ui-thinkc__node" aria-hidden="true"><svg viewBox="0 0 16 16" className={status === "done" ? "ui-thinkc__check" : "ui-thinkc__outcome"}>
            <circle cx="8" cy="8" r="6.5" />{status === "done" ? <path d="M5.4 8.3l1.75 1.75L10.7 6.4" /> : status === "error" ? <path d="m5.5 5.5 5 5m0-5-5 5" /> : <path d="m4 12 8-8" />}
          </svg></span><div className="ui-thinkc__content"><span className="ui-thinkc__label">{endLabel}</span></div>
        </li>}
      </ol></div>
    </div>
  </div>;
}

/** A source chip: a link when it has one, with a tooltip on hover and focus */
function Source({ source, delay }: { source: ThinkingSource; delay: number }) {
  const tip = source.tooltip ?? source.href?.replace(/^https?:\/\//, "");
  const inner = (
    <>
      {source.icon && (
        // A logo that fails to load hides its tile instead of showing a broken image
        <span className="ui-thinkc__favicon" onErrorCapture={(e) => (e.currentTarget.style.display = "none")}>
          {source.icon}
        </span>
      )}
      <span className="ui-thinkc__source-label" dir="auto" title={tip ? undefined : source.label}>
        {source.label}
      </span>

    </>
  );
  const props = {
    className: "ui-thinkc__source",
    title: tip,
    "data-count": !source.icon || undefined,
    style: { animationDelay: `${delay}ms` },
  };
  return source.href ? (
    <a {...props} href={source.href} target="_blank" rel="noreferrer">
      {inner}
    </a>
  ) : (
    <span {...props}>{inner}</span>
  );
}

/** Twinkle order of the 3×3 pixel mark – a diagonal wave */
const PIXEL_DELAYS = [0, 120, 240, 120, 240, 360, 240, 360, 480];

/** Only the clock ticks; no elapsed time can change a run's outcome or step count. */
function Elapsed({ since, formatTime }: { since: number; formatTime: (seconds: number) => string }) {
  const [, tick] = useState(0);
  useEffect(() => { const t=window.setInterval(()=>tick(n=>n+1),1000); return ()=>clearInterval(t) }, []);
  return <>{formatTime(Math.max(0,Date.now()-since)/1000)}</>;
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-thinkc (see Theming in the docs) */
.ui-thinkc {
  --ui-tkc-card: #ffffff;
  --ui-tkc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-tkc-strong: var(--aui-ink, #000000);
  --ui-tkc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-tkc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-tkc-faint: rgba(0, 0, 0, 0.27);
  --ui-tkc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-tkc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-tkc-rail: var(--aui-border, rgba(0, 0, 0, 0.08));
  --ui-tkc-fill: rgba(0, 0, 0, 0.06);
  --ui-tkc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-tkc-accent: var(--aui-blue, rgba(0, 96, 226, 0.765));
  --ui-tkc-shine: rgba(255, 255, 255, 0.82);
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-tkc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-tkc-shadow-pop: 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 12px 28px -10px rgba(0, 0, 0, 0.16), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-tkc-ring: 0 0 0 0.5px rgba(0, 0, 0, 0.1);
  --ui-tkc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-tkc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));

  position: relative;
  box-sizing: border-box;
  container-type: inline-size;
  width: 100%;
  max-width: 440px;
  /* No card: it sits straight on the page */
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-tkc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-thinkc *,
.ui-thinkc *::before,
.ui-thinkc *::after {
  box-sizing: border-box;
}
:where(.ui-thinkc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-thinkc) svg { display: block; flex: none; }
.ui-thinkc button:focus-visible,
.ui-thinkc a:focus-visible {
  outline: 2px solid var(--ui-tkc-focus);
  outline-offset: 2px;
}

/* Header row: pixel mark + "Thinking 4s" / "Worked for 18s" + chevron – only as wide as its words, so only they take the hover */
.ui-thinkc__head {
  display: flex;
  width: fit-content;
  align-items: center;
  gap: 10px;
  max-width: 100%;
  height: 36px;
  padding: 0 8px;
  border-radius: calc(var(--aui-radius, 7px) + 1px);
  text-align: left;
}
.ui-thinkc__mark {
  display: grid;
  grid-template-columns: repeat(3, 3px);
  grid-auto-rows: 3px;
  gap: 1.5px;
  place-content: center;
  width: 16px;
  height: 16px;
}
.ui-thinkc__mark i {
  border-radius: 0.75px;
  background: var(--ui-tkc-accent);
  animation: ui-thinkc-pixel 1.2s ease-in-out infinite both;
}
.ui-thinkc[data-done] .ui-thinkc__mark i {
  animation: ui-thinkc-settle 420ms var(--ui-tkc-ease) both;
}
.ui-thinkc__title {
  display: inline-flex;
  min-width: 0;
  gap: 8px;
  font-weight: 500;
  white-space: nowrap;
  color: var(--ui-tkc-ink);
  transition: color 160ms ease;
  animation: ui-thinkc-in 420ms var(--ui-tkc-ease) both;
}
/* Hover: no fill – the words just lighten */
.ui-thinkc__head:hover .ui-thinkc__title { color: var(--ui-tkc-muted); }
.ui-thinkc__title .ui-thinkc__shimmer {
  color: var(--ui-tkc-muted);
}
.ui-thinkc__time {
  font-weight: 400;
  font-variant-numeric: tabular-nums;
  color: var(--aui-placeholder, var(--ui-tkc-faint));
}
/* The chevron sits right after the words and shows on hover (or keyboard focus) */
.ui-thinkc__chevron {
  display: grid;
  place-items: center;
  width: 16px;
  height: 16px;
  margin-left: -4px;
  color: var(--ui-tkc-icon);
  opacity: 0;
  transition: opacity 160ms ease;
}
.ui-thinkc__head:is(:hover, :focus-visible) .ui-thinkc__chevron { opacity: 0.6; }
.ui-thinkc__chevron svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.1;
  stroke-linecap: round;
  stroke-linejoin: round;
  transform: rotate(-90deg);
  transition: transform 260ms var(--ui-tkc-ease);
}
.ui-thinkc__head[aria-expanded="true"] .ui-thinkc__chevron svg {
  transform: none;
}


/* Body: folds away under a hairline – only this part animates and clips */
.ui-thinkc__body {
  display: grid;
  grid-template-rows: 1fr;
  transition: grid-template-rows 360ms var(--ui-tkc-ease), opacity 260ms ease;
}
.ui-thinkc__body[data-open="false"] {
  grid-template-rows: 0fr;
  opacity: 0;
}
.ui-thinkc__clip {
  min-height: 0;
  /* clips only while folded, so tooltips can reach out when open */
  overflow: visible;
}
.ui-thinkc__body[data-open="false"] .ui-thinkc__clip {
  overflow: hidden;
}
.ui-thinkc__list {
  margin: 0;
  padding: 8px 8px 0;
  list-style: none;
}

/* Timeline: icon column with a thin rail growing down to the next node */
.ui-thinkc__item {
  position: relative;
  display: grid;
  grid-template-columns: 16px minmax(0, 1fr);
  gap: 10px;
  padding-bottom: 12px;
  animation: ui-thinkc-in 520ms var(--ui-tkc-ease) both;
}
.ui-thinkc__item:not(:last-child)::before {
  content: "";
  position: absolute;
  left: 7.5px;
  top: 22px;
  bottom: 2px;
  width: 1px;
  background: var(--ui-tkc-rail);
  transform-origin: top;
  animation: ui-thinkc-line 520ms var(--ui-tkc-ease) both;
}
.ui-thinkc__node {
  display: grid;
  place-items: center;
  width: 16px;
  height: 20px;
}
.ui-thinkc__globe {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: var(--ui-tkc-icon);
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
/* A finished step is a small gray dot, the running one a blue pixel that pulses */
.ui-thinkc__dot {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--ui-tkc-faint);
  transition: background-color 300ms ease, border-radius 300ms ease;
}
.ui-thinkc__item[data-active] .ui-thinkc__dot {
  width: 6px;
  height: 6px;
  border-radius: 1.5px;
  background: var(--ui-tkc-accent);
  animation: ui-thinkc-pulse 1s ease-in-out infinite;
}
.ui-thinkc__check {
  width: 16px;
  height: 16px;
  animation: ui-thinkc-pop 460ms cubic-bezier(0.34, 1.56, 0.64, 1) both;
}
.ui-thinkc__check circle {
  fill: var(--ui-tkc-accent);
}
.ui-thinkc__check path {
  fill: none;
  stroke: #fff;
  stroke-width: 1.4;
  stroke-linecap: round;
  stroke-linejoin: round;
  stroke-dasharray: 9;
  stroke-dashoffset: 9;
  animation: ui-thinkc-draw 360ms var(--ui-tkc-ease) 180ms forwards;
}

.ui-thinkc__content {
  min-width: 0;
}
.ui-thinkc__label {
  position: relative;
  display: inline-block;
  max-width: 100%;
  overflow-wrap: anywhere;
  white-space: pre-line;
  text-align: start;
  color: var(--ui-tkc-text);
  transition: color 300ms ease;
}
/* The running step: gray with a darker band sweeping through */
.ui-thinkc__item[data-active] .ui-thinkc__label {
  color: var(--ui-tkc-muted);
}
.ui-thinkc__item[data-kind="done"] .ui-thinkc__label {
  font-weight: 500;
  color: var(--ui-tkc-ink);
}

/* Sources: small white chips that pop in one after another */
.ui-thinkc__sources {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
  padding: 1px 0 2px;
}
.ui-thinkc__source {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  height: 26px;
  padding: 0 8px 0 6px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  background: var(--aui-surface, var(--ui-tkc-card));
  box-shadow: var(--ui-tkc-shadow);
  font-size: 12px;
  line-height: 16px;
  color: var(--ui-tkc-ink);
  text-decoration: none;
  animation: ui-thinkc-chip 420ms var(--ui-tkc-ease) both;
}
a.ui-thinkc__source,
.ui-thinkc__more {
  position: relative;
  cursor: pointer;
  transition: background-color 160ms ease, color 160ms ease;
}
a.ui-thinkc__source:hover {
  background: var(--ui-tkc-wash);
}
/* "+3 more" – the quiet chip with an inner hairline */
.ui-thinkc .ui-thinkc__more {
  padding: 0 8px;
  background: none;
  box-shadow: inset 0 0 0 0.5px var(--ui-tkc-line);
  font-size: 12px;
  font-weight: 500;
  color: var(--ui-tkc-text);
}
.ui-thinkc .ui-thinkc__more:hover {
  background: var(--ui-tkc-wash);
  color: var(--ui-tkc-ink);
}
.ui-thinkc .ui-thinkc__more[aria-expanded="true"] {
  background: var(--ui-tkc-fill);
  color: var(--ui-tkc-ink);
}

/* Tooltip above the chip – a small white popover */
.ui-thinkc__tip {
  position: absolute;
  left: 50%;
  bottom: calc(100% + 6px);
  z-index: 2;
  padding: 4px 8px;
  border-radius: 6px;
  background: var(--ui-tkc-card);
  box-shadow: var(--ui-tkc-shadow-pop);
  color: var(--ui-tkc-text);
  font-size: 12px;
  line-height: 16px;
  width: max-content;
  max-width: min(320px, 80vw);
  overflow-wrap: anywhere;
  pointer-events: none;
  opacity: 0;
  transform: translate(-50%, 4px);
  transition: opacity 140ms ease, transform 200ms var(--ui-tkc-ease);
}
.ui-thinkc__source:hover .ui-thinkc__tip,
.ui-thinkc__source:focus-visible .ui-thinkc__tip {
  opacity: 1;
  transform: translate(-50%, 0);
  transition-delay: 250ms;
}

.ui-thinkc__source[data-count] {
  padding: 0 8px;
  background: none;
  box-shadow: inset 0 0 0 0.5px var(--ui-tkc-line);
  color: var(--ui-tkc-text);
}
.ui-thinkc__source-label {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.ui-thinkc__favicon img {
  width: 100%;
  height: 100%;
  object-fit: contain;
}
.ui-thinkc__favicon {
  display: grid;
  place-items: center;
  flex: none;
  width: 14px;
  height: 14px;
  border-radius: 4px;
  overflow: hidden;
  box-shadow: var(--ui-tkc-ring);
}
.ui-thinkc__favicon svg {
  width: 100%;
  height: 100%;
}

/* Shimmer over the working label – a darker band runs through the gray text */
.ui-thinkc__shimmer {
  position: relative;
}
.ui-thinkc__shimmer::after {
  content: attr(data-text);
  content: attr(data-text) / "";
  position: absolute;
  inset: 0;
  /* wraps exactly like the text below it, so a long label keeps its sweep inside the card */
  white-space: inherit;
  overflow-wrap: inherit;
  color: transparent;
  background: linear-gradient(90deg, transparent 38%, var(--ui-tkc-shine) 50%, transparent 62%) 100% 0 / 300% 100%
    no-repeat;
  -webkit-background-clip: text;
  background-clip: text;
  animation: ui-thinkc-sweep 1.6s linear infinite;
  pointer-events: none;
}

@keyframes ui-thinkc-in {
  from { opacity: 0; transform: translateY(6px); filter: blur(3px); }
  to { opacity: 1; transform: none; filter: none; }
}
@keyframes ui-thinkc-line {
  from { transform: scaleY(0); }
  to { transform: scaleY(1); }
}
@keyframes ui-thinkc-chip {
  from { opacity: 0; transform: translateY(4px) scale(0.92); }
  to { opacity: 1; transform: none; }
}
@keyframes ui-thinkc-pulse {
  50% { transform: scale(0.7); opacity: 0.45; }
}
@keyframes ui-thinkc-pixel {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.2; }
}
@keyframes ui-thinkc-settle {
  from { opacity: 0.2; transform: scale(0.6); }
  to { opacity: 1; transform: none; }
}
@keyframes ui-thinkc-pop {
  0% { transform: scale(0.5); }
  60% { transform: scale(1.12); }
  100% { transform: scale(1); }
}
@keyframes ui-thinkc-draw {
  to { stroke-dashoffset: 0; }
}
@keyframes ui-thinkc-sweep {
  from { background-position: 100% 0; }
  to { background-position: 0% 0; }
}

.ui-thinkc__summary { margin: 0 8px 4px 34px; color: var(--ui-tkc-muted); font-size: 12px; overflow-wrap: anywhere; }
.ui-thinkc__detail { min-width: 0; margin-top: 4px; color: var(--ui-tkc-text); overflow-wrap: anywhere; }
.ui-thinkc__label { overflow-wrap: anywhere; }
.ui-thinkc__node > svg { width: 15px; height: 15px; }
.ui-thinkc__item[data-active] .ui-thinkc__node > svg { animation: ui-thinkc-pulse 1s ease-in-out infinite; }
.ui-thinkc__outcome { fill: none; stroke: currentColor; stroke-width: 1.4; stroke-linecap: round; }
.ui-thinkc[data-status="error"] .ui-thinkc__title,
.ui-thinkc__item[data-step-status="error"], .ui-thinkc__item[data-step-status="error"] .ui-thinkc__label, .ui-thinkc__item[data-outcome="error"] { color: var(--aui-danger, #d33); }
.ui-thinkc[data-status="error"] .ui-thinkc__mark i { background: var(--aui-danger, #d33); }
.ui-thinkc[data-status="stopped"] .ui-thinkc__mark i { background: var(--ui-tkc-muted); }
.ui-thinkc__head { min-height: 36px; height: auto; padding-block: 7px; }
.ui-thinkc__title { white-space: normal; align-items: baseline; }
.ui-thinkc__time { flex-shrink: 0; }
.reduce-motion .ui-thinkc *, .reduce-motion .ui-thinkc *::before, .reduce-motion .ui-thinkc *::after { animation: none !important; transition: none !important; }
.reduce-motion .ui-thinkc__shimmer::after { display: none; }
.reduce-motion .ui-thinkc__check path { stroke-dashoffset: 0; }

/* Narrow (phones) */
@container (max-width: 360px) {
  .ui-thinkc__head { padding: 0 8px; }
  .ui-thinkc__list { padding: 8px 8px 0; }
}
/* Touch screens: bigger tap targets */
@media (pointer: coarse) {
  .ui-thinkc__head { height: 52px; }
  .ui-thinkc__source { height: 32px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-thinkc *,
  .ui-thinkc *::before,
  .ui-thinkc *::after {
    animation-duration: 1ms !important;
    animation-delay: 0ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 1ms !important;
  }
  .ui-thinkc__shimmer::after {
    display: none;
  }
  .ui-thinkc__check path {
    stroke-dashoffset: 0;
  }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-thinkc {
  --ui-tkc-card: #1c1c1e;
  --ui-tkc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-tkc-strong: #ffffff;
  --ui-tkc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-tkc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-tkc-faint: rgba(255, 255, 255, 0.3);
  --ui-tkc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-tkc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-tkc-rail: var(--aui-border, rgba(255, 255, 255, 0.1));
  --ui-tkc-fill: rgba(255, 255, 255, 0.08);
  --ui-tkc-wash: var(--aui-subtle, rgba(255, 255, 255, 0.04));
  --ui-tkc-accent: var(--aui-blue, #4fb0fe);
  --ui-tkc-shine: #ffffff;
  --ui-tkc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-tkc-shadow-pop: 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 12px 28px -10px rgba(0, 0, 0, 0.6);
  --ui-tkc-ring: 0 0 0 0.5px rgba(255, 255, 255, 0.14);
  --ui-tkc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
}
:where(.dark, [data-theme="dark"]) .ui-thinkc__tip {
  background: #232326;
}
`;
