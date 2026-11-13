/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";
import { t, useT } from "@/i18n";

/*
 * Running Now (Clean) – a live status of background agent tasks, in a single self-contained file.
 * Collapsed it is one quiet pill for a header or a sidebar – "3 running · 2 need you". Open, a popover lists the tasks:
 * a ring that fills as the task moves on, the title, the step it is on, "2/4", and Pause / Resume – a task that waits
 * on you says so with a small badge and an Open button instead.
 * Clean style: one white button and one white popover card with hairline shadows, no tray.
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

/* Steve: the default words come from Steve's i18n, built when shown so they follow the language */
const defaults = (): RunningNowLabels => ({
  title: t("core:affect.running.title"),
  running: t("core:affect.running.running"),
  needsYou: t("core:affect.running.needsYou"),
  needsYouOne: t("core:affect.running.needsYouOne"),
  needsYouBadge: t("core:affect.running.needsYouBadge"),
  paused: t("core:affect.running.paused"),
  pausedCount: t("core:affect.running.pausedCount"),
  done: t("core:affect.running.done"),
  pause: t("core:affect.running.pause"),
  resume: t("core:affect.running.resume"),
  open: t("core:affect.running.open"),
  openAll: t("core:affect.running.openAll"),
  empty: t("core:affect.running.empty"),
  idle: t("core:affect.running.idle"),
  stepsOf: t("core:affect.running.stepsOf"),
});

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
  t.state === "done" ? "var(--ui-rnc-done)" : t.state === "paused" ? "var(--ui-rnc-muted)" : t.state === "needs-you" ? "var(--ui-rnc-amber)" : "var(--ui-rnc-blue)";

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
  useT(); // Steve: the default words follow a change of language
  // A custom needsYou without its singular is used for one task too, so the words never mix
  const labels: RunningNowLabels = { ...defaults(), ...(labelsProp?.needsYou && !labelsProp.needsYouOne ? { needsYouOne: labelsProp.needsYou } : null), ...labelsProp };
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

  const rows = () => [...(listRef.current?.querySelectorAll<HTMLButtonElement>(".ui-rnc__main") ?? [])];
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
    const li = (e.target as HTMLElement).closest?.(".ui-rnc__row");
    const i = all.findIndex((b) => b.parentElement === li);
    if (i < 0) return;
    const next = e.key === "ArrowDown" ? Math.min(all.length - 1, i + 1) : e.key === "ArrowUp" ? Math.max(0, i - 1) : e.key === "Home" ? 0 : e.key === "End" ? all.length - 1 : null;
    if (next === null) return;
    e.preventDefault();
    all[next]?.focus();
  };

  const lastFocus = useRef<{ el: HTMLElement; id?: string; index: number } | null>(null);
  const onPanelFocus = (e: FocusEvent<HTMLDivElement>) => {
    const li = (e.target as HTMLElement).closest<HTMLElement>(".ui-rnc__row");
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
      <style href="ui-running-now-clean" precedence="default">
        {css}
      </style>
      <div
        ref={rootRef}
        className={["ui-rnc", className].filter(Boolean).join(" ")}
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
          className="ui-rnc__trigger"
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
                <span className="ui-rnc__part">
                  <span className="ui-rnc__ring" aria-hidden="true" />
                  <Swap className="ui-rnc__n" value={num.format(running)} />
                  <span className="ui-rnc__word">{labels.running}</span>
                </span>
              )}
              {running > 0 && waiting > 0 && (
                <span className="ui-rnc__sep" aria-hidden="true">
                  ·
                </span>
              )}
              {waiting > 0 && (
                <span className="ui-rnc__part">
                  <span className="ui-rnc__dot" aria-hidden="true" />
                  <Swap className="ui-rnc__n" value={num.format(waiting)} />
                  <span className="ui-rnc__word">{waitingWord}</span>
                </span>
              )}
            </>
          ) : paused > 0 ? (
            <span className="ui-rnc__part">
              <span className="ui-rnc__ring" data-idle="" aria-hidden="true" />
              <Swap className="ui-rnc__n" value={num.format(paused)} />
              <span className="ui-rnc__word">{labels.pausedCount}</span>
            </span>
          ) : (
            <span className="ui-rnc__part">
              <span className="ui-rnc__ring" data-idle="" aria-hidden="true" />
              <span className="ui-rnc__word">{labels.idle}</span>
            </span>
          )}
          <ChevronDown />
        </button>

        <div ref={panelRef} id={panelId} className="ui-rnc__panel" style={shift ? { translate: `${shift}px 0` } : undefined} role="dialog" aria-label={labels.title} tabIndex={-1} hidden={!open} onKeyDown={onPanelKey} onFocus={onPanelFocus}>
          <div className="ui-rnc__head">
            <span className="ui-rnc__label">{labels.title}</span>
            <span className="ui-rnc__count">{num.format(list.length)}</span>
          </div>
          <div className="ui-rnc__body">
            {list.length ? (
              <ul ref={listRef} className="ui-rnc__list">
                {list.map((t) => (
                  <Row key={t.id} task={t} labels={labels} onPause={onPause} onResume={onResume} onOpen={onOpen} />
                ))}
              </ul>
            ) : (
              <p className="ui-rnc__empty">{labels.empty}</p>
            )}
          </div>
          {onOpenAll && (
            <div className="ui-rnc__foot">
              <button type="button" className="ui-rnc__all" onClick={onOpenAll}>
                <TasksIcon />
                <span>{labels.openAll}</span>
                <ChevronRight />
              </button>
            </div>
          )}
        </div>

        <span className="ui-rnc__sr" role="status" aria-live="polite">
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
    <li className="ui-rnc__row" data-state={t.state} data-id={t.id}>
      <button
        type="button"
        className="ui-rnc__main"
        aria-label={`${t.title} – ${state}. ${t.state === "done" || !t.step ? "" : `${t.step}. `}${total ? labels.stepsOf.replace("{done}", num.format(done)).replace("{total}", num.format(total)) : ""}`}
        onClick={() => onOpen?.(t.id)}
      >
        <Ring value={value} color={progressColor(t)} />
        <span className="ui-rnc__text">
          <span className="ui-rnc__title" dir="auto" title={t.title}>
            {t.title}
          </span>
          <Swap className="ui-rnc__step" value={step} />
        </span>
        {t.state === "needs-you" ? (
          <span className="ui-rnc__badge">{labels.needsYouBadge}</span>
        ) : total > 0 ? (
          <span className="ui-rnc__steps">
            {num.format(done)}/{num.format(total)}
          </span>
        ) : null}
      </button>
      <span className="ui-rnc__act">
        {t.state === "needs-you" || (t.state === "paused" && t.resumable === false) ? (
          <button type="button" className="ui-rnc__btn ui-rnc__btn--text" aria-label={`${labels.open} ${t.title} – ${(t.state === "paused" ? labels.paused : labels.needsYouBadge).toLowerCase()}`} onClick={() => onOpen?.(t.id)}>
            {labels.open}
          </button>
        ) : t.state === "running" && onPause ? (
          <button type="button" className="ui-rnc__btn" aria-label={`${labels.pause} ${t.title}`} title={labels.pause} onClick={() => onPause?.(t.id)}>
            <PauseIcon />
          </button>
        ) : t.state === "paused" && onResume ? (
          <button type="button" className="ui-rnc__btn" aria-label={`${labels.resume} ${t.title}`} title={labels.resume} onClick={() => onResume?.(t.id)}>
            <PlayIcon />
          </button>
        ) : t.state === "done" ? (
          <span className="ui-rnc__btn" aria-hidden="true">
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
    <svg className="ui-rnc__mark" viewBox="0 0 14 14" width="14" height="14" aria-hidden="true" style={{ color }}>
      <circle cx="7" cy="7" r="6" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle className="ui-rnc__pie" cx="7" cy="7" r={r} fill="none" stroke="currentColor" strokeWidth={r * 2} style={{ strokeDasharray: `${c * value} ${c}` } as CSSProperties} transform="rotate(-90 7 7)" />
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
    <span className={["ui-rnc__swap", className].filter(Boolean).join(" ")}>
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
  <svg className="ui-rnc__chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M18 9C18 9 13.58 15 12 15C10.42 15 6 9 6 9" />
  </svg>
);
const ChevronRight = () => (
  <svg className="ui-rnc__chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M9 6C9 6 15 10.42 15 12C15 13.58 9 18 9 18" />
  </svg>
);

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-rnc */
.ui-rnc {
  /* Steve: colors from Steve's tokens, which switch between light and dark themselves */
  --ui-rnc-card: var(--aui-overlay, #ffffff);
  --ui-rnc-strong: var(--aui-ink, #000000);
  --ui-rnc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-rnc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-rnc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-rnc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-rnc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-rnc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-rnc-hover: var(--aui-subtle, rgba(0, 0, 0, 0.04));
  --ui-rnc-press: color-mix(in srgb, var(--aui-ink, #171717) 9%, transparent);
  --ui-rnc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-rnc-amber: var(--aui-amber, #e8b000);
  --ui-rnc-blue: var(--aui-blue, #0190fe);
  --ui-rnc-done: var(--aui-success, #1e8a4f);
  --ui-rnc-badge: var(--aui-warning, #8f6c00);
  --ui-rnc-badge-wash: var(--aui-warning-soft, #f9f1d6);
  /* A 1px edge and a soft lift; the popover floats a little higher */
  --ui-rnc-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-rnc-line), 0 1px 2px rgba(0, 0, 0, 0.04));
  --ui-rnc-pop: var(--aui-shadow-popover, 0 0 0 1px var(--ui-rnc-line), 0 1px 2px rgba(0, 0, 0, 0.04), 0 14px 32px -12px rgba(0, 0, 0, 0.18));
  --ui-rnc-ease: var(--aui-ease, cubic-bezier(0.32, 0.72, 0.24, 1));
  /* How wide the popover is */
  --ui-rnc-w: 340px;
  /* One task row */
  --ui-rnc-row: 52px;
  position: relative;
  display: inline-block;
  flex: none;
  box-sizing: border-box;
  max-width: 100%;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-rnc-ink);
  text-align: start;
  -webkit-font-smoothing: antialiased;
}
.ui-rnc *, .ui-rnc *::before, .ui-rnc *::after { box-sizing: border-box; }
:where(.ui-rnc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-rnc button:focus-visible { outline: 2px solid var(--ui-rnc-focus); outline-offset: 2px; }
.ui-rnc :is(p, ul) { margin: 0; padding: 0; list-style: none; }
.ui-rnc svg { display: block; flex: none; }
.ui-rnc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* ----- The trigger: one white button, the counts in it ----- */
.ui-rnc__trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 100%;
  height: 28px;
  padding: 0 8px 0 10px;
  border-radius: calc(var(--aui-radius, 7px) - 1px);
  background: var(--aui-surface, var(--ui-rnc-card));
  box-shadow: var(--ui-rnc-shadow);
  font-size: 12px;
  line-height: 16px;
  color: var(--ui-rnc-text);
  white-space: nowrap;
  transition: background-color 120ms ease;
}
.ui-rnc__trigger:hover { background: color-mix(in srgb, var(--ui-rnc-card), #000 2%); }
.ui-rnc__part { display: inline-flex; flex: none; align-items: center; gap: 4px; }
.ui-rnc__n { flex: none; font-weight: 500; color: var(--ui-rnc-ink); font-variant-numeric: tabular-nums; }
.ui-rnc__sep { flex: none; color: var(--aui-placeholder, var(--ui-rnc-faint)); }
.ui-rnc__ring { flex: none; width: 8px; height: 8px; margin-right: 2px; border-radius: 50%; box-shadow: inset 0 0 0 1.5px var(--ui-rnc-blue); }
.ui-rnc__ring[data-idle] { box-shadow: inset 0 0 0 1.5px var(--ui-rnc-faint); }
.ui-rnc__dot { flex: none; width: 6px; height: 6px; margin: 0 3px 0 1px; border-radius: 50%; background: var(--ui-rnc-amber); }
.ui-rnc__trigger > .ui-rnc__chev { width: 12px; height: 12px; margin-left: -2px; color: var(--ui-rnc-icon); transition: rotate 180ms var(--ui-rnc-ease); }
.ui-rnc[data-open] .ui-rnc__trigger > .ui-rnc__chev { rotate: 180deg; }

/* ----- The popover: one white card under the trigger ----- */
.ui-rnc__panel {
  position: absolute;
  top: calc(100% + 6px);
  inset-inline-end: 0;
  z-index: 50;
  width: min(var(--ui-rnc-w), calc(100vw - 32px));
  padding: 4px;
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--aui-overlay, var(--ui-rnc-card));
  box-shadow: var(--ui-rnc-pop);
  transform-origin: top right;
  animation: ui-rnc-pop 180ms var(--ui-rnc-ease) both;
}
.ui-rnc[data-align="start"] .ui-rnc__panel { inset-inline: 0 auto; transform-origin: top left; }
.ui-rnc:dir(rtl) .ui-rnc__panel { transform-origin: top left; }
.ui-rnc[data-align="start"]:dir(rtl) .ui-rnc__panel { transform-origin: top right; }
.ui-rnc__panel[hidden] { display: none; }
.ui-rnc__panel:focus { outline: none; }
.ui-rnc__head { display: flex; align-items: center; gap: 6px; height: 28px; padding: 0 8px; }
.ui-rnc__label { font-size: 10px; font-weight: 500; line-height: 16px; letter-spacing: 0.5px; text-transform: uppercase; color: var(--ui-rnc-muted); }
.ui-rnc__count { font-size: 10px; font-weight: 500; line-height: 16px; color: var(--aui-placeholder, var(--ui-rnc-faint)); font-variant-numeric: tabular-nums; }
.ui-rnc__body { display: grid; grid-template-columns: minmax(0, 1fr); }
.ui-rnc__list { display: grid; grid-template-columns: minmax(0, 1fr); max-height: min(calc(var(--ui-rnc-row) * 5.5), max(calc(var(--ui-rnc-row) * 2.5), calc(100dvh - 200px))); overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; }
.ui-rnc__empty { padding: 14px 8px 16px; font-size: 12px; line-height: 16px; color: var(--ui-rnc-muted); }

/* One row – always 52px, whatever state it's in */
.ui-rnc__row { position: relative; display: flex; align-items: center; flex: none; height: var(--ui-rnc-row); border-radius: calc(var(--aui-radius-sm, 5px) + 1px); transition: background-color 120ms ease; }
.ui-rnc__row:hover { background: var(--ui-rnc-hover); }
.ui-rnc__main { display: flex; flex: 1; align-items: center; gap: 10px; min-width: 0; height: 100%; padding: 0 6px 0 9px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); }
.ui-rnc__main:focus-visible { background: var(--ui-rnc-hover); outline: 2px solid var(--ui-rnc-focus); outline-offset: -2px; }
.ui-rnc__mark { flex: none; transition: color 400ms ease; }
.ui-rnc__pie { transition: stroke-dasharray 700ms var(--ui-rnc-ease); }
.ui-rnc__text { display: grid; flex: 1; min-width: 0; }
.ui-rnc__title { overflow: hidden; font-weight: 500; color: var(--ui-rnc-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-rnc__step { font-size: 12px; line-height: 16px; color: var(--ui-rnc-muted); }
.ui-rnc__row[data-state="done"] .ui-rnc__title { color: var(--ui-rnc-text); }
.ui-rnc__steps { flex: none; font-size: 12px; line-height: 16px; color: var(--ui-rnc-muted); font-variant-numeric: tabular-nums; }
.ui-rnc__badge { flex: none; height: 18px; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-rnc-badge-wash); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-rnc-badge); white-space: nowrap; animation: ui-rnc-in 260ms var(--ui-rnc-ease) both; }
.ui-rnc__act { display: flex; flex: none; justify-content: flex-end; min-width: 40px; padding-right: 6px; }
.ui-rnc__btn { display: grid; place-items: center; width: 28px; height: 28px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-rnc-icon); transition: background-color 120ms ease, color 120ms ease; }
.ui-rnc__btn svg { width: 14px; height: 14px; }
button.ui-rnc__btn:hover { background: var(--ui-rnc-press); color: var(--ui-rnc-ink); }
span.ui-rnc__btn { color: var(--ui-rnc-blue); }
.ui-rnc__btn--text { width: auto; padding: 0 8px; background: var(--aui-surface, var(--ui-rnc-card)); box-shadow: var(--ui-rnc-shadow); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-rnc-ink); animation: ui-rnc-in 260ms var(--ui-rnc-ease) both; }
button.ui-rnc__btn--text:hover { background: color-mix(in srgb, var(--ui-rnc-card), #000 3%); }

/* Text that swaps: stacked in one cell, the old line leaves upwards while the new one rises in */
.ui-rnc__swap { display: inline-grid; min-width: 0; overflow: hidden; }
.ui-rnc__swap > * { grid-area: 1 / 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-rnc__swap > [data-fresh] { animation: ui-rnc-swap-in 340ms var(--ui-rnc-ease) both; }
.ui-rnc__swap > [data-out] { animation: ui-rnc-swap-out 340ms var(--ui-rnc-ease) both; }

/* ----- Footer: the way to the full list ----- */
.ui-rnc__foot { margin: 4px -4px -4px; padding: 4px; border-top: 0.5px solid var(--ui-rnc-line); }
.ui-rnc__all { display: flex; align-items: center; gap: 8px; width: 100%; height: 32px; padding: 0 8px 0 9px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-rnc-ink); transition: background-color 120ms ease; }
.ui-rnc__all:hover { background: var(--ui-rnc-hover); }
.ui-rnc__all > svg:first-child { width: 14px; height: 14px; color: var(--ui-rnc-icon); }
.ui-rnc__all > span { flex: 1; }
.ui-rnc__all > .ui-rnc__chev { width: 12px; height: 12px; color: var(--ui-rnc-icon); transition: translate 180ms var(--ui-rnc-ease); }
.ui-rnc__all:hover > .ui-rnc__chev { translate: 2px 0; }
.ui-rnc:dir(rtl) .ui-rnc__all > .ui-rnc__chev { scale: -1 1; }
.ui-rnc:dir(rtl) .ui-rnc__all:hover > .ui-rnc__chev { translate: -2px 0; }

@keyframes ui-rnc-pop { from { opacity: 0; transform: translateY(-4px) scale(0.98); } }
@keyframes ui-rnc-in { from { opacity: 0; transform: scale(0.92); } }
@keyframes ui-rnc-swap-in { from { opacity: 0; transform: translateY(60%); } }
@keyframes ui-rnc-swap-out { to { opacity: 0; transform: translateY(-60%); } }

/* Phones: with both counts (or long ones) the trigger keeps only the numbers – its label still reads the whole summary */
@media (max-width: 359px) {
  .ui-rnc[data-both] .ui-rnc__word { display: none; }
}
@media (max-width: 419px) {
  .ui-rnc[data-both][data-long] .ui-rnc__word { display: none; }
}
/* Touch screens: bigger targets */
@media (hover: none) and (pointer: coarse) {
  .ui-rnc__trigger { height: 36px; }
  .ui-rnc { --ui-rnc-row: 56px; }
  .ui-rnc__btn { width: 36px; height: 36px; }
  .ui-rnc__btn--text { width: auto; }
  .ui-rnc__all { height: 40px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-rnc *, .ui-rnc *::before, .ui-rnc *::after { animation: none !important; transition: none !important; }
  .ui-rnc__swap > [data-out] { visibility: hidden; }
}

/* Steve: dark mode comes with the tokens above; only the hover lifts are dark-specific */
:where(.dark, [data-theme="dark"]) .ui-rnc__trigger:hover { background: color-mix(in srgb, var(--ui-rnc-card), #fff 4%); }
:where(.dark, [data-theme="dark"]) button.ui-rnc__btn--text:hover { background: color-mix(in srgb, var(--ui-rnc-card), #fff 5%); }
`;
