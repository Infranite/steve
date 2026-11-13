/* A Bot's page, the parts beyond its settings form (manage.tsx holds the page and the other tabs):
 *  - 概览: who it is, what it did in the last 30 days (GET /stats/summary?bot=), its items (running first, then the
 *    latest closed), and its capabilities and key settings, each leading to the tab where it is changed; a Bot with no
 *    history gets a short "start configuring" guide instead of the empty figures.
 *  - 系统提示词: the generated prompt or one of your own, with "tell Steve how to change it": the Hub drafts a new
 *    version of the working rules (generated mode) or of your own prompt (POST /config/draft with `field` and `base`),
 *    shown as a before/after diff, and saved with the Bot update API only when you confirm. */
import { useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ArrowUpIcon, CheckIcon, ChevronRightIcon, SparklesIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { TextArea } from "@/components/affect/text-area-clean"
import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { useT } from "@/i18n"
import { api } from "@/api"
import { stats } from "@/api/stats"
import { fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { st, closedSt, saveBot, toast, type Item } from "@/store"
import { Section, Panel, Card, Note, Segmented, Loading } from "@/ui"
import { ItemRowExport as ItemRow } from "@/view-shared"
import { foldedDiff, diffCounts } from "@/lib/line-diff"

export type BotTab = "概览" | "设置" | "对外身份" | "能力" | "系统提示词" | "定时与流程" | "记忆"
/** how many Steve tools every Bot has (manage.tsx's STEVE_TOOLS) */
export type BotLabels = { pro: string; perm: string; acc: string; effort: string | null; steveTools: number }

/** a fact about the Bot: its label, its value, and the tab where it is changed */
function Fact({ label, children, onClick, testId }: { label: string; children: ReactNode; onClick: () => void; testId: string }) {
  return (
    <button type="button" onClick={onClick} data-bot-fact={testId}
      className="group/fact flex w-full min-w-0 items-start gap-3 rounded-lg px-3 py-2.5 text-left transition-colors duration-120 ease-(--ease-out) hover:bg-accent active:bg-accent/70 max-sm:flex-col max-sm:gap-0.5">
      <span className="w-28 shrink-0 pt-px type-callout text-muted-foreground">{label}</span>
      <span className="flex min-w-0 flex-1 items-start gap-2 self-stretch">
        <span className="min-w-0 flex-1 type-body text-foreground">{children}</span>
        <ChevronRightIcon className="mt-1 size-4 shrink-0 text-tertiary opacity-60 transition-opacity group-hover/fact:opacity-100 max-sm:hidden" aria-hidden />
      </span>
    </button>
  )
}
/** the harness and model a Bot runs on, each with its mark */
export function RunsOn({ k, className, effort }: { k: string; className?: string; effort?: string | null }) {
  const t = useT()
  const b = st.bots[k]
  if (!b) return null
  return (
    <span className={cn("inline-flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1", className)} data-runs-on>
      <span className="inline-flex min-w-0 items-center gap-1.5"><HarnessMark harness={b.harness} />{b.h}</span>
      <span className="inline-flex min-w-0 items-center gap-1.5"><ModelMark model={b.model} />{b.model ? <span className="truncate type-code">{b.model}</span> : <span className="text-muted-foreground">{t("manage:ov.defaultModel")}</span>}</span>
      {effort && <span className="text-muted-foreground">{t("manage:ov.effort", { v: effort })}</span>}
    </span>
  )
}
/** one figure of the last 30 days */
function Figure({ label, value, sub, testId }: { label: string; value: string; sub?: string; testId: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 px-4 py-3" data-bot-figure={testId}>
      <span className="type-footnote text-muted-foreground">{label}</span>
      <span className={cn("type-title tabular-nums", value === "—" && "text-tertiary")}>{value}</span>
      {sub && <span className="truncate type-footnote text-tertiary">{sub}</span>}
    </div>
  )
}
const RECENT = 6
/** its items: the open ones (newest change first), then the latest closed ones up to six in all */
export function recentItems(k: string, items: Item[]) {
  const mine = items.filter((i) => i.b === k && !i.arch)
  const open = mine.filter((i) => !closedSt(i.st)).sort((a, b) => b.updated - a.updated)
  const closed = mine.filter((i) => closedSt(i.st)).sort((a, b) => b.updated - a.updated)
  return { open, list: [...open, ...closed.slice(0, Math.max(0, RECENT - open.length))], any: mine.length > 0 }
}

export function BotOverview({ k, go, labels }: { k: string; go: (tab: BotTab) => void; labels: BotLabels }) {
  const t = useT()
  const b = st.bots[k]
  const [d] = useStats(() => stats.summary({ range: "30d", bot: k }), [k])
  if (!b) return null
  const s = d.s === "ok" ? d.v : null
  const { open, list, any } = recentItems(k, st.items)
  /* a Bot with no history: nothing it was given, no turn in the statistics */
  const fresh = !any && (d.s === "absent" || (!!s && s.agents.turns === 0 && s.throughput.done === 0))
  const promptText = b.prompt == null ? t("manage:ov.promptAuto") : t("manage:ov.promptCustom", { count: b.prompt.length })
  return (
    <div className="flex flex-col gap-8 anim-in" data-bot-overview={fresh ? "fresh" : "active"}>
      {fresh && <Guide k={k} go={go} />}

      <Section label={t("manage:ov.who")}>
        <Panel>
            <Fact label={t("manage:form.role")} onClick={() => go("设置")} testId="role">{b.role || <span className="text-muted-foreground">{t("manage:detail.noRole")}</span>}</Fact>
            <Fact label={t("manage:ov.runsOn")} onClick={() => go("设置")} testId="runs-on"><RunsOn k={k} effort={labels.effort} /></Fact>
            <Fact label={t("manage:form.voice")} onClick={() => go("设置")} testId="voice"><span className="line-clamp-3">{b.pers || <span className="text-muted-foreground">{t("manage:detail.unset")}</span>}</span></Fact>
            <Fact label={t("manage:form.pro")} onClick={() => go("设置")} testId="pro">{labels.pro}</Fact>
        </Panel>
      </Section>

      {!fresh && (
        <Section label={t("manage:ov.recent")} right={<a href="#/stats" className="type-footnote font-normal text-muted-foreground hover:text-foreground" data-bot-stats-open>{t("manage:ov.openStats")}</a>}
          note={d.s === "absent" ? t("manage:ov.statsAbsent") : undefined}>
          {d.s === "loading" ? <Loading className="py-8" /> : s && (
            <Card className="grid grid-cols-2 gap-0 p-1 sm:grid-cols-3" data-bot-figures>
              <Figure testId="turns" label={t("manage:ov.turns")} value={fmtN(s.agents.turns)} sub={s.agents.tools ? t("manage:ov.toolCalls", { n: fmtN(s.agents.tools) }) : undefined} />
              <Figure testId="done" label={t("manage:ov.done")} value={fmtN(s.throughput.done)} sub={s.throughput.wip ? t("manage:ov.wip", { n: s.throughput.wip }) : undefined} />
              <Figure testId="first-pass" label={t("manage:ov.firstPass")} value={fmtPct(s.quality.first_pass)} sub={s.quality.first_pass_n ? t("manage:ov.ofDelivered", { n: s.quality.first_pass_n }) : undefined} />
              <Figure testId="avg-time" label={t("manage:ov.avgTime")} value={s.time.lead.avg == null ? "—" : fmtMs(s.time.lead.avg)} sub={s.time.lead.p50 == null ? undefined : t("manage:ov.median", { v: fmtMs(s.time.lead.p50) })} />
              <Figure testId="avg-cost" label={t("manage:ov.avgCost")} value={s.per_item.est_avg == null ? "—" : fmtUsd(s.per_item.est_avg)} sub={s.spend.est ? t("manage:ov.total", { v: fmtUsd(s.spend.est) }) : undefined} />
              <Figure testId="cache" label={t("manage:ov.cacheHit")} value={fmtPct(s.usage.hit_rate)} sub={s.usage.total ? t("manage:ov.tokens", { n: fmtN(s.usage.total) }) : undefined} />
            </Card>
          )}
          {d.s === "error" && <Note className="mt-0">{d.msg}</Note>}
        </Section>
      )}

      {!fresh && (
        <Section label={t("manage:ov.items")} right={open.length > 0 ? <span className="type-footnote font-normal text-muted-foreground">{t("manage:ov.running", { count: open.length })}</span> : undefined}>
          {list.length ? <Panel data-bot-items>{list.map((i) => <ItemRow key={i.id} i={i} bot={false} />)}</Panel>
            : <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground">{t("manage:ov.noItems")}</div>}
        </Section>
      )}

      <Section label={t("manage:ov.setup")}>
        <Panel>
            <Fact label={t("manage:ov.caps")} onClick={() => go("能力")} testId="caps">{s?.agents.tools ? t("manage:ov.capsUsed", { n: labels.steveTools, calls: fmtN(s.agents.tools) }) : t("manage:ov.capsSum", { n: labels.steveTools })}</Fact>
            <Fact label={t("manage:ov.perm")} onClick={() => go("设置")} testId="perm">{labels.perm}</Fact>
            <Fact label={t("manage:ov.acc")} onClick={() => go("设置")} testId="acc">{labels.acc}</Fact>
            <Fact label={t("manage:detail.tabPrompt")} onClick={() => go("系统提示词")} testId="prompt">{promptText}</Fact>
            <Fact label={t("manage:detail.tabMemory")} onClick={() => go("记忆")} testId="memory">{t("manage:ov.memCount", { count: b.memCount })}</Fact>
        </Panel>
      </Section>
    </div>
  )
}

/** the first steps for a Bot that has done nothing yet: its role, what it runs on, what it may do */
function Guide({ k, go }: { k: string; go: (tab: BotTab) => void }) {
  const t = useT()
  const b = st.bots[k]
  const steps: { id: string; done: boolean; title: string; sub: ReactNode; tab: BotTab }[] = [
    { id: "role", done: !!b.role.trim(), title: t("manage:ov.stepRole"), sub: b.role.trim() || t("manage:ov.stepRoleSub"), tab: "设置" },
    { id: "model", done: !!b.model, title: t("manage:ov.stepModel"), sub: b.model ? <RunsOn k={k} /> : t("manage:ov.stepModelSub", { harness: b.h }), tab: "设置" },
    { id: "caps", done: false, title: t("manage:ov.stepCaps"), sub: t("manage:ov.stepCapsSub"), tab: "能力" },
  ]
  return (
    <Section label={t("manage:ov.guideTitle")} note={t("manage:ov.guideSub")} data-bot-guide>
      <Panel>
          {steps.map((x, n) => (
            <button key={x.id} type="button" onClick={() => go(x.tab)} data-bot-step={x.id} data-done={x.done || undefined}
              className="group/step flex w-full min-w-0 items-center gap-3 rounded-lg px-3 py-3 text-left transition-colors duration-120 ease-(--ease-out) hover:bg-accent">
              <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-full type-footnote tabular-nums", x.done ? "bg-success/12 text-success" : "bg-muted text-muted-foreground")} aria-hidden>
                {x.done ? <CheckIcon className="size-3.5" strokeWidth={2.5} /> : n + 1}</span>
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className={cn("type-body", x.done && "text-muted-foreground")}>{x.title}</span>
                <span className="min-w-0 truncate type-footnote text-muted-foreground">{x.sub}</span>
              </span>
              <span className="shrink-0 type-footnote text-muted-foreground transition-colors group-hover/step:text-foreground">{x.done ? t("manage:ov.change") : x.tab === "设置" ? t("manage:ov.go") : t("manage:ov.open")}</span>
            </button>
          ))}
      </Panel>
    </Section>
  )
}

type Drafted = { id: string; field: "rules" | "prompt"; before: string; after: string }
/** the before/after of a drafted text, unchanged runs folded; applied only on confirm */
export function TextDiff({ before, after, className }: { before: string; after: string; className?: string }) {
  const t = useT()
  return (
    <div className={cn("max-h-[min(440px,60vh)] overflow-auto py-1.5 type-code", className)} data-text-diff>
      {foldedDiff(before, after).map((l, n) => l.k === "fold"
        ? <div key={n} className="px-4 py-1 type-footnote text-tertiary" data-diff="fold">{t("manage:prompt.fold", { count: l.n })}</div>
        : <div key={n} data-diff={l.k} className={cn("flex min-w-0 gap-3 px-4 whitespace-pre-wrap break-words", l.k === "add" && "bg-success/10 text-foreground", l.k === "del" && "bg-destructive/8 text-muted-foreground line-through decoration-destructive/40")}>
            <span className={cn("w-3 shrink-0 select-none text-center", l.k === "add" ? "text-success" : l.k === "del" ? "text-destructive" : "text-transparent")} aria-hidden>{l.k === "add" ? "+" : l.k === "del" ? "−" : " "}</span>
            <span className="min-w-0 flex-1">{l.text || " "}</span>
          </div>)}
    </div>
  )
}

/** "tell Steve how to change it": the drafting bar of the prompt tab */
function PromptAssistant({ k, field, base, onApplied, autoAsk }: { k: string; field: "rules" | "prompt"; base: string; onApplied: (text: string) => void; autoAsk?: { text: string; n: number } }) {
  const t = useT()
  const [v, setV] = useState(""), [busy, setBusy] = useState(false), [note, setNote] = useState(""), [dr, setDr] = useState<Drafted | null>(null), [saving, setSaving] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  const ask = async (text: string) => {
    const q = text.trim(); if (!q || busy) return
    setBusy(true); setNote(""); setDr(null)
    try {
      const r = await api.draft({ kind: "bot", id: k, field, base }, q)
      const after = r.patch?.[field]
      if (typeof after === "string" && after.trim() && after.trim() !== base.trim()) { setDr({ id: r.id, field, before: base, after }); setV("") }
      else { setNote(t("manage:prompt.noChange")); void api.dropConfig(r.id).catch(() => {}) }
    } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  /* "let Steve draft it" on an empty prompt asks once per press */
  const asked = useRef(0)
  useEffect(() => { if (autoAsk && autoAsk.n !== asked.current) { asked.current = autoAsk.n; void ask(autoAsk.text) } }, [autoAsk?.n]) // eslint-disable-line react-hooks/exhaustive-deps
  /* a draft is of the text it was asked about; once that text changes it no longer applies */
  useEffect(() => { if (dr && dr.before !== base) setDr(null) }, [base]) // eslint-disable-line react-hooks/exhaustive-deps
  const drop = () => { if (dr) void api.dropConfig(dr.id).catch(() => {}); setDr(null) }
  const apply = async () => {
    if (!dr) return
    setSaving(true)
    try {
      await saveBot(k, dr.field === "prompt" ? { prompt: dr.after } : { rule: dr.after }, dr.field === "prompt" ? t("manage:prompt.appliedPrompt") : t("manage:prompt.appliedRules"))
      void api.dropConfig(dr.id).catch(() => {})
      onApplied(dr.after); setDr(null)
    } finally { setSaving(false) }
  }
  const c = dr ? diffCounts(dr.before, dr.after) : null
  return (
    <div className="flex flex-col gap-3" data-prompt-assist={field}>
      <div className="flex items-center gap-2 rounded-xl bg-card py-1.5 pl-3.5 pr-1.5 shadow-[var(--lift)] transition-shadow duration-180 ease-(--ease-out) focus-within:shadow-[var(--lift-hover),0_0_0_3px_color-mix(in_srgb,var(--ring)_22%,transparent)]">
        <SparklesIcon className="size-4 shrink-0 text-blue" strokeWidth={1.75} aria-hidden />
        <input ref={input} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void ask(v) }}
          placeholder={t("manage:prompt.ask")} aria-label={t("manage:prompt.ask")} className="h-8 min-w-0 flex-1 bg-transparent type-body outline-none placeholder:text-tertiary" data-prompt-ask />
        <Button size="icon-sm" className="rounded-full" disabled={!v.trim() || busy} onClick={() => void ask(v)} aria-label={t("shell:config.askDraft")} data-prompt-ask-send>{busy ? <Spinner className="size-3.5" /> : <ArrowUpIcon className="size-4" />}</Button>
      </div>
      {!dr && !busy && !note && <div className="-mt-1 px-1 type-footnote text-muted-foreground">{field === "rules" ? t("manage:prompt.askHintAuto") : t("manage:prompt.askHintCustom")}</div>}
      {busy && <div className="px-1 type-footnote text-muted-foreground shimmer">{t("shell:config.drafting")}</div>}
      {note && <div className="px-1 type-footnote text-muted-foreground anim-in">{note}</div>}
      {dr && c && (
        <Card className="gap-0 overflow-hidden p-0 anim-in" data-prompt-draft={dr.field}>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 pb-2 pt-3 type-footnote text-muted-foreground">
            <SparklesIcon className="size-3.5" aria-hidden />
            <span className="text-foreground">{dr.field === "prompt" ? t("manage:prompt.draftedPrompt") : t("manage:prompt.draftedRules")}</span>
            <span className="ml-auto tabular-nums"><span className="text-success">+{c.add}</span> <span className="text-destructive">−{c.del}</span></span>
          </div>
          <TextDiff before={dr.before} after={dr.after} className="border-y" />
          <div className="flex items-center justify-end gap-2 px-3 py-2.5">
            <span className="mr-auto px-1 type-footnote text-muted-foreground">{t("manage:prompt.confirmHint")}</span>
            <Button variant="ghost" disabled={saving} onClick={drop} data-prompt-drop>{t("shell:config.drop")}</Button>
            <Button disabled={saving} onClick={() => void apply()} data-prompt-apply>{saving && <Spinner className="size-3.5" />}{t("shell:config.apply")}</Button>
          </div>
        </Card>
      )}
    </div>
  )
}

/** 系统提示词: generated or your own, and the drafting assistant above it */
export function BotPrompt({ k }: { k: string }) {
  const t = useT()
  const b = st.bots[k]
  const [prompt, setPrompt] = useState<{ generated: string; custom: string | null } | null>(null), [pdraft, setPdraft] = useState<string | null>(null)
  const [auto, setAuto] = useState<{ text: string; n: number } | undefined>(undefined)
  useEffect(() => { api.botPrompt(k).then(setPrompt).catch(() => {}) }, [k, b?.prompt, b?.memCount, b?.rule, b?.role, b?.pers, b?.pro])
  if (!b) return null
  const custom = b.prompt != null || pdraft != null
  const text = pdraft ?? b.prompt ?? ""
  return (
    <div className="flex flex-col gap-4 anim-in" data-prompt>
      <Segmented size="sm" className="self-start" value={custom ? "自己写" : "自动生成"} onChange={(v) => { if (v === "自己写") setPdraft(b.prompt ?? prompt?.generated ?? ""); else { setPdraft(null); if (b.prompt != null) void saveBot(k, { prompt: null }, t("manage:prompt.revertedToast")) } }} options={[["自动生成", t("manage:prompt.auto")], ["自己写", t("manage:prompt.custom")]] as const} />
      <PromptAssistant k={k} field={custom ? "prompt" : "rules"} base={custom ? text : b.rule} onApplied={() => { if (custom) setPdraft(null) }} autoAsk={auto} />
      {!custom
        ? <><pre className="whitespace-pre-wrap rounded-xl bg-muted/60 px-4 py-3.5 type-code text-foreground/90">{prompt?.generated ?? t("manage:prompt.loading")}</pre><Note className="mt-0">{t("manage:prompt.autoHint")}</Note></>
        : <>
          {!text.trim() && (
            <div className="flex flex-wrap items-center gap-3 rounded-xl bg-muted/60 px-4 py-3.5" data-prompt-empty>
              <div className="min-w-0 flex-1"><div className="type-body">{t("manage:prompt.empty")}</div><div className="type-footnote text-muted-foreground">{t("manage:prompt.draftItSub")}</div></div>
              <Button size="sm" variant="outline" onClick={() => setAuto((a) => ({ text: t("manage:prompt.draftItAsk"), n: (a?.n ?? 0) + 1 }))} data-prompt-draft-it><SparklesIcon />{t("manage:prompt.draftIt")}</Button>
            </div>
          )}
          <TextArea minRows={14} maxRows={40} inputClassName="type-code font-mono!" aria-label={t("manage:detail.tabPrompt")} value={text} onChange={(e) => setPdraft(e.target.value)} data-prompt-edit />
          <div className="flex items-center gap-2"><Button size="sm" disabled={pdraft == null || pdraft === b.prompt} onClick={() => { void saveBot(k, { prompt: pdraft ?? "" }); setPdraft(null) }}>{t("common:action.save")}</Button><span className="type-footnote text-muted-foreground">{t("manage:prompt.customHint")}</span></div>
        </>}
    </div>
  )
}
