/* 信号: external events and what the flows did with them (signals-automation.md §9, prototype SignalsView) */
import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowRightIcon, ChevronRightIcon, FileIcon, ListTreeIcon, SearchIcon, WorkflowIcon } from "lucide-react"
import { Input } from "@/components/ui/input"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Page, Group, Segmented, Chips, StatusDot, Empty, go } from "@/ui"
import { st, tm, toast, botName } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, SOURCE_LABEL, type Signal, type SignalDetail } from "@/api/flow"
import { HubError } from "@/api"
import { srcIcon, Json, Failed, ms, RUN_STATUS } from "@/auto/common"

type Mode = "按结果" | "按来源"
type Range = "今天" | "7 天"
const OUTCOME_GROUPS: [string, string, string[]][] = [
  ["need", "需要你", ["need"]], ["failed", "处理失败", ["failed"]], ["item", "建了事项", ["item", "linked"]], ["digest", "进汇报", ["digest"]],
  ["ignored", "已忽略", ["ignored"]], ["recorded", "只记录", ["recorded"]], ["pending", "处理中", ["pending"]],
]
const dotOf = (o: string) => (o === "need" ? "wait" : o === "failed" ? "fail" : o === "item" || o === "linked" ? "prop" : o === "pending" ? "run" : "idle")
/* page state survives leaving and coming back, like the prototype's module-level filters */
let mode: Mode = "按结果", range: Range = "今天", query = ""
const openGroups = new Set<string>(["need", "failed"])

/** `#/signals?e=<id>`: the signal to open and scroll to */
const focusOf = () => new URLSearchParams(location.hash.split("?")[1] ?? "").get("e")

export function SignalsView() {
  const [, force] = useState(0), r = () => force((x) => x + 1)
  const focus = focusOf()
  if (focus && range === "今天") range = "7 天"
  const [q, setQ] = useState(query)
  useEffect(() => { const t = setTimeout(() => { query = q; r() }, 300); return () => clearTimeout(t) }, [q])
  const L = useLoad(() => flowApi.signals({ range: range === "今天" ? "today" : "7d", q: query }), [range, query], ["signal", "run"])
  const [row, setRow] = useState<string | null>(null)
  const groups = useMemo(() => {
    const xs = L.data?.items ?? []
    const gs: [string, string, Signal[]][] = mode === "按结果"
      ? OUTCOME_GROUPS.map(([k, l, os]) => [k, l, xs.filter((s) => os.includes(s.outcome))])
      : [...new Set(xs.map((s) => s.source))].sort((a, b) => Object.keys(SOURCE_LABEL).indexOf(a) - Object.keys(SOURCE_LABEL).indexOf(b)).map((s) => [s, sourceLabel(s), xs.filter((x) => x.source === s)])
    return gs.filter((g) => g[2].length)
  }, [L.data, mode])
  /* open the linked signal once it is loaded */
  const done = useRef<string | null>(null)
  useEffect(() => {
    if (!focus || !L.data || done.current === focus) return
    const s = L.data.items.find((x) => x.id === focus); if (!s) return
    done.current = focus
    const g = OUTCOME_GROUPS.find((x) => x[2].includes(s.outcome))?.[0]
    if (mode === "按结果" && g) openGroups.add(g)
    setRow(rowKey(s)); r()
    setTimeout(() => document.querySelector(`[data-sig-id="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }), 250)
  }, [focus, L.data])

  if (L.unwired) return <Unwired id="signals" />
  if (L.error && !L.data) return <Failed title="信号" msg={errText(L.error)} retry={() => void L.reload()} />
  const stats = L.data?.stats
  return (
    <Page title="信号" sub="邮件、IM、日程等外部信息，以及 Bot 怎么处理的" max={760}
      toolbar={<Segmented value={mode} onChange={(v) => { mode = v; r() }} options={["按结果", "按来源"] as const} />}>
      <div className="mb-6 flex flex-wrap items-center gap-2">
        <label className="relative min-w-0 flex-1"><span className="sr-only">搜索信号</span><SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-tertiary" aria-hidden />
          <Input className="h-9 rounded-xl bg-muted/70 pl-9 text-[14px]" placeholder="搜索信号内容、来源或处理结果" value={q} onChange={(e) => setQ(e.target.value)} data-sig-search /></label>
        <Chips value={range} onChange={(v) => { range = v; r() }} options={["今天", "7 天"] as const} />
      </div>
      <div className="mb-8 grid grid-cols-2 gap-y-4 sm:grid-cols-4" data-sig-stats>
        {([[range === "今天" ? "今天收到" : "7 天收到", stats?.total], ["需要你", stats?.need], ["建了事项", stats?.item], ["忽略或进汇报", stats?.quiet]] as const).map(([l, n]) => (
          <div key={l}><div className="text-[24px] font-semibold leading-7 tabular-nums tracking-tight">{n ?? "–"}</div><div className="mt-0.5 type-footnote text-muted-foreground">{l}</div></div>
        ))}
      </div>
      {L.data && !L.data.items.length && <Empty title={query ? "没有匹配的信号" : range === "今天" ? "今天还没有信号" : "7 天里没有信号"} sub={query ? undefined : "接入邮件、日程等账号后，收到的事件和处理结果会出现在这里。"} />}
      <div className="flex flex-col gap-6">
        {groups.map(([gk, gl, xs]) => {
          const open = mode === "按来源" || openGroups.has(gk)
          const byG = groupRows(xs)
          return (
            <Collapsible key={gk} open={open} onOpenChange={(o) => { o ? openGroups.add(gk) : openGroups.delete(gk); r() }}>
              <CollapsibleTrigger className="group/sg mb-2 flex w-full items-center gap-1.5 px-1 text-left" data-sig-group={gk}>
                <ChevronRightIcon className="size-3.5 text-muted-foreground transition-transform duration-200 group-aria-expanded/sg:rotate-90" aria-hidden />
                <span className="type-label text-foreground">{gl}</span><span className="type-footnote text-muted-foreground">{xs.length} 条 · {byG.length} 组</span>
              </CollapsibleTrigger>
              <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden transition-[height] duration-200 data-ending-style:h-0 data-starting-style:h-0">
                <Group>{byG.map((g) => <SignalRow key={rowKey(g[0])} g={g} open={row === rowKey(g[0])} onOpen={(o) => setRow(o ? rowKey(g[0]) : null)} reload={() => void L.reload()} />)}</Group>
              </CollapsibleContent>
            </Collapsible>
          )
        })}
      </div>
    </Page>
  )
}

/* one row per (group, outcome): a batch of 30 alerts of the same kind reads as one line */
const rowKey = (s: Signal) => (s.group_key ? "g:" + s.group_key : "e:" + s.id) + "|" + s.outcome
function groupRows(xs: Signal[]) {
  const m = new Map<string, Signal[]>()
  for (const s of xs) { const k = rowKey(s); m.set(k, [...(m.get(k) ?? []), s]) }
  return [...m.values()]
}

function SignalRow({ g, open, onOpen, reload }: { g: Signal[]; open: boolean; onOpen: (o: boolean) => void; reload: () => void }) {
  const s0 = g[0], many = g.length > 1, Ic = srcIcon(s0.source)
  return (
    <Collapsible open={open} onOpenChange={onOpen}>
      <CollapsibleTrigger className="flex w-full items-center gap-3 rounded-lg px-2 py-2.5 text-left transition-colors hover:bg-accent" data-sig-row={s0.group_key ?? s0.id} data-sig-id={s0.id}>
        <Ic className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-label={sourceLabel(s0.source)} />
        <span className="min-w-0 flex-1 truncate type-body">{many ? s0.group_label || s0.group_key : s0.summary}{many && <span className="ml-1.5 type-footnote tabular-nums text-muted-foreground">{g.length} 条</span>}</span>
        <span className="hidden items-center gap-1.5 type-footnote text-muted-foreground sm:inline-flex"><StatusDot s={dotOf(s0.outcome)} />{s0.outcome_label}</span>
        <span className="w-16 shrink-0 text-right type-footnote tabular-nums text-muted-foreground">{tm(s0.received)}</span>
      </CollapsibleTrigger>
      <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden transition-[height] duration-200 data-ending-style:h-0 data-starting-style:h-0" data-sig-detail={s0.id}>
        {open && <SignalDetailBox g={g} reload={reload} />}
      </CollapsibleContent>
    </Collapsible>
  )
}

const keepDays = () => st.settings.keepRaw || "7"
function SignalDetailBox({ g, reload }: { g: Signal[]; reload: () => void }) {
  const s0 = g[0], many = g.length > 1
  const [raw, setRaw] = useState<{ v?: unknown; gone?: boolean; err?: string } | null>(null)
  const [runs, setRuns] = useState<SignalDetail["runs"] | null>(null), [showRuns, setShowRuns] = useState(false)
  const [busy, setBusy] = useState(false)
  const toggleRaw = async () => {
    if (raw) { setRaw(null); return }
    if (!s0.raw_available) { setRaw({ gone: true }); return }
    try { setRaw({ v: (await flowApi.raw(s0.id)).payload }) } catch (e) { setRaw(e instanceof HubError && e.status === 410 ? { gone: true } : { err: errText(e) }) }
  }
  const toggleRuns = async () => {
    if (showRuns) { setShowRuns(false); return }
    setShowRuns(true)
    try { const ds = await Promise.all(g.slice(0, 10).map((x) => flowApi.signal(x.id))); setRuns(ds.flatMap((d) => d.runs)) } catch (e) { toast(errText(e)); setShowRuns(false) }
  }
  const fix = async (action: "remind" | "ignore" | "to_item") => {
    setBusy(true)
    try {
      const res = await flowApi.correct(s0.id, action, "group")
      const cs = res.changeset_id
      const msg = action === "to_item" ? `已建事项${res.item_id ? " " + res.item_id : ""}` : action === "remind" ? "已改进流程：以后这类提醒你" : "已改进流程：以后这类忽略"
      toast(msg, cs ? { action: { label: "撤销", onClick: () => void flowApi.undoConfig(cs).then(() => { toast("已撤销"); reload() }, (e) => toast(errText(e))) } } : res.item_id ? { action: { label: "查看", onClick: () => go("#/items/" + res.item_id) } } : undefined)
      reload()
    } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const who = s0.handled_by ? botName(s0.handled_by) : ""
  return (
    <div className="flex flex-col gap-2.5 px-2 pb-4 pl-9 type-callout text-muted-foreground">
      {many && <div className="max-h-48 overflow-y-auto border-l pl-3">{g.map((x) => <div key={x.id} className="py-0.5 text-foreground">{x.summary} <span className="text-muted-foreground">{tm(x.received)}</span></div>)}</div>}
      {!many && (s0.actor || s0.connection) && <div className="type-footnote">{[s0.actor?.name, s0.connection?.name, sourceLabel(s0.source)].filter(Boolean).join(" · ")}</div>}
      {s0.reason && <div>{who && <span className="text-foreground">{who}</span>}{who && "："}{s0.reason}</div>}
      <div>
        <button type="button" className="inline-flex items-center gap-1 type-footnote hover:text-foreground" onClick={() => void toggleRaw()} aria-expanded={!!raw} data-raw={s0.id}><FileIcon className="size-3.5" aria-hidden />{raw ? "收起原文" : "查看原文"}<span className="text-tertiary">· 保留 {keepDays()} 天</span></button>
        {raw && (raw.gone ? <div className="mt-1.5 type-footnote" data-raw-gone>原文已过保留期限，只剩处理结果和摘要</div> : raw.err ? <div className="mt-1.5 type-footnote text-destructive">{raw.err}</div> : <Json v={raw.v} className="mt-1.5 max-h-72 text-[12px]" />)}
      </div>
      {s0.flow && <a href={s0.flow.kind === "schedule" ? "#/auto/schedules" : "#/auto/rule/" + s0.flow.id} className="inline-flex items-center gap-1 hover:text-foreground" data-sig-flow><WorkflowIcon className="size-3.5" aria-hidden />由{s0.flow.kind === "schedule" ? "定时任务" : "流程"}「{s0.flow.name}」处理</a>}
      {s0.work_item_id && <a href={"#/items/" + s0.work_item_id} className="inline-flex items-center gap-1 text-blue hover:underline">{s0.work_item_id} {st.items.find((i) => i.id === s0.work_item_id)?.t ?? ""}<ArrowRightIcon className="size-3" aria-hidden /></a>}
      {s0.flow && <div>
        <button type="button" className="inline-flex items-center gap-1 type-footnote hover:text-foreground" onClick={() => void toggleRuns()} aria-expanded={showRuns} data-sig-runs><ListTreeIcon className="size-3.5" aria-hidden />{showRuns ? "收起处理过程" : "处理过程"}</button>
        {showRuns && (runs ? <RunSteps runs={runs} /> : <div className="mt-1.5 type-footnote">载入中…</div>)}
      </div>}
      <div className="flex flex-wrap items-center gap-1.5 pt-1"><span className="mr-1 type-footnote">处理得不对？会改进对应流程{many ? "，对这一组生效" : ""}</span>
        <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("remind")} data-fix="remind">以后这类提醒我</Button>
        <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("ignore")} data-fix="ignore">以后这类忽略</Button>
        {!s0.work_item_id && <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("to_item")} data-fix="to_item">转成事项</Button>}
      </div>
    </div>
  )
}

/** the runs that handled a signal, each step with its branch and output */
export function RunSteps({ runs }: { runs: SignalDetail["runs"] }) {
  if (!runs.length) return <div className="mt-1.5 type-footnote">没有运行记录</div>
  return (
    <div className="mt-1.5 flex flex-col gap-3" data-run-steps>
      {runs.map((run) => (
        <div key={run.id} className="rounded-lg border px-3 py-2">
          <div className="mb-1 flex items-center gap-1.5 type-footnote"><StatusDot s={RUN_STATUS[run.status]?.[1] ?? "idle"} /><span className="text-foreground">{RUN_STATUS[run.status]?.[0] ?? run.status}</span><span>· v{run.flow_version}</span>{run.outcome_label && <span>· {run.outcome_label}</span>}<a className="ml-auto hover:text-foreground" href={"#/auto/rule/" + run.flow_id + "?run=" + run.id}>在流程里查看</a></div>
          <ol className="flex flex-col gap-0.5">{(run.steps ?? []).map((s) => (
            <li key={s.seq} className="flex items-baseline gap-2 type-footnote"><span className="w-4 shrink-0 text-right font-mono tabular-nums text-tertiary">{s.seq}</span><span className="font-mono text-foreground">{s.node_id}</span>
              <span className="min-w-0 flex-1 truncate">{s.error ? <span className="text-destructive">{s.error}</span> : s.branch ? "→ " + s.branch : s.status === "skipped" ? "跳过" : s.status === "waiting" ? "等待中" : ""}</span><span className="shrink-0 tabular-nums">{ms(s.ms)}</span></li>))}</ol>
        </div>
      ))}
    </div>
  )
}
