import { useCallback, useEffect, useRef, useState } from "react"
import { DownloadIcon, RefreshCwIcon } from "lucide-react"
import { api } from "@/api"
import { IntegrationRow, IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { Badge } from "@/components/affect/badge-clean"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { HarnessMark } from "@/components/harness-mark"
import { DCard } from "@/devices-ui"
import { useDevices } from "@/devices-data"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { st, useStore } from "@/store"
import {
  availableUpgradableHarnesses,
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

const DEFAULT_AUTO_POLICY: HarnessAutoUpgradePolicy = { enabled: false, harnesses: {} }

/** Shared state and actions for Node harness updates, used by both the Harness tab and Overview's "Needs your attention". */
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

export function NodeHarnessUpdates({ v, state }: { v: NodeView; state: NodeHarnessUpdatesState }) {
  useStore()
  const t = useT()
  if (v.state === "revoked") return null

  const { data, busy, error, message, requesting, savingAuto, unavailable, reload, upgrade, setNodeAutoUpgrade, setHarnessAutoUpgrade } = state
  const policy = data?.auto_upgrade ?? DEFAULT_AUTO_POLICY
  const harnesses = data?.report?.harnesses ?? []
  const shown = harnesses.filter(
    (h) => h.components.some((c) => c.state !== "not_installed" || !!c.installed) || !!h.operation || !!data?.runs[h.harness],
  )
  const other = harnesses.filter((h) => !shown.includes(h))

  return (
    <DCard icon={<DownloadIcon />} title={t("devices:harnessUpdates.title")} sub={t("devices:harnessUpdates.sub")} data-harness-updates={v.id}>
      <div className="flex flex-col gap-3 px-3 pb-3">
        <div className="flex flex-wrap items-center justify-between gap-2 px-2">
          <span className="type-footnote text-muted-foreground" role="status">
            {data?.report?.checking
              ? t("devices:harnessUpdates.checking")
              : data?.at
                ? t("devices:harnessUpdates.observed", { at: monthDayTime(data.at) })
                : ""}
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={!st.owner || !v.linked || busy || !!data?.report?.checking || !!requesting}
            onClick={() => void reload(true)}
            data-harness-update-check
          >
            {busy || data?.report?.checking ? <Spinner /> : <RefreshCwIcon />}
            {t("devices:harnessUpdates.check")}
          </Button>
        </div>

        {st.owner && (
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
              disabled={!st.owner || !data || unavailable || savingAuto !== null}
              onCheckedChange={(checked) => void setNodeAutoUpgrade(checked)}
              aria-label={t("devices:harnessUpdates.autoNodeTitle")}
              data-harness-auto-upgrade-node
            />
          </div>
        )}

        {!st.owner && <p className="px-2 type-footnote text-muted-foreground">{t("devices:harnessUpdates.owner")}</p>}

        {(unavailable || message) && (
          <div className="px-2 type-footnote text-attention" role="alert" data-harness-update-error>
            <p>{t(data?.code === "unsupported" ? "devices:harnessUpdates.oldNode" : "devices:harnessUpdates.unavailable")}</p>
            {data?.report && <p>{t("devices:harnessUpdates.stale")}</p>}
            <details className="mt-1">
              <summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>
              <p className="break-words whitespace-pre-wrap">{message || error || data?.error}</p>
            </details>
          </div>
        )}

        {!harnesses.length && st.owner && !unavailable && (
          <p className="px-2 type-footnote text-muted-foreground">
            {t(busy ? "devices:harnessUpdates.loading" : "devices:harnessUpdates.empty")}
          </p>
        )}

        {shown.map((h) => (
          <UpdateCard
            key={h.harness}
            h={h}
            policy={policy}
            receipt={data?.runs[h.harness] ?? h.operation}
            requesting={requesting === h.harness || requesting === "__all__"}
            savingAuto={savingAuto !== null}
            disabled={!st.owner || !v.linked || v.state !== "active" || unavailable || !!requesting}
            canEditAuto={st.owner && !!data && !unavailable}
            retry={!!data?.retryable_unknown?.includes(h.harness)}
            upgrade={(retry) => void upgrade(h, retry)}
            onAutoMode={(mode) => void setHarnessAutoUpgrade(h.harness, mode)}
          />
        ))}

        {!!other.length && (
          <details className="px-2 type-footnote text-muted-foreground" data-harness-update-other>
            <summary className="cursor-pointer">{t("devices:harnessUpdates.other", { count: other.length })}</summary>
            <div className="mt-3 flex flex-col gap-3">
              {other.map((h) => (
                <UpdateCard
                  key={h.harness}
                  h={h}
                  policy={policy}
                  receipt={data?.runs[h.harness] ?? h.operation}
                  disabled
                  canEditAuto={false}
                  requesting={false}
                  savingAuto={false}
                  retry={false}
                  upgrade={() => {}}
                  onAutoMode={() => {}}
                />
              ))}
            </div>
          </details>
        )}
      </div>
    </DCard>
  )
}

function UpdateCard({
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

  const badge =
    status === "waiting" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.waiting")}</Badge>
    ) : status === "running" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.running")}</Badge>
    ) : available ? (
      <Badge tone="warning">{t("devices:harnessUpdates.available")}</Badge>
    ) : selfOnly ? (
      <Badge tone="neutral">{t("devices:harnessUpdates.badge.checkOnUpgrade")}</Badge>
    ) : undefined

  const metaParts: string[] = []
  if (activeSessions > 0) metaParts.push(t("devices:harnessUpdates.activeSessionsMeta", { count: activeSessions }))
  if (autoSupported && autoEffective) metaParts.push(t("devices:harnessUpdates.autoEnabledMeta"))

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

  return (
    <IntegrationSettings
      name={h.name}
      logo={<HarnessMark harness={h.harness} className="size-5" />}
      badge={badge}
      meta={metaParts.length ? metaParts.join(" · ") : undefined}
      data-harness-update={h.harness}
    >
      <IntegrationSection>
        <ul className="flex flex-col gap-3 type-footnote">
          {h.components.map((c) => (
            <li key={c.id} data-harness-update-component={c.id}>
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{c.name}</span>
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
              <p className="mt-1 font-mono text-muted-foreground">
                {c.installed ?? "—"}
                {c.latest && c.latest !== c.installed ? " → " + c.latest : ""}
              </p>
              {c.upgrade_method === "unsupported" && c.state !== "not_installed" && (
                <p className="mt-1 text-muted-foreground" data-component-unsupported>
                  {c.reason || t("devices:harnessUpdates.external")}
                </p>
              )}
              {c.state === "check_on_upgrade" && (
                <p className="mt-1 text-muted-foreground">{t("devices:harnessUpdates.checkOnUpgradeNote")}</p>
              )}
              {(c.upgrade_command || c.reason || c.package || c.checked) && (
                <details className="mt-1 text-muted-foreground">
                  <summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>
                  {c.upgrade_command && (
                    <p className="break-all font-mono" data-component-command>
                      {t("devices:harnessUpdates.command", { cmd: c.upgrade_command })}
                    </p>
                  )}
                  {c.package && <p className="break-all font-mono">{c.package}</p>}
                  {c.checked && <p>{t("devices:harnessUpdates.checked", { at: monthDayTime(c.checked) })}</p>}
                  {c.reason && <p className="break-words whitespace-pre-wrap">{c.reason}</p>}
                </details>
              )}
            </li>
          ))}
        </ul>
      </IntegrationSection>

      {(canAction || receipt || requesting) && (
        <IntegrationSection>
          {activeSessions > 0 && canAction && status !== "waiting" && status !== "running" && (
            <p className="mb-2 type-footnote text-attention" data-harness-active-sessions={activeSessions}>
              {t("devices:harnessUpdates.activeSessionsNotice", { count: activeSessions })}
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
            <p className="mt-2 type-footnote text-muted-foreground">
              {t(retry ? "devices:harnessUpdates.unknownRetry" : "devices:harnessUpdates.unknown")}
            </p>
          )}
          {(status === "failed" || status === "unknown") && detail && (
            <p
              className="mt-2 rounded-lg bg-attention/10 px-2.5 py-1.5 type-footnote text-attention break-words whitespace-pre-wrap"
              role="alert"
              data-harness-upgrade-detail
            >
              {detail}
            </p>
          )}
          {(h.reason || receipt) && (
            <details className="mt-2 type-footnote text-muted-foreground">
              <summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>
              {h.reason && <p className="break-words whitespace-pre-wrap">{h.reason}</p>}
              {receipt && <p className="break-all font-mono" data-harness-upgrade-operation>{receipt.operation}</p>}
              {detail && <p className="break-words whitespace-pre-wrap">{detail}</p>}
            </details>
          )}
        </IntegrationSection>
      )}

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
    </IntegrationSettings>
  )
}
