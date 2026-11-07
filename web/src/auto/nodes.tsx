/* Flow node kinds: labels, the palette, defaults, graph edits and the inspector (signals-automation.md §3.2,
 * prototype KIND / PALETTE / TableEditor / the RuleView aside) */
import { useState, type ReactNode } from "react"
import { Trans } from "react-i18next"
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
import { t, useT } from "@/i18n"
import { branchLabels, renameBranchEdges, RENAMES_EDGES } from "@/auto/branches"
export { branchLabels, renameBranchEdges, branchWarnings, type BranchWarning } from "@/auto/branches"

type Grp = "trigger" | "logic" | "code" | "connect" | "ai" | "wait" | "result"
/* [icon, group]; the group drives the palette and the inspector heading */
export const KIND = {
  trigger: [ZapIcon, "trigger"], filter: [FilterIcon, "logic"], table: [TableIcon, "logic"],
  script: [CodeIcon, "code"], cmd: [TerminalSquareIcon, "code"], http: [GlobeIcon, "connect"], flow: [WorkflowIcon, "connect"],
  bot: [BotIcon, "ai"], wait: [HourglassIcon, "wait"], approve: [UserCheckIcon, "wait"], action: [ArrowRightIcon, "result"],
} satisfies Record<string, [LucideIcon, Grp]>
type Kind = keyof typeof KIND
const isKind = (k: string): k is Kind => Object.hasOwn(KIND, k)
/** [label, icon, group] of a node kind, in the current language */
export const kindOf = (k: string): [string, LucideIcon, string] => (isKind(k) ? [t(`auto:kind.${k}`), KIND[k][0], t(`auto:group.${KIND[k][1]}`)] : [k, WorkflowIcon, t("auto:group.other")])
/* an action's key in the hub → its label key */
const ACTION_KEY = { "item.create": "itemCreate", "item.link": "itemLink", notify: "notify", digest: "digest", ignore: "ignore", reply_as: "replyAs", send_to_space: "sendToSpace" } as const
/** an action's label in the current language; undefined for an unknown action */
export const actionLabel = (a: string | null | undefined) => (a && Object.hasOwn(ACTION_KEY, a) ? t(`auto:action.${ACTION_KEY[a as keyof typeof ACTION_KEY]}`) : undefined)
const EFFECT_ACTIONS = ["reply_as", "send_to_space"]
/** a step that reaches outside Steve: needs an approval (or a grant) on every run */
export const effectful = (n: GNode) => n.kind === "cmd" || (n.kind === "http" && !!n.config?.effect) || (n.kind === "action" && EFFECT_ACTIONS.includes(n.config?.action))

type PaletteItem = [kind: string, title: string, desc: string, config: Record<string, any>]
/** the palette in the current language; titles and default branch names are what a new node starts with */
export const palette = (): { g: string; items: PaletteItem[] }[] => {
  const yes = t("auto:palette.yes"), no = t("auto:palette.no")
  return [
    { g: t("auto:group.logic"), items: [["filter", t("auto:kind.filter"), t("auto:palette.filter"), { rules: [{ label: yes, when: "" }], else_label: no }], ["table", t("auto:kind.table"), t("auto:palette.table"), { hit: "first", inputs: ["event.summary"], outputs: [t("auto:palette.result")], rows: [["", ""]] }]] },
    { g: t("auto:group.code"), items: [["script", t("auto:kind.script"), t("auto:palette.script"), { code: "export const handler = async (input) => {\n  return { ...input }\n}" }], ["cmd", t("auto:kind.cmd"), t("auto:palette.cmd"), { command: "./scripts/check.sh", project_id: "", timeout_s: 120 }]] },
    { g: t("auto:group.connect"), items: [["http", t("auto:kind.http"), t("auto:palette.http"), { method: "GET", url: "https://", headers: {}, body: "", effect: false }], ["flow", t("auto:kind.flow"), t("auto:palette.flow"), { flow_id: "", input: {} }]] },
    { g: t("auto:group.ai"), items: [["bot", t("auto:kind.bot"), t("auto:palette.bot"), { instruction: "", branches: [yes, no] }]] },
    { g: t("auto:group.wait"), items: [["wait", t("auto:kind.wait"), t("auto:palette.wait"), { mode: "delay", delay: "30m" }], ["approve", t("auto:kind.approve"), t("auto:palette.approve"), { question: t("auto:palette.question"), options: [t("auto:palette.continue"), t("auto:palette.stop")] }]] },
    { g: t("auto:group.result"), items: [["action", t("auto:action.itemCreate"), t("auto:palette.itemCreate"), { action: "item.create", mode: "start" }], ["action", t("auto:action.itemLink"), t("auto:palette.itemLink"), { action: "item.link", by: "thread", fallback: "create" }], ["action", t("auto:action.notify"), t("auto:palette.notify"), { action: "notify", text: "{{event.summary}}" }], ["action", t("auto:action.digest"), t("auto:palette.digest"), { action: "digest", text: "{{event.summary}}" }], ["action", t("auto:action.ignore"), t("auto:palette.ignore"), { action: "ignore", reason: "" }]] },
  ]
}

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
  const [paletteOpen, setPalette] = useState(false)
  const t = useT()
  const set = (f: (c: Record<string, any>, n: GNode) => void) => {
    const next = patchNode(g, n.id, (x) => { x.config ??= {}; f(x.config, x) }), m = next.nodes.find((x) => x.id === n.id)
    onChange(m && RENAMES_EDGES.has(n.kind) ? renameBranchEdges(next, n.id, branchLabels(n), branchLabels(m)) : next)
  }
  const c = n.config ?? {}
  const [label, , group] = kindOf(n.kind)
  const outs = g.edges.filter((e) => e.from === n.id), labels = branchLabels(n)
  const prev = upstream(g, n.id)
  return (
    <div className="flex flex-col gap-4" data-inspector data-kind={n.kind}>
      <SectionLabel className="mb-0" right={<span className="font-normal">{group}</span>}>{label}{n.kind === "action" && c.action ? " · " + (actionLabel(c.action) ?? c.action) : ""}</SectionLabel>
      <div className="flex flex-col gap-3 px-1">
        <F label={t("auto:node.name")}><Input className="rounded-lg" value={n.title} onChange={(e) => onChange(patchNode(g, n.id, (x) => { x.title = e.target.value }))} data-node-title /></F>
        <F label={t("auto:node.note")}><Input className="rounded-lg" value={n.note ?? ""} onChange={(e) => onChange(patchNode(g, n.id, (x) => { x.note = e.target.value }))} /></F>
        {n.kind === "trigger" && <TriggerEditor c={c} set={set} sources={sources} />}
        {n.kind === "filter" && <FilterEditor c={c} set={set} />}
        {n.kind === "table" && <TableEditor c={c} set={set} />}
        {n.kind === "script" && <F label={t("auto:node.script")} hint={t("auto:node.scriptHint")}>
          <Textarea className="min-h-40 rounded-lg bg-muted/50 font-mono text-[12px] leading-[19px]" value={c.code ?? ""} onChange={(e) => set((x) => { x.code = e.target.value })} spellCheck={false} data-script-editor /></F>}
        {n.kind === "cmd" && <><F label={t("auto:node.command")} hint={t("auto:node.commandHint")}><Input className="rounded-lg font-mono text-[12px]" value={c.command ?? ""} onChange={(e) => set((x) => { x.command = e.target.value })} /></F>
          <F label={t("auto:node.timeoutS")}><Input type="number" min={1} max={600} className="w-28 rounded-lg" value={c.timeout_s ?? 120} onChange={(e) => set((x) => { x.timeout_s = Number(e.target.value) })} /></F></>}
        {n.kind === "http" && <HttpEditor c={c} set={set} />}
        {n.kind === "flow" && <F label={t("auto:node.callFlow")} hint={t("auto:node.callFlowHint")}>
          <Pick label={t("auto:node.callFlow")} value={c.flow_id || "_"} options={[{ v: "_", l: t("auto:node.pickFlow") }, ...flows.filter((f) => f.id !== flowId).map((f) => ({ v: f.id, l: f.name }))]} onChange={(v) => set((x) => { x.flow_id = v === "_" ? "" : v })} className="w-full" /></F>}
        {n.kind === "bot" && <>
          <F label={t("auto:node.handTo")}><Pick label={t("auto:node.handToAria")} value={c.bot_id || "_"} options={[{ v: "_", l: t("auto:node.sourceBot") }, ...botOptions()]} onChange={(v) => set((x) => { if (v === "_") delete x.bot_id; else x.bot_id = v })} className="w-full" /></F>
          <F label={t("auto:node.instruction")}><Textarea className="min-h-16 rounded-lg" value={c.instruction ?? ""} onChange={(e) => set((x) => { x.instruction = e.target.value })} /></F>
          <F label={t("auto:node.branchesList")}><Input className="rounded-lg" defaultValue={(c.branches ?? []).join(t("auto:sep.input"))} onBlur={(e) => set((x) => { x.branches = list(e.target.value) })} /></F></>}
        {n.kind === "wait" && <WaitEditor c={c} set={set} />}
        {n.kind === "approve" && <>
          <F label={t("auto:node.question")}><Textarea className="min-h-16 rounded-lg" value={c.question ?? ""} onChange={(e) => set((x) => { x.question = e.target.value })} /></F>
          <F label={t("auto:node.options")} hint={t("auto:node.optionsHint")}><Input className="rounded-lg" defaultValue={(c.options ?? []).join(t("auto:sep.input"))} onBlur={(e) => set((x) => { x.options = list(e.target.value) })} /></F></>}
        {n.kind === "action" && <ActionEditor c={c} set={set} />}
        {n.kind !== "action" && n.kind !== "trigger" && <div className="type-footnote text-muted-foreground">{t("auto:node.data")}<span className="mr-1 font-mono">event</span>{prev.map((p) => <span key={p.id} className="mr-1 font-mono" title={p.title}>{`steps.${p.id}`}</span>)}</div>}
        {!["action", "trigger", "approve"].includes(n.kind) && <div className="flex items-center justify-between gap-2 type-footnote text-muted-foreground">{t("auto:node.onFail")}
          <Pick label={t("auto:node.onFail")} value={n.on_fail || "stop"} onChange={(v) => onChange(patchNode(g, n.id, (x) => { x.on_fail = v === "stop" ? null : v }))}
            options={[{ v: "stop", l: t("auto:node.failStop") }, { v: "continue", l: t("auto:node.failContinue") }, ...outs.filter((e) => e.label).map((e) => ({ v: "branch:" + e.label, l: t("auto:node.failBranch", { label: e.label }) }))]} /></div>}
        {effectful(n) && <div className="type-footnote text-muted-foreground">{t("auto:node.effectNote")}</div>}
      </div>
      {outs.length > 0 && (labels.length > 0 || outs.length > 1 || outs.some((e) => e.label)) && (
        <div className="flex flex-col gap-2 px-1" data-branches>
          <div className="type-footnote text-muted-foreground">{t("auto:node.branches")}</div>
          {outs.map((e) => <EdgeRow key={e.to} g={g} e={e} labels={labels} onChange={onChange} />)}
        </div>
      )}
      {step && <div className="px-1"><div className="mb-1 type-footnote text-muted-foreground">{[t("auto:node.thisRun", { ms: ms(step.ms) }), step.branch ? t("auto:node.tookBranch", { branch: step.branch }) : ""].filter(Boolean).join(" · ")}</div>{step.error ? <div className="type-footnote text-destructive">{step.error}</div> : <Json v={step.output} />}</div>}
      <div className="flex flex-wrap gap-2 px-1">
        {n.kind !== "action" && (
          <Popover open={paletteOpen} onOpenChange={setPalette}>
            <PopoverTrigger render={<Button size="sm" variant="outline" data-add-node />}><PlusIcon />{t("auto:node.addAfter")}</PopoverTrigger>
            <PopoverContent align="start" className="w-80 p-0">
              <Command filter={(v, s) => (v.toLowerCase().includes(s.trim().toLowerCase()) ? 1 : 0)}>
                <CommandInput placeholder={t("auto:node.search")} data-palette-search />
                <CommandList className="max-h-80">
                  <CommandEmpty>{t("auto:node.noMatch")}</CommandEmpty>
                  {palette().map((grp, gi) => <div key={grp.g}>{gi > 0 && <CommandSeparator />}<CommandGroup heading={grp.g}>
                    {grp.items.map(([k, title, d, cfg]) => { const I = kindOf(k)[1]; return <CommandItem key={title} value={title + " " + d} onSelect={() => {
                      setPalette(false)
                      const used = new Set(outs.map((e) => e.label)), lab = labels.find((l) => l && !used.has(l)) ?? ""
                      const [g2, id] = addAfter(g, n.id, k, title, cfg, lab); onChange(g2, id)
                    }} data-action-opt={title}><I /><span className="whitespace-nowrap">{title}</span><span className="ml-auto truncate type-footnote text-muted-foreground">{d}</span></CommandItem> })}
                  </CommandGroup></div>)}
                </CommandList>
              </Command>
            </PopoverContent>
          </Popover>
        )}
        {n.kind !== "trigger" && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { onChange(removeNode(g, n.id)); onSelect(null) }} data-del-node><TrashIcon />{t("auto:node.remove")}</Button>}
      </div>
    </div>
  )
}

function EdgeRow({ g, e, labels, onChange }: { g: Graph; e: GEdge; labels: string[]; onChange: (g: Graph) => void }) {
  const to = g.nodes.find((x) => x.id === e.to), lid = `edge-${e.from}-${e.to}`
  const t = useT()
  const setLabel = (v: string) => onChange({ ...g, edges: g.edges.map((x) => (x.from === e.from && x.to === e.to ? { ...x, label: v } : x)) })
  return (
    <div className="flex items-center gap-2">
      <Input aria-label={t("auto:node.edgeAria", { title: to?.title ?? e.to })} list={lid} className="h-8 w-28 rounded-lg text-[12.5px]" placeholder={t("auto:node.any")} value={e.label ?? ""} onChange={(ev) => setLabel(ev.target.value)} data-edge-label={e.to} />
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
  const t = useT()
  return (<>
    <F label={t("auto:trigger.source")}><Pick label={t("auto:trigger.source")} value={src} options={Object.entries(SOURCE_LABEL).map(([v, l]) => ({ v, l: <span className="inline-flex items-center gap-1.5">{(() => { const I = srcIcon(v); return <I className="size-3.5" /> })()}{l}</span> }))} onChange={(v) => set((x) => { x.source = v; x.types = [] })} className="w-full" /></F>
    {src === "time" ? <>
      <F label={t("auto:trigger.cron")}><Input className="rounded-lg font-mono text-[12px]" value={c.cron ?? ""} onChange={(e) => set((x) => { x.cron = e.target.value })} placeholder="0 9 * * 1-5" /></F>
      <F label={t("auto:trigger.tz")}><Input className="rounded-lg" value={c.tz ?? ""} onChange={(e) => set((x) => { x.tz = e.target.value })} placeholder="Asia/Taipei" /></F>
    </> : src !== "manual" && (known.length ? (
      <fieldset className="flex flex-col gap-1.5"><legend className="mb-1 type-footnote text-muted-foreground">{t("auto:trigger.events")}</legend>
        {known.map((y) => <label key={y.key} className="flex items-center justify-between gap-2 type-callout">{y.label}<Switch checked={types.includes(y.key)} onCheckedChange={(v) => set((x) => { x.types = v ? [...types, y.key] : types.filter((k) => k !== y.key) })} aria-label={y.label} /></label>)}
      </fieldset>
    ) : <F label={t("auto:trigger.types")}><Input className="rounded-lg font-mono text-[12px]" defaultValue={types.join(", ")} onBlur={(e) => set((x) => { x.types = list(e.target.value) })} placeholder={src + ".new"} /></F>)}
    {src !== "time" && src !== "manual" && <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey="auto:trigger.scopeHint" components={{ page: <a className="text-blue hover:underline" href={"#/auto/source/" + (sources.find((s) => s.source === src)?.adapter ?? src)} /> }} /></div>}
  </>)
}

function FilterEditor({ c, set }: Ed) {
  const rules: { label: string; when: string }[] = c.rules ?? []
  const t = useT()
  return (
    <div className="flex flex-col gap-2" data-filter-editor>
      <div className="type-footnote text-muted-foreground">{t("auto:filter.hint")}</div>
      {rules.map((r, i) => (
        <div key={i} className="flex flex-col gap-1 rounded-lg border p-2">
          <div className="flex items-center gap-2"><Input aria-label={t("auto:filter.label")} className="h-7 w-24 rounded-md text-[12.5px]" value={r.label} onChange={(e) => set((x) => { x.rules[i].label = e.target.value })} />
            <span className="flex-1" /><Button size="icon-xs" variant="ghost" aria-label={t("auto:filter.removeRule")} className="text-muted-foreground" onClick={() => set((x) => { x.rules.splice(i, 1) })}><TrashIcon /></Button></div>
          <Textarea aria-label={t("auto:filter.expr")} className="min-h-12 rounded-md font-mono text-[12px]" placeholder={'event.payload.from matches "noreply"'} value={r.when} onChange={(e) => set((x) => { x.rules[i].when = e.target.value })} />
        </div>
      ))}
      <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => set((x) => { (x.rules ??= []).push({ label: t("auto:filter.defaultLabel", { n: (x.rules?.length ?? 0) + 1 }), when: "" }) })}><PlusIcon />{t("auto:filter.addRule")}</Button>
      <F label={t("auto:filter.else")}><Input className="h-8 rounded-lg" value={c.else_label ?? ""} onChange={(e) => set((x) => { x.else_label = e.target.value })} /></F>
    </div>
  )
}

function TableEditor({ c, set }: Ed) {
  const inputs: string[] = c.inputs ?? [], outputs: string[] = c.outputs ?? [], rows: string[][] = c.rows ?? [], cols = [...inputs, ...outputs]
  const t = useT()
  return (
    <div className="flex flex-col gap-2" data-table-editor>
      <div className="flex items-center gap-2 type-footnote text-muted-foreground">{t("auto:table.hit")}<Segmented size="sm" value={c.hit ?? "first"} onChange={(v) => set((x) => { x.hit = v })} options={[["first", t("auto:table.hitFirst")], ["collect", t("auto:table.hitCollect")]] as const} /></div>
      <F label={t("auto:table.inputs")}><Input className="h-8 rounded-lg font-mono text-[12px]" defaultValue={inputs.join(", ")} onBlur={(e) => set((x) => { const ni = list(e.target.value); x.rows = rows.map((r) => [...ni.map((_, j) => r[j] ?? ""), ...r.slice(inputs.length)]); x.inputs = ni })} /></F>
      <F label={t("auto:table.outputs")}><Input className="h-8 rounded-lg font-mono text-[12px]" defaultValue={outputs.join(", ")} onBlur={(e) => set((x) => { const no = list(e.target.value); x.rows = rows.map((r) => [...r.slice(0, inputs.length), ...no.map((_, j) => r[inputs.length + j] ?? "")]); x.outputs = no })} /></F>
      <div className="overflow-x-auto rounded-lg border">
        <Table className="min-w-[240px] text-[12px]">
          <TableHeader><TableRow className="hover:bg-transparent">{cols.map((h, j) => <TableHead key={j} className={cn("h-8 px-2 text-[12px]", j >= inputs.length ? "bg-muted/60 text-foreground" : "text-muted-foreground")}>{j >= inputs.length && "→ "}{h}</TableHead>)}<TableHead className="w-6 px-0" /></TableRow></TableHeader>
          <TableBody>{rows.map((row, i) => <TableRow key={i}>{cols.map((_, j) => (
            <TableCell key={j} className={cn("p-0", j >= inputs.length && "bg-muted/30")}><input aria-label={t("auto:table.cellAria", { row: i + 1, col: cols[j] })} value={row[j] ?? ""} placeholder={t("auto:table.any")} onChange={(e) => set((x) => { x.rows[i][j] = e.target.value })} className="w-full min-w-0 bg-transparent px-2 py-1.5 font-mono outline-none placeholder:text-tertiary focus:bg-blue/5" data-cell={`${i}-${j}`} /></TableCell>))}
            <TableCell className="p-0"><Button size="icon-xs" variant="ghost" aria-label={t("auto:table.removeRow", { row: i + 1 })} className="text-muted-foreground" onClick={() => set((x) => { x.rows.splice(i, 1) })}><TrashIcon /></Button></TableCell></TableRow>)}</TableBody>
        </Table>
      </div>
      <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => set((x) => { (x.rows ??= []).push(cols.map(() => "")) })} data-add-row><PlusIcon />{t("auto:table.addRow")}</Button>
      <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey="auto:table.hint" values={{ num: "<= 500", str: t("auto:table.strSample") }} components={{ num: <span className="font-mono" />, str: <span className="font-mono" /> }} /></div>
    </div>
  )
}

function HttpEditor({ c, set }: Ed) {
  const t = useT()
  return (
    <div className="flex flex-col gap-2" data-http-editor>
      <div className="flex gap-2"><Pick label={t("auto:http.method")} value={c.method ?? "GET"} options={["GET", "POST", "PUT", "PATCH", "DELETE"].map((v) => ({ v, l: v }))} onChange={(v) => set((x) => { x.method = v; if (v !== "GET") x.effect = true })} />
        <Input aria-label="URL" className="h-8 rounded-lg font-mono text-[12px]" value={c.url ?? ""} onChange={(e) => set((x) => { x.url = e.target.value })} /></div>
      {(c.method ?? "GET") !== "GET" && <Textarea aria-label={t("auto:http.body")} className="min-h-20 rounded-lg bg-muted/50 font-mono text-[12px]" value={c.body ?? ""} onChange={(e) => set((x) => { x.body = e.target.value })} />}
      <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey="auto:http.hint" components={{ steps: <span className="font-mono" />, secrets: <span className="font-mono" /> }}
        values={{ steps: `{{steps.<${t("auto:http.stepPart")}>.<${t("auto:http.fieldPart")}>}}`, secrets: `{{secrets.<${t("auto:http.entryPart")}>.<${t("auto:http.fieldPart")}>}}` }} /></div>
      <label className="flex items-center justify-between gap-3 rounded-lg border px-3 py-2 type-callout"><span>{t("auto:http.effect")}<span className="block type-footnote text-muted-foreground">{c.effect ? t("auto:http.effectOn") : t("auto:http.effectOff")}</span></span>
        <Switch checked={!!c.effect} disabled={(c.method ?? "GET") !== "GET"} onCheckedChange={(v) => set((x) => { x.effect = v })} aria-label={t("auto:http.effect")} /></label>
    </div>
  )
}

function WaitEditor({ c, set }: Ed) {
  const mode = c.mode ?? "delay"
  const t = useT()
  return (
    <div className="flex flex-col gap-2" data-wait-editor>
      <Segmented size="sm" value={mode} onChange={(v) => set((x) => { x.mode = v })} options={[["delay", t("auto:wait.delay")], ["event", t("auto:wait.event")], ["until", t("auto:wait.until")]] as const} />
      {mode === "delay" && <F label={t("auto:wait.howLong")}><Input className="h-8 rounded-lg" value={c.delay ?? ""} onChange={(e) => set((x) => { x.delay = e.target.value })} /></F>}
      {mode === "until" && <F label={t("auto:wait.untilWhen")}><Input className="h-8 rounded-lg" value={c.until ?? ""} onChange={(e) => set((x) => { x.until = e.target.value })} /></F>}
      {mode === "event" && <>
        <div className="flex gap-2"><F label={t("auto:wait.source")}><Input className="h-8 rounded-lg" value={c.match?.source ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).source = e.target.value })} placeholder="webhook" /></F>
          <F label={t("auto:wait.type")}><Input className="h-8 rounded-lg" value={c.match?.type ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).type = e.target.value })} /></F></div>
        <F label={t("auto:wait.when")}><Input className="h-8 rounded-lg font-mono text-[12px]" value={c.match?.when ?? ""} onChange={(e) => set((x) => { (x.match ??= {}).when = e.target.value })} /></F></>}
      {mode !== "delay" && <div className="flex gap-2"><F label={t("auto:wait.timeout")}><Input className="h-8 rounded-lg" value={c.timeout ?? ""} onChange={(e) => set((x) => { x.timeout = e.target.value })} placeholder="3d" /></F>
        <F label={t("auto:wait.timeoutLabel")}><Input className="h-8 rounded-lg" value={c.timeout_label ?? ""} onChange={(e) => set((x) => { x.timeout_label = e.target.value })} placeholder={t("auto:wait.timeoutDefault")} /></F></div>}
      <div className="type-footnote text-muted-foreground">{t("auto:wait.hint")}</div>
    </div>
  )
}

function ActionEditor({ c, set }: Ed) {
  const a = c.action ?? "notify"
  const t = useT()
  return (
    <div className="flex flex-col gap-2" data-action-editor>
      <F label={t("auto:actionEd.action")}><Pick label={t("auto:actionEd.action")} value={a} options={Object.keys(ACTION_KEY).map((v) => ({ v, l: actionLabel(v) }))} onChange={(v) => set((x) => { for (const k of Object.keys(x)) delete x[k]; Object.assign(x, palette().flatMap((p) => p.items).find((i) => i[3].action === v)?.[3] ?? {}, { action: v }) })} className="w-full" /></F>
      {a === "item.create" && <>
        <F label={t("auto:actionEd.mode")}><Segmented size="sm" value={c.mode ?? "start"} onChange={(v) => set((x) => { x.mode = v })} options={[["start", t("auto:actionEd.start")], ["propose", t("auto:actionEd.propose")]] as const} /></F>
        <F label={t("auto:node.handTo")}><Pick label={t("auto:node.handToAria")} value={c.bot || "_"} options={[{ v: "_", l: t("auto:node.sourceBot") }, ...botOptions()]} onChange={(v) => set((x) => { if (v === "_") delete x.bot; else x.bot = v })} className="w-full" /></F>
        <label className="flex items-center justify-between gap-2 type-callout">{t("auto:actionEd.autoAccept")}<Switch checked={c.accept === "auto"} onCheckedChange={(v) => set((x) => { if (v) x.accept = "auto"; else delete x.accept })} aria-label={t("auto:actionEd.autoAccept")} /></label>
      </>}
      {a === "item.link" && <>
        <F label={t("auto:actionEd.findBy")}><Pick label={t("auto:actionEd.findBy")} value={c.by ?? "thread"} options={[{ v: "thread", l: t("auto:actionEd.byThread") }, { v: "branch", l: t("auto:actionEd.byBranch") }, { v: "ref", l: t("auto:actionEd.byRef") }]} onChange={(v) => set((x) => { x.by = v })} className="w-full" /></F>
        <F label={t("auto:actionEd.notFound")}><Pick label={t("auto:actionEd.notFound")} value={c.fallback ?? "create"} options={[{ v: "create", l: t("auto:actionEd.createNew") }, { v: "none", l: t("auto:actionEd.doNothing") }]} onChange={(v) => set((x) => { x.fallback = v })} className="w-full" /></F></>}
      {(a === "notify" || a === "digest") && <F label={a === "notify" ? t("auto:actionEd.notifyText") : t("auto:actionEd.digestText")}><Textarea className="min-h-16 rounded-lg" value={c.text ?? ""} onChange={(e) => set((x) => { x.text = e.target.value })} /></F>}
      {a === "ignore" && <F label={t("auto:actionEd.reason")}><Input className="h-8 rounded-lg" value={c.reason ?? ""} onChange={(e) => set((x) => { x.reason = e.target.value })} /></F>}
      {EFFECT_ACTIONS.includes(a) && <Json v={c} />}
    </div>
  )
}
