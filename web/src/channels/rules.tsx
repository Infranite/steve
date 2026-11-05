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
import { t, useT } from "@/i18n"

type Kind = When["kind"]
/** condition labels; your own account reads “@你 / 私聊你” */
export function whenLabel(k: Kind, self: boolean) {
  if (self && (k === "mention_or_dm" || k === "mention" || k === "dm")) return t(`channels:rules.when.${k}Self`)
  return (WHEN_KEYS as readonly string[]).includes(k) ? t(`channels:rules.when.${k as (typeof WHEN_KEYS)[number]}`) : k
}
const WHEN_KEYS = ["mention_or_dm", "mention", "dm", "reply_to_me", "keyword", "intent", "member", "external", "hours", "all", "other_group"] as const
/* only messages that @ the account or are private chats with it are acted on (the Hub's gate); the conditions
 * below refine those. `other_group` can't hold for such a message: offered only to show an old rule */
const KINDS: Kind[] = ["mention_or_dm", "mention", "dm", "reply_to_me", "keyword", "intent", "member", "external", "hours", "all"]
const ACTS: Act[] = ["reply", "draft", "record", "create_item", "handoff"]
const actLabel = (a: Act) => t(`channels:rules.act.${a}`)
const DAYS = ["d1", "d2", "d3", "d4", "d5", "d6", "d7"] as const

/** one rule in words: “被 @ 或私聊时直接回复” */
export function ruleText(r: Rule, self: boolean) {
  const w = r.when
  const cond = w.kind === "keyword" ? t("channels:rules.keywordCond", { words: w.words.join(t("channels:rules.wordSep")) }) : w.kind === "intent" ? t("channels:rules.intentCond", { text: w.text })
    : w.kind === "hours" ? t("channels:rules.hoursCond", { side: t(w.outside ? "channels:rules.outside" : "channels:rules.inside"), start: w.start, end: w.end })
    : w.kind === "member" ? t("channels:rules.memberCond", { count: w.parties.length + w.teams.length }) : whenLabel(w.kind, self)
  const act = r.act === "create_item" ? t(r.mode === "propose" ? "channels:rules.act.propose" : "channels:rules.act.createStart") : actLabel(r.act)
  return t("channels:rules.sentence", { cond, act })
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
  const t = useT()
  const [text, setText] = useState("")
  const add = () => { const v = text.trim(); if (v && !words.includes(v) && words.length < 20) onChange([...words, v]); setText("") }
  return (
    <span className="flex flex-wrap items-center gap-1">{words.map((w) => (
      <span key={w} className="inline-flex h-6 items-center gap-1 rounded-full bg-muted pl-2.5 pr-1 text-[12px]">{w}
        <button type="button" aria-label={t("channels:rules.removeWord", { word: w })} onClick={() => onChange(words.filter((x) => x !== w))} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>))}
      <Input aria-label={t("channels:rules.addWordAria")} className="h-7 w-28 rounded-full text-[12px]" placeholder={t("channels:rules.addWord")} maxLength={40} value={text} onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); add() } }} onBlur={add} data-rule-word /></span>
  )
}

function Members({ w, onChange }: { w: Extract<When, { kind: "member" }>; onChange: (w: When) => void }) {
  const t = useT()
  const [q, setQ] = useState(""), [xs, setXs] = useState<PartyOption[]>([]), [names, setNames] = useState<Record<string, string>>({})
  useEffect(() => {
    const s = q.trim(); if (!s) { setXs([]); return }
    let live = true
    const h = setTimeout(() => people.search(s).then((r) => live && setXs([...r.contacts, ...r.teams]), () => live && setXs([])), 150)
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
        <button type="button" aria-label={t("common:action.remove")} onClick={() => onChange(k === "p" ? { ...w, parties: w.parties.filter((x) => x !== id) } : { ...w, teams: w.teams.filter((x) => x !== id) })} className="flex size-4 items-center justify-center rounded-full hover:bg-background"><XIcon className="size-3" /></button></span>))}
      <Input aria-label={t("channels:rules.searchAria")} className="h-7 w-36 rounded-full text-[12px]" placeholder={t("channels:rules.search")} value={q} onChange={(e) => setQ(e.target.value)} />
      {xs.length > 0 && <span role="listbox" className="absolute left-0 top-8 z-20 flex max-h-56 w-64 flex-col overflow-auto rounded-lg border bg-popover p-1 shadow-md">{xs.slice(0, 8).map((p) => (
        <button key={p.id} type="button" role="option" aria-selected={false} className="rounded-md px-2 py-1.5 text-left type-callout hover:bg-accent" onClick={() => pick(p)}>{p.name}<span className="ml-1.5 type-footnote text-muted-foreground">{p.type === "team" ? t("channels:rules.team") : p.sub}</span></button>))}</span>}
    </span>
  )
}

function Params({ w, onChange }: { w: When; onChange: (w: When) => void }) {
  const t = useT()
  if (w.kind === "keyword") return <Words words={w.words} onChange={(words) => onChange({ ...w, words })} />
  if (w.kind === "intent") return <Input aria-label={t("channels:rules.intentAria")} className="h-8 w-full rounded-lg text-[13px] sm:w-80" maxLength={200} placeholder={t("channels:rules.intentPlaceholder")} value={w.text} onChange={(e) => onChange({ ...w, text: e.target.value })} data-rule-intent />
  if (w.kind === "member") return <Members w={w} onChange={onChange} />
  if (w.kind === "hours") return (
    <span className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground">
      <Select testId="hours-side" value={w.outside ? "out" : "in"} onChange={(v) => onChange({ ...w, outside: v === "out" })} options={[{ v: "out", l: t("channels:rules.outside") }, { v: "in", l: t("channels:rules.inside") }]} />
      <Input aria-label={t("channels:rules.start")} type="time" className="h-8 w-28 rounded-lg" value={w.start} onChange={(e) => onChange({ ...w, start: e.target.value })} />–
      <Input aria-label={t("channels:rules.end")} type="time" className="h-8 w-28 rounded-lg" value={w.end} onChange={(e) => onChange({ ...w, end: e.target.value })} />
      <span className="inline-flex gap-0.5" role="group" aria-label={t("channels:rules.workdays")}>{DAYS.map((d, n) => { const on = w.days.includes(n + 1); return (
        <button key={d} type="button" aria-pressed={on} onClick={() => onChange({ ...w, days: on ? w.days.filter((x) => x !== n + 1) : [...w.days, n + 1].sort() })}
          className={on ? "size-6 rounded-full bg-foreground text-[11px] text-background" : "size-6 rounded-full bg-muted text-[11px] text-muted-foreground"}>{t(`channels:rules.day.${d}`)}</button>) })}</span>
    </span>
  )
  return null
}

/** the editor; `self` = your own account (no 直接回复: only drafts, trigger words are the exception) */
export function Rules({ rules, onChange, self = false, note }: { rules: Rule[]; onChange: (r: Rule[]) => void; self?: boolean; note?: string }) {
  const t = useT()
  const set = (n: number, r: Rule) => onChange(rules.map((x, i) => (i === n ? r : x)))
  const acts = ACTS.filter((a) => !self || a !== "reply")
  return (
    <div className="flex flex-col gap-2" data-rules>
      {note && <div className="px-1 type-footnote text-muted-foreground">{note}</div>}
      {rules.length > 0 && <Group>{rules.map((r, n) => (
        <div key={n} className="flex flex-col gap-2 px-2 py-2" data-rule={n}>
          <div className="flex flex-wrap items-center gap-2">
            <span className="type-footnote text-muted-foreground">{t("channels:rules.whenWord")}</span>
            <Select testId={"when-" + n} value={r.when.kind} onChange={(v) => set(n, { ...r, when: fresh(v as Kind, r.when) })} options={(KINDS.includes(r.when.kind) ? KINDS : [...KINDS, r.when.kind]).map((k) => ({ v: k, l: whenLabel(k, self) }))} />
            <span className="type-footnote text-muted-foreground">{t("channels:rules.thenWord")}</span>
            <Select testId={"act-" + n} value={r.act} onChange={(v) => set(n, { when: r.when, act: v as Act, ...(v === "create_item" ? { mode: r.mode ?? "start" } : {}) })} options={acts.map((a) => ({ v: a, l: actLabel(a) }))} />
            {r.act === "create_item" && <Select testId={"mode-" + n} value={r.mode ?? "start"} onChange={(v) => set(n, { ...r, mode: v as "start" | "propose" })} options={[{ v: "start", l: t("channels:rules.modeStart") }, { v: "propose", l: t("channels:rules.modePropose") }]} />}
            <span className="flex-1" />
            <Button variant="ghost" size="icon-xs" aria-label={t("channels:rules.remove", { n: n + 1 })} className="text-muted-foreground" onClick={() => onChange(rules.filter((_, j) => j !== n))}><XIcon /></Button>
          </div>
          <Params w={r.when} onChange={(w) => set(n, { ...r, when: w })} />
        </div>))}</Group>}
      {rules.length < 20 && <Button size="xs" variant="ghost" className="self-start text-muted-foreground" onClick={() => onChange([...rules, { when: { kind: "mention_or_dm" }, act: self ? "draft" : "reply" }])} data-add-rule><PlusIcon />{t("channels:rules.add")}</Button>}
      <div className="px-1 type-footnote text-tertiary">{t("channels:rules.footer")}</div>
    </div>
  )
}
