import { getItems } from "@/store"
/* What a Node is running: its sessions as the Hub tracks them (device-work groups them), those waiting for your answer
 * first with a way to answer at once, then the rest by how they stand; the last few that ended fold away below. */
import { useState } from "react"
import { MessageSquareIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DecisionCard, go } from "@/ui"
import { deviceDecision, deviceWorkEvidence, st, useStore } from "@/store"
import { groupDeviceWork, type DeviceWork } from "@/device-work"
import { useT } from "@/i18n"
import { elapsed } from "@/i18n/format"
import { agoText } from "@/device-model"
import { DCard, DRow } from "@/devices-ui"

export const deviceGroups = (id: string) => {
  const e = deviceWorkEvidence()
  return groupDeviceWork(id, e.sessions, e.decisions, new Set(getItems().filter(i => i.phase === "closed" || i.arch).map(i => i.id)))
}

function WorkRow({ row, ask }: { row: DeviceWork; ask: (id: string) => void }) {
  const t = useT(), s = row.session
  const item = getItems().find(i => i.id === s.item_id)
  const title = item ? `${item.id} ${item.t}` : s.title || s.id
  const bot = s.bot_id ? st.bots[s.bot_id]?.n || s.bot_id : null
  const state = row.decisions.length ? <Badge tone="warning" size="sm" dot>{t("devices:work.asks")}</Badge>
    : s.waiting ? <Badge tone="warning" size="sm">{t("shell:devices.waitingTag")}</Badge>
    : s.status === "error" ? <Badge tone="danger" size="sm">{t("device:state.error")}</Badge>
    : s.status === "starting" ? <Badge tone="info" size="sm" dot live>{t("device:state.starting")}</Badge> : null
  const live = s.status === "running" || s.status === "starting" || s.busy
  return (
    <DRow title={<span className="min-w-0 truncate">{title}</span>} data-work-session={s.id} data-raw-state={s.status}
      onClick={() => go(s.item_id ? "#/items/" + encodeURIComponent(s.item_id) : "#/sessions/" + encodeURIComponent(s.id))}
      sub={<>{bot && <span>{bot}</span>}<span className="tabular-nums">{live ? t("devices:work.running", { time: elapsed(Date.now() - s.created) }) : t("devices:work.updated", { ago: agoText(s.updated) })}</span>
        {row.queued > 0 && <span className="text-attention">{t("devices:work.queued", { count: row.queued })}</span>}</>}>
      {state}
      {row.decisions[0] && <Button size="xs" variant="outline" onClick={(e) => { e.stopPropagation(); ask(row.decisions[0].id) }} data-open-request={row.decisions[0].id}>{t("devices:work.answer")}</Button>}
    </DRow>
  )
}

/** the sessions on a device: answer-first, then current; the recent ones folded */
export function RunningCard({ id, offline }: { id: string | null; offline: boolean }) {
  const t = useT()
  useStore()
  const [request, setRequest] = useState<string | null>(null)
  const loaded = deviceWorkEvidence().loaded
  const g = id && loaded ? deviceGroups(id) : null
  const rows = g ? [...g.attention, ...g.current] : []
  const card = request ? deviceDecision(request) : undefined
  return (
    <DCard icon={<MessageSquareIcon />} title={rows.length ? t("devices:work.title", { n: rows.length }) : t("devices:work.titleNone")}
      sub={offline && !rows.length ? t("devices:work.offline") : t("devices:work.sub")} data-node-work={rows.length}>
      {rows.length > 0 ? <div className="border-t border-border/70">{rows.map(row => <WorkRow key={row.session.id} row={row} ask={setRequest} />)}</div>
        : !offline && loaded && <p className="px-5 pb-4 type-callout text-muted-foreground" data-work-empty>{t("devices:work.none")}</p>}
      {g && g.recent.length > 0 && <details className="border-t border-border/70 px-5 py-2" data-work-recent>
        <summary className="cursor-pointer py-1 type-footnote text-muted-foreground hover:text-foreground">{t("devices:work.recent", { count: g.recent.length })}</summary>
        <div className="-mx-5">{g.recent.map(row => <WorkRow key={row.session.id} row={row} ask={setRequest} />)}</div>
      </details>}
      {request && <Dialog open onOpenChange={open => !open && setRequest(null)}><DialogContent className="sm:max-w-xl">
        <DialogHeader><DialogTitle>{t("devices:work.answer")}</DialogTitle><DialogDescription>{card?.i.t}</DialogDescription></DialogHeader>
        {card ? <DecisionCard i={card.i} w={card.w} /> : <p className="type-callout text-muted-foreground">{t("devices:work.answered")}</p>}
      </DialogContent></Dialog>}
    </DCard>
  )
}
