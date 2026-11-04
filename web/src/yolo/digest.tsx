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

const UNDO: Record<UndoKind, string> = { decision: "撤销", rollback: "回滚", recall: "撤回消息", reanswer: "改答", pause: "暂停这件" }
const ENDED: Record<string, string> = { off: "你关掉的", tripped: "熔断暂停" }
const FILTERS = ["全部", "替你做的", "留给你的"] as const
type Filter = (typeof FILTERS)[number]

const when = (ms: number) => new Date(ms).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })

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
  const [re, setRe] = useState<string | null>(null), [busy, setBusy] = useState(false), [open, setOpen] = useState(false)
  const undo = async (text?: string) => { setBusy(true); try { await yoloUndo(a.id, text); setRe(null); reload() } finally { setBusy(false) } }
  const cmds: string[] = Array.isArray(a.checks?.commands) ? a.checks.commands : []
  const checks: { name: string; passed: boolean }[] = Array.isArray(a.checks?.checks) ? a.checks.checks : []
  const what = a.outcome === "decided" ? `选了「${choiceText(a.choice, a.subject_type)}」`
    : a.outcome === "scheduled" ? `到点会选「${choiceText(a.choice, a.subject_type)}」`
    : a.outcome === "declined" ? (a.decision_status === "open" ? "留给你，还在等" : "留给你")
    : { muted: "只记录", cancelled: "取消了", failed: "执行失败" }[a.outcome as "muted" | "cancelled" | "failed"]
  const cmd = cmds.join("\n"), long = cmd.length > 90 || cmds.length > 1
  return (
    <li className={cn("flex gap-2.5 py-2.5", a.undone && "opacity-50")} data-yolo-row={a.id} data-outcome={a.outcome} data-undone={a.undone ? "1" : undefined}>
      <Glyph a={a} />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-start gap-3">
          <div className="line-clamp-2 min-w-0 flex-1 break-words type-callout">{a.question ?? a.rule}</div>
          {a.can_undo && re === null && <Button size="xs" variant="ghost" className="-my-0.5 shrink-0 text-muted-foreground" disabled={busy}
            onClick={() => (a.can_undo === "reanswer" ? setRe("") : void undo())} data-yolo-undo={a.can_undo}><RotateCcwIcon />{UNDO[a.can_undo]}</Button>}
        </div>
        <div className="mt-0.5 flex min-w-0 flex-wrap gap-x-1.5 type-footnote text-muted-foreground">
          <span className={cn(a.outcome === "decided" && "text-foreground/80", a.outcome === "failed" && "text-destructive")}>{what}</span>
          {a.reason && <><span className="text-tertiary">·</span><span className="min-w-0 break-words">{a.reason}</span></>}
          <span className="text-tertiary">·</span><span className="tabular-nums">{tm(a.done_at ?? a.created)}</span>
          {a.undone ? <><span className="text-tertiary">·</span><span>已撤销</span></> : null}
        </div>
        {checks.length > 0 && <div className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[11.5px] text-muted-foreground">{checks.map((c, n) => <span key={n}><span className={c.passed ? "text-success" : "text-destructive"}>{c.passed ? "✓" : "✗"}</span> {c.name}</span>)}</div>}
        {cmds.length > 0 && <button type="button" onClick={() => long && setOpen(!open)} className={cn("mt-1 block w-full rounded-md bg-muted/60 px-2 py-1 text-left font-mono text-[11.5px] text-foreground/75", open ? "whitespace-pre-wrap break-all" : "truncate", !long && "cursor-text")} data-yolo-cmd>{cmd}</button>}
        {re !== null && <form className="mt-1.5 flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (re.trim()) void undo(re.trim()) }}>
          <Input autoFocus value={re} onChange={(e) => setRe(e.target.value)} placeholder="你的答案，会作为更正发给它" className="h-8 rounded-lg text-[13px]" data-yolo-reanswer />
          <Button size="sm" type="submit" disabled={busy || !re.trim()}>发送</Button><Button size="sm" variant="ghost" type="button" onClick={() => setRe(null)}>取消</Button>
        </form>}
      </div>
    </li>
  )
}

const keep = (f: Filter) => (a: RYoloAction) => f === "全部" || (f === "替你做的" ? a.outcome === "decided" || a.outcome === "scheduled" : a.outcome === "declined")

function WindowBlock({ w, open, reload, filter }: { w: RYoloWindow; open: boolean; reload: () => void; filter: Filter }) {
  const ref = useRef<HTMLElement>(null)
  useEffect(() => { if (open) ref.current?.scrollIntoView({ block: "start" }) }, [open])
  const decided = w.counts.decided ?? 0, declined = w.counts.declined ?? 0
  const groups = w.items.map((g) => ({ ...g, actions: g.actions.filter(keep(filter)) })).filter((g) => g.actions.length)
  const stat = (n: number | string, l: string, test?: string) => <span className="flex items-baseline gap-1" data-stat={test}><b className="font-semibold tabular-nums text-foreground">{n}</b>{l}</span>
  return (
    <section ref={ref} className={cn("scroll-mt-4", open && "-ml-3 border-l-2 border-blue/50 pl-2.5")} data-yolo-window={w.id}>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <h2 className="type-headline">{w.ended ? `${when(w.started)} – ${when(w.ended)}` : `${when(w.started)} 起`}</h2>
        <span className="type-footnote text-muted-foreground">{w.ended ? (w.ended_why ? ENDED[w.ended_why] ?? w.ended_why : "") : "进行中"}</span>
      </div>
      <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground" data-yolo-summary>
        {stat(decided, "件替你做了", "decided")}
        {stat(declined, "件留给你", "declined")}
        {w.releases > 0 && stat(w.releases, "次发布")}
        {w.cost != null && stat(`$${w.cost.toFixed(2)}`, "相关费用")}
        {w.waiting ? <a href="#/inbox" className="text-blue hover:underline">{w.waiting} 张卡等你 →</a> : null}
      </div>
      {w.summary && <div className="mt-1 type-footnote text-muted-foreground">其中：{w.summary}</div>}
      {groups.length === 0 ? <div className="mt-3 type-footnote text-tertiary">{w.items.length ? "这一类没有记录。" : "这段时间还没有要决定的事。"}</div> :
        <div className="mt-5 flex flex-col gap-5">{groups.map((g) => (
          <div key={g.item_id ?? "none"}>
            <div className="flex min-w-0 items-center gap-2 border-b pb-1.5">
              {g.item_id ? <a href={"#/items/" + g.item_id} className="flex min-w-0 items-center gap-2 hover:underline"><ItemNo id={g.item_id} /><span className="truncate text-[14px] font-medium">{g.title ?? g.item_id}</span></a> : <span className="text-[14px] font-medium">不属于事项的卡</span>}
              <span className="ml-auto shrink-0 type-footnote tabular-nums text-tertiary">{g.actions.length}</span>
            </div>
            <ul className="flex flex-col divide-y divide-border/60">{g.actions.map((a) => <ActionRow key={a.id} a={a} reload={reload} />)}</ul>
          </div>))}</div>}
    </section>
  )
}

export function YoloDigestView({ window }: { window?: string }) {
  const [d, setD] = useState<RYoloDigest | null>(null), [err, setErr] = useState(""), [filter, setFilter] = useState<Filter>(takeDigestStart)
  const load = () => yoloApi.actions().then((x) => { setD(x); setErr("") }).catch((e) => setErr(e instanceof NotWired ? "这台 Hub 没有 YOLO" : (e as Error).message))
  useEffect(() => { void load() }, [])
  useHubEvents(["yolo", "yolo_action"], () => void load())
  return (
    <Page title="YOLO 记录" sub="你不在时 Steve 替你做的每个决定，按每次打开分段。能撤销的可以撤；1 小时内撤销 2 次，YOLO 会暂停。" max={760}
      toolbar={<><YoloPill /><Chips value={filter} onChange={setFilter} options={FILTERS} /></>}>
      {err ? <Empty title={err} /> : !d ? <div className="flex justify-center py-12"><Spinner className="size-5 text-muted-foreground" /></div>
        : d.windows.length === 0 ? <Empty title="还没开过 YOLO" sub="打开后，Steve 替你做的每个决定都会记在这里。" icon={<YoloMark />} />
        : <div className="flex flex-col gap-10">{d.windows.map((w) => <WindowBlock key={w.id} w={w} open={w.id === window} reload={() => void load()} filter={filter} />)}</div>}
    </Page>
  )
}
