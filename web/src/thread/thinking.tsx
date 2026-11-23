/* Steve's real work in affectUI Thinking State. The adapter translates steps, never schedules,
 * completes or invents them. The session transcript keeps its own detailed work timeline. */
import { useMemo, useState } from "react"
import { Markdown } from "@/components/markdown"
import { ThinkingState, type ThinkingStep, type ThinkingStatus } from "@/components/affect/thinking-state-clean"
import { ToolCallCard } from "@/components/affect/tool-call-card"
import type { TrailItem } from "@/components/work-trail"
import { clockDur } from "@/i18n/format"
import { useT } from "@/i18n"

function Reasoning({ text, live, completed }: { text: string; live: boolean; completed: boolean }) {
  const t = useT(), [open, setOpen] = useState(false)
  return <details className="rounded-md px-2" onToggle={(e) => setOpen(e.currentTarget.open)}>
    <summary className="min-h-9 cursor-pointer content-center text-muted-foreground">{t("thread:thinking.reasoning")}</summary>
    {open && <div data-thought-stream={live || undefined} data-thought-body={completed || undefined}>
      <Markdown text={text} streaming={live} className="type-callout min-w-0 wrap-anywhere" />
    </div>}
  </details>
}
const attrs = (a?: Record<string, string | undefined>) => Object.fromEntries(Object.entries(a ?? {}).filter(([k]) => k !== "kind").map(([k,v]) => ["data-"+k,v]))
const state = (s: "run" | "done" | "fail", live: boolean) => s === "fail" ? "error" : live && s === "run" ? "running" : "done"

export function ChatThinking({ items, status = "running", label, startedAt, lastActivity, durationMs, open, onOpenChange, scrollPosition, endLabel, headerAttrs }: {
  items: TrailItem[]; status?: ThinkingStatus; label?: string; startedAt?: number; lastActivity?: number; durationMs?: number
  open?: boolean; onOpenChange?: (open: boolean) => void; scrollPosition?: { top: number }; endLabel?: string; headerAttrs?: Record<string, string>
}) {
  const t = useT(), live = status === "running"
  const steps = useMemo<ThinkingStep[]>(() => items.map(item => {
    if (item.t === "tool") {
      const view = state(item.state, live), recovered = item.attrs?.recovered !== undefined
      const Icon = item.icon ?? item.chip?.icon
      return { id: item.key, kind: item.group === "search" || item.group === "fetch" ? "search" : "step",
        label: item.verb[0], status: recovered ? "done" : view, attrs: attrs(item.attrs),
        render: <ToolCallCard variant="line" openOnError={false} lazyDetails trackTime={false}
          name={item.verb[view === "running" ? 0 : 1]} summary={item.group === "run" ? undefined : item.subj || undefined} icon={Icon ? <Icon aria-hidden /> : undefined}
          status={recovered ? "success" : view === "done" ? "success" : view} duration={item.card?.ms}
          args={item.card?.args ?? item.subj} result={item.card?.result} error={item.card?.error}
          maxCodeHeight={112} missingResult={view === "running" ? undefined : t("thread:progress.noOutput")}
          recoveredLabel={recovered ? t("thread:progress.recovered") : undefined} /> }
    }
    if (item.t === "think") return { id: item.key, kind: "step", status: live && item.live ? "running" : "done", label: t("thread:thinking.reasoning"),
      render: <Reasoning text={item.text} live={live && item.live} completed={!live} />, attrs: { ...attrs(item.attrs), "data-thinking": live && item.live ? "" : undefined } }
    if (item.t === "note") return { id: item.key, kind: "step", status: "done", label: item.text }
    return { id: item.key, kind: "step", status: state(item.state, live), label: <>{item.label}{item.detail && <span className="ml-1.5">{item.detail}</span>}</>, body: item.body, attrs: attrs(item.attrs) }
  }), [items, live, t])
  const terminal = endLabel ?? t(status === "error" ? "thread:thinking.failed" : status === "stopped" ? "thread:thinking.stopped" : "thread:thinking.done")
  return <ThinkingState steps={steps} status={status} label={label ?? (live ? t("thread:thinking.working") : t("thread:progress.process"))} endLabel={terminal}
    startedAt={startedAt} lastActivity={lastActivity} formatActivity={s => t("thread:progress.lastActivity", { time: clockDur(s * 1000) })}
    durationMs={durationMs} open={open} onOpenChange={onOpenChange} scrollPosition={scrollPosition} scrollLabel={t("thread:progress.process")}
    disclosure={[t("thread:progress.show"), t("thread:progress.hide")]} headerAttrs={headerAttrs} formatTime={s => clockDur(s * 1000)} />
}
