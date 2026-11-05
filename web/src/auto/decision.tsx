/* 待处理 cards of flow decisions (signals-automation.md §9): `flow_step` (an approve node, or the approval before an
 * effect), `signal` (a notify: 知道了 / 转成事项) and `flow_run` (retry / drop after a restart; the budget stop).
 * The card shows the event and the flow it came from; the choice sent is the decision's own option. */
import { useEffect, useState, type ReactNode } from "react"
import { WorkflowIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { CardContent } from "@/components/ui/card"
import { Markdown } from "@/components/markdown"
import { flowApi } from "@/api/flow"
import { useT } from "@/i18n"

export const FLOW_SUBJECTS = ["flow_step", "signal", "flow_run"]
/* the label of each option the hub sends; any other option is shown as it is */
const LABEL = { done: "auto:decision.done", to_item: "auto:decision.toItem", retry: "common:action.retry", drop: "auto:decision.drop", continue_once: "auto:decision.continueOnce", stop: "auto:decision.stop" } as const
const QUIET = new Set(["done", "drop", "stop", "不执行"])

type Ctx = { summary?: string; flow?: { id: string; name: string } | null; event?: string | null }
/** what the decision is about: the event's summary and the flow's name (best effort; a missing one is left out) */
function useCtx(subject: string, id: string) {
  const [c, setC] = useState<Ctx>({})
  useEffect(() => {
    let live = true
    void (async () => {
      try {
        if (subject === "signal") { const s = await flowApi.signal(id); if (live) setC({ summary: s.summary, flow: s.flow, event: s.id }); return }
        const run = await flowApi.run(subject === "flow_step" ? id.split(":")[0] : id)
        const [f, s] = await Promise.all([flowApi.flow(run.flow_id).catch(() => null), run.event_id ? flowApi.signal(run.event_id).catch(() => null) : null])
        if (live) setC({ summary: s?.summary, flow: f ? { id: f.id, name: f.name } : null, event: run.event_id })
      } catch { /* the card still works without its context */ }
    })()
    return () => { live = false }
  }, [subject, id])
  return c
}

export function FlowDecisionBody({ w, act, busy, head, foot }: {
  w: { subject: string; subjectId?: string; q: string; draft?: string; options: string[]; decision: string }; act: (c: string) => void; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode
}) {
  const c = useCtx(w.subject, w.subjectId ?? "")
  const t = useT()
  const label = (o: string) => (o in LABEL ? t(LABEL[o as keyof typeof LABEL]) : o)
  const opts = w.options.length ? w.options : w.subject === "signal" ? ["done", "to_item"] : []
  /* the first option that moves things on is the primary button (rightmost); “不…”, 知道了, 放弃 stay quiet */
  const primary = opts.find((o) => !QUIET.has(o) && !o.startsWith("不")) ?? opts[0], rest = opts.filter((o) => o !== primary)
  return <>
    {head(w.q)}
    <CardContent className="flex flex-col gap-2 pb-4" data-flow-card={w.subject}>
      {((c.summary && c.summary !== w.q) || c.flow) && <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1 type-footnote text-muted-foreground">
        {c.summary && c.summary !== w.q && (c.event ? <a href={"#/signals?e=" + encodeURIComponent(c.event)} className="min-w-0 truncate hover:text-foreground hover:underline">{t("auto:quoted", { text: c.summary })}</a> : <span>{t("auto:quoted", { text: c.summary })}</span>)}
        {c.flow && <a href={"#/auto/rule/" + c.flow.id} className="inline-flex items-center gap-1 hover:text-foreground"><WorkflowIcon className="size-3.5" aria-hidden />{t("auto:flowRef", { name: c.flow.name })}</a>}
      </div>}
      {w.draft && <div className="max-h-72 overflow-auto break-words type-body text-muted-foreground"><Markdown text={w.draft} /></div>}
    </CardContent>
    {foot(<>{rest.map((o, i) => <Button key={o} variant={QUIET.has(o) ? "ghost" : "outline"} className={i === 0 && QUIET.has(o) ? "mr-auto text-muted-foreground" : undefined} disabled={busy} onClick={() => act(o)} data-flow-choice={o}>{label(o)}</Button>)}
      {primary && <Button disabled={busy} onClick={() => act(primary)} data-flow-choice={primary}>{label(primary)}</Button>}</>)}
  </>
}
