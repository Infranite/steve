/* Devices · Overview (#/devices): first what wants you, one line each with its next step (a Node to confirm, an upgrade
 * that failed and went back, one under way, a Node behind the Hub, a Node offline, sessions waiting for an answer), then
 * four numbers (sessions, Nodes online, Nodes on the Hub's version, clients online), then the map of how everything
 * connects (devices-map.tsx) and the Node cards. */
import { useState, type ReactNode } from "react"
import { ArrowUpCircleIcon, CircleAlertIcon, LoaderIcon, MessageCircleIcon, RotateCcwIcon, ShieldAlertIcon, WifiOffIcon } from "lucide-react"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { go, openSheet } from "@/ui"
import { api } from "@/api"
import { deviceWorkEvidence, st, toast, useStore } from "@/store"
import { nodeStatus, offlineReason, statusText, upgradeCard, type NodeView } from "@/device-model"
import { mapClients } from "@/device-map-model"
import { useDevices } from "@/devices-data"
import { deviceGroups } from "@/devices-work"
import { DCard, Figure } from "@/devices-ui"
import { DeviceMap } from "@/devices-map"
import { NodesGrid, nodeHref } from "@/devices-nodes"
import { cn } from "cn"

/** at most this many lines of what wants you; the rest behind "N more" */
const ATTENTION_ROWS = 4

type Row = { key: string; tone: "bad" | "warn" | "info"; icon: typeof CircleAlertIcon; text: ReactNode; actions: ReactNode }

export function OverviewTab() {
  const t = useT(), { views, hubVersion, access, reloadNodes } = useDevices()
  useStore()
  const [all, setAll] = useState(false), [busy, setBusy] = useState("")
  const now = Date.now()
  const live = views.filter((v) => v.state !== "revoked")
  const open = (v: NodeView, label = t("devices:attn.open")) => <Button size="xs" variant="ghost" onClick={() => go(nodeHref(v.id))} data-attention-open={v.id}>{label}</Button>
  const upgrade = async (v: NodeView) => {
    setBusy(v.id)
    try { await api.upgradeNode(v.id); toast(t("devices:attn.upgradeStarted", { name: v.name })); reloadNodes() } catch (e) { toast((e as Error).message) } finally { setBusy("") }
  }
  const rows: Row[] = []
  for (const v of live) {
    const s = nodeStatus(v, hubVersion), name = v.local ? t("devices:map.localNode") : v.name, card = upgradeCard(v, hubVersion)
    if (s.kind === "pending") rows.push({ key: v.id, tone: "warn", icon: ShieldAlertIcon, text: t("devices:attn.pending", { name }),
      actions: st.owner ? <Button size="xs" onClick={() => openSheet("addDevice", { kind: "node", node: v.id })} data-attention-confirm={v.id}>{t("devices:attn.confirm")}</Button> : open(v) })
    else if (s.kind === "rolledBack") rows.push({ key: v.id, tone: "bad", icon: RotateCcwIcon, text: t("devices:attn.failed", { name, to: s.version ?? "", from: s.from ?? "" }), actions: open(v, t("devices:attn.why")) })
    else if (s.kind === "upgrading") rows.push({ key: v.id, tone: "info", icon: LoaderIcon, text: t("devices:attn.upgrading", { name, to: s.version ?? "" }), actions: open(v) })
    else if (s.kind === "offline") rows.push({ key: v.id, tone: "warn", icon: WifiOffIcon,
      text: <>{t("devices:attn.nodeStatus", { name, status: statusText(s, now) })}{offlineReason(v) && <span className="text-muted-foreground">{t("devices:attn.reason", { why: offlineReason(v) })}</span>}</>, actions: open(v) })
    else if (card.kind === "outdated" || card.kind === "manual" && card.behind) rows.push({ key: v.id, tone: "warn", icon: ArrowUpCircleIcon, text: t("devices:attn.outdated", { name, v: v.version ?? "?", hub: hubVersion }),
      actions: <>{open(v, t("devices:card.details"))}{card.kind === "outdated" && st.owner && <Button size="xs" disabled={busy === v.id} onClick={() => void upgrade(v)} data-attention-upgrade={v.id}>{t("devices:attn.upgrade")}</Button>}</> })
    const asks = v.device && deviceWorkEvidence().loaded ? deviceGroups(v.device.id).attention.length : 0
    if (asks > 0) rows.push({ key: v.id + ":ask", tone: "warn", icon: MessageCircleIcon, text: t("devices:attn.ask", { name, count: asks }),
      actions: <Button size="xs" variant="outline" onClick={() => go(nodeHref(v.id))} data-attention-ask={v.id}>{t("devices:attn.answer")}</Button> })
  }
  const order = { bad: 0, warn: 1, info: 2 }
  rows.sort((a, b) => order[a.tone] - order[b.tone])
  const shown = all ? rows : rows.slice(0, ATTENTION_ROWS)
  const worst = rows.some((r) => r.tone === "bad")

  const counted = live.filter((v) => v.state !== "pending")
  const running = counted.reduce((n, v) => n + (v.linked ? v.running ?? 0 : 0), 0), max = counted.reduce((n, v) => n + (v.linked ? v.max ?? 0 : 0), 0)
  const queued = counted.reduce((n, v) => n + (v.waiting ?? 0), 0)
  const online = counted.filter((v) => v.linked).length
  const versioned = counted.filter((v) => v.version), same = versioned.filter((v) => v.version === hubVersion).length
  const clients = mapClients(access, t("devices:clients.browserSession"))
  return (
    <div className="flex min-w-0 flex-col gap-4" data-devices-overview>
      {rows.length > 0 && <div className={cn("flex flex-col overflow-hidden rounded-xl", worst ? "bg-destructive/6 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--destructive)_22%,transparent)]" : "bg-attention/8 shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--attention)_25%,transparent)]")} data-attention>
        {shown.map((r) => (
          <div key={r.key} className="flex min-h-11 flex-wrap items-center gap-x-3 gap-y-1.5 border-attention/15 px-4 py-2 [&+&]:border-t" data-attention-row={r.key}>
            <r.icon className={cn("size-4 shrink-0", r.tone === "bad" ? "text-destructive" : r.tone === "info" ? "text-blue" : "text-attention")} aria-hidden />
            <span className="min-w-0 flex-1 basis-48 type-callout text-pretty text-foreground">{r.text}</span>
            <span className="ml-auto flex shrink-0 items-center gap-1.5">{r.actions}</span>
          </div>))}
        {rows.length > ATTENTION_ROWS && <button type="button" className="border-t border-attention/15 px-4 py-2.5 text-left type-footnote text-muted-foreground hover:text-foreground" onClick={() => setAll((x) => !x)} data-attention-more>
          {all ? t("devices:attn.less") : t("devices:attn.more", { count: rows.length - ATTENTION_ROWS })}</button>}
      </div>}
      <DCard data-device-stats><div className="grid grid-cols-2 gap-x-6 gap-y-5 px-5 py-4 md:grid-cols-4">
        <Figure label={t("devices:stat.sessions")} value={running} unit={max ? ` / ${max}` : undefined} fill={max ? running / max : null}
          sub={queued ? <span className="text-attention">{t("devices:stat.queued", { n: queued })}</span> : t("devices:stat.noQueue")} data-stat="sessions" />
        <Figure label={t("devices:stat.nodesOnline")} value={online} unit={` / ${counted.length}`} sub={online < counted.length ? t("devices:stat.nodesOffline", { n: counted.length - online }) : t("devices:stat.allOnline")} data-stat="nodes" />
        <Figure label={t("devices:stat.sameVersion")} value={same} unit={` / ${versioned.length}`} sub={hubVersion ? t("devices:stat.hubVersion", { version: hubVersion }) : undefined} data-stat="versions" />
        <Figure label={t("devices:stat.clientsOnline")} value={access ? clients.online : "—"} sub={access ? t("devices:stat.signedIn", { n: clients.total }) : undefined} data-stat="clients" />
      </div></DCard>
      <DeviceMap />
      <NodesGrid />
    </div>
  )
}
