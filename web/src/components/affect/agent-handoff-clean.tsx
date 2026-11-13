/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";

/*
 * Agent Handoff – tasks passed from agent to agent like a relay, as a small flowchart, in a single self-contained file.
 *   · a card per agent, side by side in relay order: who it is, its status, and the task it's on – the first one, when
 *     it's free, shows what's up next
 *   · under each pair, a handoff column: every task one agent handed to the next, as a card – where it came from, what
 *     went along, how many things got lost, and where the task is now. Rounded lines run from one agent down into its
 *     column and on up into the next agent, with a dot where they leave a card
 *   · when an agent hands over, a streak of light runs along the line into the column, the card drops in, and the
 *     light runs on out of the column to the next agent
 *   · open a card for everything that went along; a lost thing is struck through – open it for why, and "Add back"
 *   · drag a card onto any agent (or tap it, then the agent) to give that task to it – e.g. back to the one who lost
 *     something; from there it's handed on by itself
 * The styles are injected below, so no extra files are needed.
 */

export type HandoffItemKind = "file" | "source" | "fact" | "decision" | "note";

export type HandoffItem = {
  id: string;
  label: string;
  kind?: HandoffItemKind;
  /** It didn't make it to the next agent */
  lost?: boolean;
  /** Why it was lost – shown when you point at it */
  why?: string;
};

export type HandoffAgent = {
  id: string;
  name: string;
  /** Model or role – e.g. "Opus 5.5" */
  model?: string;
  /** A small mark – a logo or avatar; defaults to the initial */
  avatar?: ReactNode;
  /** What it's for – two lines at most, under its name */
  description?: string;
  /** What its tasks cost so far, in USD – runs with a shimmer while it's working */
  cost?: number;
};

export type Handoff = {
  /** Unique per handoff – a new one sends light along the lines */
  id: string;
  from: string;
  to: string;
  task: string;
  items: HandoffItem[];
};

export type HandoffTool = {
  id: string;
  /** What the agent did with it – e.g. "Read #support thread" */
  name: string;
  /** The company's logo – Slack, Notion, Linear … */
  logo?: ReactNode;
  /** Small, on the right – e.g. "4 calls" */
  detail?: string;
};

export type HandoffTask = {
  id: string;
  title: string;
  /** One or two sentences on what it's about */
  description?: string;
  /** The tools it's used – shown when you open the agent that has it */
  tools?: HandoffTool[];
  /** Where it came from – a small logo (Linear, Notion, Slack …) */
  logo?: ReactNode;
  /** "todo" is ready to hand out; "working" is with an agent; "done" or "warning" (done, with a problem) is through */
  status: "todo" | "working" | "done" | "warning";
  /** The agent it's with now – or the last one it was with */
  agent?: string;
  /** A few words: what the agent is doing with it, or what came of it */
  note?: string;
};

export type AgentHandoffProps = {
  /** In relay order – each one hands over to the next */
  agents: HandoffAgent[];
  /** What was handed over – each pair shows its latest */
  handoffs: Handoff[];
  /** Every task – where it came from, what it's about and where it is now; a handoff refers to one by its title */
  tasks?: HandoffTask[];
  onAssign?: (task: HandoffTask, agent: HandoffAgent) => void;
  /** "Add back" on a lost item – pass it on after all */
  onRestore?: (handoff: Handoff, item: HandoffItem) => void;
  className?: string;
  /** Steve: a column holds everything handed to the agent after it – from any agent before it, each card naming
   * where it came from – instead of only what the agent before it handed on */
  inbound?: boolean;
  /** Steve: the words it shows, translated */
  labels?: Partial<HandoffLabels>;
};

export type HandoffLabels = {
  agent: string; none: string; idle: string; working: string; ready: string; done: string; warning: string; upNext: string; nothing: string;
  more: (n: string) => string; close: string; tools: string; costSoFar: (usd: string) => string; give: (name: string) => string;
  handed: (from: string, to: string) => string; received: (to: string) => string; from: (name: string) => string; allLost: string; tap: string;
  with: (name: string) => string; handingOn: string; lost: string; restore: string; empty: string;
};
const LABELS: HandoffLabels = {
  agent: "Agent", none: "No agents yet", idle: "Idle", working: "Working", ready: "Ready", done: "Done", warning: "Needs attention", upNext: "Up next", nothing: "Ready for a task",
  more: (n) => `${n} more`, close: "Close", tools: "Tools", costSoFar: (usd) => `Cost so far: ${usd}`, give: (name) => `Give the task to ${name}`,
  handed: (from, to) => `Handed from ${from} to ${to}`, received: (to) => `Handed to ${to}`, from: (name) => `From ${name}`, allLost: "All lost", tap: "Now tap an agent",
  with: (name) => `With ${name}`, handingOn: "Handing on", lost: "It didn't make it to the next agent.", restore: "Add back", empty: "Nothing handed over yet",
};

/* ---------- Icons – plain strokes, 12px ---------- */

const icon = (d: ReactNode) => (
  <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {d}
  </svg>
);
const KIND: Record<HandoffItemKind, ReactNode> = {
  file: icon(<path d="M7 1.5H3.5a1 1 0 0 0-1 1v7a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1V4Zm0 0V4h2.5" />),
  source: icon(<path d="M5 7a1.8 1.8 0 0 0 2.6 0l1.4-1.4a1.8 1.8 0 0 0-2.6-2.6l-.4.4M7 5a1.8 1.8 0 0 0-2.6 0L3 6.4A1.8 1.8 0 0 0 5.6 9l.4-.4" />),
  fact: icon(<path d="M2.5 9.5v-3M5 9.5v-6M7.5 9.5V5M10 9.5V2.5" />),
  decision: icon(<path d="m3 6.2 2 2L9.2 4" />),
  note: icon(<path d="M2.5 3h7M2.5 6h7M2.5 9h4" />),
};
const Lost = () => icon(<><circle cx="6" cy="6" r="4" /><path d="m3.2 8.8 5.6-5.6" /></>);
const Spin = () => (
  <svg viewBox="0 0 12 12" fill="none" aria-hidden="true" className="ui-ahc__spin">
    <circle cx="6" cy="6" r="4.25" stroke="currentColor" strokeOpacity="0.25" strokeWidth="1.5" />
    <path d="M6 1.75A4.25 4.25 0 0 1 10.25 6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
  </svg>
);

const Spark = () => (
  <svg viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
/* A task's state, as in Agent Inbox: half a circle when it's ready, a spinner while an agent has it, a dot when done,
   a triangle when it needs attention */
const StateIcon = ({ status }: { status: HandoffTask["status"] }) => (
  <span className="ui-ahc__state-icon" data-status={status} aria-hidden="true">
    {status === "todo" && (
      <svg viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="4.4" fill="none" stroke="currentColor" strokeWidth="1.4" />
        <path d="M6 1.6a4.4 4.4 0 0 1 0 8.8Z" fill="currentColor" />
      </svg>
    )}
    {status === "working" && (
      <svg viewBox="0 0 12 12" className="ui-ahc__spin">
        <circle cx="6" cy="6" r="4.6" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="1.6" />
        <path d="M6 1.4a4.6 4.6 0 0 1 4.6 4.6" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      </svg>
    )}
    {status === "warning" && (
      <svg viewBox="0 0 12 12">
        <path d="M6 1.6 11 10.2H1L6 1.6Z" fill="currentColor" />
      </svg>
    )}
    {status === "done" && (
      <svg viewBox="0 0 12 12">
        <circle cx="6" cy="6" r="3.4" fill="currentColor" />
      </svg>
    )}
  </span>
);
const Pass = () => icon(<path d="M2 6h7M7 3.5 9.5 6 7 8.5" />);
/** Content that changes in place: the old slides up and out while the new slides up in, both in one cell */
function Swap({ id, className, children }: { id: string; className?: string; children: ReactNode }) {
  const last = useRef({ id, node: children });
  const [out, setOut] = useState<{ id: string; node: ReactNode } | null>(null);
  if (last.current.id !== id && out?.id !== last.current.id) setOut(last.current);
  useLayoutEffect(() => {
    last.current = { id, node: children };
  });
  return (
    <span className={["ui-ahc__swap", className].filter(Boolean).join(" ")}>
      {out && (
        <span key={`out-${out.id}`} className="ui-ahc__swap-layer" data-leaving="" aria-hidden="true" onAnimationEnd={(e) => e.target === e.currentTarget && setOut(null)}>
          {out.node}
        </span>
      )}
      <span key={id} className="ui-ahc__swap-layer" data-coming={out ? "" : undefined}>
        {children}
      </span>
    </span>
  );
}

const dollars = new Intl.NumberFormat("en", { style: "currency", currency: "USD" });
const cents = new Intl.NumberFormat("en", { style: "currency", currency: "USD", minimumFractionDigits: 3, maximumFractionDigits: 3 });
const big = new Intl.NumberFormat("en", { style: "currency", currency: "USD", notation: "compact", maximumFractionDigits: 1 });
/** A cost with separators, $12.3K from ten thousand on – an unreadable or negative one isn't shown */
const usd = (n: unknown) =>
  typeof n !== "number" || !Number.isFinite(n) || n < 0
    ? null
    : n > 0 && n < 0.001
      ? "<$0.001"
      : n < 0.01
        ? cents.format(n)
        : n >= 10_000
          ? big.format(n)
          : dollars.format(n);
const num = new Intl.NumberFormat("en");
/** A small badge: 999+ past a thousand */
const many = (n: number) => (n > 999 ? "999+" : num.format(n));
/** Whole letters, so an emoji or an accent is never cut in half */
const letters = (w: string) => (typeof Intl.Segmenter === "function" ? Array.from(new Intl.Segmenter().segment(w), (x) => x.segment) : Array.from(w));
/** The first letter of the name – an emoji in front is skipped when a word follows */
const initial = (name: string) => {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const word = words.find((w) => /^[\p{L}\p{N}]/u.test(w)) ?? words[0] ?? "";
  return (letters(word)[0] ?? "").toUpperCase();
};
const STATES = ["todo", "working", "done", "warning"] as const;
/** An unknown status reads as ready – it's shown, but not as through */
const toStatus = (s: unknown): HandoffTask["status"] => (STATES.includes(s as HandoffTask["status"]) ? (s as HandoffTask["status"]) : "todo");
/** Gaps are skipped and a repeated id gets a key of its own */
const keyed = <T extends object>(list: unknown) => {
  const seen = new Map<string, number>();
  return (Array.isArray(list) ? list : [])
    .filter((x): x is T => !!x && typeof x === "object")
    .map((x) => {
      const id = String((x as { id?: unknown }).id ?? "");
      const n = seen.get(id) ?? 0;
      seen.set(id, n + 1);
      return { ...x, id, key: n ? `${id}#${n}` : id, src: x };
    });
};
type Agent = HandoffAgent & { key: string; src: HandoffAgent };
type Hand = Handoff & { key: string; src: Handoff };
/** An agent's mark – its avatar, or its initial when there is none or the image doesn't load */
function Face({ avatar, name, className }: { avatar?: ReactNode; name: string; className: string }) {
  const [broken, setBroken] = useState(false);
  const logo = avatar != null && avatar !== false && !broken;
  return (
    <span className={className} aria-hidden="true" onErrorCapture={() => setBroken(true)}>
      {logo ? avatar : initial(name)}
    </span>
  );
}

const R = 10;
/** The light's streak: this many slices, each this long – 48px in all */
const SLICES = 16;
/** When the light's head reaches the column – its card lands then */
const LAND = 850;
const SLICE = 3;

export function AgentHandoff({ agents: agentsIn, handoffs: handoffsIn, tasks: tasksIn, onAssign, onRestore, className, inbound = false, labels: labelsIn }: AgentHandoffProps) {
  const L = { ...LABELS, ...labelsIn };
  const STATUS = { idle: L.idle, working: L.working } as const;
  const STATE_WORD = { todo: L.ready, done: L.done, warning: L.warning } as const;
  // Gaps are skipped, repeated ids get keys of their own, names and statuses are ones that can be shown
  const agents: Agent[] = keyed<HandoffAgent>(agentsIn).map((a) => ({ ...a, name: String(a.name ?? "").trim() || a.id || L.agent }));
  const handoffs: Hand[] = keyed<Handoff>(handoffsIn).map((h) => ({
    ...h,
    from: String(h.from ?? ""),
    to: String(h.to ?? ""),
    task: String(h.task ?? ""),
    items: (Array.isArray(h.items) ? h.items : []).filter((x): x is HandoffItem => !!x && typeof x === "object"),
  }));
  const tasks: HandoffTask[] = (Array.isArray(tasksIn) ? tasksIn : [])
    .filter((t): t is HandoffTask => !!t && typeof t === "object")
    .map((t) => ({ ...t, status: toStatus(t.status), title: String(t.title ?? ""), tools: (Array.isArray(t.tools) ? t.tools : []).filter((x): x is HandoffTool => !!x && typeof x === "object") }));
  const [open, setOpen] = useState<string | null>(null);
  // A card picked up by a double-click or tap, waiting for an agent – from the column after agent `after`
  const [held, setHeld] = useState<{ hand: string; task: string; after: number } | null>(null);
  const [over, setOver] = useState<string | null>(null); // the agent a task is dragged over
  const box = useRef<HTMLDivElement>(null);
  const cards = useRef(new Map<string, HTMLElement>());
  const packs = useRef(new Map<number, HTMLElement>());
  const [lines, setLines] = useState<{ i: number; into: string; on: string; inLen: number; onLen: number; dots: [number, number][] }[]>([]);

  const n = agents.length;
  // A new handoff's card lands in its column when the light reaches the column – until then it's on its way
  const known = useRef(new Set(handoffs.map((h) => h.key)));
  const [landing, setLanding] = useState<string[]>([]);
  const fresh = handoffs.filter((h) => !known.current.has(h.key)).map((h) => h.key);
  const away = new Set([...landing, ...fresh]);
  useLayoutEffect(() => {
    if (!fresh.length) return;
    fresh.forEach((id) => known.current.add(id));
    setLanding((l) => [...l, ...fresh]);
    window.setTimeout(() => setLanding((l) => l.filter((id) => !fresh.includes(id))), LAND);
  });

  // Each pair's latest handoff
  const latest = agents.slice(0, -1).map((a, i) => [...handoffs].reverse().find((h) => (inbound || h.from === a.id) && h.to === agents[i + 1].id));
  // What an agent is on right now
  // (inbound: every agent has its own step – what it is on, or how its step stands)
  const current = (a: Agent) => tasks.find((t) => (inbound || t.status === "working") && t.agent === a.id);
  const taskOf = (title: string) => tasks.find((t) => t.title === title);
  const [opened, setOpened] = useState<string | null>(null);
  // An agent opened for a closer look at its task – closes on Escape or a click anywhere else
  const [peek, setPeek] = useState<string | null>(null);
  useEffect(() => {
    if (!peek) return;
    const away = (e: PointerEvent) => {
      if (!cards.current.get(peek)?.contains(e.target as Node)) setPeek(null);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setPeek(null);
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [peek]);
  // When a card drops into a column, the ones under it glide down to make room instead of jumping
  const items = useRef(new Map<string, HTMLElement>());
  const spots = useRef(new Map<string, number>());
  const order = useRef("");
  useLayoutEffect(() => {
    const now = handoffs.filter((h) => !away.has(h.key)).map((h) => h.key).join();
    const moved = now !== order.current;
    order.current = now;
    const next = new Map<string, number>();
    items.current.forEach((el, id) => {
      const y = el.offsetTop;
      next.set(id, y);
      const was = spots.current.get(id);
      if (moved && was != null && was !== y) el.animate([{ transform: `translateY(${was - y}px)` }, { transform: "none" }], { duration: 480, easing: "cubic-bezier(0.32, 0.72, 0.24, 1)" });
    });
    spots.current = next;
  });

  // The lines: from an agent's bottom down into the left of its handoff, and from the right of it up into the next agent
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const draw = () => {
      const base = el.getBoundingClientRect();
      const s = base.width / (el.offsetWidth || 1); // the page may show it scaled
      const at = (e: HTMLElement) => {
        const r = e.getBoundingClientRect();
        return { x: (r.left - base.left) / s, y: (r.top - base.top) / s, w: r.width / s, h: r.height / s };
      };
      const next: typeof lines = [];
      for (let i = 0; i < n - 1; i++) {
        const a = cards.current.get(agents[i].key);
        const b = cards.current.get(agents[i + 1].key);
        const p = packs.current.get(i);
        if (!a || !b || !p) continue;
        const A = at(a), B = at(b), P = at(p);
        const ax = A.x + A.w / 2, ay = A.y + A.h;
        const bx = B.x + B.w / 2, by = B.y + B.h;
        // Down from the agent, along, and down into the column's top – then up out of its top, along, and up into the next
        const top = P.y;
        const m = Math.round(ay + (top - ay) / 2);
        // In and out close together, either side of the column's middle
        const xi = P.x + P.w / 2 - 14;
        const xo = P.x + P.w / 2 + 14;
        const bend = (x1: number, y1: number, x2: number, y2: number) => {
          const dx = x2 - x1;
          if (Math.abs(dx) < 2) return `M${x1} ${y1}V${y2}`;
          const r = Math.max(0, Math.min(R, Math.abs(dx) / 2, Math.abs(m - y1) - 1, Math.abs(y2 - m) - 1));
          const sx = Math.sign(dx);
          const a = Math.sign(m - y1);
          const b = Math.sign(y2 - m);
          return `M${x1} ${y1}V${m - a * r}Q${x1} ${m} ${x1 + sx * r} ${m}H${x2 - sx * r}Q${x2} ${m} ${x2} ${m + b * r}V${y2}`;
        };
        // How long each line is – the light's streak is sized and moved in its pixels
        const span = (x1: number, y1: number, x2: number, y2: number) => Math.abs(m - y1) + Math.abs(x2 - x1) + Math.abs(y2 - m);
        const into = bend(ax, ay, xi, top);
        const on = bend(xo, top, bx, by);
        next.push({ i, into, on, inLen: span(ax, ay, xi, top), onLen: span(xo, top, bx, by), dots: [[ax, ay], [bx, by], [xi, top], [xo, top]] });
      }
      // The box resizes often (a column grows); only lines that actually moved re-render it
      setLines((prev) => (JSON.stringify(prev) === JSON.stringify(next) ? prev : next));
    };
    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(el);
    return () => ro.disconnect();
  }, [n, agents.map((a) => a.key).join()]);

  // The light: a short streak whose opacity runs 0 → 100% → 0 from its tail to its head – drawn as thin slices along
  // the line, each a little brighter towards the middle, all moving together
  const streak = (d: string, len: number, later?: boolean) =>
    Array.from({ length: SLICES }, (_, k) => (
      <path
        key={k}
        className="ui-ahc__streak"
        data-on={later ? "" : undefined}
        d={d}
        style={{ "--len": `${len}px`, "--shift": `${k * SLICE}px`, "--o": Math.max(0.06, 1 - Math.abs((2 * k) / (SLICES - 1) - 1)).toFixed(2) } as CSSProperties}
      />
    ));

  // A card moves on down the relay – onto any agent to the right of its column, along the lines
  type Carry = { hand: string; task: string; after: number; title: string; logo?: ReactNode; x: number; y: number; dx: number; dy: number; w: number };
  const [carry, setCarry] = useState<Carry | null>(null);
  const press = useRef<(Omit<Carry, "x" | "y"> & { sx: number; sy: number }) | null>(null);
  const carried = useRef<Carry | null>(null);
  const dragged = useRef(false);
  const moving = carry ?? held;
  const takes = (i: number) => !moving || i > moving.after;

  const assign = (taskId: string, agent: Agent, after: number) => {
    const t = (Array.isArray(tasksIn) ? tasksIn : []).find((x) => x && x.id === taskId);
    if (t && agents.findIndex((a) => a.key === agent.key) > after) onAssign?.(t, agent.src);
    setHeld(null);
    setOver(null);
  };
  const grab = (e: ReactPointerEvent<HTMLElement>, hand: string, t: HandoffTask, after: number) => {
    // Mouse and pen drag; on touch, a double-tap picks the card up instead – only where a task can be given to an agent
    if (!onAssign || e.button !== 0 || e.pointerType === "touch") return;
    dragged.current = false;
    const r = e.currentTarget.getBoundingClientRect();
    press.current = { hand, task: t.id, after, title: t.title, logo: t.logo, sx: e.clientX, sy: e.clientY, dx: e.clientX - r.left, dy: e.clientY - r.top, w: r.width };
  };
  // The agent under the pointer, if this card can go to it
  const target = (x: number, y: number, after: number) => {
    const el = document.elementFromPoint(x, y)?.closest<HTMLElement>(".ui-ahc__agent");
    const i = el && box.current?.contains(el) ? Number(el.dataset.i) : -1;
    return i > after ? agents[i] : undefined;
  };
  useEffect(() => {
    const move = (e: globalThis.PointerEvent) => {
      const p = press.current;
      if (!p) return;
      if (!carried.current && Math.hypot(e.clientX - p.sx, e.clientY - p.sy) < 5) return;
      const next = { ...p, x: e.clientX, y: e.clientY };
      carried.current = next;
      dragged.current = true;
      setCarry(next);
      setOver(target(e.clientX, e.clientY, p.after)?.key ?? null);
    };
    const drop = (e: globalThis.PointerEvent) => {
      const c = carried.current;
      press.current = null;
      carried.current = null;
      if (!c) return;
      const to = target(e.clientX, e.clientY, c.after);
      if (to) assign(c.task, to, c.after);
      setCarry(null);
      setOver(null);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      press.current = null;
      carried.current = null;
      setCarry(null);
      setOver(null);
      setHeld(null);
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", drop);
    document.addEventListener("pointercancel", drop);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", drop);
      document.removeEventListener("pointercancel", drop);
      document.removeEventListener("keydown", esc);
    };
  });

  return (
    <div ref={box} className={["ui-ahc", className].filter(Boolean).join(" ")} data-inbound={inbound ? "" : undefined} style={{ "--n": n } as CSSProperties} data-holding={held ? "" : undefined} data-carrying={carry ? "" : undefined}>
      <style href="ui-agent-handoff-clean" precedence="default">
        {css}
      </style>

      {agents.length === 0 && <p className="ui-ahc__none">{L.none}</p>}
      {/* The card in hand, following the pointer */}
      {carry && box.current && (
        <div
          className="ui-ahc__ghost"
          aria-hidden="true"
          style={{ left: carry.x - carry.dx - box.current.getBoundingClientRect().left, top: carry.y - carry.dy - box.current.getBoundingClientRect().top, width: carry.w }}
        >
          <span className="ui-ahc__card-mark">{carry.logo ?? <Spark />}</span>
          <span className="ui-ahc__card-title">{carry.title}</span>
        </div>
      )}

      {/* The lines, under the cards */}
      <svg className="ui-ahc__lines" aria-hidden="true">
        {lines.map((l) => (
          <g key={l.i} data-passed={latest[l.i] ? "" : undefined}>
            <path className="ui-ahc__line" d={l.into} />
            <path className="ui-ahc__line" d={l.on} />
            {/* A fresh handoff: light runs down into the column, then on out of it to the next agent */}
            {latest[l.i] && (
              <g key={latest[l.i]!.key} className="ui-ahc__light">
                {streak(l.into, l.inLen)}
                {streak(l.on, l.onLen, true)}
              </g>
            )}
          </g>
        ))}
      </svg>

      <div className="ui-ahc__flow">
        {agents.map((a, i) => {
          const now = current(a);
          const st = now?.status === "working" ? "working" : "idle";
          // What opening it shows: the task it's on – or, the first one when free, what's up next
          const shown = now ?? (i === 0 ? tasks.find((t) => t.status === "todo") : undefined);
          const peeking = peek === a.key && !!shown;
          const cost = a.cost != null ? usd(a.cost) : null;
          const queue = i === 0 && !inbound ? tasks.filter((t) => t.status === "todo") : [];
          return (
            <div
              key={a.key}
              ref={(e) => {
                if (e) cards.current.set(a.key, e);
                else cards.current.delete(a.key);
              }}
              className="ui-ahc__agent"
              style={{ "--c": i * 2 + 1 } as CSSProperties}
              data-status={st}
              data-over={over === a.key ? "" : undefined}
              data-i={i}
              data-blocked={moving && !takes(i) ? "" : undefined}
              data-peek={peeking ? "" : undefined}
              role="button"
              tabIndex={0}
              aria-label={held ? L.give(a.name) : `${a.name}${shown ? ` – ${shown.title}` : ""}`}
              aria-expanded={held ? undefined : peeking}
              onClick={() => (held ? takes(i) && assign(held.task, a, held.after) : shown && setPeek(peeking ? null : a.key))}
              onKeyDown={(e) => {
                if (e.target !== e.currentTarget || (e.key !== "Enter" && e.key !== " ")) return;
                e.preventDefault();
                e.currentTarget.click();
              }}
            >
              <span className="ui-ahc__top">
                <Face className="ui-ahc__face" avatar={a.avatar} name={a.name} />
                <span className="ui-ahc__who">
                  <span className="ui-ahc__name" dir="auto" title={a.name}>
                    {a.name}
                  </span>
                  {a.model && (
                    <span className="ui-ahc__model" dir="auto" title={String(a.model)}>
                      {a.model}
                    </span>
                  )}
                </span>
                {/* What it has cost so far – running, with a shimmer, while it works */}
                {cost && (
                  <span className="ui-ahc__cost" data-live={now ? "" : undefined} title={L.costSoFar(dollars.format(a.cost as number))}>
                    {cost}
                  </span>
                )}
                <span className="ui-ahc__status" data-status={st}>
                  {st === "working" && <Spin />}
                  <Swap id={inbound && now ? now.status : st} className="ui-ahc__status-word">{inbound && now && now.status !== "working" ? STATE_WORD[now.status] : STATUS[st]}</Swap>
                </span>
              </span>
              {a.description && (
                <span className="ui-ahc__desc" dir="auto" title={String(a.description)}>
                  {a.description}
                </span>
              )}
              {/* What it's on – one line, swapped in place */}
              <span className="ui-ahc__now">
                <Swap id={now ? `now-${now.id}` : queue.length ? `next-${queue[0].id}` : "idle"}>
                {now && inbound && now.status !== "working" ? (
                  <span className="ui-ahc__now-idle" data-state={now.status} title={now.note}>{now.note ?? STATE_WORD[now.status]}</span>
                ) : now ? (
                  <>
                    {now.logo && <span className="ui-ahc__logo">{now.logo}</span>}
                    <span className="ui-ahc__now-task" dir="auto" title={now.title}>
                      {now.title}
                    </span>
                    {now.note && (
                      <span className={["ui-ahc__now-note", now.status === "working" ? "ui-ahc__shine" : ""].filter(Boolean).join(" ")} dir="auto" title={now.note}>
                        {now.note}
                      </span>
                    )}
                  </>
                ) : queue.length ? (
                  // The first agent, free: what's waiting for it
                  <>
                    <span className="ui-ahc__now-idle">{L.upNext}</span>
                    {queue[0].logo && <span className="ui-ahc__logo">{queue[0].logo}</span>}
                    <span className="ui-ahc__now-task" dir="auto" title={queue[0].title}>
                      {queue[0].title}
                    </span>
                    {queue.length > 1 && (
                      <span className="ui-ahc__now-more" title={L.more(num.format(queue.length - 1))}>
                        +{many(queue.length - 1)}
                      </span>
                    )}
                  </>
                ) : (
                  <span className="ui-ahc__now-idle">{L.nothing}</span>
                )}
                </Swap>
              </span>

              {/* Opened: the task, what it's about and the tools it's used */}
              {peeking && shown && (
                <div className="ui-ahc__peek" role="dialog" aria-label={shown.title} onClick={(e) => e.stopPropagation()}>
                  <span className="ui-ahc__peek-head">
                    {shown.logo && <span className="ui-ahc__logo">{shown.logo}</span>}
                    <span className="ui-ahc__peek-title" dir="auto" title={shown.title}>
                      {shown.title}
                    </span>
                    <button type="button" className="ui-ahc__peek-x" aria-label={L.close} onClick={() => setPeek(null)}>
                      {icon(<path d="m3.5 3.5 5 5m0-5-5 5" />)}
                    </button>
                  </span>
                  <span className="ui-ahc__peek-state">
                    {now ? <span className="ui-ahc__shine">{now.note ?? L.working}</span> : L.upNext}
                  </span>
                  <div className="ui-ahc__peek-body">
                    {shown.description && <p className="ui-ahc__peek-desc">{shown.description}</p>}
                    {!!shown.tools?.length && (
                      <>
                        <span className="ui-ahc__peek-label">{L.tools}</span>
                        <ul className="ui-ahc__tools">
                          {shown.tools.map((x, k) => (
                            <li key={k} className="ui-ahc__tool">
                              <span className="ui-ahc__logo">{x.logo ?? <Spark />}</span>
                              <span className="ui-ahc__tool-name" dir="auto" title={x.name}>
                                {x.name}
                              </span>
                              {x.detail && (
                                <span className="ui-ahc__tool-detail" title={String(x.detail)}>
                                  {x.detail}
                                </span>
                              )}
                            </li>
                          ))}
                        </ul>
                      </>
                    )}
                  </div>
                </div>
              )}
            </div>
          );
        })}

        {/* A handoff column under each pair: every task the one handed to the next, latest on top */}
        {agents.slice(0, -1).map((a, i) => {
          const b = agents[i + 1];
          const list = handoffs.filter((h) => (inbound || h.from === a.id) && h.to === b.id && !away.has(h.key)).reverse();
          return (
            <section
              key={`p-${i}`}
              ref={(e) => {
                if (e) packs.current.set(i, e);
                else packs.current.delete(i);
              }}
              className="ui-ahc__col"
              style={{ "--c": i * 2 + 2 } as CSSProperties}
              aria-label={inbound ? L.received(b.name) : L.handed(a.name, b.name)}
            >
              <span className="ui-ahc__col-head">
                <span className="ui-ahc__col-who" title={inbound ? L.received(b.name) : `${a.name} → ${b.name}`}>
                  {!inbound && <Face className="ui-ahc__col-face" avatar={a.avatar} name={a.name} />}
                  {!inbound && <span className="ui-ahc__col-name" dir="auto">
                    {a.name}
                  </span>}
                  <span className="ui-ahc__col-arrow">
                    <Pass />
                  </span>
                  <Face className="ui-ahc__col-face" avatar={b.avatar} name={b.name} />
                  <span className="ui-ahc__col-name" dir="auto">
                    {b.name}
                  </span>
                </span>
                <span className="ui-ahc__lane-n" title={list.length > 999 ? num.format(list.length) : undefined}>
                  {many(list.length)}
                </span>
              </span>
              <ul className="ui-ahc__cards">
                {list.map((h) => {
                  const t = taskOf(h.task);
                  const kept = h.items.filter((x) => !x.lost);
                  const gone = h.items.filter((x) => x.lost);
                  const isOpen = opened === h.key;
                  const status = t?.status ?? "done";
                  const who = agents.find((x) => x.id === t?.agent);
                  const src = inbound ? agents.find((x) => x.id === h.from) : undefined;
                  return (
                    <li
                      key={h.key}
                      ref={(e) => {
                        if (e) items.current.set(h.key, e);
                        else items.current.delete(h.key);
                      }}
                      className="ui-ahc__card" data-status={status} data-open={isOpen ? "" : undefined} data-lost={gone.length ? "" : undefined}>
                      <button
                        type="button"
                        className="ui-ahc__card-main"
                        data-movable={t && onAssign ? "" : undefined}
                        data-carried={carry?.hand === h.key ? "" : undefined}
                        aria-expanded={isOpen}
                        aria-pressed={t ? held?.hand === h.key : undefined}
                        onPointerDown={(e) => t && grab(e, h.key, t, i)}
                        onClick={() => {
                          if (dragged.current) return void (dragged.current = false);
                          setOpened(isOpen ? null : h.key);
                        }}
                        onDoubleClick={() => t && onAssign && setHeld(held?.hand === h.key ? null : { hand: h.key, task: t.id, after: i })}
                      >
                        <span className="ui-ahc__card-top">
                          <span className="ui-ahc__card-mark" title={src ? L.from(src.name) : undefined}>{src ? <Face className="ui-ahc__col-face" avatar={src.avatar} name={src.name} /> : (t?.logo ?? <Spark />)}</span>
                          <span className="ui-ahc__card-title" dir="auto" title={h.task}>
                            {h.task}
                          </span>
                        </span>
                        {/* What went along – or that nothing made it; no line when nothing was sent */}
                        {(kept.length > 0 || gone.length > 0) && (
                          <span className="ui-ahc__card-desc" dir="auto" title={kept.length ? kept.map((x) => x.label).join(", ") : undefined}>
                            {kept.length ? kept.map((x) => x.label).join(", ") : L.allLost}
                          </span>
                        )}
                        <span className="ui-ahc__card-foot">
                          <Swap id={`${status}-${t?.agent}-${held?.hand === h.key}`} className="ui-ahc__card-state">
                            <StateIcon status={status} />
                            <span className="ui-ahc__card-word" dir="auto">{held?.hand === h.key ? L.tap : status === "working" ? (who ? L.with(who.name) : L.handingOn) : status === "todo" ? STATE_WORD.todo : (t?.note ?? STATE_WORD[status])}</span>
                          </Swap>
                        </span>
                      </button>

                      {/* Open: everything that went along – and what didn't */}
                      <div className="ui-ahc__fold" data-open={isOpen ? "" : undefined} inert={!isOpen}>
                        <div className="ui-ahc__clip">
                          <ul className="ui-ahc__items">
                            {[...kept, ...gone].map((x, k) => {
                              const key = `${h.key}-${k}`;
                              return (
                                <li key={key} className="ui-ahc__item-wrap">
                                  <button
                                    type="button"
                                    className="ui-ahc__item"
                                    data-lost={x.lost ? "" : undefined}
                                    aria-expanded={x.lost ? open === key : undefined}
                                    onClick={() => x.lost && setOpen(open === key ? null : key)}
                                  >
                                    <span className="ui-ahc__kind">{x.lost ? <Lost /> : (KIND[x.kind as HandoffItemKind] ?? KIND.note)}</span>
                                    <span className="ui-ahc__label" dir="auto" title={String(x.label ?? "")}>
                                      {x.label}
                                    </span>
                                  </button>
                                  {x.lost && open === key && (
                                    <span className="ui-ahc__why">
                                      <span>{x.why ?? L.lost}</span>
                                      {onRestore && (
                                        <button
                                          type="button"
                                          className="ui-ahc__restore"
                                          onClick={() => {
                                            onRestore(h.src, x);
                                            setOpen(null);
                                          }}
                                        >
                                          {L.restore}
                                        </button>
                                      )}
                                    </span>
                                  )}
                                </li>
                              );
                            })}
                          </ul>
                        </div>
                      </div>
                    </li>
                  );
                })}
                {list.length === 0 && <li className="ui-ahc__lane-empty">{L.empty}</li>}
              </ul>
            </section>
          );
        })}
      </div>

      {/* Dots where the lines leave a card – over the cards */}
      <svg className="ui-ahc__lines ui-ahc__top-layer" aria-hidden="true">
        {lines.map((l) =>
          l.dots.map(([x, y], k) => <circle key={`${l.i}-${k}`} cx={x} cy={y} r={3.25} className="ui-ahc__dot" data-passed={latest[l.i] ? "" : undefined} />),
        )}
      </svg>
    </div>
  );
}

const css = /* css */ `
/* Colors, sizes and motion as variables – override them on .ui-ahc */
.ui-ahc {
  --ui-ahc-card: #ffffff;
  --ui-ahc-strong: var(--aui-ink, #000000);
  --ui-ahc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-ahc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-ahc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-ahc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-ahc-line: var(--aui-border, rgba(0, 0, 0, 0.08));
  --ui-ahc-edge: var(--aui-border, #efeeee);
  --ui-ahc-well: var(--aui-subtle, rgba(0, 0, 0, 0.025));
  --ui-ahc-lane: var(--aui-subtle, #f4f4f5);
  --ui-ahc-blue: var(--aui-blue, #4fb0fe);
  --ui-ahc-board-line: var(--aui-border, #e9e9ec);
  --ui-ahc-board-ink: var(--aui-ink, #1c1c1f);
  --ui-ahc-board-text: var(--aui-muted, #55555c);
  --ui-ahc-board-green: var(--aui-success, #1e8a4f);
  --ui-ahc-board-amber: var(--aui-amber, #e8b000);
  --ui-ahc-board-red: var(--aui-red, #f04438);
  --ui-ahc-shine: rgba(0, 0, 0, 0.2);
  --ui-ahc-kpi-shine: rgba(0, 0, 0, 0.3);
  --ui-ahc-link: #83c7fe;
  --ui-ahc-link-muted: rgba(0, 0, 0, 0.12);
  --ui-ahc-accent: var(--aui-info, #0070d6);
  --ui-ahc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-ahc-accent-bg: rgba(0, 96, 226, 0.08);
  --ui-ahc-glow: rgba(0, 96, 226, 0.45);
  --ui-ahc-drop: rgba(0, 96, 226, 0.32);
  --ui-ahc-green: #16673b;
  --ui-ahc-green-bg: color-mix(in srgb, var(--aui-green, #5fc88a) 10%, transparent);
  --ui-ahc-amber: var(--aui-warning, #b25e00);
  --ui-ahc-amber-bg: color-mix(in srgb, var(--aui-amber, #e8b000) 14%, transparent);
  --ui-ahc-red: var(--aui-danger, #c4320a);
  --ui-ahc-red-line: color-mix(in srgb, var(--aui-red, #f04438) 35%, transparent);
  --ui-ahc-red-bg: color-mix(in srgb, var(--aui-red, #f04438) 5%, transparent);
  --ui-ahc-tip: #18181b;
  /* Soft, layered – like the cards in Project Flow */
  --ui-ahc-lift: 0 10px 6px rgba(0, 0, 0, 0.02), 0 5px 5px rgba(0, 0, 0, 0.03), 0 1px 2px rgba(0, 0, 0, 0.04);
  --ui-ahc-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-ahc-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  position: relative;
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  container: ui-ahc / inline-size;
  padding: 8px 0 0;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  color: var(--ui-ahc-text);
  -webkit-font-smoothing: antialiased;
}
.ui-ahc *, .ui-ahc *::before, .ui-ahc *::after { box-sizing: border-box; }
.ui-ahc :is(ol, ul) { margin: 0; padding: 0; list-style: none; }
:where(.ui-ahc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
.ui-ahc button:focus-visible, .ui-ahc__agent:focus-visible { outline: 2px solid var(--ui-ahc-focus); outline-offset: 2px; }

.ui-ahc__lines { position: absolute; inset: 0; z-index: 1; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
.ui-ahc__top-layer { z-index: 3; }
.ui-ahc__line { fill: none; stroke: var(--ui-ahc-link-muted); stroke-width: 1.5; stroke-linecap: round; transition: stroke 300ms ease; }
.ui-ahc__lines [data-passed] .ui-ahc__line { stroke: var(--ui-ahc-link); }
.ui-ahc__dot { fill: var(--ui-ahc-card); stroke: var(--ui-ahc-link-muted); stroke-width: 1.5; transition: stroke 300ms ease; }
.ui-ahc__dot[data-passed] { stroke: var(--ui-ahc-link); }

/* ----- The flow: agents on top, each handoff under its pair ----- */
.ui-ahc__flow { position: relative; z-index: 2; display: grid; grid-template-columns: repeat(calc(var(--n) * 2), minmax(0, 1fr)); row-gap: 72px; }
.ui-ahc__agent { grid-column: var(--c) / span 2; justify-self: center; width: calc(100% - 16px); max-width: 300px; border-radius: calc(var(--aui-radius-lg, 10px) + 4px); background: var(--aui-surface, var(--ui-ahc-card)); box-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-ahc-edge), var(--ui-ahc-lift)); }
.ui-ahc__agent { grid-row: 1; display: grid; gap: 10px; padding: 14px 14px 12px; transition: box-shadow 200ms ease, transform 200ms var(--ui-ahc-ease); }
/* A task over it: it lifts and rings in blue */
.ui-ahc__agent[data-over], .ui-ahc[data-holding] .ui-ahc__agent:not([data-blocked]):hover { box-shadow: 0 0 0 1px var(--ui-ahc-drop), 0 12px 24px -12px rgba(0, 96, 226, 0.14); transform: translateY(-2px); }
.ui-ahc[data-holding] .ui-ahc__agent { cursor: copy; }
.ui-ahc[data-carrying], .ui-ahc[data-carrying] * { cursor: grabbing !important; user-select: none; }
.ui-ahc__card-main[data-carried] { opacity: 0.4; }
/* The card in hand */
.ui-ahc__ghost { position: absolute; z-index: 10; display: flex; align-items: center; gap: 8px; padding: 10px 12px 10px 10px; border-radius: var(--aui-radius-lg, 10px); background: var(--aui-surface, var(--ui-ahc-card)); box-shadow: 0 0 0 1px var(--ui-ahc-board-line), 0 16px 32px -12px color-mix(in srgb, var(--aui-purple, #a651f0) 30%, transparent); pointer-events: none; transform: rotate(-2deg); animation: ui-ahc-lift 160ms var(--ui-ahc-ease) both; }
/* While a task is picked up, the agents it has already passed step back – it only moves on */
.ui-ahc__agent { transition: box-shadow 200ms ease, transform 200ms var(--ui-ahc-ease), opacity 200ms ease; }
.ui-ahc__agent[data-blocked] { opacity: 0.45; cursor: not-allowed !important; }
.ui-ahc__top { display: flex; align-items: center; gap: 8px; min-width: 0; }
.ui-ahc__face { display: grid; flex: none; place-items: center; width: 26px; height: 26px; border-radius: 7px; background: var(--ui-ahc-card); box-shadow: var(--ui-ahc-shadow); font-size: 11px; font-weight: 500; color: var(--ui-ahc-ink); }
/* An image as mark fills its tile, whatever its shape */
.ui-ahc__face > img, .ui-ahc__col-face > img, .ui-ahc__logo > img, .ui-ahc__card-mark > img { display: block; width: 100%; height: 100%; object-fit: cover; border-radius: inherit; }
.ui-ahc__face > svg { width: 14px; height: 14px; }
/* The name keeps a few letters beside a big cost */
.ui-ahc__who { display: grid; flex: 1; min-width: 3em; }
/* Each agent card is a container, so a slim card (many agents side by side) drops the status word on its own */
.ui-ahc__agent { container: ui-ahc-agent / inline-size; }
@container ui-ahc-agent (max-width: 200px) {
  .ui-ahc__status-word { display: none; }
  .ui-ahc__status { width: 20px; padding: 0; justify-content: center; }
  .ui-ahc__status[data-status="idle"] { display: none; }
}
.ui-ahc__name { overflow: hidden; font-weight: 500; line-height: 16px; color: var(--ui-ahc-strong); white-space: nowrap; text-overflow: ellipsis; }
/* What it's for: two lines, always the same height */
.ui-ahc__desc { display: -webkit-box; height: 36px; overflow: hidden; -webkit-line-clamp: 2; -webkit-box-orient: vertical; font-size: 12.5px; line-height: 18px; color: var(--ui-ahc-text); }
.ui-ahc__model { overflow: hidden; font-size: 11px; line-height: 14px; color: var(--ui-ahc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__status { display: inline-flex; flex: none; align-items: center; gap: 4px; height: 18px; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-ahc-well); font-size: 11px; font-weight: 500; color: var(--ui-ahc-muted); transition: background-color 300ms ease, color 300ms ease; }
.ui-ahc__status > svg { width: 10px; height: 10px; }
.ui-ahc__status[data-status="working"] { background: var(--ui-ahc-accent-bg); color: var(--ui-ahc-accent); }
/* What it's on: a soft gray well inside the card */
.ui-ahc__now { display: grid; align-items: center; min-width: 0; height: 32px; padding: 0 9px; overflow: hidden; border-radius: calc(var(--aui-radius, 7px) + 1px); background: var(--ui-ahc-lane); font-size: 12px; }
.ui-ahc__now > .ui-ahc__swap > .ui-ahc__swap-layer { display: flex; align-items: center; gap: 6px; }
/* Swapped content: the old line slides up and fades as the new one comes up from under it */
.ui-ahc__swap { display: inline-grid; grid-template-columns: minmax(0, 1fr); align-items: center; min-width: 0; overflow: hidden; }
.ui-ahc__swap-layer { grid-area: 1 / 1; min-width: 0; }
.ui-ahc__swap-layer[data-leaving] { animation: ui-ahc-swap-out 380ms var(--ui-ahc-ease) both; pointer-events: none; }
.ui-ahc__swap-layer[data-coming] { animation: ui-ahc-swap-in 380ms var(--ui-ahc-ease) both; }
.ui-ahc__now-task { flex: none; max-width: 55%; overflow: hidden; font-weight: 500; color: var(--ui-ahc-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__now-note { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__now-more { flex: none; }
.ui-ahc__now-idle { color: var(--ui-ahc-faint); }
/* Opened: a dropdown under the agent – the task's title, then a grey well with what it's about and its tools */
.ui-ahc__agent { position: relative; cursor: pointer; }
.ui-ahc:has(.ui-ahc__agent[data-peek]) .ui-ahc__flow { z-index: 4; }
.ui-ahc__agent[data-peek] { z-index: 1; }
.ui-ahc__peek { position: absolute; top: calc(100% + 8px); left: 0; right: 0; z-index: 2; display: grid; gap: 2px; padding: 12px; border-radius: calc(var(--aui-radius-lg, 10px) + 6px); background: var(--aui-overlay, var(--ui-ahc-card)); box-shadow: 0 0 0 1px var(--ui-ahc-edge), 0 18px 40px -14px color-mix(in srgb, var(--aui-purple, #a651f0) 28%, transparent), 0 4px 10px -4px color-mix(in srgb, var(--aui-purple, #a651f0) 8%, transparent); cursor: default; transform-origin: 50% 0; animation: ui-ahc-peek 240ms var(--ui-ahc-ease) both; }
.ui-ahc__peek-head { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 0 0 0 2px; }
.ui-ahc__peek-title { flex: 1; min-width: 0; overflow: hidden; font-size: 13px; font-weight: 500; line-height: 18px; color: var(--ui-ahc-strong); white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__peek-x { display: grid; flex: none; place-items: center; width: 22px; height: 22px; margin: -2px -2px -2px 0; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-ahc-faint); transition: background-color 150ms ease, color 150ms ease; }
.ui-ahc__peek-x:hover { background: var(--ui-ahc-lane); color: var(--ui-ahc-ink); }
.ui-ahc__peek-x > svg { width: 12px; height: 12px; }
.ui-ahc__peek-state { padding-left: 26px; font-size: 12px; line-height: 16px; color: var(--ui-ahc-muted); }
.ui-ahc__peek-body { display: grid; gap: 6px; margin-top: 8px; padding: 10px; border-radius: var(--aui-radius-lg, 10px); background: var(--ui-ahc-lane); }
.ui-ahc__peek-desc { margin: 0; font-size: 12.5px; line-height: 18px; color: var(--ui-ahc-text); }
.ui-ahc__peek-label { margin-top: 4px; font-size: 11px; font-weight: 500; line-height: 14px; color: var(--ui-ahc-faint); }
.ui-ahc__tools { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }
.ui-ahc__tool { display: flex; align-items: center; gap: 8px; min-width: 0; height: 26px; font-size: 12px; color: var(--ui-ahc-ink); animation: ui-ahc-in 260ms var(--ui-ahc-ease) both; }
.ui-ahc__tool:nth-child(2) { animation-delay: 40ms; }
.ui-ahc__tool:nth-child(3) { animation-delay: 80ms; }
.ui-ahc__tool:nth-child(n + 4) { animation-delay: 120ms; }
.ui-ahc__tool-name { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
/* A long detail shortens before the tool's name does */
.ui-ahc__tool-detail { flex: none; max-width: 45%; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-variant-numeric: tabular-nums; color: var(--ui-ahc-muted); }
.ui-ahc__cost { flex: none; font-size: 11px; line-height: 14px; font-variant-numeric: tabular-nums; color: var(--ui-ahc-muted); }
/* Working: the cost runs, with a soft light passing over it */
.ui-ahc__cost[data-live] { background: linear-gradient(90deg, var(--ui-ahc-ink) 0%, var(--ui-ahc-ink) 38%, var(--ui-ahc-kpi-shine) 50%, var(--ui-ahc-ink) 62%, var(--ui-ahc-ink) 100%) 0 0 / 300% 100%; -webkit-background-clip: text; background-clip: text; color: transparent; animation: ui-ahc-shine 1500ms linear infinite; }
.ui-ahc__shine { background: linear-gradient(90deg, var(--ui-ahc-muted) 0%, var(--ui-ahc-muted) 38%, var(--ui-ahc-shine) 50%, var(--ui-ahc-muted) 62%, var(--ui-ahc-muted) 100%) 0 0 / 300% 100%; -webkit-background-clip: text; background-clip: text; color: transparent; animation: ui-ahc-shine 1500ms linear infinite; }
/* A task's source: a small logo tile */
.ui-ahc__logo { display: grid; flex: none; place-items: center; width: 16px; height: 16px; border-radius: 4px; background: var(--ui-ahc-card); box-shadow: var(--ui-ahc-shadow); color: var(--ui-ahc-faint); }
.ui-ahc__logo > svg { width: 10px; height: 10px; }
.ui-ahc__spin { animation: ui-ahc-spin 900ms linear infinite; }

/* A fresh handoff: a short streak of light runs along the line – its slices fade 0 → 100% → 0 from tail to head, with
   a faint glow – into the column, then on out of it */
.ui-ahc__light { filter: drop-shadow(0 0 1.5px var(--ui-ahc-glow)); }
.ui-ahc__streak { --d: 48px; fill: none; stroke: var(--ui-ahc-accent); stroke-width: 2; stroke-dasharray: 3.4px calc(var(--len) + var(--d) * 2); stroke-dashoffset: calc(var(--d) - var(--shift)); opacity: 0; animation: ui-ahc-streak 1000ms cubic-bezier(0.45, 0, 0.55, 1) both; }
.ui-ahc__streak[data-on] { animation-delay: 850ms; }

/* ----- A handoff column: gray like Agent Inbox's lanes, the tasks as cards, fading out at the bottom ----- */
.ui-ahc__col { grid-column: var(--c) / span 2; grid-row: 2; justify-self: center; width: calc(100% - 16px); max-width: 360px; height: 228px; padding: 4px; overflow: hidden; border-radius: calc(var(--aui-radius-lg, 10px) + 4px); background: var(--ui-ahc-lane); -webkit-mask-image: linear-gradient(180deg, #000 calc(100% - 64px), transparent); mask-image: linear-gradient(180deg, #000 calc(100% - 64px), transparent); }
.ui-ahc__col-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; height: 30px; padding: 0 6px 0 8px; }
.ui-ahc__col-who { display: flex; align-items: center; gap: 5px; min-width: 0; overflow: hidden; font-size: 11.5px; font-weight: 500; color: var(--ui-ahc-muted); white-space: nowrap; }
.ui-ahc__col-face { display: grid; flex: none; place-items: center; width: 16px; height: 16px; border-radius: 4px; background: var(--ui-ahc-card); box-shadow: var(--ui-ahc-shadow); font-size: 9px; color: var(--ui-ahc-ink); }
.ui-ahc__col-name { min-width: 1.6em; max-width: 45%; overflow: hidden; text-overflow: ellipsis; }
.ui-ahc[data-inbound] .ui-ahc__col-name { max-width: none; }
.ui-ahc[data-inbound] .ui-ahc__col { height: 360px; }
.ui-ahc__now-idle[data-state] { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__now-idle[data-state="done"] { color: var(--ui-ahc-board-green); }
.ui-ahc__now-idle[data-state="warning"] { color: var(--ui-ahc-red); }
.ui-ahc__now-task:last-child { max-width: none; }
.ui-ahc__col-face > svg { width: 10px; height: 10px; }
.ui-ahc__col-arrow { display: grid; flex: none; place-items: center; width: 12px; height: 12px; margin: 0 2px; color: var(--ui-ahc-faint); }
.ui-ahc__col-arrow > svg { width: 12px; height: 12px; }
.ui-ahc__lane-n { display: grid; flex: none; place-items: center; min-width: 20px; height: 18px; padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: color-mix(in srgb, var(--aui-purple, #a651f0) 6%, transparent); font-size: 11px; font-weight: 500; color: var(--ui-ahc-text); font-variant-numeric: tabular-nums; }
.ui-ahc__cards { display: grid; gap: 4px; }
.ui-ahc__lane-empty { padding: 18px 0 20px; font-size: 12px; text-align: center; color: var(--ui-ahc-faint); }
.ui-ahc__none { margin: 0; padding: 24px 0; font-size: 12.5px; text-align: center; color: var(--ui-ahc-faint); }
/* A card, as in Agent Inbox */
.ui-ahc__card { border-radius: var(--aui-radius-lg, 10px); background: var(--aui-surface, var(--ui-ahc-card)); box-shadow: 0 0 0 1px var(--ui-ahc-board-line), 0 1px 2px color-mix(in srgb, var(--aui-purple, #a651f0) 4%, transparent); animation: ui-ahc-drop 420ms var(--ui-ahc-ease) both; transition: box-shadow 200ms ease; }
.ui-ahc__card:hover, .ui-ahc__card[data-open] { box-shadow: 0 0 0 1px var(--aui-border-strong, #dcdce1), 0 4px 14px -6px color-mix(in srgb, var(--aui-purple, #a651f0) 12%, transparent); }
.ui-ahc__card-main { display: grid; gap: 3px; width: 100%; padding: 9px 12px 9px 10px; border-radius: var(--aui-radius-lg, 10px); text-align: left; cursor: pointer; }
.ui-ahc__card-main[data-movable] { cursor: grab; }
.ui-ahc__card-main[data-movable]:active { cursor: grabbing; }
.ui-ahc__card-main[aria-pressed="true"] { box-shadow: 0 0 0 1.5px var(--ui-ahc-link); }
.ui-ahc__card-top { display: flex; align-items: center; gap: 8px; min-width: 0; }
.ui-ahc__card-mark { display: grid; flex: none; place-items: center; width: 14px; height: 18px; color: var(--ui-ahc-blue); }
.ui-ahc__card-mark > svg { width: 12px; height: 12px; }
.ui-ahc__card-title { min-width: 0; overflow: hidden; font-size: 13px; font-weight: 500; line-height: 18px; color: var(--ui-ahc-board-ink); white-space: nowrap; text-overflow: ellipsis; }
/* What went along, in one line ending in … */
.ui-ahc__card-desc { padding-left: 22px; overflow: hidden; font-size: 12px; line-height: 17px; color: var(--ui-ahc-board-text); white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__card-foot { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-width: 0; margin-top: 3px; padding-left: 22px; }
.ui-ahc__card-state { font-size: 12px; color: var(--ui-ahc-board-text); }
.ui-ahc__card-state > .ui-ahc__swap-layer { display: inline-flex; align-items: center; gap: 6px; }
.ui-ahc__card-word { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-ahc__state-icon { display: grid; flex: none; place-items: center; width: 11px; height: 11px; color: var(--ui-ahc-blue); }
.ui-ahc__state-icon > svg { width: 11px; height: 11px; }
.ui-ahc__state-icon[data-status="done"] { color: var(--ui-ahc-board-green); }
.ui-ahc__state-icon[data-status="warning"] { color: var(--ui-ahc-board-amber); }
/* Open: what went along, as small rows – a lost one struck through, open it for why */
.ui-ahc__fold { display: grid; grid-template-rows: 0fr; transition: grid-template-rows 360ms var(--ui-ahc-ease); }
.ui-ahc__fold[data-open] { grid-template-rows: 1fr; }
.ui-ahc__clip { min-height: 0; overflow: hidden; }
.ui-ahc__items { display: grid; grid-template-columns: minmax(0, 1fr); gap: 4px; margin: 0 10px 10px !important; padding-top: 8px !important; border-top: 0.5px solid var(--ui-ahc-board-line); }
.ui-ahc__item-wrap { display: grid; grid-template-columns: minmax(0, 1fr); gap: 4px; min-width: 0; }
.ui-ahc__item { display: flex; align-items: center; gap: 6px; width: 100%; height: 24px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-ahc-well); font-size: 12px; font-weight: 500; color: var(--ui-ahc-ink); text-align: left; white-space: nowrap; cursor: default; }
.ui-ahc__kind { display: grid; flex: none; place-items: center; width: 12px; height: 12px; color: var(--ui-ahc-muted); }
.ui-ahc__kind > svg { width: 12px; height: 12px; }
.ui-ahc__label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-ahc__item[data-lost] { background: var(--ui-ahc-red-bg); box-shadow: inset 0 0 0 0.5px var(--ui-ahc-red-line); color: var(--ui-ahc-text); cursor: pointer; }
.ui-ahc__item[data-lost] .ui-ahc__kind { color: var(--ui-ahc-red); }
.ui-ahc__item[data-lost] .ui-ahc__label { text-decoration: line-through; text-decoration-color: var(--ui-ahc-red); }
.ui-ahc__why { display: grid; gap: 8px; padding: 9px 10px; border-radius: 8px; background: var(--ui-ahc-tip); font-size: 12px; line-height: 17px; color: #ffffff; animation: ui-ahc-fade 160ms ease both; }
.ui-ahc__restore { justify-self: start; height: 24px; padding: 0 9px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: rgba(255, 255, 255, 0.14); font-size: 12px; font-weight: 500; color: #ffffff; }
.ui-ahc__restore:hover { background: rgba(255, 255, 255, 0.22); }

/* Not much room per card: the status keeps its icon, the word goes */
@container ui-ahc (max-width: 760px) {
  .ui-ahc__status-word { display: none; }
  .ui-ahc__status { width: 20px; padding: 0; justify-content: center; }
  .ui-ahc__status[data-status="idle"] { display: none; }
  .ui-ahc__status > svg { margin: 0; }
}
/* Narrow: one column – an agent, its handoff, the next agent – no lines */
@container ui-ahc (max-width: 620px) {
  .ui-ahc__flow { grid-template-columns: minmax(0, 1fr); row-gap: 12px; }
  .ui-ahc__agent, .ui-ahc__col { grid-column: 1; grid-row: auto; width: 100%; max-width: none; }
  .ui-ahc__flow > * { order: var(--c); }
  .ui-ahc__lines { display: none; }
  .ui-ahc__col { width: calc(100% - 32px); height: 220px; }
}

@keyframes ui-ahc-streak { 0% { stroke-dashoffset: calc(var(--d) - var(--shift)); opacity: 0; } 12% { opacity: var(--o); } 85% { opacity: var(--o); } 100% { stroke-dashoffset: calc(var(--len) * -1 - var(--shift)); opacity: 0; } }
@keyframes ui-ahc-drop { from { opacity: 0; transform: translateY(-6px) scale(0.97); } }
@keyframes ui-ahc-strike { to { background-size: 100% 1px; opacity: 0.6; } }
@keyframes ui-ahc-in { from { opacity: 0; transform: translateY(3px); } }
@keyframes ui-ahc-fade { from { opacity: 0; } }
@keyframes ui-ahc-lift { from { transform: none; box-shadow: 0 0 0 1px var(--ui-ahc-board-line); } }
@keyframes ui-ahc-peek { from { opacity: 0; transform: translateY(-4px) scale(0.97); } }
@keyframes ui-ahc-swap-in { from { opacity: 0; transform: translateY(70%); filter: blur(2px); } }
@keyframes ui-ahc-swap-out { to { opacity: 0; transform: translateY(-70%); filter: blur(2px); } }
@keyframes ui-ahc-shine { from { background-position: 100% 0; } to { background-position: 0 0; } }
@keyframes ui-ahc-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) {
  .ui-ahc *, .ui-ahc *::before, .ui-ahc *::after { animation-duration: 1ms !important; }
  .ui-ahc__streak { display: none; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-ahc {
  --ui-ahc-card: #1c1c1f;
  --ui-ahc-strong: var(--aui-ink, #ffffff);
  --ui-ahc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-ahc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-ahc-muted: var(--aui-muted, rgba(255, 255, 255, 0.45));
  --ui-ahc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.28));
  --ui-ahc-line: var(--aui-border, rgba(255, 255, 255, 0.1));
  --ui-ahc-edge: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-ahc-well: var(--aui-subtle, rgba(255, 255, 255, 0.04));
  --ui-ahc-lane: var(--aui-subtle, #161618);
  --ui-ahc-blue: var(--aui-blue, #4fb0fe);
  --ui-ahc-board-line: var(--aui-border, #2c2c31);
  --ui-ahc-board-ink: var(--aui-ink, #ededed);
  --ui-ahc-board-text: var(--aui-muted, #b6b6bd);
  --ui-ahc-board-green: var(--aui-green, #5fc88a);
  --ui-ahc-board-amber: var(--aui-amber, #e6c24d);
  --ui-ahc-board-red: var(--aui-red, #ff6b5f);
  --ui-ahc-shine: rgba(255, 255, 255, 0.85);
  --ui-ahc-kpi-shine: rgba(255, 255, 255, 0.4);
  --ui-ahc-link: var(--aui-info, #4fb0fe);
  --ui-ahc-link-muted: rgba(255, 255, 255, 0.16);
  --ui-ahc-accent: var(--aui-info, #4fb0fe);
  --ui-ahc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-ahc-accent-bg: color-mix(in srgb, var(--aui-blue, #4fb0fe) 16%, transparent);
  --ui-ahc-glow: color-mix(in srgb, var(--aui-blue, #4fb0fe) 50%, transparent);
  --ui-ahc-drop: color-mix(in srgb, var(--aui-blue, #4fb0fe) 40%, transparent);
  --ui-ahc-green: var(--aui-green, #7dcf9f);
  --ui-ahc-green-bg: color-mix(in srgb, var(--aui-green, #7dcf9f) 16%, transparent);
  --ui-ahc-amber: var(--aui-amber, #e6c24d);
  --ui-ahc-amber-bg: color-mix(in srgb, var(--aui-amber, #e6c24d) 16%, transparent);
  --ui-ahc-red: var(--aui-red, #ff6b5f);
  --ui-ahc-red-line: color-mix(in srgb, var(--aui-red, #ff6b5f) 40%, transparent);
  --ui-ahc-red-bg: color-mix(in srgb, var(--aui-red, #ff6b5f) 8%, transparent);
  --ui-ahc-tip: #fafafa;
  --ui-ahc-lift: 0 10px 16px -6px rgba(0, 0, 0, 0.5);
  --ui-ahc-shadow: 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4);
}
:where(.dark, [data-theme="dark"]) .ui-ahc__why { color: #111113; }
:where(.dark, [data-theme="dark"]) .ui-ahc__restore { background: rgba(0, 0, 0, 0.08); color: #111113; }
`;
