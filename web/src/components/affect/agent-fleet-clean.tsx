/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

/*
 * Agent Fleet – several agents at work side by side, in a single self-contained file.
 * Every main agent is a card in one column: what it does, its status, a small progress circle for how far its
 * sub-agents are – red where one needs you or failed – and what the whole branch cost. Click one and its sub-agents open as a tree to
 * the right; click another and the first tree folds back into its card before the next one grows out.
 *   · the open agent stays as it is, the others step back
 *   · the old tree leaves bottom first while its connectors pull back into the card – then the new connectors draw
 *     themselves out and the cards rise in, one after another
 *   · each main agent sums up its sub-agents in one phrase: what needs you first, then what failed, then what runs
 *   · costs roll up – a main agent shows what its whole branch cost, and costs that still climb shimmer
 *   · the tools an agent works with sit as small overlapping tiles before what it does
 *   · click a sub-agent and a small window drops down with the tasks it worked through
 *   · one that needs you or failed gets an Answer or Resolve button – its window leads with Approve and Reject,
 *     or Retry and Dismiss, and hands your choice to onResolve
 * Narrow containers open the tree under the card instead. The styles are injected below, so no extra files are needed.
 */

export type FleetAgentStatus = "queued" | "running" | "waiting" | "done" | "failed" | "stopped" | "idle" | "paused";

export type FleetAgent = {
  id: string;
  name: string;
  /** What it was asked to do */
  task: string;
  model?: string;
  /** A small mark, e.g. the model maker's logo – defaults to the initials */
  avatar?: ReactNode;
  /** Marks of the tools it works with, e.g. GitHub and Stripe – shown as small overlapping tiles before the task */
  tools?: ReactNode[];
  status: FleetAgentStatus;
  /** What a sub-agent got done – a click on its card opens them in a small window */
  steps?: FleetStep[];
  /** What it waits for you to decide – shown with Approve and Reject while it needs you */
  ask?: string;
  /** What went wrong – shown with Retry and Dismiss when it failed */
  error?: string;
  /** Its own cost so far – the sub-agents' costs are added on top */
  cost?: number;
  /** When it started and ended (ms) – for the time it took */
  started?: number;
  ended?: number;
  /** Its sub-agents – they may have their own */
  children?: FleetAgent[];
  /** Steve: the time after its cost, already formatted (e.g. how long it actually worked) – in place of the clock */
  time?: ReactNode;
  /** Steve: the word on its status pill, in place of its status's own (e.g. the item's own state) */
  statusText?: string;
  /** Steve: attributes on its card (data-* for tests) */
  attrs?: Record<string, string | undefined>;
};

/** One task a sub-agent worked on */
export type FleetStep = {
  title: string;
  /** Defaults to done */
  status?: FleetAgentStatus;
  /** The tool it used – a small tile before the title */
  tool?: ReactNode;
  /** How long it took (ms) */
  took?: number;
};

export type FleetAction = "approve" | "reject" | "retry" | "dismiss";

export type AgentFleetProps = {
  /** The main agents, one card each in the column */
  agents: FleetAgent[];
  /** Controlled id of the main agent whose tree is open – null for none */
  open?: string | null;
  /** Uncontrolled start – defaults to the first agent */
  defaultOpen?: string | null;
  onOpenChange?: (id: string | null) => void;
  /** A click on a sub-agent – e.g. to open its log; one with steps also opens them in a small window */
  onSelect?: (agent: FleetAgent) => void;
  /** An answer for a sub-agent that needs you (approve, reject) or failed (retry, dismiss) – update its status in your data */
  onResolve?: (agent: FleetAgent, action: FleetAction) => void;
  /** The clock for running times – defaults to now */
  now?: number;
  /** Formats every cost – defaults to dollars, e.g. "$0.42" */
  formatCost?: (amount: number) => string;
  title?: string;
  /** An agent id to point at – its lead's tree opens and the other cards fade; null or undefined for none */
  highlight?: string | null;
  className?: string;
  /** Steve: the words it shows, translated */
  labels?: Partial<FleetLabels>;
};

export type FleetLabels = {
  status: Record<FleetAgentStatus, string>;
  noSubs: string; needsYou: (n: number) => string; failed: (n: number) => string; running: (n: number, of: number) => string; allDone: (n: number) => string; subs: (n: number) => string;
  agents: (n: number) => string; subsOf: (name: string) => string; branch: string; answer: string; resolve: string; tasks: (n: number) => string; needsAnswer: string; failedWord: string;
  waitsDefault: string; errorDefault: string; reject: string; dismiss: string; approve: string; retry: string; tasksTitle: string; doneOf: (done: number, of: number) => string;
  done: string; runningWord: string; skipped: string; noSubsYet: string; noAgents: string; runningN: (n: number) => string; needsYouN: (n: number) => string;
};
const LABELS: FleetLabels = {
  status: { queued: "Queued", running: "Running", waiting: "Needs you", done: "Done", failed: "Failed", stopped: "Stopped", idle: "Idle", paused: "Paused" },
  noSubs: "No sub-agents", needsYou: (n) => `${count.format(n)} needs you`, failed: (n) => `${count.format(n)} failed`, running: (n, of) => `${count.format(n)}/${count.format(of)} running`,
  allDone: (n) => (n === 1 ? "Done" : `All ${count.format(n)} done`), subs: (n) => (n === 1 ? "1 sub-agent" : `${count.format(n)} sub-agents`),
  agents: (n) => (n === 1 ? "1 agent" : `${count.format(n)} agents`), subsOf: (name) => `Sub-agents of ${name}`, branch: "With its sub-agents", answer: "Answer", resolve: "Resolve",
  tasks: (n) => (n === 1 ? "1 task" : `${count.format(n)} tasks`), needsAnswer: "Needs your answer", failedWord: "Failed", waitsDefault: "It waits for your go-ahead.", errorDefault: "It stopped with an error.",
  reject: "Reject", dismiss: "Dismiss", approve: "Approve", retry: "Retry", tasksTitle: "Tasks", doneOf: (d, of) => `${count.format(d)}/${count.format(of)} done`,
  done: "Done", runningWord: "Running", skipped: "Skipped", noSubsYet: "No sub-agents yet", noAgents: "No agents at work",
  runningN: (n) => `${count.format(n)} running`, needsYouN: (n) => `${count.format(n)} needs you`,
};

/* ---------- Helpers ---------- */

// Less than a cent still cost something – "<$0.01", never "$0.00"
// A refund or credit keeps its sign in front – "-$42.50", not "$-42.50"
const dollars = (n: number): string => (n < 0 ? `-${dollars(-n)}` : n > 0 && n < 0.005 ? "<$0.01" : `$${n < 10 ? n.toFixed(2) : Math.round(n).toLocaleString("en-US")}`);
const count = new Intl.NumberFormat("en-US");
const took = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h ${String(m % 60).padStart(2, "0")}m` : `${Math.floor(h / 24)}d ${h % 24}h`;
};
// Whole letters only – "🔍 Researcher" gives "R", not half an emoji; "Research & Design" gives "RD"
const initials = (name: string) =>
  name
    .split(/\s+/)
    .map((w) => Array.from(w.replace(/^[^\p{L}\p{N}]+/u, ""))[0])
    .filter(Boolean)
    .slice(0, 2)
    .join("")
    .toUpperCase();
const walk = (a: FleetAgent): FleetAgent[] => [a, ...(a.children ?? []).flatMap(walk)];
const costOf = (a: FleetAgent): number => (a.cost ?? 0) + (a.children ?? []).reduce((sum, c) => sum + costOf(c), 0);
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const muted = (s: FleetAgentStatus) => s === "queued" || s === "stopped";

/** Position of an element inside the fleet, from offsets – so the cards' own motion doesn't throw the connectors off */
const place = (el: HTMLElement, box: HTMLElement) => {
  let x = 0;
  let y = 0;
  let node: HTMLElement | null = el;
  while (node && node !== box) {
    x += node.offsetLeft;
    y += node.offsetTop;
    node = node.offsetParent as HTMLElement | null;
  }
  return { x, y, w: el.offsetWidth, h: el.offsetHeight };
};

/** The sub-agents in one phrase – what needs you comes first, then what failed, then what still runs */
const sayOf = (a: FleetAgent, L: FleetLabels): { text: string; kind?: "waiting" | "failed" | "running" } => {
  const subs = walk(a).slice(1);
  if (!subs.length) return { text: L.noSubs };
  const n = (s: FleetAgentStatus) => subs.filter((x) => x.status === s).length;
  if (n("waiting")) return { text: L.needsYou(n("waiting")), kind: "waiting" };
  if (n("failed")) return { text: L.failed(n("failed")), kind: "failed" };
  if (n("running"))
    return {
      text: L.running(n("running"), subs.length),
      kind: "running",
    };
  if (n("done") === subs.length)
    return {
      text: L.allDone(subs.length),
    };
  return {
    text: L.subs(subs.length),
  };
};

/* ---------- Icons ---------- */

const Spark = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
/* Status glyphs: Hugeicons Free (MIT, © Hugeicons, hugeicons.com); the loader is a plain arc */
const StatusIcon = ({ status }: { status: FleetAgentStatus }) => {
  if (status === "running")
    return (
      <svg className="ui-agfc__spin" width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle cx="12" cy="12" r="8.5" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2.5" />
        <path d="M12 3.5A8.5 8.5 0 0 1 20.5 12" stroke="currentColor" strokeLinecap="round" strokeWidth="2.5" />
      </svg>
    );
  if (status === "done")
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M5 14L8.5 17.5L19 6.5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.4" />
      </svg>
    );
  if (status === "failed")
    return (
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
  if (status === "waiting")
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path d="M22 12C22 17.5228 17.5228 22 12 22C6.47715 22 2 17.5228 2 12C2 6.47715 6.47715 2 12 2C17.5228 2 22 6.47715 22 12Z" stroke="currentColor" strokeWidth="2" />
        <path d="M12 7.5V12L14.5 14.5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
      </svg>
    );
  if (status === "stopped")
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <rect x="6" y="6" width="12" height="12" rx="2.5" stroke="currentColor" strokeWidth="2.2" />
      </svg>
    );
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="3.5" fill="currentColor" />
    </svg>
  );
};

/** Motion: how long cards take to leave and to move, and the beat between cards that go together */
const EXIT = 220;
const MOVE = 420;
const STAGGER = 50;
const EASE = "cubic-bezier(0.32, 0.72, 0.24, 1)";
/** The beat stops after this many cards – a big tree doesn't trickle in for seconds */
const BEATS = 8;
/** A card rises a moment after its connector has started to draw */
const AFTER_LINK = 90;

type Link = { id: string; d: string; muted: boolean };
type Dot = { id: string; at: [number, number]; muted: boolean };
type Geo = {
  key: string | null;
  links: Link[];
  dots: Dot[];
  w: number;
  h: number;
};

// One shared second for every running clock – only the times render again, not the whole fleet
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
/** The time an agent took – while it works, it counts on from `since` (or follows the fleet's `now`) */
const Took = ({ since, until, now }: { since: number; until?: number; now?: number }) => {
  const tick = useNow(until === undefined && now === undefined);
  return <>{took((until ?? now ?? tick) - since)}</>;
};

export function AgentFleet({ agents, open, defaultOpen, onOpenChange, onSelect, onResolve, now: nowProp, formatCost = dollars, title = "Agents", highlight, className, labels: labelsIn }: AgentFleetProps) {
  const L = { ...LABELS, ...labelsIn, status: { ...LABELS.status, ...labelsIn?.status } };
  const STATUS = L.status;
  const uid = useId();
  const [innerOpen, setInnerOpen] = useState<string | null>(() => (defaultOpen === undefined ? (agents[0]?.id ?? null) : defaultOpen));
  // A pointed-at agent opens its lead's tree for as long as it's pointed at
  const lead = highlight ? agents.find((a) => walk(a).some((x) => x.id === highlight)) : undefined;
  const pathTo = (a: FleetAgent): string[] | null => (a.id === highlight ? [a.id] : (a.children ?? []).reduce<string[] | null>((found, k) => found ?? (pathTo(k) ? [a.id, ...pathTo(k)!] : null), null));
  const kept = new Set(lead ? (pathTo(lead) ?? []) : []);
  const faded = (id: string) => (lead && !kept.has(id) ? "" : undefined);
  const openId = lead ? lead.id : open !== undefined ? open : innerOpen;
  // A second click on the open agent folds its tree
  const choose = (id: string) => {
    const next = openId === id ? null : id;
    setInnerOpen(next);
    onOpenChange?.(next);
  };

  /* ---------- Which tree is shown: the open one – but only once the last one has folded back ---------- */

  const [shown, setShown] = useState<string | null>(openId);
  const [leaving, setLeaving] = useState(false);
  const shownAgent = agents.find((a) => a.id === shown) ?? null;
  const subs = shownAgent ? walk(shownAgent).slice(1) : [];
  const exitTotal = EXIT + Math.min(Math.max(0, subs.length - 1), BEATS) * STAGGER;
  const timer = useRef(0);
  useEffect(() => {
    window.clearTimeout(timer.current);
    if (openId === shown) {
      setLeaving(false);
      return;
    }
    if (!subs.length || reduced()) {
      setShown(openId);
      setLeaving(false);
      return;
    }
    setLeaving(true);
    timer.current = window.setTimeout(() => {
      setShown(openId);
      setLeaving(false);
    }, exitTotal + 40);
  }, [openId, shown]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => () => window.clearTimeout(timer.current), []);

  // A new tree grows in card by card; a sub-agent that turns up later in the same tree rises in on its own
  const ent = useRef<{
    key: string | null;
    delays: Map<string, number>;
    settled: boolean;
  }>({ key: null, delays: new Map(), settled: false });
  if (ent.current.key !== shown) ent.current = { key: shown, delays: new Map(), settled: false };
  let fresh = 0;
  for (const s of subs) if (!ent.current.delays.has(s.id)) ent.current.delays.set(s.id, ent.current.settled ? 0 : Math.min(fresh++, BEATS) * STAGGER);
  const enterOf = (id: string) => ent.current.delays.get(id) ?? 0;
  // Leaving goes bottom first
  const leaveOf = new Map(subs.map((s, i) => [s.id, Math.min(subs.length - 1 - i, BEATS) * STAGGER]));

  /* ---------- The tasks window of one sub-agent: a click elsewhere, Esc or another tree closes it ---------- */

  const [pop, setPop] = useState<string | null>(null);
  // Opened with the keyboard: focus goes to the main answer – with a click it stays where the pointer is
  const byKey = useRef(false);
  // The column is one tab stop: the agent you last focused (or the open one) – arrows move within it, Tab moves on into the tree
  const [stop, setStop] = useState<string | null>(null);
  useEffect(() => setPop(null), [openId, shown]);
  useEffect(() => {
    if (!pop) return;
    const away = (e: PointerEvent) => {
      if (!nodes.current.get(pop)?.contains(e.target as Node)) setPop(null);
    };
    const esc = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      nodes.current.get(pop)?.focus();
      setPop(null);
    };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [pop]);

  /* ---------- Wide or narrow: the tree to the right, or under the card ---------- */

  const rootRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const branchRef = useRef<HTMLDivElement>(null);
  const nodes = useRef(new Map<string, HTMLElement>());
  const keep = (id: string) => (el: HTMLElement | null) => {
    if (el) nodes.current.set(id, el);
    else nodes.current.delete(id);
  };
  const [wide, setWide] = useState(true);
  // The deepest level of any tree – the fleet is as wide as the column, the gap and that tree
  const deepest = Math.max(1, ...agents.map((a) => Math.max(0, ...walk(a).map((x) => depthIn(a, x.id)))));

  /* ---------- The tree's place beside the column, and the connectors – all measured from the cards ---------- */

  const [geo, setGeo] = useState<Geo>({
    key: null,
    links: [],
    dots: [],
    w: 0,
    h: 0,
  });
  const drawn = useRef("");
  const measure = () => {
    const body = bodyRef.current;
    const list = listRef.current;
    if (!body || !list) return;

    // Wide: the tree stands centered on its agent, as far as the column reaches
    const branch = branchRef.current;
    const from = shown ? nodes.current.get(shown) : undefined;
    if (wide && branch && from) {
      const want = from.offsetTop + from.offsetHeight / 2 - branch.offsetHeight / 2;
      const top = `${Math.round(Math.max(0, Math.min(want, list.offsetHeight - branch.offsetHeight)))}px`;
      if (branch.style.marginTop !== top) {
        // A tree that grows while it's open glides to its new place; a new tree just stands there
        const glide = branch.style.marginTop !== "" && !reduced();
        branch.style.transition = glide ? `margin-top ${MOVE}ms ${EASE}` : "none";
        branch.style.marginTop = top;
        if (glide) follow(MOVE + 40);
      }
    }

    const links: Link[] = [];
    const dots: Dot[] = [];
    const R = 8;
    const visit = (a: FleetAgent, pe: HTMLElement, side: boolean) => {
      const kids = a.children ?? [];
      if (!kids.length) return;
      const p = place(pe, body);
      let live = false;
      for (const k of kids) {
        const ce = nodes.current.get(k.id);
        if (!ce) continue;
        const q = place(ce, body);
        if (!muted(k.status)) live = true;
        let d: string;
        if (side) {
          // To the right: out of the card's middle, along to halfway, then up or down into the sub-agent's first row
          const x1 = p.x + p.w;
          const y1 = p.y + p.h / 2;
          const x2 = q.x;
          const y2 = q.y + 21;
          const xm = Math.round(x1 + (x2 - x1) / 2);
          const dy = y2 - y1;
          const r = Math.max(0, Math.min(R, Math.abs(dy) / 2, (x2 - x1) / 4));
          const s = Math.sign(dy);
          d = Math.abs(dy) < 1 ? `M${x1} ${y1}H${x2}` : `M${x1} ${y1}H${xm - r}Q${xm} ${y1} ${xm} ${y1 + s * r}V${y2 - s * r}Q${xm} ${y2} ${xm + r} ${y2}H${x2}`;
        } else {
          // Hanging below: down along the parent's left edge, then into the child's side
          const x1 = p.x + 21;
          const y1 = p.y + p.h;
          const x2 = q.x;
          const y2 = q.y + 21;
          const r = Math.max(0, Math.min(R, (x2 - x1) / 2, (y2 - y1) / 2));
          d = x2 - x1 < 2 ? `M${x1} ${y1}V${q.y}` : `M${x1} ${y1}V${y2 - r}Q${x1} ${y2} ${x1 + r} ${y2}H${x2}`;
        }
        links.push({ id: k.id, d, muted: muted(k.status) });
        visit(k, ce, false);
      }
      dots.push({
        id: a.id,
        at: side ? [p.x + p.w, p.y + p.h / 2] : [p.x + 21, p.y + p.h],
        muted: !live,
      });
    };
    if (shownAgent && from) visit(shownAgent, from, wide);
    const next: Geo = {
      key: shown,
      links,
      dots,
      w: body.offsetWidth,
      h: body.offsetHeight,
    };
    // Measured every frame while things move – but only a real change renders again
    const key = JSON.stringify(next);
    if (key !== drawn.current) {
      drawn.current = key;
      setGeo(next);
    }
  };
  const latest = useRef(measure);
  latest.current = measure;

  // While the tree glides or the narrow branch opens, everything is measured every frame
  const until = useRef(0);
  const raf = useRef(0);
  const follow = (ms: number) => {
    until.current = Math.max(until.current, performance.now() + ms);
    if (raf.current) return;
    const loop = () => {
      latest.current();
      raf.current = performance.now() < until.current ? requestAnimationFrame(loop) : 0;
    };
    raf.current = requestAnimationFrame(loop);
  };
  useEffect(
    () => () => {
      cancelAnimationFrame(raf.current);
      raf.current = 0;
    },
    [],
  );

  useLayoutEffect(() => {
    measure();
    ent.current.settled = true;
  });
  // A new tree in the narrow layout opens its height – follow it
  useLayoutEffect(() => {
    if (!wide) follow(480);
  }, [shown, wide]); // eslint-disable-line react-hooks/exhaustive-deps
  // One observer for size changes: it picks the layout and measures again
  useLayoutEffect(() => {
    const root = rootRef.current;
    const body = bodyRef.current;
    if (!root || !body) return;
    const fit = () => {
      const cs = getComputedStyle(root);
      const v = (name: string, fallback: number) => parseFloat(cs.getPropertyValue(name)) || fallback;
      // Side by side while the column, the gap and the deepest tree all fit – the fleet is never wider than that
      setWide(root.offsetWidth >= v("--ui-agfc-main-w", 288) + v("--ui-agfc-gap", 56) + v("--ui-agfc-card-w", 240) + v("--ui-agfc-indent", 32) * (deepest - 1));
      latest.current();
    };
    // Once before the first paint – a phone never sees the wide layout flash by
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(root);
    ro.observe(body);
    return () => ro.disconnect();
  }, [deepest]);

  /* ---------- The cards ---------- */

  // What it does – led by the tools it uses as small overlapping tiles
  const task = (a: FleetAgent) => (
    <span className="ui-agfc__task" title={a.task}>
      {a.tools && a.tools.length > 0 && (
        <span className="ui-agfc__tools" aria-hidden="true">
          {(a.tools.length > 4 ? a.tools.slice(0, 3) : a.tools).map((tool, i) => (
            <span key={i} className="ui-agfc__tool">
              {tool}
            </span>
          ))}
          {a.tools.length > 4 && <span className="ui-agfc__tool ui-agfc__tool--more">+{a.tools.length - 3}</span>}
        </span>
      )}
      <span className="ui-agfc__task-text" dir="auto">
        {a.task}
      </span>
    </span>
  );
  // Steve: with no answers taken here, what it asks or why it failed shows on its card
  const said = (a: FleetAgent) =>
    !onResolve && ((a.status === "waiting" && a.ask) || (a.status === "failed" && a.error)) ? (
      <span className="ui-agfc__said" data-kind={a.status} dir="auto" title={a.status === "waiting" ? a.ask : a.error}>
        {a.status === "waiting" ? a.ask : a.error}
      </span>
    ) : null;

  const top = (a: FleetAgent) => (
    <span className="ui-agfc__top">
      <span className="ui-agfc__avatar" data-logo={a.avatar ? "" : undefined}>
        {a.avatar ?? initials(a.name)}
      </span>
      <span className="ui-agfc__who">
        <span className="ui-agfc__name" dir="auto" title={a.name}>
          {a.name}
        </span>
        {a.model && (
          <span className="ui-agfc__model" title={a.model}>
            {a.model}
          </span>
        )}
      </span>
      <span className="ui-agfc__status" data-status={a.status}>
        <StatusIcon status={a.status} />
        {a.statusText ?? STATUS[a.status]}
      </span>
    </span>
  );
  const spent = (a: FleetAgent) => {
    const end = a.ended ?? (a.status === "running" || a.status === "waiting" ? undefined : a.started);
    return (
      <span className="ui-agfc__cost" title={a.children?.length ? L.branch : undefined}>
        <span className="ui-agfc__amount" data-live={walk(a).some((x) => x.status === "running") ? "" : undefined}>
          {formatCost(costOf(a))}
        </span>
        {a.time !== undefined ? (
          a.time ? <span className="ui-agfc__time">{" · "}{a.time}</span> : null
        ) : a.started ? (
          <span className="ui-agfc__time">
            {" · "}
            <Took since={a.started} until={end} now={nowProp} />
          </span>
        ) : null}
      </span>
    );
  };

  // Up and down move between the main agents
  const step = (e: KeyboardEvent, i: number) => {
    const to = e.key === "ArrowDown" ? i + 1 : e.key === "ArrowUp" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? agents.length - 1 : -1;
    if (to < 0 || to >= agents.length || to === i) return;
    e.preventDefault();
    nodes.current.get(agents[to].id)?.focus();
  };

  const main = (a: FleetAgent, i: number) => {
    const say = sayOf(a, L);
    const all = walk(a).slice(1);
    return (
      <button
        type="button"
        className="ui-agfc__main"
        {...a.attrs}
        data-status={a.status}
        data-faded={faded(a.id)}
        data-open={a.id === openId ? "" : undefined}
        aria-expanded={a.id === openId}
        aria-controls={a.id === shown ? `${uid}-tree` : undefined}
        ref={keep(a.id)}
        onClick={() => choose(a.id)}
        onKeyDown={(e) => step(e, i)}
        onFocus={() => setStop(a.id)}
        tabIndex={a.id === (agents.some((x) => x.id === stop) ? stop : openId && agents.some((x) => x.id === openId) ? openId : agents[0]?.id) ? 0 : -1}
      >
        {top(a)}
        {task(a)}
        <span className="ui-agfc__foot">
          <span className="ui-agfc__say" data-kind={say.kind}>
            {/* How far the sub-agents are, as in Linear's "In progress": a ring with a pie inside that fills clockwise – gray, like the phrase */}
            {all.length > 0 && (
              <svg className="ui-agfc__ring" width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
                <circle cx="6" cy="6" r="5.25" className="ui-agfc__ring-edge" />
                {/* The pie is a fat stroke on a tiny circle – its dash is the share that's through */}
                <circle
                  cx="6"
                  cy="6"
                  r="1.75"
                  pathLength={1}
                  className="ui-agfc__ring-pie"
                  style={{
                    strokeDashoffset: 1 - all.filter((s) => s.status === "done" || s.status === "failed" || s.status === "stopped").length / all.length,
                  }}
                />
              </svg>
            )}
            <span className="ui-agfc__say-text">{say.text}</span>
          </span>
          {spent(a)}
        </span>
      </button>
    );
  };

  const sub = (a: FleetAgent) => {
    const gone = leaving;
    const steps = a.steps ?? [];
    // Waiting or failed: something to answer in the window, when the fleet takes answers
    const stuck = !!onResolve && (a.status === "waiting" || a.status === "failed");
    const asks = a.status === "waiting";
    const canPop = steps.length > 0 || stuck;
    const popped = pop === a.id && !gone;
    const live = !gone && (!!onSelect || canPop);
    const press = (key = false) => {
      byKey.current = key;
      onSelect?.(a);
      if (canPop) setPop((p) => (p === a.id ? null : a.id));
    };
    const finished = steps.filter((s) => (s.status ?? "done") === "done").length;
    return (
      <div
        className="ui-agfc__sub"
        {...a.attrs}
        data-status={a.status}
        data-faded={faded(a.id)}
        data-leaving={gone ? "" : undefined}
        data-popped={popped ? "" : undefined}
        style={{
          animationDelay: `${gone ? (leaveOf.get(a.id) ?? 0) : enterOf(a.id) + AFTER_LINK}ms`,
        }}
        ref={keep(a.id)}
        onClick={live ? () => press() : undefined}
        tabIndex={live ? 0 : undefined}
        aria-haspopup={canPop ? "dialog" : undefined}
        aria-expanded={canPop ? popped : undefined}
        onKeyDown={
          live
            ? (e) => {
                if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
                  e.preventDefault();
                  press(true);
                }
              }
            : undefined
        }
        data-clickable={live ? "" : undefined}
      >
        {top(a)}
        {task(a)}
        {said(a)}
        <div className="ui-agfc__meta">
          {spent(a)}
          {/* Stuck: a small button in place of the task count – the card keeps its height */}
          {stuck ? (
            <button
              type="button"
              className="ui-agfc__review"
              tabIndex={-1}
              aria-hidden="true"
              onClick={(e) => {
                e.stopPropagation();
                byKey.current = false;
                setPop(a.id);
              }}
            >
              {asks ? L.answer : L.resolve}
            </button>
          ) : (
            steps.length > 0 && <span className="ui-agfc__count">{L.tasks(steps.length)}</span>
          )}
        </div>

        {/* The tasks it worked through – a small window under the card; first what it needs from you */}
        {popped && (
          <div className="ui-agfc__pop" role="dialog" aria-label={stuck ? `${a.name}: ${STATUS[a.status]}` : `${L.tasksTitle}: ${a.name}`} onClick={(e) => e.stopPropagation()}>
            {stuck && (
              <div className="ui-agfc__ask">
                <span className="ui-agfc__ask-label">{asks ? L.needsAnswer : L.failedWord}</span>
                <p className="ui-agfc__ask-text" dir="auto">
                  {(asks ? a.ask : a.error) ?? (asks ? L.waitsDefault : L.errorDefault)}
                </p>
                <span className="ui-agfc__ask-actions">
                  <button type="button" className="ui-agfc__btn" onClick={() => onResolve!(a, asks ? "reject" : "dismiss")}>
                    {asks ? L.reject : L.dismiss}
                  </button>
                  <button type="button" className="ui-agfc__btn ui-agfc__btn--primary" autoFocus={byKey.current} onClick={() => onResolve!(a, asks ? "approve" : "retry")}>
                    {asks ? L.approve : L.retry}
                  </button>
                </span>
              </div>
            )}
            {steps.length > 0 && (
              <>
                <div className="ui-agfc__pop-head">
                  <span>{L.tasksTitle}</span>
                  <span>
                    {L.doneOf(finished, steps.length)}
                  </span>
                </div>
                <ol className="ui-agfc__steps">
                  {steps.map((s, i) => {
                    const st = s.status ?? "done";
                    return (
                      <li
                        key={i}
                        className="ui-agfc__step"
                        data-status={st}
                        style={{
                          animationDelay: `${60 + Math.min(i, BEATS) * 30}ms`,
                        }}
                      >
                        {s.tool && (
                          <span className="ui-agfc__tool" aria-hidden="true">
                            {s.tool}
                          </span>
                        )}
                        <span className="ui-agfc__step-title" dir="auto" title={s.title}>
                          {s.title}
                        </span>
                        {/* Done shows how long it took; everything else says where it stands */}
                        <span className="ui-agfc__step-end">
                          {st === "done" ? (
                            <>
                              <span className="ui-agfc__sr">{L.done}, </span>
                              {s.took !== undefined ? took(s.took) : L.done}
                            </>
                          ) : st === "running" ? (
                            <span className="ui-agfc__amount" data-live="">
                              {L.runningWord}
                            </span>
                          ) : st === "stopped" ? (
                            L.skipped
                          ) : (
                            STATUS[st]
                          )}
                        </span>
                      </li>
                    );
                  })}
                </ol>
              </>
            )}
          </div>
        )}
      </div>
    );
  };

  /** A sub-agent with everything that hangs below it */
  const hang = (a: FleetAgent): ReactNode => (
    <div className="ui-agfc__hang" key={a.id} role="treeitem" aria-expanded={a.children?.length ? true : undefined} aria-label={`${a.name}: ${STATUS[a.status]}`}>
      {sub(a)}
      {a.children && a.children.length > 0 && (
        <div className="ui-agfc__below" role="group">
          {a.children.map(hang)}
        </div>
      )}
    </div>
  );

  /** The open tree – to the right when wide, under its card when narrow */
  const tree = (a: FleetAgent) => (
    <div
      key={a.id}
      ref={branchRef}
      id={`${uid}-tree`}
      className="ui-agfc__branch"
      role="tree"
      aria-label={L.subsOf(a.name)}
      data-leaving={leaving ? "" : undefined}
      style={{ ["--ui-agfc-exit" as string]: `${exitTotal}ms` }}
      // Once a narrow branch has opened, a tasks window may reach out of it
      onAnimationEnd={(e) => {
        if (e.target === e.currentTarget && !leaving) e.currentTarget.dataset.settled = "";
      }}
    >
      <div className="ui-agfc__branch-in">{a.children?.length ? a.children.map(hang) : <p className="ui-agfc__empty">{L.noSubsYet}</p>}</div>
    </div>
  );

  /* ---------- The whole fleet in numbers ---------- */

  const everyone = agents.flatMap(walk);
  const running = everyone.filter((a) => a.status === "running").length;
  const waiting = everyone.filter((a) => a.status === "waiting").length;
  const total = agents.reduce((sum, a) => sum + costOf(a), 0);
  const subCount = everyone.length - agents.length;

  return (
    <div ref={rootRef} className={["ui-agfc", className].filter(Boolean).join(" ")} data-wide={wide ? "" : undefined} style={{ ["--ui-agfc-depth" as string]: deepest }}>
      <style href="ui-agent-fleet-clean" precedence="default">
        {css}
      </style>

      <header className="ui-agfc__bar">
        <span className="ui-agfc__spark">
          <Spark />
        </span>
        <span className="ui-agfc__title" dir="auto" title={title}>
          {title}
        </span>
        <span className="ui-agfc__sum" aria-live="polite">
          <span>
            {L.agents(agents.length)}
            {subCount > 0 && ` / ${L.subs(subCount)}`}
          </span>
          {running > 0 && <span>{L.runningN(running)}</span>}
          {waiting > 0 && <span>{L.needsYouN(waiting)}</span>}
          <span className="ui-agfc__sum-cost">
            <span className="ui-agfc__amount" data-live={running > 0 ? "" : undefined}>
              {formatCost(total)}
            </span>
          </span>
        </span>
      </header>

      <div ref={bodyRef} className="ui-agfc__body" data-pop={pop ? "" : undefined}>
        <svg className="ui-agfc__links" width={geo.w} height={geo.h} aria-hidden="true">
          {geo.key === shown &&
            geo.links.map((l) => (
              <path
                key={`${geo.key}:${l.id}`}
                d={l.d}
                pathLength={1}
                className="ui-agfc__link"
                data-muted={l.muted ? "" : undefined}
                data-leaving={leaving ? "" : undefined}
                style={{
                  animationDelay: `${leaving ? (leaveOf.get(l.id) ?? 0) : enterOf(l.id)}ms`,
                }}
              />
            ))}
        </svg>

        <div ref={listRef} className="ui-agfc__list" data-picked={openId ? "" : undefined}>
          {agents.length === 0 && <p className="ui-agfc__none">{L.noAgents}</p>}
          {agents.map((a, i) => (
            <Fragment key={a.id}>
              {main(a, i)}
              {!wide && a.id === shown && tree(a)}
            </Fragment>
          ))}
        </div>
        {wide && <div className="ui-agfc__side">{shownAgent && tree(shownAgent)}</div>}

        <svg className="ui-agfc__links ui-agfc__dots" width={geo.w} height={geo.h} aria-hidden="true">
          {/* One hollow dot where the connectors leave a card – on top of its edge */}
          {geo.key === shown &&
            geo.dots.map((d) => (
              <circle
                key={`${geo.key}:${d.id}`}
                cx={d.at[0]}
                cy={d.at[1]}
                r={3.25}
                className="ui-agfc__dot"
                data-muted={d.muted ? "" : undefined}
                data-leaving={leaving ? "" : undefined}
                style={leaving ? { animationDelay: `${exitTotal - EXIT}ms` } : undefined}
              />
            ))}
        </svg>
      </div>
    </div>
  );
}

/** How deep an agent sits under a main agent – the main agent's sub-agents are 1 */
const depthIn = (a: FleetAgent, id: string, d = 0): number => {
  if (a.id === id) return d;
  for (const c of a.children ?? []) {
    const found = depthIn(c, id, d + 1);
    if (found >= 0) return found;
  }
  return -1;
};

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-agfc */
.ui-agfc {
  --ui-agfc-card: #ffffff;
  --ui-agfc-strong: var(--aui-ink, #000000);
  --ui-agfc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-agfc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-agfc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-agfc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-agfc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-agfc-fill: var(--aui-subtle, rgba(0, 0, 0, 0.06));
  --ui-agfc-accent: rgba(0, 96, 226, 0.765);
  /* The connectors: the soft blue of Agent Tree */
  --ui-agfc-link: #83c7fe;
  --ui-agfc-link-muted: rgba(0, 0, 0, 0.12);
  --ui-agfc-brand-bg: #008ff519;
  --ui-agfc-brand-fg: #006dcbf2;
  --ui-agfc-ok-bg: #00a43319;
  --ui-agfc-ok-fg: #007d2ee0;
  --ui-agfc-warn-bg: #ff9c0029;
  --ui-agfc-warn-fg: var(--aui-warning, #b25e00);
  --ui-agfc-danger-bg: #f52b0018;
  --ui-agfc-danger-fg: #d72400da;
  /* The rows of the tasks window sit on a soft gray */
  --ui-agfc-well: var(--aui-subtle, rgba(0, 0, 0, 0.035));
  /* The agents that aren't open */
  --ui-agfc-dim: 0.5;
  --ui-agfc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-agfc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-agfc-on-solid: var(--aui-on-primary, #fcfcfc);
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-agfc-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-agfc-hover: 0 0 0 0.5px rgba(0, 0, 0, 0.1), 0 2px 6px -2px rgba(0, 0, 0, 0.12);
  --ui-agfc-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 12px 28px -10px rgba(0, 0, 0, 0.18), 0 1px 2px rgba(0, 0, 0, 0.05));
  --ui-agfc-main-w: 288px;
  --ui-agfc-card-w: 240px;
  --ui-agfc-gap: 56px;
  --ui-agfc-indent: 32px;
  --ui-agfc-shine: rgba(0, 0, 0, 0.22);
  --ui-agfc-ease: ${EASE};
  position: relative;
  box-sizing: border-box;
  width: 100%;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-agfc-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-agfc *, .ui-agfc *::before, .ui-agfc *::after { box-sizing: border-box; }
/* Wide: no wider than the column, the gap and the deepest tree – so the bar ends where the cards do */
.ui-agfc[data-wide] { max-width: calc(var(--ui-agfc-main-w) + var(--ui-agfc-gap) + var(--ui-agfc-card-w) + var(--ui-agfc-indent) * (var(--ui-agfc-depth, 1) - 1)); }
:where(.ui-agfc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-agfc p { margin: 0; }
.ui-agfc svg { display: block; flex: none; }

/* ----- The bar: title and the whole fleet in numbers ----- */
.ui-agfc__bar { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; min-width: 0; margin-bottom: 16px; }
.ui-agfc__spark { display: grid; flex: none; place-items: center; width: 16px; color: var(--ui-agfc-accent); }
/* The title gives way first – the numbers only wrap when even they don't fit */
.ui-agfc__title { flex: 1 1 0; min-width: min(120px, 100%); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-weight: 500; color: var(--ui-agfc-strong); }
.ui-agfc__sum { display: flex; flex-wrap: wrap; justify-content: flex-end; align-items: center; gap: 4px 10px; margin-left: auto; font-size: 12px; line-height: 16px; color: var(--ui-agfc-muted); font-variant-numeric: tabular-nums; }
.ui-agfc__sum-cost { min-width: 44px; color: var(--ui-agfc-ink); font-weight: 500; text-align: right; }

/* ----- The body: the column of main agents, and the open tree beside it ----- */
.ui-agfc__body { position: relative; display: grid; grid-template-columns: var(--ui-agfc-main-w) minmax(0, 1fr); column-gap: var(--ui-agfc-gap); align-items: start; }
.ui-agfc:not([data-wide]) .ui-agfc__body { grid-template-columns: minmax(0, 1fr); }
.ui-agfc__links { position: absolute; top: 0; left: 0; z-index: 0; overflow: visible; pointer-events: none; }
.ui-agfc__dots { z-index: 3; }
.ui-agfc__list { position: relative; z-index: 1; display: grid; gap: 8px; min-width: 0; }
/* No agents at all: one quiet line across the whole fleet */
.ui-agfc__list:has(> .ui-agfc__none) { grid-column: 1 / -1; }
.ui-agfc__none { padding: 28px 12px; border-radius: var(--aui-radius-lg, 10px); box-shadow: inset 0 0 0 0.5px var(--ui-agfc-line); font-size: 12px; line-height: 16px; color: var(--ui-agfc-muted); text-align: center; }
.ui-agfc__side { position: relative; z-index: 1; min-width: 0; }

/* One main agent: a white card that opens its tree */
.ui-agfc__main { position: relative; display: grid; gap: 6px; width: 100%; min-width: 0; padding: 10px 10px 8px; border-radius: var(--aui-radius-lg, 10px); background: var(--aui-surface, var(--ui-agfc-card)); box-shadow: var(--aui-shadow-card, var(--ui-agfc-shadow)); transition: box-shadow 200ms ease, opacity 320ms var(--ui-agfc-ease); }
.ui-agfc__main:hover { box-shadow: var(--ui-agfc-hover); }
.ui-agfc__main:focus-visible { outline: 2px solid var(--ui-agfc-focus); outline-offset: 2px; }
/* While one agent is open, the others step back – and come forward again under the pointer */
.ui-agfc__list[data-picked] > .ui-agfc__main:not([data-open]) { opacity: var(--ui-agfc-dim); }
.ui-agfc__list[data-picked] > .ui-agfc__main:not([data-open]):is(:hover, :focus-visible) { opacity: 0.85; }
.ui-agfc__foot { display: flex; align-items: center; gap: 8px; min-width: 0; min-height: 20px; }
/* The progress circle sits on the phrase's text line */
.ui-agfc__say { display: inline-flex; flex: 0 1 auto; align-items: center; gap: 5px; min-width: 0; font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-muted); font-variant-numeric: tabular-nums; }
.ui-agfc__say-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
/* Text and circle stay gray – the phrase says what needs you */
.ui-agfc__ring { transform: rotate(-90deg); }
.ui-agfc__ring-edge { fill: none; stroke: currentColor; stroke-width: 1.5; }
.ui-agfc__ring-pie { fill: none; stroke: currentColor; stroke-width: 3.5; stroke-dasharray: 1 1; transition: stroke-dashoffset 600ms var(--ui-agfc-ease); }
.ui-agfc__foot .ui-agfc__cost { flex: none; margin-left: auto; }

/* The open tree: wide, beside the column; narrow, under its card */
.ui-agfc__branch-in { display: grid; gap: 10px; justify-items: start; }
.ui-agfc__hang { display: grid; align-content: start; gap: 10px; }
.ui-agfc__below { display: grid; gap: 10px; padding-left: var(--ui-agfc-indent); }
.ui-agfc__sub { width: var(--ui-agfc-card-w); }
:is(.ui-agfc__main, .ui-agfc__sub)[data-faded] { opacity: var(--ui-agfc-dim) !important; }
.ui-agfc__empty { padding: 10px 2px; font-size: 12px; line-height: 16px; color: var(--ui-agfc-muted); animation: ui-agfc-fade-in 240ms ease both; }
.ui-agfc:not([data-wide]) .ui-agfc__branch { display: grid; grid-template-rows: 1fr; padding-left: var(--ui-agfc-indent); animation: ui-agfc-open 360ms var(--ui-agfc-ease) both; }
.ui-agfc:not([data-wide]) .ui-agfc__branch[data-leaving] { animation: ui-agfc-close var(--ui-agfc-exit, 300ms) var(--ui-agfc-ease) forwards; }
/* A little room around the cards, so their hairlines aren't clipped while the branch opens */
.ui-agfc:not([data-wide]) .ui-agfc__branch-in { min-height: 0; overflow: hidden; margin: -2px -4px; padding: 2px 4px; justify-items: stretch; }
.ui-agfc:not([data-wide]) .ui-agfc__branch[data-settled]:not([data-leaving]) > .ui-agfc__branch-in { overflow: visible; }
.ui-agfc:not([data-wide]) .ui-agfc__pop { width: 100%; }
.ui-agfc:not([data-wide]) .ui-agfc__sub { width: auto; }

/* Steve: what a waiting sub-agent asks or why a failed one stopped, on its card */
.ui-agfc__said { display: -webkit-box; margin-top: 4px; overflow: hidden; -webkit-line-clamp: 2; -webkit-box-orient: vertical; font-size: 12px; line-height: 17px; }
.ui-agfc__said[data-kind="waiting"] { color: var(--ui-agfc-warn-fg); }
.ui-agfc__said[data-kind="failed"] { color: var(--ui-agfc-danger-fg); }
/* One sub-agent: a small white card that rises in a moment after its connector */
.ui-agfc__sub { position: relative; display: grid; gap: 6px; min-width: 0; padding: 10px 10px 8px; border-radius: calc(var(--aui-radius-lg, 10px) - 2px); background: var(--aui-surface, var(--ui-agfc-card)); box-shadow: var(--aui-shadow-card, var(--ui-agfc-shadow)); animation: ui-agfc-rise 360ms var(--ui-agfc-ease) both; transition: box-shadow 200ms ease, opacity 240ms ease; }
.ui-agfc__sub[data-leaving] { animation: ui-agfc-leave ${EXIT}ms var(--ui-agfc-ease) both; pointer-events: none; }
.ui-agfc__sub[data-clickable] { cursor: pointer; }
.ui-agfc__sub[data-clickable]:hover { box-shadow: var(--ui-agfc-hover); }
.ui-agfc__sub:focus-visible { outline: 2px solid var(--ui-agfc-focus); outline-offset: 2px; }
/* Queued and stopped cards step back – their content, not the card, so the tasks window over it stays solid */
.ui-agfc__sub:is([data-status="queued"], [data-status="stopped"]) > :not(.ui-agfc__pop) { opacity: 0.6; }

/* What both cards share */
.ui-agfc__top { display: flex; align-items: center; gap: 8px; min-width: 0; }
.ui-agfc__avatar { display: grid; flex: none; place-items: center; width: 22px; height: 22px; border-radius: 6px; background: var(--ui-agfc-card); box-shadow: var(--ui-agfc-shadow); font-size: 10px; font-weight: 500; color: var(--ui-agfc-text); }
.ui-agfc__avatar[data-logo] > svg { width: 13px; height: 13px; }
.ui-agfc__who { display: grid; flex: 1; min-width: 0; }
.ui-agfc__name { overflow: hidden; font-weight: 500; line-height: 16px; color: var(--ui-agfc-strong); white-space: nowrap; text-overflow: ellipsis; }
.ui-agfc__model { overflow: hidden; font-size: 11px; line-height: 14px; color: var(--ui-agfc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-agfc__status { display: inline-flex; flex: none; align-self: start; align-items: center; gap: 4px; height: 18px; padding: 0 6px 0 4px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-agfc-fill); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-text); white-space: nowrap; }
.ui-agfc__status[data-status="running"] { background: var(--ui-agfc-brand-bg); color: var(--ui-agfc-brand-fg); }
.ui-agfc__status[data-status="waiting"] { background: var(--ui-agfc-warn-bg); color: var(--ui-agfc-warn-fg); }
.ui-agfc__status[data-status="done"] { background: var(--ui-agfc-ok-bg); color: var(--ui-agfc-ok-fg); }
.ui-agfc__status[data-status="failed"] { background: var(--ui-agfc-danger-bg); color: var(--ui-agfc-danger-fg); }
.ui-agfc__spin { animation: ui-agfc-spin 900ms linear infinite; }
/* One line of what it does, led by its tools – every card keeps its height */
.ui-agfc__task { display: flex; align-items: center; gap: 6px; min-width: 0; font-size: 12px; line-height: 16px; color: var(--ui-agfc-text); }
.ui-agfc__task-text { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
/* The tools: small white tiles, each slightly over the one before */
.ui-agfc__tools { display: inline-flex; flex: none; }
.ui-agfc__tool { display: grid; flex: none; place-items: center; width: 16px; height: 16px; border-radius: 4px; background: var(--ui-agfc-card); box-shadow: var(--ui-agfc-shadow); }
.ui-agfc__tool + .ui-agfc__tool { margin-left: -4px; }
.ui-agfc__tool > svg { width: 10px; height: 10px; }
.ui-agfc__tool--more { width: auto; min-width: 16px; padding: 0 3px; font-size: 9px; font-weight: 600; line-height: 16px; color: var(--ui-agfc-muted); font-variant-numeric: tabular-nums; }
.ui-agfc__meta { display: flex; align-items: center; gap: 8px; min-height: 20px; }
/* A sub-agent that needs you or failed: a small button opens the window with the answers */
.ui-agfc__review { flex: none; display: inline-flex; align-items: center; height: 20px; margin: 0 -2px 0 auto; padding: 0 8px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-agfc-card); box-shadow: var(--ui-agfc-shadow); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-ink); transition: background-color 160ms ease; }
.ui-agfc__review:hover { background: var(--ui-agfc-fill); }
.ui-agfc__sub[data-popped] .ui-agfc__review { background: var(--ui-agfc-fill); }
/* What it needs from you, at the top of the window, with the two answers */
.ui-agfc__ask { display: grid; gap: 4px; padding: 4px 6px 8px; }
.ui-agfc__ask + .ui-agfc__pop-head { margin-top: 2px; padding-top: 8px; border-top: 0.5px solid var(--ui-agfc-line); }
.ui-agfc__ask-label { font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-ink); }
.ui-agfc__ask-text { overflow-wrap: anywhere; font-size: 12px; line-height: 16px; color: var(--ui-agfc-text); }
.ui-agfc__ask-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 6px; }
/* The answers are as small as the card's Answer button */
.ui-agfc__btn { display: inline-flex; align-items: center; height: 20px; padding: 0 8px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-agfc-card); box-shadow: var(--ui-agfc-shadow); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-ink); transition: background-color 160ms ease, opacity 160ms ease; }
.ui-agfc__btn:hover { background: var(--ui-agfc-fill); }
.ui-agfc__btn--primary { background: var(--ui-agfc-solid); box-shadow: none; color: var(--ui-agfc-on-solid); }
.ui-agfc__btn--primary:hover { background: var(--ui-agfc-solid); opacity: 0.88; }
.ui-agfc button:focus-visible { outline: 2px solid var(--ui-agfc-focus); outline-offset: 2px; }
.ui-agfc__count { flex: none; margin-left: auto; font-size: 11px; line-height: 16px; color: var(--ui-agfc-muted); font-variant-numeric: tabular-nums; }

/* The tasks window: drops down from the sub-agent, over everything below it */
.ui-agfc__body[data-pop] > :is(.ui-agfc__list, .ui-agfc__side) { z-index: 4; }
.ui-agfc__sub[data-popped] { z-index: 5; box-shadow: var(--ui-agfc-hover); }
.ui-agfc__pop { position: absolute; top: calc(100% + 6px); left: 0; z-index: 5; display: grid; gap: 2px; width: max(100%, 312px); max-width: calc(100vw - 32px); max-height: min(380px, 70vh); overflow-y: auto; overscroll-behavior: contain; padding: 6px; border-radius: var(--aui-radius-lg, 10px); background: var(--aui-overlay, var(--ui-agfc-card)); box-shadow: var(--ui-agfc-pop); cursor: default; transform-origin: 24px 0; animation: ui-agfc-drop 220ms var(--ui-agfc-ease) both; }
.ui-agfc__pop-head { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; padding: 2px 6px 6px; font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-ink); font-variant-numeric: tabular-nums; }
.ui-agfc__pop-head > :last-child { font-size: 11px; font-weight: 400; color: var(--ui-agfc-muted); }
/* The rows sit together on one soft gray well – no lines between them */
.ui-agfc__steps { display: grid; margin: 0; padding: 2px; border-radius: calc(var(--aui-radius-lg, 10px) - 3px); background: var(--ui-agfc-well); list-style: none; }
.ui-agfc__step { display: flex; align-items: center; gap: 8px; min-width: 0; height: 28px; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); animation: ui-agfc-step 260ms var(--ui-agfc-ease) both; }
/* Where a task stands reads from its text: done is quiet, the one at work stands out, what's still to come is faint */
.ui-agfc__step-title { flex: 1; min-width: 0; overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-agfc-text); white-space: nowrap; text-overflow: ellipsis; transition: color 240ms ease; }
.ui-agfc__step:is([data-status="running"], [data-status="waiting"], [data-status="failed"]) .ui-agfc__step-title { color: var(--ui-agfc-strong); font-weight: 500; }
.ui-agfc__step:is([data-status="queued"], [data-status="stopped"]) .ui-agfc__step-title { color: var(--ui-agfc-faint); }
.ui-agfc__step:is([data-status="queued"], [data-status="stopped"]) .ui-agfc__tool { opacity: 0.5; }
.ui-agfc__step-end { flex: none; font-size: 11px; line-height: 16px; color: var(--ui-agfc-muted); font-variant-numeric: tabular-nums; }
.ui-agfc__step[data-status="running"] .ui-agfc__step-end { font-weight: 500; color: var(--ui-agfc-ink); }
.ui-agfc__step:is([data-status="waiting"], [data-status="failed"]) .ui-agfc__step-end { font-weight: 500; color: var(--ui-agfc-ink); }
.ui-agfc__step:is([data-status="queued"], [data-status="stopped"]) .ui-agfc__step-end { color: var(--ui-agfc-faint); }
.ui-agfc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.ui-agfc__cost { min-width: 0; overflow: hidden; font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-agfc-ink); white-space: nowrap; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
.ui-agfc__time { font-weight: 400; color: var(--ui-agfc-muted); }
/* Costs that still climb: a light shimmer runs over the amount */
.ui-agfc__amount[data-live] {
  background: linear-gradient(90deg, currentColor 0%, currentColor 38%, var(--ui-agfc-shine) 50%, currentColor 62%, currentColor 100%) 0 0 / 300% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: ui-agfc-shine 1100ms linear infinite;
}

/* The connectors draw themselves out of the card, and pull back into it when the tree folds */
.ui-agfc__link { fill: none; stroke: var(--ui-agfc-link); stroke-width: 1.5; stroke-linecap: round; stroke-dasharray: 1 1; stroke-dashoffset: 1; animation: ui-agfc-draw 420ms var(--ui-agfc-ease) both; transition: stroke 240ms ease; }
.ui-agfc__link[data-muted] { stroke: var(--ui-agfc-link-muted); }
.ui-agfc__link[data-leaving] { stroke-dashoffset: 0; animation: ui-agfc-retract ${EXIT}ms var(--ui-agfc-ease) both; }
.ui-agfc__dot { fill: var(--ui-agfc-card); stroke: var(--ui-agfc-link); stroke-width: 1.5; animation: ui-agfc-fade-in 200ms ease both; transition: stroke 240ms ease; }
.ui-agfc__dot[data-muted] { stroke: var(--ui-agfc-link-muted); }
.ui-agfc__dot[data-leaving] { animation: ui-agfc-fade-out ${EXIT}ms ease both; }

@keyframes ui-agfc-draw { to { stroke-dashoffset: 0; } }
@keyframes ui-agfc-retract { from { stroke-dashoffset: 0; } to { stroke-dashoffset: 1; } }
@keyframes ui-agfc-rise { from { opacity: 0; transform: translateX(-8px) scale(0.98); } }
@keyframes ui-agfc-leave { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateX(-8px) scale(0.97); } }
@keyframes ui-agfc-open { from { grid-template-rows: 0fr; } }
@keyframes ui-agfc-close { to { grid-template-rows: 0fr; } }
@keyframes ui-agfc-drop { from { opacity: 0; transform: translateY(-4px) scale(0.97); } }
@keyframes ui-agfc-step { from { opacity: 0; transform: translateY(-3px); } }
@keyframes ui-agfc-fade-in { from { opacity: 0; } }
@keyframes ui-agfc-fade-out { to { opacity: 0; } }
@keyframes ui-agfc-shine { from { background-position: 100% 0; } to { background-position: 0 0; } }
@keyframes ui-agfc-spin { to { transform: rotate(360deg); } }

@media (prefers-reduced-motion: reduce) {
  .ui-agfc *, .ui-agfc *::before, .ui-agfc *::after { animation: none !important; transition: none !important; }
  .ui-agfc__link { stroke-dashoffset: 0; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-agfc {
  --ui-agfc-shine: rgba(255, 255, 255, 0.35);
  --ui-agfc-card: #1c1c1f;
  --ui-agfc-strong: var(--aui-ink, #ffffff);
  --ui-agfc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-agfc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-agfc-muted: var(--aui-muted, rgba(255, 255, 255, 0.45));
  --ui-agfc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.28));
  --ui-agfc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-agfc-fill: var(--aui-subtle, rgba(255, 255, 255, 0.08));
  --ui-agfc-accent: var(--aui-blue, #4fb0fe);
  --ui-agfc-link: var(--aui-info, #4fb0fe);
  --ui-agfc-link-muted: rgba(255, 255, 255, 0.14);
  --ui-agfc-brand-bg: #0077ff3a;
  --ui-agfc-brand-fg: #70b8ff;
  --ui-agfc-ok-bg: #00f04a1f;
  --ui-agfc-ok-fg: var(--aui-green, #5fc88a);
  --ui-agfc-warn-bg: #fb6a0025;
  --ui-agfc-warn-fg: var(--aui-warning, #ffb35c);
  --ui-agfc-danger-bg: #ff35232b;
  --ui-agfc-danger-fg: var(--aui-danger, #ff8a7a);
  --ui-agfc-well: var(--aui-subtle, rgba(255, 255, 255, 0.04));
  --ui-agfc-dim: 0.45;
  --ui-agfc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-agfc-solid: var(--aui-primary, rgba(255, 255, 255, 0.92));
  --ui-agfc-on-solid: var(--aui-on-primary, #141416);
  --ui-agfc-shadow: 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4);
  --ui-agfc-hover: 0 0 0 0.5px rgba(255, 255, 255, 0.16), 0 2px 6px -2px rgba(0, 0, 0, 0.5);
  --ui-agfc-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 12px 28px -10px rgba(0, 0, 0, 0.6));
}
`;
