import { PermissionPolicyPanel, SessionPermission } from "@/approval"
import type { BotPlacementError } from "@/api"
import { useTranscript } from "@/data/transcript"
import { HarnessMark } from "@/components/harness-mark"
import { useItemState, decisionView, useOpenDecisions, useViewKinds } from "@/data/sync"
import { decideCards, useItemDetail, useNotices, useSessionQueue, useShellStore } from "@/store"
import { getItem, getItems, getSession, itemDisplayStatus } from "@/store"
import { VirtualList } from "@/components/virtual-list"
import { isClosedItemState, itemState, itemStateSublabel, type ItemState } from "@/item-state"
import { clip } from "@/thread/topics"
import { BlockedPlacement, BotProposalCard, DispatchDetails, PlacementWhy, NO_TEAM, formatDispatchTimeline, useBotTeams } from "@/bot-dispatch"
import { SetSelect } from "@/settings-controls"
import { DependencyWaitPanel } from "@/dependency-wait-panel"
import { ItemDebugDialog } from "@/debug-panel"
import { Files } from "@/attach"
import { Markdown } from "@/components/markdown"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { draftKey, newClientId } from "@/composer-draft"
import { copyText } from "@/lib/clipboard"
import { GitHubMoreRow } from "@/github/index"
import { HubMore } from "@/hub-ui"
import { lazyView } from "@/lazy"
import { ModelPicker } from "@/model-picker"
import { EffortPicker } from "@/effort-picker"
import type { ReasoningEffort } from "@/gen"
import { RuntimeAvailability, RuntimePermissionNotice, RuntimeSelect, RuntimeSetupLink } from "@/runtime-access"
import { AnnRefs } from "@/annotate"
import { OutDialog, OutList, type OutOpen } from "@/item-outputs"
import { ItemPeopleProp } from "@/people/item-people"
import { QueuedComment, queuedComments, queueNote, QueueStrip } from "@/queue"
import { homeTilde } from "@/lib/path"
import { NativeIdLine } from "@/resume"
import { ItemStatsRows } from "@/stats/drill"
import { ACC, accKey, botName, closedSt, setDesignGate, accOf, active, addDep, archive, closeRel, defaultHarness, deleteItem, depsOpen, doFork, handoff, hm, importSessions, it, itemThread, link, loadLocalSessions, markDone, newItem, patchItem, reassign, rmDep, runtimeName, saveGoal, setAcc, actOnItem, setTags, setKind, kindOf, kindTags, isKind, DOMAIN_CATS, st, star, startItemCycle, supp, tag, tagCatLabel, tagDesc, tagLabel, threadFollow, tm, toast, type Item, type PlanRound, type Reply } from "@/store"
import { BotName, closeSheet, Empty, go, Group, ItemNo, KindTag, Meta, openSheet, Page, RelNote, Row, SectionLabel, Segmented, Status, StatusDot, Tags, ThemeSwitch, Chevron, Panel, Section, Tile } from "@/ui"
import { disambiguateActors, formatActorName, resolveActor } from "@/actor"
import { ActorAvatar } from "@/components/actor-avatar"
import { Composer, type ComposerHandle } from "@/composer"
import { DecisionCard } from "@/thread/decision-card"
import { ReplyBlock, SessionBlock } from "@/thread/transcript"
import { ChoiceCards, dueLabel, ProjectChip, Select } from "@/view-shared"
import { cn } from "cn"
import { t as tr, useT } from "@/i18n"
import { dayHeading, hm as clock, num } from "@/i18n/format"
import { ArchiveIcon, ArrowRightLeftIcon, BanIcon, BookOpenIcon, BotIcon, CalendarIcon, ChartNoAxesColumnIcon, CheckIcon, ChevronRightIcon, CircleAlertIcon, CircleCheckIcon, CircleDashedIcon, CircleHelpIcon, ContactRoundIcon, CopyIcon, FolderIcon, GitPullRequestIcon, HourglassIcon, KeyRoundIcon, LinkIcon, ListIcon, LoaderCircleIcon, LockIcon, MailIcon, MessageSquareIcon, MessagesSquareIcon, MinusIcon, MoreHorizontalIcon, PauseIcon, PlayIcon, PlugIcon, PlusIcon, RadioIcon, RotateCcwIcon, SettingsIcon, SquareTerminalIcon, StarIcon, TrashIcon, XIcon, ZapIcon, BlocksIcon, UploadIcon, NewspaperIcon, WorkflowIcon } from "lucide-react"
import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
export { ChoiceCards,ItemRowExport,Select } from "@/view-shared"
const RelationGraph = lazyView(() => import("@/graph").then((m) => ({ default: m.RelationGraph })))
const GraphLegend = lazyView(() => import("@/graph").then((m) => ({ default: m.GraphLegend })))

/* priority is stored as these Chinese values; only its labels are translated (due presets: dueLabel in view-shared; acceptance modes: ACC in the store) */
const PRI_LABEL = { 高: "views:item.priHigh", 中: "views:item.priMid", 低: "views:item.priLow" } as const
/** the label of a stored value, or the value itself when it is not one of ours (a due date the Hub wrote, say) */
const labelOf = <M extends Record<string, Parameters<typeof tr>[0]>>(m: M, v: string): string => (v in m ? String(tr(m[v as keyof M] as any)) : v)

const openNode = (n: { k: string; id: string }) => go((n.k === "item" ? "#/items/" : n.k === "proj" ? "#/projects/" : "#/sessions/") + n.id)

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
export function HandoffDialog({ item, session }: { item: string; session?: string }) {
  const t = useT()
  const i = getItem(item)
  const prev = (session ? getSession(session) : undefined) ?? (i ? active(i) ?? getSession(i.ss[i.ss.length - 1] ?? "") : undefined)
  const curBot = prev?.b ?? i?.b ?? "steve"
  const curHarness = prev?.harness ?? st.bots[curBot]?.harness ?? defaultHarness()
  const enabledBots = Object.keys(st.bots).filter((k) => st.bots[k].on)
  const [mode, setMode] = useState<"same" | "harness" | "bot">("same")
  const [bot, setBot] = useState(() => enabledBots.find((k) => k !== curBot) ?? curBot)
  const [harness, setHarness] = useState(curHarness)
  const [model, setModel] = useState("")
  const [effort, setEffort] = useState<ReasoningEffort>("default")
  const [makeDefault, setMakeDefault] = useState(false)
  const [busy, setBusy] = useState(false)
  const [placementError, setPlacementError] = useState<BotPlacementError>()
  useEffect(() => setPlacementError(undefined), [mode, bot, harness, model, effort, makeDefault])
  if (!i) return null
  const activeHarness = mode === "bot" ? (st.bots[bot]?.harness ?? curHarness) : mode === "harness" ? harness : curHarness
  const canSubmit = !busy && (mode === "same" || (mode === "harness" ? !!harness : !!st.bots[bot]))
  const showDefaultCheck = mode === "harness" || (mode === "same" && (!!model || effort !== "default"))
  const submit = async () => {
    setPlacementError(undefined)
    setBusy(true)
    const eff = effort !== "default" ? { effort } : {}
    const choice = mode === "same"
      ? { ...(model ? { model } : {}), ...eff, ...(showDefaultCheck && makeDefault ? { make_bot_default: true } : {}) }
      : mode === "harness"
        ? { harness, ...(model ? { model } : {}), ...eff, ...(makeDefault ? { make_bot_default: true } : {}) }
        : { bot, ...(model ? { model } : {}), ...eff }
    const next = await handoff({ item, session }, choice, setPlacementError)
    setBusy(false)
    if (next) {
      closeSheet()
      if (session) go("#/sessions/" + next)
    }
  }
  return (
    <Modal open title={t("views:handoff.title")} desc={t("views:handoff.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="handoff-go" disabled={!canSubmit} onClick={() => void submit()}>{busy && <Spinner className="size-3.5" />}{t("views:handoff.go")}</Button></>}>
      <FieldGroup className="gap-4">
        {prev && <div className="rounded-lg bg-muted px-3.5 py-2.5 type-footnote text-muted-foreground" data-handoff-from>
          {t("views:handoff.current", { n: prev.n, harness: runtimeName(prev.harness ?? "") ?? prev.harness ?? prev.h ?? "", model: prev.m ? ` · ${prev.m}` : "" })}
        </div>}
        <Field><FieldLabel>{t("views:handoff.mode")}</FieldLabel>
          <ChoiceCards value={mode} onChange={(v) => { setMode(v); setModel(""); setMakeDefault(false) }} cols={1} options={[
            { v: "same", t: t("views:handoff.same"), d: t("views:handoff.sameDesc") },
            { v: "harness", t: t("views:handoff.harness"), d: t("views:handoff.harnessDesc") },
            ...(enabledBots.length > 1 ? [{ v: "bot" as const, t: t("views:handoff.bot"), d: t("views:handoff.botDesc") }] : []),
          ]} /></Field>
        {mode === "harness" && <Field><FieldLabel>{t("views:handoff.chooseHarness")}</FieldLabel>
          <RuntimeSelect testId="handoff-harness" value={harness} onChange={(h) => { setHarness(h); setModel("") }} />
          {harness && <RuntimeAvailability harness={harness} />}</Field>}
        {mode === "bot" && <Field><FieldLabel>{t("views:handoff.chooseBot")}</FieldLabel>
          <SetSelect wide testId="handoff-bot" label={t("views:handoff.chooseBot")} value={bot} onChange={(k) => { setBot(k); setModel("") }}
            options={enabledBots.map((k) => ({ v: k, l: `${st.bots[k].n} · ${runtimeName(st.bots[k].harness) ?? st.bots[k].harness}` }))} /></Field>}
        {activeHarness && <Field><FieldLabel>{t("views:handoff.chooseModel")}</FieldLabel>
          <ModelPicker field testId="handoff-model" harness={activeHarness} value={model} onChange={setModel} allowDefault defaultLabel={t("views:handoff.modelDefault")} /></Field>}
        {activeHarness && <Field><FieldLabel>{t("views:handoff.chooseEffort")}</FieldLabel>
          <EffortPicker wide testId="handoff-effort" label={t("views:handoff.chooseEffort")} harness={activeHarness} model={model || null} value={effort} defaultLabel={t("views:handoff.effortInherit")} onChange={setEffort} /></Field>}
        {showDefaultCheck && <label className="flex cursor-pointer items-start gap-2.5 type-callout" data-handoff-make-default>
          <Checkbox checked={makeDefault} onCheckedChange={(v) => setMakeDefault(!!v)} className="mt-0.5" />
          <span className="min-w-0 flex-1"><span className="block">{t("views:handoff.makeBotDefault", { bot: st.bots[curBot]?.n ?? curBot })}</span>
            <span className="block type-footnote text-muted-foreground">{t("views:handoff.makeBotDefaultDesc")}</span></span>
        </label>}
        {placementError && <BlockedPlacement blocked={placementError.blocked} />}
      </FieldGroup>
    </Modal>
  )
}
export function ForkDialog({ k, at }: { k: string; at?: number }) {
  const t = useT(), data = useTranscript(k)
  const s = getSession(k), seq = at ?? data?.newest
  const turn = data?.turns.findLast((turn) => seq != null && turn.start <= seq)
  const said = turn?.ev.findLast((e, i) => turn.seqs[i] <= seq! && ["u", "a", "ask"].includes(e[0]) && typeof e[1] === "string" && e[1].trim())?.[1] as string | undefined
  const [mode, setMode] = useState<"item" | "sess">("item"), [g, setG] = useState(""), [busy, setBusy] = useState(false)
  if (!s) return null
  return (
    <Modal open title={t("views:fork.title")} desc={t("views:fork.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="fork-go" disabled={busy || seq == null} onClick={async () => { setBusy(true); const to = await doFork(k, seq!, g.trim(), mode === "sess"); closeSheet(); go(to) }}>{busy && <Spinner className="size-3.5" />}{t("views:fork.go")}</Button></>}>
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
export function NewItemDialog({ project: pre, bot: preBot }: { project?: string; bot?: string } = {}) {
  useShellStore()
  const t = useT()
  const [g, setG] = useState(""), [mode, setMode] = useState<"start" | "propose">("start"), [bot, setBot] = useState(preBot ?? "auto"), [proj, setProj] = useState(pre ?? "auto"), [busy, setBusy] = useState(false)
  const { data: teams } = useBotTeams()
  const [placementError, setPlacementError] = useState<BotPlacementError>()
  const [specialties, setSpecialties] = useState("")
  const requiredSpecialties = specialties.split(/[,\uFF0C]/u).map(tag => tag.trim()).filter(Boolean)
  const validTags = requiredSpecialties.length <= 12 && requiredSpecialties.every(tag => tag.length <= 48)
  const [team, setTeam] = useState("")
  useEffect(() => setPlacementError(undefined), [g, bot, proj, mode, team, specialties])
  const activeTeam = teams?.teams.find(v => v.id === (team || teams.default))
  const teamMembers = Object.keys(st.bots).filter(k => !st.bots[k].main && st.bots[k].on && (!activeTeam || activeTeam.members.some(m => m.bot === k)))
  const allowed = Object.keys(st.bots).filter(k => st.bots[k].on && (st.bots[k].main || !activeTeam || activeTeam.members.some(m => m.bot === k)))
  const target = bot
  const selected = st.bots[target]
  // The execution target is resolved by the backend. Hub-local probes cannot admit or reject it.
  // A proposal creates a decision, without opening a session.
  const canSubmit = validTags && (target === "auto" || !!selected)
  /* one send idempotency key while the form stays the same: a retry after a lost answer makes no second item */
  const sendKey = useRef<{ form: string; id: string } | null>(null)
  const clientId = () => {
    const form = JSON.stringify([g.trim(), target, proj, mode, team, requiredSpecialties])
    if (sendKey.current?.form !== form) sendKey.current = { form, id: newClientId() }
    return sendKey.current.id
  }
  return (
    <Modal open title={t("views:newItem.title")} desc={t("views:newItem.desc")}
      footer={<><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button data-testid="new-go" disabled={!g.trim() || busy || !canSubmit} onClick={async () => {
        setPlacementError(undefined)
        setBusy(true); const id = await newItem(g.trim(), target === "auto" ? undefined : target, proj === "auto" ? undefined : proj === "none" ? null : proj, mode, clientId(), team || undefined, { requiredSpecialties, blocked: setPlacementError })
        setBusy(false); if (id) { closeSheet(); go("#/items/" + id) } }}>{busy && <Spinner className="size-3.5" />}{t("views:newItem.create")}</Button></>}>
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="new-goal">{t("views:newItem.goal")}</FieldLabel><Textarea id="new-goal" autoFocus className="min-h-24 rounded-lg" placeholder={t("views:newItem.goalPlaceholder")} value={g} onChange={(e) => setG(e.target.value)} /></Field>
        {!!teams?.teams.length && <Field><FieldLabel>{t("manage:dispatch.teams")}</FieldLabel><SetSelect wide testId="new-item-team" label={t("manage:dispatch.teams")} value={team || "default"}
          options={[{ v: "default", l: t("manage:dispatch.useDefault", { name: teams.teams.find(v => v.id === teams.default)?.name ?? t("manage:dispatch.noTeam") }) }, { v: NO_TEAM, l: t("manage:dispatch.noTeam") }, ...teams.teams.map(v => ({ v: v.id, l: v.name }))]}
          onChange={v => { setTeam(v === "default" ? "" : v); setBot("auto") }} /></Field>}
        {activeTeam && !teamMembers.length && <p role="status" className="type-footnote text-attention">{t("manage:dispatch.noMembers")}</p>}
        <div className="grid grid-cols-2 gap-3">
          <Field><FieldLabel>{t("views:newItem.bot")}</FieldLabel><Select testId="new-bot" value={target} onChange={setBot} options={[{ v: "auto", l: t("views:newItem.auto") }, ...[...new Set([...allowed, ...(selected ? [target] : [])])].map((k) => ({ v: k, l: <span><BotName k={k} />{st.bots[k].main ? ` · ${t("manage:dispatch.coordinator")}` : ""} · {runtimeName(st.bots[k].harness) ?? st.bots[k].harness}</span> }))]} /></Field>
          <Field><FieldLabel>{t("views:newItem.project")}</FieldLabel><Select testId="new-project" value={proj} onChange={setProj} options={[{ v: "auto", l: t("views:newItem.auto") }, ...st.projects.map((p) => ({ v: p.id, l: p.name })), { v: "none", l: t("views:newItem.noProject") }]} /></Field>
        </div>
        <Field><FieldLabel htmlFor="new-specialties">{t("manage:dispatch.requiredSpecialties")}</FieldLabel><Input id="new-specialties" value={specialties} onChange={e => setSpecialties(e.target.value)} placeholder={t("manage:dispatch.specialtyPlaceholder")} /></Field>
        {placementError && <BlockedPlacement blocked={placementError.blocked} />}
        {placementError?.proposal && <BotProposalCard key={placementError.proposal.id} proposal={placementError.proposal} onClose={() => setPlacementError(undefined)} />}
        {selected && <><RuntimeAvailability harness={selected.harness} /><RuntimePermissionNotice harness={selected.harness} mode={selected.permMode} /></>}
        {!selected && <div className="type-footnote text-muted-foreground"><p>{t("shell:runtimeSetup.chooseBot")} · {t("shell:runtimeSetup.targetPending")}</p><RuntimeSetupLink /></div>}
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
  useViewKinds(["item", "session", "decision"])
  const t = useT()
  return (
    <Modal open title={t("views:link.title")} desc={t("views:link.desc")}>
      <Group>{getItems().filter((i) => !closedSt(i.st)).map((i) => (
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
const STATE_ICON: Record<ItemState["kind"], typeof CheckIcon> = {
  done: CheckIcon, dropped: BanIcon, failed: CircleAlertIcon, proposed: CircleHelpIcon, asking: CircleHelpIcon,
  paused: PauseIcon, blocked: HourglassIcon, dispatching: LoaderCircleIcon, working: LoaderCircleIcon, queued: HourglassIcon, closing: HourglassIcon, idle: CircleDashedIcon,
}
function StateMark({ state, centered }: { state: ItemState; centered?: boolean }) {
  const I = STATE_ICON[state.kind]
  return <span className={cn(!centered && "mt-0.5", "flex size-7 shrink-0 items-center justify-center rounded-full", STATE_TONE[state.tone][1])} aria-hidden><I className={cn("size-4", (state.kind === "working" || state.kind === "dispatching") && "motion-safe:animate-spin")} strokeWidth={2.25} /></span>
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
/** Short timelines stay flat in the DOM (and preserve direct-child queries in small fixtures); longer logs window with VirtualList. */
const WORKLOG_VIRTUALIZE_THRESHOLD = 40
type WorklogEntry = { e: any[]; n: number }
type WorklogRow =
  | { id: string; kind: "day"; day: string; first: boolean }
  | { id: string; kind: "run"; items: WorklogEntry[] }
  | { id: string; kind: "entry"; item: WorklogEntry }

function estimateWorklogRow(row: WorklogRow): number {
  if (row.kind === "day") return row.first ? 24 : 44
  if (row.kind === "run") return 32
  const k = row.item.e[0]
  if (k === "me") return 88
  if (k === "sess") return 44
  if (k === "reply") return 96
  return 28
}

/* a folded run of process lines: how many and the latest one; opens in place */
function ProcessRun({ count, last, renderChildren }: { count: number; last: any[]; renderChildren: () => ReactNode }) {
  const t = useT(), [open, setOpen] = useState(false)
  const preview = last[0] === "dispatch" ? formatDispatchTimeline(String(last[1] ?? ""), t) : String(last[1] ?? "")
  return <Collapsible open={open} onOpenChange={setOpen} data-process-run={count}>
    <CollapsibleTrigger className="flex w-full min-w-0 items-center gap-2 py-1 text-left type-footnote text-muted-foreground hover:text-foreground">
      <span className="flex size-5 shrink-0 items-center justify-center"><ChevronRightIcon className={cn("size-3.5 transition-transform duration-180 ease-(--ease-out)", open && "rotate-90")} /></span>
      <span className="shrink-0">{t("views:itemState.process", { count })}</span>
      <span className={cn("min-w-0 truncate text-tertiary transition-opacity duration-180", open && "opacity-0")}>· {clip(preview, 120)}</span>
      <span className="ml-auto shrink-0 tabular-nums text-tertiary">{when(last)}</span>
    </CollapsibleTrigger>
    <CollapsibleContent className="ml-2.5 border-l pl-2">{open && renderChildren()}</CollapsibleContent>
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
  useShellStore()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const t = useT()
  const scroll = useRef<HTMLDivElement>(null)
  const [target, setTarget] = useState<"auto" | "steer" | "new">("auto")
  const [editing, setEditing] = useState(false), [gtxt, setG] = useState("")
  const [confirmDrop, setConfirmDrop] = useState(false), [confirmDel, setConfirmDel] = useState(false)
  const [doneBusy, setDoneBusy] = useState(false)
  const [debugOpen, setDebugOpen] = useState(false)
  const itemComposer = useRef<ComposerHandle | null>(null)
  const [titleEdit, setTitleEdit] = useState(false), [preview, setPreview] = useState<OutOpen | null>(null)
  if (lastItem !== id) { lastItem = id; dtab = "动态" }

  useEffect(() => { setDebugOpen(false) }, [id])
  const i = useItemDetail(id)
  useNotices(i?.rel?.tone === "fail")
  const work = useItemState(id)
  const openDecs = useOpenDecisions()
  useSessionQueue(i?.qs)
  /* the one state the page states: what is happening and whose move it is (item-state.ts) */
  const rootState = i ? itemState(i, work.running) : null
  /* queued supplements show on their comments; the strip above the composer keeps only rows no comment claims */
  const queued = useMemo(
    () => (i ? queuedComments(i.tl, i.qs, i.q, "补充信息：") : { byComment: new Map<number, never>(), rest: [] }),
    [i?.tl, i?.qs, i?.q],
  )
  /* activity: everything, in order; runs of process lines fold where they are shown */
  const worklogRows = useMemo<WorklogRow[]>(() => {
    if (!i) return []
    const feed = i.tl.map((e, n) => ({ e, n }))
    const days = byDay(foldProcess(feed, (x) => (x.e[0] === "sys" || x.e[0] === "dispatch") && !queued.byComment.has(x.n)))
    const out: WorklogRow[] = []
    days.forEach(([day, gs], dayIndex) => {
      if (day) out.push({ id: `day:${day}:${dayIndex}`, kind: "day", day, first: dayIndex === 0 })
      for (const g of gs) {
        if (Array.isArray(g)) {
          out.push({ id: `run:${g[0].n}`, kind: "run", items: g })
        } else {
          const e = g.e
          const rowId = e[0] === "sess" ? `sess:${e[1]}` : e[0] === "reply" ? `reply:${e[1].s}:${e[1].turn}` : `row:${g.n}`
          out.push({ id: rowId, kind: "entry", item: g })
        }
      }
    })
    return out
  }, [i?.tl, queued.byComment])
  if (!i) return <Page title={t("views:item.title")} back={["#/items", t("views:items.title")]}><Empty title={t("views:item.notFound")} sub={t("views:item.notFoundSub")} /></Page>
  const designReview = i.waits.find((w) => w.subject === "design_implementation")
  const dependents = [...new Set([...i.dependents, ...getItems().filter((x) => x.deps?.includes(i.id)).map((x) => x.id)])].flatMap((id) => getItem(id) ?? [])
  const a = active(i)
  const targetLabel = { auto: t("views:item.targetAuto"), steer: t("views:item.targetSteer"), new: t("views:item.targetNew") }
  const targets = (a && a.st === "run" ? ["auto", "steer", "new"] : ["auto", "new"]) as ("auto" | "steer" | "new")[]
  const qSess = getSession(i.qs ?? "") ?? null
  const qNote = a ? queueNote(qSess) : t("views:item.queueNewSession")
  const activityTab = dtab === "动态"
  const virtualizeWorklog = activityTab && worklogRows.length > WORKLOG_VIRTUALIZE_THRESHOLD
  const actions = <>
    <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("views:item.star")} data-star onClick={() => star(i.id)} className="text-muted-foreground" />}><StarIcon className={cn(i.star && "fill-current text-foreground")} /></TooltipTrigger><TooltipContent>{i.star ? t("views:item.unstar") : t("views:item.star")}</TooltipContent></Tooltip>
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        <DropdownMenuItem onClick={() => { setG(i.goals[i.goals.length - 1]); setEditing(true) }}>{t("views:item.editGoal")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => setTitleEdit(true)}>{t("views:item.rename")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => openSheet("graph", { k: id })}>{t("views:item.viewGraph")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => setDebugOpen(true)} data-item-debug>{t("views:debug.open")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => openSheet("handoff", { item: i.id })} disabled={closedSt(i.st) || !i.ss.length} data-item-handoff><ArrowRightLeftIcon />{t("views:item.handoff")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => { const k = i.ss[i.ss.length - 1]; openSheet("fork", { k }) }} disabled={!i.ss.length}>{t("views:item.fork")}</DropdownMenuItem>
        <DropdownMenuItem onClick={async () => { const res = await copyText(location.href); if (!res.ok) { console.warn("item link copy failed", res.error); toast(t("common:action.copyFailed")); return } toast(t("views:item.linkCopied")) }}><LinkIcon />{t("views:item.copyLink")}</DropdownMenuItem>
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
  const [pl, PI, pa] = over ? [t("views:item.reopen"), RotateCcwIcon, () => startItemCycle(i.id)] as const
    : i.st === "pause" ? [t("views:item.resume"), PlayIcon, () => actOnItem(i.id, "resume")] as const
    : [t("views:item.pause"), PauseIcon, () => actOnItem(i.id, "pause")] as const
  const quick = (
    <div className="flex items-center gap-1.5" data-quick-actions>
      {!over && i.ss.length > 0 && <Button variant="ghost" size="sm" className="text-muted-foreground max-sm:w-8 max-sm:px-0" aria-label={t("views:item.handoff")} title={t("views:item.handoff")} onClick={() => openSheet("handoff", { item: i.id })} data-quick="handoff"><ArrowRightLeftIcon /><span className="max-sm:sr-only">{t("views:item.handoff")}</span></Button>}
      <Button variant="ghost" size="sm" className="text-muted-foreground max-sm:w-8 max-sm:px-0" aria-label={pl} title={pl} disabled={i.st === "prop"} onClick={pa} data-quick="pause"><PI /><span className="max-sm:sr-only">{pl}</span></Button>
      <Button variant="outline" size="sm" className="max-sm:w-8 max-sm:px-0" aria-label={t("views:item.markDone")} title={t("views:item.markDone")} disabled={over || !st.owner || doneBusy} onClick={() => { setDoneBusy(true); void markDone(i.id).finally(() => setDoneBusy(false)) }} data-quick="done"><CircleCheckIcon /><span className="max-sm:sr-only">{t("views:item.markDone")}</span></Button>
    </div>
  )
  const state = rootState!
  /* the now card with nothing under its headline (finished, say): one centred line in a smaller card */
  const compact = !i.waits.length && isClosedItemState(state)
  const centered = compact && !state.detail && state.kind !== "idle" && !i.rel && !i.dependencyWait
  const sublabel = itemStateSublabel(state)
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
  const actorCtx = { me: st.me, members: st.members, bots: st.bots, systemName: t("common:actor.kind.system") }
  const actorOf = (e: any[]) => resolveActor(
    typeof e[9] === "string" && e[9] ? e[9] : e[0] === "me" ? "me" : "system",
    actorCtx,
  )
  const timelineDisambig = disambiguateActors([
    ...st.members.map((m) => { const a = resolveActor(m.user_id, actorCtx); return { key: a.id ?? m.user_id, name: a.name, subtitle: a.subtitle, fallbackId: m.user_id } }),
    ...i.tl.filter((e) => e[0] === "me" || e[0] === "sys" || e[0] === "dispatch").map((e) => {
      const a = actorOf(e)
      return { key: a.id ?? a.name, name: a.name, subtitle: a.subtitle, fallbackId: a.id }
    }),
  ])
  const shownName = (a: ReturnType<typeof actorOf>) => formatActorName(a.name, timelineDisambig.get(a.id ?? a.name), (n, extra) => t("common:actor.disambiguated", { name: n, extra }))
  const entry = ({ e, n }: { e: any[]; n: number }) => {
    if (e[0] === "sess") return <SessionBlock key={e[1]} k={e[1]} defaultOpen={false} replies={replies} />
    if (e[0] === "reply") return <ReplyBlock key={"reply:" + e[1].s + ":" + e[1].turn} r={e[1]} />
    const actor = actorOf(e)
    const extra = timelineDisambig.get(actor.id ?? actor.name)
    const q = e[0] === "me" && queued.byComment.get(n)
    if (q) return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><ActorAvatar actor={actor} disambiguator={extra} size={20} className="mt-0.5" />
        <QueuedComment sid={i.qs!} q={q} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} at={e[2]} body={(t) => <div className="type-body"><Markdown text={t} /></div>} /></div>
    )
    /* notes sent from an output show what they mark and open the output there */
    const notes = e[0] === "me" && e[7] != null ? i.anns.filter((x) => x.timeline_id === e[7]) : []
    if (e[0] === "me") return (
      <div key={n} className="flex items-start gap-2 py-1.5" data-comment><ActorAvatar actor={actor} disambiguator={extra} size={20} className="mt-0.5" />
        <div className="min-w-0 flex-1">{notes.length ? <div data-comment-notes><div className="type-body">{t("views:item.notes", { count: notes.length })}</div><AnnRefs item={i.id} anns={notes} outs={i.outs} open={(o, ann) => setPreview({ o, focus: ann })} /></div>
          : (e[1] !== "（附件）" || !e[5]?.length) && <div className="type-body"><Markdown text={e[1]} /></div>}<Files files={e[5]} className="my-1" /><div className="type-footnote tabular-nums text-tertiary"><span className="font-medium text-muted-foreground" data-timeline-actor>{shownName(actor)}</span> · {when(e)}{e[3] && getSession(e[3]) ? ` · ${t("views:item.deliveredTo", { n: getSession(e[3])!.n })}` : ""}{e[6] === "thread" && <a href="#/" className="ml-1.5 rounded-md bg-muted px-1.5 hover:text-foreground" data-from-thread>{t("views:item.fromThread")}</a>}</div></div></div>
    )
    const sysText = e[0] === "dispatch" ? formatDispatchTimeline(String(e[1] ?? ""), t) : String(e[1] ?? "")
    return (
      <div key={n} className="flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-timeline-sys>
        <ActorAvatar actor={actor} disambiguator={extra} size={18} />
        {actor.kind !== "system" && <span className="shrink-0 font-medium text-foreground/80" data-timeline-actor>{shownName(actor)}</span>}
        <span className="min-w-0 truncate" title={sysText}>{clip(sysText, 240)}</span>
        <span className="shrink-0 tabular-nums text-tertiary">{when(e)}</span>
      </div>
    )
  }
  const renderWorklogRow = (row: WorklogRow) => {
    if (row.kind === "day") return <h3 className={cn("mb-1 type-label text-tertiary", !row.first && "mt-5")} data-day>{row.day}</h3>
    if (row.kind === "run") return <ProcessRun count={row.items.length} last={row.items[row.items.length - 1].e} renderChildren={() => row.items.map(entry)} />
    return entry(row.item)
  }
  return (
    <Page scrollRef={scroll} virtual={virtualizeWorklog} small title={titleEdit ? <Input autoFocus defaultValue={i.t} className="h-9 rounded-lg type-title" data-title-input
        onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== i.t) patchItem(i.id, { t: v }); setTitleEdit(false) }} onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); if (e.key === "Escape") setTitleEdit(false) }} />
      : <span className="cursor-text" onDoubleClick={() => setTitleEdit(true)} data-title>{i.t}</span>} back={["#/items", t("views:items.title")]} actions={actions} toolbar={quick} toolbarTop max={1080}
      footer={<div className="w-full shrink-0 border-t bg-background/85 backdrop-blur-xl"><div className="mx-auto w-full px-4 pb-[76px] pt-3 sm:px-8 md:pb-4" style={{ maxWidth: 1040 + 64 }}><div className="lg:pr-[340px]">
        <div className="mb-2 type-footnote font-medium text-foreground/80" data-item-feedback-label>{t("views:item.messageBot")}</div>
        <Composer draftKey={draftKey(st.workspace?.id, "item", i.id)} handle={itemComposer} placeholder={t("views:item.composer", { id: i.id })} testId="item" onSend={(text, files) => supp(i.id, text, target, files)}
          queue={i.qs && <QueueStrip sid={i.qs} rows={queued.rest} prefix="补充信息：" composer={itemComposer} note={qNote} starting={qSess?.starting} numbered={false} />}
          left={<Segmented size="sm" value={targets.includes(target) ? target : "auto"} onChange={setTarget} options={targets.map((x) => [x, targetLabel[x]] as const)} />} />
      </div></div></div>}
      sub={<Meta><button type="button" className="inline-flex items-center gap-1 font-mono tabular-nums hover:text-foreground" data-copy-id onClick={async () => { const res = await copyText(i.id); if (!res.ok) { console.warn("item id copy failed", res.error); toast(t("common:action.copyFailed")); return } toast(t("views:item.idCopied", { id: i.id })) }}>{i.id}<CopyIcon className="size-3" /></button><StatePill state={state} /><ProjectChip id={i.project} /><span>{t("views:item.updated", { time: i.up })}</span></Meta>}>
      {debugOpen && <ItemDebugDialog item={i.id} onClose={() => setDebugOpen(false)} />}
      <AlertDialog open={confirmDrop} onOpenChange={setConfirmDrop}>
        <AlertDialogContent size="sm" data-drop-popover>
          <AlertDialogHeader><AlertDialogTitle>{t("views:item.dropTitle", { id: i.id })}</AlertDialogTitle><AlertDialogDescription>{t("views:item.dropDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { actOnItem(i.id, "drop"); setConfirmDrop(false) }} data-drop-confirm>{t("views:item.drop")}</AlertDialogAction></AlertDialogFooter>
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
                  {sublabel && <span className="type-footnote text-muted-foreground" data-state-sublabel={sublabel}>{t(`views:itemState.${sublabel}`)}</span>}
                </div>
                {state.detail && !(i.waits.length > 0 && state.detail === i.waits[0].q) && <p className="mt-1 line-clamp-3 break-words type-callout text-muted-foreground">{clip(state.detail, 360)}</p>}
                {state.kind === "idle" && <p className="mt-1 type-callout text-muted-foreground">{t("views:itemState.idleHint")}</p>}
              </div>
            </div>
            {(i.dispatch || i.placement) && <DispatchDetails dispatch={i.dispatch} placement={i.placement} unassigned={!i.b} />}
            {(() => {
              const seen = new Set<string>()
              const waits = i.waits.filter((w) => {
                if (seen.has(w.decision)) return false
                seen.add(w.decision)
                return true
              })
              const noExec = !i.b ? decideCards(openDecs).filter((c) => {
                if (seen.has(c.w.decision)) return false
                if (c.w.subject !== "no_executor") return false
                const dv = decisionView(c.w.decision)
                if (!(dv?.question.kind === "key" && dv.question.template.code === "no_executor" && dv.question.template.items.some((it) => it.id === i.id))) return false
                seen.add(c.w.decision)
                return true
              }) : []
              const hasCards = waits.length > 0 || noExec.length > 0
              return hasCards ? <div className="mt-5 flex flex-col gap-3">
                {waits.map((w) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}
                {noExec.map((c) => <DecisionCard key={c.w.decision} i={c.i} w={c.w} showItem={false} />)}
              </div> : null
            })()}
            {i.rel && <RelNote rel={i.rel} box={i.rel.tone !== "ok"} className="mt-3" onClose={i.rel.tone === "fail" ? () => closeRel(i) : undefined} />}
            {i.dependencyWait && !!i.deps?.length && <div className="mt-4"><DependencyWaitPanel key={`${i.id}:${i.dependencyWait.cycle}`} item={i.id} wait={i.dependencyWait} /></div>}
          </section>
          {/* the conversation: what people said and what came back, under day headings; runs of process lines fold to one line */}
          <section className="flex flex-col gap-3" data-item-blockers={work.blockers.map((b) => b.kind).join(" ")} data-item-closing-waits={work.waiting.join(" ")}>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Segmented size="sm" value={dtab} onChange={(v) => { dtab = v; r() }} options={[["动态", t("views:item.tabActivity")], ["产出", t("views:item.tabOutputs")]] as const} />
            </div>
            {activityTab && (
              <div key="activity" className="anim-in flex flex-col" data-worklog>
                {virtualizeWorklog
                  ? <VirtualList rows={worklogRows} scroll={scroll} render={renderWorklogRow} estimate={estimateWorklogRow} />
                  : worklogRows.map((row) => <Fragment key={row.id}>{renderWorklogRow(row)}</Fragment>)}
              </div>
            )}
            {dtab === "产出" && <div key="outputs" className="anim-in"><OutList outs={i.outs} anns={i.anns} open={(o) => setPreview({ o })} /></div>}
          </section>
        </div>
        <aside className="flex min-w-0 flex-col gap-8" data-item-side>
          {(i.plan || i.rounds.length > 0) && <section data-progress>
            <h3 className="mb-2 type-label text-tertiary">{t("views:itemState.progress")}</h3>
            <Plan i={i} />
          </section>}
          <section data-details>
            <h3 className="mb-2 type-label text-tertiary">{t("views:itemState.details")}</h3>
            <dl className="grid grid-cols-[minmax(64px,max-content)_minmax(0,1fr)] items-start gap-x-3 gap-y-2.5 [&_[data-select]]:-ml-2">
            <Prop label={t("views:item.propBot")}><Select quiet testId="bot" value={i.b} placeholder={!i.b ? t("manage:dispatch.chooseBot") : undefined} onChange={(k) => reassign(i.id, k)} options={Object.keys(st.bots).filter((k) => st.bots[k].on || k === i.b).map((k) => ({ v: k, l: st.bots[k].main ? <span><BotName k={k} /> · {t("manage:dispatch.coordinator")}</span> : <BotName k={k} /> }))} /></Prop>
            <div className="col-span-2 flex min-w-0 flex-col gap-1.5">
              <dt className="type-callout text-muted-foreground">{t("views:item.propSessions")}</dt>
              <dd className="flex min-w-0 flex-col gap-2">
                {i.ss.length ? i.ss.map((k) => {
                  const s = getSession(k)
                  return (
                    <div key={k} className="flex min-w-0 flex-col gap-1 rounded-lg border border-border/60 bg-background/60 px-2.5 py-2" data-item-session={k}>
                      <a href={"#/sessions/" + k} className="flex min-w-0 flex-col gap-1 hover:opacity-85">
                        <span className="flex min-w-0 items-center gap-1.5">
                          <span className="shrink-0 type-footnote tabular-nums text-tertiary">#{s?.n ?? "—"}</span>
                          {s?.b && <ActorAvatar actorId={s.b} size={16} />}
                          <span className="min-w-0 flex-1 truncate type-callout font-medium text-foreground">{s?.b ? botName(s.b) : t("core:unknownBot")}</span>
                          {s && <Status s={s.st} sess className="shrink-0" />}
                        </span>
                        {s && (
                          <span className="flex min-w-0 items-center gap-1.5 type-footnote text-muted-foreground">
                            <HarnessMark harness={s.harness ?? s.h} title={s.h} className="size-3.5 shrink-0" />
                            <span className="min-w-0 truncate">{hm(s)}</span>
                          </span>
                        )}
                      </a>
                      <details className="type-footnote text-muted-foreground" data-item-session-details>
                        <summary className="cursor-pointer select-none text-tertiary hover:text-foreground">{t("views:item.sessionTechDetails")}</summary>
                        <div className="mt-1 flex flex-col gap-1 border-l border-border/60 pl-2">
                          <SessionPermission id={k} />
                          {s?.placement && <PlacementWhy value={s.placement} />}
                          <NativeIdLine k={k} label="ID" />
                        </div>
                      </details>
                    </div>
                  )
                }) : <span className="type-callout text-muted-foreground">{t("common:state.empty")}</span>}
              </dd>
            </div>
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
            {/* Q1: off unless the owner turns it on; others see it, switched by nobody but the owner */}
            <Prop label={t("views:item.propDesignGate")}><label className="flex items-center gap-2 pt-1" data-design-gate><Switch size="sm" checked={!!i.designGate} disabled={!st.owner} onCheckedChange={(v) => setDesignGate(i.id, v)} aria-label={t("views:item.propDesignGate")} /></label>
              <div className="type-footnote text-muted-foreground">{t(st.owner ? "views:item.designGateHint" : "views:item.designGateOwnerOnly")}</div></Prop>
            <Prop label={t("views:item.propDeps")}>
              <div className="flex flex-col gap-1 pt-1" data-deps>
                {(i.deps || []).filter(getItem).map((d) => { const x = it(d); return (
                  <div key={d} className="group/dep flex items-center gap-2"><StatusDot s={x.st} /><a href={"#/items/" + d} className="flex min-w-0 flex-1 items-baseline gap-1.5 hover:underline"><ItemNo id={d} /><span className="truncate">{x.t}</span></a>
                    <button type="button" aria-label={t("views:item.removeDep", { id: d })} onClick={() => rmDep(i.id, d)} className="text-tertiary opacity-0 transition-opacity hover:text-foreground group-hover/dep:opacity-100 [@media(hover:none)]:opacity-100"><XIcon className="size-3.5" /></button></div>) })}
                <DropdownMenu>
                  <DropdownMenuTrigger render={<button type="button" className="self-start type-footnote text-muted-foreground hover:text-foreground" data-add-dep />}>{(i.deps || []).length ? t("views:item.addMore") : t("views:item.addNone")}</DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-72 min-w-64">
                    {getItems().filter((x) => x.id !== i.id && !(i.deps || []).includes(x.id) && x.st !== "drop").map((x) => <DropdownMenuItem key={x.id} onClick={() => addDep(i.id, x.id)} data-dep-opt={x.id}><ItemNo id={x.id} /><span className="truncate">{x.t}</span></DropdownMenuItem>)}
                  </DropdownMenuContent>
                </DropdownMenu>
                {i.st === "block" && <span className="type-footnote text-muted-foreground">{t("views:item.depsLeft", { count: depsOpen(i).length })}</span>}
              </div>
            </Prop>
            {dependents.length > 0 && <Prop label={t("views:item.propDependents")}><div className="flex flex-col gap-1 pt-1">{dependents.map((x) => <a key={x.id} href={"#/items/" + x.id} className="flex items-center gap-2 hover:underline"><StatusDot s={x.st} /><span className="flex min-w-0 items-baseline gap-1.5"><ItemNo id={x.id} /><span className="truncate">{x.t}</span></span></a>)}</div></Prop>}
            {i.cwd && <Prop label={t("views:item.propCwd")}><span className="block break-all pt-1 type-code text-muted-foreground" data-cwd>{homeTilde(i.cwd)}</span></Prop>}
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

/* ═════════ 设置 / Bot ═════════ */
export const PLUGIN_ICON: Record<string, any> = { mail: MailIcon, calendar: CalendarIcon, message: MessageSquareIcon, git: GitPullRequestIcon, shield: LockIcon, list: ListIcon, zap: ZapIcon }
/** 我: the user's own contact (称呼、时区、语言) */
function MeRow() {
  const t = useT()
  return <Panel><Row icon={<ActorAvatar actorId="me" size={32} tooltip={false} />} title={st.me?.name || t("views:more.me")} sub={t("views:more.meSub")} onClick={() => go("#/contacts/me")} data-more-me><Chevron /></Row></Panel>
}
export function MoreView() {
  useShellStore()
  const t = useT()
  const groups = [
    { key: "primary", name: "", rows: [["projects", t("views:more.projects"), t("views:more.projectsSub", { name: st.workspace?.name }), FolderIcon], ["publications", t("views:more.publications"), t("views:more.publicationsSub"), UploadIcon]] },
    { key: "messages", name: t("core:navGroup.messages"), rows: [["feed", t("views:more.feed"), t("views:more.feedSub"), NewspaperIcon], ["channels", t("views:more.channels"), t("views:more.channelsSub"), MessagesSquareIcon], ["mail", t("views:more.mail"), t("views:more.mailSub"), MailIcon], ["contacts", t("views:more.contacts"), t("views:more.contactsSub"), ContactRoundIcon]] },
    { key: "library", name: t("core:navGroup.library"), rows: [["knowledge", t("views:more.knowledge"), t("views:more.knowledgeSub"), BookOpenIcon], ["integrations", t("views:more.integrations"), t("views:more.integrationsSub"), PlugIcon]] },
    { key: "automate", name: t("core:navGroup.automate"), rows: [["bots", t("views:more.bots"), t("views:more.botsSub"), BotIcon], ["sessions", t("views:more.sessions"), t("views:more.sessionsSub"), SquareTerminalIcon], ["auto", t("views:more.auto"), t("views:more.autoSub"), WorkflowIcon], ["capabilities", t("views:more.capabilities"), t("views:more.capabilitiesSub"), BlocksIcon], ["signals", t("views:more.signals"), t("views:more.signalsSub"), RadioIcon]] },
    { key: "more", name: t("core:navGroup.more"), rows: [/* the devices page is the first row of the Hub section below */["stats", t("views:more.stats"), t("views:more.statsSub"), ChartNoAxesColumnIcon], ["accounts", t("views:more.accounts"), t("views:more.accountsSub"), KeyRoundIcon], ["vault", t("views:more.vault"), t("views:more.vaultSub"), LockIcon], ["settings", t("views:more.settings"), t("views:more.settingsSub"), SettingsIcon]] },
  ] as const
  return (
    <Page title={t("views:more.title")} max={640}>
      <div className="flex flex-col gap-8">
        <MeRow />
        <Panel><Row icon={<ActorAvatar actorId="steve" size={44} />} className="py-3" title={<span className="type-headline">{st.bots.steve?.n ?? "Steve"}</span>} sub={t("views:more.steveSub")} onClick={() => go("#/bots/steve")}><Chevron /></Row></Panel>
        {groups.map((group) => <Section key={group.key} label={group.name} data-more-group={group.key}><Panel>{group.rows.map(([h, title, s, I]) => (
          <Row key={h} icon={<Tile I={I} />} title={title} sub={s} onClick={() => go("#/" + h)} data-more-nav={h}><Chevron /></Row>
        ))}</Panel></Section>)}
        <HubMore />
        <Panel><GitHubMoreRow /></Panel>
        <Section label={t("core:approval.title")}><PermissionPolicyPanel /></Section>
        <Panel><Row title={t("views:more.appearance")}><ThemeSwitch labels /></Row></Panel>
      </div>
    </Page>
  )
}

/* ═════════ 视觉规范: the living specimen lives in design-view.tsx ═════════ */
export { DesignView } from "@/design-view"

export function ImportDialog() {
  useShellStore()
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
        <section key={h} className="mb-4"><SectionLabel>{t("views:import.onHub", { harness: runtimeName(h) ?? h })}</SectionLabel>
          <Group>{left.filter((x) => x.harness === h).map((x) => (
            <label key={x.key} className="flex cursor-pointer items-center gap-3 px-4 py-2.5 hover:bg-accent" data-import-opt={x.key}>
              <Checkbox checked={pick.includes(x.key)} onCheckedChange={(v) => setPick(v ? [...pick, x.key] : pick.filter((k) => k !== x.key))} />
              <span className="min-w-0 flex-1"><span className="block truncate type-body">{x.title}</span><span className="block truncate type-footnote text-muted-foreground"><span className="font-mono">{homeTilde(x.cwd)}</span> · {num(Math.max(1, Math.round(x.size / 1024)), { useGrouping: false })} KB</span></span>
              <span className="type-footnote text-muted-foreground">{tm(x.when)}</span>
            </label>))}</Group>
        </section>)) : <Empty title={st.localSessions.length ? t("views:import.allDone") : t("views:import.reading")} />}
      </div>
    </Modal>
  )
}
