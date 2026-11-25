/* Pure view model for a single Node's tabbed page (#/devices/nodes/<id>[/<tab>]).
 * Keeps URL parsing, prioritized "needs your attention" items, per-tab badge counts,
 * and sanitized diagnostic snapshots in one testable module (node --test loads .ts directly). */
import type { RNodeHarnessReport, RVitals } from "./api.ts"
import { hostOf, linkRoute, nodeStatus, offlineReason, upgradeCard, type NodeView } from "./device-model.ts"
import { availableUpgradableHarnesses, type HarnessUpdates } from "./harness-update-status.ts"
import { harnessState } from "./node-harness-model.ts"

export const NODE_TABS = ["overview", "harness", "work", "access", "version", "diagnostics"] as const
export type NodeTab = (typeof NODE_TABS)[number]

/** Valid tab segment from the URL hash; unknown or missing falls back to overview. */
export function parseNodeTab(segment?: string | null): NodeTab {
  return segment && (NODE_TABS as readonly string[]).includes(segment) ? (segment as NodeTab) : "overview"
}

/** Canonical link to a Node's page, optionally targeting a specific tab. */
export function nodeHref(id: string, tab: NodeTab = "overview"): string {
  const base = "#/devices/nodes/" + encodeURIComponent(id)
  return tab === "overview" ? base : `${base}/${tab}`
}

export type NodeActionKind =
  | "pending"
  | "offline"
  | "fileAccess"
  | "harnessAuth"
  | "harnessProbeFailed"
  | "harnessUpdatesAvailable"
  | "upgradeFailed"
  | "upgradeRunning"
  | "upgradeOutdated"
  | "upgradeManual"
  | "sessionsWaiting"

export type NodeActionCommand =
  | "confirmPairing"
  | "recheckFileAccess"
  | "refreshHarnesses"
  | "upgradeHarnesses"
  | "startUpgrade"
  | "switchUpgradeNow"
  | "openTab"

export type NodeActionItem = {
  kind: NodeActionKind
  tone: "bad" | "warn" | "info"
  tab: NodeTab
  command: NodeActionCommand
  /** Optional version / count / names used when rendering localized copy. */
  version?: string
  from?: string | null
  phase?: string
  running?: number
  count?: number
  names?: string[]
  locations?: string[]
  offlineReason?: string
}

export type NodeActionContext = {
  hubVersion?: string
  harnessReport?: RNodeHarnessReport | null
  harnessesError?: string
  harnessUpdates?: HarnessUpdates | null
  attentionCount?: number
  canConfirm?: boolean
  canUpgrade?: boolean
  canUpgradeHarness?: boolean
}

const TONE_ORDER = { bad: 0, warn: 1, info: 2 } as const

/** Prioritized list of things that need the user's attention on this Node.
 * Shown at the top of Overview, while each item also maps to its owning tab and shared action. */
export function nodeActionItems(v: NodeView, ctx: NodeActionContext = {}): NodeActionItem[] {
  if (v.state === "revoked") return []
  const hubVersion = ctx.hubVersion ?? ""
  const out: NodeActionItem[] = []
  const status = nodeStatus(v, hubVersion)
  const up = upgradeCard(v, hubVersion)

  if (status.kind === "pending") {
    out.push({
      kind: "pending",
      tone: "warn",
      tab: "access",
      command: ctx.canConfirm ? "confirmPairing" : "openTab",
    })
  }

  if (status.kind === "offline") {
    out.push({
      kind: "offline",
      tone: "warn",
      tab: "diagnostics",
      command: "openTab",
      offlineReason: offlineReason(v),
    })
  }

  // Upgrade states: failed rollback (bad) > upgrading (info) > outdated/manual behind (warn)
  if (up.kind === "failed") {
    out.push({
      kind: "upgradeFailed",
      tone: "bad",
      tab: "version",
      command: ctx.canUpgrade && v.node?.upgrade?.can ? "startUpgrade" : "openTab",
      version: up.run.version,
      from: up.run.from_version ?? v.version ?? null,
    })
  } else if (up.kind === "running") {
    const running = v.node?.running ?? v.running ?? 0
    const canSwitch = !!ctx.canUpgrade && ["staged", "draining"].includes(up.run.phase) && running > 0
    out.push({
      kind: "upgradeRunning",
      tone: "info",
      tab: "version",
      command: canSwitch ? "switchUpgradeNow" : "openTab",
      version: up.run.version,
      from: up.run.from_version ?? null,
      phase: up.run.phase,
      running,
    })
  } else if (up.kind === "outdated") {
    out.push({
      kind: "upgradeOutdated",
      tone: "warn",
      tab: "version",
      command: ctx.canUpgrade ? "startUpgrade" : "openTab",
      version: up.to,
      from: up.from,
    })
  } else if (up.kind === "manual" && up.behind) {
    out.push({
      kind: "upgradeManual",
      tone: "bad",
      tab: "version",
      command: "openTab",
      version: up.to,
      from: up.from,
    })
  }

  // macOS Full Disk Access / protected folders missing
  const fileAccess = v.linked && !v.local && v.node?.host?.os === "macos" ? v.node?.host?.file_access : undefined
  if (fileAccess?.status === "missing") {
    out.push({
      kind: "fileAccess",
      tone: "warn",
      tab: "access",
      command: "recheckFileAccess",
      locations: fileAccess.locations.filter((l) => l.status === "missing").map((l) => l.location),
    })
  }

  // Harnesses needing login/authentication or failed probes (only while linked)
  const report = ctx.harnessReport
  if (v.linked && v.state !== "pending") {
    const authNames = (report?.harnesses ?? [])
      .filter((h) => harnessState(h) === "authentication_required")
      .map((h) => h.name)
    if (authNames.length > 0) {
      out.push({
        kind: "harnessAuth",
        tone: "warn",
        tab: "harness",
        command: "refreshHarnesses",
        count: authNames.length,
        names: authNames,
      })
    } else if (ctx.harnessesError || report?.state === "failed") {
      out.push({
        kind: "harnessProbeFailed",
        tone: "warn",
        tab: "harness",
        command: "refreshHarnesses",
      })
    }

    const upgradableHarnesses = availableUpgradableHarnesses(ctx.harnessUpdates)
    if (upgradableHarnesses.length > 0) {
      out.push({
        kind: "harnessUpdatesAvailable",
        tone: "warn",
        tab: "harness",
        command: ctx.canUpgradeHarness && v.state === "active" ? "upgradeHarnesses" : "openTab",
        count: upgradableHarnesses.length,
        names: upgradableHarnesses.map((h) => h.name),
        running: upgradableHarnesses.reduce((n, h) => n + (h.active_sessions ?? 0), 0),
      })
    }
  }

  // Sessions waiting for user answers
  const asks = ctx.attentionCount ?? 0
  if (asks > 0) {
    out.push({
      kind: "sessionsWaiting",
      tone: "warn",
      tab: "work",
      command: "openTab",
      count: asks,
    })
  }

  out.sort((a, b) => TONE_ORDER[a.tone] - TONE_ORDER[b.tone])
  return out
}

export type TabBadge = { count?: number; tone?: "bad" | "warn" | "info" }

/** Counts and warning tones shown on the Node tab bar so problems are visible without switching tabs. */
export function nodeTabBadges(
  v: NodeView,
  actions: readonly NodeActionItem[],
  opts: { activeSessions?: number; harnessReadyCount?: number } = {},
): Record<NodeTab, TabBadge> {
  const badges: Record<NodeTab, TabBadge> = {
    overview: {},
    harness: {},
    work: {},
    access: {},
    version: {},
    diagnostics: {},
  }
  const byTab = (tab: NodeTab) => actions.filter((a) => a.tab === tab)
  const worst = (items: NodeActionItem[]): TabBadge["tone"] =>
    items.some((i) => i.tone === "bad") ? "bad" : items.some((i) => i.tone === "warn") ? "warn" : items.some((i) => i.tone === "info") ? "info" : undefined

  const hItems = byTab("harness")
  if (hItems.length) badges.harness = { count: hItems.reduce((n, i) => n + (i.count ?? 1), 0), tone: worst(hItems) }
  else if (opts.harnessReadyCount) badges.harness = { count: opts.harnessReadyCount }

  const wItems = byTab("work")
  const active = opts.activeSessions ?? (v.running ?? 0)
  if (wItems.length) badges.work = { count: wItems[0].count ?? (active || 1), tone: worst(wItems) }
  else if (active > 0) badges.work = { count: active }

  const aItems = byTab("access")
  if (aItems.length) badges.access = { count: aItems.length, tone: worst(aItems) }

  const vItems = byTab("version")
  if (vItems.length) badges.version = { count: vItems.length, tone: worst(vItems) }

  // Keep diagnostics quiet unless offline, so it signals where connection diagnostics live without noise.
  const dItems = byTab("diagnostics")
  if (dItems.length) badges.diagnostics = { count: dItems.length, tone: worst(dItems) }

  return badges
}

/** Sanitized, read-only diagnostic snapshot safe to copy/share: never includes keys, secrets or tokens. */
export function nodeDiagnosticSnapshot(
  v: NodeView,
  opts: {
    hubVersion?: string
    vitals?: RVitals | null
    harnessReport?: RNodeHarnessReport | null
    harnessesError?: string
    now?: number
  } = {},
) {
  const hubVersion = opts.hubVersion ?? ""
  const vit = opts.vitals ?? null
  const hr = opts.harnessReport ?? null
  const status = nodeStatus(v, hubVersion)
  const host = hostOf(v, vit?.latest)
  return {
    captured_at: new Date(opts.now ?? Date.now()).toISOString(),
    node: {
      id: v.id,
      name: v.name,
      local: v.local,
      state: v.state,
      linked: v.linked,
      paused: v.paused,
      status: status.kind,
      version: v.version ?? null,
      hub_version: hubVersion || null,
      fingerprint: v.fingerprint ?? null,
      last_seen: v.lastSeen ?? null,
      sessions: { running: v.running ?? null, waiting: v.waiting ?? null, max: v.max ?? null },
    },
    link: {
      route: linkRoute(v),
      via: v.node?.link?.via ?? null,
      tls: v.node?.link?.tls ?? null,
      peer: v.node?.link?.peer ?? null,
      since: v.node?.link?.since ?? null,
      latency_ms: v.node?.link?.latency_ms ?? null,
      offline: v.node?.offline ?? null,
      link_error: v.linkError ?? null,
    },
    host: {
      os: host.os,
      arch: host.arch,
      cpus: host.cpus,
      mem_gb: host.memGb,
      hostname: host.hostname,
      file_access: v.node?.host?.file_access ?? null,
    },
    upgrade: v.node?.upgrade
      ? {
          build: v.node.upgrade.build ?? null,
          policy: v.node.upgrade.policy,
          own_policy: v.node.upgrade.own_policy ?? null,
          can: v.node.upgrade.can,
          reason: v.node.upgrade.reason,
          latest: v.node.upgrade.latest ?? null,
        }
      : null,
    removal: v.node?.removal
      ? {
          reason: v.node.removal.reason,
          latest: v.node.removal.latest ?? null,
        }
      : null,
    vitals: vit
      ? {
          sampling: vit.sampling,
          latest: vit.latest,
          tokens: vit.tokens,
          gaps_count: vit.gaps?.length ?? 0,
          series_points: vit.series?.length ?? 0,
        }
      : null,
    harnesses: hr
      ? {
          state: hr.state,
          checked_at: hr.checked_at,
          stale: hr.stale,
          error: opts.harnessesError || hr.error,
          items: hr.harnesses.map((h) => ({
            id: h.id,
            name: h.name,
            state: harnessState(h),
            available: h.available,
            version: h.version || null,
            adapter_version: h.adapter_version,
            degraded_reason: h.degraded_reason,
            models_count: h.models.length,
            error: h.error,
          })),
        }
      : opts.harnessesError
        ? { error: opts.harnessesError }
        : null,
  }
}
