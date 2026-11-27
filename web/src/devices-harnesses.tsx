import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react"
import {
  ArrowUpCircleIcon,
  CheckIcon,
  ChevronDownIcon,
  CopyIcon,
  CpuIcon,
  DownloadIcon,
  RefreshCwIcon,
  SearchIcon,
} from "lucide-react"
import { api, type RNodeHarness, type RNodeHarnessReport, type RRuntime } from "@/api"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/affect/badge-clean"
import { Card } from "@/components/affect/card-clean"
import { EmptyState } from "@/components/affect/empty-state-clean"
import { TextField } from "@/components/affect/text-field-clean"
import { Spinner } from "@/components/ui/spinner"
import { IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { useFieldLabels } from "@/components/ui/input"
import { useT } from "@/i18n"
import { date, monthDayTime } from "@/i18n/format"
import { isUpgradeOnlyLink, upgradeCard, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard } from "@/devices-ui"
import { UpgradeSteps } from "@/devices-upgrade"
import {
  filterModelGroups,
  OTHER_FAMILY,
  structureHarnessModels,
  type GroupedModelEntry,
  type ModelFamilyChip,
  type ModelSource,
} from "@/harness-model-list"
import {
  adapterInstallCliCommand,
  attentionActionFor,
  effectiveDegradedReason,
  formatInstallCommand,
  harnessSummary,
  partitionNodeHarnesses,
} from "@/node-harness-model"
import type { NodeTab } from "@/node-detail-model"
import { canInstallRuntime, supportsPermission } from "@/runtime-status"
import { setRuntimes, st, toast, useRuntimesQuery, useStore } from "@/store"
import { go, openSheet } from "@/ui"
import {
  DEFAULT_AUTO_POLICY,
  ExecDeniedNotice,
  HarnessUpgradeBanner,
  HarnessUpgradeDetails,
  NodeAutoUpgradeBar,
  type NodeHarnessUpdatesState,
} from "@/devices-harness-updates"
import {
  effectiveAutoUpgrade,
  mergeNodeHarnesses,
  receiptStatus,
  selfUpdateTargets,
  supportsAutoUpgrade,
  updateTargets,
  type MergedNodeHarness,
} from "@/harness-update-status"
import type { NodeActions } from "@/devices-node-actions"
import { cn } from "cn"

type HarnessActionSlice = Pick<
  NodeActions,
  "harnessBusy" | "harnessError" | "probeHarnesses" | "harnessUpdates" | "canUpgrade" | "upgradeBusy" | "startUpgrade"
>

const COPY_FEEDBACK_MS = 1800
const LOCAL_INSTALL_POLL_MS = 1500
const EMPTY_RUNTIMES: RRuntime[] = []

function stripSourcePrefixForChip(id: string, source: ModelSource): string {
  if (source === "direct") return id
  return id.replace(/^(?:passthrough\/local\/|local\/passthrough\/|passthrough\/|local\/)/i, "")
}

/** Project one local `/api/runtimes` record into the unified `RNodeHarness` contract. */
function runtimeToNodeHarness(
  rt: RRuntime,
  opts: { pending?: boolean; message?: string; restart?: boolean },
): RNodeHarness {
  const setup = rt.setup
  const active = !!opts.pending || setup?.state === "installing"
  const installError = opts.message || setup?.error || null
  const hasComponentPresence = !!setup?.components.some((c) => c.status === "installed" || !!c.detected)
  const installed =
    rt.ok ||
    active ||
    !!installError ||
    !!opts.restart ||
    hasComponentPresence ||
    (setup != null ? setup.state !== "not_installed" : true)

  let degraded: RNodeHarness["degraded_reason"] = null
  if (active) degraded = "installing"
  else if (!rt.ok) {
    if (installError) degraded = "install_failed"
    else if (setup?.state === "unknown") degraded = "probe_unknown"
    else if (setup?.state === "unavailable") degraded = canInstallRuntime(rt, opts.message) ? "adapter_missing" : "runtime_incompatible"
    else if (setup?.state === "not_installed") degraded = hasComponentPresence || opts.restart ? "adapter_missing" : null
    else degraded = "session_unavailable"
  }

  return {
    id: rt.id,
    name: rt.name,
    available: active ? false : rt.ok,
    installed,
    version: rt.version || "",
    adapter_version: rt.adapter_version ?? null,
    models: rt.models ?? [],
    default_model: rt.default_model ?? undefined,
    degraded_reason: degraded,
    error: rt.error ?? null,
    install_hint: rt.install_hint ?? undefined,
    setup: setup
      ? {
          state: active ? "installing" : setup.state,
          can_install: canInstallRuntime(rt, opts.message),
          reason: setup.reason ?? undefined,
          help: setup.help ?? undefined,
          error: installError ?? undefined,
          components: setup.components.map((c) => ({
            id: c.id,
            name: c.name,
            package: c.package,
            status: c.status,
            version: c.version ?? undefined,
            path: c.path ?? undefined,
            detected: c.detected ? { path: c.detected.path, version: c.detected.version ?? undefined } : undefined,
          })),
        }
      : null,
  }
}

/** Merge per-Node probe evidence with local runtime setup/install metadata into a single coherent report. */
function buildUnifiedReport(
  v: NodeView,
  report: RNodeHarnessReport | undefined,
  runtimes: readonly RRuntime[] | undefined,
  pending: readonly string[],
  messages: Record<string, string>,
  restarts: readonly string[],
): RNodeHarnessReport | undefined {
  if (!v.local || !runtimes) {
    if (!report || (pending.length === 0 && Object.values(messages).every((m) => !m))) return report
    return {
      ...report,
      harnesses: report.harnesses.map((h) => {
        const isPending = pending.includes(h.id)
        const msg = messages[h.id]
        if (!isPending && !msg) return h
        const active = isPending || h.setup?.state === "installing"
        return {
          ...h,
          available: active ? false : h.available,
          degraded_reason: active ? "installing" : msg ? "install_failed" : h.degraded_reason,
          setup: h.setup
            ? {
                ...h.setup,
                state: active ? "installing" : h.setup.state,
                can_install: active ? false : h.setup.can_install,
                error: msg || h.setup.error,
              }
            : null,
        }
      }),
    }
  }
  const byId = new Map(runtimes.map((rt) => [rt.id, rt]))
  if (report?.state === "reported" && report.harnesses.length > 0) {
    const merged: RNodeHarness[] = report.harnesses.map((h) => {
      const rt = byId.get(h.id)
      const isPending = pending.includes(h.id)
      const msg = messages[h.id]
      const hasRestart = restarts.includes(h.id)
      if (!rt && !isPending && !msg && !hasRestart) return h
      const fallback = rt ? runtimeToNodeHarness(rt, { pending: isPending, message: msg, restart: hasRestart }) : null
      const active = isPending || h.setup?.state === "installing" || rt?.setup?.state === "installing"
      const mergedSetup = h.setup ?? fallback?.setup ?? null
      return {
        ...h,
        available: active ? false : h.available,
        degraded_reason: active
          ? "installing"
          : msg
            ? "install_failed"
            : h.degraded_reason ?? fallback?.degraded_reason ?? null,
        error: h.error ?? fallback?.error ?? null,
        install_hint: h.install_hint ?? fallback?.install_hint,
        setup: mergedSetup
          ? {
              ...mergedSetup,
              state: active ? "installing" : mergedSetup.state,
              can_install: rt ? canInstallRuntime(rt, msg) : mergedSetup.can_install,
              error: msg || mergedSetup.error || undefined,
            }
          : null,
      }
    })
    return { ...report, harnesses: merged }
  }
  return {
    state: "reported",
    checked_at: report?.checked_at ?? null,
    attempted_at: report?.attempted_at ?? null,
    stale: false,
    refreshing: report?.refreshing ?? false,
    error: report?.error ?? null,
    harnesses: runtimes.map((rt) =>
      runtimeToNodeHarness(rt, {
        pending: pending.includes(rt.id),
        message: messages[rt.id],
        restart: restarts.includes(rt.id),
      }),
    ),
  }
}

/** Unified per-Node Harness inventory partitioned into Available, Needs Attention, and Not Installed. */
export function NodeHarnesses({
  v,
  actions,
  onOpenTab,
  children,
}: {
  v: NodeView
  actions?: HarnessActionSlice
  onOpenTab?: (tab: NodeTab) => void
  children?: ReactNode
}) {
  useStore()
  const t = useT(), { hubVersion, harnesses, harnessesError, refreshHarnesses } = useDevices()
  const localRuntimesQuery = useRuntimesQuery(v.local)
  const localRuntimes = v.local ? localRuntimesQuery.data : undefined
  const localRuntimeById = useMemo(
    () => new Map((localRuntimes ?? EMPTY_RUNTIMES).map((rt) => [rt.id, rt])),
    [localRuntimes],
  )
  const rawReport = harnesses?.[v.id]
  const [ownBusy, setOwnBusy] = useState(false), [ownError, setOwnError] = useState("")
  const [localRefreshError, setLocalRefreshError] = useState("")
  const [pending, setPending] = useState<string[]>([])
  const [installMessages, setInstallMessages] = useState<Record<string, string>>({})
  const [restarts, setRestarts] = useState<string[]>([])
  const [availableOpen, setAvailableOpen] = useState(true)
  const [attentionOpen, setAttentionOpen] = useState(true)
  const [notInstalledOpen, setNotInstalledOpen] = useState(false)
  const installingRef = useRef(new Set<string>())
  const installEpoch = useRef(0)
  const readingLocal = useRef<Promise<RRuntime[] | null> | null>(null)

  const r = useMemo(
    () => buildUnifiedReport(v, rawReport, localRuntimes, pending, installMessages, restarts),
    [v, rawReport, localRuntimes, pending, installMessages, restarts],
  )
  const hasLocalFallback = v.local && localRuntimes !== undefined
  const initialLocalLoading = v.local && !rawReport && localRuntimes === undefined && localRuntimesQuery.isPending

  const busy = (actions ? actions.harnessBusy : ownBusy) || ownBusy
  const error =
    localRefreshError ||
    (hasLocalFallback ? "" : (actions ? actions.harnessError : ownError)) ||
    (v.local && localRuntimesQuery.error instanceof Error ? localRuntimesQuery.error.message : "")
  const effectiveHarnessesError = hasLocalFallback ? "" : harnessesError
  const updates = actions?.harnessUpdates
  const upgradeOnly = isUpgradeOnlyLink(v) || r?.state === "upgrade_required"
  const stale = !v.linked || !!r?.stale || !!effectiveHarnessesError || r?.state !== "reported"

  const reloadLocalRuntimes = useCallback(async () => {
    if (!v.local) return null
    if (readingLocal.current) return readingLocal.current
    const epoch = installEpoch.current
    const load = (async () => {
      try {
        const result = await api.runtimes(true)
        setRuntimes(result)
        setLocalRefreshError("")
        if (epoch === installEpoch.current) {
          setPending((ids) =>
            ids.filter((id) => installingRef.current.has(id) || result.find((rt) => rt.id === id)?.setup?.state === "installing"),
          )
        }
        return result
      } catch (e) {
        setLocalRefreshError((e as Error).message)
        throw e
      } finally {
        readingLocal.current = null
      }
    })()
    readingLocal.current = load
    return load
  }, [v.local])

  const refresh = useCallback(async () => {
    if (v.local) {
      setOwnBusy(true)
      setOwnError("")
      try {
        const nodeProbe = actions
          ? actions.probeHarnesses().catch(() => undefined)
          : refreshHarnesses(v.id).catch(() => undefined)
        await Promise.all([nodeProbe, reloadLocalRuntimes()])
      } catch (e) {
        setLocalRefreshError((e as Error).message)
      } finally {
        setOwnBusy(false)
      }
      return
    }
    if (actions) { await actions.probeHarnesses(); return }
    setOwnBusy(true); setOwnError("")
    try { await refreshHarnesses(v.id) } catch (e) { setOwnError((e as Error).message) } finally { setOwnBusy(false) }
  }, [v.local, v.id, actions, refreshHarnesses, reloadLocalRuntimes])

  const installPolling =
    pending.length > 0 ||
    (v.local
      ? (localRuntimes ?? EMPTY_RUNTIMES).some((rt) => rt.setup?.state === "installing")
      : (rawReport?.harnesses ?? []).some((h) => h.setup?.state === "installing"))

  useEffect(() => {
    if (!installPolling) return
    let live = true
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      if (!live) return
      if (!document.hidden) {
        try {
          if (v.local) await reloadLocalRuntimes()
          else await refreshHarnesses(v.id)
        } catch {
          /* surfaced inline; keep polling while installing */
        }
      }
      if (live) timer = setTimeout(() => void poll(), LOCAL_INSTALL_POLL_MS)
    }
    timer = setTimeout(() => void poll(), LOCAL_INSTALL_POLL_MS)
    return () => { live = false; clearTimeout(timer) }
  }, [installPolling, v.local, v.id, reloadLocalRuntimes, refreshHarnesses])

  const canManageInstall = v.local
    ? st.owner || !!v.device?.can_configure
    : st.owner && v.linked && v.state === "active"

  const installAdapter = async (id: string) => {
    if (!canManageInstall || installingRef.current.has(id)) return
    if (v.local) {
      const rt = localRuntimeById.get(id)
      if (rt && (!canInstallRuntime(rt, installMessages[id]) || rt.setup?.state === "installing")) return
    } else {
      const probe = rawReport?.harnesses.find((h) => h.id === id)
      if (probe?.setup && (!probe.setup.can_install || probe.setup.state === "installing")) return
    }
    installEpoch.current++
    installingRef.current.add(id)
    setPending((ids) => [...new Set([...ids, id])])
    setInstallMessages((ms) => ({ ...ms, [id]: "" }))
    try {
      const res = v.local ? await api.installHarness(id) : await api.installNodeHarness(v.id, id)
      if ("restart" in res && res.restart) setRestarts((ids) => [...new Set([...ids, id])])
      if (!res.accepted) {
        const msg = t("shell:runtimeSetup.notAccepted")
        setInstallMessages((ms) => ({ ...ms, [id]: msg }))
        toast(t("devices:harness.action.installNotAccepted"))
      }
    } catch (e) {
      setInstallMessages((ms) => ({ ...ms, [id]: (e as Error).message }))
    } finally {
      installingRef.current.delete(id)
      if (v.local) {
        try { await readingLocal.current } catch { /* fresh probe below recovers */ }
      }
      try { await refresh() } catch { /* keep operation and refresh errors visible */ }
      if (!v.local) {
        setPending((ids) => ids.filter((item) => item !== id))
      }
    }
  }

  const { available, attention, notInstalled } = useMemo(
    () => partitionNodeHarnesses(mergeNodeHarnesses(r, updates?.data)),
    [r, updates?.data],
  )
  const totalMerged = available.length + attention.length + notInstalled.length
  const policy = updates?.data?.auto_upgrade ?? DEFAULT_AUTO_POLICY
  const showUpdatesChrome = !!updates && v.state !== "revoked" && !upgradeOnly
  const updatesUnavailable = !!updates?.unavailable

  if (upgradeOnly) {
    const effectiveHub = v.hubVersion || hubVersion
    const from = v.version || ""
    const older = v.protocolOlder ?? v.node?.protocol_older ?? null
    const up = upgradeCard(v, effectiveHub)
    const canStart = !v.local && older !== "hub" && !!actions?.canUpgrade && !!v.node?.upgrade?.can && up.kind !== "running"
    const summaryText = older === "hub"
      ? t("devices:harness.upgradeRequiredHub", { from: from || "?", to: effectiveHub || "?" })
      : from && effectiveHub
        ? t("devices:harness.upgradeRequired", { from, to: effectiveHub })
        : t("devices:harness.upgradeRequiredUnknown")
    const hintText = v.local
      ? t("devices:harness.upgradeRequiredLocalHint")
      : older === "hub"
        ? t("devices:harness.upgradeRequiredHubHint")
        : !canStart && up.kind !== "running"
          ? t("devices:harness.upgradeRequiredManualHint")
          : null

    return (
      <DCard
        icon={<CpuIcon />}
        title={t("devices:harness.title")}
        sub={summaryText}
        data-node-harnesses={v.id}
        data-node-harness-upgrade-required={v.id}
      >
        <div className="px-4 pb-5">
          <EmptyState
            icon={<ArrowUpCircleIcon />}
            title={t(older === "hub" ? "devices:harness.upgradeRequiredHubTitle" : "devices:harness.upgradeRequiredTitle")}
            text={
              <span className="flex flex-col items-center gap-2">
                <span>{summaryText}</span>
                {hintText && <span className="type-footnote text-muted-foreground">{hintText}</span>}
                {up.kind === "running" && (
                  <span className="mt-1 w-full max-w-md text-left">
                    <UpgradeSteps steps={up.steps} />
                  </span>
                )}
              </span>
            }
            action={
              <>
                {canStart && (
                  <Button
                    size="sm"
                    disabled={actions.upgradeBusy}
                    onClick={() => void actions.startUpgrade(false)}
                    data-harness-upgrade-node={v.id}
                  >
                    {actions.upgradeBusy ? <Spinner className="size-3.5" /> : <ArrowUpCircleIcon />}
                    {effectiveHub
                      ? t("devices:upgrade.nodeTo", { version: effectiveHub })
                      : t("devices:upgrade.nodeAction")}
                  </Button>
                )}
                {onOpenTab && (
                  <Button
                    size="sm"
                    variant={canStart ? "outline" : "default"}
                    onClick={() => onOpenTab("version")}
                    data-harness-open-version
                  >
                    {t("devices:nodeTab.version")}
                  </Button>
                )}
              </>
            }
          />
        </div>
      </DCard>
    )
  }

  return (
    <DCard
      icon={<CpuIcon />}
      title={t("devices:harness.title")}
      sub={harnessSummary(v, r, hubVersion)}
      data-node-harnesses={v.id}
      data-harness-updates={v.id}
      action={
        <span className="flex flex-wrap items-center gap-1.5">
          {showUpdatesChrome && st.owner && (
            <Button
              size="sm"
              variant="outline"
              disabled={!v.linked || updates.busy || !!updates.data?.report?.checking || !!updates.requesting}
              onClick={() => void updates.reload(true)}
              data-harness-update-check
            >
              {updates.busy || updates.data?.report?.checking ? <Spinner /> : <DownloadIcon />}
              {t("devices:harnessUpdates.check")}
            </Button>
          )}
          <Button
            size="sm"
            variant="ghost"
            disabled={busy || !v.linked || v.state === "pending" || v.state === "revoked"}
            onClick={() => void refresh()}
            data-node-harness-refresh
          >
            {busy || r?.refreshing ? <Spinner /> : <RefreshCwIcon />}
            {t("devices:harness.refresh")}
          </Button>
        </span>
      }
    >
      <div className="flex min-w-0 flex-col gap-4 px-3 pb-3" aria-live="polite">
        {(r?.checked_at || (showUpdatesChrome && (updates.data?.report?.checking || updates.data?.at))) && (
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 px-2 type-footnote text-muted-foreground">
            {r?.checked_at && (
              <span data-node-harness-checked>
                {t("devices:harness.checked", { at: date(r.checked_at, { dateStyle: "short", timeStyle: "medium" }) })}
              </span>
            )}
            {showUpdatesChrome && (updates.data?.report?.checking || updates.data?.at) && (
              <span role="status">
                {updates.data?.report?.checking
                  ? t("devices:harnessUpdates.checking")
                  : updates.data?.at
                    ? t("devices:harnessUpdates.observed", { at: monthDayTime(updates.data.at) })
                    : ""}
              </span>
            )}
          </div>
        )}

        {showUpdatesChrome && <NodeAutoUpgradeBar state={updates} />}

        {showUpdatesChrome && !st.owner && (
          <p className="px-2 type-footnote text-muted-foreground">{t("devices:harnessUpdates.owner")}</p>
        )}

        {(error || effectiveHarnessesError) && (
          <p role="alert" className="px-2 type-footnote text-attention" data-node-harness-error>
            {t("devices:harness.readFailed")}
          </p>
        )}
        {(error || effectiveHarnessesError || r?.error) && (
          <details className="px-2 type-footnote text-muted-foreground" data-node-harness-error-details>
            <summary className="cursor-pointer">{t("devices:harness.details")}</summary>
            <p className="mt-1 break-words whitespace-pre-wrap">{error || effectiveHarnessesError || r?.error}</p>
          </details>
        )}

        {showUpdatesChrome && (updatesUnavailable || updates.message) && (
          <div className="rounded-lg bg-attention/10 px-3 py-2 type-footnote text-attention" role="alert" data-harness-update-error>
            <p className="font-medium">
              {t(
                updates.data?.code === "unsupported"
                  ? "devices:harnessUpdates.oldNode"
                  : "devices:harnessUpdates.unavailable",
              )}
            </p>
            {(updates.message || updates.error || updates.data?.error) && (
              <p className="mt-1 break-words whitespace-pre-wrap" data-harness-update-reason>
                {updates.message || updates.error || updates.data?.error}
              </p>
            )}
            {updates.data?.report && <p className="mt-1 text-muted-foreground">{t("devices:harnessUpdates.stale")}</p>}
          </div>
        )}

        {initialLocalLoading && (
          <div className="flex justify-center px-4 py-6" data-node-harness-loading>
            <Spinner className="size-4 text-muted-foreground" />
          </div>
        )}

        {!initialLocalLoading && v.local && totalMerged === 0 && (
          <p className="px-2 py-2 type-callout text-muted-foreground" data-node-harness-empty>
            {t("shell:devices.noRuntimes")}
          </p>
        )}

        {available.length > 0 && (
          <HarnessSectionGroup
            kind="available"
            title={t("devices:harness.section.available")}
            count={available.length}
            tone="success"
            open={availableOpen}
            onToggle={() => setAvailableOpen((o) => !o)}
          >
            <div className="flex min-w-0 flex-col gap-2.5">
              {available.map((item) => (
                <AvailableHarnessCard
                  key={item.id}
                  v={v}
                  item={item}
                  stale={stale}
                  localRuntime={localRuntimeById.get(item.id)}
                  updates={showUpdatesChrome ? updates : undefined}
                  policy={policy}
                  onOpenTab={onOpenTab}
                />
              ))}
            </div>
          </HarnessSectionGroup>
        )}

        {attention.length > 0 && (
          <HarnessSectionGroup
            kind="attention"
            title={t("devices:harness.section.attention")}
            count={attention.length}
            tone="warning"
            open={attentionOpen}
            onToggle={() => setAttentionOpen((o) => !o)}
          >
            <div className="flex min-w-0 flex-col gap-2.5">
              {attention.map((item) => (
                <AttentionHarnessCard
                  key={item.id}
                  v={v}
                  item={item}
                  stale={stale}
                  busy={busy}
                  canManageInstall={canManageInstall}
                  execAllowed={r?.exec_allowed}
                  installing={pending.includes(item.id) || item.probe?.setup?.state === "installing"}
                  pendingInstall={pending.includes(item.id)}
                  restart={restarts.includes(item.id)}
                  localRuntime={localRuntimeById.get(item.id)}
                  updates={showUpdatesChrome ? updates : undefined}
                  policy={policy}
                  onOpenTab={onOpenTab}
                  onRefresh={() => void refresh()}
                  onInstall={() => void installAdapter(item.id)}
                />
              ))}
            </div>
          </HarnessSectionGroup>
        )}

        {notInstalled.length > 0 && (
          <HarnessSectionGroup
            kind="not_installed"
            title={t("devices:harness.section.not_installed")}
            count={notInstalled.length}
            tone="neutral"
            open={notInstalledOpen}
            onToggle={() => setNotInstalledOpen((o) => !o)}
          >
            <Card className="steve-card min-w-0 overflow-hidden p-0">
              <div className="flex min-w-0 flex-col divide-y divide-border/60">
                {notInstalled.map((item) => (
                  <NotInstalledHarnessRow
                    key={item.id}
                    item={item}
                    canManageInstall={canManageInstall}
                    installing={pending.includes(item.id)}
                    localRuntime={localRuntimeById.get(item.id)}
                    onInstall={() => void installAdapter(item.id)}
                  />
                ))}
              </div>
            </Card>
          </HarnessSectionGroup>
        )}

        {children && (
          <div className="mt-1 px-2" data-node-harness-manage>
            {children}
          </div>
        )}
      </div>
    </DCard>
  )
}

/* ───────────────────── Collapsible Section Chrome ───────────────────── */

function HarnessSectionGroup({
  kind,
  title,
  count,
  tone,
  open,
  onToggle,
  children,
}: {
  kind: "available" | "attention" | "not_installed"
  title: string
  count: number
  tone: "success" | "warning" | "neutral"
  open: boolean
  onToggle: () => void
  children: ReactNode
}) {
  const bodyId = useId()
  const dotColor =
    tone === "success" ? "bg-success" : tone === "warning" ? "bg-attention" : "bg-muted-foreground/50"

  return (
    <section className="flex min-w-0 flex-col gap-2" data-harness-section={kind} data-open={open ? "true" : "false"}>
      <button
        type="button"
        className="harness-sec-toggle"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={onToggle}
        data-harness-section-toggle={kind}
      >
        <span className={cn("size-2 shrink-0 rounded-full", dotColor)} aria-hidden />
        <span className="type-callout font-medium text-foreground">{title}</span>
        <Badge tone={tone} count={count} data-harness-section-count={kind} />
        <ChevronDownIcon
          className={cn("ml-auto size-4 shrink-0 text-muted-foreground transition-transform duration-150", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open && (
        <div id={bodyId} className="min-w-0">
          {children}
        </div>
      )}
    </section>
  )
}

/* ───────────────────── Section 1: Compact Available Card ───────────────────── */

function AvailableHarnessCard({
  v,
  item,
  stale,
  localRuntime,
  updates,
  policy,
  onOpenTab,
}: {
  v: NodeView
  item: MergedNodeHarness
  stale: boolean
  localRuntime?: RRuntime
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  onOpenTab?: (tab: NodeTab) => void
}) {
  const t = useT()
  const { probe, update, receipt } = item
  const uStatus = receiptStatus(receipt)
  const hasUpdate = update ? Object.keys(updateTargets(update)).length > 0 : false
  const selfOnly = update ? !hasUpdate && selfUpdateTargets(update).length > 0 : false
  const activeSessions = update?.active_sessions ?? 0
  const autoOn = !!update && supportsAutoUpgrade(update) && effectiveAutoUpgrade(policy, item.id)
  const bot = v.local && localRuntime
    ? Object.values(st.bots).find((b) => b.on && b.harness === item.id && supportsPermission(localRuntime, b.permMode))
    : undefined

  const updateBadge =
    uStatus === "waiting" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.waiting")}</Badge>
    ) : uStatus === "running" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.running")}</Badge>
    ) : hasUpdate ? (
      <Badge tone="warning">{t("devices:harnessUpdates.available")}</Badge>
    ) : selfOnly ? (
      <Badge tone="neutral">{t("devices:harnessUpdates.badge.checkOnUpgrade")}</Badge>
    ) : null

  const badge = (
    <span className="inline-flex flex-wrap items-center gap-1.5">
      <Badge
        tone={stale ? "neutral" : "success"}
        dot={!stale}
        data-node-harness-state={stale ? "stale" : "ready"}
      >
        {t("devices:harness.state.ready")}
        {stale && ` · ${t("devices:harness.lastReport")}`}
      </Badge>
      {updateBadge}
    </span>
  )

  const metaParts: string[] = []
  if (probe?.version) metaParts.push(`CLI ${probe.version}`)
  else if (!probe?.adapter_version) metaParts.push(`CLI ${t("devices:harness.versionUnknown")}`)
  if (probe?.adapter_version) metaParts.push(`ACP ${probe.adapter_version}`)
  if (activeSessions > 0) metaParts.push(t("devices:harnessUpdates.activeSessionsMeta", { count: activeSessions }))
  if (autoOn) metaParts.push(t("devices:harnessUpdates.autoEnabledMeta"))

  const upgradeDisabled =
    !updates ||
    !st.owner ||
    !v.linked ||
    v.state !== "active" ||
    updates.unavailable ||
    !!updates.requesting

  return (
    <IntegrationSettings
      name={item.name}
      logo={<HarnessMark harness={item.id} className="size-5" />}
      badge={badge}
      meta={
        <span className="flex flex-wrap gap-x-3 gap-y-0.5" data-node-harness-versions>
          {metaParts.map((text) => (
            <span key={text}>{text}</span>
          ))}
        </span>
      }
      className="harness-card harness-card--compact"
      data-node-harness-id={item.id}
      data-harness-update={item.id}
    >
      {probe && !stale && (
        <IntegrationSection>
          <div className="flex flex-col gap-2">
            <HarnessModelsSummary models={probe.models} defaultModel={probe.default_model ?? null} />
            {v.local && localRuntime && (
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => (bot ? openSheet("newItem", { bot: bot.id }) : go("#/bots"))}
                  data-harness-use={item.id}
                >
                  {t(bot ? "shell:runtimeSetup.start" : "shell:runtimeSetup.configureBot")}
                </Button>
              </div>
            )}
          </div>
        </IntegrationSection>
      )}

      {update && updates && (
        <HarnessUpgradeBanner
          h={update}
          receipt={receipt}
          requesting={updates.requesting === item.id || updates.requesting === "__all__"}
          disabled={upgradeDisabled}
          retry={!!updates.data?.retryable_unknown?.includes(item.id)}
          local={v.local}
          onOpenTab={onOpenTab}
          upgrade={(retry) => void updates.upgrade(update, retry)}
        />
      )}

      <HarnessDetailsSection
        item={item}
        localRuntime={localRuntime}
        updates={updates}
        policy={policy}
        showReportedModels={stale}
      />
    </IntegrationSettings>
  )
}

/* ───────────────────── Section 2: Needs Attention Card ───────────────────── */

function AttentionHarnessCard({
  v,
  item,
  stale,
  busy,
  canManageInstall,
  execAllowed,
  installing,
  pendingInstall,
  restart,
  localRuntime,
  updates,
  policy,
  onOpenTab,
  onRefresh,
  onInstall,
}: {
  v: NodeView
  item: MergedNodeHarness
  stale: boolean
  busy: boolean
  canManageInstall: boolean
  execAllowed?: boolean
  installing: boolean
  pendingInstall: boolean
  restart: boolean
  localRuntime?: RRuntime
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  onOpenTab?: (tab: NodeTab) => void
  onRefresh: () => void
  onInstall: () => void
}) {
  const t = useT()
  const [copiedKey, setCopiedKey] = useState<"install" | "refresh" | null>(null)
  const { probe, update, receipt } = item
  const reason = effectiveDegradedReason(item) ?? "probe_unknown"
  const uStatus = receiptStatus(receipt)
  const hasUpdate = update ? Object.keys(updateTargets(update)).length > 0 : false
  const missingDiag = probe?.missing_command ?? null

  const canInstall = canManageInstall && !!probe?.setup?.can_install && !installing
  const execDenied =
    probe?.setup?.blocked_reason === "exec_denied" ||
    update?.blocked_reason === "exec_denied" ||
    execAllowed === false
  const upgradeDisabled =
    !updates ||
    !st.owner ||
    !v.linked ||
    v.state !== "active" ||
    updates.unavailable ||
    !!updates.requesting
  const canUpgradeNow = !!update && !!updates && !upgradeDisabled && update.can_upgrade

  const adapterCliCmd = adapterInstallCliCommand(item, v.local)
  const installCmd = formatInstallCommand(probe?.install_hint)
  const refreshEnvCmd = missingDiag?.refresh_command?.trim() || null

  const primaryAction = attentionActionFor(item, reason, {
    canInstall,
    canUpgrade: canUpgradeNow,
    hasAdapterInstallCommand: !!adapterCliCmd,
  })

  const showInstallExecDenied =
    reason === "adapter_package_missing" && !installing && !canInstall && execDenied && probe?.setup != null

  const copyCommand = async (cmd: string | null, kind: "install" | "refresh") => {
    if (!cmd) return
    try {
      await navigator.clipboard.writeText(cmd)
      setCopiedKey(kind)
      window.setTimeout(() => setCopiedKey((cur) => (cur === kind ? null : cur)), COPY_FEEDBACK_MS)
    } catch {
      toast(t("devices:diagnostics.copyFailed"))
    }
  }

  const badgeTone =
    reason === "installing" ? "info" : reason === "probe_unknown" || stale ? "neutral" : "warning"
  const stateAttr =
    stale
      ? "stale"
      : reason === "authentication_required"
        ? "authentication_required"
        : reason === "installing"
          ? "installing"
          : reason === "probe_unknown"
            ? "unknown"
            : "unavailable"

  const metaParts: string[] = []
  if (probe?.version) metaParts.push(`CLI ${probe.version}`)
  if (probe?.adapter_version) metaParts.push(`ACP ${probe.adapter_version}`)
  if (!metaParts.length && update?.components.length) {
    const installed = update.components.find((c) => c.installed)
    if (installed?.installed) metaParts.push(`${installed.name} ${installed.installed}`)
  }

  const degradedCopy =
    reason === "adapter_package_missing"
      ? missingDiag?.adapter_package
        ? t("devices:harness.degraded.adapter_package_missing_pkg", {
            name: item.name,
            pkg: missingDiag.adapter_package,
          })
        : missingDiag?.command
          ? t("devices:harness.degraded.adapter_package_missing_cmd", {
              name: item.name,
              cmd: missingDiag.command,
            })
          : t("devices:harness.degraded.adapter_package_missing", { name: item.name })
      : t(`devices:harness.degraded.${reason}`)

  // Avoid showing a duplicate "execution policy disables upgrades" banner directly beneath an
  // adapter-install card that already explains `exec_denied` and has no pending upgrade/receipt.
  const suppressDuplicateExecDeniedBanner =
    showInstallExecDenied && !receipt && !hasUpdate && update?.blocked_reason === "exec_denied"
  const showSeparateUpgradeBanner =
    !!update &&
    !!updates &&
    !suppressDuplicateExecDeniedBanner &&
    (primaryAction !== "upgrade" || !!receipt || !!update.blocked_reason)

  return (
    <IntegrationSettings
      name={item.name}
      logo={<HarnessMark harness={item.id} className="size-5" />}
      badge={
        <span className="inline-flex flex-wrap items-center gap-1.5">
          <Badge
            tone={badgeTone}
            dot={reason === "installing"}
            live={reason === "installing"}
            data-node-harness-state={stateAttr}
            data-node-harness-reason={reason}
          >
            {t(`devices:harness.reasonBadge.${reason}`)}
            {stale && ` · ${t("devices:harness.lastReport")}`}
          </Badge>
          {uStatus === "running" && <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.running")}</Badge>}
          {uStatus === "waiting" && <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.waiting")}</Badge>}
          {!uStatus && hasUpdate && <Badge tone="warning">{t("devices:harnessUpdates.available")}</Badge>}
        </span>
      }
      meta={
        metaParts.length > 0 ? (
          <span className="flex flex-wrap gap-x-3 gap-y-0.5" data-node-harness-versions>
            {metaParts.map((text) => (
              <span key={text}>{text}</span>
            ))}
          </span>
        ) : undefined
      }
      className="harness-card harness-card--attention"
      data-node-harness-id={item.id}
      data-harness-update={item.id}
    >
      <IntegrationSection>
        <div className="flex flex-col gap-2.5">
          <div className="flex flex-col gap-2" data-node-harness-degraded={reason}>
            <p className="type-footnote text-foreground/90">{degradedCopy}</p>
            {installing && (
              <p className="type-footnote text-muted-foreground" role="status" data-harness-installing-status>
                {t(pendingInstall && localRuntime?.setup?.state !== "installing" ? "shell:runtimeSetup.requesting" : "shell:runtimeSetup.polling")}
              </p>
            )}
            {restart && (
              <p className="break-words type-callout text-attention" role="status" data-harness-restart>
                {t("shell:runtimeSetup.restart")}
              </p>
            )}
            {missingDiag && missingDiag.kind === "adapter_package_missing" && (
              <div
                className="flex flex-col gap-1.5 rounded-md border border-border/70 bg-muted/40 px-3 py-2 type-footnote"
                data-node-harness-missing-command
                data-missing-command-kind={missingDiag.kind}
              >
                {missingDiag.cli_service_path ? (
                  <div className="flex flex-col gap-0.5">
                    <span className="text-muted-foreground">
                      {t("devices:harness.missingCommand.cliFoundInServicePath")}
                    </span>
                    <code className="break-all font-mono text-foreground" data-missing-command-cli-path>
                      {missingDiag.cli_service_path}
                    </code>
                  </div>
                ) : missingDiag.login_shell_path ? (
                  <div className="flex flex-col gap-0.5">
                    <span className="text-muted-foreground">
                      {missingDiag.login_shell
                        ? t("devices:harness.missingCommand.cliFoundInLoginShellWithShell", {
                            shell: missingDiag.login_shell,
                          })
                        : t("devices:harness.missingCommand.cliFoundInLoginShell")}
                    </span>
                    <code className="break-all font-mono text-foreground" data-missing-command-login-path>
                      {missingDiag.login_shell_path}
                    </code>
                  </div>
                ) : null}
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t(
                      missingDiag.adapter_package
                        ? "devices:harness.missingCommand.missingAdapterPackage"
                        : "devices:harness.missingCommand.missingAdapterCommand",
                    )}
                  </span>
                  <code className="break-all font-mono text-foreground" data-missing-command-adapter-pkg>
                    {missingDiag.adapter_package ?? missingDiag.command}
                  </code>
                </div>
                {!canInstall && adapterCliCmd && (
                  <div className="flex flex-col gap-0.5">
                    <span className="text-muted-foreground">
                      {t("devices:harness.missingCommand.installAdapterHint")}
                    </span>
                    <code className="break-all font-mono text-foreground" data-missing-command-install-cli>
                      {adapterCliCmd}
                    </code>
                  </div>
                )}
              </div>
            )}
            {missingDiag && missingDiag.kind === "command_not_in_service_path" && (
              <div
                className="flex flex-col gap-1.5 rounded-md border border-border/70 bg-muted/40 px-3 py-2 type-footnote"
                data-node-harness-missing-command
                data-missing-command-kind={missingDiag.kind}
              >
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {missingDiag.login_shell_path
                      ? missingDiag.login_shell
                        ? t("devices:harness.missingCommand.loginShellFoundWithShell", {
                            shell: missingDiag.login_shell,
                          })
                        : t("devices:harness.missingCommand.loginShellFound")
                      : t("devices:harness.missingCommand.loginShellMissing")}
                  </span>
                  {missingDiag.login_shell_path && (
                    <code className="break-all font-mono text-foreground" data-missing-command-login-path>
                      {missingDiag.login_shell_path}
                    </code>
                  )}
                </div>
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t("devices:harness.missingCommand.servicePath")}
                  </span>
                  <code className="break-all font-mono text-foreground/90" data-missing-command-service-path>
                    {missingDiag.service_path || t("devices:harness.missingCommand.emptyPath")}
                  </code>
                </div>
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t("devices:harness.missingCommand.refreshHint")}
                  </span>
                  <code className="break-all font-mono text-foreground" data-missing-command-refresh>
                    {missingDiag.refresh_command}
                  </code>
                </div>
              </div>
            )}
            {missingDiag && missingDiag.kind === "both_missing" && (
              <div
                className="flex flex-col gap-1.5 rounded-md border border-border/70 bg-muted/40 px-3 py-2 type-footnote"
                data-node-harness-missing-command
                data-missing-command-kind={missingDiag.kind}
              >
                <span className="text-muted-foreground">
                  {t("devices:harness.missingCommand.loginShellMissing")}
                </span>
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t("devices:harness.missingCommand.servicePath")}
                  </span>
                  <code className="break-all font-mono text-foreground/90" data-missing-command-service-path>
                    {missingDiag.service_path || t("devices:harness.missingCommand.emptyPath")}
                  </code>
                </div>
              </div>
            )}
            {showInstallExecDenied && (
              <ExecDeniedNotice local={v.local} context="install" onOpenTab={onOpenTab} />
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {primaryAction === "copy_refresh_env" && refreshEnvCmd ? (
              <>
                <Button
                  size="sm"
                  onClick={() => void copyCommand(refreshEnvCmd, "refresh")}
                  data-harness-attention-action="copy_refresh_env"
                  data-harness-refresh-env-copy={item.id}
                >
                  {copiedKey === "refresh" ? <CheckIcon /> : <CopyIcon />}
                  {t(copiedKey === "refresh" ? "devices:harness.action.copiedRefreshEnv" : "devices:harness.action.copyRefreshEnv")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy || !v.linked || v.state === "pending" || v.state === "revoked"}
                  onClick={onRefresh}
                  data-harness-attention-action="refresh"
                >
                  {busy ? <Spinner /> : <RefreshCwIcon />}
                  {t("devices:harness.refresh")}
                </Button>
              </>
            ) : primaryAction === "copy_adapter_install" && adapterCliCmd ? (
              <>
                <Button
                  size="sm"
                  onClick={() => void copyCommand(adapterCliCmd, "install")}
                  data-harness-attention-action="copy_adapter_install"
                  data-harness-install-copy={item.id}
                >
                  {copiedKey === "install" ? <CheckIcon /> : <CopyIcon />}
                  {t(copiedKey === "install" ? "devices:harness.action.copiedInstall" : "devices:harness.action.copyInstall")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy || !v.linked || v.state === "pending" || v.state === "revoked"}
                  onClick={onRefresh}
                  data-harness-attention-action="refresh"
                >
                  {busy ? <Spinner /> : <RefreshCwIcon />}
                  {t("devices:harness.refresh")}
                </Button>
              </>
            ) : primaryAction === "upgrade" && update && updates ? (
              <Button
                size="sm"
                disabled={upgradeDisabled || uStatus === "running" || uStatus === "waiting"}
                onClick={() => void updates.upgrade(update, uStatus === "unknown")}
                data-harness-attention-action="upgrade"
                data-harness-upgrade={item.id}
              >
                {updates.requesting === item.id || uStatus === "running" || uStatus === "waiting" ? <Spinner /> : <DownloadIcon />}
                {t("devices:harnessUpdates.upgrade")}
              </Button>
            ) : installing ? (
              <Button
                size="sm"
                disabled
                data-harness-attention-action="installing"
                data-harness-install={item.id}
              >
                <Spinner />
                {t("devices:harness.action.installing")}
              </Button>
            ) : primaryAction === "install" || primaryAction === "retry_install" || (reason === "probe_unknown" && canInstall) ? (
              <>
                <Button
                  size="sm"
                  disabled={installing || !canInstall}
                  onClick={onInstall}
                  data-harness-attention-action={primaryAction === "retry_install" ? "retry_install" : "install"}
                  data-harness-install={item.id}
                >
                  {installing ? <Spinner /> : <DownloadIcon />}
                  {t(
                    installing
                      ? "devices:harness.action.installing"
                      : primaryAction === "retry_install"
                        ? "devices:harness.action.retryInstall"
                        : reason === "adapter_package_missing"
                          ? "devices:harness.action.installAdapter"
                          : "devices:harness.action.install",
                  )}
                </Button>
                {reason === "adapter_package_missing" && (
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy || !v.linked || v.state === "pending" || v.state === "revoked"}
                    onClick={onRefresh}
                    data-harness-attention-action="refresh"
                  >
                    {busy ? <Spinner /> : <RefreshCwIcon />}
                    {t("devices:harness.refresh")}
                  </Button>
                )}
              </>
            ) : (
              <Button
                size="sm"
                variant="outline"
                disabled={busy || !v.linked || v.state === "pending" || v.state === "revoked"}
                onClick={onRefresh}
                data-harness-attention-action="refresh"
              >
                {busy ? <Spinner /> : <RefreshCwIcon />}
                {t(
                  reason === "probe_unknown"
                    ? "devices:harness.action.probeNow"
                    : reason === "installing"
                      ? "devices:harness.action.refreshStatus"
                      : "devices:harness.refresh",
                )}
              </Button>
            )}

            {reason === "adapter_missing" && installCmd && primaryAction !== "install" && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void copyCommand(installCmd, "install")}
                data-harness-install-copy={item.id}
              >
                {copiedKey === "install" ? <CheckIcon /> : <CopyIcon />}
                {t(copiedKey === "install" ? "devices:harness.action.copiedInstall" : "devices:harness.action.copyInstall")}
              </Button>
            )}
          </div>
        </div>
      </IntegrationSection>

      {showSeparateUpgradeBanner && update && updates && (
        <HarnessUpgradeBanner
          h={update}
          receipt={receipt}
          requesting={updates.requesting === item.id || updates.requesting === "__all__"}
          disabled={upgradeDisabled}
          retry={!!updates.data?.retryable_unknown?.includes(item.id)}
          showButton={primaryAction !== "upgrade"}
          local={v.local}
          onOpenTab={onOpenTab}
          upgrade={(retry) => void updates.upgrade(update, retry)}
        />
      )}

      <HarnessDetailsSection
        item={item}
        localRuntime={localRuntime}
        updates={updates}
        policy={policy}
        showReportedModels
      />
    </IntegrationSettings>
  )
}

/* ───────────────────── Section 3: Compact Not Installed Row ───────────────────── */

function NotInstalledHarnessRow({
  item,
  canManageInstall,
  installing,
  localRuntime,
  onInstall,
}: {
  item: MergedNodeHarness
  canManageInstall: boolean
  installing: boolean
  localRuntime?: RRuntime
  onInstall: () => void
}) {
  const t = useT()
  const [copied, setCopied] = useState(false)
  const { probe } = item
  const canInstall = canManageInstall && !!probe?.setup?.can_install
  const installCmd = formatInstallCommand(probe?.install_hint)
  const probeDetails = probe
    ? [...new Set([probe.setup?.reason, probe.setup?.error, probe.error, probe.setup?.help].filter(Boolean))]
    : []
  const components = probe?.setup?.components ?? []
  const hasDetails = probeDetails.length > 0 || components.length > 0 || !!localRuntime?.path

  const copyCommand = async () => {
    if (!installCmd) return
    try {
      await navigator.clipboard.writeText(installCmd)
      setCopied(true)
      window.setTimeout(() => setCopied(false), COPY_FEEDBACK_MS)
    } catch {
      toast(t("devices:diagnostics.copyFailed"))
    }
  }

  return (
    <div
      className="flex min-w-0 flex-col gap-1.5 px-4 py-2.5"
      data-node-harness-id={item.id}
      data-node-harness-state="not_installed"
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
        <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted/70 text-muted-foreground">
          <HarnessMark harness={item.id} className="size-4" />
        </span>
        <div className="min-w-0 flex-1 basis-40">
          <div className="type-callout font-medium text-foreground [overflow-wrap:anywhere]">{item.name}</div>
          <p className="truncate type-footnote text-muted-foreground" title={installCmd ?? undefined}>
            {installCmd
              ? t("devices:harness.notInstalledHint", { cmd: installCmd })
              : t("devices:harness.notInstalledDesc")}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {canInstall ? (
            <Button
              size="xs"
              variant="outline"
              disabled={installing}
              onClick={onInstall}
              data-harness-install={item.id}
            >
              {installing ? <Spinner className="size-3" /> : <DownloadIcon />}
              {t(installing ? "devices:harness.action.installing" : "devices:harness.action.install")}
            </Button>
          ) : installCmd ? (
            <Button
              size="xs"
              variant="outline"
              onClick={() => void copyCommand()}
              data-harness-install-copy={item.id}
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
              {t(copied ? "devices:harness.action.copiedInstall" : "devices:harness.action.copyInstall")}
            </Button>
          ) : null}
        </div>
      </div>

      {hasDetails && (
        <details className="pl-10 type-footnote text-muted-foreground" data-node-harness-details>
          <summary className="cursor-pointer select-none">{t("devices:harness.details")}</summary>
          <div className="mt-1.5 flex flex-col gap-1.5 break-words whitespace-pre-wrap">
            {localRuntime?.path && <p className="break-all font-mono">{localRuntime.path}</p>}
            {probeDetails.map((d) => (
              <p key={d}>{d}</p>
            ))}
            {components.length > 0 && <ComponentEvidenceList components={components} />}
          </div>
        </details>
      )}
    </div>
  )
}

/* ───────────────────── Structured Model Summary & Drawer ───────────────────── */

function HarnessModelsSummary({
  models,
  defaultModel,
}: {
  models: readonly string[]
  defaultModel: string | null
}) {
  const t = useT()
  const fieldLabels = useFieldLabels()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState("")
  const drawerId = useId()

  const structured = useMemo(() => structureHarnessModels(models, defaultModel), [models, defaultModel])
  const filtered = useMemo(
    () => (open ? filterModelGroups(structured.groups, query) : { groups: structured.groups, matchedRawCount: structured.totalRaw }),
    [open, structured.groups, structured.totalRaw, query],
  )

  if (structured.totalRaw === 0 && !structured.defaultModel) {
    return (
      <p className="type-footnote text-muted-foreground" data-node-harness-models>
        {t("devices:harness.noModels")}
      </p>
    )
  }

  const hasExtraModels =
    structured.totalRaw > structured.familyChips.length ||
    structured.groups.some((g) => g.entries.some((e) => e.snapshots.length > 0)) ||
    (structured.groups.some((g) => g.family === OTHER_FAMILY) &&
      structured.familyChips.every((c) => c.family !== OTHER_FAMILY))

  return (
    <div className="flex min-w-0 flex-col gap-2" data-node-harness-models>
      {structured.defaultModel && (
        <div className="flex min-w-0 flex-wrap items-center gap-1.5" data-harness-default-model={structured.defaultModel}>
          <Badge tone="info" size="sm">{t("devices:harness.defaultModel")}</Badge>
          <span className="inline-flex min-w-0 max-w-full items-center gap-1.5 font-mono text-xs font-medium text-foreground">
            <ModelMark model={structured.defaultModel} className="size-3.5 shrink-0" />
            <span className="truncate" title={structured.defaultModel}>{structured.defaultModel}</span>
          </span>
        </div>
      )}

      {structured.familyChips.length > 0 && (
        <div className="flex min-w-0 flex-wrap items-center gap-1.5" data-harness-family-chips>
          {structured.familyChips.map((chip) => (
            <FamilyLatestChip key={`${chip.family}:${chip.latestId}`} chip={chip} />
          ))}
          {hasExtraModels && (
            <Button
              size="xs"
              variant="ghost"
              aria-expanded={open}
              aria-controls={drawerId}
              onClick={() => setOpen((v) => !v)}
              data-harness-models-toggle
            >
              {t(open ? "devices:harness.hideModels" : "devices:harness.allModels", { count: structured.totalRaw })}
              <ChevronDownIcon className={cn("size-3.5 transition-transform duration-150", open && "rotate-180")} aria-hidden />
            </Button>
          )}
        </div>
      )}

      {open && (
        <div
          id={drawerId}
          className="mt-1 flex min-w-0 flex-col gap-3 rounded-lg border border-border/70 bg-muted/25 p-3"
          data-harness-models-drawer
        >
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <TextField
              size="sm"
              className="min-w-0 flex-1 basis-48"
              prefix={<SearchIcon className="size-3.5 text-muted-foreground" aria-hidden />}
              placeholder={t("devices:harness.modelsSearchPlaceholder")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onClear={() => setQuery("")}
              clearable
              labels={fieldLabels}
              data-harness-models-search
            />
            <span className="shrink-0 type-footnote tabular-nums text-muted-foreground" data-harness-models-match-count>
              {query.trim()
                ? t("devices:harness.modelsSearchCount", { shown: filtered.matchedRawCount, total: structured.totalRaw })
                : t("devices:harness.allModels", { count: structured.totalRaw })}
            </span>
          </div>

          {filtered.groups.length === 0 ? (
            <p className="py-2 text-center type-footnote text-muted-foreground" data-harness-models-empty>
              {t("devices:harness.modelsSearchEmpty")}
            </p>
          ) : (
            <div className="flex max-h-[420px] min-w-0 flex-col gap-3 overflow-y-auto pr-1">
              {filtered.groups.map((group) => (
                <div
                  key={group.family}
                  className="flex min-w-0 flex-col gap-1.5"
                  data-model-family-group={group.family}
                >
                  <div className="flex items-center gap-1.5">
                    <ModelMark model={group.latest.id} className="size-3.5 shrink-0" />
                    <span className="type-footnote font-semibold text-foreground">
                      {group.label ?? t("devices:harness.modelFamilyOther")}
                    </span>
                    <Badge tone="neutral" count={group.rawCount} />
                  </div>
                  <ul className="flex min-w-0 flex-col divide-y divide-border/40 rounded-md border border-border/50 bg-card/60">
                    {group.entries.map((entry) => (
                      <ModelEntryRow key={`${entry.source}:${entry.id}`} entry={entry} query={query} />
                    ))}
                  </ul>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function FamilyLatestChip({ chip }: { chip: ModelFamilyChip }) {
  const t = useT()
  const shownText = chip.family === OTHER_FAMILY ? chip.latestId : stripSourcePrefixForChip(chip.latestId, chip.source)
  return (
    <span
      className="harness-model-chip"
      title={chip.latestId}
      data-harness-family-chip={chip.family}
    >
      <ModelMark model={chip.latestId} className="size-3 shrink-0" />
      <span className="truncate font-mono">{shownText}</span>
      {chip.source !== "direct" && (
        <span className="harness-chip-source" data-model-source={chip.source}>
          {t(`devices:harness.modelSource.${chip.source}`)}
        </span>
      )}
      {chip.rawCount > 1 && <span className="harness-chip-count">{chip.rawCount}</span>}
    </span>
  )
}

function ModelEntryRow({ entry, query }: { entry: GroupedModelEntry; query: string }) {
  const t = useT()
  const needle = query.trim().toLowerCase()
  const snapshotMatch = !!needle && entry.snapshots.some((s) => s.id.toLowerCase().includes(needle))
  const [snapOpen, setSnapOpen] = useState(false)
  const showSnaps = snapOpen || snapshotMatch

  return (
    <li className="flex min-w-0 flex-col gap-1 px-2.5 py-1.5" data-model-entry={entry.id}>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        <span className="min-w-0 break-all font-mono text-xs text-foreground">{entry.id}</span>
        {entry.isDefault && <Badge tone="info" size="sm">{t("devices:harness.defaultModel")}</Badge>}
        {entry.source !== "direct" && (
          <Badge tone={entry.source === "local" ? "neutral" : "purple"} size="sm" data-model-source={entry.source}>
            {t(`devices:harness.modelSource.${entry.source}`)}
          </Badge>
        )}
        {entry.snapshots.length > 0 && (
          <button
            type="button"
            className="harness-snap-toggle"
            aria-expanded={showSnaps}
            onClick={() => setSnapOpen((v) => !v)}
            data-model-snapshots-toggle={entry.id}
          >
            {t("devices:harness.modelSnapshots", { count: entry.snapshots.length })}
          </button>
        )}
      </div>
      {showSnaps && entry.snapshots.length > 0 && (
        <ul className="flex flex-wrap gap-1.5 pl-2" data-model-snapshots-list={entry.id}>
          {entry.snapshots.map((snap) => (
            <li key={snap.id} className="rounded bg-muted/80 px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground break-all">
              {snap.id}
            </li>
          ))}
        </ul>
      )}
    </li>
  )
}

/* ───────────────────── Unified Secondary Details Fold ───────────────────── */

function ComponentEvidenceList({ components }: { components: NonNullable<RNodeHarness["setup"]>["components"] }) {
  const t = useT()
  return (
    <div className="flex flex-col gap-2">
      <span className="font-medium text-foreground">{t("shell:runtimeSetup.components")}</span>
      {components.map((c) => (
        <div key={c.id} className="flex min-w-0 flex-col gap-0.5" data-node-harness-component={c.id}>
          <span className="flex flex-wrap items-center gap-1.5">
            <span className="font-medium text-foreground">{c.name}</span>
            <Badge
              tone={c.status === "installed" ? "success" : c.status === "unknown" ? "neutral" : "warning"}
              data-component-status={c.status}
            >
              {t(
                c.status === "installed"
                  ? "shell:runtimeSetup.installed"
                  : c.status === "unknown"
                    ? "shell:runtimeSetup.componentUnknown"
                    : "shell:runtimeSetup.state.not_installed",
              )}
            </Badge>
            <span className="font-mono text-muted-foreground">{c.version || t("devices:harness.versionUnknown")}</span>
          </span>
          {c.package && <span className="break-all font-mono">{c.package}</span>}
          {c.path && (
            <p className="break-all" data-component-target>
              {t("shell:runtimeSetup.componentTarget")} <span className="font-mono">{c.path}</span>
            </p>
          )}
          {c.status === "unknown" && (
            <p className="break-words" data-component-evidence>{t("shell:runtimeSetup.componentUnknownHelp")}</p>
          )}
          {c.detected && (
            <div className="flex min-w-0 flex-col gap-0.5" data-component-detected>
              <span className="break-words">{t("shell:runtimeSetup.otherInstallation")}</span>
              <span className="break-all font-mono">
                {c.detected.path}
                {c.detected.version ? ` (${c.detected.version})` : ""}
              </span>
              {c.status !== "installed" && (
                <span className="break-words">{t("shell:runtimeSetup.targetIncomplete")}</span>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  )
}

function HarnessDetailsSection({
  item,
  localRuntime,
  updates,
  policy,
  showReportedModels,
}: {
  item: MergedNodeHarness
  localRuntime?: RRuntime
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  showReportedModels: boolean
}) {
  const t = useT()
  const { probe, update, receipt } = item
  const hideReadyHelp = !!probe?.available && probe.setup?.state === "ready"
  const helpText = !hideReadyHelp ? probe?.setup?.help : undefined
  const probeDetails = probe
    ? [...new Set([probe.setup?.reason, probe.setup?.error, probe.error].filter(Boolean) as string[])]
    : []
  const componentMeta = (update?.components ?? []).filter((c) => c.upgrade_command || c.package || c.checked)
  const reportedModels = showReportedModels && probe?.models.length ? probe.models : []
  const canEditAuto = st.owner && !!updates?.data && !updates.unavailable
  const caps = localRuntime?.caps
  const capChips = caps
    ? ([
        ["sessions", caps.sessions],
        ["judge", caps.jobs],
        ["images", caps.images],
        ["resume", caps.resume],
        ["history", caps.history],
      ] as const)
        .filter(([, on]) => on)
        .map(([k]) => t(`shell:devices.harness.cap.${k}`))
    : []
  const modeChips = (localRuntime?.modes ?? []).map((m) => t(`shell:devices.harness.mode.${m}`, { defaultValue: m }))
  const localChips = [...capChips, ...modeChips]

  const hasDetails =
    reportedModels.length > 0 ||
    !!update ||
    probeDetails.length > 0 ||
    !!helpText ||
    !!probe?.setup?.components.length ||
    componentMeta.length > 0 ||
    !!receipt ||
    !!localRuntime

  if (!hasDetails) return null

  return (
    <IntegrationSection>
      <details className="type-footnote text-muted-foreground" data-node-harness-details>
        <summary className="cursor-pointer select-none font-medium text-muted-foreground hover:text-foreground">
          {t("devices:harness.details")}
        </summary>
        <div className="mt-2.5 flex min-w-0 flex-col gap-3 break-words">
          {reportedModels.length > 0 && (
            <div className="flex min-w-0 flex-col gap-1" data-node-harness-reported-models>
              <span className="font-medium text-foreground">{t("devices:harness.reportedModels")}</span>
              <p className="break-words font-mono text-xs text-muted-foreground">{reportedModels.join(" · ")}</p>
            </div>
          )}

          {update && updates && (
            <HarnessUpgradeDetails
              h={update}
              policy={policy}
              savingAuto={updates.savingAuto !== null}
              canEditAuto={canEditAuto}
              onAutoMode={(mode) => void updates.setHarnessAutoUpgrade(item.id, mode)}
            />
          )}

          {localRuntime && (
            <div className="flex min-w-0 flex-col gap-1">
              <span>{t(localRuntime.custom ? "shell:devices.harness.custom" : "shell:devices.harness.builtin")}</span>
              {localRuntime.auth && localRuntime.auth.length > 0 && (
                <span data-node-harness-auth>
                  {t("shell:devices.harness.auth", { methods: localRuntime.auth.map((a) => a.name).join(t("shell:list.sep")) })}
                </span>
              )}
              {localRuntime.note && <span className="break-words whitespace-pre-wrap">{localRuntime.note}</span>}
              {localRuntime.path && <span className="break-all font-mono">{localRuntime.path}</span>}
              {localChips.length > 0 && (
                <span className="mt-0.5 flex flex-wrap gap-1" data-node-harness-caps>
                  {localChips.map((c) => <Badge key={c}>{c}</Badge>)}
                </span>
              )}
            </div>
          )}

          {(probeDetails.length > 0 || helpText) && (
            <div className="flex flex-col gap-1 whitespace-pre-wrap">
              {probeDetails.map((d) => (
                <p key={d}>{d}</p>
              ))}
              {helpText && <p data-harness-help>{helpText}</p>}
            </div>
          )}

          {!!probe?.setup?.components.length && <ComponentEvidenceList components={probe.setup.components} />}

          {componentMeta.length > 0 && (
            <div className="flex flex-col gap-1.5">
              {componentMeta.map((c) => (
                <div key={`meta:${c.id}`} className="flex flex-col gap-0.5">
                  <span className="font-medium text-foreground">{c.name}</span>
                  {c.upgrade_command && (
                    <p className="break-all font-mono" data-component-command>
                      {t("devices:harnessUpdates.command", { cmd: c.upgrade_command })}
                    </p>
                  )}
                  {c.package && <p className="break-all font-mono">{c.package}</p>}
                  {c.checked && <p>{t("devices:harnessUpdates.checked", { at: monthDayTime(c.checked) })}</p>}
                </div>
              ))}
            </div>
          )}

          {receipt && (
            <p className="break-all font-mono" data-harness-upgrade-operation>
              {receipt.operation}
            </p>
          )}
        </div>
      </details>
    </IntegrationSection>
  )
}
