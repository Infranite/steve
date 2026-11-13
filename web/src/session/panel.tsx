/* The session page's side panel (a sheet on a phone): everything about the session that is not its transcript, from
 * what the Hub reports and nothing else.
 *   · the session at work (affectUI Agent Monitor): its harness and model, its state, the bill so far, and each turn
 *     as a step on the time strip – what opened it, how long it took, its tools and tokens
 *   · the context (affectUI Context Window): how full the window is, in the tokens the harness reported. Harnesses
 *     report one total, not its parts, so the card shows that one part rather than invented ones
 *   · what it consumed (affectUI Compact Breakdown): tokens by kind, the estimated cost, the cache hit rate and the
 *     harness's own figure
 *   · time: how old the session is, how long it actually worked, its turns and their average, when it last did anything
 *   · what it produced: the outputs it delivered to its item and the files it changed
 *   · where it sits: its item, what that came from, what came from it, the item's other sessions – a list that reads
 *     as well with one entry as with twenty
 *   · the knowledge it was given, and why; and where it runs, with the way to resume it in its harness */
import { useMemo, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ArrowUpRightIcon, ChevronRightIcon, CornerDownRightIcon, FileDiffIcon, GitForkIcon, InboxIcon, MessageSquareIcon, NetworkIcon, PackageIcon, SparklesIcon } from "lucide-react"
import { api, type RItemDetail } from "@/api"
import { stats, type ItemDetail, type SessionTotals } from "@/api/stats"
import { AgentMonitor, type AgentRunState, type AgentRunStep } from "@/components/affect/agent-monitor-clean"
import { CompactBreakdown } from "@/components/affect/compact-breakdown"
import { ContextWindow } from "@/components/affect/context-window-clean"
import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { useT } from "@/i18n"
import { clockDur, longDur, monthDayTime, nearTime, pastTime } from "@/i18n/format"
import { OutDialog, type OutOpen } from "@/item-outputs"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { SessionKnowledge } from "@/knowledge/parts"
import { ResumeCard } from "@/resume"
import { fmtEst, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { briefHead, interrupt, st, type Item, type Out, type Sess } from "@/store"
import { BotName, go, ItemNo, openSheet, SectionLabel, Status, StatusDot, useNow } from "@/ui"
import { ProjectChip } from "@/view-shared"
import { filesOf, outputsOf, turnsOf, withUsage, workedMs, type Turn } from "./data"

const find = (id: string | null | undefined) => (id ? st.items.find((x) => x.id === id) : undefined)
/** a card on the panel: Steve's card surface, the same as the affectUI cards beside it */
const Card = ({ children, className, ...rest }: { children: ReactNode; className?: string; [k: `data-${string}`]: unknown }) =>
  <div className={cn("rounded-xl bg-card p-4 shadow-[var(--lift)]", className)} {...rest}>{children}</div>

/** the session's turns with their usage: the transcript says when each ran, the item's stats what each used */
export function useTurns(k: string) {
  const s = st.S[k], item = s?.item ?? null, run = s?.st === "run"
  const [d] = useStats<ItemDetail | null>(() => (item ? stats.item(item) : Promise.resolve(null)), [item, run ? 0 : s?.at], !run)
  return useMemo(() => s ? withUsage(turnsOf(s.ev, s.ts, run, briefHead, !!s.ro), d.s === "ok" ? d.v?.turns : undefined, k) : [], [s?.ev, s?.ts, run, d, k])
}

/* ───────── the session at work ───────── */
const ICON = { u: MessageSquareIcon, hi: CornerDownRightIcon, q: InboxIcon, "": SparklesIcon } as const
function Monitor({ s, turns, tot }: { s: Sess; turns: Turn[]; tot?: SessionTotals }) {
  const t = useT()
  const run = s.st === "run", now = useNow(run)
  /* open between turns or ended, a session has not "finished" anything: idle, in the gray of the sessions list */
  const state: AgentRunState = run ? "running" : s.st === "wait" ? "waiting" : "idle"
  const live = turns.find((x) => x.live)
  const steps: AgentRunStep[] = turns.map((x) => {
    const I = ICON[x.by], u = x.usage
    const facts = [t("views:sessionPanel.turnTools", { count: x.tools }), x.fails ? t("views:sessionPanel.turnFails", { count: x.fails }) : null,
      u?.total ? t("views:sessionPanel.turnTokens", { tokens: fmtN(u.total) }) : null, u && u.est > 0 ? fmtEst(u.est, u.unpriced_tokens) : null].filter(Boolean).join(" · ")
    return {
      id: String(x.idx), label: x.prompt || t("views:sessionPanel.turnN", { n: x.n }), icon: <I strokeWidth={1.75} />,
      state: x.live ? "running" : "done", since: x.live ? x.start : undefined, took: x.end && x.end > x.start ? x.end - x.start : undefined,
      tool: t("views:sessionPanel.turnOf", { n: x.n, by: t(`views:sessionPanel.by.${x.by || "none"}`) }), detail: facts,
      output: x.stop && x.stop !== "end_turn" ? t("views:sessionPanel.stopped", { reason: x.stop }) : undefined,
    }
  })
  const u = tot?.usage
  const cost = u ? (u.total && u.est > 0 ? u.est : u.reported > 0 ? u.reported : undefined) : undefined
  const worked = workedMs(turns, now)
  /* an imported transcript has no times of its own: its turns are counted, not timed */
  const summary = turns.length
    ? worked ? t("views:sessionPanel.summary", { worked: longDur(worked), count: turns.length }) : t("views:sessionPanel.summaryUntimed", { count: turns.length })
    : s.starting ? t("views:sessionPanel.starting") : t("views:sessionPanel.noTurns")
  return (
    <div data-session-monitor={state}>
      <AgentMonitor fill state={state} stateLabel={state === "idle" ? t(`core:sessStatus.${s.st}`) : undefined}
        agent={{ name: s.h ?? t("core:unknownHarness"), role: s.m, mark: <HarnessMark harness={s.harness ?? s.h} /> }}
        steps={steps} since={live?.start ?? turns[0]?.start ?? s.since} until={run ? undefined : turns.at(-1)?.end ?? s.at}
        cost={cost} summary={summary} stepNoun={t("views:sessionPanel.turn")}
        current={run && !live ? { label: s.starting ? t("views:sessionPanel.starting") : t("core:runBar.run") } : undefined}
        ask={s.st === "wait" ? { title: t("views:sessionPanel.needsYou") } : undefined}
        onCancel={run && !s.ro ? () => interrupt(s.id) : undefined} />
    </div>
  )
}

/* ───────── context ───────── */
function Context({ s }: { s: Sess }) {
  const t = useT()
  if (!s.contextKnown || !s.ctxSize) return (
    <section data-session-context="unreported"><SectionLabel>{t("views:sessionPanel.context")}</SectionLabel>
      <Card><p className="type-callout text-muted-foreground">{t("views:sessionPanel.ctxUnreported", { h: s.h ?? t("core:unknownHarness") })}</p></Card></section>
  )
  return (
    <section data-session-context={s.ctx}>
      <SectionLabel>{t("views:sessionPanel.context")}</SectionLabel>
      <ContextWindow className="block! max-w-none!" title={t("views:sessionPanel.ctxTitle")} limit={s.ctxSize} model={s.m} modelIcon={<ModelMark model={s.m} />}
        parts={[{ id: "used", label: t("views:sessionPanel.ctxUsed"), tokens: s.ctxUsed ?? 0, locked: true }]} cells={180} columns={30} />
      <p className="mt-2 px-1 type-footnote text-tertiary">{t("views:sessionPanel.ctxWhole", { h: s.h ?? t("core:unknownHarness") })}</p>
    </section>
  )
}

/* ───────── consumption ───────── */
function Usage({ tot }: { tot: SessionTotals }) {
  const t = useT()
  const u = tot.usage
  const items = ([["input", u.input], ["cache_read", u.cache_read], ["cache_write", u.cache_write], ["output", u.output]] as const)
    .filter(([, v]) => v > 0).map(([id, value]) => ({ id, label: t(`views:sessionPanel.tok.${id}`), value }))
  const parts = [u.total ? t("views:sessionPanel.est", { usd: fmtEst(u.est, u.unpriced_tokens) }) : null, u.hit_rate != null ? t("views:sessionPanel.hit", { pct: fmtPct(u.hit_rate) }) : null,
    u.reported > 0 && Math.abs(u.reported - u.est) >= 0.01 ? t("views:sessionPanel.reported", { usd: fmtUsd(u.reported) }) : null].filter(Boolean) as string[]
  return (
    <section data-session-usage={u.total}>
      <SectionLabel right={<a href="#/stats" className="type-footnote text-muted-foreground hover:text-foreground">{t("views:sessionPanel.toStats")}</a>}>{t("views:sessionPanel.usage")}</SectionLabel>
      {items.length ? <Card className="p-0">
        <CompactBreakdown bare items={items} title={t("views:sessionPanel.usageTitle", { count: tot.turns })} totalLabel={t("views:sessionPanel.tokensTotal")} format={(n) => fmtN(Math.round(n))}
          footer={parts.length ? <span className="whitespace-normal" data-session-cost>{parts.join(" · ")}</span> : null} rowProps={(x) => ({ "data-usage-part": x.key })} />
      </Card> : <Card><p className="type-callout text-muted-foreground">{u.reported > 0 ? t("views:sessionPanel.reportedOnly", { usd: fmtUsd(u.reported) }) : t("views:sessionPanel.noUsage")}</p></Card>}
    </section>
  )
}

/* ───────── time ───────── */
function Time({ s, turns }: { s: Sess; turns: Turn[] }) {
  const t = useT()
  const run = s.st === "run", now = useNow(run)
  const end = run ? now : (s.at ?? now), worked = workedMs(turns, now), done = turns.filter((x) => x.end)
  const avg = done.length ? done.reduce((n, x) => n + (x.end! - x.start), 0) / done.length : 0
  const longest = done.reduce<Turn | null>((m, x) => (!m || x.end! - x.start > m.end! - m.start ? x : m), null)
  const cells: [string, ReactNode, ReactNode?][] = s.ro ? [
    [t("views:sessionPanel.imported"), s.since ? nearTime(s.since, now) : "—", t("views:sessionPanel.importedSub")],
    [t("views:sessionPanel.turns"), String(turns.length)],
  ] : [
    [t("views:sessionPanel.age"), s.since ? longDur(end - s.since) : "—", s.since ? t("views:sessionPanel.since", { time: nearTime(s.since, now) }) : undefined],
    [t("views:sessionPanel.worked"), turns.length ? longDur(worked) : "—", s.since && end > s.since && worked ? t("views:sessionPanel.workedShare", { pct: fmtPct(Math.min(1, worked / (end - s.since))) }) : undefined],
    [t("views:sessionPanel.turns"), String(turns.length), avg ? t("views:sessionPanel.avg", { dur: clockDur(avg) }) : undefined],
    [t("views:sessionPanel.last"), s.at ? pastTime(s.at, now) : "—", longest ? t("views:sessionPanel.longest", { dur: clockDur(longest.end! - longest.start) }) : undefined],
  ]
  return (
    <section data-session-time>
      <SectionLabel>{t("views:sessionPanel.time")}</SectionLabel>
      <Card className="grid grid-cols-2 gap-x-4 gap-y-3.5 p-4">
        {cells.map(([k, v, sub]) => <div key={k} className="min-w-0"><div className="type-footnote text-muted-foreground">{k}</div><div className="truncate type-headline tabular-nums">{v}</div>{sub && <div className="line-clamp-2 type-footnote text-tertiary">{sub}</div>}</div>)}
      </Card>
    </section>
  )
}

/* ───────── outputs ───────── */
function Outputs({ s, det }: { s: Sess; det: RItemDetail | null }) {
  const t = useT()
  const [view, setView] = useState<OutOpen | null>(null)
  const [allFiles, setAllFiles] = useState(false), [allOuts, setAllOuts] = useState(false)
  const files = useMemo(() => filesOf(s.ev), [s.ev])
  const arts = det ? outputsOf(s.id, det.artifacts, det.timeline, det.sessionIds) : []
  /* the results first, the records of the work after them; the newest first in each */
  const outs: Out[] = arts.map((a): Out => [a.kind, a.title, a.location, a.id, a.created, det?.artifactRoles?.[a.id] as Out[5]])
    .sort((a, b) => Number(a[5] === "record") - Number(b[5] === "record") || (b[4] ?? 0) - (a[4] ?? 0))
  const outsShown = allOuts ? outs : outs.slice(0, 4)
  const add = files.reduce((n, f) => n + f[1], 0), del = files.reduce((n, f) => n + f[2], 0)
  const shown = allFiles ? files : files.slice(0, 5)
  return (
    <section data-session-outputs={outs.length}>
      <SectionLabel>{t("views:sessionPanel.outputs")}</SectionLabel>
      {!outs.length && !files.length ? <Card><p className="type-callout text-muted-foreground">{s.loaded ? t("views:sessionPanel.noOutputs") : t("common:state.loading")}</p></Card> : <div className="flex flex-col gap-2">
        {outsShown.map((o) => (
          <button key={o[3]} type="button" onClick={() => setView({ o })} className="group flex w-full items-start gap-3 rounded-xl bg-card p-3.5 text-left shadow-[var(--lift)] transition-shadow duration-180 hover:shadow-[var(--lift-hover)]" data-session-out={o[3]}>
            <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><PackageIcon className="size-4" strokeWidth={1.75} /></span>
            <span className="min-w-0 flex-1"><span className="line-clamp-2 type-callout font-medium text-foreground">{o[1]}</span>
              <span className="mt-0.5 flex min-w-0 items-center gap-1.5 type-footnote text-muted-foreground"><span className="shrink-0 rounded-md bg-muted px-1.5">{o[0]}</span>{o[5] === "record" && <span className="shrink-0">{t("shell:outs.record")}</span>}{o[4] ? <span className="truncate">{t("views:sessionPanel.deliveredAt", { time: monthDayTime(o[4]) })}</span> : null}</span></span>
            <ChevronRightIcon className="mt-2 size-4 shrink-0 text-tertiary transition-transform group-hover:translate-x-0.5" />
          </button>))}
        {outs.length > 4 && <button type="button" onClick={() => setAllOuts(!allOuts)} className="self-start px-1 type-footnote text-muted-foreground hover:text-foreground" data-session-outs-more>{allOuts ? t("views:sessionPanel.less") : t("views:sessionPanel.more", { count: outs.length - 4 })}</button>}
        {files.length > 0 && <Card className="p-0 py-2.5" data-session-files={files.length}>
          <div className="flex items-center gap-2 px-4 pb-1.5 type-callout"><FileDiffIcon className="size-4 text-muted-foreground" strokeWidth={1.75} /><span className="font-medium">{t("core:transcript.filesChanged", { count: files.length })}</span>
            <span className="ml-auto type-footnote font-mono tabular-nums"><span className={add ? "text-success" : "text-tertiary"}>+{add}</span> <span className={del ? "text-destructive" : "text-tertiary"}>−{del}</span></span></div>
          <ul className="flex flex-col">{shown.map(([p, a, d]) => (
            <li key={p} className="flex min-w-0 items-center gap-2 px-4 py-0.5 type-footnote font-mono" title={p}><span className="min-w-0 truncate text-foreground">{p.split("/").pop()}</span>
              <span className="ml-auto shrink-0 tabular-nums"><span className={a ? "text-success" : "text-tertiary"}>+{a}</span> <span className={d ? "text-destructive" : "text-tertiary"}>−{d}</span></span></li>))}</ul>
          {files.length > 5 && <button type="button" onClick={() => setAllFiles(!allFiles)} className="mt-1 px-4 type-footnote text-muted-foreground hover:text-foreground" data-session-files-more>{allFiles ? t("views:sessionPanel.less") : t("core:transcript.moreFiles", { count: files.length - 5 })}</button>}
        </Card>}
      </div>}
      {det && view && <OutDialog item={det.id} outs={outs} anns={det.annotations ?? []} view={view} setView={setView} />}
    </section>
  )
}

/* ───────── where it sits ───────── */
type Rel = { id: string; kind: "item" | "sess"; why: string }
function RelRow({ r, quiet }: { r: Rel; /** the group's label already says why */ quiet?: boolean }) {
  const t = useT()
  if (r.kind === "sess") { const x = st.S[r.id]; if (!x) return null; return (
    <a href={"#/sessions/" + r.id} className="flex min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-accent" data-rel-session={r.id}>
      <HarnessMark harness={x.harness ?? x.h} className="size-3.5" /><span className="shrink-0 type-callout">{t("views:session.numbered", { n: x.n })}</span>
      <span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{r.why}</span><Status s={x.st} sess className="shrink-0" /></a>) }
  const x = find(r.id); if (!x) return null
  return (
    <a href={"#/items/" + r.id} className="flex min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-accent" data-rel-item={r.id}>
      <StatusDot s={itemDisplayStatus(x)} /><ItemNo id={x.id} /><span className="min-w-0 flex-1 truncate type-callout">{x.t}</span>
      {!quiet && <span className="shrink-0 type-footnote text-tertiary">{r.why}</span>}</a>
  )
}
function RelGroup({ label, rels, icon, test }: { label: string; rels: Rel[]; icon: ReactNode; test: string }) {
  const t = useT()
  const [all, setAll] = useState(false)
  if (!rels.length) return null
  const shown = all ? rels : rels.slice(0, 5)
  /* one reason for all of them (an item's collaboration units): said once, in the label */
  const one = rels.every((r) => r.kind === "item" && r.why === rels[0].why) ? rels[0].why : null
  return (
    <div data-rel-group={test}>
      <div className="flex items-center gap-1.5 px-2 pb-0.5 type-footnote text-tertiary">{icon}{label}{one && <span>· {one}</span>}<span className="tabular-nums">{rels.length}</span></div>
      <div className="-mx-0 flex flex-col border-l border-border/80 pl-1.5 ml-[13px]">{shown.map((r) => <RelRow key={r.kind + r.id} r={r} quiet={!!one} />)}</div>
      {rels.length > 5 && <button type="button" onClick={() => setAll(!all)} className="ml-[21px] mt-0.5 type-footnote text-muted-foreground hover:text-foreground">{all ? t("views:sessionPanel.less") : t("views:sessionPanel.more", { count: rels.length - 5 })}</button>}
    </div>
  )
}
function Relations({ s }: { s: Sess }) {
  const t = useT()
  const i = find(s.item)
  const up: Rel[] = [], down: Rel[] = [], same: Rel[] = []
  if (i?.parent?.item && find(i.parent.item)) up.push({ id: i.parent.item, kind: "item", why: i.src[1] === "workflow" ? t("views:sessionPanel.why.flowParent") : t("views:sessionPanel.why.forkedFrom") })
  if (i?.parent?.s && st.S[i.parent.s]) up.push({ id: i.parent.s, kind: "sess", why: t("views:sessionPanel.why.itemFromSession") })
  if (s.fork?.s && st.S[s.fork.s]) up.push({ id: s.fork.s, kind: "sess", why: t("views:sessionPanel.why.sessionFork") })
  for (const d of i?.deps ?? []) if (find(d)) up.push({ id: d, kind: "item", why: t("views:sessionPanel.why.dep") })
  for (const x of st.items) {
    if (i && x.parent?.item === i.id) down.push({ id: x.id, kind: "item", why: x.src[1] === "workflow" ? t("views:sessionPanel.why.flowChild") : t("views:sessionPanel.why.forked") })
    else if (x.parent?.s === s.id) down.push({ id: x.id, kind: "item", why: t("views:sessionPanel.why.fromHere") })
  }
  /* what waits on the item: the items that list it among their prerequisites */
  for (const x of st.items) if (i && x.deps?.includes(i.id) && !down.some((r) => r.id === x.id)) down.push({ id: x.id, kind: "item", why: t("views:sessionPanel.why.dependent") })
  for (const k of Object.keys(st.S)) if (k !== s.id && st.S[k].fork?.s === s.id) down.push({ id: k, kind: "sess", why: t("views:sessionPanel.why.sessionForked") })
  for (const k of i?.ss ?? []) if (k !== s.id && st.S[k]) same.push({ id: k, kind: "sess", why: st.S[k].title })
  const lonely = !up.length && !down.length && !same.length
  return (
    <section data-session-relations={up.length + down.length + same.length}>
      <SectionLabel right={<Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => openSheet("graph", { k: s.id })} data-session-graph><NetworkIcon />{t("views:sessionPanel.graph")}</Button>}>{t("views:sessionPanel.relations")}</SectionLabel>
      <div className="flex flex-col gap-3">
        <RelGroup label={t("views:sessionPanel.up")} rels={up} icon={<ArrowUpRightIcon className="size-3 -scale-y-100" />} test="up" />
        {i ? (
          <button type="button" onClick={() => go("#/items/" + i.id)} className="w-full rounded-xl bg-card p-3.5 text-left shadow-[var(--lift)] transition-shadow duration-180 hover:shadow-[var(--lift-hover)]" data-session-item={i.id}>
            <div className="type-footnote text-tertiary">{t("views:session.item")}</div>
            <div className="mt-0.5 flex min-w-0 items-baseline gap-1.5"><ItemNo id={i.id} /><span className="line-clamp-2 type-callout font-medium">{i.t}</span></div>
            <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 type-footnote text-muted-foreground"><Status s={itemDisplayStatus(i)} /><ProjectChip id={i.project} plain />{s.b && <BotName k={s.b} />}</div>
          </button>
        ) : (
          <Card data-session-unlinked><div className="type-callout font-medium">{t("views:session.unlinked")}</div><p className="mt-0.5 type-footnote text-muted-foreground">{t("views:sessionPanel.unlinkedSub")}</p>
            <div className="mt-3 flex flex-wrap gap-2">{!s.ro && <Button size="sm" variant="outline" onClick={() => openSheet("link", { k: s.id })} data-link>{t("views:session.link")}</Button>}<Button size="sm" variant="outline" onClick={() => openSheet("fork", { k: s.id, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</Button></div></Card>
        )}
        <RelGroup label={t("views:sessionPanel.down")} rels={down} icon={<CornerDownRightIcon className="size-3" />} test="down" />
        <RelGroup label={t("views:sessionPanel.same")} rels={same} icon={<GitForkIcon className="size-3" />} test="same" />
        {lonely && <p className="px-1 type-footnote text-tertiary" data-rel-none>{t("views:sessionPanel.noRelations")}</p>}
      </div>
    </section>
  )
}

/* ───────── where it runs ───────── */
function Runtime({ s }: { s: Sess }) {
  const t = useT()
  const device = st.devices.find((d) => d.id === s.device)?.name ?? "Hub"
  const rows: [string, ReactNode][] = [
    [t("views:sessionPanel.bot"), <BotName k={s.b} />],
    [t("views:sessionPanel.harness"), <span className="inline-flex min-w-0 items-center gap-1.5"><HarnessMark harness={s.harness ?? s.h} className="size-3.5" /><span className="truncate">{s.h ?? t("core:unknownHarness")}</span></span>],
    [t("views:sessionPanel.model"), <span className="inline-flex min-w-0 items-center gap-1.5"><ModelMark model={s.m} className="size-3.5" /><span className="truncate font-mono type-footnote">{s.m}</span></span>],
    [t("views:session.effortLabel"), s.reasoningEffort ?? t("views:session.effortInherit")],
    [t("views:sessionPanel.device"), <a href={"#/devices/" + s.device} className="hover:underline">{device}</a>],
    [t("views:sessionPanel.source"), s.src],
    ...(s.cwd ? [[t("views:sessionPanel.cwd"), <span className="break-all font-mono type-footnote">{s.cwd.replace(/^\/Users\/[^/]+/, "~")}</span>] as [string, ReactNode]] : []),
  ]
  return (
    <section data-session-runtime-info>
      <SectionLabel>{t("views:sessionPanel.runtime")}</SectionLabel>
      <Card className="p-0 py-1.5">
        <dl className="grid grid-cols-[minmax(56px,max-content)_minmax(0,1fr)] items-baseline gap-x-3 gap-y-2 px-4 py-2 type-callout">
          {rows.map(([k, v]) => <div key={k} className="contents"><dt className="type-footnote text-muted-foreground">{k}</dt><dd className="min-w-0 text-foreground">{v}</dd></div>)}
        </dl>
        <Collapsible className="border-t border-border/70 px-4 pt-2" data-session-resume>
          <CollapsibleTrigger className="group/res flex w-full items-center gap-1.5 py-1 type-callout text-muted-foreground hover:text-foreground"><ChevronRightIcon className="size-3.5 transition-transform duration-180 group-data-[panel-open]/res:rotate-90" />{t("views:session.resume")}</CollapsibleTrigger>
          <CollapsibleContent><div className="pb-3 pt-2"><ResumeCard k={s.id} /></div></CollapsibleContent>
        </Collapsible>
      </Card>
    </section>
  )
}

export function SessionPanel({ k }: { k: string }) {
  const s = st.S[k]
  const turns = useTurns(k)
  const [tot] = useStats(() => stats.session(k), [k, s?.st === "run" ? 0 : s?.at])
  const item: Item | undefined = find(s?.item)
  const [det] = useStats<RItemDetail | null>(() => (s?.item ? api.item(s.item) : Promise.resolve(null)), [s?.item, item?.updated], false)
  if (!s) return null
  const totals = tot.s === "ok" ? tot.v : undefined
  return (
    <div className="flex flex-col gap-7" data-session-panel-body>
      <Monitor s={s} turns={turns} tot={totals} />
      <Context s={s} />
      {totals && <Usage tot={totals} />}
      <Time s={s} turns={turns} />
      <Outputs s={s} det={det.s === "ok" ? det.v : null} />
      <Relations s={s} />
      <SessionKnowledge id={k} />
      <Runtime s={s} />
    </div>
  )
}
