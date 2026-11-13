/* The work behind an answer, drawn the way it happened: one line down the left with a marker per step — a spinner while
 * the step runs, a quiet dot once it is done, a check at the end. A step's verb changes tense when it ends ("Reading" →
 * "Read"), calls of one kind in a row merge into one line with their subjects as chips, thinking streams in while it
 * runs and folds to one line after, and a plan shows its own steps in a small card. Used by Steve's conversation and by
 * an agent's run alike; it only knows what a step is called, its state and its detail, never which harness made it. */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { CheckIcon, ChevronRightIcon, CircleCheckIcon, CircleIcon, CircleSlashIcon, XIcon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Markdown } from "@/components/markdown"
import { Spinner } from "@/components/ui/spinner"
import { useT } from "@/i18n"
import { ToolCallCard } from "@/components/affect/tool-call-card"

export type StepState = "run" | "done" | "fail"
export type Chip = { label: string; title?: string; icon?: any; mono?: boolean }
/** one tool call (or any action) on the trail */
export type ToolStep = {
  t: "tool"; key: string; state: StepState
  /** the verb while it runs and once it has ended */
  verb: [string, string]
  subj?: string; mono?: boolean; full?: string; icon?: any
  /** calls with the same group, next to each other, merge into one line; `merged` names that line */
  group?: string; merged?: (calls: number, distinct: number, running: boolean) => string; chip?: Chip
  /** after the subject: a diff count, a time */
  extra?: ReactNode
  /** opened by a click: the command and its output, the diff */
  body?: ReactNode
  /** always shown under the line: a plan's steps */
  below?: ReactNode
  attrs?: Record<string, string | undefined>
  /** the call as a Tool Call Card (an agent's transcript): the tool's own name, what went in and came out, its time.
   *  The line's words (verb, subject, diff) become the card's summary; `body` is not used then */
  card?: { name: string; icon?: ReactNode; args?: unknown; result?: unknown; resultView?: ReactNode; error?: string; t0?: number; ms?: number }
}
export type TrailItem = ToolStep
  | { t: "think"; key: string; text: string; live: boolean; /** how long it took; while it runs, when it began */ ms?: number; t0?: number; attrs?: Record<string, string | undefined> }
  | { t: "note"; key: string; text: string }
  | { t: "line"; key: string; state: StepState; label: ReactNode; detail?: ReactNode; body?: ReactNode; attrs?: Record<string, string | undefined> }

/* ───────── markers on the line ───────── */
function Marker({ s }: { s: StepState | "end" | "cancel" | "card" }) {
  return (
    <span className={cn("absolute left-0 flex size-4 items-center justify-center", s === "card" ? "top-1.5" : "top-0.5")} aria-hidden data-trail-marker={s}>
      {s === "run" ? <Spinner className="size-3.5 text-foreground/70" />
        : s === "fail" ? <XIcon className="size-3.5 text-destructive" strokeWidth={2.25} />
        : s === "end" ? <CircleCheckIcon className="size-3.5 text-success" strokeWidth={1.75} />
        : s === "cancel" ? <CircleSlashIcon className="size-3.5 text-muted-foreground" strokeWidth={1.75} />
        : <span className="size-1.5 rounded-full bg-tertiary/70" />}
    </span>
  )
}
/** a step's place on the line: the marker, and the line on to the next step (not past the last) */
function Row({ s, children, className, ...rest }: { s: StepState | "end" | "cancel" | "card"; children: ReactNode; className?: string; [k: string]: any }) {
  return (
    <li className={cn("relative min-w-0 pl-6 before:absolute before:left-[7.5px] before:top-[21px] before:-bottom-px before:w-px before:bg-foreground/12 last:before:hidden", className)} {...rest}>
      <Marker s={s} />{children}
    </li>
  )
}
const HEAD = "flex min-h-5 w-full min-w-0 items-center gap-1.5 py-0.5 text-left type-callout text-muted-foreground"
const Chevron = () => <ChevronRightIcon className="size-3.5 shrink-0 opacity-0 transition duration-180 ease-(--ease-out) group-hover/row:opacity-100 group-aria-expanded/row:rotate-90 group-aria-expanded/row:opacity-100" />
/** a head that opens a body when there is one */
function Fold({ head, body, open: open0 = false, className }: { head: ReactNode; body?: ReactNode; open?: boolean; className?: string }) {
  if (!body) return <div className={cn(HEAD, className)}>{head}</div>
  return (
    <Collapsible defaultOpen={open0}>
      <CollapsibleTrigger className={cn(HEAD, "group/row hover:text-foreground", className)}>{head}<Chevron /></CollapsibleTrigger>
      <CollapsibleContent>{body}</CollapsibleContent>
    </Collapsible>
  )
}

/* ───────── pieces ───────── */
/** subjects as chips, the first few and how many more */
export function Chips({ chips, max = 4 }: { chips: Chip[]; max?: number }) {
  const t = useT(), shown = chips.slice(0, max)
  return (
    <div className="mt-1 mb-0.5 flex flex-wrap items-center gap-1.5" data-trail-chips>
      {shown.map((c, n) => (
        <span key={n} title={c.title ?? c.label} className={cn("inline-flex h-6 max-w-60 items-center gap-1 rounded-md bg-muted/80 px-1.5 type-footnote text-foreground/80", c.mono && "font-mono")}>
          {c.icon && <c.icon className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.5} />}<span className="min-w-0 truncate">{c.label}</span>
        </span>))}
      {chips.length > max && <span className="type-footnote text-muted-foreground">{t("core:trail.more", { count: chips.length - max })}</span>}
    </div>
  )
}
/** a plan's steps in a small card: done ones checked, the next one turning while the work runs */
export function SubSteps({ steps, live }: { steps: [string, boolean][]; live: boolean }) {
  const next = steps.findIndex(([, done]) => !done)
  return (
    <ul className="mt-1.5 mb-1 flex max-w-140 flex-col rounded-xl bg-muted/60 py-1.5" data-plan>
      {steps.map(([text, done], n) => (
        <li key={n} className="flex min-w-0 items-start gap-2.5 px-3.5 py-1.5 type-callout" data-plan-step={done ? "done" : n === next && live ? "run" : "todo"}>
          <span className="mt-0.5 flex size-4 shrink-0 items-center justify-center">{done
            ? <CheckIcon className="size-3.5 text-muted-foreground" strokeWidth={2} />
            : n === next && live ? <Spinner className="size-3.5 text-muted-foreground" /> : <CircleIcon className="size-3.5 text-tertiary" strokeWidth={1.75} />}</span>
          <span className={cn("min-w-0 break-words", done ? "text-foreground" : "text-muted-foreground")}>{text}</span>
        </li>))}
    </ul>
  )
}
/** thinking as it streams: the latest lines, the older ones fading out at the top */
function Stream({ text }: { text: string }) {
  const ref = useRef<HTMLDivElement>(null), [over, setOver] = useState(false)
  useLayoutEffect(() => { const el = ref.current; if (!el) return; el.scrollTop = el.scrollHeight; setOver(el.scrollHeight > el.clientHeight + 1) }, [text])
  return (
    <div ref={ref} className={cn("mt-0.5 mb-1 max-h-36 overflow-hidden break-words text-muted-foreground", over && "[mask-image:linear-gradient(to_bottom,transparent,black_3rem)]")} data-thought-stream>
      <Markdown text={text} className="type-callout" />
    </div>
  )
}
/** the time now, again each second while `on` */
export function useTick(on: boolean) {
  const [now, setNow] = useState(Date.now())
  useEffect(() => { if (!on) return; setNow(Date.now()); const h = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(h) }, [on])
  return now
}
const sec = (ms: number) => Math.max(0, Math.round(ms / 1000))

/* ───────── steps ───────── */
function ToolRow({ s, live }: { s: ToolStep; live: boolean }) {
  const t = useT(), running = live && s.state === "run", st: StepState = s.state === "run" && !live ? "done" : s.state
  if (s.card) return <CardRow s={s} running={running} st={st} />
  const head = <>
    <span className={cn("shrink-0", running && "shimmer", st === "fail" && "text-destructive")}>{s.verb[running ? 0 : 1]}</span>
    {s.subj && <span className={cn("min-w-0 truncate text-foreground/85", s.mono && "font-mono type-footnote")} title={s.full ?? s.subj}>{s.subj}</span>}
    {s.extra}
    {st === "fail" && <span className="shrink-0 rounded-md bg-destructive/10 px-1.5 type-label text-destructive">{t("core:transcript.failed")}</span>}
  </>
  return (
    <Row s={st} data-step data-step-kind={s.attrs?.kind} data-running={running || undefined} data-failed={st === "fail" || undefined} {...attrsOf(s.attrs)}>
      <Fold head={head} body={s.body && <div className="pb-1">{s.body}</div>} />
      {s.below}
    </Row>
  )
}
/** a call as a Tool Call Card on the line: its status, timer and duration are the card's, so the marker stays a dot */
function CardRow({ s, running, st }: { s: ToolStep; running: boolean; st: StepState }) {
  const c = s.card!
  const summary = <>
    <span className={cn(running && "shimmer", st === "fail" && "text-destructive")}>{s.verb[running ? 0 : 1]}</span>
    {s.subj && <span className={cn("ml-1.5 text-foreground/85", s.mono && "font-mono type-footnote")}>{s.subj}</span>}
    {s.extra && <span className="ml-1.5 inline-flex">{s.extra}</span>}
  </>
  return (
    <Row s="card" data-step data-step-kind={s.attrs?.kind} data-running={running || undefined} data-failed={st === "fail" || undefined} data-tool-card={c.name} {...attrsOf(s.attrs)} className="-mt-0.5">
      <ToolCallCard variant="line" name={c.name} icon={c.icon} summary={summary} status={running ? "running" : st === "fail" ? "error" : "success"}
        args={c.args} result={c.result} resultView={c.resultView} error={c.error} startedAt={running ? c.t0 : undefined} duration={running ? undefined : c.ms} />
      {s.below}
    </Row>
  )
}
const attrsOf = (a?: Record<string, string | undefined>) => Object.fromEntries(Object.entries(a ?? {}).filter(([k]) => k !== "kind").map(([k, v]) => ["data-" + k, v]))
/** calls of one kind in a row: one line, its subjects as chips; opened, each call */
function GroupRow({ g, live }: { g: ToolStep[]; live: boolean }) {
  const t = useT(), running = live && g.some((s) => s.state === "run"), fail = g.filter((s) => s.state === "fail").length
  const chips: Chip[] = [], seen = new Set<string>()
  for (const s of g) { const c = s.chip ?? (s.subj ? { label: s.subj, mono: s.mono, title: s.full } : undefined); if (c && !seen.has(c.label)) { seen.add(c.label); chips.push(c) } }
  const label = g[0].merged!(g.length, chips.length, running)
  return (
    <Row s={running ? "run" : "done"} data-step data-step-group={g[0].group} data-step-count={g.length}>
      <Collapsible>
        <CollapsibleTrigger className={cn(HEAD, "group/row hover:text-foreground")}>
          <span className={cn("shrink-0", running && "shimmer")}>{label}</span>
          {fail > 0 && <span className="shrink-0 text-destructive">· {t("core:transcript.stats.failed", { count: fail })}</span>}
          <Chevron />
        </CollapsibleTrigger>
        {chips.length > 0 && <Chips chips={chips} />}
        <CollapsibleContent><ol className="mt-1 flex flex-col pb-1">{g.map((s) => <ToolRow key={s.key} s={s} live={live} />)}</ol></CollapsibleContent>
      </Collapsible>
    </Row>
  )
}
function ThinkRow({ s, live }: { s: Extract<TrailItem, { t: "think" }>; live: boolean }) {
  const t = useT(), on = live && s.live, now = useTick(on)
  const ms = on ? now - (s.t0 ?? now) : s.ms
  if (on) return (
    <Row s="run" data-step data-step-kind="th" data-thinking {...attrsOf(s.attrs)}>
      <div className={HEAD}><span className="shimmer">{t("core:trail.thinking")}</span><span className="tabular-nums">{t("common:dur.seconds", { count: sec(ms ?? 0) })}</span></div>
      {s.text.trim() && <Stream text={s.text} />}
    </Row>
  )
  return (
    <Row s="done" data-step data-step-kind="th" {...attrsOf(s.attrs)}>
      <Fold head={<span>{ms != null && ms >= 1000 ? t("core:trail.thoughtFor", { time: t("common:dur.seconds", { count: sec(ms) }) }) : t("core:trail.thought")}</span>}
        body={s.text.trim() ? <div className="mb-1.5 min-w-0 break-words text-muted-foreground" data-thought-body><Markdown text={s.text} className="type-callout" /></div> : undefined} />
    </Row>
  )
}

/** merge runs of grouped calls; a single call stays itself */
function runs(items: TrailItem[]): (TrailItem | ToolStep[])[] {
  const out: (TrailItem | ToolStep[])[] = []
  for (const x of items) {
    const prev = out[out.length - 1]
    if (x.t === "tool" && x.group && x.merged) {
      if (Array.isArray(prev) && prev[0].group === x.group) { prev.push(x); continue }
      if (prev && !Array.isArray(prev) && prev.t === "tool" && prev.group === x.group) { out[out.length - 1] = [prev, x]; continue }
    }
    out.push(x)
  }
  return out
}

/** the steps on the line; `end` closes it once the work is over */
export function Trail({ items, live, end, tail, className }: { items: TrailItem[]; live: boolean; end?: { s: "end" | "cancel" | "fail"; label: ReactNode }; /** only the last lines, the rest behind a button */ tail?: number; className?: string }) {
  const t = useT(), [all, setAll] = useState(false), rs = runs(items), cut = tail && !all && rs.length > tail ? rs.length - tail : 0
  return (
    <ol className={cn("flex min-w-0 flex-col", className)} data-trail>
      {cut > 0 && <li className="pl-6"><button type="button" onClick={() => setAll(true)} className="py-0.5 type-footnote text-muted-foreground hover:text-foreground" data-show-steps>{t("core:transcript.moreSteps", { count: cut })}</button></li>}
      {rs.slice(cut).map((x) => Array.isArray(x) ? <GroupRow key={x[0].key} g={x} live={live} />
        : x.t === "tool" ? <ToolRow key={x.key} s={x} live={live} />
        : x.t === "think" ? <ThinkRow key={x.key} s={x} live={live} />
        : x.t === "note" ? <Row key={x.key} s="done" data-narration><div className="py-0.5 type-callout text-foreground/80"><Markdown text={x.text} /></div></Row>
        : <Row key={x.key} s={live && x.state === "run" ? "run" : x.state === "fail" ? "fail" : "done"} {...attrsOf(x.attrs)}><Fold head={<>{x.label}{x.detail}</>} body={x.body} /></Row>)}
      {end && <Row s={end.s === "fail" ? "fail" : end.s} data-trail-end={end.s}><div className={HEAD}>{end.label}</div></Row>}
    </ol>
  )
}
