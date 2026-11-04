/* YOLO 的运行状态一行（ST-40，取代页面顶部的横栏）：权限模式、这次替你做了几件 / 留给你几件，以及“YOLO 记录 / 调整 / 恢复”。
 * 放在统计页概览的最上面；熔断暂停时待处理页顶上也有一条（有事等你恢复）。侧栏的 YOLO 圆点照旧。 */
import { cn } from "cn"
import { st, yoloResume } from "@/store"
import { openSheet } from "@/ui"
import { Dot, LABEL, openDigest, yoloState } from "@/yolo/switch"

const link = "font-medium text-foreground/80 underline-offset-2 hover:text-foreground hover:underline"

/** 运行状态: what the top bar used to say — the permission mode, what YOLO did and left for you — with its doors */
export function RunState({ className }: { className?: string }) {
  const y = st.yolo, s = yoloState(y)
  if (!y || !s) return null
  const what = { on: "能撤回的决定由 Steve 替你做", full: "Agent 不再请求权限", off: "每个决定都等你", paused: y.paused?.reason ?? "", forced: y.forced_reason ?? "这台 Hub 不允许" }[s]
  const live = s === "on" || s === "full"
  const n = (v: number, label: string, f: "替你做的" | "留给你的" | "全部", k: string) => (
    <button type="button" className="inline-flex items-baseline gap-1 rounded-sm hover:text-foreground" onClick={() => openDigest(f)} data-run-count={k}>
      {label}<span className="font-medium tabular-nums text-foreground">{v}</span>
    </button>
  )
  return (
    <div role={s === "paused" ? "alert" : "status"} className={cn("flex flex-wrap items-center gap-x-3 gap-y-1 type-footnote text-muted-foreground", className)} data-run-state={s}>
      <span className="inline-flex min-w-0 items-center gap-2"><Dot s={s} className="size-2" /><span className="font-medium text-foreground">{LABEL[s]}</span><span className="min-w-0 truncate">{what}</span></span>
      {live && <span className="inline-flex flex-wrap items-baseline gap-x-3" data-run-counts>
        {n(y.counts.decided, "这次替你做了", "替你做的", "decided")}
        {n(y.counts.declined, "留给你", "留给你的", "declined")}
        {y.counts.scheduled > 0 && n(y.counts.scheduled, "冷静期", "全部", "scheduled")}
      </span>}
      <span className="flex-1" />
      <span className="inline-flex items-center gap-3">
        <a href="#/yolo" className={link} data-run-digest>YOLO 记录</a>
        {s !== "forced" && <button type="button" className={link} onClick={() => openSheet("yolo")} data-run-adjust>调整</button>}
        {s === "paused" && <button type="button" className={link} onClick={() => void yoloResume()} data-run-resume>恢复</button>}
      </span>
    </div>
  )
}
