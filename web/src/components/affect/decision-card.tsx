/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";

/*
 * Decision Card – a question from an agent, asked the way Claude asks one, in a single self-contained file.
 * It sits just above your chat composer as a card of its own: the composer stays for new prompts, the card takes the answer.
 * Clean style: one white card with a hairline edge and a soft lift – no gray layer around it.
 *   · the question, then three to five numbered rows – a title and one line each, one may be Recommended
 *   · approval: two or three facts (To, Subject, …), then Approve and Decline · pick: small preview tiles
 *   · the last row takes your own words – a round send arrow shows once you type, Enter sends
 *   · a click or 1–5 answers at once; ↑ ↓ move, ↓ from the last row goes into the field; Esc or Later folds it to one line
 *   · after the last answer it folds away smoothly
 * The styles are injected below, so no extra files are needed.
 */

export type DecisionKind = "decision" | "pick" | "approval";
/** A gray block of a preview tile, on a 120 × 80 grid */
export type DecisionBlock = { x: number; y: number; w: number; h: number; r?: number; tone?: "ink" | "mid" | "soft" };
export type DecisionOption = {
  id: string;
  title: string;
  /** One line – it ends in … when it doesn't fit */
  summary?: string;
  /** A small gray Recommended tag – the row is also where the focus starts */
  recommended?: boolean;
  /** pick: what the tile shows – your own node, or gray blocks on a 120 × 80 grid */
  preview?: ReactNode;
  blocks?: DecisionBlock[];
  /** Steve: attributes on the row's title (test hooks) */
  attrs?: Record<string, string | undefined>;
};
/** Steve: what the last row (your own words) does for this decision – its placeholder and hooks; false leaves it out */
export type DecisionOwn = { placeholder?: string; required?: boolean; attrs?: Record<string, string | undefined>; sendAttrs?: Record<string, string | undefined>; send?: string };
export type Decision = {
  id: string;
  question: string;
  /** The task it belongs to – shown muted in the head */
  task?: ReactNode;
  /** Steve: the badge in the head instead of "Needs you" */
  badge?: ReactNode;
  /** Steve: more controls in the head, before Later */
  tools?: ReactNode;
  /** Steve: what the question is about, between the question and the rows */
  detail?: ReactNode;
  /** Steve: the field for your own words; false leaves it out */
  own?: DecisionOwn | false;
  /** Steve: an answer is on its way – the rows and the field wait */
  busy?: boolean;
  /** Steve: attributes on the card (test hooks) */
  attrs?: Record<string, string | undefined>;
  /** decision: rows to pick from · pick: preview tiles · approval: facts, then Approve / Decline (default: decision) */
  kind?: DecisionKind;
  /** decision and pick – three to five */
  options?: DecisionOption[];
  /** approval – two or three lines of what it will do */
  facts?: { label: string; value: string }[];
};
/** What you answered: an option or your own words – or approve / decline */
export type DecisionAnswer = { option?: string; text?: string } | "approve" | "decline";

export type DecisionCardLabels = {
  needsYou: string;
  later: string;
  show: string;
  recommended: string;
  approve: string;
  decline: string;
  own: string;
  send: string;
  answered: string;
  previous: string;
  next: string;
  allDone: string;
  waiting: (n: number) => string;
  of: (i: number, n: number) => string;
};

export type DecisionCardProps = {
  /** The open decisions, oldest first – answered ones leave the card at once; drop them from the array in onDecide */
  decisions: Decision[];
  onDecide?: (id: string, answer: DecisionAnswer) => void;
  /** Later or Esc folded the card to one line */
  onLater?: (id: string) => void;
  /** Start folded to "2 decisions waiting · Show" */
  defaultFolded?: boolean;
  /** Change it (e.g. a counter) to bring the card up from elsewhere – it unfolds, pulses gray once and takes the focus */
  reveal?: number;
  /** Steve: answered decisions stay until the array drops them (the Hub decides), so a failed answer can be given again */
  hold?: boolean;
  /** Steve: show Later (folding to one line); off where the card sits in a list */
  later?: boolean;
  labels?: Partial<DecisionCardLabels>;
  className?: string;
};

const LABELS: DecisionCardLabels = {
  needsYou: "Needs you",
  later: "Later",
  show: "Show",
  recommended: "Recommended",
  approve: "Approve",
  decline: "Decline",
  own: "Something else…",
  send: "Send",
  answered: "Answered",
  previous: "Previous decision",
  next: "Next decision",
  allDone: "Nothing else waits",
  waiting: (n) => (n === 1 ? "1 decision waiting" : `${n.toLocaleString("en-US")} decisions waiting`),
  of: (i, n) => `${i} of ${n}`,
};

type Row = { id: string; title: string; summary?: string; recommended?: boolean; preview?: ReactNode; blocks?: DecisionBlock[]; attrs?: Record<string, string | undefined>; answer: DecisionAnswer };

const rowsOf = (d: Decision, L: DecisionCardLabels): Row[] =>
  d.kind === "approval"
    ? [
        { id: "approve", title: L.approve, answer: "approve" },
        { id: "decline", title: L.decline, answer: "decline" },
      ]
    : (d.options ?? []).slice(0, 5).map((o) => ({ ...o, answer: { option: o.id } }));

const labelOf = (d: Decision, a: DecisionAnswer, L: DecisionCardLabels) =>
  a === "approve" ? L.approve : a === "decline" ? L.decline : a.option ? (d.options?.find((o) => o.id === a.option)?.title ?? a.option) : (a.text ?? "");

/* ---------- Icons: Hugeicons Free (MIT, © Hugeicons, hugeicons.com) ---------- */

const ArrowLeft = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M15 6C15 6 9 10.42 9 12C9 13.58 15 18 15 18" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
  </svg>
);
const ArrowRight = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M9 6C9 6 15 10.42 15 12C15 13.58 9 18 9 18" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
  </svg>
);
const ArrowUp = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M12 5.5V19" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.2" />
    <path d="M18 11C18 11 13.58 5 12 5C10.42 5 6 11 6 11" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2.2" />
  </svg>
);

/** A pick's preview: gray blocks on a small white page */
const Blocks = ({ blocks }: { blocks: DecisionBlock[] }) => (
  <svg className="ui-dcc__wire" viewBox="0 0 120 80" preserveAspectRatio="xMidYMid meet" aria-hidden="true">
    <rect className="ui-dcc__wire-page" x="0.5" y="0.5" width="119" height="79" rx="6" />
    {blocks.map((b, i) => (
      <rect key={i} data-tone={b.tone ?? "soft"} x={b.x} y={b.y} width={b.w} height={b.h} rx={b.r ?? 2} />
    ))}
  </svg>
);

/* ---------- The card ---------- */

export function DecisionCard({ decisions, onDecide, onLater, defaultFolded = false, reveal, hold = false, later: withLater = true, labels, className }: DecisionCardProps) {
  const L: DecisionCardLabels = { ...LABELS, ...labels };
  // Answered here – hidden at once, until the array you pass drops them too
  const [gone, setGone] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => {
    setGone((g) => {
      if (!g.size) return g;
      const ids = new Set(decisions.map((d) => d.id));
      const next = new Set([...g].filter((id) => ids.has(id)));
      return next.size === g.size ? g : next;
    });
  }, [decisions]);
  const open = decisions.filter((d) => !gone.has(d.id));
  const [curId, setCurId] = useState<string | null>(null);
  const [folded, setFolded] = useState(defaultFolded);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [pulse, setPulse] = useState(false);
  const [say, setSay] = useState("");
  const index = Math.max(0, open.findIndex((d) => d.id === curId));
  const current = open[index] ?? null;
  // The last one stays on screen while the card folds away
  const last = useRef(current);
  if (current) last.current = current;
  const shown = current ?? last.current;
  const mode = !current ? "none" : folded ? "min" : "full";
  const root = useRef<HTMLDivElement>(null);
  const want = useRef<"row" | "show" | "prev" | "next" | null>(null);
  // Was the focus inside the card while it rendered? If the focused row or button then leaves the page (an answer,
  // the pager, a decision dropped by your array), the focus moves to the next one instead of falling back to <body>
  const focusIn = useRef(false);
  focusIn.current = typeof document !== "undefined" && !!root.current?.contains(document.activeElement);

  const announce = (text: string) => setSay((s) => (s === text ? `${text} ` : text));

  // Focus moves once the next decision (or the folded line) is on screen
  useLayoutEffect(() => {
    const lost = focusIn.current && mode !== "none" && (!document.activeElement || document.activeElement === document.body);
    const w = want.current ?? (lost ? (mode === "min" ? "show" : "row") : null);
    want.current = null;
    if (!w) return;
    const box = root.current;
    const el =
      (w === "show" && box?.querySelector<HTMLElement>(".ui-dcc__show")) ||
      ((w === "prev" || w === "next") && box?.querySelector<HTMLElement>(`.ui-dcc__page[data-step="${w}"]`)) ||
      // No options: the field for your own words takes the focus
      box?.querySelector<HTMLElement>('[role="option"][tabindex="0"], .ui-dcc__input');
    el?.focus({ preventScroll: true });
  });

  // Brought up from elsewhere: unfold, a short gray pulse, and the focus
  const firstReveal = useRef(reveal);
  useEffect(() => {
    if (reveal === firstReveal.current) return;
    firstReveal.current = reveal;
    setFolded(false);
    want.current = "row";
    setPulse(true);
    const t = window.setTimeout(() => setPulse(false), 700);
    return () => window.clearTimeout(t);
  }, [reveal]);

  const answer = (d: Decision, a: DecisionAnswer) => {
    if (d.busy) return;
    const left = open.length - 1;
    const next = open[index + 1] ?? open[index - 1] ?? null;
    // An option carries the words typed so far (Steve: some answers take a note along)
    const text = drafts[d.id]?.trim();
    const full: DecisionAnswer = typeof a === "object" && a.option && text ? { ...a, text } : a;
    if (!hold) {
      setGone((g) => new Set(g).add(d.id));
      setCurId(next?.id ?? null);
      setDrafts(({ [d.id]: _, ...rest }) => rest);
    }
    announce(`${L.answered}: ${labelOf(d, a, L)}. ${left ? L.waiting(left) : L.allDone}`);
    onDecide?.(d.id, full);
  };
  const later = () => {
    if (!current) return;
    setFolded(true);
    want.current = "show";
    announce(`${L.later} – ${L.waiting(open.length)}`);
    onLater?.(current.id);
  };
  const show = () => {
    setFolded(false);
    want.current = "row";
  };
  const page = (step: number) => {
    if (open.length < 2) return;
    // The same arrow keeps the focus once the next decision is on screen
    if (root.current?.contains(document.activeElement)) want.current = step < 0 ? "prev" : "next";
    setCurId(open[(index + step + open.length) % open.length].id);
  };

  return (
    <div ref={root} className={["ui-dcc", className].filter(Boolean).join(" ")} data-mode={mode}>
      <style href="ui-decision-card-clean" precedence="default">
        {css}
      </style>
      <span className="ui-dcc__sr" role="status" aria-live="polite">
        {say}
      </span>
      <div className="ui-dcc__wrap" inert={mode === "none"}>
        <div className="ui-dcc__inner">
          {mode === "min" ? (
            <div className="ui-dcc__box ui-dcc__box--min">
              <div className="ui-dcc__head">
                <span className="ui-dcc__badge">{L.needsYou}</span>
                <span className="ui-dcc__task ui-dcc__num">{L.waiting(open.length)}</span>
                <span className="ui-dcc__tools">
                  <button type="button" className="ui-dcc__link ui-dcc__show" onClick={show}>
                    {L.show}
                  </button>
                </span>
              </div>
            </div>
          ) : shown ? (
            <Card
              key={shown.id}
              d={shown}
              L={L}
              count={open.length}
              index={index}
              pulse={pulse}
              draft={drafts[shown.id] ?? ""}
              onDraft={(text) => setDrafts((x) => ({ ...x, [shown.id]: text }))}
              onAnswer={(a) => answer(shown, a)}
              onLater={withLater ? later : undefined}
              onPage={page}
            />
          ) : null}
        </div>
      </div>
    </div>
  );
}

function Card({
  d,
  L,
  count,
  index,
  pulse,
  draft,
  onDraft,
  onAnswer,
  onLater,
  onPage,
}: {
  d: Decision;
  L: DecisionCardLabels;
  count: number;
  index: number;
  pulse: boolean;
  draft: string;
  onDraft: (text: string) => void;
  onAnswer: (a: DecisionAnswer) => void;
  onLater?: () => void;
  onPage: (step: number) => void;
}) {
  const qid = `ui-dcc-q-${useId().replace(/:/g, "")}`;
  const rows = rowsOf(d, L);
  const tiles = d.kind === "pick";
  const [at, setAt] = useState(() => Math.max(0, rows.findIndex((r) => r.recommended)));
  const box = useRef<HTMLDivElement>(null);
  const own = useRef<HTMLInputElement>(null);

  const focusRow = (i: number) => {
    setAt(i);
    box.current?.querySelector<HTMLElement>(`[data-row="${i}"]`)?.focus();
  };
  const ownSpec = d.own === false ? null : (d.own ?? {});
  const busy = !!d.busy;
  const sendOwn = () => {
    const text = draft.trim();
    if (busy) return;
    if (!text && ownSpec?.required !== false) return own.current?.focus();
    onAnswer({ text });
  };
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.metaKey || e.ctrlKey || e.altKey || e.nativeEvent.isComposing) return;
    if (e.key === "Escape" && onLater) {
      e.preventDefault();
      return onLater();
    }
    if ((e.target as HTMLElement).getAttribute("role") !== "option") return;
    const n = Number(e.key);
    if (n >= 1 && n <= rows.length) {
      e.preventDefault();
      return onAnswer(rows[n - 1].answer);
    }
    if (ownSpec && (n === rows.length + 1 || (e.key === "ArrowDown" && at === rows.length - 1))) {
      e.preventDefault();
      return own.current?.focus();
    }
    const next = tiles ? ["ArrowRight", "ArrowDown"] : ["ArrowDown"];
    const prev = tiles ? ["ArrowLeft", "ArrowUp"] : ["ArrowUp"];
    if (next.includes(e.key)) (e.preventDefault(), focusRow(Math.min(rows.length - 1, at + 1)));
    else if (prev.includes(e.key)) (e.preventDefault(), focusRow(Math.max(0, at - 1)));
    else if (e.key === "Home") (e.preventDefault(), focusRow(0));
    else if (e.key === "End") (e.preventDefault(), focusRow(rows.length - 1));
    else if (e.key === "Enter" || e.key === " ") (e.preventDefault(), onAnswer(rows[at].answer));
  };

  return (
    <div ref={box} className="ui-dcc__box" role="group" aria-labelledby={qid} aria-busy={busy || undefined} data-pulse={pulse || undefined} onKeyDown={onKey} {...d.attrs}>
      <div className="ui-dcc__head">
        <span className="ui-dcc__badge">{d.badge ?? L.needsYou}</span>
        <span className="ui-dcc__task" dir="auto">
          {d.task}
        </span>
        <span className="ui-dcc__tools">
          {count > 1 && (
            <span className="ui-dcc__pager">
              <button type="button" className="ui-dcc__page" data-step="prev" aria-label={L.previous} onClick={() => onPage(-1)}>
                <ArrowLeft />
              </button>
              {/* As wide as its longest count, so "9 of 12" → "10 of 12" doesn't shift the head */}
              <span className="ui-dcc__num ui-dcc__count" style={{ minWidth: `${L.of(count, count).length}ch` }}>
                {L.of(index + 1, count)}
              </span>
              <button type="button" className="ui-dcc__page" data-step="next" aria-label={L.next} onClick={() => onPage(1)}>
                <ArrowRight />
              </button>
            </span>
          )}
          {d.tools}
          {onLater && (
            <button type="button" className="ui-dcc__link" aria-keyshortcuts="Escape" onClick={onLater}>
              {L.later}
            </button>
          )}
        </span>
      </div>
      <div className="ui-dcc__body">
        <p id={qid} className="ui-dcc__q" dir="auto">
          {d.question}
        </p>
        {d.detail && <div className="ui-dcc__detail">{d.detail}</div>}
        {d.facts && d.facts.length > 0 && (
          <dl className="ui-dcc__facts">
            {d.facts.slice(0, 3).map((f, i) => (
              <div key={i}>
                <dt title={f.label}>{f.label}</dt>
                <dd dir="auto">{f.value}</dd>
              </div>
            ))}
          </dl>
        )}
        {rows.length > 0 && <div
          className={tiles ? "ui-dcc__tiles" : "ui-dcc__rows"}
          role="listbox"
          aria-labelledby={qid}
          aria-orientation={tiles ? "horizontal" : "vertical"}
          data-many={tiles && rows.length > 3 ? "" : undefined}
          style={tiles ? ({ "--ui-dcc-cols": Math.max(3, rows.length) } as CSSProperties) : undefined}
        >
          {rows.map((r, i) => (
            <button
              key={r.id}
              type="button"
              role="option"
              className={tiles ? "ui-dcc__tile" : "ui-dcc__opt"}
              data-row={i}
              aria-selected={i === at}
              aria-keyshortcuts={String(i + 1)}
              aria-disabled={busy || undefined}
              tabIndex={i === at ? 0 : -1}
              onFocus={() => setAt(i)}
              onClick={() => !busy && onAnswer(r.answer)}
            >
              {tiles && (
                <span className="ui-dcc__frame" aria-hidden="true">
                  {r.preview ?? (r.blocks ? <Blocks blocks={r.blocks} /> : null)}
                </span>
              )}
              {tiles && r.recommended && <span className="ui-dcc__tag ui-dcc__tag--tile">{L.recommended}</span>}
              <span className="ui-dcc__row">
                <span className="ui-dcc__key ui-dcc__num" aria-hidden="true">
                  {i + 1}
                </span>
                <span className="ui-dcc__text">
                  <span className="ui-dcc__title" dir="auto" {...r.attrs}>
                    {r.title}
                  </span>
                  {!tiles && r.summary && (
                    <span className="ui-dcc__summary" dir="auto">
                      {r.summary}
                    </span>
                  )}
                </span>
                {r.recommended && <span className="ui-dcc__tag ui-dcc__tag--row">{L.recommended}</span>}
              </span>
            </button>
          ))}
        </div>}
        {ownSpec && <div className="ui-dcc__own" onClick={(e) => e.target === e.currentTarget && own.current?.focus()}>
          <span className="ui-dcc__key ui-dcc__num" aria-hidden="true">
            {rows.length + 1}
          </span>
          <input
            ref={own}
            className="ui-dcc__input"
            value={draft}
            placeholder={ownSpec.placeholder ?? L.own}
            aria-label={`${(ownSpec.placeholder ?? L.own).replace(/…$/, "")}: ${d.question}`}
            {...ownSpec.attrs}
            aria-keyshortcuts={String(rows.length + 1)}
            dir="auto"
            onChange={(e) => onDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing) return;
              if (e.key === "Enter") (e.preventDefault(), sendOwn());
              else if (e.key === "ArrowUp" && !draft) (e.preventDefault(), focusRow(rows.length - 1));
            }}
          />
          {ownSpec.send ? (
            <button type="button" className="ui-dcc__send ui-dcc__send--word" disabled={busy || (ownSpec.required !== false && !draft.trim())} data-shown={ownSpec.required === false || draft.trim() ? "" : undefined} data-empty={draft.trim() ? undefined : ""} onClick={sendOwn} {...ownSpec.sendAttrs}>
              {ownSpec.send}
            </button>
          ) : (
            <button type="button" className="ui-dcc__send" aria-label={L.send} title={L.send} aria-disabled={!draft.trim() || busy || undefined} data-shown={draft.trim() ? "" : undefined} tabIndex={draft.trim() ? 0 : -1} onClick={sendOwn} {...ownSpec.sendAttrs}>
              <ArrowUp />
            </button>
          )}
        </div>}
      </div>
    </div>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors, sizes and motion as variables – override them on .ui-dcc */
.ui-dcc {
  --ui-dcc-card: var(--aui-surface, #ffffff);
  --ui-dcc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-dcc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-dcc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-dcc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-dcc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-dcc-fill: var(--aui-subtle, rgba(0, 0, 0, 0.05));
  --ui-dcc-well: var(--aui-subtle, rgba(0, 0, 0, 0.025));
  --ui-dcc-pulse: color-mix(in srgb, var(--aui-ink, #000) 4.5%, transparent);
  --ui-dcc-wait-bg: var(--aui-warning-soft, #ff9c0029);
  --ui-dcc-wait-fg: var(--aui-warning, #b25e00);
  --ui-dcc-wire-ink: rgba(0, 0, 0, 0.42);
  --ui-dcc-wire-mid: rgba(0, 0, 0, 0.16);
  --ui-dcc-wire-soft: rgba(0, 0, 0, 0.07);
  --ui-dcc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-dcc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-dcc-on-solid: var(--aui-on-primary, #fcfcfc);
  --ui-dcc-placeholder: var(--aui-placeholder, rgba(0, 0, 0, 0.36));
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-dcc-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-dcc-float: 0 0 0 0.5px rgba(0, 0, 0, 0.07), 0 1px 2px rgba(0, 0, 0, 0.05), 0 10px 28px -12px rgba(0, 0, 0, 0.16);
  --ui-dcc-ease: cubic-bezier(0.23, 1, 0.32, 1);
  position: relative;
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  container: ui-dcc / inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-dcc-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-dcc *, .ui-dcc *::before, .ui-dcc *::after { box-sizing: border-box; }
:where(.ui-dcc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-dcc button:focus-visible { outline: 2px solid var(--ui-dcc-focus); outline-offset: 2px; }
.ui-dcc :is(p, dl, dd) { margin: 0; }
.ui-dcc svg { display: block; flex: none; }
.ui-dcc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.ui-dcc__num { font-variant-numeric: tabular-nums; }

/* Folds away after the last answer: the row shrinks to nothing, the content fades */
.ui-dcc__wrap { display: grid; grid-template-rows: 1fr; opacity: 1; transition: grid-template-rows 420ms var(--ui-dcc-ease), opacity 260ms ease; }
.ui-dcc[data-mode="none"] .ui-dcc__wrap { grid-template-rows: 0fr; opacity: 0; }
/* Room for the focus rings and the card's edge while it clips */
.ui-dcc__inner { min-width: 0; min-height: 0; margin: -8px; padding: 8px; overflow: hidden; }
/* Open: the card's shadow may spill past the fold – it only clips while it grows open */
.ui-dcc:not([data-mode="none"]) .ui-dcc__inner { overflow: visible; animation: ui-dcc-clip 420ms both; }
@keyframes ui-dcc-clip { 0%, 99.9% { overflow: hidden; } 100% { overflow: visible; } }

/* ----- One white card: the head on top ----- */
.ui-dcc__box { display: flex; flex-direction: column; gap: 2px; min-width: 0; padding: 6px; border-radius: calc(var(--aui-radius-lg, 10px) + 4px); background: var(--aui-surface, var(--ui-dcc-card)); box-shadow: var(--aui-shadow-card, var(--ui-dcc-float)); animation: ui-dcc-in 260ms ease both; transition: background-color 400ms ease; }
.ui-dcc__box[data-pulse] { background: color-mix(in srgb, var(--ui-dcc-card), #808080 9%); }
.ui-dcc__head { display: flex; align-items: center; gap: 8px; min-width: 0; height: 28px; padding: 0 2px 0 6px; }
.ui-dcc__badge { flex: none; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-dcc-wait-bg); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-dcc-wait-fg); white-space: nowrap; }
.ui-dcc__task { flex: 1; min-width: 0; overflow: hidden; font-size: 12px; color: var(--ui-dcc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-dcc__tools { display: flex; flex: none; align-items: center; gap: 4px; margin-left: auto; }
.ui-dcc__count { text-align: center; }
.ui-dcc__pager { display: inline-flex; align-items: center; gap: 2px; font-size: 12px; color: var(--ui-dcc-muted); white-space: nowrap; }
.ui-dcc__page { display: grid; place-items: center; width: 22px; height: 22px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-dcc-muted); transition: background-color 140ms ease, color 140ms ease; }
.ui-dcc__page:hover { background: var(--ui-dcc-fill); color: var(--ui-dcc-ink); }
.ui-dcc__link { display: inline-flex; align-items: center; height: 24px; padding: 0 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); font-size: 12px; font-weight: 500; color: var(--ui-dcc-text); white-space: nowrap; transition: background-color 140ms ease, color 140ms ease; }
.ui-dcc__link:hover { background: var(--ui-dcc-fill); color: var(--ui-dcc-ink); }
.ui-dcc__box--min { animation-duration: 200ms; }
.ui-dcc__box--min .ui-dcc__head { height: 28px; }

/* ----- Question, rows and your own answer ----- */
.ui-dcc__body { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.ui-dcc__q { padding: 4px 6px 2px; font-size: 15px; font-weight: 600; line-height: 22px; letter-spacing: -0.006em; color: var(--ui-dcc-ink); overflow-wrap: anywhere; }
/* The labels share one column, as wide as the longest (up to 112px) – values wrap to two lines, then end in … */
.ui-dcc__facts { display: grid; grid-template-columns: minmax(56px, max-content) minmax(0, 1fr); gap: 2px 12px; padding: 2px 6px 4px; }
.ui-dcc__facts > div { display: grid; grid-column: 1 / -1; grid-template-columns: subgrid; font-size: 13px; line-height: 20px; }
.ui-dcc__facts dt { max-width: 112px; overflow: hidden; color: var(--ui-dcc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-dcc__facts dd { display: -webkit-box; min-width: 0; overflow: hidden; color: var(--ui-dcc-ink); overflow-wrap: break-word; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }

.ui-dcc__rows { display: grid; grid-template-columns: minmax(0, 1fr); }
.ui-dcc__opt { display: flex; align-items: center; width: 100%; min-width: 0; min-height: 36px; padding: 4px 8px 4px 6px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); color: var(--ui-dcc-ink); transition: background-color 120ms ease; }
.ui-dcc__opt:hover, .ui-dcc__rows:focus-within .ui-dcc__opt[aria-selected="true"] { background: var(--ui-dcc-fill); }
.ui-dcc :is(.ui-dcc__opt, .ui-dcc__tile):focus-visible { outline-offset: -2px; }
.ui-dcc__row { display: flex; flex: 1; align-items: center; gap: 10px; min-width: 0; }
.ui-dcc__key { display: grid; flex: none; place-items: center; width: 22px; height: 22px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-dcc-fill); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-dcc-muted); transition: background-color 120ms ease, box-shadow 120ms ease; }
.ui-dcc__text { display: flex; flex: 1; flex-direction: column; min-width: 0; }
/* A long title wraps to a second line, then ends in … – the summary stays on one */
.ui-dcc__title { display: -webkit-box; overflow: hidden; font-size: 13px; font-weight: 500; line-height: 20px; overflow-wrap: break-word; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
.ui-dcc__summary { overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-dcc-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-dcc__tag { flex: none; padding: 0 6px; border-radius: var(--aui-radius-sm, 5px); background: var(--ui-dcc-fill); font-size: 11px; font-weight: 500; line-height: 18px; color: var(--ui-dcc-muted); white-space: nowrap; transition: background-color 120ms ease, box-shadow 120ms ease; }
/* On a gray row the number and the tag turn white, so they stay apart from it */
.ui-dcc__opt:hover :is(.ui-dcc__key, .ui-dcc__tag), .ui-dcc__rows:focus-within .ui-dcc__opt[aria-selected="true"] :is(.ui-dcc__key, .ui-dcc__tag) { background: var(--ui-dcc-card); box-shadow: var(--ui-dcc-shadow); }

/* ----- pick: preview tiles ----- */
.ui-dcc__tiles { display: grid; grid-template-columns: repeat(var(--ui-dcc-cols, 3), minmax(0, 1fr)); gap: 4px; }
.ui-dcc__tile { display: flex; flex-direction: column; gap: 6px; min-width: 0; padding: 4px; border-radius: var(--aui-radius-lg, 10px); color: var(--ui-dcc-ink); transition: background-color 120ms ease; }
.ui-dcc__tile:hover, .ui-dcc__tiles:focus-within .ui-dcc__tile[aria-selected="true"] { background: var(--ui-dcc-fill); }
.ui-dcc__frame { position: relative; display: grid; place-items: center; aspect-ratio: 3 / 2; overflow: hidden; padding: 6px; border-radius: 7px; background: var(--ui-dcc-well); box-shadow: inset 0 0 0 1px var(--ui-dcc-line); }
.ui-dcc__frame > * { width: 100%; height: 100%; }
.ui-dcc__tile { position: relative; }
.ui-dcc__tile .ui-dcc__tag--tile { position: absolute; top: 10px; right: 10px; background: var(--ui-dcc-card); box-shadow: var(--ui-dcc-shadow); }
.ui-dcc__tile .ui-dcc__tag--row { display: none; }
.ui-dcc__wire-page { fill: var(--ui-dcc-card); }
.ui-dcc__wire [data-tone="ink"] { fill: var(--ui-dcc-wire-ink); }
.ui-dcc__wire [data-tone="mid"] { fill: var(--ui-dcc-wire-mid); }
.ui-dcc__wire [data-tone="soft"] { fill: var(--ui-dcc-wire-soft); }
.ui-dcc__tile .ui-dcc__row { align-items: flex-start; gap: 8px; padding: 0 2px 2px; }
.ui-dcc__tile .ui-dcc__key { width: 20px; height: 20px; font-size: 11px; }
.ui-dcc__tile:hover .ui-dcc__key, .ui-dcc__tiles:focus-within .ui-dcc__tile[aria-selected="true"] .ui-dcc__key { background: var(--ui-dcc-card); box-shadow: var(--ui-dcc-shadow); }

/* ----- Your own answer: the last row ----- */
.ui-dcc__own { display: flex; align-items: center; gap: 10px; min-width: 0; min-height: 36px; padding: 4px 6px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); cursor: text; transition: background-color 120ms ease, box-shadow 120ms ease; }
.ui-dcc__own:hover { background: var(--ui-dcc-well); }
/* Typing: no ring – the caret shows where you are, the edge only firms up a little */
.ui-dcc__own:focus-within { background: var(--ui-dcc-well); box-shadow: inset 0 0 0 1px var(--ui-dcc-line); }
.ui-dcc__input { flex: 1; min-width: 0; text-overflow: ellipsis; height: 28px; margin: 0; padding: 0; border: 0; background: none; font: inherit; font-size: 13px; color: var(--ui-dcc-ink); outline: none; }
.ui-dcc__input::placeholder { color: var(--ui-dcc-placeholder); }
.ui-dcc__send { display: grid; flex: none; place-items: center; width: 24px; height: 24px; border-radius: 999px; background: var(--ui-dcc-solid); color: var(--ui-dcc-on-solid); opacity: 0; transform: scale(0.8); pointer-events: none; transition: opacity 140ms ease, transform 180ms var(--ui-dcc-ease); }
.ui-dcc__send[data-shown] { opacity: 1; transform: none; pointer-events: auto; }
.ui-dcc__send[data-shown]:active { transform: scale(0.92); }
/* Steve: a send button with its word (the answer the field gives, e.g. "Send back for rework") */
.ui-dcc__send--word { width: auto; height: 28px; padding: 0 10px; border-radius: var(--aui-radius-sm, 7px); font-size: 12px; font-weight: 500; white-space: nowrap; }
.ui-dcc__send--word:disabled { opacity: 0.45; cursor: default; }
/* Steve: a field that needs words keeps its send out of the way until there are some; one that may go empty shows it quiet until you type */
.ui-dcc__send--word:not([data-shown]) { display: none; }
.ui-dcc__send--word[data-empty] { background: transparent; color: var(--ui-dcc-text); box-shadow: inset 0 0 0 1px var(--ui-dcc-line); }
.ui-dcc__send--word[data-empty]:hover { background: var(--ui-dcc-fill); color: var(--ui-dcc-ink); }
.ui-dcc__opt[aria-disabled="true"] { opacity: 0.55; cursor: default; }
/* Steve: what the question is about – a draft, a command, the conversation it came from */
.ui-dcc__detail:empty { display: none; }
.ui-dcc__detail { display: flex; flex-direction: column; gap: 8px; min-width: 0; padding: 2px 6px 6px; font-size: 13px; line-height: 20px; color: var(--ui-dcc-text); overflow-wrap: anywhere; }

/* Phones: everything stays inside the card's edges */
@container ui-dcc (max-width: 560px) {
  .ui-dcc__facts > div { grid-template-columns: 56px minmax(0, 1fr); }
  .ui-dcc__tiles[data-many] { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .ui-dcc__pager { gap: 0; }
}
/* Small phones: the tiles turn into rows – the number and title first, the preview small on the right */
@container ui-dcc (max-width: 420px) {
  .ui-dcc__tiles, .ui-dcc__tiles[data-many] { grid-template-columns: minmax(0, 1fr); gap: 0; }
  .ui-dcc__tile { flex-direction: row; align-items: center; gap: 10px; min-height: 44px; padding: 4px 6px; border-radius: calc(var(--aui-radius-sm, 5px) + 3px); }
  .ui-dcc__tile .ui-dcc__row { flex: 1; align-items: center; gap: 10px; padding: 0; }
  .ui-dcc__tile .ui-dcc__key { width: 22px; height: 22px; font-size: 12px; }
  .ui-dcc__frame { order: 1; flex: none; width: 54px; padding: 3px; border-radius: 6px; }
  .ui-dcc__tile .ui-dcc__tag--tile { display: none; }
  .ui-dcc__tile .ui-dcc__tag--row { display: block; }
  .ui-dcc__tile:hover .ui-dcc__tag--row, .ui-dcc__tiles:focus-within .ui-dcc__tile[aria-selected="true"] .ui-dcc__tag--row { background: var(--ui-dcc-card); box-shadow: var(--ui-dcc-shadow); }
}

/* Touch: every row at least 44px, every button 32px to see and 40px to hit */
@media (pointer: coarse) {
  .ui-dcc__opt, .ui-dcc__own { min-height: 44px; }
  .ui-dcc__page, .ui-dcc__send { position: relative; width: 32px; height: 32px; }
  .ui-dcc__send--word { width: auto; }
  .ui-dcc__link { position: relative; height: 32px; }
  .ui-dcc__page::after, .ui-dcc__send::after, .ui-dcc__link::after { content: ""; position: absolute; inset: -4px; }
  .ui-dcc__head, .ui-dcc__box--min .ui-dcc__head { height: 40px; }
}

@keyframes ui-dcc-in { from { opacity: 0; } }

@media (prefers-reduced-motion: reduce) {
  .ui-dcc *, .ui-dcc *::before, .ui-dcc *::after { animation: none !important; transition: none !important; }
  .ui-dcc__wrap { transition: none; }
}

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-dcc {
  /* Steve: every color above follows its token, which already switches with dark mode */
  --ui-dcc-wire-ink: rgba(255, 255, 255, 0.5);
  --ui-dcc-wire-mid: rgba(255, 255, 255, 0.2);
  --ui-dcc-wire-soft: rgba(255, 255, 255, 0.08);
  --ui-dcc-shadow: 0 0 0 1px var(--ui-dcc-line), 0 1px 2px rgba(0, 0, 0, 0.3);
}
`;
