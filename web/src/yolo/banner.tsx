/* The YOLO status line (modules/yolo.md §8, quieter since 2026-11-02): one slim line above the page, grey text with a
 * coloured dot — orange while full access is on, red only when the breaker paused YOLO (with 恢复), grey when this Hub
 * forces it off. Plain YOLO shows here on phones only; on wider screens the sidebar already says it. Off: nothing. */
import { cn } from "cn"
import { st, yoloResume } from "@/store"
import { openSheet } from "@/ui"
import { Dot, yoloState } from "@/yolo/switch"

export function YoloBanner() {
  const y = st.yolo, s = yoloState(y)
  if (!y || !s || s === "off" || (s === "forced" && !y.on)) return null
  const link = "shrink-0 font-medium text-foreground/80 underline-offset-2 hover:text-foreground hover:underline"
  const counts = <span className="hidden shrink-0 tabular-nums sm:inline">代办 {y.counts.decided}{y.counts.declined > 0 && ` · 留给你 ${y.counts.declined}`}{y.counts.scheduled > 0 && ` · 冷静期 ${y.counts.scheduled}`}</span>
  return (
    <div role={s === "paused" ? "alert" : "status"} data-yolo-banner={s}
      className={cn("flex h-8 shrink-0 items-center gap-2 overflow-hidden whitespace-nowrap border-b px-4 text-[12px] text-muted-foreground sm:px-5", s === "on" && "md:hidden")}>
      <Dot s={s} />
      {s === "on" && <><span className="min-w-0 truncate text-foreground/80">YOLO 替你审批中</span>{counts}</>}
      {s === "full" && <><span className="min-w-0 truncate"><span className="text-foreground/80">完全放开</span><span className="max-sm:hidden"> · Agent 不再请求权限</span></span>{counts}</>}
      {s === "paused" && <span className="min-w-0 truncate"><span className="text-foreground/80">YOLO 已暂停：</span>{y.paused?.reason}</span>}
      {s === "forced" && <span className="min-w-0 truncate">YOLO 在这台 Hub 上不可用：{y.forced_reason}</span>}
      <span className="flex-1" />
      {(s === "on" || s === "full") && <a href="#/yolo" className={link} data-yolo-banner-digest>记录</a>}
      {s !== "forced" && <button type="button" className={link} onClick={() => openSheet("yolo")} data-yolo-banner-open>调整</button>}
      {s === "paused" && <button type="button" className={link} onClick={() => void yoloResume()} data-yolo-banner-resume>恢复</button>}
    </div>
  )
}
