/* #/auto — 自动化: the chain (accounts → flows → signals → schedules), the flow list, schedules and event sources
 * (prototype AutomationView). Routes: #/auto/rules, #/auto/schedules, #/auto/sources, #/auto/rule/:id, #/auto/source/:adapter */
import { useState } from "react"
import { ArrowRightIcon, ChevronRightIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Page, Group, Row, Segmented, Meta, Empty, StatusDot, go } from "@/ui"
import { toast } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, type FlowSummary, type Load } from "@/api/flow"
import { Ico, srcIcon, Failed } from "@/auto/common"
import { NewRuleDialog } from "@/auto/new-rule"
import { Schedules, NewScheduleDialog } from "@/auto/schedules"
import { RuleView } from "@/auto/rule"
import { SourceView } from "@/auto/source"

type Tab = "rules" | "schedules" | "sources"

/** the whole #/auto/* route (App.tsx renders this for "auto") */
export function AutoRoute({ b, c }: { b?: string; c?: string }) {
  const sub = (b ?? "").split("?")[0], id = (c ?? "").split("?")[0]
  if (sub === "rule" && id) return <RuleView key={id} id={id} />
  if (sub === "source" && id) return <SourceView key={id} adapter={id} />
  return <AutomationView tab={(["rules", "schedules", "sources"].includes(sub) ? sub : "rules") as Tab} />
}

export function AutomationView({ tab }: { tab: Tab }) {
  const flows = useLoad(() => flowApi.flows("flow"), [], ["flow"])
  const sch = useLoad(() => flowApi.schedules(), [], ["flow", "run"])
  const sig = useLoad(() => flowApi.signals({ range: "today" }).then((x) => x.stats.total), [], ["signal"])
  const conns = useLoad(() => flowApi.connections().then((x) => x.length).catch(() => null), [])
  const [dialog, setDialog] = useState<null | "rule" | "schedule">(null)
  if (flows.unwired) return <Unwired id="auto" />
  const n = (v: number | null | undefined) => (v === undefined || v === null ? "–" : String(v))
  const chain: [string, string, string][] = [
    [`${n(conns.data)} 个已接入的账号`, "邮件 · 日程 · IM", "#/accounts"],
    [`${n(flows.data?.filter((f) => f.enabled).length)} 条流程`, "条件 · 决策 · 脚本 · AI", "#/auto/rules"],
    [`今天 ${n(sig.data)} 条信号`, "处理记录在信号页", "#/signals"],
    [`${n(sch.data?.filter((s) => s.enabled).length)} 个定时任务`, "也是一种事件源", "#/auto/schedules"],
  ]
  return (
    <Page title="自动化" sub="事件从哪里来，交给谁处理，什么时候定时做" max={760}
      actions={tab === "rules" ? <Button size="sm" onClick={() => setDialog("rule")} data-new-rule><PlusIcon />新建流程</Button> : tab === "schedules" ? <Button size="sm" onClick={() => setDialog("schedule")} data-new-schedule><PlusIcon />新建定时任务</Button> : undefined}
      toolbar={<Segmented value={tab} onChange={(v) => go("#/auto/" + v)} options={[["rules", "流程"], ["schedules", "定时任务"], ["sources", "事件源"]] as const} />}>
      {dialog === "rule" && <NewRuleDialog onClose={() => setDialog(null)} />}
      {dialog === "schedule" && <NewScheduleDialog onClose={() => setDialog(null)} onSaved={() => void sch.reload()} />}
      <nav aria-label="自动化链条" className="mb-8 grid grid-cols-2 overflow-hidden rounded-xl border bg-card sm:flex sm:items-stretch" data-auto-chain>
        {chain.map(([a, b, h], i) => (
          <a key={h} href={h} className="relative flex min-w-0 flex-1 flex-col px-4 py-3 text-left transition-colors hover:bg-accent max-sm:border-border max-sm:[&:nth-child(even)]:border-l max-sm:[&:nth-child(-n+2)]:border-b">
            {i > 0 && <ArrowRightIcon className="absolute -left-2 top-1/2 hidden size-4 -translate-y-1/2 rounded-full bg-card text-tertiary sm:block" aria-hidden />}
            <span className="truncate type-headline">{a}</span><span className="truncate type-footnote text-muted-foreground">{b}</span>
          </a>
        ))}
      </nav>
      {tab === "rules" && <FlowList L={flows} onNew={() => setDialog("rule")} />}
      {tab === "schedules" && <Schedules L={sch} onNew={() => setDialog("schedule")} />}
      {tab === "sources" && <SourceList />}
    </Page>
  )
}

function FlowList({ L, onNew }: { L: Load<FlowSummary[]>; onNew: () => void }) {
  const [busy, setBusy] = useState<string | null>(null)
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  if (!L.data.length) return <><Empty title="还没有流程" sub="流程决定收到某类事件时怎么处理：过滤、查表、交给 Bot 判断、等你批准、执行动作。" /><div className="flex justify-center"><Button variant="outline" onClick={onNew}><PlusIcon />新建流程</Button></div></>
  const toggle = async (f: FlowSummary, v: boolean) => {
    setBusy(f.id)
    try { await flowApi.patchFlow(f.id, { enabled: v }); toast(v ? "流程已启用" : "流程已停用"); await L.reload() } catch (e) { toast(errText(e)) } finally { setBusy(null) }
  }
  return (
    <Group>{L.data.map((f) => (
      <div key={f.id} className="flex items-center gap-3 px-2 py-3">
        <Ico I={srcIcon(f.trigger?.source)} />
        <button type="button" className="min-w-0 flex-1 text-left" onClick={() => go("#/auto/rule/" + f.id)} data-rule-row={f.id}>
          <div className="truncate type-body">{f.name}</div>
          <Meta className="mt-0.5"><span>{sourceLabel(f.trigger?.source) || "未设置触发"}{f.trigger?.types?.length ? " · " + f.trigger.types.join("、") : ""}</span><span>{f.uses_ai ? "含 AI 判断" : "不用 AI"}</span><span>{f.outcomes} 种结果</span><span>24 小时 {f.runs24} 次</span>{f.origin_label && <span>{f.origin_label}</span>}</Meta>
        </button>
        <Switch checked={f.enabled} disabled={busy === f.id} onCheckedChange={(v) => void toggle(f, v)} aria-label={"启用" + f.name} />
      </div>
    ))}</Group>
  )
}

function SourceList() {
  const L = useLoad(() => flowApi.sources(), [], ["signal"])
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  const xs = L.data.filter((s) => s.source !== "im")
  return (<>
    {xs.length ? <Group>{xs.map((s) => {
      const bad = s.connections.filter((c) => c.status !== "ok").length, on = s.types.filter((t) => t.on)
      return (
        <Row key={s.adapter} icon={<Ico I={srcIcon(s.source)} />} onClick={() => go("#/auto/source/" + s.adapter)} data-source-row={s.adapter} title={s.name}
          sub={`${s.paused ? "已暂停 · " : ""}${s.connections.length} 个账号 · ${on.length} 种事件 · 24 小时 ${on.reduce((n, t) => n + t.n24, 0)} 条`}>
          {bad ? <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s="wait" />{bad} 个账号要处理</span> : <span className="type-footnote text-muted-foreground">{s.connections.length ? "正常" : "没有账号"}</span>}
          <ChevronRightIcon className="size-4 text-tertiary" aria-hidden />
        </Row>) })}</Group>
      : <Empty title="还没有事件源" sub="接入邮件、日历、GitHub 这类账号后，它们就是事件源。" />}
    <div className="mt-2 px-1 type-footnote text-muted-foreground">外部服务在 <a href="#/integrations" className="text-blue hover:underline">接入</a> 里接入；能收也能发、带身份的群和私聊在 <a href="#/channels" className="text-blue hover:underline">渠道</a> 里管理；定时任务在“定时任务”里管理。</div>
  </>)
}
