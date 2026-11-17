/* The devices pages' one reading of the Hub's inventory: the device rows (/api/devices: limits, accepting work, the
 * Hub's own Node) and the Node registry (/api/nodes: pairing, link, upgrade) merged into one NodeView per Node, the
 * one status a Node shows, and the figures the pages print. Pure, so the overview, the lists, a Node's page and the
 * map all say the same thing; web/test/device-model.test.mjs pins it. Imports use .ts so node --test loads it. */
import type { RDevice, RNode, RVitals } from "./api.ts"
import { t } from "./i18n/index.ts"

const MIN = 60_000, DAY = 86_400_000

/** the Hub's own Node, as /api/nodes and the device rows name it */
export const LOCAL_NODE = "local"
/** a load, memory or disk fill at or above this turns amber */
export const FILL_WARN = 0.75
/** … and at or above this, red */
export const FILL_BAD = 0.9
/** a client not seen for longer than this is dimmed and listed under "not used lately" */
export const CLIENT_STALE_MS = 7 * DAY
/** … and longer than this is flagged "unused for 30+ days" (a token likewise) */
export const CLIENT_UNUSED_MS = 30 * DAY

/** the upgrade phases after which nothing more happens on its own */
const SETTLED = new Set(["succeeded", "failed", "rolled_back", "abandoned", "unknown"])
const FAILED = new Set(["failed", "rolled_back", "unknown"])

export type NodeView = {
  /** the Node's id (`local` for the Hub's own); a device row the Hub keeps without a Node, by its device id */
  id: string
  name: string
  local: boolean
  /** a device row that names no Node (an inventory entry from before Nodes were paired) */
  legacy: boolean
  device?: RDevice
  node?: RNode
  state: RNode["state"]
  linked: boolean
  paused: boolean
  running?: number
  waiting?: number
  max?: number
  lastSeen?: number | null
  linkError?: { at: number; error: string } | null
  /** the release it runs, when it said */
  version?: string | null
  fingerprint?: string
}

/** One NodeView per Node: the Hub's own first, then the paired ones as the registry lists them, then device rows
 *  that name no Node. A device row is joined to a Node only by the node_id it declares, never by name or role. */
export function nodeViews(devices: readonly RDevice[], nodes: readonly RNode[] | null, hubVersion = ""): NodeView[] {
  const out: NodeView[] = []
  const localDevices = devices.filter((d) => d.node_id === LOCAL_NODE)
  const localDevice = localDevices.length === 1 ? localDevices[0] : undefined
  const localNode = nodes?.find((n) => n.local)
  if (localDevice || localNode) {
    out.push({
      id: LOCAL_NODE, name: localDevice?.name ?? localNode!.name, local: true, legacy: false, device: localDevice, node: localNode,
      state: localNode?.state ?? "active", linked: localNode ? localNode.linked : localDevice?.connected !== false,
      paused: localNode ? localNode.state === "paused" : localDevice?.status === "暂停接活",
      running: localDevice?.running ?? localNode?.running, waiting: localDevice?.waiting ?? localNode?.waiting,
      max: localDevice?.max_sessions ?? localNode?.max_sessions, lastSeen: localNode?.last_seen, linkError: localNode?.link_error ?? null,
      version: localNode?.upgrade?.build?.version ?? (hubVersion || localDevice?.version || null), fingerprint: localNode?.fingerprint,
    })
  }
  const seen = new Set<string>([LOCAL_NODE])
  for (const n of nodes ?? []) {
    if (n.local || seen.has(n.node_id)) continue
    seen.add(n.node_id)
    const device = devices.find((d) => d.node_id === n.node_id)
    out.push({
      id: n.node_id, name: n.name, local: false, legacy: false, device, node: n, state: n.state, linked: n.linked, paused: n.state === "paused",
      running: n.running, waiting: n.waiting, max: n.max_sessions, lastSeen: n.last_seen, linkError: n.link_error ?? null,
      version: n.upgrade?.build?.version ?? null, fingerprint: n.fingerprint,
    })
  }
  for (const d of devices) {
    if (d.roles.includes("Hub") || d.node_id === LOCAL_NODE || (d.node_id && (seen.has(d.node_id) || nodes === null))) continue
    out.push({
      id: d.id, name: d.name, local: false, legacy: true, device: d, state: "active", linked: d.connected === true, paused: d.status === "暂停接活",
      running: d.running, waiting: d.waiting, max: d.max_sessions, version: d.version || null,
    })
  }
  return out
}

/** the Node a route or an old link names: by Node id, else by the device id of its row */
export function findNode(views: readonly NodeView[], id: string): NodeView | undefined {
  return views.find((v) => v.id === id) ?? views.find((v) => v.device?.id === id)
}

export type NodeStatusKind = "revoked" | "pending" | "offline" | "upgrading" | "rolledBack" | "paused" | "outdated" | "queued" | "online"
/** ok: nothing to say · warn: wants you · bad: went wrong · info: under way · off: not there · neutral: by your choice */
export type StatusTone = "ok" | "warn" | "bad" | "info" | "off" | "neutral"
export type NodeStatus = { kind: NodeStatusKind; tone: StatusTone; since?: number | null; phase?: string; version?: string; from?: string | null; n?: number }

/** The one state a Node shows, the first that holds in this order: unpaired, fingerprint to confirm, offline,
 *  upgrading, upgrade failed (and back on its old version), not taking new work, can upgrade, queued, online. */
export function nodeStatus(n: NodeView, hubVersion = ""): NodeStatus {
  if (n.state === "revoked") return { kind: "revoked", tone: "off" }
  if (n.state === "pending") return { kind: "pending", tone: "warn" }
  if (!n.linked) return { kind: "offline", tone: "off", since: n.lastSeen ?? n.linkError?.at ?? null }
  const u = n.node?.upgrade, latest = u?.latest
  if (latest && !SETTLED.has(latest.phase)) return { kind: "upgrading", tone: "info", phase: latest.phase, version: latest.version }
  if (latest && FAILED.has(latest.phase) && n.version !== latest.version) return { kind: "rolledBack", tone: "bad", version: latest.version, from: latest.from_version ?? n.version ?? null }
  if (n.paused) return { kind: "paused", tone: "neutral" }
  const target = u?.reason === "outdated" ? hubVersion || undefined : undefined
  if (u?.reason === "outdated" || (!u && !n.local && n.version && hubVersion && n.version !== hubVersion)) return { kind: "outdated", tone: "warn", version: target ?? hubVersion }
  if ((n.waiting ?? 0) > 0) return { kind: "queued", tone: "warn", n: n.waiting }
  return { kind: "online", tone: "ok" }
}

/** the status as words, for a badge (online says nothing more than its dot) */
export function statusText(s: NodeStatus, now = Date.now()): string {
  switch (s.kind) {
    case "offline": return s.since ? t("devices:status.offline", { ago: agoText(s.since, now) }) : t("devices:status.offlineNever")
    case "upgrading": return t("devices:status.upgrading", { phase: t(`device:upgrade.phase.${s.phase as "draining"}`, { defaultValue: s.phase }) })
    case "outdated": return s.version ? t("devices:status.outdated", { version: s.version }) : t("devices:status.outdatedUnknown")
    case "queued": return t("devices:status.queued", { n: s.n ?? 0 })
    default: return t(`devices:status.${s.kind}`)
  }
}

/** the tone a fill (0–1) is drawn in: amber from FILL_WARN, red from FILL_BAD */
export const fillTone = (x: number | null | undefined): "wait" | "bad" | undefined => (x == null ? undefined : x >= FILL_BAD ? "bad" : x >= FILL_WARN ? "wait" : undefined)

const GIB = 2 ** 30
/** CPU load: the 1-minute load average over the logical cores, as a whole percentage */
export function loadPct(l: RVitals["latest"] | null | undefined): number | null {
  return l?.load && l.cpus && l.cpus > 0 && Number.isFinite(l.load[0]) ? Math.round((l.load[0] / l.cpus) * 100) : null
}
/** memory in use, as a whole percentage */
export function memPct(l: RVitals["latest"] | null | undefined): number | null {
  return l?.mem_total && l.mem_available != null && l.mem_available >= 0 && l.mem_available <= l.mem_total ? Math.round((1 - l.mem_available / l.mem_total) * 100) : null
}
/** whole GB, as the pages print sizes */
export const gbOf = (bytes: number) => Math.round(bytes / GIB)
/** a number with one decimal at most, for a load: 5.88623046875 → 5.9 */
export const one = (x: number) => Math.round(x * 10) / 10
/** the project roots it reported, fullest first: free and total in GB and the fill */
export function disks(l: RVitals["latest"] | null | undefined) {
  return (l?.roots ?? []).filter((r) => r.total > 0 && r.free >= 0 && r.free <= r.total)
    .map((r) => ({ root: r.root, name: r.display || r.root, free: gbOf(r.free), total: gbOf(r.total), used: 1 - r.free / r.total }))
    .sort((a, b) => b.used - a.used)
}

/** "刚刚", "N 分钟前", "N 小时前", "N 天前": 45 seconds and less is now */
export function agoText(ts: number, now = Date.now()): string {
  const d = Math.max(0, now - ts)
  if (d < 45_000) return t("common:time.justNow")
  const m = Math.max(1, Math.round(d / MIN))
  if (m < 60) return t("common:time.minutesAgo", { count: m })
  if (m < 1440) return t("common:time.hoursAgo", { count: Math.round(m / 60) })
  return t("common:time.daysAgo", { count: Math.round(m / 1440) })
}

export type RouteKey = "local" | "lan" | "tailnet" | "tailcat" | "https" | "public" | "proxy"
const ROUTES: Record<string, RouteKey> = { loopback: "local", local: "local", lan: "lan", tailnet: "tailnet", tailcat: "tailcat", https: "https", public: "public", proxy: "proxy" }
/** a route as the Hub records it (`loopback`, `lan`, `tailnet`, `proxy`, `public`, with `+tls`), read */
export function routeOf(r: string | null | undefined): { key: RouteKey | null; raw: string; tls: boolean } {
  const [base = "", tls] = (r ?? "").split("+")
  return { key: ROUTES[base] ?? null, raw: base, tls: tls === "tls" }
}
/** the same as words: "局域网 · TLS"; an unknown route as it came, none as "unknown" */
export function routeLabel(r: string | null | undefined): string {
  const { key, raw, tls } = routeOf(r)
  if (!raw) return t("devices:route.unknown")
  return (key ? t(`devices:route.${key}`) : raw) + (tls ? " · TLS" : "")
}

export type ClientAge = "fresh" | "stale" | "unused"
/** how lately a client (or a token) was used: fresh, not used for CLIENT_STALE_MS, or for CLIENT_UNUSED_MS */
export function ageOf(last: number | null | undefined, created: number, now = Date.now()): ClientAge {
  const idle = now - (last ?? created)
  return idle > CLIENT_UNUSED_MS ? "unused" : idle > CLIENT_STALE_MS ? "stale" : "fresh"
}

export type ClientKind = "browser" | "phone" | "mac" | "cli" | "other"
/** what a client is, for its icon and its words: the kind it signed in as */
export function clientKind(kind: string, platform = ""): ClientKind {
  if (kind === "web") return /ios|iphone|ipad|android/i.test(platform) ? "phone" : "browser"
  if (kind === "ios" || kind === "android") return "phone"
  if (kind === "mac") return "mac"
  if (kind === "cli") return "cli"
  return "other"
}
