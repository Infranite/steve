/* Steve's real work in affectUI Thinking State. The adapter translates steps, never schedules,
 * completes or invents them. The session transcript keeps its own detailed work timeline. */
import { Fragment, useLayoutEffect, useRef } from "react"
import { Markdown } from "@/components/markdown"
import { ThinkingState, type ThinkingStep, type ThinkingStatus } from "@/components/affect/thinking-state-clean"
import type { TrailItem, ToolStep } from "@/components/work-trail"
import { clockDur } from "@/i18n/format"
import { useT } from "@/i18n"

function Reasoning({ text, live }: { text: string; live: boolean }) {
  const ref = useRef<HTMLDivElement>(null), follow = useRef(true)
  useLayoutEffect(() => { const el = ref.current; if (live && el && follow.current) el.scrollTop = el.scrollHeight }, [text, live])
  return <div ref={ref} className="max-h-48 min-w-0 overflow-y-auto overscroll-contain break-words" data-thought-stream={live || undefined} data-thought-body={!live || undefined}
    onScroll={() => { const el = ref.current; if (el) follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 32 }}><Markdown text={text} streaming={live} className="type-callout" /></div>
}
const attrs = (a?: Record<string, string | undefined>) => Object.fromEntries(Object.entries(a ?? {}).filter(([k]) => k !== "kind").map(([k,v]) => ["data-"+k,v]))
const state = (s: "run" | "done" | "fail", live: boolean): ThinkingStatus => s === "fail" ? "error" : live && s === "run" ? "running" : "done"

export function ChatThinking({ items, status = "running", label, startedAt, durationMs, summary, open, onOpenChange, endLabel, headerAttrs }: {
  items: TrailItem[]; status?: ThinkingStatus; label?: string; startedAt?: number; durationMs?: number; summary?: string
  open?: boolean; onOpenChange?: (open: boolean) => void; endLabel?: string; headerAttrs?: Record<string, string>
}) {
  const t = useT(), live = status === "running", steps: ThinkingStep[] = []
  // Keep consecutive calls of the same kind together, with their actual subjects as source chips.
  const groups: (TrailItem | ToolStep[])[] = []
  for (const item of items) {
    const previous = groups.at(-1)
    if (item.t === "tool" && item.group && item.merged && item.subj) {
      if (Array.isArray(previous) && previous[0].group === item.group) { previous.push(item); continue }
      if (previous && !Array.isArray(previous) && previous.t === "tool" && previous.group === item.group && previous.subj) { groups[groups.length-1] = [previous,item]; continue }
    }
    groups.push(item)
  }
  const toolStep = (tools: ToolStep[]): ThinkingStep => {
    const first = tools[0], running = live && tools.some((s) => s.state === "run"), failed = tools.some((s) => s.state === "fail")
    const sources = [...new Map(tools.filter((s) => s.subj).map((s) => [s.subj!, { label: s.subj!, tooltip: s.full ?? s.subj }])).values()]
    const Icon = first.icon ?? first.chip?.icon
    const title = tools.length > 1 && first.merged ? first.merged(tools.length,sources.length,running) : first.verb[running ? 0 : 1]
    const body = tools.some((s) => s.body || s.below) ? <div className="flex flex-col gap-2">{tools.map((s) => <Fragment key={s.key}>{s.body && <div>{tools.length > 1 && s.subj && <p className="type-code break-all">{s.subj}</p>}{s.body}</div>}{s.below}</Fragment>)}</div> : undefined
    return { id: first.key, kind: first.group === "search" || first.group === "fetch" ? "search" : "step", label: <>{title}{tools.length === 1 && first.extra}{tools.length > 1 && failed && ` · ${t("thread:thinking.callFailures", { count: tools.filter((s) => s.state === "fail").length })}`}</>,
      status: running ? "running" : failed ? "error" : "done", activeLabel: typeof title === "string" ? title : undefined, sources, body, icon: Icon ? <Icon size={16} strokeWidth={1.5} aria-hidden /> : undefined,
      attrs: { ...attrs(first.attrs), ...(tools.length > 1 ? { "data-step-group": first.group, "data-step-count": String(tools.length) } : {}) } }
  }
  for (const item of groups) {
    if (Array.isArray(item)) { steps.push(toolStep(item)); continue }
    if (item.t === "tool") { steps.push(toolStep([item])); continue }
    if (item.t === "think") { steps.push({ id: item.key, kind: "step", status: live && item.live ? "running" : "done", label: t("thread:thinking.reasoning"), body: <Reasoning text={item.text} live={live && item.live} />, attrs: { ...attrs(item.attrs), "data-thinking": live && item.live ? "" : undefined } }); continue }
    if (item.t === "note") { steps.push({ id: item.key, kind: "step", status: "done", label: item.text }); continue }
    steps.push({ id: item.key, kind: "step", status: state(item.state,live), label: <>{item.label}{item.detail && <span className="ml-1.5">{item.detail}</span>}</>, body: item.body, attrs: attrs(item.attrs) })
  }
  const terminal = endLabel ?? t(status === "error" ? "thread:thinking.failed" : status === "stopped" ? "thread:thinking.stopped" : "thread:thinking.done")
  return <ThinkingState steps={steps} status={status} label={label ?? (live ? t("thread:thinking.working") : terminal)} endLabel={terminal} summary={summary} startedAt={startedAt} durationMs={durationMs} open={open} onOpenChange={onOpenChange} headerAttrs={headerAttrs} formatTime={(seconds) => clockDur(seconds*1000)} />
}
