/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useT } from "@/i18n";
import { num as localNum } from "@/i18n/format";

/*
 * Compact Breakdown – one total split into its parts, in a small card and a single self-contained file.
 * A big number, a thin stacked bar and the parts as a ranked list. Switching the period rolls every number,
 * the bar glides to its new split and the rows trade places when the ranking changes. Hovering a part (in the
 * bar or the list) dims the rest and rolls the big number to that part's value; leaving rolls it back.
 * The styles are injected below, so no extra files are needed.
 */

export type BreakdownItem = {
  /** Stable key across periods – defaults to the label */
  id?: string;
  label: string;
  value: number;
  /** Any CSS color – defaults to shades of the accent, then grays */
  color?: string;
};

export type BreakdownPeriod = { id: string; label: string };

export type CompactBreakdownProps = {
  title?: string;
  /** The parts of the shown period – the list ranks them by value */
  items: BreakdownItem[];
  /** Options of the period switch – no switch when left out */
  periods?: BreakdownPeriod[];
  /** Controlled period id */
  period?: string;
  defaultPeriod?: string;
  /** Called with the new period id – pass the matching `items` and `delta` back in */
  onPeriodChange?: (id: string) => void;
  /** Change against the previous period in percent, e.g. 12.4 or -3 – shown as a small pill */
  delta?: number;
  /** Steve: how the pill reads – "good-up" (default: up green, down red) or "neutral" (muted either way, e.g. for spend) */
  deltaTone?: "good-up" | "neutral";
  /** Formats every value, e.g. (n) => `$${Math.round(n).toLocaleString("en-US")}` */
  format?: (value: number) => string;
  /** Text under the big number while nothing is hovered */
  totalLabel?: string;
  /** Replaces the line on the gray tray (default: the largest part); null hides the tray line */
  footer?: ReactNode;
  className?: string;
  /** Steve: inside a panel – no gray tray and no card of its own */
  bare?: boolean;
  /** Steve: the row of each part, for tests (`data-*`) */
  rowProps?: (item: { key: string; label: string }) => Record<string, string | undefined>;
};

/* ---------- Helpers ---------- */

const defaultFormat = (n: number) => new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 }).format(n);
const PALETTE = ["var(--ui-cb-c1)", "var(--ui-cb-c2)", "var(--ui-cb-c3)", "var(--ui-cb-c4)", "var(--ui-cb-c5)"];
const EASE = "cubic-bezier(0.22, 1, 0.36, 1)";
const ease = (t: number) => 1 - Math.pow(1 - t, 4);
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const keyOf = (item: BreakdownItem) => item.id ?? item.label;
// NaN, Infinity and negatives count as nothing, so no share or width turns into NaN
const num = (n: number) => (Number.isFinite(n) ? Math.max(0, n) : 0);
// A part that exists but rounds to 0% still says so – and one that isn't everything never reads 100%
const pct = (v: number) => (v > 0 && v < 1 ? "<1%" : v > 99 && v < 100 ? ">99%" : `${Math.round(v)}%`);

/**
 * Tweens a set of named numbers: every key rolls from where it is now to its target, new keys from zero.
 * One animation frame loop for the whole card, so numbers and bar widths move in step.
 */
function useTween(target: Record<string, number>, first: number, then: number) {
  const [shown, setShown] = useState<Record<string, number>>(() => Object.fromEntries(Object.keys(target).map((k) => [k, 0])));
  const current = useRef(shown);
  const started = useRef(false);
  const signature = JSON.stringify(target);
  useEffect(() => {
    const to = JSON.parse(signature) as Record<string, number>;
    const from = { ...current.current };
    if (reduced()) {
      current.current = to;
      setShown(to);
      return;
    }
    const duration = started.current ? then : first;
    started.current = true;
    const start = performance.now();
    let frame = 0;
    const step = (now: number) => {
      const t = Math.min(1, (now - start) / duration);
      const e = ease(t);
      const next: Record<string, number> = {};
      for (const k in to) next[k] = (from[k] ?? 0) + (to[k] - (from[k] ?? 0)) * e;
      current.current = next;
      setShown(next);
      if (t < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [signature]); // eslint-disable-line react-hooks/exhaustive-deps
  return shown;
}

export function CompactBreakdown({
  title,
  items,
  periods,
  period,
  defaultPeriod,
  onPeriodChange,
  delta,
  deltaTone = "good-up",
  format = defaultFormat,
  totalLabel,
  footer,
  className,
  bare = false,
  rowProps,
}: CompactBreakdownProps) {
  const t = useT();
  title ??= t("stats:breakdown.title");
  totalLabel ??= t("stats:breakdown.total");
  const uid = useId();
  const [innerPeriod, setInnerPeriod] = useState(defaultPeriod ?? periods?.[0]?.id);
  const activePeriod = period ?? innerPeriod;
  const [active, setActive] = useState<string | null>(null);

  // Every part ever shown keeps its place in the bar and its color, so the bar only glides – never reshuffles
  const known = useRef<{ key: string; label: string; color: string }[]>([]);
  // Parts with the same label or id keep their own row: the second one becomes "label#1"
  const counts = new Map<string, number>();
  const keyed = items.map((item) => {
    const base = keyOf(item);
    const n = counts.get(base) ?? 0;
    counts.set(base, n + 1);
    return { item, key: n ? `${base}#${n}` : base };
  });
  for (const { item, key } of keyed) {
    const seen = known.current.find((k) => k.key === key);
    if (seen) {
      seen.label = item.label;
      if (item.color) seen.color = item.color;
    } else {
      known.current.push({ key, label: item.label, color: item.color ?? PALETTE[known.current.length] ?? "var(--ui-cb-c5)" });
    }
  }

  const now = new Map(keyed.map(({ item, key }) => [key, num(item.value)]));
  const total = [...now.values()].reduce((a, b) => a + b, 0);
  const activeValid = active !== null && now.has(active) ? active : null;

  // One tween for everything: each part, the total and the big figure (the total, or the hovered part)
  const target: Record<string, number> = { "#total": total, "#figure": activeValid ? (now.get(activeValid) ?? 0) : total };
  for (const k of known.current) target[k.key] = now.get(k.key) ?? 0;
  const shown = useTween(target, 900, 620);
  const shownTotal = Math.max(shown["#total"] ?? 0, 0.0001);

  // Rows: ranked by the new values; parts that left the period fold away at the end
  const rows = known.current
    .map((k) => ({ ...k, value: now.get(k.key) ?? 0, leaving: !now.has(k.key) }))
    .filter((r) => !r.leaving || (shown[r.key] ?? 0) > 0.5)
    .sort((a, b) => Number(a.leaving) - Number(b.leaving) || b.value - a.value);
  const order = rows.map((r) => r.key).join("|");
  // "Largest" only names a part that has a value – not the first of all zeros
  const top = rows.find((r) => !r.leaving && r.value > 0);

  // Rows glide to their new rank (FLIP): remember where each one was, then animate from there
  const rowEls = useRef(new Map<string, HTMLLIElement>());
  const tops = useRef(new Map<string, number>());
  useLayoutEffect(() => {
    const next = new Map<string, number>();
    rowEls.current.forEach((el, key) => next.set(key, el.offsetTop));
    if (!reduced()) {
      next.forEach((y, key) => {
        const before = tops.current.get(key);
        const el = rowEls.current.get(key);
        if (el && before !== undefined && before !== y) {
          el.animate([{ transform: `translateY(${before - y}px)` }, { transform: "none" }], { duration: 560, easing: EASE });
        }
      });
    }
    tops.current = next;
  }, [order]);

  // The period switch: a pill that slides under the chosen option
  const switchRef = useRef<HTMLDivElement>(null);
  const pillRef = useRef<HTMLSpanElement>(null);
  const placed = useRef(false);
  useLayoutEffect(() => {
    const box = switchRef.current;
    const pill = pillRef.current;
    const option = box?.querySelector<HTMLElement>('[aria-checked="true"]');
    if (!box || !pill || !option) return;
    const place = () => {
      if (!placed.current) pill.style.transition = "none";
      pill.style.width = `${option.offsetWidth}px`;
      pill.style.height = `${option.offsetHeight}px`;
      pill.style.transform = `translate(${option.offsetLeft}px, ${option.offsetTop}px)`;
      pill.style.opacity = "1";
      if (!placed.current) {
        void pill.offsetWidth;
        pill.style.transition = "";
        placed.current = true;
      }
    };
    place();
    const ro = new ResizeObserver(place);
    ro.observe(box);
    return () => ro.disconnect();
  }, [activePeriod, periods?.length]);

  const choose = (id: string) => {
    if (period === undefined) setInnerPeriod(id);
    onPeriodChange?.(id);
  };
  const onSwitchKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!periods?.length) return;
    const i = periods.findIndex((p) => p.id === activePeriod);
    const to = e.key === "ArrowRight" || e.key === "ArrowDown" ? i + 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? periods.length - 1 : null;
    if (to === null) return;
    e.preventDefault();
    const next = periods[(to + periods.length) % periods.length];
    choose(next.id);
    requestAnimationFrame(() => switchRef.current?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus());
  };

  const activeRow = activeValid ? rows.find((r) => r.key === activeValid) : undefined;
  const share = (key: string) => ((shown[key] ?? 0) / shownTotal) * 100;
  const hasDelta = delta !== undefined && Number.isFinite(delta);
  const up = (delta ?? 0) >= 0;
  // Steve: the change in the page's language, not a fixed en-US
  const deltaText = localNum(Math.abs(delta ?? 0), { maximumFractionDigits: 1 });
  // Steve: the list is one tab stop – arrows move between the parts (roving tabindex), like the waterfall's tree
  const live = rows.filter((r) => !r.leaving);
  const stop = activeValid ?? live[0]?.key;
  const onItemKey = (e: KeyboardEvent<HTMLDivElement>, key: string) => {
    const i = live.findIndex((r) => r.key === key);
    const to = e.key === "ArrowDown" || e.key === "ArrowRight" ? i + 1 : e.key === "ArrowUp" || e.key === "ArrowLeft" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? live.length - 1 : null;
    if (to === null || i < 0) return;
    e.preventDefault();
    const next = live[Math.max(0, Math.min(live.length - 1, to))];
    rowEls.current.get(next.key)?.querySelector<HTMLElement>(".ui-cb__item")?.focus();
  };

  return (
    <div className={["ui-cb", className].filter(Boolean).join(" ")} role="group" aria-labelledby={`${uid}-title`} data-bare={bare ? "" : undefined}>
      {/* React 19 hoists this into <head> once, no matter how many cards render */}
      <style href="ui-compact-breakdown" precedence="default">
        {css}
      </style>

      <div className="ui-cb__card">
        <header className="ui-cb__head">
          <h3 id={`${uid}-title`} className="ui-cb__title" title={title} dir="auto">
            {title}
          </h3>
          {periods && periods.length > 1 && (
            <div ref={switchRef} className="ui-cb__switch" role="radiogroup" aria-label={t("stats:breakdown.period")} onKeyDown={onSwitchKey}>
              <span ref={pillRef} className="ui-cb__pill" aria-hidden="true" />
              {periods.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  role="radio"
                  aria-checked={p.id === activePeriod}
                  tabIndex={p.id === activePeriod ? 0 : -1}
                  className="ui-cb__option"
                  title={p.label}
                  onClick={() => choose(p.id)}
                >
                  {p.label}
                </button>
              ))}
            </div>
          )}
        </header>

        {/* The big number: the total – or the hovered part, rolled to */}
        <div className="ui-cb__figure">
          <p className="ui-cb__big" aria-hidden="true">
            {format(shown["#figure"] ?? 0)}
          </p>
          {hasDelta && !activeRow && (
            <span key={`${activePeriod}-${delta}`} className="ui-cb__delta" data-up={up || undefined} data-tone={deltaTone}>
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path d={up ? "M6 9.5v-7M2.8 5.6 6 2.4l3.2 3.2" : "M6 2.5v7M2.8 6.4 6 9.6l3.2-3.2"} />
              </svg>
              {deltaText}%
            </span>
          )}
        </div>
        <p className="ui-cb__caption" aria-hidden="true">
          <span key={activeRow?.key ?? "#total"} className="ui-cb__caption-in">
            {activeRow ? (
              <>
                <i className="ui-cb__swatch" style={{ background: activeRow.color }} />
                <span className="ui-cb__caption-label" dir="auto">{activeRow.label}</span>
                <span className="ui-cb__caption-share">· {pct(share(activeRow.key))}</span>
              </>
            ) : (
              totalLabel
            )}
          </span>
        </p>
        <p className="ui-cb__sr">
          {totalLabel}: {format(total)}
          {hasDelta ? t(up ? "stats:breakdown.upSr" : "stats:breakdown.downSr", { pct: deltaText }) : ""}
        </p>

        <div className="ui-cb__bar" data-active={activeValid ? "" : undefined} aria-hidden="true" onPointerLeave={() => setActive(null)}>
          {known.current.map((k) => {
            const w = share(k.key);
            return w > 0.05 ? (
              <span
                key={k.key}
                className="ui-cb__seg"
                data-active={k.key === activeValid || undefined}
                style={{ flexGrow: w, background: k.color }}
                onPointerEnter={() => now.has(k.key) && setActive(k.key)}
              />
            ) : null;
          })}
        </div>

        <ul className="ui-cb__list" data-active={activeValid ? "" : undefined} onPointerLeave={() => setActive(null)}>
          {rows.map((r) => (
            <li
              key={r.key}
              ref={(el) => {
                if (el) rowEls.current.set(r.key, el);
                else rowEls.current.delete(r.key);
              }}
              className="ui-cb__row"
              data-leaving={r.leaving || undefined}
              {...rowProps?.({ key: r.key, label: r.label })}
            >
              <div
                className="ui-cb__item"
                tabIndex={!r.leaving && r.key === stop ? 0 : -1}
                data-active={r.key === activeValid || undefined}
                onPointerEnter={() => !r.leaving && setActive(r.key)}
                onFocus={() => !r.leaving && setActive(r.key)}
                onBlur={() => setActive(null)}
                onKeyDown={(e) => onItemKey(e, r.key)}
              >
                <i className="ui-cb__swatch" style={{ background: r.color }} />
                <span className="ui-cb__label" title={r.label} dir="auto">
                  {r.label}
                </span>
                <span className="ui-cb__value">{format(shown[r.key] ?? 0)}</span>
                <span className="ui-cb__share">{pct(share(r.key))}</span>
              </div>
            </li>
          ))}
        </ul>
      </div>

      {footer !== null && (
        <div className="ui-cb__foot">
          {footer ??
            (top && (
              <span key={top.key} className="ui-cb__foot-in">
                {t("stats:breakdown.largest")} <b title={top.label} dir="auto">{top.label}</b>
              </span>
            ))}
        </div>
      )}
    </div>
  );
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-cb */
.ui-cb {
  /* Steve: every color from the --aui-* map (affectui-theme.css) and the stats page's series, one block for both modes */
  --ui-cb-tray: var(--aui-subtle);
  --ui-cb-card: var(--aui-surface);
  --ui-cb-line: var(--aui-border);
  --ui-cb-ink: var(--aui-ink);
  --ui-cb-text: var(--aui-ink);
  --ui-cb-muted: var(--aui-muted);
  --ui-cb-well: var(--aui-subtle);
  --ui-cb-hover: var(--aui-subtle);
  --ui-cb-up: var(--aui-success);
  --ui-cb-up-bg: var(--aui-success-soft);
  --ui-cb-down: var(--aui-danger);
  --ui-cb-down-bg: var(--aui-danger-soft);
  --ui-cb-c1: var(--series-1);
  --ui-cb-c2: var(--series-2);
  --ui-cb-c3: var(--series-3);
  --ui-cb-c4: var(--series-4);
  --ui-cb-c5: var(--series-5);
  --ui-cb-focus: var(--aui-focus);
  --ui-cb-ease: var(--aui-ease);
  box-sizing: border-box;
  container-type: inline-size;
  display: grid;
  /* The card never grows past its container, whatever the title or switch hold */
  grid-template-columns: minmax(0, 1fr);
  width: 100%;
  max-width: 360px;
  padding: 4px;
  border-radius: calc(var(--aui-radius-lg, 16px) + 4px);
  background: var(--ui-cb-tray);
  font-family: var(--aui-font, "Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 1.45;
  color: var(--ui-cb-text);
  -webkit-font-smoothing: antialiased;
}
.ui-cb *, .ui-cb *::before, .ui-cb *::after { box-sizing: border-box; }
:where(.ui-cb) button { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-cb) :is(h3, p, ul) { margin: 0; padding: 0; }
:where(.ui-cb) ul { list-style: none; }
.ui-cb :is(button, [tabindex="0"]):focus-visible { outline: 2px solid var(--ui-cb-focus); outline-offset: 2px; }
.ui-cb b { font-weight: 500; color: var(--ui-cb-ink); }
.ui-cb__sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }

.ui-cb__card { border-radius: var(--aui-radius-lg, 16px); background: var(--ui-cb-card); box-shadow: var(--aui-shadow-card, 0 0 0 1px var(--ui-cb-line), 0 1px 2px rgba(0, 0, 0, 0.04)); padding: 12px 14px 8px; }

/* Head: title, the period switch on the right */
.ui-cb__head { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-height: 30px; }
.ui-cb__title { overflow: hidden; font-size: 13.5px; font-weight: 600; line-height: 20px; color: var(--ui-cb-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-cb__switch { position: relative; display: grid; grid-auto-columns: minmax(0, auto); grid-auto-flow: column; flex: 0 1 auto; min-width: 0; padding: 2px; border-radius: var(--aui-radius, 10px); background: var(--ui-cb-well); box-shadow: inset 0 0 0 1px var(--ui-cb-line); }
.ui-cb__pill {
  position: absolute;
  top: 0;
  left: 0;
  border-radius: calc(var(--aui-radius, 10px) - 2px);
  background: var(--ui-cb-card);
  box-shadow: 0 0 0 1px var(--ui-cb-line), 0 1px 2px rgba(0, 0, 0, 0.06);
  opacity: 0;
  pointer-events: none;
  transition: transform 420ms var(--ui-cb-ease), width 420ms var(--ui-cb-ease);
}
.ui-cb__option { position: relative; z-index: 1; height: 24px; padding: 0 8px; border-radius: calc(var(--aui-radius, 10px) - 2px); font-size: 12px; font-weight: 500; color: var(--ui-cb-muted); white-space: nowrap; transition: color 200ms ease; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-cb__option:hover, .ui-cb__option[aria-checked="true"] { color: var(--ui-cb-ink); }

/* The big number and the change against the period before */
.ui-cb__figure { display: flex; align-items: center; gap: 8px; margin-top: 12px; }
.ui-cb__big { font-size: 26px; font-weight: 600; line-height: 32px; letter-spacing: -0.02em; color: var(--ui-cb-ink); font-variant-numeric: tabular-nums; }
.ui-cb__delta {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  height: 20px;
  padding: 0 6px 0 4px;
  border-radius: 999px;
  background: var(--ui-cb-down-bg);
  color: var(--ui-cb-down);
  font-size: 12px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
  animation: ui-cb-pop 380ms var(--ui-cb-ease) both;
}
.ui-cb__delta[data-up] { background: var(--ui-cb-up-bg); color: var(--ui-cb-up); }
/* Steve: a change that is neither good nor bad (spend) – muted, like the page's own "vs. last period" text */
.ui-cb__delta[data-tone="neutral"] { background: var(--ui-cb-well); color: var(--ui-cb-muted); }
.ui-cb__delta svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
.ui-cb__caption { height: 18px; overflow: hidden; font-size: 12.5px; line-height: 18px; color: var(--ui-cb-muted); font-variant-numeric: tabular-nums; }
.ui-cb__caption-label { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-cb__caption-share { flex: none; }
.ui-cb__caption-in { display: inline-flex; max-width: 100%; align-items: center; gap: 6px; animation: ui-cb-in 260ms var(--ui-cb-ease) both; }

/* The stacked bar – parts keep their place and color, only the widths glide */
.ui-cb__bar { display: flex; gap: 2px; height: 8px; margin-top: 12px; padding: 0; border-radius: 4px; }
/* Shares as flex-grow, so the 2px gaps come out of the bar instead of making it wider than the card */
.ui-cb__seg { flex: 0 1 0; height: 100%; min-width: 2px; border-radius: 2.5px; transition: opacity 200ms ease, transform 260ms var(--ui-cb-ease); }
.ui-cb__seg:first-child { border-radius: 4px 2.5px 2.5px 4px; }
.ui-cb__seg:last-child { border-radius: 2.5px 4px 4px 2.5px; }
.ui-cb__bar[data-active] .ui-cb__seg:not([data-active]) { opacity: 0.25; }
.ui-cb__seg[data-active] { transform: scaleY(1.5); }

/* The ranked list */
.ui-cb__list { position: relative; display: grid; margin-top: 8px; }
.ui-cb__row { display: grid; grid-template-rows: 1fr; transition: grid-template-rows 320ms var(--ui-cb-ease), opacity 200ms ease; }
.ui-cb__row[data-leaving] { grid-template-rows: 0fr; opacity: 0; pointer-events: none; }
.ui-cb__item { display: flex; align-items: center; gap: 9px; min-height: 0; height: 30px; margin: 0 -8px; padding: 0 8px; overflow: hidden; border-radius: calc(var(--aui-radius-sm, 7px) + 1px); cursor: default; transition: background-color 160ms ease, opacity 200ms ease; }
.ui-cb__item[data-active] { background: var(--ui-cb-hover); }
.ui-cb__list[data-active] .ui-cb__item:not([data-active]) { opacity: 0.45; }
.ui-cb__swatch { flex: none; width: 8px; height: 8px; border-radius: 2.5px; }
.ui-cb__label { flex: 1; min-width: 0; overflow: hidden; color: var(--ui-cb-text); white-space: nowrap; text-overflow: ellipsis; }
.ui-cb__value { flex: none; white-space: nowrap; color: var(--ui-cb-ink); font-weight: 500; font-variant-numeric: tabular-nums; }
.ui-cb__share { flex: none; width: 34px; white-space: nowrap; text-align: right; color: var(--ui-cb-muted); font-variant-numeric: tabular-nums; }

/* The line on the gray tray */
.ui-cb__foot { display: flex; align-items: center; min-height: 34px; padding: 4px 10px 0; overflow: hidden; font-size: 12.5px; color: var(--ui-cb-muted); white-space: nowrap; }
.ui-cb__foot-in { overflow: hidden; text-overflow: ellipsis; animation: ui-cb-in 320ms var(--ui-cb-ease) both; }

@keyframes ui-cb-in {
  from { opacity: 0; transform: translateY(5px); filter: blur(2px); }
  to { opacity: 1; transform: none; filter: none; }
}
@keyframes ui-cb-pop {
  from { opacity: 0; transform: scale(0.85); }
  to { opacity: 1; transform: none; }
}

/* Narrow cards: the share column goes, the values stay */
@container (max-width: 250px) {
  .ui-cb__share { display: none; }
  .ui-cb__option { padding: 0 6px; }
}
@media (hover: none) and (pointer: coarse) {
  .ui-cb__option { height: 32px; padding: 0 10px; }
  .ui-cb__item { height: 36px; }
}

@media (prefers-reduced-motion: reduce) {
  .ui-cb *, .ui-cb *::before, .ui-cb *::after { animation: none !important; transition: none !important; }
}

/* Steve: inside a panel – the panel is the card */
.ui-cb[data-bare] { max-width: none; padding: 0; border-radius: 0; background: none; }
.ui-cb[data-bare] .ui-cb__card { padding: 0; border-radius: 0; background: none; box-shadow: none; }
`;
