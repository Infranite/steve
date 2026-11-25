import { VirtualList } from "@/components/virtual-list"
import { useArchived, useViewTable } from "@/data/sync"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { laneOf } from "@/item-lanes"
import { archive, bulk, closedSt, deleteItem, DOMAIN_CATS, getItems, isKind, itemDisplayState, itemDisplayStatus, itemOf, kindTags, st, tag, type Item } from "@/store"
import { BotName, Chips, Empty, go, Group, KindTag, openSheet, Page, SectionLabel, Segmented, StatusDot } from "@/ui"
import { Count, ItemRow, Select, selected } from "@/view-shared"
import { t as tr, useT } from "@/i18n"
import { cn } from "cn"
import { PlusIcon, SearchIcon } from "lucide-react"
import { lazy, Suspense, useRef, useState } from "react"

let selecting = false

/* ═════════ 事项 ═════════ */
const ItemsBoard = lazy(() => import("@/item-board").then((m) => ({ default: m.ItemsBoard })))
/* view, filter and sort are keys; their labels are translated where they are rendered */
type ItemsFilt = "all" | "wait" | "run" | "star" | "arch"
type ItemsSort = "updated" | "due" | "pri" | "no"
let itemsView: "list" | "board" = "list"; let filt: ItemsFilt = "all", q = "", botF = "all", sortBy: ItemsSort = "updated", tagF = "all", kindF = "all", projF = "all"
const FILTS: ItemsFilt[] = ["all", "wait", "run", "star", "arch"]
const filtLabel = (k: ItemsFilt) => tr(({ all: "views:items.filterAll", wait: "views:items.filterWait", run: "views:items.filterRun", star: "views:items.filterStar", arch: "views:items.filterArch" } as const)[k])
const SORTS: ItemsSort[] = ["updated", "due", "pri", "no"]
const sortLabel = (k: ItemsSort) => tr(({ updated: "views:items.sortUpdated", due: "views:items.sortDue", pri: "views:items.sortPri", no: "views:items.sortNo" } as const)[k])
/** finished and dropped items shown before “显示全部”; the rest are a click away */
const CLOSED_SHOWN = 8
/** a filter select in the toolbar: compact, and muted until it narrows the list */
const FILTER = (set: boolean) => cn("h-7 px-2 type-callout [&_.ui-selc__chevron]:opacity-60", set ? "bg-accent font-medium! text-foreground" : "text-muted-foreground"); const openGroups = new Set<string>()
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
  /* the counts above, the 等你 / 进行中 filters, the board's columns and the running-status pill sort items one way (item-lanes.ts) */
  const unarchived = getItems().filter((i) => !i.arch)
  const open = unarchived.map((i) => laneOf(itemDisplayState(i)))
  let xs = pool.filter((i) => (arch || filt === "all" || (filt === "wait" && laneOf(itemDisplayState(i)) === "needs") || (filt === "run" && laneOf(itemDisplayState(i)) === "running") || (filt === "star" && i.star))
    && (botF === "all" || i.b === botF) && (tagF === "all" || !!i.tags?.includes(tagF)) && (kindF === "all" || !!i.tags?.includes(kindF)) && (projF === "all" || (projF === "none" ? !i.project : i.project === projF)) && (!q || i.t.includes(q) || i.id.toLowerCase().includes(q.toLowerCase())))
  if (sortBy === "updated") xs = [...xs].sort((a, b) => b.updated - a.updated)
  if (sortBy === "due") xs = [...xs].sort((a, b) => (a.due ? 0 : 1) - (b.due ? 0 : 1))
  if (sortBy === "pri") xs = [...xs].sort((a, b) => (PRI_ORDER[a.pri ?? ""] ?? 1) - (PRI_ORDER[b.pri ?? ""] ?? 1))
  if (sortBy === "no") xs = [...xs].sort((a, b) => +b.id.split("-")[1] - +a.id.split("-")[1])
  const groups: [Item["st"], string][] = [["wait", t("views:items.groupWait")], ["prop", t("views:items.groupProp")], ["run", t("views:items.groupRun")], ["block", t("views:items.groupBlock")], ["pause", t("views:items.groupPause")], ["done", t("views:items.groupDone")], ["fail", t("views:items.groupFail")], ["drop", t("views:items.groupDrop")]]
  const seg = <Segmented value={itemsView} onChange={(v) => { itemsView = v; selecting = false; selected.clear(); r() }} options={[["list", t("views:items.viewList")], ["board", t("views:items.viewBoard")]] as const} />
  const pick = selecting ? (id: string) => () => { selected.has(id) ? selected.delete(id) : selected.add(id); r() } : () => undefined
  const ids = [...selected]
  const done = () => { selected.clear(); selecting = false; r() }
  /* the project and bot columns only when the visible items differ in them */
  const multiProj = new Set(xs.map((i) => i.project).filter(Boolean)).size > 1, multiBot = new Set(xs.map((i) => i.b).filter(Boolean)).size > 1
  const rowOf = (i: Item) => {
    if (arch) {
      return <div key={i.id} className="flex items-center"><div className="min-w-0 flex-1"><ItemRow i={i} proj={multiProj} bot={multiBot} /></div>
        <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => archive([i.id], false)} data-unarchive={i.id}>{t("views:items.restore")}</Button>
        <Button size="xs" variant="ghost" className="text-destructive" onClick={() => setConfirmDel(i.id)} data-delete-item={i.id}>{t("common:action.delete")}</Button></div>
    }
    return <ItemRow key={i.id} i={i} pick={pick(i.id)} proj={multiProj} bot={multiBot} />
  }
  const runCount = open.filter((l) => l === "running").length, waitCount = open.filter((l) => l === "needs").length
  const subText = arch ? t("views:items.subArch") : t("views:items.sub", { run: runCount, wait: waitCount })
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
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { xs.forEach((i) => selected.add(i.id)); r() }}>{t("views:items.selectAll")}</Button>
          <span className="flex-1" />
          {([["pause", t("views:items.bulkPause")], ["run", t("views:items.bulkRun")], ["arch", t("views:items.bulkArch")], ["drop", t("views:items.bulkDrop")]] as const).map(([op, l]) => <Button key={op} size="sm" variant={op === "drop" ? "ghost" : "outline"} className={cn(op === "drop" && "text-destructive")} disabled={!ids.length} onClick={() => { bulk(ids, op); done() }} data-bulk={op}>{l}</Button>)}
        </div>
      )}
      <div key={itemsView}>
      {itemsView === "list" && (xs.length ? (arch || sortBy !== "updated"
        ? <Group><VirtualList rows={xs} scroll={scroll} render={rowOf} /></Group>
        : <div className="flex flex-col gap-8">{groups.map(([s, l]) => {
            const g = xs.filter((i) => itemDisplayStatus(i) === s), fold = closedSt(s) && !openGroups.has(s) && g.length > CLOSED_SHOWN + 2
            return g.length ? (
              <section key={s} data-items-group={s}>
                <SectionLabel className="mb-1 text-tertiary" right={fold || openGroups.has(s) ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { openGroups.has(s) ? openGroups.delete(s) : openGroups.add(s); r() }} data-group-more={s}>{fold ? t("views:items.showAll", { count: g.length }) : t("views:items.collapse")}</Button> : undefined}>
                  <span className="inline-flex items-center gap-2"><StatusDot s={s} />{l}</span><Count n={g.length} />
                </SectionLabel>
                <Group><VirtualList rows={fold ? g.slice(0, CLOSED_SHOWN) : g} scroll={scroll} render={rowOf} /></Group>
              </section>) : null })}</div>)
        : <Empty title={arch ? t("views:items.emptyArch") : getItems().length ? t("views:items.emptyFiltered") : t("views:items.emptyNone")} sub={arch ? (st.cfg.archDays === "0" ? t("views:items.archManual") : t("views:items.archAuto", { count: Number(st.cfg.archDays) })) : getItems().length ? undefined : t("views:items.emptyNoneSub")} />)}
      {arch && history.more && <Button size="sm" variant="ghost" onClick={() => void history.load()}>{t("common:action.more")}</Button>}
      {itemsView === "board" && <Suspense><ItemsBoard xs={xs} cap={CLOSED_SHOWN} /></Suspense>}
      </div>
    </Page>
  )
}
