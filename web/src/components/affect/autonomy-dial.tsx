/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { Children, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { t, useT } from "@/i18n";

/*
 * Autonomy Dial – how much an agent may do on its own, in a single self-contained file.
 * One quiet white card: three levels as a segmented control – Always ask, If risky, Full auto – and the agent's real
 * actions below it in two groups, "Runs on its own" and "Asks you first"; risky and destructive ones carry a small tag.
 *   · pick a level and the actions glide from one group to the other, so the setting reads as what it does
 *   · the lock on an action pins it to "Asks you first", whatever the dial says – e.g. spending money
 *   · the counts next to both groups roll when they change
 * Click a level or use the arrow keys. Controlled or uncontrolled for both the level and the pins.
 * The styles are injected below, so no extra files are needed.
 */

export type AutonomyLevel = 0 | 1 | 2;
export type ActionRisk = "safe" | "risky" | "destructive";

export type AgentAction = {
  id: string;
  label: string;
  /** A few words on what it touches, e.g. "Gmail · to customers" */
  detail?: string;
  risk: ActionRisk;
  /** A 16px glyph or a small brand mark */
  icon?: ReactNode;
};

export type AutonomyDialProps = {
  actions: AgentAction[];
  /** 0 Ask every time · 1 Ask before risky actions · 2 Full auto */
  level?: AutonomyLevel;
  defaultLevel?: AutonomyLevel;
  onLevelChange?: (level: AutonomyLevel) => void;
  /** Ids of actions that always ask, whatever the level */
  pinned?: string[];
  defaultPinned?: string[];
  onPinnedChange?: (pinned: string[]) => void;
  /** Heading, e.g. the agent's name */
  title?: string;
  /** Line under the heading */
  subtitle?: string;
  className?: string;
  /* Steve: */
  /** The words, translated by the caller; English when left out */
  text?: Partial<DialText>;
  /** false: locks are fixed (the pinned actions always ask and can't be unlocked here) */
  lockable?: boolean;
  /** Levels that can't be picked right now */
  disabledLevels?: AutonomyLevel[];
  /** The level that applies now, marked on its segment while another one is picked */
  currentLevel?: AutonomyLevel;
  /** Extra attributes on each level's button, e.g. test hooks */
  levelProps?: (level: AutonomyLevel) => Record<string, string>;
  /** Tone of each level's pill: Steve marks full access in its attention colour */
  tones?: Partial<Record<AutonomyLevel, "attention">>;
  /** false: actions without an icon show no icon slot instead of a generic dot */
  bullets?: boolean;
  /** Shown on the tray right under the levels, before the actions – e.g. what switching would do now */
  children?: ReactNode;
};

export type DialText = {
  levels: [{ label: string; hint: string }, { label: string; hint: string }, { label: string; hint: string }];
  group: string;
  own: string;
  ask: string;
  ownEmpty: string;
  askEmpty: string;
  risky: string;
  destructive: string;
  current: string;
  locked: string;
  lock: (label: string) => string;
  unlock: (label: string) => string;
  announce: (level: string, own: number, ask: number) => string;
};

/** Does the agent have to ask before this action? */
export const asksFirst = (action: AgentAction, level: AutonomyLevel, pinned: string[] = []) =>
  pinned.includes(action.id) || level === 0 || (level === 1 && action.risk !== "safe");

/* ---------- Helpers ---------- */

/* Steve: the default words come from Steve's i18n, built when shown so they follow the language */
const TEXT = (): DialText => ({
  levels: [
    { label: t("core:affect.dial.ask"), hint: t("core:affect.dial.askHint") },
    { label: t("core:affect.dial.risky"), hint: t("core:affect.dial.riskyHint") },
    { label: t("core:affect.dial.auto"), hint: t("core:affect.dial.autoHint") },
  ],
  group: t("core:affect.dial.group"),
  own: t("core:affect.dial.own"),
  ask: t("core:affect.dial.asks"),
  ownEmpty: t("core:affect.dial.ownEmpty"),
  askEmpty: t("core:affect.dial.askEmpty"),
  risky: t("core:affect.dial.riskyTag"),
  destructive: t("core:affect.dial.destructive"),
  current: t("core:affect.dial.current"),
  locked: t("core:affect.dial.locked"),
  lock: (label) => t("core:affect.dial.lock", { label }),
  unlock: (label) => t("core:affect.dial.unlock", { label }),
  announce: (level, own, ask) => t("core:affect.dial.announce", { level, own, ask }),
});
const LEVEL_IDS: AutonomyLevel[] = [0, 1, 2];
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
/** A calm in-and-out – the rows settle without a snap */
const EASE = "cubic-bezier(0.4, 0, 0.2, 1)";
const clamp = (n: number): AutonomyLevel => (Math.max(0, Math.min(2, Math.round(n))) as AutonomyLevel);
/** A broken or out-of-range level lands on the nearest real one */
const toLevel = (n: unknown, fallback: AutonomyLevel = 1): AutonomyLevel => (typeof n === "number" && Number.isFinite(n) ? clamp(n) : fallback);
/** An unknown risk counts as risky – it asks first, as before, and now carries its tag */
const toRisk = (r: unknown): ActionRisk => (r === "safe" || r === "destructive" ? r : "risky");
const num = new Intl.NumberFormat("en");
type Item = AgentAction & { key: string };

/* ---------- Icons ---------- */

/* Risk, lock and unlock: Hugeicons Free (MIT, © Hugeicons, hugeicons.com) – rounded 1.5px strokes */
const RiskyIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M13.9248 21H10.0752C5.44476 21 3.12955 21 2.27636 19.4939C1.42317 17.9879 2.60736 15.9914 4.97574 11.9985L6.90057 8.75333C9.17559 4.91778 10.3131 3 12 3C13.6869 3 14.8244 4.91777 17.0994 8.75332L19.0243 11.9985C21.3926 15.9914 22.5768 17.9879 21.7236 19.4939C20.8704 21 18.5552 21 13.9248 21Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M12 9V13" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M12.125 16.75H12M12.25 16.75C12.25 16.8881 12.1381 17 12 17C11.8619 17 11.75 16.8881 11.75 16.75C11.75 16.6119 11.8619 16.5 12 16.5C12.1381 16.5 12.25 16.6119 12.25 16.75Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
  </svg>
);
const DestructiveIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M5.92089 5.92089C8.15836 3.68342 9.2771 2.56468 10.5857 2.19562C11.5105 1.93479 12.4895 1.93479 13.4143 2.19562C14.7229 2.56468 15.8416 3.68342 18.0791 5.92089C20.3166 8.15836 21.4353 9.2771 21.8044 10.5857C22.0652 11.5105 22.0652 12.4895 21.8044 13.4143C21.4353 14.7229 20.3166 15.8416 18.0791 18.0791C15.8416 20.3166 14.7229 21.4353 13.4143 21.8044C12.4895 22.0652 11.5105 22.0652 10.5857 21.8044C9.2771 21.4353 8.15836 20.3166 5.92089 18.0791C3.68342 15.8416 2.56468 14.7229 2.19562 13.4143C1.93479 12.4895 1.93479 11.5105 2.19562 10.5857C2.56468 9.2771 3.68342 8.15836 5.92089 5.92089Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M12 8V12" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M12.125 15.75H12M12.25 15.75C12.25 15.8881 12.1381 16 12 16C11.8619 16 11.75 15.8881 11.75 15.75C11.75 15.6119 11.8619 15.5 12 15.5C12.1381 15.5 12.25 15.6119 12.25 15.75Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
  </svg>
);
const LockedIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M12 14.5V17.5M13 14.5C13 15.0523 12.5523 15.5 12 15.5C11.4477 15.5 11 15.0523 11 14.5C11 13.9477 11.4477 13.5 12 13.5C12.5523 13.5 13 13.9477 13 14.5Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M16.5 9V6.5C16.5 4.01472 14.4853 2 12 2C9.51471 2 7.49999 4.01472 7.49999 6.5V9" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M4.2678 18.8447C4.49268 20.515 5.87612 21.8235 7.55965 21.9009C8.97627 21.966 10.4153 22 12 22C13.5847 22 15.0237 21.966 16.4403 21.9009C18.1239 21.8235 19.5073 20.515 19.7322 18.8447C19.8789 17.7547 20 16.6376 20 15.5C20 14.3624 19.8789 13.2453 19.7322 12.1553C19.5073 10.485 18.1239 9.17649 16.4403 9.09909C15.0237 9.03397 13.5847 9 12 9C10.4153 9 8.97627 9.03397 7.55965 9.09909C5.87612 9.17649 4.49268 10.485 4.2678 12.1553C4.12104 13.2453 3.99999 14.3624 3.99999 15.5C3.99999 16.6376 4.12104 17.7547 4.2678 18.8447Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
  </svg>
);
const UnlockedIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <path d="M12 14.5V17.5M13 14.5C13 15.0523 12.5523 15.5 12 15.5C11.4477 15.5 11 15.0523 11 14.5C11 13.9477 11.4477 13.5 12 13.5C12.5523 13.5 13 13.9477 13 14.5Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M7.5 9V6.5C7.5 4.01472 9.51472 2 12 2C13.9593 2 15.8664 3.25221 16.5 5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
    <path d="M4.26781 18.8447C4.49269 20.515 5.87613 21.8235 7.55966 21.9009C8.97627 21.966 10.4153 22 12 22C13.5847 22 15.0237 21.966 16.4403 21.9009C18.1239 21.8235 19.5073 20.515 19.7322 18.8447C19.879 17.7547 20 16.6376 20 15.5C20 14.3624 19.879 13.2453 19.7322 12.1553C19.5073 10.485 18.1239 9.17649 16.4403 9.09909C15.0237 9.03397 13.5847 9 12 9C10.4153 9 8.97627 9.03397 7.55966 9.09909C5.87613 9.17649 4.49269 10.485 4.26781 12.1553C4.12104 13.2453 4 14.3624 4 15.5C4 16.6376 4.12104 17.7547 4.26781 18.8447Z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.5" />
  </svg>
);


const Spark = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path d="M7 1.2c.4 2.9 1.9 4.4 4.8 4.8-2.9.4-4.4 1.9-4.8 4.8C6.6 7.9 5.1 6.4 2.2 6 5.1 5.6 6.6 4.1 7 1.2Z" fill="currentColor" />
    <circle cx="11.6" cy="11.4" r="1.1" fill="currentColor" opacity="0.55" />
  </svg>
);
const DotIcon = () => (
  <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
    <circle cx="8" cy="8" r="2.4" fill="currentColor" />
  </svg>
);

/** A number that rolls to its new value */
function Roll({ value }: { value: number }) {
  const [shown, setShown] = useState(value);
  const [dir, setDir] = useState<"up" | "down" | null>(null);
  useEffect(() => {
    if (value === shown) return;
    setDir(value > shown ? "up" : "down");
    setShown(value);
  }, [value]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <span key={shown} className="ui-adlc__roll" data-dir={dir ?? undefined}>
      {num.format(shown)}
    </span>
  );
}

export function AutonomyDial({
  actions,
  level,
  defaultLevel = 1,
  onLevelChange,
  pinned,
  defaultPinned = [],
  onPinnedChange,
  title = t("core:affect.dial.title"),
  subtitle = t("core:affect.dial.subtitle"),
  className,
  text: textIn,
  lockable = true,
  disabledLevels = [],
  currentLevel,
  levelProps,
  tones = {},
  bullets = true,
  children,
}: AutonomyDialProps) {
  useT(); // Steve: the default words follow a change of language
  const text: DialText = { ...TEXT(), ...textIn };
  const LEVELS = LEVEL_IDS.map((id) => ({ id, ...text.levels[id] }));
  const RISK: Record<ActionRisk, string> = { safe: "", risky: text.risky, destructive: text.destructive };
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const [innerLevel, setInnerLevel] = useState<AutonomyLevel>(() => toLevel(defaultLevel));
  const [innerPinned, setInnerPinned] = useState<string[]>(() => (Array.isArray(defaultPinned) ? defaultPinned : []));
  const lvl = level == null ? innerLevel : toLevel(level, innerLevel);
  const pins = Array.isArray(pinned) ? pinned : innerPinned;

  const setLevel = (next: AutonomyLevel) => {
    if (next === lvl || disabledLevels.includes(next)) return;
    setInnerLevel(next);
    onLevelChange?.(next);
  };
  const togglePin = (id: string) => {
    const next = pins.includes(id) ? pins.filter((p) => p !== id) : [...pins, id];
    setInnerPinned(next);
    onPinnedChange?.(next);
  };

  // Gaps are skipped, labels become text, risks are known ones – and a repeated id gets its own key
  const seen = new Map<string, number>();
  const items: Item[] = (Array.isArray(actions) ? actions : [])
    .filter((a): a is AgentAction => !!a && typeof a === "object")
    .map((a) => {
      const id = String(a.id ?? "");
      const n = seen.get(id) ?? 0;
      seen.set(id, n + 1);
      return { ...a, id, label: String(a.label ?? ""), risk: toRisk(a.risk), key: n ? `${id}#${n}` : id };
    });
  const own = items.filter((a) => !asksFirst(a, lvl, pins));
  const ask = items.filter((a) => asksFirst(a, lvl, pins));

  /* ---------- The levels: a segmented control, arrow keys move between them ---------- */

  const segRef = useRef<HTMLDivElement>(null);
  const onKey = (e: KeyboardEvent) => {
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    let next = clamp(lvl + step);
    while (disabledLevels.includes(next) && next !== clamp(next + step)) next = clamp(next + step);
    if (disabledLevels.includes(next)) return;
    setLevel(next);
    segRef.current?.querySelector<HTMLElement>(`[data-level="${next}"]`)?.focus();
  };

  /* ---------- Motion: actions glide between the groups ---------- */

  const rows = useRef(new Map<string, HTMLElement>());
  const before = useRef(new Map<string, DOMRect>());
  const cardRef = useRef<HTMLDivElement>(null);
  const height = useRef(0);
  const shape = `${own.map((a) => a.key).join(",")}|${ask.map((a) => a.key).join(",")}`;
  const lastShape = useRef(shape);
  if (rootRef.current && shape !== lastShape.current) {
    before.current = new Map([...rows.current].map(([id, el]) => [id, el.getBoundingClientRect()]));
    height.current = cardRef.current?.offsetHeight ?? 0;
  }
  useLayoutEffect(() => {
    if (lastShape.current === shape) return;
    lastShape.current = shape;
    if (reduced()) return;
    const card = cardRef.current;
    if (card && height.current && Math.abs(card.offsetHeight - height.current) > 1) {
      card.animate([{ height: `${height.current}px` }, { height: `${card.offsetHeight}px` }], { duration: 420, easing: EASE });
    }
    let k = 0;
    for (const [id, el] of rows.current) {
      const from = before.current.get(id);
      if (!from) continue;
      const to = el.getBoundingClientRect();
      const dy = from.top - to.top;
      if (Math.abs(dy) < 1) continue;
      // Rows that change group glide a beat after each other
      el.animate([{ transform: `translateY(${dy}px)`, zIndex: 2 }, { transform: "none", zIndex: 2 }], {
        duration: 420,
        delay: k++ * 20,
        easing: EASE,
        fill: "backwards",
      });
    }
  }, [shape]);

  /* ---------- One action ---------- */

  const row = (a: Item, asks: boolean) => {
    const isPinned = pins.includes(a.id);
    return (
      <li
        key={a.key}
        className="ui-adlc__row"
        data-asks={asks ? "" : undefined}
        ref={(el) => {
          if (el) rows.current.set(a.key, el);
          else rows.current.delete(a.key);
        }}
      >
        {(a.icon || bullets) && <span className="ui-adlc__icon">{a.icon ?? <DotIcon />}</span>}
        <span className="ui-adlc__name">
          <span className="ui-adlc__line">
            <span className="ui-adlc__label" dir="auto" title={a.label}>
              {a.label}
            </span>
            {a.risk !== "safe" && (
              <span className="ui-adlc__risk" data-risk={a.risk}>
                {a.risk === "risky" ? <RiskyIcon /> : <DestructiveIcon />}
                {RISK[a.risk]}
              </span>
            )}
          </span>
          {a.detail && (
            <span className="ui-adlc__detail" dir="auto" title={String(a.detail)}>
              {a.detail}
            </span>
          )}
        </span>
        {lockable ? (
          <button
            type="button"
            className="ui-adlc__pin"
            aria-pressed={isPinned}
            aria-label={isPinned ? text.unlock(a.label) : text.lock(a.label)}
            title={isPinned ? text.unlock(a.label) : text.lock(a.label)}
            onClick={() => togglePin(a.id)}
          >
            {isPinned ? <LockedIcon /> : <UnlockedIcon />}
          </button>
        ) : (
          isPinned && (
            <span className="ui-adlc__pin" data-fixed="" role="img" aria-label={text.locked} title={text.locked}>
              <LockedIcon />
            </span>
          )
        )}
      </li>
    );
  };

  const current = LEVELS[lvl];

  return (
    <div ref={rootRef} className={["ui-adlc", className].filter(Boolean).join(" ")} data-level={lvl} data-tone={tones[lvl]}>
      <style href="ui-autonomy-dial-clean" precedence="default">
        {css}
      </style>

      <div ref={cardRef} className="ui-adlc__card">
        {title && <header className="ui-adlc__head">
          <span className="ui-adlc__spark">
            <Spark />
          </span>
          <span className="ui-adlc__heading">
            <span className="ui-adlc__title" dir="auto" title={title}>
              {title}
            </span>
            {subtitle && (
              <span className="ui-adlc__sub" dir="auto" title={subtitle}>
                {subtitle}
              </span>
            )}
          </span>
        </header>}

        {/* ----- The levels ----- */}
        <div className="ui-adlc__levels">
          <div
            ref={segRef}
            className="ui-adlc__segmented"
            role="radiogroup"
            aria-label={text.group}
            aria-describedby={`${uid}-hint`}
            onKeyDown={onKey}
          >
            {LEVELS.map((l) => (
              <button
                key={l.id}
                type="button"
                role="radio"
                aria-checked={l.id === lvl}
                tabIndex={l.id === lvl ? 0 : -1}
                data-level={l.id}
                data-tone={tones[l.id]}
                disabled={disabledLevels.includes(l.id)}
                className="ui-adlc__level"
                onClick={() => setLevel(l.id)}
                {...levelProps?.(l.id)}
              >
                {l.label}
                {currentLevel === l.id && currentLevel !== lvl && (
                  <span className="ui-adlc__current" title={text.current}>
                    <span className="ui-adlc__sr">{text.current}</span>
                  </span>
                )}
              </button>
            ))}
          </div>
          {/* Steve: what the chosen level means is said, not only announced */}
          <p className="ui-adlc__hint" id={`${uid}-hint`}>
            {current.hint}
          </p>
        </div>

        {/* Steve: what switching would do now, under the levels and before the actions */}
        {Children.toArray(children).length > 0 && <div className="ui-adlc__between">{children}</div>}

        {/* ----- What it does on its own, and what waits for you ----- */}
        <div className="ui-adlc__groups">
          <section className="ui-adlc__group" aria-labelledby={`${uid}-own`}>
            <h4 className="ui-adlc__group-head" id={`${uid}-own`}>
              {text.own}
              <span className="ui-adlc__count">
                <Roll value={own.length} />
              </span>
            </h4>
            <ul className="ui-adlc__list">
              {own.map((a) => row(a, false))}
              {own.length === 0 && <li className="ui-adlc__empty">{text.ownEmpty}</li>}
            </ul>
          </section>
          <section className="ui-adlc__group" aria-labelledby={`${uid}-ask`}>
            <h4 className="ui-adlc__group-head" id={`${uid}-ask`}>
              {text.ask}
              <span className="ui-adlc__count">
                <Roll value={ask.length} />
              </span>
            </h4>
            <ul className="ui-adlc__list">
              {ask.map((a) => row(a, true))}
              {ask.length === 0 && <li className="ui-adlc__empty">{text.askEmpty}</li>}
            </ul>
          </section>
        </div>
      </div>

      <span className="ui-adlc__sr" aria-live="polite">
        {text.announce(current.label, own.length, ask.length)}
      </span>
    </div>
  );
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-adlc */
.ui-adlc {
  --ui-adlc-card: var(--aui-surface, #ffffff);
  /* Steve: colors from Steve's tokens, which switch between light and dark themselves */
  --ui-adlc-strong: var(--aui-ink, #000000);
  --ui-adlc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-adlc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-adlc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-adlc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-adlc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-adlc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-adlc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.03));
  --ui-adlc-fill: var(--aui-subtle, rgba(0, 0, 0, 0.06));
  --ui-adlc-fill-hover: color-mix(in srgb, var(--aui-ink, #000) 8%, var(--ui-adlc-fill));
  --ui-adlc-accent: var(--aui-blue, rgba(0, 96, 226, 0.765));
  --ui-adlc-brand-bg: var(--aui-info-soft, #008ff519);
  --ui-adlc-brand-fg: var(--aui-info, #006dcbf2);
  --ui-adlc-warn-bg: var(--aui-warning-soft, #ff9c0029);
  --ui-adlc-warn-fg: var(--aui-warning, #b25e00);
  --ui-adlc-danger-bg: var(--aui-danger-soft, #f52b0018);
  --ui-adlc-danger-fg: var(--aui-danger, #d72400da);
  --ui-adlc-amber: var(--aui-warning, #b25e00);
  --ui-adlc-red: var(--aui-danger, #d72400da);
  --ui-adlc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-adlc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-adlc-ease: cubic-bezier(0.32, 0.72, 0.24, 1);
  position: relative;
  box-sizing: border-box;
  width: 100%;
  max-width: var(--ui-adlc-max, 400px);
  container-type: inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  font-weight: 400;
  line-height: 20px;
  color: var(--ui-adlc-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
.ui-adlc *, .ui-adlc *::before, .ui-adlc *::after { box-sizing: border-box; }
:where(.ui-adlc) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-adlc button:focus-visible { outline: 2px solid var(--ui-adlc-focus); outline-offset: 2px; }
.ui-adlc ul { margin: 0; padding: 0; list-style: none; }
.ui-adlc h4 { margin: 0; }
.ui-adlc svg { display: block; flex: none; }
.ui-adlc__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }

/* One white card – the edge is a shadow, not a border */
.ui-adlc__card { overflow: hidden; border-radius: var(--aui-radius-lg, 10px); background: var(--ui-adlc-card); box-shadow: var(--aui-shadow-card, var(--ui-adlc-shadow)); }

.ui-adlc__head { display: flex; align-items: center; gap: 10px; padding: 16px 16px 0; }
.ui-adlc__card > .ui-adlc__levels:first-child { padding-top: 12px; }
.ui-adlc__spark { display: grid; flex: none; place-items: center; width: 16px; color: var(--ui-adlc-accent); }
.ui-adlc__heading { display: grid; min-width: 0; }
.ui-adlc__title { overflow: hidden; font-weight: 500; color: var(--ui-adlc-strong); white-space: nowrap; text-overflow: ellipsis; }
.ui-adlc__sub { overflow: hidden; font-size: 12px; line-height: 16px; color: var(--ui-adlc-muted); white-space: nowrap; text-overflow: ellipsis; }

/* ----- The levels: three tab buttons ----- */
.ui-adlc__levels { padding: 16px 16px 18px; }
/* Steve: the levels wrap rather than clip when the dialog is narrow or the words are long */
.ui-adlc__segmented { position: relative; display: flex; flex-wrap: wrap; gap: 8px; }
/* Separate buttons: the others are gray, the chosen one is white with a hairline and a soft lift */
.ui-adlc__level { position: relative; flex: 1 1 0; min-width: fit-content; max-width: 100%; padding: 3px 8px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-adlc-fill); font-size: 12px; font-weight: 500; line-height: 18px; color: var(--ui-adlc-text); text-align: center !important; overflow-wrap: break-word; transition: color 180ms ease, background-color 180ms ease, box-shadow 180ms ease; }
.ui-adlc__level[aria-checked="true"] { background: var(--ui-adlc-card); box-shadow: var(--ui-adlc-shadow); color: var(--ui-adlc-strong); }
.ui-adlc__level[aria-checked="false"]:not(:disabled):hover { background: var(--ui-adlc-fill-hover); color: var(--ui-adlc-ink); }
/* Steve: levels that can't be picked now, the one in force while another is picked, full access in its colour, the hint */
.ui-adlc__level:disabled { cursor: default; opacity: 0.45; }
.ui-adlc__current { display: inline-block; width: 5px; height: 5px; margin: 0 0 1px 6px; border-radius: 50%; background: currentColor; opacity: 0.55; vertical-align: middle; }
.ui-adlc__level[data-tone="attention"][aria-checked="true"] { color: var(--ui-adlc-warn-fg); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--ui-adlc-warn-fg) 45%, transparent); background: color-mix(in srgb, var(--ui-adlc-warn-fg) 9%, var(--ui-adlc-card)); }
.ui-adlc__hint { margin: 10px 2px 0; font-size: 12px; line-height: 18px; color: var(--ui-adlc-muted); overflow-wrap: anywhere; }
.ui-adlc__pin[data-fixed] { opacity: 1; cursor: default; color: var(--ui-adlc-muted); }
.ui-adlc__pin[data-fixed]:hover { background: none; }

/* ----- The two groups: spacing between them, one hairline above ----- */
.ui-adlc__groups { display: grid; gap: 14px; padding: 14px 10px 12px; border-top: 0.5px solid var(--ui-adlc-line); }
.ui-adlc__group-head { display: flex; align-items: center; gap: 6px; margin: 0 6px 4px !important; font-size: 12px; font-weight: 400; line-height: 16px; color: var(--ui-adlc-muted); }
/* The count: a small white label with a hairline and a soft lift – no taller than the heading */
.ui-adlc__count { display: grid; place-items: center; min-width: 16px; height: 16px; padding: 0 4px; overflow: hidden; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-adlc-card); box-shadow: var(--ui-adlc-shadow); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-adlc-text); font-variant-numeric: tabular-nums; }
.ui-adlc__roll { display: block; }
.ui-adlc__roll[data-dir="up"] { animation: ui-adlc-roll-up 360ms var(--ui-adlc-ease) both; }
.ui-adlc__roll[data-dir="down"] { animation: ui-adlc-roll-down 360ms var(--ui-adlc-ease) both; }
.ui-adlc__list { display: grid; }
.ui-adlc__between { display: flex; flex-direction: column; gap: 10px; min-width: 0; padding: 0 16px 14px; }
.ui-adlc__row { position: relative; display: flex; align-items: center; gap: 12px; min-height: 30px; padding: 3px 4px 3px 6px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-adlc-card); transition: background-color 160ms ease; }
.ui-adlc__row:hover { background: var(--ui-adlc-wash); }
.ui-adlc__icon { display: grid; flex: none; place-items: center; width: 16px; height: 16px; color: var(--ui-adlc-icon); }
.ui-adlc__icon > svg { width: 16px; height: 16px; }
/* An image as icon fits its 16px tile, whatever its shape */
.ui-adlc__icon { overflow: hidden; }
.ui-adlc__icon > img { display: block; width: 16px; height: 16px; object-fit: contain; }
.ui-adlc__name { display: grid; flex: 1; min-width: 0; }
/* Steve: names and details wrap rather than clip – Chinese action names run long */
.ui-adlc__line { display: flex; flex-wrap: wrap; align-items: center; gap: 2px 8px; min-width: 0; }
.ui-adlc__label { min-width: 0; color: var(--ui-adlc-ink); overflow-wrap: anywhere; }
.ui-adlc__detail { font-size: 12px; line-height: 16px; color: var(--ui-adlc-muted); overflow-wrap: anywhere; }
/* Risk: a small tag right after the name – glyph and word on a subtle tone; safe actions carry nothing */
.ui-adlc__risk { display: inline-flex; flex: none; align-items: center; gap: 3px; height: 18px; padding: 0 6px 0 4px; border-radius: calc(var(--aui-radius-sm, 5px) - 1px); background: var(--ui-adlc-warn-bg); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-adlc-warn-fg); }
.ui-adlc__risk > svg { width: 12px; height: 12px; }
.ui-adlc__risk[data-risk="destructive"] { background: var(--ui-adlc-danger-bg); color: var(--ui-adlc-danger-fg); }
.ui-adlc__pin { display: grid; flex: none; place-items: center; width: 28px; height: 28px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); color: var(--ui-adlc-faint); opacity: 0; transition: opacity 160ms ease, color 160ms ease, background-color 160ms ease; }
.ui-adlc__row:hover .ui-adlc__pin, .ui-adlc__pin:focus-visible, .ui-adlc__pin[aria-pressed="true"] { opacity: 1; }
.ui-adlc__pin:hover { background: var(--ui-adlc-fill); color: var(--ui-adlc-ink); }
.ui-adlc__pin[aria-pressed="true"] { color: var(--ui-adlc-icon); }
/* Empty group: one quiet gray tag across the whole width */
.ui-adlc__empty { display: flex; align-items: center; justify-content: center; min-height: 30px; margin: 2px 0; padding: 6px 10px; border-radius: calc(var(--aui-radius-sm, 5px) + 1px); background: var(--ui-adlc-wash); font-size: 12px; line-height: 16px; color: var(--ui-adlc-muted); text-align: center; }

@keyframes ui-adlc-roll-up { from { opacity: 0; transform: translateY(70%); } }
@keyframes ui-adlc-roll-down { from { opacity: 0; transform: translateY(-70%); } }

/* Narrow containers: the detail lines go, the levels get tighter */
@container (max-width: 340px) {
  .ui-adlc__segmented { gap: 4px; }
  .ui-adlc__level { padding: 4px 6px; font-size: 12px; }
}
/* Touch: no hover – the locks are always visible */
@media (hover: none) {
  .ui-adlc__pin { opacity: 1; }
}
@media (pointer: coarse) {
  .ui-adlc__level { padding: 8px 10px; }
  .ui-adlc__pin { width: 34px; height: 34px; }
  .ui-adlc__row { min-height: 44px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-adlc *, .ui-adlc *::before, .ui-adlc *::after { animation: none !important; transition: none !important; }
}

/* Steve: dark mode comes with the tokens above */
`;
