/* 信号: external events and what the flows did with them (signals-automation.md §9, prototype SignalsView) */
import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowRightIcon, ChevronRightIcon, FileIcon, ListTreeIcon, SearchIcon, WorkflowIcon } from "lucide-react"
import { Input } from "@/components/ui/input"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Page, Group, Segmented, Chips, StatusDot, Empty, go } from "@/ui"
import { st, tm, toast, botName } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, SOURCE_LABEL, type Signal, type SignalDetail } from "@/api/flow"
import { HubError } from "@/api"
import { srcIcon, Json, Failed, ms, RUN_STATUS } from "@/auto/common"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"

type Mode = "outcome" | "source"
/* the range as the API takes it */
type Range = "today" | "7d"
/* each outcome group's label is misc:signals.group.<key> */
const OUTCOME_GROUPS: [Group, string[]][] = [
  ["need", ["need"]], ["failed", ["failed"]], ["item", ["item", "linked"]], ["digest", ["digest"]],
  ["ignored", ["ignored"]], ["recorded", ["recorded"]], ["pending", ["pending"]],
]
type Group = "need" | "failed" | "item" | "digest" | "ignored" | "recorded" | "pending"
const dotOf = (o: string) => (o === "need" ? "wait" : o === "failed" ? "fail" : o === "item" || o === "linked" ? "prop" : o === "pending" ? "run" : "idle")
/* page state survives leaving and coming back, like the prototype's module-level filters */
let mode: Mode = "outcome", range: Range = "today", query = ""
const openGroups = new Set<string>(["need", "failed"])

/** `#/signals?e=<id>`: the signal to open and scroll to */
const focusOf = () => new URLSearchParams(location.hash.split("?")[1] ?? "").get("e")

export function SignalsView() {
  const t = useT(), [, force] = useState(0), r = () => force((x) => x + 1)
  const focus = focusOf()
  if (focus && range === "today") range = "7d"
  const [q, setQ] = useState(query)
  useEffect(() => { const h = setTimeout(() => { query = q; r() }, 300); return () => clearTimeout(h) }, [q])
  const L = useLoad(() => flowApi.signals({ range, q: query }), [range, query], ["signal", "run"])
  const [row, setRow] = useState<string | null>(null)
  const groups = useMemo(() => {
    const xs = L.data?.items ?? []
    const gs: [string, string, Signal[]][] = mode === "outcome"
      ? OUTCOME_GROUPS.map(([k, os]) => [k, t(`misc:signals.group.${k}`), xs.filter((s) => os.includes(s.outcome))])
      : [...new Set(xs.map((s) => s.source))].sort((a, b) => Object.keys(SOURCE_LABEL).indexOf(a) - Object.keys(SOURCE_LABEL).indexOf(b)).map((s) => [s, sourceLabel(s), xs.filter((x) => x.source === s)])
    return gs.filter((g) => g[2].length)
  }, [L.data, mode, t])
  /* open the linked signal once it is loaded */
  const done = useRef<string | null>(null)
  useEffect(() => {
    if (!focus || !L.data || done.current === focus) return
    const s = L.data.items.find((x) => x.id === focus); if (!s) return
    done.current = focus
    const g = OUTCOME_GROUPS.find((x) => x[1].includes(s.outcome))?.[0]
    if (mode === "outcome" && g) openGroups.add(g)
    setRow(rowKey(s)); r()
    setTimeout(() => document.querySelector(`[data-sig-id="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }), 250)
  }, [focus, L.data])

  if (L.unwired) return <Unwired id="signals" />
  if (L.error && !L.data) return <Failed title={t("misc:signals.title")} msg={errText(L.error)} retry={() => void L.reload()} />
  const stats = L.data?.stats
  return (
    <Page title={t("misc:signals.title")} sub={t("misc:signals.sub")} max={760}
      toolbar={<Segmented value={mode} onChange={(v) => { mode = v; r() }} options={[["outcome", t("misc:signals.byOutcome")], ["source", t("misc:signals.bySource")]] as const} />}>
      <div className="mb-6 flex flex-wrap items-center gap-2">
        <label className="relative min-w-0 basis-full sm:basis-0 sm:flex-1"><span className="sr-only">{t("misc:signals.search")}</span><SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-tertiary" aria-hidden />
          <Input className="h-9 rounded-lg pl-9 md:type-callout!" placeholder={t("misc:signals.searchPlaceholder")} value={q} onChange={(e) => setQ(e.target.value)} data-sig-search /></label>
        {/* Chips shows its values, so they are the labels here and map back to the range */}
        <Chips value={rangeLabel(range)} onChange={(v) => { range = v === rangeLabel("today") ? "today" : "7d"; r() }} options={[rangeLabel("today"), rangeLabel("7d")]} />
      </div>
      {/* nothing in range: the empty state says so once, without a row of zeros above it */}
      {stats?.total !== 0 && <div className="mb-8 grid grid-cols-2 gap-y-1 rounded-xl bg-card px-1 py-2 shadow-[var(--lift)] sm:grid-cols-4" data-sig-stats>
        {([[range === "today" ? t("misc:signals.stat.today") : t("misc:signals.stat.week"), stats?.total], [t("misc:signals.stat.need"), stats?.need], [t("misc:signals.stat.item"), stats?.item], [t("misc:signals.stat.quiet"), stats?.quiet]] as const).map(([l, n]) => (
          <div key={l} className="px-4 py-2"><div className="type-title tabular-nums">{n ?? "–"}</div><div className="mt-0.5 type-footnote text-muted-foreground">{l}</div></div>
        ))}
      </div>}
      {L.data && !L.data.items.length && <Empty title={query ? t("misc:signals.empty.noMatch") : range === "today" ? t("misc:signals.empty.today") : t("misc:signals.empty.week")} sub={query ? undefined : t("misc:signals.empty.sub")} />}
      <div key={mode} className="flex flex-col gap-6 anim-in">
        {groups.map(([gk, gl, xs]) => {
          const open = mode === "source" || openGroups.has(gk)
          const byG = groupRows(xs)
          return (
            <Collapsible key={gk} open={open} onOpenChange={(o) => { o ? openGroups.add(gk) : openGroups.delete(gk); r() }}>
              <CollapsibleTrigger className="group/sg mb-2 flex w-full items-center gap-1.5 text-left" data-sig-group={gk}>
                <ChevronRightIcon className="size-3.5 text-muted-foreground transition-transform duration-180 ease-(--ease-out) group-aria-expanded/sg:rotate-90" aria-hidden />
                <span className="type-label text-muted-foreground group-hover/sg:text-foreground">{gl}</span><span className="type-footnote text-muted-foreground">{t("misc:signals.groupMeta", { signals: t("misc:signals.count", { count: xs.length }), groups: t("misc:signals.groups", { count: byG.length }) })}</span>
              </CollapsibleTrigger>
              <CollapsibleContent>
                <Group>{byG.map((g) => <SignalRow key={rowKey(g[0])} g={g} open={row === rowKey(g[0])} onOpen={(o) => setRow(o ? rowKey(g[0]) : null)} reload={() => void L.reload()} />)}</Group>
              </CollapsibleContent>
            </Collapsible>
          )
        })}
      </div>
    </Page>
  )
}

/* one row per (group, outcome): a batch of 30 alerts of the same kind reads as one line */
const rowKey = (s: Signal) => (s.group_key ? "g:" + s.group_key : "e:" + s.id) + "|" + s.outcome
function groupRows(xs: Signal[]) {
  const m = new Map<string, Signal[]>()
  for (const s of xs) { const k = rowKey(s); m.set(k, [...(m.get(k) ?? []), s]) }
  return [...m.values()]
}

function SignalRow({ g, open, onOpen, reload }: { g: Signal[]; open: boolean; onOpen: (o: boolean) => void; reload: () => void }) {
  const t = useT(), s0 = g[0], many = g.length > 1, Ic = srcIcon(s0.source)
  return (
    <Collapsible open={open} onOpenChange={onOpen}>
      <CollapsibleTrigger className="flex w-full items-center gap-3 rounded-lg px-2 py-2.5 text-left transition-colors hover:bg-accent" data-sig-row={s0.group_key ?? s0.id} data-sig-id={s0.id}>
        <Ic className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-label={sourceLabel(s0.source)} />
        <span className="min-w-0 flex-1 truncate type-body">{many ? s0.group_label || s0.group_key : s0.summary}{many && <span className="ml-1.5 type-footnote tabular-nums text-muted-foreground">{t("misc:signals.count", { count: g.length })}</span>}</span>
        <span className="hidden items-center gap-1.5 type-footnote text-muted-foreground sm:inline-flex"><StatusDot s={dotOf(s0.outcome)} />{s0.outcome_label}</span>
        <span className="w-16 shrink-0 text-right type-footnote tabular-nums text-muted-foreground">{tm(s0.received)}</span>
      </CollapsibleTrigger>
      <CollapsibleContent data-sig-detail={s0.id}>
        <SignalDetailBox g={g} reload={reload} />
      </CollapsibleContent>
    </Collapsible>
  )
}

const keepDays = () => st.settings.keepRaw || "7"
const rangeLabel = (r: Range) => (r === "today" ? t("misc:signals.today") : t("misc:signals.week"))
function SignalDetailBox({ g, reload }: { g: Signal[]; reload: () => void }) {
  const t = useT(), s0 = g[0], many = g.length > 1
  const [raw, setRaw] = useState<{ v?: unknown; gone?: boolean; err?: string } | null>(null)
  const [runs, setRuns] = useState<SignalDetail["runs"] | null>(null), [showRuns, setShowRuns] = useState(false)
  const [busy, setBusy] = useState(false)
  const toggleRaw = async () => {
    if (raw) { setRaw(null); return }
    if (!s0.raw_available) { setRaw({ gone: true }); return }
    try { setRaw({ v: (await flowApi.raw(s0.id)).payload }) } catch (e) { setRaw(e instanceof HubError && e.status === 410 ? { gone: true } : { err: errText(e) }) }
  }
  const toggleRuns = async () => {
    if (showRuns) { setShowRuns(false); return }
    setShowRuns(true)
    try { const ds = await Promise.all(g.slice(0, 10).map((x) => flowApi.signal(x.id))); setRuns(ds.flatMap((d) => d.runs)) } catch (e) { toast(errText(e)); setShowRuns(false) }
  }
  const fix = async (action: "remind" | "ignore" | "to_item") => {
    setBusy(true)
    try {
      const res = await flowApi.correct(s0.id, action, "group")
      const cs = res.changeset_id
      const msg = action === "to_item" ? (res.item_id ? t("misc:signals.itemCreatedId", { id: res.item_id }) : t("misc:signals.itemCreated")) : action === "remind" ? t("misc:signals.remindSet") : t("misc:signals.ignoreSet")
      toast(msg, cs ? { action: { label: t("misc:signals.undo"), onClick: () => void flowApi.undoConfig(cs).then(() => { toast(t("misc:signals.undone")); reload() }, (e) => toast(errText(e))) } } : res.item_id ? { action: { label: t("misc:signals.view"), onClick: () => go("#/items/" + res.item_id) } } : undefined)
      reload()
    } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const who = s0.handled_by ? botName(s0.handled_by) : ""
  return (
    <div className="flex flex-col gap-2.5 px-2 pb-4 pl-9 type-callout text-muted-foreground">
      {many && <div className="max-h-48 overflow-y-auto border-l pl-3">{g.map((x) => <div key={x.id} className="py-0.5 text-foreground">{x.summary} <span className="tabular-nums text-muted-foreground">{tm(x.received)}</span></div>)}</div>}
      {!many && (s0.actor || s0.connection) && <div className="type-footnote">{[s0.actor?.name, s0.connection?.name, sourceLabel(s0.source)].filter(Boolean).join(" · ")}</div>}
      {s0.reason && <div>{who ? <Trans t={t} i18nKey="misc:signals.reason" values={{ who, reason: s0.reason }} components={{ who: <span className="text-foreground" /> }} /> : s0.reason}</div>}
      <div>
        <button type="button" className="inline-flex items-center gap-1 type-footnote hover:text-foreground" onClick={() => void toggleRaw()} aria-expanded={!!raw} data-raw={s0.id}><FileIcon className="size-3.5" aria-hidden />{raw ? t("misc:signals.rawHide") : t("misc:signals.rawShow")}<span className="text-tertiary">{t("misc:signals.keep", { count: Number(keepDays()) })}</span></button>
        {raw && (raw.gone ? <div className="mt-1.5 type-footnote" data-raw-gone>{t("misc:signals.rawGone")}</div> : raw.err ? <div className="mt-1.5 type-footnote text-destructive">{raw.err}</div> : <Json v={raw.v} className="mt-1.5 max-h-72" />)}
      </div>
      {s0.flow && <a href={s0.flow.kind === "schedule" ? "#/auto/schedules" : "#/auto/rule/" + s0.flow.id} className="inline-flex items-center gap-1 hover:text-foreground" data-sig-flow><WorkflowIcon className="size-3.5" aria-hidden />{s0.flow.kind === "schedule" ? t("misc:signals.bySchedule", { name: s0.flow.name }) : t("misc:signals.byFlow", { name: s0.flow.name })}</a>}
      {s0.work_item_id && <a href={"#/items/" + s0.work_item_id} className="inline-flex items-center gap-1 text-blue hover:underline">{s0.work_item_id} {st.items.find((i) => i.id === s0.work_item_id)?.t ?? ""}<ArrowRightIcon className="size-3" aria-hidden /></a>}
      {s0.flow && <div>
        <button type="button" className="inline-flex items-center gap-1 type-footnote hover:text-foreground" onClick={() => void toggleRuns()} aria-expanded={showRuns} data-sig-runs><ListTreeIcon className="size-3.5" aria-hidden />{showRuns ? t("misc:signals.runsHide") : t("misc:signals.runs")}</button>
        {showRuns && (runs ? <RunSteps runs={runs} /> : <div className="mt-1.5 type-footnote">{t("misc:signals.loading")}</div>)}
      </div>}
      <div className="flex flex-wrap items-center gap-1.5 pt-1"><span className="mr-1 type-footnote">{many ? t("misc:signals.wrongGroup") : t("misc:signals.wrong")}</span>
        <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("remind")} data-fix="remind">{t("misc:signals.remind")}</Button>
        <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("ignore")} data-fix="ignore">{t("misc:signals.ignore")}</Button>
        {!s0.work_item_id && <Button size="xs" variant="outline" disabled={busy} onClick={() => void fix("to_item")} data-fix="to_item">{t("misc:signals.toItem")}</Button>}
      </div>
    </div>
  )
}

/** the runs that handled a signal, each step with its branch and output */
export function RunSteps({ runs }: { runs: SignalDetail["runs"] }) {
  const t = useT()
  if (!runs.length) return <div className="mt-1.5 type-footnote">{t("misc:signals.noRuns")}</div>
  return (
    <div className="mt-1.5 flex flex-col gap-3" data-run-steps>
      {runs.map((run) => (
        <div key={run.id} className="rounded-lg bg-muted/60 px-3 py-2.5">
          <div className="mb-1 flex items-center gap-1.5 type-footnote"><StatusDot s={RUN_STATUS[run.status]?.[1] ?? "idle"} /><span className="text-foreground">{RUN_STATUS[run.status]?.[0] ?? run.status}</span><span>· v{run.flow_version}</span>{run.outcome_label && <span>· {run.outcome_label}</span>}<a className="ml-auto hover:text-foreground" href={"#/auto/rule/" + run.flow_id + "?run=" + run.id}>{t("misc:signals.viewInFlow")}</a></div>
          <ol className="flex flex-col gap-0.5">{(run.steps ?? []).map((s) => (
            <li key={s.seq} className="flex items-baseline gap-2 type-footnote"><span className="w-4 shrink-0 text-right font-mono tabular-nums text-tertiary">{s.seq}</span><span className="font-mono text-foreground">{s.node_id}</span>
              <span className="min-w-0 flex-1 truncate">{s.error ? <span className="text-destructive">{s.error}</span> : s.branch ? "→ " + s.branch : s.status === "skipped" ? t("misc:signals.skipped") : s.status === "waiting" ? t("misc:signals.waiting") : ""}</span><span className="shrink-0 tabular-nums">{ms(s.ms)}</span></li>))}</ol>
        </div>
      ))}
    </div>
  )
}
