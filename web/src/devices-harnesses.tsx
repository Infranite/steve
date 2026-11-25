import { useMemo, useState, type ReactNode } from "react"
import { CpuIcon, DownloadIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/affect/badge-clean"
import { Spinner } from "@/components/ui/spinner"
import { IntegrationSection, IntegrationSettings } from "@/components/affect/integration-settings-clean"
import { HarnessMark } from "@/components/harness-mark"
import { useT } from "@/i18n"
import { date, monthDayTime } from "@/i18n/format"
import type { NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard } from "@/devices-ui"
import { harnessState, harnessSummary } from "@/node-harness-model"
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

type HarnessActionSlice = Pick<NodeActions, "harnessBusy" | "harnessError" | "probeHarnesses" | "harnessUpdates">

/** Unified per-Node Harness inventory: probe readiness, available models, component versions,
 * official one-click upgrades, and auto-upgrade switches live together in one card per harness. */
export function NodeHarnesses({
  v,
  actions,
  children,
}: {
  v: NodeView
  actions?: HarnessActionSlice
  children?: ReactNode
}) {
  useStore()
  const t = useT(), { harnesses, harnessesError, refreshHarnesses } = useDevices()
  const r = harnesses?.[v.id], [ownBusy, setOwnBusy] = useState(false), [ownError, setOwnError] = useState("")
  const busy = actions ? actions.harnessBusy : ownBusy
  const error = actions ? actions.harnessError : ownError
  const updates = actions?.harnessUpdates
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
  const showUpdatesChrome = !!updates && v.state !== "revoked"
  const updatesUnavailable = !!updates?.unavailable

  return (
    <DCard
      icon={<CpuIcon />}
      title={t("devices:harness.title")}
      sub={harnessSummary(v, r)}
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
          <div className="px-2 type-footnote text-attention" role="alert" data-harness-update-error>
            <p>
              {t(
                updates.data?.code === "unsupported"
                  ? "devices:harnessUpdates.oldNode"
                  : "devices:harnessUpdates.unavailable",
              )}
            </p>
            {updates.data?.report && <p>{t("devices:harnessUpdates.stale")}</p>}
            <details className="mt-1">
              <summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>
              <p className="break-words whitespace-pre-wrap">{updates.message || updates.error || updates.data?.error}</p>
            </details>
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
          <p className="type-footnote text-muted-foreground" data-node-harness-degraded>
            {t(`devices:harness.degraded.${probe.degraded_reason}`)}
          </p>
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
