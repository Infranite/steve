/* Devices · Nodes (#/devices/nodes): every computer that runs sessions for this Hub, its own Node first, each with
 * the one status device-model gives it, its sessions and its version. A row opens the Node's page. */
import { PlusIcon, ServerIcon, SquareTerminalIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { go, Loading, openSheet } from "@/ui"
import { nodeStatus, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { Chip, DCard, DRow, StatusBadge, StatusDot } from "@/devices-ui"
import { cn } from "cn"

/** one Node as a row: its dot and name, sessions, version (amber when it is not the Hub's), status */
export function NodeRow({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion } = useDevices()
  const s = nodeStatus(v, hubVersion)
  const differs = !!v.version && !!hubVersion && v.version !== hubVersion
  return (
    <DRow icon={v.local ? <SquareTerminalIcon /> : <ServerIcon />} onClick={() => go("#/devices/nodes/" + encodeURIComponent(v.id))} data-node-row={v.id}
      title={<><StatusDot status={s} /><span className="truncate">{v.local ? t("devices:map.localNode") : v.name}</span>{v.local && <Chip>{t("devices:map.builtIn")}</Chip>}</>}
      sub={<>
        {v.max != null && v.running != null && <span className="tabular-nums">{t("devices:nodes.sessions", { n: v.running, max: v.max })}</span>}
        {v.version && <span className={cn("tabular-nums", differs && "text-attention")} data-node-version>{v.version}</span>}
        {!v.linked && v.linkError && <span className="line-clamp-1 text-destructive" title={v.linkError.error}>{linkReason(v)}</span>}
        {v.local && v.device && <span>{v.device.name}</span>}
      </>}>
      <StatusBadge status={s} />
    </DRow>
  )
}

/** the Nodes: the Hub's own first, the unpaired ones folded under the list */
export function NodesCard({ title }: { title?: string }) {
  const t = useT(), { views, nodesRead, nodesError } = useDevices()
  const live = views.filter((v) => v.state !== "revoked"), gone = views.filter((v) => v.state === "revoked")
  return (
    <DCard title={title ?? t("devices:nodes.title", { n: live.length })} sub={t("devices:nodes.sub")} data-nodes
      action={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />{t("devices:nodes.add")}</Button>}>
      {nodesError && <p role="alert" className="mx-5 mb-3 rounded-lg bg-destructive/10 px-3 py-2 type-callout text-destructive" data-node-error>{t("devices:nodes.unavailable")} · {nodesError}</p>}
      {!nodesRead && !nodesError && !live.length ? <Loading className="py-8" /> : <div className="border-t border-border/70">{live.map((v) => <NodeRow key={v.id} v={v} />)}</div>}
      {gone.length > 0 && <details className="border-t border-border/70 px-5 py-2" data-nodes-revoked>
        <summary className="cursor-pointer py-1.5 type-footnote text-muted-foreground hover:text-foreground">{t("devices:nodes.revokedN", { count: gone.length })}</summary>
        <div className="-mx-5 opacity-70">{gone.map((v) => <NodeRow key={v.id} v={v} />)}</div>
      </details>}
    </DCard>
  )
}

export function NodesTab() {
  return <NodesCard />
}

/** a short reason a Node went offline: the first part of its last connection error */
export const linkReason = (v: NodeView) => (v.linkError ? v.linkError.error.split(":")[0] : "")
