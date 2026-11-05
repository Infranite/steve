/* 项目页的「合并通道」：Steve 认出的这个仓库怎么合并、合并前要过什么、怎么发布、哪些文件每次都会撞，以及近 7 天的节奏。只读，每一项都写明依据；"没找到"和"查不到"分开写 */
import { api, type RLane, type RLaneFact } from "@/api"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { errMsg } from "@/github/index"
import { Group, Meta, Row, SectionLabel } from "@/ui"
import { TriangleAlertIcon } from "lucide-react"
import { type ReactNode, useEffect, useState } from "react"
import { t, useT } from "@/i18n"

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

export function LaneSection({ project }: { project: string }) {
  const [lane, setLane] = useState<RLane | null>(null), [err, setErr] = useState<string | null>(null), t = useT()
  useEffect(() => { setLane(null); setErr(null); api.lane(project).then(setLane, (e) => setErr(errMsg(e))) }, [project])
  return (
    <section data-lane>
      <SectionLabel>{t("shell:lane.title")}</SectionLabel>
      {err ? <Group><Row title={t("shell:lane.unrecognized")} sub={err} /></Group>
        : !lane ? <Group><Row title={<span className="inline-flex items-center gap-2 text-muted-foreground"><Spinner />{t("shell:lane.loading")}</span>} /></Group>
        : <Group>
          <Row title={<span className="font-mono text-[13px]">{lane.target.upstream}#{lane.target.branch}</span>} data-lane-target
            sub={<span className="flex flex-col gap-0.5"><span>{t("shell:lane.reads", { reads: lane.reads })}</span>{lane.fetch_error && <span className="flex items-start gap-1 text-attention"><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" /><span className="break-all">{t("shell:lane.fetchError", { error: lane.fetch_error })}</span></span>}</span>} />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.mergeTitle", { value: MERGE[lane.merge.value]?.() ?? lane.merge.value })}<How f={lane.merge} /></span>} sub={<Why f={lane.merge} />} data-lane-merge />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.checksTitle", { value: lane.checks.value.filter((c) => c.from !== "suggested").length || (lane.checks.how.state === "unknown" ? t("shell:lane.unsure") : t("shell:lane.none")) })}<How f={lane.checks} /></span>} data-lane-checks
            sub={<Why f={lane.checks} extra={lane.checks.value.length > 0 && <span className="flex flex-wrap gap-1 py-0.5">{lane.checks.value.map((c) => <Badge key={c.from + c.name} variant="outline" className="h-auto max-w-full justify-start whitespace-normal text-left font-normal text-muted-foreground"><span className="font-mono break-all">{c.name}</span><span className="shrink-0">· {from(c)}</span></Badge>)}</span>} />} />
          <Row title={<span className="inline-flex items-center gap-2">{t("shell:lane.shipTitle", { value: SHIP[lane.ship.value]?.() ?? lane.ship.value })}<How f={lane.ship} /></span>} sub={<Why f={lane.ship} />} data-lane-ship />
          {lane.numbered.map((n) => <Row key={n.kind + n.path} title={<span><span className="font-mono text-[13px]">{n.path}</span> · {n.evidence}</span>} sub={n.handling} data-lane-numbered={n.kind} />)}
          <Row title={t("shell:lane.days", { count: lane.pace.days })} sub={<Meta><span>{t("shell:lane.landed", { count: lane.pace.landed })}</span><span>{t("shell:lane.releases", { count: lane.pace.releases })}</span><span>{t("shell:lane.authors", { count: lane.pace.authors })}</span></Meta>} data-lane-pace />
        </Group>}
    </section>
  )
}
