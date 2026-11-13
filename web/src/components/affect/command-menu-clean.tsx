/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { memo, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type SyntheticEvent } from "react";

/*
 * Command Menu (Clean) – search and run anything from one field, in a single self-contained file.
 * Clean style: no tray – one panel with hairline shadows, small quiet group labels, quiet key chips
 * and a last row with hints under a hairline.
 * Steve: the groups are Steve's own (actions, pages, items, sessions, the conversation, contacts) and come from
 * the caller; rows the Hub's search found (`always`) stay whatever the local filter says. The query is reported
 * (`onQueryChange`) so the caller can ask the Hub, and `busy` shows while it does. The words are the caller's
 * (`labels`), no sample data, no assistant fallback; ⌘K is left to the caller, which opens and closes the menu.
 * ↑ ↓ move row by row, Tab jumps section by section, ↵ runs, Esc clears the search or closes.
 * The styles are injected below, so no extra files are needed.
 */

export type Command = {
  id: string;
  label: string;
  /** Lighter text after the label, e.g. a person, a channel or a page */
  hint?: string;
  /** "logo" sits on a small tile; "icon" is a plain glyph */
  icon?: ReactNode;
  logo?: ReactNode;
  /** Keys shown on the right, e.g. ["G", "I"] – suggestions get ⌘1–⌘5 automatically */
  keys?: string[];
  /** Extra words that should find this command */
  keywords?: string;
  /** The hint is a code (an id, a path): set in the mono face */
  mono?: boolean;
  /** Found by a search elsewhere (the Hub's): shown whatever the typed text matches here */
  always?: boolean;
  /** data-* attributes for the row (test hooks), e.g. { "data-cmd-item": "ST-2" } */
  data?: Record<string, string>;
};

export type CommandGroup = { id: string; label: string; commands: Command[] };

const defaultLabels = {
  placeholder: "Type a command or search…",
  field: "Command",
  list: "Commands",
  empty: "Nothing matches",
  tabTip: "to jump between sections",
  openTip: "to open",
};
export type CommandMenuLabels = Partial<typeof defaultLabels>;

export type CommandMenuProps = {
  groups?: CommandGroup[];
  /** Called with the command id */
  onRun?: (id: string, query?: string) => void;
  /** Runs on Esc in an empty search */
  onClose?: () => void;
  /** The typed text, as it changes – for a search the caller runs */
  onQueryChange?: (query: string) => void;
  /** A search the caller runs is under way */
  busy?: boolean;
  labels?: CommandMenuLabels;
  /** Focus the field when the menu appears */
  autoFocus?: boolean;
  className?: string;
};

/** Lowercase and without accents, so "munchen" finds "München" */
const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
/** First whole letter or digit – no half emoji */
const initial = (s: string) => (s.match(/[\p{L}\p{N}]/u)?.[0] ?? "").toUpperCase();

/** The typed text, bold where it matches */
function Highlight({ text, query }: { text: string; query: string }) {
  const folded = fold(text);
  const needle = fold(query);
  // Only mark when folding kept every character in place – otherwise the mark would land on the wrong letters
  const i = needle && folded.length === text.length ? folded.indexOf(needle) : -1;
  if (i < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, i)}
      <mark>{text.slice(i, i + needle.length)}</mark>
      {text.slice(i + needle.length)}
    </>
  );
}

/** A command with a key of its own, so duplicate or odd ids never collide */
type Row = Command & { key: string; n: number };

/** A broken image logo falls back to the command's first letter */
const onBroken = (e: SyntheticEvent<HTMLElement>) => {
  e.currentTarget.dataset.broken = "";
};

type ItemProps = {
  cmd: Row;
  domId: string;
  selected: boolean;
  ran: boolean;
  mod: string;
  q: string;
  on: { current: { move: (cmd: Row) => void; run: (cmd: Row) => void } };
};

/** One row – memoized, so moving the highlight through thousands of commands only redraws two rows */
const Item = memo(function Item({ cmd, domId, selected, ran, mod, q, on }: ItemProps) {
  const keys = cmd.n ? [mod, String(cmd.n)] : cmd.keys;
  return (
    <button
      id={domId}
      data-key={cmd.key}
      type="button"
      role="option"
      aria-selected={selected}
      data-ran={ran || undefined}
      className="ui-cmc__item"
      tabIndex={-1}
      {...cmd.data}
      onMouseMove={() => on.current.move(cmd)}
      onClick={() => on.current.run(cmd)}
    >
      {cmd.logo ? (
        <span className="ui-cmc__logo" data-letter={initial(cmd.label)} onErrorCapture={onBroken}>
          {cmd.logo}
        </span>
      ) : (
        <span className="ui-cmc__icon" data-letter={initial(cmd.label)} onErrorCapture={onBroken}>
          {cmd.icon}
        </span>
      )}
      <span className="ui-cmc__text">
        <span className="ui-cmc__name" dir="auto" title={cmd.label}>
          <Highlight text={cmd.label} query={q} />
        </span>
        {cmd.hint && (
          <span className="ui-cmc__hint" dir="auto" title={cmd.hint} data-mono={cmd.mono || undefined}>
            <Highlight text={cmd.hint} query={q} />
          </span>
        )}
      </span>
      {keys && (
        <span className="ui-cmc__keys" aria-hidden="true">
          {keys.map((k, i) => (
            <kbd key={i}>{k}</kbd>
          ))}
        </span>
      )}
    </button>
  );
});

const isMac = () => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export function CommandMenu({
  groups = [],
  onRun,
  onClose,
  onQueryChange,
  busy,
  labels: labelProp,
  autoFocus,
  className,
}: CommandMenuProps) {
  const labels = { ...defaultLabels, ...labelProp };
  const id = "ui-cmc" + useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const [listHeight, setListHeight] = useState(0);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState<string | null>(null);
  const [ran, setRan] = useState<{ id: string; label: string } | null>(null);
  const [pill, setPill] = useState<{ y: number; h: number } | null>(null);
  // The highlight shows while the pointer is over the list or the keyboard moves – leaving the list fades it out
  const [lit, setLit] = useState(true);
  const mod = isMac() ? "⌘" : "Ctrl";

  // Every row gets a key of its own and a search text, once per groups – missing labels or commands from an API never crash
  const all = useMemo(
    () =>
      (groups ?? []).filter(Boolean).map((g, gi) => ({
        key: `g${gi}`,
        id: g.id,
        label: String(g.label ?? ""),
        commands: (Array.isArray(g.commands) ? g.commands : []).filter(Boolean).map((c, ci) => {
          const label = String(c.label ?? "");
          const hint = c.hint == null || c.hint === "" ? undefined : String(c.hint);
          const row: Row = { ...c, label, hint, key: `${gi}-${ci}`, n: 0 };
          return { row, always: !!c.always, text: fold(`${label} ${hint ?? ""} ${c.keywords ?? ""}`) };
        }),
      })),
    [groups],
  );
  const q = query.trim();
  const shown = useMemo(() => {
    const needle = fold(q);
    const result = all
      .map((g) => ({
        ...g,
        commands: g.commands
          .filter((c) => !needle || c.always || c.text.includes(needle))
          // ⌘1–⌘5 go to the first five suggestions shown – none beyond, since ⌘6 does nothing
          .map((c, i) => (g.id === "suggestions" && i < 5 ? { ...c.row, n: i + 1 } : c.row)),
      }))
      .filter((g) => g.commands.length);
    return result;
  }, [all, q]);
  const order = useMemo(() => shown.flatMap((g) => g.commands), [shown]);
  const suggestions = order.filter((c) => c.n);
  const current = order.find((c) => c.key === active) ?? order[0];

  // Keyboard moves keep the active row in view; the mouse never scrolls the list
  const byKeyboard = useRef(false);

  // The highlight glides to the active row
  useLayoutEffect(() => {
    const list = listRef.current;
    const row = list?.querySelector<HTMLElement>(`[data-key="${current?.key}"]`);
    if (!list || !row) return setPill(null);
    setPill({ y: row.offsetTop, h: row.offsetHeight });
    if (!byKeyboard.current) return;
    byKeyboard.current = false;
    // Scroll only the list (never the page), keeping a little room and the group title above the first row in view
    const pad = 8;
    const label = row.previousElementSibling?.classList.contains("ui-cmc__label") ? (row.previousElementSibling as HTMLElement).offsetHeight : 0;
    const top = row.offsetTop - pad - label;
    const bottom = row.offsetTop + row.offsetHeight + pad;
    if (top < list.scrollTop) list.scrollTop = top;
    else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight;
  }, [current?.key, order.length]);

  // The list's height follows its rows (up to its max height), so searching makes the menu glide smaller, not jump
  useLayoutEffect(() => {
    const list = listRef.current;
    const inner = innerRef.current;
    if (!list || !inner) return;
    const measure = () => {
      const style = getComputedStyle(list);
      const pad = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
      const max = parseFloat(style.maxHeight) || Infinity;
      setListHeight(Math.min(inner.offsetHeight + pad, max));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(inner);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    if (autoFocus) input.current?.focus();
  }, [autoFocus]);

  // A short confirmation in the last row after running a command
  useEffect(() => {
    if (!ran) return;
    const t = window.setTimeout(() => setRan(null), 1800);
    return () => window.clearTimeout(t);
  }, [ran]);

  const run = (cmd: Row) => {
    setActive(cmd.key);
    setRan({ id: cmd.key, label: cmd.label });
    onRun?.(cmd.id, q);
  };

  // The rows call the latest handlers through a ref, so they don't redraw when the handlers change
  const on = useRef({ move: (_: Row) => {}, run: (_: Row) => {} });
  on.current = {
    move: (cmd) => {
      setLit(true);
      if (cmd.key !== current?.key) setActive(cmd.key);
    },
    run: (cmd) => {
      run(cmd);
      input.current?.focus({ preventScroll: true });
    },
  };

  const move = (step: number) => {
    if (!order.length || !current) return;
    byKeyboard.current = true;
    setLit(true);
    const i = order.findIndex((c) => c.key === current.key);
    setActive(order[(i + step + order.length) % order.length].key);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      move(e.key === "ArrowDown" ? 1 : -1);
    } else if (e.key === "Enter" && current) {
      e.preventDefault();
      run(current);
    } else if (e.key === "Tab" && shown.length > 1) {
      // Tab jumps to the first row of the next section, Shift Tab to the previous one
      e.preventDefault();
      const g = shown.findIndex((group) => group.commands.some((c) => c.key === current?.key));
      const next = e.shiftKey ? (g <= 0 ? shown.length - 1 : g - 1) : (g + 1) % shown.length;
      byKeyboard.current = true;
      setLit(true);
      setActive(shown[next].commands[0].key);
    } else if (e.key === "Escape") {
      // A typed search is cleared first – the dialog around the menu must not close on the same key
      if (query) {
        e.preventDefault();
        e.nativeEvent.stopPropagation();
        setQuery("");
        onQueryChange?.("");
      } else onClose?.();
    } else if ((e.metaKey || e.ctrlKey) && /^[1-5]$/.test(e.key)) {
      const cmd = suggestions[Number(e.key) - 1];
      if (cmd) {
        e.preventDefault();
        run(cmd);
      }
    }
  };

  return (
    <div className={["ui-cmc", className].filter(Boolean).join(" ")}>
      {/* React 19 hoists this into <head> once, no matter how many instances render */}
      <style href="ui-command-menu-clean" precedence="default">
        {css}
      </style>

      <div className="ui-cmc__card">
        <label className="ui-cmc__search">
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="7.25" cy="7.25" r="4.75" />
            <path d="m13.5 13.5-2.9-2.9" />
          </svg>
          <input
            ref={input}
            value={query}
            dir="auto"
            placeholder={labels.placeholder}
            aria-label={labels.field}
            data-command-input=""
            role="combobox"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={current ? `${id}-${current.key}` : undefined}
            onChange={(e) => {
              setQuery(e.target.value);
              onQueryChange?.(e.target.value);
              setActive(null);
            }}
            onKeyDown={onKeyDown}
          />
          {busy ? (
            <span className="ui-cmc__busy" aria-hidden="true" data-command-busy="" />
          ) : (
            <kbd className="ui-cmc__enter" aria-hidden="true">
              <svg viewBox="0 0 16 16">
                <path d="M12.5 4v3a2 2 0 0 1-2 2h-7M6 6.5 3.5 9 6 11.5" />
              </svg>
            </kbd>
          )}
        </label>

        <div
          ref={listRef}
          id={`${id}-list`}
          className="ui-cmc__list"
          role="listbox"
          aria-label={labels.list}
          style={listHeight ? { height: listHeight } : undefined}
          onMouseLeave={() => setLit(false)}
        >
          {pill && <span className="ui-cmc__pill" data-lit={lit || undefined} style={{ transform: `translateY(${pill.y}px)`, height: pill.h }} aria-hidden="true" />}
          <div ref={innerRef} className="ui-cmc__inner">
          {!order.length && (
            <p className="ui-cmc__empty" data-command-empty="">
              {labels.empty}
            </p>
          )}
          {shown.map((group) => (
            <div key={group.key} role="group" aria-label={group.label} className="ui-cmc__group" data-command-group={group.id}>
              {group.label && (
                <span className="ui-cmc__label" dir="auto">
                  {group.label}
                </span>
              )}
              {group.commands.map((cmd) => (
                <Item
                  key={cmd.key}
                  cmd={cmd}
                  domId={`${id}-${cmd.key}`}
                  selected={cmd.key === current?.key}
                  ran={ran?.id === cmd.key}
                  mod={mod}
                  q={q}
                  on={on}
                />
              ))}
            </div>
          ))}
          </div>
        </div>

        {/* The last row of the panel, under a hairline: a hint, or what just ran */}
        <footer className="ui-cmc__foot" aria-live="polite">
          {ran ? (
            <span className="ui-cmc__done" key={ran.label}>
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="m3.5 8.5 3 3 6-7" />
              </svg>
              <span className="ui-cmc__done-text">{ran.label}</span>
            </span>
          ) : (
            <span className="ui-cmc__tip">
              <kbd>Tab</kbd> {labels.tabTip}
            </span>
          )}
          <span className="ui-cmc__foot-right">
            <span className="ui-cmc__keys">
              <kbd>{mod}</kbd>
              <kbd>K</kbd>
            </span>
            {labels.openTip}
          </span>
        </footer>
      </div>
    </div>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-cmc */
.ui-cmc {
  --ui-cmc-card: var(--aui-overlay, #ffffff);
  --ui-cmc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-cmc-strong: var(--aui-ink, #000000);
  --ui-cmc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-cmc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-cmc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-cmc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-cmc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-cmc-line-strong: var(--aui-border-strong, rgba(0, 0, 0, 0.1));
  --ui-cmc-active: var(--aui-subtle, rgba(0, 0, 0, 0.06));
  --ui-cmc-tile: var(--aui-surface, #ffffff);
  --ui-cmc-tile-ring: var(--aui-border-strong, rgba(0, 0, 0, 0.1));
  --ui-cmc-mark: color-mix(in srgb, var(--aui-primary, #0060e2) 16%, transparent);
  --ui-cmc-accent: var(--aui-primary, rgba(0, 96, 226, 0.765));
  --ui-cmc-ok: var(--aui-success, #1e8a4f);
  /* Strokes are shadows: a 0.5px hairline plus a soft lift, like a popover */
  --ui-cmc-shadow: var(--aui-shadow-popover, 0 0 0 0.5px var(--aui-border, rgba(0, 0, 0, 0.08)), 0 12px 28px -10px rgba(0, 0, 0, 0.16), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-cmc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  box-sizing: border-box;
  container-type: inline-size;
  display: grid;
  /* One column that can shrink – a long confirmation never widens the menu */
  grid-template-columns: minmax(0, 1fr);
  width: 100%;
  max-width: 600px;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: calc(14px * var(--type-scale, 1));
  line-height: 22px;
  font-weight: 400;
  color: var(--ui-cmc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-cmc *, .ui-cmc *::before, .ui-cmc *::after { box-sizing: border-box; }
:where(.ui-cmc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-cmc) svg { display: block; flex: none; }

/* One white panel: search, results and the last row – the edge is a shadow, not a border */
.ui-cmc__card { overflow: hidden; border-radius: calc(var(--aui-radius-lg, 10px) + 2px); background: var(--ui-cmc-card); box-shadow: var(--ui-cmc-shadow); }

/* Search row */
.ui-cmc__search { display: flex; align-items: center; gap: 12px; height: 48px; padding: 0 12px 0 14px; border-bottom: 0.5px solid var(--ui-cmc-line); cursor: text; }
.ui-cmc__search > svg { width: 16px; height: 16px; fill: none; stroke: var(--ui-cmc-icon); stroke-width: 1.1; stroke-linecap: round; }
.ui-cmc__search input { flex: 1; min-width: 0; height: 20px; padding: 0; border: 0; background: transparent; font: inherit; font-size: calc(14px * var(--type-scale, 1)); line-height: 20px; color: var(--ui-cmc-ink); caret-color: var(--ui-cmc-accent); outline: none; }
.ui-cmc__search input::placeholder { color: var(--ui-cmc-faint); }
.ui-cmc__search input:focus-visible { outline: none; }
.ui-cmc__enter svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.1; stroke-linecap: round; stroke-linejoin: round; }

/* Results */
.ui-cmc__list { position: relative; max-height: 360px; padding: 0 4px 4px; overflow-y: auto; scrollbar-width: thin; transition: height 320ms var(--ui-cmc-ease); }
.ui-cmc__group { display: flex; flex-direction: column; }
.ui-cmc__label { padding: 10px 8px 4px; overflow-wrap: anywhere; font-size: calc(12px * var(--type-scale, 1)); font-weight: 500; line-height: 16px; color: var(--ui-cmc-muted); }
/* With text typed, the assistant row sits on top without a label */
.ui-cmc__group:first-child:not(:has(.ui-cmc__label)) { padding-top: 4px; }
/* The highlight follows the active row */
.ui-cmc__pill {
  position: absolute;
  top: 0;
  left: 4px;
  right: 4px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  background: var(--ui-cmc-active);
  opacity: 0;
  /* A short, crisp move between rows – no long glide; it fades out when the pointer leaves the list */
  transition: transform 90ms ease-out, height 90ms ease-out, opacity 140ms ease;
}
.ui-cmc__pill[data-lit] { opacity: 1; }
.ui-cmc__item { position: relative; display: flex; align-items: center; gap: 10px; height: 36px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); text-align: start; }
.ui-cmc__logo {
  display: grid;
  place-items: center;
  flex: none;
  width: 20px;
  height: 20px;
  border-radius: 5px;
  background: var(--ui-cmc-tile);
  box-shadow: 0 0 0 0.5px var(--ui-cmc-tile-ring), 0 1px 1px -1px rgba(0, 0, 0, 0.1);
  color: var(--ui-cmc-strong);
}
.ui-cmc__logo svg { width: 12px; height: 12px; }
/* Image logos fit the tile; a broken one shows the command's first letter instead */
.ui-cmc__logo, .ui-cmc__icon { overflow: hidden; }
.ui-cmc__logo img { width: 12px; height: 12px; object-fit: contain; }
.ui-cmc__icon img { width: 16px; height: 16px; object-fit: contain; }
.ui-cmc__logo[data-broken] > *, .ui-cmc__icon[data-broken] > * { display: none; }
.ui-cmc__logo[data-broken]::after, .ui-cmc__icon[data-broken]::after { content: attr(data-letter); font-size: calc(10px * var(--type-scale, 1)); font-weight: 600; line-height: 1; color: var(--ui-cmc-text); }
.ui-cmc__icon { display: grid; place-items: center; flex: none; width: 20px; height: 20px; color: var(--ui-cmc-icon); transition: color 140ms ease; }
/* Steve: lucide glyphs and a status dot sit in the icon box at the size the rest of Steve uses */
.ui-cmc__icon > svg { width: 16px; height: 16px; }
.ui-cmc__empty { margin: 0; padding: 28px 8px; text-align: center; font-size: calc(13px * var(--type-scale, 1)); line-height: 20px; color: var(--ui-cmc-muted); }
.ui-cmc__busy { flex: none; width: 14px; height: 14px; margin: 0 4px; border-radius: 50%; border: 1.5px solid var(--ui-cmc-line-strong); border-top-color: var(--ui-cmc-icon); animation: ui-cmc-spin 700ms linear infinite; }
@keyframes ui-cmc-spin { to { transform: rotate(360deg); } }
.ui-cmc__hint[data-mono] { font-family: var(--aui-font-mono, ui-monospace, monospace); font-size: calc(12px * var(--type-scale, 1)); }
.ui-cmc__glyph { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.1; stroke-linecap: round; stroke-linejoin: round; }
.ui-cmc__glyph path { vector-effect: non-scaling-stroke; }
.ui-cmc__item[aria-selected="true"] .ui-cmc__icon { color: var(--ui-cmc-ink); }
.ui-cmc__text { display: flex; flex: 1; align-items: baseline; gap: 8px; min-width: 0; }
.ui-cmc__name { overflow: hidden; color: var(--ui-cmc-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-cmc__hint { overflow: hidden; color: var(--ui-cmc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-cmc mark { border-radius: 3px; background: var(--ui-cmc-mark); color: inherit; }

/* Keyboard hints: small chips with an inset hairline */
.ui-cmc__keys { display: inline-flex; flex: none; gap: 2px; }
.ui-cmc kbd {
  display: inline-grid;
  place-items: center;
  min-width: 20px;
  height: 20px;
  padding: 0 5px;
  border-radius: var(--aui-radius-sm, 5px);
  box-shadow: inset 0 0 0 0.5px var(--ui-cmc-line-strong);
  font-family: inherit;
  font-size: calc(11px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 16px;
  color: var(--ui-cmc-muted);
  transition: color 140ms ease;
}
.ui-cmc__item[aria-selected="true"] kbd { color: var(--ui-cmc-text); }
.ui-cmc__enter { padding: 0 4px; color: var(--ui-cmc-icon); }
/* A short press on the row that just ran */
.ui-cmc__item[data-ran] { animation: ui-cmc-ran 600ms var(--ui-cmc-ease); }
@keyframes ui-cmc-ran { 30% { transform: scale(0.985); } }

/* The last row of the panel */
.ui-cmc__foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  height: 40px;
  padding: 0 12px;
  border-top: 0.5px solid var(--ui-cmc-line);
  font-size: calc(12px * var(--type-scale, 1));
  line-height: 16px;
  color: var(--ui-cmc-muted);
}
.ui-cmc__foot > span { display: inline-flex; align-items: center; gap: 6px; min-width: 0; white-space: nowrap; }
.ui-cmc__done { color: var(--ui-cmc-ink); animation: ui-cmc-in 260ms var(--ui-cmc-ease); }
.ui-cmc__done-text { overflow: hidden; text-overflow: ellipsis; }
.ui-cmc__done svg { width: 16px; height: 16px; fill: none; stroke: var(--ui-cmc-ok); stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; }
.ui-cmc__foot-right { flex: none; }
@keyframes ui-cmc-in { from { opacity: 0; transform: translateY(3px); } to { opacity: 1; transform: none; } }

/* Narrow (phones): no shortcut keys or hints – there's no keyboard to use them with */
@container (max-width: 480px) {
  .ui-cmc__item .ui-cmc__keys, .ui-cmc__foot > .ui-cmc__foot-right, .ui-cmc__foot > .ui-cmc__tip, .ui-cmc__enter, .ui-cmc__hint { display: none; }
  .ui-cmc__item { height: 40px; }
  .ui-cmc__list { max-height: 360px; }
  .ui-cmc__foot:not(:has(.ui-cmc__done)) { display: none; }
}
/* Touch screens: taller rows */
@media (hover: none) and (pointer: coarse) {
  .ui-cmc__item { height: 44px; }
  .ui-cmc__search { height: 52px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-cmc *, .ui-cmc *::before, .ui-cmc *::after { animation: none !important; transition: none !important; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-cmc {
  --ui-cmc-card: var(--aui-overlay, #1c1c1e);
  --ui-cmc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-cmc-strong: var(--aui-ink, #ffffff);
  --ui-cmc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-cmc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-cmc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-cmc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-cmc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-cmc-line-strong: var(--aui-border-strong, rgba(255, 255, 255, 0.12));
  --ui-cmc-active: var(--aui-subtle, rgba(255, 255, 255, 0.08));
  --ui-cmc-tile: var(--aui-surface, #2a2a2d);
  --ui-cmc-tile-ring: var(--aui-border-strong, rgba(255, 255, 255, 0.12));
  --ui-cmc-mark: color-mix(in srgb, var(--aui-primary, #4d8dff) 28%, transparent);
  --ui-cmc-accent: var(--aui-primary, #4fb0fe);
  --ui-cmc-ok: var(--aui-green, #5fc88a);
  --ui-cmc-shadow: var(--aui-shadow-popover, 0 0 0 0.5px var(--aui-border, rgba(255, 255, 255, 0.1)), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
}
`;
