/* #/design/components — the component showroom: every affectUI component Steve adopted, in the states that are rare in
 * real data (decisions waiting, work running, being away, YOLO on), side by side with sample data. This page is a design
 * reference: its sample data lives here and only here, its words in views:showroom, and none of its buttons reach the Hub. */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { ArrowLeftIcon, FileTextIcon, MoreHorizontalIcon, PlusIcon, QuoteIcon, SearchIcon, TerminalIcon } from "lucide-react"
import { Page, ThemeSwitch, Card, Empty, Segmented, Chips } from "@/ui"
import { lang, useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { DateField, TimeField, NumberField } from "@/components/ui/pickers"
import { Field, FieldError, FieldLabel } from "@/components/ui/field"
import { Select } from "@/view-shared"
import { fmtN, fmtUsd } from "@/stats/format"
import { ParallelAttempts, type Attempt } from "@/components/affect/parallel-attempts-clean"
import { stepLabels } from "@/agent-tree-view"
import { handoffLabels, inspectorLabels } from "@/collab-graph"
import { fleetLabels } from "@/items-collab"
import { longDur } from "@/i18n/format"
import { RunInspector, type RunSpan } from "@/components/affect/run-inspector-clean"
import { AgentHandoff, type HandoffAgent, type Handoff, type HandoffTask } from "@/components/affect/agent-handoff-clean"
import { AgentFleet, type FleetAgent } from "@/components/affect/agent-fleet-clean"
import { AgentInbox, type AgentTask } from "@/components/affect/agent-inbox"
import { RunningNow, type RunningTask } from "@/components/affect/running-now"
import { TraceWaterfall, type TraceSpan } from "@/components/affect/trace-waterfall"
import { ToolCallCard } from "@/components/affect/tool-call-card"
import { ThinkingLoader } from "@/components/affect/thinking-loader"
import { DecisionCard, type Decision } from "@/components/affect/decision-card"
import { ApprovalCard } from "@/components/affect/approval-card"
import { AwaySummary, type AwayItem } from "@/components/affect/away-summary"
import { MessageActions } from "@/components/affect/message-actions"
import { CostWaterfall, type CostStep } from "@/components/affect/cost-waterfall"
import { CompactBreakdown } from "@/components/affect/compact-breakdown"
import { AutonomyDial, type AutonomyLevel } from "@/components/affect/autonomy-dial"
import { MemoryPanel, type Memory } from "@/components/affect/memory-panel"
import { ChatComposer } from "@/components/affect/chat-composer-clean"
import { IconEdit, IconSteer, IconUndo, RunActions, RunAct, RunContext, RunQueue, RunQueued } from "@/components/affect/run-controls-clean"
import { runLabels } from "@/composer"
import { PermissionDetails } from "@/components/permission"
import { dcLabels } from "@/thread/decide"
import { dialActions, dialText, LOCKED } from "@/yolo/switch"
import { panelText } from "@/knowledge/index"
import { memKind } from "@/api/knowledge"
import { Badge } from "@/components/ui/badge"
import { Kbd } from "@/components/ui/kbd"
import { Spinner } from "@/components/ui/spinner"
import { Skeleton } from "@/components/ui/skeleton"
import { Separator } from "@/components/ui/separator"
import { CodeBlock } from "@/components/ui/code-block"
import { ContactAvatar } from "@/people/avatar"
import { AvatarGroup, Avatar } from "@/components/ui/avatar"
import { NumberTicker } from "@/components/affect/number-ticker"

type T = ReturnType<typeof useT>
const noop = () => {}
type Copy = typeof import("@/i18n/locales/en/views").default.showroom
const S = (t: T, k: keyof Copy["s"]) => t(`views:showroom.s.${k}`)

/* ───────── sample data (a fixed afternoon: 14:00 to 15:00, "now" at 14:42) ───────── */
const T0 = new Date(2026, 10, 13, 14, 0).getTime(), MIN = 60_000, NOW = T0 + 42 * MIN
const hhmm = (ms: number) => new Date(ms).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" })

/* the collaboration: the item page's steps (Parallel Attempts without its picking), the full view's call tree and
 * handoffs, and the items page's 协作 view */
const stepRows = (t: T): Attempt[] => {
  const row = (id: string, name: string, bot: string, model: string, status: Attempt["status"], line: string, progress: number, time: string, cost: string, act?: "open" | "decide" | "retry"): Attempt => ({
    id, name, approach: `${bot} · ${model}`, status, progress, step: line, note: line, figures: [time, cost],
    icon: <span className="type-label">{bot.slice(0, 1)}</span>,
    action: act ? <Button size="xs" variant={act === "open" ? "ghost" : "outline"} className={act === "open" ? "text-muted-foreground" : undefined} onClick={noop}>{t(`views:agentTree.act.${act}`)}</Button> : undefined,
  })
  return [
    row("s1", S(t, "stepSpec"), S(t, "botDocs"), "gpt-6.1-mini", "done", `${t("views:agentTree.passedOn", { n: 2 })} · ${S(t, "stepPassed")}`, 1, longDur(12 * MIN), "$0.41", "open"),
    row("s2", S(t, "stepBuild"), S(t, "botDev"), "gpt-6.1-sol", "running", S(t, "stepDoing"), 0.6, longDur(27 * MIN), "$1.12", "open"),
    row("s3", S(t, "stepColors"), S(t, "botReview"), "claude-opus-5", "waiting", t("views:agentTree.row.needsYou", { q: S(t, "stepAsk") }), 0.45, longDur(6 * MIN), "$0.22", "decide"),
    row("s4", S(t, "stepShots"), S(t, "botDocs"), "gpt-6.1-mini", "failed", t("views:agentTree.row.failed", { reason: S(t, "stepFail") }), 0.3, longDur(4 * MIN), "$0.05", "retry"),
    row("s5", S(t, "stepDocs"), S(t, "botDocs"), "gpt-6.1-mini", "queued", t("views:workflow.waitingFor", { names: S(t, "stepBuild") }), 0, "", ""),
  ]
}
const runSample = (t: T): RunSpan => {
  const call = (id: string, kind: RunSpan["kind"], name: string, start: number, end: number, extra: Partial<RunSpan> = {}): RunSpan => ({ id, kind, name, status: "done", start, end, ...extra })
  const agent = (id: string, name: string, model: string, task: string, status: RunSpan["status"], start: number, end: number | undefined, cost: number, tokens: number, children: RunSpan[], extra: Partial<RunSpan> = {}): RunSpan =>
    ({ id, kind: "agent", name, model, task, status, start, end, cost, tokensIn: tokens, children, time: longDur((end ?? 905_000) - start), ...extra })
  return agent("root", "Steve", "gpt-6.1-sol", S(t, "st27"), "running", 0, undefined, 0.42, 38_000, [
    call("r1", "llm", t("core:trace.think"), 0, 4_000), call("r2", "tool", "steve_workflow", 4_000, 6_000),
    agent("a1", S(t, "botDocs"), "gpt-6.1-mini", S(t, "stepSpec"), "done", 6_000, 726_000, 0.41, 61_000, [
      call("a1c1", "tool", "Read", 8_000, 9_000), call("a1c2", "llm", t("core:trace.think"), 9_000, 40_000), call("a1c3", "tool", "Write", 40_000, 42_000, { input: "docs/work-area.md" })]),
    agent("a2", S(t, "botDev"), "gpt-6.1-sol", S(t, "stepBuild"), "running", 730_000, undefined, 1.12, 140_000, [
      call("a2c1", "tool", "Edit", 732_000, 735_000), call("a2c2", "tool", "Bash", 736_000, 790_000, { input: "pnpm test", status: "failed", error: "2 tests failed" }), call("a2c3", "tool", "Bash", 800_000, 900_000, { input: "pnpm test" })]),
    agent("a3", S(t, "botReview"), "claude-opus-5", S(t, "stepColors"), "waiting", 740_000, undefined, 0.22, 20_000, [call("a3c1", "llm", t("core:trace.note"), 741_000, 760_000)], { ask: S(t, "stepAsk") }),
  ])
}
const handoffSample = (t: T): { agents: HandoffAgent[]; tasks: HandoffTask[]; handoffs: Handoff[] } => ({
  agents: [
    { id: "spec", name: S(t, "botDocs"), model: "gpt-6.1-mini", description: S(t, "stepSpec"), cost: 0.41 },
    { id: "build", name: S(t, "botDev"), model: "gpt-6.1-sol", description: S(t, "stepBuild"), cost: 1.12 },
    { id: "review", name: S(t, "botReview"), model: "claude-opus-5", description: S(t, "stepReview"), cost: 0.22 },
  ],
  tasks: [
    { id: "spec", title: S(t, "stepSpec"), status: "done", agent: "spec", note: t("views:agentTree.passedOn", { n: 2 }) },
    { id: "build", title: S(t, "stepBuild"), status: "working", agent: "build", note: S(t, "stepDoing") },
    { id: "review", title: S(t, "stepReview"), status: "todo", agent: "review" },
  ],
  handoffs: [
    { id: "spec->build", from: "spec", to: "build", task: S(t, "stepSpec"), items: [{ id: "o1", label: S(t, "outSpec"), kind: "file" }, { id: "o2", label: S(t, "outContract"), kind: "fact" }, { id: "o3", label: S(t, "outNotes"), kind: "note" }] },
    { id: "spec->review", from: "spec", to: "review", task: S(t, "stepSpec"), items: [{ id: "o4", label: S(t, "outDecision"), kind: "decision" }] },
    { id: "build->review", from: "build", to: "review", task: S(t, "stepBuild"), items: [{ id: "o5", label: S(t, "outBuild"), kind: "file" }] },
  ],
})
const fleetSample = (t: T): FleetAgent[] => [
  { id: "ST-27", name: S(t, "st27"), model: `ST-27 · Steve`, task: S(t, "st27Desc"), status: "running", cost: 0.42, time: longDur(6 * MIN), children: [
    { id: "f1", name: S(t, "botDocs"), model: "gpt-6.1-mini", task: S(t, "stepSpec"), status: "done", cost: 0.41, time: longDur(12 * MIN) },
    { id: "f2", name: S(t, "botDev"), model: "gpt-6.1-sol", task: S(t, "stepBuild"), status: "running", cost: 1.12, time: longDur(27 * MIN) },
    { id: "f3", name: S(t, "botReview"), model: "claude-opus-5", task: S(t, "stepColors"), status: "waiting", ask: S(t, "stepAsk"), cost: 0.22, time: longDur(6 * MIN) },
    { id: "f4", name: S(t, "botDocs"), model: "gpt-6.1-mini", task: S(t, "stepShots"), status: "failed", error: S(t, "stepFail"), cost: 0.05, time: longDur(4 * MIN) },
  ] },
  { id: "ST-29", name: S(t, "st29"), model: `ST-29 · Steve`, task: S(t, "st29Desc"), status: "done", cost: 0.3, time: longDur(3 * MIN), children: [
    { id: "g1", name: S(t, "botDev"), model: "gpt-6.1-sol", task: S(t, "step2"), status: "done", cost: 0.8, time: longDur(15 * MIN) },
  ] },
]

const inboxTasks = (t: T): AgentTask[] => [
  { id: "ST-27", code: "ST-27", title: S(t, "st27"), agent: S(t, "botDev"), lane: "needs", status: "waiting", tone: "wait", stateText: S(t, "stAccept"), time: "14:38", description: S(t, "st27Desc"), progress: { done: 5, total: 6 } },
  { id: "ST-31", code: "ST-31", title: S(t, "st31"), agent: S(t, "botReview"), lane: "needs", status: "failed", tone: "fail", stateText: S(t, "stFailed"), time: "14:21", description: S(t, "st31Desc") },
  { id: "ST-29", code: "ST-29", title: S(t, "st29"), agent: S(t, "botDev"), lane: "running", status: "running", tone: "run", stateText: S(t, "stWorking"), time: "14:40", description: S(t, "st29Desc"), progress: { done: 2, total: 5 },
    steps: [{ label: S(t, "step1") }, { label: S(t, "step2") }, { label: S(t, "step3"), time: S(t, "stWorking") }] },
  { id: "ST-33", code: "ST-33", title: S(t, "st33"), agent: S(t, "botDocs"), lane: "running", status: "running", tone: "pause", stateText: S(t, "stPaused"), time: "13:55", progress: { done: 1, total: 3 } },
  { id: "ST-24", code: "ST-24", title: S(t, "st24"), agent: S(t, "botReview"), lane: "done", status: "done", tone: "done", stateText: S(t, "stDone"), time: "11:02" },
  { id: "ST-22", code: "ST-22", title: S(t, "st22"), agent: S(t, "botDocs"), lane: "done", status: "done", tone: "done", stateText: S(t, "stDone"), time: S(t, "yesterday") },
]

const runTasks = (t: T): RunningTask[] => [
  { id: "ST-31", title: `ST-31 ${S(t, "st31")}`, step: S(t, "rnAsk"), done: 3, total: 4, state: "needs-you" },
  { id: "ST-29", title: `ST-29 ${S(t, "st29")}`, step: S(t, "step3"), done: 2, total: 5, state: "running" },
  { id: "ST-27", title: `ST-27 ${S(t, "st27")}`, step: S(t, "rnTest"), done: 5, total: 6, state: "running" },
  { id: "ST-33", title: `ST-33 ${S(t, "st33")}`, step: `${S(t, "stPaused")} · ${S(t, "rnShots")}`, done: 1, total: 3, state: "paused", resumable: true },
]

const traceRoot = (t: T, live: boolean): TraceSpan => ({
  id: "turn", name: S(t, "botDev"), kind: "agent", status: live ? "running" : "done", start: 0, end: live ? undefined : 48_000, detail: "ST-27",
  children: [
    { id: "t1", name: S(t, "trThink"), kind: "llm", status: "done", start: 0, end: 6_200, model: "gpt-6.1-sol", tokensIn: 18_400, tokensOut: 620, cost: 0.06 },
    { id: "t2", name: "rg", kind: "tool", status: "done", start: 6_400, end: 7_100, detail: "\"lane\" web/src" },
    { id: "t3", name: "Read", kind: "tool", status: "done", start: 7_200, end: 7_600, detail: "agent-tree-view.tsx" },
    { id: "t4", name: S(t, "botReview"), kind: "subagent", status: live ? "running" : "done", start: 8_000, end: live ? undefined : 31_000, detail: S(t, "trReview"),
      children: [
        { id: "t41", name: S(t, "trThink"), kind: "llm", status: "done", start: 8_200, end: 19_000, model: "claude-opus-5", tokensIn: 42_000, tokensOut: 1_900, cost: 0.31 },
        { id: "t42", name: "Bash", kind: "tool", status: live ? "running" : "failed", start: 19_200, end: live ? undefined : 30_400, detail: "pnpm test", error: live ? undefined : S(t, "trTestFail") },
      ] },
    ...(live ? [] : [{ id: "t5", name: "Edit", kind: "tool" as const, status: "done" as const, start: 31_200, end: 32_000, detail: "agent-tree-view.tsx" },
      { id: "t6", name: S(t, "trAnswer"), kind: "llm" as const, status: "done" as const, start: 32_200, end: 48_000, model: "gpt-6.1-sol", tokensIn: 24_800, tokensOut: 2_400, cost: 0.11 }]),
  ],
})

const costSteps = (t: T): CostStep[] => [
  { id: "item:ST-27", name: `ST-27 ${S(t, "st27")}`, kind: "agent", status: "done", duration: 41 * MIN, children: [
    { id: "turn:1", name: S(t, "cwPlan"), kind: "agent", status: "done", duration: 6 * MIN, children: [
      { id: "c11", name: S(t, "trThink"), kind: "llm", status: "done", model: "gpt-6.1-sol", tokensIn: 32_000, tokensOut: 2_100, cost: 0.14, duration: 38_000 },
      { id: "c12", name: "Read", kind: "tool", status: "done", duration: 900, tokens: 0 },
    ] },
    { id: "turn:2", name: S(t, "cwBuild"), kind: "agent", status: "done", duration: 24 * MIN, children: [
      { id: "c21", name: S(t, "trThink"), kind: "llm", status: "done", model: "gpt-6.1-sol", tokensIn: 210_000, tokensOut: 18_400, cost: 1.12, duration: 9 * MIN },
      { id: "c22", name: "Bash", kind: "tool", status: "failed", duration: 72_000, error: S(t, "trTestFail"), input: "pnpm --filter web test", output: "✗ agent-tree.test.mjs › lanes keep their order\n✗ agent-tree.test.mjs › the Now line stays inside the window" },
      { id: "c23", name: "Edit", kind: "tool", status: "done", duration: 2_400 },
    ] },
    { id: "turn:3", name: S(t, "cwReview"), kind: "agent", status: "done", duration: 11 * MIN, children: [
      { id: "c31", name: S(t, "trThink"), kind: "llm", status: "done", model: "claude-opus-5", tokensIn: 96_000, tokensOut: 5_200, cost: 0.68, duration: 4 * MIN },
    ] },
  ] },
]

const memories = (t: T, k: (x: string) => string): Memory[] => [
  { id: "m1", text: S(t, "mem1"), category: k("rule"), source: S(t, "memFromChat"), date: "11-02" },
  { id: "m2", text: S(t, "mem2"), category: k("rule"), source: S(t, "memFromItem"), date: "11-08" },
  { id: "m3", text: S(t, "mem3"), category: k("preference"), source: S(t, "memYou"), date: "10-27" },
  { id: "m4", text: S(t, "mem4"), category: k("preference"), source: S(t, "memFromChat"), date: "10-12", paused: true },
]

/* ───────── layout ───────── */
type Id = keyof Copy["c"]
/** one component: what it is called, where it lives in Steve and when it shows, then its states */
function Exhibit({ id, children, wide }: { id: Id; children: ReactNode; wide?: boolean }) {
  const t = useT()
  return (
    <section className="flex scroll-mt-16 flex-col gap-4" data-showroom={id} id={"showroom-" + id}>
      <div className="flex flex-col gap-1.5 border-b pb-3">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5"><h2 className="type-title">{t(`views:showroom.c.${id}.name`)}</h2><span className="type-code text-tertiary">{id}</span></div>
        <dl className="grid gap-x-3 gap-y-1 type-callout sm:grid-cols-[auto_1fr]">
          <dt className="text-tertiary">{t("views:showroom.where")}</dt><dd className="text-muted-foreground">{t(`views:showroom.c.${id}.where`)}</dd>
          <dt className="text-tertiary">{t("views:showroom.when")}</dt><dd className="text-muted-foreground">{t(`views:showroom.c.${id}.when`)}</dd>
        </dl>
      </div>
      <div className={cn("grid items-start gap-x-5 gap-y-6", !wide && "lg:grid-cols-2")}>{children}</div>
    </section>
  )
}
/** one state of a component, with its caption */
function State({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <div className={cn("flex min-w-0 flex-col gap-2", className)}>
      <div className="px-1 type-label text-tertiary">{label}</div>
      {children}
    </div>
  )
}
const L = (t: T, k: keyof Copy["st"]) => t(`views:showroom.st.${k}`)

/* ───────── the page ───────── */
/** a session's composer as Steve draws it, with its words kept here: nothing it does leaves the page */
function SampleComposer({ running, text: first = "", queue = [], ctx }: { running?: boolean; text?: string; queue?: string[]; ctx?: number }) {
  const t = useT()
  const [text, setText] = useState(first), [stopped, setStopped] = useState(false)
  const live = running && !stopped, has = !!text.trim()
  return (
    <ChatComposer model="Claude Code · gpt-6.1-sol" addLabel={t("core:composer.add")} add={[{ rows: [{ id: "files", label: t("core:composer.addFiles"), description: t("core:composer.addFilesDesc") }] }]} onAdd={noop}
      head={queue.length ? <RunQueue label={t("shell:queue.region")} note={t("shell:queue.queuedN", { count: queue.length, note: t("shell:queue.afterTurn") })}>
        {queue.map((q, n) => <RunQueued key={n} place={n === 0 ? t("shell:queue.next") : t("shell:queue.place", { n: n + 1 })} actions={<>
          <RunAct icon={<IconEdit />} label={t("shell:queue.editAria", { text: q })} onClick={noop} />
          <RunAct icon={<IconUndo />} label={t("shell:queue.withdrawAria", { text: q })} onClick={noop} />
          <RunAct icon={<IconSteer />} text={t("shell:queue.sendNow")} tone="accent" label={t("shell:queue.sendNowAria", { text: q })} onClick={noop} />
        </>}><span className="type-callout">{q}</span></RunQueued>)}
      </RunQueue> : undefined}
      end={<>{ctx !== undefined && <RunContext value={ctx} title={t("views:session.ctxUsed", { pct: Math.round(ctx * 100) })} />}
        <RunActions running={live} has={has} canSend={has} steers labels={runLabels(t, true)} onStop={() => setStopped(true)} onSend={() => setText("")} onQueue={() => setText("")} onSteer={() => setText("")} /></>}>
      <textarea value={text} onChange={(e) => setText(e.target.value)} rows={1} placeholder={t("views:session.placeholder")} aria-label={t("views:session.placeholder")} className="ui-ccc__message" />
    </ChatComposer>
  )
}

const IDS: Id[] = ["parallel-attempts", "run-inspector", "agent-handoff", "agent-fleet", "agent-inbox", "running-now", "trace-waterfall", "tool-call-card", "thinking-loader", "decision-card", "approval-card", "away-summary", "message-actions", "cost-waterfall", "compact-breakdown", "autonomy-dial", "memory-panel", "chat-composer", "display", "form-controls"]

export function DesignShowroom() {
  const t = useT()
  const [costMode, setCostMode] = useState<"cost" | "tokens">("cost")
  const [dial, setDial] = useState<Record<string, AutonomyLevel>>({ off: 0, on: 1, full: 2, forced: 0 })
  const kind = memKind
  const mk = { ...panelText(), none: t("knowledge:record.mem.none"), newMemory: t("knowledge:record.mem.addAria"), newPlaceholder: t("knowledge:record.mem.hint") }
  const rnLabels = { title: t("views:runningNow.title"), running: t("views:runningNow.running"), needsYou: t("views:runningNow.needsYou"), needsYouBadge: t("views:runningNow.needsYouBadge"),
    paused: t("views:runningNow.paused"), pausedCount: t("views:runningNow.pausedCount"), done: t("views:runningNow.done"), pause: t("views:runningNow.pause"), resume: t("views:runningNow.resume"),
    open: t("views:runningNow.open"), openAll: t("views:runningNow.openAll"), empty: t("views:runningNow.empty"), idle: t("views:runningNow.idle"), stepsOf: t("views:runningNow.stepsOf") }
  const awayLabels = (from: number) => ({ title: t("pages:chat.away.title"), since: () => t("pages:chat.away.since", { time: hhmm(from) }), done: (n: number) => t("pages:chat.away.done", { count: n }), needsYou: (n: number) => t("pages:chat.away.needsYou", { count: n }), failed: (n: number) => t("pages:chat.away.failed", { count: n }),
    action: { "needs-you": t("pages:chat.away.decide"), done: t("pages:chat.away.open"), failed: t("pages:chat.away.look") }, showMore: (n: number) => t("pages:chat.away.showMore", { count: n }), showLess: t("pages:chat.away.showLess"), gotIt: t("pages:chat.away.gotIt"),
    collapsed: (away: string, n: number) => t("pages:chat.away.collapsed", { away, count: n }), empty: t("pages:chat.away.empty") })
  const away: AwayItem[] = [
    { id: "w:1", kind: "needs-you", title: `ST-27 ${S(t, "st27")}`, detail: S(t, "awAccept") },
    { id: "w:2", kind: "needs-you", title: `ST-31 ${S(t, "st31")}`, detail: S(t, "rnAsk") },
    { id: "i:3", kind: "done", title: `ST-24 ${S(t, "st24")}`, detail: S(t, "awDone24") },
    { id: "i:4", kind: "done", title: `ST-22 ${S(t, "st22")}`, detail: S(t, "awDone22") },
    { id: "i:5", kind: "failed", title: `ST-33 ${S(t, "st33")}`, detail: S(t, "awFail") },
  ]
  const ask: Decision = { id: "d1", question: S(t, "dcQuestion"), badge: S(t, "dcBadgeAsk"), task: <span><span className="font-mono tabular-nums">ST-27</span> {S(t, "st27")}</span>,
    tools: <span className="type-footnote text-muted-foreground">{t("core:card.waited", { bot: S(t, "botDev"), since: "14:36" })}</span>,
    options: [{ id: "0", title: S(t, "dcOpt1"), summary: S(t, "dcOpt1Sum"), recommended: true }, { id: "1", title: S(t, "dcOpt2"), summary: S(t, "dcOpt2Sum") }, { id: "2", title: S(t, "dcOpt3") }],
    own: { placeholder: t("core:card.own") } }
  const approve: Decision = { id: "d2", kind: "approval", question: S(t, "dcApprove"), badge: S(t, "dcBadgeApprove"), task: <span><span className="font-mono tabular-nums">ST-24</span> {S(t, "st24")}</span>,
    detail: <div className="type-callout text-muted-foreground">{S(t, "dcApproveDetail")}</div>,
    options: [{ id: "yes", title: t("core:card.approve") }], own: { placeholder: t("core:card.rejectPlaceholder"), send: t("core:card.reject"), required: false } }
  const accept: Decision = { id: "d3", question: S(t, "dcAccept"), badge: S(t, "dcBadgeAccept"), task: <span><span className="font-mono tabular-nums">ST-29</span> {S(t, "st29")}</span>,
    options: [{ id: "pass", title: t("core:card.pass") }], own: { placeholder: t("core:card.reworkPlaceholder", { bot: S(t, "botDev") }), send: t("core:card.sendRework") } }
  const tool = (status: "pending" | "approved" | "denied") => (
    <ApprovalCard className="steve-ap" title={t("core:card.allowTool")} description={<span><span className="font-mono tabular-nums">ST-27</span> {S(t, "st27")}</span>}
      label={<span className="flex w-full min-w-0 items-center justify-between gap-2"><span className="truncate">{t("core:wait.tool")}</span><span className="truncate type-footnote text-muted-foreground max-sm:hidden">{t("core:card.waited", { bot: S(t, "botDev"), since: "14:40" })}</span></span>}
      preview={<PermissionDetails className="-m-3 rounded-none bg-transparent" title="Bash(pnpm --filter web test -- agent-tree)" input={'{\n  "command": "pnpm --filter web test -- agent-tree",\n  "timeout": 600000\n}'} />}
      status={status} allowAlways alwaysLabel={t("core:card.allowAlwaysCheck")} approveLabel={t("core:card.allowOnce")} denyLabel={t("core:card.deny")}
      resolvedLabels={{ approved: t("core:card.allowed"), denied: t("core:card.denied"), expired: t("core:card.expired") }} onApprove={noop} onDeny={noop} />
  )
  const dialFor = (k: "off" | "on" | "full" | "forced") => (
    <AutonomyDial className="[--ui-adl-max:100%]" title="" actions={dialActions()} pinned={LOCKED} lockable={false} text={dialText()} bullets={false}
      level={k === "forced" ? 0 : dial[k]} onLevelChange={(n) => setDial((d) => ({ ...d, [k]: n }))} currentLevel={k === "forced" ? undefined : ({ off: 0, on: 1, full: 2 } as const)[k]}
      disabledLevels={k === "forced" ? [1, 2] : []} tones={{ 2: "attention" }}>
      {k === "forced" && <div className="type-footnote text-muted-foreground">{t("yolo:switch.forced", { reason: S(t, "adForcedReason") })}</div>}
    </AutonomyDial>
  )
  const bubble = (more: boolean) => (
    <div className="flex flex-col gap-1.5">
      <div className="max-w-[46ch] type-chat">{S(t, "maText")}</div>
      <MessageActions text={S(t, "maText")} actions={more ? ["copy", "regenerate", "feedback"] : ["copy"]} quiet={false} label={t("pages:chat.msgActions")}
        copyLabels={{ copy: t("common:action.copy"), copied: t("common:action.copied") }} versions={more ? 2 : undefined} onCopy={noop} onRegenerate={more ? noop : undefined} onFeedback={more ? noop : undefined}
        more={[{ key: "quote", label: S(t, "maQuote"), icon: <QuoteIcon />, onClick: noop }, { key: "item", label: S(t, "maToItem"), icon: <FileTextIcon />, onClick: noop }]} />
    </div>
  )
  return (
    <Page title={t("views:showroom.title")} sub={t("views:showroom.sub")} max={1040} back={["#/design", t("views:design.title")]} toolbar={<ThemeSwitch labels />}>
      <div className="flex flex-col gap-16" data-showroom-page>
        <nav className="-mt-6 flex flex-wrap gap-1.5" aria-label={t("views:showroom.index")}>
          {IDS.map((id) => <a key={id} href={"#showroom-" + id} onClick={(e) => { e.preventDefault(); document.getElementById("showroom-" + id)?.scrollIntoView({ behavior: "smooth", block: "start" }) }}
            className="rounded-full bg-muted/70 px-2.5 py-1 type-footnote text-muted-foreground hover:bg-accent hover:text-foreground">{t(`views:showroom.c.${id}.name`)}</a>)}
        </nav>

        <Exhibit id="parallel-attempts" wide>
          <State label={L(t, "stepsLive")}>
            <div data-showroom-steps>
              <div className="mb-3 flex flex-wrap items-center gap-x-1.5 type-footnote tabular-nums text-muted-foreground">
                <span className="text-attention">● {t("views:agentTree.dots.needs", { count: 1 })}</span>·<span className="text-destructive">● {t("views:agentTree.dots.failed", { count: 1 })}</span>·<span className="text-blue">● {t("views:agentTree.dots.running", { count: 1 })}</span>·<span>● {t("views:agentTree.dots.done", { count: 1 })}</span>
              </div>
              <ParallelAttempts pickable={false} header={null} attempts={stepRows(t)} selected="s2" onSelect={noop}
                labels={stepLabels(t)} />
            </div>
          </State>
        </Exhibit>

        <Exhibit id="run-inspector" wide>
          <State label={L(t, "inspector")}><Card><RunInspector run={runSample(t)} now={905_000} startedAt={NOW - 905_000} formatCost={fmtUsd} formatTook={longDur} labels={inspectorLabels(t)} /></Card></State>
        </Exhibit>

        <Exhibit id="agent-handoff" wide>
          <State label={L(t, "handoff")}><Card><AgentHandoff {...handoffSample(t)} inbound labels={handoffLabels(t)} /></Card></State>
        </Exhibit>

        <Exhibit id="agent-fleet" wide>
          <State label={L(t, "fleet")}><Card><AgentFleet agents={fleetSample(t)} defaultOpen="ST-27" now={NOW} formatCost={fmtUsd} title={t("views:items.fleet.title")} onSelect={noop} labels={fleetLabels(t)} /></Card></State>
        </Exhibit>

        <Exhibit id="agent-inbox" wide>
          <State label={L(t, "board")}>
            <AgentInbox tasks={inboxTasks(t)} bar={false} view="board" onOpen={noop} onApprove={noop} onReject={noop} onRetry={noop}
              labels={{ needs: t("views:board.needs"), running: t("views:board.running"), paused: t("views:board.paused"), done: t("views:board.done"), waitingState: t("views:board.waiting"), runningState: t("views:board.runningState"),
                doneState: t("views:board.doneState"), failedState: t("views:board.failed"), doneIn: t("views:board.doneIn"), empty: t("views:board.empty"), open: t("views:board.open"),
                showAll: t("views:board.showAll"), collapse: t("views:board.collapse"), summary: t("views:board.summary"), progress: t("views:board.progress") }} />
          </State>
        </Exhibit>

        <Exhibit id="running-now">
          <div className="flex flex-col gap-6">
            <State label={L(t, "pillClosed")}><Card className="flex min-h-16 items-center"><RunningNow tasks={runTasks(t)} align="start" defaultOpen={false} onPause={noop} onResume={noop} onOpen={noop} onOpenAll={noop} labels={rnLabels} /></Card></State>
            <State label={L(t, "pillIdle")}><Card className="flex min-h-16 items-center"><RunningNow tasks={[]} align="start" onOpenAll={noop} labels={rnLabels} /></Card></State>
          </div>
          <State label={L(t, "pillOpen")}><Card className="min-h-[23rem]"><RunningNow tasks={runTasks(t)} align="start" open onOpenChange={noop} onPause={noop} onResume={noop} onOpen={noop} onOpenAll={noop} labels={rnLabels} /></Card></State>
        </Exhibit>

        <Exhibit id="trace-waterfall">
          <State label={L(t, "traceLive")}><TraceWaterfall root={traceRoot(t, true)} now={27_000} formatCost={fmtUsd} /></State>
          <State label={L(t, "traceDone")}><TraceWaterfall root={traceRoot(t, false)} formatCost={fmtUsd} markSlowest /></State>
        </Exhibit>

        <Exhibit id="tool-call-card">
          <State label={L(t, "toolLine")}>
            <Card className="flex flex-col gap-1">
              <ToolCallCard variant="line" name="Read" icon={<FileTextIcon />} summary={<span className="font-mono">web/src/agent-tree-view.tsx</span>} status="success" duration={420} args={{ file_path: "web/src/agent-tree-view.tsx" }} result={S(t, "tcReadResult")} />
              <ToolCallCard variant="line" name="Grep" icon={<SearchIcon />} summary={<span className="font-mono">"AgentLanes" web/src</span>} status="success" duration={180} args={{ pattern: "AgentLanes", path: "web/src" }} result={"web/src/agent-tree-view.tsx:25\nweb/src/agent-tree-view.tsx:301"} />
              <ToolCallCard variant="line" name="Bash" icon={<TerminalIcon />} summary={<span className="font-mono">pnpm --filter web test</span>} status="running" startedAt={Date.now() - 12_000} args={{ command: "pnpm --filter web test" }} />
            </Card>
          </State>
          <State label={L(t, "toolCard")}>
            <div className="flex flex-col gap-2">
              <ToolCallCard name="Bash" icon={<TerminalIcon />} summary={<span className="font-mono">pnpm --filter web test</span>} status="error" duration={72_400} args={{ command: "pnpm --filter web test" }}
                error={S(t, "trTestFail")} onRetry={noop} defaultOpen />
              <ToolCallCard name="Edit" icon={<FileTextIcon />} summary={<span className="font-mono">agent-tree-view.tsx +12 −4</span>} status="success" duration={900} args={{ file_path: "web/src/agent-tree-view.tsx" }} result={S(t, "tcEditResult")} />
            </div>
          </State>
        </Exhibit>

        <Exhibit id="thinking-loader">
          <State label={L(t, "thinkPlain")}><Card className="flex flex-col gap-3">
            <ThinkingLoader variant="plain" size="sm" phases={[t("shell:chat.phase.start")]} phase={0} />
            <ThinkingLoader variant="plain" size="sm" phases={[S(t, "tlDoing")]} phase={0} className="[--ui-tl-ray:var(--blue)]! [--ui-tl-ink:var(--foreground)]!" />
          </Card></State>
          <State label={L(t, "thinkPill")}><Card className="flex flex-wrap items-center gap-3">
            <ThinkingLoader phases={[S(t, "tlP1"), S(t, "tlP2"), S(t, "tlP3")]} loop />
            <ThinkingLoader done doneLabel={S(t, "tlDone")} phases={[S(t, "tlP1")]} />
          </Card></State>
        </Exhibit>

        <Exhibit id="decision-card">
          <State label={L(t, "dcAsk")}><DecisionCard className="steve-dc" decisions={[ask]} onDecide={noop} hold later={false} labels={dcLabels()} /></State>
          <State label={L(t, "dcApproval")}><DecisionCard className="steve-dc" decisions={[approve]} onDecide={noop} hold later={false} labels={dcLabels()} /></State>
          <State label={L(t, "dcComposer")} className="lg:col-span-2"><DecisionCard className="steve-dc" decisions={[accept, ask, approve]} onDecide={noop} onLater={noop} hold labels={{ ...dcLabels(), later: t("core:card.hide") }} /></State>
        </Exhibit>

        <Exhibit id="approval-card">
          <State label={L(t, "apPending")}>{tool("pending")}</State>
          <div className="flex flex-col gap-6">
            <State label={L(t, "apApproved")}>{tool("approved")}</State>
            <State label={L(t, "apDenied")}>{tool("denied")}</State>
          </div>
        </Exhibit>

        <Exhibit id="away-summary">
          <State label={L(t, "awOpen")}><AwaySummary since={T0 - 3 * 60 * MIN} now={NOW} items={away} timeStyle="clock" locale={t("views:showroom.locale")} onAction={noop} onDismiss={noop} labels={awayLabels(T0 - 3 * 60 * MIN)} /></State>
          <State label={L(t, "awCollapsed")}><AwaySummary since={T0 - 3 * 60 * MIN} now={NOW} items={away} timeStyle="clock" locale={t("views:showroom.locale")} defaultCollapsed onAction={noop} onDismiss={noop} labels={awayLabels(T0 - 3 * 60 * MIN)} /></State>
        </Exhibit>

        <Exhibit id="message-actions">
          <State label={L(t, "maSteve")}><Card>{bubble(false)}</Card></State>
          <State label={L(t, "maFull")}><Card>{bubble(true)}</Card></State>
        </Exhibit>

        <Exhibit id="cost-waterfall" wide>
          <State label={L(t, costMode === "cost" ? "cwCost" : "cwTokens")}>
            <Card className="p-0">
              <CostWaterfall bare steps={costSteps(t)} mode={costMode} onModeChange={setCostMode} status="done" defaultOpenId="c22"
                formatCost={(n) => fmtUsd(n)} formatTokens={(n) => fmtN(n)} formatDuration={(ms) => (ms >= MIN ? `${Math.round(ms / MIN)} min` : `${(ms / 1000).toFixed(1)} s`)} />
            </Card>
          </State>
        </Exhibit>

        <Exhibit id="compact-breakdown">
          <State label={L(t, "cbCost")}><Card className="p-5">
            <CompactBreakdown bare title={t("stats:usage.breakdownCost")} totalLabel={t("stats:usage.totalCost")} format={(n) => (n === 0 ? "$0" : fmtUsd(n))} delta={18.4} deltaTone="neutral"
              items={[{ label: S(t, "botDev"), value: 12.84 }, { label: S(t, "botReview"), value: 6.31 }, { label: S(t, "botDocs"), value: 1.92 }, { label: "Steve", value: 0.88 }]} />
          </Card></State>
          <State label={L(t, "cbTokens")}><Card className="p-5">
            <CompactBreakdown bare title={t("stats:usage.breakdownTokens")} totalLabel={t("stats:usage.totalTokens")} format={(n) => fmtN(Math.round(n))} delta={-7.2} deltaTone="neutral"
              items={[{ label: "gpt-6.1-sol", value: 4_120_000 }, { label: "claude-opus-5", value: 1_860_000 }, { label: "gpt-6.1-mini", value: 640_000 }]}
              footer={<span className="whitespace-normal">{t("stats:usage.noCostYet")}</span>} />
          </Card></State>
        </Exhibit>

        <Exhibit id="autonomy-dial">
          <State label={L(t, "adOff")}>{dialFor("off")}</State>
          <State label={L(t, "adOn")}>{dialFor("on")}</State>
          <State label={L(t, "adFull")}>{dialFor("full")}</State>
          <State label={L(t, "adForced")}>{dialFor("forced")}</State>
        </Exhibit>

        <Exhibit id="memory-panel" wide>
          <State label={L(t, "mpList")}>
            <MemoryPanel className="[--ui-mp-width:100%]" title={t("knowledge:tab.mems")} showTitle={false} searchable={false} memories={memories(t, kind)} categories={["rule", "preference"].map(kind)}
              maxHeight={0} showSwitch={false} text={mk} addedSource={t("knowledge:mems.youWrote")} onChange={noop} onAdd={noop} onEdit={noop} onPause={noop} onForget={noop} onRestore={noop} />
          </State>
        </Exhibit>

        <Exhibit id="chat-composer">
          <State label={L(t, "ccIdle")}><SampleComposer text={S(t, "ccIdleText")} ctx={0.54} /></State>
          <State label={L(t, "ccRunning")}><SampleComposer running text={S(t, "ccTyped")} queue={[S(t, "ccQ1"), S(t, "ccQ2")]} ctx={0.86} /></State>
        </Exhibit>

        <Exhibit id="display">
          <State label={L(t, "dpBadges")}><Card className="flex flex-wrap items-center gap-2">
            <Badge variant="secondary">{S(t, "dpExternal")}</Badge><Badge tone="info" live>{S(t, "stWorking")}</Badge><Badge tone="success" dot>{S(t, "stDone")}</Badge>
            <Badge variant="secondary" className="bg-attention/12 text-attention">{S(t, "dpPending")}</Badge><Badge variant="destructive">{S(t, "stFailed")}</Badge><Badge variant="outline">{S(t, "dpDefault")}</Badge><Badge tone="info" count={128} />
          </Card></State>
          <State label={L(t, "dpKeys")}><Card className="flex flex-wrap items-center gap-x-4 gap-y-2 type-callout text-muted-foreground">
            <span className="inline-flex items-center gap-1.5"><Kbd>⏎</Kbd>{t("core:composer.hintSend")}</span><span className="inline-flex items-center gap-1.5"><Kbd>⇧⏎</Kbd>{t("core:composer.hintNewline")}</span><Kbd keys={["mod", "K"]} />
            <Separator orientation="vertical" /><span className="inline-flex items-center gap-1.5"><Spinner />{S(t, "stWorking")}</span><Spinner className="size-5 text-blue" />
          </Card></State>
          <State label={L(t, "dpSkeleton")}><Card className="flex flex-col gap-3">
            <Skeleton className="h-5 w-40" /><Skeleton className="h-3 w-full" /><Skeleton className="h-3 w-3/4" /><Separator label={S(t, "yesterday")} /><Skeleton className="h-16 w-full rounded-lg" />
          </Card></State>
          <State label={L(t, "dpAvatars")}><Card className="flex flex-wrap items-center gap-3">
            <ContactAvatar name="Mara Lindqvist" size={40} /><ContactAvatar name={S(t, "dpName1")} size={32} /><ContactAvatar name={S(t, "dpName2")} size={28} unconfirmed /><ContactAvatar name="Globex" type="org" size={32} /><ContactAvatar name="Infra" type="team" size={32} />
            <AvatarGroup size={24} max={3} total={6}><Avatar name="Mara Lindqvist" /><Avatar name={S(t, "dpName1")} /><Avatar name="Tom Okafor" /></AvatarGroup>
          </Card></State>
          <State label={L(t, "dpCode")}><div className="flex flex-col gap-3">
            <CodeBlock code="steve hub restart" language="bash" compact wrap labels={{ copy: t("pages:upgrade.copyCommand") }} />
            <CodeBlock code={"const lanes = groupBy(tasks, \"agent\")\nconst lanes = tasks\nreturn lanes"} marks={{ 1: "added", 2: "removed" }} copyable={false} />
            <CodeBlock code={"$ npm test\n✓ 159 passed"} marks={{ 2: "ok" }} copyable={false} />
          </div></State>
          <State label={L(t, "dpFigures")}><Card className="grid grid-cols-3 gap-2">
            {[[S(t, "stWorking"), 3], [S(t, "stDone"), 26], [S(t, "stFailed"), 1]].map(([k, n]) => <div key={k} className="flex flex-col gap-1"><span className="type-footnote text-muted-foreground">{k}</span><span className="type-display tabular-nums"><NumberTicker value={n as number} locale={lang()} /></span></div>)}
          </Card></State>
          <State label={L(t, "dpEmptyPage")}><Card className="p-0"><Empty icon={<SearchIcon />} title={S(t, "dpEmptyTitle")} sub={S(t, "dpEmptySub")} /></Card></State>
          <State label={L(t, "dpEmptyPanel")}><Empty compact icon={<TerminalIcon />} title={S(t, "dpEmptyTitle")} sub={S(t, "dpEmptySub")} /></State>
        </Exhibit>
        <FormControls />

        <a href="#/design" className="inline-flex items-center gap-1.5 self-start type-callout text-blue hover:underline"><ArrowLeftIcon className="size-4" />{t("views:design.title")}</a>
      </div>
    </Page>
  )
}

/** Steve's base controls (components/ui), drawn by affectUI: each in its states, wired to local state only */
function FormControls() {
  const t = useT()
  const ranges = [["today", t("stats:page.range.today")], ["7d", t("stats:page.range.d7")], ["30d", t("stats:page.range.d30")]] as const
  const [range, setRange] = useState<string>("7d"), [mode, setMode] = useState<string>("blocking"), [chip, setChip] = useState(t("stats:page.range.today"))
  const [on, setOn] = useState(true), [sw, setSw] = useState(true), [kind, setKind] = useState("all")
  const [day, setDay] = useState("2026-11-13"), [time, setTime] = useState<string | null>("09:00"), [num, setNum] = useState<number | null>(20)
  return (
    <Exhibit id="form-controls" wide>
      <State label={L(t, "fcButtons")}>
        <div className="flex flex-wrap items-center gap-2">
          <Button>{t("common:action.save")}</Button>
          <Button variant="outline">{t("common:action.cancel")}</Button>
          <Button variant="ghost">{t("common:action.edit")}</Button>
          <Button variant="destructive">{t("common:action.delete")}</Button>
          <Button variant="attention">{t("common:action.confirm")}</Button>
          <Button disabled>{t("common:action.save")}</Button>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {(["xs", "sm", "default", "lg"] as const).map((z) => <Button key={z} size={z} variant="outline"><PlusIcon />{t("common:action.add")}</Button>)}
          {(["icon-xs", "icon-sm", "icon", "icon-lg"] as const).map((z) => <Button key={z} size={z} variant="ghost" aria-label={t("common:action.more")}><MoreHorizontalIcon /></Button>)}
        </div>
      </State>
      <State label={L(t, "fcChoice")}>
        <div className="flex flex-wrap items-center gap-3">
          <Segmented value={range} onChange={setRange} options={ranges} />
          <Segmented pressed size="sm" value={mode} onChange={setMode} options={[["blocking", t("shell:graph.blocking")], ["lineage", t("shell:graph.lineage")]] as const} />
          <Chips value={chip} onChange={setChip} options={ranges.map(([, l]) => l)} />
        </div>
      </State>
      <State label={L(t, "fcChecks")}>
        <div className="flex flex-wrap items-start gap-x-8 gap-y-3">
          <Checkbox checked={on} onCheckedChange={setOn} label={t("yolo:switch.fullOk")} />
          <Checkbox checked={false} indeterminate label={L(t, "fcCheckA")} />
          <Checkbox checked={false} disabled label={L(t, "fcCheckB")} />
          <Switch checked={sw} onCheckedChange={setSw} label={t("common:action.open")} />
          <Switch size="sm" checked={false} disabled label={t("common:action.close")} />
        </div>
      </State>
      <State label={L(t, "fcFields")}>
        <div className="grid gap-3 sm:grid-cols-2">
          <Input prefix={<SearchIcon />} placeholder={t("views:items.search")} />
          <Field data-invalid>
            <FieldLabel htmlFor="showroom-fc-item">{L(t, "fcItemNo")}</FieldLabel>
            <Input id="showroom-fc-item" defaultValue="ST-999" aria-invalid aria-describedby="showroom-fc-item-err" />
            <FieldError id="showroom-fc-item-err">{L(t, "fcItemErr")}</FieldError>
          </Field>
          <Textarea rows={2} placeholder={L(t, "fcNote")} />
          <Select size="default" value={kind} onChange={setKind} options={[{ v: "all", l: t("views:items.allKinds") }, { v: "bug", l: "bug" }, { v: "feat", l: "feat" }]} />
        </div>
      </State>
      <State label={L(t, "fcPickers")}>
        <div className="flex flex-wrap items-center gap-3">
          <DateField value={day} onChange={setDay} className="w-56" aria-label={t("stats:page.fromAria")} />
          <TimeField value={time} onChange={setTime} className="w-28" aria-label={t("common:picker.times")} />
          <NumberField value={num} onValueChange={setNum} min={0} max={50} step={0.5} className="w-32" />
        </div>
      </State>
    </Exhibit>
  )
}
