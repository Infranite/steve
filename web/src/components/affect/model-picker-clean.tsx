/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import {
  memo,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";

/*
 * Model Picker (clean style) – the model menu of an AI chat, in a single self-contained file.
 * Clean style: one white popover with hairline shadows, Inter 13px, a blue accent – no gray tray.
 * A small trigger (glyph, model name, chevron) sits in your composer toolbar; it opens a menu that unfolds softly from it:
 *   · one row per model with three tiny strength bars – Speed, Quality, Cost. Point at a row and its bars fill,
 *     the other rows step back
 *   · Extended thinking and Web search as switches; the Effort control (Low / Medium / High) unfolds under thinking
 *   · the last row of the menu says what the model you point at costs you – first-token time and price –
 *     and its numbers roll when the model or the effort changes
 *   · arrows move, Enter picks, Tab reaches the switches, Esc or a click outside closes
 * The styles are injected below, so no extra files are needed.
 */

export type PickerModel = {
  id: string;
  name: string;
  /** One short line under the name, e.g. "Hardest problems" */
  description?: string;
  /** Glyph in front of the name – any node, sized to 18px. Defaults to the first letter on a soft tile */
  icon?: ReactNode;
  /** Strengths from 1 to 5. Cost 5 is the most expensive. Leave all three out for a router like "Auto" */
  speed?: number;
  quality?: number;
  cost?: number;
  /** Typical time to the first token without extended thinking, in milliseconds */
  latencyMs?: number;
  /** Price per million output tokens, in dollars */
  pricePerM?: number;
};

export type ModelEffort = "low" | "medium" | "high";

export type ModelPickerProps = {
  models: PickerModel[];
  /** Id of the chosen model (controlled) */
  value?: string;
  defaultValue?: string;
  onChange?: (id: string) => void;
  /** Extended thinking switch */
  thinking?: boolean;
  defaultThinking?: boolean;
  onThinkingChange?: (on: boolean) => void;
  /** Web search switch */
  webSearch?: boolean;
  defaultWebSearch?: boolean;
  onWebSearchChange?: (on: boolean) => void;
  /** How hard the model thinks – only shown while extended thinking is on */
  effort?: ModelEffort;
  defaultEffort?: ModelEffort;
  onEffortChange?: (effort: ModelEffort) => void;
  /** Controlled open state of the menu */
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Which edge of the trigger the menu lines up with */
  align?: "start" | "end";
  /** Whether the menu opens below or above the trigger – above suits a composer at the bottom of the screen */
  side?: "bottom" | "top";
  className?: string;
  /** Steve supplies only features backed by its current harness contract. */
  showSettings?: boolean;
  labels?: { model: string; choose: string; settings: string; none: string };
};

/* ---------- Helpers ---------- */

const EFFORTS: { id: ModelEffort; label: string }[] = [
  { id: "low", label: "Low" },
  { id: "medium", label: "Medium" },
  { id: "high", label: "High" },
];
/** How much longer the first token takes while the model thinks first */
const THINK_FACTOR: Record<ModelEffort, number> = { low: 1.6, medium: 2.5, high: 4 };
const STRENGTHS = [
  { key: "speed", label: "Speed" },
  { key: "quality", label: "Quality" },
  { key: "cost", label: "Cost" },
] as const;

const ease = (t: number) => 1 - Math.pow(1 - t, 4);
const reduced = () =>
  typeof window !== "undefined" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const clamp = (n: number) => (Number.isFinite(n) ? Math.max(0, Math.min(5, Math.round(n))) : 0);
const pad = (n: number) => String(n).padStart(2, "0");
/** 2.1s under a minute, then 2m 05s and 1h 04m – a long think never reads 125.0s */
const seconds = (ms: number) => {
  const s = Math.max(0, ms) / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${pad(Math.floor(s % 60))}s`;
  return `${Math.floor(m / 60)}h ${pad(m % 60)}m`;
};
/** $25, $3.75, $12,345.50 – and <$0.01 for a price that would round to nothing */
const dollars = (n: number, target: number) => {
  if (target > 0 && target < 0.01) return "<$0.01";
  const digits = Number.isInteger(target) ? 0 : 2;
  return new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);
};
/** A number the API sent broken (NaN, Infinity) counts as not given */
const finite = (n: number | undefined) => (n !== undefined && Number.isFinite(n) ? n : undefined);
/** The first whole letter or digit – never half an emoji */
const initial = (name: string) => (name.match(/[\p{L}\p{N}]/u)?.[0] ?? "").toUpperCase();

/** Rolls a set of named numbers from where they are to their targets, in one animation frame loop */
function useRolling(target: Record<string, number>, duration: number) {
  const [shown, setShown] = useState(target);
  const current = useRef(target);
  const signature = JSON.stringify(target);
  useEffect(() => {
    const to = JSON.parse(signature) as Record<string, number>;
    const from = { ...current.current };
    if (reduced() || Object.keys(to).every((k) => from[k] === to[k])) {
      current.current = to;
      setShown(to);
      return;
    }
    const start = performance.now();
    let frame = 0;
    const step = (now: number) => {
      const t = Math.min(1, (now - start) / duration);
      const e = ease(t);
      const next: Record<string, number> = {};
      for (const k in to)
        next[k] = (from[k] ?? to[k]) + (to[k] - (from[k] ?? to[k])) * e;
      current.current = next;
      setShown(next);
      if (t < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [signature, duration]);
  return shown;
}

/**
 * Text that changes without a jump: the old line stays for a moment and fades out underneath while the new one fades in.
 * Quick changes just retarget – nothing piles up.
 */
function Swap({ id, children }: { id: string; children: ReactNode }) {
  type Line = { id: string; node: ReactNode };
  const last = useRef<Line>({ id, node: children });
  const [shownId, setShownId] = useState(id);
  const [out, setOut] = useState<Line | null>(null);
  if (shownId !== id) {
    setShownId(id);
    setOut(last.current);
  }
  last.current = { id, node: children };
  useEffect(() => {
    if (!out) return;
    const timer = window.setTimeout(() => setOut(null), 260);
    return () => window.clearTimeout(timer);
  }, [out]);
  return (
    <span className="ui-mdpc__swap">
      {out && (
        <span key={`out-${out.id}`} className="ui-mdpc__swap-item" data-out="" aria-hidden="true">
          {out.node}
        </span>
      )}
      <span key={id} className="ui-mdpc__swap-item" dir="auto">
        {children}
      </span>
    </span>
  );
}

/* ---------- Icons ---------- */

const line = {
  stroke: "currentColor",
  strokeWidth: 1.2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;
const ChevronIcon = () => (
  <svg className="ui-mdpc__chevron" width="14" height="14" viewBox="0 0 12 12" fill="none" aria-hidden="true">
    <path d="m3 4.5 3 3 3-3" {...line} />
  </svg>
);
const CheckIcon = () => (
  <svg className="ui-mdpc__check" width="16" height="16" viewBox="0 0 14 14" fill="none" aria-hidden="true">
    <path d="m3.4 7.3 2.4 2.4 4.8-5.2" {...line} strokeWidth={1.3} />
  </svg>
);
const ThinkIcon = () => (
  <svg width="16" height="16" viewBox="0 0 14 14" fill="none" aria-hidden="true">
    <path d="M5.3 10.6h3.4M5.8 12.3h2.4M7 1.7a3.9 3.9 0 0 0-2.3 7c.4.3.6.7.6 1.1v.1h3.4v-.1c0-.4.2-.8.6-1.1A3.9 3.9 0 0 0 7 1.7Z" {...line} />
  </svg>
);
const GlobeIcon = () => (
  <svg width="16" height="16" viewBox="0 0 14 14" fill="none" aria-hidden="true">
    <circle cx="7" cy="7" r="5.3" {...line} />
    <path d="M1.9 7h10.2M7 1.7c1.4 1.5 2 3.3 2 5.3s-.6 3.8-2 5.3c-1.4-1.5-2-3.3-2-5.3s.6-3.8 2-5.3Z" {...line} />
  </svg>
);

function Glyph({ model }: { model: PickerModel }) {
  return (
    <span className="ui-mdpc__glyph" aria-hidden="true">
      {model.icon ?? initial(model.name)}
    </span>
  );
}

type OptionProps = {
  model: PickerModel;
  index: number;
  optionId: string;
  selected: boolean;
  active: boolean;
  lit: boolean;
  onPoint: (index: number) => void;
  onPick: (index: number) => void;
};

/** One model row – memoized, so pointing at a row in a long list stays quick */
const Option = memo(function Option({ model: m, index: i, optionId, selected, active, lit, onPoint, onPick }: OptionProps) {
  const rated = m.speed !== undefined || m.quality !== undefined || m.cost !== undefined;
  return (
    <div
      id={optionId}
      role="option"
      aria-selected={selected}
      aria-label={`${m.name}${m.description ? `, ${m.description}` : ""}${rated ? `. Speed ${clamp(m.speed ?? 0)}, quality ${clamp(m.quality ?? 0)}, cost ${clamp(m.cost ?? 0)} of 5` : ""}`}
      className="ui-mdpc__option"
      data-active={active ? "" : undefined}
      data-lit={lit ? "" : undefined}
      onPointerEnter={(e) => {
        if (e.pointerType !== "mouse") return;
        onPoint(i);
      }}
      onClick={() => onPick(i)}
    >
      <Glyph model={m} />
      <span className="ui-mdpc__text">
        <span className="ui-mdpc__title">
          <span className="ui-mdpc__title-name" dir="auto" title={m.name}>
            {m.name}
          </span>
          {selected && <CheckIcon />}
        </span>
        {m.description && (
          <span className="ui-mdpc__desc" dir="auto" title={m.description}>
            {m.description}
          </span>
        )}
      </span>
      {rated ? (
        <span className="ui-mdpc__bars" aria-hidden="true">
          {STRENGTHS.map((s, c) => {
            const level = clamp(m[s.key] ?? 0);
            return (
              <span key={s.key} className="ui-mdpc__meter" style={{ ["--c" as string]: c }}>
                {[0, 1, 2, 3, 4].map((k) => (
                  <span key={k} className="ui-mdpc__seg" data-on={k < level ? "" : undefined} style={{ ["--s" as string]: k }} />
                ))}
              </span>
            );
          })}
        </span>
      ) : (
        <span className="ui-mdpc__varies" aria-hidden="true">
          Varies
        </span>
      )}
    </div>
  );
});

export function ModelPicker({
  models,
  value,
  defaultValue,
  onChange,
  thinking,
  defaultThinking = false,
  onThinkingChange,
  webSearch,
  defaultWebSearch = false,
  onWebSearchChange,
  effort,
  defaultEffort = "medium",
  onEffortChange,
  open,
  defaultOpen = false,
  onOpenChange,
  align = "start",
  side = "bottom",
  className,
  showSettings = true,
  labels = { model: "Model", choose: "Choose a model", settings: "Model settings", none: "No models available" },
}: ModelPickerProps) {
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Every value works on its own; pass the value and its callback to keep the truth outside
  const [innerValue, setInnerValue] = useState(defaultValue ?? models[0]?.id);
  const [innerThinking, setInnerThinking] = useState(defaultThinking);
  const [innerSearch, setInnerSearch] = useState(defaultWebSearch);
  const [innerEffort, setInnerEffort] = useState<ModelEffort>(defaultEffort);
  const [innerOpen, setInnerOpen] = useState(defaultOpen);
  const chosenId = value ?? innerValue;
  const isThinking = thinking ?? innerThinking;
  const isSearching = webSearch ?? innerSearch;
  const effortNow = effort ?? innerEffort;
  const isOpen = open ?? innerOpen;

  const chosenIndex = models.findIndex((m) => m.id === chosenId);
  const chosen = models[chosenIndex];

  /** The row the arrows (or the pointer) are on, and whether someone is actually pointing at a row */
  const [active, setActive] = useState(Math.max(0, chosenIndex));
  const [pointing, setPointing] = useState(false);
  /** A switch under the pointer or the focus – the last row then talks about it */
  const [hint, setHint] = useState<"search" | null>(null);

  const lit = models[pointing ? active : chosenIndex] ?? chosen;

  /* ---------- Opening and closing ---------- */

  const setOpen = (next: boolean, focus?: "list" | "trigger") => {
    if (next !== isOpen) {
      if (next) {
        setActive(Math.max(0, chosenIndex));
        setPointing(false);
      } else setHint(null);
      setInnerOpen(next);
      onOpenChange?.(next);
    }
    // Focus only moves for people – a parent opening the menu through `open` leaves the focus where it is
    if (focus === "list") requestAnimationFrame(() => listRef.current?.focus({ preventScroll: true }));
    if (focus === "trigger") triggerRef.current?.focus({ preventScroll: true });
  };
  // The parent may open the menu itself – start on the chosen row
  const wasOpen = useRef(isOpen);
  useEffect(() => {
    if (isOpen && !wasOpen.current) {
      setActive(Math.max(0, chosenIndex));
      setPointing(false);
    }
    wasOpen.current = isOpen;
  }, [isOpen, chosenIndex]);

  // Near the edge of the screen the menu slides in just enough to stay whole
  const popRef = useRef<HTMLDivElement>(null);
  const [shift, setShift] = useState(0);
  useLayoutEffect(() => {
    if (!isOpen) return;
    const measure = () => {
      const trigger = triggerRef.current?.getBoundingClientRect();
      const width = popRef.current?.offsetWidth ?? 0;
      if (!trigger || !width) return;
      const room = document.documentElement.clientWidth;
      const left = align === "end" ? trigger.right - width : trigger.left;
      const margin = 12;
      setShift(
        left < margin
          ? margin - left
          : left + width > room - margin
            ? Math.min(0, room - margin - (left + width))
            : 0,
      );
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [isOpen, align]);

  // While open, a press outside closes it
  useEffect(() => {
    if (!isOpen) return;
    const down = (e: globalThis.PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", down);
    return () => document.removeEventListener("pointerdown", down);
  }); // eslint-disable-line react-hooks/exhaustive-deps

  const focusInside = () => rootRef.current?.contains(document.activeElement) ?? false;

  /* ---------- Changing things ---------- */

  const pick = (index: number) => {
    const m = models[index];
    if (!m) return;
    setInnerValue(m.id);
    if (m.id !== chosenId) onChange?.(m.id);
    setOpen(false, focusInside() ? "trigger" : undefined);
  };
  const toggleThinking = () => {
    setInnerThinking(!isThinking);
    onThinkingChange?.(!isThinking);
  };
  const toggleSearch = () => {
    setInnerSearch(!isSearching);
    onWebSearchChange?.(!isSearching);
  };
  const chooseEffort = (next: ModelEffort) => {
    setInnerEffort(next);
    if (next !== effortNow) onEffortChange?.(next);
  };

  // The rows get stable handlers, so pointing at one row re-renders only the rows that change
  const latest = useRef({ point: (_i: number) => {}, pick: (_i: number) => {} });
  latest.current.point = (i: number) => {
    setActive(i);
    setPointing(true);
  };
  latest.current.pick = (i: number) => pick(i);
  const onPoint = useCallback((i: number) => latest.current.point(i), []);
  const onPick = useCallback((i: number) => latest.current.pick(i), []);

  // A long list scrolls – the row the arrows are on stays in view
  useEffect(() => {
    const list = listRef.current;
    const row = list?.children[active] as HTMLElement | undefined;
    if (!isOpen || !list || !row) return;
    if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop;
    else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight)
      list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight;
  }, [active, isOpen]);

  /* ---------- Keyboard ---------- */

  const onTriggerKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setOpen(true, "list");
    }
  };
  const onListKey = (e: KeyboardEvent) => {
    const n = models.length;
    if (!n) return;
    const move = (to: number) => {
      e.preventDefault();
      setActive(((to % n) + n) % n);
      setPointing(true);
    };
    if (e.key === "ArrowDown") move(active + 1);
    else if (e.key === "ArrowUp") move(active - 1);
    else if (e.key === "Home") move(0);
    else if (e.key === "End") move(n - 1);
    else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      pick(active);
    }
  };
  // The fill sits under the chosen level – the levels are as wide as their words, so it is measured
  const segRef = useRef<HTMLDivElement>(null);
  const [thumb, setThumb] = useState<{ x: number; w: number } | null>(null);
  useLayoutEffect(() => {
    const btn = segRef.current?.querySelector<HTMLElement>(`[data-effort="${effortNow}"]`);
    if (btn) setThumb({ x: btn.offsetLeft, w: btn.offsetWidth });
  }, [effortNow, isOpen, isThinking]);
  const onEffortKey = (e: KeyboardEvent) => {
    const i = EFFORTS.findIndex((x) => x.id === effortNow);
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const next = EFFORTS[(i + step + EFFORTS.length) % EFFORTS.length];
    chooseEffort(next.id);
    rootRef.current?.querySelector<HTMLElement>(`[data-effort="${next.id}"]`)?.focus();
  };
  const onRootKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape" || !isOpen) return;
    e.stopPropagation();
    setOpen(false, "trigger");
  };

  /* ---------- What the last row says ---------- */

  const factor = isThinking ? THINK_FACTOR[effortNow] : 1;
  const litLatency = finite(lit?.latencyMs);
  const litPrice = finite(lit?.pricePerM);
  const latency = (litLatency ?? 0) * factor;
  const price = litPrice ?? 0;
  const rolled = useRolling({ latency, price }, 560);
  const hasStats = litLatency !== undefined || litPrice !== undefined;

  let caption: ReactNode;
  let captionKey: string;
  let spoken: string;
  if (hint === "search") {
    caption = isSearching ? "Looks things up on the web when it helps" : "Answers from what it already knows";
    captionKey = `search-${isSearching}`;
    spoken = caption as string;
  } else if (!hasStats) {
    caption = lit?.description ?? lit?.name ?? "";
    captionKey = `plain-${lit?.id}`;
    spoken = `${lit?.name ?? ""}: ${caption as string}`;
  } else {
    const parts: ReactNode[] = [];
    const words: string[] = [];
    if (litLatency !== undefined) {
      parts.push(<span key="l">~<b>{seconds(rolled.latency ?? latency)}</b> first token</span>);
      words.push(`about ${seconds(latency)} to the first token`);
    }
    if (litPrice !== undefined) {
      parts.push(<span key="p"><b>{dollars(rolled.price ?? price, price)}</b> / M tokens</span>);
      words.push(`${dollars(price, price)} per million tokens`);
    }
    caption = parts.flatMap((p, i) => (i ? [<span key={`d${i}`} className="ui-mdpc__dot"> · </span>, p] : [p]));
    // One key for every model with numbers – the numbers roll instead of the line swapping
    captionKey = "stats";
    spoken = `${lit.name}: ${words.join(", ")}`;
  }

  // Ids by position – a model id may hold spaces or slashes, which an element id can't
  const activeId = models[active] ? `${uid}-opt-${active}` : undefined;

  return (
    <div
      ref={rootRef}
      className={["ui-mdpc", className].filter(Boolean).join(" ")}
      data-open={isOpen ? "" : undefined}
      data-align={align}
      data-side={side}
      onKeyDown={onRootKey}
      onBlur={(e) => {
        if (isOpen && e.relatedTarget && !rootRef.current?.contains(e.relatedTarget as Node)) setOpen(false);
      }}
    >
      <style href="ui-model-picker-clean" precedence="default">
        {css}
      </style>

      <button
        ref={triggerRef}
        type="button"
        className="ui-mdpc__trigger"
        aria-haspopup="dialog"
        aria-expanded={isOpen}
        aria-controls={`${uid}-menu`}
        aria-label={`${labels.model}: ${chosen?.name ?? labels.choose}`}
        onClick={() => setOpen(!isOpen, isOpen ? undefined : "list")}
        onKeyDown={onTriggerKey}
      >
        {chosen && <Glyph model={chosen} />}
        <span className="ui-mdpc__name" title={chosen?.name}>
          <Swap id={chosen?.id ?? "none"}>{chosen?.name ?? labels.choose}</Swap>
        </span>
        {/* A small bulb unfolds next to the name while extended thinking is on */}
        <span className="ui-mdpc__flag" data-on={isThinking ? "" : undefined} aria-hidden="true">
          <span>
            <ThinkIcon />
          </span>
        </span>
        <ChevronIcon />
      </button>

      <div
        id={`${uid}-menu`}
        ref={popRef}
        className="ui-mdpc__pop"
        style={shift ? { translate: `${shift}px 0` } : undefined}
        role="dialog"
        aria-label={labels.settings}
        inert={!isOpen}
      >
        <div className="ui-mdpc__card">
          <div className="ui-mdpc__head" aria-hidden="true">
            <span>{labels.model}</span>
            {models.some((m) => m.speed != null || m.quality != null || m.cost != null) && <span className="ui-mdpc__cols">
              {STRENGTHS.map((s) => (
                <span key={s.key}>{s.label}</span>
              ))}
            </span>}
          </div>

          {models.length === 0 && <div className="ui-mdpc__empty">{labels.none}</div>}
          <div
            ref={listRef}
            className="ui-mdpc__list"
            role="listbox"
            aria-label={labels.model}
            tabIndex={0}
            aria-activedescendant={isOpen ? activeId : undefined}
            data-pointing={pointing ? "" : undefined}
            onKeyDown={onListKey}
            onPointerLeave={() => setPointing(false)}
          >
            {models.map((m, i) => (
              <Option
                key={m.id}
                model={m}
                index={i}
                optionId={`${uid}-opt-${i}`}
                selected={i === chosenIndex}
                active={i === active}
                lit={m.id === lit?.id}
                onPoint={onPoint}
                onPick={onPick}
              />
            ))}
          </div>

          {showSettings && <div className="ui-mdpc__settings">
            <button
              type="button"
              role="switch"
              aria-checked={isThinking}
              className="ui-mdpc__toggle"
              onClick={toggleThinking}
            >
              <span className="ui-mdpc__toggle-icon">
                <ThinkIcon />
              </span>
              <span className="ui-mdpc__toggle-label">Extended thinking</span>
              <span className="ui-mdpc__switch" aria-hidden="true" />
            </button>

            {/* Effort unfolds under thinking, in place */}
            <div className="ui-mdpc__fold" data-open={isThinking ? "" : undefined} inert={!isThinking}>
              <div className="ui-mdpc__clip">
                <div className="ui-mdpc__effort">
                  <span className="ui-mdpc__effort-label" id={`${uid}-effort`}>
                    Effort
                  </span>
                  <div
                    ref={segRef}
                    className="ui-mdpc__segmented"
                    role="radiogroup"
                    aria-labelledby={`${uid}-effort`}
                    style={thumb ? { ["--x" as string]: `${thumb.x}px`, ["--w" as string]: `${thumb.w}px` } : undefined}
                    onKeyDown={onEffortKey}
                  >
                    <span className="ui-mdpc__thumb" aria-hidden="true" />
                    {EFFORTS.map((x) => (
                      <button
                        key={x.id}
                        type="button"
                        role="radio"
                        aria-checked={x.id === effortNow}
                        tabIndex={x.id === effortNow ? 0 : -1}
                        data-effort={x.id}
                        className="ui-mdpc__seg-btn"
                        onClick={() => chooseEffort(x.id)}
                      >
                        {x.label}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            </div>

            <button
              type="button"
              role="switch"
              aria-checked={isSearching}
              className="ui-mdpc__toggle"
              onClick={toggleSearch}
              onPointerEnter={(e) => e.pointerType === "mouse" && setHint("search")}
              onPointerLeave={() => setHint(null)}
              onFocus={() => setHint("search")}
              onBlur={() => setHint(null)}
            >
              <span className="ui-mdpc__toggle-icon">
                <GlobeIcon />
              </span>
              <span className="ui-mdpc__toggle-label">Web search</span>
              <span className="ui-mdpc__switch" aria-hidden="true" />
            </button>
          </div>}
        </div>

        {/* The last row: what the model you point at costs you */}
        <div className="ui-mdpc__foot">
          <span className="ui-mdpc__caption" aria-hidden="true">
            <Swap id={captionKey}>{caption}</Swap>
          </span>
          <span className="ui-mdpc__keys" aria-hidden="true">
            <kbd>↑↓</kbd>
            <kbd>↵</kbd>
          </span>
          <span className="ui-mdpc__sr" aria-live="polite">
            {isOpen ? spoken : ""}
          </span>
        </div>
      </div>
    </div>
  );
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-mdpc */
.ui-mdpc {
  --ui-mdpc-card: #ffffff;
  --ui-mdpc-strong: var(--aui-ink, #000000);
  --ui-mdpc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-mdpc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-mdpc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-mdpc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-mdpc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-mdpc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-mdpc-line-strong: rgba(0, 0, 0, 0.1);
  --ui-mdpc-fill: rgba(0, 0, 0, 0.06);
  --ui-mdpc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-mdpc-bar: rgba(0, 0, 0, 0.2);
  --ui-mdpc-bar-off: rgba(0, 0, 0, 0.06);
  --ui-mdpc-switch: rgba(0, 0, 0, 0.1);
  --ui-mdpc-knob: #ffffff;
  --ui-mdpc-accent: rgba(0, 96, 226, 0.765);
  --ui-mdpc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-mdpc-shadow-tile: 0 0 0 0.5px rgba(0, 0, 0, 0.1), 0 1px 1px -1px rgba(0, 0, 0, 0.1);
  --ui-mdpc-shadow-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 12px 28px -10px rgba(0, 0, 0, 0.16), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-mdpc-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  position: relative;
  display: inline-block;
  max-width: 100%;
  min-width: 0;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-mdpc-ink);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-mdpc *, .ui-mdpc *::before, .ui-mdpc *::after { box-sizing: border-box; }
:where(.ui-mdpc) button { appearance: none; margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-mdpc button:focus-visible { outline: 2px solid var(--ui-mdpc-focus); outline-offset: 2px; }
.ui-mdpc svg { display: block; flex: none; }
.ui-mdpc b { font-weight: 500; color: var(--ui-mdpc-ink); }
.ui-mdpc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* ----- Trigger: a quiet button – glyph, name and a small chevron ----- */
.ui-mdpc__trigger { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; height: 28px; padding: 0 6px 0 6px; border-radius: calc(var(--aui-radius, 7px) - 1px); font-weight: 500; color: var(--ui-mdpc-text); white-space: nowrap; transition: background-color 160ms ease, color 160ms ease; }
.ui-mdpc__trigger:hover, .ui-mdpc[data-open] .ui-mdpc__trigger { background: var(--ui-mdpc-wash); color: var(--ui-mdpc-ink); }
.ui-mdpc__trigger .ui-mdpc__glyph { width: 18px; height: 18px; border-radius: 5px; font-size: 10px; }
.ui-mdpc__trigger .ui-mdpc__glyph > svg { width: 10px; height: 10px; }
.ui-mdpc__name { display: grid; min-width: 0; }
.ui-mdpc__chevron { margin-left: -2px; color: var(--ui-mdpc-icon); transition: transform 300ms var(--ui-mdpc-ease); }
.ui-mdpc[data-open] .ui-mdpc__chevron { transform: rotate(180deg); }
.ui-mdpc__flag { display: grid; grid-template-columns: 0fr; margin-left: -6px; opacity: 0; color: var(--ui-mdpc-accent); transition: grid-template-columns 360ms var(--ui-mdpc-ease), margin 360ms var(--ui-mdpc-ease), opacity 220ms ease; }
.ui-mdpc__flag > span { min-width: 0; overflow: hidden; }
.ui-mdpc__flag[data-on] { grid-template-columns: 1fr; margin-left: -2px; opacity: 1; transition: grid-template-columns 360ms var(--ui-mdpc-ease), margin 360ms var(--ui-mdpc-ease), opacity 280ms ease 80ms; }

/* Glyph: a small white tile with a hairline */
.ui-mdpc__glyph { display: grid; flex: none; place-items: center; width: 20px; height: 20px; overflow: hidden; border-radius: 5px; background: var(--ui-mdpc-card); box-shadow: var(--ui-mdpc-shadow-tile); font-size: 10px; font-weight: 500; line-height: 1; color: var(--ui-mdpc-strong); }
.ui-mdpc__glyph > svg, .ui-mdpc__glyph > img { width: 12px; height: 12px; }

/* ----- Menu: one white popover, unfolds softly from the trigger ----- */
.ui-mdpc__pop {
  position: absolute;
  z-index: 30;
  top: calc(100% + 6px);
  left: 0;
  width: min(320px, calc(100vw - 32px));
  container-type: inline-size;
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--aui-overlay, var(--ui-mdpc-card));
  box-shadow: var(--ui-mdpc-shadow-pop);
  transform-origin: top left;
  opacity: 0;
  visibility: hidden;
  transform: translateY(-4px) scale(0.96);
  transition: opacity 160ms ease, transform 220ms var(--ui-mdpc-ease), visibility 0s linear 220ms;
}
.ui-mdpc[data-align="end"] .ui-mdpc__pop { left: auto; right: 0; transform-origin: top right; }
.ui-mdpc[data-side="top"] .ui-mdpc__pop { top: auto; bottom: calc(100% + 6px); transform-origin: bottom left; transform: translateY(4px) scale(0.96); }
.ui-mdpc[data-side="top"][data-align="end"] .ui-mdpc__pop { transform-origin: bottom right; }
.ui-mdpc[data-open] .ui-mdpc__pop { opacity: 1; visibility: visible; transform: none; transition: opacity 200ms ease, transform 320ms var(--ui-mdpc-ease), visibility 0s; }

.ui-mdpc__card { padding: 4px; }
.ui-mdpc__head { display: flex; align-items: center; justify-content: space-between; gap: 12px; height: 28px; padding: 4px 8px 0; font-size: 10px; font-weight: 500; line-height: 16px; letter-spacing: 0.5px; text-transform: uppercase; color: var(--ui-mdpc-muted); }
.ui-mdpc__cols { display: grid; grid-template-columns: repeat(3, 40px); gap: 8px; }
.ui-mdpc__cols > span { display: flex; justify-content: center; white-space: nowrap; }

/* ----- Rows ----- */
.ui-mdpc__list { display: grid; gap: 0; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); outline: none; position: relative; max-height: min(360px, 50vh); overflow-y: auto; overscroll-behavior: contain; }
/* No models: one quiet line instead of an empty menu */
.ui-mdpc__empty { padding: 14px 8px; font-size: 12px; line-height: 16px; color: var(--ui-mdpc-muted); text-align: center; }
.ui-mdpc__option { display: flex; align-items: center; gap: 10px; min-height: 44px; padding: 6px 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); cursor: pointer; transition: background-color 90ms ease-out, opacity 200ms ease; }
/* The fill appears crisply on the row under the pointer and fades out when the pointer leaves the list */
.ui-mdpc__list[data-pointing] .ui-mdpc__option[data-active] { background: var(--ui-mdpc-fill); }
.ui-mdpc__list:focus-visible { outline: none; }
.ui-mdpc__list:focus-visible .ui-mdpc__option[data-active] { background: var(--ui-mdpc-fill); outline: 2px solid var(--ui-mdpc-focus); outline-offset: -2px; }
.ui-mdpc__list[data-pointing] .ui-mdpc__option:not([data-active]) { opacity: 0.55; }
.ui-mdpc__text { display: grid; flex: 1; min-width: 0; }
.ui-mdpc__title { display: flex; align-items: center; gap: 4px; min-width: 0; font-weight: 500; color: var(--ui-mdpc-strong); }
.ui-mdpc__title-name { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-mdpc__check { color: var(--ui-mdpc-accent); animation: ui-mdpc-pop 320ms var(--ui-mdpc-ease) both; }
.ui-mdpc__desc { overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-mdpc-text); white-space: nowrap; text-overflow: ellipsis; }

/* Strength bars: five small segments each. The lit row fills them one after another, in the accent */
.ui-mdpc__bars { display: grid; flex: none; grid-template-columns: repeat(3, 40px); gap: 8px; }
.ui-mdpc__meter { display: grid; grid-template-columns: repeat(5, 1fr); gap: 2px; height: 4px; }
.ui-mdpc__seg { border-radius: 1px; background: var(--ui-mdpc-bar-off); transition: background-color 180ms ease, transform 260ms var(--ui-mdpc-ease); }
.ui-mdpc__seg[data-on] { background: var(--ui-mdpc-bar); }
.ui-mdpc__option[data-lit] .ui-mdpc__seg[data-on] { background: var(--ui-mdpc-accent); transition-delay: calc(var(--c) * 70ms + var(--s) * 45ms); animation: ui-mdpc-fill 360ms var(--ui-mdpc-ease) both; animation-delay: calc(var(--c) * 70ms + var(--s) * 45ms); }
.ui-mdpc__varies { flex: none; width: 136px; font-size: 12px; line-height: 16px; color: var(--ui-mdpc-muted); text-align: center; }

/* ----- Switches and effort ----- */
.ui-mdpc__settings { margin: 4px -4px 0; padding: 4px 4px 0; border-top: 0.5px solid var(--ui-mdpc-line); }
.ui-mdpc__toggle { display: flex; align-items: center; gap: 10px; width: 100%; height: 32px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-mdpc-ink); transition: background-color 90ms ease-out; }
.ui-mdpc__toggle:hover { background: var(--ui-mdpc-fill); }
.ui-mdpc__toggle-icon { display: grid; flex: none; place-items: center; width: 20px; color: var(--ui-mdpc-icon); transition: color 200ms ease; }
.ui-mdpc__toggle[aria-checked="true"] .ui-mdpc__toggle-icon { color: var(--ui-mdpc-ink); }
.ui-mdpc__toggle-label { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
/* Switch: a soft square track – no full pill – that turns blue when on */
.ui-mdpc__switch { position: relative; flex: none; width: 26px; height: 16px; border-radius: 5px; background: var(--ui-mdpc-switch); transition: background-color 220ms ease; }
.ui-mdpc__switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 12px; height: 12px; border-radius: 3.5px; background: var(--ui-mdpc-knob); box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 2px rgba(0, 0, 0, 0.16); transition: transform 280ms var(--ui-mdpc-ease); }
.ui-mdpc__toggle[aria-checked="true"] .ui-mdpc__switch { background: var(--ui-mdpc-accent); }
.ui-mdpc__toggle[aria-checked="true"] .ui-mdpc__switch::after { transform: translateX(10px); }

.ui-mdpc__fold { display: grid; grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 420ms var(--ui-mdpc-ease), opacity 220ms ease; }
.ui-mdpc__fold[data-open] { grid-template-rows: 1fr; opacity: 1; transition: grid-template-rows 420ms var(--ui-mdpc-ease), opacity 320ms ease 100ms; }
.ui-mdpc__clip { min-height: 0; overflow: hidden; }
/* Effort: the label lines up with the switch labels, the levels are tab buttons with a fill that slides */
.ui-mdpc__effort { display: flex; align-items: center; gap: 10px; padding: 2px 8px 6px 38px; }
.ui-mdpc__effort-label { flex: 1; min-width: 0; color: var(--ui-mdpc-text); }
/* Each level is as wide as its word; the fill is measured and glides from one to the next */
.ui-mdpc__segmented { position: relative; display: flex; flex: none; gap: 4px; }
.ui-mdpc__thumb { position: absolute; top: 0; bottom: 0; left: 0; width: var(--w, 0); border-radius: var(--aui-radius-sm, 5px); background: var(--ui-mdpc-fill); transform: translateX(var(--x, 0)); transition: transform 220ms var(--ui-mdpc-ease), width 220ms var(--ui-mdpc-ease); }
.ui-mdpc__seg-btn { position: relative; height: 22px; padding: 0 7px; border-radius: var(--aui-radius-sm, 5px); box-shadow: inset 0 0 0 0.5px var(--ui-mdpc-line); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-mdpc-text); text-align: center !important; transition: color 160ms ease, background-color 160ms ease; }
.ui-mdpc__seg-btn[aria-checked="true"] { color: var(--ui-mdpc-ink); }
.ui-mdpc__seg-btn[aria-checked="false"]:hover { background: var(--ui-mdpc-wash); color: var(--ui-mdpc-ink); }

/* ----- Last row: what the model you point at costs you ----- */
.ui-mdpc__foot { display: flex; align-items: center; gap: 8px; min-height: 36px; padding: 6px 8px 6px 12px; border-top: 0.5px solid var(--ui-mdpc-line); }
.ui-mdpc__caption { flex: 1; min-width: 0; overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-mdpc-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-mdpc__dot { color: var(--ui-mdpc-faint); }
.ui-mdpc__swap { display: grid; min-width: 0; }
.ui-mdpc__swap-item { grid-area: 1 / 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; animation: ui-mdpc-swap-in 320ms ease both; }
.ui-mdpc__swap-item[data-out] { animation: ui-mdpc-swap-out 220ms ease both; }
/* The caption may take two lines – a price or a long description is never cut off on one */
.ui-mdpc__caption .ui-mdpc__swap-item { display: -webkit-box; white-space: normal; text-overflow: clip; overflow-wrap: anywhere; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
.ui-mdpc__keys { display: flex; flex: none; gap: 2px; }
.ui-mdpc__keys kbd { display: grid; place-items: center; min-width: 20px; height: 20px; padding: 0 5px; border-radius: var(--aui-radius-sm, 5px); box-shadow: inset 0 0 0 0.5px var(--ui-mdpc-line-strong); font-family: inherit; font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-mdpc-muted); }

@keyframes ui-mdpc-swap-in { from { opacity: 0; filter: blur(2px); } }
@keyframes ui-mdpc-swap-out { from { opacity: 1; } to { opacity: 0; filter: blur(2px); } }
@keyframes ui-mdpc-pop { from { opacity: 0; transform: scale(0.6); } }
@keyframes ui-mdpc-fill { 0% { transform: scaleY(1); } 45% { transform: scaleY(1.6); } 100% { transform: scaleY(1); } }

/* Narrow menus: the glyphs in the rows and the key hints give way, effort moves left */
@container (max-width: 300px) {
  .ui-mdpc__option .ui-mdpc__glyph { display: none; }
  .ui-mdpc__keys { display: none; }
  .ui-mdpc__effort { padding-left: 8px; }
}

@media (pointer: coarse) {
  .ui-mdpc__trigger { height: 34px; }
  .ui-mdpc__option { min-height: 52px; }
  .ui-mdpc__toggle { height: 40px; }
  .ui-mdpc__seg-btn { height: 30px; padding: 0 10px; }
  .ui-mdpc__keys { display: none; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-mdpc *, .ui-mdpc *::before, .ui-mdpc *::after { animation: none !important; transition: none !important; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-mdpc {
  --ui-mdpc-card: #1c1c1e;
  --ui-mdpc-strong: var(--aui-ink, #ffffff);
  --ui-mdpc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-mdpc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-mdpc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-mdpc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-mdpc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-mdpc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-mdpc-line-strong: rgba(255, 255, 255, 0.12);
  --ui-mdpc-fill: rgba(255, 255, 255, 0.08);
  --ui-mdpc-wash: var(--aui-subtle, rgba(255, 255, 255, 0.04));
  --ui-mdpc-bar: rgba(255, 255, 255, 0.24);
  --ui-mdpc-bar-off: rgba(255, 255, 255, 0.08);
  --ui-mdpc-switch: rgba(255, 255, 255, 0.14);
  --ui-mdpc-knob: #ffffff;
  --ui-mdpc-accent: var(--aui-blue, #4fb0fe);
  --ui-mdpc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-mdpc-shadow-tile: 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 1px 2px rgba(0, 0, 0, 0.4);
  --ui-mdpc-shadow-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 12px 28px -10px rgba(0, 0, 0, 0.6), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
}
:where(.dark, [data-theme="dark"]) .ui-mdpc__glyph { background: #2a2a2d; }
`;
