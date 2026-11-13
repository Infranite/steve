/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useRef, type KeyboardEvent, type ReactNode } from "react";

/*
 * Permissions – what your agent may do on its own, in a single file.
 * Decided from the top down: one choice – how much it does on its own – as tiles on a gray track, the picked one a
 * card that glides to the next pick, each said in plain words; and under it, what that means in one line.
 * Keyboard: the choices are a radio group (← → ↑ ↓). Focus rings show for the keyboard only.
 *
 * Steve: the AutonomyCard of affectUI's Agent / Permissions screen, without its App Shell, for any number of choices.
 * A Steve Bot's permissions are one mode for all of its tools (the harness's permission modes) and how its work is
 * accepted; there are no per-app actions, limits or a history of what each action did, so the app list, the per-action
 * menus, the counts bar and the App panel have no data behind them and are left out. The red lines that always ask
 * come in as the `means` line. Colours come from Steve's tokens (--aui-*, styles/affectui-theme.css).
 */

export type AutonomyOption<T extends string> = {
  value: T;
  /** The choice in a word or two */
  name: string;
  /** What it means, in plain words */
  text: string;
  icon?: ReactNode;
};

export type AutonomyChoiceProps<T extends string> = {
  /** The section's heading, also the radio group's name */
  label: string;
  options: AutonomyOption<T>[];
  value: T;
  onChange: (v: T) => void;
  /** One line under the tiles: what the pick means, or what always holds whatever is picked */
  means?: ReactNode;
  className?: string;
  /** test hook on the radio group; each tile carries data-choice={value} */
  testId?: string;
};

export function AutonomyChoice<T extends string>({ label, options, value, onChange, means, className, testId }: AutonomyChoiceProps<T>) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const at = options.findIndex((o) => o.value === value);
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const i = ((at < 0 ? (step > 0 ? -1 : options.length) : at) + step + options.length) % options.length;
    onChange(options[i].value);
    refs.current[i]?.focus();
  };
  return (
    <div className={["ui-apm", className].filter(Boolean).join(" ")}>
      <style href="ui-screen-agent-permissions" precedence="default">
        {css}
      </style>
      <div className="ui-apm-card ui-apm-auto">
        <div
          className="ui-apm-tiles"
          role="radiogroup"
          aria-label={label}
          onKeyDown={onKey}
          data-choices={testId}
          data-hidden-thumb={at < 0 || undefined}
          style={{ ["--apm-at" as string]: Math.max(0, at), ["--apm-n" as string]: options.length }}
        >
          {/* The picked one is a card on the gray track – it glides to the next pick */}
          <span className="ui-apm-tiles__thumb" aria-hidden="true" />
          {options.map((o, i) => (
            <button
              key={o.value}
              ref={(el) => {
                refs.current[i] = el;
              }}
              type="button"
              role="radio"
              className="ui-apm-tile"
              aria-checked={value === o.value}
              tabIndex={value === o.value || (at < 0 && i === 0) ? 0 : -1}
              onClick={() => onChange(o.value)}
              data-choice={o.value}
            >
              <span className="ui-apm-tile__head">
                {o.icon && (
                  <span className="ui-apm-tile__icon" aria-hidden="true">
                    {o.icon}
                  </span>
                )}
                <span className="ui-apm-tile__name">{o.name}</span>
              </span>
              <span className="ui-apm-tile__text" data-icon={o.icon ? "" : undefined}>
                {o.text}
              </span>
            </button>
          ))}
        </div>
        {means && (
          <div className="ui-apm-means">
            <p className="ui-apm-means__line">{means}</p>
          </div>
        )}
      </div>
    </div>
  );
}

/* ---------- Styles ---------- */

const css = /* css */ `
.ui-apm {
  --ui-apm-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --apm-out: cubic-bezier(0.23, 1, 0.32, 1);
  --apm-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --apm-muted: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --apm-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.42));
  --apm-card: var(--aui-surface, #ffffff);
  --apm-card-shadow: var(--aui-shadow-card, 0 0 0 0.5px var(--aui-border, rgba(0, 0, 0, 0.06)), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --apm-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --apm-allow: var(--aui-primary, #0190fe);
  --apm-track: color-mix(in srgb, var(--apm-ink) 3.5%, var(--apm-card));
  container: ui-apm / inline-size;
  font-family: var(--aui-font, inherit);
  -webkit-font-smoothing: antialiased;
}
:where(.dark, [data-theme="dark"]) .ui-apm {
  --apm-card-shadow: var(--aui-shadow-card, 0 0 0 0.5px var(--aui-border, rgba(255, 255, 255, 0.08)), 0 1px 2px 0 rgba(0, 0, 0, 0.3));
  --apm-line: var(--aui-border, rgba(255, 255, 255, 0.07));
  --apm-track: color-mix(in srgb, var(--apm-ink) 5%, var(--apm-card));
  /* the picked tile stands out from the track a step lighter, as a raised card does on Steve's dark surfaces */
  --apm-thumb: color-mix(in srgb, var(--apm-ink) 10%, var(--apm-card));
}
.ui-apm *, .ui-apm *::before, .ui-apm *::after { box-sizing: border-box; }
.ui-apm :focus-visible { outline: 2px solid var(--ui-apm-focus); outline-offset: 2px; }

.ui-apm-card { margin: 0; padding: 0; border-radius: var(--aui-radius-lg, 10px); background: var(--apm-card); box-shadow: var(--apm-card-shadow); }
.ui-apm-auto { padding: 6px; }
.ui-apm-tiles { position: relative; display: grid; grid-template-columns: repeat(var(--apm-n, 3), minmax(0, 1fr)); gap: 2px; padding: 3px; border-radius: calc(var(--aui-radius, 7px) + 2px); background: var(--apm-track); isolation: isolate; }
.ui-apm-tiles__thumb { position: absolute; z-index: -1; top: 3px; bottom: 3px; left: 3px; width: calc((100% - 6px - (var(--apm-n, 3) - 1) * 2px) / var(--apm-n, 3)); border-radius: var(--aui-radius, 7px); background: var(--apm-thumb, var(--apm-card)); box-shadow: var(--apm-card-shadow); transform: translateX(calc(var(--apm-at, 0) * (100% + 2px))); transition: transform 260ms var(--apm-out); }
.ui-apm-tiles[data-hidden-thumb] .ui-apm-tiles__thumb { display: none; }
.ui-apm-tile { display: grid; align-content: start; gap: 4px; min-width: 0; padding: 10px 12px 12px; border: 0; border-radius: var(--aui-radius, 7px); background: none; font: inherit; text-align: left; cursor: pointer; transition: color 180ms ease; }
.ui-apm-tile:focus-visible { outline-offset: -2px; }
.ui-apm-tile__head { display: flex; align-items: center; gap: 8px; min-width: 0; }
.ui-apm-tile__icon { display: grid; flex: none; place-items: center; width: 16px; color: var(--apm-faint); transition: color 180ms ease; }
.ui-apm-tile__icon > svg { width: 15px; height: 15px; }
.ui-apm-tile:hover .ui-apm-tile__icon { color: var(--apm-muted); }
.ui-apm-tile[aria-checked="true"] .ui-apm-tile__icon { color: var(--apm-allow); }
.ui-apm-tile__name { min-width: 0; font-size: 13px; font-weight: 500; line-height: 20px; color: var(--apm-ink); }
.ui-apm-tile:not([aria-checked="true"]) .ui-apm-tile__name { color: var(--apm-muted); }
.ui-apm-tile__text { font-size: 12px; line-height: 16px; color: var(--apm-muted); overflow-wrap: anywhere; }
.ui-apm-tile__text[data-icon] { padding-left: 24px; }
.ui-apm-means { display: grid; gap: 10px; margin-top: 6px; padding: 10px 8px 6px; box-shadow: inset 0 0.5px 0 var(--apm-line); }
.ui-apm-means__line { display: flex; align-items: flex-start; gap: 8px; margin: 0; font-size: 12.5px; line-height: 18px; color: var(--apm-muted); }
.ui-apm-means__line > svg { flex: none; width: 14px; height: 14px; margin-top: 2px; }
.ui-apm-means__line b { font-weight: 500; color: var(--apm-ink); }

/* Narrow, or more choices than fit side by side: one under the other, the picked one a card of its own */
@container ui-apm (max-width: 620px) {
  .ui-apm-tiles { grid-template-columns: minmax(0, 1fr); }
  .ui-apm-tiles__thumb { display: none; }
  .ui-apm-tile { padding: 8px 10px; }
  .ui-apm-tile[aria-checked="true"] { background: var(--apm-thumb, var(--apm-card)); box-shadow: var(--apm-card-shadow); }
}
@media (pointer: coarse) {
  .ui-apm-tile { min-height: 36px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-apm-tiles__thumb, .ui-apm-tile, .ui-apm-tile__icon { transition: none; }
}
@media (forced-colors: active) {
  .ui-apm-tile[aria-checked="true"] { outline: 2px solid Highlight; }
}
`;
