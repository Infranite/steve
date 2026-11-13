/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useId, useState, type ReactNode } from "react";

/*
 * Integration Settings (clean style) – the settings card of one connected service, in a single self-contained file.
 * One card: the header with the switch on top, the settings in hairline-separated sections below,
 * and the save bar as the card's last row. The styles are injected below, so no extra files are needed.
 *
 * Steve: the card is a connection (GET /connections/:id). The switch is whether it receives (paused or not), the
 * sections are what that connection really has – its state, its capabilities, a mailbox's settings – and the save
 * bar (IntegrationSaveBar) sits under the fields whose edits it holds. Keys, addresses and time zones of the original are not Steve's data,
 * so the sections come in as children. All copy comes in through props, so it follows Steve's i18n.
 */

export type IntegrationSettingsProps = {
  name: string;
  logo: ReactNode;
  /** Small number next to the name */
  count?: number;
  /** Line under the name while it is on */
  meta?: ReactNode;
  /** Line under the name while it is off */
  pausedMeta?: ReactNode;
  /** Right of the name: a status, a badge */
  badge?: ReactNode;
  /** Right of the header, before the switch: a link, an action */
  aside?: ReactNode;
  /** The switch: absent → no switch */
  enabled?: boolean;
  switchLabel?: string;
  switchDisabled?: boolean;
  onToggle?: (enabled: boolean) => void;
  /** Sections, folded away while switched off */
  children?: ReactNode;
  /** Sections that stay while switched off (e.g. disconnect) */
  footer?: ReactNode;
  className?: string;
} & Record<`data-${string}`, string | number | boolean | undefined>;

/** First whole letter or digit of the name – the tile when there is no logo or it fails to load */
const initial = (name: string) => name.match(/[\p{L}\p{N}]/u)?.[0]?.toUpperCase() ?? "";

export function IntegrationSettings({
  name,
  logo,
  count,
  meta,
  pausedMeta,
  badge,
  aside,
  enabled,
  switchLabel,
  switchDisabled,
  onToggle,
  children,
  footer,
  className,
  ...rest
}: IntegrationSettingsProps) {
  const [logoFailed, setLogoFailed] = useState(false);
  const on = enabled !== false;

  return (
    <>
      <style href="ui-integration-settings-clean" precedence="default">
        {css}
      </style>
      <section className={["ui-isc", className].filter(Boolean).join(" ")} aria-label={name} data-off={!on || undefined} {...rest}>
        <header className="ui-isc__head">
          <span className="ui-isc__logo" onErrorCapture={() => setLogoFailed(true)}>
            {logo == null || logo === false || logoFailed ? <span className="ui-isc__initial">{initial(name)}</span> : logo}
          </span>
          <span className="ui-isc__who">
            <span className="ui-isc__name">
              <span className="ui-isc__name-text" dir="auto" title={name}>
                {name}
              </span>
              {count !== undefined && Number.isFinite(count) && <span className="ui-isc__count">{count.toLocaleString()}</span>}
              {badge}
            </span>
            <span className="ui-isc__meta" dir="auto">
              {on ? meta : pausedMeta ?? meta}
            </span>
          </span>
          {aside}
          {enabled !== undefined && (
            <button
              type="button"
              role="switch"
              aria-checked={on}
              aria-label={switchLabel ?? name}
              className="ui-isc__switch"
              disabled={switchDisabled}
              onClick={() => onToggle?.(!on)}
              data-isc-switch
            >
              <span />
            </button>
          )}
        </header>

        <div className="ui-isc__fold" data-open={on || undefined} inert={!on}>
          <div className="ui-isc__fold-inner">
            {children}
          </div>
        </div>
        {footer && <div className="ui-isc__footer">{footer}</div>}
      </section>
    </>
  );
}

/** The save bar: a row of the card under the settings it saves, sliding open while `dirty`. Steve: it sits right under
 * the fields whose edits it holds, so a switch elsewhere in the card (which saves at once) never looks unsaved. */
export function IntegrationSaveBar({ dirty, saving = false, canSave = true, onSave, onDiscard, labels }: { dirty: boolean; saving?: boolean; canSave?: boolean; onSave: () => void; onDiscard: () => void; labels: { unsaved: string; discard: string; save: string; saving: string } }) {
  return (
    <div className="ui-isc__save" data-open={dirty || undefined} inert={!dirty} data-isc-save-bar>
      <div className="ui-isc__save-inner">
        <span className="ui-isc__save-text">{labels.unsaved}</span>
        <div className="ui-isc__save-actions">
          <button type="button" className="ui-isc__btn" onClick={onDiscard} data-isc-discard>
            {labels.discard}
          </button>
          <button type="button" className="ui-isc__btn ui-isc__btn--primary" onClick={onSave} disabled={!canSave || saving} data-isc-save>
            {saving ? labels.saving : labels.save}
          </button>
        </div>
      </div>
    </div>
  );
}

/** One section of the card: an optional label, then its content */
export function IntegrationSection({ label, hint, children, ...rest }: { label?: ReactNode; hint?: ReactNode; children?: ReactNode } & Record<`data-${string}`, string | number | boolean | undefined>) {
  return (
    <div className="ui-isc__section" {...rest}>
      {label && <span className="ui-isc__label">{label}</span>}
      {children}
      {hint && <p className="ui-isc__hint">{hint}</p>}
    </div>
  );
}

/** A setting on one line: what it is (and a hint) on the left, its control on the right */
export function IntegrationRow({ title, hint, children, ...rest }: { title: ReactNode; hint?: ReactNode; children?: ReactNode } & Record<`data-${string}`, string | number | boolean | undefined>) {
  return (
    <div className="ui-isc__row" {...rest}>
      <span className="ui-isc__row-text">
        <span className="ui-isc__row-title">{title}</span>
        {hint && <span className="ui-isc__row-hint">{hint}</span>}
      </span>
      {children != null && <span className="ui-isc__row-control">{children}</span>}
    </div>
  );
}

/** A labelled multi-line text field in a section */
export function IntegrationTextarea({ label, hint, value, onChange, maxLength, rows = 3, ...rest }: { label: ReactNode; hint?: ReactNode; value: string; onChange: (v: string) => void; maxLength?: number; rows?: number } & Record<`data-${string}`, string | number | boolean | undefined>) {
  const id = useId();
  const hintId = useId();
  return (
    <div className="ui-isc__section">
      <label className="ui-isc__label" htmlFor={id}>
        {label}
      </label>
      {hint && <p className="ui-isc__hint ui-isc__hint--top" id={hintId}>{hint}</p>}
      <div className="ui-isc__field ui-isc__field--area">
        <textarea id={id} value={value} maxLength={maxLength} rows={rows} onChange={(e) => onChange(e.target.value)} aria-describedby={hint ? hintId : undefined} {...rest} />
      </div>
    </div>
  );
}
const css = /* css */ `
/* Colors and motion as variables – override them on .ui-isc */
.ui-isc {
  --ui-isc-card: var(--aui-surface, #ffffff);
  --ui-isc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-isc-strong: var(--aui-ink, #000000);
  --ui-isc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-isc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-isc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-isc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-isc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-isc-accent: var(--aui-primary, rgba(0, 96, 226, 0.765));
  --ui-isc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-isc-solid-hover: var(--aui-primary-hover, color-mix(in srgb, var(--ui-isc-solid) 88%, var(--ui-isc-ink)));
  --ui-isc-on-solid: var(--aui-on-primary, #fcfcfc);
  --ui-isc-switch-off: color-mix(in srgb, var(--aui-border-strong, rgba(0, 0, 0, 0.12)) 90%, transparent);
  /* Strokes are shadows: a 0.5px hairline plus a soft lift */
  --ui-isc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-isc-shadow-hover: 0 0 0 0.5px var(--aui-border-strong, rgba(0, 0, 0, 0.12)), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05);
  --ui-isc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-isc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  box-sizing: border-box;
  container-type: inline-size;
  width: 100%;
  /* One white card – the edge is a shadow, not a border */
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--aui-surface, var(--ui-isc-card));
  box-shadow: var(--aui-shadow-card, var(--ui-isc-shadow));
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-isc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-isc *, .ui-isc *::before, .ui-isc *::after { box-sizing: border-box; }
/* Steve: only the card's own buttons are reset – host buttons inside the sections keep their styles */
:where(.ui-isc) button:where([class*="ui-isc__"]) { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; cursor: pointer; }
:where(.ui-isc) svg { display: block; flex: none; }
.ui-isc button:focus-visible { outline: 2px solid var(--ui-isc-focus); outline-offset: 2px; }

/* Header: logo tile, name with count, meta line and the switch */
.ui-isc__head { display: flex; align-items: center; gap: 12px; padding: 16px; }
.ui-isc__logo {
  display: grid;
  place-items: center;
  flex: none;
  width: 40px;
  height: 40px;
  border-radius: 8px;
  background: var(--aui-subtle, var(--ui-isc-card));
  box-shadow: var(--ui-isc-shadow);
}
.ui-isc__logo { overflow: hidden; }
.ui-isc__logo svg { width: 22px; height: 22px; }
/* Image logos of any size fit the tile; without one, the first letter of the name */
.ui-isc__logo img { display: block; width: 22px; height: 22px; object-fit: contain; }
.ui-isc__initial { font-size: 15px; font-weight: 600; color: var(--ui-isc-text); }
.ui-isc__who { display: grid; flex: 1; min-width: 0; }
.ui-isc__name { display: flex; align-items: center; gap: 8px; min-width: 0; font-size: 15px; line-height: 24px; font-weight: 500; color: var(--ui-isc-strong); }
.ui-isc__name-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-isc__count {
  flex: none;
  padding: 0 5px;
  border-radius: var(--aui-radius-sm, 5px);
  background: var(--aui-subtle);
  font-size: 11px;
  line-height: 16px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
  color: var(--ui-isc-text);
}
.ui-isc__meta { overflow: hidden; font-size: 12px; line-height: 16px; white-space: nowrap; text-overflow: ellipsis; color: var(--ui-isc-muted); }

/* The switch, at the size of Steve's own switches (32×20, a 16px knob) */
.ui-isc__switch {
  position: relative;
  flex: none;
  width: 32px;
  height: 20px;
  border-radius: 10px;
  background: var(--ui-isc-switch-off) !important;
  transition: background-color 180ms ease;
}
.ui-isc__switch[aria-checked="true"] { background: var(--ui-isc-accent) !important; }
.ui-isc__switch:disabled { opacity: 0.5; cursor: default; }
.ui-isc__switch span {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: var(--aui-surface, #fff);
  box-shadow: var(--ui-isc-shadow);
  transition: transform 180ms var(--ui-isc-ease);
}
.ui-isc__switch[aria-checked="true"] span { transform: translateX(12px); }
/* A bigger invisible hit area around the small switch */
.ui-isc__switch::after { content: ""; position: absolute; inset: -8px -6px; }

/* Switching off folds the settings away – only this part animates, the header stays */
.ui-isc__fold { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-rows: 0fr; opacity: 0; transition: grid-template-rows 420ms var(--ui-isc-ease), opacity 260ms ease; }
.ui-isc__fold[data-open] { grid-template-rows: 1fr; opacity: 1; }
.ui-isc__fold-inner { min-width: 0; min-height: 0; overflow: hidden; }
.ui-isc__fold[data-open] .ui-isc__fold-inner { overflow: visible; }

/* Sections sit right in the card, split by hairlines */
.ui-isc__section { padding: 14px 16px 16px; border-top: 0.5px solid var(--ui-isc-line); }
.ui-isc__label { display: block; margin: 0 0 6px; font-size: 13px; line-height: 20px; font-weight: 500; color: var(--ui-isc-ink); }

/* Fields: white with the hairline shadow */
.ui-isc__field {
  display: flex;
  align-items: center;
  gap: 2px;
  width: 100%;
  height: 32px;
  padding: 0 8px 0 10px;
  border-radius: calc(var(--aui-radius, 7px) + 1px);
  background: var(--aui-surface, var(--ui-isc-card));
  box-shadow: var(--aui-shadow-card, var(--ui-isc-shadow));
  transition: box-shadow 160ms ease, background-color 160ms ease;
}
.ui-isc__field:hover { box-shadow: var(--ui-isc-shadow-hover); }
/* Steve: typing in a field shows the same focus ring as Steve's own text areas */
.ui-isc__field:focus-within { outline: none; box-shadow: 0 0 0 0.5px var(--aui-border-strong, rgba(0, 0, 0, 0.12)), 0 0 0 3px color-mix(in srgb, var(--ui-isc-focus) 35%, transparent); }
.ui-isc__hint { overflow-wrap: anywhere; margin: 6px 0 0; font-size: 12px; line-height: 16px; color: var(--ui-isc-muted); }

/* Save bar: the card's last row under a hairline, slides open when something changed */
.ui-isc__save { display: grid; grid-template-columns: minmax(0, 1fr); grid-template-rows: 0fr; transition: grid-template-rows 360ms var(--ui-isc-ease); }
.ui-isc__save[data-open] { grid-template-rows: 1fr; }
.ui-isc__save-inner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  min-height: 0;
  overflow: hidden;
  padding: 0 12px 0 16px;
  border-top: 0.5px solid var(--ui-isc-line);
  color: var(--ui-isc-text);
}
.ui-isc__save[data-open] .ui-isc__save-inner { padding-top: 10px; padding-bottom: 10px; }
.ui-isc__save-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-isc__save-actions { display: flex; flex: none; gap: 6px; }
.ui-isc__btn {
  height: 28px;
  padding: 0 10px !important;
  border-radius: calc(var(--aui-radius, 7px) - 1px);
  background: var(--aui-surface, var(--ui-isc-card)) !important;
  box-shadow: var(--ui-isc-shadow);
  font-size: 13px;
  font-weight: 500;
  color: var(--ui-isc-strong) !important;
  transition: box-shadow 140ms ease, background-color 140ms ease;
}
.ui-isc__btn:hover:not(:disabled) { box-shadow: var(--ui-isc-shadow-hover); }
.ui-isc__btn--primary { min-width: 56px; background: var(--ui-isc-solid) !important; box-shadow: none; color: var(--ui-isc-on-solid) !important; }
.ui-isc__btn--primary:hover:not(:disabled) { background: var(--ui-isc-solid-hover) !important; box-shadow: none; }
.ui-isc__btn--primary:disabled { opacity: 0.4; cursor: default; }


/* Narrow (phones): tighter padding */
@container (max-width: 400px) {
  .ui-isc__head { gap: 10px; padding: 14px 14px 14px 12px; }
  .ui-isc__logo { width: 36px; height: 36px; }
  .ui-isc__logo svg { width: 20px; height: 20px; }
  .ui-isc__section { padding: 12px 12px 14px; }
  .ui-isc__save-inner { padding: 0 10px 0 12px; }
}
/* Touch screens: bigger targets; 16px text in fields so iOS doesn't zoom in */
@media (hover: none) and (pointer: coarse) {
  .ui-isc__btn { height: 36px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-isc *, .ui-isc *::before, .ui-isc *::after { animation: none !important; transition: none !important; }
}

/* Text fields: the field's ring above is the focus cue – no second box from global :focus-visible rules of the host page */
.ui-isc textarea:focus-visible { outline: none; }

/* Steve: setting rows, text areas and a footer that stays while switched off */
.ui-isc__row { display: flex; align-items: center; justify-content: space-between; gap: 12px; min-height: 36px; padding: 6px 0; }
.ui-isc__row + .ui-isc__row { border-top: 0.5px solid var(--ui-isc-line); }
.ui-isc__row-text { display: grid; min-width: 0; }
.ui-isc__row-title { font-size: 13px; line-height: 20px; color: var(--ui-isc-ink); overflow-wrap: anywhere; }
.ui-isc__row-hint { font-size: 12px; line-height: 18px; color: var(--ui-isc-muted); overflow-wrap: anywhere; }
.ui-isc__row-control { display: flex; flex: none; align-items: center; gap: 6px; }
.ui-isc__hint--top { margin: -4px 0 8px; }
.ui-isc__field--area { height: auto; padding: 8px 10px; }
.ui-isc__field textarea { flex: 1; min-width: 0; margin: 0; padding: 0; border: 0; outline: none; background: transparent; resize: vertical; font: inherit; color: var(--ui-isc-ink); }
.ui-isc__field textarea::placeholder { color: var(--ui-isc-faint); }
.ui-isc__footer { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; padding: 8px 10px; border-top: 0.5px solid var(--ui-isc-line); }
.ui-isc[data-off] .ui-isc__footer { border-top: 0; }
@media (hover: none) and (pointer: coarse) { .ui-isc__field textarea { font-size: 16px; } }
/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-isc {
  --ui-isc-card: var(--aui-surface, #1c1c1e);
  --ui-isc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-isc-strong: var(--aui-ink, #ffffff);
  --ui-isc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-isc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-isc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-isc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-isc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-isc-accent: var(--aui-primary, #4fb0fe);
  --ui-isc-solid: var(--aui-primary, rgba(255, 255, 255, 0.9));
  --ui-isc-on-solid: var(--aui-on-primary, #161618);
  --ui-isc-switch-off: color-mix(in srgb, var(--aui-border-strong, rgba(255, 255, 255, 0.16)) 90%, transparent);
  --ui-isc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-isc-shadow-hover: 0 0 0 0.5px var(--aui-border-strong, rgba(255, 255, 255, 0.16)), 0 1px 2px 0 rgba(0, 0, 0, 0.4);
  --ui-isc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
}
:where(.dark, [data-theme="dark"]) .ui-isc__switch span { background: var(--aui-on-primary, #f4f4f5); box-shadow: 0 1px 2px rgba(0, 0, 0, 0.4); }
`;
