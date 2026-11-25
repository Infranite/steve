import { Fragment, Suspense, lazy, memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react"
import { cn } from "cn"
import { clockDur } from "@/i18n/format"
import { t, useT } from "@/i18n"
import type { SessionWaiting } from "@/gen"
import {
  ArrowUpRightIcon, ChevronDownIcon, CircleCheckIcon, CircleSlashIcon, CornerDownRightIcon, FileDiffIcon,
  FilePlusIcon, FileTextIcon, GitForkIcon, GlobeIcon, InboxIcon, InfoIcon, PencilIcon, SearchIcon, SquareTerminalIcon, TerminalIcon, WrenchIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Trail, SubSteps, type TrailItem, type Chip } from "@/components/work-trail"
import { ThinkingLoader } from "@/components/affect/thinking-loader"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Spinner } from "@/components/ui/spinner"
import { CodeBlock } from "@/components/ui/code-block"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Markdown } from "@/components/markdown"
import { PermissionRecord } from "@/components/permission"
import { getSession, permissionDecision, botName, type Reply, useDeviceInventory } from "@/store"
import { useDecisionView, useSettledVersion } from "@/data/sync"
import { Brief } from "@/thread/brief"
import { Files } from "@/attach"
import { useTurnUsage } from "@/session/turn-usage"
import { fmtUsageCost, fmtUsage } from "@/stats/format"
import { steveToolIcon } from "@/tool-icons"
import { Segmented, Status, StatusDot, go, openSheet } from "@/ui"
import { ActorAvatar } from "@/components/actor-avatar"

import { getTranscript, loadOlderTranscript, useTranscript, useTranscriptSelector } from "@/data/transcript"
import { isActiveSubtaskStatus, partitionWork, segmentsOf, subtaskOf, subtaskTimes, toolDurationMs, type Seg } from "@/data/transcript-model"
import { SubtaskList, type SubtaskEntry } from "@/components/subtask-list"
import { recordTranscriptOpen } from "@/perf"
import { VirtualList } from "@/components/virtual-list"
import { ScrollArea } from "@/components/affect/scroll-area-clean"

/* ───────── transcript ─────────
 * read like a coding agent's own app: each turn is the message, a work block (thinking, commands, reads, edits —
 * open while it runs, folded to one line once done), the answer, and the files the turn changed */
const base = (p: string) => String(p || "").split("/").filter(Boolean).pop() ?? ""
export const fmtDur = (ms: number) => clockDur(ms)
const clock = (ms: number) => { const s = Math.max(0, Math.floor(ms / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` }
/* one clock for every running timer on the page: two views of the same run tick together and never disagree */
const ticking = new Set<(n: number) => void>()
let tickAt = 0, tickTimer: ReturnType<typeof setInterval> | undefined
/** the current time, ticking every second while `on` */
export function useNow(on: boolean) {
  const [n, setN] = useState(Date.now)
  useEffect(() => {
    if (!on) return
    if (!tickTimer) { tickAt = Date.now(); tickTimer = setInterval(() => { tickAt = Date.now(); ticking.forEach((f) => f(tickAt)) }, 1000) }
    setN(tickAt); ticking.add(setN)
    return () => { ticking.delete(setN); if (!ticking.size) { clearInterval(tickTimer); tickTimer = undefined } }
  }, [on])
  return n
}
/** a session not opened on its Node yet: how long it has been starting, and past two minutes that this is long.
 * Nothing else on the page says a start stands still: there is no turn, no event and no error to show. */
const SLOW_START_MS = 120_000
/** why a session's turn has not gone yet: its device runs as many as its owner set; it goes by itself once one ends */
export const roomLine = (r: SessionWaiting) => r.reason === "node"
  ? t("core:runBar.nodeWait", { detail: r.detail ?? "" })
  : t("core:runBar.room")
export function StartingLine({ since, room }: { since?: number; room?: SessionWaiting | null }) {
  const t = useT(), now = useNow(true), waited = since ? now - since : 0, slow = !room && waited >= SLOW_START_MS
  if (room) return <div className="type-callout text-attention" data-starting="room">{roomLine(room)}{waited >= 10_000 && <span className="ml-1.5 tabular-nums text-muted-foreground">{clock(waited)}</span>}</div>
  return <div className="type-callout text-muted-foreground" data-starting={slow ? "slow" : "ok"}>
    {t("core:transcript.starting")}{waited >= 10_000 && <span className="ml-1.5 tabular-nums">{clock(waited)}</span>}
    {slow && <div className="mt-1 type-footnote text-attention" data-starting-slow>{t("core:transcript.startingSlow")}</div>}
  </div>
}
const TOOLN = ["ToolSearch", "TodoWrite", "Skill"] as const
const toolName = (tool: string) => ((TOOLN as readonly string[]).includes(tool) ? t(`core:transcript.tool.${tool as (typeof TOOLN)[number]}`) : undefined)
type StepView = { I: any; verb: string; subj: string; mono: boolean; full?: string }
/** icon, verb and subject of one tool call: the command (with the agent's own description), the file, the pattern */
function stepView(e: any[]): StepView {
  const tool = String(e[9] ?? ""), file = e[3] && e[3] !== "Steve" ? String(e[3]) : ""
  if (e[1] === "steve") return { I: steveToolIcon(tool) ?? WrenchIcon, verb: e[2], subj: e[10] ?? "", mono: false }
  if (e[1] === "read") return { I: FileTextIcon, verb: t("core:transcript.read"), subj: base(file) || e[2], mono: true, full: file }
  if (e[1] === "edit") return { I: tool === "Write" ? FilePlusIcon : PencilIcon, verb: t(tool === "Write" ? "core:transcript.write" : "core:transcript.edit"), subj: base(file) || e[2], mono: true, full: file }
  if (e[1] === "search") { const web = /^Web/.test(tool); return { I: web ? GlobeIcon : SearchIcon, verb: t(web ? "core:transcript.webSearch" : "core:transcript.search"), subj: e[11] ?? e[3] ?? "", mono: !web, full: e[11] } }
  if (tool && tool !== "Bash") return { I: WrenchIcon, verb: toolName(tool) ?? tool.replace(/^mcp__[^_]+__/, ""), subj: e[10] ?? e[11] ?? "", mono: !e[10] && !!e[11] }
  const cmd = String(e[11] ?? [e[2], e[3]].filter(Boolean).join(" "))
  return { I: TerminalIcon, verb: e[10] || t("core:transcript.run"), subj: cmd.split("\n")[0], mono: true, full: cmd }
}
/** "运行 14 条命令 · 读取 3 个文件 · 编辑 2 个文件 · 思考 1 次" */
function workStats(evs: any[]) {
  const n = { run: 0, read: 0, edit: 0, search: 0, other: 0, sub: 0, th: 0, fail: 0 }
  const files = new Set<string>()
  const { subtaskIds } = partitionWork(evs)
  for (const e of evs) {
    if (e[0] === "th") { n.th++; continue }
    if (e[0] === "sub") {
      const s = subtaskOf(e)
      n.sub++
      if (s?.status === "failed") n.fail++
      continue
    }
    if (e[0] !== "tool") continue
    if (e[15] && subtaskIds.has(String(e[15]))) continue
    if (e[8] === 1) n.fail++
    const v = stepView(e)
    if (v.I === TerminalIcon) n.run++
    else if (e[1] === "read") n.read++
    else if (e[1] === "edit") { n.edit++; if (e[3]) files.add(e[3]) }
    else if (e[1] === "search") n.search++
    else n.other++
  }
  return [n.sub && t("core:transcript.stats.subtask", { count: n.sub }), n.run && t("core:transcript.stats.run", { count: n.run }), n.read && t("core:transcript.stats.read", { count: n.read }), files.size && t("core:transcript.stats.changed", { count: files.size }), n.search && t("core:transcript.stats.search", { count: n.search }), n.other && t("core:transcript.stats.other", { count: n.other }), n.th && t("core:transcript.stats.thought", { count: n.th }), n.fail && t("core:transcript.stats.failed", { count: n.fail })].filter(Boolean) as string[]
}
function Detail({ text }: { text: string }) {
  const lines = String(text || "").split("\n").filter(Boolean)
  if (!lines.length) return null
  return (
    <CodeBlock code={lines.map((l) => l.replace(/^[+-] /, "")).join("\n")} copyable={false} maxHeight={320} className="mt-1 max-w-160 rounded-lg"
      marks={Object.fromEntries(lines.flatMap((l, n): [number, "added" | "removed" | "ok"][] => (l.startsWith("+") ? [[n + 1, "added"]] : l.startsWith("-") ? [[n + 1, "removed"]] : l.startsWith("✓") ? [[n + 1, "ok"]] : [])))} data-tool-detail />
  )
}
/* Steve's tools that tell or ask you something: each stays its own line, never merged */
const SAY = ["steve_update", "steve_ask", "steve_request_approval", "steve_deliver", "steve_publish"]
/** convert one tool/thought/note tuple into a TrailItem */
function stepItem(e: any[], j: number, evs: any[], ts: number[], t1: number | undefined, live: boolean, lastPlan: number): TrailItem {
  const key = String(j)
  if (e[0] === "a") return { t: "note", key, text: e[1] }
  if (e[0] === "th") {
    const at = ts[j] || 0, next = j + 1 < evs.length ? ts[j + 1] : t1
    return { t: "think", key, text: String(e[1] ?? ""), live: live && j === evs.length - 1, t0: at || undefined, ms: at && next ? next - at : undefined }
  }
  const v = stepView(e), tool = String(e[9] ?? ""), state = e[5] === 1 ? "run" : e[8] === 1 ? "fail" : "done", web = /^Web/.test(tool)
  const [verb, group, chip]: [[string, string], string | undefined, Chip | undefined] =
    e[1] === "read" ? [[t("core:trail.live.read"), v.verb], "read", { label: v.subj, title: v.full, mono: true, icon: FileTextIcon }]
    : e[1] === "edit" ? [[t(tool === "Write" ? "core:trail.live.write" : "core:trail.live.edit"), v.verb], "edit", { label: v.subj, title: v.full, mono: true, icon: v.I }]
    : e[1] === "search" ? [[t(web ? "core:trail.live.webSearch" : "core:trail.live.search"), v.verb], "search", { label: v.subj, title: v.full, mono: v.mono, icon: v.I }]
    : v.I === TerminalIcon ? [e[10] ? [v.verb, v.verb] : [t("core:trail.live.run"), v.verb], "run", { label: e[10] || v.subj, title: v.full, mono: !e[10], icon: TerminalIcon }]
    : [[v.verb, v.verb], e[12]?.length || SAY.includes(tool.replace(/^mcp__steve__/, "")) ? undefined : "tool:" + tool, v.subj ? { label: v.subj, mono: v.mono } : undefined]
  const g = group?.startsWith("tool:") ? "tool" : group
  const merged = g ? (calls: number, distinct: number, running: boolean) => g === "tool" ? t("core:trail.group.tool", { name: v.verb, count: calls })
    : t(`core:trail.group.${g}${running ? "Live" : ""}` as "core:trail.group.read", { count: g === "read" || g === "edit" ? distinct : calls }) : undefined
  const plan: [string, boolean][] | undefined = e[12]?.length ? e[12] : undefined
  const diff = e[1] === "edit" && e[6] != null ? <span className="shrink-0 type-footnote font-mono tabular-nums"><span className="text-success">+{e[6]}</span>{e[7] ? <span className="ml-1 text-destructive">−{e[7]}</span> : null}</span> : undefined
  /* the call as a Tool Call Card: the tool's own name, what Steve has of its input (the command, file, query or brief)
   * and what came back, its time from adapter duration_ms or the update that settled it ([13]) */
  const out = String(e[4] ?? "").trim().replace(/^```[a-z]*\n?/, "").replace(/\n?```$/, "").trim(), t0 = ts[j] || undefined
  const observedMs = toolDurationMs(e) ?? (e[13] && t0 && e[13] > t0 ? e[13] - t0 : undefined)
  const card = { name: (tool || String(e[2] ?? "")).replace(/^mcp__steve__/, "").replace(/^mcp__[^_]+__/, ""), icon: <v.I strokeWidth={1.5} aria-hidden />,
    args: e[1] === "steve" ? e[14] ?? (e[10] || undefined) : v.I === TerminalIcon ? v.full ?? v.subj : e[11] ?? (e[3] && e[3] !== "Steve" ? e[3] : undefined),
    result: state === "fail" ? undefined : plan ? (j === lastPlan ? undefined : plan.map(([x, d]) => (d ? "✓ " : "○ ") + x).join("\n")) : out || undefined,
    resultView: plan && j !== lastPlan ? <SubSteps steps={plan} live={false} /> : e[1] === "edit" && out ? <Detail text={out} /> : undefined,
    error: state === "fail" ? out || undefined : undefined, t0, ms: observedMs }
  return { t: "tool", key, state, verb, subj: v.subj, mono: v.mono, full: v.full, group, merged, chip, extra: diff, card,
    below: plan && j === lastPlan ? <SubSteps steps={plan} live={live} /> : undefined, attrs: { kind: e[1] } }
}
/** index-aligned TrailItems for TurnTrace (every event index `j` maps to its TrailItem; subtasks leave undefined) */
function rawTrailOf(evs: any[], ts: number[], t1: number | undefined, live: boolean): (TrailItem | undefined)[] {
  const lastPlan = evs.findLastIndex((e) => e[0] === "tool" && e[12]?.length)
  return evs.map((e, j) => (e[0] === "sub" ? undefined : stepItem(e, j, evs, ts, t1, live, lastPlan)))
}
/** the turn's tuples as steps on the trail: contiguous subtasks coalesce into one unified SubtaskList with nested child steps */
function trailOf(k: string, evs: any[], ts: number[], t1: number | undefined, live: boolean, readOnly = false): TrailItem[] {
  const { rootIndices, childrenBySubtask } = partitionWork(evs)
  const raw = rawTrailOf(evs, ts, t1, live)
  const out: TrailItem[] = []
  let subBuf: SubtaskEntry[] = []
  const flushSubs = () => {
    if (!subBuf.length) return
    const entries = subBuf
    subBuf = []
    const anyRun = live && entries.some((x) => isActiveSubtaskStatus(x.subtask.status))
    const anyFail = entries.some((x) => x.subtask.status === "failed")
    out.push({
      t: "block",
      key: "sub:" + entries.map((x) => x.subtask.id).join(","),
      state: anyRun ? "run" : anyFail ? "fail" : "done",
      body: <SubtaskList sessionId={k} entries={entries} live={live} readOnly={readOnly} />,
      attrs: { kind: "subtask" },
    })
  }
  for (const idx of rootIndices) {
    const e = evs[idx]
    const sub = subtaskOf(e)
    if (sub) {
      const childIndices = childrenBySubtask.get(sub.id) ?? []
      const children = childIndices.map((ci) => raw[ci]).filter((x): x is TrailItem => !!x)
      const lastChildEv = childIndices.length ? evs[childIndices.at(-1)!] : undefined
      const latestChildLabel = lastChildEv && lastChildEv[0] === "tool" ? (() => { const v = stepView(lastChildEv); return v.subj ? `${v.verb} ${v.subj}` : v.verb })() : undefined
      const { started, ended } = subtaskTimes(e)
      subBuf.push({ subtask: sub, started: started ?? ts[idx], ended, children, latestChildLabel })
      continue
    }
    flushSubs()
    const item = raw[idx]
    if (item) out.push(item)
  }
  flushSubs()
  return out
}
/** what the agent is doing right now, in a few words: "运行 npm test", "正在思考" */
function doing(evs: any[]) {
  const runSub = evs.findLast((e) => { const s = subtaskOf(e); return s && isActiveSubtaskStatus(s.status) })
  const run = evs.findLast((e) => e[0] === "tool" && e[5] === 1 && !e[16])
  if (run) { const v = stepView(run); return v.subj ? `${v.verb} ${v.subj}` : v.verb }
  if (runSub) {
    const s = subtaskOf(runSub)!
    const origin = t(`core:subtask.origin.${s.origin}`)
    return s.title ? `${origin} · ${s.title}` : origin
  }
  const last = evs[evs.length - 1]
  return t(!last ? "core:transcript.preparing" : last[0] === "th" ? "core:transcript.thinking" : last[0] === "a" ? "core:transcript.replying" : last[0] === "ask" ? "core:transcript.awaitingApproval" : "core:transcript.processing")
}
/* a folded block stays as the reader left it, across re-renders and remounts */
const workOpen = new Map<string, boolean>(), workView = new Map<string, "steps" | "timeline">()
/* the timeline (Trace Waterfall) is a big component: loaded the first time someone asks for it */
const TurnTrace = lazy(() => import("@/thread/turn-trace"))
const EMPTY_TRAIL: TrailItem[] = []
const EMPTY_RAW_TRAIL: (TrailItem | undefined)[] = []

function WorkClock({ t0, t1, live }: { t0?: number; t1?: number; live: boolean }) {
  const now = useNow(live)
  const ms = live ? now - (t0 ?? now) : (t1 ?? 0) - (t0 ?? 0)
  if (ms <= 0 && !live) return null
  return <span className="shrink-0 tabular-nums text-muted-foreground" data-work-clock>{live ? clock(ms) : fmtDur(ms)}</span>
}

function WorkBlock({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const key = k + ":" + g.seq
  const [open, setOpenS] = useState(() => workOpen.get(key) ?? live)
  useEffect(() => { if (!workOpen.has(key)) setOpenS(live) }, [live, key])
  const setOpen = (v: boolean) => { workOpen.set(key, v); setOpenS(v) }
  const [view, setViewS] = useState(() => workView.get(key) ?? "steps")
  const setView = (v: "steps" | "timeline") => { workView.set(key, v); setViewS(v) }
  const stats = useMemo(() => workStats(g.evs), [g.evs, t])
  const cancelled = g.end === "cancelled"
  const sess = getSession(k)
  const ro = !!sess?.ro
  const hasSteps = g.evs.length > 0
  const items = useMemo(
    () => (open && view === "steps" ? trailOf(k, g.evs, g.at ?? [], g.t1, live, ro) : EMPTY_TRAIL),
    [open, view, k, g.evs, g.at, g.t1, live, ro, t],
  )
  const traceItems = useMemo(
    () => (open && view === "timeline" ? rawTrailOf(g.evs, g.at ?? [], g.t1, live) : EMPTY_RAW_TRAIL),
    [open, view, g.evs, g.at, g.t1, live, t],
  )
  /* a finished turn of an item's session: its tokens and estimated cost, as the Hub's stats keep them */
  const usage = useTurnUsage(k, g.t0, g.t1, live || !g.end)
  return (
    <Collapsible open={open} onOpenChange={setOpen} data-work={live ? "live" : "done"} data-work-view={open ? view : undefined} className="relative anim-in">
      <CollapsibleTrigger className={cn("group/work flex w-full min-w-0 items-center gap-1.5 rounded-lg py-1 text-left type-callout hover:[&>svg]:text-foreground", open && hasSteps && "sm:pr-36")}>
        <ChevronDownIcon className="size-4 shrink-0 -rotate-90 text-muted-foreground transition-transform duration-180 ease-(--ease-out) group-aria-expanded/work:rotate-0" />
        <span className={cn("shrink-0", live ? "text-foreground" : "text-foreground/85")}>{live ? <span className="shimmer">{t("core:transcript.working")}</span> : t(cancelled ? "core:transcript.interrupted" : "core:transcript.handled")}</span>
        <WorkClock t0={g.t0} t1={g.t1} live={live} />
        {stats.length > 0 && <span className="min-w-0 truncate text-muted-foreground">· {stats.join(" · ")}</span>}
        {usage && <span className="ml-auto hidden shrink-0 pl-2 type-footnote tabular-nums text-tertiary sm:inline" data-work-usage>{usage.coverage.total === null ? fmtUsage(usage) : t("views:sessionPanel.turnUsage", { tokens: fmtUsage(usage), usd: fmtUsageCost(usage) })}</span>}
      </CollapsibleTrigger>
      {/* open, the work can be read as steps or on a timeline; the switch sits on the header's line, beside the trigger */}
      {open && hasSteps && <Segmented size="sm" value={view} onChange={setView} label={t("core:trace.view.label")} className="absolute right-0 top-0 z-1 max-sm:hidden"
        options={[["steps", t("core:trace.view.steps")], ["timeline", t("core:trace.view.timeline")]] as const} />}
      <CollapsibleContent>
        {open && hasSteps && <Segmented size="sm" value={view} onChange={setView} label={t("core:trace.view.label")} className="mt-1 sm:hidden"
          options={[["steps", t("core:trace.view.steps")], ["timeline", t("core:trace.view.timeline")]] as const} />}
        {open && <div className="mt-1 flex flex-col">
          {view === "timeline"
            ? <Suspense fallback={<div className="flex items-center gap-2 py-2 type-callout text-muted-foreground"><Spinner className="size-3.5" />{t("common:state.loading")}</div>}><TurnTrace k={k} prompt={g.prompt} evs={g.evs} at={g.at ?? []} items={traceItems} t0={g.t0} t1={g.t1} live={live} cancelled={cancelled} /></Suspense>
            : <Trail items={items} live={live} tail={live ? 12 : undefined} end={live || !g.end ? undefined : cancelled ? { s: "cancel", label: t("core:transcript.interrupted") } : { s: "end", label: t("core:trail.finished") }} />}
        </div>}
      </CollapsibleContent>
    </Collapsible>
  )
}
/** the files a turn changed, like a review summary */
function FilesCard({ files }: { files: [string, number, number][] }) {
  const t = useT()
  const add = files.reduce((n, f) => n + f[1], 0), del = files.reduce((n, f) => n + f[2], 0)
  return (
    <div className="max-w-160 overflow-hidden rounded-xl bg-card shadow-[var(--lift)] anim-in" data-files-changed>
      <div className="flex items-center gap-2 px-4 pt-3 pb-1.5 type-callout"><FileDiffIcon className="size-4 text-muted-foreground" strokeWidth={1.75} /><span className="font-medium text-foreground">{t("core:transcript.filesChanged", { count: files.length })}</span><span className="type-footnote font-mono tabular-nums"><span className={add ? "text-success" : "text-tertiary"}>+{add}</span> <span className={del ? "text-destructive" : "text-tertiary"}>−{del}</span></span></div>
      <ul className="flex flex-col pb-2.5">{files.slice(0, 12).map(([p, a, d]) => (
        <li key={p} className="flex min-w-0 items-center gap-2 px-4 py-0.5 type-footnote font-mono" title={p}><span className="shrink-0 text-foreground">{base(p)}</span><span className="min-w-0 truncate text-muted-foreground">{p.split("/").slice(-4, -1).join("/")}</span><span className="ml-auto shrink-0 tabular-nums"><span className={a ? "text-success" : "text-tertiary"}>+{a}</span> <span className={d ? "text-destructive" : "text-tertiary"}>−{d}</span></span></li>
      ))}{files.length > 12 && <li className="px-4 py-0.5 type-footnote text-muted-foreground">{t("core:transcript.moreFiles", { count: files.length - 12 })}</li>}</ul>
    </div>
  )
}
/* U10: a session whose Node events were set aside never recovers by itself; the way forward is offered right there */
function FaultedLine({ k, text }: { k: string; text: string }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const act = (f: (api: typeof import("@/api").api) => Promise<void>) => {
    setBusy(true)
    void import("@/api").then(m => f(m.api)).catch((e: Error) => { void import("@/store").then(s => s.toast(e.message)) }).finally(() => setBusy(false))
  }
  return (
    <div className="flex flex-col items-center gap-2 type-footnote text-muted-foreground" data-faulted>
      <span className="max-w-[80%] text-center">{text}</span>
      <div className="flex gap-2">
        <Button size="xs" variant="outline" disabled={busy} data-continue-new onClick={() => act(async api => { const r = await api.continueSession(k); go("#/sessions/" + r.session) })}>{t("core:transcript.continueNew")}</Button>
        <Button size="xs" variant="ghost" disabled={busy} data-close-session onClick={() => act(async api => { await api.endSession(k) })}>{t("core:transcript.closeSession")}</Button>
      </div>
    </div>
  )
}

function ForkBtn({ k, at }: { k: string; at: number }) {
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:transcript.fork")} data-fork={at} onClick={() => openSheet("fork", { k, at })}
        className="shrink-0 rounded-full text-muted-foreground opacity-0 transition-opacity duration-120 group-hover/msg:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100" />}><GitForkIcon /></TooltipTrigger>
      <TooltipContent>{t("core:transcript.fork")}</TooltipContent>
    </Tooltip>
  )
}
/** range = [from, to) into the session's tuples: only that part of the conversation (one reply in the work log) */
function TranscriptPermission({ e }: { e: any[] }) {
  useDecisionView(e[2])
  useSettledVersion()
  return <PermissionRecord title={e[1]} id={e[2]} decision={permissionDecision(e[2])} />
}
const SegView = memo(function SegView({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const e = g.evs[0]
  if (g.t === "work") return <WorkBlock k={k} g={g} live={live} />
  if (g.t === "files") return <FilesCard files={g.files!} />
  if (g.t === "u") return (
    <div className="group/msg flex items-center justify-end gap-1 pl-12 anim-in" data-user-msg>
      <ForkBtn k={k} at={g.through} />
      <div className="flex max-w-[85%] flex-col items-end gap-1.5">
        <Files files={e[3]} align="end" />
        {(e[1] !== "（附件）" || !e[3]?.length) && <Bubble variant="muted" align="end" className="max-w-full">
          <BubbleContent className="min-w-0 break-words rounded-2xl px-4 py-2.5 type-body text-foreground">{e[2] && <div className="mb-0.5 type-label text-muted-foreground">{e[2]}</div>}<Markdown text={e[1]} /></BubbleContent>
        </Bubble>}
      </div>
    </div>
  )
  if (g.t === "a") return (
    <div className="group/msg flex items-start gap-1 anim-in" data-agent-msg>
      <div className="min-w-0 flex-1 break-words type-body text-foreground"><Markdown text={e[1]} streaming={live} replyFiles /></div>
      <ForkBtn k={k} at={g.through} />
    </div>
  )
  if (g.t === "q") return (
    <div className="ml-auto max-w-[85%] rounded-2xl bg-muted px-4 py-2.5 anim-in" data-merged>
      <div className="mb-1 flex items-center gap-1.5 type-label text-muted-foreground"><InboxIcon className="size-3.5" />{t("core:transcript.merged", { count: e[1].length })}</div>
      {e[1].map((x: string) => <Markdown key={x} text={x} className="type-body" />)}
    </div>
  )
  if (g.t === "hi") return <div className="flex gap-2.5 rounded-xl bg-muted/60 px-3.5 py-2.5 type-callout text-muted-foreground anim-in" data-handoff><CornerDownRightIcon className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} /><div className="min-w-0 flex-1"><Brief text={e[3] ?? e[1]} label={t("core:transcript.steveBrief")} /><Files files={e[2]} className="mt-1.5" /></div></div>
  if (g.t === "ho") return (
    <div className="flex items-center gap-3 type-footnote text-muted-foreground"><span className="h-px flex-1 bg-border" />
      <button type="button" onClick={() => go("#/sessions/" + e[2])} className="inline-flex items-center gap-1 text-blue hover:underline">{e[1]}<ArrowUpRightIcon className="size-3" /></button><span className="h-px flex-1 bg-border" /></div>
  )
  if (g.t === "ask") return <TranscriptPermission e={e} />
  if (g.t === "away") return <div className="flex items-center gap-2 type-footnote text-muted-foreground" data-away><CornerDownRightIcon className="size-3.5 shrink-0" strokeWidth={1.5} />{t("core:transcript.away")}</div>
  if (g.t === "sys" && e[2] === "faulted") return <FaultedLine k={k} text={e[1]} />
  if (g.t === "sys") return <SysRow text={String(e[1] ?? "")} />
  return null
})
/** an event in the conversation (started, queued, compacted…): a short one is a centred line between rules, truncated
 *  with the whole text on hover; a longer notice is a left-aligned muted fill with an icon, two lines at most */
export function SysRow({ text, className }: { text: string; className?: string }) {
  if (text.length <= 40 && !text.includes("\n")) return (
    <div className={cn("flex items-center gap-3 type-footnote text-muted-foreground", className)} data-sys="line"><span className="h-px flex-1 bg-border/70" /><span className="min-w-0 max-w-[80%] truncate text-center" title={text}>{text}</span><span className="h-px flex-1 bg-border/70" /></div>
  )
  return (
    <div className={cn("flex items-start gap-2 rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground", className)} title={text} data-sys="notice">
      <InfoIcon className="mt-0.5 size-3.5 shrink-0" strokeWidth={1.75} aria-hidden /><span className="min-w-0 flex-1 line-clamp-2 break-words">{text}</span>
    </div>
  )
}
const EMPTY_SEGS: Seg[] = []
function transcriptRowHeight(row: Seg) {
  if (row.t === "sys") return String(row.evs[0][1] ?? "").length <= 40 ? 36 : 60
  return 112
}
export function Transcript({ k, scroll, skip, reply }: {
  k: string; scroll?: RefObject<HTMLDivElement | null>; skip?: Reply[]; reply?: Reply
}) {
  const t = useT(), s = getSession(k), data = useTranscript(k)
  const ownScroll = useRef<HTMLDivElement>(null)
  const [full, setFull] = useState(false)
  const opened = useRef<number | null>(performance.now())
  useLayoutEffect(() => {
    if (!scroll || !data || opened.current === null) return
    recordTranscriptOpen(performance.now() - opened.current)
    opened.current = null
  }, [data, scroll])
  const rows = useMemo(() => {
    if (reply) {
      const turn = data?.turns.find((turn) => turn.start === reply.turn)
      if (!turn) return EMPTY_SEGS
      const from = turn.seqs.findIndex((seq) => seq >= reply.from)
      return from < 0 ? EMPTY_SEGS : segmentsOf(turn, from)
    }
    const away = new Set(skip?.map((r) => r.turn))
    return data?.turns.flatMap((turn) => away.has(turn.start)
      ? [{ id: turn.start + ":away", seq: turn.start, through: turn.start, t: "away", evs: [] }]
      : turn.rows) ?? EMPTY_SEGS
  }, [data, skip, reply])
  const hasEarlier = !!data?.oldest && !reply
  const earlier = useMemo(() => (hasEarlier ? { load: () => loadOlderTranscript(k) } : undefined), [hasEarlier, k])
  const last = rows.findLast((row) => row.t !== "files")
  const running = !!s && s.st === "run" && last?.t !== "away" && (!reply || reply.live)
  const live = running && last?.t === "work" && !last.end ? last : undefined
  const liveAnswer = running && last?.t === "a" ? last : undefined
  const render = useCallback(
    (g: Seg) => <div className="pb-4"><SegView k={k} g={g} live={g === live || g === liveAnswer} /></div>,
    [k, live, liveAnswer],
  )
  if (!s) return null
  if (!data) return <div className="flex items-center gap-2 type-callout text-muted-foreground"><Spinner className="size-3.5" />{t("core:transcript.loading")}</div>
  if (!data.newest && !reply) return s.starting || s.room ? <StartingLine since={s.since} room={s.room} /> : <div className="type-callout text-muted-foreground">{t(s.st === "run" ? "core:transcript.starting" : "core:transcript.empty")}</div>
  const pending = running && !live && <div className="flex" data-transcript-pending><ThinkingLoader variant="plain" size="sm" phases={[s.room ? roomLine(s.room) : t(last?.t === "a" ? "core:transcript.replying" : "core:transcript.startingShort")]} phase={0} className="[--ui-tlc-accent:var(--blue)]!" /></div>
  const list = (viewport: RefObject<HTMLDivElement | null>) => <div className={earlier ? "pt-7" : undefined}>
    <VirtualList rows={rows} scroll={viewport} render={render} end earlier={earlier} estimate={transcriptRowHeight} />
    {pending}
  </div>
  return <div data-transcript={k}>
    {scroll ? list(scroll) : full || reply ? (
      <ScrollArea viewportRef={ownScroll} maxHeight="65dvh">{list(ownScroll)}</ScrollArea>
    ) : <>
      {(rows.length > 4 || earlier) && <button type="button" onClick={() => setFull(true)} className="mb-2 type-footnote text-muted-foreground hover:text-foreground" data-show-all>{t(earlier ? "core:transcript.earlier" : "core:transcript.moreSegs", { count: rows.length - 4 })}</button>}
      {rows.slice(-4).map((g) => <Fragment key={g.id}>{render(g)}</Fragment>)}
      {pending}
    </>}
  </div>
}

function RunBarClock({ run, t0, endAt }: { run: boolean; t0?: number; endAt: number }) {
  const t = useT()
  const now = useNow(run)
  const start = t0 || now
  return <span className="ml-auto shrink-0 type-footnote tabular-nums text-muted-foreground">{run ? clock(now - start) : endAt && t0 ? t("core:runBar.took", { dur: fmtDur(endAt - t0) }) : ""}</span>
}

type RunBarSlice = {
  hasData: boolean
  newest: number
  hasLatest: boolean
  t0: number
  endAt: number
  endStop: string
  stats: string[]
  doingText: string
}

function runBarSliceEqual(a: RunBarSlice, b: RunBarSlice): boolean {
  return a.hasData === b.hasData && a.newest === b.newest && a.hasLatest === b.hasLatest
    && a.t0 === b.t0 && a.endAt === b.endAt && a.endStop === b.endStop && a.doingText === b.doingText
    && a.stats.length === b.stats.length && a.stats.every((x, i) => x === b.stats[i])
}

/** the session's state at a glance, above the composer: running and doing what, waiting on you, or done and how it went.
 *  Only a running or waiting turn keeps its tinted box; a finished one (done, stopped, ended) is one quiet line. On a
 *  phone the bar keeps the state and the time, not the step summary. Stopping the turn is the composer's (its Stop and Esc). */
export function RunBar({ k }: { k: string }) {
  const t = useT()
  const s = getSession(k), run = s?.st === "run"
  const slice = useTranscriptSelector<RunBarSlice>(k, (data) => {
    const latest = data?.turns.at(-1)
    const turn = latest?.ev ?? []
    const endIndex = turn.findLastIndex((e) => e[0] === "end")
    return {
      hasData: !!data,
      newest: data?.newest ? 1 : 0,
      hasLatest: !!latest,
      t0: latest?.events[0]?.ts || 0,
      endAt: latest?.ts[endIndex] ?? 0,
      endStop: endIndex >= 0 ? String(turn[endIndex][1] ?? "") : "",
      stats: latest && !run ? workStats(turn) : [],
      doingText: latest && run ? doing(turn) : "",
    }
  }, runBarSliceEqual, run)
  if (!s || !slice.hasData || (!slice.newest && !run)) return null
  const starting = run && !!s.starting && !slice.hasLatest
  const t0 = (starting && s.since) || slice.t0 || 0
  const tone = run ? "run" : s.st === "wait" ? "wait" : slice.endStop === "cancelled" ? "stop" : s.st === "ended" ? "ended" : "done"
  const label = run && s.room ? roomLine(s.room) : starting ? t("core:transcript.startingShort") : t(`core:runBar.${tone}`)
  const what = run ? null : tone === "wait" ? t("core:runBar.waitWhat") : slice.stats.join(" · ")
  return (
    /* running, the Thinking Loader inside is the live region that speaks each step; the bar itself would say it twice */
    <div role={run ? undefined : "status"} aria-live={run ? undefined : "polite"} data-runbar={tone}
      className={cn("mb-2 flex items-center gap-2.5 rounded-xl type-callout transition-colors duration-180 ease-(--ease-out)",
        tone === "run" ? "min-h-10 bg-blue/8 px-3.5 py-2" : tone === "wait" ? "min-h-10 bg-attention/10 px-3.5 py-2" : "min-h-8 py-1")}>
      {/* running: a Thinking Loader whose label is the step under way, rolling over as the steps change */}
      {run ? <span className="flex min-w-0 flex-1 items-center" data-doing><ThinkingLoader variant="plain" size="sm" phases={[starting || s.room ? label : slice.doingText]} phase={0} className="min-w-0 [--ui-tlc-accent:var(--blue)]! [--ui-tlc-ink:var(--foreground)]!" /></span>
        : tone === "wait" ? <StatusDot s="wait" className="mx-1" /> : tone === "done" ? <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /> : tone === "stop" ? <CircleSlashIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /> : <StatusDot s="ended" className="mx-1" />}
      {!run && <span className={cn("shrink-0 font-medium", tone === "wait" ? "text-attention" : "text-foreground/85")}>{label}</span>}
      {/* plain text in its own box: a flex row cuts text off without the ellipsis */}
      {what && <span className={cn("flex min-w-0 items-baseline gap-1.5 truncate max-sm:hidden", run ? "text-foreground/85" : "text-muted-foreground")} data-doing>{typeof what === "string" ? <span className="min-w-0 truncate">{what}</span> : what}</span>}
      <RunBarClock run={run} t0={t0} endAt={slice.endAt} />
    </div>
  )
}

/* a session as a block inside the item's work log */
const expanded = new Set<string>(), collapsedLive = new Set<string>()
/** a reply as one line of plain text: the Markdown marks of its first lines dropped */
const plainLine = (md: string) => md.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim()
export function SessionBlock({ k, defaultOpen, replies }: { k: string; defaultOpen?: boolean; /** comments' replies the feed shows on their own */ replies?: Reply[] }) {
  const t = useT()
  const [, f] = useState(0)
  const s = getSession(k)
  const live = s?.st === "run" || s?.st === "wait"
  const open = expanded.has(k) ? true : collapsedLive.has(k) ? false : (defaultOpen ?? live)
  const toggle = () => { if (open) { expanded.delete(k); collapsedLive.add(k) } else { expanded.add(k); collapsedLive.delete(k) } f((x) => x + 1) }
  const devices = useDeviceInventory(open && !!s?.node)
  if (!s) return null
  const skip = replies?.filter((r) => r.s === k)
  const lastA = getTranscript(k)?.turns.findLast((turn) => turn.ev.some((e) => e[0] === "a"))?.ev.findLast((e) => e[0] === "a")
  return (
    <Collapsible open={open} onOpenChange={(v) => { if (v !== open) toggle() }} data-session-block={k}>
      <div className="group/sess flex min-h-9 items-center gap-2">
        <span className="flex size-5 shrink-0 items-center justify-center"><SquareTerminalIcon className="size-4 text-muted-foreground" strokeWidth={1.5} /></span>
        <button type="button" onClick={toggle} className="group/toggle flex min-w-0 flex-1 items-center gap-2.5 text-left" aria-expanded={open}>
          <span className="shrink-0 type-callout font-medium">{t("core:session.no", { n: s.n })}</span>
          <Status s={s.st} sess />
          <span className="min-w-0 truncate type-footnote text-muted-foreground">{open ? `${s.node ? devices.find((d) => d.node_id === s.node || d.id === s.node)?.name ?? s.node : t("core:unknownNode")} · ${s.h ?? t("core:unknownHarness")} · ${s.m ?? t("core:unknownModel")} · ${s.dur}${s.ctx ? t("core:session.context", { pct: s.ctx }) : ""}` : s.room ? roomLine(s.room) : lastA ? plainLine(String(lastA[1])) : s.starting ? t("core:transcript.startingShort") : s.st === "run" ? t("core:runBar.run") : ""}</span>
          <span className="ml-auto shrink-0 type-footnote text-muted-foreground group-hover/toggle:text-foreground">{t(open ? "core:session.collapse" : "core:session.expand")}</span>
        </button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + k)} data-enter-session={k} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      <CollapsibleContent>
        <div className="mb-2 ml-2.5 mt-1 border-l pl-4.5">{open && <Transcript k={k} skip={skip} />}</div>
      </CollapsibleContent>
    </Collapsible>
  )
}

/* the agent's answer to one comment, right under the comment; opens to that turn of the session.
 * Folded, its header sums the work up; open, the work block inside does, so the header says only whose reply it is.
 * Whether it is open outlives a remount: lines written while it runs shift its place in the feed */
const replyOpen = new Set<string>()
export function ReplyBlock({ r }: { r: Reply }) {
  const t = useT()
  const key = r.s + ":" + r.turn
  const [open, setOpenS] = useState(() => replyOpen.has(key))
  const setOpen = (v: boolean) => { if (v) replyOpen.add(key); else replyOpen.delete(key); setOpenS(v) }
  const s = getSession(r.s)
  if (!s) return null
  const who = s.b ?? "agent"
  if (!r.text && !r.tools) return r.live ? <div className="ml-7 flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-reply-live><Spinner className="size-3" /><span className="shimmer">{t("core:session.botReplying", { bot: botName(who) })}</span></div> : null
  /* counted as the work block counts them: commands, files changed */
  const turn = getTranscript(r.s)?.turns.find((turn) => turn.start === r.turn)
  const stats = open || !turn ? [] : workStats(turn.ev)
  return (
    <div className="mb-1.5 ml-2.5 border-l pl-4.5" data-reply={r.s}>
      <div className="flex min-h-7 min-w-0 items-center gap-1.5 type-footnote text-muted-foreground">
        <ActorAvatar actorId={who} size={16} /><span className="shrink-0 font-medium text-foreground/80">{botName(who)}</span>
        <span className="shrink-0">{t("core:session.replyNo", { n: s.n })}</span>
        {stats.length > 0 && <span className="min-w-0 truncate" data-reply-stats>· {stats.join(" · ")}</span>}
        {r.live && !open && <Spinner className="size-3 shrink-0" />}
        <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} className="ml-auto shrink-0 hover:text-foreground" data-reply-toggle>{t(open ? "core:session.collapseWork" : "core:session.expandWork")}</button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + r.s)} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleContent>
          <div className="pb-2 pt-1">{open && <Transcript k={r.s} reply={r} />}</div>
        </CollapsibleContent>
      </Collapsible>
      {!open && r.text ? <div className="line-clamp-6 whitespace-pre-wrap break-words pb-1 type-body text-foreground animate-[steve-fade_180ms_var(--ease-out)_both]" data-reply-text><Markdown text={r.text} streaming={r.live} replyFiles /></div> : null}
    </div>
  )
}
