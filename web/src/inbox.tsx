import { getItems } from "@/store"
import { Fragment, memo, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react"
import { BookOpenIcon, ChevronRightIcon } from "lucide-react"
import { useVirtualizer } from "@tanstack/react-virtual"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { useOpenDecisions } from "@/data/sync"
import { lang, useT } from "@/i18n"
import { nearTime } from "@/i18n/format"
import { InboxDone } from "@/inbox-done"
import { itemDisplayState } from "@/item-workflow-relation"
import { dismissDrafts, memCards, pendingCards, snoozed, st, unsnooze, WT, type Orphan } from "@/store"
import { busyOf } from "@/thread/decide"
import { Chips, DecisionCard, Empty, go, Group, ItemNo, Page, Row, SectionLabel, Segmented } from "@/ui"
import { Count } from "@/view-shared"
import { RunState } from "@/yolo/status"
import { YoloPill, yoloState } from "@/yolo/switch"

/* ibType is a decision type key ("all" or one of `types`); the chips show labels, mapped back on change */
let ibTab: "pending" | "done" = "pending"; let ibType = "all"
export function InboxView() {
  const t = useT()
  const decisions = useOpenDecisions()
  const scroll = useRef<HTMLDivElement>(null)
  const [ignoring, setIgnoring] = useState(false)
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const all = pendingCards(decisions), sz = snoozed(), mems = memCards(decisions).length
  const p = all.filter((c) => c.w.tier === "decide")
  const drafts = new Map<string, Orphan[]>()
  const folded = all.filter((c) => c.w.tier === "folded")
  for (const c of all) {
    if (c.w.tier !== "draft") continue
    const key = c.w.space ?? c.w.decision
    const group = drafts.get(key)
    if (group) group.push(c)
    else drafts.set(key, [c])
  }
  async function ignoreDrafts() {
    setIgnoring(true)
    try { await dismissDrafts([...drafts.values()].flatMap((group) => group.map(({ w }) => w.decision))) }
    finally { setIgnoring(false) }
  }
  /* an item whose graph cannot run (its Node is gone, a Bot is off…) counts as waiting on you everywhere else, but asks
   * nothing: it is listed here too, with why, so 等你 always has something to open */
  const stuck = getItems().filter((i) => !i.arch && !p.some((c) => c.i.id === i.id) && itemDisplayState(i).kind === "unavailable")
  const types = ["accept", "approve", "ask", "propose", "fail", "confirm"] as const
  const typeKeys = ["all", ...types.filter((ty) => p.some((c) => c.w.type === ty))]
  const typeLabel = (k: string) => (k === "all" ? t("views:inbox.all") : WT[k])
  const currentType = typeKeys.includes(ibType) ? ibType : "all"
  const xs = p.filter((c) => currentType === "all" || c.w.type === currentType)
  return (
    <Page title={t("views:inbox.title")} sub={p.length + mems ? t("views:inbox.sub", { count: p.length + mems }) : t("views:inbox.subNone")} max={640} scrollRef={scroll}
      toolbar={<><YoloPill className="max-sm:hidden" /><YoloPill short className="sm:hidden" /><Segmented value={ibTab} onChange={(v) => { ibTab = v; r() }} options={[["pending", t("views:inbox.tabPending")], ["done", t("views:inbox.tabDone")]] as const} /></>}>
      {ibTab === "pending" && <>
        {yoloState(st.yolo) === "paused" && <RunState className="mb-6 rounded-xl bg-destructive/10 px-4 py-3" />}
        {p.length > 0 && <Chips className="mb-6" value={typeLabel(currentType)} onChange={(v) => { ibType = typeKeys.find((k) => typeLabel(k) === v) ?? "all"; r() }} options={typeKeys.map(typeLabel)} />}
        {!all.length && !stuck.length && !mems && !sz.length && <Empty title={t("views:inbox.emptyTitle")} sub={t("views:inbox.emptySub")} />}
        {mems > 0 && <button type="button" onClick={() => go("#/knowledge?tab=pending")} className="mb-8 flex w-full items-center gap-3 rounded-xl bg-card px-4 py-3 text-left type-body shadow-[var(--lift)] hover:shadow-[var(--lift-hover)]" data-inbox-mems>
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-blue/10 text-blue"><BookOpenIcon className="size-4" strokeWidth={1.75} /></span><span className="min-w-0 flex-1">{t("views:inbox.mems", { count: mems })}</span><ChevronRightIcon className="size-4 shrink-0 text-tertiary" /></button>}
        <div className="flex flex-col gap-8">
          {types.map((ty) => {
            const g = xs.filter((c) => c.w.type === ty)
            return g.length ? <section key={ty} className="anim-in"><SectionLabel>{WT[ty]}<Count n={g.length} /></SectionLabel><InboxRows scroll={scroll} rows={g.map((c) => ({ id: c.w.decision, body: <InboxDecision {...c} busy={busyOf(c.w)} /> }))} /></section> : null
          })}
          {drafts.size > 0 && <section data-inbox-drafts>
            <SectionLabel right={<Button size="xs" variant="ghost" disabled={ignoring} onClick={() => void ignoreDrafts()}>{t("views:inbox.ignoreDrafts")}</Button>}>
              {t("views:inbox.drafts")}<Count n={drafts.size} />
            </SectionLabel>
            <InboxRows scroll={scroll} rows={[...drafts].map(([id, group]) => {
              const c = group[group.length - 1]
              return { id, body: <div className="flex flex-col gap-1" data-draft-conversation={id}>
                <InboxDecision {...c} busy={busyOf(c.w)} />
                {group.length > 1 && <span className="px-1 type-footnote text-tertiary">{t("views:inbox.olderDrafts", { count: group.length - 1 })}</span>}
              </div> }
            })} />
          </section>}
          {folded.length > 0 && <section data-inbox-folded>
            <SectionLabel>{t("views:inbox.folded")}<Count n={folded.length} /></SectionLabel>
            <Group><InboxRows scroll={scroll} gap={0} estimate={64} rows={folded.map((c) => ({ id: c.w.decision, body: <FoldedRow c={c} /> }))} /></Group>
          </section>}
          {stuck.length > 0 && <section className="anim-in" data-inbox-stuck><SectionLabel>{t("views:inbox.stuck")}<Count n={stuck.length} /></SectionLabel>
            <Group>{stuck.map((i) => <Row key={i.id} onClick={() => go("#/items/" + i.id)} title={<span className="flex min-w-0 items-baseline gap-2"><ItemNo id={i.id} /><span className="min-w-0 truncate">{i.t}</span></span>}
              sub={<span className="line-clamp-2 text-destructive/90">{itemDisplayState(i).detail ?? t("views:inbox.stuckWhy")}</span>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group></section>}
          {sz.length > 0 && <section data-snoozed><SectionLabel>{t("views:inbox.snoozed")}<Count n={sz.length} /></SectionLabel>
            <Group>{sz.map((i) => <Row key={i.id} title={<span className="flex min-w-0 items-baseline gap-2"><ItemNo id={i.id} /><span className="min-w-0">{i.t}</span></span>} sub={t("views:inbox.snoozedAt", { type: WT[i.wait!.type], when: lang() === "zh-CN" ? i.snooze : nearTime(i.snoozeUntil) })}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => unsnooze(i.id)} data-unsnooze>{t("views:inbox.handleNow")}</Button></Row>)}</Group></section>}
        </div>
      </>}
      {ibTab === "done" && <InboxDone />}
    </Page>
  )
}

/* Let the page own scrolling. Short groups render directly, so a first paint needs no measurements. */
function InboxRows({ scroll, rows, estimate = 200, gap = 12 }: {
  scroll: RefObject<HTMLDivElement | null>; rows: { id: string; body: ReactNode }[]; estimate?: number; gap?: number
}) {
  const list = useRef<HTMLDivElement>(null)
  const [margin, setMargin] = useState(0)
  const enabled = rows.length > 24
  useLayoutEffect(() => {
    const el = list.current, parent = scroll.current
    if (!enabled || !el || !parent) return
    const measure = () => setMargin(el.getBoundingClientRect().top - parent.getBoundingClientRect().top + parent.scrollTop)
    const observer = new ResizeObserver(measure)
    observer.observe(parent)
    if (parent.firstElementChild) observer.observe(parent.firstElementChild)
    measure()
    return () => observer.disconnect()
  }, [enabled, scroll])
  const virtual = useVirtualizer({
    count: rows.length, enabled, getScrollElement: () => scroll.current, estimateSize: () => estimate,
    getItemKey: (index) => rows[index].id, scrollMargin: margin, overscan: 4, gap,
  })
  if (!enabled) return <div className="flex flex-col" style={{ gap }}>{rows.map(({ id, body }) => <Fragment key={id}>{body}</Fragment>)}</div>
  return <div ref={list} className="relative" style={{ height: virtual.getTotalSize() }}>
    {virtual.getVirtualItems().map((row) =>
      <div key={row.key} data-index={row.index} ref={virtual.measureElement} className="absolute inset-x-0 top-0"
        style={{ transform: `translateY(${row.start - margin}px)` }}>{rows[row.index].body}</div>)}
  </div>
}

const InboxDecision = memo(function InboxDecision({ i, w }: Orphan & { busy: boolean }) {
  return <DecisionCard i={i} w={w} />
})

/** a folded card: one line with why; it opens in place */
function FoldedRow({ c }: { c: ReturnType<typeof pendingCards>[number] }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  return <div data-folded={c.w.folded}>
    <Row onClick={() => setOpen(!open)} title={<span className="min-w-0 truncate">{c.w.q}</span>}
      sub={<span className="text-tertiary">{c.w.folded && t(`views:inbox.foldedWhy.${c.w.folded}`)}</span>}><ChevronRightIcon className={cn("size-4 text-tertiary transition-transform", open && "rotate-90")} /></Row>
    {open && <div className="px-2 pb-3"><DecisionCard i={c.i} w={c.w} /></div>}
  </div>
}
