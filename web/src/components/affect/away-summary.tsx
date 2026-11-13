/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";

/*
 * Away Summary – the "While you were away" card for a chat thread, in a single self-contained file.
 * It sits inline in the conversation, right where you left off:
 *   · a head with how long you were gone ("2h 14m", or "since 9:40") and quiet counts – 3 done · 2 need you · 1 failed
 *   · a short list, what needs you first: a small status mark, the task, one gray line of what happened, one action
 *   · at most `limit` rows, the rest behind "Show 3 more"
 *   · "Got it" folds it into a one-line divider – "You were away 2h 14m · 6 updates" – that opens it again
 *   · rows keep their place and size when something changes: a decided item turns done in place, the counts follow
 * The styles are injected below, so no extra files are needed.
 */

export type AwayKind = "done" | "needs-you" | "failed";

export type AwayItem = {
  id: string;
  /** The task, e.g. "Ingest pipeline" */
  title: string;
  kind: AwayKind;
  /** One short line of what happened – "Opened PR #482", "Waiting on: Which queue?", "Failed: rate limit on Stripe" */
  detail?: string;
  /** The row's action – defaults to Decide, Open or Retry by kind; false leaves the row without one */
  action?: string | false;
};

export type AwaySummaryLabels = {
  title: string;
  /** "since 9:40" with timeStyle="clock" */
  since: (time: string) => string;
  done: (n: number) => string;
  needsYou: (n: number) => string;
  failed: (n: number) => string;
  /** The default action per kind */
  action: Record<AwayKind, string>;
  showMore: (n: number) => string;
  showLess: string;
  gotIt: string;
  /** The folded divider – `away` is the duration or "since 9:40" */
  collapsed: (away: string, updates: number) => string;
  empty: string;
};

export type AwaySummaryProps = {
  /** When you left – a Date, ms or an ISO string */
  since: Date | number | string;
  items: AwayItem[];
  /** A row's action was pressed. Return a promise to show the button working until it settles */
  onAction?: (id: string, item: AwayItem) => void | Promise<unknown>;
  /** "Got it" was pressed – store that the person has seen it */
  onDismiss?: () => void;
  /** Start folded into the divider */
  defaultCollapsed?: boolean;
  /** Folded or open, if you control it – pair with onCollapsedChange */
  collapsed?: boolean;
  onCollapsedChange?: (collapsed: boolean) => void;
  /** Rows shown before "Show N more" */
  limit?: number;
  /** "duration" reads "2h 14m", "clock" reads "since 9:40" */
  timeStyle?: "duration" | "clock";
  /** Now – defaults to the clock, updated every half minute */
  now?: Date | number;
  locale?: string;
  labels?: Partial<AwaySummaryLabels>;
  className?: string;
};

/* ---------- Helpers ---------- */

const LABELS: AwaySummaryLabels = {
  title: "While you were away",
  since: (t) => `since ${t}`,
  done: (n) => `${n} done`,
  needsYou: (n) => `${n} need${n === 1 ? "s" : ""} you`,
  failed: (n) => `${n} failed`,
  action: { "needs-you": "Decide", done: "Open", failed: "Retry" },
  showMore: (n) => `Show ${n} more`,
  showLess: "Show less",
  gotIt: "Got it",
  collapsed: (away, n) => `You were away ${away} · ${n} update${n === 1 ? "" : "s"}`,
  empty: "Nothing happened while you were away",
};
/** What needs you first, then what failed, then what's done */
const RANK: Record<AwayKind, number> = { "needs-you": 0, failed: 1, done: 2 };
const STATE: Record<AwayKind, string> = { "needs-you": "needs you", failed: "failed", done: "done" };

const toMs = (v: Date | number | string) => {
  const t = v instanceof Date ? v.getTime() : typeof v === "number" ? v : new Date(v).getTime();
  // A date that can't be read counts as now – "a moment", never "NaNd" or a thrown RangeError
  return Number.isFinite(t) ? t : Date.now();
};
/** 42m · 2h 14m · 2h · 1d 3h */
const duration = (ms: number) => {
  const m = Math.max(0, Math.floor(ms / 60_000));
  if (m < 1) return "a moment";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  return h % 24 ? `${Math.floor(h / 24)}d ${h % 24}h` : `${Math.floor(h / 24)}d`;
};
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/* ---------- Marks and icons: Hugeicons Free (MIT, © Hugeicons, hugeicons.com) for the chevrons and tick ---------- */

function Mark({ kind }: { kind: AwayKind }) {
  if (kind === "done")
    return (
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
        <circle cx="7" cy="7" r="6" fill="var(--ui-aw-done)" />
        <path className="ui-aw__tick" d="M4.3 7.2 6.1 9 9.7 5.2" fill="none" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" pathLength={1} />
      </svg>
    );
  if (kind === "failed")
    return (
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
        <circle cx="7" cy="7" r="6" fill="var(--ui-aw-failed)" />
        <path d="M7 4v3.4" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" />
        <circle cx="7" cy="9.8" r="0.85" fill="#fff" />
      </svg>
    );
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
      <circle className="ui-aw__halo" cx="7" cy="7" r="5.25" fill="var(--ui-aw-needs)" />
      <circle cx="7" cy="7" r="5.25" fill="none" stroke="var(--ui-aw-needs)" strokeWidth="1.5" />
      <circle cx="7" cy="7" r="2.25" fill="var(--ui-aw-needs)" />
    </svg>
  );
}
const Chevron = ({ up }: { up?: boolean }) => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true" style={up ? { transform: "rotate(180deg)" } : undefined}>
    <path d="M18 9.00005C18 9.00005 13.5811 15 12 15C10.4188 15 6 9 6 9" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </svg>
);
const Tick = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M5 14L8.5 17.5L19 6.5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </svg>
);

/* ---------- The component ---------- */

export function AwaySummary({
  since,
  items,
  onAction,
  onDismiss,
  defaultCollapsed = false,
  collapsed: controlled,
  onCollapsedChange,
  limit = 5,
  timeStyle = "duration",
  now: givenNow,
  locale,
  labels: given,
  className,
}: AwaySummaryProps) {
  const L = { ...LABELS, ...given, action: { ...LABELS.action, ...given?.action } };
  const uid = useId();
  const cardId = `${uid}-card`;

  /* ----- Folded or open ----- */
  const [own, setOwn] = useState(defaultCollapsed);
  const collapsed = controlled ?? own;
  // Open and done sliding: the card's edge and lift may reach past its box again
  const [settled, setSettled] = useState(true);
  const lastCollapsed = useRef(collapsed);
  if (lastCollapsed.current !== collapsed) {
    lastCollapsed.current = collapsed;
    if (settled) setSettled(false);
  }
  useEffect(() => {
    if (settled || !reduced()) return;
    setSettled(true);
  }, [settled]);
  const rootRef = useRef<HTMLElement>(null);
  const setCollapsed = (next: boolean) => {
    if (controlled === undefined) setOwn(next);
    onCollapsedChange?.(next);
    if (next) onDismiss?.();
    // The button that was pressed goes away – keepFocus below moves the focus to the one that takes its place
  };

  /* ----- How long you were gone ----- */
  const [clock, setClock] = useState(() => Date.now());
  useEffect(() => {
    if (givenNow !== undefined) return;
    const t = window.setInterval(() => setClock(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, [givenNow]);
  const now = givenNow === undefined ? clock : toMs(givenNow);
  const sinceMs = toMs(since);
  const away =
    timeStyle === "clock"
      ? L.since(new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit" }).format(sinceMs))
      : duration(now - sinceMs);

  /* ----- Order: what needs you first – and a row keeps its place when its kind changes, so nothing jumps ----- */
  const ranks = useRef(new Map<string, number>());
  const counter = useRef(0);
  for (const it of items) if (!ranks.current.has(it.id)) ranks.current.set(it.id, RANK[it.kind] * 1e6 + counter.current++);
  const sorted = [...items].sort((a, b) => ranks.current.get(a.id)! - ranks.current.get(b.id)!);
  const [all, setAll] = useState(false);
  // The first `limit` rows always show; the rest sit in a fold that grows open with Show more
  const first = sorted.slice(0, limit);
  const rest = sorted.slice(limit);
  const hidden = all ? 0 : rest.length;

  const n = { "needs-you": 0, failed: 0, done: 0 } as Record<AwayKind, number>;
  for (const it of items) n[it.kind]++;
  const counts = (
    [
      ["needs-you", L.needsYou],
      ["failed", L.failed],
      ["done", L.done],
    ] as const
  ).filter(([k]) => n[k] > 0);

  /* ----- Working: an action that returned a promise ----- */
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const act = (it: AwayItem) => {
    if (busy.has(it.id)) return;
    const r = onAction?.(it.id, it);
    if (!r || typeof (r as Promise<unknown>).then !== "function") return;
    setBusy((s) => new Set(s).add(it.id));
    const clear = () =>
      setBusy((s) => {
        const next = new Set(s);
        next.delete(it.id);
        return next;
      });
    (r as Promise<unknown>).then(clear, clear);
  };

  /* ----- Said politely when an item changes: "Ingest pipeline: done. 1 needs you" ----- */
  const [announce, setAnnounce] = useState("");
  const kinds = useRef<Map<string, AwayKind> | null>(null);
  useEffect(() => {
    const before = kinds.current;
    kinds.current = new Map(items.map((it) => [it.id, it.kind]));
    if (!before) return;
    const changed = items.filter((it) => before.has(it.id) && before.get(it.id) !== it.kind);
    const added = items.filter((it) => !before.has(it.id));
    if (!changed.length && !added.length) return;
    const what = [...changed, ...added].map((it) => `${it.title}: ${STATE[it.kind]}`).join(". ");
    setAnnounce(`${what}. ${n["needs-you"] ? L.needsYou(n["needs-you"]) : "Nothing needs you"}`);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items]);

  /* ----- Focus never drops to the page: folding, opening, a row losing its button – it moves to what takes its place ----- */
  const lastFocus = useRef<{ el: HTMLElement; row: string | null } | null>(null);
  // The row whose action had the focus when the card folded – opening again returns there
  const foldRow = useRef<string | null>(null);
  const usable = (el: Element | null): el is HTMLElement => el instanceof HTMLElement && el.isConnected && !el.closest("[inert]");
  const nextFocus = (from: { el: HTMLElement; row: string | null }) => {
    const root = rootRef.current;
    if (!root) return null;
    const q = (s: string) => root.querySelector<HTMLElement>(s);
    if (collapsed) {
      foldRow.current = from.el.classList.contains("ui-aw__act") ? from.row : null;
      return q(".ui-aw__divider-btn");
    }
    const acts = [...root.querySelectorAll<HTMLElement>(".ui-aw__act")].filter(usable);
    const rowId = from.el.classList.contains("ui-aw__divider-btn") ? foldRow.current : from.row;
    if (rowId !== null) {
      const li = [...root.querySelectorAll<HTMLElement>("[data-row]")].find((x) => x.dataset.row === rowId);
      if (usable(li ?? null)) {
        // Its own action, else the next row's, else the one before
        const after = acts.find((a) => li!.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING);
        const before = acts.filter((a) => li!.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_PRECEDING).pop();
        if (after || before) return (after ?? before)!;
      } else if (usable(q(".ui-aw__more"))) return q(".ui-aw__more");
    }
    return acts[0] ?? q(".ui-aw__done-btn");
  };
  const keepFocus = () => {
    const from = lastFocus.current;
    const root = rootRef.current;
    if (!from || !root || usable(from.el)) return;
    const active = document.activeElement;
    // The focus already went somewhere else on purpose
    if (active && active !== document.body && active !== from.el) {
      if (!root.contains(active)) lastFocus.current = null;
      return;
    }
    nextFocus(from)?.focus({ preventScroll: true });
  };
  useLayoutEffect(() => {
    keepFocus();
    const f = requestAnimationFrame(keepFocus);
    return () => cancelAnimationFrame(f);
  });

  // Rows that come in with Show more fade in; the first paint just appears
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(!reduced()), []);

  const row = (it: AwayItem) => {
    const label = it.action === false ? null : (it.action ?? L.action[it.kind]);
    const working = busy.has(it.id);
    return (
      <li key={it.id} className="ui-aw__row" data-kind={it.kind} data-row={it.id} data-detail={it.detail ? "" : undefined}>
        <span key={it.kind} className="ui-aw__mark" data-pop={mounted ? "" : undefined}>
          <Mark kind={it.kind} />
          <span className="ui-aw__sr">{STATE[it.kind]}: </span>
        </span>
        <span className="ui-aw__text">
          <span className="ui-aw__name" dir="auto" title={it.title}>
            {it.title}
          </span>
          {it.detail && (
            <span key={it.detail} className="ui-aw__detail" dir="auto" title={it.detail} data-pop={mounted ? "" : undefined}>
              {it.detail}
            </span>
          )}
        </span>
        {label && (
          <button
            type="button"
            className="ui-aw__act"
            data-kind={it.kind}
            aria-label={`${label}: ${it.title}`}
            title={typeof it.action === "string" ? label : undefined}
            aria-busy={working || undefined}
            // Not disabled – a disabled button drops the keyboard focus to the page while it works
            aria-disabled={working || undefined}
            onClick={() => act(it)}
          >
            <span className="ui-aw__act-label">{label}</span>
            {working && <span className="ui-aw__spin" aria-hidden="true" />}
          </button>
        )}
      </li>
    );
                  };

  const summary = counts.map(([k, f]) => f(n[k])).join(", ");

  return (
    <section
      ref={rootRef}
      className={["ui-aw", className].filter(Boolean).join(" ")}
      aria-label={L.title}
      data-collapsed={collapsed ? "" : undefined}
      onFocus={(e) => {
        const el = e.target as HTMLElement;
        lastFocus.current = { el, row: el.closest<HTMLElement>("[data-row]")?.dataset.row ?? null };
      }}
      onBlur={(e) => {
        const from = lastFocus.current;
        if (e.relatedTarget && !e.currentTarget.contains(e.relatedTarget as Node)) lastFocus.current = null;
        // Clicked away onto the page: the button is still there, so this wasn't the card taking it away
        else if (!e.relatedTarget && from && usable(from.el))
          requestAnimationFrame(() => {
            if (lastFocus.current === from && document.activeElement === document.body) lastFocus.current = null;
          });
      }}
    >
      <style href="ui-away-summary" precedence="default">
        {css}
      </style>
      <span className="ui-aw__sr" role="status" aria-live="polite">
        {announce}
      </span>

      {/* Folded: one line in the thread that opens it again */}
      <div className="ui-aw__fold" data-open={collapsed ? "" : undefined}>
        <div className="ui-aw__fold-in" inert={!collapsed}>
          <div className="ui-aw__divider">
            <span className="ui-aw__rule" aria-hidden="true" />
            <button type="button" className="ui-aw__divider-btn" aria-expanded={false} aria-controls={cardId} onClick={() => setCollapsed(false)}>
              {n["needs-you"] > 0 && <span className="ui-aw__dot" data-kind="needs-you" aria-hidden="true" />}
              <span className="ui-aw__divider-text" title={L.collapsed(away, items.length)}>
                {L.collapsed(away, items.length)}
              </span>
              {n["needs-you"] > 0 && <span className="ui-aw__sr">, {L.needsYou(n["needs-you"])}</span>}
              <Chevron />
            </button>
            <span className="ui-aw__rule" aria-hidden="true" />
          </div>
        </div>
      </div>

      {/* Open: the card */}
      <div
        className="ui-aw__fold"
        data-open={collapsed ? undefined : ""}
        data-settled={settled && !collapsed ? "" : undefined}
        onTransitionEnd={(e) => {
          if (e.target === e.currentTarget && e.propertyName === "grid-template-rows") setSettled(true);
        }}
      >
        <div className="ui-aw__fold-in" inert={collapsed}>
          <div
            id={cardId}
            className="ui-aw__card"
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                setCollapsed(true);
              }
            }}
          >
            <header className="ui-aw__head">
              <h3 className="ui-aw__title">
                {L.title}
                <span className="ui-aw__when">
                  <span aria-hidden="true"> · </span>
                  <span className="ui-aw__sr">, </span>
                  {away}
                </span>
              </h3>
              {counts.length > 0 && (
                <p className="ui-aw__counts" aria-label={summary}>
                  {counts.map(([k, f], i) => (
                    <span key={k} className="ui-aw__count" aria-hidden="true">
                      {i > 0 && <span className="ui-aw__sep">·</span>}
                      <span className="ui-aw__dot" data-kind={k} />
                      <span key={n[k]} className="ui-aw__n">
                        {f(n[k])}
                      </span>
                    </span>
                  ))}
                </p>
              )}
            </header>

            {items.length === 0 ? (
              <p className="ui-aw__empty">{L.empty}</p>
            ) : (
              <div className="ui-aw__rows">
                <ul className="ui-aw__list">
                  {first.map(row)}
                </ul>
                {rest.length > 0 && (
                  <div className="ui-aw__extra" data-open={all ? "" : undefined}>
                    <div className="ui-aw__extra-in" inert={!all}>
                      <ul className="ui-aw__list">{rest.map(row)}</ul>
                    </div>
                  </div>
                )}
              </div>
            )}

            <footer className="ui-aw__foot">
              {(hidden > 0 || all) && sorted.length > limit && (
                <button type="button" className="ui-aw__more" aria-expanded={all} onClick={() => {
                    setAll(!all);
                    // Show more: the focus goes on to the first row that came in, so Tab reads on from there; Show less keeps it here
                    if (!all) requestAnimationFrame(() => rootRef.current?.querySelector<HTMLElement>(".ui-aw__extra .ui-aw__act")?.focus());
                  }}
                >
                  {all ? L.showLess : L.showMore(hidden)}
                  <Chevron up={all} />
                </button>
              )}
              <button type="button" className="ui-aw__done-btn" aria-expanded={true} aria-controls={cardId} onClick={() => setCollapsed(true)}>
                <Tick />
                {L.gotIt}
              </button>
            </footer>
          </div>
        </div>
      </div>
    </section>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and sizes as variables – override them on .ui-aw */
.ui-aw {
  --ui-aw-tray: var(--aui-subtle, #f0f0f0);
  --ui-aw-card: var(--aui-surface, #ffffff);
  --ui-aw-strong: var(--aui-ink, #171717);
  --ui-aw-ink: var(--aui-ink, #171717);
  --ui-aw-text: var(--aui-muted, #6b6b6b);
  --ui-aw-muted: var(--aui-muted, #8a8a8a);
  --ui-aw-faint: var(--aui-placeholder, #b8b8b8);
  --ui-aw-line: var(--aui-border, #ececec);
  --ui-aw-rule: var(--aui-border, #e2e2e2);
  --ui-aw-fill: var(--aui-border, #ebebeb);
  --ui-aw-hover: var(--aui-subtle, #f7f7f7);
  --ui-aw-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-aw-needs: var(--aui-amber, #e8b000);
  --ui-aw-done: var(--aui-success, #0190fe);
  --ui-aw-failed: var(--aui-red, #f04438);
  /* A 1px edge and a soft lift */
  --ui-aw-shadow: 0 0 0 1px var(--ui-aw-line), 0 1px 2px rgba(0, 0, 0, 0.04);
  --ui-aw-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  position: relative;
  box-sizing: border-box;
  width: 100%;
  container: ui-aw / inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-aw-text);
  text-align: start;
  -webkit-font-smoothing: antialiased;
}
.ui-aw *, .ui-aw *::before, .ui-aw *::after { box-sizing: border-box; }
:where(.ui-aw) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-aw button:focus-visible { outline: 2px solid var(--ui-aw-focus); outline-offset: 2px; }
.ui-aw :is(p, h3, ul) { margin: 0; }
.ui-aw ul { padding: 0; list-style: none; }
.ui-aw svg { display: block; flex: none; }
.ui-aw__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* ----- Folding: the card and the divider swap, the height follows ----- */
.ui-aw__fold { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 320ms var(--ui-aw-ease), opacity 220ms ease; }
.ui-aw__fold[data-open] { grid-template-rows: 1fr; opacity: 1; }
.ui-aw__fold-in { min-height: 0; overflow: hidden; }
/* Open and still: nothing clips the card's hairline and lift */
.ui-aw__fold[data-settled] > .ui-aw__fold-in { overflow: visible; }

/* ----- The folded line ----- */
.ui-aw__divider { display: flex; align-items: center; gap: 8px; min-width: 0; }
/* The lines give way first, so a narrow divider keeps its words */
.ui-aw__rule { flex: 1 1 0; min-width: 8px; height: 0.5px; background: var(--ui-aw-rule); }
.ui-aw__divider-btn { display: inline-flex; flex: 0 1 auto; align-items: center; gap: 6px; min-width: 0; height: 28px; padding-inline: 10px 8px; border-radius: 14px; font-size: 12px; line-height: 16px; color: var(--ui-aw-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-aw__divider-btn { background: var(--ui-aw-tray); }
.ui-aw__divider-btn:hover { background: var(--ui-aw-fill); color: var(--ui-aw-ink); }
.ui-aw__divider-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
/* The fold clips anything outside the line – the ring sits inside the pill */
.ui-aw .ui-aw__divider-btn:focus-visible { outline-offset: -2px; }
.ui-aw__divider-btn > svg { color: var(--ui-aw-faint); }

/* ----- The card ----- */
/* A gray tray: the head and the buttons sit on it, the rows on a white card inside */
.ui-aw__card { display: grid; grid-template-columns: minmax(0, 1fr); padding: 4px; border-radius: var(--aui-radius-lg, 16px); background: var(--ui-aw-tray); }
.ui-aw__head { display: flex; flex-wrap: wrap; align-items: baseline; column-gap: 12px; row-gap: 2px; min-width: 0; padding: 6px 10px 8px; }
.ui-aw__title { flex: 1 1 auto; min-width: 0; overflow: hidden; font-size: 13px; font-weight: 500; line-height: 20px; color: var(--ui-aw-strong); white-space: nowrap; text-overflow: ellipsis; }
.ui-aw__when { font-weight: 400; color: var(--ui-aw-muted); font-variant-numeric: tabular-nums; }
.ui-aw__counts { display: flex; flex: none; flex-wrap: wrap; align-items: center; column-gap: 6px; font-size: 12px; line-height: 16px; color: var(--ui-aw-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-aw__count { display: inline-flex; align-items: center; gap: 6px; }
.ui-aw__sep { color: var(--ui-aw-faint); }
.ui-aw__dot { flex: none; width: 6px; height: 6px; border-radius: 50%; background: var(--ui-aw-muted); }
.ui-aw__dot[data-kind="needs-you"] { background: var(--ui-aw-needs); }
.ui-aw__dot[data-kind="failed"] { background: var(--ui-aw-failed); }
.ui-aw__dot[data-kind="done"] { background: var(--ui-aw-done); }
.ui-aw__n { display: inline-block; animation: ui-aw-tick 320ms var(--ui-aw-ease) both; }

/* ----- Rows: always the same height – a change swaps the text in place ----- */
.ui-aw__rows, .ui-aw__empty { border-radius: calc(var(--aui-radius-lg, 16px) - 4px); background: var(--ui-aw-card); box-shadow: var(--aui-shadow-card, var(--ui-aw-shadow)); }
.ui-aw__rows { padding: 4px; }
.ui-aw__list { display: grid; grid-template-columns: minmax(0, 1fr); margin: 0; padding: 0; list-style: none; }
/* Show more: the extra rows grow open and fold back – the card morphs instead of jumping */
.ui-aw__extra { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 380ms var(--ui-aw-ease), opacity 260ms ease; }
.ui-aw__extra[data-open] { grid-template-rows: 1fr; opacity: 1; }
.ui-aw__extra-in { min-height: 0; overflow: hidden; }
.ui-aw__row { display: grid; grid-template-columns: 14px minmax(0, 1fr) auto; align-items: center; column-gap: 10px; height: 48px; padding-inline: 10px 8px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); transition: background-color 140ms ease; }
.ui-aw__row:hover { background: var(--ui-aw-hover); }
.ui-aw__row[data-in] { animation: ui-aw-in 300ms var(--ui-aw-ease) both; }
.ui-aw__mark { position: relative; display: grid; place-items: center; width: 14px; height: 14px; align-self: start; margin-top: 9px; }
/* No detail line: the mark centers on the title */
.ui-aw__row:not([data-detail]) .ui-aw__mark { align-self: center; margin-top: 0; }
.ui-aw__mark[data-pop] { animation: ui-aw-pop 360ms var(--ui-aw-ease) both; }
.ui-aw__mark[data-pop] .ui-aw__tick { stroke-dasharray: 1; animation: ui-aw-draw 360ms 120ms ease-out both; }
/* What needs you breathes – softly, the one thing that moves */
.ui-aw__halo { transform-origin: 7px 7px; opacity: 0; animation: ui-aw-breathe 2400ms ease-out infinite; }
.ui-aw__text { display: grid; grid-template-columns: minmax(0, 1fr); min-width: 0; }
/* Lines line up with the card's own direction, also when a title reads right to left */
.ui-aw__text { text-align: left; }
.ui-aw__text:dir(rtl) { text-align: right; }
.ui-aw__name { min-width: 0; overflow: hidden; font-size: 13px; font-weight: 500; line-height: 20px; color: var(--ui-aw-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-aw__detail { min-width: 0; overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-aw-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-aw__detail[data-pop] { animation: ui-aw-fade 320ms ease both; }

.ui-aw__act { position: relative; display: inline-grid; place-items: center; min-width: 56px; max-width: max(64px, 42cqi); height: 26px; padding: 0 10px; border-radius: calc(var(--aui-radius-sm, 7px) - 1px); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-aw-text); white-space: nowrap; transition: background-color 140ms ease, color 140ms ease, box-shadow 140ms ease; }
.ui-aw__act:hover { background: var(--ui-aw-fill); color: var(--ui-aw-ink); }
/* The one that needs you is a real button; Open and Retry stay quiet */
.ui-aw__act[data-kind="needs-you"] { background: var(--ui-aw-card); box-shadow: var(--ui-aw-shadow); color: var(--ui-aw-ink); }
.ui-aw__act[data-kind="needs-you"]:hover { background: var(--ui-aw-hover); }
.ui-aw__act:not([data-kind="needs-you"]):hover { background: var(--ui-aw-hover); }
/* A long label of your own ends in … – the task keeps its room */
.ui-aw__act-label { min-width: 0; max-width: 100%; overflow: hidden; text-overflow: ellipsis; }
.ui-aw__act:active:not([aria-disabled]) { transform: scale(0.97); }
.ui-aw__act[aria-disabled] { cursor: default; }
.ui-aw__act[aria-busy] .ui-aw__act-label { opacity: 0; }
.ui-aw__spin { position: absolute; width: 12px; height: 12px; border-radius: 50%; border: 1.5px solid var(--ui-aw-fill); border-top-color: var(--ui-aw-muted); animation: ui-aw-spin 700ms linear infinite; }

.ui-aw__empty { padding: 14px; font-size: 12px; line-height: 16px; color: var(--ui-aw-muted); }

/* ----- Show more and Got it ----- */
.ui-aw__foot { display: flex; align-items: center; gap: 4px; padding: 4px 2px 0; }
.ui-aw__more, .ui-aw__done-btn { display: inline-flex; align-items: center; gap: 4px; height: 28px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) - 1px); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-aw-text); white-space: nowrap; transition: background-color 140ms ease, color 140ms ease; }
.ui-aw__more > svg { color: var(--ui-aw-faint); }
.ui-aw__done-btn { gap: 6px; margin-inline-start: auto; padding-inline: 8px 10px; }
.ui-aw__done-btn > svg { color: var(--ui-aw-muted); }
.ui-aw__more:hover, .ui-aw__done-btn:hover { background: var(--ui-aw-fill); color: var(--ui-aw-ink); }

/* ----- Narrow: the counts drop under the title ----- */
@container ui-aw (max-width: 460px) {
  .ui-aw__head { flex-direction: column; align-items: stretch; padding: 6px 8px 8px; }
  .ui-aw__title { flex: none; }
  .ui-aw__row { padding-inline: 8px 6px; }
}
@media (pointer: coarse) {
  .ui-aw__act { height: 36px; min-width: 64px; }
  .ui-aw__row { height: 52px; }
  .ui-aw__more, .ui-aw__done-btn, .ui-aw__divider-btn { height: 36px; }
  .ui-aw__divider-btn { border-radius: 18px; }
  .ui-aw__mark { margin-top: 11px; }
}

@keyframes ui-aw-tick { from { opacity: 0; transform: translateY(4px); } }
@keyframes ui-aw-in { from { opacity: 0; transform: translateY(-4px); } }
@keyframes ui-aw-fade { from { opacity: 0; } }
@keyframes ui-aw-pop { from { opacity: 0; transform: scale(0.4); } 60% { transform: scale(1.12); } }
@keyframes ui-aw-draw { from { stroke-dashoffset: 1; } to { stroke-dashoffset: 0; } }
@keyframes ui-aw-breathe { 0% { opacity: 0.35; transform: scale(1); } 70%, 100% { opacity: 0; transform: scale(1.6); } }
@keyframes ui-aw-spin { to { transform: rotate(360deg); } }

@media (prefers-reduced-motion: reduce) {
  .ui-aw *, .ui-aw *::before, .ui-aw *::after { animation: none !important; transition: none !important; }
  .ui-aw__halo { opacity: 0; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-aw {
  /* Steve: the colors above follow tokens that already switch with dark mode */
  --ui-aw-shadow: 0 0 0 1px var(--ui-aw-line), 0 1px 2px rgba(0, 0, 0, 0.3);
}
`;
