/* Devices · Nodes (#/devices/nodes): every computer that runs sessions for this Hub, one card each, its own Node first.
 * A card leads with what needs doing (at most two lines), then four figures with their last hour: CPU load, memory,
 * network throughput and session tokens, and at the foot what its agents are doing and its harnesses.
 * The whole card opens the Node's page. Unpaired Nodes fold away under the cards. */
import type { ReactNode } from "react"
import { ChevronRightIcon, LaptopIcon, MonitorIcon, PlusIcon, ServerIcon } from "lucide-react"
import { useT } from "@/i18n"
import { num } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Card } from "@/components/affect/card-clean"
import { HoverCard } from "@/components/affect/hover-card-clean"
import { go, Loading, openSheet } from "@/ui"
import { deviceWorkEvidence, useStore } from "@/store"
import type { RVitals } from "@/api"
import { hostLine, hostOf, linkRoute, loadPct, memPct, nodeAlerts, nodeStatus, one, type HostKind, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { deviceGroups } from "@/devices-work"
import { harnessSummary } from "@/node-harness-model"
import { Alert, Chip, Spark, StatusBadge } from "@/devices-ui"
import { nodeHref } from "@/node-detail-model"
import { cn } from "cn"

/** how many of the latest samples a card draws: the last hour at one a minute */
const SPARK_POINTS = 60
const GIB = 2 ** 30

export const hostIcon = (k: HostKind) => (k === "mac" ? <LaptopIcon /> : k === "linux" ? <ServerIcon /> : <MonitorIcon />)
export { nodeHref }
/** a short reason a Node went offline: the first part of its last connection error */

/** what its agents are doing, from the sessions the Hub tracks on it: none, or how many and how many at work */
export function agentsLine(v: NodeView): { n: number; working: number } {
  if (v.device && deviceWorkEvidence().loaded) {
    const g = deviceGroups(v.device.id)
    return { n: g.attention.length + g.current.length, working: g.running }
  }
  return { n: v.running ?? 0, working: v.running ?? 0 }
}

/** one of a card's four figures: its label, the value large, one quiet line and its last hour */
function Metric({ label, value, sub, children, ...rest }: { label: ReactNode; value: ReactNode; sub?: ReactNode; children?: ReactNode } & { [k: `data-${string}`]: unknown }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5" {...rest}>
      <span className="type-footnote text-muted-foreground">{label}</span>
      <span className="truncate type-headline tabular-nums text-foreground">{value}</span>
      <span className="truncate type-footnote tabular-nums text-muted-foreground">{sub ?? " "}</span>
      <div className="mt-1.5">{children}</div>
    </div>
  )
}

const tail = <T,>(xs: T[]) => xs.slice(-SPARK_POINTS)

function bytesPerSecond(bytes: number) {
  const [unit, divisor] = bytes >= 1e6 ? ["MB/s", 1e6] : bytes >= 1e3 ? ["KB/s", 1e3] : ["B/s", 1]
  return `${num(bytes / divisor, { maximumFractionDigits: 1 })} ${unit}`
}

const compactTokens = (n: number) => num(n, { notation: "compact", maximumFractionDigits: 1 })

/** one Node as a card */
export function NodeCard({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion, vitals, harnesses, harnessesError } = useDevices()
  useStore()
  const vit: RVitals | undefined = vitals?.[v.id]
  const l = v.linked ? vit?.latest : undefined
  const s = nodeStatus(v, hubVersion), host = hostOf(v, vit?.latest), facts = hostLine(host)
  const alerts = nodeAlerts(v, hubVersion)
  const lp = loadPct(l), mp = memPct(l)
  const series = tail(vit?.series ?? [])
  const network = l?.network
  const received = series.map((p) => p.network?.rx_bytes_per_second ?? null)
  const sent = series.map((p) => p.network?.tx_bytes_per_second ?? null)
  const networkMax = Math.max(1, ...received.map((n) => n ?? 0), ...sent.map((n) => n ?? 0))
  const tokens = vit?.tokens
  const agents = agentsLine(v), harness = harnessesError ? t("devices:harness.readFailed") : harnessSummary(v, harnesses?.[v.id])
  const name = v.local ? t("devices:map.localNode") : v.name
  const open = () => go(nodeHref(v.id))
  return (
    <Card interactive className={cn("steve-card flex min-w-0 cursor-pointer flex-col gap-3 p-4", !v.linked && "opacity-75")} onClick={open} data-node-row={v.id} data-node-card={s.kind}>
      <header className="flex min-w-0 items-start gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4.5 [&_svg]:stroke-[1.75]" aria-hidden>{hostIcon(host.kind)}</span>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <button type="button" className="min-w-0 truncate text-left type-headline text-foreground focus-visible:underline focus-visible:outline-none" onClick={(e) => { e.stopPropagation(); open() }}>{name}</button>
            {v.local && <Chip>{t("devices:map.builtIn")}</Chip>}
            <StatusBadge status={s} />
          </div>
          {(facts || v.local) && <HoverCard side="bottom" align="start" content={<HostCard v={v} facts={facts} host={host} />}>
            <span tabIndex={0} className="block truncate type-footnote text-muted-foreground outline-none focus-visible:underline" data-node-host>{facts || v.device?.name}</span>
          </HoverCard>}
        </div>
      </header>
      {alerts.length > 0 && <div className="flex flex-col gap-1.5">{alerts.map((a) => <Alert key={a.kind} tone={a.tone} data-node-alert={a.kind}>{a.text}</Alert>)}</div>}
      <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4">
        <Metric label={t("devices:vitals.cpu")} value={lp != null ? `${lp}%` : "—"} sub={l?.load ? t("devices:card.load", { load: num(one(l.load[0]), { minimumFractionDigits: 2, maximumFractionDigits: 2 }) }) : undefined} data-metric="cpu">
          <Spark values={series.map((p) => (p.load == null ? null : p.load * 100))} max={100} color="var(--series-1)" />
        </Metric>
        <Metric label={t("devices:vitals.mem")} value={mp != null ? `${mp}%` : "—"} data-metric="memory"
          sub={l?.mem_total && l.mem_available != null ? t("devices:vitals.memSub", { used: num((l.mem_total - l.mem_available) / GIB, { maximumFractionDigits: 1 }), total: num(l.mem_total / GIB, { maximumFractionDigits: 1 }) }) : undefined}>
          <Spark values={series.map((p) => p.mem)} max={1} color="var(--series-2)" />
        </Metric>
        <Metric label={t("devices:vitals.network")} data-metric="network"
          value={network ? <span title={t("devices:vitals.receive")}>↓ {bytesPerSecond(network.rx_bytes_per_second)}</span> : "—"}
          sub={network && <span title={t("devices:vitals.send")}>↑ {bytesPerSecond(network.tx_bytes_per_second)}</span>}>
          {series.some((p) => p.network) && <div className="relative">
            <Spark values={received} max={networkMax} color="var(--series-3)" data-network="receive" />
            <Spark values={sent} max={networkMax} color="var(--series-4)" className="absolute inset-0" data-network="send" />
          </div>}
        </Metric>
        <Metric label={t("devices:vitals.tokens")} data-metric="tokens"
          value={tokens ? t("devices:vitals.tokensMinute", { n: compactTokens(tokens.per_minute) }) : "—"}
          sub={tokens && t("devices:vitals.tokensToday", { n: compactTokens(tokens.today) })} />
      </div>
      <footer className="-mx-4 -mb-4 mt-auto flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 border-t border-border/70 px-4 py-2.5 type-footnote text-muted-foreground">
        <span className="min-w-0 flex-1 truncate" data-node-agents={agents.n}>{agents.n ? t("devices:card.agents", { count: agents.n, working: agents.working }) : t("devices:card.noAgents")}</span>
        <span className="order-last w-full min-w-0 truncate" title={harness} data-node-harness>{harness}</span>
        <span className="inline-flex shrink-0 items-center text-foreground/80">{t("devices:card.details")}<ChevronRightIcon className="size-3.5" aria-hidden /></span>
      </footer>
    </Card>
  )
}

/** the hover card on a Node's machine line: everything known about the machine and how it is reached */
function HostCard({ v, facts, host }: { v: NodeView; facts: string; host: ReturnType<typeof hostOf> }) {
  const t = useT(), route = linkRoute(v)
  const rows: [string, string][] = [
    [t("devices:card.machine"), facts || "—"],
    ...(host.hostname ? [[t("devices:card.hostname"), host.hostname] as [string, string]] : []),
    ...(route ? [[t("devices:card.route"), t(`devices:route.${route}`) + (v.node?.link?.tls ? " · TLS" : "")] as [string, string]] : []),
    [t("devices:node.nodeId"), v.id],
    ...(v.version ? [[t("devices:card.version"), v.version] as [string, string]] : []),
  ]
  return <dl className="grid max-w-80 grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 type-footnote" data-node-hostcard>
    {rows.map(([k, val]) => <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="[overflow-wrap:anywhere] text-foreground">{val}</dd></div>)}
  </dl>
}

/** the Nodes as cards: the Hub's own first, the unpaired ones folded under them */
export function NodesGrid({ action = true }: { action?: boolean }) {
  const t = useT(), { views, nodesRead, nodesError, upgradeDefault } = useDevices()
  const live = views.filter((v) => v.state !== "revoked"), gone = views.filter((v) => v.state === "revoked")
  return (
    <section className="flex min-w-0 flex-col gap-3" data-nodes>
      <header className="flex flex-wrap items-end gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1">
          <h2 className="type-headline text-foreground">{t("devices:nodes.title", { n: live.length })}</h2>
          <p className="type-footnote text-pretty text-muted-foreground">{t("devices:nodes.sub")}</p>
        </div>
        {action && <Button size="sm" variant="outline" onClick={() => openSheet("addDevice", { kind: "node" })} data-pair><PlusIcon />{t("devices:nodes.add")}</Button>}
      </header>
      {nodesError && <p role="alert" className="rounded-lg bg-destructive/10 px-3 py-2 type-callout text-destructive" data-node-error>{t("devices:nodes.unavailable")} · {nodesError}</p>}
      {!nodesRead && !nodesError && !live.length ? <Loading className="py-8" /> : <div className="grid gap-4 lg:grid-cols-2">{live.map((v) => <NodeCard key={v.id} v={v} />)}</div>}
      {upgradeDefault && <p className="type-footnote text-muted-foreground" data-nodes-policy>{t("devices:nodes.policyNote", { policy: t(`device:upgrade.policy.${upgradeDefault.policy}`) })} <a className="text-blue hover:underline" href="#/devices/hub">{t("devices:tab.hub")}</a></p>}
      {gone.length > 0 && <details className="rounded-xl px-1 py-1" data-nodes-revoked>
        <summary className="cursor-pointer py-1.5 type-footnote text-muted-foreground hover:text-foreground">{t("devices:nodes.revokedN", { count: gone.length })}</summary>
        <div className="mt-2 flex flex-col gap-1 opacity-70">{gone.map((v) => <button key={v.id} type="button" className="flex items-center gap-2 rounded-lg px-2 py-1.5 text-left type-callout hover:bg-accent/60" onClick={() => go(nodeHref(v.id))} data-node-row={v.id}>
          <span className="min-w-0 flex-1 truncate">{v.name}</span><StatusBadge status={nodeStatus(v)} /></button>)}</div>
      </details>}
    </section>
  )
}

export function NodesTab() {
  return <NodesGrid />
}
