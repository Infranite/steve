/* The YOLO control (modules/yolo.md §8; 完全放开 added 2026-11-02): one place to choose how much Steve may do while
 * you are away — 关 / YOLO 替你审批 / 完全放开 — with a line on what each means. Switching on shows what YOLO would
 * do with the cards open now; full access asks you to confirm in plain words what it gives away. The levels are an
 * affectUI Autonomy Dial: what each one hands over moves between "handled for you" and "asks you first". While on: counts
 * and the record; paused by the breaker: why and 恢复; forced off on this Hub: why, nothing to press.
 * The status shows as a quiet dot + label (sidebar, toolbars), never as a coloured pill. */

import { cn } from "cn"

import type { RYolo, YoloLevel } from "@/api/yolo"
import { st } from "@/store"
import { go, openSheet } from "@/ui"

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
  { m: "off", title: t("common:state.off"), sub: t("yolo:scope.offSub") },
  { m: "on", title: t("yolo:level.on"), sub: t("yolo:scope.onSub") },
  { m: "full", title: t("yolo:level.full"), sub: t("yolo:scope.fullSub") },
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
