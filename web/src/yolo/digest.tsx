/* The YOLO record (#/yolo, modules/yolo.md §8): what Steve decided for you, by each time YOLO was on, then by item.
 * Each row says what was asked, what YOLO chose (or why it left it to you), and offers the undo it can do now
 * (撤销 / 回滚 / 撤回 / 改答 / 暂停这件). No boxes around lists: a line per decision, a grey glyph for its outcome,
 * the commands it ran folded to one line. #/yolo/<window> opens at that window (the end-of-YOLO notice links here). */
import { useEffect, useRef, useState } from "react"
import { CheckIcon, ClockIcon, RotateCcwIcon, XIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { NotWired } from "@/api"
import { yoloApi, type RYoloAction, type RYoloDigest, type RYoloWindow, type UndoKind } from "@/api/yolo"
import { tm, useHubEvents, yoloUndo } from "@/store"
import { Chips, Empty, ItemNo, Page } from "@/ui"
import { takeDigestStart, YoloPill } from "@/yolo/switch"
import { choiceText, YoloMark } from "@/yolo/badges"
import { t as tr, useT } from "@/i18n"
import { date, num } from "@/i18n/format"

const undoLabel = (k: UndoKind) => tr(`yolo:undo.${k}`)
const ENDED = new Set(["off", "tripped"])
const endedLabel = (why: string) => (ENDED.has(why) ? tr(`yolo:ended.${why as "off" | "tripped"}`) : why)
const FILTERS = ["all", "decided", "declined"] as const
type Filter = (typeof FILTERS)[number]
const filterLabel = (f: Filter) => tr(`yolo:filter.${f}`)

const when = (ms: number) => date(ms, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })

/** the outcome as a small grey glyph (orange only for a card still waiting for you) */
function Glyph({ a }: { a: RYoloAction }) {
  const c = "size-3.5"
  const waiting = a.outcome === "declined" && a.decision_status === "open"
  return (
    <span className="mt-[3px] flex size-4 shrink-0 items-center justify-center text-muted-foreground" aria-hidden>
      {a.outcome === "decided" ? <CheckIcon className={c} strokeWidth={2} />
        : a.outcome === "scheduled" ? <ClockIcon className={cn(c, "text-blue")} strokeWidth={2} />
        : a.outcome === "failed" ? <XIcon className={cn(c, "text-destructive")} strokeWidth={2} />
        : <span className={cn("size-1.5 rounded-full", waiting ? "bg-attention" : "bg-tertiary/70")} />}
    </span>
  )
}

function ActionRow({ a, reload }: { a: RYoloAction; reload: () => void }) {
  const t = useT()
  const [re, setRe] = useState<string | null>(null), [busy, setBusy] = useState(false), [open, setOpen] = useState(false)
  const undo = async (text?: string) => { setBusy(true); try { await yoloUndo(a.id, text); setRe(null); reload() } finally { setBusy(false) } }
  const cmds: string[] = Array.isArray(a.checks?.commands) ? a.checks.commands : []
  const checks: { name: string; passed: boolean }[] = Array.isArray(a.checks?.checks) ? a.checks.checks : []
  const what = a.outcome === "decided" ? t("yolo:digest.chose", { choice: choiceText(a.choice, a.subject_type) })
    : a.outcome === "scheduled" ? t("yolo:digest.willChoose", { choice: choiceText(a.choice, a.subject_type) })
    : a.outcome === "declined" ? (a.decision_status === "open" ? t("yolo:digest.leftWaiting") : t("yolo:digest.left"))
    : a.outcome === "muted" ? t("yolo:digest.muted") : a.outcome === "cancelled" ? t("yolo:digest.cancelled") : a.outcome === "failed" ? t("yolo:digest.failed") : undefined
  const cmd = cmds.join("\n"), long = cmd.length > 90 || cmds.length > 1
  return (
    <li className={cn("flex gap-2.5 py-2.5", a.undone && "opacity-50")} data-yolo-row={a.id} data-outcome={a.outcome} data-undone={a.undone ? "1" : undefined}>
      <Glyph a={a} />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-start gap-3">
          <div className="line-clamp-2 min-w-0 flex-1 break-words type-callout">{a.question ?? a.rule}</div>
          {a.can_undo && re === null && <Button size="xs" variant="ghost" className="-my-0.5 shrink-0 text-muted-foreground" disabled={busy}
            onClick={() => (a.can_undo === "reanswer" ? setRe("") : void undo())} data-yolo-undo={a.can_undo}><RotateCcwIcon />{undoLabel(a.can_undo)}</Button>}
        </div>
        <div className="mt-0.5 flex min-w-0 flex-wrap gap-x-1.5 type-footnote text-muted-foreground">
          <span className={cn(a.outcome === "decided" && "text-foreground/80", a.outcome === "failed" && "text-destructive")}>{what}</span>
          {a.reason && <><span className="text-tertiary">·</span><span className="min-w-0 break-words">{a.reason}</span></>}
          <span className="text-tertiary">·</span><span className="tabular-nums">{tm(a.done_at ?? a.created)}</span>
          {a.undone ? <><span className="text-tertiary">·</span><span>{t("yolo:digest.undone")}</span></> : null}
        </div>
        {checks.length > 0 && <div className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[11.5px] text-muted-foreground">{checks.map((c, n) => <span key={n}><span className={c.passed ? "text-success" : "text-destructive"}>{c.passed ? "✓" : "✗"}</span> {c.name}</span>)}</div>}
        {cmds.length > 0 && <button type="button" onClick={() => long && setOpen(!open)} className={cn("mt-1 block w-full rounded-md bg-muted/60 px-2 py-1 text-left font-mono text-[11.5px] text-foreground/75", open ? "whitespace-pre-wrap break-all" : "truncate", !long && "cursor-text")} data-yolo-cmd>{cmd}</button>}
        {re !== null && <form className="mt-1.5 flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (re.trim()) void undo(re.trim()) }}>
          <Input autoFocus value={re} onChange={(e) => setRe(e.target.value)} placeholder={t("yolo:digest.reanswer")} className="h-8 rounded-lg text-[13px]" data-yolo-reanswer />
          <Button size="sm" type="submit" disabled={busy || !re.trim()}>{t("yolo:digest.send")}</Button><Button size="sm" variant="ghost" type="button" onClick={() => setRe(null)}>{t("common:action.cancel")}</Button>
        </form>}
      </div>
    </li>
  )
}

const keep = (f: Filter) => (a: RYoloAction) => f === "all" || (f === "decided" ? a.outcome === "decided" || a.outcome === "scheduled" : a.outcome === "declined")

function WindowBlock({ w, open, reload, filter }: { w: RYoloWindow; open: boolean; reload: () => void; filter: Filter }) {
  const t = useT()
  const ref = useRef<HTMLElement>(null)
  useEffect(() => { if (open) ref.current?.scrollIntoView({ block: "start" }) }, [open])
  const decided = w.counts.decided ?? 0, declined = w.counts.declined ?? 0
  const groups = w.items.map((g) => ({ ...g, actions: g.actions.filter(keep(filter)) })).filter((g) => g.actions.length)
  const stat = (n: number | string, l: string, test?: string) => <span className="flex items-baseline gap-1" data-stat={test}><b className="font-semibold tabular-nums text-foreground">{n}</b>{l}</span>
  return (
    <section ref={ref} className={cn("scroll-mt-4", open && "-ml-3 border-l-2 border-blue/50 pl-2.5")} data-yolo-window={w.id}>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <h2 className="type-headline">{w.ended ? t("common:time.range", { from: when(w.started), to: when(w.ended) }) : t("yolo:digest.since", { time: when(w.started) })}</h2>
        <span className="type-footnote text-muted-foreground">{w.ended ? (w.ended_why ? endedLabel(w.ended_why) : "") : t("yolo:digest.live")}</span>
      </div>
      <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground" data-yolo-summary>
        {stat(num(decided), t("yolo:digest.decided", { count: decided }), "decided")}
        {stat(num(declined), t("yolo:digest.declined", { count: declined }), "declined")}
        {w.releases > 0 && stat(num(w.releases), t("yolo:digest.releases", { count: w.releases }))}
        {w.cost != null && stat("$" + num(w.cost, { minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: false }), t("yolo:digest.cost"))}
        {w.waiting ? <a href="#/inbox" className="text-blue hover:underline">{t("yolo:digest.waiting", { count: w.waiting })}</a> : null}
      </div>
      {w.summary && <div className="mt-1 type-footnote text-muted-foreground">{t("yolo:digest.summary", { summary: w.summary })}</div>}
      {groups.length === 0 ? <div className="mt-3 type-footnote text-tertiary">{w.items.length ? t("yolo:digest.emptyFilter") : t("yolo:digest.emptyWindow")}</div> :
        <div className="mt-5 flex flex-col gap-5">{groups.map((g) => (
          <div key={g.item_id ?? "none"}>
            <div className="flex min-w-0 items-center gap-2 border-b pb-1.5">
              {g.item_id ? <a href={"#/items/" + g.item_id} className="flex min-w-0 items-center gap-2 hover:underline"><ItemNo id={g.item_id} /><span className="truncate text-[14px] font-medium">{g.title ?? g.item_id}</span></a> : <span className="text-[14px] font-medium">{t("yolo:digest.noItem")}</span>}
              <span className="ml-auto shrink-0 type-footnote tabular-nums text-tertiary">{g.actions.length}</span>
            </div>
            <ul className="flex flex-col divide-y divide-border/60">{g.actions.map((a) => <ActionRow key={a.id} a={a} reload={reload} />)}</ul>
          </div>))}</div>}
    </section>
  )
}

export function YoloDigestView({ window }: { window?: string }) {
  const t = useT()
  const [d, setD] = useState<RYoloDigest | null>(null), [err, setErr] = useState(""), [filter, setFilter] = useState<Filter>(takeDigestStart)
  const load = () => yoloApi.actions().then((x) => { setD(x); setErr("") }).catch((e) => setErr(e instanceof NotWired ? tr("yolo:digest.noYolo") : (e as Error).message))
  useEffect(() => { void load() }, [])
  useHubEvents(["yolo", "yolo_action"], () => void load())
  return (
    <Page title={t("yolo:digest.title")} sub={t("yolo:digest.sub")} max={760}
      toolbar={<><YoloPill /><Chips value={filterLabel(filter)} onChange={(v) => setFilter(FILTERS.find((f) => filterLabel(f) === v) ?? "all")} options={FILTERS.map(filterLabel)} /></>}>
      {err ? <Empty title={err} /> : !d ? <div className="flex justify-center py-12"><Spinner className="size-5 text-muted-foreground" /></div>
        : d.windows.length === 0 ? <Empty title={t("yolo:digest.never")} sub={t("yolo:digest.neverSub")} icon={<YoloMark />} />
        : <div className="flex flex-col gap-10">{d.windows.map((w) => <WindowBlock key={w.id} w={w} open={w.id === window} reload={() => void load()} filter={filter} />)}</div>}
    </Page>
  )
}
