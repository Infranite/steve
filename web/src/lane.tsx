/* 项目页的「合并通道」：Steve 认出的这个仓库怎么合并、托管方要求什么检查、怎么发布、哪些文件每次都会撞，以及近 7 天的节奏，每一项都写明依据；"没找到"和"查不到"分开写。下面是通道本身：在途和合批中的批次、"立即合并发布"、合批窗口和上限 */
import { api, type RLane, type RLaneFact, type RLaneMember, type RLaneState, type RLaneUnit } from "@/api"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { toast } from "@/store"
import { errMsg } from "@/github/index"
import { Group, Meta, Row, SectionLabel } from "@/ui"
import { TriangleAlertIcon } from "lucide-react"
import { type ReactNode, useEffect, useState } from "react"
import { t, useT } from "@/i18n"
import { QueueSection } from "@/merge-queue"

/* labels are functions so they follow a language change */
const MERGE: Record<string, () => string> = { host_queue: () => t("shell:lane.merge.hostQueue"), host_pr: () => t("shell:lane.merge.hostPr"), push: () => t("shell:lane.merge.push") }
const SHIP: Record<string, () => string> = { tag: () => t("shell:lane.ship.tag"), script: () => t("shell:lane.ship.script"), merge: () => t("shell:lane.ship.merge") }
const FROM: Record<string, () => string> = { required: () => t("shell:lane.from.required"), suggested: () => t("shell:lane.from.suggested") }
const EVENT: Record<string, () => string> = { pull_request: () => "PR", pull_request_target: () => "PR", merge_group: () => t("shell:lane.event.mergeGroup") }
const from = (c: { from: string; on?: string[] }) => c.from === "workflow" ? t("shell:lane.runsOn", { events: [...new Set((c.on ?? []).map((e) => EVENT[e]?.() ?? e))].join(t("shell:list.sep")) }) : FROM[c.from]?.()

function How({ f }: { f: RLaneFact<unknown> }) {
  const t = useT()
  if (f.how.state === "unknown") return <Badge variant="outline" className="font-normal text-attention">{t("shell:lane.unknown")}</Badge>
  if (f.how.state === "default") return <Badge variant="outline" className="font-normal text-muted-foreground">{t("shell:lane.default")}</Badge>
  return null
}

function Why({ f, extra }: { f: RLaneFact<unknown>; extra?: ReactNode }) {
  const lines = [...(f.how.state === "unknown" ? [f.how.why] : []), ...f.evidence]
  return <span className="flex flex-col gap-0.5">{extra}{lines.map((l, i) => <span key={i}>{l}</span>)}</span>
}

const names = (ms: RLaneMember[]) => ms.map((m) => m.label).join(t("shell:list.sep"))
const clock = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
const unitState = (u: RLaneUnit) => u.merged ? t("shell:lane.batch.releasing") : u.approved ? t("shell:lane.batch.merging") : t("shell:lane.batch.waiting")

/* the lane itself: the unit in flight, the batch collecting (with "merge and release now"), and your limits */
function Batches({ project, lane, onLane }: { project: string; lane: RLaneState | null; onLane: (l: RLaneState | null) => void }) {
  const t = useT()
  const minutes = (ms: number | null | undefined) => (ms == null ? "" : String(Math.round(ms / 60_000)))
  const [win, setWin] = useState(minutes(lane?.set.window_ms)), [max, setMax] = useState(lane?.set.max_members == null ? "" : String(lane.set.max_members)), [busy, setBusy] = useState(false)
  useEffect(() => { setWin(minutes(lane?.set.window_ms)); setMax(lane?.set.max_members == null ? "" : String(lane.set.max_members)) }, [lane?.set.window_ms, lane?.set.max_members])
  const run = async (f: () => Promise<void>) => { setBusy(true); try { await f() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) } }
  const now = () => run(async () => { const r = await api.laneNow(project); onLane(r.lane); toast(r.closed ? t("shell:lane.batch.nowDone") : t("shell:lane.batch.nowNothing")) })
  const save = () => run(async () => {
    const num = (v: string) => (v.trim() === "" ? null : Number(v))
    const r = await api.lanePolicy(project, { window_min: num(win), max_members: num(max) }); onLane(r.lane); toast(t("common:state.saved"))
  })
  const d = lane?.defaults ?? { window_ms: 2 * 3_600_000, max_members: 10 }
  const r = lane?.running, c = lane?.collecting
  return (
    <Group>
      {r && <Row title={t("shell:lane.batch.unit", { no: r.no, state: unitState(r) })} sub={[names(r.members), r.deferred.length ? t("shell:lane.batch.carries", { names: names(r.deferred) }) : ""].filter(Boolean).join(" · ")} data-lane-running />}
      {c && <Row title={c.closed ? t("shell:lane.batch.closed", { no: c.no }) : t("shell:lane.batch.collecting", { no: c.no, time: clock(c.closes_at) })} data-lane-collecting
        sub={[names(c.members), c.deferred.length ? t("shell:lane.batch.carries", { names: names(c.deferred) }) : ""].filter(Boolean).join(" · ")}>
        {!c.closed && <Button size="sm" variant="outline" disabled={busy} onClick={() => void now()} data-lane-now>{t("shell:lane.batch.now")}</Button>}
      </Row>}
      {!r && !c && <Row title={t("shell:lane.batch.idle")} data-lane-idle />}
      <Row title={t("shell:lane.batch.limits")} sub={t("shell:lane.batch.limitsSub", { minutes: Math.round(d.window_ms / 60_000), count: d.max_members })} data-lane-limits>
        <span className="flex items-center gap-1.5">
          <Input className="h-8 w-16 text-right tabular-nums" inputMode="numeric" value={win} placeholder={String(Math.round(d.window_ms / 60_000))} onChange={(e) => setWin(e.target.value)} aria-label={t("shell:lane.batch.window")} data-lane-window />
          <span className="text-muted-foreground type-footnote">{t("shell:lane.batch.minutes")}</span>
          <Input className="h-8 w-14 text-right tabular-nums" inputMode="numeric" value={max} placeholder={String(d.max_members)} onChange={(e) => setMax(e.target.value)} aria-label={t("shell:lane.batch.max")} data-lane-max />
          <span className="text-muted-foreground type-footnote">{t("shell:lane.batch.items")}</span>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => void save()} data-lane-save>{t("common:action.save")}</Button>
        </span>
      </Row>
    </Group>
  )
}

export function LaneSection({ project }: { project: string }) {
  const [lane, setLane] = useState<RLane | null>(null), [err, setErr] = useState<string | null>(null), t = useT()
  useEffect(() => { setLane(null); setErr(null); api.lane(project).then(setLane, (e) => setErr(errMsg(e))) }, [project])
  return (
    <section data-lane>
      <SectionLabel className="text-tertiary">{t("shell:lane.title")}</SectionLabel>
      {err ? <Group><Row title={t("shell:lane.unrecognized")} sub={err} /></Group>
        : !lane ? <Group><Row title={<span className="inline-flex items-center gap-2 text-muted-foreground"><Spinner />{t("shell:lane.loading")}</span>} /></Group>
        : <Group>
          <Row title={<span className="type-code">{lane.target.upstream}#{lane.target.branch}</span>} data-lane-target
            sub={<span className="flex flex-col gap-0.5"><span>{t("shell:lane.reads", { reads: lane.reads })}</span>{lane.fetch_error && <span className="flex items-start gap-1 text-attention"><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" /><span className="break-all">{t("shell:lane.fetchError", { error: lane.fetch_error })}</span></span>}</span>} />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.mergeTitle", { value: MERGE[lane.merge.value]?.() ?? lane.merge.value })}<How f={lane.merge} /></span>} sub={<Why f={lane.merge} />} data-lane-merge />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.checksTitle", { value: lane.checks.value.filter((c) => c.from !== "suggested").length || (lane.checks.how.state === "unknown" ? t("shell:lane.unsure") : t("shell:lane.none")) })}<How f={lane.checks} /></span>} data-lane-checks
            sub={<Why f={lane.checks} extra={<><span>{t("shell:lane.checksNote")}</span>{lane.checks.value.length > 0 && <span className="flex flex-wrap gap-1 py-0.5">{lane.checks.value.map((c) => <Badge key={c.from + c.name} variant="outline" className="h-auto max-w-full justify-start whitespace-normal text-left font-normal text-muted-foreground"><span className="font-mono break-all">{c.name}</span><span className="shrink-0">· {from(c)}</span></Badge>)}</span>}</>} />} />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.shipTitle", { value: SHIP[lane.ship.value]?.() ?? lane.ship.value })}<How f={lane.ship} /></span>} sub={<Why f={lane.ship} />} data-lane-ship />
          {lane.numbered.map((n) => <Row key={n.kind + n.path} title={<span><span className="type-code">{n.path}</span> · {n.evidence}</span>} sub={n.handling} data-lane-numbered={n.kind} />)}
          <Row title={t("shell:lane.days", { count: lane.pace.days })} sub={<Meta><span>{t("shell:lane.landed", { count: lane.pace.landed })}</span><span>{t("shell:lane.releases", { count: lane.pace.releases })}</span><span>{t("shell:lane.authors", { count: lane.pace.authors })}</span></Meta>} data-lane-pace />
        </Group>}
      {lane && (lane.queue
        ? <div className="mt-3"><QueueSection project={project} queue={lane.queue} onQueue={(q) => setLane({ ...lane, queue: q })} /></div>
        : <div className="mt-3" data-lane-batches><Batches project={project} lane={lane.lane} onLane={(l) => setLane({ ...lane, lane: l })} /></div>)}
    </section>
  )
}
