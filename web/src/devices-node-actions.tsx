/* Shared interactive actions for a Node's detail page.
 * Overview's pinned "needs your attention" cards and each tab's full controls call this exact hook,
 * so there is only one implementation of pairing confirmation, file-access recheck, harness probing,
 * Node upgrades, and work settings (pause / resume / session limit). */
import { useState } from "react"
import { ArrowUpCircleIcon, ChevronRightIcon, RefreshCwIcon, RotateCcwIcon, ShieldCheckIcon } from "lucide-react"
import { api, type RUpgradePolicy } from "@/api"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { openSheet } from "@/ui"
import { st, toast } from "@/store"
import { useT } from "@/i18n"
import type { NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { useWorkSettings } from "@/devices-config"
import type { NodeActionItem, NodeTab } from "@/node-detail-model"

export function useNodeActions(v: NodeView) {
  const t = useT(), { hubVersion, reloadNodes, refreshHarnesses } = useDevices()
  const work = useWorkSettings(v)
  const [fileAccessBusy, setFileAccessBusy] = useState(false)
  const [harnessBusy, setHarnessBusy] = useState(false)
  const [harnessError, setHarnessError] = useState("")
  const [upgradeBusy, setUpgradeBusy] = useState(false)

  const remote = !!v.node && !v.local
  const pairable = remote && (v.state === "active" || v.state === "paused")
  const canUpgrade = st.owner && pairable
  const canConfirm = st.owner && remote && v.state === "pending"

  const confirmPairing = () => {
    openSheet("addDevice", { kind: "node", node: v.id })
  }

  const recheckFileAccess = async () => {
    if (fileAccessBusy || !v.linked) return
    setFileAccessBusy(true)
    try {
      const checked = await api.recheckNodeFileAccess(v.id)
      if (checked.status === "unknown") toast(t("devices:fileAccess.state.unknown"))
      reloadNodes()
    } catch {
      toast(t("devices:fileAccess.checkFailed"))
    } finally {
      setFileAccessBusy(false)
    }
  }

  const probeHarnesses = async () => {
    if (harnessBusy || !v.linked || v.state === "pending" || v.state === "revoked") return
    setHarnessBusy(true)
    setHarnessError("")
    try {
      await refreshHarnesses(v.id)
    } catch (e) {
      setHarnessError((e as Error).message)
    } finally {
      setHarnessBusy(false)
    }
  }

  const startUpgrade = async (force = false) => {
    if (upgradeBusy || !v.node) return
    setUpgradeBusy(true)
    try {
      await api.upgradeNode(v.node.node_id, force)
      toast(t("device:upgrade.started"))
      reloadNodes()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setUpgradeBusy(false)
    }
  }

  const switchUpgradeNow = async () => {
    if (upgradeBusy || !v.node) return
    setUpgradeBusy(true)
    try {
      await api.switchNodeUpgrade(v.node.node_id)
      toast(t("devices:upgrade.switching"))
      reloadNodes()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setUpgradeBusy(false)
    }
  }

  const setUpgradePolicy = async (policy: string) => {
    if (!v.node) return
    try {
      await api.nodeUpgradePolicy(v.node.node_id, policy === "default" ? "" : (policy as RUpgradePolicy))
      reloadNodes()
    } catch (e) {
      toast((e as Error).message)
    }
  }

  return {
    hubVersion,
    remote,
    pairable,
    canUpgrade,
    canConfirm,
    work,
    confirmPairing,
    fileAccessBusy,
    recheckFileAccess,
    harnessBusy,
    harnessError,
    probeHarnesses,
    upgradeBusy,
    startUpgrade,
    switchUpgradeNow,
    setUpgradePolicy,
  }
}

export type NodeActions = ReturnType<typeof useNodeActions>

/** Renders the shared primary button for a NodeActionItem plus an optional link to its owning tab. */
export function NodeActionControls({
  item,
  actions,
  onOpenTab,
  showTabLink = true,
}: {
  item: NodeActionItem
  actions: NodeActions
  onOpenTab: (tab: NodeTab) => void
  showTabLink?: boolean
}) {
  const t = useT()
  const primary = (() => {
    switch (item.command) {
      case "confirmPairing":
        return (
          <Button size="xs" onClick={actions.confirmPairing} data-node-action-run="confirmPairing">
            <ShieldCheckIcon />{t("devices:attn.confirm")}
          </Button>
        )
      case "recheckFileAccess":
        return (
          <Button size="xs" variant="outline" disabled={actions.fileAccessBusy} onClick={() => void actions.recheckFileAccess()} data-node-action-run="recheckFileAccess">
            {actions.fileAccessBusy ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("devices:fileAccess.recheckShort")}
          </Button>
        )
      case "refreshHarnesses":
        return (
          <Button size="xs" variant="outline" disabled={actions.harnessBusy} onClick={() => void actions.probeHarnesses()} data-node-action-run="refreshHarnesses">
            {actions.harnessBusy ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("devices:harness.refresh")}
          </Button>
        )
      case "startUpgrade":
        return (
          <Button size="xs" disabled={actions.upgradeBusy} onClick={() => void actions.startUpgrade(false)} data-node-action-run="startUpgrade">
            {actions.upgradeBusy ? <Spinner className="size-3" /> : item.kind === "upgradeFailed" ? <RotateCcwIcon /> : <ArrowUpCircleIcon />}
            {item.kind === "upgradeFailed" ? t("devices:upgrade.retry") : t("devices:upgrade.to", { version: item.version ?? actions.hubVersion })}
          </Button>
        )
      case "switchUpgradeNow":
        return (
          <Button size="xs" variant="outline" className="text-destructive" disabled={actions.upgradeBusy} onClick={() => void actions.switchUpgradeNow()} data-node-action-run="switchUpgradeNow">
            {actions.upgradeBusy && <Spinner className="size-3" />}{t("devices:upgrade.switchNow", { count: item.running ?? 0 })}
          </Button>
        )
      default:
        return null
    }
  })()

  return (
    <span className="flex shrink-0 flex-wrap items-center gap-1.5">
      {primary}
      {showTabLink && (
        <Button size="xs" variant={primary ? "ghost" : "outline"} onClick={() => onOpenTab(item.tab)} data-node-action-goto={item.tab}>
          {t(`devices:nodeTab.${item.tab}`)}<ChevronRightIcon className="size-3.5" aria-hidden />
        </Button>
      )}
    </span>
  )
}
