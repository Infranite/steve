/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import type { HTMLAttributes, KeyboardEvent, ReactNode } from "react";

/*
 * Run Controls (clean) – every control a running agent needs, where you type.
 *   · type while the agent works: Enter steers it right away and ⌥ Enter queues it for when the run is done
 *   · the buttons swap in place – Send when it is idle; Stop while it runs; Queue and Steer once you have typed
 *   · what waits for the run sits above the field, next first, each with its own controls
 *   · how full the context is, as a small ring
 * Steve: the run itself is shown by Steve's own transcript and run bar, so the demo's answer log, its Resume / Retry /
 * Dismiss, the Chat / Act mode and the permission menu are left out (Steve has no resume-from-a-word, its autonomy is
 * the Hub-wide YOLO setting). What is left are the pieces Steve's composers put together: RunActions, RunQueue and
 * RunQueued, RunContext. The words come from the caller (Steve's i18n).
 * Icons: Hugeicons Free (MIT, © Hugeicons, hugeicons.com).
 */

/* ---------- Icons: Hugeicons Free, 24 × 24 strokes drawn at 16px ---------- */
const svg = (children: ReactNode, size = 16) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {children}
  </svg>
);
const IconUp = () => svg(<><path d="M12 5.5V19" /><path d="M18 11C18 11 13.5811 5.00001 12 5C10.4188 4.99999 6 11 6 11" /></>);
const IconStop = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
    <rect x="6" y="6" width="12" height="12" rx="3" fill="currentColor" />
  </svg>
);
const IconClose = () => svg(<path d="M18 6L6.00081 17.9992M17.9992 18L6 6.00085" />, 14);
/** Steve: a turn down and on (Lucide's corner-down-right, Steve's sign for "into the running turn") – the Hugeicons
 * curve read as a phone handset at 14px */
const IconSteer = () => svg(<><path d="M15 10l5 5-5 5" /><path d="M4 4v7a4 4 0 0 0 4 4h12" /></>, 14);
const IconQueue = () => svg(<><path d="M18 18H6C5.05719 18 4.58579 18 4.29289 17.7071C4 17.4142 4 16.9428 4 16C4 15.0572 4 14.5858 4.29289 14.2929C4.58579 14 5.05719 14 6 14H18C18.9428 14 19.4142 14 19.7071 14.2929C20 14.5858 20 15.0572 20 16C20 16.9428 20 17.4142 19.7071 17.7071C19.4142 18 18.9428 18 18 18Z" /><path d="M4 10H20" /><path d="M4 6H20" /></>, 14);
const IconEdit = () => svg(<><path d="M16.4249 4.60509L17.4149 3.6151C18.2351 2.79497 19.5648 2.79497 20.3849 3.6151C21.205 4.43524 21.205 5.76493 20.3849 6.58507L19.3949 7.57506M16.4249 4.60509L9.76558 11.2644C9.25807 11.772 8.89804 12.4078 8.72397 13.1041L8 16L10.8959 15.276C11.5922 15.102 12.228 14.7419 12.7356 14.2344L19.3949 7.57506M16.4249 4.60509L19.3949 7.57506" /><path d="M18.9999 13.5C18.9999 16.7875 18.9999 18.4312 18.092 19.5376C17.9258 19.7401 17.7401 19.9258 17.5375 20.092C16.4312 21 14.7874 21 11.4999 21H11C7.22876 21 5.34316 21 4.17159 19.8284C3.00003 18.6569 3 16.7712 3 13V12.5C3 9.21252 3 7.56879 3.90794 6.46244C4.07417 6.2599 4.2599 6.07417 4.46244 5.90794C5.56879 5 7.21252 5 10.5 5" /></>);
const IconRetry = () => svg(<path d="M20.0092 2V5.13219C20.0092 5.42605 19.6418 5.55908 19.4537 5.33333C17.6226 3.2875 14.9617 2 12 2C6.47715 2 2 6.47715 2 12C2 17.5228 6.47715 22 12 22C17.5228 22 22 17.5228 22 12" />, 14);
/** A turn back – for taking a queued message back */
const IconUndo = () => svg(<><path d="M4 7H15C17.7614 7 20 9.23858 20 12C20 14.7614 17.7614 17 15 17H8" /><path d="M7.5 3.5C7.5 3.5 4 6.07798 4 7C4 7.92202 7.5 10.5 7.5 10.5" /></>, 14);

export { IconEdit, IconUndo, IconSteer, IconQueue, IconClose, IconRetry };

const Spin = () => <span className="ui-rclc__spin" aria-hidden="true" />;

/** Option + Enter queues while a run is on, plain Enter sends (or steers); Shift + Enter is a new line */
export const runKey = (e: KeyboardEvent<HTMLElement>): "send" | "queue" | null =>
  e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing ? null : e.altKey ? "queue" : "send";

export type RunActionsLabels = {
  send: string;
  sending: string;
  stop: string;
  stopping: string;
  /** The tooltip of Stop, with its key */
  stopTip: string;
  queue: string;
  queueTip: string;
  steer?: string;
  steerTip?: string;
};

export type RunActionsProps = {
  /** The agent is working: Stop shows, and what is typed queues (or steers) */
  running?: boolean;
  /** Something is typed or attached */
  has?: boolean;
  /** Sending is possible now (uploads done, not disabled) */
  canSend?: boolean;
  /** A send is on its way */
  sending?: boolean;
  /** Stop was pressed and the run has not ended yet */
  stopping?: boolean;
  /** While it runs, Enter steers the agent; without it what is typed only queues */
  steers?: boolean;
  labels: RunActionsLabels;
  onSend?: () => void;
  onQueue?: () => void;
  onSteer?: () => void;
  /** Without it a running agent can't be stopped from here, and nothing can be sent until it is done */
  onStop?: () => void;
};

/** Send, Stop, Queue and Steer – the buttons swap in place */
export function RunActions({ running, has, canSend, sending, stopping, steers, labels, onSend, onQueue, onSteer, onStop }: RunActionsProps) {
  const live = running && !!onStop;
  return (
    <span className="ui-rclc ui-rclc__actions" data-running={live || undefined}>
      <style href="ui-run-controls-clean" precedence="default">
        {css}
      </style>
      {live && (
        <button type="button" className="ui-rclc__round" data-stop="" data-quiet={has || undefined} data-stopping={stopping || undefined} aria-label={stopping ? labels.stopping : labels.stop} title={stopping ? labels.stopping : labels.stopTip} disabled={stopping} onClick={(e) => (e.stopPropagation(), onStop?.())}>
          {stopping ? <Spin /> : <IconStop />}
        </button>
      )}
      {live && has ? (
        steers ? (
          <>
            <button type="button" className="ui-rclc__btn" title={labels.queueTip} aria-label={labels.queue} disabled={!canSend || sending} onClick={(e) => (e.stopPropagation(), onQueue?.())} data-send-mode="queue">
              <IconQueue />
              {labels.queue}
            </button>
            <button type="button" className="ui-rclc__btn" data-solid="" title={labels.steerTip} aria-label={sending ? labels.sending : labels.steer} disabled={!canSend || sending} onClick={(e) => (e.stopPropagation(), onSteer?.())} data-send="" data-send-mode="steer">
              {sending ? <Spin /> : <IconSteer />}
              {labels.steer}
            </button>
          </>
        ) : (
          <button type="button" className="ui-rclc__btn" data-solid="" title={labels.queueTip} aria-label={sending ? labels.sending : labels.queue} disabled={!canSend || sending} onClick={(e) => (e.stopPropagation(), onQueue?.())} data-send="" data-send-queues="">
            {sending ? <Spin /> : <IconQueue />}
            {labels.queue}
          </button>
        )
      ) : live ? null : (
        <button type="button" className="ui-rclc__round" aria-label={sending ? labels.sending : labels.send} title={labels.send} disabled={!canSend || sending || (running && !onStop)} onClick={(e) => (e.stopPropagation(), onSend?.())} data-send="">
          {sending ? <Spin /> : <IconUp />}
        </button>
      )}
    </span>
  );
}

/** Messages waiting for the run: a note on top, next first */
export function RunQueue({ note, label, children, className, ...rest }: HTMLAttributes<HTMLDivElement> & { note?: ReactNode; label: string }) {
  return (
    <div role="region" aria-label={label} className={["ui-rclc ui-rclc__queue-box", className].filter(Boolean).join(" ")} {...rest}>
      <style href="ui-run-controls-clean" precedence="default">
        {css}
      </style>
      {note && (
        <p className="ui-rclc__queue-note" data-sq-note="">
          <IconQueue />
          <span>{note}</span>
        </p>
      )}
      <ul className="ui-rclc__queue">{children}</ul>
    </div>
  );
}

/** One waiting message: its words, its place in line, and what can be done with it */
export function RunQueued({ place, children, actions, className, ...rest }: HTMLAttributes<HTMLLIElement> & { place?: ReactNode; actions?: ReactNode }) {
  return (
    <li className={["ui-rclc__queued", className].filter(Boolean).join(" ")} {...rest}>
      <div className="ui-rclc__queued-text">{children}</div>
      {(place || actions) && (
        <div className="ui-rclc__queued-side">
          {place && <span className="ui-rclc__queued-n">{place}</span>}
          {actions}
        </div>
      )}
    </li>
  );
}

/** A small button in a queued row: an icon, a label when `text` */
export function RunAct({ icon, label, text, tone, className, ...rest }: HTMLAttributes<HTMLButtonElement> & { icon: ReactNode; label: string; text?: string; tone?: "accent"; disabled?: boolean }) {
  return (
    <button type="button" className={[text ? "ui-rclc__act" : "ui-rclc__icon", className].filter(Boolean).join(" ")} data-tone={tone} aria-label={label} title={text ? undefined : label} {...rest}>
      <style href="ui-run-controls-clean" precedence="default">
        {css}
      </style>
      {icon}
      {text}
    </button>
  );
}

/** How full the context is: a ring and its percent (on phones too – a touch screen shows no tooltip), orange from 80% */
export function RunContext({ value, title }: { value: number; title: string }) {
  const ctx = Math.min(1, Math.max(0, value));
  const R = 6;
  const C = 2 * Math.PI * R;
  return (
    <span className="ui-rclc ui-rclc__ctx" data-high={ctx >= 0.8 ? "" : undefined} title={title} data-context={Math.round(ctx * 100)}>
      <style href="ui-run-controls-clean" precedence="default">
        {css}
      </style>
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <circle cx="8" cy="8" r={R} className="ui-rclc__ctx-track" />
        <circle cx="8" cy="8" r={R} className="ui-rclc__ctx-fill" strokeDasharray={`${C * ctx} ${C}`} transform="rotate(-90 8 8)" />
      </svg>
      <span className="ui-rclc__ctx-n" aria-hidden="true">{Math.round(ctx * 100)}%</span>
      <span className="ui-rclc__sr">{title}</span>
    </span>
  );
}

const css = /* css */ `
.ui-rclc {
  /* Every color is a design system token (--aui-*, see the Design System page); the fallbacks are the clean values */
  --ui-rclc-card: var(--aui-surface, #ffffff);
  --ui-rclc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-rclc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-rclc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.3));
  --ui-rclc-line: var(--aui-border, rgba(0, 0, 0, 0.08));
  --ui-rclc-hair: var(--aui-border-strong, rgba(0, 0, 0, 0.13));
  --ui-rclc-subtle: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-rclc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-rclc-solid-hover: var(--aui-primary-hover, rgba(0, 0, 0, 0.78));
  --ui-rclc-on-solid: var(--aui-on-primary, #ffffff);
  --ui-rclc-info: var(--aui-info, #0070d6);
  --ui-rclc-warn: var(--aui-orange, #f28a2e);
  --ui-rclc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-rclc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  /* Derived from the tokens */
  --ui-rclc-wash: color-mix(in srgb, var(--ui-rclc-ink) 5%, transparent);
  --ui-rclc-tray: color-mix(in srgb, var(--ui-rclc-ink) 3.5%, var(--ui-rclc-card));
  box-sizing: border-box;
  min-width: 0;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  color: var(--ui-rclc-ink);
  -webkit-font-smoothing: antialiased;
}
/* Steve: only the pieces' own lists, buttons and icons – what a page puts in a row (its text, files) keeps its look */
:where(.ui-rclc__queue, .ui-rclc__queue-note) { margin: 0; padding: 0; list-style: none; }
:where(.ui-rclc__round, .ui-rclc__btn, .ui-rclc__icon, .ui-rclc__act) { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:is(.ui-rclc__round, .ui-rclc__btn, .ui-rclc__icon, .ui-rclc__act, .ui-rclc__queue-note, .ui-rclc__ctx) > svg { display: block; flex: none; }
:is(.ui-rclc__round, .ui-rclc__btn, .ui-rclc__icon, .ui-rclc__act):focus-visible { outline: 2px solid var(--ui-rclc-focus); outline-offset: 2px; }
.ui-rclc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.ui-rclc__spin { width: 14px; height: 14px; border-radius: 50%; border: 1.5px solid currentColor; border-right-color: transparent; animation: ui-rclc-spin 700ms linear infinite; }

/* ----- The queue: what waits for the run, next first – a soft tray, four rows before it scrolls ----- */
.ui-rclc__queue-box { display: flex; flex-direction: column; border-radius: 10px; background: var(--ui-rclc-tray); animation: ui-rclc-rise 260ms var(--ui-rclc-ease) both; }
.ui-rclc__queue-note { display: flex; align-items: center; gap: 6px; padding: 7px 10px 2px; font-size: 12px; line-height: 18px; color: var(--ui-rclc-muted); }
.ui-rclc__queue-note > svg { width: 14px; height: 14px; }
.ui-rclc__queue { display: grid; grid-template-columns: minmax(0, 1fr); max-height: min(232px, 34vh); overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; padding: 2px 4px 4px; }
.ui-rclc__queued { display: flex; align-items: flex-start; gap: 8px; min-height: 32px; padding: 5px 2px 5px 6px; border-radius: 7px; font-size: 13px; color: var(--ui-rclc-muted); animation: ui-rclc-rise 260ms var(--ui-rclc-ease) both; }
.ui-rclc__queued + .ui-rclc__queued { box-shadow: 0 -0.5px 0 var(--ui-rclc-line); border-radius: 0; }
.ui-rclc__queued-text { flex: 1; min-width: 0; padding-top: 1px; color: var(--ui-rclc-ink); overflow-wrap: anywhere; }
.ui-rclc__queued-side { display: flex; flex: none; align-items: center; gap: 2px; min-height: 22px; }
/* Steve: while a row is edited – and on a phone – its words take the full row and its controls go underneath */
.ui-rclc__queued[data-editing] { flex-direction: column; align-items: stretch; gap: 4px; }
.ui-rclc__queued[data-editing] > .ui-rclc__queued-text { flex: none; width: 100%; }
@media (max-width: 639.98px) {
  .ui-rclc__queued { flex-direction: column; align-items: stretch; gap: 4px; }
  .ui-rclc__queued > .ui-rclc__queued-text { flex: none; width: 100%; }
}
.ui-rclc__queued-n { flex: none; margin-inline-end: 4px; font-size: 11.5px; color: var(--ui-rclc-faint); font-variant-numeric: tabular-nums; white-space: nowrap; }
.ui-rclc__icon { display: grid; flex: none; place-items: center; width: 24px; height: 24px; border-radius: 6px; color: var(--ui-rclc-muted); transition: background-color 120ms ease, color 120ms ease; }
.ui-rclc__icon:hover:not(:disabled) { background: var(--ui-rclc-wash); color: var(--ui-rclc-ink); }
.ui-rclc__act { display: inline-flex; flex: none; align-items: center; gap: 4px; height: 24px; padding: 0 7px 0 5px; border-radius: 6px; font-size: 12px; font-weight: 500; white-space: nowrap; color: var(--ui-rclc-muted); transition: background-color 120ms ease, color 120ms ease; }
.ui-rclc__act:hover:not(:disabled) { background: var(--ui-rclc-wash); color: var(--ui-rclc-ink); }
.ui-rclc__act[data-tone="accent"] { color: var(--ui-rclc-info); }
.ui-rclc__act[data-tone="accent"]:hover:not(:disabled) { background: color-mix(in srgb, var(--ui-rclc-info) 10%, transparent); color: var(--ui-rclc-info); }
:is(.ui-rclc__icon, .ui-rclc__act):disabled { opacity: 0.45; cursor: default; }

/* ----- Send, Stop, Queue and Steer ----- */
.ui-rclc__actions { display: inline-flex; flex: none; align-items: center; gap: 6px; }
.ui-rclc__round { display: grid; flex: none; place-items: center; width: 28px; height: 28px; border-radius: 50%; background: var(--ui-rclc-solid); color: var(--ui-rclc-on-solid); transition: background-color 120ms ease, color 120ms ease, opacity 120ms ease, transform 120ms var(--ui-rclc-ease); animation: ui-rclc-pop 180ms var(--ui-rclc-ease) both; }
.ui-rclc__round:hover:not(:disabled) { background: var(--ui-rclc-solid-hover); }
.ui-rclc__round:active:not(:disabled) { transform: scale(0.92); }
.ui-rclc__round:disabled { background: var(--ui-rclc-wash); color: var(--ui-rclc-faint); cursor: default; }
/* Stop next to Queue and Steer: an outline, so the one solid button is what Enter does */
.ui-rclc__round[data-quiet], .ui-rclc__round[data-stopping] { background: transparent; color: var(--ui-rclc-ink); box-shadow: inset 0 0 0 1px var(--ui-rclc-hair); }
.ui-rclc__round[data-quiet]:hover:not(:disabled) { background: var(--ui-rclc-wash); }
.ui-rclc__round[data-stopping] { color: var(--ui-rclc-muted); }
.ui-rclc__btn { display: inline-flex; flex: none; align-items: center; gap: 5px; height: 28px; padding: 0 10px 0 8px; border-radius: 8px; font-size: 12.5px; font-weight: 500; white-space: nowrap; color: var(--ui-rclc-muted); transition: background-color 120ms ease, color 120ms ease, opacity 120ms ease; animation: ui-rclc-pop 180ms var(--ui-rclc-ease) both; }
.ui-rclc__btn:hover:not(:disabled) { background: var(--ui-rclc-wash); color: var(--ui-rclc-ink); }
.ui-rclc__btn[data-solid] { background: var(--ui-rclc-solid); color: var(--ui-rclc-on-solid); }
.ui-rclc__btn[data-solid]:hover:not(:disabled) { background: var(--ui-rclc-solid-hover); color: var(--ui-rclc-on-solid); }
.ui-rclc__btn:disabled { opacity: 0.5; cursor: default; }

/* ----- Context: a small ring, orange once it's nearly full ----- */
.ui-rclc__ctx { display: inline-flex; flex: none; align-items: center; gap: 5px; height: 28px; padding: 0 4px; font-size: 12px; color: var(--ui-rclc-muted); font-variant-numeric: tabular-nums; }
.ui-rclc__ctx-track { fill: none; stroke: var(--ui-rclc-line); stroke-width: 2; }
.ui-rclc__ctx-fill { fill: none; stroke: var(--ui-rclc-muted); stroke-width: 2; stroke-linecap: round; transition: stroke-dasharray 260ms var(--ui-rclc-ease), stroke 180ms ease; }
.ui-rclc__ctx[data-high] { color: var(--ui-rclc-warn); }
.ui-rclc__ctx[data-high] .ui-rclc__ctx-fill { stroke: var(--ui-rclc-warn); }

@keyframes ui-rclc-rise { from { opacity: 0; transform: translateY(6px); filter: blur(2px); } }
@keyframes ui-rclc-pop { from { opacity: 0; transform: scale(0.9); } }
@keyframes ui-rclc-spin { to { transform: rotate(360deg); } }

/* Touch screens: taller hit targets */
@media (pointer: coarse) {
  .ui-rclc__round { width: 34px; height: 34px; }
  .ui-rclc__btn { height: 34px; }
  .ui-rclc__icon { width: 30px; height: 30px; }
  .ui-rclc__act { height: 30px; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-rclc, .ui-rclc [class*="ui-rclc__"] { animation: none !important; transition: none !important; }
  .ui-rclc__spin { animation: ui-rclc-spin 1.4s linear infinite !important; }
}
`;
