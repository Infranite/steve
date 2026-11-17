/* Devices · Overview (#/devices): first what wants you (a Node to confirm, offline, behind the Hub, an upgrade under
 * way or failed), then four numbers (sessions, Nodes online, Nodes on the Hub's version, clients), then the Nodes. */
import { CircleAlertIcon, ShieldAlertIcon, ArrowUpCircleIcon, WifiOffIcon, LoaderIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { go } from "@/ui"
import { agoText, nodeStatus, type NodeStatus, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard, Figure } from "@/devices-ui"
import { NodesGrid, linkReason } from "@/devices-nodes"
import { cn } from "cn"
import type { ReactNode } from "react"

const ICON: Partial<Record<NodeStatus["kind"], typeof CircleAlertIcon>> = { pending: ShieldAlertIcon, offline: WifiOffIcon, rolledBack: CircleAlertIcon, outdated: ArrowUpCircleIcon, upgrading: LoaderIcon }

export function OverviewTab() {
  const t = useT(), { views, hubVersion, access } = useDevices()
  const now = Date.now()
  const live = views.filter((v) => v.state !== "revoked")
  const rows: { v: NodeView; s: NodeStatus; text: ReactNode }[] = []
  for (const v of live) {
    const s = nodeStatus(v, hubVersion), name = v.local ? t("devices:map.localNode") : v.name
    const text = s.kind === "pending" ? t("devices:attn.pending", { name })
      : s.kind === "offline" ? <>{t("devices:attn.offline", { name, ago: s.since ? agoText(s.since, now) : "—" })}{linkReason(v) && <span className="text-muted-foreground"> · {linkReason(v)}</span>}</>
      : s.kind === "rolledBack" ? t("devices:attn.failed", { name, to: s.version ?? "", from: s.from ?? "" })
      : s.kind === "outdated" ? t("devices:attn.outdated", { name, v: v.version ?? "?", hub: hubVersion })
      : s.kind === "upgrading" ? t("devices:attn.upgrading", { name, to: s.version ?? "" }) : null
    if (text) rows.push({ v, s, text })
  }
  const running = live.reduce((n, v) => n + (v.running ?? 0), 0), max = live.reduce((n, v) => n + (v.max ?? 0), 0), queued = live.reduce((n, v) => n + (v.waiting ?? 0), 0)
  const online = live.filter((v) => v.linked && v.state !== "pending").length
  const versioned = live.filter((v) => v.version), same = versioned.filter((v) => v.version === hubVersion).length
  const clients = access?.clients.filter((c) => !c.revoked).length
  return (
    <div className="flex min-w-0 flex-col gap-4" data-devices-overview>
      {rows.length > 0 && <div className="flex flex-col overflow-hidden rounded-xl bg-attention/8 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--attention)_25%,transparent)]" data-attention>
        {rows.map(({ v, s, text }) => { const I = ICON[s.kind] ?? CircleAlertIcon; return (
          <div key={v.id} className="flex items-center gap-3 px-4 py-2.5 [&+&]:border-t [&+&]:border-attention/15" data-attention-row={s.kind}>
            <I className={cn("size-4 shrink-0", s.tone === "bad" ? "text-destructive" : s.tone === "info" ? "text-blue" : s.tone === "off" ? "text-muted-foreground" : "text-attention")} aria-hidden />
            <span className="min-w-0 flex-1 type-callout text-foreground">{text}</span>
            <Button size="xs" variant="ghost" onClick={() => go("#/devices/nodes/" + encodeURIComponent(v.id))}>{t("devices:attn.open")}</Button>
          </div>) })}
      </div>}
      <DCard data-device-stats><div className="grid grid-cols-2 gap-x-6 gap-y-5 px-5 py-4 md:grid-cols-4">
        <Figure label={t("devices:stat.sessions")} value={running} unit={max ? ` / ${max}` : undefined} fill={max ? running / max : null} sub={queued ? t("devices:stat.queued", { n: queued }) : t("devices:stat.noQueue")} data-stat="sessions" />
        <Figure label={t("devices:stat.nodesOnline")} value={online} unit={` / ${live.length}`} sub={online < live.length ? t("devices:stat.nodesOffline", { n: live.length - online }) : t("devices:stat.allOnline")} data-stat="nodes" />
        <Figure label={t("devices:stat.sameVersion")} value={same} unit={` / ${versioned.length}`} sub={hubVersion ? t("devices:stat.hubVersion", { version: hubVersion }) : undefined} data-stat="versions" />
        <Figure label={t("devices:stat.clientsSignedIn")} value={clients ?? "—"} sub={t("devices:stat.clientsSub")} data-stat="clients" />
      </div></DCard>
      <NodesGrid />
    </div>
  )
}
