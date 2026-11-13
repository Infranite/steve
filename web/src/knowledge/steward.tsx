/* 整理: the knowledge steward. It is an ordinary Bot (the 知识管家 template) on an ordinary schedule whose runs
 * use the memory tools (steve_memory_list / steve_memory_tidy); the knowledge page only sets
 * the two up (and keeps which schedule it is, `/kb/steward`), switches the schedule on and off, picks how often it
 * runs, and lists what each run changed — merges to confirm in the usual 待确认 flow, conflicts to answer, and
 * everything else with Undo. */
import { useCallback, useEffect, useState, type ReactNode } from "react"
import { ArchiveIcon, CheckIcon, GitMergeIcon, LinkIcon, PlayIcon, ScaleIcon, SparklesIcon, TagIcon, TriangleAlertIcon, Undo2Icon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Card, Panel, Section, Empty, Meta, Pill, Tile, Notice, Loading, BotAvatar } from "@/ui"
import { Select } from "@/view-shared"
import { when } from "@/auto/common"
import { useHubEvents, toast, tm, st, botName, makeBot, saveBot } from "@/store"
import { flowApi, errText, type Schedule } from "@/api/flow"
import { kb, isGone, type Change, type ChangeRun, type GoneMem, type Mem } from "@/api/knowledge"
import { errMsg } from "@/knowledge/fields"
import { aboutName, MetaLink } from "@/knowledge/origin"
import { t as tt, useT } from "@/i18n"

/** how often it runs: the Hub's rule parser reads these Chinese phrases, the UI shows its own words */
const FREQ = [["daily", "每天 03:00"], ["weekly", "每周日 03:00"], ["monthly", "每月 1 日 03:00"]] as const
type Freq = (typeof FREQ)[number][0]
/** one run's budget: a tidy is a short agent run */
const BUDGET_USD = 5

/** the steward's Bot text, the same as the Bot template in 新建 Bot (manage:tpl.knowledge*) */
export const stewardTemplate = () => ({ name: tt("manage:tpl.knowledgeName"), role: tt("manage:tpl.knowledgeRole"), rule: tt("manage:tpl.knowledgeRule") })

/** sets the steward up: its Bot (from the template, unless one by that name is there) and its schedule */
async function setUp(): Promise<Schedule | null> {
  const x = stewardTemplate()
  let bot = Object.keys(st.bots).find((k) => st.bots[k].n === x.name && st.bots[k].on !== false)
  if (!bot) { bot = await makeBot(x.name, x.role); if (bot) await saveBot(bot, { rule: x.rule }, "") }
  if (!bot) return null
  const s = await flowApi.newSchedule({ name: tt("knowledge:steward.scheduleName"), when: FREQ[1][1], what: tt("knowledge:steward.what"), bot, mode: "item_auto", budget_usd: BUDGET_USD, missed: "catch_up_once", overlap: "skip", enabled: true })
  await kb.setSteward(s.id)
  return s
}

export function StewardTab() {
  const t = useT()
  const [s, setS] = useState<Schedule | null | undefined>(undefined), [sErr, setSErr] = useState("")
  const [runs, setRuns] = useState<ChangeRun[] | null>(null), [err, setErr] = useState(""), [busy, setBusy] = useState("")
  /* the schedule the page set up; gone (deleted on the 自动化 page) is the same as never set up */
  const loadS = useCallback(() => Promise.all([kb.steward(), flowApi.schedules()]).then(([m, xs]) => setS(xs.find((x) => x.id === m.schedule_id) ?? null), (e) => setSErr(errText(e))), [])
  const loadRuns = useCallback(() => kb.changes().then(setRuns, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void loadS(); void loadRuns() }, [loadS, loadRuns])
  useHubEvents(["mm"], () => void loadRuns())
  useHubEvents(["flow", "schedule", "item"], () => void loadS(), 800)
  const act = async (key: string, f: () => Promise<unknown>, ok?: string) => {
    if (busy) return
    setBusy(key)
    try { await f(); if (ok) toast(ok) } catch (e) { toast(errText(e)) } finally { setBusy(""); void loadS(); void loadRuns() }
  }
  const toggle = (on: boolean) => act("on", async () => {
    if (s) await flowApi.patchSchedule(s.id, { enabled: on })
    else if (on) { const n = await setUp(); if (n) setS(n) }
  }, on ? t("knowledge:steward.turnedOn") : t("knowledge:steward.turnedOff"))
  const freq: Freq | "" = FREQ.find(([, w]) => w === s?.when)?.[0] ?? ""
  return (
    <div className="flex flex-col gap-6">
      <Card className="flex flex-col gap-3" data-steward={s ? (s.enabled ? "on" : "off") : "none"}>
        <div className="flex items-start gap-3">
          <Tile I={SparklesIcon} />
          <div className="min-w-0 flex-1">
            <div className="type-body font-medium">{t("knowledge:steward.title")}</div>
            <p className="mt-0.5 type-callout text-muted-foreground">{t("knowledge:steward.desc")}</p>
          </div>
          {s !== undefined && !sErr && <Switch checked={!!s?.enabled} disabled={!!busy} onCheckedChange={(on) => void toggle(on)} aria-label={t("knowledge:steward.switchAria")} data-steward-switch />}
        </div>
        {sErr ? <Notice tone="attention">{t("knowledge:steward.noSchedules", { error: sErr })}</Notice>
          : s === undefined ? <div className="flex justify-center py-2"><Spinner className="size-4 text-muted-foreground" /></div>
          : s && <div className="flex flex-col gap-3 border-t pt-3">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
              <Select testId="steward-freq" label={t("knowledge:steward.freqAria")} value={freq || "custom"} disabled={!!busy}
                options={[...FREQ.map(([k]) => ({ v: k, l: t(`knowledge:steward.freq.${k}`) })), ...(freq ? [] : [{ v: "custom", l: s.describe || s.when }])]}
                onChange={(v) => { const w = FREQ.find(([k]) => k === v)?.[1]; if (w) void act("freq", () => flowApi.patchSchedule(s.id, { when: w }), t("knowledge:steward.freqChanged")) }} />
              <Meta className="min-w-0 flex-1 max-sm:order-last max-sm:basis-full">
                <span>{s.enabled ? (s.next_at ? t("knowledge:steward.next", { time: when(s.next_at) }) : t("knowledge:steward.onPlan")) : t("knowledge:steward.paused")}</span>
                <MetaLink href={"#/bots/" + s.bot} data-steward-bot><span className="inline-flex items-center gap-1"><BotAvatar k={s.bot} size={14} />{t("knowledge:steward.by", { name: botName(s.bot) })}</span></MetaLink>
                {s.last && <MetaLink href={s.last.item_id ? "#/items/" + s.last.item_id : undefined} data-steward-last>{s.last.status === "failed" ? t("knowledge:steward.lastFailed", { time: tm(s.last.at) }) : t("knowledge:steward.last", { time: tm(s.last.at) })}</MetaLink>}
              </Meta>
              <Button size="sm" variant="outline" disabled={!!busy} onClick={() => void act("run", () => flowApi.runSchedule(s.id), t("knowledge:steward.started"))} data-steward-run>{busy === "run" ? <Spinner className="size-3.5" /> : <PlayIcon />}{t("knowledge:steward.runNow")}</Button>
            </div>
          </div>}
      </Card>
      {err ? <Empty title={t("knowledge:steward.failed")} sub={err} /> : !runs ? <Loading className="py-8" /> : !runs.length
        ? <Empty title={t("knowledge:steward.noneTitle")} sub={s ? t("knowledge:steward.noneSub") : t("knowledge:steward.noneSubOff")} data-steward-empty />
        : runs.map((r) => <RunReport key={r.run_id + r.writer} r={r} busy={busy} act={act} />)}
      <div className="-mt-2 px-1 type-footnote text-muted-foreground">{t("knowledge:steward.footer")}</div>
    </div>
  )
}

const quote = (m: Mem | GoneMem) => (isGone(m) ? tt("knowledge:steward.gone") : m.text)
/** a memory's text, opening it on its tab */
function MemText({ m, className }: { m: Mem | GoneMem; className?: string }) {
  if (isGone(m)) return <span className="text-muted-foreground line-through">{tt("knowledge:steward.gone")}</span>
  return <a href={"#/knowledge?mem=" + encodeURIComponent(m.id)} className={"break-words underline-offset-2 hover:underline " + (className ?? "")} data-change-mem={m.id}>{m.text}</a>
}

const OP_ICON = { merge: GitMergeIcon, resolve: ScaleIcon, flag: TriangleAlertIcon, stale: ArchiveIcon, about: TagIcon, source: LinkIcon }
type Act = (key: string, f: () => Promise<unknown>, ok?: string) => Promise<void>

function RunReport({ r, busy, act }: { r: ChangeRun; busy: string; act: Act }) {
  const t = useT()
  const open = r.changes.filter((c) => c.state === "pending" || c.state === "open").length
  return (
    <Section data-steward-run-report={r.run_id}
      label={<span className="inline-flex min-w-0 items-center gap-1.5"><MetaLink href={r.href}>{r.title ? `${r.run_id} ${r.title}` : r.run_id}</MetaLink><span aria-hidden>·</span><span>{tm(r.at)}</span></span>}
      right={<span className="type-footnote text-muted-foreground">{open ? t("knowledge:steward.waiting", { count: open }) : t("knowledge:steward.changes", { count: r.changes.length })}</span>}>
      <Panel>{r.changes.map((c) => <ChangeRow key={c.id} c={c} busy={busy} act={act} />)}</Panel>
    </Section>
  )
}

/** one change: what it did, why, and what you can do about it */
function ChangeRow({ c, busy, act }: { c: Change; busy: string; act: Act }) {
  const t = useT()
  const I = OP_ICON[c.op] ?? SparklesIcon, b = !!busy
  const undo = (ok = t("knowledge:steward.undone")) => act(c.id, () => kb.undoChange(c.id), ok)
  const UndoBtn = ({ label = t("knowledge:panel.undo") }: { label?: string }) => <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={b} onClick={() => void undo()} data-change-undo={c.id}><Undo2Icon />{label}</Button>
  const done = c.state === "undone" ? <Pill data-change-state="undone">{t("knowledge:steward.state.undone")}</Pill> : null
  let what: ReactNode, more: ReactNode = null, actions: ReactNode = null
  const m = c.mem
  switch (c.op) {
    case "merge":
      what = <>{t("knowledge:steward.op.merge", { count: c.targets.length })}<span className="text-foreground">“<MemText m={m} />”</span></>
      more = <ul className="flex flex-col gap-0.5">{c.targets.map((x) => <li key={x.id} className="break-words text-muted-foreground">– {quote(x)}</li>)}</ul>
      actions = c.state === "pending" ? <>
        <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={b} onClick={() => void undo(t("knowledge:steward.notMerged"))} data-change-ignore={c.id}>{t("knowledge:steward.dontMerge")}</Button>
        <Button size="xs" variant="outline" disabled={b} onClick={() => void act(c.id, () => kb.resolve([m.id], "yes"), t("knowledge:steward.merged"))} data-change-confirm={c.id}><CheckIcon />{t("knowledge:steward.confirmMerge")}</Button>
      </> : c.state === "confirmed" ? <><Pill tone="success">{t("knowledge:steward.state.merged")}</Pill><UndoBtn /></> : done ?? <Pill>{t("knowledge:steward.state.notMerged")}</Pill>
      break
    case "resolve":
      what = <>{t("knowledge:steward.op.resolve")}<span className="text-foreground">“<MemText m={m} />”</span></>
      more = <ul className="flex flex-col gap-0.5">{c.targets.map((x) => <li key={x.id} className="break-words text-muted-foreground line-through decoration-muted-foreground/40">{quote(x)}</li>)}</ul>
      actions = done ?? <UndoBtn />
      break
    case "flag": {
      const all = [m, ...c.targets]
      what = t("knowledge:steward.op.flag", { count: all.length })
      const kept = c.state === "kept"
      more = <ul className="flex flex-col gap-1">{all.map((x) => (
        <li key={x.id} className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="min-w-0 flex-1 break-words text-foreground/90">{isGone(x) ? quote(x) : <MemText m={x} />}{!isGone(x) && x.status === "stale" && <span className="ml-1.5 text-muted-foreground">{t("knowledge:steward.staleMark")}</span>}</span>
          {c.state === "open" && !isGone(x) && <Button size="xs" variant="outline" disabled={b} onClick={() => void act(c.id, () => kb.keepChange(c.id, x.id), t("knowledge:steward.keptOne"))} data-change-keep={x.id}>{t("knowledge:steward.keepThis")}</Button>}
        </li>))}</ul>
      actions = c.state === "open" ? <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={b} onClick={() => void act(c.id, () => kb.keepChange(c.id, null), t("knowledge:steward.keptAll"))} data-change-keep-all={c.id}>{t("knowledge:steward.keepAll")}</Button>
        : <><Pill tone={kept ? "success" : "neutral"}>{kept ? t("knowledge:steward.state.kept") : t("knowledge:steward.state.allKept")}</Pill><UndoBtn label={t("knowledge:steward.reopen")} /></>
      break
    }
    case "stale":
      what = <>{t("knowledge:steward.op.stale")}<span className="text-foreground">“<MemText m={m} />”</span></>
      actions = done ?? <UndoBtn />
      break
    case "about": {
      const a = (c.after as { about?: { type: string; id: string } }).about
      const name = !isGone(m) ? m.about.find((x) => x.type === a?.type && x.id === a?.id) : undefined
      what = <>{t("knowledge:steward.op.about", { name: name ? aboutName(name) : a?.id ?? "" })}<span className="text-foreground">“<MemText m={m} />”</span></>
      actions = done ?? <UndoBtn />
      break
    }
    default: {
      const label = String((c.after as { label?: string }).label ?? "")
      what = <>{t("knowledge:steward.op.source", { source: label })}<span className="text-foreground">“<MemText m={m} />”</span></>
      actions = done ?? <UndoBtn />
    }
  }
  return (
    <div className="flex items-start gap-3 px-3 py-3" data-change={c.id} data-change-op={c.op} data-change-state={c.state}>
      <Tile I={I} />
      <div className="flex min-w-0 flex-1 flex-col gap-1.5">
        <div className="type-body text-muted-foreground">{what}</div>
        {more && <div className="type-callout">{more}</div>}
        {c.reason && <div className="type-footnote text-muted-foreground" data-change-reason>{t("knowledge:steward.reason", { reason: c.reason })}</div>}
        <div className="flex flex-wrap items-center justify-end gap-1 max-sm:justify-start">{actions}</div>
      </div>
    </div>
  )
}
