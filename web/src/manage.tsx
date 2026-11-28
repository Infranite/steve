import { getItems, nextItemNumber, useRuntimes, useShellStore, useTagUses, useBotMemories } from "@/store"
import { useViewKinds } from "@/data/sync"
/* Bot 菜单与设置：Bot 是独立菜单；设置是分类页。所有改动都写到 Hub，也都可以直接告诉 Steve。 */
import { itemDisplayStatus } from "@/store"
import { useT } from "@/i18n"
import { Trans } from "react-i18next"
import { WorkspaceDisk } from "@/disk-settings"
import { HubData } from "@/backup-settings"
import { OutboxDead } from "@/outbox-settings"
import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, LayoutTemplateIcon, FilePlusIcon, MoreHorizontalIcon, CopyIcon, TrashIcon, SparklesIcon, SlidersHorizontalIcon, ListChecksIcon, TagIcon, BellIcon, SunMoonIcon, DatabaseIcon, InfoIcon, PencilIcon, CheckIcon, LayersIcon, FolderIcon, BotIcon, ListTodoIcon, ClockIcon, EyeIcon, HandIcon, ShieldOffIcon, ShieldAlertIcon, UserCheckIcon, CheckCheckIcon, ArrowUpIcon, ArrowDownIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Checkbox } from "@/components/ui/checkbox"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Robot, Face, AVATAR_COLORS } from "@/robot"
import { BOT_ACCESSORIES, type BotAccessory } from "@/steve-mascot"
import { Card as ACard, CardHeader } from "@/components/affect/card-clean"
import { Badge } from "@/components/affect/badge-clean"
import { MemoryPanel, type Memory } from "@/components/affect/memory-panel"
import { AutonomyChoice } from "@/components/affect/agent-permissions"
import { PermissionPolicyPanel } from "@/approval"
import {
  st, botName, formatBotName, ownsBot, PRO, ACC, PERM, permModes, runtimeName, defaultHarness, saveBot, makeBot, draftBot, deleteBot, removeBotFromWorkspace, dupBot, addMem, editMem, delMem, setBotWorkspaces, tag, tagLabel, tagDesc, tagUses, TAG_CATS, KIND_CAT, DOMAIN_CATS, isKind, kindTags, makeTag, saveTag, deleteTag, mergeTag, setTags, it,
  setSetting, toast, emit, closedSt, type ExecutionSurface,
} from "@/store"
import { api } from "@/api"
import { go, Page, KindTag, Row, Segmented, Meta, Empty, openSheet, closeSheet, ThemeSwitch, LanguageSwitch, useRoute, Panel, Card, Section, Lead, Pill, Tile, Chevron, Loading, LIFT, Blank, Strip, EASE } from "@/ui"
import { ActorAvatar } from "@/components/actor-avatar"
import { PersonasSection } from "@/channels/persona"
import { OwnerChip } from "@/people/owner-chip"
import { Select } from "@/view-shared"
import { showTag } from "@/items-list"
import { AskSteve } from "@/config-ui"
import { AppearanceSection } from "@/appearance-settings"
import { VAULT_CAT, VaultSettingsPane } from "@/vault/settings"
import { BotCaps } from "@/caps/bot"
import { STATS_CAT, StatsSettingsPane } from "@/stats/prices"
import { AUDIT_CAT, AuditSettingsPane } from "@/audit-settings"
import { MODELS_CAT, ModelsSettingsPane, SteveJudges } from "@/models-settings"
import { PLUGINS_CAT, PluginsSettingsPane } from "@/plugins"
import { ModelPicker } from "@/model-picker"
import { EffortPicker, effortShortLabel, normalizeEffort } from "@/effort-picker"
import { SetSelect, SetSwitch } from "@/settings-controls"
import { TextField } from "@/components/ui/input"
import { TextArea } from "@/components/affect/text-area-clean"
import { AboutVersions } from "@/version"
import { NotificationSettingsPane } from "@/notification-settings"
import { currentHub } from "@/hub"
import { flowApi, errText } from "@/api/flow"
import { useHubQuery } from "@/data/use-hub-query"
import { when } from "@/auto/common"
import { ExecutionNodes, TeamsSection, useBotTeams } from "@/bot-dispatch"
import { BotOverview, BotPrompt, RunsOn } from "@/bot-page"
import { WORKSPACE_SETTING_DEFAULTS, WORKSPACE_SETTING_VALUES } from "@/gen"
import { HarnessMark } from "@/components/harness-mark"
import { RuntimeAvailability, RuntimePermissionNotice, RuntimeSelect, RuntimeSetupLink } from "@/runtime-access"
import { canConfigurePermission } from "@/runtime-status"

/* ═════════ Bot 列表 ═════════ */
let botFilter: "全部" | "启用" | "停用" = "全部"
type T = ReturnType<typeof useT>
const PRO_KEYS = ["low", "medium", "high"] as const
/** a proactiveness level (0–2) as shown */
const proLabel = (t: T, i: number) => (PRO_KEYS[i] ? t(`manage:pro.${PRO_KEYS[i]}`) : PRO[i] ?? "")
const ACC_KEYS = ["inherit", "manual", "auto"] as const
/** an accept mode by backend key as shown; unknown keys fall back to the store's label */
const accLabel = (t: T, k: string) => ((ACC_KEYS as readonly string[]).includes(k) ? t(`manage:acc.${k as (typeof ACC_KEYS)[number]}`) : ACC[k] ?? k)
const PERM_KEYS = ["plan", "ask", "accept_edits", "auto", "full_access"] as const
const permLabel = (t: T, k: string) => ((PERM_KEYS as readonly string[]).includes(k) ? t(`manage:perm.${k as (typeof PERM_KEYS)[number]}`) : PERM[k] ?? k)
/** a permission mode in a word or two (the tile's name); its sentence is permLabel */
const permName = (t: T, k: string) => ((PERM_KEYS as readonly string[]).includes(k) ? t(`manage:form.permName.${k as (typeof PERM_KEYS)[number]}`) : PERM[k] ?? k)
const PERM_ICON: Record<string, ReactNode> = { plan: <EyeIcon />, ask: <HandIcon />, accept_edits: <PencilIcon />, auto: <SparklesIcon />, full_access: <ShieldOffIcon /> }
const ACC_ICON: Record<string, ReactNode> = { inherit: <SlidersHorizontalIcon />, manual: <UserCheckIcon />, auto: <CheckCheckIcon /> }
/* tag categories are stored as Chinese words (store TAG_CATS); only the label is translated */
const TAG_CAT_KEYS: Record<string, "all" | "project" | "kind" | "delivery" | "domain" | "client" | "topic"> = { 全部: "all", 项目: "project", 类型: "kind", 交付: "delivery", 领域: "domain", 客户: "client", 主题: "topic" }
const catLabel = (t: T, c: string) => (TAG_CAT_KEYS[c] ? t(`manage:tagCat.${TAG_CAT_KEYS[c]}`) : c)

export function BotsView() {
  useShellStore()
  useViewKinds(["item", "session", "decision"])
  const t = useT()
  const teams = useBotTeams()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const ks = Object.keys(st.bots).filter((k) => !st.bots[k].main && (botFilter === "全部" || (botFilter === "启用") === st.bots[k].on))
  const S = st.bots.steve
  const runOf = (k: string) => getItems().filter((i) => i.b === k && ["run", "wait", "block"].includes(itemDisplayStatus(i)) && !i.arch).length
  const others = st.allBots.filter((b) => !st.bots[b.id])
  return (
    <Page title="Bot" sub={t("manage:list.sub")} max={720}>
      <div className="flex flex-col gap-8">
      {S && <Section label={t("manage:list.main")}>
        <BotCard k="steve" teams={teams} size={44} title={S.n} sub={S.role} meta={[S.h, S.m, t("manage:bot.running", { count: runOf("steve") })]} action={<Chevron />} />
      </Section>}
      <TeamsSection key={st.workspace?.id} load={teams} />
      <Section label={<><span className="hidden sm:inline">{t("manage:list.others", { ws: st.workspace?.name, n: Object.keys(st.bots).length - 1 })}</span><span className="sm:hidden">{t("manage:list.othersShort", { n: Object.keys(st.bots).length - 1 })}</span></>} note={<Trans t={t} i18nKey="manage:list.hint" components={{ code: <span className="font-mono" /> }} />}
        right={<Button size="xs" variant="outline" onClick={() => openSheet("newBot")} data-new-bot><PlusIcon />{t("manage:list.newBot")}</Button>}>
        <div className="mb-3 flex justify-end"><Segmented size="sm" value={botFilter} onChange={(v) => { botFilter = v; r() }} options={[["全部", t("manage:list.filterAll")], ["启用", t("manage:list.filterOn")], ["停用", t("manage:list.filterOff")]] as const} /></div>
        {ks.length ? <ul className="grid gap-3 sm:grid-cols-2" data-bot-cards>{ks.map((k) => { const b = st.bots[k]; return (
          <BotCard key={k} as="li" k={k} teams={teams} off={!b.on} title={formatBotName(b.n, b.main)} sub={b.role}
            meta={[b.h, b.m, t("manage:bot.running", { count: runOf(k) }), b.project && t("manage:list.defaultProject", { name: b.ws }), b.workspaces.length > 1 && t("manage:list.inWorkspaces", { count: b.workspaces.length }), !b.on && t("manage:bot.disabled")]}
            action={<><SetSwitch testId={"bot-on-" + k} checked={b.on} onChange={(v) => void saveBot(k, { on: v }, v ? t("manage:list.enabledToast", { name: b.n }) : t("manage:list.disabledToast", { name: b.n }))} label={t("manage:bot.enableNamed", { name: b.n })} /><BotMenu k={k} /></>} />) })}</ul>
          : <Empty title={Object.keys(st.bots).length > 1 ? t("manage:list.noMatch") : t("manage:list.onlySteve")} sub={t("manage:list.emptySub")} />}
      </Section>
      {others.length > 0 && <Section label={t("manage:list.elsewhere", { n: others.length })}>
        <Panel>{others.map((b) => <Row key={b.id} title={formatBotName(b.name)} sub={t("manage:list.inWorkspaces", { count: b.workspaces.length })}>{ownsBot(b.owner) && <Button size="xs" variant="outline" onClick={() => setBotWorkspaces(b.id, [...b.workspaces, st.workspace!.id])}><PlusIcon />{t("manage:list.joinHere")}</Button>}</Row>)}</Panel></Section>}
      </div>
    </Page>
  )
}
/** a Bot as affectUI's Card: its avatar, its name as the button that opens it (the whole card is the target), its role,
 *  and what it runs on underneath; the switch and the menu stay their own buttons above the card's target */
function BotCard({ k, teams, title, sub, meta, action, off, size = 36, as }: { k: string; teams: ReturnType<typeof useBotTeams>; title: ReactNode; sub: string; meta: (string | false | null | undefined)[]; action: ReactNode; off?: boolean; size?: number; as?: "li" }) {
  const t = useT()
  const isMain = !!st.bots[k]?.main
  const joined = teams.data?.teams.filter(team => team.members.some(member => member.bot === k)) ?? []
  return (
    <ACard as={as} interactive className="anim-in" data-bot-card={k} data-off={off || undefined}>
      <CardHeader icon={<ActorAvatar actorId={k} size={size} className={cn("transition-[opacity,filter]", EASE, off && "opacity-40 grayscale")} />} action={action}
        title={<button type="button" onClick={() => go("#/bots/" + k)} data-bot-row={k}>{title}</button>}
        description={<span className="line-clamp-2">{sub || t("manage:detail.noRole")}</span>} />
      <Meta>{meta.filter(Boolean).map((x, i) => <span key={i}>{x}</span>)}</Meta>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5 type-footnote text-muted-foreground" data-bot-teams>
        {isMain
          ? <Badge tone="info">{t("manage:dispatch.coordinatorAlwaysOn")}</Badge>
          : <>
              <span>{t("manage:list.joinedTeams")}</span>
              {teams.error ? <span>{t("manage:list.teamsLoadFailed")}</span> : !teams.data ? <span>{t("manage:list.teamsLoading")}</span>
                : joined.length ? joined.map(team => <Badge key={team.id} title={team.name}>{team.name}</Badge>) : <span>{t("manage:list.noTeams")}</span>}
            </>}
      </div>
    </ACard>
  )
}
function BotMenu({ k, align = "end" }: { k: string; align?: "end" | "start" }) {
  const t = useT()
  const b = st.bots[k]
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-bot-menu={k} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align={align} className="min-w-40">
        <DropdownMenuItem onClick={() => { botTab = "设置"; go("#/bots/" + k) }}><PencilIcon />{t("common:action.edit")}</DropdownMenuItem>
        <DropdownMenuItem onClick={async () => { const nk = await dupBot(k); if (nk) { botTab = "设置"; go("#/bots/" + nk) } }} data-dup-bot><CopyIcon />{t("common:action.copy")}</DropdownMenuItem>
        {!b?.main && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => openSheet("deleteBot", { k })} data-del-bot={k}><TrashIcon />{t("common:action.delete")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
export function DeleteBotDialog({ k }: { k: string }) {
  useShellStore()
  useViewKinds(["item"])
  const memories = useBotMemories(k)
  const t = useT()
  const b = st.bots[k]
  const canDeleteGlobally = b ? ownsBot(b.owner) : false
  const multiWorkspace = (b?.workspaces.length ?? 0) > 1
  const [scope, setScope] = useState<"workspace" | "global">(() => (canDeleteGlobally && !multiWorkspace ? "global" : "workspace"))
  const [to, setTo] = useState("steve"), [mem, setMem] = useState<"delete" | "export">("export")
  if (!b) return null
  const effectiveScope: "workspace" | "global" = canDeleteGlobally ? scope : "workspace"
  const mine = getItems().filter((i) => i.b === k && !closedSt(i.st))
  const confirm = () => {
    closeSheet()
    go("#/bots")
    if (effectiveScope === "global") void deleteBot(k, to, mem)
    else void removeBotFromWorkspace(k, to, mem)
  }
  return (
    <AlertDialog open onOpenChange={(o) => !o && closeSheet()}>
      <AlertDialogContent className="sm:max-w-[480px]!">
        <AlertDialogHeader><AlertDialogTitle>{effectiveScope === "global" ? t("manage:del.title", { name: b.n }) : t("manage:del.removeTitle", { name: b.n })}</AlertDialogTitle>
          <AlertDialogDescription>{effectiveScope === "global" ? t("manage:del.desc", { count: mine.length }) : t("manage:del.removeDesc", { count: mine.length })}</AlertDialogDescription></AlertDialogHeader>
        <FieldGroup className="gap-4">
          {canDeleteGlobally && (
            <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.scope")}</FieldLabel>
              <Segmented size="sm" value={effectiveScope} onChange={setScope} options={[["workspace", t("manage:del.scopeWorkspace")], ["global", t("manage:del.scopeGlobal")]] as const} /></Field>
          )}
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.handTo")}</FieldLabel>
            <SetSelect testId="del-to" label={t("manage:del.handTo")} value={to} onChange={setTo} options={Object.keys(st.bots).filter((x) => x !== k && st.bots[x].on).map((x) => ({ v: x, l: botName(x) }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.itsMemories", { count: memories?.length ?? 0 })}</FieldLabel><Segmented size="sm" value={mem} onChange={setMem} options={[["export", t("manage:del.export")], ["delete", t("common:action.delete")]] as const} /></Field>
        </FieldGroup>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" data-del-bot-confirm onClick={confirm}>{effectiveScope === "global" ? t("common:action.delete") : t("manage:del.removeAction")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
/* starter bots: [id, harness, model]; the name, role and rules come from manage:tpl in the UI language */
/* a template runs on the harness a new bot gets (the first that is ready) and its default model */
const TEMPLATES = ["review", "research", "writing", "knowledge"] as const
type TplId = (typeof TEMPLATES)[number]
const tplText = (t: T, id: TplId) => ({ name: t(`manage:tpl.${id}Name`), role: t(`manage:tpl.${id}Role`), rule: t(`manage:tpl.${id}Rule`) })
export function NewBotDialog() {
  useShellStore()
  const t = useT()
  const [mode, setMode] = useState<"tpl" | "desc" | "blank">("tpl"), [tpl, setTpl] = useState<TplId>(TEMPLATES[0]), [desc, setDesc] = useState(""), [busy, setBusy] = useState(false)
  const [harness, setHarness] = useState(defaultHarness), [perm, setPerm] = useState("ask")
  const runtimes = useRuntimes()
  useEffect(() => { if (!harness && runtimes.length) setHarness(defaultHarness()) }, [harness, runtimes])
  const runtime = runtimes.find(r => r.id === harness)
  const ready = !!harness && canConfigurePermission(runtime, perm)
  const create = async () => {
    setBusy(true); let k: string | undefined
    if (mode === "tpl") { const x = tplText(t, tpl); k = await makeBot(x.name, x.role, harness, null, perm); if (k) await saveBot(k, { rule: x.rule }, "") }
    else if (mode === "blank") k = await makeBot(t("manage:newBot.blankName"), "", harness, null, perm)
    else k = await draftBot(desc.trim())
    setBusy(false); if (k) { closeSheet(); botTab = "设置"; go("#/bots/" + k) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-title">{t("manage:list.newBot")}</DialogTitle><DialogDescription className="type-callout">{t("manage:newBot.desc", { ws: st.workspace?.name })}</DialogDescription></DialogHeader>
        <AutonomyChoice testId="new-bot" label={t("manage:list.newBot")} value={mode} onChange={setMode} options={[{ value: "tpl", name: t("manage:newBot.fromTpl"), text: t("manage:newBot.fromTplSub"), icon: <LayoutTemplateIcon /> }, { value: "desc", name: t("manage:newBot.fromDesc"), text: t("manage:newBot.fromDescSub"), icon: <SparklesIcon /> }, { value: "blank", name: t("manage:newBot.blank"), text: t("manage:newBot.blankSub"), icon: <FilePlusIcon /> }]} />
        {mode === "tpl" && <Panel className="anim-in">{TEMPLATES.map((id) => { const x = tplText(t, id); return <Row key={id} title={x.name} sub={<span className="inline-flex min-w-0 flex-wrap items-center gap-x-1.5"><span>{x.role}</span><span aria-hidden>·</span><span className="inline-flex items-center gap-1"><HarnessMark harness={harness} className="size-3.5" />{runtimeName(harness) ?? harness}</span></span>} onClick={() => setTpl(id)} data-tpl={id}>{tpl === id && <CheckIcon className="size-4 text-blue" />}</Row> })}</Panel>}
        {mode === "desc" && <TextArea autoFocus className="anim-in" minRows={4} maxRows={10} aria-label={t("manage:newBot.fromDesc")} placeholder={t("manage:newBot.descPlaceholder")} value={desc} onChange={(e) => setDesc(e.target.value)} data-bot-desc />}
        {mode !== "desc" && <div className="flex flex-col gap-3"><RuntimeSelect testId="new-bot-harness" value={harness} onChange={setHarness} />
          <RuntimeAvailability harness={harness} />
          <RuntimePermissionNotice harness={harness} mode={perm} />
          <SetSelect wide testId="new-bot-perm" label={t("manage:form.permSection")} value={perm} onChange={setPerm} options={permModes(harness).map(v => ({ v, l: permName(t, v) }))} />
        </div>}
        <RuntimeSetupLink />
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={(mode === "desc" ? !desc.trim() : !ready) || busy} onClick={() => void create()} data-testid="bot-go">{busy ? <Spinner className="size-3.5" /> : mode === "desc" && <SparklesIcon />}{mode === "desc" ? t("manage:newBot.draft") : t("manage:newBot.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ═════════ 头像：炭笔涂鸦家族，颜色、头型圆度与配饰 ═════════ */
/* head shapes: [label key under manage:avatar, roundness] */
const SHAPES = [["shapeSquare", 15], ["shapeRounded", 45], ["shapeSoft", 70], ["shapeRound", 95]] as const
type PickerLook = { color: string; round: number; accessory: BotAccessory | null }
function AvatarPicker({ id, look, onChange }: { id: string; look: PickerLook; onChange: (p: Partial<PickerLook>) => void }) {
  const t = useT()
  const accessoryChoices: readonly (BotAccessory | null)[] = [null, ...BOT_ACCESSORIES]
  return (
    <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:gap-6" data-avatar-picker>
      <div className="shrink-0 pt-1.5">
        <Face id={id} look={look} size={88} />
      </div>
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 shrink-0 type-footnote text-muted-foreground">{t("manage:avatar.color")}</span>
          {AVATAR_COLORS.map(([c, nm]) => <Tooltip key={c}><TooltipTrigger render={<button type="button" aria-label={nm} aria-pressed={look.color === c} data-color={c} onClick={() => onChange({ color: c })}
            className={cn("size-6 rounded-full ring-offset-2 ring-offset-background transition-shadow", look.color === c ? "ring-2 ring-foreground/70" : "hover:ring-2 hover:ring-border")} style={{ background: c }} />} /><TooltipContent>{nm}</TooltipContent></Tooltip>)}
          <label className="relative size-6 cursor-pointer overflow-hidden rounded-full ring-1 ring-border" title={t("manage:avatar.customColor")} style={{ background: "conic-gradient(red, yellow, lime, aqua, blue, magenta, red)" }}>
            <input type="color" className="absolute inset-0 cursor-pointer opacity-0" value={look.color} onChange={(e) => onChange({ color: e.target.value })} data-color-custom /></label>
        </div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 shrink-0 type-footnote text-muted-foreground">{t("manage:avatar.shape")}</span>
          <Strip className="min-w-0"><Segmented size="sm" value={SHAPES.reduce<(typeof SHAPES)[number]>((a, b) => (Math.abs(b[1] - look.round) < Math.abs(a[1] - look.round) ? b : a), SHAPES[0])[0]} onChange={(v) => onChange({ round: SHAPES.find((x) => x[0] === v)![1] })} options={SHAPES.map((x) => [x[0], t(`manage:avatar.${x[0]}`)] as const)} /></Strip>
          <input type="range" min={0} max={100} value={look.round} onChange={(e) => onChange({ round: +e.target.value })} className="w-28 accent-foreground" aria-label={t("manage:avatar.roundness")} data-round /></div>
        <div className="flex items-start gap-2"><span className="w-10 shrink-0 pt-2 type-footnote text-muted-foreground">{t("manage:avatar.accessory")}</span>
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5" data-accessory-picker>
            {accessoryChoices.map((acc) => {
              const key = acc ?? "none"
              const selected = (look.accessory ?? null) === acc
              return (
                <button
                  key={key}
                  type="button"
                  aria-pressed={selected}
                  data-accessory={key}
                  onClick={() => onChange({ accessory: acc })}
                  className={cn(
                    "inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 type-footnote transition-colors",
                    selected
                      ? "border-foreground/70 bg-accent font-medium text-foreground"
                      : "border-border/80 bg-background text-muted-foreground hover:border-foreground/30 hover:text-foreground",
                  )}
                >
                  <Face id={id} look={{ color: look.color, round: look.round, accessory: acc }} size={28} />
                  <span>{t(`manage:accessory.${key}`)}</span>
                </button>
              )
            })}
          </div>
        </div>
      </div>
    </div>
  )
}

/* ═════════ Bot 详情 ═════════ */
function F({ label, children, hint, htmlFor }: { label: string; children: ReactNode; hint?: string; htmlFor?: string }) {
  return <Field className="gap-1.5"><FieldLabel htmlFor={htmlFor} className="type-label text-muted-foreground">{label}</FieldLabel>{children}{hint && <FieldDescription className="type-footnote">{hint}</FieldDescription>}</Field>
}
export let botTab: "概览" | "设置" | "对外身份" | "能力" | "系统提示词" | "定时与流程" | "记忆" = "概览"; let lastBot = ""
/** open a bot's 设置 tab */
export function editBot(k: string) { botTab = "设置"; lastBot = k; go("#/bots/" + k) }
const BOT_TABS = [["概览", "tabOverview"], ["设置", "tabSettings"], ["能力", "tabCaps"], ["系统提示词", "tabPrompt"], ["记忆", "tabMemory"], ["对外身份", "tabPersona"], ["定时与流程", "tabSchedule"]] as const
function FallbackSurfacesEditor({ value, onChange }: { value: ExecutionSurface[]; onChange: (next: ExecutionSurface[]) => void }) {
  const t = useT()
  const runtimes = useRuntimes()
  const updateAt = (idx: number, patch: Partial<ExecutionSurface>) =>
    onChange(value.map((s, n) => (n === idx ? { ...s, ...patch } : s)))
  const move = (idx: number, delta: -1 | 1) => {
    const to = idx + delta
    if (to < 0 || to >= value.length) return
    const next = [...value]
    ;[next[idx], next[to]] = [next[to], next[idx]]
    onChange(next)
  }
  const add = () => {
    if (value.length >= 8) return
    const pick = runtimes.find((r) => r.ok)?.id ?? runtimes[0]?.id ?? defaultHarness()
    if (!pick) return
    onChange([...value, { harness: pick, model: null, reasoning_effort: "default" }])
  }
  return (
    <F label={t("manage:form.fallbackSurfaces")} hint={t("manage:form.fallbackSurfacesHint")}>
      <div className="flex flex-col gap-2" data-bot-fallbacks>
        {value.length === 0 && <div className="type-footnote text-muted-foreground">{t("manage:form.fallbackSurfacesEmpty")}</div>}
        {value.map((s, idx) => {
          const eff = normalizeEffort(s.reasoning_effort)
          return (
            <div key={idx} className="flex flex-col gap-2 rounded-lg border border-border/70 p-3" data-bot-fallback-row={idx}>
              <div className="flex items-center justify-between gap-2">
                <span className="type-footnote font-medium text-muted-foreground">#{idx + 1}</span>
                <div className="flex items-center gap-1">
                  <Button type="button" size="icon-xs" variant="ghost" disabled={idx === 0} aria-label={t("manage:form.fallbackSurfaceMoveUp")} onClick={() => move(idx, -1)}><ArrowUpIcon className="size-3.5" /></Button>
                  <Button type="button" size="icon-xs" variant="ghost" disabled={idx === value.length - 1} aria-label={t("manage:form.fallbackSurfaceMoveDown")} onClick={() => move(idx, 1)}><ArrowDownIcon className="size-3.5" /></Button>
                  <Button type="button" size="icon-xs" variant="ghost" aria-label={t("manage:form.fallbackSurfaceRemove")} onClick={() => onChange(value.filter((_, n) => n !== idx))} data-bot-fallback-remove={idx}><TrashIcon className="size-3.5" /></Button>
                </div>
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <RuntimeSelect testId={`bot-fallback-harness-${idx}`} value={s.harness} onChange={(h) => updateAt(idx, { harness: h, model: null })} />
                <ModelPicker field testId={`bot-fallback-model-${idx}`} harness={s.harness} value={s.model ?? ""} allowDefault onChange={(m) => updateAt(idx, { model: m || null })} />
                <EffortPicker wide testId={`bot-fallback-effort-${idx}`} label={t("manage:form.effort")} harness={s.harness} model={s.model} value={eff} onChange={(v) => updateAt(idx, { reasoning_effort: v })} />
              </div>
            </div>
          )
        })}
        {value.length < 8 && (
          <div><Button type="button" size="sm" variant="outline" onClick={add} data-bot-fallback-add><PlusIcon className="size-3.5" />{t("manage:form.fallbackSurfacesAdd")}</Button></div>
        )}
      </div>
    </F>
  )
}
export function BotView({ k }: { k: string }) {
  useShellStore()
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const b = st.bots[k]
  const [form, setForm] = useState<any>(null)
  const [settingsTab, setSettingsTab] = useState<"identity" | "model" | "permissions">("identity")
  useEffect(() => { setSettingsTab("identity") }, [k])
  if (lastBot !== k) { lastBot = k; if (botTab !== "设置") botTab = "概览" }
  const runtimes = useRuntimes(!!b)
  if (!b) return <Page title="Bot" back={["#/bots", "Bot"]}><Empty title={t("manage:detail.missing")} sub={t("manage:detail.missingSub")} /></Page>
  const fm = form && form.k === k ? form : { k, n: b.n, role: b.role, specialties: b.specialties?.join("\n") ?? "", pers: b.pers, pro: PRO[b.pro], rule: b.rule, acc: b.acc, harness: b.harness, model: b.model ?? "", reasoningEffort: normalizeEffort(b.reasoningEffort), fallbackSurfaces: b.fallbackSurfaces ?? [], color: b.color, round: b.round, accessory: b.accessory ?? null, project: b.project ?? "none", permMode: b.permMode, ws: b.workspaces }
  const effort = normalizeEffort(fm.reasoningEffort ?? b.reasoningEffort)
  const runtime = runtimes.find(r => r.id === fm.harness)
  const modeSupported = (fm.permMode === b.permMode && fm.harness === b.harness) || canConfigurePermission(runtime, fm.permMode)
  const specialties = String(fm.specialties).split("\n").map(tag => tag.trim()).filter(Boolean)
  const specialtiesValid = specialties.length <= 12 && specialties.every(tag => tag.length <= 48)
  const set = (p: any) => setForm({ ...fm, reasoningEffort: effort, ...p })
  const tabs = BOT_TABS.map(([v, l]) => [v, t(`manage:detail.${l}`)] as const)
  const displayName = formatBotName(b.n, b.main)
  const saveAll = async () => {
    await saveBot(k, { n: fm.n.trim() || b.n, role: fm.role, specialties, pers: fm.pers, pro: PRO.indexOf(fm.pro), rule: fm.rule, acc: fm.acc, harness: fm.harness, model: fm.model || null, reasoningEffort: effort, fallbackSurfaces: fm.fallbackSurfaces ?? [], color: fm.color, round: fm.round, accessory: fm.accessory ?? null, project: fm.project === "none" ? null : fm.project, permMode: fm.permMode })
    if (!b.main && JSON.stringify([...fm.ws].sort()) !== JSON.stringify([...b.workspaces].sort())) setBotWorkspaces(k, fm.ws)
    setForm(null); botTab = "概览"; r()
  }
  const cancelEdit = () => { setForm(null); botTab = "概览"; r() }
  return (
    <Page back={["#/bots", "Bot"]} max={720} toolbarTop toolbar={<OwnerChip objectType="bot" objectId={k} />}
      lead={<Robot k={k} size={56} className="max-sm:size-12!" />}
      title={<span className="block truncate">{displayName}</span>}
      sub={<div className="flex flex-col gap-1"><div className="text-muted-foreground">{b.role || t("manage:detail.noRole")}</div><RunsOn k={k} className="type-footnote text-muted-foreground" /></div>}
      actions={<>{!b.main && <><span className="hidden type-footnote text-muted-foreground sm:inline">{b.on ? t("manage:bot.enabled") : t("manage:bot.disabled")}</span><SetSwitch testId="bot-on" checked={b.on} onChange={(v) => void saveBot(k, { on: v }, v ? t("manage:bot.enabled") : t("manage:bot.disabled"))} label={t("manage:bot.enable")} /></>}<BotMenu k={k} /></>}>
      <Strip className="mb-8" watch={botTab}><Segmented full className="min-w-max" value={botTab} onChange={(v) => { botTab = v; r() }} options={tabs} /></Strip>
      {botTab === "概览" && <BotOverview k={k} go={(v) => { botTab = v; r() }}
        labels={{ pro: proLabel(t, b.pro), perm: permName(t, b.permMode), acc: accLabel(t, b.acc), effort: b.reasoningEffort && b.reasoningEffort !== "default" ? effortShortLabel(t, normalizeEffort(b.reasoningEffort)) : null, steveTools: 0 }} />}
      {/* the channels page's list, laid into this page's section and panel */}
      {botTab === "对外身份" && <div className="anim-in"><PersonasSection bot={k} panel /></div>}
      {botTab === "定时与流程" && <BotSchedules k={k} />}
      {botTab === "能力" && <BotCaps k={k} permName={permName(t, b.permMode)} permSub={permLabel(t, b.permMode)} onPerm={() => { setSettingsTab("permissions"); botTab = "设置"; r() }} />}
      {botTab === "系统提示词" && <BotPrompt k={k} />}
      {botTab === "记忆" && <BotMemories k={k} />}
      {botTab === "设置" && (
        <FieldGroup className="gap-6 anim-in" data-bot-settings-tab={settingsTab}>
          <div className="sticky top-0 z-10 -mx-1 flex flex-wrap items-center justify-between gap-2 border-b border-border/60 bg-background/90 px-1 py-2 backdrop-blur-md" data-bot-settings-bar>
            <div className="min-w-0" data-bot-settings-tabs>
              <Segmented size="sm" value={settingsTab} onChange={setSettingsTab} options={[
                ["identity", t("manage:form.subTabIdentity")],
                ["model", t("manage:form.subTabModel")],
                ["permissions", t("manage:form.subTabPermissions")],
              ] as const} />
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Button size="sm" variant="ghost" onClick={cancelEdit}>{t("common:action.cancel")}</Button>
              <Button size="sm" data-save-bot disabled={!modeSupported || !specialtiesValid} onClick={() => void saveAll()}>{t("common:action.save")}</Button>
            </div>
          </div>

          {settingsTab === "identity" && (
            <>
              <AskSteve key={k} ctx={{ kind: "bot", id: k }} placeholder={t("manage:form.ask", { name: displayName })} />
              <Section label={t("manage:form.identity")}><Card className="flex flex-col gap-5 p-5">
                {b.main
                  ? <F label={t("manage:form.avatar")}><div className="flex items-center gap-4" data-steve-avatar-preview><Robot k={k} size={72} /></div></F>
                  : <F label={t("manage:form.avatar")} hint={t("manage:form.avatarHint")}><AvatarPicker id={k} look={{ color: fm.color, round: fm.round, accessory: fm.accessory ?? null }} onChange={(p) => set(p)} /></F>}
                <F label={t("manage:form.name")} htmlFor="bot-name"><TextField id="bot-name" value={fm.n} onChange={(e) => set({ n: e.target.value })} data-bot-name /></F>
                <F label={t("manage:form.role")} htmlFor="bot-role" hint={t("manage:form.roleHint")}><TextField id="bot-role" value={fm.role} onChange={(e) => set({ role: e.target.value })} data-bot-role /></F>
                <F label={t("manage:form.specialties")} htmlFor="bot-specialties" hint={t("manage:form.specialtiesHint")}><TextArea id="bot-specialties" minRows={2} maxRows={6} value={fm.specialties} onChange={(e) => set({ specialties: e.target.value })} /></F>
              </Card></Section>
              <Section label={t("manage:form.personality")}><Card className="flex flex-col gap-5 p-5">
                <F label={t("manage:form.voice")} htmlFor="bot-voice" hint={t("manage:form.voiceHint")}><TextArea id="bot-voice" minRows={3} maxRows={10} value={fm.pers} onChange={(e) => set({ pers: e.target.value })} /></F>
                <F label={t("manage:form.pro")} hint={t("manage:form.proHint")}><Strip><Segmented size="sm" className="w-max" value={fm.pro} onChange={(v) => set({ pro: v })} options={PRO.map((v, i) => [v, proLabel(t, i)] as const)} /></Strip></F>
              </Card></Section>
              <Section label={t("manage:form.work")}><Card className="flex flex-col gap-5 p-5">
                <F label={t("manage:form.rule")} htmlFor="bot-rule" hint={t("manage:form.ruleHint")}><TextArea id="bot-rule" minRows={3} maxRows={12} value={fm.rule} onChange={(e) => set({ rule: e.target.value })} /></F>
              </Card></Section>
            </>
          )}

          {settingsTab === "model" && (
            <>
              <Section label={t("manage:form.run")}><Card className="flex flex-col gap-5 p-5">
                <div className="grid gap-4 sm:grid-cols-2">
                  <F label="Harness"><RuntimeSelect testId="bot-harness" value={fm.harness} onChange={(v) => set({ harness: v, model: "" })} /><RuntimeSetupLink /></F>
                  <F label={t("manage:form.model")}><ModelPicker field harness={fm.harness} value={fm.model ?? ""} allowDefault testId="bot-model" onChange={(m) => set({ model: m })} /></F>
                  <F label={t("manage:form.effort")} hint={b.main ? t("manage:form.effortHintMain") : t("manage:form.effortHint")}><EffortPicker wide testId="bot-effort" label={t("manage:form.effort")} harness={fm.harness} model={fm.model} value={effort} onChange={(v) => set({ reasoningEffort: v })} /></F>
                </div>
                <RuntimeAvailability harness={fm.harness} />
                <FallbackSurfacesEditor value={fm.fallbackSurfaces ?? []} onChange={(fallbackSurfaces) => set({ fallbackSurfaces })} />
                {b.main && <SteveJudges harness={fm.harness} model={fm.model ?? ""} />}
              </Card></Section>
              {!b.main && <ExecutionNodes key={`${st.workspace?.id}:${k}`} bot={k} />}
            </>
          )}

          {settingsTab === "permissions" && (
            <>
              {/* affectUI's Agent / Permissions: what it may do without asking, one choice each, said in plain words */}
              <Section label={t("manage:form.permSection")} data-bot-perms><AutonomyChoice testId="bot-perm" label={t("manage:form.permSection")} value={fm.permMode} onChange={(v) => set({ permMode: v })}
                options={[...new Set([fm.permMode, ...permModes(fm.harness)])].map((v) => ({ value: v, name: permName(t, v), text: (PERM_KEYS as readonly string[]).includes(v) ? t(`manage:form.permText.${v as (typeof PERM_KEYS)[number]}`) : permLabel(t, v), icon: PERM_ICON[v] }))}
                means={<><ShieldAlertIcon aria-hidden />{t("manage:form.permHint")}</>} /><RuntimePermissionNotice harness={fm.harness} mode={fm.permMode} /></Section>
              <Section label={t("manage:form.acc")} data-bot-acc><AutonomyChoice testId="bot-acc" label={t("manage:form.acc")} value={fm.acc} onChange={(v) => set({ acc: v })}
                options={ACC_KEYS.map((key) => ({ value: ACC[key], name: accLabel(t, key), text: t(`manage:form.accText.${key}`), icon: ACC_ICON[key] }))} /></Section>
              <Section label={t("manage:form.scopeSection")}><Card className="flex flex-col gap-5 p-5">
                <F label={t("manage:form.project")} hint={t("manage:form.projectHint")}><SetSelect wide testId="bot-project" label={t("manage:form.project")} value={fm.project} onChange={(v) => set({ project: v })} options={[{ v: "none", l: t("manage:form.projectInfer") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} /></F>
                {!b.main && <F label={t("manage:form.workspaces")} hint={t("manage:form.workspacesHint")}>
                  <div className="flex flex-col gap-1.5">{st.workspaces.map((w) => <label key={w.id} className="flex items-center gap-2 type-callout"><Checkbox checked={fm.ws.includes(w.id)} disabled={fm.ws.length === 1 && fm.ws.includes(w.id)} onCheckedChange={(v) => set({ ws: v ? [...fm.ws, w.id] : fm.ws.filter((x: string) => x !== w.id) })} />{w.name}</label>)}</div></F>}
              </Card></Section>
            </>
          )}
        </FieldGroup>
      )}
    </Page>
  )
}
const MEM_UNDO_MS = 6000
/** a Bot's memories in this workspace: affectUI's Memory Panel, as on the knowledge page — added, edited in place and
 *  forgotten with a few seconds to undo (the Hub deletes it once Undo is gone). Bot memories have no kinds and no pause. */
function BotMemories({ k }: { k: string }) {
  const t = useT()
  const rows = useBotMemories(k)
  const [gone, setGone] = useState<string[]>([])
  const timers = useRef(new Map<string, number>())
  /* leaving the view, or closing or reloading the page, deletes what is still waiting for its Undo */
  useEffect(() => {
    const flush = () => { for (const [id, h] of timers.current) { clearTimeout(h); delMem(k, id) } timers.current.clear() }
    addEventListener("pagehide", flush)
    return () => { removeEventListener("pagehide", flush); flush() }
  }, [k])
  const general = t("core:affect.memory.general")
  const memories: Memory[] = (rows ?? []).filter(({ id }) => !gone.includes(id)).map(({ id, text, source }) => ({ id, text, source, category: general, canPause: false }))
  const forget = (id: string) => {
    setGone((g) => [...g, id])
    timers.current.set(id, window.setTimeout(() => { timers.current.delete(id); delMem(k, id) }, MEM_UNDO_MS))
  }
  const restore = (id: string) => {
    const h = timers.current.get(id); if (h) clearTimeout(h)
    timers.current.delete(id); setGone((g) => g.filter((x) => x !== id))
  }
  return (
    <div className="flex flex-col gap-3 anim-in" data-mem>
      <Lead className="px-1">{t("manage:mem.scope", { ws: st.workspace?.name })}</Lead>
      {!rows ? <Loading className="py-8" /> :
        <MemoryPanel className="[--ui-mpc-width:100%]" title={t("manage:detail.tabMemory")} showTitle={false} searchable={false} showSwitch={false} maxHeight={0} undoFor={MEM_UNDO_MS}
          memories={memories} categories={[general]} onChange={() => {}} addedSource={t("manage:mem.added")}
          text={{ none: t("manage:mem.emptySub"), newPlaceholder: t("manage:mem.placeholder") }}
          onAdd={(m) => addMem(k, m.text)} onEdit={(id, text) => { const v = text.trim(); if (v) editMemFn(k, id, v) }} onForget={forget} onRestore={(m) => restore(m.id)} />}
    </div>
  )
}
const editMemFn = (k: string, id: string, t: string) => editMem(k, id, t)

/* ═════════ 设置：分类页 ═════════ */
/* the categories, in the language of the moment: [id, label, icon, summary] */
const cats = (t: T): [string, string, any, string][] => [
  ["general", t("manage:settings.catGeneral"), SlidersHorizontalIcon, t("manage:settings.catGeneralSub")],
  ["workspace", t("manage:settings.catWorkspace"), LayersIcon, t("manage:settings.catWorkspaceSub")],
  ["items", t("manage:settings.catItems"), ListChecksIcon, t("manage:settings.catItemsSub")],
  ["tags", t("manage:settings.catTags"), TagIcon, t("manage:settings.catTagsSub")],
  ["notify", t("manage:settings.catNotify"), BellIcon, t("manage:settings.catNotifySub")],
  ["look", t("manage:settings.catLook"), SunMoonIcon, t("manage:settings.catLookSub")],
  MODELS_CAT,
  VAULT_CAT,
  STATS_CAT,
  /* the audit trail names every member, client and token: the Hub answers the instance's owner only */
  ...(st.owner ? [AUDIT_CAT] : []),
  /* what this Hub has installed beside it (channels, themes): the instance's, so it sits with data and about */
  PLUGINS_CAT,
  ["data", t("manage:settings.catData"), DatabaseIcon, t("manage:settings.catDataSub")],
  ["about", t("manage:settings.catAbout"), InfoIcon, t("manage:settings.catAboutSub")],
]
const PH_KEYS: Record<string, "phGeneral" | "phItems" | "phTags" | "phModels" | "phNotify" | "phLook" | "phData"> = {
  general: "phGeneral", items: "phItems", tags: "phTags", models: "phModels", look: "phLook", data: "phData",
}
const Sel = ({ v, o, on, testId, label }: { v: string; o: [string, string][]; on: (v: string) => void; testId?: string; label?: string }) => <SetSelect testId={testId} label={label} value={v} options={o} onChange={on} />
export function SettingsView() {
  useShellStore()
  const t = useT()
  const [, cat] = useRoute()
  const wide = typeof window !== "undefined" && window.innerWidth >= 768
  const cur = cat ?? (wide ? "general" : "")
  if (["runtime", "accounts"].includes(cur)) { setTimeout(() => go(cur === "runtime" ? "#/devices" : "#/accounts")); return null }
  const CATS = cats(t)
  const C = CATS.find((c) => c[0] === cur)
  const nav = (
    <nav className="flex flex-col gap-0.5" data-settings-nav>
      {CATS.map(([id, l, I, d]) => (
        <a key={id} href={"#/settings/" + id} data-settings-cat={id} aria-current={cur === id ? "page" : undefined} className={cn("group/cat flex items-center gap-3 rounded-lg px-2.5 py-2 type-body text-foreground/80 transition-colors hover:bg-accent hover:text-foreground md:py-1.5 md:type-callout", cur === id && "bg-accent font-medium text-foreground")}>
          <I className={cn("size-4 shrink-0 text-muted-foreground transition-colors", cur === id && "text-foreground")} strokeWidth={1.75} /><span className="min-w-0 flex-1"><span className="flex items-center gap-1.5">{l}{id === "about" && st.update && <span className="size-1.5 rounded-full bg-attention" data-settings-update-dot />}</span><span className="block truncate type-footnote font-normal text-muted-foreground md:hidden">{d}</span></span><ChevronRightIcon className="size-4 shrink-0 text-tertiary md:hidden" />
        </a>))}
    </nav>
  )
  if (!wide && !C) return <Page title={t("manage:settings.title")} max={640}><div className={cn(LIFT, "p-1 [&_[data-settings-cat]]:py-2.5")}>{nav}</div></Page>
  if (!C) return null
  return (
    <Page title={wide ? t("manage:settings.title") : C[1]} back={wide ? undefined : ["#/settings", t("manage:settings.title")]} max={960}>
      <div className="grid gap-12 md:grid-cols-[200px_minmax(0,1fr)]">
        {wide && <div className="-mt-1"><div className="sticky top-4">{nav}</div></div>}
        <div key={cur} className="min-w-0 max-w-[680px] anim-in [&_[data-slot=item-description]]:line-clamp-none" data-settings-pane={cur}>
          {/* every pane opens the same way: its title and one line on what it holds */}
          {wide ? <><h2 className="mb-1 type-title">{C[1]}</h2><p className="mb-6 type-callout text-muted-foreground">{C[3]}</p></> : <p className="-mt-4 mb-6 px-1 type-callout text-muted-foreground">{C[3]}</p>}
          {PH_KEYS[cur] && <AskSteve key={cur} className="mb-8" ctx={cur === "tags" || cur === "models" ? { kind: cur as "tags" | "models" } : { kind: "settings", id: cur }} placeholder={t(`manage:settings.${PH_KEYS[cur]}`)} />}
          <Pane id={cur} />
        </div>
      </div>
    </Page>
  )
}
function LanguageRow() {
  const t = useT()
  return <Row title={t("common:settings.language")} sub={t("common:settings.languageSub")} data-language-row><LanguageSwitch /></Row>
}
/** a Bot's schedules, read from 自动化 where they are made and changed */
function BotSchedules({ k }: { k: string }) {
  const t = useT()
  const L = useHubQuery(["flow", "schedules"], () => flowApi.schedules(), { events: ["flow", "run"], debounceMs: 300 })
  if (L.error && !L.data) return <Empty title={errText(L.error)} />
  if (!L.data) return <Loading />
  const mine = L.data.filter((s) => s.bot === k)
  return <Section data-bot-schedules className="anim-in" note={t("manage:detail.scheduleNote")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/auto/schedules")} data-bot-schedules-open>{t("manage:detail.scheduleOpen")}</Button>} label={t("manage:detail.scheduleCount", { n: mine.length })}>
    {mine.length ? <Panel>{mine.map((s) => <Row key={s.id} icon={<Tile I={ClockIcon} />} title={s.name} onClick={() => go("#/auto/schedules")} data-bot-schedule={s.id}
      sub={<Meta><span>{s.describe}</span><span>{s.enabled ? (s.next_at ? t("auto:schedule.next", { time: when(s.next_at) }) : t("auto:schedule.onPlan")) : t("auto:disabled")}</span></Meta>}><Chevron /></Row>)}</Panel>
      : <Blank>{t("manage:detail.scheduleNone")}</Blank>}
  </Section>
}
function Pane({ id }: { id: string }) {
  const t = useT()
  const c = st.settings
  if (id === "tags") return <TagsPane />
  if (id === "workspace") return <WorkspacePane />
  if (id === "vault") return <VaultSettingsPane />
  if (id === "stats") return <StatsSettingsPane />
  if (id === "audit") return <AuditSettingsPane />
  if (id === "models") return <ModelsSettingsPane />
  if (id === "plugins") return <PluginsSettingsPane />
  if (id === "general") {
    const homeLabels = [t("manage:general.homeChat"), t("manage:general.homeInbox"), t("manage:general.homeItems")]
    const homeOpts: [string, string][] = WORKSPACE_SETTING_VALUES.home.map((v, i) => [v, homeLabels[i] ?? v])
    const awayOpts: [string, string][] = WORKSPACE_SETTING_VALUES.awayH.map((v) => [
      v,
      v === "0" ? t("manage:general.awayNever") : t("common:dur.hours", { count: Number(v) }),
    ])
    return <div className="flex flex-col gap-8">
      <Section label={t("manage:general.assistant")}><Panel><Row icon={<ActorAvatar actorId="steve" size={36} />} title={st.bots.steve?.n} sub={t("manage:general.assistantSub")} onClick={() => go("#/bots/steve")}><Chevron /></Row></Panel></Section>
      <Panel>
        <Row title={t("manage:general.home")} sub={t("manage:general.homeSub")}><Sel label={t("manage:general.home")} testId="home" v={c.home ?? WORKSPACE_SETTING_DEFAULTS.home} o={homeOpts} on={(v) => setSetting({ home: v })} /></Row>
        <Row title={t("manage:general.away")} sub={t("manage:general.awaySub")}><Sel label={t("manage:general.away")} testId="away" v={c.awayH ?? WORKSPACE_SETTING_DEFAULTS.awayH} o={awayOpts} on={(v) => setSetting({ awayH: v })} /></Row>
      </Panel>
      <Panel>
        <Row title={t("manage:general.onboard")} sub={t("manage:general.onboardSub")}><Button size="sm" variant="outline" onClick={() => { localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }} data-restart-onboard>{t("manage:general.restart")}</Button></Row>
        <Row title={t("manage:general.shortcuts")} sub={t("manage:general.shortcutsSub")} />
      </Panel>
    </div>
  }
  if (id === "items") {
    const noExecOpts = WORKSPACE_SETTING_VALUES.noExecutor.map((v) => ({
      v,
      l: v === "steve" ? t("manage:items.noExecutorSteve") : t("manage:items.noExecutorAsk"),
    }))
    const accOpts = WORKSPACE_SETTING_VALUES.accDefault.map((v): [string, string] => [v, v === "manual" ? t("manage:acc.manual") : t("manage:acc.auto")])
    const depsLabels = [t("manage:items.depsAuto"), t("manage:items.depsRemind")]
    const depsOpts: [string, string][] = WORKSPACE_SETTING_VALUES.depsAfter.map((v, i) => [v, depsLabels[i] ?? v])
    const archOpts: [string, string][] = WORKSPACE_SETTING_VALUES.archDays.map((v) => [
      v,
      v === "0" ? t("manage:items.archNever") : t("common:dur.days", { count: Number(v) }),
    ])
    const quotaAutoOpts: [string, string][] = WORKSPACE_SETTING_VALUES.quotaAutoHandoff.map((v) => [
      v,
      v === "off" ? t("manage:items.quotaAutoHandoffOff") : t("manage:items.quotaAutoHandoffOn"),
    ])
    const quotaWaitOpts: [string, string][] = WORKSPACE_SETTING_VALUES.quotaWaitMinutes.map((v) => [
      v,
      v === "0" ? t("manage:items.quotaWaitMinutesOff") : t("manage:items.quotaWaitMinutesMin", { minutes: Number(v) }),
    ])
    return <div className="flex flex-col gap-8">
      {/* the pane's title already says 事项: the first group goes without a label; the number prefix lives in 工作空间 */}
      <Panel>
        <Row className="max-sm:flex-col max-sm:items-stretch max-sm:gap-2" title={t("manage:items.noExecutor")} sub={t("manage:items.noExecutorSub")}><SetSelect testId="settings-no-executor" label={t("manage:items.noExecutor")} value={c.noExecutor === "steve" ? "steve" : WORKSPACE_SETTING_DEFAULTS.noExecutor} disabled={!st.owner} options={noExecOpts} onChange={(v) => setSetting({ noExecutor: v })} /></Row>
        <Row className="max-sm:flex-col max-sm:items-stretch max-sm:gap-2" title={t("manage:items.accDefault")} sub={t("manage:items.accDefaultSub")}><Segmented size="sm" value={c.accDefault === "manual" ? "manual" : WORKSPACE_SETTING_DEFAULTS.accDefault} onChange={(v) => setSetting({ accDefault: v })} options={accOpts} /></Row>
        <Row title={t("manage:items.depsAfter")} sub={t("manage:items.depsAfterSub")}><Sel label={t("manage:items.depsAfter")} testId="deps-after" v={c.depsAfter ?? WORKSPACE_SETTING_DEFAULTS.depsAfter} o={depsOpts} on={(v) => setSetting({ depsAfter: v })} /></Row>
        <Row title={t("manage:items.archDays")} sub={t("manage:items.archDaysSub")}><Sel label={t("manage:items.archDays")} testId="arch-days" v={c.archDays ?? WORKSPACE_SETTING_DEFAULTS.archDays} o={archOpts} on={(v) => setSetting({ archDays: v })} /></Row>
        {/* the personal agent config default is the Hub's (a Node serves every workspace): set on Devices · Hub */}
        <Row title={t("manage:items.ownerContext")} sub={t("manage:items.ownerContextSub")}><Button size="sm" variant="outline" onClick={() => go("#/devices/hub")} data-owner-context-link>{t("manage:items.ownerContextOpen")}</Button></Row></Panel>
      <Section label={t("manage:items.sessions")}><Panel>
        <Row title={t("manage:items.quotaAutoHandoff")} sub={t("manage:items.quotaAutoHandoffSub")}><Sel label={t("manage:items.quotaAutoHandoff")} testId="quota-auto-handoff" v={c.quotaAutoHandoff ?? WORKSPACE_SETTING_DEFAULTS.quotaAutoHandoff} o={quotaAutoOpts} on={(v) => setSetting({ quotaAutoHandoff: v })} /></Row>
        <Row title={t("manage:items.quotaWaitMinutes")} sub={t("manage:items.quotaWaitMinutesSub")}><Sel label={t("manage:items.quotaWaitMinutes")} testId="quota-wait-minutes" v={c.quotaWaitMinutes ?? WORKSPACE_SETTING_DEFAULTS.quotaWaitMinutes} o={quotaWaitOpts} on={(v) => setSetting({ quotaWaitMinutes: v })} /></Row></Panel></Section>
    </div>
  }
  if (id === "notify") return st.workspace ? <NotificationSettingsPane key={`${currentHub()}:${st.workspace.id}`} workspace={st.workspace.id} /> : null
  if (id === "look") {
    const fontLabels = [t("manage:look.fontNormal"), t("manage:look.fontLarge")]
    const fontOpts: [string, string][] = WORKSPACE_SETTING_VALUES.fontSize.map((v, i) => [v, fontLabels[i] ?? v])
    const [motionOn, motionOff] = WORKSPACE_SETTING_VALUES.reduceMotion
    return <div className="flex flex-col gap-8">
      <Section label={t("manage:look.ui")}><Panel>
        <Row title={t("manage:look.theme")}><ThemeSwitch labels /></Row>
        <LanguageRow />
        <Row title={t("manage:look.fontSize")}><Segmented size="sm" value={c.fontSize} onChange={(v) => setSetting({ fontSize: v })} options={fontOpts} /></Row>
        <Row title={t("manage:look.reduceMotion")} sub={t("manage:look.reduceMotionSub")}><SetSwitch testId="reduce-motion" label={t("manage:look.reduceMotion")} checked={c.reduceMotion === motionOn} onChange={(v) => setSetting({ reduceMotion: v ? motionOn : motionOff })} /></Row>
      </Panel></Section>
      <AppearanceSection />
      {/* the Bots' avatars are set on each Bot: one row that shows them and leads there, not a gallery */}
      <Panel><Row title={t("manage:look.avatars")} sub={t("manage:look.avatarsHint")} onClick={() => go("#/bots")} data-avatar-preview>
        <span className="hidden items-center gap-1 sm:flex" aria-hidden>{Object.keys(st.bots).slice(0, 5).map((k) => <Robot key={k} k={k} size={22} />)}</span><Chevron /></Row></Panel>
    </div>
  }
  if (id === "data") {
    const [keep90d, keep1y, keepForever] = WORKSPACE_SETTING_VALUES.keepSess
    const keepSess = c.keepSess || WORKSPACE_SETTING_DEFAULTS.keepSess
    return <div className="flex flex-col gap-8">
      <HubData />
      <WorkspaceDisk />
      <OutboxDead />
      <Section label={t("manage:data.keep")}><Panel>
        {/* read-only: whole logs are kept for good; a shorter keepSess (set by a confirmed config draft) compacts archived sessions */}
        <Row title={t("manage:data.sessions")} sub={keepSess !== keepForever ? t("manage:data.sessionsCompact", { period: keepSess === keep1y ? t("manage:data.keepYear") : t("common:dur.days", { count: 90 }) }) : t("manage:data.sessionsForever")} data-keep-sess={keepSess}>
          <span className="type-callout text-muted-foreground">{keepSess === keep1y ? t("manage:data.keepYear") : keepSess === keep90d ? t("common:dur.days", { count: 90 }) : t("manage:data.keepForever")}</span></Row></Panel></Section>
      <Section label={t("manage:data.danger")} data-danger-zone><Panel className="shadow-[var(--lift),0_0_0_1px_color-mix(in_srgb,var(--destructive)_18%,transparent)]">
        <DangerRow testId="clear-mem" title={t("manage:data.clearMem")} sub={t("manage:data.clearMemSub")} label={t("manage:data.clear")} what={t("manage:data.clearWhat")} onOk={() => void api.clearMemories().then(() => toast(t("manage:data.cleared")))} /></Panel></Section>
    </div>
  }
  if (id === "about") return <AboutVersions />
  return null
}
/* the settings-general pattern of affectUI: name and prefix as rows with their field on the right, the description as a
 * field of its own below; text saves when it loses focus, so there is no save bar */
function WorkspacePane() {
  const t = useT()
  const w = st.workspace!
  const [n, setN] = useState(w.name), [d, setD] = useState(w.description), [p, setP] = useState(w.item_prefix)
  const [failed, setFailed] = useState(false)
  /* a save refreshes the workspace; take the new values only into fields the user has not changed since they last
   * matched the Hub, so typing in one field is never replaced by the refresh that saving another one starts */
  const seen = useRef({ id: w.id, name: w.name, description: w.description, item_prefix: w.item_prefix })
  useLayoutEffect(() => {
    const s = seen.current, same = s.id === w.id
    setN((v) => (same && v !== s.name ? v : w.name))
    setD((v) => (same && v !== s.description ? v : w.description))
    setP((v) => (same && v !== s.item_prefix ? v : w.item_prefix))
    if (!same) setFailed(false)
    seen.current = { id: w.id, name: w.name, description: w.description, item_prefix: w.item_prefix }
  }, [w.id, w.name, w.description, w.item_prefix])
  const prefix = p.toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6)
  const save = () => {
    const next = { name: n.trim(), description: d.trim(), item_prefix: prefix }
    if (!next.name) { setN(w.name); toast(t("manage:ws.nameNeeded")); return }
    if (!next.item_prefix) { setP(w.item_prefix); toast(t("manage:ws.prefixNeeded")); return }
    if (next.name === w.name && next.description === w.description && next.item_prefix === w.item_prefix) return
    void api.patchWs(w.id, next).then(() => { setFailed(false); toast(t("common:state.saved")); return import("@/store").then((m) => m.refresh()) }).catch((e) => { setFailed(true); toast(e.message) })
  }
  /* Enter saves (by leaving the field); Escape puts back what the Hub has, so the blur after it saves nothing */
  const keys = (undo: () => void) => (e: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
    if (e.key === "Enter" && e.currentTarget instanceof HTMLInputElement) e.currentTarget.blur()
    if (e.key === "Escape") { e.preventDefault(); undo(); setFailed(false) }
  }
  /* a failed save leaves the text in place to try again, marked as not saved */
  const unsaved = failed && (n.trim() !== w.name || d.trim() !== w.description || prefix !== w.item_prefix) ? t("manage:ws.unsaved") : undefined
  /* below 640px the field goes under its label, full width, as on affectUI's settings screens */
  const stack = "max-sm:flex-col max-sm:items-stretch max-sm:gap-2 [&>[data-slot=item-actions]]:max-sm:w-full"
  return <div className="flex flex-col gap-8">
    <Section note={t("manage:ws.intro")}><Panel data-ws-form>
      <Row className={stack} title={t("manage:ws.name")} sub={t("manage:ws.nameSub")}>
        <TextField size="sm" className="w-full sm:w-60" value={n} maxLength={60} aria-label={t("manage:ws.name")} error={unsaved && n.trim() !== w.name ? unsaved : undefined} onChange={(e) => setN(e.target.value)} onBlur={save} onKeyDown={keys(() => setN(w.name))} data-ws-name /></Row>
      <Row className={stack} title={t("manage:ws.prefix")} sub={t("manage:ws.prefixHint", { example: `${prefix || "ST"}-${nextItemNumber()}` })}>
        <TextField size="sm" className="w-full sm:w-28" inputClassName="font-mono uppercase" value={p} maxLength={6} spellCheck={false} aria-label={t("manage:ws.prefix")} error={unsaved && prefix !== w.item_prefix ? unsaved : undefined} onChange={(e) => setP(e.target.value)} onBlur={save} onKeyDown={keys(() => setP(w.item_prefix))} data-prefix /></Row>
    </Panel></Section>
    <Section label={t("manage:ws.desc")}>
      <TextArea minRows={3} maxRows={10} value={d} aria-label={t("manage:ws.desc")} placeholder={t("manage:ws.descPlaceholder")} hint={t("manage:ws.descHint")} error={unsaved && d.trim() !== w.description ? unsaved : undefined} onChange={(e) => setD(e.target.value)} onBlur={save} onKeyDown={keys(() => setD(w.description))} data-ws-desc /></Section>
    <Section label={t("core:approval.title")}><PermissionPolicyPanel /></Section>
    <Section label={t("manage:ws.here")}><Panel>
      <Row icon={<Tile I={FolderIcon} />} title={t("manage:ws.projects", { count: st.projects.length })} onClick={() => go("#/projects")}><Chevron /></Row>
      <Row icon={<Tile I={BotIcon} />} title={t("manage:ws.bots", { count: Object.keys(st.bots).length })} onClick={() => go("#/bots")}><Chevron /></Row>
      <Row icon={<Tile I={ListTodoIcon} />} title={t("manage:ws.items", { count: getItems().length })} onClick={() => go("#/items")}><Chevron /></Row></Panel></Section>
    <Section label={st.workspaces.length > 1 ? t("manage:ws.all", { n: st.workspaces.length }) : undefined}>
      {st.workspaces.length > 1 && <Panel>{st.workspaces.map((x) => <Row key={x.id} title={x.name} sub={x.description || undefined}>{x.id === w.id ? <Pill tone="info">{t("manage:ws.current")}</Pill> : <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void import("@/store").then((m) => m.switchWorkspace(x.id))}>{t("manage:ws.switch")}</Button>}</Row>)}</Panel>}
      <Button size="sm" variant="outline" className={st.workspaces.length > 1 ? "mt-3" : undefined} onClick={() => openSheet("newWorkspace")}><PlusIcon />{t("manage:ws.new")}</Button></Section>
  </div>
}
/* the danger zone of affectUI's settings-general: the action in red, and confirming opens in place under the row
 * instead of a dialog; Escape or Cancel folds it back and hands the focus to the button that opened it */
function DangerRow({ title, sub, label, what, onOk, testId }: { title: string; sub: string; label: string; what: string; onOk: () => void; testId: string }) {
  const t = useT()
  const [o, setO] = useState(false)
  const opener = useRef<HTMLButtonElement>(null)
  const id = useId()
  const close = () => { setO(false); setTimeout(() => opener.current?.focus()) }
  /* the opener stays in place (disabled while the confirm is open, and saying so with aria-expanded); the focus goes to
   * Cancel, so an Enter pressed twice or held cannot clear anything */
  return <div data-danger={testId} data-danger-open={o || undefined}>
    <Row title={title} sub={sub}><Button ref={opener} size="sm" variant="outline" className="border-destructive/35 text-destructive shadow-none hover:bg-destructive/8 hover:text-destructive dark:border-destructive/40" disabled={o} aria-expanded={o} aria-controls={id} onClick={() => setO(true)} data-danger-open-btn><TrashIcon />{label}</Button></Row>
    {o && <form id={id} role="group" aria-labelledby={id + "-what"} className="anim-in mx-3 flex flex-col gap-3 border-t border-border/60 pt-3 pb-2.5 sm:flex-row sm:items-center sm:justify-between" onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); close() } }} onSubmit={(e) => { e.preventDefault(); setO(false); onOk() }}>
      <p id={id + "-what"} className="min-w-0 type-callout text-muted-foreground">{what}</p>
      <div className="flex shrink-0 gap-2 max-sm:[&>*]:flex-1">
        <Button size="sm" variant="ghost" type="button" autoFocus onClick={close} data-danger-cancel>{t("common:action.cancel")}</Button>
        <Button size="sm" variant="destructive" type="submit" data-danger-confirm>{t("manage:danger.confirm", { label })}</Button></div>
    </form>}
  </div>
}

/* ═════════ 标签 ═════════ */
let tagCat = "全部"
function TagsPane() {
  useShellStore()
  useTagUses()
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [del, setDel] = useState<string | null>(null), [merge, setMerge] = useState<string | null>(null), [into, setInto] = useState("")
  const cats = ["全部", ...TAG_CATS.filter((c) => st.tags.some((tg) => tg.cat === c))]
  const d = del ? tag(del) : null, m = merge ? tag(merge) : null
  const domain = st.tags.filter((x) => !isKind(x))
  return <div className="flex flex-col gap-8" data-tags-pane>
    <div className="flex flex-wrap items-center gap-2"><Segmented size="sm" value={cats.includes(tagCat) ? tagCat : "全部"} onChange={(v) => { tagCat = v; r() }} options={cats.map((c) => [c, catLabel(t, c)] as const)} /><span className="flex-1" /><Button size="sm" onClick={() => openSheet("tag", {})} data-new-tag-btn><PlusIcon />{t("manage:tags.new")}</Button></div>
    {!st.tags.length && <Empty title={t("manage:tags.empty")} sub={t("manage:tags.emptySub")} />}
    {TAG_CATS.filter((c) => tagCat === "全部" || c === tagCat).map((c) => { const kind = c === KIND_CAT; const xs = kind ? [...kindTags(), ...domain.filter((tg) => tg.cat === c)] : st.tags.filter((tg) => tg.cat === c); return xs.length ? (
      <Section key={c} label={<>{kind ? t("manage:tags.kindSection") : catLabel(t, c)} <span className="tabular-nums">· {xs.length}</span></>} note={kind ? t("manage:tags.intro") : undefined}>
        <Panel>{xs.map((tg) => { const kind = isKind(tg); return (
          <Row key={tg.id} title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5">{kind ? <KindTag id={tg.id} /> : <span className="font-medium">{tagLabel(tg)}</span>}<span className="type-footnote tabular-nums text-muted-foreground">{t("manage:tags.uses", { count: tagUses(tg.id) })}</span>{!kind && tg.by === "Steve" && <span className="type-footnote text-tertiary">{t("manage:tags.bySteve", { created: tg.created })}</span>}</span>} sub={tagDesc(tg)} data-tag-row={tg.name}>
            <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-tag-menu={tg.name} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-40">
                <DropdownMenuItem onClick={() => showTag(tg.id)} data-show-tag>{t("manage:tags.showItems")}</DropdownMenuItem>
                <DropdownMenuItem onClick={() => openSheet("tag", { id: tg.id })} data-edit-tag><PencilIcon />{kind ? t("manage:tags.editDesc") : t("common:action.edit")}</DropdownMenuItem>
                {!kind && domain.length > 1 && <DropdownMenuItem onClick={() => { setMerge(tg.id); setInto(domain.find((x) => x.id !== tg.id && x.cat === tg.cat)?.id ?? domain.find((x) => x.id !== tg.id)!.id) }} data-merge-tag>{t("manage:tags.mergeTo")}</DropdownMenuItem>}
                {!kind && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(tg.id)} data-del-tag><TrashIcon />{t("common:action.delete")}</DropdownMenuItem></>}
              </DropdownMenuContent></DropdownMenu>
          </Row>)})}</Panel></Section>) : null })}
    <AlertDialog open={!!d} onOpenChange={(o) => !o && setDel(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("manage:tags.delTitle", { name: d?.name })}</AlertDialogTitle><AlertDialogDescription>{t("manage:tags.delDesc", { count: del ? tagUses(del) : 0 })}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteTag(del!); setDel(null) }} data-del-tag-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
    <AlertDialog open={!!m} onOpenChange={(o) => !o && setMerge(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("manage:tags.mergeTitle", { name: m?.name })}</AlertDialogTitle><AlertDialogDescription>{t("manage:tags.mergeDesc", { count: merge ? tagUses(merge) : 0, name: m?.name })}</AlertDialogDescription></AlertDialogHeader>
      <Select testId="merge-into" value={into} onChange={setInto} options={domain.filter((x) => x.id !== merge).map((x) => ({ v: x.id, l: `${x.name} · ${catLabel(t, x.cat)}` }))} />
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction onClick={() => { mergeTag(merge!, into); setMerge(null) }} data-merge-confirm>{t("manage:tags.merge")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </div>
}
/** create or edit a tag; from an item's picker the new tag is attached to that item */
export function TagDialog({ id, item, name }: { id?: string; item?: string; name?: string }) {
  useShellStore()
  const t = useT()
  const tg = id ? tag(id) : undefined
  const kind = isKind(tg)
  const [n, setN] = useState(tg?.name ?? name ?? ""), [c, setC] = useState(tg?.cat ?? "领域"), [d, setD] = useState(tg?.desc ?? "")
  const dup = st.tags.find((x) => x.name === n.trim() && x.id !== id)
  const ok = n.trim() && d.trim() && !dup
  const save = async () => {
    if (!ok) return
    if (tg) saveTag(tg.id, kind ? { desc: d.trim() } : { name: n.trim(), cat: c, desc: d.trim() })
    else { const nid = await makeTag({ name: n.trim(), cat: c, desc: d.trim() }); if (nid && item) setTags(item, [...(it(item).tags ?? []), nid]); if (nid) toast(t("manage:tagDialog.created", { name: n.trim() })) }
    closeSheet()
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 sm:max-w-[480px]" data-tag-dialog>
        <DialogHeader><DialogTitle className="type-title">{tg ? t("manage:tagDialog.edit") : t("manage:tagDialog.new")}</DialogTitle><DialogDescription className="type-callout">{t("manage:tagDialog.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <F label={t("manage:tagDialog.name")} htmlFor="tag-name" hint={dup ? t("manage:tagDialog.dup", { cat: catLabel(t, dup.cat) }) : undefined}><Input id="tag-name" autoFocus={!kind} disabled={kind} value={n} onChange={(e) => setN(e.target.value)} placeholder={t("manage:tagDialog.namePlaceholder")} /></F>
          <F label={t("manage:tagDialog.cat")} hint={kind ? t("manage:tagDialog.catBuiltin") : undefined}><div>{kind ? <span className="type-callout text-muted-foreground">{t("manage:tagDialog.kind")}</span> : <Select testId="tag-cat" value={c} onChange={setC} options={(tg?.cat === KIND_CAT ? [KIND_CAT, ...DOMAIN_CATS] : DOMAIN_CATS).map((x) => ({ v: x, l: catLabel(t, x) }))} />}</div></F>
          <F label={t("manage:tagDialog.descLabel")} htmlFor="tag-desc"><Textarea id="tag-desc" className="min-h-20" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("manage:tagDialog.descPlaceholder")} data-tag-desc /></F>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={!ok} onClick={() => void save()} data-testid="tag-go">{tg ? t("common:action.save") : t("manage:tagDialog.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
