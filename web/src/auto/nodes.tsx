/* Flow node kinds: labels, the palette, defaults, graph edits and the inspector (signals-automation.md §3.2,
 * prototype KIND / PALETTE / TableEditor / the RuleView aside) */
import { useState, type ReactNode } from "react"
import { ArrowRightIcon, BotIcon, CodeIcon, FilterIcon, GlobeIcon, HourglassIcon, PlusIcon, TableIcon, TerminalSquareIcon, TrashIcon, UserCheckIcon, WorkflowIcon, ZapIcon, type LucideIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList, CommandSeparator } from "@/components/ui/command"
import { SectionLabel, Segmented } from "@/ui"
import { SOURCE_LABEL, type FlowSummary, type GEdge, type GNode, type Graph, type Source, type Step } from "@/api/flow"
import { Pick, Json, botOptions, srcIcon, ms } from "@/auto/common"

/* [label, icon, group]; the group drives the palette and the inspector heading */
export const KIND: Record<string, [string, LucideIcon, string]> = {
  trigger: ["触发", ZapIcon, "触发"], filter: ["条件分支", FilterIcon, "逻辑"], table: ["决策表", TableIcon, "逻辑"],
  script: ["脚本", CodeIcon, "代码"], cmd: ["运行命令", TerminalSquareIcon, "代码"], http: ["HTTP 请求", GlobeIcon, "连接"], flow: ["调用流程", WorkflowIcon, "连接"],
  bot: ["Bot 判断", BotIcon, "AI"], wait: ["等待", HourglassIcon, "等待"], approve: ["等你审批", UserCheckIcon, "等待"], action: ["动作", ArrowRightIcon, "结果"],
}
export const kindOf = (k: string) => KIND[k] ?? [k, WorkflowIcon, "其他"]
export const ACTION_LABEL: Record<string, string> = { "item.create": "建事项", "item.link": "关联到已有事项", notify: "提醒你", digest: "进汇报", ignore: "忽略", reply_as: "回复", send_to_space: "发到群里" }
const EFFECT_ACTIONS = ["reply_as", "send_to_space"]
/** a step that reaches outside Steve: needs an approval (or a grant) on every run */
export const effectful = (n: GNode) => n.kind === "cmd" || (n.kind === "http" && !!n.config?.effect) || (n.kind === "action" && EFFECT_ACTIONS.includes(n.config?.action))

type PaletteItem = [kind: string, title: string, desc: string, config: Record<string, any>]
export const PALETTE: { g: string; items: PaletteItem[] }[] = [
  { g: "逻辑", items: [["filter", "条件分支", "按字段判断，走不同分支", { rules: [{ label: "是", when: "" }], else_label: "否" }], ["table", "决策表", "一张表写清楚条件和结果", { hit: "first", inputs: ["event.summary"], outputs: ["结果"], rows: [["", ""]] }]] },
  { g: "代码", items: [["script", "脚本", "JavaScript，沙箱里运行", { code: "export const handler = async (input) => {\n  return { ...input }\n}" }], ["cmd", "运行命令", "在项目目录执行 shell，需审批", { command: "./scripts/check.sh", project_id: "", timeout_s: 120 }]] },
  { g: "连接", items: [["http", "HTTP 请求", "调用任意 API", { method: "GET", url: "https://", headers: {}, body: "", effect: false }], ["flow", "调用流程", "复用另一条流程", { flow_id: "", input: {} }]] },
  { g: "AI", items: [["bot", "Bot 判断", "交给模型判断，给出分支", { instruction: "", branches: ["是", "否"] }]] },
  { g: "等待", items: [["wait", "等待", "延时、等事件或等到某个时间", { mode: "delay", delay: "30m" }], ["approve", "等你审批", "进待处理，你决定后继续", { question: "要继续吗？", options: ["继续", "停止"] }]] },
  { g: "结果", items: [["action", "建事项", "直接开始或先给方案", { action: "item.create", mode: "start" }], ["action", "关联到已有事项", "补充到同一往来或同一分支的事项", { action: "item.link", by: "thread", fallback: "create" }], ["action", "提醒你", "进待处理", { action: "notify", text: "{{event.summary}}" }], ["action", "进汇报", "离开期间汇报", { action: "digest", text: "{{event.summary}}" }], ["action", "忽略", "记录原因", { action: "ignore", reason: "" }]] },
]

/* ───────── graph edits (pure: each returns a new graph) ───────── */
const clone = (g: Graph): Graph => JSON.parse(JSON.stringify(g))
export function freshId(g: Graph, kind: string) {
  const base = ({ trigger: "t", filter: "f", table: "tb", script: "s", cmd: "c", http: "h", flow: "fl", bot: "b", wait: "w", approve: "ap", action: "a" } as Record<string, string>)[kind] ?? "n"
  for (let n = 1; ; n++) { const id = base + n; if (!g.nodes.some((x) => x.id === id)) return id }
}
export function addAfter(g0: Graph, from: string, kind: string, title: string, config: Record<string, any>, label = ""): [Graph, string] {
  const g = clone(g0), id = freshId(g, kind)
  g.nodes.push({ id, kind, title, note: "", config: JSON.parse(JSON.stringify(config)) })
  g.edges.push({ from, to: id, label })
  return [g, id]
}
/** remove a node; its parents take over its children (with the child edge's label) */
export function removeNode(g0: Graph, id: string): Graph {
  const g = clone(g0), ins = g.edges.filter((e) => e.to === id), outs = g.edges.filter((e) => e.from === id)
  g.nodes = g.nodes.filter((n) => n.id !== id); g.edges = g.edges.filter((e) => e.from !== id && e.to !== id)
  for (const i of ins) for (const o of outs) if (!g.edges.some((e) => e.from === i.from && e.to === o.to)) g.edges.push({ from: i.from, to: o.to, label: outs.length > 1 ? o.label : i.label })
  return g
}
export function patchNode(g0: Graph, id: string, f: (n: GNode) => void): Graph { const g = clone(g0); const n = g.nodes.find((x) => x.id === id); if (n) f(n); return g }
const unq = (s: string) => s.trim().replace(/^"(.*)"$/, "$1")
/** labels a node's outgoing edges are expected to carry */
export function branchLabels(n: GNode): string[] {
  const c = n.config ?? {}
  switch (n.kind) {
    case "filter": return [...(c.rules ?? []).map((r: any) => r.label), c.else_label].filter(Boolean)
    case "bot": return c.branches ?? []
    case "approve": return c.options ?? []
    case "table": return [...new Set<string>((c.rows ?? []).map((r: string[]) => unq(r[(c.inputs ?? []).length] ?? "")).filter(Boolean))]
    case "wait": return c.timeout_label ? ["", c.timeout_label] : []
    default: return []
  }
}
/** the nodes upstream of `id` (their outputs are `steps.<id>` there) */
export function upstream(g: Graph, id: string) {
  const seen = new Set<string>(), q = [id]
  while (q.length) { const c = q.pop()!; g.edges.filter((e) => e.to === c).forEach((e) => { if (!seen.has(e.from)) { seen.add(e.from); q.push(e.from) } }) }
  return g.nodes.filter((x) => seen.has(x.id) && x.kind !== "trigger")
}

/* ───────── text form: the graph as read-only YAML (prototype toYaml) ───────── */
const q = (s: unknown) => { const v = String(s ?? ""); return /[:#{}[\],&*?|<>=!%@`'"\n]/.test(v) || !v || /^\s|\s$/.test(v) ? JSON.stringify(v) : v }
function yamlOf(v: any, ind: string): string[] {
  if (v === null || typeof v !== "object") return [q(v)]
  if (Array.isArray(v)) return v.length ? v.flatMap((x) => { const l = yamlOf(x, ind + "  "); return typeof x === "object" && x !== null ? [`${ind}- ${l[0].trimStart()}`, ...l.slice(1)] : [`${ind}- ${l[0]}`] }) : ["[]"]
  const ks = Object.keys(v); if (!ks.length) return ["{}"]
  return ks.flatMap((k) => {
    const x = v[k]
    if (typeof x === "string" && x.includes("\n")) return [`${ind}${k}: |`, ...x.split("\n").map((l) => ind + "  " + l)]
    if (x !== null && typeof x === "object" && (Array.isArray(x) ? x.length : Object.keys(x).length)) return [`${ind}${k}:`, ...yamlOf(x, ind + "  ")]
    return [`${ind}${k}: ${yamlOf(x, ind)[0]}`]
  })
}
export function toYaml(name: string, version: number, g: Graph) {
  const out = [`name: ${q(name)}`, `version: ${version}`, "nodes:"]
  for (const n of g.nodes) {
    out.push(`  - id: ${n.id}`, `    kind: ${n.kind}`, `    title: ${q(n.title)}`)
    if (n.note) out.push(`    note: ${q(n.note)}`)
    if (n.config && Object.keys(n.config).length) out.push("    config:", ...yamlOf(n.config, "      "))
    if (n.on_fail) out.push(`    on_fail: ${q(n.on_fail)}`)
    const next = g.edges.filter((e) => e.from === n.id)
    if (next.length) out.push(`    next: [${next.map((e) => (e.label ? `{ when: ${q(e.label)}, to: ${e.to} }` : e.to)).join(", ")}]`)
  }
  return out.join("\n")
}

/* ───────── inspector ───────── */
const F = ({ label, children, hint }: { label: string; children: ReactNode; hint?: ReactNode }) => (
  <label className="flex flex-col gap-1"><span className="type-footnote text-muted-foreground">{label}</span>{children}{hint && <span className="type-footnote text-muted-foreground">{hint}</span>}</label>
)
const list = (s: string) => s.split(/[,，、]/).map((x) => x.trim()).filter(Boolean)

export function Inspector({ g, n, onChange, onSelect, flows, sources, step, flowId }: {
  g: Graph; n: GNode; onChange: (g: Graph, sel?: string) => void; onSelect: (id: string | null) => void; flows: FlowSummary[]; sources: Source[]; step?: Step; flowId: string
}) {
  const [palette, setPalette] = useState(false)
  const set = (f: (c: Record<string, any>, n: GNode) => void) => onChange(patchNode(g, n.id, (x) => { x.config ??= {}; f(x.config, x) }))
  const c = n.config ?? {}
  const [label, , group] = kindOf(n.kind)
  const outs = g.edges.filter((e) => e.from === n.id), labels = branchLabels(n)
  const prev = upstream(g, n.id)
  return (
    <div className="flex flex-col gap-4" data-inspector data-kind={n.kind}>
      <SectionLabel className="mb-0" right={<span className="font-normal">{group}</span>}>{label}{n.kind === "action" && c.action ? " · " + (ACTION_LABEL[c.action] ?? c.action) : ""}</SectionLabel>
      <div className="flex flex-col gap-3 px-1">
        <F label="名称"><Input className="rounded-lg" value={n.title} onChange={(e) => onChange(patchNode(g, n.id, (x) => { x.title = e.target.value }))} data-node-title /></F>
        <F label="说明（画布上显示）"><Input className="rounded-lg" value={n.note ?? ""} onChange={(e) => onChange(patchNode(g, n.id, (x) => { x.note = e.target.value }))} /></F>
        {n.kind === "trigger" && <TriggerEditor c={c} set={set} sources={sources} />}
        {n.kind === "filter" && <FilterEditor c={c} set={set} />}
        {n.kind === "table" && <TableEditor c={c} set={set} />}
        {n.kind === "script" && <F label="JavaScript · 输入是 input，返回值是这一步的输出" hint="在沙箱里运行，最长 1 秒，不能访问网络和文件；要调接口用 HTTP 请求节点。">
          <Textarea className="min-h-40 rounded-lg bg-muted/50 font-mono text-[12px] leading-[19px]" value={c.code ?? ""} onChange={(e) => set((x) => { x.code = e.target.value })} spellCheck={false} data-script-editor /></F>}
        {n.kind === "cmd" && <><F label="命令" hint="在项目目录里执行，每次都要你批准。"><Input className="rounded-lg font-mono text-[12px]" value={c.command ?? ""} onChange={(e) => set((x) => { x.command = e.target.value })} /></F>
          <F label="超时（秒，最多 600）"><Input type="number" min={1} max={600} className="w-28 rounded-lg" value={c.timeout_s ?? 120} onChange={(e) => set((x) => { x.timeout_s = Number(e.target.value) })} /></F></>}
        {n.kind === "http" && <HttpEditor c={c} set={set} />}
        {n.kind === "flow" && <F label="调用哪条流程" hint="同步调用，最多嵌套 5 层。">
          <Pick label="调用哪条流程" value={c.flow_id || "_"} options={[{ v: "_", l: "选一条流程" }, ...flows.filter((f) => f.id !== flowId).map((f) => ({ v: f.id, l: f.name }))]} onChange={(v) => set((x) => { x.flow_id = v === "_" ? "" : v })} className="w-full" /></F>}
        {n.kind === "bot" && <>
          <F label="交给"><Pick label="交给哪个 Bot" value={c.bot_id || "_"} options={[{ v: "_", l: "这个事件源的判断 Bot" }, ...botOptions()]} onChange={(v) => set((x) => { if (v === "_") delete x.bot_id; else x.bot_id = v })} className="w-full" /></F>
          <F label="要它判断什么（分支按判断结果走）"><Textarea className="min-h-16 rounded-lg" value={c.instruction ?? ""} onChange={(e) => set((x) => { x.instruction = e.target.value })} /></F>
          <F label="分支（逗号分隔）"><Input className="rounded-lg" defaultValue={(c.branches ?? []).join("，")} onBlur={(e) => set((x) => { x.branches = list(e.target.value) })} /></F></>}
        {n.kind === "wait" && <WaitEditor c={c} set={set} />}
        {n.kind === "approve" && <>
          <F label="问你什么"><Textarea className="min-h-16 rounded-lg" value={c.question ?? ""} onChange={(e) => set((x) => { x.question = e.target.value })} /></F>
          <F label="选项（逗号分隔，对应后面的分支）" hint="进待处理，你选的就是走的分支。"><Input className="rounded-lg" defaultValue={(c.options ?? []).join("，")} onBlur={(e) => set((x) => { x.options = list(e.target.value) })} /></F></>}
        {n.kind === "action" && <ActionEditor c={c} set={set} />}
        {n.kind !== "action" && n.kind !== "trigger" && <div className="type-footnote text-muted-foreground">可用的数据：<span className="mr-1 font-mono">event</span>{prev.map((p) => <span key={p.id} className="mr-1 font-mono" title={p.title}>{`steps.${p.id}`}</span>)}</div>}
        {!["action", "trigger", "approve"].includes(n.kind) && <div className="flex items-center justify-between gap-2 type-footnote text-muted-foreground">失败时
          <Pick label="失败时" value={n.on_fail || "stop"} onChange={(v) => onChange(patchNode(g, n.id, (x) => { x.on_fail = v === "stop" ? null : v }))}
            options={[{ v: "stop", l: "停止这次运行" }, { v: "continue", l: "跳过，继续下一步" }, ...outs.filter((e) => e.label).map((e) => ({ v: "branch:" + e.label, l: `走“${e.label}”分支` }))]} /></div>}
        {effectful(n) && <div className="type-footnote text-muted-foreground">对外发送、花钱、删除类动作先进待处理，等你批准；试运行时不执行。</div>}
      </div>
      {outs.length > 0 && (labels.length > 0 || outs.length > 1 || outs.some((e) => e.label)) && (
        <div className="flex flex-col gap-2 px-1" data-branches>
          <div className="type-footnote text-muted-foreground">分支</div>
          {outs.map((e) => <EdgeRow key={e.to} g={g} e={e} labels={labels} onChange={onChange} />)}
        </div>
      )}
      {step && <div className="px-1"><div className="mb-1 type-footnote text-muted-foreground">这次运行 · {ms(step.ms)}{step.branch ? " · 走了“" + step.branch + "”" : ""}</div>{step.error ? <div className="type-footnote text-destructive">{step.error}</div> : <Json v={step.output} />}</div>}
      <div className="flex flex-wrap gap-2 px-1">
        {n.kind !== "action" && (
          <Popover open={palette} onOpenChange={setPalette}>
            <PopoverTrigger render={<Button size="sm" variant="outline" data-add-node />}><PlusIcon />在后面添加</PopoverTrigger>
            <PopoverContent align="start" className="w-80 p-0">
              <Command filter={(v, s) => (v.toLowerCase().includes(s.trim().toLowerCase()) ? 1 : 0)}>
                <CommandInput placeholder="搜索节点，例如 决策表、HTTP" data-palette-search />
                <CommandList className="max-h-80">
                  <CommandEmpty>没有这类节点</CommandEmpty>
                  {PALETTE.map((grp, gi) => <div key={grp.g}>{gi > 0 && <CommandSeparator />}<CommandGroup heading={grp.g}>
                    {grp.items.map(([k, t, d, cfg]) => { const I = kindOf(k)[1]; return <CommandItem key={t} value={t + " " + d} onSelect={() => {
                      setPalette(false)
                      const used = new Set(outs.map((e) => e.label)), lab = labels.find((l) => l && !used.has(l)) ?? ""
                      const [g2, id] = addAfter(g, n.id, k, t, cfg, lab); onChange(g2, id)
                    }} data-action-opt={t}><I /><span className="whitespace-nowrap">{t}</span><span className="ml-auto truncate type-footnote text-muted-foreground">{d}</span></CommandItem> })}
                  </CommandGroup></div>)}
                </CommandList>
              </Command>
            </PopoverContent>
          </Popover>
        )}
        {n.kind !== "trigger" && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { onChange(removeNode(g, n.id)); onSelect(null) }} data-del-node><TrashIcon />删除这个节点</Button>}
      </div>
    </div>
  )
}

function EdgeRow({ g, e, labels, onChange }: { g: Graph; e: GEdge; labels: string[]; onChange: (g: Graph) => void }) {
  const to = g.nodes.find((x) => x.id === e.to), lid = `edge-${e.from}-${e.to}`
  const setLabel = (v: string) => onChange({ ...g, edges: g.edges.map((x) => (x.from === e.from && x.to === e.to ? { ...x, label: v } : x)) })
  return (
    <div className="flex items-center gap-2">
      <Input aria-label={`到「${to?.title ?? e.to}」的分支`} list={lid} className="h-8 w-28 rounded-lg text-[12.5px]" placeholder="不限" value={e.label ?? ""} onChange={(ev) => setLabel(ev.target.value)} data-edge-label={e.to} />
      <datalist id={lid}>{labels.filter(Boolean).map((l) => <option key={l} value={l} />)}</datalist>
      <ArrowRightIcon className="size-3.5 shrink-0 text-tertiary" aria-hidden />
      <span className="min-w-0 flex-1 truncate type-callout">{to?.title ?? e.to}</span>
    </div>
  )
}

type Ed = { c: Record<string, any>; set: (f: (c: Record<string, any>) => void) => void }
function TriggerEditor({ c, set, sources }: Ed & { sources: Source[] }) {
  const src = c.source ?? "manual", known = sources.filter((s) => s.source === src).flatMap((s) => s.types)
  const types: string[] = c.types ?? []
  return (<>
    <F label="事件源"><Pick label="事件源" value={src} options={Object.entries(SOURCE_LABEL).map(([v, l]) => ({ v, l: <span className="inline-flex items-center gap-1.5">{(() => { const I = srcIcon(v); return <I className="size-3.5" /> })()}{l}</span> }))} onChange={(v) => set((x) => { x.source = v; x.types = [] })} className="w-full" /></F>
    {src === "time" ? <>
      <F label="cron（5 段）"><Input className="rounded-lg font-mono text-[12px]" value={c.cron ?? ""} onChange={(e) => set((x) => { x.cron = e.target.value })} placeholder="0 9 * * 1-5" /></F>
      <F label="时区"><Input className="rounded-lg" value={c.tz ?? ""} onChange={(e) => set((x) => { x.tz = e.target.value })} placeholder="Asia/Taipei" /></F>
    </> : src !== "manual" && (known.length ? (
      <fieldset className="flex flex-col gap-1.5"><legend className="mb-1 type-footnote text-muted-foreground">事件（不选 = 全部）</legend>
        {known.map((t) => <label key={t.key} className="flex items-center justify-between gap-2 type-callout">{t.label}<Switch checked={types.includes(t.key)} onCheckedChange={(v) => set((x) => { x.types = v ? [...types, t.key] : types.filter((k) => k !== t.key) })} aria-label={t.label} /></label>)}
      </fieldset>
    ) : <F label="事件类型（逗号分隔，留空 = 全部）"><Input className="rounded-lg font-mono text-[12px]" defaultValue={types.join(", ")} onBlur={(e) => set((x) => { x.types = list(e.target.value) })} placeholder={src + ".new"} /></F>)}
    {src !== "time" && src !== "manual" && <div className="type-footnote text-muted-foreground">账号和范围在 <a className="text-blue hover:underline" href={"#/auto/source/" + (sources.find((s) => s.source === src)?.adapter ?? src)}>事件源</a> 里改。</div>}
  </>)
}

function FilterEditor({ c, set }: Ed) {
  const rules: { label: string; when: string }[] = c.rules ?? []
  return (
    <div className="flex flex-col gap-2" data-filter-editor>
      <div className="type-footnote text-muted-foreground">从上往下，第一条成立的条件决定分支</div>
      {rules.map((r, i) => (
        <div key={i} className="flex flex-col gap-1 rounded-lg border p-2">
          <div className="flex items-center gap-2"><Input aria-label="分支名" className="h-7 w-24 rounded-md text-[12.5px]" value={r.label} onChange={(e) => set((x) => { x.rules[i].label = e.target.value })} />
            <span className="flex-1" /><Button size="icon-xs" variant="ghost" aria-label="删除这条条件" className="text-muted-foreground" onClick={() => set((x) => { x.rules.splice(i, 1) })}><TrashIcon /></Button></div>
          <Textarea aria-label="条件表达式" className="min-h-12 rounded-md font-mono text-[12px]" placeholder={'event.payload.from matches "noreply"'} value={r.when} onChange={(e) => set((x) => { x.rules[i].when = e.target.value })} />
        </div>
      ))}
      <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => set((x) => { (x.rules ??= []).push({ label: "分支" + ((x.rules?.length ?? 0) + 1), when: "" }) })}><PlusIcon />加一条条件</Button>
      <F label="都不成立时走"><Input className="h-8 rounded-lg" value={c.else_label ?? ""} onChange={(e) => set((x) => { x.else_label = e.target.value })} /></F>
    </div>
  )
}

function TableEditor({ c, set }: Ed) {
  const inputs: string[] = c.inputs ?? [], outputs: string[] = c.outputs ?? [], rows: string[][] = c.rows ?? [], cols = [...inputs, ...outputs]
  return (
    <div className="flex flex-col gap-2" data-table-editor>
      <div className="flex items-center gap-2 type-footnote text-muted-foreground">命中方式<Segmented size="sm" value={c.hit ?? "first"} onChange={(v) => set((x) => { x.hit = v })} options={[["first", "第一条"], ["collect", "全部收集"]] as const} /></div>
      <F label="输入列（字段路径，逗号分隔）"><Input className="h-8 rounded-lg font-mono text-[12px]" defaultValue={inputs.join(", ")} onBlur={(e) => set((x) => { const ni = list(e.target.value); x.rows = rows.map((r) => [...ni.map((_, j) => r[j] ?? ""), ...r.slice(inputs.length)]); x.inputs = ni })} /></F>
      <F label="输出列（逗号分隔）"><Input className="h-8 rounded-lg font-mono text-[12px]" defaultValue={outputs.join(", ")} onBlur={(e) => set((x) => { const no = list(e.target.value); x.rows = rows.map((r) => [...r.slice(0, inputs.length), ...no.map((_, j) => r[inputs.length + j] ?? "")]); x.outputs = no })} /></F>
      <div className="overflow-x-auto rounded-lg border">
        <Table className="min-w-[240px] text-[12px]">
          <TableHeader><TableRow className="hover:bg-transparent">{cols.map((h, j) => <TableHead key={j} className={cn("h-8 px-2 text-[12px]", j >= inputs.length ? "bg-muted/60 text-foreground" : "text-muted-foreground")}>{j >= inputs.length && "→ "}{h}</TableHead>)}<TableHead className="w-6 px-0" /></TableRow></TableHeader>
          <TableBody>{rows.map((row, i) => <TableRow key={i}>{cols.map((_, j) => (
            <TableCell key={j} className={cn("p-0", j >= inputs.length && "bg-muted/30")}><input aria-label={`第 ${i + 1} 行 ${cols[j]}`} value={row[j] ?? ""} placeholder="任意" onChange={(e) => set((x) => { x.rows[i][j] = e.target.value })} className="w-full min-w-0 bg-transparent px-2 py-1.5 font-mono outline-none placeholder:text-tertiary focus:bg-blue/5" data-cell={`${i}-${j}`} /></TableCell>))}
            <TableCell className="p-0"><Button size="icon-xs" variant="ghost" aria-label={`删除第 ${i + 1} 行`} className="text-muted-foreground" onClick={() => set((x) => { x.rows.splice(i, 1) })}><TrashIcon /></Button></TableCell></TableRow>)}</TableBody>
        </Table>
      </div>
      <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => set((x) => { (x.rows ??= []).push(cols.map(() => "")) })} data-add-row><PlusIcon />加一行</Button>
      <div className="type-footnote text-muted-foreground">从上往下匹配；单元格写表达式，例如 <span className="font-mono">{"<= 500"}</span>、<span className="font-mono">{'"交通", "餐饮"'}</span>，留空表示任意。出边的分支名等于第一个输出值。这一步不调用 AI，同样的输入总是同样的结果。</div>
    </div>
  )
}

function HttpEditor({ c, set }: Ed) {
  return (
    <div className="flex flex-col gap-2" data-http-editor>
      <div className="flex gap-2"><Pick label="请求方法" value={c.method ?? "GET"} options={["GET", "POST", "PUT", "PATCH", "DELETE"].map((v) => ({ v, l: v }))} onChange={(v) => set((x) => { x.method = v; if (v !== "GET") x.effect = true })} />
        <Input aria-label="URL" className="h-8 rounded-lg font-mono text-[12px]" value={c.url ?? ""} onChange={(e) => set((x) => { x.url = e.target.value })} /></div>
      {(c.method ?? "GET") !== "GET" && <Textarea aria-label="请求体" className="min-h-20 rounded-lg bg-muted/50 font-mono text-[12px]" value={c.body ?? ""} onChange={(e) => set((x) => { x.body = e.target.value })} />}
      <div className="type-footnote text-muted-foreground">用 <span className="font-mono">{"{{steps.<步骤>.<字段>}}"}</span> 引用前面步骤的输出，用 <span className="font-mono">{"{{secrets.<条目>.<字段>}}"}</span> 引用保险箱里的机密（只能写在请求头和请求体里），机密值不会显示。</div>
      <label className="flex items-center justify-between gap-3 rounded-lg border px-3 py-2 type-callout"><span>会写入或发送数据<span className="block type-footnote text-muted-foreground">{c.effect ? "每次执行前进待处理等你批准" : "只读请求，直接执行"}</span></span>
        <Switch checked={!!c.effect} disabled={(c.method ?? "GET") !== "GET"} onCheckedChange={(v) => set((x) => { x.effect = v })} aria-label="会写入或发送数据" /></label>
    </div>
  )
}

function WaitEditor({ c, set }: Ed) {
  const mode = c.mode ?? "delay"
  return (
    <div className="flex flex-col gap-2" data-wait-editor>
      <Segmented size="sm" value={mode} onChange={(v) => set((x) => { x.mode = v })} options={[["delay", "延时"], ["event", "等事件"], ["until", "等到时间"]] as const} />
      {mode === "delay" && <F label="多久（例如 30m、2h、1d）"><Input className="h-8 rounded-lg" value={c.delay ?? ""} onChange={(e) => set((x) => { x.delay = e.target.value })} /></F>}
      {mode === "until" && <F label="到什么时候（例如 next_workday 09:00）"><Input className="h-8 rounded-lg" value={c.until ?? ""} onChange={(e) => set((x) => { x.until = e.target.value })} /></F>}
      {mode === "event" && <>
        <div className="flex gap-2"><F label="来源"><Input className="h-8 rounded-lg" value={c.match?.source ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).source = e.target.value })} placeholder="webhook" /></F>
          <F label="事件类型"><Input className="h-8 rounded-lg" value={c.match?.type ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).type = e.target.value })} /></F></div>
        <F label="条件"><Input className="h-8 rounded-lg font-mono text-[12px]" value={c.match?.when ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).when = e.target.value })} /></F></>}
      {mode !== "delay" && <div className="flex gap-2"><F label="最长等多久"><Input className="h-8 rounded-lg" value={c.timeout ?? ""} onChange={(e) => set((x) => { x.timeout = e.target.value })} placeholder="3d" /></F>
        <F label="超时分支"><Input className="h-8 rounded-lg" value={c.timeout_label ?? ""} onChange={(e) => set((x) => { x.timeout_label = e.target.value })} placeholder="超时" /></F></div>}
      <div className="type-footnote text-muted-foreground">等待期间 Steve 重启也不会丢；超时后走“超时”分支。</div>
    </div>
  )
}

function ActionEditor({ c, set }: Ed) {
  const a = c.action ?? "notify"
  return (
    <div className="flex flex-col gap-2" data-action-editor>
      <F label="动作"><Pick label="动作" value={a} options={Object.entries(ACTION_LABEL).map(([v, l]) => ({ v, l }))} onChange={(v) => set((x) => { for (const k of Object.keys(x)) delete x[k]; Object.assign(x, PALETTE.flatMap((p) => p.items).find((i) => i[3].action === v)?.[3] ?? {}, { action: v }) })} className="w-full" /></F>
      {a === "item.create" && <>
        <F label="方式"><Segmented size="sm" value={c.mode ?? "start"} onChange={(v) => set((x) => { x.mode = v })} options={[["start", "直接开始"], ["propose", "先给方案"]] as const} /></F>
        <F label="交给"><Pick label="交给哪个 Bot" value={c.bot || "_"} options={[{ v: "_", l: "这个事件源的判断 Bot" }, ...botOptions()]} onChange={(v) => set((x) => { if (v === "_") delete x.bot; else x.bot = v })} className="w-full" /></F>
        <label className="flex items-center justify-between gap-2 type-callout">自动验收<Switch checked={c.accept === "auto"} onCheckedChange={(v) => set((x) => { if (v) x.accept = "auto"; else delete x.accept })} aria-label="自动验收" /></label>
      </>}
      {a === "item.link" && <>
        <F label="按什么找已有事项"><Pick label="按什么找已有事项" value={c.by ?? "thread"} options={[{ v: "thread", l: "同一往来" }, { v: "branch", l: "分支名 steve/<编号>" }, { v: "ref", l: "正文里的编号" }]} onChange={(v) => set((x) => { x.by = v })} className="w-full" /></F>
        <F label="找不到时"><Pick label="找不到时" value={c.fallback ?? "create"} options={[{ v: "create", l: "建一个新事项" }, { v: "none", l: "什么都不做" }]} onChange={(v) => set((x) => { x.fallback = v })} className="w-full" /></F></>}
      {(a === "notify" || a === "digest") && <F label={a === "notify" ? "提醒你的内容" : "写进汇报的内容"}><Textarea className="min-h-16 rounded-lg" value={c.text ?? ""} onChange={(e) => set((x) => { x.text = e.target.value })} /></F>}
      {a === "ignore" && <F label="理由（记在信号上）"><Input className="h-8 rounded-lg" value={c.reason ?? ""} onChange={(e) => set((x) => { x.reason = e.target.value })} /></F>}
      {EFFECT_ACTIONS.includes(a) && <Json v={c} />}
    </div>
  )
}
