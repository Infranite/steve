import { useId, useMemo, useState, type ReactNode } from "react"
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
import { api } from "@/api"
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
  attentionActionFor,
  effectiveDegradedReason,
  formatInstallCommand,
  harnessSummary,
  partitionNodeHarnesses,
} from "@/node-harness-model"
import type { NodeTab } from "@/node-detail-model"
import { st, toast, useStore } from "@/store"
import {
  DEFAULT_AUTO_POLICY,
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

function stripSourcePrefixForChip(id: string, source: ModelSource): string {
  if (source === "direct") return id
  return id.replace(/^(?:passthrough\/local\/|local\/passthrough\/|passthrough\/|local\/)/i, "")
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
  const r = harnesses?.[v.id], [ownBusy, setOwnBusy] = useState(false), [ownError, setOwnError] = useState("")
  const [installingId, setInstallingId] = useState<string | null>(null)
  const [availableOpen, setAvailableOpen] = useState(true)
  const [attentionOpen, setAttentionOpen] = useState(true)
  const [notInstalledOpen, setNotInstalledOpen] = useState(false)

  const busy = actions ? actions.harnessBusy : ownBusy
  const error = actions ? actions.harnessError : ownError
  const updates = actions?.harnessUpdates
  const upgradeOnly = isUpgradeOnlyLink(v) || r?.state === "upgrade_required"
  const stale = !v.linked || !!r?.stale || !!harnessesError || r?.state !== "reported"

  const refresh = async () => {
    if (actions) { await actions.probeHarnesses(); return }
    setOwnBusy(true); setOwnError("")
    try { await refreshHarnesses(v.id) } catch (e) { setOwnError((e as Error).message) } finally { setOwnBusy(false) }
  }

  const installLocal = async (id: string) => {
    if (!v.local || !st.owner || installingId) return
    setInstallingId(id)
    try {
      const res = await api.installHarness(id)
      if (!res.accepted) toast(t("devices:harness.action.installNotAccepted"))
      await refresh()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setInstallingId(null)
    }
  }

  const { available, attention, notInstalled } = useMemo(
    () => partitionNodeHarnesses(mergeNodeHarnesses(r, updates?.data)),
    [r, updates?.data],
  )
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

        {(error || harnessesError) && (
          <p role="alert" className="px-2 type-footnote text-attention" data-node-harness-error>
            {t("devices:harness.readFailed")}
          </p>
        )}
        {(error || harnessesError || r?.error) && (
          <details className="px-2 type-footnote text-muted-foreground" data-node-harness-error-details>
            <summary className="cursor-pointer">{t("devices:harness.details")}</summary>
            <p className="mt-1 break-words whitespace-pre-wrap">{error || harnessesError || r?.error}</p>
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
                  updates={showUpdatesChrome ? updates : undefined}
                  policy={policy}
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
                  installing={installingId === item.id}
                  updates={showUpdatesChrome ? updates : undefined}
                  policy={policy}
                  onRefresh={() => void refresh()}
                  onInstall={() => void installLocal(item.id)}
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
                    v={v}
                    item={item}
                    installing={installingId === item.id}
                    onInstall={() => void installLocal(item.id)}
                  />
                ))}
              </div>
            </Card>
          </HarnessSectionGroup>
        )}

        {children && (
          <div className="mt-1" data-node-harness-manage>
            <p className="mb-3 px-2 type-footnote text-muted-foreground">{t("devices:harness.manageLocal")}</p>
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
  updates,
  policy,
}: {
  v: NodeView
  item: MergedNodeHarness
  stale: boolean
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
}) {
  const t = useT()
  const { probe, update, receipt } = item
  const uStatus = receiptStatus(receipt)
  const hasUpdate = update ? Object.keys(updateTargets(update)).length > 0 : false
  const selfOnly = update ? !hasUpdate && selfUpdateTargets(update).length > 0 : false
  const activeSessions = update?.active_sessions ?? 0
  const autoOn = !!update && supportsAutoUpgrade(update) && effectiveAutoUpgrade(policy, item.id)

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
          <HarnessModelsSummary models={probe.models} defaultModel={probe.default_model ?? null} />
        </IntegrationSection>
      )}

      {update && updates && (
        <HarnessUpgradeBanner
          h={update}
          receipt={receipt}
          requesting={updates.requesting === item.id || updates.requesting === "__all__"}
          disabled={upgradeDisabled}
          retry={!!updates.data?.retryable_unknown?.includes(item.id)}
          upgrade={(retry) => void updates.upgrade(update, retry)}
        />
      )}

      <HarnessDetailsSection
        item={item}
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
  installing,
  updates,
  policy,
  onRefresh,
  onInstall,
}: {
  v: NodeView
  item: MergedNodeHarness
  stale: boolean
  busy: boolean
  installing: boolean
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  onRefresh: () => void
  onInstall: () => void
}) {
  const t = useT()
  const [copiedKey, setCopiedKey] = useState<"install" | "refresh" | null>(null)
  const { probe, update, receipt } = item
  const reason = effectiveDegradedReason(item) ?? "probe_unknown"
  const uStatus = receiptStatus(receipt)
  const hasUpdate = update ? Object.keys(updateTargets(update)).length > 0 : false

  const canInstallLocal = v.local && st.owner && !!probe?.setup?.can_install && !installing
  const upgradeDisabled =
    !updates ||
    !st.owner ||
    !v.linked ||
    v.state !== "active" ||
    updates.unavailable ||
    !!updates.requesting
  const canUpgradeNow = !!update && !!updates && !upgradeDisabled && update.can_upgrade

  const primaryAction = attentionActionFor(item, reason, {
    canInstallLocal,
    canUpgrade: canUpgradeNow,
  })
  const installCmd = formatInstallCommand(probe?.install_hint)
  const refreshEnvCmd = probe?.missing_command?.refresh_command?.trim() || null

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

  const showSeparateUpgradeBanner =
    !!update && !!updates && (primaryAction !== "upgrade" || !!receipt || !!update.blocked_reason)

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
            <p className="type-footnote text-foreground/90">
              {t(`devices:harness.degraded.${reason}`)}
            </p>
            {probe?.missing_command && (
              <div
                className="flex flex-col gap-1.5 rounded-md border border-border/70 bg-muted/40 px-3 py-2 type-footnote"
                data-node-harness-missing-command
              >
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {probe.missing_command.login_shell_path
                      ? probe.missing_command.login_shell
                        ? t("devices:harness.missingCommand.loginShellFoundWithShell", {
                            shell: probe.missing_command.login_shell,
                          })
                        : t("devices:harness.missingCommand.loginShellFound")
                      : t("devices:harness.missingCommand.loginShellMissing")}
                  </span>
                  {probe.missing_command.login_shell_path && (
                    <code className="break-all font-mono text-foreground" data-missing-command-login-path>
                      {probe.missing_command.login_shell_path}
                    </code>
                  )}
                </div>
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t("devices:harness.missingCommand.servicePath")}
                  </span>
                  <code className="break-all font-mono text-foreground/90" data-missing-command-service-path>
                    {probe.missing_command.service_path || t("devices:harness.missingCommand.emptyPath")}
                  </code>
                </div>
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground">
                    {t("devices:harness.missingCommand.refreshHint")}
                  </span>
                  <code className="break-all font-mono text-foreground" data-missing-command-refresh>
                    {probe.missing_command.refresh_command}
                  </code>
                </div>
              </div>
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
            ) : primaryAction === "install" || primaryAction === "retry_install" ? (
              <Button
                size="sm"
                disabled={installing || !canInstallLocal}
                onClick={onInstall}
                data-harness-attention-action={primaryAction}
                data-harness-install={item.id}
              >
                {installing ? <Spinner /> : <DownloadIcon />}
                {t(
                  installing
                    ? "devices:harness.action.installing"
                    : primaryAction === "retry_install"
                      ? "devices:harness.action.retryInstall"
                      : "devices:harness.action.install",
                )}
              </Button>
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
          upgrade={(retry) => void updates.upgrade(update, retry)}
        />
      )}

      <HarnessDetailsSection
        item={item}
        updates={updates}
        policy={policy}
        showReportedModels
      />
    </IntegrationSettings>
  )
}

/* ───────────────────── Section 3: Compact Not Installed Row ───────────────────── */

function NotInstalledHarnessRow({
  v,
  item,
  installing,
  onInstall,
}: {
  v: NodeView
  item: MergedNodeHarness
  installing: boolean
  onInstall: () => void
}) {
  const t = useT()
  const [copied, setCopied] = useState(false)
  const { probe } = item
  const canInstallLocal = v.local && st.owner && !!probe?.setup?.can_install
  const installCmd = formatInstallCommand(probe?.install_hint)
  const probeDetails = probe
    ? [...new Set([probe.setup?.reason, probe.setup?.error, probe.error, probe.setup?.help].filter(Boolean))]
    : []

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
          {canInstallLocal ? (
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

      {probeDetails.length > 0 && (
        <details className="pl-10 type-footnote text-muted-foreground" data-node-harness-details>
          <summary className="cursor-pointer select-none">{t("devices:harness.details")}</summary>
          <div className="mt-1.5 flex flex-col gap-1 break-words whitespace-pre-wrap">
            {probeDetails.map((d) => (
              <p key={d}>{d}</p>
            ))}
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

function HarnessDetailsSection({
  item,
  updates,
  policy,
  showReportedModels,
}: {
  item: MergedNodeHarness
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  showReportedModels: boolean
}) {
  const t = useT()
  const { probe, update, receipt } = item
  const probeDetails = probe
    ? [...new Set([probe.setup?.reason, probe.setup?.error, probe.error, probe.setup?.help].filter(Boolean))]
    : []
  const componentMeta = (update?.components ?? []).filter((c) => c.upgrade_command || c.package || c.checked)
  const reportedModels = showReportedModels && probe?.models.length ? probe.models : []
  const canEditAuto = st.owner && !!updates?.data && !updates.unavailable

  const hasDetails =
    reportedModels.length > 0 ||
    !!update ||
    probeDetails.length > 0 ||
    !!probe?.setup?.components.length ||
    componentMeta.length > 0 ||
    !!receipt

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

          {probeDetails.length > 0 && (
            <div className="flex flex-col gap-1 whitespace-pre-wrap">
              {probeDetails.map((d) => (
                <p key={d}>{d}</p>
              ))}
            </div>
          )}

          {!!probe?.setup?.components.length && (
            <div className="flex flex-col gap-1.5">
              {probe.setup.components.map((c) => (
                <div key={c.id} data-node-harness-component={c.id}>
                  <span>
                    {c.name} · {c.version || t("devices:harness.versionUnknown")} ·{" "}
                    {t(
                      c.status === "installed"
                        ? "devices:harness.componentInstalled"
                        : c.status === "unknown"
                          ? "devices:harness.state.unknown"
                          : "devices:harness.state.not_installed",
                    )}
                  </span>
                  {c.path && <p className="break-all font-mono">{c.path}</p>}
                  {c.detected && (
                    <p className="break-all font-mono">
                      {c.detected.path}
                      {c.detected.version ? ` (${c.detected.version})` : ""}
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}

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
