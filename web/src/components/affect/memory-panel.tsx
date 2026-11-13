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
} from "react";

/*
 * Memory Panel – what an AI assistant remembers about someone, and a calm place to change it, in one self-contained file.
 * Memories are short facts grouped by category, each with where it came from.
 *   · a master switch turns memory off: the list dims and a short note unfolds under the head
 *   · category chips (and a small search) filter the list – rows fold away instead of jumping
 *   · hover a memory for its actions: edit the text in place, pause it (it stays, but isn't used) or forget it
 *   · a forgotten memory folds away and the gray tray offers Undo for a few seconds
 *   · "New memory" at the bottom unfolds into an inline editor with a category picker
 *   · the tray line counts memories and paused ones with rolling numbers and says what you are pointing at
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
const TEXT: PanelText = {
  paused: "Paused", cancel: "Cancel", save: "Save", edit: "Edit", pause: "Pause", resume: "Resume", forget: "Forget", undo: "Undo",
  editAria: "Edit memory", editOf: (x) => `Edit “${x}”`, pauseOf: (x) => `Pause “${x}”`, resumeOf: (x) => `Resume “${x}”`, forgetOf: (x) => `Forget “${x}”`,
  memories: (n) => (n === 1 ? "memory" : "memories"), pausedN: (n) => `${n} paused`, forgot: ["Forgot “", "”"],
  editing: "Enter to save · Esc to cancel", adding: "Pick a category · Enter to save", other: "Other",
  used: "used in answers", notUsed: "paused, not used in answers", memoryOff: "memory is off", off: "Memory is off",
  useMemory: "Use memory", offNote: "The assistant won't use or save memories in new chats. Nothing here is deleted.",
  category: "Category", all: "All", search: "Search memories", closeSearch: "Close search",
  none: "Nothing remembered yet", noneFor: (q) => `No memory mentions “${q}”`, noneHere: "No memories here",
  newMemory: "New memory", newPlaceholder: "Something the assistant should remember…", newCategory: "Category of the new memory", general: "General",
};

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
  /** How long Undo stays on the tray, in ms */
  undoFor?: number;
  /** Source and date given to memories added here */
  addedSource?: string;
  /** A button on the tray, e.g. { label: "Manage", onClick } – Undo takes its place for a moment */
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
const reduced = () =>
  typeof window !== "undefined" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const wait = (ms: number) =>
  new Promise<void>((resolve) => window.setTimeout(resolve, ms));
const today = () =>
  new Date().toLocaleDateString("en-US", { month: "short", day: "numeric" });
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
    <span className="ui-mp__swap">
      {out && (
        <span key={`out-${out.id}`} className="ui-mp__swap-item" data-out="" aria-hidden="true">
          {out.node}
        </span>
      )}
      <span key={id} className="ui-mp__swap-item">
        {children}
      </span>
    </span>
  );
}

/* ---------- Icons ---------- */

const line = {
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;
const Icon = ({ children, size = 14 }: { children: ReactNode; size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 14 14" fill="none" aria-hidden="true">
    {children}
  </svg>
);
const EditIcon = () => (
  <Icon>
    <path d="M8.6 3.1 10.9 5.4M2.8 11.2l.5-2.6 6-6a1.2 1.2 0 0 1 1.7 0l.6.6a1.2 1.2 0 0 1 0 1.7l-6 6-2.8.3Z" {...line} />
  </Icon>
);
const PauseIcon = () => (
  <Icon>
    <path d="M5.3 3.8v6.4M8.7 3.8v6.4" {...line} strokeWidth={1.6} />
  </Icon>
);
const PlayIcon = () => (
  <Icon>
    <path d="M4.8 3.6v6.8a.4.4 0 0 0 .6.3l5.2-3.4a.4.4 0 0 0 0-.6L5.4 3.3a.4.4 0 0 0-.6.3Z" {...line} />
  </Icon>
);
const ForgetIcon = () => (
  <Icon>
    <path d="M2.8 4.2h8.4M5.6 4.2V3a.8.8 0 0 1 .8-.8h1.2a.8.8 0 0 1 .8.8v1.2M3.9 4.2l.5 6.6a1 1 0 0 0 1 .9h3.2a1 1 0 0 0 1-.9l.5-6.6" {...line} />
  </Icon>
);
const UndoIcon = () => (
  <Icon size={12}>
    <path d="M4.6 2.8 2.4 5l2.2 2.2M2.6 5h5.6a3.2 3.2 0 0 1 0 6.4H6" {...line} />
  </Icon>
);
const PlusIcon = () => (
  <Icon>
    <path d="M7 3v8M3 7h8" {...line} />
  </Icon>
);
const SearchIcon = () => (
  <Icon>
    <circle cx="6.2" cy="6.2" r="3.6" {...line} />
    <path d="m8.9 8.9 2.4 2.4" {...line} />
  </Icon>
);
const CloseIcon = () => (
  <Icon size={12}>
    <path d="m4 4 6 6m0-6-6 6" {...line} />
  </Icon>
);
const InfoIcon = () => (
  <Icon>
    <circle cx="7" cy="7" r="5.2" {...line} />
    <path d="M7 6.4v3M7 4.6v.01" {...line} strokeWidth={1.6} />
  </Icon>
);
const ArrowIcon = () => (
  <Icon size={12}>
    <path d="M2.5 7h8M7.5 4l3 3-3 3" {...line} />
  </Icon>
);

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
    <li className="ui-mp__fold ui-mp__fold--row" data-open={open ? "" : undefined} data-leaving={leaving ? "" : undefined} inert={!open}>
      <div className="ui-mp__clip">
        <div
          className="ui-mp__item"
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
              className="ui-mp__edit"
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
            <p className="ui-mp__text" dir="auto">
              {m.text}
            </p>
          )}
          <div className="ui-mp__meta">
            <span className="ui-mp__source">
              {m.source && (
                <span className="ui-mp__source-name" dir="auto" title={m.source}>
                  {m.source}
                </span>
              )}
              {m.source && m.date && <span className="ui-mp__dot"> · </span>}
              {m.date && <span className="ui-mp__date">{m.date}</span>}
            </span>
            {m.tag && <span className="ui-mp__tag" data-on="" data-kind="note">{m.tag}</span>}
            <span className="ui-mp__tag" data-on={m.paused ? "" : undefined} aria-hidden={!m.paused}>
              {text.paused}
            </span>
            {meta && <span className="ui-mp__extra">{meta}</span>}
            {editing ? (
              <span className="ui-mp__acts ui-mp__acts--edit">
                <button type="button" className="ui-mp__text-btn" onClick={() => act.current.cancelEdit()}>
                  {text.cancel}
                </button>
                <button type="button" className="ui-mp__text-btn ui-mp__text-btn--main" onClick={() => act.current.saveEdit()} data-mem-save="">
                  {text.save}
                </button>
              </span>
            ) : (
              <span className="ui-mp__acts">
                <button
                  type="button"
                  className="ui-mp__icon"
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
                  className="ui-mp__icon"
                  data-act="pause"
                  data-mem-pause=""
                  aria-label={m.paused ? text.resumeOf(m.text) : text.pauseOf(m.text)}
                  title={m.paused ? text.resume : text.pause}
                  onClick={() => act.current.togglePause(m)}
                >
                  <span className="ui-mp__icon-swap" key={m.paused ? "play" : "pause"}>
                    {m.paused ? <PlayIcon /> : <PauseIcon />}
                  </span>
                </button>}
                <button
                  type="button"
                  className="ui-mp__icon ui-mp__icon--forget"
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
  title = "Memory",
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
  const text: PanelText = { ...TEXT, ...textIn };
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

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

  // Undo stays on the tray for a few seconds
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
      rootRef.current?.querySelector<HTMLElement>(".ui-mp__add")?.focus({ preventScroll: true }),
    );
  };
  const saveAdd = () => {
    const words = newText.trim().replace(/\s+/g, " ");
    if (!words) return;
    const memory: Memory = { id: newId(), text: words, category: addCategory, source: addedSource ?? "You added", date: today() };
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
      rootRef.current?.querySelector<HTMLElement>(".ui-mp__search-open")?.focus({ preventScroll: true }),
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

  /* ---------- What the tray says ---------- */

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
        <span className="ui-mp__forgot">
          <span className="ui-mp__flat">{text.forgot[0]}</span>
          <b className="ui-mp__quote">{forgot.memory.text}</b>
          <span className="ui-mp__flat">{text.forgot[1]}</span>
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
      className={["ui-mp", className].filter(Boolean).join(" ")}
      role="group"
      aria-labelledby={`${uid}-title`}
      data-off={on ? undefined : ""}
      style={{ ["--ui-mp-max" as string]: maxHeight ? `${maxHeight}px` : "none" }}
    >
      <style href="ui-memory-panel" precedence="default">
        {css}
      </style>

      <div className="ui-mp__card">
        <div className="ui-mp__head" hidden={!showTitle && !showSwitch}>
          <span id={`${uid}-title`} className="ui-mp__title" title={title}>
            {title}
          </span>
          <span className="ui-mp__count" aria-hidden="true">
            {num.format(total)}
          </span>
          {showSwitch && <button
            type="button"
            role="switch"
            aria-checked={on}
            aria-controls={`${uid}-note`}
            className="ui-mp__switch"
            onClick={() => setOn(!on)}
          >
            <span className="ui-mp__switch-label">{text.useMemory}</span>
            <span className="ui-mp__track" aria-hidden="true">
              <span className="ui-mp__knob" />
            </span>
          </button>}
        </div>

        {/* A short note unfolds while memory is off */}
        <div id={`${uid}-note`} className="ui-mp__fold" data-open={on ? undefined : ""} inert={on}>
          <div className="ui-mp__clip">
            <p className="ui-mp__note">
              <InfoIcon />
              <span>{text.offNote}</span>
            </p>
          </div>
        </div>

        {/* Filter row: category chips, or a search field in their place */}
        <div className="ui-mp__filters" data-searching={searching ? "" : undefined}>
          <div
            className="ui-mp__chips"
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
                  className="ui-mp__chip"
                  data-linked={c !== null && hotMemory?.category === c ? "" : undefined}
                  onClick={() => setFilter(c)}
                >
                  {c === null ? text.all : c || text.other}
                  <span className="ui-mp__chip-count">{num.format(count)}</span>
                </button>
              );
            })}
          </div>
          {searchable && <>
          <div className="ui-mp__search" inert={!searching}>
            <SearchIcon />
            <input
              className="ui-mp__input"
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
            <button type="button" className="ui-mp__icon" aria-label={text.closeSearch} onClick={closeSearch}>
              <CloseIcon />
            </button>
          </div>
          <button
            type="button"
            className="ui-mp__icon ui-mp__search-open"
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
        </div>

        <div
          ref={listRef}
          className="ui-mp__list"
          onPointerLeave={() => setHot(null)}
        >
          {groups.map((g) => {
            const items = list.filter((m) => m.category === g);
            if (!items.length) return null;
            const open = items.some((m) => shows(m) && !leaving[m.id]);
            return (
              <section key={g} className="ui-mp__fold" data-open={open ? "" : undefined} inert={!open} aria-label={g || text.other}>
                <div className="ui-mp__clip">
                  <h3 className="ui-mp__group" dir="auto">
                    {g || text.other}
                  </h3>
                  <ul className="ui-mp__items">
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

          <div className="ui-mp__fold" data-open={anyVisible ? undefined : ""} inert={anyVisible}>
            <div className="ui-mp__clip">
              <p className="ui-mp__empty" dir="auto">
                {list.length === 0 ? text.none : q ? text.noneFor(query.trim()) : text.noneHere}
              </p>
            </div>
          </div>
        </div>

        {/* New memory: a row that unfolds into an inline editor */}
        <div className="ui-mp__bottom">
          <div className="ui-mp__fold" data-open={adding ? undefined : ""} inert={adding}>
            <div className="ui-mp__clip">
              <button type="button" className="ui-mp__add" aria-expanded={adding} aria-controls={`${uid}-new`} onClick={openAdd} data-mem-new="">
                <PlusIcon />
                {text.newMemory}
              </button>
            </div>
          </div>
          <div id={`${uid}-new`} className="ui-mp__fold" data-open={adding ? "" : undefined} inert={!adding}>
            <div className="ui-mp__clip">
              <div className="ui-mp__new">
                <textarea
                  className="ui-mp__edit ui-mp__edit--new"
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
                <div className="ui-mp__new-row">
                  <div
                    className="ui-mp__cats"
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
                        className="ui-mp__cat"
                        title={g || text.other}
                        onClick={() => setNewCat(g)}
                      >
                        <span className="ui-mp__cat-label" dir="auto">{g || text.other}</span>
                      </button>
                    ))}
                  </div>
                  <span className="ui-mp__acts ui-mp__acts--edit">
                    <button type="button" className="ui-mp__text-btn" onClick={closeAdd}>
                      {text.cancel}
                    </button>
                    <button
                      type="button"
                      className="ui-mp__text-btn ui-mp__text-btn--main"
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
      </div>

      {/* The tray: counts, what you point at, and Undo after forgetting */}
      <div className="ui-mp__foot">
        <span className="ui-mp__caption" aria-live="polite">
          <Swap id={captionKey}>{caption}</Swap>
        </span>
        {forgot ? (
          <button key={`undo-${forgot.key}`} type="button" className="ui-mp__action ui-mp__action--undo" onClick={undo}>
            <UndoIcon />
            {text.undo}
          </button>
        ) : (
          action && (
            <button key="action" type="button" className="ui-mp__action" title={action.label} onClick={action.onClick}>
              <span className="ui-mp__action-label">{action.label}</span>
              <ArrowIcon />
            </button>
          )
        )}
      </div>
    </div>
  );
}

const css = /* css */ `
.ui-mp {
  --ui-mp-tray: var(--aui-subtle, #f0f0f0);
  --ui-mp-card: #ffffff;
  --ui-mp-line: var(--aui-border, #ececec);
  --ui-mp-ink: var(--aui-ink, #171717);
  --ui-mp-text: var(--aui-ink, #404040);
  --ui-mp-muted: var(--aui-muted, #8f8f8f);
  --ui-mp-well: var(--aui-subtle, #f4f4f5);
  --ui-mp-hover: #ececee;
  --ui-mp-track: #dcdce0;
  --ui-mp-accent: var(--aui-blue, #4fb0fe);
  --ui-mp-red: var(--aui-danger, #c4320a);
  --ui-mp-red-bg: color-mix(in srgb, var(--aui-red, #f04438) 10%, transparent);
  --ui-mp-tag: #5f5f66;
  --ui-mp-tag-bg: rgba(110, 110, 120, 0.1);
  --ui-mp-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-mp-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  box-sizing: border-box;
  width: 100%;
  max-width: var(--ui-mp-width, 380px);
  min-width: 0;
  container-type: inline-size;
  padding: 4px;
  border-radius: calc(var(--aui-radius-lg, 16px) + 4px);
  background: var(--ui-mp-tray);
  font-family: var(--aui-font, "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  color: var(--ui-mp-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-mp *, .ui-mp *::before, .ui-mp *::after { box-sizing: border-box; }
:where(.ui-mp) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
:where(.ui-mp) :is(p, h3, ul) { margin: 0; padding: 0; list-style: none; font: inherit; }
.ui-mp button:focus-visible { outline: 2px solid var(--ui-mp-focus); outline-offset: 2px; }
/* Typing in a field: no ring – the caret shows where you are, the edge only firms up a little */
.ui-mp :is(input, textarea):focus-visible { outline: none; }
.ui-mp svg { display: block; flex: none; }
.ui-mp b { font-weight: 500; color: var(--ui-mp-ink); }

.ui-mp__card { border-radius: var(--aui-radius-lg, 16px); background: var(--aui-surface, var(--ui-mp-card)); box-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-mp-line), 0 1px 2px rgba(0, 0, 0, 0.04)); }

/* Everything that opens or goes does it the same quiet way: it unfolds or folds and fades, nothing travels */
.ui-mp__fold { display: grid; grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 420ms var(--ui-mp-ease), opacity 220ms ease; }
.ui-mp__fold[data-open] { grid-template-rows: 1fr; opacity: 1; transition: grid-template-rows 420ms var(--ui-mp-ease), opacity 320ms ease 80ms; }
.ui-mp__fold--row[data-leaving] { transition: grid-template-rows 320ms var(--ui-mp-ease), opacity 200ms ease, filter 200ms ease; filter: blur(2px); }
.ui-mp__clip { min-height: 0; overflow: hidden; }

/* ----- Head ----- */
.ui-mp__head { display: flex; align-items: center; gap: 8px; height: 48px; padding: 0 12px 0 14px; }
.ui-mp__head[hidden] { display: none; }
.ui-mp__head[hidden] + .ui-mp__fold + .ui-mp__filters { padding-top: 10px; }
.ui-mp__title { min-width: 0; overflow: hidden; font-size: 13.5px; font-weight: 600; color: var(--ui-mp-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-mp__count { flex: none; min-width: 20px; height: 20px; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 7px) - 2px); background: var(--ui-mp-well); font-size: 12px; font-weight: 500; line-height: 20px; text-align: center; color: var(--ui-mp-muted); font-variant-numeric: tabular-nums; }
.ui-mp__switch { display: inline-flex; flex: none; align-items: center; gap: 8px; height: 28px; margin-left: auto; padding: 0 2px 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); font-size: 12.5px; color: var(--ui-mp-text); transition: background-color 160ms ease; }
.ui-mp__switch:hover { background: var(--ui-mp-well); }
.ui-mp__track { position: relative; width: 28px; height: 16px; border-radius: 8px; background: var(--ui-mp-track); transition: background-color 240ms ease; }
.ui-mp__knob { position: absolute; top: 2px; left: 2px; width: 12px; height: 12px; border-radius: 50%; background: #fff; box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18); transition: transform 280ms var(--ui-mp-ease); }
.ui-mp__switch[aria-checked="true"] .ui-mp__track { background: var(--aui-primary, var(--ui-mp-ink)); }
.ui-mp__switch[aria-checked="true"] .ui-mp__knob { transform: translateX(12px); }

.ui-mp__note { display: flex; gap: 8px; margin: 0 12px 10px !important; padding: 8px 10px; border-radius: calc(var(--aui-radius-lg, 16px) - 6px); background: var(--ui-mp-well); font-size: 12.5px; line-height: 18px; color: var(--ui-mp-text); }
.ui-mp__note svg { margin-top: 2px; color: var(--ui-mp-muted); }

/* ----- Filters ----- */
.ui-mp__filters { position: relative; display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 4px; padding: 0 10px 10px 12px; }
.ui-mp__chips, .ui-mp__search { grid-area: 1 / 1; transition: opacity 220ms ease, filter 220ms ease; }
.ui-mp__chips { display: flex; gap: 4px; min-width: 0; margin: -2px -4px; padding: 2px 4px; overflow-x: auto; scrollbar-width: none; mask-image: linear-gradient(90deg, #000 calc(100% - 14px), transparent); }
.ui-mp__chips::-webkit-scrollbar { display: none; }
.ui-mp__chip { display: inline-flex; flex: none; align-items: center; gap: 5px; height: 26px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); font-size: 12.5px; color: var(--ui-mp-text); white-space: nowrap; transition: background-color 160ms ease, color 160ms ease, box-shadow 160ms ease; }
.ui-mp__chip:hover { background: var(--ui-mp-well); }
.ui-mp__chip[data-linked] { color: var(--ui-mp-ink); }
.ui-mp__chip[data-linked]:not([aria-checked="true"]) { background: var(--ui-mp-well); }
.ui-mp__chip[aria-checked="true"] { background: var(--ui-mp-well); color: var(--ui-mp-ink); box-shadow: inset 0 0 0 1px var(--ui-mp-line); }
.ui-mp__chip-count { font-size: 11.5px; color: var(--ui-mp-muted); font-variant-numeric: tabular-nums; }
.ui-mp__search { display: flex; align-items: center; gap: 6px; height: 28px; padding: 0 2px 0 8px; border-radius: calc(var(--aui-radius, 10px) - 2px); background: var(--ui-mp-well); color: var(--ui-mp-muted); opacity: 0; visibility: hidden; }
.ui-mp__input { flex: 1; min-width: 0; height: 28px; margin: 0; padding: 0; border: 0; background: none; font: inherit; font-size: 12.5px; color: var(--ui-mp-ink); outline: none; }
.ui-mp__input::placeholder { color: var(--ui-mp-muted); }
.ui-mp__search .ui-mp__icon { width: 24px; height: 24px; }
.ui-mp__filters[data-searching] .ui-mp__chips { opacity: 0; filter: blur(2px); }
.ui-mp__filters[data-searching] .ui-mp__search { grid-area: 1 / 1 / 2 / 3; opacity: 1; visibility: visible; }
.ui-mp__filters[data-searching] .ui-mp__search-open { opacity: 0; visibility: hidden; }
.ui-mp__search-open { grid-area: 1 / 2; transition: opacity 200ms ease, background-color 160ms ease, color 160ms ease; }

/* ----- List ----- */
.ui-mp__list { max-height: var(--ui-mp-max); padding: 2px 6px 6px; overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; scrollbar-color: var(--ui-mp-track) transparent; border-top: 1px solid var(--ui-mp-line); transition: opacity 300ms ease; }
.ui-mp[data-off] .ui-mp__list, .ui-mp[data-off] .ui-mp__filters { opacity: 0.5; }
.ui-mp[data-off] .ui-mp__list:hover, .ui-mp[data-off] .ui-mp__list:focus-within { opacity: 0.85; }
.ui-mp__filters { transition: opacity 300ms ease; }
.ui-mp__group { padding: 10px 8px 2px; font-size: 12px !important; font-weight: 500 !important; line-height: 18px !important; color: var(--ui-mp-muted); }
.ui-mp__item { padding: 8px 8px 6px; border-radius: calc(var(--aui-radius-sm, 7px) + 3px); transition: background-color 160ms ease, opacity 220ms ease; }
.ui-mp__item[data-hot], .ui-mp__item[data-editing] { background: var(--ui-mp-well); }
.ui-mp__text { font-size: 14px; line-height: 22px; color: var(--ui-mp-ink); overflow-wrap: anywhere; transition: color 240ms ease; }
.ui-mp__item[data-paused] .ui-mp__text { color: var(--ui-mp-muted); }
.ui-mp__meta { display: flex; flex-wrap: wrap; align-items: center; gap: 2px 6px; min-height: 24px; font-size: 12px; color: var(--ui-mp-muted); }
/* Steve: pickers on the meta line, a tag such as "waiting for you", the memory a link pointed at */
.ui-mp__extra { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 0 8px; min-width: 0; }
.ui-mp__tag[data-kind="note"] { background: var(--aui-warning-soft, rgba(240, 160, 60, 0.12)); color: var(--aui-warning, #b25e00); }
.ui-mp__item[data-focus] { background: color-mix(in srgb, var(--aui-primary, #4f46e5) 7%, transparent); }
.ui-mp__source { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-mp__tag { display: inline-flex; flex: none; align-items: center; height: 18px; padding: 0 6px; border-radius: calc(var(--aui-radius-sm, 7px) - 2px); background: var(--ui-mp-tag-bg); font-size: 11.5px; font-weight: 500; color: var(--ui-mp-tag); opacity: 0; transform: scale(0.9); transition: opacity 220ms ease, transform 280ms var(--ui-mp-ease); }
.ui-mp__tag:not([data-on]) { position: absolute; visibility: hidden; }
.ui-mp__tag[data-on] { opacity: 1; transform: none; animation: ui-mp-in 280ms var(--ui-mp-ease) both; }
.ui-mp__acts { display: inline-flex; flex: none; align-items: center; gap: 2px; margin: -1px -4px -1px auto; opacity: 0; transition: opacity 180ms ease; }
.ui-mp__item:is([data-hot], :focus-within) .ui-mp__acts, .ui-mp__acts--edit { opacity: 1; }
.ui-mp__icon { display: grid; flex: none; place-items: center; width: 26px; height: 26px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); color: var(--ui-mp-muted); transition: background-color 160ms ease, color 160ms ease, opacity 200ms ease; }
.ui-mp__icon:hover { background: var(--ui-mp-hover); color: var(--ui-mp-ink); }
.ui-mp__icon--forget:hover { background: var(--ui-mp-red-bg); color: var(--ui-mp-red); }
.ui-mp__icon-swap { display: grid; animation: ui-mp-in 260ms ease both; }
.ui-mp__text-btn { display: inline-flex; align-items: center; height: 26px; padding: 0 9px; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); font-size: 12.5px; font-weight: 500; color: var(--ui-mp-text); transition: background-color 160ms ease, color 160ms ease, opacity 160ms ease; }
.ui-mp__text-btn:hover { background: var(--ui-mp-hover); color: var(--ui-mp-ink); }
.ui-mp__text-btn--main { background: var(--aui-primary, var(--ui-mp-ink)); color: var(--aui-on-primary, var(--ui-mp-card)); }
.ui-mp__text-btn--main:hover { background: var(--aui-primary, var(--ui-mp-ink)); color: var(--aui-on-primary, var(--ui-mp-card)); opacity: 0.86; }
.ui-mp__text-btn:disabled { cursor: default; opacity: 0.35; }
.ui-mp__item .ui-mp__edit { font-size: 14px; line-height: 22px; }
.ui-mp__edit { display: block; width: 100%; min-height: 20px; margin: 0; padding: 0; border: 0; background: none; font: inherit; color: var(--ui-mp-ink); resize: none; overflow: hidden; outline: none; }
.ui-mp__edit::placeholder { color: var(--ui-mp-muted); }
.ui-mp__item .ui-mp__edit:focus-visible { outline: none; }
.ui-mp__empty { padding: 18px 8px 12px; font-size: 12.5px; text-align: center; color: var(--ui-mp-muted); }

/* ----- New memory ----- */
.ui-mp__bottom { padding: 4px 6px 6px; border-top: 1px solid var(--ui-mp-line); }
.ui-mp__add { display: flex; align-items: center; gap: 8px; width: 100%; height: 32px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) + 3px); font-size: 13px; color: var(--ui-mp-muted); transition: background-color 160ms ease, color 160ms ease; }
.ui-mp__add:hover { background: var(--ui-mp-well); color: var(--ui-mp-ink); }
.ui-mp__new { margin: 2px 0 0; padding: 8px 8px 6px; border-radius: var(--aui-radius, 10px); background: var(--ui-mp-well); }
.ui-mp__new-row { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: 6px; margin-top: 8px; }
.ui-mp__cats { display: flex; flex: 1 1 auto; flex-wrap: wrap; gap: 4px; min-width: 0; }
.ui-mp__cat { display: inline-flex; align-items: center; height: 24px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) - 1px); font-size: 12px; color: var(--ui-mp-muted); transition: background-color 160ms ease, color 160ms ease, box-shadow 160ms ease; }
.ui-mp__cat:hover { color: var(--ui-mp-ink); }
.ui-mp__cat[aria-checked="true"] { background: var(--ui-mp-card); color: var(--ui-mp-ink); box-shadow: 0 0 0 1px var(--ui-mp-line), 0 1px 2px rgba(0, 0, 0, 0.04); }

/* ----- Tray ----- */
.ui-mp__foot { display: flex; align-items: center; gap: 8px; min-height: 38px; padding: 4px 4px 0 10px; }
.ui-mp__caption { flex: 1; min-width: 0; overflow: hidden; font-size: 12.5px; color: var(--ui-mp-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.ui-mp__swap { display: grid; min-width: 0; }
.ui-mp__swap-item { grid-area: 1 / 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; animation: ui-mp-swap-in 320ms ease both; }
.ui-mp__swap-item[data-out] { animation: ui-mp-swap-out 220ms ease both; }
.ui-mp__forgot { display: flex; min-width: 0; }
.ui-mp__flat { flex: none; white-space: pre; }
.ui-mp__quote { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-mp__action { display: inline-flex; flex: none; align-items: center; gap: 5px; height: 26px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 7px) + 3px); font-size: 12.5px; font-weight: 500; color: var(--ui-mp-text); white-space: nowrap; animation: ui-mp-swap-in 300ms ease both; transition: background-color 160ms ease, color 160ms ease; }
.ui-mp__action:hover { background: var(--ui-mp-card); color: var(--ui-mp-ink); }
.ui-mp__action--undo { padding: 0 9px 0 7px; background: var(--ui-mp-card); color: var(--ui-mp-ink); box-shadow: 0 0 0 1px var(--ui-mp-line), 0 1px 2px rgba(0, 0, 0, 0.04); }
.ui-mp__action:not(.ui-mp__action--undo) svg { transition: transform 220ms var(--ui-mp-ease); }
.ui-mp__action:not(.ui-mp__action--undo):hover svg { transform: translateX(2px); }

/* Worst-case data: the date never hides behind a long source, long labels shorten with the full text on hover */
.ui-mp__source { display: flex; }
.ui-mp__source-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-mp__dot, .ui-mp__date { flex: none; white-space: pre; }
.ui-mp__text { white-space: pre-line; text-align: start; }
.ui-mp__group, .ui-mp__empty { overflow-wrap: anywhere; }
.ui-mp__group { text-align: start; }
.ui-mp__action:not(.ui-mp__action--undo) { flex: 0 1 auto; min-width: 0; max-width: 60%; }
.ui-mp__action-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-mp__cat { max-width: 100%; white-space: nowrap; }
.ui-mp__cat-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }

@keyframes ui-mp-in { from { opacity: 0; transform: scale(0.9); } }
@keyframes ui-mp-swap-in { from { opacity: 0; filter: blur(2px); } }
@keyframes ui-mp-swap-out { from { opacity: 1; } to { opacity: 0; filter: blur(2px); } }

/* Narrow panels: chips drop their counts and the switch keeps only its track, so the filters fit */
@container (max-width: 340px) {
  .ui-mp__chip-count { display: none; }
  .ui-mp__chip { padding: 0 7px; }
}
@container (max-width: 320px) {
  .ui-mp__switch-label { display: none; }
  .ui-mp__switch { padding: 0 2px; }
}

/* Steve: when the meta line wraps, the actions get a line of their own – show them rather than leave a blank one */
@container (max-width: 520px) {
  .ui-mp__acts { opacity: 1; }
}

@media (pointer: coarse) {
  .ui-mp__acts { opacity: 1; }
  .ui-mp__icon { width: 32px; height: 32px; }
  .ui-mp__meta { min-height: 32px; }
  .ui-mp__chip, .ui-mp__text-btn { height: 32px; }
  .ui-mp__cat { height: 30px; }
  .ui-mp__switch, .ui-mp__add { height: 36px; }
  .ui-mp__action { height: 30px; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-mp *, .ui-mp *::before, .ui-mp *::after { animation: none !important; transition: none !important; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-mp {
  --ui-mp-tray: var(--aui-subtle, #111113);
  --ui-mp-card: #19191b;
  --ui-mp-line: var(--aui-border, #2a2a2e);
  --ui-mp-ink: var(--aui-ink, #ededed);
  --ui-mp-text: var(--aui-ink, #c8c8cc);
  --ui-mp-muted: var(--aui-muted, #8b8b92);
  --ui-mp-well: var(--aui-subtle, #222225);
  --ui-mp-hover: #2c2c30;
  --ui-mp-track: #3a3a40;
  --ui-mp-red: var(--aui-danger, #ff8a7a);
  --ui-mp-red-bg: color-mix(in srgb, var(--aui-red, #ff6b5f) 16%, transparent);
  --ui-mp-tag: #b4b4bb;
  --ui-mp-tag-bg: rgba(160, 160, 170, 0.14);
  --ui-mp-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
}
:where(.dark, [data-theme="dark"]) .ui-mp__knob { background: #19191b; }
:where(.dark, [data-theme="dark"]) .ui-mp__switch[aria-checked="false"] .ui-mp__knob { background: #8b8b92; }
`;
