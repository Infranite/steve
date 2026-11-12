import { api } from "@/api"
import { AgentDetailPanel, AgentTree, hasAgentTree, useAgentTree, useSelectedNode } from "@/agent-tree-view"
import { itemDisplayStatus, workflowRelation, workflowRelationText } from "@/item-workflow-relation"
import { DependencyWaitPanel } from "@/dependency-wait-panel"
import { ItemDebugDialog } from "@/debug-panel"
import { usePeopleOn } from "@/api/people"
import { Files } from "@/attach"
import { SessionCards } from "@/chat2"
import { Markdown } from "@/components/markdown"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
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
import { ACC, accKey, accOf, active, addDep, archive, bulk, closeRel, deleteItem, depsOpen, doFork, hm, importSessions, interrupt, it, itemThread, link, loadEvents, loadItem, loadLocalSessions, markDone, newItem, patchItem, pending, pendingCards, memCards, reassign, rmDep, saveGoal, sessPatch, sessRemove, sessSend, setAcc, setSt, setTags, setKind, kindOf, kindTags, isKind, DOMAIN_CATS, snoozed, st, stageOf, star, supp, tag, tagCatLabel, threadFollow, tm, toast, undoDecision, unsnooze, WT, type Item, type PlanRound, type Reply } from "@/store"
import { BotAvatar, BotName, Chips, closeSheet, Composer, DecisionCard, Empty, go, Group, ItemNo, KindTag, Meta, openSheet, Page, RelNote, ReplyBlock, Row, RunBar, SectionLabel, Segmented, SessionBlock, Status, StatusDot, Tags, ThemeSwitch, Transcript, type ComposerHandle } from "@/ui"
import { ChoiceCards, dueLabel, ItemRow, PlanBar, ProjectChip, Select, selected } from "@/view-shared"
import { choiceText, YoloItemSwitch, YoloMark } from "@/yolo/badges"
import { YoloPill, yoloState } from "@/yolo/switch"
import { RunState } from "@/yolo/status"
import { cn } from "cn"
import { lang, t as tr, useT } from "@/i18n"
import { nearTime, num } from "@/i18n/format"
import { ArchiveIcon, BanIcon, BookOpenIcon, BotIcon, CalendarIcon, ChartNoAxesColumnIcon, CheckIcon, ChevronDownIcon, ChevronRightIcon, CircleCheckIcon, ContactRoundIcon, CopyIcon, DownloadIcon, FolderIcon, GitPullRequestIcon, KeyRoundIcon, ServerIcon, LinkIcon, ListIcon, LockIcon, MailIcon, MessageSquareIcon, MessagesSquareIcon, MinusIcon, MoreHorizontalIcon, PauseIcon, PencilIcon, PlayIcon, PlugIcon, PlusIcon, RadioIcon, RotateCcwIcon, SearchIcon, SettingsIcon, SquareIcon, SquareTerminalIcon, StarIcon, TrashIcon, UploadIcon, UserRoundIcon, WorkflowIcon, XIcon, ZapIcon } from "lucide-react"
import { useEffect, useRef, useState, type ReactNode } from "react"
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
        {yoloState(st.yolo) === "paused" && <RunState className="mb-6 rounded-lg bg-destructive/5 px-3 py-2" />}
        {p.length > 0 && <Chips className="mb-6" value={typeLabel(typeKeys.includes(ibType) ? ibType : "all")} onChange={(v) => { ibType = typeKeys.find((k) => typeLabel(k) === v) ?? "all"; r() }} options={typeKeys.map(typeLabel)} />}
        {p.length === 0 && !mems && <Empty title={t("views:inbox.emptyTitle")} sub={t("views:inbox.emptySub")} />}
        {mems > 0 && <button type="button" onClick={() => go("#/knowledge?tab=pending")} className="mb-6 flex w-full items-center gap-2.5 rounded-xl border px-3 py-2.5 text-left type-callout transition-colors hover:bg-accent" data-inbox-mems>
          <BookOpenIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /><span className="min-w-0 flex-1">{t("views:inbox.mems", { count: mems })}</span><ChevronRightIcon className="size-4 shrink-0 text-muted-foreground" /></button>}
        <div className="flex flex-col gap-8">
          {types.map((ty) => {
            const g = xs.filter((c) => c.w.type === ty)
            return g.length ? <section key={ty}><SectionLabel>{WT[ty]} · {g.length}</SectionLabel><div className="flex flex-col gap-3">{g.map((c) => <DecisionCard key={c.w.decision} i={c.i} w={c.w} />)}</div></section> : null
          })}
          {sz.length > 0 && <section data-snoozed><SectionLabel>{t("views:inbox.snoozed", { count: sz.length })}</SectionLabel>
            <Group>{sz.map((i) => <Row key={i.id} title={<><ItemNo id={i.id} className="mr-2" />{i.t}</>} sub={t("views:inbox.snoozedAt", { type: WT[i.wait!.type], when: lang() === "zh-CN" ? i.snooze : nearTime(i.snoozeUntil) })}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => unsnooze(i.id)} data-unsnooze>{t("views:inbox.handleNow")}</Button></Row>)}</Group></section>}
        </div>
      </>}
      {ibTab === "done" && (st.done.length ? <>
        <Group>{st.done.map((d, n) => (
          <Row key={n} title={<><ItemNo id={d.id} className="mr-2" />{d.q}</>} sub={d.rel ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <RelNote rel={d.rel} className="inline-flex" /> · {d.tm}</span>
            : d.yolo !== undefined ? <span className="flex min-w-0 flex-wrap items-center gap-x-1">{WT[d.type]} · <YoloMark /> {d.yolo ? t("views:inbox.yoloChoseWhy", { choice: choiceText(d.choice), reason: d.yolo }) : t("views:inbox.yoloChose", { choice: choiceText(d.choice) })} · {d.tm}{d.undone ? ` · ${t("views:inbox.undone")}` : ""}</span>
            : `${WT[d.type]} · ${d.how === "dismissed" ? t("views:inbox.dismissed") : d.how === "expired" ? t("views:inbox.expired", { choice: d.choice }) : t("views:inbox.chose", { choice: d.choice })}${d.via ? ` · ${t("views:inbox.via", { client: d.via })}` : ""} · ${d.tm}${d.undone ? ` · ${t("views:inbox.undone")}` : ""}`} data-done-row={d.id}>
            {d.canUndo && <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => undoDecision(n)} data-undo-decision><RotateCcwIcon />{t("views:inbox.undo")}</Button>}
          </Row>))}</Group>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("views:inbox.doneNote")}</div>
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
const FILTER = (set: boolean) => cn("h-7 px-2 text-[12.5px] [&>svg]:opacity-60", set ? "text-foreground" : "text-muted-foreground"); const openGroups = new Set<string>()
/** the list, filtered: the stats page's drill-downs land here (“在事项页查看”) */
export const showItems = (f: "全部" | "等你" | "进行中", project?: string, bot?: string) => { filt = f === "等你" ? "wait" : f === "进行中" ? "run" : "all"; projF = project ?? "all"; botF = bot ?? "all"; tagF = "all"; kindF = "all"; q = ""; itemsView = "list"; go("#/items") }
export const showTag = (id: string) => { if (isKind(tag(id))) kindF = id; else tagF = id; filt = "all"; itemsView = "list"; go("#/items") }
const PRI_ORDER: Record<string, number> = { 高: 0, 中: 1, 低: 2 }
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
  const groups: [Item["st"], string][] = [["wait", t("views:items.groupWait")], ["prop", t("views:items.groupProp")], ["run", t("views:items.groupRun")], ["block", t("views:items.groupBlock")], ["pause", t("views:items.groupPause")], ["done", t("views:items.groupDone")], ["drop", t("views:items.groupDrop")]]
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
    <Page title={t("views:items.title")} sub={arch ? t("views:items.subArch") : t("views:items.sub", { run: st.items.filter((i) => itemDisplayStatus(i) === "run" && !i.arch).length, wait: pending().length })} max={itemsView === "list" ? 880 : 1120}
      actions={<>{itemsView === "list" && !arch && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { selecting = !selecting; selected.clear(); r() }} data-select-mode>{selecting ? t("views:items.selectDone") : t("views:items.select")}</Button>}<Button size="sm" onClick={() => openSheet("newItem")} data-new-item><PlusIcon />{t("views:items.newItem")}</Button></>}
      toolbar={<span className="hidden sm:block">{seg}</span>}>
      <AlertDialog open={!!confirmDel} onOpenChange={(o) => !o && setConfirmDel(null)}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("views:items.deleteTitle", { id: confirmDel })}</AlertDialogTitle><AlertDialogDescription>{t("views:items.deleteDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteItem(confirmDel!); setConfirmDel(null) }} data-delete-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <div className="mb-4 sm:hidden">{seg}</div>
      <div className="mb-4 flex flex-wrap items-center gap-1.5 border-b pb-3" data-items-toolbar>
        <Chips value={filtLabel(filt)} onChange={(v) => { filt = FILTS.find((k) => filtLabel(k) === v) ?? "all"; done() }} options={FILTS.map(filtLabel)} />
        <span className="flex-1" />
        <Select quiet className={FILTER(kindF !== "all")} testId="kind-filter" value={tag(kindF) ? kindF : "all"} onChange={(v) => { kindF = v; r() }} options={[{ v: "all", l: t("views:items.allKinds") }, ...kindTags().map((x) => ({ v: x.id, l: <KindTag id={x.id} className="border-0 bg-transparent px-0" /> }))]} />
        <Select quiet className={FILTER(tagF !== "all")} testId="tag-filter" value={tag(tagF) ? tagF : "all"} onChange={(v) => { tagF = v; r() }} options={[{ v: "all", l: t("views:items.allTags") }, ...DOMAIN_CATS.flatMap((c) => st.tags.filter((x) => x.cat === c).map((x) => ({ v: x.id, l: <span>{x.name} <span className="text-muted-foreground">· {c}</span></span> })))]} />
        <Select quiet className={FILTER(projF !== "all")} testId="project-filter" value={st.projects.some((p) => p.id === projF) || projF === "none" ? projF : "all"} onChange={(v) => { projF = v; r() }} options={[{ v: "all", l: t("views:items.allProjects") }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: t("views:items.noProject") }]} />
        <Select quiet className={FILTER(botF !== "all")} testId="bot-filter" value={botF} onChange={(v) => { botF = v; r() }} options={[{ v: "all", l: t("views:items.allBots") }, ...Object.keys(st.bots).map((k) => ({ v: k, l: <BotName k={k} /> }))]} />
        {itemsView === "list" && <Select quiet className={FILTER(sortBy !== "updated")} testId="sort" value={sortBy} onChange={(v) => { sortBy = v as ItemsSort; r() }} options={SORTS.map((v) => ({ v, l: sortLabel(v) }))} />}
        <label className="relative"><SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-tertiary" /><Input className={cn("h-7 rounded-lg pl-8 text-[13px] transition-[width] duration-200", q ? "w-40" : "w-8 cursor-pointer border-transparent bg-transparent placeholder:text-transparent focus:w-40 focus:cursor-text focus:border-border focus:placeholder:text-muted-foreground")} placeholder={t("views:items.search")} aria-label={t("views:items.search")} data-items-search defaultValue={q} onChange={(e) => { q = e.target.value; r() }} /></label>
      </div>
      {selecting && (
        <div className="sticky top-0 z-10 mb-4 flex flex-wrap items-center gap-2 rounded-xl border bg-background/95 px-3 py-2 backdrop-blur" data-bulk-bar>
          <span className="type-callout">{t("views:items.selected", { count: ids.length })}</span>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { xs.forEach((i) => selected.add(i.id)); r() }}>{t("views:items.selectAll")}</Button>
          <span className="flex-1" />
          {([["pause", t("views:items.bulkPause")], ["run", t("views:items.bulkRun")], ["arch", t("views:items.bulkArch")], ["drop", t("views:items.bulkDrop")]] as const).map(([op, l]) => <Button key={op} size="sm" variant={op === "drop" ? "ghost" : "outline"} className={cn(op === "drop" && "text-destructive")} disabled={!ids.length} onClick={() => { bulk(ids, op); done() }} data-bulk={op}>{l}</Button>)}
        </div>
      )}
      {itemsView === "list" && (xs.length ? (arch || sortBy !== "updated"
        ? <Group>{xs.map(rowOf)}</Group>
        : <div className="flex flex-col gap-5">{groups.map(([s, l]) => {
            const g = xs.filter((i) => itemDisplayStatus(i) === s), fold = (s === "done" || s === "drop") && !openGroups.has(s) && g.length > CLOSED_SHOWN + 2
            return g.length ? (
              <section key={s} data-items-group={s}>
                <SectionLabel className="mb-1 px-0.5" right={fold || openGroups.has(s) ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { openGroups.has(s) ? openGroups.delete(s) : openGroups.add(s); r() }} data-group-more={s}>{fold ? t("views:items.showAll", { count: g.length }) : t("views:items.collapse")}</Button> : undefined}>
                  <span className="inline-flex items-center gap-2"><StatusDot s={s} />{l}<span className="tabular-nums text-tertiary">{g.length}</span></span>
                </SectionLabel>
                <Group>{(fold ? g.slice(0, CLOSED_SHOWN) : g).map(rowOf)}</Group>
              </section>) : null })}</div>)
        : <Empty title={arch ? t("views:items.emptyArch") : st.items.length ? t("views:items.emptyFiltered") : t("views:items.emptyNone")} sub={arch ? (st.cfg.archDays === "0" ? t("views:items.archManual") : t("views:items.archAuto", { count: Number(st.cfg.archDays) })) : st.items.length ? undefined : t("views:items.emptyNoneSub")} />)}
      {itemsView === "board" && (
        <div className="-mx-1 grid auto-cols-[minmax(232px,1fr)] grid-flow-col gap-3 overflow-x-auto px-1 pb-2">
          {groups.filter((g) => g[0] !== "drop").map(([s, l]) => { const g = xs.filter((i) => itemDisplayStatus(i) === s); return (
            <div key={s} className="flex min-h-72 flex-col gap-2 rounded-xl bg-muted/60 p-2">
              <div className="flex items-center gap-2 px-2 py-1.5 type-label"><StatusDot s={s} />{l}<span className="tabular-nums text-muted-foreground">{g.length}</span></div>
              {g.map((i) => { const relation = workflowRelation(i, st.items); return (
                <button key={i.id} type="button" onClick={() => go("#/items/" + i.id)} className="rounded-lg border bg-card p-3 text-left transition-shadow duration-150 hover:shadow-md" data-kanban-card={i.id}>
                  <ItemNo id={i.id} /><div className="mt-0.5 type-callout font-medium">{i.t}</div>
                  {relation && <div className="mt-1 type-footnote font-medium text-foreground/80" data-workflow-relation={relation.kind}>{workflowRelationText(relation)}</div>}
                  {relation?.kind !== "paused" && relation?.kind !== "previous" && <div className="mt-1 line-clamp-2 type-footnote text-muted-foreground">{i.now}</div>}
                  <Tags ids={i.tags} max={2} className="mt-2" /><Meta className="mt-2"><BotName k={i.b} />{i.project && <ProjectChip id={i.project} plain />}{i.due && <span>{t("views:items.due", { due: dueLabel(i.due) })}</span>}{i.plan && <PlanBar i={i} />}</Meta>
                </button>
              ) })}
            </div>) })}
        </div>
      )}
      {itemsView === "graph" && (
        <div className="flex flex-col gap-3">
          <GraphLegend />
          <RelationGraph scope="all" hideSimple={!showAll} height={560} onOpen={openNode} />
          <div className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground">{showAll ? t("views:items.graphAll") : t("views:items.graphFiltered")}<Button size="xs" variant="outline" onClick={() => { showAll = !showAll; r() }}>{showAll ? t("views:items.graphLinkedOnly") : t("views:items.graphShowAll")}</Button></div>
        </div>
      )}
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
  const s = st.S[k], e = s?.ev[at]
  const [mode, setMode] = useState<"item" | "sess">("item"), [g, setG] = useState(""), [busy, setBusy] = useState(false)
  useEffect(() => { if (s && !s.loaded) void loadEvents(k) }, [k])
  if (!s) return null
  return (
    <Modal open title={t("views:fork.title")} desc={t("views:fork.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="fork-go" disabled={busy} onClick={async () => { setBusy(true); const to = await doFork(k, at, g.trim(), mode === "sess"); closeSheet(); go(to) }}>{busy && <Spinner className="size-3.5" />}{t("views:fork.go")}</Button></>}>
      {e && <div className="line-clamp-3 rounded-lg bg-muted px-3.5 py-2.5 type-callout text-muted-foreground">{String(e[1])}</div>}
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
      <Group>{st.items.filter((i) => !["done", "drop"].includes(i.st)).map((i) => (
        <Row key={i.id} title={i.t} onClick={() => { link(k, i.id); closeSheet() }}><Status s={i.st} /></Row>
      ))}</Group>
    </Modal>
  )
}
/* ═════════ 事项详情 ═════════ */
/* tab values stay the original words: they are the data-seg hooks the browser tests click; labels are translated */
let dtab: "动态" | "产出" = "动态"; let lastItem = ""; let feedMode: "key" | "all" = "all"
/* system lines worth keeping in the “重要” view; the rest (建立、来自信号…) is shown only in “全部” */
const KEY_SYS = /派生|交付|目标更新|前置|开新会话|交接|自动开始|暂停|放弃|继续|关联|发布|rebase|冲突|失败|回到 /
function Prop({ label, children, top }: { label: string; children: ReactNode; top?: boolean }) {
  return <><dt className={cn("pt-1.5 type-footnote text-muted-foreground", top && "pt-1")}>{label}</dt><dd className="min-w-0 type-callout">{children}</dd></>
}
/* add a tag: search existing ones by name, category or description; create a new one when nothing fits */
function TagPicker({ i }: { i: Item }) {
  const t = useT()
  const [q, setQ] = useState("")
  const has = new Set(i.tags ?? [])
  const xs = st.tags.filter((x) => !isKind(x) && !has.has(x.id) && (!q || [x.name, x.cat, x.desc].some((v) => v.toLowerCase().includes(q.toLowerCase()))))
  const domain = (i.tags ?? []).filter((x) => !isKind(tag(x)))
  return (
    <DropdownMenu onOpenChange={(o) => !o && setQ("")}>
      <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-tag />}>{domain.length ? t("views:item.addMore") : t("views:item.addNone")}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-80 w-72">
        <div className="p-1"><Input autoFocus className="h-8 rounded-md text-[13px]" placeholder={t("views:item.tagSearch")} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} data-tag-search /></div>
        {DOMAIN_CATS.map((c) => { const g = xs.filter((x) => x.cat === c); return g.length ? <div key={c}><div className="px-2 pb-0.5 pt-2 type-footnote text-muted-foreground">{tagCatLabel(c)}</div>
          {g.map((x) => <DropdownMenuItem key={x.id} onClick={() => setTags(i.id, [...(i.tags ?? []), x.id])} data-tag-opt={x.name} className="flex-col items-start gap-0"><span className="type-callout">{x.name}</span><span className="line-clamp-1 type-footnote text-muted-foreground">{x.desc}</span></DropdownMenuItem>)}</div> : null })}
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
            <ChevronRightIcon className="size-3.5 transition-transform group-data-[panel-open]/round:rotate-90" />
            <span className={cn(r.end === "replaced" && "line-through decoration-tertiary")}>{t("views:item.round", { n: r.n })}</span>
            <span>· {r.end === "done" ? t("views:item.roundDone", { done: k, total: r.steps.length }) : t("views:item.roundReplaced", { done: k, total: r.steps.length })}</span>
            <span className="text-tertiary">· {r.at}</span>
          </CollapsibleTrigger>
          <CollapsibleContent className="mt-1.5 pl-5"><PlanSteps steps={r.steps} old={r.end} /></CollapsibleContent>
        </Collapsible>) })}
      {i.rounds.length > 0 && cur.length > 0 && <div className="mt-1 type-footnote text-muted-foreground">{t("views:item.roundCurrent", { n: i.rounds.length + 1, done: dn, total: cur.length })}</div>}
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
  const [older, setOlder] = useState(false), [titleEdit, setTitleEdit] = useState(false), [preview, setPreview] = useState<OutOpen | null>(null)
  if (lastItem !== id) { lastItem = id; dtab = "动态" }
  useEffect(() => { void loadItem(id, true) }, [id])
  useEffect(() => { setDebugOpen(false) }, [id])
  const i = st.items.find((x) => x.id === id)
  /* replies to comments are read from the sessions' events */
  const latest = i && (active(i)?.id ?? i.ss.at(-1))
  useEffect(() => { if (latest) void loadEvents(latest) }, [latest])
  /* the collaboration area: the tree of everything this item started; a selected agent's detail replaces the
   * properties in the side column (a sheet on narrow pages) */
  const at = useAgentTree(id, i?.workflow)
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
  /* activity feed: filter, then fold everything but the latest four entries (a queued comment is never folded) */
  const feed = i.tl.map((e, n) => ({ e, n })).filter(({ e }) => feedMode === "all" || e[0] !== "sys" || KEY_SYS.test(e[1]))
  const firstQueued = feed.findIndex(({ n }) => queued.byComment.has(n))
  const KEEP = 4, hidden = feedMode === "key" && !older ? Math.max(0, Math.min(feed.length - KEEP, firstQueued < 0 ? Infinity : firstQueued)) : 0
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
        {["done", "drop"].includes(i.st) && <DropdownMenuItem onClick={() => { archive([i.id], !i.arch); if (!i.arch) return; go("#/items") }} data-archive><ArchiveIcon />{i.arch ? t("views:item.restore") : t("views:item.archive")}</DropdownMenuItem>}
        {i.arch && <DropdownMenuItem variant="destructive" onClick={() => setConfirmDel(true)} data-delete><TrashIcon />{t("views:item.deleteForever")}</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  </>
  /* always-visible quick actions beside the title; labels fold into icons on narrow screens */
  const over = ["done", "drop"].includes(i.st)
  const [pl, PI, pa] = over ? [t("views:item.reopen"), RotateCcwIcon, () => setSt(i.id, "run")] as const
    : i.st === "pause" ? [t("views:item.resume"), PlayIcon, () => setSt(i.id, "run")] as const
    : [t("views:item.pause"), PauseIcon, () => setSt(i.id, "pause")] as const
  const quick = (
    <div className="flex items-center gap-1.5" data-quick-actions>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={pl} title={pl} disabled={i.st === "prop"} onClick={pa} data-quick="pause"><PI /><span className="max-sm:sr-only">{pl}</span></Button>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={t("views:item.markDone")} title={t("views:item.markDone")} disabled={over || doneBusy} onClick={() => { setDoneBusy(true); void markDone(i.id).finally(() => setDoneBusy(false)) }} data-quick="done"><CircleCheckIcon /><span className="max-sm:sr-only">{t("views:item.markDone")}</span></Button>
      <Popover open={confirmDrop} onOpenChange={setConfirmDrop}>
        <PopoverTrigger render={<Button variant="destructive" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={t("views:item.drop")} title={t("views:item.drop")} disabled={over} data-quick="drop" />}><BanIcon /><span className="max-sm:sr-only">{t("views:item.drop")}</span></PopoverTrigger>
        <PopoverContent align="end" className="w-64 gap-3 rounded-2xl p-3" data-drop-popover>
          <div className="type-callout"><div className="font-medium">{t("views:item.dropTitle", { id: i.id })}</div><div className="mt-0.5 text-muted-foreground">{t("views:item.dropDesc")}</div></div>
          <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setConfirmDrop(false)}>{t("common:action.cancel")}</Button><Button size="sm" variant="destructive" autoFocus onClick={() => { setSt(i.id, "drop"); setConfirmDrop(false) }} data-drop-confirm>{t("views:item.drop")}</Button></div>
        </PopoverContent>
      </Popover>
    </div>
  )
  const tree = hasAgentTree(at)
  const top = <>
          {/* 1 — goal reads as the description of the title */}
          <section data-goal className="-mt-3">
            {editing ? (
              <Field><Textarea className="min-h-24 rounded-lg" value={gtxt} onChange={(e) => setG(e.target.value)} data-goal-input autoFocus />
                <div className="flex flex-wrap items-center gap-2"><Button size="sm" onClick={() => { saveGoal(i.id, gtxt.trim()); setEditing(false) }}>{t("views:item.saveAs", { n: i.goals.length + 1 })}</Button><Button size="sm" variant="ghost" onClick={() => setEditing(false)}>{t("common:action.cancel")}</Button><FieldDescription className="type-footnote">{t("views:item.goalHint")}</FieldDescription></div></Field>
            ) : (
              <div className="group/goal">
                <Markdown text={i.goals[i.goals.length - 1]} className="type-body text-foreground/80" />
                <div className="mt-1 flex items-center gap-2 type-footnote text-muted-foreground">
                  <span>{t("views:item.goalVersion", { n: i.goals.length })}</span>
                  {i.goals.length > 1 && <Collapsible><CollapsibleTrigger className="hover:text-foreground">{t("views:item.history")}</CollapsibleTrigger><CollapsibleContent className="mt-1">{i.goals.slice(0, -1).map((g, n) => <div key={n}>{t("views:item.historyRow", { n: n + 1, goal: g })}</div>)}</CollapsibleContent></Collapsible>}
                  <button type="button" className="opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/goal:opacity-100 [@media(hover:none)]:opacity-100" data-edit-goal onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>{t("views:item.edit")}</button>
                </div>
              </div>
            )}
          </section>
          {/* 2 — what needs you is the only boxed element on the page */}
          {i.waits.length > 0 && <section className="flex flex-col gap-3">{i.waits.map((w) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}</section>}
  </>
  const replies: Reply[] = i.tl.flatMap((e) => e[0] === "reply" && e[1] ? [e[1]] : [])
  const entry = ({ e, n }: { e: any[]; n: number }) => {
    if (e[0] === "sess") return <SessionBlock key={e[1]} k={e[1]} defaultOpen={false} replies={replies} />
    if (e[0] === "reply") return <ReplyBlock key={"reply:" + e[1].s + ":" + e[1].from} r={e[1]} />
    const q = e[0] === "me" && queued.byComment.get(n)
    if (q) return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[10px] font-medium text-muted-foreground">{t("views:item.me")}</span>
        <QueuedComment sid={i.qs!} q={q} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} at={e[2]} body={(t) => <div className="type-body"><Markdown text={t} /></div>} /></div>
    )
    /* notes sent from an output show what they mark and open the output there */
    const notes = e[0] === "me" && e[7] != null ? i.anns.filter((x) => x.timeline_id === e[7]) : []
    if (e[0] === "me") return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[10px] font-medium text-muted-foreground">{t("views:item.me")}</span>
        <div className="min-w-0 flex-1">{notes.length ? <div data-comment-notes><div className="type-body">{t("views:item.notes", { count: notes.length })}</div><AnnRefs item={i.id} anns={notes} outs={i.outs} open={(o, ann) => setPreview({ o, focus: ann })} /></div>
          : (e[1] !== "（附件）" || !e[5]?.length) && <div className="type-body"><Markdown text={e[1]} /></div>}<Files files={e[5]} className="my-1" /><div className="type-footnote text-muted-foreground">{e[2]}{e[3] && st.S[e[3]] ? ` · ${t("views:item.deliveredTo", { n: st.S[e[3]].n })}` : ""}{e[6] === "thread" && <a href="#/" className="ml-1.5 rounded bg-muted px-1.5 hover:text-foreground" data-from-thread>{t("views:item.fromThread")}</a>}</div></div></div>
    )
    const yolo = typeof e[1] === "string" && e[1].startsWith("YOLO")
    return <div key={n} className="flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-yolo-line={yolo ? "" : undefined}><span className="flex size-5 shrink-0 items-center justify-center"><span className="size-1 rounded-full bg-tertiary" /></span>{yolo && <YoloMark />}<span className="min-w-0 truncate">{yolo ? e[1].replace(/^YOLO[：:]\s*/, "") : e[1]}</span><span className="shrink-0 text-tertiary">{e[2]}</span></div>
  }
  return (
    <Page small title={titleEdit ? <Input autoFocus defaultValue={i.t} className="h-9 rounded-lg text-[20px] font-semibold" data-title-input
        onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== i.t) patchItem(i.id, { t: v }); setTitleEdit(false) }} onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); if (e.key === "Escape") setTitleEdit(false) }} />
      : <span className="cursor-text" onDoubleClick={() => setTitleEdit(true)} data-title>{i.t}</span>} back={["#/items", t("views:items.title")]} actions={actions} toolbar={quick} toolbarTop max={1080}
      footer={<div className="w-full shrink-0 border-t bg-background/85 backdrop-blur-xl"><div className="mx-auto w-full px-4 pb-[76px] pt-3 sm:px-8 md:pb-4" style={{ maxWidth: 1080 + 64 }}><div className="lg:pr-[296px]">
        {i.qs && <QueueStrip sid={i.qs} rows={queued.rest} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} />}
        <div className="mb-2 type-footnote font-medium text-foreground/80" data-item-feedback-label>{t("views:item.messageBot")}</div>
        <Composer draftKey={draftKey(st.workspace?.id, "item", i.id)} handle={itemComposer} placeholder={t("views:item.composer", { id: i.id })} testId="item" onSend={(text, files) => supp(i.id, text, target, files)}
          left={<Segmented size="sm" value={targets.includes(target) ? target : "auto"} onChange={setTarget} options={targets.map((x) => [x, targetLabel[x]] as const)} />} />
      </div></div></div>}
      sub={<Meta><button type="button" className="inline-flex items-center gap-1 font-mono tabular-nums hover:text-foreground" data-copy-id onClick={() => { navigator.clipboard?.writeText(i.id).catch(() => {}); toast(t("views:item.idCopied", { id: i.id })) }}>{i.id}<CopyIcon className="size-3" /></button><Status s={i.st} /><ProjectChip id={i.project} /><span>{t("views:item.updated", { time: i.up })}</span></Meta>}>
      {st.yolo && !over && <div className="-mt-3 mb-5"><YoloItemSwitch i={i} /></div>}
      {debugOpen && <ItemDebugDialog item={i.id} onClose={() => setDebugOpen(false)} />}
      <AlertDialog open={confirmDel} onOpenChange={setConfirmDel}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("views:item.deleteTitle", { id: i.id })}</AlertDialogTitle><AlertDialogDescription>{t("views:item.deleteDesc", { count: i.ss.length })}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { go("#/items"); deleteItem(i.id) }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <OutDialog item={i.id} outs={i.outs} anns={i.anns} view={preview} setView={setPreview} reviewLabel={t(designReview ? "views:item.reviewDesign" : "views:item.messageBot")}
        onReview={() => { setPreview(null); window.setTimeout(() => { if (designReview) document.querySelector<HTMLTextAreaElement>("[data-design-feedback]")?.focus(); else itemComposer.current?.focus() }, 250) }} />
      {i.arch && <div className="mb-6 flex items-center gap-3 rounded-xl border px-4 py-2.5 type-callout" data-arch-banner><ArchiveIcon className="size-4 text-muted-foreground" /><span className="flex-1">{t("views:item.archived")}</span><Button size="sm" variant="ghost" onClick={() => archive([i.id], false)}>{t("views:item.restore")}</Button></div>}
      {/* an item that started others leads with its collaboration, across the page: a tree needs the width */}
      {tree && <div className="mb-12 flex flex-col gap-10">{top}<AgentTree at={at} selected={agent?.key ?? null} onSelect={selectAgent} /></div>}
      <AgentDetailPanel at={at} node={agent} onClose={() => selectAgent(null)} />
      <div className="grid gap-12 lg:grid-cols-[minmax(0,1fr)_248px]">
        <div className="flex min-w-0 flex-col gap-10">
          {!tree && top}
          <section data-now>
            <div className="mb-1.5 flex flex-wrap items-center gap-x-1 gap-y-1 type-footnote" data-stage={stageOf(i)}>
              {STAGES.slice(2).map((g, n, arr) => { const cur = STAGES.indexOf(stageOf(i) as any), me = STAGES.indexOf(g); return (
                <span key={g} className="inline-flex items-center gap-1"><span className={cn(me === cur ? "font-medium text-foreground" : me < cur ? "text-muted-foreground" : "text-tertiary")}>{t(STAGE_LABEL[g])}</span>{n < arr.length - 1 && <ChevronRightIcon className="size-3 text-tertiary" />}</span>) })}
            </div>
            <div className="flex items-center gap-2"><StatusDot s={i.st} /><span className={cn("type-headline", i.st === "run" && "shimmer shimmer-once")}>{i.now}</span></div>
            {i.rel && <RelNote rel={i.rel} box={i.rel.tone !== "ok"} className="mt-2" onClose={i.rel.tone === "fail" ? () => closeRel(i) : undefined} />}
            {(i.plan || i.rounds.length > 0) && <Plan i={i} />}
          </section>
          {i.dependencyWait && !!i.deps?.length && <DependencyWaitPanel key={`${i.id}:${i.dependencyWait.cycle}`} item={i.id} wait={i.dependencyWait} /> }
          {/* 3 — activity, newest at the bottom, older entries folded */}
          <section className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center justify-between gap-2 border-b pb-2">
              <Segmented size="sm" value={dtab} onChange={(v) => { dtab = v; r() }} options={[["动态", t("views:item.tabActivity")], ["产出", t("views:item.tabOutputs")]] as const} />
              {dtab === "动态" && <Chips value={feedMode === "key" ? t("views:item.feedKey") : t("views:item.feedAll")} onChange={(v) => { feedMode = v === t("views:item.feedKey") ? "key" : "all"; setOlder(false); r() }} options={[t("views:item.feedKey"), t("views:item.feedAll")]} />}
            </div>
            {dtab === "动态" && (
              <div className="flex flex-col" data-worklog>
                {hidden > 0 && <button type="button" onClick={() => setOlder(true)} className="mb-1 flex items-center gap-2 self-start py-1 type-footnote text-muted-foreground hover:text-foreground" data-older><ChevronDownIcon className="size-3.5" />{t("views:item.older", { count: hidden })}</button>}
                {feed.slice(hidden).map(entry)}
              </div>
            )}
            {dtab === "产出" && <OutList outs={i.outs} anns={i.anns} open={(o) => setPreview({ o })} />}
          </section>
        </div>
        {/* properties: one aligned label column, controls stay quiet until hovered */}
        <aside className="flex flex-col gap-8">
          <dl className="grid grid-cols-[64px_minmax(0,1fr)] items-start gap-x-3 gap-y-2.5">
            <Prop label={t("views:item.propBot")}><Select quiet testId="bot" value={i.b} onChange={(k) => reassign(i.id, k)} options={Object.keys(st.bots).filter((k) => st.bots[k].on || k === i.b).map((k) => ({ v: k, l: <BotName k={k} /> }))} />{B && <div className="pl-2 type-footnote text-muted-foreground">{B.h} · {B.m}</div>}</Prop>
            <ItemPeopleProp id={i.id} Prop={Prop} />
            <Prop label={t("views:item.propKind")}><Select quiet testId="kind" value={kindOf(i) ?? ""} onChange={(k) => setKind(i.id, k)} options={kindTags().map((x) => ({ v: x.id, l: <KindTag id={x.id} className="border-0 bg-transparent px-0" /> }))} />{kindOf(i) && <div className="pl-2 type-footnote text-muted-foreground">{tag(kindOf(i)!)?.desc}</div>}</Prop>
            <Prop label={t("views:item.propTags")}><div className="flex flex-col gap-1.5 pt-1" data-item-tags>
              <Tags ids={(i.tags ?? []).filter((x) => !isKind(tag(x)))} onRemove={(id) => setTags(i.id, (i.tags ?? []).filter((x) => x !== id))} />
              <TagPicker i={i} /></div></Prop>
            <Prop label={t("views:item.propPri")}><Select quiet testId="pri" value={i.pri ?? "中"} onChange={(v) => patchItem(i.id, { pri: v as Item["pri"] })} options={["高", "中", "低"].map((v) => ({ v, l: labelOf(PRI_LABEL, v) }))} /></Prop>
            <Prop label={t("views:item.propDue")}><Select quiet testId="due" value={i.due ?? "无"} onChange={(v) => patchItem(i.id, { due: v === "无" ? undefined : v })} options={[...new Set(["无", i.due ?? "无", "今天 18:00", "明天", "周五", "下周一"])].map((v) => ({ v, l: dueLabel(v) }))} /></Prop>
            <Prop label={t("views:item.propAcc")}><Select quiet testId="acc" value={accKey(i.acc)} onChange={(v) => setAcc(i.id, v)} options={["inherit", "manual", "auto"].map((k) => ({ v: k, l: ACC[k] }))} />{accKey(i.acc) === "inherit" && <div className="pl-2 type-footnote text-muted-foreground">{t("views:item.accEffective", { mode: accOf(i) })}</div>}</Prop>
            <Prop label={t("views:item.propDeps")}>
              <div className="flex flex-col gap-1 pt-1" data-deps>
                {(i.deps || []).filter((d) => st.items.some((x) => x.id === d)).map((d) => { const x = it(d); return (
                  <div key={d} className="group/dep flex items-center gap-2"><StatusDot s={x.st} /><a href={"#/items/" + d} className="min-w-0 flex-1 truncate hover:underline"><span className="font-mono text-[12px] tabular-nums text-muted-foreground">{d}</span> {x.t}</a>
                    <button type="button" aria-label={t("views:item.removeDep", { id: d })} onClick={() => rmDep(i.id, d)} className="text-tertiary opacity-0 transition-opacity hover:text-foreground group-hover/dep:opacity-100 [@media(hover:none)]:opacity-100"><XIcon className="size-3.5" /></button></div>) })}
                <DropdownMenu>
                  <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-dep />}>{(i.deps || []).length ? t("views:item.addMore") : t("views:item.addNone")}</DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-72 min-w-64">
                    {st.items.filter((x) => x.id !== i.id && !(i.deps || []).includes(x.id) && x.st !== "drop").map((x) => <DropdownMenuItem key={x.id} onClick={() => addDep(i.id, x.id)} data-dep-opt={x.id}><span className="font-mono text-[12px] text-muted-foreground">{x.id}</span><span className="truncate">{x.t}</span></DropdownMenuItem>)}
                  </DropdownMenuContent>
                </DropdownMenu>
                {i.st === "block" && <span className="type-footnote text-muted-foreground">{t("views:item.depsLeft", { count: depsOpen(i).length })}</span>}
              </div>
            </Prop>
            {dependents.length > 0 && <Prop label={t("views:item.propDependents")}><div className="flex flex-col gap-1 pt-1">{dependents.map((x) => <a key={x.id} href={"#/items/" + x.id} className="flex items-center gap-2 hover:underline"><StatusDot s={x.st} /><span className="min-w-0 truncate"><span className="font-mono text-[12px] tabular-nums text-muted-foreground">{x.id}</span> {x.t}</span></a>)}</div></Prop>}
            <Prop label={t("views:item.propSessions")}><div className="flex flex-col gap-2 pt-1">{i.ss.filter((k) => st.S[k]).length ? i.ss.filter((k) => st.S[k]).map((k) => <div key={k} className="flex min-w-0 flex-col gap-0.5" data-item-session={k}>
              <a href={"#/sessions/" + k} className="flex items-center gap-2 hover:underline"><span>#{st.S[k].n}</span><Status s={st.S[k].st} sess /><span className="min-w-0 truncate type-footnote text-muted-foreground">{st.S[k].h}</span><span className="ml-auto type-footnote tabular-nums text-muted-foreground">{st.S[k].ctx ? st.S[k].ctx + "%" : ""}</span></a>
              <NativeIdLine k={k} label="ID" /></div>) : <span className="text-muted-foreground">{t("common:state.empty")}</span>}</div></Prop>
            <ItemStatsRows id={i.id} render={(u, time, href) => <><Prop label={t("views:item.propUsage")}><a href={href} className="block pt-1 hover:underline" data-item-usage>{u}</a></Prop><Prop label={t("views:item.propTime")}><a href={href} className="block pt-1 hover:underline" data-item-time>{time}</a></Prop></>} />
            <Prop label={t("views:item.propProject")}><Select quiet testId="project" value={i.project ?? "none"} onChange={(v) => patchItem(i.id, { project: v === "none" ? null : v })} options={[{ v: "none", l: t("common:state.none") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} />
              {i.projNext && <div className="pl-2 type-footnote text-muted-foreground">{t("views:item.projNext")}</div>}</Prop>
            {i.cwd && <Prop label={t("views:item.propCwd")}><span className="block break-all pt-1 font-mono text-[12px] text-muted-foreground" data-cwd>{i.cwd.replace(/^\/Users\/[^/]+/, "~")}</span></Prop>}
            {i.branch && <Prop label={t("views:item.propBranch")}><span className="block break-all pt-1 font-mono text-[12px]" data-branch>{i.branch}</span></Prop>}
            <Prop label={t("views:item.propSource")}><span className="block pt-1">{i.parent?.item ? <a href={"#/items/" + i.parent.item} className="hover:underline">{t("views:item.forkedFrom", { id: i.parent.item })}</a> : i.src[1] === "chat" ? <a href="#/" className="hover:underline">{t("views:item.fromChat")}</a> : i.src[0]}</span>
              {itemThread(i.id) && <label className="mt-1.5 flex items-center gap-2 pl-0.5 type-footnote text-muted-foreground" data-thread-follow><Switch size="sm" checked={!itemThread(i.id)!.follow} onCheckedChange={(v) => void threadFollow(i.id, !v)} />{t("views:item.noFollow")}</label>}</Prop>
          </dl>
        </aside>
      </div>
    </Page>
  )
}

/* ═════════ 会话 ═════════ */
type SessFilt = "all" | "run" | "unlinked" | "imported"
let sf: SessFilt = "all", sq = "", hf = "all"
const SFILTS: SessFilt[] = ["all", "run", "unlinked", "imported"]
const sfLabel = (k: SessFilt) => tr(({ all: "views:sessions.filterAll", run: "views:sessions.filterRun", unlinked: "views:sessions.filterUnlinked", imported: "views:sessions.filterImported" } as const)[k])
export function SessionsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [hits, setHits] = useState<Set<string> | null>(null)
  useEffect(() => { if (!sq) { setHits(null); return } const tm = setTimeout(() => api.searchSessions(sq).then((xs) => setHits(new Set(xs))).catch(() => setHits(new Set())), 250); return () => clearTimeout(tm) }, [sq])
  const all = Object.keys(st.S).map((k) => [k, st.S[k]] as const)
  const xs = all.filter(([k, s]) => (hf === "all" || s.h === hf) && (sf === "all" || (sf === "run" && s.st === "run") || (sf === "unlinked" && !s.item) || (sf === "imported" && !!s.ro))
    && (!sq || s.title.includes(sq) || (s.item ?? "").toLowerCase().includes(sq.toLowerCase()) || !!hits?.has(k)))
  return (
    <Page title={t("views:sessions.title")} sub={t("views:sessions.sub", { total: all.length, run: all.filter((x) => x[1].st === "run").length })} actions={<Button size="sm" variant="outline" onClick={() => openSheet("import")} data-import><UploadIcon />{t("views:sessions.import")}</Button>}>
      <label className="relative mb-3 block"><SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-tertiary" />
        <Input className="h-10 rounded-xl bg-muted/70 pl-9 text-[14px]" placeholder={t("views:sessions.search")} defaultValue={sq} onChange={(e) => { sq = e.target.value.trim(); r() }} data-session-search /></label>
      <div className="mb-5 flex flex-wrap items-center gap-2"><Chips value={sfLabel(sf)} onChange={(v) => { sf = SFILTS.find((k) => sfLabel(k) === v) ?? "all"; r() }} options={SFILTS.map(sfLabel)} /><span className="flex-1" />
        <Select testId="harness-filter" value={hf} onChange={(v) => { hf = v; r() }} options={[{ v: "all", l: t("views:sessions.allHarness") }, ...st.runtimes.map((x) => ({ v: x.name, l: x.name }))]} /></div>
      {xs.length ? (
        <Group>{xs.map(([k, s]) => (
          <div key={k} role="button" tabIndex={0} onClick={() => go("#/sessions/" + k)} data-session-row={k} className="flex cursor-pointer items-start gap-3 rounded-lg px-2 py-3 transition-colors hover:bg-accent">
            <StatusDot s={s.st} className="mt-[7px]" />
            <div className="min-w-0 flex-1">
              <div className="truncate type-body font-medium">{s.title}</div>
              <Meta className="mt-0.5">{s.item ? <span className="max-w-[300px] truncate"><span className="font-mono">{s.item}</span> · #{s.n}</span> : <span>{t("views:sessions.unlinked")}</span>}{s.ro && <Badge variant="outline" className="h-4.5 font-normal text-muted-foreground"><LockIcon />{t("views:sessions.readOnly")}</Badge>}<span>{hm(s)}</span><span>{st.devices.find((d) => d.id === s.device)?.name ?? "Hub"}</span><span>{s.src}</span>{s.ctx > 0 && <span>{t("views:sessions.ctx", { pct: s.ctx })}</span>}</Meta>
              {sq && hits?.has(k) && <div className="mt-1 type-footnote text-muted-foreground">{t("views:sessions.contentHit", { q: sq })}</div>}
            </div>
            <span className="shrink-0 type-footnote text-muted-foreground">{s.up}</span>
          </div>
        ))}</Group>
      ) : <Empty title={all.length ? t("views:sessions.emptyFiltered") : t("views:sessions.emptyNone")} sub={all.length ? undefined : t("views:sessions.emptyNoneSub")} />}
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
      <HoverCardContent className="w-60"><div className="type-label">{t("views:session.ctx")}</div>{s.contextKnown && <Progress value={Math.min(100, s.ctx)} className="my-2" />}<div className="type-footnote text-muted-foreground">{s.contextKnown ? t("views:session.ctxUsed", { pct: s.ctx }) : t("views:session.ctxNone")}</div></HoverCardContent>
    </HoverCard>
  )
  const panel = (
    <div className="flex flex-col gap-6">
      {i ? (
        <div><SectionLabel>{t("views:session.item")}</SectionLabel>
          <button type="button" onClick={() => go("#/items/" + i.id)} className="-mx-2 w-[calc(100%+16px)] rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-accent"><div className="type-callout font-medium"><span className="mr-1.5 font-mono text-[12px] font-normal text-muted-foreground">{i.id}</span>{i.t}</div><Status s={i.st} className="mt-1" /></button>
          <div className="mt-1 px-1 type-callout text-muted-foreground"><ProjectChip id={i.project} /></div></div>
      ) : (
        <div><SectionLabel>{t("views:session.unlinked")}</SectionLabel><div className="flex flex-wrap gap-2">{!s.ro && <Button size="sm" variant="outline" onClick={() => openSheet("link", { k })} data-link>{t("views:session.link")}</Button>}<Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</Button></div></div>
      )}
      <div><SectionLabel right={<Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => openSheet("graph", { k })}>{t("views:session.zoom")}</Button>}>{t("views:session.near")}</SectionLabel>
        <RelationGraph key={k} focus={k} scope="near" height={260} minimal onOpen={openNode} /></div>
      <SessionKnowledge id={k} />
      <div data-session-resume><SectionLabel>{t("views:session.resume")}</SectionLabel><ResumeCard k={k} /></div>
      <div><SectionLabel>{t("views:session.info")}</SectionLabel><div className="flex flex-col gap-1 px-1 type-callout text-muted-foreground"><BotName k={s.b} className="text-foreground" /><span>{hm(s)}</span><span>{t("views:session.effort", { v: s.reasoningEffort ?? t("views:session.effortInherit") })}</span><a href={"#/devices/" + s.device} className="hover:text-foreground">{t("views:session.runsOn", { device: st.devices.find((d) => d.id === s.device)?.name ?? "Hub" })}</a><span>{t("views:session.source", { src: s.src })}</span><span>{t("views:session.durCtx", { dur: s.dur, ctx: s.contextKnown ? `${s.ctx}%` : t("views:session.unreported") })}</span></div></div>
    </div>
  )
  return (
    <div className="flex h-full min-h-0">
      <div className="min-w-0 flex-1">
        <Page noTitle title={t("views:session.numbered", { n: s.n })} back={i ? ["#/items/" + i.id, t("views:items.title")] : ["#/sessions", t("views:sessions.title")]} max={720} scrollRef={scroller}
          bar={<div className="flex min-w-0 items-center gap-2"><span className="truncate type-headline">{s.n ? t("views:session.numberedBar", { n: s.n }) : ""}{renaming ? <Input autoFocus defaultValue={s.title} className="inline-flex h-7 w-56 rounded-md" data-rename-input onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== s.title) sessPatch(k, { title: v }, t("views:session.renamed")); setRenaming(false) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : s.title}</span><Status s={s.st} sess pill className="hidden sm:inline-flex" />{i && <ProjectChip id={i.project} className="hidden shrink-0 type-footnote text-muted-foreground sm:inline-flex" />}</div>}
          actions={<>{!s.ro && <SessionStats id={k} />}{/* the runtime line under the conversation says it too: below xl the title needs the room */}<span className="hidden xl:inline-flex">{ctx}</span><Button size="sm" variant="ghost" className="text-muted-foreground xl:hidden" onClick={() => setInfoOpen(true)}>{t("views:session.info")}</Button>{s.ro && <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><LockIcon className="size-3" />{t("views:session.readOnly")}</span>}{menu}</>}
          footer={
            <div className="mx-auto w-full max-w-[784px] shrink-0 px-4 pb-[76px] sm:px-8 lg:pb-5">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 py-2 type-footnote text-muted-foreground" data-session-runtime>
                <span>Harness <span className="text-foreground">{s.h ?? t("views:session.unknown")}</span></span>
                <NativeIdLine k={k} label={t("views:session.nativeId")} className="w-auto" />
                <span className="break-all">{t("views:session.model")} <span className="text-foreground">{s.m ?? t("views:session.defaultModel")}</span></span>
                <span>{t("views:session.effortLabel")} <span className="text-foreground">{s.reasoningEffort ?? t("views:session.effortInherit")}</span></span>
                <span className={s.ctx >= 80 ? "text-attention" : ""}>{s.contextKnown ? t("views:session.ctxPct", { pct: s.ctx }) : t("views:session.ctxUnknown")}</span>
              </div>
              <RunBar k={k} onStop={s.ro ? undefined : () => interrupt(k)} />
              {!s.ro && <QueueStrip sid={k} rows={s.queue} note={queueNote(s)} starting={s.starting} composer={composer} />}
              {s.ro ? (
                <div className="flex flex-wrap items-center gap-3 rounded-2xl border bg-card px-4 py-3" data-readonly>
                  <LockIcon className="size-4 text-muted-foreground" /><span className="min-w-0 flex-1 type-callout text-muted-foreground">{t("views:session.readOnlyNote", { src: s.src.startsWith("本机 ") ? t("views:session.srcLocal", { h: s.src.slice(3) }) : s.src })}</span>
                  <Button size="sm" variant="outline" onClick={() => openSheet("fork", { k, at: Math.max(0, s.ev.length - 1) })}>{t("views:session.toItem")}</Button>
                </div>
              ) : <Composer key={k} draftKey={draftKey(st.workspace?.id, "session", k)} handle={composer} testId="session" model={hm(s)} placeholder={s.st === "ended" ? t("views:session.endedPlaceholder") : t("views:session.placeholder")} onSend={(text, files) => sessSend(k, text, mode, files)}
                left={run ? <Segmented size="sm" value={mode} onChange={setMode} options={[["插话", t("views:session.modeSteer")], ["排队", t("views:session.modeQueue")]] as const} /> : undefined} />}
            </div>
          }>
          <div className="pt-6"><Transcript k={k} /><SessionCards k={k} /></div>
        </Page>
      </div>
      <aside className="hidden w-[300px] shrink-0 overflow-y-auto border-l border-sidebar-border bg-sidebar px-5 py-5 xl:block" data-session-panel>{panel}</aside>
      {debugOpen && s.item && <ItemDebugDialog item={s.item} session={k} onClose={() => setDebugOpen(false)} />}
      <Dialog open={infoOpen} onOpenChange={setInfoOpen}><DialogContent className="max-h-[85dvh] overflow-y-auto"><DialogHeader><DialogTitle>{t("views:session.info")}</DialogTitle><DialogDescription>{t("views:session.infoDesc")}</DialogDescription></DialogHeader>{panel}</DialogContent></Dialog>
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
  return on ? <Group><Row icon={<Ico I={UserRoundIcon} />} title={t("views:more.me")} sub={t("views:more.meSub")} onClick={() => go("#/contacts/me")} data-more-me><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group> : null
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
        <Group><Row icon={<BotAvatar k="steve" size={40} />} title={<span className="type-headline">{st.bots.steve?.n ?? "Steve"}</span>} sub={t("views:more.steveSub")} onClick={() => go("#/bots/steve")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
        {groups.map((group) => <section key={group.name}><SectionLabel>{group.name}</SectionLabel><Group>{group.rows.map(([h, title, s, I]) => (
          <Row key={h} icon={<Ico I={I} />} title={title} sub={s} onClick={() => go("#/" + h)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
        ))}</Group></section>)}
        <HubMore />
        <Group><GitHubMoreRow /></Group>
        {st.yolo && <Group><Row icon={<Ico I={ZapIcon} />} title={t("views:more.yolo")} sub={{ on: t("views:more.yoloOn"), full: t("views:more.yoloFull"), off: t("views:more.yoloOff"), paused: t("views:more.yoloPaused", { reason: st.yolo.paused?.reason ?? "" }), forced: t("views:more.yoloForced") }[yoloState(st.yolo)!]} onClick={() => openSheet("yolo")} data-more-yolo><ChevronRightIcon className="size-4 text-tertiary" /></Row>
          <Row title={t("views:more.yoloLog")} sub={t("views:more.yoloLogSub")} onClick={() => go("#/yolo")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>}
        <Group><Row title={t("views:more.appearance")}><ThemeSwitch labels /></Row></Group>
      </div>
    </Page>
  )
}

/* ═════════ 视觉规范 ═════════ */
function lum(hex: string) {
  const m = hex.replace("#", ""); const c = [0, 2, 4].map((n) => parseInt(m.slice(n, n + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
const ratio = (a: string, b: string) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05) }
function cssHex(v: string) {
  const el = document.createElement("span"); el.style.color = `var(${v})`; document.body.appendChild(el)
  const c = getComputedStyle(el).color; el.remove()
  const m = c.match(/[\d.]+/g)?.map(Number) ?? [0, 0, 0]
  return "#" + m.slice(0, 3).map((n) => Math.round(n).toString(16).padStart(2, "0")).join("")
}
/* translated when the page renders, so a language switch shows at once */
const ROLES = (): [string, string, string][] => [
  ["--foreground", tr("views:design.roleForeground"), tr("views:design.roleForegroundUse")], ["--muted-foreground", tr("views:design.roleMuted"), tr("views:design.roleMutedUse")], ["--tertiary", tr("views:design.roleTertiary"), tr("views:design.roleTertiaryUse")],
  ["--blue", tr("views:design.roleBlue"), tr("views:design.roleBlueUse")], ["--attention", tr("views:design.roleAttention"), tr("views:design.roleAttentionUse")], ["--destructive", tr("views:design.roleDestructive"), tr("views:design.roleDestructiveUse")], ["--success", tr("views:design.roleSuccess"), tr("views:design.roleSuccessUse")],
]
const SURF = (): [string, string][] => [["--background", tr("views:design.surfBackground")], ["--sidebar", tr("views:design.surfSidebar")], ["--muted", tr("views:design.surfMuted")], ["--card", tr("views:design.surfCard")], ["--border", tr("views:design.surfBorder")]]
export function DesignView() {
  const t = useT()
  const [, f] = useState(0)
  useEffect(() => { const tm = setTimeout(() => f((x) => x + 1), 30); return () => clearTimeout(tm) }, [st.ver])
  const bg = cssHex("--background")
  const Do = ({ ok: good, children, note }: { ok: boolean; children: ReactNode; note: string }) => (
    <div className="flex flex-col gap-2"><div className="flex min-h-24 items-center justify-center rounded-xl border bg-card p-4">{children}</div>
      <div className="flex items-start gap-1.5 type-footnote">{good ? <CheckIcon className="mt-px size-3.5 text-success" /> : <XIcon className="mt-px size-3.5 text-destructive" />}<span className="text-muted-foreground">{note}</span></div></div>
  )
  return (
    <Page title={t("views:design.title")} sub={t("views:design.sub")} max={880} toolbar={<ThemeSwitch labels />}>
      <div className="flex flex-col gap-12">
        <section>
          <SectionLabel>{t("views:design.principles")}</SectionLabel>
          <div className="grid gap-3 sm:grid-cols-3">
            {[[t("views:design.p1"), t("views:design.p1Desc")], [t("views:design.p2"), t("views:design.p2Desc")], [t("views:design.p3"), t("views:design.p3Desc")]].map(([title, d]) => (
              <div key={title} className="rounded-xl border bg-card px-4 py-3.5"><div className="type-headline">{title}</div><div className="mt-1 type-callout text-muted-foreground">{d}</div></div>
            ))}
          </div>
        </section>
        <section>
          <SectionLabel>{t("views:design.colors")}</SectionLabel>
          <Group>{ROLES().map(([v, n, use]) => { const hx = cssHex(v), cr = ratio(hx, bg); return (
            <div key={v} className="flex items-center gap-3 px-4 py-2.5">
              <span className="size-7 shrink-0 rounded-full ring-1 ring-inset ring-black/5" style={{ background: `var(${v})` }} />
              <div className="min-w-0 flex-1"><div className="type-body">{n} <span className="font-mono text-[12px] text-muted-foreground">{v.slice(2)}</span></div><div className="type-footnote text-muted-foreground">{use}</div></div>
              <span className="font-mono text-[12px] text-muted-foreground">{hx.toUpperCase()}</span>
              <span className={cn("w-16 text-right font-mono text-[12px] tabular-nums", cr >= 4.5 ? "text-foreground" : "text-muted-foreground")} data-contrast={v}>{cr.toFixed(1)}:1</span>
            </div>) })}</Group>
          <div className="mt-6 grid grid-cols-2 gap-3 sm:grid-cols-5">
            {SURF().map(([v, n]) => <div key={v}><div className="h-16 rounded-xl border" style={{ background: `var(${v})` }} /><div className="mt-1.5 type-footnote"><span className="text-foreground">{n}</span> <span className="font-mono text-muted-foreground">{cssHex(v).toUpperCase()}</span></div></div>)}
          </div>
        </section>
        <section>
          <SectionLabel>{t("views:design.type")}</SectionLabel>
          <Group>{([["type-large-title", t("views:design.typeLargeTitle"), t("views:design.typeLargeTitleSample")], ["type-title", t("views:design.typeTitle"), t("views:design.typeTitleSample")], ["type-headline", t("views:design.typeHeadline"), t("views:design.typeHeadlineSample")], ["type-body", t("views:design.typeBody"), t("views:design.typeBodySample")], ["type-callout", t("views:design.typeCallout"), t("views:design.typeCalloutSample")], ["type-footnote", t("views:design.typeFootnote"), t("views:design.typeFootnoteSample")], ["type-code", t("views:design.typeCode"), "let target = state.return_to.unwrap_or(\"/\");"]] as const).map(([c, l, s]) => (
            <div key={c} className="flex flex-col gap-1 px-4 py-3 sm:flex-row sm:items-baseline sm:gap-6"><span className="w-40 shrink-0 type-footnote text-muted-foreground">{l}</span><span className={cn(c, "min-w-0 truncate")}>{s}</span></div>
          ))}</Group>
        </section>
        <section>
          <SectionLabel>{t("views:design.status")}</SectionLabel>
          <div className="flex flex-wrap gap-x-6 gap-y-3 rounded-xl border bg-card px-4 py-4">{["run", "wait", "prop", "pause", "done", "drop"].map((s) => <Status key={s} s={s} className="type-callout" />)}<span className="w-full" />{["run", "wait", "idle", "ended"].map((s) => <Status key={s} s={s} sess className="type-callout" />)}</div>
        </section>
        <section>
          <SectionLabel>{t("views:design.components")}</SectionLabel>
          <div className="grid gap-6 lg:grid-cols-2">
            <div className="flex flex-col gap-5 rounded-xl border bg-card p-5">
              <div className="type-footnote text-muted-foreground">{t("views:design.buttons")}</div>
              <div className="flex flex-wrap items-center gap-2"><Button>{t("views:design.accept")}</Button><Button variant="outline">{t("views:design.rework")}</Button><Button variant="ghost">{t("common:action.cancel")}</Button><Button variant="destructive">{t("views:design.dropItem")}</Button></div>
              <div className="type-footnote text-muted-foreground">{t("views:design.controls")}</div>
              <div className="flex flex-wrap items-center gap-3"><Segmented value="list" onChange={() => {}} options={[["list", t("views:items.viewList")], ["board", t("views:items.viewBoard")], ["graph", t("views:items.viewGraph")]]} /><Switch defaultChecked /><Input className="h-8 w-40 rounded-lg" placeholder={t("views:design.searchItems")} /></div>
              <div className="type-footnote text-muted-foreground">{t("views:design.composer")}</div>
              <Composer placeholder={t("views:design.composerPlaceholder")} onSend={() => toast(t("views:design.composerToast"))} />
            </div>
            <div className="flex flex-col gap-5 rounded-xl border bg-card p-5">
              <div className="type-footnote text-muted-foreground">{t("views:design.agent")}</div>
              <div className="rounded-lg border p-4">{(() => { const k = Object.keys(st.S).find((x) => !st.S[x].ro); return k ? <Transcript k={k} compact tail={3} /> : <span className="type-callout text-muted-foreground">{t("views:design.agentEmpty")}</span> })()}</div>
            </div>
          </div>
        </section>
        <section>
          <SectionLabel>{t("views:design.dos")}</SectionLabel>
          <div className="grid gap-x-4 gap-y-6 sm:grid-cols-2">
            <Do ok note={t("views:design.doStatus")}><div className="flex gap-4"><Status s="run" className="type-callout" /><Status s="wait" className="type-callout" /><Status s="done" className="type-callout" /></div></Do>
            <Do ok={false} note={t("views:design.dontStatus")}><div className="flex gap-2">{[[t("views:design.tagRun"), "#0071e3"], [t("views:design.tagWait"), "#ff9500"], [t("views:design.tagDone"), "#28a745"], [t("views:design.tagProp"), "#9a5cff"]].map(([l, c]) => <span key={l} className="rounded-md px-2 py-0.5 text-[12px] font-medium text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note={t("views:design.doAvatar")}><div className="flex gap-2">{Object.keys(st.bots).slice(0, 5).map((k) => <BotAvatar key={k} k={k} size={32} />)}</div></Do>
            <Do ok={false} note={t("views:design.dontAvatar")}><div className="flex gap-2">{["#5b8def", "#e0703a", "#2fa36b", "#9a5cff", "#d6a23a"].map((c, n) => <span key={c} className="flex size-8 items-center justify-center rounded-lg text-[13px] font-semibold text-white" style={{ background: c }}>{[...t("views:design.botInitials")][n]}</span>)}</div></Do>
            <Do ok note={t("views:design.doButtons")}><div className="flex gap-2"><Button variant="ghost">{t("views:design.noReply")}</Button><Button variant="outline">{t("views:design.moreFormal")}</Button><Button>{t("views:design.send")}</Button></div></Do>
            <Do ok={false} note={t("views:design.dontButtons")}><div className="flex gap-2">{[[t("views:design.noReply"), "#e5352b"], [t("views:design.rewrite"), "#ff9500"], [t("views:design.send"), "#28a745"]].map(([l, c]) => <span key={l} className="rounded-lg px-3 py-1.5 text-[13px] font-medium text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note={t("views:design.doHint")}><span className="type-footnote text-muted-foreground">{t("views:item.goalHint")}</span></Do>
            <Do ok={false} note={t("views:design.dontHint")}><span className="rounded-lg bg-[#e8f0fe] px-3 py-2 text-[12.5px] font-medium text-[#1a56db]">{t("views:design.hintBox")}</span></Do>
          </div>
        </section>
      </div>
    </Page>
  )
}

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
