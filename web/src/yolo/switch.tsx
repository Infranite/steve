/* The YOLO control (modules/yolo.md §8; 完全放开 added 2026-11-02): one place to choose how much Steve may do while
 * you are away — 关 / YOLO 替你审批 / 完全放开 — with a line on what each means. Switching on shows what YOLO would
 * do with the cards open now; full access asks you to confirm in plain words what it gives away. While on: counts
 * and the record; paused by the breaker: why and 恢复; forced off on this Hub: why, nothing to press.
 * The status shows as a quiet dot + label (sidebar, toolbars), never as a coloured pill. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { yoloApi, type RYolo, type RYoloPreview, type YoloLevel } from "@/api/yolo"
import { st, yoloResume, yoloSet } from "@/store"
import { closeSheet, go, openSheet } from "@/ui"
import { choiceText } from "@/yolo/badges"

/** the hard limits, verbatim from the spec */
export const NEVER = "花钱、以你的名义发消息或邮件（除非带 [AI] 标识并通过披露检查）、推到 GitHub 等远端、删工作区外的数据、可能泄露的内容，YOLO 永远不替你做。"
/** what full access gives away, and what still holds — said before it is switched on */
export const FULL_GIVES = "Agent 会以你的身份运行任何命令、修改任何文件，不再先问你。"
export const FULL_KEEPS = "仍然守住：以你的名义发消息（只发带 [AI] 标识并通过披露检查的）、付款和银行卡、高敏感保险箱条目、Steve 自己的发布。Agent 也会被告知：不碰 ~/.steve 和 8440 端口，事项没要求就不推送，只按 PID 停自己启动的进程。"

export type YoloState = "off" | "on" | "full" | "paused" | "forced"
export function yoloState(y: RYolo | null): YoloState | null {
  if (!y) return null
  if (y.forced_off) return "forced"
  if (y.paused) return "paused"
  if (y.full || y.mode === "full") return "full"
  return y.on ? "on" : "off"
}
export const levelOf = (y: RYolo | null): YoloLevel => (!y || !y.on ? "off" : y.full || y.mode === "full" ? "full" : "on")

/** the dot's colour: blue for YOLO, orange for full access, red only for a tripped breaker */
export const DOT: Record<YoloState, string> = { on: "bg-blue", full: "bg-attention", off: "bg-tertiary/60", paused: "bg-destructive", forced: "bg-tertiary/60" }
export const LABEL: Record<YoloState, string> = { on: "YOLO", full: "完全放开", off: "YOLO 关", paused: "YOLO 已暂停", forced: "YOLO 不可用" }

export const LEVELS: { m: YoloLevel; title: string; sub: string }[] = [
  { m: "off", title: "关", sub: "每个决定都等你。" },
  { m: "on", title: "YOLO 替你审批", sub: "Steve 替你做能撤回的决定：工作区里的命令、检查全过的交付、Agent 的提问。每件都记账，能一键撤销。" },
  { m: "full", title: "完全放开", sub: "在 YOLO 之上，Agent 不再请求权限，想做什么就直接做。只在你信得过这段时间的所有事项时打开。" },
]

/** the YOLO record opened on one of its filters (统计页的“替你做了 / 留给你”下钻到这里) */
let digestStart: "全部" | "替你做的" | "留给你的" = "全部"
export const openDigest = (f: typeof digestStart) => { digestStart = f; go("#/yolo") }
export const takeDigestStart = () => { const f = digestStart; digestStart = "全部"; return f }

export function Dot({ s, className }: { s: YoloState; className?: string }) {
  return <span className={cn("size-1.5 shrink-0 rounded-full", DOT[s], className)} aria-hidden />
}

/** the status: a dot and a word, with what it did; opens the control */
export function YoloPill({ className, short = false }: { className?: string; short?: boolean }) {
  const y = st.yolo, s = yoloState(y)
  if (!y || !s) return null
  const n = s === "on" || s === "full" ? y.counts.decided : 0
  return (
    <button type="button" onClick={() => openSheet("yolo")} aria-label={`YOLO 设置：${LABEL[s]}`} data-yolo-pill={s}
      className={cn("inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-[12.5px] transition-colors hover:bg-muted",
        s === "off" || s === "forced" ? "text-muted-foreground" : "text-foreground/85", className)}>
      <Dot s={s} /><span className="font-medium">{LABEL[s]}</span>
      {!short && n > 0 && <span className="tabular-nums text-muted-foreground">· 代办 {n}</span>}
    </button>
  )
}

function LevelRow({ m, title, sub, on, current, disabled, pick }: { m: YoloLevel; title: string; sub: string; on: boolean; current: boolean; disabled: boolean; pick: () => void }) {
  return (
    <button type="button" role="radio" aria-checked={on} disabled={disabled} onClick={pick} data-yolo-level={m}
      className={cn("flex w-full items-start gap-3 rounded-xl border px-3.5 py-3 text-left transition-colors disabled:opacity-50",
        on ? (m === "full" ? "border-attention/60 bg-attention/5" : "border-foreground/25 bg-muted/50") : "border-transparent hover:bg-muted/50")}>
      <span className={cn("mt-[3px] flex size-4 shrink-0 items-center justify-center rounded-full border", on ? (m === "full" ? "border-attention" : "border-foreground") : "border-border")} aria-hidden>
        {on && <span className={cn("size-2 rounded-full", m === "full" ? "bg-attention" : "bg-foreground")} />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2 type-headline">{title}{current && <span className="type-footnote font-normal text-muted-foreground">现在</span>}</span>
        <span className="mt-0.5 block type-footnote text-muted-foreground">{sub}</span>
      </span>
    </button>
  )
}

/** the control (sheet kind `yolo`) */
export function YoloDialog() {
  const y = st.yolo, s = yoloState(y), cur = levelOf(y)
  const [sel, setSel] = useState<YoloLevel>(cur)
  const [pv, setPv] = useState<RYoloPreview | null>(null), [err, setErr] = useState(""), [all, setAll] = useState(false), [busy, setBusy] = useState(false), [ok, setOk] = useState(false)
  const turningOn = cur === "off" && sel !== "off" && s !== "forced" && s !== "paused"
  useEffect(() => {
    if (!turningOn || pv) return
    yoloApi.preview().then(setPv).catch((e) => setErr((e as Error).message))
  }, [turningOn, pv])
  if (!y || !s) return null
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  const n = pv?.would_handle.length ?? 0
  const locked = s === "forced" || s === "paused"
  const action = sel === cur ? null : sel === "off" ? "关闭" : sel === "on" ? (cur === "full" ? "退回 YOLO" : "打开 YOLO") : "打开完全放开"
  const needsOk = sel === "full" && cur !== "full"
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="max-h-[92dvh] gap-4 overflow-y-auto rounded-2xl sm:max-w-[520px]" data-yolo-dialog={s}>
        <DialogHeader>
          <DialogTitle className="type-headline">你不在时，Steve 能做多少</DialogTitle>
          <DialogDescription className="type-callout">一直生效，直到你改回来。事项和项目可以单独设置。</DialogDescription>
        </DialogHeader>
        {s === "forced" && <div className="rounded-xl bg-muted/60 px-3.5 py-3 type-callout text-muted-foreground" data-yolo-forced>这台 Hub 上用不了：{y.forced_reason}</div>}
        {s === "paused" && <div role="alert" className="flex items-start gap-2.5 rounded-xl border border-destructive/40 px-3.5 py-3 type-callout" data-yolo-paused>
          <Dot s="paused" className="mt-[7px]" />
          <div className="min-w-0 flex-1"><div className="font-medium">YOLO 已暂停</div><div className="mt-0.5 text-muted-foreground">{y.paused?.reason}。恢复后计数清零，接着处理新来的卡。</div></div>
          <Button size="sm" disabled={busy} onClick={() => void run(async () => { await yoloResume(); closeSheet() })} data-yolo-resume>恢复</Button></div>}
        <div role="radiogroup" aria-label="YOLO 模式" className="-mx-1.5 flex flex-col gap-1">
          {LEVELS.map((l) => <LevelRow key={l.m} {...l} on={sel === l.m} current={cur === l.m && !locked} disabled={locked && l.m !== "off"} pick={() => { setSel(l.m); setOk(false) }} />)}
        </div>
        {(cur === "on" || cur === "full") && !locked && <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground" data-yolo-on>
          <span>这次已替你做 <b className="font-medium text-foreground">{y.counts.decided}</b> 件，留给你 <b className="font-medium text-foreground">{y.counts.declined}</b> 件{y.counts.scheduled ? <>，<b className="font-medium text-foreground">{y.counts.scheduled}</b> 件在冷静期</> : null}</span>
          <span>·</span><button type="button" className="text-blue hover:underline" onClick={() => { closeSheet(); go("#/yolo") }} data-yolo-digest-link>查看记录</button></div>}
        {turningOn && <div className="flex flex-col gap-2" data-yolo-preview>
          {!pv && !err && <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3.5" />看看现在待处理的卡…</div>}
          {err && <div className="type-footnote text-destructive">{err}</div>}
          {pv && <>
            <div className="type-footnote text-muted-foreground">现在待处理的卡里，YOLO 能处理 <b className="font-medium text-foreground">{n}</b> 张，留给你 <b className="font-medium text-foreground">{pv.keep_for_you}</b> 张{pv.hard_limited ? `（${pv.hard_limited} 张碰到硬限制）` : ""}。</div>
            {n > 0 && <ul className="flex max-h-32 flex-col gap-1 overflow-auto" data-yolo-would>
              {pv.would_handle.slice(0, 6).map((w) => <li key={w.decision_id} className="flex min-w-0 items-baseline gap-2 type-footnote"><span className="min-w-0 flex-1 truncate">{w.question}</span><span className="shrink-0 text-muted-foreground">「{choiceText(w.choice, w.rule.startsWith("release") ? "release" : undefined)}」{w.delay_s ? ` · ${Math.round(w.delay_s / 60)} 分钟后` : ""}</span></li>)}
            </ul>}
            {n > 0 && <label className="flex items-center gap-2 type-footnote"><Checkbox checked={all} onCheckedChange={(v) => setAll(!!v)} data-yolo-include-open />也处理这 {n} 张（不勾只处理之后新来的）</label>}
          </>}
        </div>}
        {needsOk && <div className="flex flex-col gap-2 rounded-xl bg-attention/8 px-3.5 py-3" data-yolo-full-confirm>
          <div className="type-callout font-medium">{FULL_GIVES}</div>
          <div className="type-footnote text-muted-foreground">{FULL_KEEPS}</div>
          <label className="mt-1 flex items-start gap-2 type-callout"><Checkbox className="mt-0.5" checked={ok} onCheckedChange={(v) => setOk(!!v)} data-yolo-full-ok />我明白，由我负责这段时间里 Agent 做的事</label>
        </div>}
        {s !== "forced" && !needsOk && <div className="type-footnote text-tertiary" data-yolo-never>{NEVER}</div>}
        <DialogFooter className="gap-2">
          <Button variant="ghost" onClick={closeSheet}>{action ? "取消" : "完成"}</Button>
          {action && <Button disabled={busy || (needsOk && !ok) || (turningOn && !pv && !err)} data-yolo-apply={sel}
            className={cn(sel === "full" && "bg-attention text-white hover:bg-attention/90")} variant={sel === "off" ? "outline" : "default"}
            onClick={() => void run(async () => { await yoloSet(sel, turningOn && all); closeSheet() })}>{action}</Button>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
