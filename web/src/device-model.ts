/* The devices pages' one reading of the Hub's inventory: the device rows (/api/devices: limits, accepting work, the
 * Hub's own Node) and the Node registry (/api/nodes: pairing, link, upgrade) merged into one NodeView per Node, the
 * one status a Node shows, and the figures the pages print. Pure, so the overview, the lists, a Node's page and the
 * map all say the same thing; web/test/device-model.test.mjs pins it. Imports use .ts so node --test loads it. */
import type { RDevice, RNode, RNodeUpgradeRun, RVitals } from "./api.ts"
import { t } from "./i18n/index.ts"
import { longDur } from "./i18n/format.ts"

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
  /** the Node's id (`local` for the Hub's own) */
  id: string
  name: string
  local: boolean
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

/** One NodeView per Node: the Hub's own first, then the paired ones as the registry lists them. A device row is
 *  joined to a Node only by the node_id it declares, never by name or role. */
export function nodeViews(devices: readonly RDevice[], nodes: readonly RNode[] | null, hubVersion = ""): NodeView[] {
  const out: NodeView[] = []
  const localDevices = devices.filter((d) => d.node_id === LOCAL_NODE)
  const localDevice = localDevices.length === 1 ? localDevices[0] : undefined
  const localNode = nodes?.find((n) => n.local)
  if (localDevice || localNode) {
    out.push({
      id: LOCAL_NODE, name: localDevice?.name ?? localNode!.name, local: true, device: localDevice, node: localNode,
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
      id: n.node_id, name: n.name, local: false, device, node: n, state: n.state, linked: n.linked, paused: n.state === "paused",
      running: n.running, waiting: n.waiting, max: n.max_sessions, lastSeen: n.last_seen, linkError: n.link_error ?? null,
      version: n.upgrade?.build?.version ?? null, fingerprint: n.fingerprint,
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
export type NodeStatus = { kind: NodeStatusKind; tone: StatusTone; since?: number | null; reason?: NonNullable<RNode["offline"]>["reason"]; phase?: string; version?: string; from?: string | null; n?: number }

/** A failed run only describes the Node while it is still the current attempt: the Node is on the version it fell
 *  back to (a hand reinstall moves it on) and the run aimed at the Hub's release (once the Hub moves on, the Node is
 *  simply behind again). An unknown Hub version (development build) does not retire it. */
function stillRolledBack(n: NodeView, latest: RNodeUpgradeRun, hubVersion: string) {
  return FAILED.has(latest.phase) && n.version !== latest.version && n.version === (latest.from_version ?? n.version) && (!hubVersion || latest.version === hubVersion)
}

/** The one state a Node shows, the first that holds in this order: unpaired, fingerprint to confirm, offline,
 *  upgrading, upgrade failed (and back on its old version), not taking new work, can upgrade, queued, online. */
export function nodeStatus(n: NodeView, hubVersion = ""): NodeStatus {
  if (n.state === "revoked") return { kind: "revoked", tone: "off" }
  if (n.state === "pending") return { kind: "pending", tone: "warn" }
  if (!n.linked) return { kind: "offline", tone: "off", since: n.node?.offline?.since ?? n.lastSeen ?? n.linkError?.at ?? null, reason: n.node?.offline?.reason }
  const u = n.node?.upgrade, latest = u?.latest
  if (latest && !SETTLED.has(latest.phase)) return { kind: "upgrading", tone: "info", phase: latest.phase, version: latest.version }
  if (latest && stillRolledBack(n, latest, hubVersion)) return { kind: "rolledBack", tone: "bad", version: latest.version, from: latest.from_version ?? n.version ?? null }
  if (n.paused) return { kind: "paused", tone: "neutral" }
  const target = u?.reason === "outdated" ? hubVersion || undefined : undefined
  if (u?.reason === "outdated" || (!u && !n.local && n.version && hubVersion && n.version !== hubVersion)) return { kind: "outdated", tone: "warn", version: target ?? hubVersion }
  if ((n.waiting ?? 0) > 0) return { kind: "queued", tone: "warn", n: n.waiting }
  return { kind: "online", tone: "ok" }
}

/** the status as words, for a badge (online says nothing more than its dot) */
export function statusText(s: NodeStatus, now = Date.now()): string {
  switch (s.kind) {
    case "offline":
      if (s.reason && s.since != null) return t("devices:status.offlineFor", { time: longDur(Math.max(0, now - s.since)) })
      return s.since ? t("devices:status.offline", { ago: agoText(s.since, now) }) : t("devices:status.offlineNever")
    case "upgrading": return t("devices:status.upgrading", { phase: t(`device:upgrade.phase.${s.phase as "draining"}`, { defaultValue: s.phase }) })
    case "outdated": return s.version ? t("devices:status.outdated", { version: s.version }) : t("devices:status.outdatedUnknown")
    case "queued": return t("devices:status.queued", { n: s.n ?? 0 })
    default: return t(`devices:status.${s.kind}`)
  }
}

/** Translate the Hub's code; old Hubs only include optional raw diagnostics. */
export function offlineReason(v: NodeView): string {
  if (v.linked) return ""
  const code = v.node?.offline?.reason
  if (code) return t(`devices:offlineReason.${code}`, { defaultValue: t("devices:offlineReason.unreachable") })
  return v.linkError?.error.split(":")[0] ?? ""
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

/** Rust's names of a system and an architecture (`node.host`), as people say them */
const OS_NAMES: Record<string, string> = { macos: "macOS", linux: "Linux", windows: "Windows", freebsd: "FreeBSD" }
const ARCH_NAMES: Record<string, string> = { aarch64: "arm64" }
export type HostKind = "mac" | "linux" | "windows" | "other"
export type HostFacts = { os: string | null; kind: HostKind; arch: string | null; cpus: number | null; memGb: number | null; hostname: string | null }
/** What a Node's machine is: the host it reported (a remote Node, newer releases), else the Hub's device row for its own
 *  machine; the cores and memory from the host, else from its last sample. Nothing is guessed from a name. */
export function hostOf(v: NodeView, l?: RVitals["latest"] | null): HostFacts {
  const h = v.node?.host
  let os: string | null = null
  if (h) {
    const base = h.os_name || OS_NAMES[h.os] || h.os
    os = h.os_version && !base.includes(h.os_version) ? `${base} ${h.os_version}` : base
  } else if (v.local && v.device?.os) os = v.device.os
  const said = `${h?.os ?? ""} ${os ?? ""}`
  const kind: HostKind = /mac|darwin/i.test(said) ? "mac" : /linux|ubuntu|debian|fedora|centos|alpine|arch/i.test(said) ? "linux" : /windows/i.test(said) ? "windows" : "other"
  const mem = h?.mem_total ?? l?.mem_total
  return { os, kind, arch: h?.arch ? ARCH_NAMES[h.arch] ?? h.arch : null, cpus: h?.cpus ?? l?.cpus ?? null, memGb: mem ? gbOf(mem) : null, hostname: h?.hostname ?? null }
}
/** "macOS 26.1 · arm64 · 12 核 · 36 GB": what is known, in that order (a size the system line already names is not repeated) */
export function hostLine(f: HostFacts): string {
  const gb = f.memGb ? t("devices:vitals.gb", { n: f.memGb }) : ""
  return [f.os, f.arch, f.cpus ? t("devices:hub.cores", { n: f.cpus }) : "", gb && !f.os?.includes(gb) ? gb : ""].filter(Boolean).join(" · ")
}

/** the route of the link a Node came in on, as the devices pages name it: the Hub's own is on this machine; tailcat and a
 *  proxy by what they came through; otherwise where its machine is, a public one by whether it is HTTPS. Unknown: null. */
export function linkRoute(v: NodeView): RouteKey | null {
  if (v.local) return "local"
  const l = v.node?.link
  if (!l) return null
  if (l.via === "tailcat") return "tailcat"
  if (l.via === "proxy") return "proxy"
  return ({ loopback: "local", lan: "lan", tailnet: "tailnet", public: l.tls ? "https" : "public" } as const)[l.route as "lan"] ?? null
}
/** the latest round trip to a remote Node in ms: its link's last call, else its last sample (none for the Hub's own) */
export function rttOf(v: NodeView, l?: RVitals["latest"] | null): number | null {
  if (v.local) return null
  const ms = v.node?.link?.latency_ms ?? l?.rtt_ms
  return ms == null || !Number.isFinite(ms) ? null : Math.round(ms)
}

/** the five steps of a Hub-managed upgrade, as the version card draws them */
export const UPGRADE_STEPS = ["preparing", "staging", "draining", "switching", "linking"] as const
const STEP_OF: Record<string, number> = { preparing: 0, staging: 1, staged: 2, draining: 2, switching: 3 }
export type StepState = "done" | "active" | "todo" | "bad" | "back"
export type UpgradeCard =
  | { kind: "builtIn" } | { kind: "none" } | { kind: "devHub" } | { kind: "unknown" }
  | { kind: "running"; run: RNodeUpgradeRun; steps: StepState[] }
  | { kind: "offline"; last: string | null }
  | { kind: "failed"; run: RNodeUpgradeRun; steps: StepState[] | null }
  | { kind: "outdated"; from: string | null; to: string }
  | { kind: "manual"; from: string | null; to: string; behind: boolean }
  | { kind: "current"; last: RNodeUpgradeRun | null }

/** What a Node's version card says, one of: built into the Hub; an upgrade under way (and its step); offline (its last
 *  version); the last upgrade failed and it went back; behind and upgradable from here; upgraded only on its own machine;
 *  on the Hub's build; or nothing to say (a development Hub, not asked yet). A failed run reads as in
 *  nodeStatus. Only a rollback has a known failed step (the switch); for another failure no step is claimed. */
export function upgradeCard(v: NodeView, hubVersion = ""): UpgradeCard {
  if (v.local) return { kind: "builtIn" }
  const u = v.node?.upgrade
  if (!u) return { kind: "none" }
  const latest = u.latest ?? null
  if (latest && !SETTLED.has(latest.phase)) {
    const at = STEP_OF[latest.phase] ?? 0
    return { kind: "running", run: latest, steps: UPGRADE_STEPS.map((_, i) => (i < at ? "done" : i === at ? "active" : "todo")) }
  }
  if (!v.linked) return { kind: "offline", last: v.version ?? null }
  if (latest && stillRolledBack(v, latest, hubVersion)) return { kind: "failed", run: latest, steps: latest.phase === "rolled_back" ? ["done", "done", "done", "bad", "back"] : null }
  const to = hubVersion || u.build?.version || ""
  if (u.reason === "outdated" && u.can) return { kind: "outdated", from: v.version ?? null, to }
  if (u.reason === "manual") return { kind: "manual", from: v.version ?? null, to, behind: !!v.version && !!hubVersion && v.version !== hubVersion }
  if (u.reason === "current") return { kind: "current", last: latest?.phase === "succeeded" ? latest : null }
  return { kind: u.reason === "hub_dev_build" ? "devHub" : "unknown" }
}

export type NodeAlert = { tone: "bad" | "warn"; kind: "rolledBack" | "manual" | "outdated" | "pending" | "offline"; text: string }
/** At most two lines a Node's card leads with, the one that must be dealt with first: what went wrong (an upgrade that
 *  went back; a Node too far behind to upgrade from here), then what wants a step (behind and upgradable, a
 *  fingerprint to confirm, offline). An upgrade under way is the status badge's to say. Nothing to say: none. */
export function nodeAlerts(v: NodeView, hubVersion = "", now = Date.now()): NodeAlert[] {
  const out: NodeAlert[] = []
  const s = nodeStatus(v, hubVersion), card = upgradeCard(v, hubVersion)
  if (s.kind === "rolledBack") out.push({ tone: "bad", kind: "rolledBack", text: t("devices:upgrade.failedTitle", { to: s.version ?? "", from: s.from ?? "" }) })
  if (card.kind === "manual" && card.behind) out.push({ tone: "bad", kind: "manual", text: t("devices:card.manual", { v: card.from ?? "", hub: card.to }) })
  if (s.kind === "pending") out.push({ tone: "warn", kind: "pending", text: t("devices:card.pending") })
  if (s.kind === "offline") out.push({ tone: "warn", kind: "offline", text: s.reason ? offlineReason(v) : v.linkError ? t("devices:card.offlineWhy", { ago: agoText(v.linkError.at, now), why: v.linkError.error }) : t("devices:node.offlineNote") })
  if (card.kind === "outdated") out.push({ tone: "warn", kind: "outdated", text: t("devices:card.outdated", { v: card.from ?? "?", hub: card.to }) })
  return out.slice(0, 2)
}

/** a fingerprint is read in groups of this many hex digits, as `steve-node` prints it */
export const FP_GROUP = 4
/** the hex digits of a fingerprint as typed or pasted: case, spaces, dashes and the words around it fall away */
export const fpDigits = (text: string) => text.toLowerCase().replace(/fingerprint|sha256:/g, "").replace(/[^0-9a-f]/g, "")
/** a fingerprint in its groups of four */
export const fpGroups = (fp: string) => fpDigits(fp).match(new RegExp(`.{1,${FP_GROUP}}`, "g")) ?? []
/** How the groups typed so far compare with the fingerprint the Hub received: each group `match`, `wrong` (complete and
 *  different) or `open`; `all` only when every group of the whole fingerprint matches. */
export function fpCheck(typed: readonly string[], fp: string): { groups: ("match" | "wrong" | "open")[]; all: boolean; wrong: number } {
  const want = fpGroups(fp)
  const groups = want.map((g, i) => { const got = fpDigits(typed[i] ?? ""); return got.length < g.length ? "open" as const : got === g ? "match" as const : "wrong" as const })
  return { groups, all: want.length > 0 && groups.every((g) => g === "match"), wrong: groups.filter((g) => g === "wrong").length }
}
/** what was pasted, spread over the groups from group `from` on (a whole fingerprint always from the first) */
export function fpSpread(typed: readonly string[], text: string, from: number, groups: number): string[] {
  const d = fpDigits(text), out = Array.from({ length: groups }, (_, i) => typed[i] ?? "")
  const start = d.length >= groups * FP_GROUP ? 0 : from
  for (let i = 0; i * FP_GROUP < d.length && start + i < groups; i++) out[start + i] = d.slice(i * FP_GROUP, (i + 1) * FP_GROUP)
  return out
}
