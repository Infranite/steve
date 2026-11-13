/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";

/*
 * Running Now – a live status of background agent tasks, in a single self-contained file.
 * Collapsed it is one quiet pill for a header or a sidebar – "3 running · 2 need you". Open, a popover lists the tasks:
 * a ring that fills as the task moves on, the title, the step it is on, "2/4", and Pause / Resume – a task that waits
 * on you says so with a small badge and an Open button instead.
 * Classic style: the popover is a gray tray with the tasks on a white card, the way to all tasks on the tray below it.
 * Icons: Hugeicons Free (MIT, © Hugeicons, hugeicons.com). The styles are injected below, so no extra files are needed.
 */

export type RunningTaskState = "running" | "paused" | "needs-you" | "done";

export type RunningTask = {
  id: string;
  title: string;
  /** The step it is on right now, e.g. "Embedding 1,284 chunks" – or what it waits for */
  step: string;
  /** Steps finished */
  done: number;
  /** Steps in all */
  total: number;
  state: RunningTaskState;
  /** Steve: a paused task that can't be resumed from here (it needs its own page) offers Open instead of Resume */
  resumable?: boolean;
};

export type RunningNowLabels = {
  title: string;
  running: string;
  needsYou: string;
  /** The same for exactly one task – "1 needs you" */
  needsYouOne: string;
  needsYouBadge: string;
  paused: string;
  /** The trigger when nothing runs or waits but some tasks are paused – "5 paused" */
  pausedCount: string;
  done: string;
  pause: string;
  resume: string;
  open: string;
  openAll: string;
  empty: string;
  idle: string;
  /** Steve: the steps for a screen reader, with {done} and {total} */
  stepsOf: string;
};

export type RunningNowProps = {
  tasks: RunningTask[];
  onPause?: (id: string) => void;
  onResume?: (id: string) => void;
  /** Open a task – the row itself, or the Open button of a task that needs you */
  onOpen?: (id: string) => void;
  /** Open the full list – shows the footer link when set */
  onOpenAll?: () => void;
  defaultOpen?: boolean;
  /** Control the popover from outside */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Which edge of the trigger the popover lines up with */
  align?: "start" | "end";
  labels?: Partial<RunningNowLabels>;
  className?: string;
};

const defaults: RunningNowLabels = {
  title: "Background tasks",
  running: "running",
  needsYou: "need you",
  needsYouOne: "needs you",
  needsYouBadge: "Needs you",
  paused: "Paused",
  pausedCount: "paused",
  done: "Done",
  pause: "Pause",
  resume: "Resume",
  open: "Open",
  openAll: "Open Tasks",
  empty: "Nothing running right now",
  idle: "No tasks running",
  stepsOf: "{done} of {total} steps",
};

/* ---------- Helpers ---------- */

const num = new Intl.NumberFormat("en-US");
const finite = (n: unknown) => (typeof n === "number" && Number.isFinite(n) ? n : 0);
/** Steps as a ratio 0…1 – a done task is always full */
const progressOf = (t: RunningTask) => {
  if (t.state === "done") return 1;
  const total = Math.max(0, finite(t.total));
  return total > 0 ? Math.min(1, Math.max(0, finite(t.done) / total)) : 0;
};
/** Steve: a task's color is its state's (run blue, waiting amber, paused gray, done green), not how far it is */
const progressColor = (t: RunningTask) =>
  t.state === "done" ? "var(--ui-rn-done)" : t.state === "paused" ? "var(--ui-rn-muted)" : t.state === "needs-you" ? "var(--ui-rn-amber)" : "var(--ui-rn-blue)";

/* ---------- Component ---------- */

export function RunningNow({
  tasks,
  onPause,
  onResume,
  onOpen,
  onOpenAll,
  defaultOpen = false,
  open: openProp,
  onOpenChange,
  align = "end",
  labels: labelsProp,
  className,
}: RunningNowProps) {
  // A custom needsYou without its singular is used for one task too, so the words never mix
  const labels: RunningNowLabels = { ...defaults, ...(labelsProp?.needsYou && !labelsProp.needsYouOne ? { needsYouOne: labelsProp.needsYou } : null), ...labelsProp };
  const list = (Array.isArray(tasks) ? tasks : []).filter((t): t is RunningTask => !!t && typeof t === "object" && !!t.id);
  const [openState, setOpenState] = useState(defaultOpen);
  const open = openProp ?? openState;
  const setOpen = (next: boolean) => {
    if (openProp === undefined) setOpenState(next);
    onOpenChange?.(next);
  };

  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const panelId = useId();

  const running = list.filter((t) => t.state === "running").length;
  const waiting = list.filter((t) => t.state === "needs-you").length;
  const waitingWord = waiting === 1 ? labels.needsYouOne : labels.needsYou;
  // Nothing runs or waits, but tasks are paused: the trigger says so instead of "No tasks running"
  const paused = list.filter((t) => t.state === "paused").length;

  // A click outside closes it; so does focus leaving it (Tab past the last row)
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  });

  // The popover stays on screen: on a narrow one it slides sideways until it clears both edges by 16px
  const panelRef = useRef<HTMLDivElement>(null);
  const [shift, setShift] = useState(0);
  useLayoutEffect(() => {
    if (!open) return;
    const fit = () => {
      const el = panelRef.current;
      const root = rootRef.current;
      if (!el || !root) return;
      // Measured from the trigger, not the panel – its opening animation scales it a little
      const box = root.getBoundingClientRect();
      const w = el.offsetWidth;
      // In a right-to-left page "start" is the right edge
      const rtl = getComputedStyle(root).direction === "rtl";
      const left = (align === "start") !== rtl ? box.left : box.right - w;
      const vw = document.documentElement.clientWidth;
      const next = left < 16 ? 16 - left : left + w > vw - 16 ? Math.max(16 - left, vw - 16 - left - w) : 0;
      setShift(Math.round(next));
    };
    fit();
    window.addEventListener("resize", fit);
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(fit);
    if (rootRef.current) ro?.observe(rootRef.current);
    return () => {
      window.removeEventListener("resize", fit);
      ro?.disconnect();
    };
  }, [open, align]);

  // Changes worth hearing: a task that starts waiting on you, a task that finishes
  const [announce, setAnnounce] = useState("");
  const seen = useRef<Map<string, RunningTaskState> | null>(null);
  useEffect(() => {
    const before = seen.current;
    const now = new Map(list.map((t) => [t.id, t.state]));
    seen.current = now;
    if (!before) return;
    const said: string[] = [];
    for (const t of list) {
      const was = before.get(t.id);
      if (was === t.state || was === undefined) continue;
      if (t.state === "needs-you") said.push(`${t.title} ${labels.needsYouBadge.toLowerCase()}`);
      if (t.state === "done") said.push(`${t.title}: ${labels.done.toLowerCase()}`);
    }
    if (said.length) setAnnounce(said.join(". "));
  });

  const rows = () => [...(listRef.current?.querySelectorAll<HTMLButtonElement>(".ui-rn__main") ?? [])];
  /** Into the list: the first (or last) task – the footer link or the panel itself when there are none */
  const focusIn = (last = false) =>
    requestAnimationFrame(() => {
      const all = rows();
      (all[last ? all.length - 1 : 0] ?? panelRef.current?.querySelector<HTMLButtonElement>("button") ?? panelRef.current)?.focus();
    });
  const onTriggerKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    if (!open) setOpen(true);
    focusIn(e.key === "ArrowUp");
  };
  const onPanelKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
      triggerRef.current?.focus();
      return;
    }
    const all = rows();
    // From a row or from its Pause / Resume / Open button
    const li = (e.target as HTMLElement).closest?.(".ui-rn__row");
    const i = all.findIndex((b) => b.parentElement === li);
    if (i < 0) return;
    const next = e.key === "ArrowDown" ? Math.min(all.length - 1, i + 1) : e.key === "ArrowUp" ? Math.max(0, i - 1) : e.key === "Home" ? 0 : e.key === "End" ? all.length - 1 : null;
    if (next === null) return;
    e.preventDefault();
    all[next]?.focus();
  };

  const lastFocus = useRef<{ el: HTMLElement; id?: string; index: number } | null>(null);
  const onPanelFocus = (e: FocusEvent<HTMLDivElement>) => {
    const li = (e.target as HTMLElement).closest<HTMLElement>(".ui-rn__row");
    lastFocus.current = { el: e.target as HTMLElement, id: li?.dataset.id, index: li ? rows().findIndex((b) => b.parentElement === li) : -1 };
  };
  useLayoutEffect(() => {
    const f = lastFocus.current;
    if (!open || !f || f.el.isConnected) return;
    lastFocus.current = null;
    const active = document.activeElement;
    if (active && active !== document.body && active !== f.el) return;
    const all = rows();
    const same = all.find((b) => b.parentElement?.dataset.id === f.id);
    (same ?? all[Math.min(f.index, all.length - 1)] ?? panelRef.current?.querySelector<HTMLButtonElement>("button") ?? panelRef.current)?.focus();
  });

  const summary =
    running || waiting
      ? [running ? `${num.format(running)} ${labels.running}` : "", waiting ? `${num.format(waiting)} ${waitingWord}` : ""].filter(Boolean).join(", ")
      : paused
        ? `${num.format(paused)} ${labels.pausedCount}`
        : labels.idle;

  return (
    <>
      <style href="ui-running-now" precedence="default">
        {css}
      </style>
      <div
        ref={rootRef}
        className={["ui-rn", className].filter(Boolean).join(" ")}
        data-open={open || undefined}
        data-align={align}
        data-both={(running > 0 && waiting > 0) || undefined}
        data-long={num.format(running).length + num.format(waiting).length > 4 || undefined}
        onKeyDown={(e) => {
          if (e.key === "Escape" && open && e.target === triggerRef.current) setOpen(false);
        }}
        onBlur={(e) => {
          if (open && e.relatedTarget && !rootRef.current?.contains(e.relatedTarget as Node)) setOpen(false);
        }}
      >
        <button
          ref={triggerRef}
          type="button"
          className="ui-rn__trigger"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-controls={panelId}
          aria-label={`${labels.title}: ${summary}`}
          onClick={(e) => {
            setOpen(!open);
            // Enter or Space (a click without a pointer) takes focus into the list, as ↓ does
            if (!open && e.detail === 0) focusIn();
          }}
          onKeyDown={onTriggerKey}
        >
          {running || waiting ? (
            <>
              {running > 0 && (
                <span className="ui-rn__part">
                  <span className="ui-rn__ring" aria-hidden="true" />
                  <Swap className="ui-rn__n" value={num.format(running)} />
                  <span className="ui-rn__word">{labels.running}</span>
                </span>
              )}
              {running > 0 && waiting > 0 && (
                <span className="ui-rn__sep" aria-hidden="true">
                  ·
                </span>
              )}
              {waiting > 0 && (
                <span className="ui-rn__part">
                  <span className="ui-rn__dot" aria-hidden="true" />
                  <Swap className="ui-rn__n" value={num.format(waiting)} />
                  <span className="ui-rn__word">{waitingWord}</span>
                </span>
              )}
            </>
          ) : paused > 0 ? (
            <span className="ui-rn__part">
              <span className="ui-rn__ring" data-idle="" aria-hidden="true" />
              <Swap className="ui-rn__n" value={num.format(paused)} />
              <span className="ui-rn__word">{labels.pausedCount}</span>
            </span>
          ) : (
            <span className="ui-rn__part">
              <span className="ui-rn__ring" data-idle="" aria-hidden="true" />
              <span className="ui-rn__word">{labels.idle}</span>
            </span>
          )}
          <ChevronDown />
        </button>

        <div ref={panelRef} id={panelId} className="ui-rn__panel" style={shift ? { translate: `${shift}px 0` } : undefined} role="dialog" aria-label={labels.title} tabIndex={-1} hidden={!open} onKeyDown={onPanelKey} onFocus={onPanelFocus}>
          <div className="ui-rn__head">
            <span className="ui-rn__label">{labels.title}</span>
            <span className="ui-rn__count">{num.format(list.length)}</span>
          </div>
          <div className="ui-rn__body">
            {list.length ? (
              <ul ref={listRef} className="ui-rn__list">
                {list.map((t) => (
                  <Row key={t.id} task={t} labels={labels} onPause={onPause} onResume={onResume} onOpen={onOpen} />
                ))}
              </ul>
            ) : (
              <p className="ui-rn__empty">{labels.empty}</p>
            )}
          </div>
          {onOpenAll && (
            <div className="ui-rn__foot">
              <button type="button" className="ui-rn__all" onClick={onOpenAll}>
                <TasksIcon />
                <span>{labels.openAll}</span>
                <ChevronRight />
              </button>
            </div>
          )}
        </div>

        <span className="ui-rn__sr" role="status" aria-live="polite">
          {announce}
        </span>
      </div>
    </>
  );
}

/** One task: ring, title, the step it is on, its steps or the Needs you badge, and its one action */
function Row({
  task: t,
  labels,
  onPause,
  onResume,
  onOpen,
}: {
  task: RunningTask;
  labels: RunningNowLabels;
  onPause?: (id: string) => void;
  onResume?: (id: string) => void;
  onOpen?: (id: string) => void;
}) {
  const value = progressOf(t);
  const total = Math.max(0, finite(t.total));
  const done = t.state === "done" ? total : Math.min(total, Math.max(0, finite(t.done)));
  // Steve: a paused task's step already says how it is paused ("协作已暂停 · ST-41 失败"), so it isn't prefixed again
  const step = t.state === "done" ? labels.done : t.state === "paused" ? t.step || labels.paused : t.step;
  const state = t.state === "needs-you" ? labels.needsYouBadge : t.state === "paused" ? labels.paused : t.state === "done" ? labels.done : labels.running;
  return (
    <li className="ui-rn__row" data-state={t.state} data-id={t.id}>
      <button
        type="button"
        className="ui-rn__main"
        aria-label={`${t.title} – ${state}. ${t.state === "done" || !t.step ? "" : `${t.step}. `}${total ? labels.stepsOf.replace("{done}", num.format(done)).replace("{total}", num.format(total)) : ""}`}
        onClick={() => onOpen?.(t.id)}
      >
        <Ring value={value} color={progressColor(t)} />
        <span className="ui-rn__text">
          <span className="ui-rn__title" dir="auto" title={t.title}>
            {t.title}
          </span>
          <Swap className="ui-rn__step" value={step} />
        </span>
        {t.state === "needs-you" ? (
          <span className="ui-rn__badge">{labels.needsYouBadge}</span>
        ) : total > 0 ? (
          <span className="ui-rn__steps">
            {num.format(done)}/{num.format(total)}
          </span>
        ) : null}
      </button>
      <span className="ui-rn__act">
        {t.state === "needs-you" || (t.state === "paused" && t.resumable === false) ? (
          <button type="button" className="ui-rn__btn ui-rn__btn--text" aria-label={`${labels.open} ${t.title} – ${(t.state === "paused" ? labels.paused : labels.needsYouBadge).toLowerCase()}`} onClick={() => onOpen?.(t.id)}>
            {labels.open}
          </button>
        ) : t.state === "running" && onPause ? (
          <button type="button" className="ui-rn__btn" aria-label={`${labels.pause} ${t.title}`} title={labels.pause} onClick={() => onPause?.(t.id)}>
            <PauseIcon />
          </button>
        ) : t.state === "paused" && onResume ? (
          <button type="button" className="ui-rn__btn" aria-label={`${labels.resume} ${t.title}`} title={labels.resume} onClick={() => onResume?.(t.id)}>
            <PlayIcon />
          </button>
        ) : t.state === "done" ? (
          <span className="ui-rn__btn" aria-hidden="true">
            <CheckIcon />
          </span>
        ) : null}
      </span>
    </li>
  );
}

/** Linear's "In progress" circle: a ring with a pie inside that fills as the steps go by */
function Ring({ value, color }: { value: number; color: string }) {
  const r = 2;
  const c = 2 * Math.PI * r;
  return (
    <svg className="ui-rn__mark" viewBox="0 0 14 14" width="14" height="14" aria-hidden="true" style={{ color }}>
      <circle cx="7" cy="7" r="6" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle className="ui-rn__pie" cx="7" cy="7" r={r} fill="none" stroke="currentColor" strokeWidth={r * 2} style={{ strokeDasharray: `${c * value} ${c}` } as CSSProperties} transform="rotate(-90 7 7)" />
    </svg>
  );
}

/** Text that crossfades when it changes – the old line slides up and out while the new one comes in */
function Swap({ value, className }: { value: string; className?: string }) {
  const [items, setItems] = useState<{ k: number; v: string; out?: boolean; fresh?: boolean }[]>([{ k: 0, v: value }]);
  const last = useRef(value);
  const n = useRef(0);
  useEffect(() => {
    if (value === last.current) return;
    last.current = value;
    const k = ++n.current;
    setItems((list) => [{ k, v: value, fresh: true }, ...list.filter((i) => !i.out).slice(0, 1).map((i) => ({ ...i, out: true, fresh: false }))]);
    const timer = setTimeout(() => setItems((list) => list.filter((i) => !i.out)), 360);
    return () => clearTimeout(timer);
  }, [value]);
  return (
    <span className={["ui-rn__swap", className].filter(Boolean).join(" ")}>
      {items.map((i) => (
        <span key={i.k} data-out={i.out || undefined} data-fresh={i.fresh || undefined} aria-hidden={i.out || undefined} dir="auto" title={i.out ? undefined : i.v}>
          {i.v}
        </span>
      ))}
    </span>
  );
}

/* ---------- Icons – Hugeicons Free (MIT, © Hugeicons, hugeicons.com) ---------- */

const Svg = ({ children }: { children: ReactNode }) => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {children}
  </svg>
);
const PauseIcon = () => (
  <Svg>
    <path d="M4 7C4 5.59 4 4.88 4.44 4.44C4.88 4 5.59 4 7 4C8.41 4 9.12 4 9.56 4.44C10 4.88 10 5.59 10 7V17C10 18.41 10 19.12 9.56 19.56C9.12 20 8.41 20 7 20C5.59 20 4.88 20 4.44 19.56C4 19.12 4 18.41 4 17V7Z" />
    <path d="M14 7C14 5.59 14 4.88 14.44 4.44C14.88 4 15.59 4 17 4C18.41 4 19.12 4 19.56 4.44C20 4.88 20 5.59 20 7V17C20 18.41 20 19.12 19.56 19.56C19.12 20 18.41 20 17 20C15.59 20 14.88 20 14.44 19.56C14 19.12 14 18.41 14 17V7Z" />
  </Svg>
);
const PlayIcon = () => (
  <Svg>
    <path d="M18.89 12.85C18.54 14.19 16.87 15.14 13.53 17.04C10.3 18.87 8.68 19.79 7.38 19.42C6.84 19.27 6.35 18.98 5.96 18.58C5 17.61 5 15.74 5 12C5 8.26 5 6.39 5.96 5.42C6.35 5.02 6.84 4.73 7.38 4.58C8.68 4.21 10.3 5.13 13.53 6.96C16.87 8.86 18.54 9.81 18.89 11.15C19.04 11.71 19.04 12.29 18.89 12.85Z" />
  </Svg>
);
const CheckIcon = () => (
  <Svg>
    <path d="M5 14L8.5 17.5L19 6.5" />
  </Svg>
);
const TasksIcon = () => (
  <Svg>
    <path d="M13.5 2H8.5C7.67 2 7 2.67 7 3.5C7 4.33 7.67 5 8.5 5H13.5C14.33 5 15 4.33 15 3.5C15 2.67 14.33 2 13.5 2Z" />
    <path d="M7 15H10.43M7 11H15" />
    <path d="M19 13.5V9.48C19 6.65 19 5.24 18.12 4.36C17.48 3.72 16.55 3.55 15 3.5M12 22L9 22C6.17 22 4.76 22 3.88 21.12C3 20.24 3 18.83 3 16L3 9.48C3 6.65 3 5.24 3.88 4.36C4.52 3.72 5.44 3.55 7 3.5" />
    <path d="M14 20C14 20 15 20 16 22C16 22 18.17 17 21 16" />
  </Svg>
);
const ChevronDown = () => (
  <svg className="ui-rn__chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M18 9C18 9 13.58 15 12 15C10.42 15 6 9 6 9" />
  </svg>
);
const ChevronRight = () => (
  <svg className="ui-rn__chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M9 6C9 6 15 10.42 15 12C15 13.58 9 18 9 18" />
  </svg>
);

/* ---------- Styles ---------- */


const css = /* css */ `
/* Colors and motion as variables – override them on .ui-rn */
.ui-rn {
  --ui-rn-tray: var(--aui-subtle, #f0f0f0);
  --ui-rn-tray-hover: color-mix(in srgb, var(--aui-ink, #171717) 7%, var(--ui-rn-tray));
  --ui-rn-card: var(--aui-overlay, #ffffff);
  --ui-rn-strong: var(--aui-ink, #171717);
  --ui-rn-ink: var(--aui-ink, #171717);
  --ui-rn-text: var(--aui-muted, #6b6b6b);
  --ui-rn-muted: var(--aui-muted, #8a8a8a);
  --ui-rn-faint: var(--aui-placeholder, #b8b8b8);
  --ui-rn-icon: var(--aui-muted, #8a8a8a);
  --ui-rn-line: var(--aui-border, #ececec);
  --ui-rn-hover: var(--aui-subtle, #f5f5f5);
  --ui-rn-press: color-mix(in srgb, var(--aui-ink, #171717) 9%, transparent);
  --ui-rn-fill: var(--aui-subtle, #f2f2f2);
  --ui-rn-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-rn-amber: var(--aui-amber, #e8b000);
  --ui-rn-blue: var(--aui-blue, #0190fe);
  --ui-rn-done: var(--aui-success, #1e8a4f);
  --ui-rn-badge: var(--aui-warning, #8f6c00);
  --ui-rn-badge-wash: var(--aui-warning-soft, #f9f1d6);
  /* A 1px edge and a soft lift; the popover floats a little higher */
  --ui-rn-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-rn-line), 0 1px 2px rgba(0, 0, 0, 0.04));
  --ui-rn-pop: var(--aui-shadow-popover, 0 0 0 1px var(--ui-rn-line), 0 1px 2px rgba(0, 0, 0, 0.04), 0 14px 32px -12px rgba(0, 0, 0, 0.18));
  --ui-rn-ease: var(--aui-ease, cubic-bezier(0.32, 0.72, 0.24, 1));
  /* How wide the popover is */
  --ui-rn-w: 348px;
  /* One task row */
  --ui-rn-row: 52px;
  position: relative;
  display: inline-block;
  flex: none;
  box-sizing: border-box;
  max-width: 100%;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-rn-text);
  text-align: start;
  -webkit-font-smoothing: antialiased;
}
.ui-rn *, .ui-rn *::before, .ui-rn *::after { box-sizing: border-box; }
:where(.ui-rn) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-rn button:focus-visible { outline: 2px solid var(--ui-rn-focus); outline-offset: 2px; }
.ui-rn :is(p, ul) { margin: 0; padding: 0; list-style: none; }
.ui-rn svg { display: block; flex: none; }
.ui-rn__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* ----- The trigger: a gray pill with the counts in it ----- */
.ui-rn__trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 100%;
  height: 30px;
  padding: 0 9px 0 11px;
  border-radius: calc(var(--aui-radius, 10px) - 2px);
  background: var(--ui-rn-tray);
  font-size: 12px;
  line-height: 16px;
  color: var(--ui-rn-muted);
  white-space: nowrap;
  transition: background-color 120ms ease;
}
.ui-rn__trigger:hover, .ui-rn[data-open] .ui-rn__trigger { background: var(--ui-rn-tray-hover); }
.ui-rn__part { display: inline-flex; flex: none; align-items: center; gap: 4px; }
.ui-rn__n { flex: none; font-weight: 500; color: var(--ui-rn-ink); font-variant-numeric: tabular-nums; }
.ui-rn__sep { flex: none; color: var(--aui-placeholder, var(--ui-rn-faint)); }
.ui-rn__ring { flex: none; width: 8px; height: 8px; margin-right: 2px; border-radius: 50%; box-shadow: inset 0 0 0 1.5px var(--ui-rn-blue); }
.ui-rn__ring[data-idle] { box-shadow: inset 0 0 0 1.5px var(--ui-rn-faint); }
.ui-rn__dot { flex: none; width: 6px; height: 6px; margin: 0 3px 0 1px; border-radius: 50%; background: var(--ui-rn-amber); }
.ui-rn__trigger > .ui-rn__chev { width: 12px; height: 12px; margin-left: -2px; color: var(--ui-rn-icon); transition: rotate 180ms var(--ui-rn-ease); }
.ui-rn[data-open] .ui-rn__trigger > .ui-rn__chev { rotate: 180deg; }

/* ----- The popover: a gray tray, the tasks on a white card in it ----- */
.ui-rn__panel {
  position: absolute;
  top: calc(100% + 6px);
  inset-inline-end: 0;
  z-index: 50;
  width: min(var(--ui-rn-w), calc(100vw - 32px));
  padding: 4px;
  border-radius: var(--aui-radius-lg, 16px);
  background: var(--ui-rn-tray);
  box-shadow: var(--ui-rn-pop);
  transform-origin: top right;
  animation: ui-rn-pop 180ms var(--ui-rn-ease) both;
}
.ui-rn[data-align="start"] .ui-rn__panel { inset-inline: 0 auto; transform-origin: top left; }
.ui-rn:dir(rtl) .ui-rn__panel { transform-origin: top left; }
.ui-rn[data-align="start"]:dir(rtl) .ui-rn__panel { transform-origin: top right; }
.ui-rn__panel[hidden] { display: none; }
.ui-rn__panel:focus { outline: none; }
.ui-rn__head { display: flex; align-items: center; gap: 6px; height: 32px; padding: 0 10px 2px; }
.ui-rn__label { font-size: 13px; font-weight: 500; line-height: 20px; color: var(--ui-rn-strong); }
.ui-rn__count { font-size: 12px; line-height: 16px; color: var(--ui-rn-muted); font-variant-numeric: tabular-nums; }
.ui-rn__body { padding: 4px; border-radius: calc(var(--aui-radius-lg, 16px) - 4px); background: var(--ui-rn-card); box-shadow: var(--ui-rn-shadow); }
.ui-rn__list { display: grid; grid-template-columns: minmax(0, 1fr); max-height: min(calc(var(--ui-rn-row) * 5.5), max(calc(var(--ui-rn-row) * 2.5), calc(100dvh - 200px))); overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; }
.ui-rn__empty { padding: 14px 8px 16px; font-size: 12px; line-height: 16px; color: var(--ui-rn-muted); }

/* One row – always 52px, whatever state it's in */
.ui-rn__row { position: relative; display: flex; align-items: center; flex: none; height: var(--ui-rn-row); border-radius: calc(var(--aui-radius-sm, 7px) + 1px); transition: background-color 120ms ease; }
.ui-rn__row:hover { background: var(--ui-rn-hover); }
.ui-rn__main { display: flex; flex: 1; align-items: center; gap: 10px; min-width: 0; height: 100%; padding: 0 6px 0 9px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); }
.ui-rn__main:focus-visible { background: var(--ui-rn-hover); outline: 2px solid var(--ui-rn-focus); outline-offset: -2px; }
.ui-rn__mark { flex: none; transition: color 400ms ease; }
.ui-rn__pie { transition: stroke-dasharray 700ms var(--ui-rn-ease); }
.ui-rn__text { display: grid; flex: 1; min-width: 0; }
.ui-rn__title { overflow: hidden; font-weight: 500; color: var(--ui-rn-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-rn__step { font-size: 12px; line-height: 16px; color: var(--ui-rn-muted); }
.ui-rn__row[data-state="done"] .ui-rn__title { color: var(--ui-rn-text); }
.ui-rn__steps { flex: none; font-size: 12px; line-height: 16px; color: var(--ui-rn-muted); font-variant-numeric: tabular-nums; }
.ui-rn__badge { flex: none; height: 20px; padding: 0 7px; border-radius: calc(var(--aui-radius-sm, 7px) - 2px); background: var(--ui-rn-badge-wash); font-size: 11.5px; font-weight: 500; line-height: 20px; color: var(--ui-rn-badge); white-space: nowrap; animation: ui-rn-in 260ms var(--ui-rn-ease) both; }
.ui-rn__act { display: flex; flex: none; justify-content: flex-end; min-width: 40px; padding-right: 6px; }
.ui-rn__btn { display: grid; place-items: center; width: 28px; height: 28px; border-radius: var(--aui-radius-sm, 7px); color: var(--ui-rn-icon); transition: background-color 120ms ease, color 120ms ease; }
.ui-rn__btn svg { width: 14px; height: 14px; }
button.ui-rn__btn:hover { background: var(--ui-rn-press); color: var(--ui-rn-ink); }
span.ui-rn__btn { color: var(--ui-rn-blue); }
.ui-rn__btn--text { width: auto; padding: 0 9px; background: var(--ui-rn-fill); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-rn-ink); animation: ui-rn-in 260ms var(--ui-rn-ease) both; }
button.ui-rn__btn--text:hover { background: var(--ui-rn-press); }

/* Text that swaps: stacked in one cell, the old line leaves upwards while the new one rises in */
.ui-rn__swap { display: inline-grid; min-width: 0; overflow: hidden; }
.ui-rn__swap > * { grid-area: 1 / 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-rn__swap > [data-fresh] { animation: ui-rn-swap-in 340ms var(--ui-rn-ease) both; }
.ui-rn__swap > [data-out] { animation: ui-rn-swap-out 340ms var(--ui-rn-ease) both; }

/* ----- Footer: on the tray, the way to the full list ----- */
.ui-rn__foot { padding: 4px 0 0; }
.ui-rn__all { display: flex; align-items: center; gap: 8px; width: 100%; height: 32px; padding: 0 9px 0 13px; border-radius: calc(var(--aui-radius-sm, 7px) + 3px); color: var(--ui-rn-text); transition: background-color 120ms ease, color 120ms ease; }
.ui-rn__all:hover { background: var(--ui-rn-tray-hover); color: var(--ui-rn-ink); }
.ui-rn__all > svg:first-child { width: 14px; height: 14px; color: var(--ui-rn-icon); }
.ui-rn__all > span { flex: 1; font-weight: 500; }
.ui-rn__all > .ui-rn__chev { width: 12px; height: 12px; color: var(--ui-rn-icon); transition: translate 180ms var(--ui-rn-ease); }
.ui-rn__all:hover > .ui-rn__chev { translate: 2px 0; }
.ui-rn:dir(rtl) .ui-rn__all > .ui-rn__chev { scale: -1 1; }
.ui-rn:dir(rtl) .ui-rn__all:hover > .ui-rn__chev { translate: -2px 0; }

@keyframes ui-rn-pop { from { opacity: 0; transform: translateY(-4px) scale(0.98); } }
@keyframes ui-rn-in { from { opacity: 0; transform: scale(0.92); } }
@keyframes ui-rn-swap-in { from { opacity: 0; transform: translateY(60%); } }
@keyframes ui-rn-swap-out { to { opacity: 0; transform: translateY(-60%); } }

/* Phones: with both counts (or long ones) the trigger keeps only the numbers – its label still reads the whole summary */
@media (max-width: 359px) {
  .ui-rn[data-both] .ui-rn__word { display: none; }
}
@media (max-width: 419px) {
  .ui-rn[data-both][data-long] .ui-rn__word { display: none; }
}
/* Touch screens: bigger targets */
@media (hover: none) and (pointer: coarse) {
  .ui-rn__trigger { height: 36px; }
  .ui-rn { --ui-rn-row: 56px; }
  .ui-rn__btn { width: 36px; height: 36px; }
  .ui-rn__btn--text { width: auto; }
  .ui-rn__all { height: 40px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-rn *, .ui-rn *::before, .ui-rn *::after { animation: none !important; transition: none !important; }
  .ui-rn__swap > [data-out] { visibility: hidden; }
}

`;
