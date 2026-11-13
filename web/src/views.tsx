import { AgentDetailPanel, hasAgentTree, History, Notices, TeamTimeline, useAgentTree, useSelectedNode, WorkflowButtons } from "@/agent-tree-view"
import { itemState, type ItemState } from "@/item-state"
import type { AgentStatus } from "@/agent-tree"
import { clip } from "@/thread/topics"
import { itemDisplayStatus, workflowRelation, workflowRelationText } from "@/item-workflow-relation"
import { DependencyWaitPanel } from "@/dependency-wait-panel"
import { ItemDebugDialog } from "@/debug-panel"
import { usePeopleOn } from "@/api/people"
import { Files } from "@/attach"
import { SessionCards } from "@/chat2"
import { Markdown } from "@/components/markdown"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card"
import { Input } from "@/components/ui/input"
import { Progress } from "@/components/ui/progress"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { draftKey, newClientId } from "@/composer-draft"
import { GitHubMoreRow } from "@/github/index"
import { HubMore } from "@/hub-ui"
import { SessionKnowledge } from "@/knowledge/parts"
import { lazyView } from "@/lazy"
import { AnnRefs } from "@/annotate"
import { OutDialog, OutList, type OutOpen } from "@/item-outputs"
import { ItemPeopleProp } from "@/people/item-people"
import { QueuedComment, queuedComments, queueNote, QueueStrip } from "@/queue"
import { NativeIdLine, ResumeCard } from "@/resume"
import { ItemStatsRows, SessionStats } from "@/stats/drill"
import { ACC, accKey, closedSt, accOf, active, addDep, archive, bulk, closeRel, deleteItem, depsOpen, doFork, hm, importSessions, interrupt, it, itemThread, link, loadEvents, loadItem, loadLocalSessions, markDone, newItem, patchItem, pending, pendingCards, memCards, reassign, rmDep, saveGoal, sessPatch, sessRemove, sessSend, setAcc, setSt, setTags, setKind, kindOf, kindTags, isKind, DOMAIN_CATS, snoozed, st, stageOf, star, supp, tag, tagCatLabel, tagDesc, tagLabel, threadFollow, tm, toast, undoDecision, unsnooze, WT, type Item, type PlanRound, type Reply } from "@/store"
import { BotAvatar, BotName, permissionQuestion, Chips, closeSheet, useScrollMore, Composer, DecisionCard, Empty, go, Group, ItemNo, KindTag, Meta, openSheet, Page, RelNote, ReplyBlock, Row, RunBar, SectionLabel, Segmented, SessionBlock, Status, StatusDot, Tags, ThemeSwitch, Transcript, type ComposerHandle, Chevron, Panel, Section, Tile, PANEL_W } from "@/ui"
import { ChoiceCards, Count, dueLabel, ItemRow, PlanBar, ProjectChip, Select, selected } from "@/view-shared"
import { choiceText, YoloItemSwitch, YoloMark } from "@/yolo/badges"
import { YoloPill, yoloState } from "@/yolo/switch"
import { RunState } from "@/yolo/status"
import { cn } from "cn"
import { lang, t as tr, useT } from "@/i18n"
import { dayHeading, hm as clock, nearTime, num } from "@/i18n/format"
import { ArchiveIcon, BanIcon, BookOpenIcon, BotIcon, CalendarIcon, ChartNoAxesColumnIcon, CheckIcon, ChevronRightIcon, CircleAlertIcon, CircleCheckIcon, CircleDashedIcon, CircleHelpIcon, ContactRoundIcon, CopyIcon, DownloadIcon, FolderIcon, GitPullRequestIcon, HourglassIcon, KeyRoundIcon, LinkIcon, ListIcon, LoaderCircleIcon, LockIcon, MailIcon, MessageSquareIcon, MessagesSquareIcon, MinusIcon, MoreHorizontalIcon, PanelRightIcon, PauseIcon, PencilIcon, PlayIcon, PlugIcon, PlusIcon, RadioIcon, RotateCcwIcon, SearchIcon, ServerIcon, SettingsIcon, SquareIcon, SquareTerminalIcon, StarIcon, TrashIcon, UserRoundIcon, WorkflowIcon, XIcon, ZapIcon } from "lucide-react"
import { Fragment, useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react"
export { ChoiceCards,ItemRowExport,Select } from "@/view-shared"
let selecting = false
const RelationGraph = lazyView(() => import("@/graph").then((m) => ({ default: m.RelationGraph })))
const GraphLegend = lazyView(() => import("@/graph").then((m) => ({ default: m.GraphLegend })))

/* stage values come from stageOf() in the store; STAGE_LABEL gives each one's translation key */
export const STAGES = ["接收", "分流", "定义", "规划", "准备", "执行", "交付与验收", "收尾"] as const
const STAGE_LABEL = { 接收: "views:stage.intake", 分流: "views:stage.triage", 定义: "views:stage.define", 规划: "views:stage.plan", 准备: "views:stage.prepare", 执行: "views:stage.execute", 交付与验收: "views:stage.deliver", 收尾: "views:stage.wrapUp" } as const
/* priority is stored as these Chinese values; only its labels are translated (due presets: dueLabel in view-shared; acceptance modes: ACC in the store) */
const PRI_LABEL = { 高: "views:item.priHigh", 中: "views:item.priMid", 低: "views:item.priLow" } as const
/** the label of a stored value, or the value itself when it is not one of ours (a due date the Hub wrote, say) */
const labelOf = <M extends Record<string, Parameters<typeof tr>[0]>>(m: M, v: string): string => (v in m ? String(tr(m[v as keyof M] as any)) : v)

const openNode = (n: { k: string; id: string }) => go((n.k === "item" ? "#/items/" : n.k === "proj" ? "#/projects/" : "#/sessions/") + n.id)

/* ═════════ 待处理 ═════════ */
/* ibType is a decision type key ("all" or one of `types`); the chips show labels, mapped back on change */
let ibTab: "pending" | "done" = "pending"; let ibType = "all"
export function InboxView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const p = pendingCards(), sz = snoozed(), mems = memCards().length
  const types = ["accept", "approve", "ask", "propose", "fail", "confirm"] as const
  const typeKeys = ["all", ...types.filter((ty) => p.some((c) => c.w.type === ty))]
  const typeLabel = (k: string) => (k === "all" ? t("views:inbox.all") : WT[k])
  const xs = p.filter((c) => ibType === "all" || c.w.type === ibType)
  return (
    <Page title={t("views:inbox.title")} sub={p.length ? t("views:inbox.sub", { count: p.length }) : t("views:inbox.subNone")} max={640}
      toolbar={<><YoloPill className="max-sm:hidden" /><YoloPill short className="sm:hidden" /><Segmented value={ibTab} onChange={(v) => { ibTab = v; r() }} options={[["pending", t("views:inbox.tabPending")], ["done", t("views:inbox.tabDone")]] as const} /></>}>
      {ibTab === "pending" && <>
        {yoloState(st.yolo) === "paused" && <RunState className="mb-6 rounded-xl bg-destructive/10 px-4 py-3" />}
        {p.length > 0 && <Chips className="mb-6" value={typeLabel(typeKeys.includes(ibType) ? ibType : "all")} onChange={(v) => { ibType = typeKeys.find((k) => typeLabel(k) === v) ?? "all"; r() }} options={typeKeys.map(typeLabel)} />}
        {p.length === 0 && !mems && <Empty title={t("views:inbox.emptyTitle")} sub={t("views:inbox.emptySub")} />}
        {mems > 0 && <button type="button" onClick={() => go("#/knowledge?tab=pending")} className="mb-8 flex w-full items-center gap-3 rounded-xl bg-card px-4 py-3 text-left type-body shadow-[var(--lift)] hover:shadow-[var(--lift-hover)]" data-inbox-mems>
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-blue/10 text-blue"><BookOpenIcon className="size-4" strokeWidth={1.75} /></span><span className="min-w-0 flex-1">{t("views:inbox.mems", { count: mems })}</span><ChevronRightIcon className="size-4 shrink-0 text-tertiary" /></button>}
        <div className="flex flex-col gap-8">
          {types.map((ty) => {
            const g = xs.filter((c) => c.w.type === ty)
            return g.length ? <section key={ty} className="anim-in"><SectionLabel>{WT[ty]}<Count n={g.length} /></SectionLabel><div className="flex flex-col gap-3">{g.map((c) => <DecisionCard key={c.w.decision} i={c.i} w={c.w} />)}</div></section> : null
          })}
          {sz.length > 0 && <section data-snoozed><SectionLabel>{t("views:inbox.snoozed")}<Count n={sz.length} /></SectionLabel>
            <Group>{sz.map((i) => <Row key={i.id} title={<span className="flex min-w-0 items-baseline gap-2"><ItemNo id={i.id} /><span className="min-w-0">{i.t}</span></span>} sub={t("views:inbox.snoozedAt", { type: WT[i.wait!.type], when: lang() === "zh-CN" ? i.snooze : nearTime(i.snoozeUntil) })}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => unsnooze(i.id)} data-unsnooze>{t("views:inbox.handleNow")}</Button></Row>)}</Group></section>}
        </div>
      </>}
      {ibTab === "done" && (st.done.length ? <>
        <Group>{st.done.map((d, n) => (
          <Row key={n} title={<span className="flex min-w-0 items-baseline gap-2"><ItemNo id={d.id} /><span className="min-w-0">{permissionQuestion(d.q)}</span></span>} sub={d.rel ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <RelNote rel={d.rel} className="inline-flex" /> · {d.tm}</span>
            : d.yolo !== undefined ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <YoloMark /> {d.yolo ? t("views:inbox.yoloChoseWhy", { choice: choiceText(d.choice), reason: d.yolo }) : t("views:inbox.yoloChose", { choice: choiceText(d.choice) })} · {d.tm}{d.undone ? ` · ${t("views:inbox.undone")}` : ""}</span>
            : `${WT[d.type]} · ${d.how === "dismissed" ? t("views:inbox.dismissed") : d.how === "expired" ? t("views:inbox.expired", { choice: d.choice }) : t("views:inbox.chose", { choice: d.choice })}${d.via ? ` · ${t("views:inbox.via", { client: d.via })}` : ""} · ${d.tm}${d.undone ? ` · ${t("views:inbox.undone")}` : ""}`} data-done-row={d.id}>
            {d.canUndo && <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => undoDecision(n)} data-undo-decision><RotateCcwIcon />{t("views:inbox.undo")}</Button>}
          </Row>))}</Group>
        <div className="mt-3 type-footnote text-tertiary">{t("views:inbox.doneNote")}</div>
      </> : <Empty title={t("views:inbox.doneEmpty")} />)}
    </Page>
  )
}

/* ═════════ 事项 ═════════ */
/* view, filter and sort are keys; their labels are translated where they are rendered */
type ItemsFilt = "all" | "wait" | "run" | "star" | "arch"
type ItemsSort = "updated" | "due" | "pri" | "no"
let itemsView: "list" | "board" | "graph" = "list"; let filt: ItemsFilt = "all", q = "", showAll = false, botF = "all", sortBy: ItemsSort = "updated", tagF = "all", kindF = "all", projF = "all"
const FILTS: ItemsFilt[] = ["all", "wait", "run", "star", "arch"]
const filtLabel = (k: ItemsFilt) => tr(({ all: "views:items.filterAll", wait: "views:items.filterWait", run: "views:items.filterRun", star: "views:items.filterStar", arch: "views:items.filterArch" } as const)[k])
const SORTS: ItemsSort[] = ["updated", "due", "pri", "no"]
const sortLabel = (k: ItemsSort) => tr(({ updated: "views:items.sortUpdated", due: "views:items.sortDue", pri: "views:items.sortPri", no: "views:items.sortNo" } as const)[k])
/** finished and dropped items shown before “显示全部”; the rest are a click away */
const CLOSED_SHOWN = 8
/** a filter select in the toolbar: compact, and muted until it narrows the list */
const FILTER = (set: boolean) => cn("h-7 px-2 type-callout [&>svg]:opacity-60", set ? "bg-accent font-medium! text-foreground" : "text-muted-foreground"); const openGroups = new Set<string>()
/** the list, filtered: the stats page's drill-downs land here (“在事项页查看”) */
export const showItems = (f: "全部" | "等你" | "进行中", project?: string, bot?: string) => { filt = f === "等你" ? "wait" : f === "进行中" ? "run" : "all"; projF = project ?? "all"; botF = bot ?? "all"; tagF = "all"; kindF = "all"; q = ""; itemsView = "list"; go("#/items") }
export const showTag = (id: string) => { if (isKind(tag(id))) kindF = id; else tagF = id; filt = "all"; itemsView = "list"; go("#/items") }
const PRI_ORDER: Record<string, number> = { 高: 0, 中: 1, 低: 2 }
/** the board: a column per open status. An empty column folds to a narrow rail with its name down the side, and the
 *  columns with cards share the rest (each at least 11rem), so three busy statuses fit a 1066px window beside the
 *  sidebar; when they still don't, the board scrolls sideways in its own area, columns snapping to its edge and the
 *  side with more fading out, and the page itself never scrolls sideways. On a phone a column with cards takes most of
 *  the width and the board snaps column by column, so a card is never squeezed to a few characters a line. */
function ItemsBoard({ groups, xs }: { groups: [Item["st"], string][]; xs: Item[] }) {
  const t = useT(), [ref, more] = useScrollMore<HTMLDivElement>("x")
  const cols = groups.map(([s, l]) => [s, l, xs.filter((i) => itemDisplayStatus(i) === s)] as const)
  return (
    <div ref={ref} className="-mx-1 grid snap-x snap-mandatory scroll-px-1 grid-flow-col grid-cols-[var(--cols-phone)] gap-3 overflow-x-auto px-1 pb-2 data-[more~=end]:mask-r-from-[calc(100%-3rem)] data-[more~=start]:mask-l-from-[calc(100%-3rem)] sm:snap-proximity sm:grid-cols-[var(--cols)]"
      style={{ "--cols": cols.map(([, , g]) => (g.length ? "minmax(11rem,1fr)" : "2.5rem")).join(" "), "--cols-phone": cols.map(([, , g]) => (g.length ? "minmax(78vw,1fr)" : "2.5rem")).join(" ") } as CSSProperties} data-items-board data-more={more}>
        {cols.map(([s, l, g]) => !g.length ? (
        <div key={s} className="flex min-h-72 snap-start flex-col items-center gap-2 rounded-xl bg-muted/50 py-3.5 text-tertiary" title={l} data-board-col={s} data-board-empty>
          <StatusDot s={s} /><span className="type-label [writing-mode:vertical-rl]">{l}</span><span className="type-label tabular-nums">0</span>
        </div>) : (
        <div key={s} className="flex min-h-72 min-w-0 snap-start flex-col gap-2 rounded-xl bg-muted/50 p-2" data-board-col={s}>
          <div className="flex items-center gap-2 px-2 pb-1 pt-1.5 type-label text-foreground/80"><StatusDot s={s} />{l}<span className="tabular-nums text-tertiary">{g.length}</span></div>
          {g.map((i) => { const relation = workflowRelation(i, st.items); return (
            <button key={i.id} type="button" onClick={() => go("#/items/" + i.id)} className="rounded-lg bg-card p-3 text-left shadow-[var(--lift)] hover:shadow-[var(--lift-hover)] active:scale-[.99]" data-kanban-card={i.id}>
              <ItemNo id={i.id} /><div className="mt-1 type-body font-medium leading-5 text-foreground">{i.t}</div>
              {relation && <div className="mt-1 type-footnote font-medium text-foreground/80" data-workflow-relation={relation.kind}>{workflowRelationText(relation)}</div>}
              {relation?.kind !== "paused" && relation?.kind !== "previous" && i.now && <div className="mt-1 line-clamp-2 type-callout text-muted-foreground">{i.now}</div>}
              <Tags ids={i.tags} max={2} className="mt-2" />
              {/* the plan bar keeps its own corner so a narrow card never wraps it under a dangling separator */}
              <div className="mt-3 flex items-center gap-2 type-footnote text-muted-foreground"><Meta className="min-w-0 flex-1"><BotName k={i.b} />{i.project && <ProjectChip id={i.project} plain />}{i.due && <span>{t("views:items.due", { due: dueLabel(i.due) })}</span>}</Meta>{i.plan && <PlanBar i={i} />}</div>
            </button>
          ) })}
        </div>))}
    </div>
  )
}
export function ItemsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [confirmDel, setConfirmDel] = useState<string | null>(null)
  const arch = filt === "arch"
  let xs = st.items.filter((i) => (arch ? i.arch : !i.arch) && (arch || filt === "all" || (filt === "wait" && ["wait", "prop"].includes(i.st)) || (filt === "run" && itemDisplayStatus(i) === "run") || (filt === "star" && i.star))
    && (botF === "all" || i.b === botF) && (tagF === "all" || !!i.tags?.includes(tagF)) && (kindF === "all" || !!i.tags?.includes(kindF)) && (projF === "all" || (projF === "none" ? !i.project : i.project === projF)) && (!q || i.t.includes(q) || i.id.toLowerCase().includes(q.toLowerCase())))
  if (sortBy === "due") xs = [...xs].sort((a, b) => (a.due ? 0 : 1) - (b.due ? 0 : 1))
  if (sortBy === "pri") xs = [...xs].sort((a, b) => PRI_ORDER[a.pri ?? "中"] - PRI_ORDER[b.pri ?? "中"])
  if (sortBy === "no") xs = [...xs].sort((a, b) => +b.id.split("-")[1] - +a.id.split("-")[1])
  const groups: [Item["st"], string][] = [["wait", t("views:items.groupWait")], ["prop", t("views:items.groupProp")], ["run", t("views:items.groupRun")], ["block", t("views:items.groupBlock")], ["pause", t("views:items.groupPause")], ["done", t("views:items.groupDone")], ["fail", t("views:items.groupFail")], ["drop", t("views:items.groupDrop")]]
  const seg = <Segmented value={itemsView} onChange={(v) => { itemsView = v; selecting = false; selected.clear(); r() }} options={[["list", t("views:items.viewList")], ["board", t("views:items.viewBoard")], ["graph", t("views:items.viewGraph")]] as const} />
  const pick = selecting ? (id: string) => () => { selected.has(id) ? selected.delete(id) : selected.add(id); r() } : () => undefined
  const ids = [...selected]
  const done = () => { selected.clear(); selecting = false; r() }
  /* the project and bot columns only when the visible items differ in them */
  const multiProj = new Set(xs.map((i) => i.project).filter(Boolean)).size > 1, multiBot = new Set(xs.map((i) => i.b).filter(Boolean)).size > 1
  const rowOf = (i: Item) => arch
    ? <div key={i.id} className="flex items-center"><div className="min-w-0 flex-1"><ItemRow i={i} proj={multiProj} bot={multiBot} /></div>
        <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => archive([i.id], false)} data-unarchive={i.id}>{t("views:items.restore")}</Button>
        <Button size="xs" variant="ghost" className="text-destructive" onClick={() => setConfirmDel(i.id)} data-delete-item={i.id}>{t("common:action.delete")}</Button></div>
    : <ItemRow key={i.id} i={i} pick={pick(i.id)} proj={multiProj} bot={multiBot} />
  return (
    <Page title={t("views:items.title")} sub={arch ? t("views:items.subArch") : t("views:items.sub", { run: st.items.filter((i) => itemDisplayStatus(i) === "run" && !i.arch).length, wait: pending().length })} max={1080}
      actions={<>{itemsView === "list" && !arch && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { selecting = !selecting; selected.clear(); r() }} data-select-mode>{selecting ? t("views:items.selectDone") : t("views:items.select")}</Button>}<Button size="sm" onClick={() => openSheet("newItem")} data-new-item><PlusIcon />{t("views:items.newItem")}</Button></>}
      toolbar={<span className="hidden sm:block">{seg}</span>}>
      <AlertDialog open={!!confirmDel} onOpenChange={(o) => !o && setConfirmDel(null)}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("views:items.deleteTitle", { id: confirmDel })}</AlertDialogTitle><AlertDialogDescription>{t("views:items.deleteDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteItem(confirmDel!); setConfirmDel(null) }} data-delete-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <div className="mb-4 sm:hidden">{seg}</div>
      {/* the graph draws every item's relations, so the list filters would only look like they apply */}
      {itemsView !== "graph" && <div className="mb-6 flex items-center gap-1 max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none] sm:flex-wrap [&>*]:shrink-0" data-items-toolbar>
        <Chips value={filtLabel(filt)} onChange={(v) => { filt = FILTS.find((k) => filtLabel(k) === v) ?? "all"; done() }} options={FILTS.map(filtLabel)} />
        <span className="flex-1 max-sm:hidden" />
        <Select quiet className={FILTER(kindF !== "all")} testId="kind-filter" value={tag(kindF) ? kindF : "all"} onChange={(v) => { kindF = v; r() }} options={[{ v: "all", l: t("views:items.allKinds") }, ...kindTags().map((x) => ({ v: x.id, l: <KindTag id={x.id} className="border-0 bg-transparent px-0" /> }))]} />
        <Select quiet className={FILTER(tagF !== "all")} testId="tag-filter" value={tag(tagF) ? tagF : "all"} onChange={(v) => { tagF = v; r() }} options={[{ v: "all", l: t("views:items.allTags") }, ...DOMAIN_CATS.flatMap((c) => st.tags.filter((x) => x.cat === c).map((x) => ({ v: x.id, l: <span>{x.name} <span className="text-muted-foreground">· {c}</span></span> })))]} />
        <Select quiet className={FILTER(projF !== "all")} testId="project-filter" value={st.projects.some((p) => p.id === projF) || projF === "none" ? projF : "all"} onChange={(v) => { projF = v; r() }} options={[{ v: "all", l: t("views:items.allProjects") }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: t("views:items.noProject") }]} />
        <Select quiet className={FILTER(botF !== "all")} testId="bot-filter" value={botF} onChange={(v) => { botF = v; r() }} options={[{ v: "all", l: t("views:items.allBots") }, ...Object.keys(st.bots).map((k) => ({ v: k, l: <BotName k={k} /> }))]} />
        {itemsView === "list" && <Select quiet className={FILTER(sortBy !== "updated")} testId="sort" value={sortBy} onChange={(v) => { sortBy = v as ItemsSort; r() }} options={SORTS.map((v) => ({ v, l: sortLabel(v) }))} />}
        <label className="relative"><SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-tertiary" /><Input className={cn("h-7 rounded-lg pl-8 type-callout transition-[width,background-color,border-color] duration-180 ease-(--ease-out)", q ? "w-40" : "w-8 cursor-pointer border-transparent bg-transparent placeholder:text-transparent hover:bg-accent focus:w-40 focus:cursor-text focus:border-border focus:bg-transparent focus:placeholder:text-muted-foreground")} placeholder={t("views:items.search")} aria-label={t("views:items.search")} data-items-search defaultValue={q} onChange={(e) => { q = e.target.value; r() }} /></label>
      </div>}
      {selecting && (
        <div className="anim-in sticky top-2 z-10 mb-6 flex flex-wrap items-center gap-2 rounded-xl bg-popover/95 px-3 py-2 shadow-md backdrop-blur-xl" data-bulk-bar>
          <span className="type-callout font-medium tabular-nums">{t("views:items.selected", { count: ids.length })}</span>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { xs.forEach((i) => selected.add(i.id)); r() }}>{t("views:items.selectAll")}</Button>
          <span className="flex-1" />
          {([["pause", t("views:items.bulkPause")], ["run", t("views:items.bulkRun")], ["arch", t("views:items.bulkArch")], ["drop", t("views:items.bulkDrop")]] as const).map(([op, l]) => <Button key={op} size="sm" variant={op === "drop" ? "ghost" : "outline"} className={cn(op === "drop" && "text-destructive")} disabled={!ids.length} onClick={() => { bulk(ids, op); done() }} data-bulk={op}>{l}</Button>)}
        </div>
      )}
      <div key={itemsView} className="anim-in">
      {itemsView === "list" && (xs.length ? (arch || sortBy !== "updated"
        ? <Group>{xs.map(rowOf)}</Group>
        : <div className="flex flex-col gap-8">{groups.map(([s, l]) => {
            const g = xs.filter((i) => itemDisplayStatus(i) === s), fold = closedSt(s) && !openGroups.has(s) && g.length > CLOSED_SHOWN + 2
            return g.length ? (
              <section key={s} data-items-group={s}>
                <SectionLabel className="mb-1 text-tertiary" right={fold || openGroups.has(s) ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { openGroups.has(s) ? openGroups.delete(s) : openGroups.add(s); r() }} data-group-more={s}>{fold ? t("views:items.showAll", { count: g.length }) : t("views:items.collapse")}</Button> : undefined}>
                  <span className="inline-flex items-center gap-2"><StatusDot s={s} />{l}</span><Count n={g.length} />
                </SectionLabel>
                <Group>{(fold ? g.slice(0, CLOSED_SHOWN) : g).map(rowOf)}</Group>
              </section>) : null })}</div>)
        : <Empty title={arch ? t("views:items.emptyArch") : st.items.length ? t("views:items.emptyFiltered") : t("views:items.emptyNone")} sub={arch ? (st.cfg.archDays === "0" ? t("views:items.archManual") : t("views:items.archAuto", { count: Number(st.cfg.archDays) })) : st.items.length ? undefined : t("views:items.emptyNoneSub")} />)}
      {itemsView === "board" && <ItemsBoard groups={groups.filter((g) => g[0] !== "drop" && g[0] !== "fail")} xs={xs} />}
      {itemsView === "graph" && (
        <div className="flex flex-col gap-4">
          <GraphLegend />
          <RelationGraph scope="all" hideSimple={!showAll} height={560} onOpen={openNode} />
          <div className="flex flex-wrap items-center gap-2 type-callout text-muted-foreground">{showAll ? t("views:items.graphAll") : t("views:items.graphFiltered")}<Button size="xs" variant="ghost" className="text-foreground/80" onClick={() => { showAll = !showAll; r() }}>{showAll ? t("views:items.graphLinkedOnly") : t("views:items.graphShowAll")}</Button></div>
        </div>
      )}
      </div>
    </Page>
  )
}
function Modal({ open, title, desc, children, footer, wide }: { open: boolean; title: string; desc?: string; children: ReactNode; footer?: ReactNode; wide?: boolean }) {
  return (
    <Dialog open={open} onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className={cn("gap-4 rounded-2xl", wide ? "sm:max-w-[960px]" : "sm:max-w-[480px]")}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}
export function ForkDialog({ k, at }: { k: string; at: number }) {
  const t = useT()
  /* the last thing said up to the fork point (a turn's end or a tool call has no words to show) */
  const s = st.S[k], said = s?.ev.slice(0, at + 1).findLast((x) => ["u", "a", "ask"].includes(x?.[0]) && typeof x[1] === "string" && x[1].trim())?.[1] as string | undefined
  const [mode, setMode] = useState<"item" | "sess">("item"), [g, setG] = useState(""), [busy, setBusy] = useState(false)
  useEffect(() => { if (s && !s.loaded) void loadEvents(k) }, [k])
  if (!s) return null
  return (
    <Modal open title={t("views:fork.title")} desc={t("views:fork.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="fork-go" disabled={busy} onClick={async () => { setBusy(true); const to = await doFork(k, at, g.trim(), mode === "sess"); closeSheet(); go(to) }}>{busy && <Spinner className="size-3.5" />}{t("views:fork.go")}</Button></>}>
      {said && <div className="line-clamp-3 rounded-lg bg-muted px-3.5 py-2.5 type-callout text-muted-foreground" data-fork-quote>{said}</div>}
      <FieldGroup className="gap-4">
        <Field><FieldLabel>{t("views:fork.as")}</FieldLabel>
          <ChoiceCards value={mode} onChange={setMode} cols={s.item ? 2 : 1} options={[{ v: "item", t: t("views:fork.asItem"), d: t("views:fork.asItemDesc") }, ...(s.item ? [{ v: "sess" as const, t: t("views:fork.asSess"), d: t("views:fork.asSessDesc") }] : [])]} /></Field>
        <Field><FieldLabel htmlFor="fork-goal">{t("views:fork.goal")}</FieldLabel>
          <Textarea id="fork-goal" className="min-h-20 rounded-lg" placeholder={t("views:fork.goalPlaceholder")} value={g} onChange={(ev) => setG(ev.target.value)} /></Field>
      </FieldGroup>
    </Modal>
  )
}
export function NewItemDialog({ project: pre }: { project?: string } = {}) {
  const t = useT()
  const [g, setG] = useState(""), [mode, setMode] = useState<"start" | "propose">("start"), [bot, setBot] = useState("auto"), [proj, setProj] = useState(pre ?? "auto"), [busy, setBusy] = useState(false)
  /* one send idempotency key while the form stays the same: a retry after a lost answer makes no second item */
  const sendKey = useRef<{ form: string; id: string } | null>(null)
  const clientId = () => {
    const form = JSON.stringify([g.trim(), bot, proj, mode])
    if (sendKey.current?.form !== form) sendKey.current = { form, id: newClientId() }
    return sendKey.current.id
  }
  return (
    <Modal open title={t("views:newItem.title")} desc={t("views:newItem.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="new-go" disabled={!g.trim() || busy} onClick={async () => {
        setBusy(true); const id = await newItem(g.trim(), bot === "auto" ? undefined : bot, proj === "auto" ? undefined : proj === "none" ? null : proj, mode, clientId())
        setBusy(false); if (id) { closeSheet(); go("#/items/" + id) } }}>{busy && <Spinner className="size-3.5" />}{t("views:newItem.create")}</Button></>}>
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="new-goal">{t("views:newItem.goal")}</FieldLabel><Textarea id="new-goal" autoFocus className="min-h-24 rounded-lg" placeholder={t("views:newItem.goalPlaceholder")} value={g} onChange={(e) => setG(e.target.value)} /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field><FieldLabel>{t("views:newItem.bot")}</FieldLabel><Select testId="new-bot" value={bot} onChange={setBot} options={[{ v: "auto", l: t("views:newItem.auto") }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Field>
          <Field><FieldLabel>{t("views:newItem.project")}</FieldLabel><Select testId="new-project" value={proj} onChange={setProj} options={[{ v: "auto", l: t("views:newItem.auto") }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: t("views:newItem.noProject") }]} /></Field>
        </div>
        <Field><FieldLabel>{t("views:newItem.mode")}</FieldLabel><ChoiceCards value={mode} onChange={setMode} options={[{ v: "start", t: t("views:newItem.start"), d: t("views:newItem.startDesc") }, { v: "propose", t: t("views:newItem.propose"), d: t("views:newItem.proposeDesc") }]} /></Field>
      </FieldGroup>
    </Modal>
  )
}
export function GraphDialog({ k }: { k: string }) {
  const t = useT()
  return <Modal open wide title={t("views:graph.title")}><GraphLegend /><RelationGraph focus={k} scope="comp" height={520} onOpen={(n) => { closeSheet(); openNode(n) }} /></Modal>
}
export function LinkDialog({ k }: { k: string }) {
  const t = useT()
  return (
    <Modal open title={t("views:link.title")} desc={t("views:link.desc")}>
      <Group>{st.items.filter((i) => !closedSt(i.st)).map((i) => (
        <Row key={i.id} title={i.t} onClick={() => { link(k, i.id); closeSheet() }}><Status s={itemDisplayStatus(i)} /></Row>
      ))}</Group>
    </Modal>
  )
}
/* ═════════ 事项详情 ═════════ */
/* tab values stay the original words: they are the data-seg hooks the browser tests click; labels are translated */
let dtab: "动态" | "产出" = "动态"; let lastItem = ""
/* the item page's state: its mark and pill take the state's tone; the "now" card's border too */
const STATE_TONE: Record<ItemState["tone"], [string, string]> = {
  run: ["text-blue", "bg-blue/12 text-blue"], wait: ["text-attention", "bg-attention/12 text-attention"], pause: ["text-muted-foreground", "bg-muted text-muted-foreground"],
  done: ["text-success", "bg-success/12 text-success"], fail: ["text-destructive", "bg-destructive/12 text-destructive"], neutral: ["text-muted-foreground", "bg-muted text-muted-foreground"],
}
const ROOT_STATUS: Record<ItemState["kind"], AgentStatus> = {
  done: "done", dropped: "stopped", failed: "failed", proposed: "proposed", asking: "asking", unavailable: "blocked",
  graphPaused: "paused", paused: "paused", blocked: "blocked", working: "running", queued: "idle", idle: "idle",
}
const STATE_ICON: Record<ItemState["kind"], typeof CheckIcon> = {
  done: CheckIcon, dropped: BanIcon, failed: CircleAlertIcon, proposed: CircleHelpIcon, asking: CircleHelpIcon,
  unavailable: CircleAlertIcon, graphPaused: PauseIcon, paused: PauseIcon, blocked: HourglassIcon, working: LoaderCircleIcon, queued: HourglassIcon, idle: CircleDashedIcon,
}
function StateMark({ state, centered }: { state: ItemState; centered?: boolean }) {
  const I = STATE_ICON[state.kind]
  return <span className={cn(!centered && "mt-0.5", "flex size-7 shrink-0 items-center justify-center rounded-full", STATE_TONE[state.tone][1])} aria-hidden><I className={cn("size-4", state.kind === "working" && "motion-safe:animate-spin")} strokeWidth={2.25} /></span>
}
export function StatePill({ state }: { state: ItemState }) {
  const t = useT()
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap type-footnote", STATE_TONE[state.tone][0])} data-status={state.kind}><span className="size-1.5 rounded-full bg-current" />{t(`views:itemState.${state.kind}`)}</span>
}
/** runs of `quiet` rows (two or more in a row) as arrays, to be folded; everything else as it is */
function foldProcess<T>(rows: T[], quiet: (x: T) => boolean): (T | T[])[] {
  const out: (T | T[])[] = []
  for (let n = 0; n < rows.length;) {
    let m = n
    while (m < rows.length && quiet(rows[m])) m++
    if (m - n >= 2) { out.push(rows.slice(n, m)); n = m } else { out.push(rows[n]); n++ }
  }
  return out
}
/** an activity line's time under its day heading: the clock only, when the moment is known */
const when = (e: any[]) => typeof e[8] === "number" ? clock(e[8]) : e[2]
/** activity in days: each day's heading and its rows; a row without a moment (a session, a reply) stays in the day
 * before it */
function byDay<T extends { e: any[] }>(groups: (T | T[])[]): [string, (T | T[])[]][] {
  const out: [string, (T | T[])[]][] = []
  for (const g of groups) {
    const ts = (Array.isArray(g) ? g[0] : g).e[8]
    const day = typeof ts === "number" ? dayHeading(ts) : out.at(-1)?.[0] ?? ""
    if (out.at(-1)?.[0] !== day) out.push([day, []])
    out.at(-1)![1].push(g)
  }
  return out
}
/* a folded run of process lines: how many and the latest one; opens in place */
function ProcessRun({ count, last, children }: { count: number; last: any[]; children: ReactNode }) {
  const t = useT(), [open, setOpen] = useState(false)
  return <Collapsible open={open} onOpenChange={setOpen} data-process-run={count}>
    <CollapsibleTrigger className="flex w-full min-w-0 items-center gap-2 py-1 text-left type-footnote text-muted-foreground hover:text-foreground">
      <span className="flex size-5 shrink-0 items-center justify-center"><ChevronRightIcon className={cn("size-3.5 transition-transform duration-180 ease-(--ease-out)", open && "rotate-90")} /></span>
      <span className="shrink-0">{t("views:itemState.process", { count })}</span>
      <span className={cn("min-w-0 truncate text-tertiary transition-opacity duration-180", open && "opacity-0")}>· {clip(String(last[1] ?? ""), 120)}</span>
      <span className="ml-auto shrink-0 tabular-nums text-tertiary">{when(last)}</span>
    </CollapsibleTrigger>
    <CollapsibleContent className="ml-2.5 border-l pl-2">{children}</CollapsibleContent>
  </Collapsible>
}
/* a long goal (a pasted brief) folds to its first lines so the page still opens on what is happening */
function GoalText({ text, children, after }: { text: string; children: ReactNode; after?: ReactNode }) {
  const t = useT()
  const inner = useRef<HTMLDivElement>(null)
  /* the text's full height, measured on the text itself so a clamped box still learns when it grows */
  const [open, setOpen] = useState(false), [full, setFull] = useState(0)
  useEffect(() => { setOpen(false) }, [text])
  useEffect(() => {
    const el = inner.current
    if (!el) return
    const measure = () => setFull(el.scrollHeight)
    measure()
    const ro = new ResizeObserver(measure); ro.observe(el)
    return () => ro.disconnect()
  }, [text])
  const long = full > GOAL_FOLD + 1
  return <>
    {/* opening and closing ease the height between the fold and the measured text, like every other collapse */}
    <div className={cn("overflow-hidden transition-[max-height] duration-260 ease-(--ease-out)", !open && long && "[mask-image:linear-gradient(to_bottom,black_70%,transparent)]")}
      style={{ maxHeight: open && full ? full : GOAL_FOLD }} data-goal-text>
      <div ref={inner}><Markdown text={text} className="type-body text-foreground/80" /></div>
    </div>
    <div className="mt-2 flex items-center gap-3 type-footnote text-muted-foreground">
      {(long || open) && <button type="button" className="font-medium text-foreground/80 hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-goal-more>{open ? t("views:item.goalLess") : t("views:item.goalMore")}</button>}
      {children}
    </div>
    {after}
  </>
}
const GOAL_FOLD = 68
function Prop({ label, children, top }: { label: string; children: ReactNode; top?: boolean }) {
  return <><dt className={cn("pt-1.5 type-callout text-muted-foreground", top && "pt-1")}>{label}</dt><dd className="min-w-0 type-callout text-foreground">{children}</dd></>
}
/* add a tag: search existing ones by name, category or description; create a new one when nothing fits */
function TagPicker({ i }: { i: Item }) {
  const t = useT()
  const [q, setQ] = useState("")
  const has = new Set(i.tags ?? [])
  const xs = st.tags.filter((x) => !isKind(x) && !has.has(x.id) && (!q || [x.name, tagLabel(x), x.cat, x.desc, tagDesc(x)].some((v) => v.toLowerCase().includes(q.toLowerCase()))))
  const domain = (i.tags ?? []).filter((x) => !isKind(tag(x)))
  return (
    <DropdownMenu onOpenChange={(o) => !o && setQ("")}>
      <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-tag />}>{domain.length ? t("views:item.addMore") : t("views:item.addNone")}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-80 w-72">
        <div className="p-1"><Input autoFocus className="h-8 rounded-md type-callout" placeholder={t("views:item.tagSearch")} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} data-tag-search /></div>
        {DOMAIN_CATS.map((c) => { const g = xs.filter((x) => x.cat === c); return g.length ? <div key={c}><div className="px-2 pb-1 pt-2 type-label text-tertiary">{tagCatLabel(c)}</div>
          {g.map((x) => <DropdownMenuItem key={x.id} onClick={() => setTags(i.id, [...(i.tags ?? []), x.id])} data-tag-opt={x.name} className="flex-col items-start gap-0"><span className="type-callout">{tagLabel(x)}</span><span className="line-clamp-1 type-footnote text-muted-foreground">{tagDesc(x)}</span></DropdownMenuItem>)}</div> : null })}
        {!xs.length && <div className="px-2 py-2 type-footnote text-muted-foreground">{t("views:item.tagNoMatch")}</div>}
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => openSheet("tag", { item: i.id, name: q })} data-new-tag><PlusIcon />{q ? t("views:item.newTagNamed", { name: q }) : t("views:item.newTag")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
/* the plan: earlier rounds folded above the current one; a replaced round is struck through, its open steps abandoned */
function PlanSteps({ steps, run, old }: { steps: [string, number][]; run?: boolean; old?: PlanRound["end"] }) {
  const t = useT()
  const next = old ? -1 : steps.findIndex((x) => !x[1])
  return (
    <ol className="flex flex-col gap-1.5 pl-0.5">
      {steps.map(([l, d], n) => {
        const gone = old === "replaced" && !d
        return (
          <li key={n} className="flex items-start gap-2.5 type-callout" data-step={d === 1 ? "done" : d === 2 ? "left" : gone ? "abandoned" : "open"}>
            <span className="flex h-[1lh] shrink-0 items-center">
              {d === 1 ? <CheckIcon className={cn("size-4", old ? "text-muted-foreground" : "text-success")} strokeWidth={2} />
                : d === 2 || gone ? <MinusIcon className="size-4 text-tertiary" strokeWidth={2} />
                : run && n === next ? <Spinner className="size-4 text-blue" /> : <span className="mx-[3px] size-2.5 rounded-full border border-tertiary" />}
            </span>
            <span className={cn("min-w-0 break-words", gone ? "text-tertiary line-through decoration-tertiary" : n === next && run ? "text-foreground" : "text-muted-foreground")}>{l}</span>
            {d === 2 && <span className="shrink-0 pt-px type-footnote text-tertiary">{t("views:item.stepSkipped")}</span>}
          </li>
        )
      })}
    </ol>
  )
}
function Plan({ i }: { i: Item }) {
  const t = useT()
  const cur = i.plan ?? [], dn = cur.filter((x) => x[1] === 1).length
  return (
    <div className="mt-3 flex flex-col gap-2" data-plan>
      {i.rounds.map((r) => { const k = r.steps.filter((x) => x[1] === 1).length; return (
        <Collapsible key={r.n} data-plan-round={r.end}>
          <CollapsibleTrigger className="group/round flex items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground">
            <ChevronRightIcon className="size-3.5 transition-transform duration-180 group-data-[panel-open]/round:rotate-90" />
            <span className={cn(r.end === "replaced" && "line-through decoration-tertiary")}>{t("views:item.round", { n: r.n })}</span>
            <span>· {r.end === "done" ? t("views:item.roundDone", { done: k, total: r.steps.length }) : t("views:item.roundReplaced", { done: k, total: r.steps.length })}</span>
            <span className="tabular-nums text-tertiary">· {r.at}</span>
          </CollapsibleTrigger>
          <CollapsibleContent><div className="pl-5 pt-1.5"><PlanSteps steps={r.steps} old={r.end} /></div></CollapsibleContent>
        </Collapsible>) })}
      {i.rounds.length > 0 && cur.length > 0 && <div className="mt-1 type-footnote tabular-nums text-muted-foreground">{t("views:item.roundCurrent", { n: i.rounds.length + 1, done: dn, total: cur.length })}</div>}
      {cur.length > 0 && <PlanSteps steps={cur} run={i.st === "run"} />}
    </div>
  )
}
export function ItemView({ id }: { id: string }) {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const t = useT()
  const [target, setTarget] = useState<"auto" | "steer" | "new">("auto")
  const [editing, setEditing] = useState(false), [gtxt, setG] = useState("")
  const [confirmDrop, setConfirmDrop] = useState(false), [confirmDel, setConfirmDel] = useState(false)
  const [doneBusy, setDoneBusy] = useState(false)
  const [debugOpen, setDebugOpen] = useState(false)
  const itemComposer = useRef<ComposerHandle | null>(null)
  const [titleEdit, setTitleEdit] = useState(false), [preview, setPreview] = useState<OutOpen | null>(null)
  if (lastItem !== id) { lastItem = id; dtab = "动态" }
  useEffect(() => { void loadItem(id, true) }, [id])
  useEffect(() => { setDebugOpen(false) }, [id])
  const i = st.items.find((x) => x.id === id)
  /* replies to comments are read from the sessions' events */
  const latest = i && (active(i)?.id ?? i.ss.at(-1))
  useEffect(() => { if (latest) void loadEvents(latest) }, [latest])
  /* the collaboration area: the tree of everything this item started; a selected agent's detail replaces the
   * properties in the side column (a sheet on narrow pages) */
  /* the one state the page states: what is happening and whose move it is (item-state.ts); the tree's root takes it */
  const rootState = i ? itemState(i, i.workflow, i.ss.some((k) => st.S[k]?.st === "run")) : null
  const at = useAgentTree(id, i?.workflow, rootState ? ROOT_STATUS[rootState.kind] : undefined)
  const [agent, selectAgent] = useSelectedNode(at)
  if (!i) return <Page title={t("views:item.title")} back={["#/items", t("views:items.title")]}><Empty title={t("views:item.notFound")} sub={t("views:item.notFoundSub")} /></Page>
  const designReview = i.waits.find((w) => w.subject === "design_implementation")
  const dependents = st.items.filter((x) => i.dependents.includes(x.id) || (x.deps || []).includes(i.id))
  const a = active(i), B = st.bots[i.b]
  const targetLabel = { auto: t("views:item.targetAuto"), steer: t("views:item.targetSteer"), new: t("views:item.targetNew") }
  const targets = (a && a.st === "run" ? ["auto", "steer", "new"] : ["auto", "new"]) as ("auto" | "steer" | "new")[]
  /* queued supplements show on their comments; the strip above the composer keeps only rows no comment claims */
  const queued = queuedComments(i.tl, i.qs, i.q, "补充信息：")
  const qSess = st.S[i.qs ?? ""] ?? null
  const qNote = a ? queueNote(qSess) : t("views:item.queueNewSession")
  /* activity: everything, in order; runs of process lines fold where they are shown */
  const feed = i.tl.map((e, n) => ({ e, n }))
  const actions = <>
    <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("views:item.star")} data-star onClick={() => star(i.id)} className="text-muted-foreground" />}><StarIcon className={cn(i.star && "fill-current text-foreground")} /></TooltipTrigger><TooltipContent>{i.star ? t("views:item.unstar") : t("views:item.star")}</TooltipContent></Tooltip>
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        <DropdownMenuItem onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>{t("views:item.editGoal")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => setTitleEdit(true)}>{t("views:item.rename")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => openSheet("graph", { k: id })}>{t("views:item.viewGraph")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => setDebugOpen(true)} data-item-debug>{t("views:debug.open")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => { const k = i.ss[i.ss.length - 1]; openSheet("fork", { k, at: Math.max(0, (st.S[k]?.seqs.length ?? 1) - 1) }) }} disabled={!i.ss.length}>{t("views:item.fork")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => { navigator.clipboard?.writeText(location.href).catch(() => {}); toast(t("views:item.linkCopied")) }}><LinkIcon />{t("views:item.copyLink")}</DropdownMenuItem>
        <DropdownMenuSeparator />
        {!closedSt(i.st) && <DropdownMenuItem variant="destructive" onClick={() => setConfirmDrop(true)} data-quick="drop"><BanIcon />{t("views:item.drop")}</DropdownMenuItem>}
        {closedSt(i.st) && <DropdownMenuItem onClick={() => { archive([i.id], !i.arch); if (!i.arch) return; go("#/items") }} data-archive><ArchiveIcon />{i.arch ? t("views:item.restore") : t("views:item.archive")}</DropdownMenuItem>}
        {i.arch && <DropdownMenuItem variant="destructive" onClick={() => setConfirmDel(true)} data-delete><TrashIcon />{t("views:item.deleteForever")}</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  </>
  /* always-visible quick actions beside the title: one outlined (mark done), the pause beside it quiet; dropping the
   * item is in the ⋯ menu behind a confirmation. Labels fold into icons on narrow screens */
  const over = closedSt(i.st)
  const [pl, PI, pa] = over ? [t("views:item.reopen"), RotateCcwIcon, () => setSt(i.id, "run")] as const
    : i.st === "pause" ? [t("views:item.resume"), PlayIcon, () => setSt(i.id, "run")] as const
    : [t("views:item.pause"), PauseIcon, () => setSt(i.id, "pause")] as const
  const quick = (
    <div className="flex items-center gap-1.5" data-quick-actions>
      <Button variant="ghost" size="sm" className="text-muted-foreground max-sm:w-8 max-sm:px-0" aria-label={pl} title={pl} disabled={i.st === "prop"} onClick={pa} data-quick="pause"><PI /><span className="max-sm:sr-only">{pl}</span></Button>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={t("views:item.markDone")} title={t("views:item.markDone")} disabled={over || doneBusy} onClick={() => { setDoneBusy(true); void markDone(i.id).finally(() => setDoneBusy(false)) }} data-quick="done"><CircleCheckIcon /><span className="max-sm:sr-only">{t("views:item.markDone")}</span></Button>
    </div>
  )
  const tree = hasAgentTree(at)
  const state = rootState!
  /* the now card with nothing under its headline (finished, say): one centred line in a smaller card */
  const compact = !i.waits.length && state.move === "none"
  const centered = compact && !i.workflow && !state.detail && state.kind !== "idle" && !i.rel && !i.dependencyWait
  const goal = <>
          {/* 1 — goal reads as the description of the title */}
          <section data-goal className="-mt-3">
            {editing ? (
              <Field><Textarea className="min-h-24 rounded-lg" value={gtxt} onChange={(e) => setG(e.target.value)} data-goal-input autoFocus />
                <div className="flex flex-wrap items-center gap-2"><Button size="sm" onClick={() => { saveGoal(i.id, gtxt.trim()); setEditing(false) }}>{t("views:item.saveAs", { n: i.goals.length + 1 })}</Button><Button size="sm" variant="ghost" onClick={() => setEditing(false)}>{t("common:action.cancel")}</Button><FieldDescription className="type-footnote">{t("views:item.goalHint")}</FieldDescription></div></Field>
            ) : (
              /* the earlier versions open under the whole line, not inside it */
              <Collapsible className="group/goal">
                <GoalText text={i.goals[i.goals.length - 1]} after={i.goals.length > 1 && <CollapsibleContent>
                  <div className="flex flex-col gap-1 pt-2 type-footnote text-muted-foreground" data-goal-history>{i.goals.slice(0, -1).map((g, n) => <div key={n} className="line-clamp-2">{t("views:item.historyRow", { n: n + 1, goal: g })}</div>)}</div></CollapsibleContent>}>
                  <span>{t("views:item.goalVersion", { n: i.goals.length })}</span>
                  {i.goals.length > 1 && <CollapsibleTrigger className="hover:text-foreground">{t("views:item.history")}</CollapsibleTrigger>}
                  <button type="button" className="opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/goal:opacity-100 [@media(hover:none)]:opacity-100" data-edit-goal onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>{t("views:item.edit")}</button>
                </GoalText>
              </Collapsible>
            )}
          </section>
  </>
  const replies: Reply[] = i.tl.flatMap((e) => e[0] === "reply" && e[1] ? [e[1]] : [])
  const entry = ({ e, n }: { e: any[]; n: number }) => {
    if (e[0] === "sess") return <SessionBlock key={e[1]} k={e[1]} defaultOpen={false} replies={replies} />
    if (e[0] === "reply") return <ReplyBlock key={"reply:" + e[1].s + ":" + e[1].from} r={e[1]} />
    const q = e[0] === "me" && queued.byComment.get(n)
    if (q) return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><span className="mt-px flex size-5 shrink-0 items-center justify-center rounded-full bg-blue/10 text-blue" title={t("views:item.me")} aria-label={t("views:item.me")}><UserRoundIcon className="size-3" strokeWidth={2.25} /></span>
        <QueuedComment sid={i.qs!} q={q} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} at={e[2]} body={(t) => <div className="type-body"><Markdown text={t} /></div>} /></div>
    )
    /* notes sent from an output show what they mark and open the output there */
    const notes = e[0] === "me" && e[7] != null ? i.anns.filter((x) => x.timeline_id === e[7]) : []
    if (e[0] === "me") return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><span className="mt-px flex size-5 shrink-0 items-center justify-center rounded-full bg-blue/10 text-blue" title={t("views:item.me")} aria-label={t("views:item.me")}><UserRoundIcon className="size-3" strokeWidth={2.25} /></span>
        <div className="min-w-0 flex-1">{notes.length ? <div data-comment-notes><div className="type-body">{t("views:item.notes", { count: notes.length })}</div><AnnRefs item={i.id} anns={notes} outs={i.outs} open={(o, ann) => setPreview({ o, focus: ann })} /></div>
          : (e[1] !== "（附件）" || !e[5]?.length) && <div className="type-body"><Markdown text={e[1]} /></div>}<Files files={e[5]} className="my-1" /><div className="type-footnote tabular-nums text-tertiary">{when(e)}{e[3] && st.S[e[3]] ? ` · ${t("views:item.deliveredTo", { n: st.S[e[3]].n })}` : ""}{e[6] === "thread" && <a href="#/" className="ml-1.5 rounded-md bg-muted px-1.5 hover:text-foreground" data-from-thread>{t("views:item.fromThread")}</a>}</div></div></div>
    )
    const yolo = typeof e[1] === "string" && e[1].startsWith("YOLO")
    return <div key={n} className="flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-yolo-line={yolo ? "" : undefined}><span className="flex size-5 shrink-0 items-center justify-center"><span className="size-1 rounded-full bg-tertiary" /></span>{yolo && <YoloMark />}<span className="min-w-0 truncate" title={String(e[1] ?? "")}>{clip(yolo ? e[1].replace(/^YOLO[：:]\s*/, "") : String(e[1] ?? ""), 240)}</span><span className="shrink-0 tabular-nums text-tertiary">{when(e)}</span></div>
  }
  return (
    <Page small title={titleEdit ? <Input autoFocus defaultValue={i.t} className="h-9 rounded-lg type-title" data-title-input
        onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== i.t) patchItem(i.id, { t: v }); setTitleEdit(false) }} onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); if (e.key === "Escape") setTitleEdit(false) }} />
      : <span className="cursor-text" onDoubleClick={() => setTitleEdit(true)} data-title>{i.t}</span>} back={["#/items", t("views:items.title")]} actions={actions} toolbar={quick} toolbarTop max={1080}
      footer={<div className="w-full shrink-0 border-t bg-background/85 backdrop-blur-xl"><div className="mx-auto w-full px-4 pb-[76px] pt-3 sm:px-8 md:pb-4" style={{ maxWidth: 1040 + 64 }}><div className="lg:pr-[340px]">
        {i.qs && <QueueStrip sid={i.qs} rows={queued.rest} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} />}
        <div className="mb-2 type-footnote font-medium text-foreground/80" data-item-feedback-label>{t("views:item.messageBot")}</div>
        <Composer draftKey={draftKey(st.workspace?.id, "item", i.id)} handle={itemComposer} placeholder={t("views:item.composer", { id: i.id })} testId="item" onSend={(text, files) => supp(i.id, text, target, files)}
          left={<Segmented size="sm" value={targets.includes(target) ? target : "auto"} onChange={setTarget} options={targets.map((x) => [x, targetLabel[x]] as const)} />} />
      </div></div></div>}
      sub={<Meta><button type="button" className="inline-flex items-center gap-1 font-mono tabular-nums hover:text-foreground" data-copy-id onClick={() => { navigator.clipboard?.writeText(i.id).catch(() => {}); toast(t("views:item.idCopied", { id: i.id })) }}>{i.id}<CopyIcon className="size-3" /></button><StatePill state={state} /><ProjectChip id={i.project} /><span>{t("views:item.updated", { time: i.up })}</span>{st.yolo && !over && <YoloItemSwitch i={i} />}</Meta>}>
      {debugOpen && <ItemDebugDialog item={i.id} onClose={() => setDebugOpen(false)} />}
      <AlertDialog open={confirmDrop} onOpenChange={setConfirmDrop}>
        <AlertDialogContent size="sm" data-drop-popover>
          <AlertDialogHeader><AlertDialogTitle>{t("views:item.dropTitle", { id: i.id })}</AlertDialogTitle><AlertDialogDescription>{t("views:item.dropDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { setSt(i.id, "drop"); setConfirmDrop(false) }} data-drop-confirm>{t("views:item.drop")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <AlertDialog open={confirmDel} onOpenChange={setConfirmDel}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("views:item.deleteTitle", { id: i.id })}</AlertDialogTitle><AlertDialogDescription>{t("views:item.deleteDesc", { count: i.ss.length })}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { go("#/items"); deleteItem(i.id) }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <OutDialog item={i.id} outs={i.outs} anns={i.anns} view={preview} setView={setPreview} reviewLabel={t(designReview ? "views:item.reviewDesign" : "views:item.messageBot")}
        onReview={() => { setPreview(null); window.setTimeout(() => { if (designReview) document.querySelector<HTMLTextAreaElement>("[data-design-feedback]")?.focus(); else itemComposer.current?.focus() }, 250) }} />
      {i.arch && <div className="mb-8 flex items-center gap-3 rounded-xl bg-muted/60 px-4 py-2.5 type-callout" data-arch-banner><ArchiveIcon className="size-4 text-muted-foreground" /><span className="flex-1">{t("views:item.archived")}</span><Button size="sm" variant="ghost" onClick={() => archive([i.id], false)}>{t("views:item.restore")}</Button></div>}
      <AgentDetailPanel at={at} node={agent} onClose={() => selectAgent(null)} />
      <div className="grid gap-10 lg:grid-cols-[minmax(0,1fr)_300px]">
        <div className="flex min-w-0 flex-col gap-8">
          {goal}
          {/* now: the one state, what keeps it there, and what you can do about it — the only boxed block */}
          <section className={cn("anim-in", i.waits.length ? "" : compact ? "rounded-xl bg-card p-4 shadow-[var(--lift)]" : "rounded-xl bg-card p-5 shadow-[var(--lift)]")} data-now data-state={state.kind}>
            <div className={cn("flex flex-wrap gap-x-3 gap-y-3", centered ? "items-center" : "items-start")}>
              <StateMark state={state} centered={centered} />
              <div className="min-w-[min(100%,14rem)] flex-1 basis-0">
                <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                  <h2 className={cn("type-headline", state.kind === "working" && "shimmer shimmer-once")} data-state-title>{t(`views:itemState.${state.kind}`)}</h2>
                  {state.move !== "none" && <span className="type-footnote text-muted-foreground">{t(`views:itemState.${state.move}`)}</span>}
                </div>
                {state.detail && !i.workflow && <p className="mt-1 line-clamp-3 break-words type-callout text-muted-foreground">{clip(state.detail, 360)}</p>}
                {state.kind === "working" && i.workflow && state.detail && <p className="mt-1 line-clamp-2 break-words type-callout text-muted-foreground">{clip(state.detail, 240)}</p>}
                {state.kind === "idle" && !i.workflow && <p className="mt-1 type-callout text-muted-foreground">{t("views:itemState.idleHint")}</p>}
                {i.workflow && <div className="mt-1.5 flex flex-col gap-2"><Notices at={at} inline /></div>}
              </div>
              {i.workflow && <div className="max-sm:basis-full max-sm:pl-10"><WorkflowButtons at={at} /></div>}
            </div>
            {at.dialogs}
            {i.waits.length > 0 && <div className="mt-5 flex flex-col gap-3">{i.waits.map((w) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}</div>}
            {i.rel && <RelNote rel={i.rel} box={i.rel.tone !== "ok"} className="mt-3" onClose={i.rel.tone === "fail" ? () => closeRel(i) : undefined} />}
            {i.dependencyWait && !!i.deps?.length && <div className="mt-4"><DependencyWaitPanel key={`${i.id}:${i.dependencyWait.cycle}`} item={i.id} wait={i.dependencyWait} /></div>}
          </section>
          {/* who works on it: one row per step on one time axis */}
          {tree && <TeamTimeline at={at} selected={agent?.key ?? null} onSelect={selectAgent} />}
          {/* the conversation: what people said and what came back, under day headings; runs of process lines fold to one line */}
          <section className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Segmented size="sm" value={dtab} onChange={(v) => { dtab = v; r() }} options={[["动态", t("views:item.tabActivity")], ["产出", t("views:item.tabOutputs")]] as const} />
            </div>
            {dtab === "动态" && (
              <div key="activity" className="anim-in flex flex-col" data-worklog>
                {byDay(foldProcess(feed, (x) => x.e[0] === "sys" && !queued.byComment.has(x.n))).map(([day, gs]) => <Fragment key={day}>
                  {day && <h3 className="mb-1 mt-5 type-label text-tertiary first:mt-0" data-day>{day}</h3>}
                  {gs.map((g) => Array.isArray(g)
                    ? <ProcessRun key={"run:" + g[0].n} count={g.length} last={g[g.length - 1].e}>{g.map(entry)}</ProcessRun>
                    : entry(g))}
                </Fragment>)}
              </div>
            )}
            {dtab === "产出" && <div key="outputs" className="anim-in"><OutList outs={i.outs} anns={i.anns} open={(o) => setPreview({ o })} /></div>}
          </section>
        </div>
        <aside className="flex min-w-0 flex-col gap-8" data-item-side>
          {(i.plan || i.rounds.length > 0 || i.workflow) && <section data-progress>
            <h3 className="mb-2 type-label text-tertiary">{t("views:itemState.progress")}</h3>
            <div className="flex flex-wrap items-center gap-x-1 gap-y-1 type-callout" data-stage={stageOf(i)}>
              {STAGES.slice(2).map((g, n, arr) => { const cur = STAGES.indexOf(stageOf(i) as any), me = STAGES.indexOf(g); return (
                <span key={g} className="inline-flex items-center gap-1"><span className={cn(me === cur ? "font-medium text-foreground" : me < cur ? "text-muted-foreground" : "text-tertiary")}>{t(STAGE_LABEL[g])}</span>{n < arr.length - 1 && <ChevronRightIcon className="size-3 text-tertiary" />}</span>) })}
            </div>
            {(i.plan || i.rounds.length > 0) && <Plan i={i} />}
            {i.workflow && <History workflow={i.workflow} />}
          </section>}
          <section data-details>
            <h3 className="mb-2 type-label text-tertiary">{t("views:itemState.details")}</h3>
            <dl className="grid grid-cols-[minmax(64px,max-content)_minmax(0,1fr)] items-start gap-x-3 gap-y-2.5 [&_[data-select]]:-ml-2">
            <Prop label={t("views:item.propBot")}><Select quiet testId="bot" value={i.b} onChange={(k) => reassign(i.id, k)} options={Object.keys(st.bots).filter((k) => st.bots[k].on || k === i.b).map((k) => ({ v: k, l: <BotName k={k} /> }))} />{B && <div className="type-footnote text-muted-foreground">{B.h} · {B.m}</div>}</Prop>
            <Prop label={t("views:item.propSessions")}><div className="flex flex-col gap-2 pt-1">{i.ss.filter((k) => st.S[k]).length ? i.ss.filter((k) => st.S[k]).map((k) => <div key={k} className="flex min-w-0 flex-col gap-0.5" data-item-session={k}>
              <a href={"#/sessions/" + k} className="flex items-center gap-2 hover:underline"><span>#{st.S[k].n}</span><Status s={st.S[k].st} sess /><span className="min-w-0 truncate type-footnote text-muted-foreground">{st.S[k].h}</span><span className="ml-auto type-footnote tabular-nums text-tertiary">{st.S[k].ctx ? st.S[k].ctx + "%" : ""}</span></a>
              <NativeIdLine k={k} label="ID" /></div>) : <span className="text-muted-foreground">{t("common:state.empty")}</span>}</div></Prop>
            <ItemStatsRows id={i.id} render={(u, time, href) => <><Prop label={t("views:item.propUsage")}><a href={href} className="block pt-1 hover:underline" data-item-usage>{u}</a></Prop><Prop label={t("views:item.propTime")}><a href={href} className="block pt-1 hover:underline" data-item-time>{time}</a></Prop></>} />
            <Prop label={t("views:item.propProject")}><Select quiet testId="project" value={i.project ?? "none"} onChange={(v) => patchItem(i.id, { project: v === "none" ? null : v })} options={[{ v: "none", l: t("common:state.none") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} />
              {i.projNext && <div className="type-footnote text-muted-foreground">{t("views:item.projNext")}</div>}</Prop>
            </dl>
            <Collapsible className="mt-3">
              <CollapsibleTrigger className="group/more flex items-center gap-1 type-callout text-muted-foreground hover:text-foreground" data-more-props><ChevronRightIcon className="size-3.5 transition-transform duration-180 group-data-[panel-open]/more:rotate-90" />{t("views:itemState.moreProps")}</CollapsibleTrigger>
              <CollapsibleContent>
            <dl className="grid grid-cols-[minmax(64px,max-content)_minmax(0,1fr)] items-start gap-x-3 gap-y-2.5 pt-3 [&_[data-select]]:-ml-2">
            <ItemPeopleProp id={i.id} Prop={Prop} />
            <Prop label={t("views:item.propKind")}><Select quiet testId="kind" value={kindOf(i) ?? ""} onChange={(k) => setKind(i.id, k)} options={kindTags().map((x) => ({ v: x.id, l: <KindTag id={x.id} className="border-0 bg-transparent px-0" /> }))} />{kindOf(i) && tag(kindOf(i)!) && <div className="type-footnote text-muted-foreground">{tagDesc(tag(kindOf(i)!)!)}</div>}</Prop>
            <Prop label={t("views:item.propTags")}><div className="flex flex-col gap-1.5 pt-1" data-item-tags>
              <Tags ids={(i.tags ?? []).filter((x) => !isKind(tag(x)))} onRemove={(id) => setTags(i.id, (i.tags ?? []).filter((x) => x !== id))} />
              <TagPicker i={i} /></div></Prop>
            <Prop label={t("views:item.propPri")}><Select quiet testId="pri" value={i.pri ?? "中"} onChange={(v) => patchItem(i.id, { pri: v as Item["pri"] })} options={["高", "中", "低"].map((v) => ({ v, l: labelOf(PRI_LABEL, v) }))} /></Prop>
            <Prop label={t("views:item.propDue")}><Select quiet testId="due" value={i.due ?? "无"} onChange={(v) => patchItem(i.id, { due: v === "无" ? undefined : v })} options={[...new Set(["无", i.due ?? "无", "今天 18:00", "明天", "周五", "下周一"])].map((v) => ({ v, l: dueLabel(v) }))} /></Prop>
            <Prop label={t("views:item.propAcc")}><Select quiet testId="acc" value={accKey(i.acc)} onChange={(v) => setAcc(i.id, v)} options={["inherit", "manual", "auto"].map((k) => ({ v: k, l: ACC[k] }))} />{accKey(i.acc) === "inherit" && <div className="type-footnote text-muted-foreground">{t("views:item.accEffective", { mode: accOf(i) })}</div>}</Prop>
            <Prop label={t("views:item.propDeps")}>
              <div className="flex flex-col gap-1 pt-1" data-deps>
                {(i.deps || []).filter((d) => st.items.some((x) => x.id === d)).map((d) => { const x = it(d); return (
                  <div key={d} className="group/dep flex items-center gap-2"><StatusDot s={x.st} /><a href={"#/items/" + d} className="flex min-w-0 flex-1 items-baseline gap-1.5 hover:underline"><ItemNo id={d} /><span className="truncate">{x.t}</span></a>
                    <button type="button" aria-label={t("views:item.removeDep", { id: d })} onClick={() => rmDep(i.id, d)} className="text-tertiary opacity-0 transition-opacity hover:text-foreground group-hover/dep:opacity-100 [@media(hover:none)]:opacity-100"><XIcon className="size-3.5" /></button></div>) })}
                <DropdownMenu>
                  <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-dep />}>{(i.deps || []).length ? t("views:item.addMore") : t("views:item.addNone")}</DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-72 min-w-64">
                    {st.items.filter((x) => x.id !== i.id && !(i.deps || []).includes(x.id) && x.st !== "drop").map((x) => <DropdownMenuItem key={x.id} onClick={() => addDep(i.id, x.id)} data-dep-opt={x.id}><ItemNo id={x.id} /><span className="truncate">{x.t}</span></DropdownMenuItem>)}
                  </DropdownMenuContent>
                </DropdownMenu>
                {i.st === "block" && <span className="type-footnote text-muted-foreground">{t("views:item.depsLeft", { count: depsOpen(i).length })}</span>}
              </div>
            </Prop>
            {dependents.length > 0 && <Prop label={t("views:item.propDependents")}><div className="flex flex-col gap-1 pt-1">{dependents.map((x) => <a key={x.id} href={"#/items/" + x.id} className="flex items-center gap-2 hover:underline"><StatusDot s={x.st} /><span className="flex min-w-0 items-baseline gap-1.5"><ItemNo id={x.id} /><span className="truncate">{x.t}</span></span></a>)}</div></Prop>}
            {i.cwd && <Prop label={t("views:item.propCwd")}><span className="block break-all pt-1 type-code text-muted-foreground" data-cwd>{i.cwd.replace(/^\/Users\/[^/]+/, "~")}</span></Prop>}
            {i.branch && <Prop label={t("views:item.propBranch")}><span className="block break-all pt-1 type-code" data-branch>{i.branch}</span></Prop>}
            <Prop label={t("views:item.propSource")}><span className="block pt-1">{i.parent?.item ? <a href={"#/items/" + i.parent.item} className="hover:underline">{t("views:item.forkedFrom", { id: i.parent.item })}</a> : i.src[1] === "chat" ? <a href="#/" className="hover:underline">{t("views:item.fromChat")}</a> : i.src[0]}</span>
              {itemThread(i.id) && <label className="mt-1.5 flex items-center gap-2 pl-0.5 type-footnote text-muted-foreground" data-thread-follow><Switch size="sm" checked={!itemThread(i.id)!.follow} onCheckedChange={(v) => void threadFollow(i.id, !v)} />{t("views:item.noFollow")}</label>}</Prop>

            </dl>
              </CollapsibleContent>
            </Collapsible>
          </section>
        </aside>
      </div>
    </Page>
  )
}

export function SessionView({ k }: { k: string }) {
  const t = useT()
  const [mode, setMode] = useState<"插话" | "排队">("插话"), [renaming, setRenaming] = useState(false)
  const [infoOpen, setInfoOpen] = useState(false)
  const [debugOpen, setDebugOpen] = useState(false)
  const composer = useRef<ComposerHandle | null>(null)
  const s = st.S[k]
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => { void loadEvents(k) }, [k])
  useEffect(() => { setDebugOpen(false) }, [k])
  useEffect(() => { scroller.current?.scrollTo({ top: 1e9 }) }, [s?.ev.length, k])
  if (!s) return <Page title={t("views:session.title")} back={["#/sessions", t("views:sessions.title")]}><Empty title={t("views:session.notFound")} /></Page>
  const i = s.item ? it(s.item) : null, run = s.st === "run"
  const exportMd = () => {
    const md = `# ${s.title}\n\n` + s.ev.map((e) => e[0] === "u" ? t("views:session.exportYou") + e[1] : e[0] === "hi" ? "> " + e[1] : e[0] === "a" ? e[1] : e[0] === "tool" ? "- " + e[2] + " " + e[3] : e[0] === "sys" ? "_" + e[1] + "_" : "").filter(Boolean).join("\n\n")
    const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([md], { type: "text/markdown" })); a.download = `${s.title.slice(0, 40)}.md`; a.click(); toast(t("views:session.exported"))
  }
  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-session-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        {!s.ro && <DropdownMenuItem onClick={() => setRenaming(true)} data-rename><PencilIcon />{t("views:session.rename")}</DropdownMenuItem>}
        <DropdownMenuItem onClick={exportMd} data-export><DownloadIcon />{t("views:session.export")}</DropdownMenuItem>
        {s.item && <DropdownMenuItem onClick={() => setDebugOpen(true)} data-session-debug>{t("views:debug.open")}</DropdownMenuItem>}
        {!s.ro && s.st === "idle" && <DropdownMenuItem onClick={() => sessPatch(k, { st: "ended" }, t("views:session.ended"))} data-end><SquareIcon />{t("views:session.end")}</DropdownMenuItem>}
        {s.ro && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => { go("#/sessions"); sessRemove(k) }} data-remove-import><TrashIcon />{t("views:session.remove")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
  const ctx = (
    <HoverCard>
      <HoverCardTrigger render={<button type="button" className="rounded-full px-2 py-1 type-footnote tabular-nums text-muted-foreground hover:bg-accent" />}>{s.contextKnown ? t("views:session.ctxPct", { pct: s.ctx }) : t("views:session.ctxUnknown")}</HoverCardTrigger>
      <HoverCardContent className="w-60"><div className="type-label text-tertiary">{t("views:session.ctx")}</div>{s.contextKnown && <Progress value={Math.min(100, s.ctx)} className="my-2.5" />}<div className="type-callout text-muted-foreground">{s.contextKnown ? t("views:session.ctxUsed", { pct: s.ctx }) : t("views:session.ctxNone")}</div></HoverCardContent>
    </HoverCard>
  )
  const panel = (
    <div className="flex flex-col gap-8">
      {i ? (
        <div><SectionLabel>{t("views:session.item")}</SectionLabel>
          <button type="button" onClick={() => go("#/items/" + i.id)} className="-mx-2 w-[calc(100%+16px)] rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-accent"><div className="type-callout font-medium"><span className="mr-1.5 font-mono type-footnote font-normal text-muted-foreground">{i.id}</span>{i.t}</div><Status s={itemDisplayStatus(i)} className="mt-1" /></button>
          <div className="mt-1 px-1 type-callout text-muted-foreground"><ProjectChip id={i.project} /></div></div>
      ) : (
        <div><SectionLabel>{t("views:session.unlinked")}</SectionLabel><div className="flex flex-wrap gap-2">{!s.ro && <Button size="sm" variant="outline" onClick={() => openSheet("link", { k })} data-link>{t("views:session.link")}</Button>}<Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</Button></div></div>
      )}
      <div><SectionLabel right={<Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => openSheet("graph", { k })}>{t("views:session.zoom")}</Button>}>{t("views:session.near")}</SectionLabel>
        <RelationGraph key={k} focus={k} scope="near" height={260} minimal onOpen={openNode} /></div>
      <SessionKnowledge id={k} />
      <div data-session-resume><SectionLabel>{t("views:session.resume")}</SectionLabel><ResumeCard k={k} /></div>
      <div><SectionLabel>{t("views:session.info")}</SectionLabel><div className="flex flex-col gap-1.5 px-1 type-callout text-muted-foreground"><BotName k={s.b} className="font-medium text-foreground" /><span>{hm(s)}</span><span>{t("views:session.effort", { v: s.reasoningEffort ?? t("views:session.effortInherit") })}</span><a href={"#/devices/" + s.device} className="hover:text-foreground">{t("views:session.runsOn", { device: st.devices.find((d) => d.id === s.device)?.name ?? "Hub" })}</a><span>{t("views:session.source", { src: s.src })}</span><span>{t("views:session.durCtx", { dur: s.dur, ctx: s.contextKnown ? `${s.ctx}%` : t("views:session.unreported") })}</span></div></div>
    </div>
  )
  return (
    <div className="flex h-full min-h-0">
      <div className="min-w-0 flex-1">
        <Page noTitle title={t("views:session.numbered", { n: s.n })} back={i ? ["#/items/" + i.id, t("views:items.title")] : ["#/sessions", t("views:sessions.title")]} max={720} scrollRef={scroller}
          bar={<div className="flex min-w-0 items-center gap-2"><span className="truncate type-headline">{s.n ? t("views:session.numberedBar", { n: s.n }) : ""}{renaming ? <Input autoFocus defaultValue={s.title} className="inline-flex h-7 w-56 rounded-md" data-rename-input onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== s.title) sessPatch(k, { title: v }, t("views:session.renamed")); setRenaming(false) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : s.title}</span><Status s={s.st} sess pill className="hidden sm:inline-flex" />{i && <ProjectChip id={i.project} className="hidden shrink-0 type-footnote text-muted-foreground sm:inline-flex" />}</div>}
          actions={<>{!s.ro && <SessionStats id={k} />}{/* the runtime line under the conversation says it too: below xl the title needs the room */}<span className="hidden xl:inline-flex">{ctx}</span><Button size="icon-sm" variant="ghost" className="text-muted-foreground sm:hidden" aria-label={t("views:session.info")} onClick={() => setInfoOpen(true)} data-session-info><PanelRightIcon /></Button><Button size="sm" variant="ghost" className="hidden text-muted-foreground sm:inline-flex xl:hidden" onClick={() => setInfoOpen(true)}>{t("views:session.info")}</Button>{s.ro && <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><LockIcon className="size-3" />{t("views:session.readOnly")}</span>}{menu}</>}
          footer={
            /* on the page's own frame and reading width: the composer and the transcript share one left edge */
            <div className="mx-auto w-full max-w-[1104px] shrink-0 px-4 pb-[76px] sm:px-8 lg:pb-5"><div className="max-w-[760px]">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 py-2 type-footnote text-tertiary" data-session-runtime>
                <span>Harness <span className="text-muted-foreground">{s.h ?? t("views:session.unknown")}</span></span>
                <NativeIdLine k={k} label={t("views:session.nativeId")} className="w-auto" />
                <span className="break-all">{t("views:session.model")} <span className="text-muted-foreground">{s.m ?? t("views:session.defaultModel")}</span></span>
                <span>{t("views:session.effortLabel")} <span className="text-muted-foreground">{s.reasoningEffort ?? t("views:session.effortInherit")}</span></span>
                {/* the header says it from xl up */}
                <span className={cn("tabular-nums xl:hidden", s.ctx >= 80 ? "text-attention" : "text-muted-foreground")}>{s.contextKnown ? t("views:session.ctxPct", { pct: s.ctx }) : t("views:session.ctxUnknown")}</span>
              </div>
              <RunBar k={k} onStop={s.ro ? undefined : () => interrupt(k)} />
              {!s.ro && <QueueStrip sid={k} rows={s.queue} note={queueNote(s)} starting={s.starting} composer={composer} />}
              {s.ro ? (
                <div className="flex flex-wrap items-center gap-3 rounded-xl bg-muted/60 px-4 py-3" data-readonly>
                  <LockIcon className="size-4 text-muted-foreground" /><span className="min-w-0 flex-1 type-callout text-muted-foreground">{t("views:session.readOnlyNote", { src: s.src.startsWith("本机 ") ? t("views:session.srcLocal", { h: s.src.slice(3) }) : s.src })}</span>
                  <Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</Button>
                </div>
              ) : <Composer key={k} draftKey={draftKey(st.workspace?.id, "session", k)} handle={composer} testId="session" model={hm(s)} placeholder={s.st === "ended" ? t("views:session.endedPlaceholder") : t("views:session.placeholder")} onSend={(text, files) => sessSend(k, text, mode, files)}
                left={run ? <Segmented size="sm" value={mode} onChange={setMode} options={[["插话", t("views:session.modeSteer")], ["排队", t("views:session.modeQueue")]] as const} /> : undefined} />}
            </div></div>
          }>
          <div className="pt-6"><Transcript k={k} /><SessionCards k={k} /></div>
        </Page>
      </div>
      {/* the same panel as the chat rail: one width, a head on the top bar's line, the content scrolling under it */}
      <aside className={cn("hidden shrink-0 flex-col border-l border-sidebar-border bg-sidebar xl:flex", PANEL_W)} data-session-panel>
        <div className="flex h-12 shrink-0 items-center border-b border-sidebar-border px-5 type-headline" data-session-panel-head>{t("views:session.info")}</div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 pb-6 pt-5">{panel}</div>
      </aside>
      {debugOpen && s.item && <ItemDebugDialog item={s.item} session={k} onClose={() => setDebugOpen(false)} />}
      <Dialog open={infoOpen} onOpenChange={setInfoOpen}><DialogContent className="max-h-[85dvh] overflow-y-auto"><DialogHeader><DialogTitle>{t("views:session.info")}</DialogTitle><DialogDescription>{t("views:session.infoDesc")}</DialogDescription></DialogHeader><div>{panel}</div></DialogContent></Dialog>
    </div>
  )
}

/* ═════════ 设置 / Bot ═════════ */
export const PLUGIN_ICON: Record<string, any> = { mail: MailIcon, calendar: CalendarIcon, message: MessageSquareIcon, git: GitPullRequestIcon, shield: LockIcon, list: ListIcon, zap: ZapIcon }
export const Ico = ({ I }: { I: any }) => <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"><I className="size-4" strokeWidth={1.5} /></span>
/** 我: the user's own contact (称呼、时区、语言); only where the Hub has the contacts module */
function MeRow() {
  const t = useT()
  const on = usePeopleOn()
  return on ? <Panel><Row icon={<Tile I={UserRoundIcon} />} title={t("views:more.me")} sub={t("views:more.meSub")} onClick={() => go("#/contacts/me")} data-more-me><Chevron /></Row></Panel> : null
}
export function MoreView() {
  const t = useT()
  const groups = [
    { name: t("views:more.groupWork"), rows: [["projects", t("views:more.projects"), t("views:more.projectsSub", { name: st.workspace?.name }), FolderIcon], ["sessions", t("views:more.sessions"), t("views:more.sessionsSub"), SquareTerminalIcon], ["stats", t("views:more.stats"), t("views:more.statsSub"), ChartNoAxesColumnIcon]] },
    { name: t("views:more.groupLinks"), rows: [["integrations", t("views:more.integrations"), t("views:more.integrationsSub"), PlugIcon], ["channels", t("views:more.channels"), t("views:more.channelsSub"), MessagesSquareIcon], ["mail", t("views:more.mail"), t("views:more.mailSub"), MailIcon], ["contacts", t("views:more.contacts"), t("views:more.contactsSub"), ContactRoundIcon], ["knowledge", t("views:more.knowledge"), t("views:more.knowledgeSub"), BookOpenIcon]] },
    { name: t("views:more.groupAuto"), rows: [["bots", t("views:more.bots"), t("views:more.botsSub"), BotIcon], ["auto", t("views:more.auto"), t("views:more.autoSub"), WorkflowIcon], ["signals", t("views:more.signals"), t("views:more.signalsSub"), RadioIcon]] },
    { name: t("views:more.groupSystem"), rows: [["devices", t("views:more.devices"), t("views:more.devicesSub"), ServerIcon], ["accounts", t("views:more.accounts"), t("views:more.accountsSub"), KeyRoundIcon], ["vault", t("views:more.vault"), t("views:more.vaultSub"), LockIcon], ["settings", t("views:more.settings"), t("views:more.settingsSub"), SettingsIcon]] },
  ] as const
  return (
    <Page title={t("views:more.title")} max={640}>
      <div className="flex flex-col gap-8">
        <MeRow />
        <Panel><Row icon={<BotAvatar k="steve" size={44} />} className="py-3" title={<span className="type-headline">{st.bots.steve?.n ?? "Steve"}</span>} sub={t("views:more.steveSub")} onClick={() => go("#/bots/steve")}><Chevron /></Row></Panel>
        {groups.map((group) => <Section key={group.name} label={group.name}><Panel>{group.rows.map(([h, title, s, I]) => (
          <Row key={h} icon={<Tile I={I} />} title={title} sub={s} onClick={() => go("#/" + h)}><Chevron /></Row>
        ))}</Panel></Section>)}
        <div className="[&>[data-more-hub]>[data-slot=item-group]]:mx-0 [&>[data-more-hub]>[data-slot=item-group]]:w-auto [&>[data-more-hub]>[data-slot=item-group]]:rounded-xl [&>[data-more-hub]>[data-slot=item-group]]:bg-card [&>[data-more-hub]>[data-slot=item-group]]:p-1 [&>[data-more-hub]>[data-slot=item-group]]:shadow-[var(--lift)] [&>[data-more-hub]>div:first-child]:px-1 [&>[data-more-hub]>div:first-child]:text-tertiary [&>[data-more-hub]_[data-slot=item]]:px-3 [&>[data-more-hub]_[data-slot=item-separator]]:mx-3 [&>[data-more-hub]_[data-slot=item-media]>span]:size-8 [&>[data-more-hub]_[data-slot=item-media]>span]:rounded-lg"><HubMore /></div>
        <Panel><GitHubMoreRow /></Panel>
        {st.yolo && <Panel><Row icon={<Tile I={ZapIcon} />} title={t("views:more.yolo")} sub={{ on: t("views:more.yoloOn"), full: t("views:more.yoloFull"), off: t("views:more.yoloOff"), paused: t("views:more.yoloPaused", { reason: st.yolo.paused?.reason ?? "" }), forced: t("views:more.yoloForced") }[yoloState(st.yolo)!]} onClick={() => openSheet("yolo")} data-more-yolo><Chevron /></Row>
          <Row title={t("views:more.yoloLog")} sub={t("views:more.yoloLogSub")} onClick={() => go("#/yolo")}><Chevron /></Row></Panel>}
        <Panel><Row title={t("views:more.appearance")}><ThemeSwitch labels /></Row></Panel>
      </div>
    </Page>
  )
}

/* ═════════ 视觉规范: the living specimen lives in design-view.tsx ═════════ */
export { DesignView } from "@/design-view"

export function ImportDialog() {
  const t = useT()
  const [pick, setPick] = useState<string[]>([]), [busy, setBusy] = useState(false)
  useEffect(() => { void loadLocalSessions() }, [])
  const left = st.localSessions.filter((x) => !x.imported)
  const groups = [...new Set(left.map((x) => x.harness))]
  return (
    <Modal open wide title={t("views:import.title")} desc={t("views:import.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={!pick.length || busy} data-testid="import-go" onClick={async () => { setBusy(true); await importSessions(pick); setBusy(false); closeSheet(); go("#/sessions") }}>{busy && <Spinner className="size-3.5" />}{pick.length ? t("views:import.go", { count: pick.length }) : t("views:import.goNone")}</Button></>}>
      <div className="max-h-[60vh] overflow-y-auto">
      {left.length ? groups.map((h) => (
        <section key={h} className="mb-4"><SectionLabel>{t("views:import.onHub", { harness: h })}</SectionLabel>
          <Group>{left.filter((x) => x.harness === h).map((x) => (
            <label key={x.key} className="flex cursor-pointer items-center gap-3 px-4 py-2.5 hover:bg-accent" data-import-opt={x.key}>
              <Checkbox checked={pick.includes(x.key)} onCheckedChange={(v) => setPick(v ? [...pick, x.key] : pick.filter((k) => k !== x.key))} />
              <span className="min-w-0 flex-1"><span className="block truncate type-body">{x.title}</span><span className="block truncate type-footnote text-muted-foreground"><span className="font-mono">{x.cwd.replace(/^\/Users\/[^/]+/, "~")}</span> · {num(Math.max(1, Math.round(x.size / 1024)), { useGrouping: false })} KB</span></span>
              <span className="type-footnote text-muted-foreground">{tm(x.when)}</span>
            </label>))}</Group>
        </section>)) : <Empty title={st.localSessions.length ? t("views:import.allDone") : t("views:import.reading")} />}
      </div>
    </Modal>
  )
}
