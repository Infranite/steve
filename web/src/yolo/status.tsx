/* YOLO 的运行状态一行（ST-40，取代页面顶部的横栏）：权限模式、这次替你做了几件 / 留给你几件，以及“YOLO 记录 / 调整 / 恢复”。
 * 放在统计页概览的最上面；熔断暂停时待处理页顶上也有一条（有事等你恢复）。侧栏的 YOLO 圆点照旧。 */
import { cn } from "cn"
import { st, yoloResume } from "@/store"
import { openSheet } from "@/ui"
import { Dot, label, openDigest, yoloState } from "@/yolo/switch"
import { useT } from "@/i18n"

const link = "font-medium text-foreground/80 underline-offset-2 hover:text-foreground hover:underline"

/** 运行状态: what the top bar used to say — the permission mode, what YOLO did and left for you — with its doors */
export function RunState({ className }: { className?: string }) {
  const t = useT()
  const y = st.yolo, s = yoloState(y)
  if (!y || !s) return null
  const what = { on: t("yolo:run.on"), full: t("yolo:scope.fullSub"), off: t("yolo:scope.offSub"), paused: y.paused?.reason ?? "", forced: y.forced_reason ?? t("yolo:run.forced") }[s]
  const live = s === "on" || s === "full"
  const n = (v: number, text: string, f: "decided" | "declined" | "all", k: string) => (
    <button type="button" className="inline-flex items-baseline gap-1 rounded-sm hover:text-foreground" onClick={() => openDigest(f)} data-run-count={k}>
      {text}<span className="font-medium tabular-nums text-foreground">{v}</span>
    </button>
  )
  return (
    <div role={s === "paused" ? "alert" : "status"} className={cn("flex flex-wrap items-center gap-x-3 gap-y-1 type-footnote text-muted-foreground", className)} data-run-state={s}>
      <span className="inline-flex min-w-0 items-center gap-2"><Dot s={s} className="size-2 shrink-0" /><span className="shrink-0 whitespace-nowrap font-medium text-foreground">{label(s)}</span><span className="min-w-0 break-words">{what}</span></span>
      {live && <span className="inline-flex flex-wrap items-baseline gap-x-3" data-run-counts>
        {n(y.counts.decided, t("yolo:run.decided"), "decided", "decided")}
        {n(y.counts.declined, t("yolo:run.declined"), "declined", "declined")}
        {y.counts.scheduled > 0 && n(y.counts.scheduled, t("yolo:run.scheduled"), "all", "scheduled")}
      </span>}
      <span className="flex-1" />
      <span className="inline-flex items-center gap-3">
        <a href="#/yolo" className={link} data-run-digest>{t("yolo:digest.title")}</a>
        {s !== "forced" && <button type="button" className={link} onClick={() => openSheet("yolo")} data-run-adjust>{t("yolo:run.adjust")}</button>}
        {s === "paused" && <button type="button" className={link} onClick={() => void yoloResume()} data-run-resume>{t("yolo:run.resume")}</button>}
      </span>
    </div>
  )
}
