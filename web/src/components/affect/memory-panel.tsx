/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import {
  memo,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { t, useT } from "@/i18n";
import { monthDay } from "@/i18n/format";
import { reducedMotion } from "@/lib/motion";

/*
 * Memory Panel (Clean) – what an AI assistant remembers about someone, and a calm place to change it, in one self-contained file.
 * Clean style: no tray – one white card with hairline shadows. Head, filters, the list, New memory and the status line
 * are rows of that card; quiet gray type, thin 16px icons, states as soft tinted fills.
 * Memories are short facts grouped by category, each with where it came from.
 *   · a master switch turns memory off: the list dims and a short note unfolds under the head
 *   · category chips (and a small search) filter the list – rows fold away instead of jumping
 *   · hover a memory for its actions: edit the text in place, pause it (it stays, but isn't used) or forget it
 *   · a forgotten memory folds away and the last row of the card offers Undo for a few seconds
 *   · "New memory" unfolds into an inline editor with a category picker
 *   · the last row counts memories and paused ones with rolling numbers and says what you are pointing at
 * The styles are injected below, so no extra files are needed.
 */

export type Memory = {
  id: string;
  /** The remembered fact, one short sentence */
  text: string;
  /** Group of the memory, e.g. "Work", "Preferences", "Projects" */
  category: string;
  /** Where it came from, ready to show, e.g. "From chat" */
  source?: string;
  /** When it was learned, ready to show, e.g. "Oct 12" */
  date?: string;
  /** Kept, but not used in answers */
  paused?: boolean;
  /* Steve: */
  /** The source as links (what it is about, where it came from); `source` stays its plain text */
  sourceNode?: ReactNode;
  /** A quiet tag after the source, e.g. "Waiting for you" */
  tag?: string;
  /** false: no pause button (e.g. a candidate that is confirmed elsewhere) */
  canPause?: boolean;
};

/** Steve: every word the panel says, translated by the caller; English when left out */
export type PanelText = {
  paused: string; cancel: string; save: string; edit: string; pause: string; resume: string; forget: string; undo: string;
  editAria: string; editOf: (text: string) => string; pauseOf: (text: string) => string; resumeOf: (text: string) => string; forgetOf: (text: string) => string;
  memories: (n: number) => string; pausedN: (n: number) => string; forgot: [string, string];
  editing: string; adding: string; other: string; used: string; notUsed: string; memoryOff: string; off: string;
  useMemory: string; offNote: string; category: string; all: string; search: string; closeSearch: string;
  none: string; noneFor: (q: string) => string; noneHere: string; newMemory: string; newPlaceholder: string; newCategory: string; general: string;
};
/* Steve: the default words come from Steve's i18n, built when shown so they follow the language */
const TEXT = (): PanelText => ({
  paused: t("core:affect.memory.paused"),
  cancel: t("core:affect.memory.cancel"),
  save: t("core:affect.memory.save"),
  edit: t("core:affect.memory.edit"),
  pause: t("core:affect.memory.pause"),
  resume: t("core:affect.memory.resume"),
  forget: t("core:affect.memory.forget"),
  undo: t("core:affect.memory.undo"),
  editAria: t("core:affect.memory.editAria"),
  editing: t("core:affect.memory.editing"),
  adding: t("core:affect.memory.adding"),
  other: t("core:affect.memory.other"),
  used: t("core:affect.memory.used"),
  notUsed: t("core:affect.memory.notUsed"),
  memoryOff: t("core:affect.memory.memoryOff"),
  off: t("core:affect.memory.off"),
  useMemory: t("core:affect.memory.useMemory"),
  offNote: t("core:affect.memory.offNote"),
  category: t("core:affect.memory.category"),
  all: t("core:affect.memory.all"),
  search: t("core:affect.memory.search"),
  closeSearch: t("core:affect.memory.closeSearch"),
  none: t("core:affect.memory.none"),
  noneHere: t("core:affect.memory.noneHere"),
  newMemory: t("core:affect.memory.newMemory"),
  newPlaceholder: t("core:affect.memory.newPlaceholder"),
  newCategory: t("core:affect.memory.newCategory"),
  general: t("core:affect.memory.general"),
  editOf: (x) => t("core:affect.memory.editOf", { x }), pauseOf: (x) => t("core:affect.memory.pauseOf", { x }), resumeOf: (x) => t("core:affect.memory.resumeOf", { x }), forgetOf: (x) => t("core:affect.memory.forgetOf", { x }),
  memories: (n) => t("core:affect.memory.memories", { count: n }), pausedN: (n) => t("core:affect.memory.pausedN", { n }), forgot: [t("core:affect.memory.forgotBefore"), t("core:affect.memory.forgotAfter")],
  noneFor: (q) => t("core:affect.memory.noneFor", { q }),
});

export type MemoryPanelProps = {
  memories: Memory[];
  /** Called with the full list after any change. With it the panel shows exactly the memories you pass; without it the changes live inside the panel */
  onChange?: (memories: Memory[]) => void;
  /** Granular callbacks – handy to call your API. They fire in both modes */
  onAdd?: (memory: Memory) => void;
  onEdit?: (id: string, text: string) => void;
  onPause?: (id: string, paused: boolean) => void;
  onForget?: (id: string) => void;
  /** Called when a forgotten memory is brought back with Undo */
  onRestore?: (memory: Memory) => void;
  /** The master switch – controlled when given */
  enabled?: boolean;
  defaultEnabled?: boolean;
  onEnabledChange?: (enabled: boolean) => void;
  /** Order of the groups and the choices for new memories – defaults to the categories in the order they appear */
  categories?: string[];
  title?: string;
  /** Height of the list before it scrolls, in px */
  maxHeight?: number;
  /** How long Undo stays in the last row, in ms */
  undoFor?: number;
  /** Source and date given to memories added here */
  addedSource?: string;
  /** A button in the last row, e.g. { label: "Manage", onClick } – Undo takes its place for a moment */
  action?: { label: string; onClick: () => void };
  className?: string;
  /* Steve: */
  text?: Partial<PanelText>;
  /** false: no "Use memory" switch (the product has none) */
  showSwitch?: boolean;
  /** false: the title only labels the panel for screen readers (the page already shows it); the head goes when there is no switch either */
  showTitle?: boolean;
  /** false: no search of its own (the page has one) */
  searchable?: boolean;
  /** More on a memory's meta line, after its source – e.g. pickers */
  renderMeta?: (m: Memory) => ReactNode;
  /** A memory to mark, e.g. one a link pointed at */
  focus?: string | null;
};

/* ---------- Helpers ---------- */

const ease = (t: number) => 1 - Math.pow(1 - t, 4);
const reduced = () => reducedMotion();
const wait = (ms: number) =>
  new Promise<void>((resolve) => window.setTimeout(resolve, ms));
const today = () => monthDay(Date.now());
/** Counts with separators, e.g. 1,284 */
const num = new Intl.NumberFormat("en");
const newId = () => `m-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
/** Textareas grow with their text */
const grow = (el: HTMLTextAreaElement | null) => {
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
};

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

/** True while a sideways-scrolling row has more content hidden past its right edge */
function useMore(ref: RefObject<HTMLElement | null>) {
  const [more, setMore] = useState(false);
  useEffect(() => {
    const row = ref.current;
    if (!row) return;
    const check = () => setMore(row.scrollLeft + row.clientWidth < row.scrollWidth - 1);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(row);
    const mo = new MutationObserver(check);
    mo.observe(row, { childList: true, subtree: true, characterData: true });
    row.addEventListener("scroll", check, { passive: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
      row.removeEventListener("scroll", check);
    };
  }, [ref]);
  return more;
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
    // A new line: what was on screen until now becomes the one that fades out (Steve: without motion it just goes)
    setShownId(id);
    setOut(reduced() ? null : last.current);
  }
  last.current = { id, node: children };
  useEffect(() => {
    if (!out) return;
    const timer = window.setTimeout(() => setOut(null), 260);
    return () => window.clearTimeout(timer);
  }, [out]);
  return (
    <span className="ui-mpc__swap">
      {out && (
        <span key={`out-${out.id}`} className="ui-mpc__swap-item" data-out="" aria-hidden="true">
          {out.node}
        </span>
      )}
      <span key={id} className="ui-mpc__swap-item">
        {children}
      </span>
    </span>
  );
}

/* ---------- Icons ---------- */

/* 16px, thin 1.1px lines with round caps – or solid shapes */
const Glyph = ({ d, solid }: { d: string; solid?: boolean }) => (
  <svg viewBox="0 0 16 16" className="ui-mpc__glyph" data-solid={solid || undefined} aria-hidden="true">
    <path d={d} fillRule="evenodd" />
  </svg>
);
const EditIcon = () => <Glyph d="M9.8 3.7l2.5 2.5M3 13l.6-3 6.9-6.9a1.4 1.4 0 0 1 2 0l.4.4a1.4 1.4 0 0 1 0 2L6 12.4 3 13Z" />;
const PauseIcon = () => <Glyph solid d="M4.5 3.5a1 1 0 0 1 1-1h1a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-1a1 1 0 0 1-1-1v-9Zm4 0a1 1 0 0 1 1-1h1a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-1a1 1 0 0 1-1-1v-9Z" />;
const PlayIcon = () => <Glyph solid d="M4.5 3.4v9.2a.9.9 0 0 0 1.37.77l7.4-4.6a.9.9 0 0 0 0-1.54l-7.4-4.6a.9.9 0 0 0-1.37.77Z" />;
const ForgetIcon = () => <Glyph d="M2.5 4.5h11M6.5 4.5V3.2a.7.7 0 0 1 .7-.7h1.6a.7.7 0 0 1 .7.7v1.3M4 4.5l.6 8a1.1 1.1 0 0 0 1.1 1h4.6a1.1 1.1 0 0 0 1.1-1l.6-8M6.8 7.2v3.6M9.2 7.2v3.6" />;
const UndoIcon = () => <Glyph d="M5.5 3.5 3 6l2.5 2.5M3.2 6h6.3a3.5 3.5 0 0 1 0 7H7" />;
const PlusIcon = () => <Glyph d="M8 3.5v9M3.5 8h9" />;
const SearchIcon = () => <Glyph d="M7.2 11.9a4.7 4.7 0 1 0 0-9.4 4.7 4.7 0 0 0 0 9.4ZM10.6 10.6l2.9 2.9" />;
const CloseIcon = () => <Glyph d="m4.5 4.5 7 7m0-7-7 7" />;
const InfoIcon = () => (
  <Glyph solid d="M8 1.5a6.5 6.5 0 1 1 0 13 6.5 6.5 0 0 1 0-13Zm0 5.4a.6.6 0 0 0-.6.6v3.6a.6.6 0 0 0 1.2 0V7.5a.6.6 0 0 0-.6-.6Zm0-2.3a.8.8 0 1 0 0 1.6.8.8 0 0 0 0-1.6Z" />
);
const ArrowIcon = () => <Glyph d="M3.5 8h9M9 4.5 12.5 8 9 11.5" />;

/* ---------- One memory ---------- */

type RowActions = {
  setHot: (id: string | null) => void;
  setDraft: (text: string) => void;
  startEdit: (m: Memory) => void;
  saveEdit: () => void;
  cancelEdit: () => void;
  togglePause: (m: Memory) => void;
  forget: (m: Memory) => void;
};

/** One row – it only renders again when its own memory or state changes; the handlers come through a ref */
const Row = memo(function Row({
  m,
  open,
  leaving,
  hot,
  editing,
  draft,
  act,
  text,
  meta,
  focus,
}: {
  m: Memory;
  open: boolean;
  leaving: boolean;
  hot: boolean;
  editing: boolean;
  draft: string;
  act: { current: RowActions };
  text: PanelText;
  meta?: ReactNode;
  focus: boolean;
}) {
  return (
    <li className="ui-mpc__fold ui-mpc__fold--row" data-open={open ? "" : undefined} data-leaving={leaving ? "" : undefined} inert={!open}>
      <div className="ui-mpc__clip">
        <div
          className="ui-mpc__item"
          data-memory={m.id}
          data-mem={m.id}
          data-focus={focus ? "" : undefined}
          data-paused={m.paused ? "" : undefined}
          data-hot={hot ? "" : undefined}
          data-editing={editing ? "" : undefined}
          onPointerEnter={(e) => e.pointerType === "mouse" && act.current.setHot(m.id)}
          onFocus={() => act.current.setHot(m.id)}
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node)) act.current.setHot(null);
          }}
        >
          {editing ? (
            <textarea
              className="ui-mpc__edit"
              value={draft}
              rows={1}
              dir="auto"
              aria-label={text.editAria}
              data-mem-edit-text=""
              ref={(el) => {
                grow(el);
                if (el && !el.dataset.focused) {
                  el.dataset.focused = "1";
                  el.focus({ preventScroll: true });
                  el.setSelectionRange(el.value.length, el.value.length);
                }
              }}
              onChange={(e) => {
                act.current.setDraft(e.target.value);
                grow(e.target);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  act.current.saveEdit();
                } else if (e.key === "Escape") {
                  e.stopPropagation();
                  act.current.cancelEdit();
                }
              }}
            />
          ) : (
            <p className="ui-mpc__text" dir="auto">
              {m.text}
            </p>
          )}
          <div className="ui-mpc__meta">
            <span className="ui-mpc__source">
              {m.source && (
                <span className="ui-mpc__source-name" dir="auto" title={m.source}>
                  {m.sourceNode ?? m.source}
                </span>
              )}
              {m.source && m.date && <span className="ui-mpc__dot"> · </span>}
              {m.date && <span className="ui-mpc__date">{m.date}</span>}
            </span>
            {m.tag && <span className="ui-mpc__tag" data-on="" data-kind="note">{m.tag}</span>}
            <span className="ui-mpc__tag" data-on={m.paused ? "" : undefined} aria-hidden={!m.paused}>
              {text.paused}
            </span>
            {meta && <span className="ui-mpc__extra">{meta}</span>}
            {editing ? (
              <span className="ui-mpc__acts ui-mpc__acts--edit">
                <button type="button" className="ui-mpc__text-btn" onClick={() => act.current.cancelEdit()}>
                  {text.cancel}
                </button>
                <button type="button" className="ui-mpc__text-btn ui-mpc__text-btn--main" onClick={() => act.current.saveEdit()} data-mem-save="">
                  {text.save}
                </button>
              </span>
            ) : (
              <span className="ui-mpc__acts">
                <button
                  type="button"
                  className="ui-mpc__icon"
                  data-act="edit"
                  data-mem-edit=""
                  aria-label={text.editOf(m.text)}
                  title={text.edit}
                  onClick={() => act.current.startEdit(m)}
                >
                  <EditIcon />
                </button>
                {m.canPause !== false && <button
                  type="button"
                  className="ui-mpc__icon"
                  data-act="pause"
                  data-mem-pause=""
                  aria-label={m.paused ? text.resumeOf(m.text) : text.pauseOf(m.text)}
                  title={m.paused ? text.resume : text.pause}
                  onClick={() => act.current.togglePause(m)}
                >
                  <span className="ui-mpc__icon-swap" key={m.paused ? "play" : "pause"}>
                    {m.paused ? <PlayIcon /> : <PauseIcon />}
                  </span>
                </button>}
                <button
                  type="button"
                  className="ui-mpc__icon ui-mpc__icon--forget"
                  data-act="forget"
                  data-mem-del=""
                  aria-label={text.forgetOf(m.text)}
                  title={text.forget}
                  onClick={() => act.current.forget(m)}
                >
                  <ForgetIcon />
                </button>
              </span>
            )}
          </div>
        </div>
      </div>
    </li>
  );
});

export function MemoryPanel({
  memories,
  onChange,
  onAdd,
  onEdit,
  onPause,
  onForget,
  onRestore,
  enabled,
  defaultEnabled = true,
  onEnabledChange,
  categories,
  title = t("core:affect.memory.title"),
  maxHeight = 360,
  undoFor = 5000,
  addedSource,
  action,
  className,
  text: textIn,
  showSwitch = true,
  showTitle = true,
  searchable = true,
  renderMeta,
  focus,
}: MemoryPanelProps) {
  useT(); // Steve: the default words follow a change of language
  const text: PanelText = { ...TEXT(), ...textIn };
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const chipsRef = useRef<HTMLDivElement>(null);
  const catsRef = useRef<HTMLDivElement>(null);
  // Chip rows fade at their right edge only while more chips hide past it
  const chipsMore = useMore(chipsRef);
  const catsMore = useMore(catsRef);

  // Without onChange the panel keeps its own copy; with it the parent holds the truth
  const [inner, setInner] = useState(memories);
  const list = onChange ? memories : inner;
  const latest = useRef(list);
  latest.current = list;
  const commit = (next: Memory[]) => {
    latest.current = next;
    if (!onChange) setInner(next);
    onChange?.(next);
  };

  const [innerOn, setInnerOn] = useState(defaultEnabled);
  const on = enabled ?? innerOn;
  const setOn = (next: boolean) => {
    setInnerOn(next);
    onEnabledChange?.(next);
  };

  /** Rows folding away before they go, and rows that just arrived and unfold on the next frame */
  const [leaving, setLeaving] = useState<Record<string, true>>({});
  const [born, setBorn] = useState<Record<string, true>>({});
  const [filter, setFilter] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [hot, setHot] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [adding, setAdding] = useState(false);
  const [newText, setNewText] = useState("");
  const [newCat, setNewCat] = useState<string | null>(null);
  const [forgot, setForgot] = useState<{ memory: Memory; index: number; key: number } | null>(null);
  const reveal = useRef<string | null>(null);

  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // New rows render closed once, then unfold – and the list scrolls to them
  useEffect(() => {
    if (!Object.keys(born).length) return;
    let second = 0;
    const first = requestAnimationFrame(() => {
      second = requestAnimationFrame(() => setBorn({}));
    });
    return () => {
      cancelAnimationFrame(first);
      cancelAnimationFrame(second);
    };
  }, [born]);
  useEffect(() => {
    if (Object.keys(born).length || !reveal.current) return;
    const id = reveal.current;
    reveal.current = null;
    const timer = window.setTimeout(() => {
      const box = listRef.current;
      const row = box?.querySelector<HTMLElement>(`[data-memory="${CSS.escape(id)}"]`);
      if (!box || !row) return;
      const top = row.offsetTop - box.offsetTop;
      if (top < box.scrollTop || top + row.offsetHeight > box.scrollTop + box.clientHeight)
        box.scrollTo({ top: top - 8, behavior: reduced() ? "auto" : "smooth" });
    }, 120);
    return () => window.clearTimeout(timer);
  }, [born]);

  // Undo stays in the last row for a few seconds
  useEffect(() => {
    if (!forgot) return;
    const timer = window.setTimeout(() => setForgot(null), undoFor);
    return () => window.clearTimeout(timer);
  }, [forgot, undoFor]);

  /* ---------- Derived ---------- */

  const groups = [...(categories ?? [])];
  for (const m of list) if (!groups.includes(m.category)) groups.push(m.category);
  const present = list.filter((m) => !leaving[m.id]);
  const pausedCount = present.filter((m) => m.paused).length;
  const q = query.trim().toLowerCase();
  const shows = (m: Memory) =>
    (filter === null || m.category === filter) &&
    (!q || m.text.toLowerCase().includes(q) || m.category.toLowerCase().includes(q));
  const visible = (m: Memory) => shows(m) && !leaving[m.id] && !born[m.id];
  const anyVisible = list.some((m) => shows(m) && !leaving[m.id]);
  const hotMemory = list.find((m) => m.id === hot) ?? null;

  const shown = useRolling({ total: present.length, paused: pausedCount }, 520);
  const total = Math.round(shown.total ?? present.length);
  const paused = Math.round(shown.paused ?? pausedCount);

  /* ---------- Actions ---------- */

  const forget = async (m: Memory) => {
    if (leaving[m.id]) return;
    if (editing === m.id) setEditing(null);
    setHot(null);
    setLeaving((l) => ({ ...l, [m.id]: true }));
    if (!reduced()) await wait(320);
    if (!alive.current) return;
    const index = latest.current.findIndex((x) => x.id === m.id);
    commit(latest.current.filter((x) => x.id !== m.id));
    onForget?.(m.id);
    setLeaving(({ [m.id]: _gone, ...rest }) => rest);
    setForgot({ memory: m, index, key: Date.now() });
  };

  const undo = () => {
    if (!forgot) return;
    const { memory, index } = forgot;
    const next = [...latest.current];
    next.splice(Math.max(0, Math.min(index, next.length)), 0, memory);
    setBorn({ [memory.id]: true });
    reveal.current = memory.id;
    if (filter !== null && filter !== memory.category) setFilter(null);
    commit(next);
    onRestore?.(memory);
    setForgot(null);
  };

  const togglePause = (m: Memory) => {
    commit(latest.current.map((x) => (x.id === m.id ? { ...x, paused: !x.paused } : x)));
    onPause?.(m.id, !m.paused);
  };

  const startEdit = (m: Memory) => {
    setAdding(false);
    setDraft(m.text);
    setEditing(m.id);
  };
  const saveEdit = () => {
    const id = editing;
    const words = draft.trim().replace(/\s+/g, " ");
    setEditing(null);
    if (!id) return;
    const before = latest.current.find((x) => x.id === id);
    if (!before || !words || words === before.text) return;
    commit(latest.current.map((x) => (x.id === id ? { ...x, text: words } : x)));
    onEdit?.(id, words);
    // Back to the edit button, so the keyboard keeps its place
    window.requestAnimationFrame(() =>
      rootRef.current?.querySelector<HTMLElement>(`[data-memory="${CSS.escape(id)}"] [data-act="edit"]`)?.focus({ preventScroll: true }),
    );
  };
  const cancelEdit = () => {
    const id = editing;
    setEditing(null);
    if (id)
      window.requestAnimationFrame(() =>
        rootRef.current?.querySelector<HTMLElement>(`[data-memory="${CSS.escape(id)}"] [data-act="edit"]`)?.focus({ preventScroll: true }),
      );
  };

  const addCategory = newCat ?? filter ?? groups[0] ?? text.general;
  const openAdd = () => {
    setEditing(null);
    setNewText("");
    setNewCat(filter ?? groups[0] ?? text.general);
    setAdding(true);
  };
  const closeAdd = () => {
    setAdding(false);
    window.requestAnimationFrame(() =>
      rootRef.current?.querySelector<HTMLElement>(".ui-mpc__add")?.focus({ preventScroll: true }),
    );
  };
  const saveAdd = () => {
    const words = newText.trim().replace(/\s+/g, " ");
    if (!words) return;
    const memory: Memory = { id: newId(), text: words, category: addCategory, source: addedSource ?? t("core:affect.memory.added"), date: today() };
    setBorn({ [memory.id]: true });
    reveal.current = memory.id;
    if (filter !== null && filter !== memory.category) setFilter(null);
    if (q) setQuery("");
    commit([...latest.current, memory]);
    onAdd?.(memory);
    setNewText("");
    closeAdd();
  };

  const closeSearch = () => {
    setSearching(false);
    setQuery("");
    window.requestAnimationFrame(() =>
      rootRef.current?.querySelector<HTMLElement>(".ui-mpc__search-open")?.focus({ preventScroll: true }),
    );
  };

  const act = useRef<RowActions>(null!);
  act.current = { setHot, setDraft, startEdit, saveEdit, cancelEdit, togglePause, forget };

  // Chips move with the arrow keys like any radio group
  const chips = [null, ...groups.filter((g) => list.some((m) => m.category === g))];
  const onChipsKey = (e: KeyboardEvent) => {
    const keys = ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(e.key)) return;
    e.preventDefault();
    const at = chips.indexOf(filter);
    const next =
      e.key === "Home" ? 0
      : e.key === "End" ? chips.length - 1
      : (at + (e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1) + chips.length) % chips.length;
    setFilter(chips[next]);
    const buttons = (e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>("[role='radio']");
    buttons[next]?.focus();
    buttons[next]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };

  /* ---------- What the last row says ---------- */

  let caption: ReactNode = (
    <>
      <b>{num.format(total)}</b> {text.memories(total)}
      {(paused > 0 || pausedCount > 0) && <> · {text.pausedN(paused)}</>}
    </>
  );
  let captionKey = "rest";
  if (forgot) {
    caption = (
      <>
        <span className="ui-mpc__forgot">
          <span className="ui-mpc__flat">{text.forgot[0]}</span>
          <b className="ui-mpc__quote">{forgot.memory.text}</b>
          <span className="ui-mpc__flat">{text.forgot[1]}</span>
        </span>
      </>
    );
    captionKey = `forgot-${forgot.key}`;
  } else if (editing) {
    caption = text.editing;
    captionKey = "editing";
  } else if (adding) {
    caption = text.adding;
    captionKey = "adding";
  } else if (hotMemory) {
    caption = (
      <>
        <b>{hotMemory.category || text.other}</b> ·{" "}
        {!on ? text.memoryOff : hotMemory.paused ? text.notUsed : text.used}
      </>
    );
    captionKey = `hot-${hotMemory.id}-${hotMemory.paused ? 1 : 0}-${on ? 1 : 0}`;
  } else if (!on) {
    caption = text.off;
    captionKey = "off";
  }

  return (
    <div
      ref={rootRef}
      className={["ui-mpc", className].filter(Boolean).join(" ")}
      role="group"
      aria-labelledby={`${uid}-title`}
      data-off={on ? undefined : ""}
      style={{ ["--ui-mpc-max" as string]: maxHeight ? `${maxHeight}px` : "none" }}
    >
      <style href="ui-memory-panel-clean" precedence="default">
        {css}
      </style>

      <div className="ui-mpc__card">
        <div className="ui-mpc__head" hidden={!showTitle && !showSwitch}>
          <span id={`${uid}-title`} className="ui-mpc__title" title={title}>
            {title}
          </span>
          <span className="ui-mpc__count" aria-hidden="true">
            {num.format(total)}
          </span>
          {showSwitch && <button
            type="button"
            role="switch"
            aria-checked={on}
            aria-controls={`${uid}-note`}
            className="ui-mpc__switch"
            onClick={() => setOn(!on)}
          >
            <span className="ui-mpc__switch-label">{text.useMemory}</span>
            <span className="ui-mpc__track" aria-hidden="true">
              <span className="ui-mpc__knob" />
            </span>
          </button>}
        </div>

        {/* A short note unfolds while memory is off */}
        <div id={`${uid}-note`} className="ui-mpc__fold" data-open={on ? undefined : ""} inert={on}>
          <div className="ui-mpc__clip">
            <p className="ui-mpc__note">
              <InfoIcon />
              <span>{text.offNote}</span>
            </p>
          </div>
        </div>

        {/* Filter row: category chips, or a search field in their place (Steve: left out when it has nothing to filter by) */}
        {(searchable || chips.length > 2) && <div className="ui-mpc__filters" data-searching={searching ? "" : undefined}>
          <div
            ref={chipsRef}
            className="ui-mpc__chips"
            data-more={chipsMore ? "" : undefined}
            role="radiogroup"
            aria-label={text.category}
            inert={searching}
            onKeyDown={onChipsKey}
          >
            {chips.map((c) => {
              const count = present.filter((m) => c === null || m.category === c).length;
              const checked = filter === c;
              return (
                <button
                  key={c ?? "#all"}
                  type="button"
                  role="radio"
                  aria-checked={checked}
                  tabIndex={checked ? 0 : -1}
                  className="ui-mpc__chip"
                  data-linked={c !== null && hotMemory?.category === c ? "" : undefined}
                  onClick={() => setFilter(c)}
                >
                  {c === null ? text.all : c || text.other}
                  <span className="ui-mpc__chip-count">{num.format(count)}</span>
                </button>
              );
            })}
          </div>
          {searchable && <>
          <div className="ui-mpc__search" inert={!searching}>
            <SearchIcon />
            <input
              className="ui-mpc__input"
              type="text"
              value={query}
              placeholder={text.search}
              aria-label={text.search}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.stopPropagation();
                  closeSearch();
                }
              }}
              ref={(el) => {
                if (el && searching && document.activeElement !== el && !el.dataset.focused) {
                  el.dataset.focused = "1";
                  el.focus({ preventScroll: true });
                }
                if (el && !searching) delete el.dataset.focused;
              }}
            />
            <button type="button" className="ui-mpc__icon" aria-label={text.closeSearch} onClick={closeSearch}>
              <CloseIcon />
            </button>
          </div>
          <button
            type="button"
            className="ui-mpc__icon ui-mpc__search-open"
            aria-label={text.search}
            aria-expanded={searching}
            inert={searching}
            onClick={() => {
              setFilter(null);
              setSearching(true);
            }}
          >
            <SearchIcon />
          </button>
          </>}
        </div>}

        <div
          ref={listRef}
          className="ui-mpc__list"
          onPointerLeave={() => setHot(null)}
        >
          {groups.map((g) => {
            const items = list.filter((m) => m.category === g);
            if (!items.length) return null;
            const open = items.some((m) => shows(m) && !leaving[m.id]);
            return (
              <section key={g} className="ui-mpc__fold" data-open={open ? "" : undefined} inert={!open} aria-label={g || text.other}>
                <div className="ui-mpc__clip">
                  <h3 className="ui-mpc__group" dir="auto">
                    {g || text.other}
                  </h3>
                  <ul className="ui-mpc__items">
                    {items.map((m) => (
                      <Row
                        key={m.id}
                        m={m}
                        open={visible(m)}
                        leaving={!!leaving[m.id]}
                        hot={hot === m.id}
                        editing={editing === m.id}
                        draft={editing === m.id ? draft : ""}
                        act={act}
                        text={text}
                        meta={renderMeta?.(m)}
                        focus={focus === m.id}
                      />
                    ))}
                  </ul>
                </div>
              </section>
            );
          })}

          <div className="ui-mpc__fold" data-open={anyVisible ? undefined : ""} inert={anyVisible}>
            <div className="ui-mpc__clip">
              <p className="ui-mpc__empty" dir="auto">
                {list.length === 0 ? text.none : q ? text.noneFor(query.trim()) : text.noneHere}
              </p>
            </div>
          </div>
        </div>

        {/* New memory: a row that unfolds into an inline editor */}
        <div className="ui-mpc__bottom">
          <div className="ui-mpc__fold" data-open={adding ? undefined : ""} inert={adding}>
            <div className="ui-mpc__clip">
              <button type="button" className="ui-mpc__add" aria-expanded={adding} aria-controls={`${uid}-new`} onClick={openAdd} data-mem-new="">
                <PlusIcon />
                {text.newMemory}
              </button>
            </div>
          </div>
          <div id={`${uid}-new`} className="ui-mpc__fold" data-open={adding ? "" : undefined} inert={!adding}>
            <div className="ui-mpc__clip">
              <div className="ui-mpc__new">
                <textarea
                  className="ui-mpc__edit ui-mpc__edit--new"
                  value={newText}
                  rows={1}
                  placeholder={text.newPlaceholder}
                  dir="auto"
                  aria-label={text.newMemory}
                  data-mem-input=""
                  ref={(el) => {
                    grow(el);
                    if (el && adding && !el.dataset.focused) {
                      el.dataset.focused = "1";
                      el.focus({ preventScroll: true });
                    }
                    if (el && !adding) delete el.dataset.focused;
                  }}
                  onChange={(e) => {
                    setNewText(e.target.value);
                    grow(e.target);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      saveAdd();
                    } else if (e.key === "Escape") {
                      e.stopPropagation();
                      closeAdd();
                    }
                  }}
                />
                <div className="ui-mpc__new-row">
                  <div
                    ref={catsRef}
                    className="ui-mpc__cats"
                    data-more={catsMore ? "" : undefined}
                    role="radiogroup"
                    aria-label={text.newCategory}
                    onKeyDown={(e) => {
                      const opts = groups.length ? groups : [text.general];
                      const keys = ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp"];
                      if (!keys.includes(e.key)) return;
                      e.preventDefault();
                      const at = opts.indexOf(addCategory);
                      const next = (at + (e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1) + opts.length) % opts.length;
                      setNewCat(opts[next]);
                      (e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>("[role='radio']")[next]?.focus();
                    }}
                  >
                    {(groups.length ? groups : [text.general]).map((g) => (
                      <button
                        key={g}
                        type="button"
                        role="radio"
                        aria-checked={addCategory === g}
                        tabIndex={addCategory === g ? 0 : -1}
                        className="ui-mpc__cat"
                        title={g || text.other}
                        onClick={() => setNewCat(g)}
                      >
                        <span className="ui-mpc__cat-label" dir="auto">{g || text.other}</span>
                      </button>
                    ))}
                  </div>
                  <span className="ui-mpc__acts ui-mpc__acts--edit">
                    <button type="button" className="ui-mpc__text-btn" onClick={closeAdd}>
                      {text.cancel}
                    </button>
                    <button
                      type="button"
                      className="ui-mpc__text-btn ui-mpc__text-btn--main"
                      disabled={!newText.trim()}
                      onClick={saveAdd}
                      data-mem-add=""
                    >
                      {text.save}
                    </button>
                  </span>
                </div>
              </div>
            </div>
          </div>
        </div>

        {/* The last row of the card: counts, what you point at, and Undo after forgetting (Steve: not under an empty list) */}
        {(list.length > 0 || forgot || editing || adding || action) && <div className="ui-mpc__foot">
          <span className="ui-mpc__caption" aria-live="polite">
            <Swap id={captionKey}>{caption}</Swap>
          </span>
          {forgot ? (
            <button key={`undo-${forgot.key}`} type="button" className="ui-mpc__action ui-mpc__action--undo" onClick={undo}>
              <UndoIcon />
              {text.undo}
            </button>
          ) : (
            action && (
              <button key="action" type="button" className="ui-mpc__action" title={action.label} onClick={action.onClick}>
                <span className="ui-mpc__action-label">{action.label}</span>
                <ArrowIcon />
              </button>
            )
          )}
        </div>}
      </div>
    </div>
  );
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-mpc */
.ui-mpc {
  /* Steve: colors from Steve's tokens, which switch between light and dark themselves */
  --ui-mpc-card: var(--aui-surface, #ffffff);
  --ui-mpc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-mpc-strong: var(--aui-ink, #000000);
  --ui-mpc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-mpc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-mpc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-mpc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-mpc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-mpc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-mpc-fill: color-mix(in srgb, var(--aui-ink, #000) 6%, transparent);
  --ui-mpc-fill-strong: color-mix(in srgb, var(--aui-ink, #000) 12%, transparent);
  --ui-mpc-accent: var(--aui-blue, rgba(0, 96, 226, 0.765));
  --ui-mpc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-mpc-red: var(--aui-danger, #f04438);
  --ui-mpc-red-soft: var(--aui-danger-soft, color-mix(in srgb, #f04438 8%, transparent));
  --ui-mpc-amber: var(--aui-warning, #8f6c00);
  --ui-mpc-amber-soft: var(--aui-warning-soft, color-mix(in srgb, #e8b000 12%, transparent));
  --ui-mpc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-mpc-on-solid: var(--aui-on-primary, #fcfcfc);
  --ui-mpc-knob: #ffffff;
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-mpc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-mpc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  box-sizing: border-box;
  width: 100%;
  max-width: var(--ui-mpc-width, 400px);
  min-width: 0;
  container-type: inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-mpc-ink);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-mpc *, .ui-mpc *::before, .ui-mpc *::after { box-sizing: border-box; }
:where(.ui-mpc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
:where(.ui-mpc) :is(p, h3, ul) { margin: 0; padding: 0; list-style: none; font: inherit; }
.ui-mpc button:focus-visible { outline: 2px solid var(--ui-mpc-focus); outline-offset: 2px; }
/* Text fields: no focus box while typing – also against global :focus-visible rules of the host page */
.ui-mpc :is(input, textarea):focus-visible { outline: none; }
:where(.ui-mpc) svg { display: block; flex: none; }
.ui-mpc b { font-weight: 500; color: var(--ui-mpc-ink); }

.ui-mpc__glyph { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.1; stroke-linecap: round; stroke-linejoin: round; }
.ui-mpc__glyph[data-solid] { fill: currentColor; stroke: none; }

/* One white card – the edge is a shadow, not a border */
.ui-mpc__card { border-radius: var(--aui-radius-lg, 10px); background: var(--aui-surface, var(--ui-mpc-card)); box-shadow: var(--aui-shadow-card, var(--ui-mpc-shadow)); }

/* Everything that opens or goes does it the same quiet way: it unfolds or folds and fades, nothing travels */
.ui-mpc__fold { display: grid; grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 420ms var(--ui-mpc-ease), opacity 220ms ease; }
.ui-mpc__fold[data-open] { grid-template-rows: 1fr; opacity: 1; transition: grid-template-rows 420ms var(--ui-mpc-ease), opacity 320ms ease 80ms; }
.ui-mpc__fold--row[data-leaving] { transition: grid-template-rows 320ms var(--ui-mpc-ease), opacity 200ms ease, filter 200ms ease; filter: blur(2px); }
.ui-mpc__clip { min-height: 0; overflow: hidden; }

/* ----- Head ----- */
.ui-mpc__head { display: flex; align-items: center; gap: 8px; padding: 12px 10px 0 16px; }
.ui-mpc__head[hidden] { display: none; }
.ui-mpc__head[hidden] + .ui-mpc__fold + .ui-mpc__filters { padding-top: 10px; }
.ui-mpc__title { min-width: 0; overflow: hidden; font-size: calc(15px * var(--type-scale, 1)); font-weight: 500; line-height: 24px; color: var(--ui-mpc-strong); white-space: nowrap; text-overflow: ellipsis; }
.ui-mpc__count { flex: none; min-width: 20px; height: 18px; padding: 0 5px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-mpc-fill); font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 18px; text-align: center; color: var(--ui-mpc-text); font-variant-numeric: tabular-nums; }
.ui-mpc__switch { display: inline-flex; flex: none; align-items: center; gap: 8px; height: 28px; margin-left: auto; padding: 0 6px 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-mpc-text); transition: background-color 90ms ease, color 90ms ease; }
.ui-mpc__switch:hover { background: var(--ui-mpc-wash); color: var(--ui-mpc-ink); }
.ui-mpc__track { position: relative; width: 26px; height: 16px; border-radius: 5px; background: var(--ui-mpc-fill-strong); transition: background-color 220ms ease; }
.ui-mpc__knob { position: absolute; top: 2px; left: 2px; width: 12px; height: 12px; border-radius: 3.5px; background: var(--ui-mpc-knob); box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 2px rgba(0, 0, 0, 0.16); transition: transform 260ms var(--ui-mpc-ease); }
.ui-mpc__switch[aria-checked="true"] .ui-mpc__track { background: var(--ui-mpc-accent); }
.ui-mpc__switch[aria-checked="true"] .ui-mpc__knob { transform: translateX(10px); }

/* The note while memory is off: a soft gray fill, no stroke */
.ui-mpc__note { display: flex; gap: 8px; margin: 10px 12px 0 16px !important; padding: 8px 10px; border-radius: calc(var(--aui-radius-lg, 10px) - 2px); background: var(--ui-mpc-wash); font-size: calc(12px * var(--type-scale, 1)) !important; line-height: 16px !important; color: var(--ui-mpc-text); }
.ui-mpc__note .ui-mpc__glyph { color: var(--ui-mpc-icon); }

/* ----- Filters: the button-switch look ----- */
.ui-mpc__filters { position: relative; display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 8px; padding: 12px 10px 12px 16px; border-bottom: 0.5px solid var(--ui-mpc-line); transition: opacity 300ms ease; }
.ui-mpc__chips, .ui-mpc__search { grid-area: 1 / 1; transition: opacity 220ms ease, filter 220ms ease; }
.ui-mpc__chips { display: flex; gap: 4px; min-width: 0; margin: -3px; padding: 3px; overflow-x: auto; scrollbar-width: none; }
.ui-mpc__chips[data-more], .ui-mpc__cats[data-more] { -webkit-mask-image: linear-gradient(90deg, #000 calc(100% - 24px), transparent); mask-image: linear-gradient(90deg, #000 calc(100% - 24px), transparent); }
.ui-mpc__chips::-webkit-scrollbar { display: none; }
.ui-mpc__chip { display: inline-flex; flex: none; align-items: center; gap: 6px; padding: 4px 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); box-shadow: inset 0 0 0 0.5px var(--ui-mpc-line); font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; color: var(--ui-mpc-text); white-space: nowrap; transition: background-color 90ms ease, color 90ms ease; }
.ui-mpc__chip:hover { background: var(--ui-mpc-wash); }
.ui-mpc__chip[data-linked] { color: var(--ui-mpc-ink); }
.ui-mpc__chip[aria-checked="true"] { background: var(--ui-mpc-fill); color: var(--ui-mpc-ink); }
.ui-mpc__chip-count { font-size: calc(12px * var(--type-scale, 1)); font-weight: 400; color: var(--ui-mpc-muted); font-variant-numeric: tabular-nums; }
.ui-mpc__chip[aria-checked="true"] .ui-mpc__chip-count { color: var(--ui-mpc-text); }
.ui-mpc__search { display: flex; align-items: center; gap: 6px; height: 28px; padding: 0 2px 0 8px; border-radius: calc(var(--aui-radius, 7px) - 1px); box-shadow: inset 0 0 0 0.5px var(--ui-mpc-line); background: var(--ui-mpc-wash); color: var(--ui-mpc-icon); opacity: 0; visibility: hidden; }
.ui-mpc__input { flex: 1; min-width: 0; height: 28px; margin: 0; padding: 0; border: 0; background: none; font: inherit; font-size: calc(13px * var(--type-scale, 1)); color: var(--ui-mpc-strong); caret-color: var(--ui-mpc-accent); outline: none; }
.ui-mpc__input::placeholder { color: var(--ui-mpc-faint); opacity: 1; }
.ui-mpc__search .ui-mpc__icon { width: 24px; height: 24px; }
.ui-mpc__filters[data-searching] .ui-mpc__chips { opacity: 0; filter: blur(2px); }
.ui-mpc__filters[data-searching] .ui-mpc__search { grid-area: 1 / 1 / 2 / 3; opacity: 1; visibility: visible; }
.ui-mpc__filters[data-searching] .ui-mpc__search-open { opacity: 0; visibility: hidden; }
.ui-mpc__search-open { grid-area: 1 / 2; transition: opacity 200ms ease, background-color 90ms ease, color 90ms ease; }

/* ----- List ----- */
.ui-mpc__list { max-height: var(--ui-mpc-max); padding: 2px 6px 6px; overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; scrollbar-color: var(--ui-mpc-fill-strong) transparent; transition: opacity 300ms ease; }
.ui-mpc[data-off] .ui-mpc__list, .ui-mpc[data-off] .ui-mpc__filters { opacity: 0.5; }
.ui-mpc[data-off] .ui-mpc__list:hover, .ui-mpc[data-off] .ui-mpc__list:focus-within { opacity: 0.85; }
.ui-mpc__group { padding: 12px 10px 4px; font-size: calc(12px * var(--type-scale, 1)) !important; font-weight: 500 !important; line-height: 16px !important; color: var(--ui-mpc-muted); }
/* The hovered row gets a soft wash – short and crisp, and gone when the pointer leaves */
.ui-mpc__item { padding: 8px 10px 7px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); transition: background-color 90ms ease-out, opacity 220ms ease; }
.ui-mpc__item[data-hot], .ui-mpc__item[data-editing] { background: var(--ui-mpc-wash); }
/* Steve: memory text reads as body text */
.ui-mpc__text { font-size: calc(14px * var(--type-scale, 1)); line-height: 22px; color: var(--ui-mpc-ink); overflow-wrap: anywhere; transition: color 240ms ease; }
.ui-mpc__item[data-paused] .ui-mpc__text { color: var(--ui-mpc-muted); }
.ui-mpc__meta { display: flex; flex-wrap: wrap; align-items: center; gap: 2px 6px; min-height: 20px; margin-top: 2px; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; color: var(--ui-mpc-muted); }
/* Steve: pickers on the meta line, a tag such as "waiting for you", the memory a link pointed at */
.ui-mpc__extra { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 0 8px; min-width: 0; }
.ui-mpc__tag[data-kind="note"] { background: var(--aui-warning-soft, rgba(240, 160, 60, 0.12)); color: var(--aui-warning, #b25e00); }
.ui-mpc__item[data-focus] { background: color-mix(in srgb, var(--aui-primary, #4f46e5) 7%, transparent); }
.ui-mpc__source { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-mpc__dot { color: var(--ui-mpc-faint); }
.ui-mpc__tag { display: inline-flex; flex: none; align-items: center; height: 18px; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-mpc-amber-soft); font-size: calc(11px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; color: var(--ui-mpc-amber); opacity: 0; transform: scale(0.9); transition: opacity 220ms ease, transform 280ms var(--ui-mpc-ease); }
.ui-mpc__tag:not([data-on]) { position: absolute; visibility: hidden; }
.ui-mpc__tag[data-on] { opacity: 1; transform: none; animation: ui-mpc-in 280ms var(--ui-mpc-ease) both; }
.ui-mpc__acts { display: inline-flex; flex: none; align-items: center; gap: 2px; margin: -4px -6px -4px auto; opacity: 0; transition: opacity 120ms ease; }
.ui-mpc__item:is([data-hot], :focus-within) .ui-mpc__acts, .ui-mpc__acts--edit { opacity: 1; }
.ui-mpc__acts--edit { gap: 4px; margin-top: 4px; margin-right: -2px; margin-bottom: 0; }
.ui-mpc__item[data-editing] { padding-bottom: 8px; }
/* Ghost icon button */
.ui-mpc__icon { display: grid; flex: none; place-items: center; width: 28px; height: 28px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-mpc-icon); transition: background-color 90ms ease, color 90ms ease, opacity 200ms ease; }
.ui-mpc__icon:hover { background: var(--ui-mpc-wash); color: var(--ui-mpc-ink); }
.ui-mpc__item .ui-mpc__icon:hover { background: var(--ui-mpc-fill); }
.ui-mpc__item .ui-mpc__icon--forget:hover { background: var(--ui-mpc-red-soft); color: var(--ui-mpc-red); }
.ui-mpc__icon-swap { display: grid; animation: ui-mpc-in 260ms ease both; }
/* Cancel: ghost text; Save: the dark chip */
.ui-mpc__text-btn { display: inline-flex; flex: none; align-items: center; height: 28px; padding: 0 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; color: var(--ui-mpc-text); white-space: nowrap; transition: background-color 90ms ease, color 90ms ease, opacity 160ms ease; }
.ui-mpc__text-btn:hover { background: var(--ui-mpc-fill); color: var(--ui-mpc-ink); }
.ui-mpc__text-btn--main, .ui-mpc__text-btn--main:hover { background: var(--ui-mpc-solid); color: var(--ui-mpc-on-solid); }
.ui-mpc__text-btn--main:hover { opacity: 0.88; }
.ui-mpc__text-btn:disabled { cursor: default; opacity: 0.3; }
.ui-mpc__edit { display: block; width: 100%; min-height: 20px; margin: 0; padding: 0; border: 0; background: none; font: inherit; color: var(--ui-mpc-strong); caret-color: var(--ui-mpc-accent); resize: none; overflow: hidden; outline: none; }
.ui-mpc__edit::placeholder { color: var(--ui-mpc-faint); opacity: 1; }
.ui-mpc__empty { padding: 20px 8px 14px; font-size: calc(13px * var(--type-scale, 1)); text-align: center; color: var(--ui-mpc-muted); }
.ui-mpc__item .ui-mpc__edit { font-size: calc(14px * var(--type-scale, 1)); line-height: 22px; }
.ui-mpc__item .ui-mpc__edit:focus-visible { outline: none; }

/* ----- New memory ----- */
.ui-mpc__bottom { padding: 6px; border-top: 0.5px solid var(--ui-mpc-line); }
.ui-mpc__add { display: flex; align-items: center; gap: 8px; width: 100%; height: 32px; padding: 0 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-mpc-text); transition: background-color 90ms ease, color 90ms ease; }
.ui-mpc__add .ui-mpc__glyph { color: var(--ui-mpc-icon); transition: color 90ms ease; }
.ui-mpc__add:hover { background: var(--ui-mpc-wash); color: var(--ui-mpc-ink); }
.ui-mpc__add:hover .ui-mpc__glyph { color: var(--ui-mpc-ink); }
.ui-mpc__new { padding: 6px 4px 2px 10px; }
.ui-mpc__new-row { display: flex; align-items: center; gap: 8px; margin-top: 10px; }
.ui-mpc__cats { display: flex; flex: 1 1 auto; gap: 4px; min-width: 0; margin: -3px; padding: 3px; overflow-x: auto; scrollbar-width: none; }
.ui-mpc__cats::-webkit-scrollbar { display: none; }
.ui-mpc__cat { display: inline-flex; flex: none; align-items: center; padding: 4px 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); box-shadow: inset 0 0 0 0.5px var(--ui-mpc-line); font-size: calc(12px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; color: var(--ui-mpc-text); white-space: nowrap; transition: background-color 90ms ease, color 90ms ease; }
.ui-mpc__cat:hover { background: var(--ui-mpc-wash); }
.ui-mpc__cat[aria-checked="true"] { background: var(--ui-mpc-fill); color: var(--ui-mpc-ink); }
.ui-mpc__new .ui-mpc__acts--edit { margin: 0; }

/* ----- The last row of the card ----- */
.ui-mpc__foot { display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 8px 0 16px; border-top: 0.5px solid var(--ui-mpc-line); }
.ui-mpc__caption { flex: 1; min-width: 0; overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; color: var(--ui-mpc-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-mpc__swap { display: grid; min-width: 0; }
.ui-mpc__swap-item { grid-area: 1 / 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; animation: ui-mpc-swap-in 320ms ease both; }
.ui-mpc__swap-item[data-out] { animation: ui-mpc-swap-out 220ms ease both; }
.ui-mpc__forgot { display: flex; min-width: 0; }
.ui-mpc__flat { flex: none; white-space: pre; }
.ui-mpc__quote { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
/* The action button: ghost text with an arrow */
.ui-mpc__action { display: inline-flex; flex: none; align-items: center; gap: 4px; height: 28px; padding: 0 6px 0 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; color: var(--ui-mpc-text); white-space: nowrap; animation: ui-mpc-swap-in 300ms ease both; transition: background-color 90ms ease, color 90ms ease; }
.ui-mpc__action .ui-mpc__glyph { color: var(--ui-mpc-icon); transition: transform 220ms var(--ui-mpc-ease), color 90ms ease; }
.ui-mpc__action:hover { background: var(--ui-mpc-wash); color: var(--ui-mpc-ink); }
.ui-mpc__action:not(.ui-mpc__action--undo):hover .ui-mpc__glyph { transform: translateX(2px); color: var(--ui-mpc-ink); }
/* Undo: the white button with the hairline shadow */
.ui-mpc__action--undo { gap: 6px; padding: 0 10px 0 8px; background: var(--ui-mpc-card); box-shadow: var(--ui-mpc-shadow); color: var(--ui-mpc-strong); }
.ui-mpc__action--undo:hover { background: #fafafa; color: var(--ui-mpc-strong); }

/* Worst-case data: the date never hides behind a long source, long labels shorten with the full text on hover */
.ui-mpc__source { display: flex; }
.ui-mpc__source-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-mpc__dot, .ui-mpc__date { flex: none; white-space: pre; }
.ui-mpc__text { white-space: pre-line; text-align: start; }
.ui-mpc__group, .ui-mpc__empty { overflow-wrap: anywhere; }
.ui-mpc__group { text-align: start; }
.ui-mpc__action:not(.ui-mpc__action--undo) { flex: 0 1 auto; min-width: 0; max-width: 60%; }
.ui-mpc__action-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-mpc__cat { max-width: 100%; white-space: nowrap; }
.ui-mpc__cat-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }

@keyframes ui-mpc-in { from { opacity: 0; transform: scale(0.9); } }
@keyframes ui-mpc-swap-in { from { opacity: 0; filter: blur(2px); } }
@keyframes ui-mpc-swap-out { from { opacity: 1; } to { opacity: 0; filter: blur(2px); } }

/* Narrow panels: chips drop their counts and the switch keeps only its track, so the filters fit */
@container (max-width: 360px) {
  .ui-mpc__chip-count { display: none; }
  .ui-mpc__head { padding-left: 14px; }
  .ui-mpc__filters { padding-left: 14px; }
  .ui-mpc__foot { padding-left: 14px; }
  .ui-mpc__note { margin-left: 14px !important; }
}
@container (max-width: 320px) {
  .ui-mpc__switch-label { display: none; }
  .ui-mpc__switch { padding: 0 6px; }
  .ui-mpc__chip { padding: 4px 8px; }
}

/* Steve: when the meta line wraps, the actions get a line of their own – show them rather than leave a blank one */
@container (max-width: 520px) {
  .ui-mpc__acts { opacity: 1; }
}

/* Touch screens: actions always visible, taller targets */
@media (pointer: coarse) {
  .ui-mpc__acts { opacity: 1; }
  .ui-mpc__icon { width: 34px; height: 34px; }
  .ui-mpc__acts { margin-top: -7px; margin-bottom: -7px; }
  .ui-mpc__acts--edit { margin-top: 4px; margin-bottom: 0; }
  .ui-mpc__chip { padding: 6px 12px; }
  .ui-mpc__cat { padding: 5px 10px; }
  .ui-mpc__text-btn, .ui-mpc__action { height: 34px; }
  .ui-mpc__switch, .ui-mpc__add { height: 36px; }
  .ui-mpc__search, .ui-mpc__input { height: 34px; }
  .ui-mpc__foot { min-height: 52px; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-mpc *, .ui-mpc *::before, .ui-mpc *::after { animation: none !important; transition: none !important; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
/* Steve: dark mode comes with the tokens above; only the off switch's knob is dark-specific */
:where(.dark, [data-theme="dark"]) .ui-mpc__action--undo:hover { background: var(--ui-mpc-fill); }
:where(.dark, [data-theme="dark"]) .ui-mpc__switch[aria-checked="false"] .ui-mpc__knob { background: rgba(255, 255, 255, 0.55); }
`;
