/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";

/*
 * Agent Inbox – every background task your agents are working on, as a calm board, in a single self-contained file.
 * On a soft gray tray, tasks sit in three columns – Needs you, Running, Done – as white cards: a status glyph and the title,
 * two lines on what the agent is doing, and a quiet footer with the state ("Waiting for you", "Running · 2m 14s")
 * and how many tools and sources it used.
 *   · click a card and it unfolds in place: the last steps as a tiny timeline, and what you can do now –
 *     Approve / Reject when it waits, Retry when it failed, Open when it runs or is done
 *   · a card that changes state shows it in place first, then glides over to its new column
 *   · Board / List switches the layout; narrow containers always get the list
 * The styles are injected below, so no extra files are needed.
 */

export type AgentTaskStatus = "running" | "waiting" | "done" | "failed";

export type AgentTaskStep = {
  label: string;
  /** Ready-to-show time, e.g. "2m ago" or "14:02" */
  time?: string;
};

export type AgentTask = {
  id: string;
  title: string;
  /** One or two sentences on what the task is about or where it stands */
  description?: string;
  /** Name of the agent or model, e.g. "Opus 5.5" */
  agent: string;
  status: AgentTaskStatus;
  /** When the task started – the clock ticks from here while it runs or waits */
  startedAt?: number | Date;
  /** How long it took in ms – shown for done and failed tasks */
  elapsed?: number;
  /** What the agent did so far, oldest first – the last four are shown when the card is open */
  steps?: AgentTaskStep[];
  /** Steve: a short reference shown above the title (the item's number) */
  code?: string;
  /** Steve: the state in words, instead of the generic one for the status ("Paused", "Waiting for prerequisites") */
  stateText?: string;
  /** Steve: a ready-to-show time for the footer, instead of the ticking clock */
  time?: string;
  /** Steve: the state's tone when it isn't the status's own (paused reads gray, not amber) */
  tone?: "run" | "wait" | "pause" | "done" | "fail" | "neutral";
  /** Steve: plan steps done and in all, shown in the footer */
  progress?: { done: number; total: number };
  /** Steve: the column, when it isn't the status's own (a paused item sits under Paused, not Needs you) */
  lane?: AgentInboxLane;
  /** Steve: more about the task in the open card, under the agent's name (its project, due date, tags) */
  details?: ReactNode;
};

export type AgentInboxLabels = {
  crumb: string;
  needs: string;
  running: string;
  done: string;
  waitingState: string;
  runningState: string;
  doneState: string;
  failedState: string;
  doneIn: string;
  empty: string;
  approve: string;
  reject: string;
  retry: string;
  open: string;
  layout: string;
  board: string;
  list: string;
  /** with {needs} and {running} */
  summary: string;
  /** with {done} and {total} */
  progress: string;
  /** Steve: the Paused column */
  paused: string;
  /** Steve: a capped column's button, with {count} */
  showAll: string;
  collapse: string;
};
const defaults: AgentInboxLabels = {
  crumb: "Agents",
  needs: "Needs you",
  running: "Running",
  done: "Done",
  waitingState: "Waiting for you",
  runningState: "Running",
  doneState: "Done",
  failedState: "Failed",
  doneIn: "Done in",
  empty: "Nothing here",
  approve: "Approve",
  reject: "Reject",
  retry: "Retry",
  open: "Open",
  layout: "Layout",
  board: "Board",
  list: "List",
  summary: "{needs} need you, {running} running",
  progress: "{done} of {total} plan steps done",
  paused: "Paused",
  showAll: "Show all {count}",
  collapse: "Show fewer",
};

export type AgentInboxView = "board" | "list";

export type AgentInboxProps = {
  tasks: AgentTask[];
  /** With a handler the inbox shows exactly the tasks you pass; without one it changes the status on its own */
  onApprove?: (task: AgentTask) => void;
  onReject?: (task: AgentTask) => void;
  onRetry?: (task: AgentTask) => void;
  /** Shows an Open button on running and done tasks */
  onOpen?: (task: AgentTask) => void;
  /** Heading on the left of the bar */
  title?: string;
  /** A button in the bar, e.g. { label: "New task", onClick } */
  action?: { label: string; onClick: () => void };
  /** Controlled layout */
  view?: AgentInboxView;
  defaultView?: AgentInboxView;
  onViewChange?: (view: AgentInboxView) => void;
  /** Which lanes show – e.g. ["needs"] for just what needs you; defaults to all three */
  lanes?: AgentInboxLane[];
  /** A task id to point at – the other cards fade; null or undefined for none */
  highlight?: string | null;
  /** Steve: the bar (heading, action, layout switch) – off when the page around it has its own */
  bar?: boolean;
  /** Steve: a column with more cards than this (and two more) shows this many and a Show all button */
  cap?: Partial<Record<AgentInboxLane, number>>;
  labels?: Partial<AgentInboxLabels>;
  className?: string;
};

/* ---------- Helpers ---------- */

/** Steve: a fourth column, Paused, for tasks that neither run nor wait on an answer */
export type AgentInboxLane = "needs" | "running" | "paused" | "done";
type Lane = AgentInboxLane;
const LANES: Lane[] = ["needs", "running", "paused", "done"];
const statusLane = (s: AgentTaskStatus): Lane => (s === "waiting" || s === "failed" ? "needs" : s === "running" ? "running" : "done");
const laneOf = (t: { status: AgentTaskStatus; lane?: Lane }): Lane => (t.lane && LANES.includes(t.lane) ? t.lane : statusLane(t.status));
const live = (s: AgentTaskStatus) => s === "running" || s === "waiting";
/** An unknown status reads as running – the task still shows up, with a ticking clock */
const toStatus = (s: unknown): AgentTaskStatus => (s === "waiting" || s === "done" || s === "failed" ? s : "running");
/** A time as a number, a Date or an ISO string – NaN when it can't be read */
const ms = (d: unknown) => (d instanceof Date ? d.getTime() : typeof d === "number" ? d : typeof d === "string" ? Date.parse(d) : NaN);
/** A count or duration that can be shown: finite and not negative */
const whole = (n: unknown) => (typeof n === "number" && Number.isFinite(n) && n >= 0 ? n : undefined);
/** A task as the inbox shows it: a key of its own even when ids repeat, a known status, and the task as you passed it */
type Item = AgentTask & { key: string; src: AgentTask };
const pad = (n: number) => String(n).padStart(2, "0");
const clock = (time: number) => {
  if (!Number.isFinite(time)) return "—";
  const s = Math.max(0, Math.floor(time / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${pad(s % 60)}s`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${pad(m % 60)}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
};
// the exact number in a tooltip
const exact = new Intl.NumberFormat("en");

// One shared second for every running clock – only the clocks render again, not the whole inbox
const ticker = { subs: new Set<() => void>(), id: 0 };
const useNow = (active: boolean) => {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const tick = () => setNow(Date.now());
    tick();
    ticker.subs.add(tick);
    if (!ticker.id) ticker.id = window.setInterval(() => ticker.subs.forEach((f) => f()), 1000);
    return () => {
      ticker.subs.delete(tick);
      if (!ticker.subs.size) {
        window.clearInterval(ticker.id);
        ticker.id = 0;
      }
    };
  }, [active]);
  return now;
};
/** Live tasks count up from `since`; finished ones show `fixed` */
const Clock = ({ since, fixed }: { since?: number; fixed: number }) => {
  const now = useNow(since !== undefined);
  return <>{clock(since !== undefined ? now - since : fixed)}</>;
};
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const EASE = "cubic-bezier(0.32, 0.72, 0.24, 1)";

/* ---------- Icons ---------- */

const line = { fill: "none", stroke: "currentColor", strokeWidth: 1.4, strokeLinecap: "round", strokeLinejoin: "round" } as const;
/** The agent's mark – a small four-point spark, colored by state */
const Spark = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
/* Steve: a paused task shows two bars and an idle one an open ring, not the running spinner or the waiting triangle */
const StateIcon = ({ status, tone }: { status: AgentTaskStatus; tone?: AgentTask["tone"] }) => (
  <span className="ui-ai__state-icon" data-status={tone === "pause" ? "paused" : tone === "neutral" ? "idle" : status} aria-hidden="true">
    {tone === "pause" ? (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <rect x="2.6" y="2" width="2.3" height="8" rx="0.8" fill="currentColor" />
        <rect x="7.1" y="2" width="2.3" height="8" rx="0.8" fill="currentColor" />
      </svg>
    ) : tone === "neutral" ? (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="4.2" fill="none" stroke="currentColor" strokeWidth="1.6" />
      </svg>
    ) : <>
    {status === "running" && (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="4.6" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="1.6" />
        <path d="M6 1.4a4.6 4.6 0 0 1 4.6 4.6" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      </svg>
    )}
    {status === "waiting" && (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <path d="M6 1.6 11 10.2H1L6 1.6Z" fill="currentColor" />
      </svg>
    )}
    {status === "done" && (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="3.4" fill="currentColor" />
      </svg>
    )}
    {status === "failed" && (
      <svg width="12" height="12" viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="4.8" fill="currentColor" />
        <path d="M6 3.6v2.8M6 8.3v.1" stroke="var(--ui-ai-card)" strokeWidth="1.5" strokeLinecap="round" />
      </svg>
    )}
    </>}
  </span>
);
const GoIcon = () => (
  <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
    <path d="M4 8 8.2 3.8M4.6 3.6h3.8v3.8" {...line} strokeWidth={1.4} />
  </svg>
);
const BoardIcon = () => (
  <svg width="13" height="13" viewBox="0 0 14 14" aria-hidden="true">
    <rect x="1.8" y="2.2" width="3.2" height="9.6" rx="1" {...line} strokeWidth={1.2} />
    <rect x="8.9" y="2.2" width="3.2" height="6.4" rx="1" {...line} strokeWidth={1.2} />
  </svg>
);
const ListIcon = () => (
  <svg width="13" height="13" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M2.2 3.6h9.6M2.2 7h9.6M2.2 10.4h9.6" {...line} strokeWidth={1.2} />
  </svg>
);

const PlusIcon = () => (
  <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
    <path d="M6 2.2v7.6M2.2 6h7.6" {...line} strokeWidth={1.5} />
  </svg>
);

export function AgentInbox({
  tasks,
  onApprove,
  onReject,
  onRetry,
  onOpen,
  title = "Agent inbox",
  action,
  view,
  defaultView = "board",
  onViewChange,
  lanes: shownLanes,
  highlight,
  bar = true,
  cap,
  labels: labelsProp,
  className,
}: AgentInboxProps) {
  const L: AgentInboxLabels = { ...defaults, ...labelsProp };
  const STATE: Record<AgentTaskStatus, string> = { waiting: L.waitingState, running: L.runningState, done: L.doneState, failed: L.failedState };
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const [innerView, setInnerView] = useState(defaultView);
  const layout = view ?? innerView;
  const [opened, setOpened] = useState<string | null>(null);
  // Steve: the capped columns you asked to see whole
  const [unfolded, setUnfolded] = useState<Lane[]>([]);

  // Without handlers the inbox runs the decisions itself
  const [local, setLocal] = useState<Record<string, { status: AgentTaskStatus; startedAt?: number; steps?: AgentTaskStep[] }>>({});
  // Gaps are skipped, a repeated id gets its own key, steps without a label are left out
  const ids = new Map<string, number>();
  const list: Item[] = (Array.isArray(tasks) ? tasks : [])
    .filter((t): t is AgentTask => !!t && typeof t === "object")
    .map((src) => {
      const id = String(src.id ?? "");
      const n = ids.get(id) ?? 0;
      ids.set(id, n + 1);
      const key = n ? `${id}#${n}` : id;
      const t = local[key] ? { ...src, ...local[key] } : src;
      const steps = (Array.isArray(t.steps) ? t.steps : [])
        .filter((x): x is AgentTaskStep => !!x && typeof x === "object" && String(x.label ?? "").trim() !== "")
        .map((x) => ({ label: String(x.label), time: x.time ? String(x.time) : undefined }));
      return { ...t, key, src, status: toStatus(t.status), title: String(t.title ?? "").trim() || String(t.code ?? id), steps };
    });

  // Where a live task's clock counts from – without a readable startedAt, from when the inbox first saw it
  const seen = useRef<Record<string, number>>({});
  for (const t of list) seen.current[t.key] ??= Date.now();
  const sinceOf = (t: Item) => {
    if (!live(t.status)) return undefined;
    const start = ms(t.startedAt);
    return Number.isFinite(start) ? start : seen.current[t.key] - (whole(t.elapsed) ?? 0);
  };

  /* ---------- A changed card shows its new state in place, then moves to its new column ---------- */

  const [placed, setPlaced] = useState<Record<string, Lane>>(() => Object.fromEntries(list.map((t) => [t.key, laneOf(t)])));
  const laneKey = list.map((t) => `${t.key}:${laneOf(t)}`).join("|");
  useEffect(() => {
    const pending = list.some((t) => placed[t.key] !== laneOf(t));
    if (!pending) return;
    const timer = window.setTimeout(() => setPlaced(Object.fromEntries(list.map((t) => [t.key, laneOf(t)]))), reduced() ? 0 : 900);
    return () => window.clearTimeout(timer);
  }, [laneKey]); // eslint-disable-line react-hooks/exhaustive-deps
  const laneFor = (t: Item) => placed[t.key] ?? laneOf(t);

  // FLIP: remember where every card was, and glide from there after the move
  const cards = useRef(new Map<string, HTMLElement>());
  const before = useRef(new Map<string, DOMRect>());
  const placedKey = JSON.stringify(placed) + layout;
  const lastKey = useRef(placedKey);
  // Only measure when a card is about to move – not on every render
  if (rootRef.current && lastKey.current !== placedKey) {
    before.current = new Map([...cards.current].map(([id, el]) => [id, el.getBoundingClientRect()]));
  }
  useLayoutEffect(() => {
    if (lastKey.current === placedKey) return;
    lastKey.current = placedKey;
    if (reduced()) return;
    for (const [id, el] of cards.current) {
      const from = before.current.get(id);
      if (!from) {
        el.animate([{ opacity: 0, transform: "translateY(6px)" }, { opacity: 1, transform: "none" }], { duration: 380, easing: EASE });
        continue;
      }
      const to = el.getBoundingClientRect();
      const dx = from.left - to.left;
      const dy = from.top - to.top;
      if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
      el.animate([{ transform: `translate(${dx}px, ${dy}px)`, zIndex: 2 }, { transform: "none", zIndex: 2 }], { duration: 560, easing: EASE });
    }
  }, [placedKey]);

  /* ---------- Decisions ---------- */

  const decide = (task: Item, next: AgentTaskStatus, note: string, handler?: (t: AgentTask) => void) => {
    if (handler) return handler(task.src);
    setLocal((l) => ({
      ...l,
      [task.key]: { status: next, startedAt: Date.now(), steps: [...(task.steps ?? []), { label: note, time: "now" }] },
    }));
  };
  const toggle = (id: string) => setOpened((o) => (o === id ? null : id));
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape" && opened) {
      e.stopPropagation();
      cards.current.get(opened)?.querySelector<HTMLElement>(".ui-ai__card-main")?.focus({ preventScroll: true });
      setOpened(null);
    }
  };
  const setView = (v: AgentInboxView) => {
    setInnerView(v);
    onViewChange?.(v);
  };

  /* ---------- One card ---------- */

  const card = (t: Item) => {
    const isOpen = opened === t.key;
    const steps = (t.steps ?? []).slice(-4);
    const elapsed = whole(t.elapsed);
    // Done without a readable time just says Done
    const timed = t.time ? true : t.status !== "failed" && (t.status !== "done" || elapsed !== undefined);
    const desc = typeof t.description === "string" ? t.description.trim() : "";
    const agent = String(t.agent ?? "").trim();
    const panel = `${uid}-${encodeURIComponent(t.key)}`;
    // The word goes first when a card is slim – the glyph already says it – so the time always fits.
    // Steve: a state of its own ("你的消息在排队") is the card's news, so a slim card keeps it and drops the time instead
    const said = t.stateText || STATE[t.status];
    const word = t.time ? said : t.status === "done" ? (timed ? L.doneIn : said) : said;
    const dot = t.time ? true : t.status === "running" || t.status === "waiting";
    const prog = t.progress && t.progress.total > 0 ? t.progress : undefined;
    return (
      <li
        key={t.key}
        data-id={t.id}
        data-kanban-card={t.id}
        className="ui-ai__card"
        data-faded={highlight && t.id !== highlight ? "" : undefined}
        data-status={t.status}
        data-tone={t.tone}
        data-open={isOpen ? "" : undefined}
        data-go={onOpen ? "" : undefined}
        ref={(el) => {
          if (el) cards.current.set(t.key, el);
          else cards.current.delete(t.key);
        }}
      >
        <button type="button" className="ui-ai__card-main" aria-expanded={isOpen} aria-controls={panel} onClick={() => toggle(t.key)}>
          {t.code && <span className="ui-ai__code">{t.code}</span>}
          <span className="ui-ai__title-row">
            <span className="ui-ai__spark">
              <Spark />
            </span>
            <span className="ui-ai__title" dir="auto" title={t.title}>
              {t.title}
            </span>
          </span>
          {desc && (
            <span className="ui-ai__desc" dir="auto">
              {desc}
            </span>
          )}
          <span className="ui-ai__foot">
            <span className="ui-ai__state" data-status={t.status} data-bare={timed ? undefined : ""} data-said={t.stateText ? "" : undefined} title={t.time ? `${said} · ${t.time}` : undefined}>
              <StateIcon status={t.status} tone={t.tone} />
              {/* The word may shorten, the time never does */}
              <span className="ui-ai__state-word" data-state-word>{word}{dot && <span className="ui-ai__dot"> ·</span>}</span>
              {timed && (
                <span className="ui-ai__time">
                  {t.time ?? <Clock since={sinceOf(t)} fixed={elapsed ?? 0} />}
                </span>
              )}
            </span>
            <span className="ui-ai__meta">
              {prog && (
                <span title={L.progress.replace("{done}", exact.format(prog.done)).replace("{total}", exact.format(prog.total))} data-plan>
                  <span className="ui-ai__plan" aria-hidden="true"><span style={{ width: `${(Math.min(prog.done, prog.total) / prog.total) * 100}%` }} /></span>
                  {exact.format(prog.done)}/{exact.format(prog.total)}
                </span>
              )}
            </span>
          </span>
        </button>
        {/* Steve: opening the task is one click; the card itself unfolds */}
        {onOpen && (
          <button type="button" className="ui-ai__go" aria-label={`${L.open} ${t.code ?? t.title}`} title={L.open} onClick={() => onOpen(t)} data-board-go={t.id}>
            <GoIcon />
          </button>
        )}

        <div id={panel} className="ui-ai__fold" data-open={isOpen ? "" : undefined} inert={!isOpen}>
          <div className="ui-ai__clip">
            <div className="ui-ai__more">
              {agent && <div className="ui-ai__agent" dir="auto">{agent}</div>}
              {t.details && <div className="ui-ai__details">{t.details}</div>}
              {steps.length > 0 && (
                <ol className="ui-ai__steps">
                  {steps.map((s, i) => (
                    <li key={i} data-last={i === steps.length - 1 ? "" : undefined}>
                      <span dir="auto">{s.label}</span>
                      {s.time && <time dir="auto">{s.time}</time>}
                    </li>
                  ))}
                </ol>
              )}
              <div className="ui-ai__actions">
                {/* Steve: a decision is answered on its own card (many kinds, many answers), so Approve / Reject only with handlers */}
                {t.status === "waiting" && onReject && (
                  <button type="button" className="ui-ai__btn" onClick={() => decide(t, "failed", L.reject, onReject)}>
                    {L.reject}
                  </button>
                )}
                {t.status === "waiting" && onApprove && (
                  <button type="button" className="ui-ai__btn ui-ai__btn--primary" onClick={() => decide(t, "running", L.approve, onApprove)}>
                    {L.approve}
                  </button>
                )}
                {t.status === "failed" && onRetry && (
                  <button type="button" className="ui-ai__btn" onClick={() => decide(t, "running", L.retry, onRetry)}>
                    {L.retry}
                  </button>
                )}
                {onOpen && (
                  <button type="button" className={["ui-ai__btn", t.status === "waiting" && !onApprove ? "ui-ai__btn--primary" : ""].filter(Boolean).join(" ")} onClick={() => onOpen(t)} data-board-open={t.id}>
                    {L.open}
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      </li>
    );
  };

  // Big lanes read 999+, the exact number on hover
  const many = (n: number) => (n > 999 ? "999+" : String(n));
  const lanes = LANES.filter((lane) => (shownLanes ? shownLanes.includes(lane) : true));
  const counts = Object.fromEntries(LANES.map((l) => [l, list.filter((t) => laneFor(t) === l).length])) as Record<Lane, number>;
  const summary = L.summary.replace("{needs}", String(counts.needs)).replace("{running}", String(counts.running));

  return (
    <div ref={rootRef} className={["ui-ai", className].filter(Boolean).join(" ")} data-view={layout} onKeyDown={onKeyDown}>
      <style href="ui-agent-inbox" precedence="default">
        {css}
      </style>

      {bar && <div className="ui-ai__bar">
        <span className="ui-ai__heading">
          <span className="ui-ai__crumb">{L.crumb}</span>
          <span className="ui-ai__slash" aria-hidden="true">
            /
          </span>
          <span className="ui-ai__heading-title" title={title}>
            {title}
          </span>
        </span>
        <div className="ui-ai__tools">
        {action && (
          // In a narrow inbox only the plus remains – the label stays its name
          <button type="button" className="ui-ai__action" aria-label={action.label} title={action.label} onClick={action.onClick}>
            <PlusIcon />
            <span className="ui-ai__action-label">{action.label}</span>
          </button>
        )}
        {(view === undefined || onViewChange) && (
        <div className="ui-ai__views" role="radiogroup" aria-label={L.layout}>
          {(["board", "list"] as const).map((v) => (
            <button key={v} type="button" role="radio" aria-checked={layout === v} className="ui-ai__view" onClick={() => setView(v)}>
              {v === "board" ? <BoardIcon /> : <ListIcon />}
              {v === "board" ? L.board : L.list}
            </button>
          ))}
        </div>
        )}
        </div>
      </div>}

      {/* Steve: an empty column folds to a narrow rail on the board, so the columns with cards keep their width */}
      <div className="ui-ai__lanes" style={{ "--ui-ai-cols": lanes.map((l) => (counts[l] ? "minmax(0, 1fr)" : "2.75rem")).join(" ") } as CSSProperties}>
        {lanes.map((lane) => {
          const all = list.filter((t) => laneFor(t) === lane), n = cap?.[lane];
          const folds = n !== undefined && all.length > n + 2, shown = folds && !unfolded.includes(lane) ? all.slice(0, n) : all;
          return (
          <section key={lane} className="ui-ai__lane" aria-label={L[lane]} data-lane={lane} data-board-col={lane} data-empty={counts[lane] ? undefined : ""} title={counts[lane] ? undefined : L[lane]}>
            <header className="ui-ai__lane-head">
              <span className="ui-ai__lane-spark" aria-hidden="true">
                <Spark />
              </span>
              <span className="ui-ai__lane-name">{L[lane]}</span>
              <span className="ui-ai__count" title={counts[lane] > 999 ? exact.format(counts[lane]) : undefined}>
                {many(counts[lane])}
              </span>
            </header>
            <ul className="ui-ai__cards">
              {shown.map(card)}
              {counts[lane] === 0 && <li className="ui-ai__empty">{L.empty}</li>}
              {folds && (
                <li>
                  <button type="button" className="ui-ai__all" onClick={() => setUnfolded((w) => (w.includes(lane) ? w.filter((x) => x !== lane) : [...w, lane]))} data-lane-more={lane}>
                    {unfolded.includes(lane) ? L.collapse : L.showAll.replace("{count}", exact.format(all.length))}
                  </button>
                </li>
              )}
            </ul>
          </section>
          );
        })}
      </div>

      <span className="ui-ai__sr" aria-live="polite">
        {summary}
      </span>
    </div>
  );
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-ai */
.ui-ai {
  --ui-ai-tray: var(--aui-subtle, #f0f0f0);
  --ui-ai-card: var(--aui-surface, #ffffff);
  --ui-ai-line: var(--aui-border, #ececec);
  --ui-ai-chip: var(--aui-subtle, #f5f5f5);
  --ui-ai-track: color-mix(in srgb, var(--aui-ink, #171717) 7%, var(--ui-ai-tray));
  --ui-ai-ink: var(--aui-ink, #171717);
  --ui-ai-text: var(--aui-muted, #6b6b6b);
  --ui-ai-muted: var(--aui-muted, #8a8a8a);
  --ui-ai-faint: var(--aui-placeholder, #b8b8b8);
  --ui-ai-blue: var(--aui-blue, #4fb0fe);
  --ui-ai-amber: var(--aui-amber, #e8b000);
  --ui-ai-green: var(--aui-success, #1e8a4f);
  --ui-ai-red: var(--aui-red, #f04438);
  --ui-ai-brand-bg: var(--aui-info-soft, #eaf1ff);
  --ui-ai-brand-fg: var(--aui-info, #2456d6);
  --ui-ai-warn-bg: var(--aui-warning-soft, #fff1e0);
  --ui-ai-warn-fg: var(--aui-warning, #b25e00);
  --ui-ai-ok-bg: var(--aui-success-soft, #e6f6ed);
  --ui-ai-ok-fg: var(--aui-success, #1e8a4f);
  --ui-ai-danger-bg: var(--aui-danger-soft, #fdecea);
  --ui-ai-danger-fg: var(--aui-danger, #c4320a);
  --ui-ai-paused-bg: var(--aui-subtle, #f0f0f0);
  --ui-ai-paused-fg: var(--aui-muted, #6b6b6b);
  --ui-ai-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-ai-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  padding: 4px;
  border-radius: calc(var(--aui-radius-lg, 16px) + 4px);
  background: var(--ui-ai-tray);
  /* Steve: named, so the narrow-inbox rules below ask the inbox and not the nearest card */
  container: ui-ai / inline-size;
  font-family: var(--aui-font, "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 14px;
  line-height: 20px;
  letter-spacing: -0.006em;
  color: var(--ui-ai-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-ai *, .ui-ai *::before, .ui-ai *::after { box-sizing: border-box; }
:where(.ui-ai) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-ai button:focus-visible { outline: 2px solid var(--ui-ai-focus); outline-offset: 2px; }
.ui-ai ul, .ui-ai ol { margin: 0; padding: 0; list-style: none; }
.ui-ai svg { display: block; flex: none; }
.ui-ai__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* ----- Bar on the tray: where you are, an optional action and the layout ----- */
.ui-ai__bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 8px 10px 14px; }
.ui-ai__heading { display: flex; align-items: center; gap: 8px; min-width: 0; font-size: 14px; font-weight: 600; color: var(--ui-ai-ink); white-space: nowrap; }
.ui-ai__crumb { flex: none; color: var(--ui-ai-muted); font-weight: 500; }
.ui-ai__slash { flex: none; color: var(--aui-placeholder, var(--ui-ai-faint)); font-weight: 400; }
.ui-ai__heading-title { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-ai__tools { display: flex; flex: none; align-items: center; gap: 8px; }
.ui-ai__action-label { max-width: 200px; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-ai__action { display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 12px 0 10px; border-radius: 999px; font-size: 13px; font-weight: 500; color: var(--ui-ai-ink); transition: background-color 160ms ease; }
.ui-ai__action:hover { background: var(--ui-ai-track); }
/* A gray track, the chosen layout raised in white */
.ui-ai__views { display: flex; flex: none; padding: 3px; border-radius: calc(var(--aui-radius, 10px) + 2px); background: var(--ui-ai-track); }
.ui-ai__view { display: inline-flex; align-items: center; gap: 6px; height: 28px; padding: 0 12px; border-radius: calc(var(--aui-radius, 10px) - 1px); font-size: 13px; font-weight: 500; color: var(--ui-ai-muted); transition: background-color 200ms ease, color 200ms ease, box-shadow 200ms ease; }
.ui-ai__view:hover { color: var(--ui-ai-ink); }
.ui-ai__view[aria-checked="true"] { background: var(--ui-ai-card); color: var(--ui-ai-ink); box-shadow: var(--aui-shadow-card, 0 1px 2px rgba(0, 0, 0, 0.08)); }

/* ----- Columns: headings on the tray, white cards below ----- */
.ui-ai__lanes { display: grid; grid-template-columns: var(--ui-ai-cols, repeat(3, minmax(0, 1fr))); gap: 4px; align-items: start; }
.ui-ai__lane { min-width: 0; }
.ui-ai__lane-head { display: flex; align-items: center; gap: 8px; height: 32px; padding: 0 10px; }
.ui-ai__lane-spark { display: grid; color: var(--ui-ai-blue); }
.ui-ai__lane[data-lane="needs"] .ui-ai__lane-spark { color: var(--ui-ai-amber); }
.ui-ai__lane[data-lane="done"] .ui-ai__lane-spark { color: var(--ui-ai-green); }
.ui-ai__lane[data-lane="paused"] .ui-ai__lane-spark { color: var(--ui-ai-muted); }
.ui-ai__lane-name { overflow: hidden; font-size: 13px; font-weight: 600; color: var(--ui-ai-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-ai__count { display: grid; place-items: center; min-width: 20px; height: 20px; padding: 0 6px; border-radius: 999px; background: var(--ui-ai-card); box-shadow: 0 0 0 1px var(--ui-ai-line); font-size: 11.5px; font-weight: 500; color: var(--ui-ai-text); font-variant-numeric: tabular-nums; }
.ui-ai__cards { display: grid; gap: 4px; }
.ui-ai__empty { padding: 18px 0 20px; border-radius: var(--aui-radius-lg, 16px); box-shadow: inset 0 0 0 1px var(--ui-ai-track); font-size: 13px; text-align: center; color: var(--ui-ai-muted); }

/* ----- A card ----- */
.ui-ai__card[data-faded] { opacity: 0.4; }
.ui-ai__card { position: relative; border-radius: var(--aui-radius-lg, 16px); background: var(--ui-ai-card); box-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-ai-line), 0 1px 2px rgba(0, 0, 0, 0.04)); transition: box-shadow 200ms ease, opacity 240ms ease; }
.ui-ai__card:hover, .ui-ai__card[data-open] { box-shadow: var(--aui-shadow-card-hover, var(--lift-hover, 0 0 0 1px #e0e0e0, 0 6px 16px -8px rgba(0, 0, 0, 0.14))); }
.ui-ai__card-main { display: grid; gap: 6px; width: 100%; padding: 14px 14px 12px; border-radius: var(--aui-radius-lg, 16px); container: ai-card / inline-size; }
/* Top-aligned: the spark stays on the first line when a long title wraps */
.ui-ai__code { padding-left: 26px; font-family: var(--aui-font-mono, ui-monospace, monospace); font-size: 12px; line-height: 16px; color: var(--ui-ai-muted); text-align: start; }
.ui-ai__plan { position: relative; display: block; width: 32px; height: 4px; border-radius: 999px; background: var(--ui-ai-track); overflow: hidden; }
.ui-ai__plan > span { position: absolute; inset: 0 auto 0 0; border-radius: inherit; background: var(--ui-ai-muted); }
.ui-ai__card[data-tone="pause"] .ui-ai__spark, .ui-ai__card[data-tone="neutral"] .ui-ai__spark { color: var(--ui-ai-muted); }
.ui-ai__card[data-tone="pause"] .ui-ai__state, .ui-ai__card[data-tone="neutral"] .ui-ai__state { background: var(--ui-ai-paused-bg); color: var(--ui-ai-paused-fg); }
.ui-ai__title-row { display: flex; align-items: flex-start; gap: 10px; min-width: 0; }
.ui-ai__spark { display: grid; flex: none; place-items: center; width: 16px; height: 20px; color: var(--ui-ai-blue); transition: color 300ms ease; }
.ui-ai__card[data-status="waiting"] .ui-ai__spark { color: var(--ui-ai-amber); }
.ui-ai__card[data-status="failed"] .ui-ai__spark { color: var(--ui-ai-red); }
.ui-ai__card[data-status="done"] .ui-ai__spark { color: var(--ui-ai-green); }
/* Steve: two lines before it is cut, so a slim card still says what the task is */
.ui-ai__title { display: -webkit-box; min-width: 0; overflow: hidden; font-size: 14px; font-weight: 600; line-height: 20px; color: var(--ui-ai-ink); text-align: start; overflow-wrap: anywhere; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
.ui-ai__desc { display: -webkit-box; padding-left: 26px; overflow: hidden; font-size: 13px; line-height: 19px; color: var(--ui-ai-text); text-align: start; overflow-wrap: anywhere; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
/* Open: the whole title and description, nothing cut off */
.ui-ai__card[data-open] .ui-ai__title { -webkit-line-clamp: unset; }
.ui-ai__card[data-open] .ui-ai__desc { -webkit-line-clamp: unset; }
.ui-ai__foot { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-width: 0; margin-top: 6px; padding-left: 26px; }
/* The state: a pill on a soft tone, with its glyph */
.ui-ai__state { display: inline-flex; align-items: center; gap: 6px; min-width: 0; height: 24px; padding: 0 10px 0 8px; border-radius: 999px; background: var(--ui-ai-brand-bg); font-size: 12.5px; font-weight: 500; color: var(--ui-ai-brand-fg); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-ai__state[data-status="waiting"] { background: var(--ui-ai-warn-bg); color: var(--ui-ai-warn-fg); }
.ui-ai__state[data-status="done"] { background: var(--ui-ai-ok-bg); color: var(--ui-ai-ok-fg); }
.ui-ai__state[data-status="failed"] { background: var(--ui-ai-danger-bg); color: var(--ui-ai-danger-fg); }
/* Never cut down to a single letter – a few letters and … at least; a space's width to the time, cut or not */
.ui-ai__state-word { min-width: 2.6em; overflow: hidden; text-overflow: ellipsis; white-space: pre; margin-right: calc(0.28em - 6px); }
.ui-ai__time { flex: none; }
.ui-ai__dot { white-space: pre; }
.ui-ai__state-icon { display: grid; flex: none; place-items: center; width: 12px; height: 12px; color: currentColor; animation: ui-ai-pop 360ms var(--ui-ai-ease) both; }
.ui-ai__state-icon[data-status="running"] svg { animation: ui-ai-spin 900ms linear infinite; }
.ui-ai__meta { display: inline-flex; flex: none; align-items: center; font-size: 12.5px; color: var(--ui-ai-muted); font-variant-numeric: tabular-nums; }
.ui-ai__meta > span { display: inline-flex; align-items: center; gap: 5px; padding: 0 9px; }
.ui-ai__meta > span + span { border-left: 1px solid var(--ui-ai-line); }
.ui-ai__meta > span:last-child { padding-right: 0; }

/* Open: steps and decisions unfold under the card's text */
.ui-ai__fold { display: grid; grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 460ms var(--ui-ai-ease), opacity 240ms ease; }
.ui-ai__fold[data-open] { grid-template-rows: 1fr; opacity: 1; transition: grid-template-rows 460ms var(--ui-ai-ease), opacity 320ms ease 100ms; }
.ui-ai__clip { min-height: 0; overflow: hidden; }
.ui-ai__more { margin: 0 14px; padding: 12px 0 14px; border-top: 1px solid var(--ui-ai-line); }
.ui-ai__agent { font-size: 12.5px; font-weight: 500; color: var(--ui-ai-muted); }
.ui-ai__details { margin-top: 6px; font-size: 12.5px; color: var(--ui-ai-muted); }
/* Steve: Open, one click, in the card's top corner; the card's first line leaves it room */
.ui-ai__go { position: absolute; top: 9px; right: 9px; display: grid; place-items: center; width: 24px; height: 24px; border-radius: var(--aui-radius-sm, 8px); color: var(--ui-ai-faint); transition: background-color 160ms ease, color 160ms ease; }
.ui-ai__go:hover { background: var(--ui-ai-chip); color: var(--ui-ai-ink); }
.ui-ai__card[data-go] .ui-ai__card-main > :first-child { padding-right: 24px; }
/* Steve: a capped column's Show all */
.ui-ai__all { display: block; width: 100%; height: 32px; border-radius: var(--aui-radius, 10px); font-size: 13px; font-weight: 500; color: var(--ui-ai-muted); text-align: center; transition: background-color 160ms ease, color 160ms ease; }
.ui-ai__all:hover { background: var(--ui-ai-track); color: var(--ui-ai-ink); }
.ui-ai__steps { position: relative; display: grid; gap: 6px; margin-top: 8px !important; padding-left: 14px !important; }
.ui-ai__steps li { position: relative; display: flex; justify-content: space-between; gap: 10px; font-size: 13px; line-height: 18px; color: var(--ui-ai-text); }
.ui-ai__steps li::before { content: ""; position: absolute; top: 6px; left: -14px; width: 6px; height: 6px; border-radius: 50%; background: var(--ui-ai-faint); }
.ui-ai__steps li:not(:last-child)::after { content: ""; position: absolute; top: 15px; bottom: -9px; left: -11.5px; width: 1px; background: var(--ui-ai-line); }
.ui-ai__steps li[data-last]::before { background: var(--ui-ai-ink); }
.ui-ai__steps li > span { min-width: 0; text-align: start; overflow-wrap: anywhere; white-space: pre-line; }
/* A long time wraps on its side instead of squeezing the step to nothing */
.ui-ai__steps time { flex: none; max-width: 45%; color: var(--ui-ai-muted); text-align: end; }
.ui-ai__actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 12px; }
.ui-ai__actions:empty { display: none; }
.ui-ai__btn { display: inline-flex; align-items: center; height: 32px; padding: 0 14px; border-radius: var(--aui-radius, 10px); background: var(--ui-ai-card); box-shadow: 0 0 0 1px var(--ui-ai-line), 0 1px 2px rgba(0, 0, 0, 0.04); font-size: 13.5px; font-weight: 500; color: var(--ui-ai-ink); transition: background-color 160ms ease, opacity 160ms ease; }
.ui-ai__btn:hover { background: var(--ui-ai-chip); }
.ui-ai__btn--primary { background: var(--aui-primary, var(--ui-ai-ink)); box-shadow: none; color: var(--aui-on-primary, var(--ui-ai-card)); }
.ui-ai__btn--primary:hover { background: var(--aui-primary, var(--ui-ai-ink)); opacity: 0.88; }

/* ----- List layout: the columns stack, one after another ----- */
.ui-ai[data-view="list"] .ui-ai__lanes { grid-template-columns: minmax(0, 1fr); gap: 8px; }
/* Steve: on a wide board an empty column is a rail with its name down the side */
@container ui-ai (min-width: 621px) {
  .ui-ai[data-view="board"] .ui-ai__lane[data-empty] { min-height: 12rem; border-radius: var(--aui-radius-lg, 16px); box-shadow: inset 0 0 0 1px var(--ui-ai-track); }
  .ui-ai[data-view="board"] .ui-ai__lane[data-empty] .ui-ai__lane-head { flex-direction: column; height: auto; padding: 12px 0; gap: 8px; }
  .ui-ai[data-view="board"] .ui-ai__lane[data-empty] .ui-ai__lane-name { writing-mode: vertical-rl; color: var(--ui-ai-muted); }
  .ui-ai[data-view="board"] .ui-ai__lane[data-empty] .ui-ai__cards { display: none; }
}
.ui-ai__lanes { animation: ui-ai-in 320ms ease both; }

@keyframes ui-ai-spin { to { transform: rotate(360deg); } }
@keyframes ui-ai-pop { from { opacity: 0; transform: scale(0.6); } }
@keyframes ui-ai-in { from { opacity: 0.4; } }

/* Narrow containers: always the list, without the switch */
@container ui-ai (max-width: 620px) {
  .ui-ai__views { display: none; }
  .ui-ai__action { justify-content: center; width: 30px; padding: 0; background: var(--ui-ai-track); }
  .ui-ai__action-label { display: none; }
  .ui-ai .ui-ai__lanes { grid-template-columns: minmax(0, 1fr); gap: 8px; }
}
/* Slim cards (three columns in a narrow board): the text starts under the spark, so the footer keeps its words */
@container ai-card (max-width: 280px) {
  .ui-ai__desc, .ui-ai__foot, .ui-ai__code { padding-left: 0; }
  .ui-ai__state:not([data-status="failed"]):not([data-bare]):not([data-said]) .ui-ai__state-word { display: none; }
  .ui-ai__state[data-said] .ui-ai__time, .ui-ai__state[data-said] .ui-ai__dot { display: none; }
}
@container ui-ai (max-width: 360px) {
  .ui-ai__desc, .ui-ai__foot, .ui-ai__code { padding-left: 0; }
  /* A long title wraps to three lines at most – the open card shows all of it */
  .ui-ai__card:not([data-open]) .ui-ai__title { display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; }
}

@media (pointer: coarse) {
  .ui-ai__view { height: 34px; }
  .ui-ai__btn { height: 40px; padding: 0 18px; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-ai *, .ui-ai *::before, .ui-ai *::after { animation: none !important; transition: none !important; }
}

`;
