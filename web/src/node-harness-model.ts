import type { RNodeHarness, RNodeHarnessReport } from "./api.ts"
import { isUpgradeOnlyLink, type NodeView } from "./device-model.ts"
import type { HarnessDegradedReason } from "./gen/HarnessDegradedReason.ts"
import { hasUpgradeAction, type MergedNodeHarness } from "./harness-update-status.ts"
import { t } from "./i18n/index.ts"

export type HarnessDisplayState =
  | "ready"
  | "authentication_required"
  | "unknown"
  | "not_installed"
  | "installing"
  | "unavailable"

export type HarnessSection = "available" | "attention" | "not_installed"

export type HarnessAttentionAction =
  | "copy_refresh_env"
  | "upgrade"
  | "install"
  | "retry_install"
  | "copy_adapter_install"
  | "refresh"

export type PartitionedNodeHarnesses = {
  available: MergedNodeHarness[]
  attention: MergedNodeHarness[]
  notInstalled: MergedNodeHarness[]
}

const REASON_SEVERITY: Record<HarnessDegradedReason, number> = {
  authentication_required: 0,
  adapter_package_missing: 1,
  command_not_in_service_path: 2,
  version_outdated: 3,
  runtime_incompatible: 4,
  adapter_missing: 5,
  install_failed: 6,
  initialize_failed: 7,
  session_unavailable: 8,
  installing: 9,
  probe_unknown: 10,
}

export function harnessState(h: RNodeHarness): HarnessDisplayState {
  if (h.degraded_reason === "authentication_required") return "authentication_required"
  if (h.available) return "ready"
  if (h.degraded_reason === "installing" || h.setup?.state === "installing") return "installing"
  if (h.available == null) return "unknown"
  if (h.installed === false || (h.setup?.state === "not_installed" && !h.degraded_reason)) return "not_installed"
  return "unavailable"
}

/** Whether a merged harness entry has any installed, partially installed, or configured presence. */
export function isHarnessInstalled(item: MergedNodeHarness): boolean {
  const { probe, update, receipt } = item
  if (probe?.available === true) return true
  if (receipt != null) return true
  if (update?.components.some((c) => c.state !== "not_installed" || !!c.installed)) return true
  if (!probe) return false
  if (probe.installed === true) return true
  if (
    probe.missing_command?.kind === "command_not_in_service_path" ||
    probe.missing_command?.kind === "adapter_package_missing"
  ) return true
  if (probe.degraded_reason != null) return true
  if (probe.available == null) return true
  if (probe.setup) {
    if (probe.setup.state !== "not_installed") return true
    if (probe.setup.error || probe.version) return true
    if (probe.setup.components.some((c) => c.status === "installed" || !!c.detected)) return true
    return false
  }
  return probe.installed !== false && !!probe.version
}

/** Classify a merged harness into one of the three Harness tab sections. */
export function harnessSection(item: MergedNodeHarness): HarnessSection {
  if (item.probe?.available === true && item.probe.degraded_reason == null) return "available"
  return isHarnessInstalled(item) ? "attention" : "not_installed"
}

/** Resolve the single strongly-typed reason explaining why an installed harness needs attention. */
export function effectiveDegradedReason(item: MergedNodeHarness): HarnessDegradedReason | null {
  if (harnessSection(item) !== "attention") return null
  const { probe } = item
  if (probe?.degraded_reason) return probe.degraded_reason
  if (probe?.setup?.state === "installing") return "installing"
  if (probe?.setup?.state === "unavailable") {
    return probe.setup.error ? "install_failed" : "runtime_incompatible"
  }
  if (probe?.setup?.state === "not_installed") return "adapter_missing"
  if (!probe || probe.available == null) return "probe_unknown"
  return "session_unavailable"
}

/** Operator CLI command to install a missing ACP adapter on the target Node host. */
export function adapterInstallCliCommand(item: MergedNodeHarness, local: boolean): string | null {
  const probe = item.probe
  if (!probe) return null
  if (probe.missing_command?.adapter_package || (probe.setup?.components.length ?? 0) > 0) {
    return `steve adapters install ${item.id} ${local ? "hub" : "node"}`
  }
  return formatInstallCommand(probe.install_hint)
}

/** Choose the single concrete primary action for a card in the "Needs attention" section. */
export function attentionActionFor(
  item: MergedNodeHarness,
  reason: HarnessDegradedReason,
  opts: { canInstall?: boolean; canInstallLocal?: boolean; canUpgrade: boolean; hasAdapterInstallCommand?: boolean },
): HarnessAttentionAction {
  const canInstall = opts.canInstall ?? opts.canInstallLocal ?? false
  const upgradeReady = opts.canUpgrade && !!item.update && hasUpgradeAction(item.update)
  if (reason === "command_not_in_service_path" && item.probe?.missing_command?.refresh_command) {
    return "copy_refresh_env"
  }
  if (reason === "adapter_package_missing") {
    if (canInstall) return "install"
    if (opts.hasAdapterInstallCommand) return "copy_adapter_install"
    return "refresh"
  }
  if (reason === "install_failed") {
    return canInstall ? "retry_install" : "refresh"
  }
  if (reason === "version_outdated") {
    if (upgradeReady) return "upgrade"
    return canInstall ? "install" : "refresh"
  }
  if (reason === "adapter_missing") {
    if (canInstall) return "install"
    if (upgradeReady) return "upgrade"
    return "refresh"
  }
  if (reason === "initialize_failed" && upgradeReady) {
    return "upgrade"
  }
  return "refresh"
}

/** Turn a harness `install_hint` (package name or shell snippet) into a copyable terminal command. */
export function formatInstallCommand(hint?: string | null): string | null {
  const trimmed = hint?.trim()
  if (!trimmed) return null
  if (/\s/.test(trimmed)) return trimmed
  return `npm install -g ${trimmed}`
}

function compareByName(a: MergedNodeHarness, b: MergedNodeHarness): number {
  const nameOrder = a.name.localeCompare(b.name, "en", { sensitivity: "base" })
  if (nameOrder !== 0) return nameOrder
  return a.id.localeCompare(b.id)
}

/** Partition merged harnesses into Available, Needs Attention, and Not Installed with deterministic ordering. */
export function partitionNodeHarnesses(items: readonly MergedNodeHarness[]): PartitionedNodeHarnesses {
  const available: MergedNodeHarness[] = []
  const attention: MergedNodeHarness[] = []
  const notInstalled: MergedNodeHarness[] = []

  for (const item of items) {
    const sec = harnessSection(item)
    if (sec === "available") available.push(item)
    else if (sec === "attention") attention.push(item)
    else notInstalled.push(item)
  }

  available.sort(compareByName)
  attention.sort((a, b) => {
    const ra = effectiveDegradedReason(a)
    const rb = effectiveDegradedReason(b)
    const sa = ra ? REASON_SEVERITY[ra] : 99
    const sb = rb ? REASON_SEVERITY[rb] : 99
    if (sa !== sb) return sa - sb
    return compareByName(a, b)
  })
  notInstalled.sort((a, b) => {
    const aInstall = a.probe?.setup?.can_install ? 0 : 1
    const bInstall = b.probe?.setup?.can_install ? 0 : 1
    if (aInstall !== bInstall) return aInstall - bInstall
    return compareByName(a, b)
  })

  return { available, attention, notInstalled }
}

/** The target Node's own evidence. Offline, upgrade-only, and failures never look like a confirmed empty list. */
export function harnessSummary(
  v: Pick<NodeView, "linked" | "linkMode" | "node" | "version" | "hubVersion" | "protocolOlder">,
  r?: RNodeHarnessReport | null,
  hubVersion = "",
): string {
  if (!v.linked || r?.state === "offline") return t(r?.checked_at ? "devices:harness.offlineCached" : "devices:harness.offline")
  if (isUpgradeOnlyLink(v) || r?.state === "upgrade_required") {
    const older = v.protocolOlder ?? v.node?.protocol_older ?? null
    const from = v.version || ""
    const to = v.hubVersion || hubVersion || ""
    if (older === "hub") return t("devices:harness.upgradeRequiredHub", { from: from || "?", to: to || "?" })
    return from && to
      ? t("devices:harness.upgradeRequired", { from, to })
      : t("devices:harness.upgradeRequiredUnknown")
  }
  if (!r || r.state === "unreported") return t("devices:harness.unreported")
  if (r.state === "unsupported") return t("devices:harness.unsupported")
  if (r.state === "failed") return t(r.checked_at ? "devices:harness.failedCached" : "devices:harness.failed")
  const ready = r.harnesses.filter(h => h.available)
  const text = ready.length ? t("devices:harness.available", { names: ready.map(h => [h.name, h.version].filter(Boolean).join(" ")).join(" · ") })
    : t(r.harnesses.some(h => h.available == null) ? "devices:harness.unprobed" : "devices:harness.none")
  return r.stale ? `${t("devices:harness.stale")} · ${text}` : text
}
