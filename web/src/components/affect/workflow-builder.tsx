/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

/*
 * Workflow Builder – an automation as nodes on a canvas, in a single file.
 * A canvas with a quiet dot grid. The flow runs from top to bottom – small node cards joined by smooth curves,
 * each with its kind above it, a status chip and its details in a gray well – a click opens its settings.
 * A palette on the left lists every node by group – drag one onto the canvas (it turns into the card under the pointer),
 * or click it to add it under the selected node; a free output's blue + opens the same list right there. Drag from an
 * output dot to another node to join them; click a curve to select it, Delete removes it. A floating toolbar adds,
 * fits and zooms, and Run plays a run: the run panel slides up from the bottom and lists each step as it completes
 * while the matching node shimmers and the curves light up one after another.
 *
 * Adapted for Steve (web/src/auto/canvas.tsx drives it): the screen is controlled – the nodes, their ports and what
 * their cards say, the edges, the palette, the settings' content and the runs all come from Steve's flow model and its
 * API; the screen only does the canvas (pan, zoom, drag, wire, keyboard) and plays a run Steve really made. The App
 * Shell, path bar, Publish, comments, credits and the sample settings are left out; icons are Steve's (lucide), copy
 * goes through Steve's i18n and colours through its tokens (--aui-*, web/src/styles/affectui-theme.css).
 */

import { useEffect, useLayoutEffect, useId, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import {
  AlertTriangleIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  ClockIcon,
  CodeIcon,
  HistoryIcon,
  HourglassIcon,
  MaximizeIcon,
  MinusIcon,
  PlayIcon,
  PlusIcon,
  SearchIcon,
  Trash2Icon,
  XIcon,
  CheckIcon,
  type LucideIcon,
} from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { msDur } from "@/i18n/format";
import { t } from "@/i18n";
import { reducedMotion } from "@/lib/motion";

/** A lucide icon at the size and stroke the screen asks for (the original's Hugeicons wrapper) */
const Icon = ({ icon: I, size = 16, stroke = 1.9 }: { icon: LucideIcon; size?: number; stroke?: number }) => <I size={size} strokeWidth={stroke} aria-hidden="true" />;
/** ↑ ↓ / ← → / Home End between a set of tabs (the App Shell's rove) */
function rove<T extends string>(e: KeyboardEvent<HTMLElement>, items: readonly T[], current: T, pick: (item: T) => void) {
  const at = items.indexOf(current);
  const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : e.key === "ArrowRight" || e.key === "ArrowDown" ? (at + 1) % items.length : e.key === "ArrowLeft" || e.key === "ArrowUp" ? (at - 1 + items.length) % items.length : -1;
  if (next < 0) return;
  e.preventDefault();
  pick(items[next]);
  requestAnimationFrame(() => (e.currentTarget?.querySelectorAll<HTMLElement>('[role="tab"]')[next] ?? null)?.focus());
}
/** A quiet empty state inside the canvas or the run panel */
const EmptyState = ({ title, text, action }: { title: string; text: string; action?: ReactNode }) => (
  <div className="ui-wf-emptystate">
    <b>{title}</b>
    <span>{text}</span>
    {action}
  </div>
);

export type WfKind = "trigger" | "agent" | "tool" | "logic" | "action";
/** One kind of node, as the palette lists it */
export type WfNodeType = {
  type: string;
  label: string;
  kind: WfKind;
  icon: LucideIcon;
  /** What the palette's tooltip says about it */
  description: string;
};
export type WfPaletteGroup = { id: string; label: string; items: WfNodeType[] };
/** A card's two detail lines: what it does in words, then the specifics in mono – or a few chips */
export type Detail = { text: string; mono?: string; chips?: string[]; warn?: boolean };
/** An output on a node's bottom edge; `label` names a branch */
export type WfPort = { id: string; label?: string };
export type WfNode = {
  id: string;
  kind: WfKind;
  /** The small tag above the card */
  tag: string;
  title: string;
  icon: LucideIcon;
  x: number;
  y: number;
  /** Whether it takes connections (a trigger doesn't) */
  input: boolean;
  ports: WfPort[];
  detail: Detail;
  /** The status chip when no run is shown */
  chip: { text: string; tone?: "warn" | "done" | "info" | "attn" };
  /** Can be removed (Delete, the settings' bin) */
  removable: boolean;
  /** Extra attributes on the card (test hooks) */
  attrs?: Record<string, string>;
};
/** A curve from one node's output port to another node's input */
export type WfEdge = { id: string; from: string; port: string; to: string; label?: string };
/** A step of a real run, in the order it ran; `via` is the edge it came along */
export type WfStep = { node: string; title: string; kind: WfKind; ms: number; state: "done" | "failed" | "waiting"; via?: string; error?: string; note?: string; attrs?: Record<string, string> };
/** A run as the run panel lists it; `steps` once they are loaded */
export type WfRun = { id: string; title: string; when: string; state: "running" | "waiting" | "done" | "failed" | "idle"; steps?: WfStep[]; error?: string | null; payload?: unknown; extra?: ReactNode; attrs?: Record<string, string> };

/* ---------- Geometry ---------- */

export const NODE_W = 272;
/** Every card is the same height, so nothing jumps when its content changes:
 *  12 padding · 24 header · 8 · 18 description · 8 · 36 well · 4 */
export const NODE_H = 110;
/** The kind tag above a card */
export const TAG_H = 26;
/** How tall a node is with the + under a free output */
export const PLUS_H = 34;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const snap = (v: number) => Math.round(v / 10) * 10;

type Port = { id: string; label?: string; dx: number };
/** Output ports on a node's bottom edge, spread evenly – one in the middle, branches side by side */
const outPorts = (n: WfNode): Port[] => n.ports.map((p, i) => ({ ...p, dx: Math.round((NODE_W * (i + 1)) / (n.ports.length + 1)) }));
/** Where a node's ports sit on the canvas – the input in the middle of the top edge */
const inPoint = (n: WfNode) => ({ x: n.x + NODE_W / 2, y: n.y });
const outPoint = (n: WfNode, p: Port) => ({ x: n.x + p.dx, y: n.y + NODE_H });
const hasInput = (n: WfNode) => n.input;

/** A smooth curve that leaves a port downwards and enters the next node from the top */
const curve = (x1: number, y1: number, x2: number, y2: number) => {
  const d = Math.max(28, Math.abs(y2 - y1) / 2);
  return `M${x1} ${y1} C${x1} ${y1 + d} ${x2} ${y2 - d} ${x2} ${y2}`;
};
/** A point on that curve at t (0 = the output, 1 = the input) – where an edge's branch tag sits */
const curvePoint = (x1: number, y1: number, x2: number, y2: number, t: number) => {
  const d = Math.max(28, Math.abs(y2 - y1) / 2);
  const u = 1 - t;
  const bez = (a: number, b: number, c: number, e: number) => u * u * u * a + 3 * u * u * t * b + 3 * u * t * t * c + t * t * t * e;
  return { x: bez(x1, x1, x2, x2), y: bez(y1, y1 + d, y2 - d, y2) };
};

/* ---------- Runs ---------- */

/** `at`: the step that is on; `phase`: its impulse still travelling along the edge in, or the step itself running */
type Anim = { key: string; at: number; phase: "travel" | "run" };
/** How long an impulse takes along an edge, and a step's trace round its card */
const TRAVEL_MS = 520;
const TRACE_MS = 1500;
/** Durations roll up in the UI language (Steve's msDur) */
const ms = (v: number) => (Number.isFinite(v) && v >= 0 ? msDur(v) : "–");

/* ---------- Small marks ---------- */

/** The shared spinner; its size comes from where it sits */
const Spin = () => <Spinner className="ui-wf-spin" />;
const Done = () => (
  <span className="ui-wf-done" aria-hidden="true">
    <Icon icon={CheckIcon} size={10} stroke={2.4} />
  </span>
);
const Failed = () => (
  <span className="ui-wf-done" data-failed aria-hidden="true">
    <Icon icon={XIcon} size={10} stroke={2.4} />
  </span>
);
/** The step a replay is at: the run already happened, so no spinner */
const Replay = () => (
  <span className="ui-wf-done" data-waiting aria-hidden="true">
    <Icon icon={PlayIcon} size={9} stroke={2.4} />
  </span>
);
const Waiting = () => (
  <span className="ui-wf-done" data-waiting aria-hidden="true">
    <Icon icon={HourglassIcon} size={10} stroke={2.2} />
  </span>
);

/** Keeps Tab and Shift+Tab inside a sheet on a phone */
const trapTab = (e: KeyboardEvent<HTMLElement>) => {
  if (e.key !== "Tab") return;
  const list = [...e.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled), input:not([type="hidden"]), textarea, [tabindex]:not([tabindex="-1"])')].filter((el) => el.offsetParent !== null);
  if (!list.length) return;
  const first = list[0];
  const last = list[list.length - 1];
  if (e.shiftKey && (document.activeElement === first || document.activeElement === e.currentTarget)) (e.preventDefault(), last.focus());
  else if (!e.shiftKey && document.activeElement === last) (e.preventDefault(), first.focus());
};

/** Typing in a field, a text area or a menu – canvas shortcuts stay out of the way */
const typing = (el: EventTarget | null) => el instanceof HTMLElement && !!el.closest("input, textarea, select, [contenteditable], [role=listbox], [role=menu], [data-slot=popover-content], [data-slot=select-content]");

/* ---------- Screen ---------- */

export type WorkflowBuilderScreenProps = {
  className?: string;
  style?: CSSProperties;
  /** The nodes on the canvas – x and y in canvas pixels */
  nodes: WfNode[];
  /** The curves between them – `port` is the output they leave from */
  edges: WfEdge[];
  /** The palette's groups and node types (no triggers: a flow has its one trigger) */
  palette: WfPaletteGroup[];
  /** The node whose settings are open */
  settings: string | null;
  onSettings: (id: string | null) => void;
  /** What the settings show for a node, under their head */
  renderSettings: (id: string) => ReactNode;
  /** Adds a node of a type at a place, after a node's output when `from` is given; returns its id */
  onAdd: (type: string, at: { x: number; y: number }, from?: { node: string; port: string }) => string | undefined;
  onMove: (id: string, x: number, y: number) => void;
  onConnect: (from: string, port: string, to: string) => void;
  onDisconnect: (edge: string) => void;
  onRemove: (id: string) => void;
  /** ⌘Z after a removal; false when there was nothing to bring back */
  onUndo?: () => boolean;
  /** The runs to list – the newest first */
  runs: WfRun[];
  /** The run to play on the canvas: a new key replays it */
  play: { key: string; run: WfRun } | null;
  /** Run pressed: Steve makes a dry run and hands it back through `play` */
  onRun: () => void;
  /** A run's head clicked: Steve loads its steps and plays it */
  onOpenRun: (id: string) => void;
  /** The run panel closed: the canvas lets go of the played run */
  onCloseRun?: () => void;
  /** A dry run is under way */
  busy?: boolean;
  /** Run is off (e.g. while saving) */
  runDisabled?: boolean;
};

type View = { x: number; y: number; k: number };
export type RunTab = "runs" | "timeline" | "payload" | "errors";
type Gesture =
  | { type: "pan"; sx: number; sy: number; view: View; moved: boolean; background: boolean }
  | { type: "node"; id: string; sx: number; sy: number; ox: number; oy: number; moved: boolean }
  | { type: "wire"; from: string; port: string }
  | { type: "pinch"; d: number; mx: number; my: number; view: View };

export function WorkflowBuilderScreen({
  className,
  style,
  nodes: items,
  edges: links,
  palette,
  settings: settingsId,
  onSettings,
  renderSettings,
  onAdd,
  onMove,
  onConnect,
  onDisconnect,
  onRemove,
  onUndo,
  runs,
  play,
  onRun,
  onOpenRun,
  onCloseRun,
  busy = false,
  runDisabled = false,
}: WorkflowBuilderScreenProps) {
  const [view, setView] = useState<View>({ x: 80, y: 120, k: 1 });
  const viewRef = useRef(view);
  viewRef.current = view;
  const [size, setSize] = useState({ w: 0, h: 0, root: 0 });
  const [narrow, setNarrow] = useState(false);
  // A new flow opens with the palette; one that has its nodes already opens on the canvas (Add node brings the palette)
  const [paletteOpen, setPaletteOpen] = useState(() => items.length <= 1);
  const [space, setSpace] = useState(false);
  const [panning, setPanning] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [picker, setPicker] = useState<{ from: string; port: string; x: number; y: number } | null>(null);
  const [tip, setTip] = useState<{ type: string; text: string; x: number; y: number } | null>(null);
  const [runOpen, setRunOpen] = useState(false);
  const [runH, setRunH] = useState(() => (typeof window !== "undefined" && window.innerWidth < 640 ? 220 : 280));
  const [resizing, setResizing] = useState(false);
  const [tab, setTab] = useState<RunTab>("runs");
  const [anim, setAnim] = useState<Anim | null>(null);
  const [selectedEdge, setSelectedEdge] = useState<string | null>(null);
  // A connection being dragged from an output: where the pointer is (canvas pixels) and the node it would join
  const [wire, setWire] = useState<{ from: string; port: string; x: number; y: number; to: string | null } | null>(null);
  // A palette item being dragged: where it is in the canvas area and whether it would land on the canvas
  const [ghost, setGhost] = useState<{ type: string; x: number; y: number; over: boolean } | null>(null);

  const root = useRef<HTMLDivElement>(null);
  const area = useRef<HTMLDivElement>(null);
  const vp = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const gesture = useRef<Gesture | null>(null);
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  // In a page that scrolls, a plain wheel pans the canvas only after a click into it – until then the page scrolls as usual
  const engaged = useRef(false);
  const focusPanel = useRef(false);
  const fitted = useRef(false);
  const narrowNow = useRef(false);
  const count = useRef(items.length);
  count.current = items.length;
  const allTypes = palette.flatMap((g) => g.items);
  const nodeOf = (id?: string | null) => items.find((n) => n.id === id);

  // Pointer travel is divided by any scale on the page, so drags follow the pointer exactly
  const scaleOf = () => {
    const el = vp.current;
    return el ? el.getBoundingClientRect().width / el.offsetWidth || 1 : 1;
  };
  /** Pointer position in the canvas's own pixels */
  const local = (cx: number, cy: number) => {
    const r = vp.current?.getBoundingClientRect();
    const s = scaleOf();
    return r ? { x: (cx - r.left) / s, y: (cy - r.top) / s } : { x: 0, y: 0 };
  };
  const toWorld = (p: { x: number; y: number }, v = viewRef.current) => ({ x: (p.x - v.x) / v.k, y: (p.y - v.y) / v.k });
  const focusNode = (id: string) => requestAnimationFrame(() => root.current?.querySelector<HTMLElement>(`[data-node="${CSS.escape(id)}"]`)?.focus({ preventScroll: true }));

  // Size of the canvas (for fitting) and of the screen (narrow: palette and settings turn into sheets)
  useLayoutEffect(() => {
    const el = root.current;
    const canvas = vp.current;
    if (!el || !canvas) return;
    const measure = () => {
      if (!el.offsetWidth) return;
      const isNarrow = el.offsetWidth < 640;
      // Narrow: the palette waits behind Add node; wide again, it comes back for a flow still to be drawn
      if (isNarrow !== narrowNow.current) {
        narrowNow.current = isNarrow;
        setNarrow(isNarrow);
        setPaletteOpen(!isNarrow && count.current <= 1);
      }
      setSize({ w: canvas.offsetWidth, h: canvas.offsetHeight, root: el.offsetHeight });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    ro.observe(canvas);
    return () => ro.disconnect();
  }, []);

  /**
   * Fits every node into the part of the canvas the palette, settings and toolbar leave free – never above 100%.
   * `first`: the view the screen opens with, which stays at 90% or more (70% on a phone) and starts at the top when that doesn't fit
   */
  const fit = (list = items, first = false, height?: number) => {
    const w = vp.current?.offsetWidth ?? size.w;
    // `height`: the canvas's height once the run panel has finished sliding in or out
    const h = height ?? vp.current?.offsetHeight ?? size.h;
    if (!w || !h) return;
    if (!list.length) return setView({ x: w / 2, y: h / 2, k: 1 });
    const minX = Math.min(...list.map((n) => n.x));
    const minY = Math.min(...list.map((n) => n.y)) - TAG_H;
    const maxX = Math.max(...list.map((n) => n.x + NODE_W));
    const maxY = Math.max(...list.map((n) => n.y + NODE_H + PLUS_H));
    const pal = !narrow && paletteOpen;
    const left = pal ? 236 : 16;
    const right = !narrow && settingsId ? 352 : 16;
    // Room for the toolbar at the bottom
    const top = 24;
    const bottom = 72;
    const aw = Math.max(80, w - left - right - 48);
    const ah = Math.max(80, h - top - bottom - 24);
    // Phones keep the cards readable too: when it all can't fit, the flow starts at the top left and pans from there
    const k = clamp(Math.min(aw / (maxX - minX), ah / (maxY - minY)), first ? (narrow ? 0.7 : 0.9) : narrow ? 0.5 : 0.2, 1);
    const wide = (maxX - minX) * k > aw;
    const tall = (maxY - minY) * k > ah;
    // Too wide to fit: the view starts centred on the top of the flow (the trigger), the rest pans into view
    const top0 = [...list].sort((a, b) => a.y - b.y || a.x - b.x)[0];
    setView({
      x: wide ? left + 24 + aw / 2 - (top0.x + NODE_W / 2) * k : left + 24 + (aw - (maxX - minX) * k) / 2 - minX * k,
      y: (tall ? top + TAG_H : top + 12 + (ah - (maxY - minY) * k) / 2) - minY * k,
      k,
    });
  };
  // Fit once the canvas has its size, so the flow reads at once
  useLayoutEffect(() => {
    if (fitted.current || !size.w) return;
    fitted.current = true;
    fit(items, true);
  });

  const zoomAt = (p: { x: number; y: number }, factor: number) =>
    setView((v) => {
      const k = clamp(v.k * factor, 0.2, 2);
      return { k, x: p.x - (p.x - v.x) * (k / v.k), y: p.y - (p.y - v.y) * (k / v.k) };
    });
  const zoomBy = (factor: number) => zoomAt({ x: size.w / 2, y: size.h / 2 }, factor);

  // Wheel: ctrl / ⌘ (and a trackpad pinch, which arrives as ctrl + wheel) zooms at the pointer; a plain wheel pans
  useEffect(() => {
    const el = vp.current;
    if (!el) return;
    const wheel = (e: WheelEvent) => {
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        const r = el.getBoundingClientRect();
        const s = r.width / el.offsetWidth || 1;
        zoomAt({ x: (e.clientX - r.left) / s, y: (e.clientY - r.top) / s }, Math.exp(-e.deltaY * 0.01));
        return;
      }
      // The canvas sits in a page that scrolls: a plain wheel keeps scrolling it until you click into the canvas
      if (!engaged.current) return;
      e.preventDefault();
      setView((v) => ({ ...v, x: v.x - e.deltaX, y: v.y - e.deltaY }));
    };
    const leave = () => (engaged.current = false);
    el.addEventListener("wheel", wheel, { passive: false });
    el.addEventListener("pointerleave", leave);
    return () => {
      el.removeEventListener("wheel", wheel);
      el.removeEventListener("pointerleave", leave);
    };
  }, []);

  // Space held down turns any drag into panning (not while typing or on a button, where Space presses it)
  useEffect(() => {
    const down = (e: globalThis.KeyboardEvent) => {
      if (e.key !== " " || typing(e.target) || (e.target instanceof HTMLElement && e.target.closest("button, [role=button]"))) return;
      if (!vp.current?.contains(document.activeElement)) return;
      e.preventDefault();
      setSpace(true);
    };
    const up = (e: globalThis.KeyboardEvent) => e.key === " " && setSpace(false);
    const blur = () => setSpace(false);
    document.addEventListener("keydown", down);
    document.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    return () => {
      document.removeEventListener("keydown", down);
      document.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
    };
  }, []);

  /* ----- Nodes ----- */

  const freePort = (n: WfNode) => outPorts(n).find((p) => !links.some((e) => e.from === n.id && e.port === p.id));

  /** A free spot: to the right of the wanted one until nothing is in the way (the flow runs down, so siblings sit side by side) */
  const place = (x: number, y: number, list = items) => {
    let at = x;
    // A node only blocks the spot when it overlaps that row – the one it hangs from, a row above, doesn't
    while (list.some((n) => at < n.x + NODE_W + 24 && n.x < at + NODE_W + 24 && Math.abs(n.y - y) < NODE_H + TAG_H)) at += 40;
    return { x: snap(at), y: snap(y) };
  };

  /** Adds a node – at a point, after a node's output (joined to it), or in the middle of the view; its settings open */
  const add = (type: string, opts: { at?: { x: number; y: number }; from?: { node: string; port: string } } = {}) => {
    const ty = allTypes.find((x) => x.type === type);
    if (!ty) return;
    const fromNode = nodeOf(opts.from?.node);
    let pos: { x: number; y: number };
    // Dropped onto other cards, it steps aside to the first free spot in that row
    if (opts.at) pos = place(opts.at.x, opts.at.y);
    else if (fromNode) {
      const p = outPorts(fromNode).find((x) => x.id === opts.from?.port);
      // Under the port it leaves from, one row down
      pos = place(fromNode.x + (p?.dx ?? NODE_W / 2) - NODE_W / 2, fromNode.y + NODE_H + TAG_H + 34);
    } else {
      const c = toWorld({ x: size.w / 2, y: size.h / 2 });
      pos = place(c.x - NODE_W / 2, c.y - NODE_H / 2);
    }
    const id = onAdd(type, pos, fromNode && opts.from ? opts.from : undefined);
    if (!id) return;
    setSelected(id);
    openSettings(id);
  };
  /** Palette click or Enter: after the selected node (its first free output, else its first), otherwise in the middle of the view */
  const addNext = (type: string) => {
    if (skipPick.current) return;
    const after = nodeOf(selected ?? settingsId);
    const port = after && (freePort(after) ?? outPorts(after)[0]);
    if (after && port) add(type, { from: { node: after.id, port: port.id } });
    else add(type);
    if (narrow) setPaletteOpen(false);
  };

  const remove = (id: string) => {
    const node = nodeOf(id);
    if (!node?.removable) return;
    const at = items.indexOf(node);
    onRemove(id);
    if (settingsId === id) onSettings(null);
    setSelected(null);
    // The focus moves on to the next node (or the canvas when it was the last one)
    const next = items[at + 1] ?? items[at - 1];
    if (next && next.id !== id) focusNode(next.id);
    else requestAnimationFrame(() => vp.current?.focus({ preventScroll: true }));
  };

  /* ----- Settings ----- */

  const openSettings = (id: string, withFocus = false) => {
    setSelected(id);
    onSettings(id);
    setPicker(null);
    focusPanel.current = withFocus;
  };
  const closeSettings = () => {
    const id = settingsId;
    const inside = !!panelRef.current?.contains(document.activeElement);
    onSettings(null);
    if (id && inside) focusNode(id);
  };
  useEffect(() => {
    if (settingsId && focusPanel.current) panelRef.current?.focus({ preventScroll: true });
    focusPanel.current = false;
    if (settingsId) setSelected(settingsId);
  }, [settingsId]);

  /* ----- Runs ----- */

  // A new run to play: the panel opens on its steps and the canvas plays them one after another
  useEffect(() => {
    if (!play) return void setAnim(null);
    setAnim({ key: play.key, at: 0, phase: "run" });
    setRunOpen(true);
    // On a phone a played run gets about half the screen, so its steps and error show without scrolling a sliver
    if (narrowNow.current && size.root) setRunH((h) => Math.max(h, Math.min(Math.round(size.root * 0.5), size.root - 200)));
    setTab("runs");
  }, [play?.key]);
  // On a phone the page lines the canvas up so the open run panel sits just above the bottom tab bar (60px): a played
  // run's steps and error show without scrolling, and the browser's own jump to keep the focused toolbar button in
  // view doesn't leave the canvas above the screen
  useEffect(() => {
    if (!runOpen || !narrowNow.current) return;
    const timer = window.setTimeout(() => {
      const el = root.current;
      const r = el?.getBoundingClientRect();
      if (!el || !r) return;
      const room = window.innerHeight - (window.innerWidth < 768 ? 68 : 8);
      const top = 56;
      // Bottom on the line, unless that would push the top of the canvas under the header
      const d = Math.min(r.bottom - room, r.top - top);
      if (Math.abs(d) < 4) return;
      let box: HTMLElement | null = el.parentElement;
      while (box && !(/(auto|scroll)/.test(getComputedStyle(box).overflowY) && box.scrollHeight > box.clientHeight)) box = box.parentElement;
      const by = { top: d, behavior: (reducedMotion() ? "auto" : "smooth") as ScrollBehavior };
      if (box) box.scrollBy(by);
      else window.scrollBy(by);
    }, 380);
    return () => window.clearTimeout(timer);
  }, [runOpen, play?.key]);
  const shown = play && anim?.key === play.key ? play.run : undefined;
  const steps = shown?.steps ?? [];
  const at = anim ? Math.min(anim.at, steps.length) : steps.length;
  const current = shown && at < steps.length ? steps[at] : undefined;
  // The node whose trace runs, and the edge an impulse is travelling along on its way there
  const running = current && anim?.phase === "run" ? current.node : null;
  const travelling = current && anim?.phase === "travel" ? current.via : undefined;
  // The node the impulse comes from: its kind colours the trail, the dot and the glow where it arrives
  const sender = current?.via ? nodeOf(links.find((e) => e.id === current.via)?.from) : undefined;
  const senderKind = sender?.kind;
  const travel = travelling && senderKind ? { edge: travelling, kind: senderKind } : undefined;
  const doneSteps = shown ? steps.slice(0, at) : [];
  const doneNodes = new Map(doneSteps.map((s) => [s.node, s] as const));
  // What a screen reader hears: one polite line per finished step and at the end – not every frame of the animation
  const announce = (() => {
    if (!shown || !runOpen) return "";
    if (!steps.length) return t("auto:builder.noSteps");
    const last = steps[at - 1];
    if (at >= steps.length) {
      const total = steps.reduce((n, x) => n + x.ms, 0);
      return last?.error ? t("auto:builder.sayFailed", { title: last.title, error: last.error }) : t("auto:builder.sayDone", { count: steps.length, ms: ms(total) });
    }
    return last ? t("auto:builder.sayNext", { title: last.title, ms: ms(last.ms), next: steps[at].title }) : t("auto:builder.sayRunning", { title: steps[0].title });
  })();
  // Edges the run has taken stay in accent; the one being travelled lights once the impulse arrives
  const litEdges = new Set(shown ? steps.slice(1, at + (anim?.phase === "run" ? 1 : 0)).flatMap((s) => (s.via ? [s.via] : [])) : []);
  // Once the run has played, the nodes it didn't pass step back
  const passed = shown && at >= steps.length ? new Set(steps.map((s) => s.node)) : null;
  // One step after another: the impulse travels along the edge in, then the step's trace runs once round its card.
  // Reduced motion: no travelling, a short beat per step
  useEffect(() => {
    if (!anim || !shown || anim.at >= steps.length) return;
    const calm = reducedMotion();
    const wait = anim.phase === "travel" ? TRAVEL_MS : calm ? 300 : Math.min(TRACE_MS, 900);
    const timer = window.setTimeout(
      () =>
        setAnim((a) => {
          if (!a || a.key !== anim.key) return a;
          return a.phase === "travel" ? { ...a, phase: "run" } : { ...a, at: a.at + 1, phase: !calm && steps[a.at + 1]?.via ? "travel" : "run" };
        }),
      wait,
    );
    return () => window.clearTimeout(timer);
  }, [anim, shown]);
  // While the run plays, the view glides (never zooms) so the running node stays in sight above the panel
  const [follow, setFollow] = useState(false);
  useEffect(() => {
    if (!running) return;
    const n = nodeOf(running);
    const h = vp.current?.offsetHeight ?? size.h;
    if (!n) return;
    const v = viewRef.current;
    const top = (n.y - TAG_H) * v.k + v.y;
    const bottom = (n.y + NODE_H) * v.k + v.y;
    const dy = bottom > h - 80 ? h - 80 - bottom : top < 30 ? 30 - top : 0;
    if (!dy) return;
    setFollow(true);
    setView({ ...v, y: v.y + dy });
    const timer = window.setTimeout(() => setFollow(false), 700);
    return () => window.clearTimeout(timer);
  }, [running]);
  const closeRun = () => {
    const inside = !!root.current?.querySelector(".ui-wf-run")?.contains(document.activeElement);
    setRunOpen(false);
    onCloseRun?.();
    if (inside) requestAnimationFrame(() => root.current?.querySelector<HTMLElement>(".ui-wf-tb__run")?.focus({ preventScroll: true }));
  };

  /* ----- Escape and shortcuts ----- */

  // Escape closes the topmost thing: the node picker, the settings, the palette sheet, the run panel – focus goes back where it came from
  useEffect(() => {
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || !root.current?.contains(document.activeElement) && !wire && !picker) return;
      const at = document.activeElement;
      if (wire) {
        gesture.current = null;
        setWire(null);
      } else if (picker) {
        const from = picker;
        setPicker(null);
        requestAnimationFrame(() => root.current?.querySelector<HTMLElement>(`[data-add="${CSS.escape(from.from + ":" + from.port)}"]`)?.focus({ preventScroll: true }));
      } else if (settingsId && (narrow || panelRef.current?.contains(at))) closeSettings();
      else if (paletteOpen && (narrow || root.current?.querySelector(".ui-wf-pal")?.contains(at))) {
        setPaletteOpen(false);
        setTip(null);
        requestAnimationFrame(() => root.current?.querySelector<HTMLElement>(".ui-wf-tb__add")?.focus({ preventScroll: true }));
      } else if (runOpen && root.current?.querySelector(".ui-wf-run")?.contains(at)) closeRun();
      else if (selectedEdge) setSelectedEdge(null);
      else if (settingsId) closeSettings();
      else if (selected) setSelected(null);
      else return;
      e.preventDefault();
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  });

  const shortcuts = (e: KeyboardEvent<HTMLElement>) => {
    if (typing(e.target) || e.altKey) return;
    // Only on the canvas itself: the settings and the run panel keep their keys
    if (!(e.target instanceof Element) || !vp.current?.contains(e.target)) return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "z" && onUndo) {
      if (onUndo()) e.preventDefault();
      return;
    }
    if (e.metaKey || e.ctrlKey) return;
    const k = e.key;
    if ((k === "Delete" || k === "Backspace") && selectedEdge) {
      e.preventDefault();
      return disconnect(selectedEdge);
    }
    if (k === "+" || k === "=") zoomBy(1.2);
    else if (k === "-") zoomBy(1 / 1.2);
    else if (k === "!" || (k === "1" && e.shiftKey)) fit();
    else return;
    e.preventDefault();
  };

  /* ----- Pointer: pan, drag nodes, pinch ----- */

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    engaged.current = true;
    const target = e.target as HTMLElement;
    if (target.closest("button, input, textarea")) return;
    const p = local(e.clientX, e.clientY);
    pointers.current.set(e.pointerId, p);
    e.currentTarget.setPointerCapture(e.pointerId);
    // Two fingers: pinch to zoom, the midpoint pans along
    if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      gesture.current = { type: "pinch", d: Math.hypot(a.x - b.x, a.y - b.y) || 1, mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2, view: viewRef.current };
      return;
    }
    if (e.button !== 0 && e.button !== 1) return;
    // An output dot: drag a connection from it
    const out = target.closest<HTMLElement>("[data-out]")?.dataset.out;
    if (out && e.button === 0 && !space) {
      const cut = out.indexOf(":");
      const from = out.slice(0, cut);
      const port = out.slice(cut + 1);
      const w = toWorld(p);
      gesture.current = { type: "wire", from, port };
      setWire({ from, port, x: w.x, y: w.y, to: null });
      setSelectedEdge(null);
      return;
    }
    // A curve: select it (Delete removes it)
    const edge = target.closest<SVGElement>("[data-edge]")?.dataset.edge;
    if (edge && e.button === 0 && !space) {
      setSelectedEdge(edge);
      setSelected(null);
      vp.current?.focus({ preventScroll: true });
      return;
    }
    const nodeEl = target.closest<HTMLElement>("[data-node]");
    const node = nodeEl && nodeOf(nodeEl.dataset.node);
    if (node && !space && e.button === 0) {
      setSelectedEdge(null);
      gesture.current = { type: "node", id: node.id, sx: e.clientX, sy: e.clientY, ox: node.x, oy: node.y, moved: false };
      return;
    }
    gesture.current = { type: "pan", sx: e.clientX, sy: e.clientY, view: viewRef.current, moved: false, background: !node };
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (pointers.current.has(e.pointerId)) pointers.current.set(e.pointerId, local(e.clientX, e.clientY));
    const g = gesture.current;
    if (!g) return;
    if (g.type === "pinch") {
      const [a, b] = [...pointers.current.values()];
      if (!a || !b) return;
      const k = clamp((g.view.k * Math.hypot(a.x - b.x, a.y - b.y)) / g.d, 0.2, 2);
      const mx = (a.x + b.x) / 2;
      const my = (a.y + b.y) / 2;
      setView({ k, x: mx - (g.mx - g.view.x) * (k / g.view.k), y: my - (g.my - g.view.y) * (k / g.view.k) });
      return;
    }
    if (g.type === "wire") {
      const w = toWorld(local(e.clientX, e.clientY));
      setWire((d) => (d ? { ...d, x: w.x, y: w.y, to: wireTarget(g.from, g.port, w) } : d));
      return;
    }
    const s = scaleOf();
    const dx = (e.clientX - g.sx) / s;
    const dy = (e.clientY - g.sy) / s;
    if (!g.moved && Math.hypot(dx, dy) < 3) return;
    g.moved = true;
    if (g.type === "pan") {
      setPanning(true);
      setView({ ...g.view, x: g.view.x + dx, y: g.view.y + dy });
    } else {
      const k = viewRef.current.k;
      onMove(g.id, snap(g.ox + dx / k), snap(g.oy + dy / k));
    }
  };
  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    pointers.current.delete(e.pointerId);
    const g = gesture.current;
    if (g?.type === "pinch") {
      // The other finger rests until it lifts too
      const rest = [...pointers.current.entries()][0];
      gesture.current = null;
      if (rest) pointers.current.delete(rest[0]);
      return;
    }
    gesture.current = null;
    setPanning(false);
    if (g?.type === "wire") {
      // Dropped on a node that can take it: joined; anywhere else: nothing happens
      if (wire?.to && e.type !== "pointercancel") connect(g.from, g.port, wire.to);
      setWire(null);
      return;
    }
    if (!g || g.moved || e.type === "pointercancel") return;
    // A click without a drag: on a node it selects and opens its settings, on the background it lets go of everything
    if (g.type === "node") openSettings(g.id);
    else if (g.background) {
      setSelected(null);
      setSelectedEdge(null);
      if (settingsId) onSettings(null);
      setPicker(null);
    }
  };

  /* ----- Connections ----- */

  /** Whether `to` is reachable from `from` along the connections */
  const reaches = (from: string, to: string) => {
    const seen = new Set([from]);
    const stack = [from];
    while (stack.length) {
      const c = stack.pop()!;
      if (c === to) return true;
      for (const e of links) {
        if (e.from !== c || seen.has(e.to)) continue;
        seen.add(e.to);
        stack.push(e.to);
      }
    }
    return false;
  };
  /**
   * A node can take a connection when it has an input, isn't the node it comes from, isn't joined to that port already
   * and doesn't lead back to it – a flow has no loops
   */
  const canWire = (from: string, port: string, to: WfNode) =>
    to.id !== from && hasInput(to) && !links.some((e) => e.from === from && e.port === port && e.to === to.id) && !reaches(to.id, from);
  /** The node a dragged connection snaps to: the pointer on its card, or within 28px of its input dot */
  const wireTarget = (from: string, port: string, w: { x: number; y: number }) => {
    const k = viewRef.current.k;
    let best: { id: string; d: number } | null = null;
    for (const n of items) {
      if (!canWire(from, port, n)) continue;
      const on = w.x >= n.x && w.x <= n.x + NODE_W && w.y >= n.y && w.y <= n.y + NODE_H;
      const i = inPoint(n);
      const d = on ? 0 : Math.hypot(i.x - w.x, i.y - w.y) * k;
      if (d < 28 && (!best || d < best.d)) best = { id: n.id, d };
    }
    return best?.id ?? null;
  };
  const connect = (from: string, port: string, to: string) => {
    const target = nodeOf(to);
    if (!target || !canWire(from, port, target)) return;
    onConnect(from, port, to);
  };
  const disconnect = (id: string) => {
    if (!links.some((e) => e.id === id)) return;
    onDisconnect(id);
    if (selectedEdge === id) setSelectedEdge(null);
  };

  /* ----- Dragging from the palette ----- */

  // Pointer-driven, so the thing under the pointer is the node card itself, at the canvas's zoom (no browser ghost image)
  const pending = useRef<{ type: string; sx: number; sy: number; id: number; started: boolean } | null>(null);
  const skipPick = useRef(false);
  const paletteDrag = useRef({ move: (_e: globalThis.PointerEvent) => {}, up: (_e: globalThis.PointerEvent) => {} });
  const startPaletteDrag = (type: string, e: PointerEvent<HTMLElement>) => {
    if (e.button !== 0) return;
    pending.current = { type, sx: e.clientX, sy: e.clientY, id: e.pointerId, started: false };
  };
  paletteDrag.current.move = (e) => {
    const p = pending.current;
    if (!p || e.pointerId !== p.id || !area.current || !vp.current) return;
    if (!p.started && Math.hypot(e.clientX - p.sx, e.clientY - p.sy) < 5) return;
    if (!p.started) {
      p.started = true;
      setTip(null);
      // On a phone the sheet steps aside, so the canvas shows where the node will land
      if (narrow) setPaletteOpen(false);
    }
    const a = area.current.getBoundingClientRect();
    const s = a.width / area.current.offsetWidth || 1;
    const v = vp.current.getBoundingClientRect();
    const pal = narrow ? null : root.current?.querySelector(".ui-wf-pal")?.getBoundingClientRect();
    const inside = e.clientX >= v.left && e.clientX <= v.right && e.clientY >= v.top && e.clientY <= v.bottom;
    const onPalette = !!pal && e.clientX >= pal.left && e.clientX <= pal.right && e.clientY >= pal.top && e.clientY <= pal.bottom;
    setGhost({ type: p.type, x: (e.clientX - a.left) / s, y: (e.clientY - a.top) / s, over: inside && !onPalette });
  };
  paletteDrag.current.up = (e) => {
    const p = pending.current;
    if (!p || e.pointerId !== p.id) return;
    pending.current = null;
    if (!p.started) return;
    // The click that follows a drag doesn't add a second node
    skipPick.current = true;
    window.setTimeout(() => (skipPick.current = false));
    setGhost(null);
    if (e.type === "pointercancel" || !ghost?.over) return;
    const w = toWorld(local(e.clientX, e.clientY));
    // Dropped where you want it, joined after the selected node the way a palette click is (a node on its own can't run)
    const after = nodeOf(selected ?? settingsId);
    const port = after && (freePort(after) ?? outPorts(after)[0]);
    add(p.type, { at: { x: w.x - NODE_W / 2, y: w.y - NODE_H / 2 }, from: after && port ? { node: after.id, port: port.id } : undefined });
  };
  useEffect(() => {
    const move = (e: globalThis.PointerEvent) => paletteDrag.current.move(e);
    const up = (e: globalThis.PointerEvent) => paletteDrag.current.up(e);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
    };
  }, []);

  /** A node that gets the focus by Tab and sits outside the view is brought into it */
  const reveal = (n: WfNode) => {
    const v = viewRef.current;
    const sx = n.x * v.k + v.x;
    const sy = n.y * v.k + v.y;
    const left = !narrow && paletteOpen ? 240 : 24;
    const right = size.w - (!narrow && settingsId ? 356 : 24);
    let x = v.x;
    let y = v.y;
    if (sx < left) x += left - sx;
    else if (sx + NODE_W * v.k > right) x -= sx + NODE_W * v.k - right;
    if (sy < 36) y += 36 - sy;
    else if (sy + NODE_H * v.k > size.h - 72) y -= sy + NODE_H * v.k - (size.h - 72);
    if (x !== v.x || y !== v.y) setView({ ...v, x, y });
  };
  // A node picked elsewhere (a warning, a run's step) comes into view. When the whole flow fits beside the open panels it
  // slides as one into the room left, so opening settings doesn't push the far side of the flow under the palette
  useEffect(() => {
    const n = nodeOf(settingsId);
    if (!n || !size.w) return;
    const v = viewRef.current;
    const left = !narrow && paletteOpen ? 240 : 24;
    const right = size.w - (!narrow && settingsId ? 356 : 24);
    const minX = Math.min(...items.map((x) => x.x)) * v.k + v.x;
    const maxX = Math.max(...items.map((x) => x.x + NODE_W)) * v.k + v.x;
    if (!narrow && maxX - minX <= right - left) {
      const dx = minX < left ? left - minX : maxX > right ? right - maxX : 0;
      const sy = n.y * v.k + v.y;
      const dy = sy < 36 ? 36 - sy : sy + NODE_H * v.k > size.h - 72 ? size.h - 72 - (sy + NODE_H * v.k) : 0;
      if (dx || dy) setView({ ...v, x: v.x + dx, y: v.y + dy });
      return;
    }
    reveal(n);
  }, [settingsId]);

  /** The nodes in reading order of the flow: top to bottom, then left to right */
  const order = [...items].sort((a, b) => a.y - b.y || a.x - b.x);
  /** The node that takes Tab – the selected one, else the first in the flow */
  const tabNode = items.some((n) => n.id === selected) ? selected : order[0]?.id;
  const goNode = (id?: string) => {
    if (!id) return;
    setSelected(id);
    setSelectedEdge(null);
    focusNode(id);
  };
  /** Where an arrow goes from a node: ↓ along its first outgoing edge, ↑ back along an incoming one, ← → to the nearest node in its row */
  const neighbour = (n: WfNode, key: string) => {
    const at = order.indexOf(n);
    if (key === "Home") return order[0]?.id;
    if (key === "End") return order[order.length - 1]?.id;
    if (key === "ArrowDown") {
      const out = outPorts(n).flatMap((p) => links.filter((e) => e.from === n.id && e.port === p.id).map((e) => e.to));
      return out[0] ?? order[at + 1]?.id;
    }
    if (key === "ArrowUp") return links.find((e) => e.to === n.id)?.from ?? order[at - 1]?.id;
    const left = key === "ArrowLeft";
    const row = items.filter((m) => m.id !== n.id && Math.abs(m.y - n.y) < NODE_H && (left ? m.x < n.x : m.x > n.x));
    row.sort((a, b) => Math.abs(a.x - n.x) - Math.abs(b.x - n.x));
    return row[0]?.id ?? order[at + (left ? -1 : 1)]?.id;
  };
  const edgesOf = (id: string) => [...links.filter((e) => e.from === id), ...links.filter((e) => e.to === id)];
  const focusEdge = (id: string) => requestAnimationFrame(() => root.current?.querySelector<SVGElement>(`[data-edge="${CSS.escape(id)}"]`)?.focus({ preventScroll: true }));
  // The node an edge was reached from, so ← → cycle through that node's edges and Esc goes back to it
  const edgeHome = useRef<string | null>(null);

  const onNodeKey = (e: KeyboardEvent<HTMLElement>, n: WfNode) => {
    if (e.target !== e.currentTarget) return;
    const k = e.key;
    if (k === "Enter" || k === " ") {
      e.preventDefault();
      openSettings(n.id, true);
    } else if (k === "Delete" || k === "Backspace") {
      e.preventDefault();
      remove(n.id);
    } else if (k.startsWith("Arrow") && e.altKey) {
      // Alt + arrows nudge the node (Shift for bigger steps)
      e.preventDefault();
      const step = e.shiftKey ? 50 : 10;
      const dx = k === "ArrowLeft" ? -step : k === "ArrowRight" ? step : 0;
      const dy = k === "ArrowUp" ? -step : k === "ArrowDown" ? step : 0;
      setSelected(n.id);
      onMove(n.id, n.x + dx, n.y + dy);
    } else if (k.startsWith("Arrow") || k === "Home" || k === "End") {
      e.preventDefault();
      goNode(neighbour(n, k));
    } else if (k === "e" || k === "E") {
      // E: to the node's first edge – from there Delete removes it
      const first = edgesOf(n.id)[0];
      if (!first) return;
      e.preventDefault();
      e.stopPropagation();
      edgeHome.current = n.id;
      focusEdge(first.id);
    } else if (k === "a" || k === "A") {
      // A: the node picker at its first free output
      const port = freePort(n);
      const btn = port && root.current?.querySelector<HTMLElement>(`[data-add="${CSS.escape(n.id + ":" + port.id)}"]`);
      if (!btn || !port) return;
      e.preventDefault();
      e.stopPropagation();
      openPicker(n, port.id, btn);
    }
  };
  const onEdgeKey = (e: KeyboardEvent<SVGPathElement>, edge: WfEdge) => {
    const home = edgeHome.current && items.some((n) => n.id === edgeHome.current) ? edgeHome.current : edge.from;
    const k = e.key;
    if (k === "Delete" || k === "Backspace") {
      disconnect(edge.id);
      goNode(home);
    } else if (k === "Escape" || k === "ArrowUp") goNode(k === "Escape" ? home : edge.from);
    else if (k === "ArrowDown") goNode(edge.to);
    else if (k === "ArrowLeft" || k === "ArrowRight" || k === "e" || k === "E") {
      const list = edgesOf(home);
      const at = list.findIndex((x) => x.id === edge.id);
      const next = list[(at + (k === "ArrowLeft" ? -1 : 1) + list.length) % list.length];
      if (next) focusEdge(next.id);
    } else return;
    e.preventDefault();
    e.stopPropagation();
  };

  /** The palette's tooltip sits in the canvas area, beside the item – outside the palette's scrolling list */
  const showTip = (ty: WfNodeType | null, el?: HTMLElement) => {
    if (!ty || !el || narrow || !area.current) return setTip(null);
    const r = el.getBoundingClientRect();
    const a = area.current.getBoundingClientRect();
    const s = a.width / area.current.offsetWidth || 1;
    setTip({ type: ty.type, text: ty.description, x: (r.right - a.left) / s + 10, y: (r.top - a.top + r.height / 2) / s });
  };

  const openPicker = (n: WfNode, port: string, el: HTMLElement) => {
    if (!area.current) return;
    const r = el.getBoundingClientRect();
    const a = area.current.getBoundingClientRect();
    const s = a.width / area.current.offsetWidth || 1;
    const aw = area.current.offsetWidth;
    const ah = area.current.offsetHeight;
    // Below the +, centred on it – the flow continues downwards; above it when there's no room below
    const x = clamp((r.left + r.width / 2 - a.left) / s - 120, 8, aw - 248);
    const below = (r.bottom - a.top) / s + 8;
    const y = below + 340 > ah - 8 ? clamp((r.top - a.top) / s - 348, 8, Math.max(8, ah - 340)) : below;
    setSelected(n.id);
    setPicker({ from: n.id, port, x, y });
  };

  const wireFrom = wire && nodeOf(wire.from);
  const wirePort = wireFrom && outPorts(wireFrom).find((p) => p.id === wire.port);
  const wireStart = wireFrom && wirePort ? outPoint(wireFrom, wirePort) : null;
  const wireTo = wire?.to ? nodeOf(wire.to) : undefined;
  const settingsNode = nodeOf(settingsId);
  const sheet = narrow && (paletteOpen || !!settingsNode);
  const failedSteps = steps.slice(0, at).filter((s) => s.state === "failed");

  return (
    <div
      ref={root}
      className={["ui-wf", className].filter(Boolean).join(" ")}
      data-narrow={narrow || undefined}
      data-resizing={resizing || undefined}
      style={{ ...style, "--run-h": `${runH}px`, "--run-space": `${runOpen ? runH : 0}px` } as CSSProperties}
      onKeyDown={shortcuts}
    >
      <style href="ui-screen-workflow-builder" precedence="default">
        {css}
      </style>
      <div ref={area} className="ui-wf__area" data-settings={(settingsNode && !narrow) || undefined} data-palette={(paletteOpen && !narrow) || undefined}>
        {/* In the Tab order the palette comes before the canvas, though it floats over it */}
        {paletteOpen && (
          <NodePalette
            groups={palette}
            sheet={narrow}
            tip={tip?.type}
            onPick={addNext}
            onDragStart={startPaletteDrag}
            onTip={showTip}
            onClose={() => {
              setPaletteOpen(false);
              setTip(null);
            }}
          />
        )}
        {/* The canvas: a dot grid that pans and zooms with the nodes, its dots always the same size */}
        <div
          ref={vp}
          className="ui-wf-vp"
          tabIndex={-1}
          role="group"
          aria-label={t("auto:builder.canvas")}
          aria-roledescription={t("auto:builder.canvasRole")}
          data-tool={space ? "hand" : "select"}
          data-panning={panning || undefined}
          data-wiring={wire ? "" : undefined}
          data-follow={follow || undefined}
          style={{ backgroundSize: `${gridStep(view.k)}px ${gridStep(view.k)}px`, backgroundPosition: `${view.x}px ${view.y}px` } as CSSProperties}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          // Focus on a node far off makes the browser scroll this box – the view pans instead, so the box stays put
          onScroll={(e) => {
            e.currentTarget.scrollLeft = 0;
            e.currentTarget.scrollTop = 0;
          }}
        >
          <div className="ui-wf-world" style={{ transform: `translate(${view.x}px, ${view.y}px) scale(${view.k})` }}>
            <EdgeLayer
              nodes={items}
              edges={links}
              lit={litEdges}
              selected={selectedEdge}
              travel={travel}
              dim={passed}
              onEdgeKey={onEdgeKey}
              onEdgeFocus={(edge) => {
                setSelectedEdge(edge.id);
                setSelected(null);
              }}
              draft={wire && wireStart ? { x1: wireStart.x, y1: wireStart.y, ...(wireTo ? { x2: inPoint(wireTo).x, y2: inPoint(wireTo).y } : { x2: wire.x, y2: wire.y }), snapped: !!wireTo } : null}
            />
            <EdgeMarks nodes={items} edges={links} />
            {items.map((n) => {
              const s = doneNodes.get(n.id);
              return (
                <NodeCard
                  key={n.id}
                  node={n}
                  selected={selected === n.id}
                  run={running === n.id ? "running" : s ? (s.state === "failed" ? "failed" : s.state === "waiting" ? "waiting" : "done") : undefined}
                  took={s?.ms}
                  skipped={!!passed && !passed.has(n.id)}
                  arrival={running === n.id ? senderKind : undefined}
                  freePorts={outPorts(n).filter((p) => !links.some((e) => e.from === n.id && e.port === p.id)).map((p) => p.id)}
                  pickerOpen={picker?.from === n.id ? picker.port : undefined}
                  wire={wire ? (wire.to === n.id ? "snap" : canWire(wire.from, wire.port, n) ? "valid" : undefined) : undefined}
                  tabStop={tabNode === n.id}
                  onAdd={(port, el) => openPicker(n, port, el)}
                  onKeyDown={(e) => onNodeKey(e, n)}
                  onFocus={(visible) => {
                    setSelected(n.id);
                    setSelectedEdge(null);
                    if (visible) reveal(n);
                  }}
                />
              );
            })}
          </div>
        </div>

        {tip && !narrow && paletteOpen && (
          <span id="ui-wf-tip" role="tooltip" className="ui-wf-tip" style={{ left: tip.x, top: tip.y }}>
            {tip.text}
          </span>
        )}
        {picker && (
          <NodePicker
            groups={palette}
            x={narrow ? 8 : picker.x}
            y={narrow ? 8 : picker.y}
            onPick={(type) => {
              const from = picker;
              setPicker(null);
              add(type, { from: { node: from.from, port: from.port } });
            }}
            onClose={() => setPicker(null)}
          />
        )}

        <CanvasToolbar
          zoom={view.k}
          paletteOpen={paletteOpen}
          running={busy}
          // Not disabled while the dry run is pending: a focused button that turns disabled loses focus, and the
          // browser scrolls the page when React gives it back
          disabled={runDisabled}
          runOpen={runOpen}
          onPalette={() => {
            setPaletteOpen((o) => !o);
            setTip(null);
          }}
          onFit={() => fit()}
          onZoom={zoomBy}
          onZoomReset={() => zoomAt({ x: size.w / 2, y: size.h / 2 }, 1 / view.k)}
          onRun={onRun}
          onRuns={() => (runOpen ? closeRun() : setRunOpen(true))}
        />

        {/* The palette item being dragged, drawn as the node card it will become, at the canvas's zoom */}
        {ghost && (
          <div className="ui-wf-ghost" data-over={ghost.over || undefined} style={{ left: ghost.x, top: ghost.y, transform: `scale(${view.k})` }} inert aria-hidden="true">
            {(() => {
              const ty = allTypes.find((x) => x.type === ghost.type);
              return ty ? (
                <NodeCard preview node={{ id: "ghost", kind: ty.kind, tag: ty.label, title: ty.label, icon: ty.icon, x: -NODE_W / 2, y: -NODE_H / 2, input: true, ports: [], detail: { text: ty.description }, chip: { text: t("auto:builder.new") }, removable: false }} freePorts={[]} />
              ) : null;
            })()}
          </div>
        )}
      </div>

      <RunPanel
        open={runOpen}
        height={runH}
        maxHeight={Math.max(180, size.root - 200)}
        tab={tab}
        runs={runs}
        shown={shown}
        at={at}
        busy={busy}
        failed={failedSteps}
        announce={announce}
        onTab={setTab}
        onResize={setRunH}
        onResizing={setResizing}
        onClose={closeRun}
        onRun={onRun}
        onOpenRun={onOpenRun}
        onPick={(id) => openSettings(id)}
      />
      {/* The settings come last in the Tab order (focus moves into them when they open); they sit over the canvas, above the run panel */}
      {sheet && <div className="ui-wf-scrim" aria-hidden="true" onClick={() => (settingsNode ? onSettings(null) : setPaletteOpen(false))} />}
      {settingsNode && (
        <aside ref={panelRef} className="ui-wf-set" data-sheet={narrow || undefined} tabIndex={-1} aria-labelledby="ui-wf-set-title" role={narrow ? "dialog" : "region"} aria-modal={narrow || undefined} onKeyDown={narrow ? trapTab : undefined}>
          <header className="ui-wf-set__head">
            <span className="ui-wf-node__tile" data-kind={settingsNode.kind} aria-hidden="true">
              <Icon icon={settingsNode.icon} size={15} />
            </span>
            <span className="ui-wf-set__title">
              <b id="ui-wf-set-title" title={settingsNode.title} dir="auto">
                {settingsNode.title}
              </b>
              <small>{settingsNode.tag}</small>
            </span>
            {settingsNode.removable && (
              <button type="button" className="ui-wf-icon-btn" aria-label={t("auto:builder.removeNode", { title: settingsNode.title })} title={t("auto:node.remove")} onClick={() => remove(settingsNode.id)} data-del-node>
                <Icon icon={Trash2Icon} size={15} />
              </button>
            )}
            <button type="button" className="ui-wf-icon-btn" aria-label={t("auto:builder.closeSettings")} onClick={closeSettings}>
              <Icon icon={XIcon} size={15} />
            </button>
          </header>
          <div className="ui-wf-set__body">{renderSettings(settingsNode.id)}</div>
        </aside>
      )}
    </div>
  );
}


/* ---------- Node palette ---------- */

/** The node types by group – each item an icon and a name; drag it out, click it or press Enter to add it */
function PaletteList({
  groups,
  tip,
  onPick,
  onDragStart,
  onTip,
  filter = "",
  skipTriggers,
  roving,
}: {
  groups: WfPaletteGroup[];
  /** One Tab stop for the whole list – ↑ ↓ Home End move between the items */
  roving?: boolean;
  /** Pointer down on an item – the screen turns a drag into a node under the pointer */
  onDragStart?: (type: string, e: PointerEvent<HTMLElement>) => void;
  /** The type whose tooltip shows, for aria-describedby */
  tip?: string;
  onPick: (type: string) => void;
  onTip?: (t: WfNodeType | null, el?: HTMLElement) => void;
  filter?: string;
  /** Leaves the triggers out – after an output, a trigger can't follow */
  skipTriggers?: boolean;
}) {
  const q = filter.trim().toLowerCase();
  const shown = groups
    .filter((g) => !(skipTriggers && g.id === "triggers"))
    .map((g) => ({ ...g, items: g.items.filter((t) => !q || t.label.toLowerCase().includes(q) || t.description.toLowerCase().includes(q)) }))
    .filter((g) => g.items.length);
  const [active, setActive] = useState<string | null>(null);
  const flat = shown.flatMap((g) => g.items.map((t) => t.type));
  const stop = active && flat.includes(active) ? active : flat[0];
  if (!shown.length) return <p className="ui-wf-pal__none">{t("auto:builder.noMatch", { q: filter.trim() })}</p>;
  return (
    <div
      className="ui-wf-pal__groups"
      onKeyDown={(e) => {
        if (!roving || !["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
        const list = [...e.currentTarget.querySelectorAll<HTMLElement>(".ui-wf-pal__item")];
        const at = list.indexOf(document.activeElement as HTMLElement);
        const next = e.key === "Home" ? 0 : e.key === "End" ? list.length - 1 : clamp(at + (e.key === "ArrowDown" ? 1 : -1), 0, list.length - 1);
        e.preventDefault();
        list[next]?.focus();
      }}
    >
      {shown.map((g) => (
        <section key={g.id} className="ui-wf-pal__group" aria-label={g.label}>
          <h3 className="ui-wf-pal__head">
            {g.label}
            <span>{g.items.length}</span>
          </h3>
          {g.items.map((t) => (
            <button
              key={t.type}
              type="button"
              className="ui-wf-pal__item"
              data-drag={onDragStart ? "" : undefined}
              data-palette-item={t.type}
              tabIndex={roving ? (stop === t.type ? 0 : -1) : undefined}
              aria-describedby={tip === t.type ? "ui-wf-tip" : undefined}
              onClick={() => onPick(t.type)}
              onPointerEnter={(e) => onTip?.(t, e.currentTarget)}
              onPointerLeave={() => onTip?.(null)}
              onFocus={(e) => {
                setActive(t.type);
                if (e.currentTarget.matches(":focus-visible")) onTip?.(t, e.currentTarget);
              }}
              onBlur={() => onTip?.(null)}
              onPointerDown={(e) => onDragStart?.(t.type, e)}
            >
              <span className="ui-wf-pal__icon">
                <Icon icon={t.icon} size={15} />
              </span>
              <span className="ui-wf-pal__text">
                <span className="ui-wf-pal__label">{t.label}</span>
                <span className="ui-wf-pal__desc">{t.description}</span>
              </span>
            </button>
          ))}
        </section>
      ))}
    </div>
  );
}

/** The palette on the canvas's left – a floating card, or a sheet from the bottom on phones */
function NodePalette({
  groups,
  sheet,
  tip,
  onPick,
  onDragStart,
  onTip,
  onClose,
}: {
  groups: WfPaletteGroup[];
  sheet: boolean;
  tip?: string;
  onPick: (type: string) => void;
  onDragStart: (type: string, e: PointerEvent<HTMLElement>) => void;
  onTip: (t: WfNodeType | null, el?: HTMLElement) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLElement>(null);
  // As a sheet it takes the focus, like any window
  useEffect(() => {
    if (sheet) ref.current?.querySelector<HTMLElement>(".ui-wf-pal__item")?.focus({ preventScroll: true });
  }, [sheet]);
  return (
    <aside ref={ref} className="ui-wf-pal" aria-label={t("auto:builder.addNodes")} data-palette role={sheet ? "dialog" : undefined} aria-modal={sheet || undefined} data-sheet={sheet || undefined} onKeyDown={sheet ? trapTab : undefined}>
      <header className="ui-wf-pal__top">
        <span>{sheet ? t("auto:builder.addNode") : t("auto:builder.nodes")}</span>
        <button type="button" className="ui-wf-icon-btn" aria-label={t("auto:builder.closeNodes")} onClick={onClose}>
          <Icon icon={XIcon} size={14} />
        </button>
      </header>
      <div className="ui-wf-pal__scroll" onScroll={() => onTip(null)}>
        <PaletteList groups={groups} tip={tip} onPick={onPick} onDragStart={onDragStart} onTip={onTip} roving />
      </div>
    </aside>
  );
}

/** The same list in a small card at a free output's + – with a search field on top */
function NodePicker({ groups, x, y, onPick, onClose }: { groups: WfPaletteGroup[]; x: number; y: number; onPick: (type: string) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const down = (e: globalThis.PointerEvent) => {
      const t = e.target as HTMLElement;
      if (!ref.current?.contains(t) && !t.closest("[data-add]")) onClose();
    };
    document.addEventListener("pointerdown", down);
    return () => document.removeEventListener("pointerdown", down);
  }, [onClose]);
  const first = groups.filter((g) => g.id !== "triggers").flatMap((g) => g.items).find((t) => !q.trim() || t.label.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div
      ref={ref}
      className="ui-wf-picker"
      role="dialog"
      aria-label={t("auto:builder.addHere")}
      style={{ left: x, top: y }}
      // Tabbing out of the picker closes it
      onBlur={(e) => {
        const to = e.relatedTarget as Node | null;
        if (to && !e.currentTarget.contains(to)) onClose();
      }}
    >
      <label className="ui-wf-picker__search">
        <Icon icon={SearchIcon} size={14} />
        <input
          autoFocus
          value={q}
          placeholder={t("auto:node.search")}
          aria-label={t("auto:builder.searchNodes")}
          data-palette-search
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && first) {
              e.preventDefault();
              onPick(first.type);
            }
            if (e.key === "ArrowDown") {
              e.preventDefault();
              ref.current?.querySelector<HTMLElement>(".ui-wf-pal__item")?.focus();
            }
          }}
        />
      </label>
      <div
        className="ui-wf-picker__list"
        onKeyDown={(e) => {
          // Up and down walk the list; up from the first item goes back to the search field
          if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
          const list = [...(ref.current?.querySelectorAll<HTMLElement>(".ui-wf-pal__item") ?? [])];
          const at = list.indexOf(document.activeElement as HTMLElement);
          e.preventDefault();
          if (e.key === "ArrowUp" && at <= 0) ref.current?.querySelector("input")?.focus();
          else list[clamp(at + (e.key === "ArrowDown" ? 1 : -1), 0, list.length - 1)]?.focus();
        }}
      >
        <PaletteList groups={groups} filter={q} skipTriggers onPick={onPick} />
      </div>
    </div>
  );
}

/* ---------- Canvas: node card and edges ---------- */

/** The card's outline as one path, starting at the middle of the bottom edge – the trace runs round it once */
const TRACE_D = (() => {
  const w = NODE_W;
  const h = NODE_H;
  const r = 10;
  return `M${w / 2},${h} H${r} A${r},${r} 0 0 1 0,${h - r} V${r} A${r},${r} 0 0 1 ${r},0 H${w - r} A${r},${r} 0 0 1 ${w},${r} V${h - r} A${r},${r} 0 0 1 ${w - r},${h} Z`;
})();

/**
 * A node: a small tinted kind tag above the card; the card itself has an icon tile, the title and a status chip on
 * top, and below it two lines that say what this node does – in words, then in mono (or the model and tools as chips).
 * The flow runs down: an input dot on the top edge, outputs on the bottom (a free one ends in a blue +; drag from a dot
 * to another node's input to join them).
 * In a run: an impulse arrives at the input with a soft glow, a trace runs once round the card, then the chip shows the time
 */
function NodeCard({
  node,
  selected,
  run,
  took,
  skipped,
  arrival,
  freePorts,
  pickerOpen,
  wire,
  preview,
  tabStop,
  onAdd,
  onKeyDown,
  onFocus,
}: {
  node: WfNode;
  /** The canvas is one Tab stop: only this node takes Tab, arrows move between the nodes */
  tabStop?: boolean;
  selected?: boolean;
  run?: "running" | "done" | "failed" | "waiting";
  /** How long its step took */
  took?: number;
  /** The run that was played didn't pass it */
  skipped?: boolean;
  /** The kind of the node an impulse just came from – its colour glows in at the input */
  arrival?: WfKind;
  freePorts: string[];
  /** The port whose picker is open */
  pickerOpen?: string;
  /** While a connection is dragged: this node's input can take it ("valid"), or it is the one it snaps to ("snap") */
  wire?: "valid" | "snap";
  /** Drawn under the pointer while a palette item is dragged – looks the same, does nothing */
  preview?: boolean;
  onAdd?: (port: string, el: HTMLElement) => void;
  onKeyDown?: (e: KeyboardEvent<HTMLElement>) => void;
  /** visible: the focus came from the keyboard */
  onFocus?: (visible: boolean) => void;
}) {
  const title = node.title.trim() || node.tag;
  const d = node.detail;
  const warn = node.chip.tone === "warn";
  // The chip on the right: how it stands now – running, done in a time, or what Steve says about it
  const chip: { text: string; tone?: "warn" | "done" | "info" | "attn"; icon?: ReactNode } =
    run === "running"
      ? { text: t("auto:builder.replay"), tone: "info", icon: <Icon icon={PlayIcon} size={10} stroke={2.4} /> }
      : run === "failed"
        ? { text: t("auto:runStatus.failed"), tone: "warn", icon: <Icon icon={XIcon} size={11} stroke={2.4} /> }
        : run === "waiting"
          ? { text: t("auto:runStatus.waiting"), tone: "info", icon: <Icon icon={HourglassIcon} size={11} stroke={2.2} /> }
          : run === "done"
            ? { text: ms(took ?? 0), tone: "done", icon: <Icon icon={CheckIcon} size={11} stroke={2.4} /> }
            : node.chip;
  return (
    <div
      {...node.attrs}
      className="ui-wf-node"
      data-node={preview ? undefined : node.id}
      data-kind={node.kind}
      data-selected={selected || undefined}
      data-run={run}
      data-skipped={skipped || undefined}
      data-wire={wire}
      data-arrived={arrival}
      style={{ left: node.x, top: node.y }}
      tabIndex={!preview && tabStop ? 0 : -1}
      role={preview ? undefined : "button"}
      aria-hidden={preview || undefined}
      aria-pressed={preview ? undefined : !!selected}
      aria-label={preview ? undefined : t("auto:builder.nodeAria", { title, kind: node.tag, state: chip.text })}
      aria-keyshortcuts={preview ? undefined : "Enter Delete A E Alt+ArrowUp Alt+ArrowDown Alt+ArrowLeft Alt+ArrowRight"}
      onKeyDown={onKeyDown}
      onFocus={(e) => e.target === e.currentTarget && onFocus?.(e.currentTarget.matches(":focus-visible"))}
    >
      <span className="ui-wf-node__kind" aria-hidden="true">
        {node.tag}
      </span>
      {/* Where the impulse came in: a soft glow inside the card at the input, in the sender's colour */}
      {arrival && <span className="ui-wf-arrive" data-from={arrival} aria-hidden="true" />}
      {run === "running" && (
        <svg className="ui-wf-trace" viewBox={`0 0 ${NODE_W} ${NODE_H}`} aria-hidden="true">
          <g className="ui-wf-trace__glow">
            {[1, 2, 3].map((i) => (
              <path key={i} className={`ui-wf-trace__seg ui-wf-trace__seg--${i}`} d={TRACE_D} pathLength={1} />
            ))}
          </g>
          {[1, 2, 3].map((i) => (
            <path key={i} className={`ui-wf-trace__line ui-wf-trace__seg ui-wf-trace__seg--${i}`} d={TRACE_D} pathLength={1} />
          ))}
        </svg>
      )}
      <span className="ui-wf-node__head">
        <span className="ui-wf-node__tile" data-tone={warn ? "warn" : undefined} aria-hidden="true">
          <Icon icon={warn ? AlertTriangleIcon : node.icon} size={14} />
        </span>
        <b title={title} dir="auto">
          {title}
        </b>
        {chip.text && (
          <span className="ui-wf-node__chip" data-tone={chip.tone}>
            {chip.icon}
            {chip.text}
          </span>
        )}
      </span>
      <span className="ui-wf-node__line" data-warn={d.warn || undefined} title={d.text} dir="auto">
        {d.text}
      </span>
      {/* The specifics in a gray well at the bottom: mono text, or chips */}
      <span className="ui-wf-node__well">
        {d.chips?.length ? (
          <span className="ui-wf-node__chips" title={d.chips.length > 3 ? `${d.chips[0]} · ${d.chips.slice(1).join(", ")}` : undefined}>
            {d.chips.slice(0, d.chips.length > 3 ? 2 : 3).map((c, i) => (
              <span key={`${i}-${c}`} className="ui-wf-node__tag" title={c} dir="auto">
                <span>{c}</span>
              </span>
            ))}
            {d.chips.length > 3 && <span className="ui-wf-node__more">+{d.chips.length - 2}</span>}
          </span>
        ) : (
          <code className="ui-wf-node__mono" title={d.mono}>
            {d.mono || "–"}
          </code>
        )}
      </span>
      {hasInput(node) && <span className="ui-wf-port ui-wf-port--in" aria-hidden="true" />}
      {outPorts(node).map((p) => (
        <span key={p.id} className="ui-wf-out" style={{ left: p.dx }}>
          <span className="ui-wf-port ui-wf-port--out" data-out={preview ? undefined : `${node.id}:${p.id}`} title={preview ? undefined : t("auto:builder.dragToConnect")} aria-hidden="true" />
          {/* A free port names itself; once joined, the tag sits on its curve */}
          {p.label && freePorts.includes(p.id) && <span className="ui-wf-out__label">{p.label}</span>}
          {freePorts.includes(p.id) && (
            <button
              type="button"
              className="ui-wf-add"
              data-add={`${node.id}:${p.id}`}
              data-tip={t("auto:builder.addNode")}
              // Reached with A on the node, not with Tab – the canvas stays one stop
              tabIndex={-1}
              aria-label={t(p.label ? "auto:builder.addAfterBranch" : "auto:builder.addAfter", { title, label: p.label })}
              aria-expanded={pickerOpen === p.id}
              aria-haspopup="dialog"
              onClick={(e) => onAdd?.(p.id, e.currentTarget)}
            >
              <Icon icon={PlusIcon} size={10} stroke={2.4} />
            </button>
          )}
        </span>
      ))}
    </div>
  );
}

/** Where an edge runs: its curve, from the output it leaves to the input it enters */
const edgeCurve = (nodes: WfNode[], e: WfEdge) => {
  const a = nodes.find((n) => n.id === e.from);
  const b = nodes.find((n) => n.id === e.to);
  if (!a || !b) return null;
  const p = outPorts(a).find((x) => x.id === e.port) ?? outPorts(a)[0];
  if (!p) return null;
  const o = outPoint(a, p);
  const i = inPoint(b);
  return { o, i, d: curve(o.x, o.y, i.x, i.y), label: e.label ?? p.label, from: a };
};

/**
 * The curves between the nodes – one SVG under the cards. Each curve has a wide invisible twin that takes the click;
 * the selected one is accent, a run draws an accent line over each curve it has taken, an impulse's trail runs along
 * the one it travels, and a connection being dragged follows the pointer
 */
function EdgeLayer({
  nodes,
  edges,
  lit,
  selected,
  travel,
  draft,
  dim,
  onEdgeKey,
  onEdgeFocus,
}: {
  nodes: WfNode[];
  edges: WfEdge[];
  lit: Set<string>;
  selected: string | null;
  /** After a run has played: the nodes it passed – curves it didn't take step back */
  dim?: Set<string> | null;
  /** Keys on a focused edge – Delete, arrows back to its nodes */
  onEdgeKey?: (e: KeyboardEvent<SVGPathElement>, edge: WfEdge) => void;
  onEdgeFocus?: (edge: WfEdge) => void;
  /** The edge an impulse is travelling along, coloured by the node it left */
  travel?: { edge: string; kind: WfKind };
  draft?: { x1: number; y1: number; x2: number; y2: number; snapped: boolean } | null;
}) {
  return (
    <svg className="ui-wf-edges">
      {edges.map((e) => {
        const c = edgeCurve(nodes, e);
        if (!c) return null;
        const to = nodes.find((n) => n.id === e.to);
        const name = (n?: WfNode) => (n ? n.title.trim() || n.tag : "–");
        return (
          <g key={e.id}>
            {/* The wide twin takes the click and, reached with E from a node, the focus */}
            <path
              className="ui-wf-edge-hit"
              d={c.d}
              data-edge={e.id}
              tabIndex={-1}
              role="button"
              aria-label={t(c.label ? "auto:builder.edgeAriaBranch" : "auto:builder.edgeAria", { from: name(c.from), to: name(to), label: c.label })}
              aria-keyshortcuts="Delete ArrowUp ArrowDown ArrowLeft ArrowRight"
              onKeyDown={(ev) => onEdgeKey?.(ev, e)}
              onFocus={() => onEdgeFocus?.(e)}
            />
            <path className="ui-wf-edge" d={c.d} data-selected={selected === e.id || undefined} data-dim={(dim && !lit.has(e.id)) || undefined} aria-hidden="true" />
            {lit.has(e.id) && <path className="ui-wf-edge ui-wf-edge--lit" d={c.d} pathLength={1} aria-hidden="true" />}
            {travel?.edge === e.id && <FlowTrail key={`${e.id}-trail`} d={c.d} kind={travel.kind} />}
          </g>
        );
      })}
      {draft && <path aria-hidden="true" className="ui-wf-edge ui-wf-edge--draft" data-snapped={draft.snapped || undefined} d={curve(draft.x1, draft.y1, draft.x2, draft.y2)} />}
    </svg>
  );
}

/** HTML over the curves: the Yes / No tags on a branch's edges, on the line a little before its middle */
function EdgeMarks({ nodes, edges }: { nodes: WfNode[]; edges: WfEdge[] }) {
  return (
    <>
      {edges.map((e) => {
        const c = edgeCurve(nodes, e);
        if (!c?.label) return null;
        const at = curvePoint(c.o.x, c.o.y, c.i.x, c.i.y, 0.42);
        return (
          <span key={e.id} className="ui-wf-edge-tag" style={{ left: at.x, top: at.y }} aria-hidden="true">
            {c.label}
          </span>
        );
      })}
    </>
  );
}

/**
 * The impulse along an edge, as in the snippet's play: a stretch of the curve lights up and runs from the node it
 * leaves to the next – a gradient from clear at its tail to the sender's colour at its head, half the curve long
 */
function FlowTrail({ d, kind }: { d: string; kind: WfKind }) {
  const path = useRef<SVGPathElement>(null);
  const grad = useRef<SVGLinearGradientElement>(null);
  const id = useId();
  useEffect(() => {
    const el = path.current;
    const g = grad.current;
    if (!el || !g) return;
    const total = el.getTotalLength();
    if (!total) return;
    const trail = total * 0.5;
    const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
    const start = performance.now();
    let frame = 0;
    const tick = (now: number) => {
      const raw = Math.min((now - start) / TRAVEL_MS, 1);
      const head = ease(raw) * total;
      const tail = Math.max(0, head - trail);
      // Hidden up to the tail, drawn from tail to head, hidden after
      el.setAttribute("stroke-dasharray", `${tail} 0 ${head - tail} ${total}`);
      const a = el.getPointAtLength(tail);
      const b = el.getPointAtLength(head);
      g.setAttribute("x1", `${a.x}`);
      g.setAttribute("y1", `${a.y}`);
      g.setAttribute("x2", `${b.x}`);
      g.setAttribute("y2", `${b.y}`);
      if (raw < 1) frame = requestAnimationFrame(tick);
    };
    el.setAttribute("stroke-dasharray", `0 ${total}`);
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [d]);
  return (
    <g className="ui-wf-flow" data-kind={kind}>
      <defs>
        <linearGradient ref={grad} id={id} gradientUnits="userSpaceOnUse">
          <stop offset="0%" className="ui-wf-flow__stop" stopOpacity={0} />
          <stop offset="100%" className="ui-wf-flow__stop" stopOpacity={1} />
        </linearGradient>
      </defs>
      <path ref={path} className="ui-wf-flow__line" d={d} stroke={`url(#${id})`} />
    </g>
  );
}

/* ---------- Canvas toolbar ---------- */

/** Floating at the bottom: add node, fit, zoom, the run panel and Run */
function CanvasToolbar({
  zoom,
  paletteOpen,
  running,
  disabled,
  runOpen,
  onPalette,
  onFit,
  onZoom,
  onZoomReset,
  onRun,
  onRuns,
}: {
  zoom: number;
  paletteOpen: boolean;
  running: boolean;
  /** Run is off while a run is being made or the flow saved */
  disabled?: boolean;
  runOpen: boolean;
  onPalette: () => void;
  onFit: () => void;
  onZoom: (factor: number) => void;
  onZoomReset: () => void;
  onRun: () => void;
  onRuns: () => void;
}) {
  const btn = (label: string, icon: LucideIcon, onClick: () => void, extra: { pressed?: boolean; key?: string; className?: string } = {}) => (
    <button
      type="button"
      className={["ui-wf-tb__btn", extra.className].filter(Boolean).join(" ")}
      aria-label={label}
      aria-pressed={extra.pressed}
      aria-keyshortcuts={extra.key}
      data-tip={extra.key ? `${label}  ${extra.key}` : label}
      onClick={onClick}
    >
      <Icon icon={icon} size={16} />
    </button>
  );
  return (
    <div className="ui-wf-tb" role="toolbar" aria-label={t("auto:builder.toolbar")}>
      {btn(t("auto:builder.addNode"), PlusIcon, onPalette, { pressed: paletteOpen, className: "ui-wf-tb__add" })}
      <span className="ui-wf-tb__sep" aria-hidden="true" />
      {btn(t("auto:builder.fit"), MaximizeIcon, onFit, { key: "⇧1" })}
      {btn(t("auto:builder.zoomOut"), MinusIcon, () => onZoom(1 / 1.2), { key: "-", className: "ui-wf-tb__zoomstep" })}
      <button type="button" className="ui-wf-tb__zoom" aria-label={t("auto:builder.zoomReset", { n: Math.round(zoom * 100) })} data-tip={t("auto:builder.zoomResetTip")} onClick={onZoomReset}>
        {Math.round(zoom * 100)}%
      </button>
      {btn(t("auto:builder.zoomIn"), PlusIcon, () => onZoom(1.2), { key: "+", className: "ui-wf-tb__zoomstep" })}
      <span className="ui-wf-tb__sep" aria-hidden="true" />
      {btn(t("auto:builder.runs"), HistoryIcon, onRuns, { pressed: runOpen, className: "ui-wf-tb__runs" })}
      <button type="button" className="ui-wf-tb__btn ui-wf-tb__run" aria-label={running ? t("auto:builder.running") : t("auto:rule.dryRun")} data-tip={running ? t("auto:builder.running") : t("auto:rule.dryRun")} disabled={disabled} aria-disabled={running || undefined} onClick={running ? undefined : onRun} data-dry>
        {running ? <Spin /> : <Icon icon={PlayIcon} size={14} stroke={2.2} />}
      </button>
    </div>
  );
}

/* ---------- Run panel ---------- */

const tabIcons: Record<RunTab, LucideIcon> = { runs: PlayIcon, timeline: ClockIcon, payload: CodeIcon, errors: CircleAlertIcon };
const runTabs: RunTab[] = ["runs", "timeline", "payload", "errors"];
const stateMark = (state: WfRun["state"] | WfStep["state"]) => (state === "done" ? <Done /> : state === "failed" ? <Failed /> : state === "waiting" ? <Waiting /> : state === "running" ? <Spin /> : <span className="ui-wf-idle" aria-hidden="true" />);

/** Slides up under the canvas: tabs, the played run's steps as they complete, the recent runs, and Run again – drag its top edge to resize */
function RunPanel({
  open,
  height,
  maxHeight,
  tab,
  runs,
  shown,
  at,
  busy,
  failed,
  announce,
  onTab,
  onResize,
  onResizing,
  onClose,
  onRun,
  onOpenRun,
  onPick,
}: {
  open: boolean;
  height: number;
  maxHeight: number;
  tab: RunTab;
  runs: WfRun[];
  /** The run playing (or played) on the canvas, and how far it is */
  shown?: WfRun;
  at: number;
  busy: boolean;
  failed: WfStep[];
  /** One polite line for screen readers per finished step */
  announce: string;
  onTab: (tab: RunTab) => void;
  onResize: (height: number) => void;
  onResizing: (on: boolean) => void;
  onClose: () => void;
  onRun: () => void;
  onOpenRun: (id: string) => void;
  onPick: (id: string) => void;
}) {
  const drag = useRef<{ y: number; h: number; s: number } | null>(null);
  const min = 160;
  const max = Math.max(min, maxHeight);
  const steps = shown?.steps ?? [];
  const errorCount = failed.length + (shown?.error && !failed.length ? 1 : 0);
  const bodyRef = useRef<HTMLDivElement>(null);
  // The run being played stays in view as its steps come in: its row at the top of the list (newer runs above it scroll
  // away), or its latest step at the bottom once it is taller than the panel. Only the list scrolls, never the page
  useEffect(() => {
    const body = bodyRef.current;
    const row = body?.querySelector<HTMLElement>(".ui-wf-runs__run[data-shown]");
    if (!body || !row || !open || tab !== "runs") return;
    const r = row.getBoundingClientRect();
    const b = body.getBoundingClientRect();
    const d = r.height + 8 > b.height ? r.bottom - b.bottom + 4 : r.top - b.top - 4;
    if (Math.abs(d) > 1) body.scrollBy({ top: d, behavior: reducedMotion() ? "auto" : "smooth" });
  }, [shown?.id, at, open, tab, height]);

  const stepRow = (s: WfStep, i: number) => {
    const state = i < at ? s.state : "running";
    return (
      <li key={`${i}-${s.node}`} className="ui-wf-step" data-state={state} {...s.attrs}>
        <button type="button" className="ui-wf-step__row" aria-label={t("auto:builder.stepAria", { title: s.title, n: i + 1, count: steps.length, state: state === "running" ? t("auto:builder.replaying") : state === "failed" ? t("auto:runStatus.failed") : state === "waiting" ? t("auto:runStatus.waiting") : ms(s.ms) })} onClick={() => onPick(s.node)}>
          <span className="ui-wf-step__mark">{state === "running" ? <Replay /> : stateMark(state)}</span>
          <span className="ui-wf-step__name" title={s.title} dir="auto">
            {s.title}
          </span>
          <span className="ui-wf-count">
            {i + 1}/{steps.length}
          </span>
          <span className="ui-wf-step__note" title={s.note} dir="auto">
            {state === "running" ? "" : s.note}
          </span>
          <span className="ui-wf-step__time">{state === "running" ? t("auto:builder.replaying") : ms(s.ms)}</span>
        </button>
        {state === "failed" && s.error && (
          <p className="ui-wf-step__error" title={s.error} dir="auto">
            {s.error}
          </p>
        )}
      </li>
    );
  };

  const body = () => {
    if (tab === "payload")
      return shown?.payload !== undefined ? (
        <div className="ui-wf-run__pad">
          <span className="ui-wf-label">{t("auto:rule.eventUsed")}</span>
          <pre className="ui-wf-code" tabIndex={0}>{typeof shown.payload === "string" ? shown.payload : JSON.stringify(shown.payload, null, 2)}</pre>
        </div>
      ) : (
        <EmptyState title={t("auto:builder.noPayload")} text={t("auto:builder.noPayloadSub")} />
      );
    if (tab === "errors")
      return errorCount ? (
        <ul className="ui-wf-list">
          {failed.map((s) => (
            <li key={`run-${s.node}`}>
              <button type="button" className="ui-wf-list__row ui-wf-list__row--error" onClick={() => onPick(s.node)}>
                <Icon icon={AlertTriangleIcon} size={15} />
                <span className="ui-wf-step__name" title={s.title} dir="auto">
                  {s.title}
                </span>
                <span className="ui-wf-list__meta" title={s.error} dir="auto">
                  {s.error}
                </span>
                <span className="ui-wf-tag" data-tone="warn">
                  {t("auto:runStatus.failed")}
                </span>
              </button>
            </li>
          ))}
          {!failed.length && shown?.error && (
            <li>
              <p className="ui-wf-step__error ui-wf-run__error" dir="auto">
                {shown.error}
              </p>
            </li>
          )}
        </ul>
      ) : (
        <EmptyState title={t("auto:builder.noErrors")} text={shown ? t("auto:builder.noErrorsSub") : t("auto:builder.noRunSub")} />
      );
    if (tab === "timeline") {
      if (!steps.length) return <EmptyState title={t("auto:builder.noTimeline")} text={t("auto:builder.noRunSub")} />;
      const total = steps.reduce((n, s) => n + s.ms, 0) || 1;
      let offset = 0;
      return (
        <ul className="ui-wf-timeline">
          {steps.map((s, i) => {
            const left = (offset / total) * 100;
            offset += s.ms;
            return (
              <li key={`${i}-${s.node}`} data-state={i < at ? "done" : i === at ? "running" : "waiting"}>
                <span className="ui-wf-step__name" title={s.title}>
                  {s.title}
                </span>
                <span className="ui-wf-timeline__track">
                  <i style={{ left: `${left}%`, width: `max(3px, ${(s.ms / total) * 100}%)` }} />
                </span>
                <span className="ui-wf-step__time">{i < at ? ms(s.ms) : ""}</span>
              </li>
            );
          })}
        </ul>
      );
    }
    if (!runs.length) return <EmptyState title={t("auto:noRuns")} text={t("auto:builder.noRunSub")} />;
    return (
      <div className="ui-wf-runs">
        {runs.map((r) => {
          const isShown = shown?.id === r.id;
          const replaying = isShown && at < steps.length;
          const total = isShown ? steps.reduce((n, s) => n + s.ms, 0) : 0;
          return (
            <section key={r.id} className="ui-wf-runs__run" {...r.attrs} data-shown={isShown || undefined} data-replaying={replaying || undefined}>
              <div className="ui-wf-runs__line">
              <button type="button" className="ui-wf-runs__head" aria-expanded={isShown} onClick={() => onOpenRun(r.id)}>
                <span className="ui-wf-step__mark">{stateMark(r.state)}</span>
                <Icon icon={ChevronRightIcon} size={12} />
                <b dir="auto">{r.title}</b>
                <span className="ui-wf-list__meta">{r.when}</span>
                <span className="ui-wf-step__time">{replaying ? t("auto:builder.replay") : isShown && steps.length ? ms(total) : ""}</span>
              </button>
              {r.extra}
              </div>
              {isShown && steps.length > 0 && <ol className="ui-wf-steps">{steps.slice(0, Math.min(at + 1, steps.length)).map((s, i) => stepRow(s, i))}</ol>}
              {isShown && r.error && at >= steps.length && !steps.some((s) => s.error) && <p className="ui-wf-step__error ui-wf-run__error" dir="auto">{r.error}</p>}
            </section>
          );
        })}
      </div>
    );
  };

  return (
    <section className="ui-wf-run" data-open={open || undefined} aria-label={t("auto:builder.runPanel")} inert={!open} aria-hidden={!open || undefined} data-dry-result={shown ? "" : undefined}>
      <div
        className="ui-wf-run__grip"
        role="separator"
        aria-orientation="horizontal"
        aria-label={t("auto:builder.resizeRuns")}
        aria-valuenow={Math.round(height)}
        aria-valuemin={min}
        aria-valuemax={Math.round(max)}
        tabIndex={0}
        onPointerDown={(e) => {
          const el = e.currentTarget;
          el.setPointerCapture(e.pointerId);
          drag.current = { y: e.clientY, h: height, s: el.getBoundingClientRect().width / el.offsetWidth || 1 };
          onResizing(true);
        }}
        onPointerMove={(e) => {
          const d = drag.current;
          if (d) onResize(clamp(d.h - (e.clientY - d.y) / d.s, min, max));
        }}
        onPointerUp={() => {
          drag.current = null;
          onResizing(false);
        }}
        onPointerCancel={() => {
          drag.current = null;
          onResizing(false);
        }}
        onDoubleClick={() => onResize(280)}
        onKeyDown={(e) => {
          const step = e.shiftKey ? 64 : 16;
          if (e.key === "ArrowUp") onResize(clamp(height + step, min, max));
          else if (e.key === "ArrowDown") onResize(clamp(height - step, min, max));
          else if (e.key === "Home") onResize(min);
          else if (e.key === "End") onResize(max);
          else return;
          e.preventDefault();
        }}
      />
      <header className="ui-wf-run__head">
        <div className="ui-wf-tabs" role="tablist" aria-label={t("auto:builder.runDetails")} onKeyDown={(e) => rove(e, runTabs, tab, onTab)}>
          {runTabs.map((x) => (
            <button key={x} type="button" role="tab" id={`ui-wf-tab-${x}`} aria-selected={tab === x} aria-controls="ui-wf-run-body" tabIndex={tab === x ? 0 : -1} onClick={() => onTab(x)} aria-label={t(`auto:builder.tab.${x}`)}>
              <Icon icon={tabIcons[x]} size={14} />
              <span>{t(`auto:builder.tab.${x}`)}</span>
              {x === "errors" && errorCount > 0 && <span className="ui-wf-tabs__count">{errorCount}</span>}
            </button>
          ))}
        </div>
        <button type="button" className="ui-wf-icon-btn" aria-label={t("auto:builder.closeRuns")} onClick={onClose}>
          <Icon icon={XIcon} size={15} />
        </button>
      </header>
      <div ref={bodyRef} className="ui-wf-run__body" id="ui-wf-run-body" role="tabpanel" aria-labelledby={`ui-wf-tab-${tab}`}>
        {body()}
      </div>
      {/* Read out once per finished step, politely */}
      <span className="ui-wf-sr" role="status" aria-live="polite" aria-atomic="true">
        {announce}
      </span>
      <footer className="ui-wf-run__foot">
        <button type="button" className="ui-wf-btn" disabled={busy} onClick={onRun}>
          {busy ? <Spin /> : <Icon icon={PlayIcon} size={14} />}
          {busy ? t("auto:builder.running") : t("auto:rule.dryRun")}
        </button>
      </footer>
    </section>
  );
}


/** The dots' spacing on screen: 20 canvas units at the zoom, doubled as you zoom out so it never gets tighter than 16px */
function gridStep(k: number) {
  let step = 20 * k;
  while (step < 16) step *= 2;
  return step;
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Workflow Builder: the canvas fills its box, the run panel slides up under it; palette, toolbar and settings float on top.
 * Every colour comes from the --aui-* tokens (Steve's, both modes: web/src/styles/affectui-theme.css). The screen's own
 * names carry a --wf- prefix so nothing inside the settings (Steve's own components) picks them up. */
.ui-wf {
  /* The page never scrolls to follow what moves inside the canvas or the run panel */
  overflow-anchor: none;
  --wf-c-panel: var(--aui-surface);
  --wf-c-card: var(--aui-surface);
  --wf-c-line: var(--aui-border);
  --wf-c-line-strong: var(--aui-border-strong);
  --wf-c-strong: var(--aui-ink);
  --wf-c-ink: var(--aui-ink);
  --wf-c-text: color-mix(in srgb, var(--aui-ink) 72%, var(--aui-muted));
  --wf-c-muted: var(--aui-muted);
  --wf-c-faint: var(--aui-placeholder);
  --wf-c-placeholder: var(--aui-placeholder);
  --wf-c-chip: var(--aui-subtle);
  --wf-c-hover: color-mix(in srgb, var(--aui-ink) 5%, transparent);
  --wf-c-accent: var(--aui-info);
  --wf-c-accent-soft: var(--aui-info-soft);
  --wf-c-button: var(--aui-primary);
  --wf-c-button-hover: color-mix(in srgb, var(--aui-primary) 88%, var(--aui-surface));
  --wf-c-card-shadow: var(--aui-shadow-card);
  --wf-c-card-shadow-hover: var(--lift-hover, var(--aui-shadow-card));
  --wf-c-ease: var(--aui-ease);
  --wf-c-edge-active: var(--aui-info);
  --wf-canvas: var(--canvas-graph, var(--aui-subtle));
  --ui-wf-focus: var(--aui-focus);
  --wf-dot: color-mix(in srgb, var(--aui-ink) 16%, transparent);
  --wf-edge: color-mix(in srgb, var(--aui-ink) 24%, transparent);
  --wf-port: color-mix(in srgb, var(--aui-ink) 26%, transparent);
  --wf-field: var(--aui-subtle);
  --wf-set-ring: color-mix(in srgb, var(--aui-info) 30%, transparent);
  --wf-set-tile: var(--aui-info-soft);
  --wf-warn: var(--aui-danger);
  --wf-warn-bg: color-mix(in srgb, var(--aui-danger) 6%, transparent);
  --wf-warn-tile: var(--aui-danger-soft);
  --wf-ok: var(--aui-success);
  --wf-ok-bg: var(--aui-success-soft);
  --wf-run: var(--aui-primary);
  --wf-out: var(--aui-ease);
  --wf-well: var(--aui-subtle);
  --wf-card-well: color-mix(in srgb, var(--aui-subtle) 70%, var(--aui-surface));
  --wf-violet: var(--aui-info);
  --wf-violet-bg: var(--aui-info-soft);
  --wf-gray: var(--aui-muted);
  --wf-amber: var(--aui-warning);
  --wf-amber-bg: var(--aui-warning-soft);
  --wf-green: var(--aui-success);
  --wf-green-bg: var(--aui-success-soft);
  --wf-tip: var(--aui-ink);
  --wf-tip-ink: var(--aui-surface);
  --wf-float: var(--aui-shadow-popover);
  --mono: var(--aui-font-mono);
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  overflow: hidden;
  font-family: var(--aui-font);
  background: var(--wf-canvas);
}
/* Icon buttons in the heads of the palette, the settings and the run panel */
.ui-wf-icon-btn { display: grid; flex: none; place-items: center; width: 28px; height: 28px; border-radius: var(--aui-radius-sm); color: var(--wf-c-muted); transition: background-color 120ms ease, color 120ms ease; }
.ui-wf-icon-btn:hover { background: var(--wf-c-hover); color: var(--wf-c-ink); }
.ui-wf-icon-btn:focus-visible, .ui-wf button:focus-visible, .ui-wf [role="button"]:focus-visible { outline: 2px solid var(--ui-wf-focus); outline-offset: 2px; }
/* A quiet empty state: a title and a line, centred */
.ui-wf-emptystate { display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px 16px; text-align: center; }
.ui-wf-emptystate b { font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; color: var(--wf-c-strong); }
.ui-wf-emptystate span { max-width: 340px; font-size: calc(12px * var(--type-scale, 1)); line-height: 18px; color: var(--wf-c-muted); }

/* Buttons: white with the card's hairline, one dark primary */
.ui-wf-btn { display: inline-flex; align-items: center; justify-content: center; gap: 6px; height: 28px; padding: 0 10px; border-radius: calc(var(--aui-radius, 7px) - 1px); background: var(--wf-c-card); box-shadow: var(--wf-c-card-shadow); font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; color: var(--wf-c-ink); white-space: nowrap; transition: box-shadow 140ms ease, background-color 140ms ease, opacity 140ms ease; }
.ui-wf-btn:hover { box-shadow: var(--wf-c-card-shadow-hover); }
.ui-wf-btn:disabled { opacity: 0.6; cursor: default; }
.ui-wf-btn > svg { color: var(--wf-c-muted); }
.ui-wf-btn--primary { background: var(--wf-c-button); box-shadow: none; color: var(--aui-on-primary); }
.ui-wf-btn--primary > svg { color: inherit; }
.ui-wf-btn--primary:hover { background: var(--wf-c-button-hover); box-shadow: none; }
.ui-wf-btn--quiet { align-self: flex-start; margin-left: -8px; background: none; box-shadow: none; color: var(--wf-c-muted); }
.ui-wf-btn--quiet:hover { background: var(--wf-c-hover); box-shadow: none; color: var(--wf-c-ink); }

/* ---------- Canvas ---------- */
.ui-wf__area { position: relative; flex: 1; min-height: 0; overflow: hidden; }
.ui-wf-vp {
  position: absolute;
  inset: 0;
  overflow: hidden;
  outline: none;
  touch-action: none;
  user-select: none;
  background-color: var(--wf-canvas);
  /* Small dots that keep their size at every zoom – the spacing and position follow the canvas (set inline) */
  background-image: radial-gradient(circle at 1px 1px, var(--wf-dot) 0.7px, transparent 1.05px);
  background-size: 20px 20px;
  cursor: default;
}
.ui-wf-vp[data-tool="hand"] { cursor: grab; }
.ui-wf-vp[data-panning] { cursor: grabbing; }
.ui-wf-world { position: absolute; top: 0; left: 0; width: 0; height: 0; transform-origin: 0 0; }
.ui-wf-edges { position: absolute; top: 0; left: 0; width: 1px; height: 1px; overflow: visible; pointer-events: none; }
.ui-wf-edge { fill: none; stroke: var(--wf-edge); stroke-width: 1.25; transition: stroke 140ms ease; }
.ui-wf-edge[data-selected] { stroke: var(--wf-c-accent); stroke-width: 1.6; }
/* A wide invisible twin of each curve takes the click */
.ui-wf-edge-hit { fill: none; stroke: transparent; stroke-width: 12; pointer-events: stroke; cursor: pointer; }
.ui-wf-edge-hit:hover + .ui-wf-edge:not([data-selected]) { stroke: var(--wf-c-muted); }
/* A focused edge: no box outline (it would be the curve's bounding box) – the curve itself thickens with a soft accent halo */
.ui-wf-edge-hit:focus { outline: none; }
.ui-wf-edge-hit:focus-visible { stroke: var(--wf-c-accent-soft); stroke-width: 8; }
.ui-wf-edge-hit:focus-visible + .ui-wf-edge { stroke: var(--wf-c-accent); stroke-width: 2.2; }
.ui-wf-vp:is([data-tool="hand"], [data-panning], [data-wiring]) .ui-wf-edge-hit { cursor: inherit; }
/* The connection being dragged: dashed while it follows the pointer, solid once it snaps to an input */
.ui-wf-edge--draft { stroke: var(--wf-c-accent); stroke-width: 1.5; stroke-dasharray: 4 3; }
.ui-wf-edge--draft[data-snapped] { stroke-dasharray: none; }
/* A run draws the curve it takes in accent, from its start to its end */
/* A branch's Yes / No sits on its curve, on a solid chip so the line doesn't run through the text */
.ui-wf-edge-tag { position: absolute; padding: 1px 5px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--wf-c-card); box-shadow: var(--wf-c-card-shadow); translate: -50% -50%; font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 14px; color: var(--wf-c-muted); white-space: nowrap; pointer-events: none; }
.ui-wf-edge--lit { stroke: var(--wf-c-accent); stroke-width: 1.6; stroke-dasharray: 1; stroke-dashoffset: 1; animation: ui-wf-draw 520ms var(--wf-c-ease) forwards; }
@keyframes ui-wf-draw { to { stroke-dashoffset: 0; } }

/* Kinds: a quiet tag above the card – a trigger in ink, AI in the accent, the rest gray (Steve keeps colour for state).
 * The trace, the impulse and the glow where it arrives are always the accent */
.ui-wf-node { --kind: var(--wf-gray); --kind-bg: var(--wf-c-chip); }
.ui-wf-node[data-kind="trigger"] { --kind: var(--wf-c-ink); }
.ui-wf-node[data-kind="agent"] { --kind: var(--wf-violet); --kind-bg: var(--wf-violet-bg); }
.ui-wf-flow, .ui-wf-arrive, .ui-wf-trace { --kind: var(--wf-c-accent); }

/* Node: a kind tag above, then a white card – header (tile, title, status chip) and two detail lines; every card the same size */
.ui-wf-node {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: ${NODE_W}px;
  height: ${NODE_H}px;
  padding: 12px 12px 4px;
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--wf-c-card);
  box-shadow: var(--wf-c-card-shadow);
  cursor: grab;
  transition: box-shadow 160ms ease, opacity 200ms ease;
}
.ui-wf-node:hover { box-shadow: var(--wf-c-card-shadow-hover); }
.ui-wf-vp[data-panning] .ui-wf-node, .ui-wf-vp[data-tool="hand"] .ui-wf-node { cursor: inherit; }
/* After a run has played: the nodes it didn't pass step back */
.ui-wf-node[data-skipped] { opacity: 0.5; }
.ui-wf-edge[data-dim] { opacity: 0.45; }
/* Selected: a hairline accent ring, no glow */
.ui-wf-node[data-selected] { box-shadow: 0 0 0 1.5px var(--wf-c-accent), 0 0 0 5px var(--wf-c-accent-soft); }
.ui-wf-node:focus-visible { outline: 2px solid var(--ui-wf-focus); outline-offset: 2px; }
.ui-wf-node__kind { position: absolute; bottom: calc(100% + 6px); left: 0; padding: 2px 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--kind-bg); font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; color: var(--kind); white-space: nowrap; }
.ui-wf-node__head { display: flex; align-items: center; gap: 8px; min-width: 0; height: 24px; }
.ui-wf-node__head > b { flex: 1; min-width: 0; overflow: hidden; font-size: calc(14px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-strong); }
.ui-wf-node__tile { display: grid; flex: none; place-items: center; width: 24px; height: 24px; border-radius: 7px; background: var(--wf-c-chip); color: var(--wf-c-text); }
.ui-wf-node__tile[data-kind="trigger"], .ui-wf-node[data-kind="trigger"] .ui-wf-node__tile:not([data-tone]) { background: var(--wf-c-ink); color: var(--wf-c-card); }
.ui-wf-node__tile[data-kind="agent"], .ui-wf-node[data-kind="agent"] .ui-wf-node__tile:not([data-tone]) { background: var(--wf-set-tile); color: var(--wf-c-accent); }
.ui-wf-node__tile[data-tone="warn"] { background: var(--wf-warn-tile); color: var(--wf-warn); }
/* The status chip: quiet gray; red-tinted when something is missing, green with a check once its step is done */
.ui-wf-node__chip { display: inline-flex; flex: none; align-items: center; gap: 4px; max-width: 120px; height: 20px; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--wf-c-chip); font-size: calc(12px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; white-space: nowrap; color: var(--wf-c-muted); font-variant-numeric: tabular-nums; }
.ui-wf-node__chip[data-tone="warn"] { background: var(--wf-warn-tile); color: var(--wf-warn); }
.ui-wf-node__chip[data-tone="attn"] { background: var(--wf-amber-bg); color: var(--wf-amber); }
.ui-wf-node__chip[data-tone="info"] { background: var(--wf-c-accent-soft); color: var(--wf-c-accent); }
.ui-wf-node__chip[data-tone="done"] { background: var(--wf-ok-bg); color: var(--wf-ok); animation: ui-wf-swap 420ms var(--wf-out) both; }
.ui-wf-node__chip .ui-wf-spin { width: 11px; height: 11px; color: var(--wf-c-accent); }
@keyframes ui-wf-swap { from { opacity: 0; transform: translateY(3px); } }
.ui-wf-node__line { flex: none; overflow: hidden; height: 18px; padding-left: 32px; font-size: calc(13px * var(--type-scale, 1)); line-height: 18px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-text); }
.ui-wf-node__line[data-warn] { color: var(--wf-warn); }
.ui-wf-node__mono { overflow: hidden; font-family: var(--mono); font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-muted); }
/* The well: a light gray inner panel 4px in from the card's edges, holding the mono line or the chips */
.ui-wf-node__well { display: flex; flex: none; align-items: center; min-width: 0; height: 36px; margin: 0 -8px; padding: 6px 8px; border-radius: calc(var(--aui-radius-lg, 10px) - 3px); background: var(--wf-card-well); }
/* 3px of room around the chips, so overflow: hidden doesn't cut off their hairline and shadow */
.ui-wf-node__chips, .ui-wf-node__files { display: flex; gap: 4px; min-width: 0; margin: -3px; padding: 3px; overflow: hidden; }
/* On the gray well, chips are white with the card's hairline so they still stand out */
/* File chips, as in Collaboration Stream: soft gray, radius 7, a two-gray icon; hover darkens; picked ones take the accent tint with an × */
.ui-wf-node__tag { display: inline-flex; flex: none; align-items: center; gap: 3px; height: 24px; padding: 0 7px; border-radius: calc(var(--aui-radius-sm, 5px) + 2px); background: var(--wf-c-card); box-shadow: var(--wf-c-card-shadow); font-size: calc(12px * var(--type-scale, 1)); line-height: 18px; color: var(--wf-c-text); white-space: nowrap; }

/* A run, after the snippet's play: the impulse arrives with a soft glow at the input, then a trace runs once round the card */
.ui-wf-arrive { position: absolute; inset: 0; overflow: hidden; border-radius: inherit; pointer-events: none; opacity: 0; animation: ui-wf-arrive 700ms cubic-bezier(0.2, 0, 0.25, 1) forwards; }
.ui-wf-arrive::before { content: ""; position: absolute; top: 0; left: 50%; width: 96px; height: 96px; border-radius: 50%; background: radial-gradient(circle, var(--kind) 0%, transparent 66%); filter: blur(9px); opacity: 0.22; transform: translate(-50%, -50%); }
@keyframes ui-wf-arrive { 22% { opacity: 1; } 100% { opacity: 0; } }
.ui-wf-node[data-arrived] .ui-wf-port--in { animation: ui-wf-port-glow 700ms var(--wf-out) both; }
@keyframes ui-wf-port-glow { 0%, 28% { background: var(--kind-arrive, var(--wf-c-accent)); box-shadow: 0 0 0 1.5px var(--kind-arrive, var(--wf-c-accent)), 0 0 8px var(--kind-arrive, var(--wf-c-accent)); scale: 1.4; } }
/* Three overlapping dashes of different lengths: their opacity adds up in the middle and fades out at head and tail */
.ui-wf-trace { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
.ui-wf-trace__seg { fill: none; stroke: var(--kind); stroke-linecap: round; opacity: 0; }
.ui-wf-trace__seg--1 { stroke-dasharray: 0.22 0.78; animation: ui-wf-trace1 ${TRACE_MS}ms linear forwards; }
.ui-wf-trace__seg--2 { stroke-dasharray: 0.14 0.86; animation: ui-wf-trace2 ${TRACE_MS}ms linear forwards; }
.ui-wf-trace__seg--3 { stroke-dasharray: 0.06 0.94; animation: ui-wf-trace3 ${TRACE_MS}ms linear forwards; }
.ui-wf-trace__line { stroke-width: 1; }
.ui-wf-trace__line.ui-wf-trace__seg--1 { stroke-opacity: 0.18; }
.ui-wf-trace__line.ui-wf-trace__seg--2 { stroke-opacity: 0.24; }
.ui-wf-trace__line.ui-wf-trace__seg--3 { stroke-opacity: 0.32; }
.ui-wf-trace__glow { filter: blur(4px); }
.ui-wf-trace__glow .ui-wf-trace__seg { stroke-width: 3.5; stroke-opacity: 0.1; }
@keyframes ui-wf-trace1 { 0% { stroke-dashoffset: 0.11; opacity: 0; } 18%, 80% { opacity: 1; } 100% { stroke-dashoffset: -0.89; opacity: 0; } }
@keyframes ui-wf-trace2 { 0% { stroke-dashoffset: 0.07; opacity: 0; } 18%, 80% { opacity: 1; } 100% { stroke-dashoffset: -0.93; opacity: 0; } }
@keyframes ui-wf-trace3 { 0% { stroke-dashoffset: 0.03; opacity: 0; } 18%, 80% { opacity: 1; } 100% { stroke-dashoffset: -0.97; opacity: 0; } }
/* The impulse along an edge: a gradient stretch of the curve in the sender's colour (drawn by FlowTrail) */
.ui-wf-flow__line { fill: none; stroke-width: 1.6; stroke-linecap: round; }
.ui-wf-flow__stop { stop-color: var(--kind); }
/* While the run follows the running node, the view glides */
.ui-wf-vp[data-follow] .ui-wf-world { transition: transform 600ms var(--wf-out); }
.ui-wf-vp[data-follow] { transition: background-position 600ms var(--wf-out); }

.ui-wf-done { display: grid; flex: none; place-items: center; width: 16px; height: 16px; border-radius: 50%; background: var(--wf-ok-bg); color: var(--wf-ok); animation: ui-wf-pop 260ms var(--wf-c-ease) both; }
.ui-wf-done[data-failed] { background: var(--wf-warn-tile); color: var(--wf-warn); }
.ui-wf-done[data-waiting] { background: var(--wf-c-accent-soft); color: var(--wf-c-accent); }
.ui-wf-idle { width: 8px; height: 8px; border-radius: 50%; background: var(--wf-c-faint); }
/* Read by screen readers only */
.ui-wf-sr { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip-path: inset(50%); white-space: nowrap; border: 0; }
/* Chips in the well give way before they overflow: a long name ends in "…", more than three become "+N" */
.ui-wf-node__tag > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-wf-node__tag > svg { flex: none; }
.ui-wf-node__more { display: inline-flex; flex: none; align-items: center; height: 24px; padding: 0 7px; border-radius: calc(var(--aui-radius-sm, 5px) + 2px); background: var(--wf-c-card); box-shadow: var(--wf-c-card-shadow); font-size: calc(12px * var(--type-scale, 1)); line-height: 18px; color: var(--wf-c-muted); font-variant-numeric: tabular-nums; }

/* Ports: the input in the middle of the top edge, outputs on the bottom edge – a free one ends in a blue + below it */
.ui-wf-port { position: absolute; width: 8px; height: 8px; border-radius: 50%; background: var(--wf-c-card); box-shadow: 0 0 0 1px var(--wf-port); transition: box-shadow 140ms ease, background-color 140ms ease, scale 160ms var(--wf-c-ease); }
.ui-wf-port--in { top: -4px; left: ${NODE_W / 2 - 4}px; }
.ui-wf-out { position: absolute; top: 100%; width: 0; height: 0; }
.ui-wf-out > .ui-wf-port { top: -4px; left: -4px; cursor: crosshair; }
/* A bigger, invisible target round the output dot, to start dragging a connection */
.ui-wf-port--out::after { content: ""; position: absolute; inset: -6px; border-radius: 50%; }
.ui-wf-port--out:hover { box-shadow: 0 0 0 1px var(--wf-c-accent); scale: 1.25; }
.ui-wf-out__label { position: absolute; top: 3px; left: 7px; font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 14px; color: var(--wf-c-faint); white-space: nowrap; }
/* A 16px dot inside a transparent 6px border – the border is the larger hit area (::after is the tooltip, so it can't be) */
.ui-wf-add { position: absolute; top: 8px; left: -14px; display: grid; place-items: center; box-sizing: content-box; width: 16px; height: 16px; border: 6px solid transparent; border-radius: 50%; background: var(--wf-c-accent) padding-box; color: var(--aui-on-primary); transition: transform 160ms var(--wf-c-ease); }
.ui-wf-add::before { content: ""; position: absolute; bottom: 100%; left: 7.5px; width: 1px; height: 10px; background: var(--wf-edge); }
.ui-wf-add:hover, .ui-wf-add[aria-expanded="true"] { transform: scale(1.12); }
.ui-wf-add:focus-visible { outline-offset: 2px; }
/* While a connection is dragged: every input that can take it rings in accent, the one it snaps to fills */
.ui-wf-node[data-wire] .ui-wf-port--in { background: var(--wf-c-accent-soft); box-shadow: 0 0 0 1.5px var(--wf-c-accent); scale: 1.3; }
.ui-wf-node[data-wire="snap"] { box-shadow: 0 0 0 1.5px var(--wf-c-accent); }
.ui-wf-node[data-wire="snap"] .ui-wf-port--in { background: var(--wf-c-accent); scale: 1.5; }
.ui-wf-vp[data-wiring], .ui-wf-vp[data-wiring] .ui-wf-node { cursor: crosshair; }
/* Small dark tooltips for the + and the toolbar */
.ui-wf-add[data-tip]:is(:hover, :focus-visible)::after, .ui-wf-tb [data-tip]:is(:hover, :focus-visible)::after {
  content: attr(data-tip);
  position: absolute;
  inset: auto;
  bottom: calc(100% + 8px);
  left: 50%;
  translate: -50% 0;
  padding: 3px 7px;
  border-radius: 6px;
  background: var(--wf-tip);
  font-size: calc(12px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 16px;
  white-space: pre;
  color: var(--wf-tip-ink);
  pointer-events: none;
  animation: ui-wf-fade 140ms ease both;
}
/* The +'s tooltip goes below it, out of the curve's way */
.ui-wf-add[data-tip]:is(:hover, :focus-visible)::after { top: calc(100% + 6px); bottom: auto; }

/* ---------- Palette ---------- */
.ui-wf-pal {
  position: absolute;
  top: 10px;
  bottom: 10px;
  left: 10px;
  z-index: 6;
  display: flex;
  flex-direction: column;
  width: 216px;
  border-radius: calc(var(--aui-radius-lg, 10px) + 2px);
  background: var(--wf-c-card);
  box-shadow: var(--wf-float);
  animation: ui-wf-in-left 260ms var(--wf-c-ease) both;
}
.ui-wf-pal__top { display: flex; flex: none; align-items: center; justify-content: space-between; height: 40px; padding: 0 6px 0 14px; font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; color: var(--wf-c-strong); }
.ui-wf-pal__scroll { flex: 1; min-height: 0; overflow-y: auto; padding: 0 6px 6px; scrollbar-width: thin; }
.ui-wf-pal__groups { display: flex; flex-direction: column; }
.ui-wf-pal__group + .ui-wf-pal__group { margin-top: 6px; padding-top: 6px; box-shadow: inset 0 0.5px 0 var(--wf-c-line); }
.ui-wf-pal__head { display: flex; align-items: center; gap: 6px; height: 28px; margin: 0; padding: 0 8px; font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; color: var(--wf-c-muted); }
.ui-wf-pal__head span { color: var(--wf-c-faint); font-variant-numeric: tabular-nums; }
.ui-wf-pal__item { display: flex; align-items: center; gap: 8px; width: 100%; height: 30px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: calc(13px * var(--type-scale, 1)); text-align: left; color: var(--wf-c-ink); cursor: pointer; transition: background-color 120ms ease; }
/* Drag an item out to place it; on touch a sideways drag picks it up, an up-and-down one scrolls the list */
.ui-wf-pal__item[data-drag] { cursor: grab; touch-action: pan-y; user-select: none; -webkit-user-select: none; -webkit-touch-callout: none; }
/* The node card under the pointer while an item is dragged – scaled like the canvas */
.ui-wf-ghost { position: absolute; z-index: 10; width: 0; height: 0; transform-origin: 0 0; pointer-events: none; transition: opacity 120ms ease; }
.ui-wf-ghost .ui-wf-node { cursor: grabbing; box-shadow: var(--wf-float); }
.ui-wf-ghost:not([data-over]) { opacity: 0.5; }
.ui-wf-pal__item:hover, .ui-wf-pal__item:focus-visible { background: var(--wf-c-hover); }
.ui-wf-pal__item:focus-visible { outline-offset: -2px; }
.ui-wf-pal__icon { display: grid; flex: none; place-items: center; width: 18px; color: var(--wf-c-muted); }
.ui-wf-pal__text { display: grid; min-width: 0; }
.ui-wf-pal__label { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-wf-pal__desc { display: none; overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-muted); }
.ui-wf-pal__none { margin: 8px; font-size: calc(13px * var(--type-scale, 1)); color: var(--wf-c-muted); }
/* What an item does, beside it */
.ui-wf-tip { position: absolute; z-index: 9; width: max-content; max-width: 220px; padding: 7px 10px; border-radius: 8px; background: var(--aui-overlay, var(--wf-c-card)); box-shadow: var(--aui-shadow-popover, var(--wf-float)); translate: 0 -50%; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; color: var(--wf-c-text); pointer-events: none; animation: ui-wf-fade 160ms ease both; }

/* The picker at a free output's + */

/* The picker at a free output's + */
.ui-wf-picker { position: absolute; z-index: 9; display: flex; flex-direction: column; width: 240px; max-height: 340px; border-radius: calc(var(--aui-radius-lg, 10px) + 2px); background: var(--aui-overlay, var(--wf-c-card)); box-shadow: var(--aui-shadow-popover, var(--wf-float)); animation: ui-wf-pop 200ms var(--wf-c-ease) both; }
.ui-wf-picker__search { display: flex; flex: none; align-items: center; gap: 8px; height: 40px; padding: 0 12px; box-shadow: inset 0 -0.5px 0 var(--wf-c-line); color: var(--wf-c-faint); }
.ui-wf-picker__search input { flex: 1; min-width: 0; border: 0; outline: 0; background: none; font: inherit; font-size: calc(13px * var(--type-scale, 1)); color: var(--wf-c-ink); }
.ui-wf-picker__search input::placeholder { color: var(--wf-c-placeholder); }
.ui-wf-picker__search:focus-within { box-shadow: inset 0 -1px 0 var(--wf-c-line-strong); }
.ui-wf-picker__list { flex: 1; min-height: 0; overflow-y: auto; padding: 4px 6px 6px; }

/* ---------- Toolbar and minimap ---------- */
.ui-wf-tb { position: absolute; bottom: 14px; left: 50%; z-index: 5; display: flex; align-items: center; gap: 2px; padding: 4px; border-radius: var(--aui-radius-lg, 10px); background: var(--wf-c-card); box-shadow: var(--wf-float); translate: -50% 0; }
/* The toolbar centres in the free canvas between the palette (226px) and the settings (330px) */
.ui-wf__area[data-palette] .ui-wf-tb { left: calc(50% + 113px); }
.ui-wf__area[data-settings] .ui-wf-tb { left: calc(50% - 165px); }
.ui-wf__area[data-palette][data-settings] .ui-wf-tb { left: calc(50% - 52px); }
.ui-wf-tb__btn { position: relative; display: grid; place-items: center; width: 28px; height: 28px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--wf-c-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-wf-tb__btn:hover { background: var(--wf-c-hover); color: var(--wf-c-ink); }
.ui-wf-tb__btn[aria-pressed="true"] { background: var(--wf-c-chip); color: var(--wf-c-ink); }
.ui-wf-tb__add[aria-pressed="true"] { background: var(--wf-c-accent-soft); color: var(--wf-c-accent); }
.ui-wf-tb__sep { width: 0.5px; height: 16px; margin: 0 4px; background: var(--wf-c-line-strong); }
.ui-wf-tb__zoom { position: relative; width: 44px; height: 28px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: calc(12px * var(--type-scale, 1)); color: var(--wf-c-muted); font-variant-numeric: tabular-nums; }
.ui-wf-tb__zoom:hover { background: var(--wf-c-hover); color: var(--wf-c-ink); }
.ui-wf-tb__run, .ui-wf-tb__run:hover { background: var(--wf-run); color: var(--aui-on-primary); }
.ui-wf-tb__run .ui-wf-spin { color: inherit; }
.ui-wf-tb__run:hover:not(:disabled) { filter: brightness(1.06); }
.ui-wf-tb__run:is(:disabled, [aria-disabled="true"]) { opacity: 0.6; cursor: default; }

/* Loading: the same cards in the same places, bars where the words go */

/* ---------- Run panel ---------- */
.ui-wf-run { position: relative; flex: none; display: flex; flex-direction: column; height: 0; overflow: hidden; background: var(--wf-c-panel); box-shadow: inset 0 0.5px 0 var(--wf-c-line-strong); opacity: 0; transition: height 340ms var(--wf-c-ease), opacity 240ms ease; }
.ui-wf-run[data-open] { height: var(--run-h); opacity: 1; }
.ui-wf[data-resizing] :is(.ui-wf-run, .ui-wf-set) { transition: none; }
.ui-wf[data-resizing] { cursor: row-resize; user-select: none; }
.ui-wf-run__grip { position: absolute; top: 0; left: 0; right: 0; z-index: 2; height: 6px; cursor: row-resize; touch-action: none; }
.ui-wf-run__grip::after { content: ""; position: absolute; top: 0; left: 0; right: 0; height: 1px; background: transparent; transition: background-color 140ms ease; }
.ui-wf-run__grip:hover::after, .ui-wf[data-resizing] .ui-wf-run__grip::after { background: var(--wf-c-edge-active); }
.ui-wf-run__grip:focus-visible { outline-offset: -2px; }
.ui-wf-run__head { display: flex; flex: none; align-items: center; justify-content: space-between; gap: 8px; height: 44px; padding: 0 10px; box-shadow: inset 0 -0.5px 0 var(--wf-c-line); }
.ui-wf-tabs { display: flex; gap: 2px; min-width: 0; overflow-x: auto; scrollbar-width: none; }
.ui-wf-tabs::-webkit-scrollbar { display: none; }
.ui-wf-tabs button { position: relative; display: inline-flex; flex: none; align-items: center; gap: 6px; height: 28px; padding: 0 8px; border-radius: calc(var(--aui-radius, 7px) - 1px); font-size: calc(13px * var(--type-scale, 1)); color: var(--wf-c-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-wf-tabs button:hover { color: var(--wf-c-ink); }
.ui-wf-tabs button[aria-selected="true"] { background: var(--wf-c-chip); color: var(--wf-c-ink); }
.ui-wf-tabs button:focus-visible { outline-offset: -2px; }
.ui-wf-tabs__count { min-width: 16px; padding: 0 4px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--wf-warn-tile); font-size: calc(11px * var(--type-scale, 1)); line-height: 16px; text-align: center; color: var(--wf-warn); }
.ui-wf-run__body { flex: 1; min-height: 0; overflow-y: auto; padding: 6px 6px 8px; }
.ui-wf-run__pad { padding: 6px 6px 0; }
.ui-wf-run__foot { display: flex; flex: none; justify-content: center; padding: 8px; box-shadow: inset 0 0.5px 0 var(--wf-c-line); }
/* A run and its steps: rows of one height, figures on the right in fixed columns */
.ui-wf-runs__run + .ui-wf-runs__run { margin-top: 2px; }
.ui-wf-runs__head, .ui-wf-step__row, .ui-wf-list__row {
  display: grid;
  grid-template-columns: 16px auto minmax(0, max-content) auto minmax(0, 1fr) 64px 96px;
  align-items: center;
  column-gap: 8px;
  width: 100%;
  height: 32px;
  padding: 0 10px 0 8px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  font-size: calc(13px * var(--type-scale, 1));
  text-align: left;
  color: var(--wf-c-ink);
  transition: background-color 120ms ease;
}
.ui-wf-runs__head { grid-template-columns: 16px 12px minmax(0, max-content) minmax(0, 1fr) 64px; }
.ui-wf-runs__line { display: flex; align-items: center; gap: 4px; }
.ui-wf-runs__line > .ui-wf-runs__head { flex: 1; min-width: 0; }
.ui-wf-runs__head > svg { color: var(--wf-c-faint); transition: rotate 200ms var(--wf-c-ease); }
.ui-wf-runs__head[aria-expanded="true"] > svg { rotate: 90deg; }
.ui-wf-runs__head b { font-weight: 500; color: var(--wf-c-strong); }
.ui-wf-runs__id { color: var(--wf-c-faint); font-variant-numeric: tabular-nums; }
.ui-wf-runs__head:hover, .ui-wf-step__row:hover, .ui-wf-list__row:hover { background: var(--wf-c-hover); }
.ui-wf-runs__head:focus-visible, .ui-wf-step__row:focus-visible, .ui-wf-list__row:focus-visible { outline-offset: -2px; }
.ui-wf-steps { margin: 0; padding: 0 0 0 20px; list-style: none; }
.ui-wf-step { animation: ui-wf-rise 320ms var(--wf-c-ease) both; }
.ui-wf-step__row { grid-template-columns: 16px minmax(0, max-content) auto minmax(0, 1fr) 64px; }
.ui-wf-step__note { min-width: 0; overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-muted); }
.ui-wf-run__error { margin: 2px 10px 6px 32px; }
.ui-wf-step__row > .ui-wf-count { grid-column: 3; }
.ui-wf-step__row > .ui-wf-step__time { grid-column: 5; }
.ui-wf-step__mark { display: grid; place-items: center; width: 16px; height: 16px; color: var(--wf-c-accent); }
.ui-wf-step__mark > svg { color: var(--wf-warn); }
.ui-wf-step__mark > .ui-wf-spin { color: var(--wf-c-accent); }
/* A failed step's message under its row: in the warning colour, wrapped at any point, two lines at most (all of it in the tooltip) */
.ui-wf-step__error { display: -webkit-box; margin: -2px 10px 6px 32px; overflow: hidden; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow-wrap: anywhere; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; color: var(--wf-warn); }
.ui-wf-list__row--error .ui-wf-list__meta { color: var(--wf-warn); }
.ui-wf-step__name { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-wf-step__time { font-size: calc(12px * var(--type-scale, 1)); text-align: right; color: var(--wf-c-muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
.ui-wf-step__cost { grid-column: 7; overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); text-align: right; color: var(--wf-c-ink); font-variant-numeric: tabular-nums; white-space: nowrap; text-overflow: ellipsis; }
.ui-wf-runs__head .ui-wf-step__cost { color: var(--wf-c-text); }
.ui-wf-badge { flex: none; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--wf-c-chip); font-size: calc(11px * var(--type-scale, 1)); line-height: 16px; color: var(--wf-c-muted); }
.ui-wf-count { flex: none; padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); box-shadow: inset 0 0 0 0.5px var(--wf-c-line-strong); font-size: calc(11px * var(--type-scale, 1)); line-height: 16px; color: var(--wf-c-muted); font-variant-numeric: tabular-nums; }
.ui-wf-spin { width: 14px; height: 14px; }
.ui-wf-list { margin: 0; padding: 0; list-style: none; }
.ui-wf-list__row { grid-template-columns: 16px minmax(0, max-content) minmax(0, 1fr) auto; }
.ui-wf-list__row > svg { color: var(--wf-c-muted); }
.ui-wf-list__meta { min-width: 0; overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-muted); }
.ui-wf-tag { padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--wf-c-chip); font-size: calc(11px * var(--type-scale, 1)); line-height: 16px; color: var(--wf-c-muted); white-space: nowrap; }
.ui-wf-tag[data-tone="ok"] { background: var(--wf-ok-bg); color: var(--wf-ok); }
.ui-wf-tag[data-tone="warn"] { background: var(--wf-warn-tile); color: var(--wf-warn); }
/* Timeline: each step's share of the run as a quiet bar */
.ui-wf-timeline { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }
.ui-wf-timeline li { display: grid; grid-template-columns: 160px minmax(0, 1fr) 64px; align-items: center; gap: 12px; height: 30px; padding: 0 10px 0 8px; font-size: calc(13px * var(--type-scale, 1)); color: var(--wf-c-ink); }
.ui-wf-timeline__track { position: relative; height: 8px; border-radius: 4px; background: var(--wf-field); }
.ui-wf-timeline__track i { position: absolute; top: 0; bottom: 0; border-radius: 4px; background: var(--wf-c-faint); transition: background-color 200ms ease; }
.ui-wf-timeline li[data-state="running"] i { background: var(--wf-c-accent); }
.ui-wf-timeline li[data-state="waiting"] { color: var(--wf-c-faint); }
.ui-wf-timeline li[data-state="waiting"] i { background: var(--wf-c-chip); }

/* ---------- Node settings ---------- */
.ui-wf-set {
  position: absolute;
  /* Full height – nothing floats above it on the right; with the run panel open it ends above it */
  top: 10px;
  right: 10px;
  bottom: calc(10px + var(--run-space, 0px));
  transition: bottom 340ms var(--wf-c-ease);
  z-index: 7;
  display: flex;
  flex-direction: column;
  width: 320px;
  border-radius: calc(var(--aui-radius-lg, 10px) + 2px);
  outline: none;
  background: var(--wf-c-card);
  box-shadow: var(--wf-float);
  animation: ui-wf-in-right 260ms var(--wf-c-ease) both;
}
.ui-wf-set__head { display: flex; flex: none; align-items: center; gap: 2px; height: 52px; padding: 0 8px 0 12px; box-shadow: inset 0 -0.5px 0 var(--wf-c-line); }
.ui-wf-set__head .ui-wf-node__tile { width: 28px; height: 28px; margin-right: 8px; }
.ui-wf-set__title { display: grid; flex: 1; min-width: 0; }
.ui-wf-set__title b { overflow: hidden; font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-strong); }
.ui-wf-set__title small { overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; white-space: nowrap; text-overflow: ellipsis; color: var(--wf-c-muted); }
.ui-wf-set__body { display: flex; flex: 1; flex-direction: column; gap: 16px; min-height: 0; overflow-y: auto; padding: 14px 14px 16px; }
.ui-wf-label { font-weight: 500; line-height: 16px; display: block; font-size: calc(11px * var(--type-scale, 1)); color: var(--wf-c-muted); }
.ui-wf-run__pad .ui-wf-label { margin-bottom: 6px; }
/* Text inputs are the library's Text Field, tinted like the inspector's selects and text areas */
/* Typing in a field: no ring – the caret shows where you are, the hairline only firms up a little */
/* A select is a button: its ring shows for the keyboard only, as the shell's accent ring */
/* Selects: the library's Select, tinted like the text fields next to them */
/* Switches: the library's Switch, small, at the start of their row */
/* Connections in the settings: where each output goes, and Connect to… */
/* Radios: a ring that fills in accent */
/* Chips: gray outlines; picked ones tinted accent with an × */
/* Switch */
/* JSON: mono on a light gray fill */
.ui-wf-code { margin: 0; padding: 10px 12px; overflow: auto; border-radius: calc(var(--aui-radius-lg, 10px) - 2px); background: var(--wf-field); box-shadow: inset 0 0 0 0.5px var(--wf-c-line); font-family: var(--mono); font-size: calc(11.5px * var(--type-scale, 1)); line-height: 18px; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--wf-c-text); user-select: text; }
.ui-wf-code:focus-visible { outline-offset: 2px; }

/* The toast sits above the floating toolbar */

/* Phones: the palette and the settings become sheets from the bottom over a soft scrim */
/* The same scrim as Steve's dialogs and sheets */
.ui-wf-scrim { position: absolute; inset: 0; z-index: 6; background: rgb(0 0 0 / 0.25); animation: ui-wf-fade 200ms ease both; }
:where(.dark, [data-theme="dark"]) .ui-wf-scrim { background: rgb(0 0 0 / 0.55); }
.ui-wf-pal[data-sheet], .ui-wf-set[data-sheet] { top: auto; right: 8px; bottom: 8px; left: 8px; z-index: 7; width: auto; max-height: min(72%, 560px); animation: ui-wf-in-up 300ms var(--wf-c-ease) both; }
.ui-wf-set[data-sheet] { height: min(72%, 560px); }
.ui-wf-pal[data-sheet] .ui-wf-pal__item { height: 48px; }
.ui-wf-pal[data-sheet] .ui-wf-pal__desc { display: block; }
.ui-wf-pal[data-sheet] .ui-wf-pal__icon { width: 30px; height: 30px; border-radius: 8px; background: var(--wf-c-chip); }
.ui-wf[data-narrow] .ui-wf-tb { bottom: 10px; max-width: calc(100% - 16px); }
.ui-wf[data-narrow] :is(.ui-wf-tb__zoom, .ui-wf-tb__zoomstep) { display: none; }
.ui-wf[data-narrow] .ui-wf-picker { right: 8px; left: 8px !important; width: auto; }
.ui-wf[data-narrow] .ui-wf-runs__head { grid-template-columns: 16px 12px minmax(0, max-content) minmax(0, 1fr) 0; }
/* On a phone a step keeps its name readable: the kind badge and the counter step aside, time and cost stay */
.ui-wf[data-narrow] .ui-wf-step__row { grid-template-columns: 16px minmax(0, 1fr) minmax(0, 0.8fr) 52px; }
.ui-wf[data-narrow] .ui-wf-step__row > .ui-wf-step__time { grid-column: 4; }
.ui-wf[data-narrow] .ui-wf-step__row > .ui-wf-count { display: none; }
.ui-wf[data-narrow] :is(.ui-wf-count, .ui-wf-runs__head .ui-wf-step__time) { display: none; }
.ui-wf[data-narrow] .ui-wf-tabs button span:not(.ui-wf-tabs__count) { display: none; }
.ui-wf[data-narrow] .ui-wf-timeline li { grid-template-columns: 96px minmax(0, 1fr) 56px; }

@keyframes ui-wf-fade { from { opacity: 0; } }
@keyframes ui-wf-pop { from { opacity: 0; transform: scale(0.97) translateY(-4px); } }
@keyframes ui-wf-rise { from { opacity: 0; transform: translateY(4px); } }
@keyframes ui-wf-in-left { from { opacity: 0; transform: translateX(-8px); } }
@keyframes ui-wf-in-right { from { opacity: 0; transform: translateX(8px); } }
@keyframes ui-wf-in-up { from { opacity: 0; transform: translateY(16px); } }

/* Reduced motion: no light travelling round the card – a steady accent ring shows the running step instead */
@media (prefers-reduced-motion: reduce) {
  .ui-wf-trace, .ui-wf-flow, .ui-wf-arrive { display: none; }
  .ui-wf-node[data-arrived] .ui-wf-port--in { animation: none; }
  .ui-wf-node[data-run="running"] { box-shadow: 0 0 0 1.5px var(--wf-c-accent); }
  .ui-wf-edge--lit { animation: none; stroke-dashoffset: 0; }
  .ui-wf-run { transition: none; }
}

/* Touch: every control at least 36px, fields at 16px so phones don't zoom in */
@media (pointer: coarse) {
  .ui-wf-tb__btn, .ui-wf-icon-btn { width: 36px; height: 36px; }
  .ui-wf-btn, .ui-wf-tabs button { min-height: 36px; }
  .ui-wf-picker__search { height: 44px; }
  .ui-wf-picker__search input { font-size: calc(16px * var(--type-scale, 1)); }
  .ui-wf-pal__item, .ui-wf-runs__head, .ui-wf-step__row, .ui-wf-list__row { height: 40px; }
  .ui-wf-add { top: 4px; left: -18px; border-width: 10px; }
  .ui-wf-run__grip { height: 14px; }
}
`;
