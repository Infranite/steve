import { VirtualList } from "@/components/virtual-list"
import { useArchived, useViewTable } from "@/data/sync"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { isAttachedWorkflowChild, isWorkflowChild, itemDisplayState, itemDisplayStatus, partitionSubtasks } from "@/item-workflow-relation"
import { laneOf } from "@/item-lanes"
import { archive, bulk, closedSt, deleteItem, DOMAIN_CATS, getItems, isKind, itemOf, kindTags, st, tag, type Item } from "@/store"
import { BotName, Chips, Empty, go, Group, KindTag, openSheet, Page, SectionLabel, Segmented, StatusDot } from "@/ui"
import { Count, ItemRow, Select, selected } from "@/view-shared"
import { t as tr, useT } from "@/i18n"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, SearchIcon, WorkflowIcon } from "lucide-react"
import { lazy, Suspense, useRef, useState, type MouseEvent } from "react"

let selecting = false

/* ═════════ 事项 ═════════ */
/* the 协作 view (Agent Fleet) is a big component: loaded the first time it is opened */
const ItemsBoard = lazy(() => import("@/item-board").then((m) => ({ default: m.ItemsBoard })))
const ItemsFleet = lazy(() => import("@/items-collab").then((m) => ({ default: m.ItemsFleet })))
/* view, filter and sort are keys; their labels are translated where they are rendered */
type ItemsFilt = "all" | "wait" | "run" | "star" | "arch"
type ItemsSort = "updated" | "due" | "pri" | "no"
let itemsView: "list" | "board" | "collab" = "list"; let filt: ItemsFilt = "all", q = "", botF = "all", sortBy: ItemsSort = "updated", tagF = "all", kindF = "all", projF = "all"
const FILTS: ItemsFilt[] = ["all", "wait", "run", "star", "arch"]
const filtLabel = (k: ItemsFilt) => tr(({ all: "views:items.filterAll", wait: "views:items.filterWait", run: "views:items.filterRun", star: "views:items.filterStar", arch: "views:items.filterArch" } as const)[k])
const SORTS: ItemsSort[] = ["updated", "due", "pri", "no"]
const sortLabel = (k: ItemsSort) => tr(({ updated: "views:items.sortUpdated", due: "views:items.sortDue", pri: "views:items.sortPri", no: "views:items.sortNo" } as const)[k])
/** finished and dropped items shown before “显示全部”; the rest are a click away */
const CLOSED_SHOWN = 8
/** a filter select in the toolbar: compact, and muted until it narrows the list */
const FILTER = (set: boolean) => cn("h-7 px-2 type-callout [&_.ui-selc__chevron]:opacity-60", set ? "bg-accent font-medium! text-foreground" : "text-muted-foreground"); const openGroups = new Set<string>(), collapsedTrees = new Set<string>(), expandedSettled = new Set<string>()
/** the list, filtered: the stats page's drill-downs land here (“在事项页查看”) */
export const showItems = (f: "all" | "wait" | "run", project?: string, bot?: string) => { filt = f; projF = project ?? "all"; botF = bot ?? "all"; tagF = "all"; kindF = "all"; q = ""; itemsView = "list"; go("#/items") }
export const showTag = (id: string) => { if (isKind(tag(id))) kindF = id; else tagF = id; filt = "all"; itemsView = "list"; go("#/items") }
const PRI_ORDER: Record<string, number> = { 高: 0, 中: 1, 低: 2 }
export function ItemsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [confirmDel, setConfirmDel] = useState<string | null>(null)
  const arch = filt === "arch"
  const scroll = useRef<HTMLDivElement>(null)
  useViewTable()
  const history = useArchived(arch)
  const pool = arch ? history.rows.map((view) => itemOf(view)) : getItems()
  /* the counts above, the 等你 / 进行中 filters, the board's columns and the running-status pill sort root items one way (item-lanes.ts), while nesting workflow children under their parent */
  const unarchived = getItems().filter((i) => !i.arch)
  const open = unarchived.filter((i) => !isAttachedWorkflowChild(i, unarchived)).map((i) => laneOf(itemDisplayState(i)))
  const subRunning = unarchived.filter((i) => isAttachedWorkflowChild(i, unarchived) && laneOf(itemDisplayState(i)) === "running").length
  const childrenByParent = new Map<string, Item[]>()
  for (const c of pool) {
    if ((arch ? c.arch : !c.arch) && isWorkflowChild(c)) {
      const pid = c.parent!.item!
      const arr = childrenByParent.get(pid) ?? []
      arr.push(c)
      childrenByParent.set(pid, arr)
    }
  }
  const effectiveUpdated = (i: Item) => {
    const kids = childrenByParent.get(i.id)
    if (!kids?.length) return i.updated
    let max = i.updated
    for (const c of kids) if (c.updated > max) max = c.updated
    return max
  }
  let xs = pool.filter((i) => (arch || filt === "all" || (filt === "wait" && laneOf(itemDisplayState(i)) === "needs") || (filt === "run" && laneOf(itemDisplayState(i)) === "running") || (filt === "star" && i.star))
    && (botF === "all" || i.b === botF) && (tagF === "all" || !!i.tags?.includes(tagF)) && (kindF === "all" || !!i.tags?.includes(kindF)) && (projF === "all" || (projF === "none" ? !i.project : i.project === projF)) && (!q || i.t.includes(q) || i.id.toLowerCase().includes(q.toLowerCase())))
  const narrowed = !!q || botF !== "all" || tagF !== "all" || kindF !== "all" || projF !== "all" || filt === "star"
  const attachPool = arch ? pool : narrowed ? xs : unarchived
  let listRoots = xs.filter((i) => !isAttachedWorkflowChild(i, attachPool))
  if (sortBy === "updated") listRoots = [...listRoots].sort((a, b) => effectiveUpdated(b) - effectiveUpdated(a))
  if (sortBy === "due") listRoots = [...listRoots].sort((a, b) => (a.due ? 0 : 1) - (b.due ? 0 : 1))
  if (sortBy === "pri") listRoots = [...listRoots].sort((a, b) => (PRI_ORDER[a.pri ?? ""] ?? 1) - (PRI_ORDER[b.pri ?? ""] ?? 1))
  if (sortBy === "no") listRoots = [...listRoots].sort((a, b) => +b.id.split("-")[1] - +a.id.split("-")[1])
  const groups: [Item["st"], string][] = [["wait", t("views:items.groupWait")], ["prop", t("views:items.groupProp")], ["run", t("views:items.groupRun")], ["block", t("views:items.groupBlock")], ["pause", t("views:items.groupPause")], ["done", t("views:items.groupDone")], ["fail", t("views:items.groupFail")], ["drop", t("views:items.groupDrop")]]
  const seg = <Segmented value={itemsView} onChange={(v) => { itemsView = v; selecting = false; selected.clear(); r() }} options={[["list", t("views:items.viewList")], ["board", t("views:items.viewBoard")], ["collab", t("views:items.viewCollab")]] as const} />
  const pick = selecting ? (id: string) => () => { selected.has(id) ? selected.delete(id) : selected.add(id); r() } : () => undefined
  const ids = [...selected]
  const done = () => { selected.clear(); selecting = false; r() }
  /* the project and bot columns only when the visible items differ in them */
  const multiProj = new Set(listRoots.map((i) => i.project).filter(Boolean)).size > 1, multiBot = new Set(listRoots.map((i) => i.b).filter(Boolean)).size > 1
  const rowOf = (i: Item) => {
    if (arch) {
      return <div key={i.id} className="flex items-center"><div className="min-w-0 flex-1"><ItemRow i={i} proj={multiProj} bot={multiBot} /></div>
        <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => archive([i.id], false)} data-unarchive={i.id}>{t("views:items.restore")}</Button>
        <Button size="xs" variant="ghost" className="text-destructive" onClick={() => setConfirmDel(i.id)} data-delete-item={i.id}>{t("common:action.delete")}</Button></div>
    }
    const kids = childrenByParent.get(i.id)
    if (!kids?.length) return <ItemRow key={i.id} i={i} pick={pick(i.id)} proj={multiProj} bot={multiBot} />
    const sub = partitionSubtasks(i, kids)
    const parentClosed = closedSt(i.st)
    const treeOpen = parentClosed ? expandedSettled.has(i.id) : !collapsedTrees.has(i.id)
    const settledOpen = expandedSettled.has(i.id)
    const failLive = sub.live.filter((c) => itemDisplayStatus(c) === "fail").length
    const runLive = sub.live.length - failLive
    const badgeLabel = failLive > 0 && runLive === 0
      ? (sub.doneCount > 0 ? t("views:items.subtasksFailedDone", { failed: failLive, done: sub.doneCount }) : t("views:items.subtasksFailed", { count: failLive }))
      : sub.live.length > 0
        ? (sub.doneCount > 0 ? t("views:items.subtasksLive", { live: sub.live.length, done: sub.doneCount }) : t("views:items.subtasksRunning", { count: sub.live.length }))
        : t("views:items.subtasksDone", { count: sub.doneCount || sub.total })
    const toggleTree = (e: MouseEvent) => {
      e.stopPropagation()
      if (parentClosed) {
        expandedSettled.has(i.id) ? expandedSettled.delete(i.id) : expandedSettled.add(i.id)
      } else {
        collapsedTrees.has(i.id) ? collapsedTrees.delete(i.id) : collapsedTrees.add(i.id)
      }
      r()
    }
    const badge = (
      <button type="button" onClick={toggleTree} data-subtasks-toggle={i.id}
        className={cn("inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 type-footnote font-normal tabular-nums transition-colors", failLive > 0 ? "bg-destructive/12 text-destructive hover:bg-destructive/18" : "bg-muted/70 text-muted-foreground hover:bg-muted hover:text-foreground")}>
        <WorkflowIcon className="size-3 shrink-0 opacity-75" />
        <span className="max-w-36 truncate">{badgeLabel}</span>
        <ChevronRightIcon className={cn("size-3 shrink-0 transition-transform duration-150", treeOpen && "rotate-90")} />
      </button>
    )
    const showTreeBody = treeOpen && (sub.live.length > 0 || sub.settled.length > 0)
    const settledText = sub.doneCount > 0 && sub.retiredCount > 0
      ? t("views:items.settledMixed", { done: sub.doneCount, tries: sub.retiredCount })
      : sub.doneCount > 0
        ? t("views:items.settledDone", { count: sub.doneCount })
        : t("views:items.settledTries", { count: sub.retiredCount })
    return (
      <div key={i.id} data-item-tree={i.id} className="flex flex-col">
        <ItemRow i={i} pick={pick(i.id)} proj={multiProj} bot={multiBot} badge={badge} />
        {showTreeBody && (
          <div data-subtasks-of={i.id} className="ml-5 mb-1.5 flex flex-col gap-0.5 border-l border-border/75 pl-2.5">
            {sub.live.map((c) => <ItemRow key={c.id} i={c} pick={pick(c.id)} proj={false} bot={true} nested />)}
            {!parentClosed && sub.settled.length > 0 && (
              <div className="flex flex-col">
                <button type="button" onClick={(e) => { e.stopPropagation(); settledOpen ? expandedSettled.delete(i.id) : expandedSettled.add(i.id); r() }} data-subtasks-more={i.id}
                  className="relative flex max-w-full self-start items-center gap-1 rounded-md px-2 py-1 text-left type-footnote text-muted-foreground transition-colors hover:bg-accent hover:text-foreground before:pointer-events-none before:absolute before:-left-2.5 before:top-1/2 before:w-2.5 before:border-t before:border-border/75">
                  <ChevronRightIcon className={cn("size-3 shrink-0 transition-transform duration-150", settledOpen && "rotate-90")} />
                  <span className="truncate">{settledOpen ? t("views:items.hideSettled") : settledText}</span>
                </button>
                {settledOpen && sub.settled.map((c) => <ItemRow key={c.id} i={c} pick={pick(c.id)} proj={false} bot={true} nested />)}
              </div>
            )}
            {parentClosed && sub.settled.map((c) => <ItemRow key={c.id} i={c} pick={pick(c.id)} proj={false} bot={true} nested />)}
          </div>
        )}
      </div>
    )
  }
  const runCount = open.filter((l) => l === "running").length, waitCount = open.filter((l) => l === "needs").length
  const subText = arch ? t("views:items.subArch") : subRunning > 0 ? t("views:items.subWithChildren", { run: runCount, wait: waitCount, count: subRunning }) : t("views:items.sub", { run: runCount, wait: waitCount })
  return (
    <Page scrollRef={scroll} virtual={itemsView === "list"} title={t("views:items.title")} sub={subText} max={1080}
      actions={<>{itemsView === "list" && !arch && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { selecting = !selecting; selected.clear(); r() }} data-select-mode>{selecting ? t("views:items.selectDone") : t("views:items.select")}</Button>}<Button size="sm" onClick={() => openSheet("newItem")} data-new-item><PlusIcon />{t("views:items.newItem")}</Button></>}
      toolbar={<span className="hidden sm:block">{seg}</span>}>
      <AlertDialog open={!!confirmDel} onOpenChange={(o) => !o && setConfirmDel(null)}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("views:items.deleteTitle", { id: confirmDel })}</AlertDialogTitle><AlertDialogDescription>{t("views:items.deleteDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteItem(confirmDel!); setConfirmDel(null) }} data-delete-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <div className="mb-4 sm:hidden">{seg}</div>
      <div className="mb-6 flex items-center gap-1 max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none] sm:flex-wrap [&>*]:shrink-0" data-items-toolbar>
        <Chips value={filtLabel(filt)} onChange={(v) => { filt = FILTS.find((k) => filtLabel(k) === v) ?? "all"; done() }} options={FILTS.map(filtLabel)} />
        <span className="flex-1 max-sm:hidden" />
        <Select quiet className={FILTER(kindF !== "all")} testId="kind-filter" value={tag(kindF) ? kindF : "all"} onChange={(v) => { kindF = v; r() }} options={[{ v: "all", l: t("views:items.allKinds") }, ...kindTags().map((x) => ({ v: x.id, l: <KindTag id={x.id} className="border-0 bg-transparent px-0" /> }))]} />
        <Select quiet className={FILTER(tagF !== "all")} testId="tag-filter" value={tag(tagF) ? tagF : "all"} onChange={(v) => { tagF = v; r() }} options={[{ v: "all", l: t("views:items.allTags") }, ...DOMAIN_CATS.flatMap((c) => st.tags.filter((x) => x.cat === c).map((x) => ({ v: x.id, l: <span>{x.name} <span className="text-muted-foreground">· {c}</span></span> })))]} />
        <Select quiet className={FILTER(projF !== "all")} testId="project-filter" value={st.projects.some((p) => p.id === projF) || projF === "none" ? projF : "all"} onChange={(v) => { projF = v; r() }} options={[{ v: "all", l: t("views:items.allProjects") }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: t("views:items.noProject") }]} />
        <Select quiet className={FILTER(botF !== "all")} testId="bot-filter" value={botF} onChange={(v) => { botF = v; r() }} options={[{ v: "all", l: t("views:items.allBots") }, ...Object.keys(st.bots).map((k) => ({ v: k, l: <BotName k={k} /> }))]} />
        {itemsView === "list" && <Select quiet className={FILTER(sortBy !== "updated")} testId="sort" value={sortBy} onChange={(v) => { sortBy = v as ItemsSort; r() }} options={SORTS.map((v) => ({ v, l: sortLabel(v) }))} />}
        <Input prefix={<SearchIcon className="size-3.5" />} className={cn("steve-collapse-search h-7 type-callout", q ? "w-40" : "w-7 focus-within:w-40")} placeholder={t("views:items.search")} aria-label={t("views:items.search")} data-items-search defaultValue={q} onChange={(e) => { q = e.target.value; r() }} />
      </div>
      {selecting && (
        <div className="anim-in sticky top-2 z-10 mb-6 flex flex-wrap items-center gap-2 rounded-xl bg-popover/95 px-3 py-2 shadow-md backdrop-blur-xl" data-bulk-bar>
          <span className="type-callout font-medium tabular-nums">{t("views:items.selected", { count: ids.length })}</span>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { listRoots.forEach((i) => selected.add(i.id)); r() }}>{t("views:items.selectAll")}</Button>
          <span className="flex-1" />
          {([["pause", t("views:items.bulkPause")], ["run", t("views:items.bulkRun")], ["arch", t("views:items.bulkArch")], ["drop", t("views:items.bulkDrop")]] as const).map(([op, l]) => <Button key={op} size="sm" variant={op === "drop" ? "ghost" : "outline"} className={cn(op === "drop" && "text-destructive")} disabled={!ids.length} onClick={() => { bulk(ids, op); done() }} data-bulk={op}>{l}</Button>)}
        </div>
      )}
      <div key={itemsView}>
      {itemsView === "list" && (listRoots.length ? (arch || sortBy !== "updated"
        ? <Group><VirtualList rows={listRoots} scroll={scroll} render={rowOf} /></Group>
        : <div className="flex flex-col gap-8">{groups.map(([s, l]) => {
            const g = listRoots.filter((i) => itemDisplayStatus(i) === s), fold = closedSt(s) && !openGroups.has(s) && g.length > CLOSED_SHOWN + 2
            return g.length ? (
              <section key={s} data-items-group={s}>
                <SectionLabel className="mb-1 text-tertiary" right={fold || openGroups.has(s) ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { openGroups.has(s) ? openGroups.delete(s) : openGroups.add(s); r() }} data-group-more={s}>{fold ? t("views:items.showAll", { count: g.length }) : t("views:items.collapse")}</Button> : undefined}>
                  <span className="inline-flex items-center gap-2"><StatusDot s={s} />{l}</span><Count n={g.length} />
                </SectionLabel>
                <Group><VirtualList rows={fold ? g.slice(0, CLOSED_SHOWN) : g} scroll={scroll} render={rowOf} /></Group>
              </section>) : null })}</div>)
        : <Empty title={arch ? t("views:items.emptyArch") : getItems().length ? t("views:items.emptyFiltered") : t("views:items.emptyNone")} sub={arch ? (st.cfg.archDays === "0" ? t("views:items.archManual") : t("views:items.archAuto", { count: Number(st.cfg.archDays) })) : getItems().length ? undefined : t("views:items.emptyNoneSub")} />)}
      {arch && history.more && <Button size="sm" variant="ghost" onClick={() => void history.load()}>{t("common:action.more")}</Button>}
      {itemsView === "board" && <Suspense><ItemsBoard xs={listRoots} cap={CLOSED_SHOWN} /></Suspense>}
      {itemsView === "collab" && <Suspense fallback={<div className="flex justify-center py-16"><Spinner className="size-4" /></div>}><ItemsFleet xs={xs} /></Suspense>}
      </div>
    </Page>
  )
}
