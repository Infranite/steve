/* Devices · Monitor (#/devices/monitor, instance owner only):
 * 1. Status overview + 15m / 1h / 24h window switch + refresh + 4 key figures
 * 2. Pinned alerts (critical first, with contextual links to Node / backup settings)
 * 3. Golden-signal trends (4 affectUI KpiCharts: http_p95, write_lock_wait_p95, hub_cpu, hub_memory)
 * 4. Hub subsystem cards (measured checks first; insufficient-sample checks collapsed)
 * 5. Node & plugin health matrices (responsive table/cards, weakened insufficient cells with tooltips) */
import { useCallback, useEffect, useRef, useState } from "react"
import {
  ActivityIcon,
  AlertTriangleIcon,
  ChevronRightIcon,
  CpuIcon,
  GlobeIcon,
  PlugIcon,
  RadioIcon,
  RefreshCwIcon,
  ServerIcon,
  ShieldAlertIcon,
  ShieldCheckIcon,
} from "lucide-react"
import { cn } from "cn"
import { HubError } from "@/api"
import { systemApi } from "@/api/system"
import { KpiChart } from "@/components/affect/kpi-chart-clean"
import { EmptyState } from "@/components/affect/empty-state-clean"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { Loading, Skeleton } from "@/components/affect/skeleton-clean"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { useDevices } from "@/devices-data"
import { formatBytes } from "@/devices-install"
import { Chip, DCard, Figure } from "@/devices-ui"
import {
  GOLDEN_KEYS,
  HEALTH_WINDOWS,
  checkCounts,
  checkId,
  displayValue,
  goldenSeries,
  label,
  limitText,
  nodeHealthMatrix,
  overallStatus,
  pluginHealthMatrix,
  scopeLabel,
  severityOf,
  sortedWarnings,
  subsystemGroups,
  utilization,
  warningText,
  windowCoverageShort,
  type GoldenKey,
  type HealthCheck,
  type HealthOverallStatus,
  type HealthWindow,
  type HubHealth,
  type SubsystemId,
} from "@/health"
import { useT } from "@/i18n"
import { date, longDur, msDur } from "@/i18n/format"
import { Meter, type Tone } from "@/panel"
import { Notice, go } from "@/ui"

const POLL_MS = 60_000

const SUBSYSTEM_ICON: Record<SubsystemId, typeof ServerIcon> = {
  request: ServerIcon,
  realtime: RadioIcon,
  resources: CpuIcon,
  clientAndJobs: GlobeIcon,
}

const GOLDEN_COLOR: Record<GoldenKey, string> = {
  http_p95: "var(--series-1)",
  write_lock_wait_p95: "var(--series-3)",
  hub_cpu: "var(--series-2)",
  hub_memory: "var(--series-4)",
}

const formatPercent = (n: number) => `${n < 10 && n > 0 ? n.toFixed(1) : Math.round(n)}%`

function goldenFormat(key: GoldenKey): (n: number) => string {
  if (key === "http_p95" || key === "write_lock_wait_p95") return msDur
  if (key === "hub_cpu") return formatPercent
  return formatBytes
}

function statusBadgeTone(status: HealthOverallStatus): "success" | "warning" | "danger" {
  if (status === "critical") return "danger"
  if (status === "warning") return "warning"
  return "success"
}

function InsufficientCell({ check }: { check: HealthCheck | null }) {
  const t = useT()
  if (!check) return <span className="text-tertiary">—</span>
  const lim = limitText(check)
  const tip = lim ? t("devices:monitor.insufficientTipLimit", { limit: lim }) : t("devices:monitor.insufficientTip")
  return (
    <Tooltip>
      <TooltipTrigger render={<span tabIndex={0} className="inline-block cursor-default tabular-nums text-tertiary outline-none" data-sample-state="insufficient" />}>
        —
      </TooltipTrigger>
      <TooltipContent>{tip}</TooltipContent>
    </Tooltip>
  )
}

function MatrixCell({ check, warnSet }: { check: HealthCheck | null; warnSet: Set<string> }) {
  if (!check || check.value == null) return <InsufficientCell check={check} />
  const warned = warnSet.has(checkId(check))
  const sev = severityOf(check)
  return (
    <span className={cn("tabular-nums", warned ? (sev === "critical" ? "font-medium text-destructive" : "font-medium text-attention") : "text-foreground")}>
      {displayValue(check)}
    </span>
  )
}

function MonitorSkeleton() {
  const t = useT()
  return (
    <Loading label={t("devices:monitor.loading")} className="flex flex-col gap-4" data-monitor-loading>
      <DCard>
        <div className="flex flex-col gap-4 p-5">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <Skeleton width={220} height={22} />
            <Skeleton width={210} height={32} />
          </div>
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Skeleton height={54} />
            <Skeleton height={54} />
            <Skeleton height={54} />
            <Skeleton height={54} />
          </div>
        </div>
      </DCard>
      <DCard>
        <div className="grid grid-cols-1 gap-3 p-5 md:grid-cols-2">
          <Skeleton height={210} />
          <Skeleton height={210} />
          <Skeleton height={210} />
          <Skeleton height={210} />
        </div>
      </DCard>
    </Loading>
  )
}

export function MonitorTab() {
  const t = useT()
  const { views, health: defaultHealth, replaceHealth } = useDevices()
  const [win, setWin] = useState<HealthWindow>("15m")
  const [data, setData] = useState<HubHealth | null>(() => defaultHealth)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [forbidden, setForbidden] = useState(false)
  const seqRef = useRef(0)

  useEffect(() => {
    if (win === "15m" && defaultHealth && !data) {
      setData(defaultHealth)
    }
  }, [win, defaultHealth, data])

  const load = useCallback(async (target: HealthWindow) => {
    const seq = ++seqRef.current
    setBusy(true)
    try {
      const next = await systemApi.health(target)
      if (seq !== seqRef.current) return
      setData(next)
      if (target === "15m") replaceHealth(next)
      setError("")
      setForbidden(false)
    } catch (e) {
      if (seq !== seqRef.current) return
      if (e instanceof HubError && e.status === 403) {
        setForbidden(true)
        setData(null)
        setError("")
      } else {
        setError((e as Error).message)
      }
    } finally {
      if (seq === seqRef.current) setBusy(false)
    }
  }, [replaceHealth])

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    let live = true
    const tick = async () => {
      if (!live) return
      if (!document.hidden) await load(win)
      if (live) timer = setTimeout(() => void tick(), POLL_MS)
    }
    const onVisible = () => {
      if (!document.hidden) {
        clearTimeout(timer)
        void tick()
      }
    }
    void tick()
    document.addEventListener("visibilitychange", onVisible)
    return () => {
      live = false
      clearTimeout(timer)
      document.removeEventListener("visibilitychange", onVisible)
    }
  }, [win, load])

  if (forbidden) {
    return (
      <DCard data-monitor-forbidden>
        <div className="px-5 py-6">
          <EmptyState bare icon={<ShieldAlertIcon />} title={t("devices:monitor.forbiddenTitle")} text={t("devices:monitor.forbiddenText")} />
        </div>
      </DCard>
    )
  }

  if (!data) {
    if (error) {
      return (
        <DCard data-monitor-error-state>
          <div className="px-5 py-6">
            <EmptyState
              bare
              icon={<AlertTriangleIcon />}
              title={t("devices:monitor.errorTitle")}
              text={error}
              action={<Button size="sm" variant="outline" onClick={() => void load(win)} data-monitor-refresh>{t("devices:monitor.refresh")}</Button>}
            />
          </div>
        </DCard>
      )
    }
    return <MonitorSkeleton />
  }

  const past = sortedWarnings(data)
  const warnSet = new Set(past.map(checkId))
  const status = overallStatus(data)
  const counts = checkCounts(data)
  const subsystems = subsystemGroups(data)
  const nodeRows = nodeHealthMatrix(data)
  const pluginRows = pluginHealthMatrix(data)

  const criticalCount = past.filter((c) => severityOf(c) === "critical").length
  const warningCount = past.length - criticalCount
  const nodeWarnedCount = nodeRows.filter((r) => r.warnings.length > 0).length
  const pluginWarnedCount = pluginRows.filter((r) => r.warnings.length > 0).length

  const durationText = longDur(Math.max(1000, Math.round(data.window_seconds) * 1000))
  const coverageText = windowCoverageShort(data)
    ? t("devices:monitor.coverageShort", { duration: durationText })
    : t("devices:monitor.coverageNormal", { duration: durationText })
  const sampledText = data.sampled_at
    ? t("devices:monitor.updatedAt", { time: date(data.sampled_at, { hour: "2-digit", minute: "2-digit", second: "2-digit" }) })
    : ""
  const overviewSub = [coverageText, sampledText].filter(Boolean).join(" · ")

  const statusTitle = status === "healthy"
    ? t("devices:monitor.status.healthy")
    : status === "critical"
      ? t("devices:monitor.status.critical", { count: past.length })
      : t("devices:monitor.status.warning", { count: past.length })
  const StatusIcon = status === "healthy" ? ShieldCheckIcon : status === "critical" ? ShieldAlertIcon : AlertTriangleIcon

  const seriesPoints = data.series ?? []
  const labelFmt = win === "24h"
    ? { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" } as const
    : { hour: "2-digit", minute: "2-digit" } as const
  const labels = seriesPoints.map((p) => date(p.at, labelFmt))
  const tooltipLabels = seriesPoints.map((p) => date(p.at, { dateStyle: "medium", timeStyle: "short" }))

  const nodeDisplayName = (id: string) => {
    if (id === "local") return t("devices:map.localNode")
    return views.find((v) => v.id === id)?.name ?? id
  }

  return (
    <div className="flex min-w-0 flex-col gap-4" data-devices-monitor>
      {error && (
        <Notice role="alert" tone="attention" data-monitor-error>
          {t("devices:monitor.staleError", { error })}
        </Notice>
      )}

      {/* 1. Status Overview */}
      <DCard
        icon={<StatusIcon className={status === "critical" ? "text-destructive" : status === "warning" ? "text-attention" : "text-success"} />}
        title={
          <span className="inline-flex flex-wrap items-center gap-2">
            <span>{statusTitle}</span>
            <Badge tone={statusBadgeTone(status)} size="sm" dot data-monitor-status-badge={status}>
              {t(`devices:monitor.badge.${status}`)}
            </Badge>
          </span>
        }
        sub={<span data-monitor-coverage>{overviewSub}</span>}
        action={
          <div className="flex flex-wrap items-center gap-2">
            <SegmentedControl
              kind="radio"
              variant="tray"
              size="sm"
              aria-label={t("devices:monitor.windowLabel")}
              value={win}
              onChange={(next) => { if (next !== win) setWin(next) }}
              data={{ "data-monitor-window": "" }}
              options={HEALTH_WINDOWS.map((w) => ({
                value: w,
                label: t(`devices:monitor.window.${w}`),
                data: { "data-window": w },
              }))}
            />
            <Button size="sm" variant="outline" disabled={busy} onClick={() => void load(win)} data-monitor-refresh>
              <RefreshCwIcon className={busy ? "animate-spin" : undefined} />
              {t(busy ? "devices:monitor.refreshing" : "devices:monitor.refresh")}
            </Button>
          </div>
        }
        data-monitor-overview=""
        data-monitor-status={status}
      >
        <div className="grid grid-cols-2 gap-x-6 gap-y-5 border-t border-border/70 px-5 py-4 md:grid-cols-4">
          <Figure
            label={t("devices:monitor.kpi.alerts")}
            value={past.length}
            sub={past.length === 0 ? t("devices:monitor.kpi.alertsOk") : <span className="text-attention">{t("devices:monitor.kpi.alertsWarn", { critical: criticalCount, warning: warningCount })}</span>}
            data-monitor-kpi="alerts"
          />
          <Figure
            label={t("devices:monitor.kpi.measured")}
            value={counts.measured}
            unit={` / ${counts.total}`}
            fill={counts.total ? counts.measured / counts.total : null}
            sub={counts.noData === 0 ? t("devices:monitor.kpi.measuredAll", { total: counts.total }) : t("devices:monitor.kpi.measuredPending", { pending: counts.noData, total: counts.total })}
            data-monitor-kpi="measured"
          />
          <Figure
            label={t("devices:monitor.kpi.nodes")}
            value={nodeRows.length}
            sub={
              nodeRows.length === 0
                ? t("devices:monitor.kpi.nodesNone")
                : nodeWarnedCount > 0
                  ? <span className="text-attention">{t("devices:monitor.kpi.nodesWarn", { warned: nodeWarnedCount })}</span>
                  : t("devices:monitor.kpi.nodesAllOk")
            }
            data-monitor-kpi="nodes"
          />
          <Figure
            label={t("devices:monitor.kpi.plugins")}
            value={pluginRows.length}
            sub={
              pluginRows.length === 0
                ? t("devices:monitor.kpi.pluginsNone")
                : pluginWarnedCount > 0
                  ? <span className="text-attention">{t("devices:monitor.kpi.pluginsWarn", { warned: pluginWarnedCount })}</span>
                  : t("devices:monitor.kpi.pluginsAllOk")
            }
            data-monitor-kpi="plugins"
          />
        </div>
      </DCard>

      {/* 2. Pinned Alerts */}
      {past.length > 0 && (
        <DCard
          icon={<AlertTriangleIcon className={status === "critical" ? "text-destructive" : "text-attention"} />}
          title={t("devices:monitor.warningsTitle")}
          sub={t("devices:monitor.warningsSub")}
          data-monitor-warnings
        >
          <div className="border-t border-border/70">
            {past.map((c) => {
              const sev = severityOf(c)
              const action = c.key.startsWith("node_") && c.subject
                ? <Button size="xs" variant="outline" onClick={() => go("#/devices/nodes/" + encodeURIComponent(c.subject!))}>{t("devices:monitor.actionNode")}</Button>
                : c.key === "backup_destination_space"
                  ? <Button size="xs" variant="outline" onClick={() => go("#/settings/data")}>{t("devices:monitor.actionBackup")}</Button>
                  : null
              return (
                <div
                  key={checkId(c)}
                  className="flex flex-wrap items-start gap-x-3 gap-y-2 border-border/70 px-5 py-3 [&+&]:border-t"
                  data-health-warning={c.key}
                  data-warning-severity={sev}
                >
                  <div className="flex shrink-0 items-center gap-1.5 pt-0.5">
                    <Badge tone={sev === "critical" ? "danger" : "warning"} size="sm" dot>
                      {t(`devices:monitor.severity.${sev}`)}
                    </Badge>
                    <Chip>{scopeLabel(c)}</Chip>
                  </div>
                  <div className="min-w-0 flex-1 basis-56">
                    <p className="type-callout text-pretty text-foreground">{warningText(c)}</p>
                    {limitText(c) && <p className="mt-0.5 type-footnote text-muted-foreground">{label(c)} · {limitText(c)}</p>}
                  </div>
                  {action && <div className="ml-auto shrink-0">{action}</div>}
                </div>
              )
            })}
          </div>
        </DCard>
      )}

      {/* 3. 4 Golden Signals */}
      <DCard
        icon={<ActivityIcon />}
        title={t("devices:monitor.goldenTitle")}
        sub={t("devices:monitor.goldenSub")}
        data-monitor-golden
      >
        <div className="grid grid-cols-1 gap-3 px-5 pb-5 md:grid-cols-2">
          {GOLDEN_KEYS.map((key) => {
            const g = goldenSeries(data, key)
            const title = t(`shell:health.key.${key}`)
            const headline = g.check ? displayValue(g.check) : "—"
            const aside = g.warned
              ? <Badge tone="danger" size="sm" dot>{t("devices:monitor.overLimit")}</Badge>
              : (!g.check || g.check.value == null)
                ? <Badge tone="neutral" size="sm">{t("devices:monitor.awaitingSamples")}</Badge>
                : undefined
            return (
              <div key={key} className="min-w-0" data-golden-metric={key}>
                <KpiChart
                  className="monitor-kpi"
                  title={title}
                  value={headline}
                  labels={labels}
                  tooltipLabels={tooltipLabels}
                  series={[{ label: title, color: GOLDEN_COLOR[key], values: g.values }]}
                  format={goldenFormat(key)}
                  footer={g.check ? (limitText(g.check) ?? undefined) : undefined}
                  metricAside={aside}
                  emptyText={t("devices:monitor.chartEmpty")}
                  chartLabel={t("devices:monitor.chartAria", { title })}
                  missingText="—"
                />
              </div>
            )
          })}
        </div>
      </DCard>

      {/* 4. Hub Subsystem Groups */}
      <section className="flex flex-col gap-3" data-monitor-subsystems>
        <div className="px-1">
          <h2 className="type-headline text-foreground">{t("devices:monitor.subsystemsTitle")}</h2>
          <p className="type-footnote text-muted-foreground">{t("devices:monitor.subsystemsSub")}</p>
        </div>
        <div className="grid grid-cols-1 items-start gap-4 lg:grid-cols-2">
          {subsystems.map((g) => {
            const Icon = SUBSYSTEM_ICON[g.id]
            const hasCritical = g.warnings.some((w) => severityOf(w) === "critical")
            return (
              <DCard
                key={g.id}
                icon={<Icon />}
                title={t(`devices:monitor.subsystem.${g.id}.title`)}
                sub={t(`devices:monitor.subsystem.${g.id}.sub`)}
                action={g.warnings.length > 0 ? <Badge tone={hasCritical ? "danger" : "warning"} size="sm" count={g.warnings.length} /> : undefined}
                data-health-subsystem={g.id}
              >
                {g.measured.map((c) => {
                  const warned = warnSet.has(checkId(c))
                  const sev = severityOf(c)
                  const fill = utilization(c)
                  const meterTone: Tone = warned ? (sev === "critical" ? "bad" : "wait") : fill != null && fill >= 0.8 ? "wait" : "run"
                  const lim = limitText(c)
                  return (
                    <div key={checkId(c)} className="flex flex-col gap-1.5 border-t border-border/70 px-5 py-3" data-health-check={c.key}>
                      <div className="flex items-baseline justify-between gap-3">
                        <span className="min-w-0 flex-1 type-body text-foreground [overflow-wrap:anywhere]">
                          <span>{label(c)}</span>
                          {c.subject && <span className="ml-1.5 type-footnote text-muted-foreground">· {c.subject}</span>}
                        </span>
                        <span className={cn("shrink-0 type-callout tabular-nums font-medium", warned ? (sev === "critical" ? "text-destructive" : "text-attention") : "text-foreground")}>
                          {displayValue(c)}
                        </span>
                      </div>
                      {lim && <div className="type-footnote text-muted-foreground">{lim}</div>}
                      {fill != null && <Meter value={fill} tone={meterTone} />}
                    </div>
                  )
                })}
                {g.noData.length > 0 && (
                  <div className="border-t border-border/70 bg-muted/30 px-5 py-2.5">
                    <details className="group/fold" data-health-insufficient={g.id}>
                      <summary className="inline-flex cursor-pointer list-none items-center gap-1.5 type-footnote text-tertiary hover:text-muted-foreground [&::-webkit-details-marker]:hidden">
                        <ChevronRightIcon className="size-3.5 shrink-0 transition-transform group-open/fold:rotate-90" aria-hidden />
                        <span>
                          {g.measured.length === 0
                            ? t("devices:monitor.insufficientAll", { count: g.noData.length })
                            : t("devices:monitor.insufficientSummary", { count: g.noData.length, names: g.noData.slice(0, 3).map((c) => label(c)).join(" · ") })}
                        </span>
                      </summary>
                      <ul className="mt-2 flex flex-col gap-1.5 pl-5">
                        {g.noData.map((c) => {
                          const lim = limitText(c)
                          return (
                            <li key={checkId(c)} className="flex items-baseline justify-between gap-3 type-footnote text-tertiary" data-health-check={c.key} data-sample-state="insufficient">
                              <span className="min-w-0 [overflow-wrap:anywhere]">
                                {label(c)}
                                {lim ? ` · ${lim}` : ""}
                              </span>
                              <span className="shrink-0 tabular-nums">—</span>
                            </li>
                          )
                        })}
                      </ul>
                    </details>
                  </div>
                )}
              </DCard>
            )
          })}
        </div>
      </section>

      {/* 5. Node & Plugin Health Matrices */}
      <DCard
        icon={<ServerIcon />}
        title={t("devices:monitor.nodesTitle")}
        sub={t("devices:monitor.nodesSub")}
        data-monitor-nodes
      >
        {nodeRows.length === 0 ? (
          <div className="px-5 pb-5">
            <EmptyState bare icon={<ServerIcon />} title={t("devices:monitor.nodesEmpty")} />
          </div>
        ) : (
          <>
            <div className="hidden overflow-x-auto border-t border-border/70 sm:block">
              <table className="w-full border-collapse text-left type-callout">
                <thead>
                  <tr className="border-b border-border/70 type-footnote text-muted-foreground">
                    <th className="py-2.5 pl-5 pr-3 font-medium">{t("devices:monitor.col.node")}</th>
                    <th className="px-3 py-2.5 font-medium">{t("devices:monitor.col.link")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.drops")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.cmdFail")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.backlog")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.diskWrites")}</th>
                    <th className="py-2.5 pl-3 pr-5 text-right font-medium">{t("devices:monitor.col.status")}</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/50">
                  {nodeRows.map((r) => {
                    const rowCrit = r.warnings.some((w) => severityOf(w) === "critical")
                    return (
                      <tr key={r.node} data-monitor-node-row={r.node}>
                        <td className="py-2.5 pl-5 pr-3">
                          <a href={"#/devices/nodes/" + encodeURIComponent(r.node)} className="font-medium text-foreground hover:text-blue hover:underline">
                            {nodeDisplayName(r.node)}
                          </a>
                        </td>
                        <td className="px-3 py-2.5"><MatrixCell check={r.link} warnSet={warnSet} /></td>
                        <td className="px-3 py-2.5 text-right"><MatrixCell check={r.linkDrops} warnSet={warnSet} /></td>
                        <td className="px-3 py-2.5 text-right"><MatrixCell check={r.commandFailures} warnSet={warnSet} /></td>
                        <td className="px-3 py-2.5 text-right"><MatrixCell check={r.backlog} warnSet={warnSet} /></td>
                        <td className="px-3 py-2.5 text-right"><MatrixCell check={r.diskWrites} warnSet={warnSet} /></td>
                        <td className="py-2.5 pl-3 pr-5 text-right">
                          <Badge tone={rowCrit ? "danger" : r.warnings.length > 0 ? "warning" : "success"} size="sm" dot>
                            {r.warnings.length > 0 ? t("devices:monitor.statusWarn") : t("devices:monitor.statusOk")}
                          </Badge>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
            <div className="divide-y divide-border/70 border-t border-border/70 sm:hidden">
              {nodeRows.map((r) => {
                const rowCrit = r.warnings.some((w) => severityOf(w) === "critical")
                return (
                  <div key={r.node} className="flex flex-col gap-2 px-5 py-3" data-monitor-node-card={r.node}>
                    <div className="flex items-center justify-between gap-2">
                      <a href={"#/devices/nodes/" + encodeURIComponent(r.node)} className="min-w-0 truncate type-body font-medium text-foreground hover:text-blue hover:underline">
                        {nodeDisplayName(r.node)}
                      </a>
                      <Badge tone={rowCrit ? "danger" : r.warnings.length > 0 ? "warning" : "success"} size="sm" dot>
                        {r.warnings.length > 0 ? t("devices:monitor.statusWarn") : t("devices:monitor.statusOk")}
                      </Badge>
                    </div>
                    <div className="grid grid-cols-2 gap-x-4 gap-y-1.5 type-footnote">
                      <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.link")}</span><MatrixCell check={r.link} warnSet={warnSet} /></div>
                      <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.drops")}</span><MatrixCell check={r.linkDrops} warnSet={warnSet} /></div>
                      <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.cmdFail")}</span><MatrixCell check={r.commandFailures} warnSet={warnSet} /></div>
                      <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.backlog")}</span><MatrixCell check={r.backlog} warnSet={warnSet} /></div>
                      <div className="col-span-2 flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.diskWrites")}</span><MatrixCell check={r.diskWrites} warnSet={warnSet} /></div>
                    </div>
                  </div>
                )
              })}
            </div>
          </>
        )}
      </DCard>

      <DCard
        icon={<PlugIcon />}
        title={t("devices:monitor.pluginsTitle")}
        sub={t("devices:monitor.pluginsSub")}
        data-monitor-plugins
      >
        {pluginRows.length === 0 ? (
          <div className="px-5 pb-5">
            <EmptyState bare icon={<PlugIcon />} title={t("devices:monitor.pluginsEmpty")} />
          </div>
        ) : (
          <>
            <div className="hidden overflow-x-auto border-t border-border/70 sm:block">
              <table className="w-full border-collapse text-left type-callout">
                <thead>
                  <tr className="border-b border-border/70 type-footnote text-muted-foreground">
                    <th className="py-2.5 pl-5 pr-3 font-medium">{t("devices:monitor.col.plugin")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.callFail")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.restarts")}</th>
                    <th className="px-3 py-2.5 text-right font-medium">{t("devices:monitor.col.inboundBacklog")}</th>
                    <th className="py-2.5 pl-3 pr-5 text-right font-medium">{t("devices:monitor.col.status")}</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/50">
                  {pluginRows.map((r) => (
                    <tr key={r.plugin} data-monitor-plugin-row={r.plugin}>
                      <td className="py-2.5 pl-5 pr-3 font-medium text-foreground">{r.plugin}</td>
                      <td className="px-3 py-2.5 text-right"><MatrixCell check={r.errors} warnSet={warnSet} /></td>
                      <td className="px-3 py-2.5 text-right"><MatrixCell check={r.restarts} warnSet={warnSet} /></td>
                      <td className="px-3 py-2.5 text-right"><MatrixCell check={r.backlog} warnSet={warnSet} /></td>
                      <td className="py-2.5 pl-3 pr-5 text-right">
                        <Badge tone={r.warnings.length > 0 ? "warning" : "success"} size="sm" dot>
                          {r.warnings.length > 0 ? t("devices:monitor.statusWarn") : t("devices:monitor.statusOk")}
                        </Badge>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="divide-y divide-border/70 border-t border-border/70 sm:hidden">
              {pluginRows.map((r) => (
                <div key={r.plugin} className="flex flex-col gap-2 px-5 py-3" data-monitor-plugin-card={r.plugin}>
                  <div className="flex items-center justify-between gap-2">
                    <span className="min-w-0 truncate type-body font-medium text-foreground">{r.plugin}</span>
                    <Badge tone={r.warnings.length > 0 ? "warning" : "success"} size="sm" dot>
                      {r.warnings.length > 0 ? t("devices:monitor.statusWarn") : t("devices:monitor.statusOk")}
                    </Badge>
                  </div>
                  <div className="grid grid-cols-2 gap-x-4 gap-y-1.5 type-footnote">
                    <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.callFail")}</span><MatrixCell check={r.errors} warnSet={warnSet} /></div>
                    <div className="flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.restarts")}</span><MatrixCell check={r.restarts} warnSet={warnSet} /></div>
                    <div className="col-span-2 flex justify-between gap-2"><span className="text-muted-foreground">{t("devices:monitor.col.inboundBacklog")}</span><MatrixCell check={r.backlog} warnSet={warnSet} /></div>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </DCard>
    </div>
  )
}
