import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import { Panel } from "@/panel"
import { DecisionCard, go } from "@/ui"
import { deviceDecision, deviceWorkEvidence, st } from "@/store"
import { groupDeviceWork, type DeviceWork } from "@/device-work"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import "./devices.css"

export const deviceGroups = (id: string) => {
  const e = deviceWorkEvidence()
  return groupDeviceWork(id, e.sessions, e.decisions, new Set(st.items.filter(i => i.phase === "closed" || i.arch).map(i => i.id)))
}
export function WorkCounts({ id }: { id: string }) {
  const t = useT(), g = deviceGroups(id)
  if (!deviceWorkEvidence().loaded) return <p>{t("device:unknown")}</p>
  return <div className="device-counts" data-work-counts>
    <Badge tone="warning">{t("device:attention")} {g.attention.length}</Badge>
    <Badge tone="info">{t("device:running", { n: g.running })}</Badge>
    <Badge tone="neutral">{t("device:starting", { n: g.starting })}</Badge>
    <span>{t("device:queue", { messages: g.queueMessages, sessions: g.queueSessions })}</span>
  </div>
}
function WorkRow({ row }: { row: DeviceWork }) {
  const t = useT(), [request, setRequest] = useState<string | null>(null), s = row.session
  const card = request ? deviceDecision(request) : undefined
  const item = st.items.find(i => i.id === s.item_id)
  const title = item?.t || s.title
  return <article className="device-work-row" data-work-session={s.id} data-raw-state={s.status}>
    <h3>{title}</h3>
    <div className="device-counts"><span>{t("device:bot", { name: s.bot_id ? st.bots[s.bot_id]?.n || s.bot_id : t("device:unknown") })}</span>
      {s.item_id && <span>{t("device:item", { id: s.item_id })}</span>}<span>{t("device:session", { id: s.id })}</span>
      <Badge tone={s.status === "error" ? "danger" : s.status === "running" ? "info" : "neutral"}>{t(`device:state.${s.status}`)}</Badge>
      {s.readonly && <span>{t("device:readonly")}</span>}
    </div>
    {item?.now && <p>{item.now}</p>}
    {row.decisions.map(d => <p key={d.id}>{d.question}</p>)}
    {s.busy && s.status !== "running" && <p>{t("device:busy")}</p>}
    {s.status === "error" && <p>{t("device:errorEvidence")}</p>}
    <p>{t("device:updated", { time: date(s.updated, { dateStyle: "short", timeStyle: "medium" }) })}</p>
    <div className="device-actions">{row.decisions.map(d => <Button key={d.id} size="sm" variant="outline" onClick={() => setRequest(d.id)} data-open-request={d.id}>{t("device:openRequest")} · {d.id}</Button>)}
      {s.item_id && <Button size="sm" variant="outline" onClick={() => go("#/items/" + encodeURIComponent(s.item_id!))}>{t("device:openWork")}</Button>}
      <Button size="sm" variant="ghost" onClick={() => go("#/sessions/" + encodeURIComponent(s.id))}>{t("device:openSession")}</Button>
    </div>
    {row.queue.length > 0 && <details data-session-queue={s.id}><summary>{t("device:queue", { messages: row.queue.length, sessions: 1 })}</summary>
      <p>{t("device:queueNote")}</p>{row.queue.map(q => <div key={q.id} data-queue-id={q.id}><strong>{q.id}</strong><p>{q.text}</p></div>)}
    </details>}
    {request && <Dialog open onOpenChange={open => !open && setRequest(null)}><DialogContent className="device-evidence sm:max-w-xl"><DialogHeader>
      <DialogTitle>{t("device:requests", { id: request })}</DialogTitle><DialogDescription>{title} · {s.item_id} · {s.id} · {t("device:raw", { status: s.status })}</DialogDescription>
    </DialogHeader>{card ? <DecisionCard i={card.i} w={card.w} /> : <p>{t("device:unknown")}</p>}
    <Button variant="outline" onClick={() => go("#/sessions/" + encodeURIComponent(s.id))}>{t("device:openSession")}</Button></DialogContent></Dialog>}
  </article>
}
export function DeviceWorkSection({ id }: { id: string }) {
  const t = useT(), g = deviceGroups(id), evidence = deviceWorkEvidence()
  const unassigned = evidence.sessions.some(s => !s.readonly && s.queued.length && (!s.device_id || !st.devices.some(d => d.id === s.device_id)))
  return <div className="device-work-groups">
    {st.down && <p role="status">{t("device:offline")}</p>}
    <Panel k="attention" title={`${t("device:attention")} · ${g.attention.length}`}>{g.attention.map(row => <WorkRow key={row.session.id} row={row} />)}</Panel>
    <Panel k="current" title={`${t("device:current")} · ${g.current.length}`}>
      {g.current.map(row => <WorkRow key={row.session.id} row={row} />)}
      {!g.current.length && <p className="p-5">{t("device:empty")}</p>}
    </Panel>
    {unassigned && <p role="note">{t("device:unassigned")}</p>}
    <details className="device-recent"><summary>{t("device:recent", { count: g.recentTotal })}</summary>{g.recent.map(row => <WorkRow key={row.session.id} row={row} />)}</details>
  </div>
}
