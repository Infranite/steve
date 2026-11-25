/* Single source of truth for navigation IA: sidebar groups, item ordering, mobile tabs, and route-to-item mapping
 * shared by the shell, mobile navigation, navigation usage telemetry and the Stats page. */

export const NAV_GROUPS = ["primary", "schedule", "messages", "library", "automate", "more"] as const
export type NavGroup = (typeof NAV_GROUPS)[number]

/** Desktop sidebar scrollable groups; `more` (Stats) stays pinned in the footer so it is always visible. */
export const SIDEBAR_SCROLL_GROUPS = ["primary", "schedule", "messages", "library", "automate"] as const

export type SidebarNavKey =
  | "chat" | "inbox" | "items" | "projects"
  | "calendar" | "todos"
  | "feed" | "channels" | "mail" | "contacts"
  | "knowledge" | "integrations"
  | "bots" | "sessions" | "auto" | "capabilities" | "signals"
  | "stats"

export type ExtraNavKey = "me" | "devices" | "accounts" | "vault" | "settings" | "publications" | "github"
export type NavItemKey = SidebarNavKey | ExtraNavKey

export type SidebarNavEntry = { h: string; l: SidebarNavKey; group: NavGroup }

/** Sidebar entries ordered by human frequency: top unlabelled workflow group, then short task-oriented groups
 *  with each group's highest-frequency destination first, and low-frequency Stats under More at the bottom. */
export const SIDEBAR_NAV: readonly SidebarNavEntry[] = [
  { h: "", l: "chat", group: "primary" },
  { h: "inbox", l: "inbox", group: "primary" },
  { h: "items", l: "items", group: "primary" },
  { h: "projects", l: "projects", group: "primary" },
  { h: "calendar", l: "calendar", group: "schedule" },
  { h: "todos", l: "todos", group: "schedule" },
  { h: "feed", l: "feed", group: "messages" },
  { h: "channels", l: "channels", group: "messages" },
  { h: "mail", l: "mail", group: "messages" },
  { h: "contacts", l: "contacts", group: "messages" },
  { h: "knowledge", l: "knowledge", group: "library" },
  { h: "integrations", l: "integrations", group: "library" },
  { h: "bots", l: "bots", group: "automate" },
  { h: "sessions", l: "sessions", group: "automate" },
  { h: "auto", l: "auto", group: "automate" },
  { h: "capabilities", l: "capabilities", group: "automate" },
  { h: "signals", l: "signals", group: "automate" },
  { h: "stats", l: "stats", group: "more" },
] as const

export const SIDEBAR_FOOTER_NAV: readonly SidebarNavEntry[] = SIDEBAR_NAV.filter((x) => x.group === "more")

/** Phone bottom tab bar: top workflow entries first, then Schedule, keeping 事项 and 我的待办 separated. */
export const MOBILE_TAB_KEYS: readonly SidebarNavKey[] = ["chat", "inbox", "items", "calendar", "todos"] as const

const EXTRA_HREF: Record<ExtraNavKey, string> = {
  me: "contacts/me",
  devices: "devices",
  accounts: "accounts",
  vault: "vault",
  settings: "settings",
  publications: "publications",
  github: "github",
}

const SIDEBAR_BY_KEY = new Map<string, SidebarNavEntry>(SIDEBAR_NAV.map((e) => [e.l, e]))

/** Resolves any location hash (e.g. `#/items/ST-42`, `#/contacts/me`, `#/todos?view=goals`) to its bounded nav key. */
export function navItemOf(hash: string): NavItemKey | null {
  const clean = hash.replace(/^#\/?/, "").split("?")[0]
  const [a = "", b = ""] = clean.split("/")
  if (!a || a === "c") return "chat"
  if (a === "contacts") return b === "me" ? "me" : "contacts"
  if (a === "orgs" || a === "teams") return "contacts"
  if (SIDEBAR_BY_KEY.has(a)) return a as SidebarNavKey
  if (Object.prototype.hasOwnProperty.call(EXTRA_HREF, a)) return a as ExtraNavKey
  return null
}

/** Route hash and group for any tracked nav item key (for Stats navigation usage display). */
export function navMetaOf(item: string): { href: string; group: NavGroup | "account" } | null {
  const entry = SIDEBAR_BY_KEY.get(item)
  if (entry) return { href: "#/" + entry.h, group: entry.group }
  if (Object.prototype.hasOwnProperty.call(EXTRA_HREF, item)) {
    return { href: "#/" + EXTRA_HREF[item as ExtraNavKey], group: "account" }
  }
  return null
}
