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
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"

/** the hard limits, verbatim from the spec */
export const never = () => t("yolo:switch.never")
/** what full access gives away, and what still holds — said before it is switched on */
export const fullGives = () => t("yolo:switch.fullGives")
export const fullKeeps = () => t("yolo:switch.fullKeeps")

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
export const label = (s: YoloState) => t(`yolo:label.${s}`)

export const levels = (): { m: YoloLevel; title: string; sub: string }[] => [
  { m: "off", title: t("common:state.off"), sub: t("yolo:switch.offSub") },
  { m: "on", title: t("yolo:level.on"), sub: t("yolo:switch.onSub") },
  { m: "full", title: t("yolo:level.full"), sub: t("yolo:switch.fullSub") },
]

/** the YOLO record opened on one of its filters (统计页的“替你做了 / 留给你”下钻到这里) */
let digestStart: "all" | "decided" | "declined" = "all"
export const openDigest = (f: typeof digestStart) => { digestStart = f; go("#/yolo") }
export const takeDigestStart = () => { const f = digestStart; digestStart = "all"; return f }

export function Dot({ s, className }: { s: YoloState; className?: string }) {
  return <span className={cn("size-1.5 shrink-0 rounded-full", DOT[s], className)} aria-hidden />
}

/** the status: a dot and a word, with what it did; opens the control */
export function YoloPill({ className, short = false }: { className?: string; short?: boolean }) {
  const t = useT()
  const y = st.yolo, s = yoloState(y)
  if (!y || !s) return null
  const n = s === "on" || s === "full" ? y.counts.decided : 0
  return (
    <button type="button" onClick={() => openSheet("yolo")} aria-label={t("yolo:switch.pillLabel", { label: label(s) })} data-yolo-pill={s}
      className={cn("inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2 type-callout hover:bg-accent",
        s === "off" || s === "forced" ? "text-muted-foreground" : "text-foreground/80", className)}>
      <Dot s={s} /><span className="font-medium">{label(s)}</span>
      {!short && n > 0 && <span className="tabular-nums text-muted-foreground">{t("yolo:switch.pillCount", { count: n })}</span>}
    </button>
  )
}

function LevelRow({ m, title, sub, on, current, disabled, pick }: { m: YoloLevel; title: string; sub: string; on: boolean; current: boolean; disabled: boolean; pick: () => void }) {
  const t = useT()
  return (
    <button type="button" role="radio" aria-checked={on} disabled={disabled} onClick={pick} data-yolo-level={m}
      className={cn("flex min-w-0 w-full items-start gap-3 rounded-xl px-4 py-3 text-left disabled:opacity-50",
        on ? (m === "full" ? "bg-attention/10 ring-1 ring-attention/40 ring-inset" : "bg-blue/10 ring-1 ring-blue/40 ring-inset") : "hover:bg-accent")}>
      <span className={cn("mt-[3px] flex size-4 shrink-0 items-center justify-center rounded-full border transition-colors duration-[180ms]", on ? (m === "full" ? "border-attention" : "border-blue") : "border-border")} aria-hidden>
        {on && <span className={cn("anim-in size-2 rounded-full", m === "full" ? "bg-attention" : "bg-blue")} />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-x-2 type-headline">{title}{current && <span className="type-footnote font-normal text-muted-foreground">{t("yolo:switch.current")}</span>}</span>
        <span className="mt-0.5 block type-callout text-muted-foreground">{sub}</span>
      </span>
    </button>
  )
}

/** the control (sheet kind `yolo`) */
export function YoloDialog() {
  const t = useT()
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
  const action = sel === cur ? null : sel === "off" ? t("yolo:switch.turnOff") : sel === "on" ? (cur === "full" ? t("yolo:switch.backToYolo") : t("yolo:switch.turnOnYolo")) : t("yolo:switch.turnOnFull")
  const needsOk = sel === "full" && cur !== "full"
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] grid-rows-[auto_minmax(0,1fr)_auto] gap-4 rounded-2xl p-4 [overflow-wrap:anywhere] sm:max-w-[520px] sm:p-6" data-yolo-dialog={s}>
        <DialogHeader className="min-w-0 pr-8">
          <DialogTitle className="type-title">{t("yolo:switch.title")}</DialogTitle>
          <DialogDescription className="type-callout">{t("yolo:switch.desc")}</DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 min-w-0 flex-col gap-4 overflow-y-auto p-1 *:shrink-0" data-yolo-scroll>
          {s === "forced" && <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground" data-yolo-forced>{t("yolo:switch.forced", { reason: y.forced_reason })}</div>}
          {s === "paused" && <div role="alert" className="flex flex-wrap items-start gap-2.5 rounded-xl bg-destructive/10 px-4 py-3 type-callout" data-yolo-paused>
            <Dot s="paused" className="mt-[7px]" />
            <div className="min-w-0 flex-1"><div className="font-medium">{label("paused")}</div><div className="mt-0.5 text-muted-foreground">{t("yolo:switch.pausedNote", { reason: y.paused?.reason })}</div></div>
            <Button size="sm" disabled={busy} onClick={() => void run(async () => { await yoloResume(); closeSheet() })} data-yolo-resume>{t("yolo:run.resume")}</Button></div>}
          <div role="radiogroup" aria-label={t("yolo:switch.modes")} className="flex min-w-0 shrink-0 flex-col gap-1">
            {levels().map((l) => <LevelRow key={l.m} {...l} on={sel === l.m} current={cur === l.m && !locked} disabled={locked && l.m !== "off"} pick={() => { setSel(l.m); setOk(false) }} />)}
          </div>
          {(cur === "on" || cur === "full") && !locked && <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground" data-yolo-on>
            <span><Trans t={t} i18nKey={y.counts.scheduled ? "yolo:switch.countsScheduled" : "yolo:switch.counts"} values={{ decided: y.counts.decided, declined: y.counts.declined, scheduled: y.counts.scheduled }} components={{ b: <b className="font-medium text-foreground" /> }} /></span>
            <span>·</span><button type="button" className="text-blue hover:underline" onClick={() => { closeSheet(); go("#/yolo") }} data-yolo-digest-link>{t("yolo:switch.viewRecord")}</button></div>}
          {turningOn && <div className="anim-in flex min-w-0 shrink-0 flex-col gap-2" data-yolo-preview>
            {!pv && !err && <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3.5" />{t("yolo:switch.previewLoading")}</div>}
            {err && <div className="type-footnote text-destructive">{err}</div>}
            {pv && <>
              <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey={pv.hard_limited ? "yolo:switch.previewHard" : "yolo:switch.preview"} values={{ n, keep: pv.keep_for_you, hard: pv.hard_limited }} components={{ b: <b className="font-medium text-foreground" /> }} /></div>
              {n > 0 && <ul className="flex min-w-0 flex-col gap-1" data-yolo-would>
                {pv.would_handle.slice(0, 6).map((w) => <li key={w.decision_id} className="flex min-w-0 flex-wrap items-baseline gap-x-2 type-footnote"><span className="min-w-0 max-w-full">{w.question}</span><span className="min-w-0 max-w-full text-muted-foreground">{t("auto:quoted", { text: choiceText(w.choice, w.rule.startsWith("release") ? "release" : undefined) })}{w.delay_s ? t("yolo:switch.delay", { when: t("yolo:badge.inMinutes", { count: Math.round(w.delay_s / 60) }) }) : ""}</span></li>)}
              </ul>}
              {n > 0 && <label className="flex items-start gap-2 type-footnote"><Checkbox checked={all} onCheckedChange={(v) => setAll(!!v)} data-yolo-include-open /><span className="min-w-0">{t("yolo:switch.includeOpen", { count: n })}</span></label>}
            </>}
          </div>}
          {needsOk && <div className="anim-in flex min-w-0 shrink-0 flex-col gap-2 rounded-xl bg-attention/10 px-4 py-3" data-yolo-full-confirm>
            <div className="type-callout font-medium">{fullGives()}</div>
            <div className="type-footnote text-muted-foreground">{fullKeeps()}</div>
            <label className="mt-1 flex items-start gap-2 type-callout"><Checkbox className="mt-0.5" checked={ok} onCheckedChange={(v) => setOk(!!v)} data-yolo-full-ok /><span className="min-w-0">{t("yolo:switch.fullOk")}</span></label>
          </div>}
          {s !== "forced" && !needsOk && <div className="type-footnote text-tertiary" data-yolo-never>{never()}</div>}
        </div>
        <DialogFooter className="min-w-0 gap-2 *:h-auto *:min-h-8 *:min-w-0 *:whitespace-normal">
          <Button variant="ghost" onClick={closeSheet}>{action ? t("common:action.cancel") : t("yolo:switch.done")}</Button>
          {action && <Button disabled={busy || (needsOk && !ok) || (turningOn && !pv && !err)} data-yolo-apply={sel}
            className={cn(sel === "full" && "bg-attention text-white hover:bg-attention/90")} variant={sel === "off" ? "outline" : "default"}
            onClick={() => void run(async () => { await yoloSet(sel, turningOn && all); closeSheet() })}>{action}</Button>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
