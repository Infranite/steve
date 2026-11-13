/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useState, type KeyboardEvent, type ReactNode } from "react";

/*
 * Skills – what your agent knows how to do, in a single file.
 * Filters by what a skill is for on top, then the skills as calm cards – a small icon and the name, two quiet lines on
 * what it does, a chip for where it comes from and how often it ran.
 * Keyboard: ← / → move from filter to filter.
 *
 * Steve: the tile grid of affectUI's Agent / Skills screen, without its App Shell. Steve's Bots have tools, not
 * installable skills, and no per-tool switch: Discover, Install, New skill, the switch and the side panel (its two weeks
 * of runs, triggers and steps) have no data behind them in Steve and are left out. Colours come from Steve's tokens
 * (--aui-*, styles/affectui-theme.css); icons are passed in by the caller (lucide).
 */

export type Skill = {
  id: string;
  /** Shown as the card's title */
  name: string;
  /** What it does, in one line */
  description: string;
  /** What it's for – the filter it shows under */
  category: string;
  /** A small mark before the name */
  icon?: ReactNode;
  /** A palette name (purple, orange, teal, blue, amber, pink, green) or any CSS color */
  tone?: string;
  /** Where it comes from, ready to show (the chip) */
  source?: string;
  /** How often it ran, ready to show – e.g. "120 calls in 30 days"; left out when unknown */
  uses?: string;
  /** Shown faded: the agent can use it but hasn't */
  idle?: boolean;
};

export type SkillLabels = { all: string; categories: string; empty: string };

const TONES: Record<string, string> = {
  purple: "var(--agk-c-purple)",
  orange: "var(--agk-c-orange)",
  teal: "var(--agk-c-teal)",
  blue: "var(--agk-c-blue)",
  amber: "var(--agk-c-amber)",
  pink: "var(--agk-c-pink)",
  green: "var(--agk-c-green)",
};
const toneOf = (t?: string) => (t ? (TONES[t] ?? t) : TONES.blue);

/** Roving focus through a list of choices with the arrow keys */
function rove<T extends string>(e: KeyboardEvent<HTMLElement>, list: T[], cur: T, set: (v: T) => void) {
  const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
  if (!step) return;
  e.preventDefault();
  const next = list[(list.indexOf(cur) + step + list.length) % list.length];
  set(next);
  (e.currentTarget.querySelectorAll<HTMLElement>("[role=tab]")[list.indexOf(next)])?.focus();
}

export type SkillGridProps = {
  skills: Skill[];
  /** The order of the filters – defaults to the categories in the order they appear */
  categories?: string[];
  labels: SkillLabels;
  className?: string;
  /** a test hook on each card */
  "data-testid"?: string;
};

/** The skills as calm cards in as many columns as fit, with the categories as filters above */
export function SkillGrid({ skills, categories, labels, className, ...rest }: SkillGridProps) {
  const [cat, setCat] = useState("all");
  const cats = ["all", ...(categories ?? [...new Set(skills.map((s) => s.category))]).filter((c) => skills.some((s) => s.category === c))];
  const cur = cats.includes(cat) ? cat : "all";
  const shown = skills.filter((s) => cur === "all" || s.category === cur);
  return (
    <div className={["ui-agk", className].filter(Boolean).join(" ")} {...rest}>
      <style href="ui-screen-agent-skills" precedence="default">
        {css}
      </style>
      {cats.length > 2 && (
        <div className="ui-agk-chips" role="tablist" aria-label={labels.categories} onKeyDown={(e) => rove(e, cats, cur, setCat)}>
          {cats.map((c) => (
            <button key={c} type="button" role="tab" aria-selected={cur === c} tabIndex={cur === c ? 0 : -1} onClick={() => setCat(c)} data-skill-cat={c}>
              {c === "all" ? labels.all : c}
            </button>
          ))}
        </div>
      )}
      {shown.length ? (
        <ul className="ui-agk-tiles">
          {shown.map((s) => (
            <SkillTile key={s.id} skill={s} />
          ))}
        </ul>
      ) : (
        <p className="ui-agk-note">{labels.empty}</p>
      )}
    </div>
  );
}

/** A skill's mark: its icon in its color on a light tile of the same color */
function SkillMark({ skill, size = 20 }: { skill: Skill; size?: number }) {
  return (
    <span className="ui-agk-mark" style={{ ["--agk-tone" as string]: toneOf(skill.tone), width: size, height: size }} aria-hidden="true">
      {skill.icon}
    </span>
  );
}

/** One skill as a calm card: icon and name, what it does, where it comes from and how often it ran */
function SkillTile({ skill: s }: { skill: Skill }) {
  return (
    <li className="ui-agk-card" data-off={s.idle || undefined}>
      <div className="ui-agk-card__main" data-skill={s.id}>
        <span className="ui-agk-card__top">
          <SkillMark skill={s} />
          <span className="ui-agk-card__name" dir="auto" title={s.name}>
            {s.name}
          </span>
        </span>
        <span className="ui-agk-card__desc" dir="auto" title={s.description}>
          {s.description}
        </span>
        <span className="ui-agk-card__foot">
          {s.source && (
            <span className="ui-agk-chip" dir="auto">
              {s.source}
            </span>
          )}
          {s.uses && <span className="ui-agk-card__count ui-agk-num">{s.uses}</span>}
        </span>
      </div>
    </li>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
.ui-agk {
  --ui-agk-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --agk-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --agk-muted: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --agk-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.42));
  --agk-card: var(--aui-surface, #ffffff);
  --agk-chip: color-mix(in srgb, var(--agk-ink) 6%, var(--agk-card));
  --agk-edge: var(--aui-border, rgba(0, 0, 0, 0.09));
  --agk-edge-soft: color-mix(in srgb, var(--agk-edge) 60%, transparent);
  --agk-c-purple: #a651f0;
  --agk-c-orange: #f28a2e;
  --agk-c-teal: #13b3a0;
  --agk-c-blue: var(--aui-primary, #0190fe);
  --agk-c-amber: #e8b000;
  --agk-c-pink: #ee3e9e;
  --agk-c-green: #5fc88a;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 12px;
  container: ui-agk / inline-size;
  font-family: var(--aui-font, inherit);
  -webkit-font-smoothing: antialiased;
}
:where(.dark, [data-theme="dark"]) .ui-agk {
  --agk-chip: color-mix(in srgb, var(--agk-ink) 9%, var(--agk-card));
  --agk-c-purple: #b98af0;
  --agk-c-orange: #f2a15c;
  --agk-c-teal: #4fc3b4;
  --agk-c-amber: #e6c24d;
  --agk-c-pink: #f06db5;
  --agk-c-green: #7dcf9f;
}
.ui-agk *, .ui-agk *::before, .ui-agk *::after { box-sizing: border-box; }
.ui-agk-num { font-variant-numeric: tabular-nums; }
.ui-agk :focus-visible { outline: 2px solid var(--ui-agk-focus); outline-offset: 2px; }

.ui-agk-chips { display: flex; gap: 4px; margin: 0 -4px; padding: 0 4px; overflow-x: auto; scrollbar-width: none; }
.ui-agk-chips::-webkit-scrollbar { display: none; }
.ui-agk-chips button { flex: none; height: 28px; padding: 0 10px; border: 0; border-radius: var(--aui-radius-sm, 6px); background: none; font: inherit; font-size: calc(13px * var(--type-scale, 1)); color: var(--agk-muted); white-space: nowrap; cursor: pointer; transition: background-color 120ms ease, color 120ms ease; }
.ui-agk-chips button:hover { color: var(--agk-ink); }
.ui-agk-chips button[aria-selected="true"] { background: var(--agk-chip); color: var(--agk-ink); }
.ui-agk-tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; margin: 0; padding: 0; list-style: none; }
.ui-agk-mark { display: grid; flex: none; place-items: center; border-radius: 5px; background: color-mix(in srgb, var(--agk-tone) 14%, var(--agk-card)); color: var(--agk-tone); transition: filter 180ms ease, opacity 180ms ease; }
.ui-agk-mark > svg { width: 12px; height: 12px; }

/* Cards – calm: a hairline, small icon and name, two quiet lines, one chip */
.ui-agk-card { position: relative; display: flex; min-width: 0; border-radius: var(--aui-radius-lg, 10px); background: var(--agk-card); box-shadow: var(--aui-shadow-card, inset 0 0 0 0.5px var(--agk-edge)); transition: box-shadow 180ms ease, background-color 180ms ease; }
.ui-agk-card__main { display: grid; flex: 1; align-content: start; gap: 6px; min-width: 0; padding: 12px 14px; border-radius: inherit; text-align: left; }
.ui-agk-card__main:focus-visible { outline-offset: -2px; }
.ui-agk-card__top { display: flex; align-items: center; gap: 8px; min-width: 0; height: 24px; }
.ui-agk-card__name { overflow: hidden; font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 20px; color: var(--agk-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-agk-card__desc { display: -webkit-box; min-height: 36px; overflow: hidden; overflow-wrap: anywhere; font-size: calc(12.5px * var(--type-scale, 1)); line-height: 18px; color: var(--agk-muted); -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
.ui-agk-card__foot { display: flex; align-items: center; gap: 8px; min-width: 0; margin-top: 4px; }
.ui-agk-card__count { margin-left: auto; font-size: calc(11.5px * var(--type-scale, 1)); color: var(--agk-faint); white-space: nowrap; }
.ui-agk-chip { max-width: 100%; overflow: hidden; padding: 0 6px; border-radius: 5px; background: var(--agk-chip); font-size: calc(11.5px * var(--type-scale, 1)); line-height: 18px; color: var(--agk-muted); white-space: nowrap; text-overflow: ellipsis; }
.ui-agk-card[data-off] .ui-agk-card__name { color: var(--agk-muted); }
.ui-agk-card[data-off] .ui-agk-mark { filter: grayscale(1); opacity: 0.55; }
.ui-agk-note { margin: 0; font-size: calc(13px * var(--type-scale, 1)); color: var(--agk-faint); }

@container ui-agk (max-width: 480px) {
  .ui-agk-tiles { grid-template-columns: minmax(0, 1fr); }
  .ui-agk-card__desc { min-height: 0; }
}
@media (pointer: coarse) {
  .ui-agk-chips button { min-height: 36px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-agk-card, .ui-agk-mark { transition: none; }
}
`;
