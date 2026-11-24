import { useCallback, useEffect, useRef, useState } from "react"
import { DownloadIcon } from "lucide-react"
import { api } from "@/api"
import { IntegrationRow, IntegrationSection } from "@/components/affect/integration-settings-clean"
import { Badge } from "@/components/affect/badge-clean"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { useDevices } from "@/devices-data"
import { useT } from "@/i18n"
import { st, useStore } from "@/store"
import {
  availableUpgradableHarnesses,
  componentReasonI18n,
  effectiveAutoUpgrade,
  hasUpgradeAction,
  isAutoUpgradeOverridden,
  receiptStatus,
  selfUpdateTargets,
  supportsAutoUpgrade,
  updateTargets,
  type HarnessAutoUpgradeMode,
  type HarnessAutoUpgradePolicy,
  type HarnessReceipt,
  type HarnessUpdate,
  type HarnessUpdates,
} from "@/harness-update-status"
import type { NodeView } from "@/device-model"

/** Cache by Node id only; every receipt, installed version, and auto-upgrade policy comes from that target's API. */
const cache = new Map<string, HarnessUpdates>()

export const DEFAULT_AUTO_POLICY: HarnessAutoUpgradePolicy = { enabled: false, harnesses: {} }

/** Shared state and actions for Node harness updates, used by both the unified Harness tab and Overview's "Needs your attention". */
export function useNodeHarnessUpdates(v: NodeView) {
  useStore()
  const t = useT(), { refreshHarnesses } = useDevices()
  const [data, setData] = useState<HarnessUpdates | null>(() => cache.get(v.id) ?? null)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [message, setMessage] = useState("")
  const [requesting, setRequesting] = useState<string | null>(null)
  const [savingAuto, setSavingAuto] = useState<string | null>(null)
  const reading = useRef(false), sending = useRef(false), mounted = useRef(true)
  const seeded = useRef(false), seenSucceeded = useRef<Set<string>>(new Set())

  const reload = useCallback(async (fresh = false) => {
    if (reading.current) return
    reading.current = true
    setBusy(true)
    try {
      const result = await api.harnessUpdates(v.id, fresh)
      if (result.report && result.report.node !== v.id) throw new Error(t("devices:harnessUpdates.wrongNode"))
      cache.set(v.id, result)
      let newlySucceeded = false
      for (const receipt of Object.values(result.runs ?? {})) {
        if (receiptStatus(receipt) === "succeeded" && !seenSucceeded.current.has(receipt.operation)) {
          seenSucceeded.current.add(receipt.operation)
          if (seeded.current) newlySucceeded = true
        }
      }
      seeded.current = true
      if (mounted.current) {
        setData(result)
        setError("")
      }
      if (newlySucceeded) void refreshHarnesses(v.id)
    } catch (e) {
      if (mounted.current) setError((e as Error).message)
    } finally {
      reading.current = false
      if (mounted.current) setBusy(false)
    }
  }, [v.id, t, refreshHarnesses])

  useEffect(() => {
    mounted.current = true
    seeded.current = false
    seenSucceeded.current = new Set()
    setData(cache.get(v.id) ?? null)
    if (st.owner && v.state !== "revoked") void reload()
    return () => { mounted.current = false }
  }, [reload, v.id, v.state, st.owner])

  const active =
    !!data?.report?.checking ||
    Object.values(data?.runs ?? {}).some((r) => ["running", "waiting", "unknown"].includes(receiptStatus(r) ?? ""))

  useEffect(() => {
    if (!st.owner || v.state === "revoked") return
    const timer = setInterval(() => { if (!document.hidden) void reload() }, active ? 1500 : 10_000)
    const visible = () => { if (!document.hidden) void reload() }
    document.addEventListener("visibilitychange", visible)
    return () => { clearInterval(timer); document.removeEventListener("visibilitychange", visible) }
  }, [reload, active, v.state, st.owner])

  const unavailable = !!data?.error || !!error

  const upgrade = useCallback(async (h: HarnessUpdate, retry = false) => {
    const versions = updateTargets(h)
    const selfUpdate = selfUpdateTargets(h)
    if (
      sending.current ||
      !h.can_upgrade ||
      (!Object.keys(versions).length && !selfUpdate.length) ||
      !v.linked ||
      v.state !== "active" ||
      !st.owner ||
      data?.error
    ) return
    sending.current = true
    setRequesting(h.harness)
    setMessage("")
    try {
      const result = await api.upgradeHarness(
        v.id,
        h.harness,
        "hup_" + crypto.randomUUID().replaceAll("-", ""),
        versions,
        retry,
        selfUpdate,
      )
      if (mounted.current) {
        setData((prev) => {
          if (!prev) return prev
          const next = { ...prev, runs: { ...prev.runs, [h.harness]: result.receipt } }
          cache.set(v.id, next)
          return next
        })
      }
    } catch (e) {
      if (mounted.current) setMessage((e as Error).message)
    } finally {
      sending.current = false
      if (mounted.current) {
        setRequesting(null)
        void reload()
      }
    }
  }, [v.id, v.linked, v.state, data?.error, reload])

  const upgradeAvailable = useCallback(async () => {
    const targets = availableUpgradableHarnesses(data)
    if (sending.current || !targets.length || !v.linked || v.state !== "active" || !st.owner || data?.error) return
    sending.current = true
    setRequesting(targets.length === 1 ? targets[0].harness : "__all__")
    setMessage("")
    try {
      for (const h of targets) {
        const versions = updateTargets(h)
        if (!Object.keys(versions).length) continue
        const result = await api.upgradeHarness(
          v.id,
          h.harness,
          "hup_" + crypto.randomUUID().replaceAll("-", ""),
          versions,
          false,
          [],
        )
        if (mounted.current) {
          setData((prev) => {
            if (!prev) return prev
            const next = { ...prev, runs: { ...prev.runs, [h.harness]: result.receipt } }
            cache.set(v.id, next)
            return next
          })
        }
      }
    } catch (e) {
      if (mounted.current) setMessage((e as Error).message)
    } finally {
      sending.current = false
      if (mounted.current) {
        setRequesting(null)
        void reload()
      }
    }
  }, [data, v.id, v.linked, v.state, reload])

  const setNodeAutoUpgrade = useCallback(async (enabled: boolean) => {
    if (!st.owner || v.state === "revoked" || savingAuto) return
    setSavingAuto("__node__")
    setMessage("")
    try {
      const policy = await api.setHarnessAutoUpgrade(v.id, { enabled })
      if (mounted.current) {
        setData((prev) => {
          if (!prev) return prev
          const next = { ...prev, auto_upgrade: policy }
          cache.set(v.id, next)
          return next
        })
      }
    } catch (e) {
      if (mounted.current) setMessage((e as Error).message)
    } finally {
      if (mounted.current) setSavingAuto(null)
    }
  }, [v.id, v.state, savingAuto])

  const setHarnessAutoUpgrade = useCallback(async (harness: string, mode: HarnessAutoUpgradeMode) => {
    if (!st.owner || v.state === "revoked" || savingAuto) return
    setSavingAuto(harness)
    setMessage("")
    try {
      const policy = await api.setHarnessAutoUpgrade(v.id, { harness, mode })
      if (mounted.current) {
        setData((prev) => {
          if (!prev) return prev
          const next = { ...prev, auto_upgrade: policy }
          cache.set(v.id, next)
          return next
        })
      }
    } catch (e) {
      if (mounted.current) setMessage((e as Error).message)
    } finally {
      if (mounted.current) setSavingAuto(null)
    }
  }, [v.id, v.state, savingAuto])

  return {
    data,
    busy,
    error,
    message,
    requesting,
    savingAuto,
    unavailable,
    reload,
    upgrade,
    upgradeAvailable,
    setNodeAutoUpgrade,
    setHarnessAutoUpgrade,
  }
}

export type NodeHarnessUpdatesState = ReturnType<typeof useNodeHarnessUpdates>

/** Node-level auto-upgrade master toggle, displayed at the top of the Harness tab. */
export function NodeAutoUpgradeBar({ state }: { state: NodeHarnessUpdatesState }) {
  useStore()
  const t = useT()
  if (!st.owner) return null
  const policy = state.data?.auto_upgrade ?? DEFAULT_AUTO_POLICY
  return (
    <div
      className="flex items-center justify-between gap-3 rounded-xl border border-border/60 bg-muted/35 px-3.5 py-2.5"
      data-harness-auto-upgrade-card
    >
      <div className="min-w-0">
        <div className="type-callout font-medium text-foreground">{t("devices:harnessUpdates.autoNodeTitle")}</div>
        <p className="type-footnote text-muted-foreground">{t("devices:harnessUpdates.autoNodeSub")}</p>
      </div>
      <Switch
        checked={policy.enabled}
        disabled={!st.owner || !state.data || state.unavailable || state.savingAuto !== null}
        onCheckedChange={(checked) => void state.setNodeAutoUpgrade(checked)}
        aria-label={t("devices:harnessUpdates.autoNodeTitle")}
        data-harness-auto-upgrade-node
      />
    </div>
  )
}

/** Component version list, one-click upgrade controls, visible error alert, and per-harness auto-upgrade override. */
export function HarnessUpgradeSections({
  h,
  policy,
  receipt,
  requesting,
  savingAuto,
  disabled,
  canEditAuto,
  retry,
  upgrade,
  onAutoMode,
}: {
  h: HarnessUpdate
  policy: HarnessAutoUpgradePolicy
  receipt?: HarnessReceipt | null
  requesting: boolean
  savingAuto: boolean
  disabled: boolean
  canEditAuto: boolean
  retry: boolean
  upgrade: (retry: boolean) => void
  onAutoMode: (mode: HarnessAutoUpgradeMode) => void
}) {
  const t = useT()
  const status = receiptStatus(receipt)
  const versions = updateTargets(h)
  const selfTargets = selfUpdateTargets(h)
  const canAction = hasUpgradeAction(h)
  const available = Object.keys(versions).length > 0
  const selfOnly = !available && selfTargets.length > 0
  const activeSessions = h.active_sessions ?? 0
  const autoSupported = supportsAutoUpgrade(h)
  const autoEffective = effectiveAutoUpgrade(policy, h.harness)
  const autoOverridden = isAutoUpgradeOverridden(policy, h.harness)
  const detail =
    receipt?.state.state === "unknown"
      ? receipt.state.reason
      : receipt?.state.state === "finished"
        ? receipt.state.error?.safe_detail
        : null

  const buttonLabel = requesting
    ? t("devices:harnessUpdates.requesting")
    : status === "waiting"
      ? t("devices:harnessUpdates.waiting")
      : status === "running"
        ? t("devices:harnessUpdates.running")
        : status === "unknown" && retry
          ? t("devices:harnessUpdates.retry")
          : activeSessions > 0
            ? t("devices:harnessUpdates.upgradeWhenIdle")
            : selfOnly
              ? t("devices:harnessUpdates.checkAndUpgrade")
              : t("devices:harnessUpdates.upgrade")

  const autoHint = !autoSupported
    ? t("devices:harnessUpdates.autoUnsupported")
    : autoOverridden
      ? t(autoEffective ? "devices:harnessUpdates.autoOverrideOn" : "devices:harnessUpdates.autoOverrideOff")
      : t(autoEffective ? "devices:harnessUpdates.autoInheritOn" : "devices:harnessUpdates.autoInheritOff")

  const formatComponentReason = (reason: NonNullable<(typeof h.components)[number]["reason"]>) => {
    const mapped = componentReasonI18n(reason)
    return t(mapped.key as "devices:harnessUpdates.external", mapped.params)
  }

  return (
    <>
      <IntegrationSection label={t("devices:harnessUpdates.versionsLabel")}>
        <ul className="flex flex-col gap-2.5 type-footnote">
          {h.components.map((c) => {
            const reasonText = c.reason ? formatComponentReason(c.reason) : null
            return (
              <li key={c.id} data-harness-update-component={c.id}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium text-foreground">{c.name}</span>
                  <span className="font-mono text-muted-foreground">
                    {c.installed ?? "—"}
                    {c.latest && c.latest !== c.installed ? " → " + c.latest : ""}
                  </span>
                  <Badge
                    tone={
                      c.state === "available" || c.state === "check_failed"
                        ? "warning"
                        : c.state === "current"
                          ? "success"
                          : c.state === "check_on_upgrade"
                            ? "info"
                            : "neutral"
                    }
                  >
                    {t(`devices:harnessUpdates.state.${c.state}`)}
                  </Badge>
                  {c.state !== "not_installed" && (
                    <Badge tone="neutral" data-component-source={c.source}>
                      {t(`devices:harnessUpdates.source.${c.source}`)}
                    </Badge>
                  )}
                </div>
                {c.upgrade_method === "unsupported" && c.state !== "not_installed" && (
                  <p className="mt-1 text-muted-foreground" data-component-unsupported>
                    {reasonText || t("devices:harnessUpdates.external")}
                  </p>
                )}
                {c.state === "check_failed" && reasonText && (
                  <p className="mt-1 text-attention" data-component-check-failed>
                    {reasonText}
                  </p>
                )}
                {c.state === "check_on_upgrade" && (
                  <p className="mt-1 text-muted-foreground">{t("devices:harnessUpdates.checkOnUpgradeNote")}</p>
                )}
              </li>
            )
          })}
        </ul>

        {(canAction || receipt || requesting || h.blocked_reason) && (
          <div className="mt-3 flex flex-col gap-2">
            {activeSessions > 0 && canAction && status !== "waiting" && status !== "running" && (
              <p className="type-footnote text-attention" data-harness-active-sessions={activeSessions}>
                {t("devices:harnessUpdates.activeSessionsNotice", { count: activeSessions })}
              </p>
            )}
            {h.blocked_reason && status !== "waiting" && status !== "running" && (
              <p className="type-footnote text-muted-foreground" data-harness-blocked-reason={h.blocked_reason}>
                {t(`devices:harnessUpdates.blockReason.${h.blocked_reason}`)}
              </p>
            )}
            <div className="flex flex-wrap items-center gap-2">
              {canAction && (
                <Button
                  size="sm"
                  disabled={disabled || !h.can_upgrade || status === "running" || status === "waiting" || (status === "unknown" && !retry)}
                  onClick={() => upgrade(status === "unknown")}
                  data-harness-upgrade={h.harness}
                >
                  {requesting || status === "running" || status === "waiting" ? <Spinner /> : <DownloadIcon />}
                  {buttonLabel}
                </Button>
              )}
              {status && (
                <span
                  role="status"
                  className={status === "failed" || status === "unknown" ? "type-footnote text-attention" : "type-footnote text-muted-foreground"}
                  data-harness-upgrade-result={status}
                >
                  {t(`devices:harnessUpdates.result.${status}`)}
                </span>
              )}
            </div>
            {status === "unknown" && (
              <p className="type-footnote text-muted-foreground">
                {t(retry ? "devices:harnessUpdates.unknownRetry" : "devices:harnessUpdates.unknown")}
              </p>
            )}
            {(status === "failed" || status === "unknown") && detail && (
              <p
                className="rounded-lg bg-attention/10 px-2.5 py-1.5 type-footnote text-attention break-words whitespace-pre-wrap"
                role="alert"
                data-harness-upgrade-detail
              >
                {detail}
              </p>
            )}
          </div>
        )}
      </IntegrationSection>

      {canEditAuto && (
        <IntegrationSection>
          <IntegrationRow title={t("devices:harnessUpdates.autoHarnessTitle")} hint={autoHint}>
            {autoSupported && autoOverridden && (
              <Button
                size="xs"
                variant="ghost"
                disabled={savingAuto}
                onClick={() => onAutoMode("inherit")}
                data-harness-auto-upgrade-reset={h.harness}
              >
                {t("devices:harnessUpdates.autoReset")}
              </Button>
            )}
            <Switch
              size="sm"
              checked={autoSupported && autoEffective}
              disabled={savingAuto || !autoSupported}
              onCheckedChange={(next) => onAutoMode(next ? "on" : "off")}
              aria-label={t("devices:harnessUpdates.autoHarnessAria", { name: h.name })}
              data-harness-auto-upgrade={h.harness}
            />
          </IntegrationRow>
        </IntegrationSection>
      )}
    </>
  )
}
