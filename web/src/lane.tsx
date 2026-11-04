/* 项目页的「合并通道」：Steve 认出的这个仓库怎么合并、合并前要过什么、怎么发布、哪些文件每次都会撞，以及近 7 天的节奏。只读，每一项都写明依据；"没找到"和"查不到"分开写 */
import { api, type RLane, type RLaneFact } from "@/api"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { errMsg } from "@/github/index"
import { Group, Meta, Row, SectionLabel } from "@/ui"
import { TriangleAlertIcon } from "lucide-react"
import { type ReactNode, useEffect, useState } from "react"

const MERGE: Record<string, string> = { host_queue: "托管方的合并队列", host_pr: "经 PR 合并", push: "直接推送到分支" }
const SHIP: Record<string, string> = { tag: "推送版本 tag", script: "运行项目的发布命令", merge: "合并即完成" }
const FROM: Record<string, string> = { required: "必须通过", workflow: "PR 时运行", suggested: "仓库里的命令" }

function How({ f }: { f: RLaneFact<unknown> }) {
  if (f.how.state === "unknown") return <Badge variant="outline" className="font-normal text-attention">查不到</Badge>
  if (f.how.state === "default") return <Badge variant="outline" className="font-normal text-muted-foreground">没找到，按默认</Badge>
  return null
}

function Why({ f, extra }: { f: RLaneFact<unknown>; extra?: ReactNode }) {
  const lines = [...(f.how.state === "unknown" ? [f.how.why] : []), ...f.evidence]
  return <span className="flex flex-col gap-0.5">{extra}{lines.map((l, i) => <span key={i}>{l}</span>)}</span>
}

export function LaneSection({ project }: { project: string }) {
  const [lane, setLane] = useState<RLane | null>(null), [err, setErr] = useState<string | null>(null)
  useEffect(() => { setLane(null); setErr(null); api.lane(project).then(setLane, (e) => setErr(errMsg(e))) }, [project])
  return (
    <section data-lane>
      <SectionLabel>合并通道</SectionLabel>
      {err ? <Group><Row title="认不出这个仓库的合并方式" sub={err} /></Group>
        : !lane ? <Group><Row title={<span className="inline-flex items-center gap-2 text-muted-foreground"><Spinner />正在读取仓库和托管方…</span>} /></Group>
        : <Group>
          <Row title={<span className="font-mono text-[13px]">{lane.target.upstream}#{lane.target.branch}</span>} data-lane-target
            sub={<span className="flex flex-col gap-0.5"><span>读的是 {lane.reads}</span>{lane.fetch_error && <span className="flex items-start gap-1 text-attention"><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" /><span className="break-all">没取到最新状态，以上是上次同步的结果：{lane.fetch_error}</span></span>}</span>} />
          <Row title={<span className="inline-flex items-center gap-2">合并方式 · {MERGE[lane.merge.value]}<How f={lane.merge} /></span>} sub={<Why f={lane.merge} />} data-lane-merge />
          <Row title={<span className="inline-flex items-center gap-2">合并前要过 · {lane.checks.value.filter((c) => c.from !== "suggested").length || (lane.checks.how.state === "unknown" ? "不确定" : "没有")}<How f={lane.checks} /></span>} data-lane-checks
            sub={<Why f={lane.checks} extra={lane.checks.value.length > 0 && <span className="flex flex-wrap gap-1 py-0.5">{lane.checks.value.map((c) => <Badge key={c.from + c.name} variant="outline" className="h-auto max-w-full justify-start whitespace-normal text-left font-normal text-muted-foreground"><span className="font-mono break-all">{c.name}</span><span className="shrink-0">· {FROM[c.from]}</span></Badge>)}</span>} />} />
          <Row title={<span className="inline-flex items-center gap-2">发布方式 · {SHIP[lane.ship.value]}<How f={lane.ship} /></span>} sub={<Why f={lane.ship} />} data-lane-ship />
          {lane.numbered.map((n) => <Row key={n.kind + n.path} title={<span><span className="font-mono text-[13px]">{n.path}</span> · {n.evidence}</span>} sub={n.handling} data-lane-numbered={n.kind} />)}
          <Row title={`近 ${lane.pace.days} 天`} sub={<Meta><span>合入 {lane.pace.landed} 次</span><span>发布 {lane.pace.releases} 个版本</span><span>{lane.pace.authors} 位作者</span></Meta>} data-lane-pace />
        </Group>}
    </section>
  )
}
