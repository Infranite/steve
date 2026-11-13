/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

/*
 * Activity – what was done, in a single file.
 * Not a log of API calls: every row is one thing done, said as a sentence, with the time, a mark for what kind of
 * thing it was, what it was done to and who did it, by a coloured dot (a Bot on its own, YOLO for you, you).
 * Grouped by day under quiet heads with a count. A row opens under itself: what exactly happened and why it could.
 * A run of the same thing folded into one row opens into its steps.
 * Filter by who, or search. The keyboard: J / K (↑ ↓) move from row to row, Enter opens, / searches.
 * Focus rings show for the keyboard only – never on a text field.
 *
 * Steve: taken from the affectUI Agent / Activity screen without its App Shell (the page sits in Steve's settings),
 * its sample apps and steps, its Undo / Final column and cost (Steve's audit trail has neither) and Hugeicons (the
 * caller passes its own glyphs). Who did it is the caller's list (`whos`), not the fixed on-its-own / you-approved
 * pair. The colours are the --aui-* tokens, so it follows Steve's theme in both modes.
 */

import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

/* ---------- Types ---------- */

/** Who can have done a step: the filter tab, and the dot's colour */
export type ActivityWho = { id: string; label: string; tone: "auto" | "approved" | "self" | "other" };
/** One thing that was done */
export type ActivityItem = {
  id: string;
  at: number;
  /** A small glyph for the kind of thing it was */
  mark?: ReactNode;
  /** What was done, as a sentence */
  text: string;
  /** What it was done to, or a few words on what changed – after who did it */
  task?: string;
  /** The id of an entry of `whos` */
  by: string;
  /** Who by name, when the who's label is not enough (another member) */
  byName?: string;
  /** Exactly what – "When" · "11月13日 14:20" */
  details?: { label: string; value: ReactNode; mono?: boolean }[];
  /** Why it could: one sentence */
  why?: string;
  /** A link out of the step: label and what it does */
  open?: { label: string; run: () => void };
  /** The steps folded into this one, the newest first (this row is the newest of them) */
  steps?: ActivityItem[];
  /** data-* attributes for the row (test hooks) */
  data?: Record<string, string>;
};

/* ---------- Labels and formats ---------- */

const defaultLabels = {
  all: "All",
  search: "Search activity",
  what: "What happened",
  why: "Why it could",
  noMatch: "Nothing matches",
  noMatchText: "Try other words – or show everything.",
  showAll: "Show all",
  steps: "{count} times",
  filters: "Show",
};
export type ActivityLabels = Partial<typeof defaultLabels>;
type Labels = typeof defaultLabels;
const NO_LABELS: ActivityLabels = {};

const pad = (n: number) => String(n).padStart(2, "0");
const time = (at: number) => { const d = new Date(at); return `${pad(d.getHours())}:${pad(d.getMinutes())}`; };
const fill = (s: string, vars: Record<string, string>) => s.replace(/\{(\w+)\}/g, (_, k) => vars[k] ?? "");
const dayKey = (at: number) => new Date(at).toDateString();
const isTyping = (el: Element | null) => !!el && (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || (el as HTMLElement).isContentEditable);

/* ---------- List ---------- */

export type ActivityListProps = {
  /** Everything that was done, newest first */
  items: ActivityItem[];
  /** Who can have done a step, in the order of the filter tabs; a who no step has is left out */
  whos: ActivityWho[];
  /** The name of a day: Today, Yesterday or its date */
  dayName?: (at: number) => string;
  labels?: ActivityLabels;
  /** Under the list (a "load more" button) */
  footer?: ReactNode;
  className?: string;
};

export function ActivityList({ items, whos, dayName, labels: labelProp = NO_LABELS, footer, className }: ActivityListProps) {
  const labels = { ...defaultLabels, ...labelProp };
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const q = query.trim().toLowerCase();
  const size = (a: ActivityItem) => a.steps?.length || 1;
  const count = (by: string) => items.reduce((n, a) => n + (a.by === by ? size(a) : 0), 0);
  const total = items.reduce((n, a) => n + size(a), 0);
  const present = whos.filter((w) => count(w.id) > 0);
  const filters = present.length > 1 ? ["all", ...present.map((w) => w.id)] : [];
  const who = (id: string) => whos.find((w) => w.id === id);
  const shown = items.filter(
    (a) => (filter === "all" || a.by === filter) && (!q || `${a.text} ${a.task ?? ""} ${a.byName ?? who(a.by)?.label ?? ""}`.toLowerCase().includes(q)),
  );
  const days: { key: string; name: string; items: ActivityItem[]; n: number }[] = [];
  for (const a of shown) {
    const k = dayKey(a.at);
    const last = days[days.length - 1];
    if (last?.key === k) { last.items.push(a); last.n += size(a); }
    else days.push({ key: k, name: dayName ? dayName(a.at) : new Date(a.at).toLocaleDateString(), items: [a], n: size(a) });
  }

  // J / K and ↑ ↓ from row to row; Enter opens (the row is a button); / searches
  const rows = () => [...(listRef.current?.querySelectorAll<HTMLElement>(".ui-aga-row__main") ?? [])];
  const step = (dir: number) => {
    const list = rows();
    if (!list.length) return;
    const at = list.indexOf(document.activeElement as HTMLElement);
    const next = at < 0 ? (open ? list.find((r) => r.dataset.id === open) ?? list[0] : list[0]) : list[Math.min(list.length - 1, Math.max(0, at + dir))];
    next?.focus();
  };
  const onKey = (e: globalThis.KeyboardEvent) => {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return;
    const t = document.activeElement;
    if (t && t !== document.body && !root.current?.contains(t)) return;
    if (isTyping(t)) return;
    // A dialog or menu over the page has the keys
    if (document.querySelector("[role=dialog],[role=menu]")) return;
    const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    if (k === "/") {
      e.preventDefault();
      return searchRef.current?.focus();
    }
    if (k === "j" || k === "k") {
      e.preventDefault();
      return step(k === "j" ? 1 : -1);
    }
  };
  const keyRef = useRef(onKey);
  keyRef.current = onKey;
  useEffect(() => {
    const h = (e: globalThis.KeyboardEvent) => keyRef.current(e);
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);
  const onListKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      step(e.key === "ArrowDown" ? 1 : -1);
    }
  };
  const rove = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const i = filters.indexOf(filter);
    const n = filters[(i + (e.key === "ArrowRight" ? 1 : -1) + filters.length) % filters.length];
    setFilter(n);
    e.currentTarget.querySelector<HTMLElement>(`[data-filter="${n}"]`)?.focus();
  };
  const current = filters.includes(filter) ? filter : "all";

  return (
    <div ref={root} className={["ui-aga", className].filter(Boolean).join(" ")} data-activity="">
      <style href="ui-screen-agent-activity" precedence="default">
        {css}
      </style>
      <header className="ui-aga-head">
        {filters.length > 0 && (
          <div className="ui-aga-seg" role="group" aria-label={labels.filters} onKeyDown={rove}>
            {filters.map((f) => (
              <button key={f} type="button" data-filter={f} aria-pressed={current === f} tabIndex={current === f ? 0 : -1} onClick={() => setFilter(f)}>
                {f !== "all" && <span className="ui-aga-dot" data-tone={who(f)?.tone} aria-hidden="true" />}
                <span className="ui-aga-seg__name">{f === "all" ? labels.all : who(f)?.label}</span>
                <span className="ui-aga-seg__n">{f === "all" ? total : count(f)}</span>
              </button>
            ))}
          </div>
        )}
        <label className="ui-aga-search">
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="7.25" cy="7.25" r="4.75" />
            <path d="m13.5 13.5-2.9-2.9" />
          </svg>
          <input
            ref={searchRef}
            type="search"
            value={query}
            placeholder={labels.search}
            aria-label={labels.search}
            aria-keyshortcuts="/"
            aria-controls="ui-aga-list"
            dir="auto"
            data-activity-search=""
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                if (query) setQuery("");
                else e.currentTarget.blur();
              } else if (e.key === "ArrowDown") {
                e.preventDefault();
                rows()[0]?.focus();
              }
            }}
          />
          <kbd aria-hidden="true">/</kbd>
        </label>
      </header>

      {days.length === 0 ? (
        <div id="ui-aga-list" className="ui-aga-center" data-activity-nomatch="">
          <p className="ui-aga-center__title">{labels.noMatch}</p>
          <p className="ui-aga-center__text">{labels.noMatchText}</p>
          <button type="button" className="ui-aga-btn" onClick={() => { setQuery(""); setFilter("all"); }}>
            {labels.showAll}
          </button>
        </div>
      ) : (
        <div ref={listRef} id="ui-aga-list" className="ui-aga-list" onKeyDown={onListKey}>
          {days.map((d) => (
            <section key={d.key} className="ui-aga-day" aria-label={d.name} data-activity-day={d.name}>
              <DayHead name={d.name} count={d.n} />
              <ul className="ui-aga-rows">
                {d.items.map((a) => (
                  <ActivityRow key={a.id} item={a} who={who(a.by)} whoOf={who} labels={labels} open={open === a.id} onOpen={() => setOpen(open === a.id ? null : a.id)} />
                ))}
              </ul>
            </section>
          ))}
        </div>
      )}
      {footer}
    </div>
  );
}

/** A day: its name and how many steps it holds, on a quiet gray head */
function DayHead({ name, count }: { name: string; count: number }) {
  return (
    <h2 className="ui-aga-dhead">
      <span>{name}</span>
      <span className="ui-aga-dhead__n">{count}</span>
    </h2>
  );
}

/**
 * One step: the time, its mark, the sentence and under it who did it and on what. It opens under itself: what exactly
 * happened and why it could – or, for a folded run, its steps, each of which opens the same way
 */
function ActivityRow({ item: a, who, whoOf, labels, open, onOpen, nested }: { item: ActivityItem; who?: ActivityWho; whoOf: (id: string) => ActivityWho | undefined; labels: Labels; open: boolean; onOpen: () => void; nested?: boolean }) {
  const name = a.byName ?? who?.label ?? a.by;
  const folded = (a.steps?.length ?? 0) > 1;
  const [sub, setSub] = useState<string | null>(null);
  const span = folded ? `${time(a.steps![a.steps!.length - 1].at)} – ${time(a.at)}` : time(a.at);
  return (
    <li className="ui-aga-row" data-id={a.id} data-open={open || undefined} data-nested={nested || undefined} {...a.data}>
      <button
        type="button"
        className="ui-aga-row__main"
        data-id={a.id}
        aria-expanded={open}
        aria-label={`${span}, ${a.text}, ${name}${a.task ? `, ${a.task}` : ""}${folded ? `, ${fill(labels.steps, { count: String(a.steps!.length) })}` : ""}`}
        onClick={onOpen}
      >
        <span className="ui-aga-num ui-aga-row__time">{time(a.at)}</span>
        {!nested && <span className="ui-aga-mark" aria-hidden="true">{a.mark}</span>}
        <span className="ui-aga-row__text">
          <span className="ui-aga-row__line" dir="auto" title={a.text}>
            <span className="ui-aga-row__sentence" data-activity-text="">{a.text}</span>
            {nested && a.task && <span className="ui-aga-row__task" data-activity-task="">· {a.task}</span>}
            {folded && <span className="ui-aga-tag" data-activity-fold={a.steps!.length}>×{a.steps!.length}</span>}
          </span>
          {!nested && (
            <span className="ui-aga-row__meta" data-activity-meta="">
              <span className="ui-aga-who">
                <span className="ui-aga-dot" data-tone={who?.tone} aria-hidden="true" />
                {name}
              </span>
              {folded && <span className="ui-aga-num ui-aga-row__task">· {span}</span>}
              {a.task && (
                <span className="ui-aga-row__task" dir="auto" title={a.task}>
                  · {a.task}
                </span>
              )}
            </span>
          )}
        </span>
        <svg className="ui-aga-row__turn" viewBox="0 0 16 16" aria-hidden="true">
          <path d="m6 4 4 4-4 4" />
        </svg>
      </button>
      {open && (folded ? (
        <ul className="ui-aga-rows ui-aga-steps" data-activity-steps="">
          {a.steps!.map((x) => (
            <ActivityRow key={x.id} item={x} who={whoOf(x.by)} whoOf={whoOf} labels={labels} open={sub === x.id} onOpen={() => setSub(sub === x.id ? null : x.id)} nested />
          ))}
        </ul>
      ) : (
        <ActivityPanel item={a} who={who} labels={labels} />
      ))}
    </li>
  );
}

/** The step, open under its row: what exactly happened, and why it could */
function ActivityPanel({ item: a, who, labels }: { item: ActivityItem; who?: ActivityWho; labels: Labels }) {
  return (
    <div className="ui-aga-panel" data-activity-detail="">
      {a.details?.length ? (
        <section className="ui-aga-section" aria-label={labels.what}>
          <h3 className="ui-aga-label">{labels.what}</h3>
          <dl className="ui-aga-card ui-aga-details">
            {a.details.map((d, i) => (
              <div key={i}>
                <dt>{d.label}</dt>
                <dd dir="auto" data-mono={d.mono || undefined}>{d.value}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}
      {a.why && (
        <section className="ui-aga-section" aria-label={labels.why}>
          <h3 className="ui-aga-label">{labels.why}</h3>
          <div className="ui-aga-card ui-aga-why">
            <span className="ui-aga-who">
              <span className="ui-aga-dot" data-tone={who?.tone} aria-hidden="true" />
              {a.byName ?? who?.label}
            </span>
            <p className="ui-aga-why__text">{a.why}</p>
          </div>
        </section>
      )}
      {a.open && (
        <p className="ui-aga-open">
          <button type="button" className="ui-aga-link--text" onClick={a.open.run} data-activity-open="">
            {a.open.label}
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="m6 4 4 4-4 4" />
            </svg>
          </button>
        </p>
      )}
    </div>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
.ui-aga {
  --ui-aga-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-aga-muted: var(--aui-muted, rgba(0, 0, 0, 0.55));
  --ui-aga-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.35));
  --ui-aga-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-aga-chip: var(--aui-subtle, rgba(0, 0, 0, 0.05));
  --ui-aga-hover: color-mix(in srgb, var(--aui-subtle, rgba(0, 0, 0, 0.05)) 60%, transparent);
  --ui-aga-card: var(--aui-surface, #ffffff);
  --ui-aga-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-aga-out: var(--aui-ease, cubic-bezier(0.23, 1, 0.32, 1));
  --ui-aga-card-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-aga-head: color-mix(in srgb, var(--aui-ink, #000) 4%, transparent);
  --ui-aga-auto: var(--aui-primary, #0190fe);
  --ui-aga-approved: var(--aui-amber, #e8b000);
  --ui-aga-self: var(--aui-green, #1e8a4f);
  --ui-aga-other: var(--aui-muted, rgba(0, 0, 0, 0.45));
  position: relative;
  container: ui-aga / inline-size;
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
  font-family: var(--aui-font, ui-sans-serif, system-ui, sans-serif);
  color: var(--ui-aga-ink);
}
.ui-aga *, .ui-aga *::before, .ui-aga *::after { box-sizing: border-box; }
:where(.ui-aga) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-aga) svg { display: block; flex: none; }
.ui-aga-num { font-variant-numeric: tabular-nums; }
.ui-aga :focus-visible { outline: 2px solid var(--ui-aga-focus); outline-offset: 2px; }
.ui-aga :is(input, textarea):focus-visible { outline: none; }

/* ---------- Head: who did it, and the search ---------- */
.ui-aga-head { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; }
.ui-aga-seg { display: flex; flex: none; gap: 2px; max-width: 100%; }
.ui-aga-seg button { display: inline-flex; align-items: center; gap: 6px; height: 28px; padding: 0 8px; border-radius: var(--aui-radius-sm, 6px); font-size: 13px; color: var(--ui-aga-muted); white-space: nowrap; transition: background-color 120ms ease, color 120ms ease; }
.ui-aga-seg button:hover { color: var(--ui-aga-ink); }
.ui-aga-seg button[aria-pressed="true"] { background: var(--ui-aga-chip); color: var(--ui-aga-ink); font-weight: 500; }
.ui-aga-seg__n { font-size: 12px; font-weight: 400; color: var(--ui-aga-faint); font-variant-numeric: tabular-nums; }
.ui-aga-search { display: flex; flex: 0 1 240px; align-items: center; gap: 6px; min-width: 0; height: 30px; margin-left: auto; padding: 0 6px 0 8px; border-radius: var(--aui-radius-sm, 6px); background: var(--ui-aga-chip); color: var(--ui-aga-faint); cursor: text; transition: box-shadow 120ms ease, background-color 120ms ease; }
.ui-aga-search:focus-within { background: var(--ui-aga-card); box-shadow: 0 0 0 1px var(--aui-border-strong, rgba(0, 0, 0, 0.12)), 0 0 0 3px var(--aui-focus, transparent); }
.ui-aga-search > svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.3; stroke-linecap: round; }
.ui-aga-search input { flex: 1; min-width: 0; height: 100%; padding: 0; border: 0; background: none; font: inherit; font-size: 13px; color: var(--ui-aga-ink); outline: none; appearance: none; }
.ui-aga-search input::placeholder { color: var(--ui-aga-faint); }
.ui-aga-search input::-webkit-search-cancel-button { display: none; }
.ui-aga-search kbd { display: inline-grid; place-items: center; min-width: 18px; height: 18px; border-radius: 4px; box-shadow: inset 0 0 0 0.5px var(--aui-border-strong, rgba(0, 0, 0, 0.12)); font-family: inherit; font-size: 11px; line-height: 1; color: var(--ui-aga-faint); }
.ui-aga-search:focus-within kbd { display: none; }

/* ---------- List ---------- */
.ui-aga-list { display: grid; gap: 20px; }
.ui-aga-center { display: grid; justify-items: center; gap: 4px; padding: 48px 24px; text-align: center; }
.ui-aga-center__title { margin: 0; font-size: 14px; font-weight: 500; color: var(--ui-aga-ink); }
.ui-aga-center__text { margin: 0 0 10px; font-size: 13px; color: var(--ui-aga-muted); }
.ui-aga-day { display: grid; gap: 4px; }
.ui-aga-dhead { display: flex; align-items: center; gap: 8px; height: 30px; margin: 0; padding: 0 12px; border-radius: var(--aui-radius, 8px); background: var(--ui-aga-head); font-size: 12px; font-weight: 500; color: var(--ui-aga-ink); }
.ui-aga-dhead__n { min-width: 20px; padding: 0 5px; border-radius: var(--aui-radius-sm, 6px); background: var(--ui-aga-card); box-shadow: 0 0 0 0.5px var(--ui-aga-line); font-size: 11px; font-weight: 400; line-height: 18px; text-align: center; color: var(--ui-aga-muted); font-variant-numeric: tabular-nums; }
.ui-aga-rows { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }

.ui-aga-row { position: relative; display: grid; border-radius: var(--aui-radius, 8px); }
.ui-aga-row > .ui-aga-row__main:hover { background: var(--ui-aga-hover); }
.ui-aga-row[data-open] > .ui-aga-row__main { background: var(--ui-aga-chip); }
.ui-aga-row__main { display: flex; align-items: center; gap: 12px; min-width: 0; min-height: 56px; padding: 8px 10px 8px 12px; border-radius: var(--aui-radius, 8px); text-align: left; transition: background-color 120ms ease; }
.ui-aga-row[data-nested] > .ui-aga-row__main { min-height: 36px; padding-block: 6px; }
.ui-aga-row__main:focus-visible { outline-offset: -2px; }
.ui-aga-row__time { flex: none; width: 40px; font-size: 12px; color: var(--ui-aga-faint); }
.ui-aga-row__text { display: grid; flex: 1; min-width: 0; }
.ui-aga-row__line { display: flex; align-items: center; gap: 8px; min-width: 0; font-size: 14px; line-height: 22px; color: var(--ui-aga-ink); text-align: left; }
.ui-aga-row[data-nested] .ui-aga-row__line { font-size: 13px; line-height: 20px; color: var(--ui-aga-muted); }
.ui-aga-row__sentence { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-aga-row__meta { display: flex; align-items: center; gap: 5px; min-width: 0; font-size: 12px; line-height: 18px; color: var(--ui-aga-muted); white-space: nowrap; }
.ui-aga-row__task { min-width: 0; overflow: hidden; text-overflow: ellipsis; color: var(--ui-aga-faint); }
.ui-aga-row__turn { width: 14px; height: 14px; fill: none; stroke: var(--ui-aga-faint); stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; transition: transform 180ms var(--ui-aga-out); }
.ui-aga-row[data-open] > .ui-aga-row__main .ui-aga-row__turn { transform: rotate(90deg); }
.ui-aga-who { display: inline-flex; flex: none; align-items: center; gap: 6px; }
.ui-aga-dot { flex: none; width: 6px; height: 6px; border-radius: 50%; background: var(--ui-aga-other); }
.ui-aga-dot[data-tone="auto"] { background: var(--ui-aga-auto); }
.ui-aga-dot[data-tone="approved"] { background: var(--ui-aga-approved); }
.ui-aga-dot[data-tone="self"] { background: var(--ui-aga-self); }
.ui-aga-tag { flex: none; padding: 0 6px; border-radius: 5px; background: var(--ui-aga-chip); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-aga-muted); font-variant-numeric: tabular-nums; }

.ui-aga-mark { display: grid; flex: none; place-items: center; width: 28px; height: 28px; border-radius: var(--aui-radius, 8px); background: var(--ui-aga-chip); color: var(--ui-aga-muted); }
.ui-aga-mark > svg { width: 15px; height: 15px; }
.ui-aga-row[data-open] > .ui-aga-row__main .ui-aga-tag { background: var(--ui-aga-card); }
.ui-aga-row[data-open] > .ui-aga-row__main .ui-aga-mark { background: var(--ui-aga-card); box-shadow: 0 0 0 0.5px var(--ui-aga-line); }

.ui-aga-btn { display: inline-flex; flex: none; align-items: center; justify-content: center; gap: 6px; height: 28px; padding: 0 10px; border-radius: var(--aui-radius-sm, 6px); background: var(--ui-aga-card); box-shadow: var(--ui-aga-card-shadow); font-size: 13px; font-weight: 500; color: var(--ui-aga-ink); white-space: nowrap; }
.ui-aga-btn:active { transform: scale(0.97); }

/* ---------- Opened under the row ---------- */
.ui-aga-steps { margin: 2px 0 6px 52px; padding-left: 8px; box-shadow: inset 1px 0 0 var(--ui-aga-line); }
.ui-aga-panel { display: grid; gap: 14px; margin: 6px 0 12px 92px; animation: ui-aga-in 180ms var(--ui-aga-out); }
.ui-aga-steps .ui-aga-panel { margin-left: 52px; }
.ui-aga-section { display: grid; grid-template-columns: minmax(0, 1fr); gap: 6px; }
.ui-aga-label { margin: 0; font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-aga-muted); }
.ui-aga-card { margin: 0; border-radius: var(--aui-radius, 8px); background: var(--ui-aga-card); box-shadow: var(--ui-aga-card-shadow); }
.ui-aga-details { display: grid; gap: 4px; padding: 10px 12px; }
.ui-aga-details > div { display: grid; grid-template-columns: 96px minmax(0, 1fr); gap: 4px 16px; font-size: 12px; line-height: 18px; }
.ui-aga-details dt { color: var(--ui-aga-muted); overflow-wrap: anywhere; }
.ui-aga-details dd { margin: 0; color: var(--ui-aga-ink); overflow-wrap: anywhere; white-space: pre-wrap; }
.ui-aga-details dd[data-mono] { font-family: var(--aui-font-mono, ui-monospace, monospace); font-size: 11.5px; }
.ui-aga-why { display: grid; gap: 4px; padding: 10px 12px; font-size: 13px; }
.ui-aga-why .ui-aga-who { font-size: 12px; font-weight: 500; color: var(--ui-aga-ink); }
.ui-aga-why__text { margin: 0; line-height: 20px; color: var(--ui-aga-muted); }
.ui-aga-open { margin: 0; }
.ui-aga-why__rule { display: flex; align-items: center; margin: 4px 0 0; padding-top: 6px; box-shadow: inset 0 0.5px 0 var(--ui-aga-line); }
.ui-aga-link--text { display: inline-flex; align-items: center; gap: 2px; height: 24px; padding: 0 4px 0 0; font-size: 12px; color: var(--ui-aga-muted); }
.ui-aga-link--text:hover { color: var(--ui-aga-ink); }
.ui-aga-link--text svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; }

@container ui-aga (max-width: 560px) {
  .ui-aga-search { flex: 1 1 100%; margin-left: 0; order: -1; }
  .ui-aga-search kbd { display: none; }
  .ui-aga-seg { flex: 1 1 auto; min-width: 0; overflow-x: auto; scrollbar-width: none; }
  .ui-aga-seg::-webkit-scrollbar { display: none; }
  .ui-aga-seg button:focus-visible { outline-offset: -2px; }
  .ui-aga-row__main { gap: 10px; padding-left: 8px; }
  .ui-aga-row__time { order: 3; width: auto; min-width: 0; }
  .ui-aga-row__turn { display: none; }
  .ui-aga-row__sentence { display: -webkit-box; white-space: normal; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
  .ui-aga-panel, .ui-aga-steps, .ui-aga-steps .ui-aga-panel { margin-left: 8px; }
}
@keyframes ui-aga-in { from { opacity: 0; transform: translateY(-2px); } }
@media (prefers-reduced-motion: reduce) {
  .ui-aga *, .ui-aga *::before, .ui-aga *::after { animation: none !important; transition: none !important; }
}
@media (pointer: coarse) {
  .ui-aga-btn, .ui-aga-seg button, .ui-aga-search { min-height: 36px; }
}
`;
