/** Shared, pure harness maintenance states, reason-code mapping, and unified Harness card merge.
 * Acceptance and a network timeout never mean success. */
import type { RNodeHarness, RNodeHarnessReport } from "./api.ts"
import type { ComponentInstallSource } from "./gen/ComponentInstallSource.ts"
import type { ComponentUpdateReason } from "./gen/ComponentUpdateReason.ts"
import type { ComponentUpdateState } from "./gen/ComponentUpdateState.ts"
import type { HarnessAutoUpgradeMode } from "./gen/HarnessAutoUpgradeMode.ts"
import type { HarnessAutoUpgradePolicy } from "./gen/HarnessAutoUpgradePolicy.ts"
import type { HarnessComponentUpdateView } from "./gen/HarnessComponentUpdateView.ts"
import type { HarnessUpdateReportView } from "./gen/HarnessUpdateReportView.ts"
import type { HarnessUpdatesResponse } from "./gen/HarnessUpdatesResponse.ts"
import type { HarnessUpdateView } from "./gen/HarnessUpdateView.ts"
import type { HarnessUpgradeBlockReason } from "./gen/HarnessUpgradeBlockReason.ts"
import type { HarnessUpgradeReceipt } from "./gen/HarnessUpgradeReceipt.ts"
import type { SetHarnessAutoUpgrade } from "./gen/SetHarnessAutoUpgrade.ts"
import type { UpgradeHarnessResponse } from "./gen/UpgradeHarnessResponse.ts"
import type { UpgradeMethod } from "./gen/UpgradeMethod.ts"

export type {
  ComponentInstallSource,
  ComponentUpdateReason,
  ComponentUpdateState,
  HarnessAutoUpgradeMode,
  HarnessAutoUpgradePolicy,
  HarnessComponentUpdateView as ComponentUpdate,
  HarnessUpdateReportView,
  HarnessUpdatesResponse as HarnessUpdates,
  HarnessUpdateView as HarnessUpdate,
  HarnessUpgradeBlockReason,
  HarnessUpgradeReceipt as HarnessReceipt,
  SetHarnessAutoUpgrade,
  UpgradeHarnessResponse,
  UpgradeMethod,
}

export type ReceiptStatus = "waiting" | "running" | "unknown" | "succeeded" | "failed" | null

/** Components with a checked newer version that this Node can upgrade via an official method. */
export function updateTargets(h: Pick<HarnessUpdateView, "components">): Record<string, string> {
  return Object.fromEntries(
    h.components
      .filter((c) => c.can_upgrade && c.state === "available" && c.latest)
      .map((c) => [c.id, c.latest!]),
  )
}

/** Components whose official updater checks and upgrades in one command (manual trigger only). */
export function selfUpdateTargets(h: Pick<HarnessUpdateView, "components">): string[] {
  return h.components
    .filter((c) => c.can_upgrade && c.state === "check_on_upgrade")
    .map((c) => c.id)
}

/** Whether the harness currently has any actionable one-click upgrade target. */
export function hasUpgradeAction(h: Pick<HarnessUpdateView, "can_upgrade" | "components">): boolean {
  if (!h.can_upgrade) return false
  return Object.keys(updateTargets(h)).length > 0 || selfUpdateTargets(h).length > 0
}

export function receiptStatus(r?: HarnessUpgradeReceipt | null): ReceiptStatus {
  if (!r) return null
  if (r.state.state === "finished") return r.state.outcome === "succeeded" ? "succeeded" : "failed"
  if (r.state.state === "unknown") return "unknown"
  if (r.state.state === "waiting") return "waiting"
  return "running"
}

/** Effective automatic upgrade setting for one harness (per-harness override > Node default, default off). */
export function effectiveAutoUpgrade(
  policy: HarnessAutoUpgradePolicy | null | undefined,
  harness: string,
): boolean {
  if (!policy) return false
  if (Object.prototype.hasOwnProperty.call(policy.harnesses, harness)) return !!policy.harnesses[harness]
  return !!policy.enabled
}

/** True when this harness explicitly overrides the Node-level auto-upgrade switch. */
export function isAutoUpgradeOverridden(
  policy: HarnessAutoUpgradePolicy | null | undefined,
  harness: string,
): boolean {
  return !!policy && Object.prototype.hasOwnProperty.call(policy.harnesses, harness)
}

/** Auto-upgrade requires at least one upgradable component whose version can be checked before running. */
export function supportsAutoUpgrade(h: Pick<HarnessUpdateView, "components">): boolean {
  return h.components.some(
    (c) =>
      c.can_upgrade &&
      c.state !== "check_on_upgrade" &&
      c.state !== "not_installed" &&
      c.state !== "unsupported",
  )
}

/** Harnesses with a known newer version that can be upgraded right now (and are not already active/unknown). */
export function availableUpgradableHarnesses(data?: HarnessUpdatesResponse | null): HarnessUpdateView[] {
  if (!data?.report || data.error) return []
  return data.report.harnesses.filter((h) => {
    if (!h.can_upgrade || Object.keys(updateTargets(h)).length === 0) return false
    const st = receiptStatus(data.runs?.[h.harness] ?? h.operation)
    return st !== "running" && st !== "waiting" && st !== "unknown"
  })
}

/** Map a strongly-typed component update reason to its i18n key and interpolation parameters. */
export function componentReasonI18n(
  reason: ComponentUpdateReason,
): { key: string; params?: Record<string, string> } {
  switch (reason.code) {
    case "package_manager_unsupported":
      return { key: "devices:harnessUpdates.componentReason.package_manager_unsupported", params: { manager: reason.manager } }
    case "tool_missing":
      return { key: "devices:harnessUpdates.componentReason.tool_missing", params: { tool: reason.tool } }
    case "not_installed":
    case "custom_manifest":
    case "symlink_escape":
    case "read_only_path":
    case "homebrew_unverified":
    case "unverified_upgrader":
    case "custom_install_path":
    case "node_runtime_incompatible":
    case "npm_registry_failed":
    case "brew_info_failed":
    case "brew_version_missing":
      return { key: `devices:harnessUpdates.componentReason.${reason.code}` }
  }
}

export type MergedNodeHarness = {
  id: string
  name: string
  probe: RNodeHarness | null
  update: HarnessUpdateView | null
  receipt: HarnessUpgradeReceipt | null
}

/** Combine probe observations and component update inventory into one card per harness. */
export function mergeNodeHarnesses(
  probeReport?: RNodeHarnessReport | null,
  updates?: HarnessUpdatesResponse | null,
): { shown: MergedNodeHarness[]; other: MergedNodeHarness[] } {
  const probeList = probeReport?.harnesses ?? []
  const updateList = updates?.report?.harnesses ?? []
  const runs = updates?.runs ?? {}
  const updateById = new Map(updateList.map((u) => [u.harness, u]))
  const seen = new Set<string>()
  const all: MergedNodeHarness[] = []

  for (const probe of probeList) {
    seen.add(probe.id)
    const update = updateById.get(probe.id) ?? null
    all.push({
      id: probe.id,
      name: probe.name || update?.name || probe.id,
      probe,
      update,
      receipt: runs[probe.id] ?? update?.operation ?? null,
    })
  }
  for (const update of updateList) {
    if (seen.has(update.harness)) continue
    seen.add(update.harness)
    all.push({
      id: update.harness,
      name: update.name || update.harness,
      probe: null,
      update,
      receipt: runs[update.harness] ?? update.operation ?? null,
    })
  }
  for (const [id, receipt] of Object.entries(runs)) {
    if (seen.has(id)) continue
    seen.add(id)
    all.push({ id, name: id, probe: null, update: null, receipt })
  }

  const shown: MergedNodeHarness[] = []
  const other: MergedNodeHarness[] = []
  for (const entry of all) {
    const installedInUpdate = !!entry.update?.components.some((c) => c.state !== "not_installed" || !!c.installed)
    if (entry.probe || entry.receipt || installedInUpdate) shown.push(entry)
    else other.push(entry)
  }
  return { shown, other }
}
