/* 响应规则 editor (channels-personas.md §2.4, prototype channels.tsx Rules): “当 <条件> 就 <动作>”, one row per rule, first
 * match wins. Conditions with parameters open their inputs below the row: keywords, the intent text, members, hours. */
import { useEffect, useState } from "react"
import { PlusIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Group } from "@/ui"
import { Select } from "@/view-shared"
import { people, type PartyOption } from "@/api/people"
import type { Act, Rule, When } from "@/api/channels"

type Kind = When["kind"]
/** condition labels; your own account reads “@你 / 私聊你” */
export function whenLabel(k: Kind, self: boolean) {
  const L: Record<Kind, string> = {
    mention_or_dm: self ? "@你 或私聊你" : "被 @ 或私聊", mention: self ? "群里 @你" : "被 @", dm: self ? "私聊你" : "私聊", reply_to_me: "回复了它的消息",
    keyword: "消息包含关键词", intent: "AI 判断符合", member: "指定成员发言", external: "外部成员发言", hours: "按工作时间", all: "全部消息", other_group: "其他群消息",
  }
  return L[k] ?? k
}
const KINDS: Kind[] = ["mention_or_dm", "mention", "dm", "reply_to_me", "keyword", "intent", "member", "external", "hours", "other_group", "all"]
const ACT_LABEL: Record<Act, string> = { reply: "直接回复", draft: "起草，等你批准", record: "只记录", create_item: "建事项", handoff: "转给你" }
const DAYS = ["一", "二", "三", "四", "五", "六", "日"]

/** one rule in words: “被 @ 或私聊时直接回复” */
export function ruleText(r: Rule, self: boolean) {
  const w = r.when
  const cond = w.kind === "keyword" ? `消息包含「${w.words.join("、")}」` : w.kind === "intent" ? `AI 判断「${w.text}」` : w.kind === "hours" ? `${w.outside ? "工作时间外" : "工作时间内"}（${w.start}–${w.end}）`
    : w.kind === "member" ? `指定的 ${w.parties.length + w.teams.length} 个联系人或团队发言` : whenLabel(w.kind, self)
  const act = r.act === "create_item" ? (r.mode === "propose" ? "整理成需求提给你" : "建事项并开始") : ACT_LABEL[r.act]
  return `当${cond}，就${act}`
}

function fresh(kind: Kind, prev: When): When {
  if (kind === prev.kind) return prev
  switch (kind) {
    case "keyword": return { kind, words: [] }
    case "intent": return { kind, text: "" }
    case "member": return { kind, parties: [], teams: [] }
    case "hours": return { kind, outside: true, start: "09:00", end: "18:00", days: [1, 2, 3, 4, 5] }
    default: return { kind } as When
  }
}

/** a rule is ready to save when its parameters are filled */
export const ruleOk = (r: Rule) => { const w = r.when; return w.kind === "keyword" ? w.words.length > 0 : w.kind === "intent" ? !!w.text.trim() : w.kind === "member" ? w.parties.length + w.teams.length > 0 : true }

function Words({ words, onChange }: { words: string[]; onChange: (w: string[]) => void }) {
  const [t, setT] = useState("")
  const add = () => { const v = t.trim(); if (v && !words.includes(v) && words.length < 20) onChange([...words, v]); setT("") }
  return (
    <span className="flex flex-wrap items-center gap-1">{words.map((w) => (
      <span key={w} className="inline-flex h-6 items-center gap-1 rounded-full bg-muted pl-2.5 pr-1 text-[12px]">{w}
        <button type="button" aria-label={"删除关键词 " + w} onClick={() => onChange(words.filter((x) => x !== w))} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>))}
      <Input aria-label="添加关键词，回车确认" className="h-7 w-28 rounded-full text-[12px]" placeholder="添加关键词…" maxLength={40} value={t} onChange={(e) => setT(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); add() } }} onBlur={add} data-rule-word /></span>
  )
}

function Members({ w, onChange }: { w: Extract<When, { kind: "member" }>; onChange: (w: When) => void }) {
  const [q, setQ] = useState(""), [xs, setXs] = useState<PartyOption[]>([]), [names, setNames] = useState<Record<string, string>>({})
  useEffect(() => {
    const t = q.trim(); if (!t) { setXs([]); return }
    let live = true
    const h = setTimeout(() => people.search(t).then((r) => live && setXs([...r.contacts, ...r.teams]), () => live && setXs([])), 150)
    return () => { live = false; clearTimeout(h) }
  }, [q])
  const ids = [...w.parties.map((id) => ["p", id] as const), ...w.teams.map((id) => ["t", id] as const)]
  const pick = (p: PartyOption) => {
    setNames({ ...names, [p.id]: p.name }); setQ("")
    if (p.type === "team") { if (!w.teams.includes(p.id)) onChange({ ...w, teams: [...w.teams, p.id] }) }
    else if (!w.parties.includes(p.id)) onChange({ ...w, parties: [...w.parties, p.id] })
  }
  return (
    <span className="relative flex flex-wrap items-center gap-1">{ids.map(([k, id]) => (
      <span key={id} className="inline-flex h-6 items-center gap-1 rounded-full bg-muted pl-2.5 pr-1 text-[12px]">{names[id] ?? id}
        <button type="button" aria-label="移除" onClick={() => onChange(k === "p" ? { ...w, parties: w.parties.filter((x) => x !== id) } : { ...w, teams: w.teams.filter((x) => x !== id) })} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>))}
      <Input aria-label="搜索联系人或团队" className="h-7 w-36 rounded-full text-[12px]" placeholder="搜索联系人或团队…" value={q} onChange={(e) => setQ(e.target.value)} />
      {xs.length > 0 && <span role="listbox" className="absolute left-0 top-8 z-20 flex max-h-56 w-64 flex-col overflow-auto rounded-lg border bg-popover p-1 shadow-md">{xs.slice(0, 8).map((p) => (
        <button key={p.id} type="button" role="option" aria-selected={false} className="rounded-md px-2 py-1.5 text-left type-callout hover:bg-accent" onClick={() => pick(p)}>{p.name}<span className="ml-1.5 type-footnote text-muted-foreground">{p.type === "team" ? "团队" : p.sub}</span></button>))}</span>}
    </span>
  )
}

function Params({ w, onChange }: { w: When; onChange: (w: When) => void }) {
  if (w.kind === "keyword") return <Words words={w.words} onChange={(words) => onChange({ ...w, words })} />
  if (w.kind === "intent") return <Input aria-label="AI 判断的条件" className="h-8 w-full rounded-lg text-[13px] sm:w-80" maxLength={200} placeholder="例如：需要动手操作" value={w.text} onChange={(e) => onChange({ ...w, text: e.target.value })} data-rule-intent />
  if (w.kind === "member") return <Members w={w} onChange={onChange} />
  if (w.kind === "hours") return (
    <span className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground">
      <Select testId="hours-side" value={w.outside ? "out" : "in"} onChange={(v) => onChange({ ...w, outside: v === "out" })} options={[{ v: "out", l: "工作时间外" }, { v: "in", l: "工作时间内" }]} />
      <Input aria-label="开始时间" type="time" className="h-8 w-28 rounded-lg" value={w.start} onChange={(e) => onChange({ ...w, start: e.target.value })} />–
      <Input aria-label="结束时间" type="time" className="h-8 w-28 rounded-lg" value={w.end} onChange={(e) => onChange({ ...w, end: e.target.value })} />
      <span className="inline-flex gap-0.5" role="group" aria-label="工作日">{DAYS.map((d, n) => { const on = w.days.includes(n + 1); return (
        <button key={d} type="button" aria-pressed={on} onClick={() => onChange({ ...w, days: on ? w.days.filter((x) => x !== n + 1) : [...w.days, n + 1].sort() })}
          className={on ? "size-6 rounded-full bg-foreground text-[11px] text-background" : "size-6 rounded-full bg-muted text-[11px] text-muted-foreground"}>{d}</button>) })}</span>
    </span>
  )
  return null
}

/** the editor; `self` = your own account (no 直接回复: only drafts, trigger words are the exception) */
export function Rules({ rules, onChange, self = false, note }: { rules: Rule[]; onChange: (r: Rule[]) => void; self?: boolean; note?: string }) {
  const set = (n: number, r: Rule) => onChange(rules.map((x, i) => (i === n ? r : x)))
  const acts = (Object.keys(ACT_LABEL) as Act[]).filter((a) => !self || a !== "reply")
  return (
    <div className="flex flex-col gap-2" data-rules>
      {note && <div className="px-1 type-footnote text-muted-foreground">{note}</div>}
      {rules.length > 0 && <Group>{rules.map((r, n) => (
        <div key={n} className="flex flex-col gap-2 px-2 py-2" data-rule={n}>
          <div className="flex flex-wrap items-center gap-2">
            <span className="type-footnote text-muted-foreground">当</span>
            <Select testId={"when-" + n} value={r.when.kind} onChange={(v) => set(n, { ...r, when: fresh(v as Kind, r.when) })} options={KINDS.map((k) => ({ v: k, l: whenLabel(k, self) }))} />
            <span className="type-footnote text-muted-foreground">就</span>
            <Select testId={"act-" + n} value={r.act} onChange={(v) => set(n, { when: r.when, act: v as Act, ...(v === "create_item" ? { mode: r.mode ?? "start" } : {}) })} options={acts.map((a) => ({ v: a, l: ACT_LABEL[a] }))} />
            {r.act === "create_item" && <Select testId={"mode-" + n} value={r.mode ?? "start"} onChange={(v) => set(n, { ...r, mode: v as "start" | "propose" })} options={[{ v: "start", l: "直接开始" }, { v: "propose", l: "先提给你" }]} />}
            <span className="flex-1" />
            <Button variant="ghost" size="icon-xs" aria-label={`删除第 ${n + 1} 条规则`} className="text-muted-foreground" onClick={() => onChange(rules.filter((_, j) => j !== n))}><XIcon /></Button>
          </div>
          <Params w={r.when} onChange={(w) => set(n, { ...r, when: w })} />
        </div>))}</Group>}
      {rules.length < 20 && <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => onChange([...rules, { when: { kind: "mention_or_dm" }, act: self ? "draft" : "reply" }])} data-add-rule><PlusIcon />添加规则</Button>}
      <div className="px-1 type-footnote text-tertiary">按顺序第一条命中的生效；都不命中就只记录。</div>
    </div>
  )
}
