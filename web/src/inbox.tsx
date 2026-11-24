import { useSettledDecisions } from "@/data/sync"
import { Fragment, memo, useRef, useState, type ReactNode, type RefObject } from "react"
import { BookOpenIcon, ChevronRightIcon } from "lucide-react"
import { VirtualList } from "@/components/virtual-list"
import { ItemSeparator } from "@/components/ui/item"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { useOpenDecisions } from "@/data/sync"
import { lang, useT } from "@/i18n"
import { nearTime } from "@/i18n/format"
import { InboxDone } from "@/inbox-done"
import { dismissDrafts, memCards, pendingCards, snoozed, unsnooze, WT, type Orphan } from "@/store"
import { busyOf } from "@/thread/decide"
import { Chips, Empty, go, Group, ItemNo, Page, Row, SectionLabel, Segmented } from "@/ui"
import { DecisionCard } from "@/thread/decision-card"
import { Count } from "@/view-shared"

/* ibType is a decision type key ("all" or one of `types`); the chips show labels, mapped back on change */
let ibTab: "pending" | "done" = "pending"; let ibType = "all"
export function InboxView() {
  useSettledDecisions(ibTab === "done")
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
  const virtualize = all.length + sz.length > 24
  const types = ["accept", "approve", "ask", "propose", "fail", "confirm"] as const
  const typeKeys = ["all", ...types.filter((ty) => p.some((c) => c.w.type === ty))]
  const typeLabel = (k: string) => (k === "all" ? t("views:inbox.all") : WT[k])
  const currentType = typeKeys.includes(ibType) ? ibType : "all"
  const xs = p.filter((c) => currentType === "all" || c.w.type === currentType)
  return (
    <Page title={t("views:inbox.title")} sub={p.length + mems ? t("views:inbox.sub", { count: p.length + mems }) : t("views:inbox.subNone")} max={640} scrollRef={scroll}
      toolbar={<><Segmented value={ibTab} onChange={(v) => { ibTab = v; r() }} options={[["pending", t("views:inbox.tabPending")], ["done", t("views:inbox.tabDone")]] as const} /></>}>
      {ibTab === "pending" && <>
        {p.length > 0 && <Chips className="mb-6" value={typeLabel(currentType)} onChange={(v) => { ibType = typeKeys.find((k) => typeLabel(k) === v) ?? "all"; r() }} options={typeKeys.map(typeLabel)} />}
        {!all.length && !mems && !sz.length && <Empty title={t("views:inbox.emptyTitle")} sub={t("views:inbox.emptySub")} />}
        {mems > 0 && <button type="button" onClick={() => go("#/knowledge?tab=pending")} className="mb-8 flex w-full items-center gap-3 rounded-xl bg-card px-4 py-3 text-left type-body shadow-[var(--lift)] hover:shadow-[var(--lift-hover)]" data-inbox-mems>
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-blue/10 text-blue"><BookOpenIcon className="size-4" strokeWidth={1.75} /></span><span className="min-w-0 flex-1">{t("views:inbox.mems", { count: mems })}</span><ChevronRightIcon className="size-4 shrink-0 text-tertiary" /></button>}
        <div className="flex flex-col gap-8">
          {types.map((ty) => {
            const g = xs.filter((c) => c.w.type === ty)
            return g.length ? <section key={ty} className="anim-in"><SectionLabel>{WT[ty]}<Count n={g.length} /></SectionLabel><InboxRows scroll={scroll} virtualize={virtualize} rows={g.map((card) => ({ id: card.w.decision, card }))}
              render={({ card }) => <InboxDecision {...card} busy={busyOf(card.w)} />} /></section> : null
          })}
          {drafts.size > 0 && <section data-inbox-drafts>
            <SectionLabel right={<Button size="xs" variant="ghost" disabled={ignoring} onClick={() => void ignoreDrafts()}>{t("views:inbox.ignoreDrafts")}</Button>}>
              {t("views:inbox.drafts")}<Count n={drafts.size} />
            </SectionLabel>
            <InboxRows scroll={scroll} virtualize={virtualize} estimate={65} rows={[...drafts].map(([id, group]) => ({ id, group }))}
              render={({ id, group }) => {
                const c = group[group.length - 1]
                return <div className="flex flex-col gap-1" data-draft-conversation={id}>
                  <Button variant="ghost" className="h-auto min-w-0 justify-between px-2 py-2 text-left" onClick={() => go("#/c/" + encodeURIComponent("chan:" + id))}>
                    <span className="min-w-0"><span className="block truncate">{c.w.q}</span><span className="block truncate text-muted-foreground">{c.w.draft}</span></span>
                    <ChevronRightIcon aria-hidden />
                  </Button>
                  {group.length > 1 && <span className="px-1 type-footnote text-tertiary">{t("views:inbox.olderDrafts", { count: group.length - 1 })}</span>}
                </div>
              }} />
          </section>}
          {folded.length > 0 && <section data-inbox-folded>
            <SectionLabel>{t("views:inbox.folded")}<Count n={folded.length} /></SectionLabel>
            <Group><InboxRows scroll={scroll} virtualize={virtualize} gap={0} estimate={64} rows={folded.map((card) => ({ id: card.w.decision, card }))}
              render={({ card }) => <FoldedRow c={card} />} /></Group>
          </section>}
          {sz.length > 0 && <section data-snoozed><SectionLabel>{t("views:inbox.snoozed")}<Count n={sz.length} /></SectionLabel>
            <InboxItemRows scroll={scroll} virtualize={virtualize} rows={sz} render={(i) => <Row title={<span className="flex min-w-0 items-baseline gap-2"><ItemNo id={i.id} /><span className="min-w-0">{i.t}</span></span>} sub={t("views:inbox.snoozedAt", { type: WT[i.wait!.type], when: lang() === "zh-CN" ? i.snooze : nearTime(i.snoozeUntil) })}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => unsnooze(i.id)} data-unsnooze>{t("views:inbox.handleNow")}</Button></Row>} /></section>}
        </div>
      </>}
      {ibTab === "done" && <InboxDone />}
    </Page>
  )
}

type InboxListProps<T> = {
  scroll: RefObject<HTMLDivElement | null>
  rows: T[]
  render: (row: T) => ReactNode
  virtualize: boolean
}

/* The total page length determines whether to window every group. */
function InboxRows<T extends { id: string }>({ scroll, rows, render, virtualize, estimate = 200, gap = 12 }: InboxListProps<T> & { estimate?: number; gap?: number }) {
  if (virtualize) return <VirtualList scroll={scroll} rows={rows} render={render} estimate={estimate} gap={gap} />
  return <div className="flex flex-col" style={{ gap }}>{rows.map((row) => <Fragment key={row.id}>{render(row)}</Fragment>)}</div>
}

function InboxItemRows<T extends { id: string }>({ scroll, rows, render, virtualize }: InboxListProps<T>) {
  if (!virtualize) return <Group>{rows.map((row) => <Fragment key={row.id}>{render(row)}</Fragment>)}</Group>
  return <Group><VirtualList scroll={scroll} rows={rows.map((row, index) => ({ id: row.id, row, index }))} estimate={58}
    render={({ row, index }) => <>{index > 0 && <ItemSeparator className="mx-2 my-0 w-auto!" />}{render(row)}</>} /></Group>
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
