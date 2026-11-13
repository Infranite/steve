/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import {
  useId,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type HTMLAttributes,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
  type RefObject,
} from "react";

/*
 * Chat Composer (Clean) – a single self-contained file.
 * One card with a hairline shadow: anything waiting above the message (attachments, the queue), the message itself,
 * and the tools in a row below it. The plus opens a menu of things to add, right above the composer.
 *
 * Steve: the composer's behaviour (the marked text, the #/@// menus, drafts, uploads, sending) stays Steve's own; this
 * file draws the card around it. The demo's voice input, approval modes, folder picker, app connections and model
 * picker are left out: Steve has no speech-to-text, its autonomy is the Hub-wide YOLO setting, a session's harness and
 * model are fixed when it starts (shown as a quiet label), and integrations live in their own settings.
 */

export type ComposerAddRow = { id: string; label: string; description?: string; icon?: ReactNode };

export type ChatComposerProps = Omit<HTMLAttributes<HTMLDivElement>, "children"> & {
  /** The message field: a textarea with the class `ui-ccc__message`, and whatever is laid over it */
  children: ReactNode;
  /** Inside the card above the message: the queue, the files picked so far */
  head?: ReactNode;
  /** The plus menu, in sections; without it the plus is left out */
  add?: { title?: string; rows: ComposerAddRow[] }[];
  addLabel?: string;
  onAdd?: (id: string) => void;
  /** After the plus: the page's own toggles */
  tools?: ReactNode;
  /** A quiet label for where the message goes (harness and model) – not a picker */
  model?: string;
  /** The right end: hints, the context ring and the send / stop / queue / steer buttons */
  end?: ReactNode;
  /** `data-composer` of the writing area (the message and its tools) */
  testId?: string;
  /** A file is dragged over the composer */
  drop?: boolean;
  disabled?: boolean;
  /** Called when the card is clicked outside the field and its buttons */
  onFocusField?: () => void;
};

export function ChatComposer({ children, head, add, addLabel = "Add", onAdd, tools, model, end, testId, drop, disabled, onFocusField, className, ...rest }: ChatComposerProps) {
  const [menu, setMenu] = useState(false);
  const body = useRef<HTMLElement>(null);
  const addButton = useRef<HTMLButtonElement>(null);

  // Clicking anywhere on the writing area puts the caret in the text area
  const focusMessage = (event: PointerEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest("button, textarea, input, a, [role=tablist], [role=radiogroup]")) return;
    event.preventDefault();
    onFocusField?.();
  };

  return (
    <div className={["ui-ccc", className].filter(Boolean).join(" ")} data-tray="off" data-drop={drop || undefined} data-disabled={disabled || undefined} {...rest}>
      {/* React 19 hoists this into <head> once, no matter how many composers render */}
      <style href="ui-chat-composer-clean" precedence="default">
        {css}
      </style>

      <section ref={body} className="ui-ccc__body">
        {head && <div className="ui-ccc__head">{head}</div>}
        <div className="ui-ccc__write" onPointerDown={focusMessage} data-composer={testId}>
          <div className="ui-ccc__field">{children}</div>

          <div className="ui-ccc__tools">
            <div className="ui-ccc__group">
              {add && (
                <button
                  ref={addButton}
                  type="button"
                  className="ui-ccc__icon-btn"
                  aria-label={addLabel}
                  title={addLabel}
                  aria-haspopup="menu"
                  aria-expanded={menu}
                  disabled={disabled}
                  onClick={() => setMenu((v) => !v)}
                  onKeyDown={(e: KeyboardEvent<HTMLButtonElement>) => {
                    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
                    e.preventDefault();
                    setMenu(true);
                  }}
                  data-composer-add
                >
                  <PlusIcon />
                </button>
              )}
              {tools}
            </div>
            <div className="ui-ccc__group ui-ccc__group--end">
              {model && (
                <span className="ui-ccc__model" title={model} data-model>
                  <span className="ui-ccc__model-name" dir="auto">{model}</span>
                </span>
              )}
              {end}
            </div>
          </div>
        </div>

        {add && (
          <PopMenu
            open={menu}
            role="menu"
            label={addLabel}
            inline
            wide
            sections={add.map((s) => ({ title: s.title, rows: s.rows }))}
            anchor={addButton}
            container={body}
            onPick={(id) => {
              setMenu(false);
              onAdd?.(id);
            }}
            onClose={(refocus) => {
              setMenu(false);
              if (refocus) addButton.current?.focus();
            }}
          />
        )}
      </section>
    </div>
  );
}

/** The rectangle the menu has to stay in: the nearest ancestor that clips, otherwise the viewport */
function clippingFrame(from: HTMLElement) {
  for (let el = from.parentElement; el && el !== document.body; el = el.parentElement) {
    const { overflowX, overflowY } = getComputedStyle(el);
    if (overflowX !== "visible" || overflowY !== "visible") {
      // …and never past the edge of the window
      const r = el.getBoundingClientRect();
      return { top: Math.max(r.top, 0), left: Math.max(r.left, 0), right: Math.min(r.right, window.innerWidth), bottom: Math.min(r.bottom, window.innerHeight) };
    }
  }
  return { top: 0, left: 0, right: window.innerWidth, bottom: window.innerHeight };
}

/* ---------- Menus: open upward, or downward when there is no room above ---------- */

type MenuRow = { id: string; label: string; description?: string; icon?: ReactNode; tone?: "alert"; checked?: boolean };

function PopMenu({
  open,
  role = "listbox",
  label,
  head,
  sections,
  anchor,
  container,
  align = "end",
  width = 260,
  wide = false,
  inline = false,
  onPick,
  onClose,
}: {
  open: boolean;
  /** A listbox picks one row (with a check); a menu runs actions */
  role?: "listbox" | "menu";
  label: string;
  /** A line on top, e.g. a question and a link */
  head?: ReactNode;
  sections: { title?: string; rows: MenuRow[] }[];
  anchor: RefObject<HTMLButtonElement | null>;
  container: RefObject<HTMLElement | null>;
  /** Which edge of the button the menu lines up with */
  align?: "start" | "end";
  width?: number;
  /** As wide as the composer, above it */
  wide?: boolean;
  /** Descriptions next to the names instead of under them */
  inline?: boolean;
  onPick: (id: string) => void;
  onClose: (refocus: boolean) => void;
}) {
  const id = useId();
  const panel = useRef<HTMLDivElement>(null);
  const rows = useRef<(HTMLDivElement | null)[]>([]);
  const flat = sections.flatMap((section) => section.rows);
  const [active, setActive] = useState(0);
  const [place, setPlace] = useState({ top: 0, left: 0, width, up: true, fromTop: 0, fromLeft: 0, fromRadius: 0 });
  const [fill, setFill] = useState({ y: 0, h: 0, glide: false });
  // The fill only shows while the pointer is over a row (or the keyboard moves through the list)
  const [shown, setShown] = useState(false);
  const wasShown = useRef(false);

  // The menu opens in two steps: first the closed shape moves to the button, then it morphs open.
  // In one step the very first opening had no start shape and just appeared.
  const [visible, setVisible] = useState(false);
  const [placing, setPlacing] = useState(false);
  // More rows below the visible ones: the list fades out at its lower edge
  const [more, setMore] = useState(false);
  const checkMore = () => {
    const el = panel.current;
    if (el) setMore(el.scrollTop + el.clientHeight < el.scrollHeight - 2);
  };
  // Closed and done animating, the menu leaves the layout – so it never makes the page around it scroll
  const [parked, setParked] = useState(true);

  // Place the menu next to its button, always inside the visible frame (the nearest clipping ancestor, or the viewport), and focus it
  useLayoutEffect(() => {
    if (!open) {
      setVisible(false);
      const park = window.setTimeout(() => setParked(true), 320);
      return () => clearTimeout(park);
    }
    if (parked) {
      setParked(false);
      return;
    }
    if (!anchor.current || !container.current || !panel.current) return;
    const button = anchor.current.getBoundingClientRect();
    const box = container.current.getBoundingClientRect();
    const frame = clippingFrame(container.current);
    // Inside a scaled preview (a CSS transform) the measured pixels are scaled too – work in those, then scale back
    const scale = box.width / (container.current.offsetWidth || box.width) || 1;
    // Never wider than the visible frame
    const layoutWidth = Math.min(wide ? container.current.offsetWidth : width, (frame.right - frame.left) / scale - 16);
    panel.current.style.width = `${layoutWidth}px`;
    panel.current.style.maxHeight = "";
    const menuWidth = layoutWidth * scale;
    const natural = panel.current.offsetHeight * scale;
    const gap = 8 * scale;
    // Above the composer the plus menu ends a small gap above it, like Codex's
    const tuck = wide ? -6 * scale : 0;
    const inset = 8 * scale;
    // A wide menu sits right above the whole composer, the others next to their button
    const edge = wide ? box : button;

    const roomAbove = edge.top + tuck - (frame.top + inset);
    const roomBelow = frame.bottom - inset - (edge.bottom + gap);
    // The plus menu stays above the composer while a few rows fit there; the others take the side with more room
    const up = wide ? roomAbove >= Math.min(natural, 160 * scale) || roomAbove >= roomBelow : roomAbove >= natural || roomAbove >= roomBelow;
    // Too long for its side: it ends before the frame's edge and scrolls inside
    const height = Math.min(natural, Math.max(0, up ? roomAbove : roomBelow));
    const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(value, max));
    const top = clamp(up ? edge.top + tuck - height : edge.bottom + gap, frame.top + inset, frame.bottom - inset - height);
    const left = clamp(wide ? box.left : align === "start" ? button.left : button.right - menuWidth, frame.left + inset, frame.right - inset - menuWidth);

    // The morph starts as a button-sized piece in the corner closest to the button
    const fromLeft = clamp(button.left - left, 0, menuWidth - 1) / scale;
    const fromTop = (up ? clamp(height - button.height, 0, height - 1) : 0) / scale;
    panel.current.style.maxHeight = `${height / scale}px`;
    setPlace({ top: (top - box.top) / scale, left: (left - box.left) / scale, width: layoutWidth, up, fromTop, fromLeft, fromRadius: 6 });
    setActive(Math.max(0, flat.findIndex((row) => row.checked)));
    setShown(false);
    setPlacing(true);
  }, [open, parked]); // eslint-disable-line react-hooks/exhaustive-deps

  // Once the closed shape sits at the button, apply it, then open – so the browser has a start shape to morph from
  useLayoutEffect(() => {
    if (!open || visible || !panel.current) return;
    void getComputedStyle(panel.current).clipPath;
    setPlacing(false);
    setVisible(true);
  }, [place]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (visible) {
      panel.current?.focus({ preventScroll: true });
      // Opened from the keyboard: the active row shows right away, so it's clear where the arrows start
      if (panel.current?.matches(":focus-visible")) setShown(true);
    }
    checkMore();
  }, [visible]);

  // One fill glides between rows; when it first appears it fades in on the spot
  useLayoutEffect(() => {
    const row = rows.current[active];
    if (row) setFill({ y: row.offsetTop, h: row.offsetHeight, glide: wasShown.current && shown });
    // A long list scrolls – keep the active row in view while the keyboard moves
    const list = panel.current;
    if (row && list && open) {
      if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop - 4;
      else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight + 4;
    }
    wasShown.current = shown;
  }, [active, open, shown]);

  // Click outside closes
  useEffect(() => {
    if (!open) return;
    const away = (event: globalThis.PointerEvent) => {
      const target = event.target as Node;
      if (panel.current?.contains(target) || anchor.current?.contains(target)) return;
      onClose(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open, onClose, anchor]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    // A link in the head (e.g. Learn more) is the menu's one other stop: Tab reaches it from the list, Shift+Tab goes back
    const onList = event.target === panel.current;
    const link = panel.current?.querySelector<HTMLElement>(".ui-ccc__menu-head :is(a, button)");
    if (event.key === "Tab" && link && onList !== event.shiftKey) {
      event.preventDefault();
      (onList ? link : panel.current)?.focus({ preventScroll: true });
      return;
    }
    // On the link only Tab and Escape belong to the menu – Enter and Space press the link itself
    if (!onList && event.key !== "Tab" && event.key !== "Escape") return;
    const last = flat.length - 1;
    const moves: Record<string, number> = {
      ArrowDown: Math.min(last, active + 1),
      ArrowUp: Math.max(0, active - 1),
      Home: 0,
      End: last,
    };
    if (event.key in moves) {
      event.preventDefault();
      setActive(moves[event.key]);
      setShown(true);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onPick(flat[active].id);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose(true);
    } else if (event.key === "Tab") {
      // Leaves from the menu's button, so Tab goes on to the control after it (Shift+Tab to the one before)
      anchor.current?.focus({ preventScroll: true });
      onClose(false);
    }
  };

  let index = -1;
  return (
    <div
      className="ui-ccc__menu"
      data-open={visible}
      data-placing={placing}
      data-up={place.up}
      data-inline={inline || undefined}
      data-above={(wide && place.up) || undefined}
      style={
        {
          display: parked ? "none" : undefined,
          top: place.top,
          left: place.left,
          width: place.width,
          "--from-top": `${place.fromTop}px`,
          "--from-left": `${place.fromLeft}px`,
          "--from-bottom": place.up ? "0px" : `calc(100% - ${place.fromRadius * 2}px)`,
          "--from-radius": `${place.fromRadius}px`,
        } as CSSProperties
      }
      inert={!visible}
    >
      <div
        ref={panel}
        className="ui-ccc__menu-panel"
        role={role}
        tabIndex={-1}
        aria-label={label}
        aria-activedescendant={`${id}-${active}`}
        onKeyDown={onKeyDown}
        onPointerLeave={() => setShown(false)}
        onScroll={checkMore}
        data-more={more || undefined}
      >
        <span
          className="ui-ccc__menu-fill"
          aria-hidden="true"
          data-glide={fill.glide}
          style={{ transform: `translateY(${fill.y}px)`, height: fill.h, opacity: shown ? 1 : 0 }}
        />
        {head && <div className="ui-ccc__menu-head">{head}</div>}
        {sections.map((section, si) => (
          <div key={si} className="ui-ccc__menu-section" role="group" aria-label={section.title}>
            {section.title && (
              <div className="ui-ccc__menu-title" aria-hidden="true">
                {section.title}
              </div>
            )}
            {section.rows.map((row) => {
              const i = ++index;
              return (
                <div
                  key={row.id}
                  ref={(el) => {
                    rows.current[i] = el;
                  }}
                  id={`${id}-${i}`}
                  role={role === "listbox" ? "option" : row.checked === undefined ? "menuitem" : "menuitemcheckbox"}
                  aria-selected={role === "listbox" ? !!row.checked : undefined}
                  aria-checked={role === "menu" && row.checked !== undefined ? row.checked : undefined}
                  data-checked={row.checked || undefined}
                  data-tone={row.tone}
                  className="ui-ccc__option"
                  onPointerEnter={() => {
                    setActive(i);
                    setShown(true);
                  }}
                  onClick={() => onPick(row.id)}
                >
                  <span className="ui-ccc__option-logo" aria-hidden="true">
                    {row.icon}
                  </span>
                  <span className="ui-ccc__option-text">
                    <span className="ui-ccc__option-name" title={row.label} dir="auto">
                      {row.label}
                    </span>
                    {row.description && (
                      <span className="ui-ccc__option-desc" dir="auto">
                        {row.description}
                      </span>
                    )}
                  </span>
                  <span className="ui-ccc__option-check" aria-hidden="true">
                    <TickIcon />
                  </span>
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}


/* ---------- Icons: 16px, thin lines and solid shapes ---------- */

const PlusIcon = () => (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path d="M8 2.5c.28 0 .5.22.5.5v4.5H13a.5.5 0 0 1 0 1H8.5V13a.5.5 0 0 1-1 0V8.5H3a.5.5 0 0 1 0-1h4.5V3c0-.28.22-.5.5-.5Z" />
  </svg>
);

const line = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.6,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

const TickIcon = () => (
  <svg viewBox="0 0 24 24" {...line} strokeWidth={2} aria-hidden="true">
    <path d="M5 14L8.5 17.5L19 6.5" />
  </svg>
);


/* ---------- Styles ---------- */

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-ccc. Steve: every one reads a design token (--aui-*, mapped
   to Steve's own in styles/affectui-theme.css), so one block serves light and dark */
.ui-ccc {
  --ui-ccc-card: var(--aui-surface, #ffffff);
  --ui-ccc-pop: var(--aui-overlay, #ffffff);
  --ui-ccc-strong: var(--aui-ink, #000000);
  --ui-ccc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-ccc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-ccc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-ccc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  /* The placeholder: lighter than the controls, darker than faint lines */
  --ui-ccc-hint: var(--aui-placeholder, rgba(0, 0, 0, 0.36));
  --ui-ccc-icon: var(--aui-muted, rgba(0, 0, 0, 0.447));
  --ui-ccc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-ccc-edge: var(--aui-border-strong, rgba(0, 0, 0, 0.13));
  --ui-ccc-fill: color-mix(in srgb, var(--ui-ccc-ink) 7%, transparent);
  --ui-ccc-wash: color-mix(in srgb, var(--ui-ccc-ink) 4%, transparent);
  --ui-ccc-solid: var(--aui-primary, rgba(0, 0, 0, 0.88));
  --ui-ccc-on-solid: var(--aui-on-primary, #fcfcfc);
  --ui-ccc-accent: var(--aui-blue, rgba(0, 96, 226, 0.765));
  --ui-ccc-drop: var(--aui-info, #0070d6);
  /* Strokes are shadows: a hairline plus a soft lift */
  --ui-ccc-shadow: var(--aui-shadow-card-hover, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-ccc-shadow-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 12px 28px -10px rgba(0, 0, 0, 0.16), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-ccc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-ccc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  --ui-ccc-ease-pop: cubic-bezier(0.34, 1.5, 0.64, 1);
  /* Steve: the message in the body size of the conversation it joins */
  --ui-ccc-size: 14px;
  --ui-ccc-leading: 22px;
  position: relative;
  container-type: inline-size;
  width: 100%;
}

.ui-ccc__body {
  position: relative;
  border-radius: 16px;
  background: var(--ui-ccc-card);
  box-shadow: 0 0 0 1px var(--ui-ccc-line), var(--ui-ccc-shadow);
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: 13px;
  line-height: 20px;
  font-weight: 400;
  color: var(--ui-ccc-text);
  -webkit-font-smoothing: antialiased;
  -webkit-tap-highlight-color: transparent;
  transition: box-shadow 180ms var(--ui-ccc-ease);
}
/* No ring while typing – the card's edge firms up instead */
.ui-ccc__body:has(.ui-ccc__message:focus-visible) { box-shadow: 0 0 0 1px var(--ui-ccc-edge), var(--ui-ccc-shadow); }
.ui-ccc[data-drop] .ui-ccc__body { box-shadow: 0 0 0 1.5px var(--ui-ccc-drop), 0 0 0 5px color-mix(in srgb, var(--ui-ccc-drop) 15%, transparent); }
.ui-ccc[data-disabled] .ui-ccc__write { opacity: 0.6; }
.ui-ccc__body *, .ui-ccc__body *::before, .ui-ccc__body *::after { box-sizing: border-box; }
:where(.ui-ccc__icon-btn, .ui-ccc__menu-link) {
  appearance: none;
  margin: 0;
  padding: 0;
  border: 0;
  background: none;
  font: inherit;
  color: inherit;
  text-align: left;
  cursor: pointer;
}
/* Steve: only the card's own buttons and icons – what a page puts inside (toggles, files, the queue) keeps its look */
:is(.ui-ccc__icon-btn, .ui-ccc__menu) svg { display: block; flex: none; max-width: none; }
.ui-ccc__icon-btn:focus-visible { outline: 2px solid var(--ui-ccc-focus); outline-offset: 2px; }

/* ---------- Above the message: the queue, the files ---------- */
.ui-ccc__head { position: relative; z-index: 3; display: flex; flex-direction: column; gap: 8px; padding: 10px 10px 0; }
.ui-ccc__head:empty { display: none; }

/* ---------- Writing area ---------- */
.ui-ccc__write { position: relative; z-index: 2; padding: 12px 10px 10px; border-radius: 16px; cursor: text; }
.ui-ccc__field { position: relative; padding: 0 4px; }
/* The message and anything drawn under it (Steve: the marks of items, mentions and commands) share one box */
.ui-ccc__message,
.ui-ccc__under {
  display: block;
  width: 100%;
  margin: 0;
  padding: 0;
  border: 0;
  font: inherit;
  font-size: var(--ui-ccc-size);
  line-height: var(--ui-ccc-leading);
  color: var(--ui-ccc-strong);
  white-space: pre-wrap;
  overflow-wrap: break-word;
  text-align: start;
}
.ui-ccc__message {
  /* grows with the text from two lines to seven, then scrolls */
  field-sizing: content;
  min-height: calc(var(--ui-ccc-leading) * 2);
  max-height: calc(var(--ui-ccc-leading) * 7);
  outline: 0;
  background: transparent;
  resize: none;
  overflow-y: auto;
  scrollbar-width: none;
  caret-color: var(--ui-ccc-accent);
}
.ui-ccc__message::-webkit-scrollbar { display: none; }
.ui-ccc__message::placeholder { color: var(--ui-ccc-hint); opacity: 1; }
.ui-ccc__message:focus-visible { outline: none; }
.ui-ccc__message:disabled { cursor: default; }
.ui-ccc__under { position: absolute; inset: 0 4px; width: auto; overflow: hidden; pointer-events: none; }
/* Text drawn by the layer under it: the field's own letters are see-through, a selection only tints */
.ui-ccc__message[data-under] { color: transparent; }
.ui-ccc__message[data-under]::selection { background: color-mix(in srgb, var(--ui-ccc-accent) 25%, transparent); color: transparent; }

/* ---------- Tools under the message ---------- */
.ui-ccc__tools { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-top: 10px; }
.ui-ccc__group { display: flex; align-items: center; gap: 4px; min-width: 0; }
.ui-ccc__group--end { flex: 0 1 auto; justify-content: flex-end; }

/* Ghost icon buttons: 28px, icon in the gray of the reference */
.ui-ccc__icon-btn {
  display: grid;
  flex: none;
  place-items: center;
  width: 28px;
  height: 28px;
  border-radius: 8px;
  color: var(--ui-ccc-icon);
  transition: background-color 120ms ease, color 120ms ease, transform 120ms var(--ui-ccc-ease);
}
.ui-ccc__icon-btn svg { width: 16px; height: 16px; }
.ui-ccc__icon-btn svg:not([fill="none"]) { fill: currentColor; }
.ui-ccc__icon-btn:hover:not(:disabled), .ui-ccc__icon-btn[aria-expanded="true"] { background: var(--ui-ccc-wash); color: var(--ui-ccc-ink); }
.ui-ccc__icon-btn:active:not(:disabled) { transform: scale(0.94); }
.ui-ccc__icon-btn:disabled { opacity: 0.45; cursor: default; }
.ui-ccc__icon-btn[aria-expanded="true"] svg { transform: rotate(45deg); }
.ui-ccc__icon-btn svg { transition: transform 260ms var(--ui-ccc-ease); }

/* Model: a quiet label – where the message goes */
.ui-ccc__model {
  display: inline-flex;
  align-items: center;
  height: 28px;
  min-width: 0;
  padding: 0 6px;
  font-size: 12px;
  white-space: nowrap;
  color: var(--ui-ccc-muted);
}
.ui-ccc__model-name { min-width: 0; max-width: 220px; overflow: hidden; text-overflow: ellipsis; }

/* ---------- Menus: grow out of the corner next to their button ---------- */
.ui-ccc__menu {
  --morph: 380ms cubic-bezier(0.32, 0.72, 0, 1);
  position: absolute;
  z-index: 5;
  width: 260px;
  transition: visibility 0s linear 0s;
}
.ui-ccc__menu[data-open="false"] { visibility: hidden; pointer-events: none; transition: visibility 0s linear 300ms; }
.ui-ccc__menu-panel {
  position: relative;
  display: flex;
  flex-direction: column;
  padding: 4px;
  border-radius: 10px;
  background: var(--ui-ccc-pop);
  box-shadow: var(--ui-ccc-shadow-pop);
  outline: none;
  clip-path: inset(-40px -40px -40px -40px round 10px);
  transition: clip-path var(--morph);
  /* A long list scrolls inside the panel */
  max-height: 340px;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: thin;
}
.ui-ccc__menu[data-placing="true"] .ui-ccc__menu-panel { transition: none !important; }
.ui-ccc__menu[data-open="false"] .ui-ccc__menu-panel {
  clip-path: inset(var(--from-top) 0 var(--from-bottom) var(--from-left) round var(--from-radius));
  transition: clip-path 260ms cubic-bezier(0.4, 0, 0.2, 1);
}
.ui-ccc__menu-panel > * { transition: opacity 220ms ease-out 100ms, transform 360ms var(--ui-ccc-ease) 60ms; }
.ui-ccc__menu[data-open="false"] .ui-ccc__menu-panel > * { opacity: 0; transition: opacity 120ms ease-in; }
.ui-ccc__menu[data-open="false"] .ui-ccc__option { transform: translateY(4px); }
.ui-ccc__menu[data-open="false"][data-up="false"] .ui-ccc__option { transform: translateY(-4px); }
.ui-ccc__menu-panel:focus-visible { outline: none; }
/* The plus menu above the composer: as round as it, the composer's own hairline instead of a lifted shadow,
   and the list fades out toward its lower edge */
.ui-ccc__menu[data-above] .ui-ccc__menu-panel { border-radius: 16px; box-shadow: var(--ui-ccc-shadow); }
.ui-ccc__menu[data-above]:not([data-open="false"]) .ui-ccc__menu-panel { clip-path: inset(-2px round 16px); }
.ui-ccc__menu[data-above] .ui-ccc__menu-panel[data-more]::after {
  content: "";
  position: sticky;
  bottom: -4px;
  flex: none;
  height: 28px;
  margin: -28px -4px -4px;
  background: linear-gradient(to bottom, color-mix(in srgb, var(--ui-ccc-pop) 0%, transparent), var(--ui-ccc-pop) 90%);
  pointer-events: none;
}
.ui-ccc__menu-fill {
  position: absolute;
  left: 4px;
  right: 4px;
  top: 0;
  border-radius: 6px;
  background: var(--ui-ccc-fill);
  pointer-events: none;
  transition: transform 190ms cubic-bezier(0.25, 1, 0.3, 1), height 190ms cubic-bezier(0.25, 1, 0.3, 1), opacity 160ms ease-out;
}
.ui-ccc__menu-fill[data-glide="false"] { transition: opacity 160ms ease-out; }
.ui-ccc__option { position: relative; display: flex; align-items: flex-start; gap: 10px; padding: 6px 8px; border-radius: 6px; cursor: pointer; }
.ui-ccc__option-logo { display: grid; place-items: center; flex: none; width: 16px; height: 16px; margin-top: 2px; color: var(--ui-ccc-icon); }
.ui-ccc__option-logo svg { width: 100%; height: 100%; }
.ui-ccc__option-text { display: flex; flex-direction: column; flex: 1; min-width: 0; }
/* Names in the body text: 13px regular in the ink gray – the picked one in medium and full black */
.ui-ccc__option-name { overflow: hidden; text-align: start; font-size: 13px; font-weight: 400; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; color: var(--ui-ccc-ink); }
.ui-ccc__option[aria-selected="true"] .ui-ccc__option-name { color: var(--ui-ccc-strong); }
.ui-ccc__option-desc { text-align: start; font-size: 12px; line-height: 16px; color: var(--ui-ccc-hint); }
.ui-ccc__option-check { flex: none; width: 16px; height: 16px; margin-top: 2px; color: var(--ui-ccc-ink); opacity: 0; transform: scale(0.6); transition: opacity 160ms ease-out, transform 260ms var(--ui-ccc-ease-pop); }
.ui-ccc__option-check svg { width: 100%; height: 100%; }
.ui-ccc__option[data-checked] .ui-ccc__option-check { opacity: 1; transform: none; }
/* Full access: only its icon and name in red, the line under it stays gray */
.ui-ccc__option[data-tone="alert"] :is(.ui-ccc__option-logo, .ui-ccc__option-name) { color: var(--ui-ccc-alert); }
/* The plus menu: one line per row, the description beside the name */
.ui-ccc__menu[data-inline] .ui-ccc__option-text { flex-direction: row; align-items: baseline; gap: 8px; }
.ui-ccc__menu[data-inline] .ui-ccc__option-desc { overflow: hidden; font-size: 13px; line-height: 20px; white-space: nowrap; text-overflow: ellipsis; }
/* A question with a link on top, titles over the sections */
.ui-ccc__menu-head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; padding: 6px 8px 4px; font-size: 12px; line-height: 16px; color: var(--ui-ccc-hint); }
.ui-ccc__menu-link { flex: none; font-size: 12px; color: var(--ui-ccc-muted) !important; transition: color 140ms ease; }
.ui-ccc__menu-link:hover { color: var(--ui-ccc-ink) !important; text-decoration: underline; text-underline-offset: 3px; }
.ui-ccc__menu-section { display: flex; flex-direction: column; }
.ui-ccc__menu-section + .ui-ccc__menu-section { margin-top: 4px; }
.ui-ccc__menu-title { padding: 6px 8px 2px; font-size: 12px; line-height: 16px; color: var(--ui-ccc-muted); }

.ui-ccc__sr-only { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }

@keyframes ui-ccc-model-in { from { opacity: 0; filter: blur(3px); transform: translateY(30%); } }
@keyframes ui-ccc-logo-in { from { opacity: 0; transform: scale(0.6) rotate(-12deg); } }

/* Narrow containers: the message starts at one line, and the label of where it goes gives way first – Steve: only as
   far as the buttons need, since on a phone it is the one place the session's harness and model are said */
@container (max-width: 480px) {
  .ui-ccc__message { min-height: var(--ui-ccc-leading); max-height: calc(var(--ui-ccc-leading) * 5); }
  .ui-ccc__model { flex: 0 1 auto; padding: 0 4px; }
  .ui-ccc__model-name { max-width: none; }
}
/* Touch screens: taller hit targets */
@media (pointer: coarse) {
  .ui-ccc__icon-btn { width: 34px; height: 34px; }
  .ui-ccc__model { height: 34px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-ccc [class*="ui-ccc__"], .ui-ccc [class*="ui-ccc__"]::after { animation: none !important; transition-duration: 1ms !important; transition-delay: 0ms !important; }
}

/* Text fields: no focus box while typing – also against global :focus-visible rules of the host page */
.ui-ccc textarea:focus-visible { outline: none; }

/* Dark mode: every color above is a token that switches on its own (affectUI's dark defaults stay as fallbacks) */
:where(.dark, [data-theme="dark"]) .ui-ccc {
  --ui-ccc-card: var(--aui-surface, #1c1c1e);
  --ui-ccc-pop: var(--aui-overlay, #1f1f22);
  --ui-ccc-strong: var(--aui-ink, #ffffff);
  --ui-ccc-ink: var(--aui-ink, rgba(255, 255, 255, 0.9));
  --ui-ccc-text: var(--aui-muted, rgba(255, 255, 255, 0.62));
  --ui-ccc-muted: var(--aui-muted, rgba(255, 255, 255, 0.46));
  --ui-ccc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.3));
  --ui-ccc-hint: var(--aui-placeholder, rgba(255, 255, 255, 0.36));
  --ui-ccc-icon: var(--aui-muted, rgba(255, 255, 255, 0.5));
  --ui-ccc-line: var(--aui-border, rgba(255, 255, 255, 0.08));
  --ui-ccc-edge: var(--aui-border-strong, rgba(255, 255, 255, 0.2));
  --ui-ccc-solid: var(--aui-primary, rgba(255, 255, 255, 0.92));
  --ui-ccc-on-solid: var(--aui-on-primary, #161618);
  --ui-ccc-accent: var(--aui-blue, #4fb0fe);
  --ui-ccc-shadow: var(--aui-shadow-card-hover, 0 0 0 0.5px rgba(255, 255, 255, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-ccc-shadow-pop: var(--aui-shadow-popover, 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 12px 28px -10px rgba(0, 0, 0, 0.6));
  --ui-ccc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
}
`;
