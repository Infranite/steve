/* The session page's side panel (a sheet on a phone): what the session is besides its transcript, from what the Hub
 * reports and nothing else. The transcript is the page; the panel is for a glance and the occasional look-up, so it
 * is one quiet column without boxes: sections set apart by space and a small label, and rows of a short muted label
 * beside its value.
 *   · the head: the harness and model, the state, the session's number and its item
 *   · the figures: how full the context is (the harness's own Context Window behind ⓘ), the cost, the time worked in
 *     how many turns, how old it is, the tokens (the Compact Breakdown behind 明细). A row shows only what is known
 *   · what it produced: the outputs it delivered to its item and the files it changed
 *   · where it sits: its item, what that came from, what came from it (folded to a count), the item's other sessions
 *   · the knowledge it was given; where it runs, folded, with the way to resume it in its harness */
import { useMemo, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronRightIcon, FileDiffIcon, InfoIcon, PackageIcon } from "lucide-react"
import { api, type RItemDetail } from "@/api"
import { stats, type SessionTotals } from "@/api/stats"
import { CompactBreakdown } from "@/components/affect/compact-breakdown"
import { ContextWindow } from "@/components/affect/context-window-clean"
import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { useT } from "@/i18n"
import { clockDur, longDur, nearTime, pastTime } from "@/i18n/format"
import { OutDialog, type OutOpen } from "@/item-outputs"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { SessionKnowledge } from "@/knowledge/parts"
import { ResumeCard } from "@/resume"
import { fmtEst, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { briefHead, st, type Item, type Out, type Sess } from "@/store"
import { BotName, DiscChevron, ItemNo, openSheet, SectionLabel, Status, StatusDot, useNow } from "@/ui"
import { filesOf, outputsOf, turnsOf, workedMs, type Turn } from "./data"

const find = (id: string | null | undefined) => (id ? st.items.find((x) => x.id === id) : undefined)
/** tokens to one decimal below a million: "107.9K", as the harness's own context figure reads */
const kTok = (n: number) => (n >= 1e3 && n < 1e6 ? (n / 1e3).toFixed(1).replace(/\.0$/, "") + "K" : fmtN(n))

/* ───────── the parts every section is made of ───────── */
/** a section: its small label (with something on the right of it) and what is under it */
function Section({ label, right, children, ...rest }: { label: ReactNode; right?: ReactNode; children: ReactNode; [k: `data-${string}`]: unknown }) {
  return <section {...rest}><SectionLabel className="mb-2.5 px-0" right={right}>{label}</SectionLabel>{children}</section>
}
/** rows of a label column (short, muted) and its value */
const Rows = ({ children, className }: { children: ReactNode; className?: string }) =>
  <dl className={cn("grid grid-cols-[56px_minmax(0,1fr)] gap-x-3 gap-y-2.5 type-callout", className)}>{children}</dl>
function Row({ k, children, ...rest }: { k: ReactNode; children: ReactNode; [x: `data-${string}`]: unknown }) {
  return <div className="contents" {...rest}><dt className="truncate text-muted-foreground">{k}</dt><dd className="min-w-0 text-foreground">{children}</dd></div>
}
/** the quieter parts of a value: "· part" each, in the secondary gray */
const Quiet = ({ parts }: { parts: (ReactNode | null | undefined | false)[] }) =>
  <>{parts.filter(Boolean).map((p, n) => <span key={n} className="text-muted-foreground"> · <span className="whitespace-nowrap">{p}</span></span>)}</>
/** a small text button: "显示全部 12 个", "明细 ›" */
const LINK = "rounded-md type-footnote text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
/** a row you can open: the plain list row of the panel, its background only on hover */
const ROW = "-mx-2 flex w-[calc(100%+1rem)] min-w-0 items-start gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors duration-120 hover:bg-accent"
function More({ count, all, set, ...rest }: { count: number; all: boolean; set: (v: boolean) => void; [k: `data-${string}`]: unknown }) {
  const t = useT()
  return <button type="button" onClick={() => set(!all)} className={cn(LINK, "mt-1")} {...rest}>{all ? t("views:sessionPanel.less") : t("views:sessionPanel.showAll", { count })}</button>
}

/** the session's turns, from its transcript */
export function useTurns(k: string) {
  const s = st.S[k], run = s?.st === "run"
  return useMemo(() => s ? turnsOf(s.ev, s.ts, run, briefHead, !!s.ro) : [], [s?.ev, s?.ts, run])
}

/* ───────── the head ───────── */
function Head({ s }: { s: Sess }) {
  const t = useT()
  const i = find(s.item)
  return (
    <header data-session-head={s.st}>
      <div className="flex min-w-0 items-center gap-2">
        <HarnessMark harness={s.harness ?? s.h} className="size-4 shrink-0" />
        <span className="min-w-0 flex-1 truncate type-body"><span className="font-medium text-foreground">{s.h ?? t("core:unknownHarness")}</span>{s.m && <span className="text-muted-foreground"> · {s.m}</span>}</span>
        <Status s={s.st} sess className="type-callout" />
      </div>
      {/* its number and its item; an unlinked session says so under 关系, not twice */}
      {(s.n > 0 || i) && <div className="mt-1 flex min-w-0 items-baseline gap-1 type-callout text-muted-foreground">
        {s.n > 0 && <span className="shrink-0">{t("views:session.numbered", { n: s.n })}{i && " ·"}</span>}
        {i && <><span className="shrink-0">{t("views:sessionPanel.belongs")}</span>
          <a href={"#/items/" + i.id} className="flex min-w-0 items-baseline gap-1 hover:text-foreground" data-session-item={i.id}><ItemNo id={i.id} className="type-callout" /><span className="truncate text-foreground/90 underline-offset-2 hover:underline">{i.t}</span></a></>}
      </div>}
    </header>
  )
}

/* ───────── the figures ───────── */
function Figures({ s, turns, tot }: { s: Sess; turns: Turn[]; tot?: SessionTotals }) {
  const t = useT()
  const run = s.st === "run", now = useNow(run)
  const u = tot?.usage
  const h = s.h ?? t("core:unknownHarness")
  const worked = workedMs(turns, now), done = turns.filter((x) => x.end)
  const avg = done.length ? done.reduce((n, x) => n + (x.end! - x.start), 0) / done.length : 0
  const end = run ? now : (s.at ?? now)
  const est = u && u.total > 0 && u.est > 0 ? u.est : null, reported = u && u.reported > 0 ? u.reported : null
  const pct = Math.min(100, Math.max(0, s.ctx))
  return (
    <Rows>
      {s.contextKnown && s.ctxSize ? (
        <Row k={t("views:sessionPanel.row.ctx")} data-session-context={s.ctx}>
          <div className="flex min-w-0 items-center gap-2">
            <span className="tabular-nums">{pct}%</span>
            <span className="truncate tabular-nums text-muted-foreground">{kTok(s.ctxUsed ?? 0)} / {kTok(s.ctxSize)}</span>
            <Popover>
              <PopoverTrigger render={<button type="button" className="-my-1 ml-auto inline-flex size-6 shrink-0 items-center justify-center rounded-md text-tertiary outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50" aria-label={t("views:sessionPanel.ctxTitle")} data-ctx-info />}><InfoIcon className="size-3.5" strokeWidth={1.75} /></PopoverTrigger>
              <PopoverContent align="end" className="w-[min(340px,calc(100vw-24px))] gap-1 rounded-xl p-1 pb-4" data-ctx-pop>
                <ContextWindow className="block! max-w-none! [--ui-cwc-card:transparent]! [--ui-cwc-shadow:none]! [--aui-shadow-card:none]!" title={t("views:sessionPanel.ctxTitle")} limit={s.ctxSize} model={s.m} modelIcon={<ModelMark model={s.m} />}
                  parts={[{ id: "used", label: t("views:sessionPanel.ctxUsed"), tokens: s.ctxUsed ?? 0, locked: true }]} cells={180} columns={30} />
                <p className="px-4 type-footnote text-muted-foreground">{t("views:sessionPanel.ctxWhole", { h })}</p>
              </PopoverContent>
            </Popover>
          </div>
          <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-muted" aria-hidden><div className={cn("h-full rounded-full transition-[width] duration-260", pct >= 80 ? "bg-attention" : "bg-primary")} style={{ width: `${pct}%` }} /></div>
        </Row>
      ) : null}
      {(est != null || reported != null) && (
        <Row k={t("views:sessionPanel.row.cost")} data-session-cost>
          {est != null ? <><span className="tabular-nums">{fmtEst(est, u!.unpriced_tokens)}</span>{reported != null && Math.abs(reported - est) >= 0.01 && <span className="ml-2 text-muted-foreground tabular-nums">{t("views:sessionPanel.costByHarness", { usd: fmtUsd(reported) })}</span>}</>
            : <><span className="tabular-nums">{fmtUsd(reported)}</span><span className="ml-2 text-muted-foreground">{t("views:sessionPanel.costHarnessOnly")}</span></>}
        </Row>
      )}
      {s.ro ? <>
        {s.since ? <Row k={t("views:sessionPanel.row.imported")} data-session-time="imported">{nearTime(s.since, now)}<div className="type-footnote text-tertiary">{t("views:sessionPanel.importedSub")}</div></Row> : null}
        {turns.length > 0 && <Row k={t("views:sessionPanel.row.work")} data-session-work>{t("views:sessionPanel.turns", { count: turns.length })}</Row>}
      </> : <>
        {turns.length > 0 && <Row k={t("views:sessionPanel.row.work")} data-session-work>
          <span className="tabular-nums">{worked ? longDur(worked) : t("views:sessionPanel.turns", { count: turns.length })}</span>
          <Quiet parts={[worked ? t("views:sessionPanel.turns", { count: turns.length }) : null, avg ? t("views:sessionPanel.avg", { dur: clockDur(avg) }) : null]} />
        </Row>}
        {s.since ? <Row k={t("views:sessionPanel.row.age")} data-session-time="age">
          <span className="tabular-nums">{longDur(end - s.since)}</span>
          <Quiet parts={[t("views:sessionPanel.since", { time: nearTime(s.since, now) }), s.at && !run ? t("views:sessionPanel.lastAt", { time: pastTime(s.at, now) }) : null]} />
        </Row> : null}
      </>}
      {u && u.total > 0 && <Row k={t("views:sessionPanel.row.tokens")} data-session-usage={u.total}>
        <div className="flex min-w-0 items-start gap-2"><span className="min-w-0 flex-1">
        <span className="tabular-nums">{fmtN(u.total)}</span>
        <Quiet parts={[u.cache_read > 0 ? t("views:sessionPanel.cacheShare", { pct: fmtPct(u.cache_read / u.total) }) : null, u.input > 0 ? t("views:sessionPanel.tokIn", { n: fmtN(u.input) }) : null, u.output > 0 ? t("views:sessionPanel.tokOut", { n: fmtN(u.output) }) : null]} />
        </span><Breakdown tot={tot!} /></div>
      </Row>}
    </Rows>
  )
}

/** the tokens by kind, the estimate, the cache hit rate and the harness's own figure: behind 明细 */
function Breakdown({ tot }: { tot: SessionTotals }) {
  const t = useT()
  const u = tot.usage
  const items = ([["input", u.input], ["cache_read", u.cache_read], ["cache_write", u.cache_write], ["output", u.output]] as const)
    .filter(([, v]) => v > 0).map(([id, value]) => ({ id, label: t(`views:sessionPanel.tok.${id}`), value }))
  const parts = [t("views:sessionPanel.est", { usd: fmtEst(u.est, u.unpriced_tokens) }), u.hit_rate != null ? t("views:sessionPanel.hit", { pct: fmtPct(u.hit_rate) }) : null,
    u.reported > 0 && Math.abs(u.reported - u.est) >= 0.01 ? t("views:sessionPanel.reported", { usd: fmtUsd(u.reported) }) : null].filter(Boolean) as string[]
  return (
    <Popover>
      <PopoverTrigger render={<button type="button" className={cn(LINK, "inline-flex shrink-0 items-center whitespace-nowrap type-callout")} data-usage-more />}>{t("views:sessionPanel.details")}<ChevronRightIcon className="size-3.5" /></PopoverTrigger>
      <PopoverContent align="end" className="w-[min(320px,calc(100vw-24px))] gap-0 rounded-xl p-0" data-usage-pop>
        <CompactBreakdown bare items={items} title={t("views:sessionPanel.usageTitle", { count: tot.turns })} totalLabel={t("views:sessionPanel.tokensTotal")} format={(n) => fmtN(Math.round(n))}
          footer={<span className="flex w-full items-center gap-2 whitespace-normal"><span className="min-w-0 flex-1" data-session-cost-detail>{parts.join(" · ")}</span><a href="#/stats" className="shrink-0 hover:text-foreground">{t("views:sessionPanel.toStats")}</a></span>} rowProps={(x) => ({ "data-usage-part": x.key })} />
      </PopoverContent>
    </Popover>
  )
}

/* ───────── outputs ───────── */
function Outputs({ s, det }: { s: Sess; det: RItemDetail | null }) {
  const t = useT()
  const [view, setView] = useState<OutOpen | null>(null)
  const [allFiles, setAllFiles] = useState(false), [allOuts, setAllOuts] = useState(false)
  const now = useNow(false)
  const files = useMemo(() => filesOf(s.ev), [s.ev])
  const arts = det ? outputsOf(s.id, det.artifacts, det.timeline, det.sessionIds) : []
  /* the results first, the records of the work after them; the newest first in each */
  const outs: Out[] = arts.map((a): Out => [a.kind, a.title, a.location, a.id, a.created, det?.artifactRoles?.[a.id] as Out[5]])
    .sort((a, b) => Number(a[5] === "record") - Number(b[5] === "record") || (b[4] ?? 0) - (a[4] ?? 0))
  if (!outs.length && !files.length) return null
  const outsShown = allOuts ? outs : outs.slice(0, 3)
  const add = files.reduce((n, f) => n + f[1], 0), del = files.reduce((n, f) => n + f[2], 0)
  const shown = allFiles ? files : files.slice(0, 5)
  const num = (a: number, d: number) => <span className="shrink-0 font-mono type-footnote tabular-nums"><span className={a ? "text-success" : "text-tertiary"}>+{a}</span> <span className={d ? "text-destructive" : "text-tertiary"}>−{d}</span></span>
  return (
    <Section label={<>{t("views:sessionPanel.outputs")}{outs.length > 0 && <span className="ml-1.5 tabular-nums">{outs.length}</span>}</>} data-session-outputs={outs.length}>
      {outs.length > 0 && <div className="flex flex-col">
        {outsShown.map((o) => (
          <button key={o[3]} type="button" onClick={() => setView({ o })} className={ROW} data-session-out={o[3]}>
            <PackageIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} />
            <span className="min-w-0 flex-1"><span className="block truncate type-callout text-foreground">{o[1]}</span>
              <span className="block truncate type-footnote text-muted-foreground">{[o[0], o[5] === "record" ? t("views:sessionPanel.record") : null, o[4] ? pastTime(o[4], now) : null].filter(Boolean).join(" · ")}</span></span>
          </button>))}
        {outs.length > 3 && <div><More count={outs.length} all={allOuts} set={setAllOuts} data-session-outs-more /></div>}
      </div>}
      {files.length > 0 && <div className={cn(outs.length > 0 && "mt-3")} data-session-files={files.length}>
        <div className="flex min-w-0 items-center gap-2.5 py-1 type-callout"><FileDiffIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /><span className="min-w-0 flex-1 truncate">{t("core:transcript.filesChanged", { count: files.length })}</span>{num(add, del)}</div>
        <ul className="flex flex-col pl-[26px]">{shown.map(([p, a, d]) => (
          <li key={p} className="flex min-w-0 items-center gap-2 py-0.5" title={p}><span className="min-w-0 flex-1 truncate font-mono type-footnote text-muted-foreground">{p.split("/").pop()}</span>{num(a, d)}</li>))}</ul>
        {files.length > 5 && <div className="pl-[26px]"><button type="button" onClick={() => setAllFiles(!allFiles)} className={cn(LINK, "mt-1")} data-session-files-more>{allFiles ? t("views:sessionPanel.less") : t("core:transcript.moreFiles", { count: files.length - 5 })}</button></div>}
      </div>}
      {det && view && <OutDialog item={det.id} outs={outs} anns={det.annotations ?? []} view={view} setView={setView} />}
    </Section>
  )
}

/* ───────── where it sits ───────── */
type Why = "flowParent" | "forkedFrom" | "itemFromSession" | "sessionFork" | "dep" | "flowChild" | "forked" | "fromHere" | "dependent" | "sessionForked"
type Rel = { id: string; kind: "item" | "sess"; why: Why }
/** one related item or session on one line: its state, its number, its name; why it is related when the row's label
 *  does not already say it */
function RelLine({ r, why = true, under = false }: { r: Rel; why?: boolean; /** the reason on a line of its own */ under?: boolean }) {
  const t = useT()
  if (r.kind === "sess") { const x = st.S[r.id]; if (!x) return null; return (
    <a href={"#/sessions/" + r.id} className={cn(ROW, "items-center py-1")} data-rel-session={r.id}>
      <StatusDot s={x.st} /><span className="shrink-0 tabular-nums text-muted-foreground">#{x.n}</span><span className="min-w-0 flex-1 truncate">{x.title}</span>
      {why && r.why !== "itemFromSession" && <span className="shrink-0 type-footnote text-tertiary">{t(`views:sessionPanel.why.${r.why}`)}</span>}</a>) }
  const x = find(r.id); if (!x) return null
  if (under) return (
    <a href={"#/items/" + r.id} className={cn(ROW, "block py-0.5")} data-rel-item={r.id}>
      <span className="line-clamp-2"><ItemNo id={x.id} className="mr-1.5 type-callout" />{x.t}</span>
      <span className="mt-0.5 flex min-w-0 items-center gap-1.5"><Status s={itemDisplayStatus(x)} /><span className="truncate type-footnote text-muted-foreground">· {t(`views:sessionPanel.why.${r.why}`)}</span></span></a>
  )
  return (
    <a href={"#/items/" + r.id} className={cn(ROW, "items-center py-1")} data-rel-item={r.id}>
      <StatusDot s={itemDisplayStatus(x)} /><ItemNo id={x.id} /><span className="min-w-0 flex-1 truncate">{x.t}</span>
      {why && <span className="shrink-0 type-footnote text-tertiary">{t(`views:sessionPanel.why.${r.why}`)}</span>}</a>
  )
}
function Relations({ s }: { s: Sess }) {
  const t = useT()
  const [openDown, setOpenDown] = useState(false), [allSame, setAllSame] = useState(false)
  const i = find(s.item)
  const up: Rel[] = [], down: Rel[] = [], same: Rel[] = []
  if (i?.parent?.item && find(i.parent.item)) up.push({ id: i.parent.item, kind: "item", why: i.src[1] === "workflow" ? "flowParent" : "forkedFrom" })
  if (i?.parent?.s && st.S[i.parent.s]) up.push({ id: i.parent.s, kind: "sess", why: "itemFromSession" })
  if (s.fork?.s && st.S[s.fork.s]) up.push({ id: s.fork.s, kind: "sess", why: "sessionFork" })
  for (const d of i?.deps ?? []) if (find(d)) up.push({ id: d, kind: "item", why: "dep" })
  for (const x of st.items) {
    if (i && x.parent?.item === i.id) down.push({ id: x.id, kind: "item", why: x.src[1] === "workflow" ? "flowChild" : "forked" })
    else if (x.parent?.s === s.id) down.push({ id: x.id, kind: "item", why: "fromHere" })
  }
  /* what waits on the item: the items that list it among their prerequisites */
  for (const x of st.items) if (i && x.deps?.includes(i.id) && !down.some((r) => r.id === x.id)) down.push({ id: x.id, kind: "item", why: "dependent" })
  for (const k of Object.keys(st.S)) if (k !== s.id && st.S[k].fork?.s === s.id) down.push({ id: k, kind: "sess", why: "sessionForked" })
  for (const k of i?.ss ?? []) if (k !== s.id && st.S[k]) same.push({ id: k, kind: "sess", why: "itemFromSession" })
  /* one reason for all of them (an item's collaboration units): said once, in the count */
  const one = down.length && down.every((r) => r.why === down[0].why) ? down[0].why : null
  const sameShown = allSame ? same : same.slice(0, 3)
  return (
    <Section label={t("views:sessionPanel.relations")} data-session-relations={up.length + down.length + same.length}
      right={<button type="button" onClick={() => openSheet("graph", { k: s.id })} className={LINK} data-session-graph>{t("views:sessionPanel.graph")}</button>}>
      <Rows className="gap-y-1.5">
        <Row k={t("views:sessionPanel.row.item")} data-rel-group="item">
          {i ? <a href={"#/items/" + i.id} className={cn(ROW, "block py-0.5")} data-session-item-row={i.id}><span className="line-clamp-2"><ItemNo id={i.id} className="mr-1.5 type-callout" />{i.t}</span><Status s={itemDisplayStatus(i)} className="mt-0.5" /></a>
            : <div data-session-unlinked><span className="text-muted-foreground">{t("views:session.unlinked")}</span>
              <div className="mt-0.5 flex flex-wrap gap-x-3">{!s.ro && <button type="button" className={LINK} onClick={() => openSheet("link", { k: s.id })} data-link>{t("views:session.link")}</button>}
                <button type="button" className={LINK} onClick={() => openSheet("fork", { k: s.id, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</button></div></div>}
        </Row>
        {up.length > 0 && <Row k={t("views:sessionPanel.row.up")} data-rel-group="up">{up.map((r) => <RelLine key={r.kind + r.id} r={r} under />)}</Row>}
        {down.length > 0 && <Row k={t("views:sessionPanel.row.down")} data-rel-group="down">
          <Collapsible open={openDown} onOpenChange={setOpenDown}>
            <CollapsibleTrigger className={cn(ROW, "group/disc items-center py-0.5 text-foreground")} data-rel-down-toggle>
              <span className="min-w-0 flex-1 truncate">{t("views:sessionPanel.downN", { count: down.length, what: t(`views:sessionPanel.downWhat.${(one as "flowChild" | "forked" | "fromHere" | "dependent" | "sessionForked" | null) ?? "mixed"}`) })}</span>
              <DiscChevron className="text-tertiary" />
            </CollapsibleTrigger>
            <CollapsibleContent><div className="pt-0.5">{down.map((r) => <RelLine key={r.kind + r.id} r={r} why={!one} />)}</div></CollapsibleContent>
          </Collapsible>
        </Row>}
        {same.length > 0 && <Row k={t("views:sessionPanel.row.same")} data-rel-group="same">
          {sameShown.map((r) => <RelLine key={r.id} r={r} why={false} />)}
          {same.length > 3 && <More count={same.length} all={allSame} set={setAllSame} />}
        </Row>}
      </Rows>
    </Section>
  )
}

/* ───────── where it runs ───────── */
function Runtime({ s }: { s: Sess }) {
  const t = useT()
  const device = st.devices.find((d) => d.id === s.device)?.name ?? "Hub"
  return (
    <Collapsible data-session-runtime-info>
      <CollapsibleTrigger className="group/disc flex min-h-6 items-center gap-1 rounded-md type-label text-tertiary outline-none transition-colors hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50" data-runtime-toggle>
        {t("views:sessionPanel.runtime")}<DiscChevron className="size-3" />
      </CollapsibleTrigger>
      <CollapsibleContent>
        <Rows className="pt-2.5">
          {s.b && <Row k={t("views:sessionPanel.row.bot")}><BotName k={s.b} /></Row>}
          <Row k={t("views:sessionPanel.row.harness")}><span className="inline-flex min-w-0 items-center gap-1.5"><HarnessMark harness={s.harness ?? s.h} className="size-3.5" /><span className="truncate">{s.h ?? t("core:unknownHarness")}</span></span></Row>
          {s.m && <Row k={t("views:sessionPanel.row.model")}><span className="inline-flex min-w-0 items-center gap-1.5"><ModelMark model={s.m} className="size-3.5" /><span className="truncate font-mono type-footnote">{s.m}</span></span></Row>}
          <Row k={t("views:sessionPanel.row.effort")}>{s.reasoningEffort ?? t("views:session.effortInherit")}</Row>
          <Row k={t("views:sessionPanel.row.device")}><a href={"#/devices/" + s.device} className="hover:underline">{device}</a></Row>
          <Row k={t("views:sessionPanel.row.source")}>{s.src}</Row>
          {s.cwd && <Row k={t("views:sessionPanel.row.cwd")}><span className="break-all font-mono type-footnote text-muted-foreground">{s.cwd.replace(/^\/Users\/[^/]+/, "~")}</span></Row>}
        </Rows>
        <Collapsible className="mt-3" data-session-resume>
          <CollapsibleTrigger className={cn(LINK, "group/disc inline-flex items-center gap-1 type-callout")}>{t("views:session.resume")}<DiscChevron className="size-3" /></CollapsibleTrigger>
          <CollapsibleContent><div className="pt-3"><ResumeCard k={s.id} /></div></CollapsibleContent>
        </Collapsible>
      </CollapsibleContent>
    </Collapsible>
  )
}

export function SessionPanel({ k }: { k: string }) {
  const s = st.S[k]
  const turns = useTurns(k)
  const [tot] = useStats(() => stats.session(k), [k, s?.st === "run" ? 0 : s?.at])
  const item: Item | undefined = find(s?.item)
  const [det] = useStats<RItemDetail | null>(() => (s?.item ? api.item(s.item) : Promise.resolve(null)), [s?.item, item?.updated], false)
  if (!s) return null
  return (
    <div className="flex flex-col gap-8" data-session-panel-body>
      <div className="flex flex-col gap-6">
        <Head s={s} />
        <Figures s={s} turns={turns} tot={tot.s === "ok" ? tot.v : undefined} />
      </div>
      <Outputs s={s} det={det.s === "ok" ? det.v : null} />
      <Relations s={s} />
      <SessionKnowledge id={k} />
      <Runtime s={s} />
    </div>
  )
}
