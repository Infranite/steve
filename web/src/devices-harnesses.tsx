import { useMemo, useState, type ReactNode } from "react"
import { ArrowUpCircleIcon, CpuIcon, DownloadIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/affect/badge-clean"
import { EmptyState } from "@/components/affect/empty-state-clean"
import { Spinner } from "@/components/ui/spinner"
import { IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { HarnessMark } from "@/components/harness-mark"
import { useT } from "@/i18n"
import { date, monthDayTime } from "@/i18n/format"
import { isUpgradeOnlyLink, upgradeCard, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard } from "@/devices-ui"
import { UpgradeSteps } from "@/devices-upgrade"
import { harnessState, harnessSummary } from "@/node-harness-model"
import type { NodeTab } from "@/node-detail-model"
import { st, useStore } from "@/store"
import {
  DEFAULT_AUTO_POLICY,
  HarnessUpgradeSections,
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

type HarnessActionSlice = Pick<
  NodeActions,
  "harnessBusy" | "harnessError" | "probeHarnesses" | "harnessUpdates" | "canUpgrade" | "upgradeBusy" | "startUpgrade"
>

/** Unified per-Node Harness inventory: probe readiness, available models, component versions,
 * official one-click upgrades, and auto-upgrade switches live together in one card per harness. */
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

  const { shown, other } = useMemo(
    () => mergeNodeHarnesses(r, updates?.data),
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
      <div className="flex min-w-0 flex-col gap-3 px-3 pb-3" aria-live="polite">
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

        {shown.map((item) => (
          <UnifiedHarnessCard
            key={item.id}
            v={v}
            item={item}
            stale={stale}
            updates={showUpdatesChrome ? updates : undefined}
            policy={policy}
          />
        ))}

        {!!other.length && showUpdatesChrome && (
          <details className="px-2 type-footnote text-muted-foreground" data-harness-update-other>
            <summary className="cursor-pointer">{t("devices:harnessUpdates.other", { count: other.length })}</summary>
            <div className="mt-3 flex flex-col gap-3">
              {other.map((item) => (
                <UnifiedHarnessCard
                  key={item.id}
                  v={v}
                  item={item}
                  stale={stale}
                  updates={updates}
                  policy={policy}
                  readOnly
                />
              ))}
            </div>
          </details>
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

function UnifiedHarnessCard({
  v,
  item,
  stale,
  updates,
  policy,
  readOnly = false,
}: {
  v: NodeView
  item: MergedNodeHarness
  stale: boolean
  updates?: NodeHarnessUpdatesState
  policy: typeof DEFAULT_AUTO_POLICY
  readOnly?: boolean
}) {
  const t = useT()
  const { probe, update, receipt } = item
  const pState = probe ? harnessState(probe) : null
  const uStatus = receiptStatus(receipt)
  const available = update ? Object.keys(updateTargets(update)).length > 0 : false
  const selfOnly = update ? !available && selfUpdateTargets(update).length > 0 : false
  const activeSessions = update?.active_sessions ?? 0
  const autoOn = !!update && supportsAutoUpgrade(update) && effectiveAutoUpgrade(policy, item.id)

  const updateBadge =
    uStatus === "waiting" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.waiting")}</Badge>
    ) : uStatus === "running" ? (
      <Badge tone="info" dot live>{t("devices:harnessUpdates.badge.running")}</Badge>
    ) : available ? (
      <Badge tone="warning">{t("devices:harnessUpdates.available")}</Badge>
    ) : selfOnly ? (
      <Badge tone="neutral">{t("devices:harnessUpdates.badge.checkOnUpgrade")}</Badge>
    ) : null

  const badge = (probe || updateBadge) && (
    <span className="inline-flex flex-wrap items-center gap-1.5">
      {probe && pState && (
        <Badge
          tone={stale || pState === "unknown" ? "neutral" : probe.available ? "success" : "warning"}
          data-node-harness-state={stale ? "stale" : pState}
        >
          {t(`devices:harness.state.${pState}`)}
          {stale && ` · ${t("devices:harness.lastReport")}`}
        </Badge>
      )}
      {updateBadge}
    </span>
  )

  const metaExtras: string[] = []
  if (activeSessions > 0) metaExtras.push(t("devices:harnessUpdates.activeSessionsMeta", { count: activeSessions }))
  if (autoOn) metaExtras.push(t("devices:harnessUpdates.autoEnabledMeta"))

  const meta = (probe || metaExtras.length > 0) && (
    <span className="flex flex-wrap gap-x-3 gap-y-1" data-node-harness-versions>
      {probe && (
        <>
          <span>CLI {probe.version || t("devices:harness.versionUnknown")}</span>
          <span>ACP {probe.adapter_version || t("devices:harness.versionUnknown")}</span>
        </>
      )}
      {metaExtras.map((text) => (
        <span key={text}>{text}</span>
      ))}
    </span>
  )

  const probeDetails = probe ? [...new Set([probe.setup?.reason, probe.setup?.error, probe.error, probe.setup?.help].filter(Boolean))] : []
  const componentMeta = (update?.components ?? []).filter((c) => c.upgrade_command || c.package || c.checked)
  const hasDetails = probeDetails.length > 0 || !!probe?.setup?.components.length || componentMeta.length > 0 || !!receipt

  const upgradeDisabled =
    readOnly ||
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
      meta={meta}
      data-node-harness-id={item.id}
      data-harness-update={item.id}
    >
      {probe?.degraded_reason && (
        <IntegrationSection>
          <div className="flex flex-col gap-2" data-node-harness-degraded>
            <p className="type-footnote text-muted-foreground">
              {t(`devices:harness.degraded.${probe.degraded_reason}`)}
            </p>
            {probe.missing_command && (
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
        </IntegrationSection>
      )}

      {probe && (
        <IntegrationSection label={t(stale || !probe.available ? "devices:harness.reportedModels" : "devices:harness.models")}>
          <p className="break-words type-footnote text-muted-foreground" data-node-harness-models>
            {probe.models.length ? probe.models.join(" · ") : t("devices:harness.noModels")}
          </p>
        </IntegrationSection>
      )}

      {update && updates && (
        <HarnessUpgradeSections
          h={update}
          policy={policy}
          receipt={receipt}
          requesting={updates.requesting === item.id || updates.requesting === "__all__"}
          savingAuto={updates.savingAuto !== null}
          disabled={upgradeDisabled}
          canEditAuto={!readOnly && st.owner && !!updates.data && !updates.unavailable}
          retry={!!updates.data?.retryable_unknown?.includes(item.id)}
          upgrade={(retry) => void updates.upgrade(update, retry)}
          onAutoMode={(mode) => void updates.setHarnessAutoUpgrade(item.id, mode)}
        />
      )}

      {hasDetails && (
        <IntegrationSection>
          <details className="type-footnote text-muted-foreground" data-node-harness-details>
            <summary className="cursor-pointer">{t("devices:harness.details")}</summary>
            <div className="mt-2 flex flex-col gap-2 break-words whitespace-pre-wrap">
              {probeDetails.map((d) => (
                <p key={d}>{d}</p>
              ))}
              {probe?.setup?.components.map((c) => (
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
                </div>
              ))}
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
              {receipt && (
                <p className="break-all font-mono" data-harness-upgrade-operation>
                  {receipt.operation}
                </p>
              )}
            </div>
          </details>
        </IntegrationSection>
      )}
    </IntegrationSettings>
  )
}
