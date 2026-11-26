/* Devices · a Node (#/devices/nodes/<id>[/<tab>]): task-oriented tabs instead of one crowded wall of cards.
 * - Overview: can this Node take work right now? Needs-your-attention pinned at top, live snapshot, 4 module gateways.
 * - Harness: per-Node probes, local runtime setup/install, and CLI/ACP updates.
 * - Work & Resources: active/waiting sessions, token & network & disk usage, 24h charts, dispatch rules.
 * - Access: macOS file access / Full Disk Access, sandbox & execution boundary, personal config, projects, unpair/remove.
 * - Version: current release vs Hub, upgrade progress/policy/actions, upgrade & rollback record.
 * - Diagnostics: sanitized diagnostic copy, local log/status commands, link/sampling diagnostics, identity, raw JSON. */
import { useMemo, useState, type ReactNode } from "react"
import {
  ActivityIcon, ArrowUpCircleIcon, CheckCircle2Icon, ChevronRightIcon, CopyIcon, CpuIcon, DownloadIcon, FolderIcon,
  KeyRoundIcon, LoaderIcon, LockIcon, MessageCircleIcon, MoreHorizontalIcon, PauseIcon, PlayIcon, PlusIcon,
  RotateCcwIcon, ShieldAlertIcon, ShieldCheckIcon, StethoscopeIcon, TerminalIcon, WifiOffIcon, XIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { Input } from "@/components/ui/input"
import { CodeBlock } from "@/components/ui/code-block"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { Switch } from "@/components/affect/switch-clean"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { SetSelect } from "@/settings-controls"
import { st, toast, refresh, useStore, deviceWorkEvidence } from "@/store"
import { api, type AddHarnessModes, type AddHarnessProperties } from "@/api"
import { go, Page, Empty, Loading } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { homeTilde } from "@/lib/path"
import { agoText, findNode, fpGroups, hostLine, hostOf, isUpgradeOnlyLink, linkRoute, nodeStatus, offlineReason, statusText, upgradeCard, type NodeView } from "@/device-model"
import { harnessSummary } from "@/node-harness-model"
import {
  NODE_TABS, nodeActionItems, nodeDiagnosticSnapshot, nodeHref, nodeTabBadges,
  type NodeActionItem, type NodeTab,
} from "@/node-detail-model"
import { useDevices, useNodeVitals } from "@/devices-data"
import { Chip, DCard, DRow, StatusBadge } from "@/devices-ui"
import { NodeFigures, NodeResources, NodeTrends } from "@/devices-vitals"
import { NodeUpgradeHistory, NodeVersion, UpgradeSteps } from "@/devices-upgrade"
import { RunningCard, deviceGroups } from "@/devices-work"
import { TakeWork } from "@/devices-config"
import { NodeContext } from "@/devices-context"
import { LeaveCard, RemoveDialog } from "@/devices-remove-dialog"
import { NodeHarnesses } from "@/devices-harnesses"
import { NodeActionControls, useNodeActions, type NodeActions } from "@/devices-node-actions"
import { cn } from "cn"

const BACK = "#/devices/nodes"

export function NodePage({ id, tab = "overview" }: { id: string; tab?: NodeTab }) {
  const t = useT(), { views, nodesRead, devicesError } = useDevices()
  const v = findNode(views, id)
  const back: [string, string] = [BACK, t("devices:node.back")]
  if (!v) return <Page title={t("devices:title")} back={back}>{nodesRead || devicesError ? <Empty title={t("devices:node.notFound")} /> : <Loading />}</Page>
  return <NodeDetail key={v.id} v={v} tab={tab} />
}

function NodeDetail({ v, tab }: { v: NodeView; tab: NodeTab }) {
  const t = useT(), { hubVersion, harnesses, harnessesError, reloadNodes } = useDevices()
  const { value: vit } = useNodeVitals(v.id)
  useStore()
  const actions = useNodeActions(v)
  const w = actions.work
  const [renaming, setRenaming] = useState(false), [revoking, setRevoking] = useState(false), [removing, setRemoving] = useState(false), [busy, setBusy] = useState(false)

  const status = nodeStatus(v, hubVersion), route = linkRoute(v), facts = hostLine(hostOf(v, vit?.latest))
  const name = v.local ? t("devices:map.localNode") : v.name
  const via = v.local ? t("devices:node.onHub", { name: v.device?.name ?? "" }) : route ? t("devices:node.via", { route: t(`devices:route.${route}`) }) : ""

  const workEvidence = v.device && deviceWorkEvidence().loaded ? deviceGroups(v.device.id) : null
  const attentionCount = workEvidence ? workEvidence.attention.length : 0
  const activeSessions = workEvidence ? workEvidence.attention.length + workEvidence.current.length : (v.running ?? 0)
  const harnessReport = harnesses?.[v.id] ?? null
  const harnessReadyCount = harnessReport?.state === "reported" ? harnessReport.harnesses.filter((h) => h.available).length : 0

  const actionItems = useMemo(
    () => nodeActionItems(v, {
      hubVersion,
      harnessReport,
      harnessesError,
      harnessUpdates: actions.harnessUpdates.data,
      attentionCount,
      canConfirm: actions.canConfirm,
      canUpgrade: actions.canUpgrade,
      canUpgradeHarness: actions.canUpgradeHarness,
    }),
    [
      v,
      hubVersion,
      harnessReport,
      harnessesError,
      actions.harnessUpdates.data,
      attentionCount,
      actions.canConfirm,
      actions.canUpgrade,
      actions.canUpgradeHarness,
    ],
  )
  const tabBadges = useMemo(
    () => nodeTabBadges(v, actionItems, { activeSessions, harnessReadyCount }),
    [v, actionItems, activeSessions, harnessReadyCount],
  )

  const switchTab = (next: NodeTab) => go(nodeHref(v.id, next))
  const revoke = async () => {
    setBusy(true)
    try { await api.revokeNode(v.id); toast(t("devices:node.revoked", { name: v.name })); reloadNodes(); go(BACK) } catch (e) { toast((e as Error).message) } finally { setBusy(false); setRevoking(false) }
  }

  return (
    <div className="device-page">
      <Page max={1040} back={[BACK, t("devices:node.back")]} toolbarTop
        title={<span className="inline-flex flex-wrap items-center gap-x-2.5 gap-y-1">{name}{v.local && <Chip>{t("devices:map.builtIn")}</Chip>}<StatusBadge status={status} /></span>}
        sub={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5" data-node-sub>{[via, facts].filter(Boolean).map((p, i, all) => <span key={i} className="inline-flex items-center gap-2"><span>{p}</span>{i < all.length - 1 && <span className="text-tertiary" aria-hidden>·</span>}</span>)}</span>}
        toolbar={(w.editable || actions.pairable) && <>
          {w.editable && v.state !== "pending" && (
            <Button size="sm" variant="outline" disabled={w.busy} onClick={() => void w.setReceive(v.paused)} data-node-pause={v.paused ? "paused" : "active"}>
              {w.busy ? <Spinner className="size-3.5" /> : v.paused ? <PlayIcon /> : <PauseIcon />}
              {t(v.paused ? "devices:node.resume" : "devices:node.pause")}
            </Button>
          )}
          {actions.pairable && (
            <DropdownMenu>
              <DropdownMenuTrigger render={<Button size="icon-sm" variant="outline" aria-label={t("devices:node.more")} data-node-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => setRenaming(true)} data-node-rename>{t("devices:node.rename")}</DropdownMenuItem>
                <DropdownMenuItem variant="destructive" onClick={() => setRevoking(true)} data-node-revoke>{t("devices:node.revoke")}</DropdownMenuItem>
                <DropdownMenuItem variant="destructive" onClick={() => setRemoving(true)} data-node-remove>{t("devices:remove.action")}</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </>}>
        <div data-node-detail={v.id} data-node-page={v.id} data-node-active-tab={tab}>
          <SegmentedControl
            kind="tabs"
            variant="tray"
            size="md"
            aria-label={t("devices:nodeTab.label")}
            className="steve-seg mb-6"
            value={tab}
            onChange={(next) => switchTab(next)}
            data={{ "data-node-tabs": "" }}
            options={NODE_TABS.map((k) => {
              const badge = tabBadges[k]
              return {
                value: k,
                label: t(`devices:nodeTab.${k}`),
                count: badge.count,
                controls: `node-panel-${k}`,
                id: `node-tab-${k}`,
                data: {
                  "data-node-tab": k,
                  "data-warn": badge.tone === "warn" ? "" : undefined,
                  "data-bad": badge.tone === "bad" ? "" : undefined,
                  "data-info": badge.tone === "info" ? "" : undefined,
                },
              }
            })}
          />

          <div role="tabpanel" id={`node-panel-${tab}`} aria-labelledby={`node-tab-${tab}`} className="flex min-w-0 flex-col gap-4" data-node-panel={tab}>
            {tab === "overview" && <OverviewNodeTab v={v} actions={actions} actionItems={actionItems} onOpenTab={switchTab} />}
            {tab === "harness" && <HarnessNodeTab v={v} actions={actions} actionItems={actionItems} onOpenTab={switchTab} />}
            {tab === "work" && <WorkNodeTab v={v} actions={actions} />}
            {tab === "access" && <AccessNodeTab v={v} actions={actions} actionItems={actionItems} onOpenTab={switchTab} onUnpair={() => setRevoking(true)} onRemove={() => setRemoving(true)} />}
            {tab === "version" && <VersionNodeTab v={v} actions={actions} />}
            {tab === "diagnostics" && <DiagnosticsNodeTab v={v} />}
          </div>
        </div>

        {removing && <RemoveDialog v={v} onClose={() => setRemoving(false)} />}
        {renaming && <Rename v={v} onClose={() => setRenaming(false)} onDone={reloadNodes} />}
        {revoking && (
          <Dialog open onOpenChange={(x) => !x && setRevoking(false)}>
            <DialogContent className="sm:max-w-md" data-node-revoke-dialog>
              <DialogHeader>
                <DialogTitle>{t("devices:node.revokeTitle", { name: v.name })}</DialogTitle>
                <DialogDescription>{t("devices:node.revokeSub")}</DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button variant="outline" onClick={() => setRevoking(false)}>{t("common:action.cancel")}</Button>
                <Button variant="destructive" disabled={busy} onClick={() => void revoke()} data-node-revoke-confirm>{t("devices:node.revoke")}</Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        )}
      </Page>
    </div>
  )
}

/* ───────────────────── Shared Action Row Copy & Icon ───────────────────── */

const ACTION_ICON: Record<NodeActionItem["kind"], typeof ShieldAlertIcon> = {
  pending: ShieldAlertIcon,
  offline: WifiOffIcon,
  fileAccess: FolderIcon,
  harnessAuth: KeyRoundIcon,
  harnessProbeFailed: CpuIcon,
  harnessUpdatesAvailable: DownloadIcon,
  upgradeFailed: RotateCcwIcon,
  upgradeRunning: LoaderIcon,
  upgradeRequired: ArrowUpCircleIcon,
  upgradeOutdated: ArrowUpCircleIcon,
  upgradeManual: ArrowUpCircleIcon,
  sessionsWaiting: MessageCircleIcon,
}

function ActionText({ v, item }: { v: NodeView; item: NodeActionItem }) {
  const t = useT()
  switch (item.kind) {
    case "pending":
      return <>{t("devices:nodeOverview.actionPending")}</>
    case "offline": {
      const s = nodeStatus(v)
      return (
        <span data-node-offline>
          <span data-node-offline-reason={v.node?.offline?.reason ?? ""}>
            {t("devices:nodeOverview.actionOffline", {
              status: statusText(s),
              why: item.offlineReason || t("devices:offlineReason.unreachable"),
            })}
          </span>
        </span>
      )
    }
    case "fileAccess":
      return (
        <>
          {item.locations?.length
            ? t("devices:nodeOverview.actionFileAccess", {
                locations: item.locations.map((loc) => t(`devices:fileAccess.location.${loc as "documents"}`, { defaultValue: loc })).join(t("shell:list.sep")),
              })
            : t("devices:nodeOverview.actionFileAccessPlain")}
        </>
      )
    case "harnessAuth":
      return <>{t("devices:nodeOverview.actionHarnessAuth", { names: (item.names ?? []).join(" · ") })}</>
    case "harnessProbeFailed":
      return <>{t("devices:nodeOverview.actionHarnessProbeFailed")}</>
    case "harnessUpdatesAvailable": {
      const names = (item.names ?? []).join(" · ")
      return (
        <>
          {(item.running ?? 0) > 0
            ? t("devices:nodeOverview.actionHarnessUpdatesBusy", { names, running: item.running })
            : t("devices:nodeOverview.actionHarnessUpdates", { names })}
        </>
      )
    }
    case "upgradeFailed":
      return <>{t("devices:nodeOverview.actionUpgradeFailed", { to: item.version ?? "?", from: item.from ?? "?" })}</>
    case "upgradeRunning":
      return (
        <>
          {t("devices:nodeOverview.actionUpgradeRunning", {
            to: item.version ?? "?",
            phase: t(`device:upgrade.phase.${(item.phase ?? "draining") as "draining"}`, { defaultValue: item.phase ?? "" }),
          })}
        </>
      )
    case "upgradeRequired": {
      const from = item.from || ""
      const to = item.version || ""
      if (item.protocolOlder === "hub") {
        return <>{t("devices:nodeOverview.actionUpgradeHub", { from: from || "?", to: to || "?" })}</>
      }
      if (v.local) {
        return <>{t("devices:nodeOverview.actionUpgradeRequiredLocal", { from: from || "?", to: to || "?" })}</>
      }
      if (from && to) {
        return <>{t(item.command === "startUpgrade" ? "devices:nodeOverview.actionUpgradeRequired" : "devices:nodeOverview.actionUpgradeRequiredManual", { from, to })}</>
      }
      return <>{t("devices:nodeOverview.actionUpgradeRequiredUnknown")}</>
    }
    case "upgradeOutdated":
      return <>{t("devices:nodeOverview.actionUpgradeOutdated", { from: item.from ?? "?", to: item.version ?? "?" })}</>
    case "upgradeManual":
      return <>{t("devices:nodeOverview.actionUpgradeManual", { from: item.from ?? "?", to: item.version ?? "?" })}</>
    case "sessionsWaiting":
      return <>{t("devices:nodeOverview.actionSessionsWaiting", { count: item.count ?? 0 })}</>
  }
}

/* ───────────────────── Tab 1: Overview ───────────────────── */

function OverviewNodeTab({
  v, actions, actionItems, onOpenTab,
}: {
  v: NodeView
  actions: NodeActions
  actionItems: NodeActionItem[]
  onOpenTab: (tab: NodeTab) => void
}) {
  const t = useT(), { hubVersion, harnesses, harnessesError, contextDefault } = useDevices()
  const up = upgradeCard(v, hubVersion)
  const upgradeOnly = isUpgradeOnlyLink(v)
  const effectiveHub = v.hubVersion || hubVersion
  const worst = actionItems.some((a) => a.tone === "bad")
  const hasWarn = actionItems.some((a) => a.tone === "warn")

  const harnessText = upgradeOnly
    ? harnessSummary(v, harnesses?.[v.id], effectiveHub)
    : harnessesError
      ? t("devices:harness.readFailed")
      : harnessSummary(v, harnesses?.[v.id], effectiveHub)
  const contextChoice = v.node?.owner_context == null
    ? (contextDefault ? t("shell:devices.ownerContext.inherit", { value: t(`shell:devices.ownerContext.${contextDefault}`) }) : t("devices:context.inheritPlain"))
    : t(`shell:devices.ownerContext.${v.node.owner_context ? "on" : "off"}`)
  const isMac = v.node?.host?.os === "macos" || (!v.node?.host && hostOf(v).kind === "mac")
  const fileAccess = isMac && v.linked && v.node?.host?.file_access?.status !== "not_applicable"
    ? v.node?.host?.file_access
    : undefined
  const accessText = isMac && v.node?.host?.file_access?.status !== "not_applicable"
    ? fileAccess?.status === "missing"
      ? t("devices:nodeOverview.summaryAccessMissing")
      : fileAccess?.status === "granted"
        ? t("devices:nodeOverview.summaryAccessGranted", { cfg: contextChoice })
        : t("devices:nodeOverview.summaryAccessUnknown", { cfg: contextChoice })
    : v.local
      ? t("devices:nodeOverview.summaryAccessLocal", { cfg: contextChoice })
      : t("devices:nodeOverview.summaryAccessOther", { cfg: contextChoice })
  const versionText = up.kind === "running"
    ? t("devices:nodeOverview.summaryVersionUpgrading", { to: up.run.version })
    : up.kind === "failed"
      ? t("devices:nodeOverview.summaryVersionFailed", { to: up.run.version, from: up.run.from_version ?? "?" })
      : upgradeOnly
        ? t("devices:nodeOverview.summaryVersionUpgradeRequired", { from: v.version ?? "?", to: effectiveHub || "?" })
        : up.kind === "builtIn"
          ? t("devices:nodeOverview.summaryVersionBuiltIn", { version: effectiveHub || v.version || "?" })
          : up.kind === "outdated"
            ? t("devices:nodeOverview.summaryVersionOutdated", { from: up.from ?? "?", to: up.to })
            : up.kind === "offline"
              ? t("devices:nodeOverview.summaryVersionOffline", { version: up.last ?? "?" })
              : t("devices:nodeOverview.summaryVersionSame", { version: v.version ?? effectiveHub ?? "?" })
  const dirs = v.device ? st.projects.filter((p) => p.device_id === v.device!.id) : []
  const workSummaryParts = [
    v.max != null ? t(v.paused ? "devices:nodeOverview.summaryWorkPaused" : "devices:nodeOverview.summaryWorkActive", { max: v.max }) : "",
    dirs.length ? t("devices:nodeOverview.summaryProjects", { count: dirs.length }) : t("devices:nodeOverview.summaryNoProjects"),
  ].filter(Boolean).join(" · ")

  return (
    <>
      {v.state === "revoked" ? (
        <div className="flex items-start gap-3 rounded-xl bg-muted/70 px-4 py-3.5" data-node-readiness="revoked">
          <WifiOffIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div className="min-w-0 flex-1">
            <div className="type-callout font-medium text-foreground">{t("devices:nodeOverview.revokedTitle")}</div>
            <p className="type-footnote text-muted-foreground">{t("devices:nodeOverview.revokedSub")}</p>
          </div>
        </div>
      ) : actionItems.length > 0 ? (
        <section
          aria-label={t("devices:nodeOverview.needsAttention")}
          className={cn(
            "flex flex-col overflow-hidden rounded-xl",
            worst
              ? "bg-destructive/6 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--destructive)_22%,transparent)]"
              : hasWarn
                ? "bg-attention/8 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--attention)_25%,transparent)]"
                : "bg-blue/8 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--blue)_24%,transparent)]",
          )}
          data-node-actions
        >
          <header className="flex items-center justify-between gap-2 px-4 pt-3 pb-1.5">
            <div>
              <h2 className="type-footnote font-semibold text-foreground">{t("devices:nodeOverview.needsAttention")}</h2>
              <p className="type-footnote text-muted-foreground">{t("devices:nodeOverview.needsAttentionSub")}</p>
            </div>
          </header>
          {actionItems.map((item) => {
            const Icon = ACTION_ICON[item.kind]
            return (
              <div
                key={item.kind}
                className="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-2 border-t border-border/60 px-4 py-2.5"
                data-node-action={item.kind}
              >
                <Icon
                  className={cn(
                    "size-4 shrink-0",
                    item.tone === "bad" ? "text-destructive" : item.tone === "info" ? "text-blue" : "text-attention",
                    item.kind === "upgradeRunning" && "animate-spin",
                  )}
                  aria-hidden
                />
                <span className="min-w-0 flex-1 basis-52 type-callout text-pretty text-foreground">
                  <ActionText v={v} item={item} />
                </span>
                <NodeActionControls item={item} actions={actions} onOpenTab={onOpenTab} />
                {item.kind === "upgradeRunning" && up.kind === "running" && (
                  <div className="w-full pt-1">
                    <UpgradeSteps steps={up.steps} />
                  </div>
                )}
              </div>
            )
          })}
        </section>
      ) : v.paused ? (
        <div className="flex flex-wrap items-center gap-3 rounded-xl bg-muted/70 px-4 py-3" data-node-readiness="paused">
          <PauseIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div className="min-w-0 flex-1">
            <div className="type-callout font-medium text-foreground">{t("devices:nodeOverview.pausedTitle")}</div>
            <p className="type-footnote text-muted-foreground">{t("devices:nodeOverview.pausedSub")}</p>
          </div>
          {actions.work.editable && (
            <Button size="xs" variant="outline" disabled={actions.work.busy} onClick={() => void actions.work.setReceive(true)} data-node-resume-inline>
              {actions.work.busy ? <Spinner className="size-3" /> : <PlayIcon />}{t("devices:node.resume")}
            </Button>
          )}
        </div>
      ) : (
        <div className="flex items-center gap-3 rounded-xl bg-success/8 px-4 py-3 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--success)_22%,transparent)]" data-node-readiness="ready">
          <CheckCircle2Icon className="size-4 shrink-0 text-success" aria-hidden />
          <div className="min-w-0 flex-1">
            <div className="type-callout font-medium text-foreground">{t("devices:nodeOverview.readyTitle")}</div>
            <p className="type-footnote text-muted-foreground">{t("devices:nodeOverview.readySub")}</p>
          </div>
        </div>
      )}

      {v.state !== "revoked" && (
        <NodeFigures
          v={v}
          action={
            <Button size="xs" variant="ghost" onClick={() => onOpenTab("work")} data-overview-goto="work">
              {t("devices:nodeOverview.metricsMore")}<ChevronRightIcon className="size-3.5" aria-hidden />
            </Button>
          }
        />
      )}

      <DCard title={t("devices:nodeOverview.summaryTitle")} data-node-overview-summary>
        <div className="border-t border-border/70">
          <DRow icon={<CpuIcon />} title={t("devices:nodeTab.harness")} sub={harnessText} onClick={() => onOpenTab("harness")} data-overview-link="harness">
            {actionItems.some((a) => a.kind === "harnessUpdatesAvailable") && (
              <Badge tone="warning" size="sm">{t("devices:harnessUpdates.available")}</Badge>
            )}
          </DRow>
          <DRow icon={<ActivityIcon />} title={t("devices:nodeTab.work")} sub={workSummaryParts} onClick={() => onOpenTab("work")} data-overview-link="work" />
          <DRow icon={<LockIcon />} title={t("devices:nodeTab.access")} sub={accessText} onClick={() => onOpenTab("access")} data-overview-link="access">
            {fileAccess?.status === "missing" && <Badge tone="warning" size="sm">{t("devices:fileAccess.state.missing")}</Badge>}
          </DRow>
          <DRow icon={<ArrowUpCircleIcon />} title={t("devices:nodeTab.version")} sub={versionText} onClick={() => onOpenTab("version")} data-overview-link="version">
            {upgradeOnly && up.kind !== "running" && up.kind !== "failed" && <Badge tone="danger" size="sm">{t("devices:upgrade.requiredBadge")}</Badge>}
            {!upgradeOnly && up.kind === "outdated" && <Badge tone="warning" size="sm">{t("devices:upgrade.can")}</Badge>}
            {up.kind === "running" && <Badge tone="info" size="sm" dot live>{t("devices:upgrade.upgrading")}</Badge>}
            {up.kind === "failed" && <Badge tone="danger" size="sm">{t("devices:upgrade.rolledBack")}</Badge>}
          </DRow>
        </div>
      </DCard>
    </>
  )
}

/* ───────────────────── Tab 2: Harness ───────────────────── */

function HarnessNodeTab({
  v, actions, actionItems, onOpenTab,
}: {
  v: NodeView
  actions: NodeActions
  actionItems: NodeActionItem[]
  onOpenTab: (tab: NodeTab) => void
}) {
  const upgradeOnly = isUpgradeOnlyLink(v)
  const harnessActions = actionItems.filter((a) => a.tab === "harness")
  return (
    <>
      {harnessActions.map((item) => {
        const Icon = ACTION_ICON[item.kind]
        return (
          <div key={item.kind} className="flex flex-wrap items-center gap-3 rounded-xl bg-attention/10 px-4 py-3" role="status" data-tab-action={item.kind}>
            <Icon className="size-4 shrink-0 text-attention" aria-hidden />
            <span className="min-w-0 flex-1 type-callout text-foreground"><ActionText v={v} item={item} /></span>
            <NodeActionControls item={item} actions={actions} onOpenTab={onOpenTab} showTabLink={false} />
          </div>
        )
      })}
      <NodeHarnesses v={v} actions={actions} onOpenTab={onOpenTab}>
        {v.local && !upgradeOnly && v.device?.can_configure && !st.down && <AddHarness />}
      </NodeHarnesses>
    </>
  )
}

/* ───────────────────── Tab 3: Work & Resources ───────────────────── */

function WorkNodeTab({ v, actions }: { v: NodeView; actions: NodeActions }) {
  return (
    <>
      <RunningCard id={v.device?.id ?? null} offline={!v.linked && !v.local} />
      <NodeResources v={v} />
      <NodeTrends v={v} />
      <TakeWork v={v} w={actions.work} />
    </>
  )
}

/* ───────────────────── Tab 4: Permissions & Access ───────────────────── */

function AccessNodeTab({
  v, actions, actionItems, onOpenTab, onUnpair, onRemove,
}: {
  v: NodeView
  actions: NodeActions
  actionItems: NodeActionItem[]
  onOpenTab: (tab: NodeTab) => void
  onUnpair: () => void
  onRemove: () => void
}) {
  const pendingItem = actionItems.find((a) => a.kind === "pending")
  return (
    <>
      {pendingItem && (
        <div className="flex flex-wrap items-center gap-3 rounded-xl bg-attention/10 px-4 py-3" role="status" data-tab-action="pending">
          <ShieldAlertIcon className="size-4 shrink-0 text-attention" aria-hidden />
          <span className="min-w-0 flex-1 type-callout text-foreground"><ActionText v={v} item={pendingItem} /></span>
          <NodeActionControls item={pendingItem} actions={actions} onOpenTab={onOpenTab} showTabLink={false} />
        </div>
      )}
      <NodeFileAccess v={v} actions={actions} />
      <NodeSandbox v={v} />
      <NodeContext v={v} editable={v.local || actions.pairable} />
      <Projects v={v} />
      {actions.pairable && <LeaveCard onUnpair={onUnpair} onRemove={onRemove} />}
    </>
  )
}

function NodeFileAccess({ v, actions }: { v: NodeView; actions: NodeActions }) {
  const t = useT()
  const host = hostOf(v)
  const report = v.linked ? v.node?.host?.file_access : undefined
  const isMac = v.node?.host?.os === "macos" || (!v.node?.host && host.kind === "mac")

  if (!isMac || report?.status === "not_applicable") {
    return (
      <DCard icon={<FolderIcon />} title={t("devices:fileAccess.title")} data-node-file-access="not-applicable">
        <p className="px-5 pb-5 type-callout text-muted-foreground">
          {t("devices:fileAccess.nonMacNote", { os: host.os || "Linux" })}
        </p>
      </DCard>
    )
  }

  const status = report?.status ?? "unknown"
  const fda = report?.full_disk_access
  const locations = report?.locations ?? []
  const repairCmd = v.local ? "steve hub file-access" : "steve node file-access"

  if (status === "granted") {
    return (
      <DCard
        icon={<FolderIcon />}
        title={t("devices:fileAccess.title")}
        action={
          <div className="flex items-center gap-2">
            <Badge tone="success" size="sm" dot>{t("devices:fileAccess.state.granted")}</Badge>
            <Button size="xs" variant="ghost" disabled={actions.fileAccessBusy || !v.linked} onClick={() => void actions.recheckFileAccess()} data-node-file-access-recheck>
              {actions.fileAccessBusy && <Spinner className="size-3" />}{t("devices:fileAccess.recheckShort")}
            </Button>
          </div>
        }
        data-node-file-access="granted"
      >
        <div className="flex flex-wrap items-center gap-2 px-5 pb-4 type-footnote">
          {fda && fda !== "not_applicable" && (
            <span className="inline-flex items-center gap-1.5 rounded-md bg-success/8 px-2.5 py-1 text-foreground" data-file-access-fda={fda}>
              <span>{t("devices:fileAccess.fullDiskAccess")}</span>
              <Badge tone={fda === "granted" ? "success" : "neutral"} size="sm">{t(`devices:fileAccess.state.${fda}`)}</Badge>
            </span>
          )}
          {locations.map(({ location, status: locStatus, path }, idx) => (
            <span
              key={`${location}:${path ?? idx}`}
              className="inline-flex items-center gap-1.5 rounded-md bg-success/8 px-2.5 py-1 text-foreground"
              data-file-access-location={location}
              data-file-access-path={path || undefined}
              title={path}
            >
              <span>{t(`devices:fileAccess.location.${location}`)}</span>
              <Badge tone="success" size="sm">{t(`devices:fileAccess.state.${locStatus}`)}</Badge>
            </span>
          ))}
          {!locations.length && (!fda || fda === "not_applicable") && (
            <p className="text-muted-foreground">{t("devices:fileAccess.noneNeeded")}</p>
          )}
        </div>
      </DCard>
    )
  }

  return (
    <DCard
      icon={<FolderIcon />}
      title={t("devices:fileAccess.title")}
      action={<Badge tone={status === "missing" ? "warning" : "neutral"} size="sm" dot>{t(`devices:fileAccess.state.${status}`)}</Badge>}
      data-node-file-access={status}
    >
      <div className="space-y-3 px-5 pb-5 type-callout">
        {fda && fda !== "not_applicable" && (
          <div className="flex items-center justify-between gap-2 rounded-lg bg-muted/50 px-3 py-2" data-file-access-fda={fda}>
            <span className="text-foreground">{t("devices:fileAccess.fullDiskAccess")}</span>
            <Badge tone={fda === "granted" ? "success" : fda === "missing" ? "warning" : "neutral"} size="sm">
              {t(`devices:fileAccess.state.${fda}`)}
            </Badge>
          </div>
        )}
        {!locations.length && (!fda || fda === "not_applicable") && (
          <p className="text-muted-foreground">{t(`devices:fileAccess.state.${status}`)}</p>
        )}
        {!!locations.length && (
          <ul className="flex flex-col gap-1.5 text-muted-foreground">
            {locations.map(({ location, status: locStatus, path }, idx) => (
              <li
                key={`${location}:${path ?? idx}`}
                className="flex items-center justify-between gap-2 rounded-lg bg-muted/50 px-3 py-2"
                data-file-access-location={location}
                data-file-access-path={path || undefined}
              >
                <div className="min-w-0">
                  <div className="text-foreground">{t(`devices:fileAccess.location.${location}`)}</div>
                  {path && <div className="break-all font-mono type-footnote text-muted-foreground">{path}</div>}
                </div>
                <Badge tone={locStatus === "granted" ? "success" : locStatus === "missing" ? "warning" : "neutral"} size="sm">
                  {t(`devices:fileAccess.state.${locStatus}`)}
                </Badge>
              </li>
            ))}
          </ul>
        )}
        {report?.executable_path && (
          <div className="rounded-lg bg-muted/40 px-3 py-2 type-footnote" data-file-access-executable>
            <div className="text-muted-foreground">{t("devices:fileAccess.executable")}</div>
            <div className="mt-0.5 break-all font-mono text-foreground">{report.executable_path}</div>
          </div>
        )}
        {status === "missing" && (
          <div className="space-y-2">
            <p className="text-muted-foreground">{t(v.local ? "devices:fileAccess.repairLocal" : "devices:fileAccess.repair")}</p>
            <CodeBlock code={repairCmd} language="bash" />
          </div>
        )}
        <Button size="sm" variant="outline" disabled={actions.fileAccessBusy || !v.linked} onClick={() => void actions.recheckFileAccess()} data-node-file-access-recheck>
          {actions.fileAccessBusy && <Spinner className="size-3.5" />}{t("devices:fileAccess.recheck")}
        </Button>
      </div>
    </DCard>
  )
}

function NodeSandbox({ v }: { v: NodeView }) {
  const t = useT()
  return (
    <DCard
      icon={<LockIcon />}
      title={t("devices:sandbox.title")}
      sub={t("devices:sandbox.sub")}
      action={<Badge tone={v.local ? "info" : "neutral"} size="sm">{t(v.local ? "devices:sandbox.badgeLocal" : "devices:sandbox.badgeOwnerOnly")}</Badge>}
      data-node-sandbox
    >
      <div className="border-t border-border/70">
        <DRow title={t("devices:sandbox.ownerScope")} sub={t(v.local ? "devices:sandbox.ownerScopeLocal" : "devices:sandbox.ownerScopeRemote")} />
        <DRow title={t("devices:sandbox.isolation")} sub={t(v.local ? "devices:sandbox.isolationLocal" : "devices:sandbox.isolationRemote")} />
        <DRow title={t("devices:sandbox.projectBoundary")} sub={t("devices:sandbox.projectBoundaryDesc")} />
      </div>
    </DCard>
  )
}

/** the project folders on this computer */
function Projects({ v }: { v: NodeView }) {
  const t = useT()
  const dirs = v.device ? st.projects.filter((p) => p.device_id === v.device!.id) : []
  if (!dirs.length) return null
  return (
    <DCard icon={<FolderIcon />} title={t("devices:node.projects", { n: dirs.length })} data-node-projects>
      <div className="border-t border-border/70">
        {dirs.map((p) => (
          <DRow key={p.id} title={p.name} sub={<span className="break-all font-mono">{homeTilde(p.path)}</span>} onClick={() => go("#/projects/" + p.id)} />
        ))}
      </div>
    </DCard>
  )
}

/* ───────────────────── Tab 5: Version & Upgrades ───────────────────── */

function VersionNodeTab({ v, actions }: { v: NodeView; actions: NodeActions }) {
  return (
    <>
      <NodeVersion v={v} editable={actions.canUpgrade} actions={actions} />
      <NodeUpgradeHistory v={v} />
    </>
  )
}

/* ───────────────────── Tab 6: Diagnostics ───────────────────── */

function DiagnosticsNodeTab({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion, harnesses, harnessesError } = useDevices()
  const { value: vit } = useNodeVitals(v.id)
  const hr = harnesses?.[v.id] ?? null
  const snapshot = useMemo(
    () => nodeDiagnosticSnapshot(v, { hubVersion, vitals: vit ?? null, harnessReport: hr, harnessesError }),
    [v, hubVersion, vit, hr, harnessesError],
  )
  const snapshotJson = useMemo(() => JSON.stringify(snapshot, null, 2), [snapshot])
  const copySnapshot = async () => {
    try {
      await navigator.clipboard.writeText(snapshotJson)
      toast(t("devices:diagnostics.copiedBundle"))
    } catch {
      toast(t("devices:diagnostics.copyFailed"))
    }
  }
  const route = linkRoute(v)
  const link = v.node?.link
  const every = vit?.sampling ? Math.round(vit.sampling.interval_ms / 1000) : null
  const localCmds = v.local ? "steve hub status\nsteve node doctor" : "steve node status\nsteve node doctor"

  const row = (k: string, val: ReactNode, data?: Record<string, string>) => (
    <div className="contents" {...data}>
      <dt className="type-callout text-muted-foreground">{k}</dt>
      <dd className="min-w-0 type-callout [overflow-wrap:anywhere]">{val}</dd>
    </div>
  )

  return (
    <div className="flex min-w-0 flex-col gap-4" data-node-diagnostics>
      <DCard
        icon={<StethoscopeIcon />}
        title={t("devices:diagnostics.title")}
        sub={t("devices:diagnostics.sub")}
        action={
          <Button size="sm" variant="outline" onClick={() => void copySnapshot()} data-node-diagnostic-copy>
            <CopyIcon />{t("devices:diagnostics.copyBundle")}
          </Button>
        }
      >
        <div className="flex flex-col gap-2.5 px-5 pb-5">
          <div className="type-callout font-medium text-foreground">{t("devices:diagnostics.localCommands")}</div>
          <p className="type-footnote text-muted-foreground">
            {t(v.local ? "devices:diagnostics.localHubCommandsSub" : "devices:diagnostics.localCommandsSub")}
          </p>
          <CodeBlock code={localCmds} language="bash" />
        </div>
      </DCard>

      <DCard icon={<TerminalIcon />} title={t("devices:diagnostics.connectionTitle")} sub={t("devices:diagnostics.connectionSub")} data-node-connection-diagnostics>
        <dl className="grid gap-x-6 gap-y-2.5 px-5 pb-5 sm:grid-cols-[8.5rem_minmax(0,1fr)]">
          {row(
            t("devices:diagnostics.linkState"),
            <span className="inline-flex flex-wrap items-center gap-1.5">
              <Badge tone={v.linked ? (isUpgradeOnlyLink(v) ? "warning" : "success") : "neutral"} size="sm" dot>
                {t(
                  !v.linked
                    ? "devices:diagnostics.linkDisconnected"
                    : isUpgradeOnlyLink(v)
                      ? "devices:diagnostics.linkUpgradeOnly"
                      : "devices:diagnostics.linkConnected",
                )}
              </Badge>
              {v.node?.protocol != null && (
                <span className="type-code text-muted-foreground">
                  {t("devices:diagnostics.linkProtocol", { proto: v.node.protocol })}
                </span>
              )}
            </span>,
          )}
          {route && row(t("devices:diagnostics.linkRoute"), `${t(`devices:route.${route}`)}${link?.tls ? " · TLS" : ""}`)}
          {link?.peer && row(t("devices:diagnostics.linkPeer"), <span className="type-code">{link.peer}</span>)}
          {link?.since ? row(t("devices:diagnostics.linkSince"), <span className="tabular-nums">{date(link.since, { dateStyle: "medium", timeStyle: "short" })}</span>) : null}
          {!v.local && v.linked && link?.latency_ms != null && row(t("devices:diagnostics.linkLatency"), <span className="tabular-nums">{Math.round(link.latency_ms)} ms</span>)}
          {!v.linked && !v.local && row(t("devices:diagnostics.offlineReason"), offlineReason(v) || t("devices:offlineReason.unreachable"))}
          {v.linkError && (
            <>
              <dt className="type-callout text-muted-foreground">{t("devices:diagnostics.lastFailure")}</dt>
              <dd className="min-w-0 type-code text-destructive [overflow-wrap:anywhere]" data-node-link-error>
                <span className="text-muted-foreground">{agoText(v.linkError.at)} · </span>{v.linkError.error}
              </dd>
            </>
          )}
          {row(
            t("devices:diagnostics.vitalsSampling"),
            every != null
              ? t("devices:diagnostics.vitalsEvery", { s: every, gaps: vit?.gaps?.length ?? 0 })
              : t("devices:diagnostics.vitalsNone"),
          )}
          {row(
            t("devices:diagnostics.harnessProbe"),
            hr?.checked_at && hr.state === "reported"
              ? t("devices:diagnostics.harnessProbeOk", { at: date(hr.checked_at, { dateStyle: "short", timeStyle: "medium" }) })
              : t("devices:diagnostics.harnessProbeState", { state: harnessesError ? "error" : (hr?.state ?? "unreported") }),
          )}
          {v.node?.removal?.latest && row(
            t("devices:diagnostics.removalState"),
            <span className="type-code">{v.node.removal.latest.phase}{v.node.removal.latest.error ? ` · ${v.node.removal.latest.error}` : ""}</span>,
          )}
        </dl>
      </DCard>

      <Identity v={v} />

      <DCard title={t("devices:diagnostics.rawTitle")} sub={t("devices:diagnostics.rawSub")} data-node-raw-state>
        <div className="px-5 pb-5">
          <details className="group">
            <summary className="cursor-pointer type-callout text-muted-foreground hover:text-foreground" data-node-raw-toggle>
              {t("devices:diagnostics.rawToggle")}
            </summary>
            <div className="mt-3">
              <CodeBlock code={snapshotJson} language="json" filename={`node-${v.id}-diagnostics.json`} />
            </div>
          </details>
        </div>
      </DCard>
    </div>
  )
}

/** who and what it is: its id, the fingerprint checked when it was paired, its owner, when it last connected */
function Identity({ v }: { v: NodeView }) {
  const t = useT()
  const build = v.node?.upgrade?.build
  const row = (k: string, val: ReactNode, data?: Record<string, string>) => <div className="contents" {...data}><dt className="type-callout text-muted-foreground">{k}</dt><dd className="min-w-0 type-callout [overflow-wrap:anywhere]">{val}</dd></div>
  return (
    <DCard icon={<ShieldCheckIcon />} title={t("devices:node.identity")} sub={t("devices:node.identitySub")} data-node-identity>
      <dl className="grid gap-x-6 gap-y-2.5 px-5 pb-5 sm:grid-cols-[8.5rem_minmax(0,1fr)]">
        {row(t("devices:node.nodeId"), <span className="type-code select-all">{v.id}</span>)}
        {v.fingerprint && <><dt className="type-callout text-muted-foreground">{t("devices:node.fingerprint")}</dt><dd className="type-code select-all [overflow-wrap:anywhere]" data-node-fingerprint={v.fingerprint}>{fpGroups(v.fingerprint).join(" ") || v.fingerprint}</dd></>}
        {v.local && v.device && <><dt className="type-callout text-muted-foreground">{t("devices:node.owner")}</dt><dd className="min-w-0"><OwnerChip objectType="device" objectId={v.device.id} bare /></dd></>}
        {v.lastSeen != null && row(t("devices:node.lastSeen"), <span className="tabular-nums" title={date(v.lastSeen, { dateStyle: "medium", timeStyle: "medium" })}>{v.linked ? t("devices:node.now") : agoText(v.lastSeen)}</span>)}
        {v.version && row(t("devices:card.version"), <span className="tabular-nums">{v.version}{build?.commit ? ` (${build.commit.slice(0, 8)})` : ""}{build?.target ? ` · ${build.target}` : ""}</span>)}
      </dl>
    </DCard>
  )
}

function Rename({ v, onClose, onDone }: { v: NodeView; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [name, setName] = useState(v.name), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try { await api.renameNode(v.id, name.trim()); toast(t("devices:node.renamed", { name: name.trim() })); onClose(); onDone() } catch (e) { toast((e as Error).message); setBusy(false) }
  }
  return <Dialog open onOpenChange={(x) => !x && onClose()}><DialogContent className="sm:max-w-sm" data-node-rename-dialog>
    <DialogHeader><DialogTitle>{t("devices:node.renameTitle")}</DialogTitle></DialogHeader>
    <form onSubmit={(e) => { e.preventDefault(); void save() }} className="flex flex-col gap-4">
      <Input autoFocus value={name} maxLength={64} onChange={(e) => setName(e.target.value)} aria-label={t("devices:node.renameTitle")} data-node-rename-input />
      <DialogFooter><Button type="button" variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button type="submit" disabled={busy || !name.trim() || name.trim() === v.name}>{t("common:action.save")}</Button></DialogFooter>
    </form>
  </DialogContent></Dialog>
}

const ADD_HARNESS_MODE_CHOICES = [
  { v: "__default", lKey: "shell:devices.harness.modeDefault" },
  { v: "default", l: "default" },
  { v: "plan", l: "plan" },
  { v: "acceptEdits", l: "acceptEdits" },
  { v: "auto", l: "auto" },
  { v: "bypassPermissions", l: "bypassPermissions" },
] as const

/** Declares a custom ACP harness adapter in the Hub's manifest using strongly-typed controls (no raw JSON). */
function AddHarness() {
  const t = useT()
  const [open, setOpen] = useState(false), [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [id, setId] = useState(""), [name, setName] = useState(""), [command, setCommand] = useState("")
  const [args, setArgs] = useState<string[]>([]), [argDraft, setArgDraft] = useState("")
  const [modeAsk, setModeAsk] = useState("default")
  const [modePlan, setModePlan] = useState("plan")
  const [modeAuto, setModeAuto] = useState("auto")
  const [nestedSandbox, setNestedSandbox] = useState(true)
  const [supportsResume, setSupportsResume] = useState(false)
  const [resumeCli, setResumeCli] = useState("")

  if (!open) return <Button size="sm" variant="outline" onClick={() => setOpen(true)} data-add-harness><PlusIcon />{t("shell:devices.harness.add")}</Button>

  const modeOptions = ADD_HARNESS_MODE_CHOICES.map((opt) => ({
    v: opt.v,
    l: "lKey" in opt ? t(opt.lKey) : opt.l,
  }))

  const addArg = () => {
    const trimmed = argDraft.trim()
    if (!trimmed) return
    setArgs((cur) => [...cur, trimmed])
    setArgDraft("")
  }
  const removeArg = (index: number) => setArgs((cur) => cur.filter((_, i) => i !== index))

  const save = async () => {
    setBusy(true); setError("")
    try {
      const pendingArg = argDraft.trim()
      const finalArgs = pendingArg ? [...args, pendingArg] : args
      const modes: AddHarnessModes = {}
      if (modeAsk !== "__default") modes.ask = modeAsk
      if (modePlan !== "__default") modes.plan = modePlan
      if (modeAuto !== "__default") modes.auto = modeAuto
      const properties: AddHarnessProperties = {
        nested_sandbox: nestedSandbox,
        probe_initialize: true,
        ...(Object.keys(modes).length ? { modes } : {}),
        ...(supportsResume && resumeCli.trim() ? { resume_cli: resumeCli.trim() } : {}),
      }
      await api.addHarness({
        id: id.trim(),
        name: name.trim() || undefined,
        command: command.trim(),
        args: finalArgs,
        properties,
      })
      toast(t("shell:devices.harness.saved"))
      setOpen(false)
      setId(""); setName(""); setCommand(""); setArgs([]); setArgDraft("")
      setModeAsk("default"); setModePlan("plan"); setModeAuto("auto")
      setNestedSandbox(true); setSupportsResume(false); setResumeCli("")
      await refresh()
    } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }

  return (
    <div className="anim-in flex flex-col gap-3 rounded-xl bg-muted/60 p-4" data-harness-form>
      <div className="type-callout text-muted-foreground">{t("shell:devices.harness.intro")}</div>
      <div className="grid gap-2.5 sm:grid-cols-2">
        <label className="flex min-w-0 flex-col gap-1">
          <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.idLabel")}</span>
          <Input placeholder={t("shell:devices.harness.id")} value={id} onChange={(e) => setId(e.target.value)} data-harness-id />
        </label>
        <label className="flex min-w-0 flex-col gap-1">
          <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.nameLabel")}</span>
          <Input placeholder={t("shell:devices.harness.name")} value={name} onChange={(e) => setName(e.target.value)} data-harness-name />
        </label>
      </div>
      <label className="flex min-w-0 flex-col gap-1">
        <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.commandLabel")}</span>
        <Input placeholder={t("shell:devices.harness.command")} value={command} onChange={(e) => setCommand(e.target.value)} className="font-mono" data-harness-command />
      </label>
      <div className="flex min-w-0 flex-col gap-1.5" data-harness-args-field>
        <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.argsLabel")}</span>
        <div className="flex min-w-0 items-center gap-2">
          <Input
            placeholder={t("shell:devices.harness.argsPlaceholder")}
            value={argDraft}
            onChange={(e) => setArgDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addArg() } }}
            className="min-w-0 flex-1 font-mono"
            data-harness-arg-input
          />
          <Button type="button" size="sm" variant="outline" disabled={!argDraft.trim()} onClick={addArg} data-harness-arg-add>
            <PlusIcon />{t("common:action.add")}
          </Button>
        </div>
        {args.length > 0 && (
          <ul className="flex flex-wrap gap-1.5" data-harness-args-list>
            {args.map((arg, idx) => (
              <li key={`${arg}:${idx}`} className="inline-flex items-center gap-1 rounded-md bg-background/80 px-2 py-1 font-mono type-footnote text-foreground ring-1 ring-border/70" data-harness-arg-item={arg}>
                <span className="break-all">{arg}</span>
                <button
                  type="button"
                  className="text-muted-foreground hover:text-foreground"
                  aria-label={t("shell:devices.harness.removeArg", { arg })}
                  onClick={() => removeArg(idx)}
                  data-harness-arg-remove={idx}
                >
                  <XIcon className="size-3" />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="flex min-w-0 flex-col gap-2.5 rounded-lg bg-background/60 p-3 ring-1 ring-border/60" data-harness-capabilities>
        <div className="type-footnote font-medium text-foreground">{t("shell:devices.harness.modesTitle")}</div>
        <div className="grid gap-2.5 sm:grid-cols-3">
          <div className="flex min-w-0 flex-col gap-1">
            <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.modeMapAsk")}</span>
            <SetSelect wide testId="harness-mode-ask" label={t("shell:devices.harness.modeMapAsk")} value={modeAsk} onChange={setModeAsk} options={modeOptions} />
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.modeMapPlan")}</span>
            <SetSelect wide testId="harness-mode-plan" label={t("shell:devices.harness.modeMapPlan")} value={modePlan} onChange={setModePlan} options={modeOptions} />
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.modeMapAuto")}</span>
            <SetSelect wide testId="harness-mode-auto" label={t("shell:devices.harness.modeMapAuto")} value={modeAuto} onChange={setModeAuto} options={modeOptions} />
          </div>
        </div>
        <div className="flex flex-col gap-2 border-t border-border/60 pt-2.5">
          <Switch
            placement="end"
            label={t("shell:devices.harness.nestedSandbox")}
            description={t("shell:devices.harness.nestedSandboxHint")}
            checked={nestedSandbox}
            onChange={(e) => setNestedSandbox(e.currentTarget.checked)}
            data-harness-nested-sandbox
          />
          <Switch
            placement="end"
            label={t("shell:devices.harness.supportsResume")}
            description={t("shell:devices.harness.supportsResumeHint")}
            checked={supportsResume}
            onChange={(e) => setSupportsResume(e.currentTarget.checked)}
            data-harness-supports-resume
          />
          {supportsResume && (
            <label className="flex min-w-0 flex-col gap-1 pt-1">
              <span className="type-footnote text-muted-foreground">{t("shell:devices.harness.resumeCliLabel")}</span>
              <Input
                placeholder={t("shell:devices.harness.resumeCliPlaceholder")}
                value={resumeCli}
                onChange={(e) => setResumeCli(e.target.value)}
                className="font-mono"
                data-harness-resume-cli
              />
            </label>
          )}
        </div>
      </div>
      {error && <div className="type-callout text-attention" role="alert">{error}</div>}
      <div className="mt-1 flex gap-2">
        <Button size="sm" disabled={busy || !id.trim() || !command.trim()} onClick={() => void save()} data-harness-save>
          {busy && <Spinner className="size-3" />}{t("common:action.save")}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>{t("common:action.cancel")}</Button>
      </div>
    </div>
  )
}
