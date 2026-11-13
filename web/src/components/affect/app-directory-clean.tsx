/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
"use client";

import { useLayoutEffect, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode, type RefObject, type HTMLAttributes } from "react";

/*
 * App Directory (clean) – browse and search the services Steve can connect, in a single self-contained file.
 * No tray: a quiet header with the connected count, a gray search field, a calm list of categories
 * and single app cards with hairline shadows. The styles are injected below, so no extra files are needed.
 *
 * Steve: the cards are the Hub's providers and the categories its capability areas (GET /connections/integrations).
 * Connecting is never a toggle here – every service has its own steps (credentials, OAuth, an app to create), so a
 * card's button opens the service's page. All copy comes in through `labels`, so it follows Steve's i18n.
 */

export type App = {
  id: string;
  name: string;
  /** The line under the name, e.g. whether and how much is connected */
  vendor: ReactNode;
  /** Two lines at most: what it can do, or the state of its installations */
  description: ReactNode;
  /** Words the search also matches (the description may be markup) */
  keywords?: string;
  categories: string[];
  logo: ReactNode;
  /** Brand color behind the logo – leave empty for a plain tile with a thin edge */
  tile?: string;
  connected?: boolean;
  /** Something needs the owner's attention (missing permissions, re-authorization): the line under the name says so,
   * and the card's button reads Manage in the warning tone instead of the green Connected */
  attention?: boolean;
  /** Extra attributes on the card, e.g. test hooks */
  attrs?: HTMLAttributes<HTMLLIElement> & Record<`data-${string}`, string | number | undefined>;
};

export type AppCategory = { id: string; label: string; icon: ReactNode };

export type AppDirectoryLabels = {
  connected: (n: number) => string;
  search: string;
  filter: string;
  all: string;
  onlyConnected: string;
  notConnected: string;
  show: string;
  categories: string;
  results: (q: string) => string;
  noMatch: string;
  noneInCategory: string;
  none: string;
  /** The empty state's button when a search hides the apps */
  clearSearch: string;
  /** … when only the status filter does */
  clearFilter: string;
  connectShort: string;
  connectedShort: string;
  manage: string;
};

export type AppDirectoryProps = {
  title?: string;
  subtitle?: ReactNode;
  /** The section's name when it has no title (the page above names it) */
  "aria-label"?: string;
  apps: App[];
  categories: AppCategory[];
  labels: AppDirectoryLabels;
  /** A card's button or the card itself was pressed */
  onOpen?: (app: App) => void;
  className?: string;
} & Record<`data-${string}`, string | number | undefined>;

type Status = "all" | "connected" | "available";

const formatCount = (n: number) => new Intl.NumberFormat().format(n);
/** First letter as a whole grapheme, so accents and emoji stay intact – the tile of an app without a logo */
const initial = (name: string) => (Array.from(name.trim())[0] ?? "?").toLocaleUpperCase();
const text = (n: ReactNode) => (typeof n === "string" || typeof n === "number" ? String(n) : "");

/** The app's logo, or its first letter when there is none */
const Logo = ({ app }: { app: App }) =>
  app.logo != null && app.logo !== false ? <>{app.logo}</> : <span className="ui-adc__tile-initial">{initial(app.name)}</span>;

export function AppDirectory({ title, subtitle, "aria-label": ariaLabel, apps, categories, labels: L, onOpen, className, ...rest }: AppDirectoryProps) {
  const connectedN = apps.filter((a) => a.connected).length;
  const [category, setCategory] = useState("all");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<Status>("all");
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLButtonElement>(null);
  const searchId = useId();
  const uid = useId();
  const mainRef = useRef<HTMLDivElement>(null);
  const morphRef = useSmoothHeight(mainRef);

  useEffect(() => {
    if (!menu) return;
    const away = (e: PointerEvent) => !menuRef.current?.contains(e.target as Node) && setMenu(false);
    // Escape closes the menu and hands focus back to its button
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      setMenu(false);
      filterRef.current?.focus();
    };
    // Focus moves into the menu, onto the chosen option
    const items = menuItems();
    (items.find((i) => i.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
    window.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc);
    return () => {
      window.removeEventListener("pointerdown", away);
      window.removeEventListener("keydown", esc);
    };
  }, [menu]);

  const menuItems = () => Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? []);

  // Arrows, Home and End move through the options; Tab leaves and closes the menu
  const onMenuKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const items = menuItems();
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const last = items.length - 1;
    const to =
      e.key === "ArrowDown" ? (at < last ? at + 1 : 0)
      : e.key === "ArrowUp" ? (at > 0 ? at - 1 : last)
      : e.key === "Home" ? 0
      : e.key === "End" ? last
      : -1;
    if (to >= 0) {
      e.preventDefault();
      items[to]?.focus();
    } else if (e.key === "Tab") setMenu(false);
  };

  const inCategory = (app: App, id: string) => (id === "all" ? true : app.categories.includes(id));

  const q = query.trim().toLowerCase();
  const shown = useMemo(
    () =>
      apps.filter((a) => {
        if (q && !`${a.name} ${text(a.vendor)} ${text(a.description)} ${a.keywords ?? ""}`.toLowerCase().includes(q)) return false;
        if (!q && !inCategory(a, category)) return false;
        if (status === "connected" && !a.connected) return false;
        if (status === "available" && a.connected) return false;
        return true;
      }),
    [apps, q, category, status],
  );

  const heading = q ? L.results(query.trim()) : category === "all" ? L.all : categories.find((c) => c.id === category)?.label ?? L.all;
  const statusLabel = { all: L.all, connected: L.onlyConnected, available: L.notConnected }[status];
  const allCategory: AppCategory = { id: "all", label: L.all, icon: <AllIcon /> };

  return (
    <>
      <style href="ui-app-directory-clean" precedence="default">
        {css}
      </style>
      <section className={["ui-adc", className].filter(Boolean).join(" ")} aria-label={title ?? ariaLabel} {...rest}>
        <div className="ui-adc__card">
          {(title || subtitle) && (
            <header className="ui-adc__intro">
              <div className="ui-adc__intro-text">
                {title && <h2 className="ui-adc__title">{title}</h2>}
                {subtitle && <p className="ui-adc__subtitle">{subtitle}</p>}
              </div>
              <span className="ui-adc__meta">{L.connected(connectedN)}</span>
            </header>
          )}

          <div className="ui-adc__tools">
            <label className="ui-adc__search" htmlFor={searchId}>
              <SearchIcon />
              <input
                id={searchId}
                type="search"
                placeholder={L.search}
                aria-label={L.search}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                autoComplete="off"
                data-adc-search
              />
            </label>
            <div className="ui-adc__filter" ref={menuRef}>
              <button
                ref={filterRef}
                type="button"
                className="ui-adc__btn"
                aria-haspopup="menu"
                aria-expanded={menu}
                data-active={status !== "all" || undefined}
                onClick={() => setMenu((m) => !m)}
                onKeyDown={(e) => {
                  // Arrow keys open the menu too
                  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                    e.preventDefault();
                    setMenu(true);
                  }
                }}
              >
                <FilterIcon />
                {status === "all" ? L.filter : statusLabel}
              </button>
              {menu && (
                <div className="ui-adc__menu" role="menu" aria-label={L.show} onKeyDown={onMenuKey}>
                  {(["all", "connected", "available"] as Status[]).map((s) => (
                    <button
                      key={s}
                      type="button"
                      role="menuitemradio"
                      aria-checked={status === s}
                      tabIndex={-1}
                      className="ui-adc__menu-item"
                      onClick={() => {
                        setStatus(s);
                        setMenu(false);
                        filterRef.current?.focus();
                      }}
                    >
                      <span className="ui-adc__menu-icon">{{ all: <GridIcon />, connected: <LinkIcon />, available: <UnlinkIcon /> }[s]}</span>
                      <span className="ui-adc__menu-label">{{ all: L.all, connected: L.onlyConnected, available: L.notConnected }[s]}</span>
                      {status === s && <CheckIcon />}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="ui-adc__layout">
            <nav className="ui-adc__nav" aria-label={L.categories}>
              {[allCategory, ...categories].map((c) => {
                const count = apps.filter((a) => inCategory(a, c.id)).length;
                const current = !q && category === c.id;
                return (
                  <button
                    key={c.id}
                    type="button"
                    className="ui-adc__nav-item"
                    aria-current={current || undefined}
                    data-adc-category={c.id}
                    onClick={() => {
                      setCategory(c.id);
                      setQuery("");
                    }}
                  >
                    <span className="ui-adc__nav-icon">{c.icon}</span>
                    <span className="ui-adc__nav-label" title={c.label}>
                      {c.label}
                    </span>
                    <span className="ui-adc__nav-count">{formatCount(count)}</span>
                  </button>
                );
              })}
            </nav>

            <div className="ui-adc__morph" ref={morphRef}>
            <div className="ui-adc__main" ref={mainRef}>
              <h3 className="ui-adc__heading" aria-live="polite">
                <span className="ui-adc__heading-text" title={heading}>
                  {heading}
                </span>
                <span>{formatCount(shown.length)}</span>
              </h3>
              {shown.length === 0 ? (
                <div className="ui-adc__empty">
                  {/* Only offer to clear when a search or filter hides the apps */}
                  <p>{q || status !== "all" ? L.noMatch : apps.length ? L.noneInCategory : L.none}</p>
                  {(q || status !== "all") && (
                  <button
                    type="button"
                    className="ui-adc__btn"
                    onClick={() => {
                      setQuery("");
                      setStatus("all");
                    }}
                  >
                    {q ? L.clearSearch : L.clearFilter}
                  </button>
                  )}
                </div>
              ) : (
                <ul className="ui-adc__grid">
                  {shown.map((app, i) => (
                    <li
                      key={app.id}
                      className="ui-adc__app"
                      data-connected={app.connected || undefined}
                      data-clickable={onOpen ? true : undefined}
                      style={{ animationDelay: `${Math.min(i, 12) * 30}ms` }}
                      // the whole card opens the service; the button is the keyboard's way in
                      onClick={onOpen ? () => onOpen(app) : undefined}
                      {...app.attrs}
                    >
                      <div className="ui-adc__app-head">
                        <span className="ui-adc__tile" data-filled={app.tile ? true : undefined} style={app.tile ? { background: app.tile } : undefined}>
                          <Logo app={app} />
                        </span>
                        <span className="ui-adc__app-name">
                          <span className="ui-adc__name">
                            <span className="ui-adc__name-text" title={app.name} id={`${uid}-${app.id}`}>
                              {app.name}
                            </span>
                          </span>
                          {app.vendor && (
                            <span className="ui-adc__vendor" data-attention={app.attention || undefined} title={text(app.vendor) || undefined}>
                              {app.vendor}
                            </span>
                          )}
                        </span>
                      </div>
                      <div className="ui-adc__desc" title={text(app.description) || undefined}>
                        {app.description}
                      </div>
                      <OpenButton app={app} labels={L} nameId={`${uid}-${app.id}`} onClick={onOpen ? () => onOpen(app) : undefined} />
                    </li>
                  ))}
                </ul>
              )}
            </div>
            </div>
          </div>
        </div>
      </section>
    </>
  );
}

/** Connect, Connected that turns into Manage on hover, or Manage in the warning tone when the service needs
 * attention – all open the service's page. Steve: the button's name is its visible text (voice control can say what it
 * sees); the service's name comes in as its description. */
function OpenButton({ app, labels: L, nameId, onClick }: { app: App; labels: AppDirectoryLabels; nameId: string; onClick?: () => void }) {
  const state = app.connected ? (app.attention ? "attention" : "on") : "off";
  return (
    <button
      type="button"
      className="ui-adc__connect"
      data-state={state}
      aria-describedby={nameId}
      onClick={(e) => {
        e.stopPropagation();
        onClick?.();
      }}
      data-adc-open={app.id}
    >
      <span className="ui-adc__connect-label">
        {state === "on" ? (
          <>
            <LinkIcon /> <span className="ui-adc__on-label">{L.connectedShort}</span>
            <span className="ui-adc__off-label">{L.manage}</span>
          </>
        ) : state === "attention" ? (
          L.manage
        ) : (
          L.connectShort
        )}
      </span>
    </button>
  );
}

/* ---------- Icons ---------- */
const line = (d: string, size = 16) => (
  <svg viewBox="0 0 16 16" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={d} />
  </svg>
);
const SearchIcon = () => line("M7 12.5a5.5 5.5 0 1 0 0-11 5.5 5.5 0 0 0 0 11ZM11 11l3.5 3.5");
const FilterIcon = () => line("M2.5 4.5h11M4.5 8h7M6.5 11.5h3");
const CheckIcon = () => line("m3.5 8.5 3 3 6-7", 16);
const GridIcon = () => line("M2.5 2.5h4v4h-4zM9.5 2.5h4v4h-4zM2.5 9.5h4v4h-4zM9.5 9.5h4v4h-4z");
const LinkIcon = () => line("M6.5 9.5l3-3M7.3 4.7l.9-.9a2.5 2.5 0 0 1 3.5 3.5l-.9.9M8.7 11.3l-.9.9a2.5 2.5 0 0 1-3.5-3.5l.9-.9");
const UnlinkIcon = () => line("M7.3 4.7l.9-.9a2.5 2.5 0 0 1 3.5 3.5l-.9.9M8.7 11.3l-.9.9a2.5 2.5 0 0 1-3.5-3.5l.9-.9M2.5 2.5l11 11");

/** Steve: the "all" category's icon; the areas bring their own */
const AllIcon = GridIcon;


/**
 * Lets a box glide to the height of its content instead of jumping: the outer box follows the inner one
 * with a height transition whenever the content grows or shrinks.
 */
function useSmoothHeight(inner: RefObject<HTMLElement | null>) {
  const outer = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const o = outer.current;
    const i = inner.current;
    if (!o || !i) return;
    const sync = () => {
      o.style.height = `${i.offsetHeight}px`;
    };
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(i);
    // Transitions switch on after the first measurement, so the first render doesn't animate
    const frame = requestAnimationFrame(() => o.setAttribute("data-smooth", ""));
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [inner]);
  return outer;
}

/* ---------- Styles ---------- */

const css = /* css */ `
.ui-adc {
  --ui-adc-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-adc-card: var(--aui-surface, #fff);
  --ui-adc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(0, 0, 0, 0.06), 0 1px 1px -1px rgba(0, 0, 0, 0.1), 0 1px 2px 0 rgba(0, 0, 0, 0.05));
  --ui-adc-shadow-hover: var(--aui-shadow-card-hover, 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 1px 1px -1px rgba(0, 0, 0, 0.12), 0 4px 10px -2px rgba(0, 0, 0, 0.08));
  --ui-adc-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-adc-ink: var(--aui-ink, rgba(0, 0, 0, 0.88));
  --ui-adc-strong: var(--aui-ink, #000);
  --ui-adc-text: var(--aui-muted, rgba(0, 0, 0, 0.61));
  --ui-adc-muted: var(--aui-muted, rgba(0, 0, 0, 0.45));
  --ui-adc-faint: var(--aui-placeholder, rgba(0, 0, 0, 0.27));
  --ui-adc-accent: var(--aui-info, #0070d6);
  --ui-adc-accent-soft: color-mix(in srgb, var(--aui-info, #0060e2) 10%, transparent);
  --ui-adc-field: var(--aui-subtle, rgba(0, 0, 0, 0.04));
  --ui-adc-wash: var(--aui-subtle, rgba(0, 0, 0, 0.04));
  --ui-adc-wash-hover: color-mix(in srgb, var(--aui-subtle, rgba(0, 0, 0, 0.04)) 70%, transparent);
  --ui-adc-empty: var(--aui-subtle, rgba(0, 0, 0, 0.025));
  --ui-adc-tile: var(--aui-subtle, #fff);
  --ui-adc-tile-filled-line: var(--aui-border, rgba(0, 0, 0, 0.06));
  --ui-adc-ok: var(--aui-success, #1e8a4f);
  --ui-adc-ok-bg: color-mix(in srgb, var(--aui-green, #5fc88a) 8%, transparent);
  --ui-adc-warn: var(--aui-warning, #b54708);
  --ui-adc-warn-bg: var(--aui-warning-soft, color-mix(in srgb, #f79009 12%, transparent));
  --ui-adc-ease: var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1));
  box-sizing: border-box;
  width: 100%;
  container-type: inline-size;
  font-family: var(--aui-font, "Inter", "InterVariable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif);
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  color: var(--ui-adc-ink);
  -webkit-font-smoothing: antialiased;
}
.ui-adc *, .ui-adc *::before, .ui-adc *::after { box-sizing: border-box; }
.ui-adc button { margin: 0; border: 0; font: inherit; color: inherit; cursor: pointer; }
.ui-adc button:focus-visible,
.ui-adc a:focus-visible { outline: 2px solid var(--ui-adc-focus); outline-offset: 2px; }

/* No tray: the block sits on the page, only the app cards are cards */
.ui-adc__card { padding: 8px 0 16px; }

.ui-adc__intro { display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; }
.ui-adc__intro-text { min-width: 0; }
.ui-adc__title {
  margin: 0;
  font-size: calc(15px * var(--type-scale, 1));
  font-weight: 600;
  line-height: 22px;
  letter-spacing: -0.006em;
  color: var(--ui-adc-strong);
  overflow-wrap: anywhere;
}
.ui-adc__subtitle { margin: 4px 0 0; font-size: calc(13px * var(--type-scale, 1)); line-height: 20px; color: var(--ui-adc-text); overflow-wrap: anywhere; }
.ui-adc__meta {
  flex: none;
  margin-top: 6px;
  font-size: calc(12px * var(--type-scale, 1));
  line-height: 20px;
  color: var(--ui-adc-muted);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.ui-adc .ui-adc__tools { display: flex; gap: 8px; margin-top: 24px; }
/* Steve: without a title the search is the first row */
.ui-adc .ui-adc__tools:first-child { margin-top: 0; }
.ui-adc__search {
  display: flex;
  flex: 1;
  min-width: 0;
  align-items: center;
  gap: 8px;
  height: 32px;
  padding: 0 10px;
  border-radius: calc(var(--aui-radius, 7px) + 1px);
  background: var(--ui-adc-field);
  color: var(--ui-adc-muted);
  transition: background-color 160ms ease, box-shadow 160ms ease;
}
/* Typing in a field: no ring – the caret shows where you are, the edge only firms up a little */
.ui-adc__search:focus-within { background: var(--ui-adc-card); box-shadow: var(--ui-adc-shadow-hover); }
.ui-adc__search svg { flex: none; }
.ui-adc__search input {
  flex: 1;
  min-width: 0;
  height: 100%;
  padding: 0;
  border: 0;
  outline: none;
  background: transparent;
  font: inherit;
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  color: var(--ui-adc-ink);
}
.ui-adc__search input::placeholder { color: var(--ui-adc-muted); }
.ui-adc__search input::-webkit-search-cancel-button { display: none; }

.ui-adc .ui-adc__btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: calc(var(--aui-radius, 7px) + 1px);
  background: var(--ui-adc-card);
  box-shadow: var(--ui-adc-shadow);
  font-size: calc(13px * var(--type-scale, 1)) !important;
  font-weight: 500;
  line-height: 20px;
  color: var(--ui-adc-ink) !important;
  white-space: nowrap;
  transition: background-color 160ms ease, box-shadow 160ms ease, color 160ms ease;
}
.ui-adc .ui-adc__btn:hover { box-shadow: var(--ui-adc-shadow-hover); }
.ui-adc .ui-adc__btn svg { flex: none; color: var(--ui-adc-muted); }
.ui-adc .ui-adc__btn[data-active] { background: var(--ui-adc-accent-soft); box-shadow: none; color: var(--ui-adc-accent) !important; }
.ui-adc .ui-adc__btn[data-active] svg { color: currentColor; }
.ui-adc__filter { position: relative; flex: none; }
.ui-adc__menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 5;
  display: grid;
  gap: 1px;
  min-width: 184px;
  padding: 4px;
  border-radius: var(--aui-radius-lg, 10px);
  background: var(--aui-overlay, var(--ui-adc-card));
  box-shadow: var(--aui-shadow-popover, 0 0 0 0.5px rgba(0, 0, 0, 0.08), 0 8px 24px -6px rgba(0, 0, 0, 0.14));
  animation: ui-adc-drop 180ms var(--ui-adc-ease);
}
.ui-adc .ui-adc__menu-item {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 28px;
  padding: 0 8px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  background: transparent;
  font-size: calc(13px * var(--type-scale, 1)) !important;
  line-height: 20px;
  text-align: left;
  color: var(--ui-adc-ink) !important;
}
.ui-adc .ui-adc__menu-item:hover { background: var(--ui-adc-wash); }
.ui-adc__menu-icon { display: grid; flex: none; color: var(--ui-adc-muted); }
.ui-adc__menu-label { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-adc .ui-adc__menu-item[aria-checked="true"] { font-weight: 500; }
.ui-adc .ui-adc__menu-item > svg { flex: none; color: var(--ui-adc-accent); }

.ui-adc .ui-adc__layout {
  display: grid;
  grid-template-columns: 184px minmax(0, 1fr);
  gap: 32px;
  margin-top: 24px;
}
.ui-adc__nav { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.ui-adc .ui-adc__nav-item {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 28px;
  padding: 0 8px;
  border-radius: calc(var(--aui-radius-sm, 5px) + 1px);
  background: transparent;
  font-size: calc(13px * var(--type-scale, 1)) !important;
  line-height: 20px;
  text-align: left;
  color: var(--ui-adc-text) !important;
  transition: background-color 140ms ease, color 140ms ease;
}
.ui-adc .ui-adc__nav-item:hover { background: var(--ui-adc-wash-hover); color: var(--ui-adc-ink) !important; }
.ui-adc .ui-adc__nav-item[aria-current] { background: var(--ui-adc-wash); color: var(--ui-adc-ink) !important; font-weight: 500; }
.ui-adc__nav-icon { display: grid; flex: none; color: var(--ui-adc-muted); }
.ui-adc__nav-item[aria-current] .ui-adc__nav-icon { color: var(--ui-adc-ink); }
.ui-adc__nav-label { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-adc__nav-count { flex: none; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; font-weight: 400; font-variant-numeric: tabular-nums; color: var(--ui-adc-faint); }

.ui-adc .ui-adc__heading {
  display: flex;
  align-items: baseline;
  gap: 6px;
  margin: 4px 0 12px;
  font-size: calc(13px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 20px;
  color: var(--ui-adc-strong);
}
.ui-adc__heading-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-adc__heading span:not(.ui-adc__heading-text) { flex: none; font-variant-numeric: tabular-nums; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; font-weight: 400; color: var(--ui-adc-muted); }

.ui-adc__grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
  margin: 0;
  padding: 0;
  list-style: none;
}
.ui-adc__app {
  display: flex;
  flex-direction: column;
  min-width: 0;
  padding: 16px;
  border-radius: var(--aui-radius-lg, 12px);
  background: var(--ui-adc-card);
  box-shadow: var(--aui-shadow-card, var(--ui-adc-shadow));
  animation: ui-adc-in 420ms var(--ui-adc-ease) both;
  transition: box-shadow 160ms ease;
}
.ui-adc__app:hover { box-shadow: var(--ui-adc-shadow-hover); }
.ui-adc__app-head { display: flex; align-items: center; gap: 10px; min-width: 0; }
.ui-adc__tile {
  display: grid;
  place-items: center;
  flex: none;
  width: 32px;
  height: 32px;
  border-radius: 8px;
  background: var(--ui-adc-tile);
  box-shadow: var(--ui-adc-shadow);
  color: var(--ui-adc-strong);
}
.ui-adc__tile[data-filled] { box-shadow: inset 0 0 0 0.5px var(--ui-adc-tile-filled-line); }
.ui-adc__tile svg { width: 17px; height: 17px; }
.ui-adc__tile-initial { font-size: calc(13px * var(--type-scale, 1)); font-weight: 500; line-height: 1; color: var(--ui-adc-text); }
.ui-adc__tile[data-filled] .ui-adc__tile-initial { color: #fff; }
.ui-adc__app-name { display: grid; flex: 1; min-width: 0; }
.ui-adc__name {
  display: inline-flex;
  min-width: 0;
  align-items: center;
  gap: 4px;
  font-size: calc(13px * var(--type-scale, 1));
  font-weight: 500;
  line-height: 20px;
  color: var(--ui-adc-strong);
}
.ui-adc__name-text { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.ui-adc__vendor { overflow: hidden; font-size: calc(12px * var(--type-scale, 1)); line-height: 16px; white-space: nowrap; text-overflow: ellipsis; color: var(--ui-adc-muted); }
/* Two lines at most – keeps every card the same height */
.ui-adc .ui-adc__desc {
  display: -webkit-box;
  flex: 1;
  margin: 12px 0 16px;
  overflow: hidden;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
  line-clamp: 2;
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  min-height: 40px;
  overflow-wrap: anywhere;
  color: var(--ui-adc-text);
}

.ui-adc .ui-adc__connect {
  display: inline-flex;
  align-items: center;
  overflow: hidden;
  white-space: nowrap;
  align-self: flex-start;
  height: 28px;
  padding: 0 10px;
  border-radius: calc(var(--aui-radius, 7px) - 1px);
  background: var(--ui-adc-card);
  box-shadow: var(--ui-adc-shadow);
  font-size: calc(13px * var(--type-scale, 1)) !important;
  font-weight: 500;
  line-height: 20px;
  color: var(--ui-adc-ink) !important;
  transition: background-color 160ms ease, color 160ms ease, box-shadow 160ms ease;
}
.ui-adc__connect-label { display: inline-flex; flex: none; align-items: center; gap: 6px; }
.ui-adc .ui-adc__connect:hover { box-shadow: var(--ui-adc-shadow-hover); }
/* Connected reads as a small tinted badge; needing attention, a warning-toned Manage */
.ui-adc .ui-adc__connect[data-state="on"] { background: var(--ui-adc-ok-bg); box-shadow: none; color: var(--ui-adc-ok) !important; }
.ui-adc .ui-adc__connect[data-state="attention"] { background: var(--ui-adc-warn-bg); box-shadow: none; color: var(--ui-adc-warn) !important; }
.ui-adc__off-label { display: none; }
/* Steve: hovering a connected card offers to manage it – its page – never a one-click disconnect */
.ui-adc .ui-adc__app:is(:hover, :focus-within) .ui-adc__connect[data-state="on"] { background: var(--ui-adc-wash); color: var(--ui-adc-ink) !important; }
.ui-adc__app:is(:hover, :focus-within) .ui-adc__connect[data-state="on"] .ui-adc__on-label { display: none; }
.ui-adc__app:is(:hover, :focus-within) .ui-adc__connect[data-state="on"] .ui-adc__off-label { display: inline; }
.ui-adc__app[data-clickable] { cursor: pointer; }
.ui-adc__vendor[data-attention] { color: var(--ui-adc-warn); }
.ui-adc__empty {
  display: grid;
  justify-items: center;
  gap: 12px;
  padding: 48px 16px;
  border-radius: calc(var(--aui-radius-lg, 10px) + 2px);
  background: var(--ui-adc-empty);
  font-size: calc(13px * var(--type-scale, 1));
  line-height: 20px;
  text-align: center;
  color: var(--ui-adc-muted);
}
.ui-adc__empty p { margin: 0; }

/* Narrower containers: categories turn into a row of chips, the grid gets fewer columns */
@container (max-width: 820px) {
  .ui-adc .ui-adc__layout { grid-template-columns: minmax(0, 1fr); gap: 16px; }
  .ui-adc__nav { flex-direction: row; overflow-x: auto; scrollbar-width: none; margin: -4px; padding: 4px; }
  .ui-adc .ui-adc__nav-item { flex: none; max-width: 240px; }
  .ui-adc__nav-count { display: none; }
  .ui-adc__grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
@container (max-width: 560px) {
  .ui-adc__intro { flex-direction: column; gap: 4px; }
  .ui-adc__meta { margin-top: 0; }
  .ui-adc__grid { grid-template-columns: minmax(0, 1fr); }
}

@keyframes ui-adc-in { from { opacity: 0; transform: translateY(4px); } }
@keyframes ui-adc-drop { from { opacity: 0; transform: translateY(-4px) scale(0.98); } }

/* Height changes glide instead of jumping */
.ui-adc__morph { overflow: hidden; margin: -6px; }
/* Room inside the clipping, so card shadows aren't cut */
.ui-adc__main { padding: 6px; }
.ui-adc__morph[data-smooth] { transition: height 420ms var(--aui-ease, cubic-bezier(0.22, 1, 0.36, 1)); }

@media (pointer: coarse) {
  .ui-adc__search { height: 36px; }
  .ui-adc .ui-adc__btn { height: 36px; }
  .ui-adc .ui-adc__nav-item { height: 36px; }
  .ui-adc .ui-adc__menu-item { height: 40px; }
  .ui-adc .ui-adc__connect { height: 36px; }
  .ui-adc__search input { font-size: calc(16px * var(--type-scale, 1)); }
}

@media (prefers-reduced-motion: reduce) {
  .ui-adc__app, .ui-adc__menu { animation: none; }
  .ui-adc__morph[data-smooth] { transition: none; }
}

/* Text fields: no focus box while typing – also against global :focus-visible rules of the host page */
.ui-adc input:not([type="checkbox"]):not([type="radio"]):focus-visible,
.ui-adc textarea:focus-visible,
.ui-adc [contenteditable]:focus-visible { outline: none; }

/* Dark mode: follows a .dark class or data-theme="dark" on any ancestor, e.g. <html> */
:where(.dark, [data-theme="dark"]) .ui-adc {
  --ui-adc-focus: var(--aui-focus, rgba(77, 141, 255, 0.8));
  --ui-adc-card: var(--aui-surface, #1c1c1e);
  --ui-adc-shadow: var(--aui-shadow-card, 0 0 0 0.5px rgba(255, 255, 255, 0.08), 0 1px 2px 0 rgba(0, 0, 0, 0.4));
  --ui-adc-shadow-hover: var(--aui-shadow-card-hover, 0 0 0 0.5px rgba(255, 255, 255, 0.12), 0 4px 12px -2px rgba(0, 0, 0, 0.5));
  --ui-adc-line: var(--aui-border, rgba(255, 255, 255, 0.12));
  --ui-adc-ink: var(--aui-ink, rgba(255, 255, 255, 0.88));
  --ui-adc-strong: var(--aui-ink, #fff);
  --ui-adc-text: var(--aui-muted, rgba(255, 255, 255, 0.61));
  --ui-adc-muted: var(--aui-muted, rgba(255, 255, 255, 0.45));
  --ui-adc-faint: var(--aui-placeholder, rgba(255, 255, 255, 0.27));
  --ui-adc-accent: var(--aui-info, #4fb0fe);
  --ui-adc-accent-soft: color-mix(in srgb, var(--aui-blue, #4fb0fe) 14%, transparent);
  --ui-adc-field: var(--aui-subtle, rgba(255, 255, 255, 0.06));
  --ui-adc-wash: var(--aui-subtle, rgba(255, 255, 255, 0.06));
  --ui-adc-empty: var(--aui-subtle, rgba(255, 255, 255, 0.03));
  --ui-adc-tile: var(--aui-subtle, #2a2a2d);
  --ui-adc-ok: var(--aui-green, #5fc88a);
  --ui-adc-ok-bg: color-mix(in srgb, var(--aui-green, #7dcf9f) 12%, transparent);
}
`;
