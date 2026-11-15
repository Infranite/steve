/* 项目页里合并通道的几块，projects.tsx 决定它们排在哪里：
 *  - useLane：读一次 GET /projects/:id/lane（Steve 认出的这个仓库怎么合并、要什么检查、怎么发布、哪些文件每次都会撞、近 7 天的节奏）
 *  - MERGE / SHIP / How / Unknown：合并方式和发布方式的说法，概览的"仓库与交付"卡片用它们，一行一项
 *  - LaneNow：在途和合批中的批次和"立即合并发布"；没有批次时什么都不显示（合并队列开着时是它的在途部分）
 *  - LaneConventions：折叠的"仓库约定"：检查、每次都会撞的文件、每项是怎么认出来的；每样只列一次
 *  - LaneSettings：合批窗口和上限（合并队列开着时是它的各项设置），在设置页
 * "没找到，按默认"和"查不到"分开写，查不到时写明原因。 */
import { api, type RLane, type RLaneFact, type RLaneMember, type RLaneUnit } from "@/api"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { toast } from "@/store"
import { errMsg } from "@/github/index"
import { Disclosure, Fact, Meta, Panel, Pill, Row, Section } from "@/ui"
import { useEffect, useState } from "react"
import { t, useT } from "@/i18n"
import { hm } from "@/i18n/format"
import { QueueNow, QueueSettings } from "@/merge-queue"

/* labels are functions so they follow a language change */
export const MERGE: Record<string, () => string> = { host_queue: () => t("shell:lane.merge.hostQueue"), host_pr: () => t("shell:lane.merge.hostPr"), push: () => t("shell:lane.merge.push") }
export const SHIP: Record<string, () => string> = { tag: () => t("shell:lane.ship.tag"), script: () => t("shell:lane.ship.script"), merge: () => t("shell:lane.ship.merge") }
const FROM: Record<string, () => string> = { required: () => t("shell:lane.from.required"), suggested: () => t("shell:lane.from.suggested") }
const EVENT: Record<string, () => string> = { pull_request: () => "PR", pull_request_target: () => "PR", merge_group: () => t("shell:lane.event.mergeGroup") }
const from = (c: { from: string; on?: string[] }) => c.from === "workflow" ? t("shell:lane.runsOn", { events: [...new Set((c.on ?? []).map((e) => EVENT[e]?.() ?? e))].join(t("shell:list.sep")) }) : FROM[c.from]?.()

export type LaneLoad = { s: "loading" } | { s: "error"; msg: string } | { s: "ok"; v: RLane }
/** the lane of a git project (`on` false: not a git project, nothing is read) */
export function useLane(project: string, on: boolean) {
  const [l, setL] = useState<LaneLoad>({ s: "loading" })
  useEffect(() => {
    if (!on) return
    let live = true
    setL({ s: "loading" })
    api.lane(project).then((v) => live && setL({ s: "ok", v }), (e) => live && setL({ s: "error", msg: errMsg(e) }))
    return () => { live = false }
  }, [project, on])
  return [l, (v: RLane) => setL({ s: "ok", v })] as const
}

/** a fact Steve fell back on or could not read: a small tag beside its value */
export function How({ f }: { f: RLaneFact<unknown> }) {
  const t = useT()
  if (f.how.state === "unknown") return <Pill tone="attention">{t("shell:lane.unknown")}</Pill>
  if (f.how.state === "default") return <Pill>{t("shell:lane.default")}</Pill>
  return null
}
/** why a fact could not be read, under its value */
export function Unknown({ f }: { f: RLaneFact<unknown> }) {
  return f.how.state === "unknown" ? <span className="mt-0.5 block type-footnote text-attention">{f.how.why}</span> : null
}

const names = (ms: RLaneMember[]) => ms.map((m) => m.label).join(t("shell:list.sep"))
const clock = (ms: number) => hm(ms)
const unitState = (u: RLaneUnit) => u.merged ? t("shell:lane.batch.releasing") : u.approved ? t("shell:lane.batch.merging") : t("shell:lane.batch.waiting")
const carried = (u: RLaneUnit) => [names(u.members), u.deferred.length ? t("shell:lane.batch.carries", { names: names(u.deferred) }) : ""].filter(Boolean).join(" · ")

/** what is being merged now: the unit in flight and the batch collecting (with "merge and release now"); nothing when idle */
export function LaneNow({ project, lane, onLane }: { project: string; lane: RLane; onLane: (l: RLane) => void }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  if (lane.queue) return <QueueNow project={project} queue={lane.queue} onQueue={(q) => onLane({ ...lane, queue: q })} />
  const r = lane.lane?.running, c = lane.lane?.collecting
  if (!r && !c) return null
  const now = async () => {
    setBusy(true)
    try { const x = await api.laneNow(project); onLane({ ...lane, lane: x.lane }); toast(x.closed ? t("shell:lane.batch.nowDone") : t("shell:lane.batch.nowNothing")) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Panel data-lane-batches>
      {r && <Row title={t("shell:lane.batch.unit", { no: r.no, state: unitState(r) })} sub={carried(r)} data-lane-running />}
      {c && <Row title={c.closed ? t("shell:lane.batch.closed", { no: c.no }) : t("shell:lane.batch.collecting", { no: c.no, time: clock(c.closes_at) })} sub={carried(c)} data-lane-collecting>
        {!c.closed && <Button size="sm" variant="outline" disabled={busy} onClick={() => void now()} data-lane-now>{t("shell:lane.batch.now")}</Button>}
      </Row>}
    </Panel>
  )
}

/** what Steve found in the repository, folded: the checks, the files every change collides on, and where each fact came from */
export function LaneConventions({ lane }: { lane: RLane }) {
  const t = useT()
  const checks = lane.checks.value
  const sources: [string, string, string[]][] = [
    ["merge", t("shell:lane.conv.merge"), lane.merge.evidence],
    ["ship", t("shell:lane.conv.ship"), lane.ship.evidence],
    ["reads", t("shell:lane.conv.reads"), [lane.reads]],
  ]
  return (
    <Disclosure data-lane-conventions className="px-1"
      label={<Meta inline one className="type-callout text-inherit"><span>{t("shell:lane.conv.title")}</span><span>{t("shell:lane.conv.checks", { count: checks.length })}</span><span>{t("shell:lane.conv.files", { count: lane.numbered.length })}</span></Meta>}>
      <div className="-mx-1 flex flex-col gap-6 pt-4">
        <Section label={t("shell:lane.conv.checksLabel")} right={<How f={lane.checks} />} note={t("shell:lane.checksNote")}>
          <Panel data-lane-checks>
            {lane.checks.how.state === "unknown" && <Row title={t("shell:lane.unknown")} sub={lane.checks.how.why} />}
            {checks.map((c) => <Row key={c.from + c.name} className="min-h-10 py-2" title={<span className="font-mono text-[13px] break-all">{c.name}</span>} data-lane-check={c.from}><span className="type-footnote whitespace-nowrap text-muted-foreground">{from(c)}</span></Row>)}
            {!checks.length && lane.checks.how.state !== "unknown" && <Row title={t("shell:lane.conv.noChecks")} />}
          </Panel>
        </Section>
        {lane.numbered.length > 0 && <Section label={t("shell:lane.conv.filesLabel")}>
          <Panel>{lane.numbered.map((n) => <Row key={n.kind + n.path} data-lane-numbered={n.kind}
            title={<span className="font-mono text-[13px] break-all">{n.path}</span>} sub={<Meta><span>{n.evidence}</span><span className="whitespace-normal">{n.handling}</span></Meta>} />)}</Panel>
        </Section>}
        <Section label={t("shell:lane.conv.how")}>
          <Panel>{sources.filter(([, , xs]) => xs.length).map(([k, label, xs]) => (
            <Fact key={k} label={label} testId={"lane-" + k}><span className="flex flex-col gap-0.5 type-callout text-muted-foreground">{xs.map((x, i) => <span key={i} className="break-words">{x}</span>)}</span></Fact>))}</Panel>
        </Section>
      </div>
    </Disclosure>
  )
}

/** your limits on the lane (window and size of a batch), or the merge queue's settings while it is on */
export function LaneSettings({ project, lane, onLane }: { project: string; lane: RLane; onLane: (l: RLane) => void }) {
  const t = useT()
  const l = lane.lane
  const minutes = (ms: number | null | undefined) => (ms == null ? "" : String(Math.round(ms / 60_000)))
  const [win, setWin] = useState(minutes(l?.set.window_ms)), [max, setMax] = useState(l?.set.max_members == null ? "" : String(l.set.max_members)), [busy, setBusy] = useState(false)
  useEffect(() => { setWin(minutes(l?.set.window_ms)); setMax(l?.set.max_members == null ? "" : String(l.set.max_members)) }, [l?.set.window_ms, l?.set.max_members])
  if (lane.queue) return <QueueSettings project={project} queue={lane.queue} onQueue={(q) => onLane({ ...lane, queue: q })} />
  const save = async () => {
    const num = (v: string) => (v.trim() === "" ? null : Number(v))
    setBusy(true)
    try { const r = await api.lanePolicy(project, { window_min: num(win), max_members: num(max) }); onLane({ ...lane, lane: r.lane }); toast(t("common:state.saved")) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const d = l?.defaults ?? { window_ms: 2 * 3_600_000, max_members: 10 }
  return (
    <Panel>
      <Row className="max-sm:flex-wrap max-sm:[&>[data-slot=item-actions]]:basis-full max-sm:[&>[data-slot=item-actions]]:justify-end" title={t("shell:lane.batch.limits")} sub={t("shell:lane.batch.limitsSub", { minutes: Math.round(d.window_ms / 60_000), count: d.max_members })} data-lane-limits>
        <span className="flex items-center gap-1.5">
          <Input className="h-8 w-16 text-right tabular-nums" inputMode="numeric" value={win} placeholder={String(Math.round(d.window_ms / 60_000))} onChange={(e) => setWin(e.target.value)} aria-label={t("shell:lane.batch.window")} data-lane-window />
          <span className="text-muted-foreground type-footnote">{t("shell:lane.batch.minutes")}</span>
          <Input className="h-8 w-14 text-right tabular-nums" inputMode="numeric" value={max} placeholder={String(d.max_members)} onChange={(e) => setMax(e.target.value)} aria-label={t("shell:lane.batch.max")} data-lane-max />
          <span className="text-muted-foreground type-footnote">{t("shell:lane.batch.items")}</span>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => void save()} data-lane-save>{t("common:action.save")}</Button>
        </span>
      </Row>
    </Panel>
  )
}
