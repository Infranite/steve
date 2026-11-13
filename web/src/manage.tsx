/* Bot 菜单与设置：Bot 是独立菜单；设置是分类页。所有改动都写到 Hub，也都可以直接告诉 Steve。 */
import { ACCESS_CAT, AccessSettingsPane } from "@/access-settings"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { useT } from "@/i18n"
import { Trans } from "react-i18next"
import { WorkspaceDisk } from "@/disk-settings"
import { HubData } from "@/backup-settings"
import { OutboxDead } from "@/outbox-settings"
import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, LayoutTemplateIcon, FilePlusIcon, MoreHorizontalIcon, CopyIcon, TrashIcon, SparklesIcon, SlidersHorizontalIcon, ListChecksIcon, TagIcon, BellIcon, SunMoonIcon, DatabaseIcon, InfoIcon, PencilIcon, CheckIcon, LayersIcon, FolderIcon, BotIcon, ListTodoIcon, ClockIcon, EyeIcon, HandIcon, FilePenIcon, ZapIcon, ShieldOffIcon, ShieldAlertIcon, UserCheckIcon, CheckCheckIcon, MessageSquareTextIcon, MessageCircleQuestionIcon, ShieldCheckIcon, PackageCheckIcon, RocketIcon, GitMergeIcon, WrenchIcon, PlugIcon, TerminalIcon } from "lucide-react"
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
import { Card as ACard, CardHeader } from "@/components/affect/card-clean"
import { MemoryPanel, type Memory } from "@/components/affect/memory-panel"
import { AutonomyChoice } from "@/components/affect/agent-permissions"
import { SkillGrid, type Skill } from "@/components/affect/agent-skills"
import { stats } from "@/api/stats"
import { useStats } from "@/stats/format"
import {
  st, botName, PRO, ACC, PERM, harnessEfforts, permModes, runtimeName, defaultHarness, saveBot, makeBot, draftBot, deleteBot, dupBot, addMem, editMem, delMem, loadMemories, setBotWorkspaces, tag, tagLabel, tagDesc, tagUses, TAG_CATS, KIND_CAT, DOMAIN_CATS, isKind, kindOf, kindTags, setKind, loadKindsReview, kindsReviewed, kindsBackfill, makeTag, saveTag, deleteTag, mergeTag, setTags, it,
  setSetting, toast, finishOnboarding, emit, closedSt,
} from "@/store"
import { api, type RKindReview } from "@/api"
import { go, Page, BotAvatar, ItemNo, KindTag, Row, Segmented, Meta, Empty, openSheet, closeSheet, ThemeSwitch, LanguageSwitch, useRoute, Panel, Card, Section, Note, Lead, Notice, Pill, Tile, Chevron, Loading, LIFT, Blank, Strip, EASE } from "@/ui"
import { PersonasSection } from "@/channels/persona"
import { OwnerChip } from "@/people/owner-chip"
import { Select, ItemRowExport as ItemRow } from "@/view-shared"
import { showTag } from "@/views"
import { AskSteve } from "@/config-ui"
import { AppearanceSection } from "@/appearance-settings"
import { VAULT_CAT, VaultSettingsPane } from "@/vault/settings"
import { BotMcpSection, PlannedCaps } from "@/bot-mcp"
import { STATS_CAT, StatsSettingsPane } from "@/stats/prices"
import { AUDIT_CAT, AuditSettingsPane } from "@/audit-settings"
import { MODELS_CAT, ModelsSettingsPane, SteveJudges } from "@/models-settings"
import { ModelPicker } from "@/model-picker"
import { SetSelect, SetSwitch } from "@/settings-controls"
import { TextField } from "@/components/ui/input"
import { TextArea } from "@/components/affect/text-area-clean"
import { RecentStats } from "@/stats/drill"
import { AboutVersions } from "@/version"
import { flowApi, useLoad as useFlowLoad, errText } from "@/api/flow"
import { when } from "@/auto/common"

/* ═════════ Bot 列表 ═════════ */
let botFilter: "全部" | "启用" | "停用" = "全部"
type T = ReturnType<typeof useT>
const PRO_KEYS = ["low", "medium", "high"] as const
/** a proactiveness level (0–2) as shown */
const proLabel = (t: T, i: number) => (PRO_KEYS[i] ? t(`manage:pro.${PRO_KEYS[i]}`) : PRO[i] ?? "")
const ACC_KEYS = ["inherit", "manual", "auto"] as const
/** an accept mode by backend key as shown; unknown keys fall back to the store's label */
const accLabel = (t: T, k: string) => ((ACC_KEYS as readonly string[]).includes(k) ? t(`manage:acc.${k as (typeof ACC_KEYS)[number]}`) : ACC[k] ?? k)
const PERM_KEYS = ["plan", "default", "acceptEdits", "auto", "bypassPermissions"] as const
const permLabel = (t: T, k: string) => ((PERM_KEYS as readonly string[]).includes(k) ? t(`manage:perm.${k as (typeof PERM_KEYS)[number]}`) : PERM[k] ?? k)
/** a permission mode in a word or two (the tile's name); its sentence is permLabel */
const permName = (t: T, k: string) => ((PERM_KEYS as readonly string[]).includes(k) ? t(`manage:form.permName.${k as (typeof PERM_KEYS)[number]}`) : PERM[k] ?? k)
const PERM_ICON: Record<string, ReactNode> = { plan: <EyeIcon />, default: <HandIcon />, acceptEdits: <FilePenIcon />, auto: <ZapIcon />, bypassPermissions: <ShieldOffIcon /> }
const ACC_ICON: Record<string, ReactNode> = { inherit: <SlidersHorizontalIcon />, manual: <UserCheckIcon />, auto: <CheckCheckIcon /> }
/* tag categories are stored as Chinese words (store TAG_CATS); only the label is translated */
const TAG_CAT_KEYS: Record<string, "all" | "project" | "kind" | "delivery" | "domain" | "client" | "topic"> = { 全部: "all", 项目: "project", 类型: "kind", 交付: "delivery", 领域: "domain", 客户: "client", 主题: "topic" }
const catLabel = (t: T, c: string) => (TAG_CAT_KEYS[c] ? t(`manage:tagCat.${TAG_CAT_KEYS[c]}`) : c)

export function BotsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const ks = Object.keys(st.bots).filter((k) => !st.bots[k].main && (botFilter === "全部" || (botFilter === "启用") === st.bots[k].on))
  const S = st.bots.steve
  const runOf = (k: string) => st.items.filter((i) => i.b === k && ["run", "wait", "block"].includes(itemDisplayStatus(i)) && !i.arch).length
  const others = st.allBots.filter((b) => !st.bots[b.id])
  return (
    <Page title="Bot" sub={t("manage:list.sub")} max={720}
      actions={<Button size="sm" onClick={() => openSheet("newBot")} data-new-bot><PlusIcon />{t("manage:list.newBot")}</Button>}>
      <div className="flex flex-col gap-8">
      {S && <Section label={t("manage:list.main")}>
        <BotCard k="steve" size={44} title={S.n} sub={S.role} meta={[S.h, S.m, t("manage:bot.running", { count: runOf("steve") })]} action={<Chevron />} />
      </Section>}
      <Section label={<><span className="hidden sm:inline">{t("manage:list.others", { ws: st.workspace?.name, n: Object.keys(st.bots).length - 1 })}</span><span className="sm:hidden">{t("manage:list.othersShort", { n: Object.keys(st.bots).length - 1 })}</span></>} note={<Trans ns="manage" i18nKey="list.hint" components={{ code: <span className="font-mono" /> }} />}
        right={<Segmented size="sm" value={botFilter} onChange={(v) => { botFilter = v; r() }} options={[["全部", t("manage:list.filterAll")], ["启用", t("manage:list.filterOn")], ["停用", t("manage:list.filterOff")]] as const} />}>
        {ks.length ? <ul className="grid gap-3 sm:grid-cols-2" data-bot-cards>{ks.map((k) => { const b = st.bots[k]; return (
          <BotCard key={k} as="li" k={k} off={!b.on} title={t("manage:bot.name", { name: b.n })} sub={b.role}
            meta={[b.h, b.m, t("manage:bot.running", { count: runOf(k) }), b.project && t("manage:list.defaultProject", { name: b.ws }), t("manage:bot.memories", { count: b.memCount }), b.workspaces.length > 1 && t("manage:list.inWorkspaces", { count: b.workspaces.length }), !b.on && t("manage:bot.disabled")]}
            action={<><SetSwitch testId={"bot-on-" + k} checked={b.on} onChange={(v) => void saveBot(k, { on: v }, v ? t("manage:list.enabledToast", { name: b.n }) : t("manage:list.disabledToast", { name: b.n }))} label={t("manage:bot.enableNamed", { name: b.n })} /><BotMenu k={k} /></>} />) })}</ul>
          : <Empty title={Object.keys(st.bots).length > 1 ? t("manage:list.noMatch") : t("manage:list.onlySteve")} sub={t("manage:list.emptySub")} />}
      </Section>
      {others.length > 0 && <Section label={t("manage:list.elsewhere", { n: others.length })}>
        <Panel>{others.map((b) => <Row key={b.id} title={t("manage:bot.name", { name: b.name })} sub={t("manage:list.inWorkspaces", { count: b.workspaces.length })}><Button size="xs" variant="outline" onClick={() => setBotWorkspaces(b.id, [...b.workspaces, st.workspace!.id])}><PlusIcon />{t("manage:list.joinHere")}</Button></Row>)}</Panel></Section>}
      </div>
    </Page>
  )
}
/** a Bot as affectUI's Card: its avatar, its name as the button that opens it (the whole card is the target), its role,
 *  and what it runs on underneath; the switch and the menu stay their own buttons above the card's target */
function BotCard({ k, title, sub, meta, action, off, size = 36, as }: { k: string; title: ReactNode; sub: string; meta: (string | false | null | undefined)[]; action: ReactNode; off?: boolean; size?: number; as?: "li" }) {
  const t = useT()
  return (
    <ACard as={as} interactive className="anim-in" data-bot-card={k} data-off={off || undefined}>
      <CardHeader icon={<BotAvatar k={k} size={size} className={cn("transition-[opacity,filter]", EASE, off && "opacity-40 grayscale")} />} action={action}
        title={<button type="button" onClick={() => go("#/bots/" + k)} data-bot-row={k}>{title}</button>}
        description={<span className="line-clamp-2">{sub || t("manage:detail.noRole")}</span>} />
      <Meta>{meta.filter(Boolean).map((x, i) => <span key={i}>{x}</span>)}</Meta>
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
  const t = useT()
  const b = st.bots[k]
  const [to, setTo] = useState("steve"), [mem, setMem] = useState<"删除" | "导出">("导出")
  if (!b) return null
  const mine = st.items.filter((i) => i.b === k && !closedSt(i.st))
  return (
    <AlertDialog open onOpenChange={(o) => !o && closeSheet()}>
      <AlertDialogContent className="sm:max-w-[480px]!">
        <AlertDialogHeader><AlertDialogTitle>{t("manage:del.title", { name: b.n })}</AlertDialogTitle>
          <AlertDialogDescription>{t("manage:del.desc", { count: mine.length })}</AlertDialogDescription></AlertDialogHeader>
        <FieldGroup className="gap-4">
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.handTo")}</FieldLabel>
            <SetSelect testId="del-to" label={t("manage:del.handTo")} value={to} onChange={setTo} options={Object.keys(st.bots).filter((x) => x !== k && st.bots[x].on).map((x) => ({ v: x, l: botName(x) }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.itsMemories", { count: b.memCount })}</FieldLabel><Segmented size="sm" value={mem} onChange={setMem} options={[["导出", t("manage:del.export")], ["删除", t("common:action.delete")]] as const} /></Field>
        </FieldGroup>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" data-del-bot-confirm onClick={() => { closeSheet(); go("#/bots"); void deleteBot(k, to, mem) }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
/* starter bots: [id, harness, model]; the name, role and rules come from manage:tpl in the UI language */
/* a template runs on the harness a new bot gets (the first that is ready) and its default model */
const TEMPLATES = ["review", "research", "writing"] as const
type TplId = (typeof TEMPLATES)[number]
const tplText = (t: T, id: TplId) => ({ name: t(`manage:tpl.${id}Name`), role: t(`manage:tpl.${id}Role`), rule: t(`manage:tpl.${id}Rule`) })
export function NewBotDialog() {
  const t = useT()
  const [mode, setMode] = useState<"tpl" | "desc" | "blank">("tpl"), [tpl, setTpl] = useState<TplId>(TEMPLATES[0]), [desc, setDesc] = useState(""), [busy, setBusy] = useState(false)
  const create = async () => {
    setBusy(true); let k: string | undefined
    if (mode === "tpl") { const x = tplText(t, tpl); k = await makeBot(x.name, x.role); if (k) await saveBot(k, { rule: x.rule }, "") }
    else if (mode === "blank") k = await makeBot(t("manage:newBot.blankName"), "")
    else k = await draftBot(desc.trim())
    setBusy(false); if (k) { closeSheet(); botTab = "设置"; go("#/bots/" + k) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-title">{t("manage:list.newBot")}</DialogTitle><DialogDescription className="type-callout">{t("manage:newBot.desc", { ws: st.workspace?.name })}</DialogDescription></DialogHeader>
        <AutonomyChoice testId="new-bot" label={t("manage:list.newBot")} value={mode} onChange={setMode} options={[{ value: "tpl", name: t("manage:newBot.fromTpl"), text: t("manage:newBot.fromTplSub"), icon: <LayoutTemplateIcon /> }, { value: "desc", name: t("manage:newBot.fromDesc"), text: t("manage:newBot.fromDescSub"), icon: <SparklesIcon /> }, { value: "blank", name: t("manage:newBot.blank"), text: t("manage:newBot.blankSub"), icon: <FilePlusIcon /> }]} />
        {mode === "tpl" && <Panel className="anim-in">{TEMPLATES.map((id) => { const x = tplText(t, id), h = defaultHarness(); return <Row key={id} title={x.name} sub={`${x.role} · ${runtimeName(h) ?? h}`} onClick={() => setTpl(id)} data-tpl={id}>{tpl === id && <CheckIcon className="size-4 text-blue" />}</Row> })}</Panel>}
        {mode === "desc" && <TextArea autoFocus className="anim-in" minRows={4} maxRows={10} aria-label={t("manage:newBot.fromDesc")} placeholder={t("manage:newBot.descPlaceholder")} value={desc} onChange={(e) => setDesc(e.target.value)} data-bot-desc />}
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={(mode === "desc" && !desc.trim()) || busy} onClick={() => void create()} data-testid="bot-go">{busy ? <Spinner className="size-3.5" /> : mode === "desc" && <SparklesIcon />}{mode === "desc" ? t("manage:newBot.draft") : t("manage:newBot.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ═════════ 头像：Agent Robot Avatar，颜色、头型、皮肤 ═════════ */
/* skin series: [id, label key under manage:series, first skin, count] */
const SERIES = [["", "plain", 0, 0], ["glaze", "glaze", 101, 6], ["ice", "ice", 201, 6], ["gummy", "gummy", 301, 12], ["candy", "candy", 401, 6], ["acrylic", "acrylic", 501, 12], ["jelly", "jelly", 601, 12], ["gradient", "gradient", 701, 12], ["frosted", "frosted", 801, 6], ["metal", "metal", 901, 30], ["gem", "gem", 1001, 12], ["wood", "wood", 1101, 6], ["ball", "ball", 1201, 10], ["gloss", "gloss", 1301, 5], ["fabric", "fabric", 1401, 12], ["food", "food", 1501, 12], ["nature", "nature", 1601, 12], ["pattern", "pattern", 1701, 17]] as const
/* head shapes: [label key under manage:avatar, roundness] */
const SHAPES = [["shapeSquare", 15], ["shapeRounded", 45], ["shapeSoft", 70], ["shapeRound", 95]] as const
function AvatarPicker({ look, onChange }: { look: { color: string; round: number; skin: string | null }; onChange: (p: Partial<{ color: string; round: number; skin: string | null }>) => void }) {
  const t = useT()
  const cur: string = look.skin ? SERIES.find(([, , a, n]) => +look.skin! >= a && +look.skin! < a + n)?.[0] ?? "" : ""
  const [series, setSeries] = useState(cur)
  const sr = SERIES.find((x) => x[0] === series)!
  return (
    <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:gap-6" data-avatar-picker>
      <Face look={look} size={88} />
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.color")}</span>
          {AVATAR_COLORS.map(([c, nm]) => <Tooltip key={c}><TooltipTrigger render={<button type="button" aria-label={nm} aria-pressed={look.color === c && !look.skin} data-color={c} onClick={() => onChange({ color: c, skin: null })}
            className={cn("size-6 rounded-full ring-offset-2 ring-offset-background transition-shadow", look.color === c && !look.skin ? "ring-2 ring-foreground/70" : "hover:ring-2 hover:ring-border")} style={{ background: c }} />} /><TooltipContent>{nm}</TooltipContent></Tooltip>)}
          <label className="relative size-6 cursor-pointer overflow-hidden rounded-full ring-1 ring-border" title={t("manage:avatar.customColor")} style={{ background: "conic-gradient(red, yellow, lime, aqua, blue, magenta, red)" }}>
            <input type="color" className="absolute inset-0 cursor-pointer opacity-0" value={look.color} onChange={(e) => onChange({ color: e.target.value, skin: null })} data-color-custom /></label>
        </div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.shape")}</span>
          <Strip className="min-w-0"><Segmented size="sm" value={SHAPES.reduce<(typeof SHAPES)[number]>((a, b) => (Math.abs(b[1] - look.round) < Math.abs(a[1] - look.round) ? b : a), SHAPES[0])[0]} onChange={(v) => onChange({ round: SHAPES.find((x) => x[0] === v)![1] })} options={SHAPES.map((x) => [x[0], t(`manage:avatar.${x[0]}`)] as const)} /></Strip>
          <input type="range" min={0} max={100} value={look.round} onChange={(e) => onChange({ round: +e.target.value })} className="w-28 accent-foreground" aria-label={t("manage:avatar.roundness")} data-round /></div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.skin")}</span>
          <SetSelect testId="skin-series" label={t("manage:avatar.skin")} value={series} onChange={(v) => { setSeries(v); if (!v) onChange({ skin: null }) }} options={SERIES.map(([id, n]) => ({ v: id, l: t(`manage:series.${n}`) }))} /></div>
        {sr[3] > 0 && <div className="flex flex-wrap gap-1.5 sm:pl-12" data-skins>{Array.from({ length: sr[3] }, (_, n) => String(sr[2] + n)).map((sk) => (
          <button key={sk} type="button" onClick={() => onChange({ skin: sk })} aria-pressed={look.skin === sk} data-skin={sk} className={cn("rounded-lg p-0.5 transition-colors", look.skin === sk ? "bg-accent ring-1 ring-border" : "hover:bg-accent")}><Face look={{ color: look.color, round: look.round, skin: sk }} size={30} /></button>))}</div>}
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
const EFFORT_KEYS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"] as const
const isEffortKey = (v: string): v is (typeof EFFORT_KEYS)[number] => (EFFORT_KEYS as readonly string[]).includes(v)
export function BotView({ k }: { k: string }) {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const b = st.bots[k]
  const [form, setForm] = useState<any>(null)
  const [prompt, setPrompt] = useState<{ generated: string; custom: string | null } | null>(null), [pdraft, setPdraft] = useState<string | null>(null)
  if (lastBot !== k) { lastBot = k; if (botTab !== "设置") botTab = "概览" }
  useEffect(() => { if (b && botTab === "记忆" && !b.memLoaded) void loadMemories(k) }, [k, botTab, b?.memLoaded])
  useEffect(() => { if (b && botTab === "系统提示词") api.botPrompt(k).then(setPrompt).catch(() => {}) }, [k, botTab, b?.prompt, b?.memCount])
  if (!b) return <Page title="Bot" back={["#/bots", "Bot"]}><Empty title={t("manage:detail.missing")} sub={t("manage:detail.missingSub")} /></Page>
  const fm = form && form.k === k ? form : { k, n: b.n, role: b.role, pers: b.pers, pro: PRO[b.pro], rule: b.rule, acc: b.acc, harness: b.harness, model: b.model ?? "", color: b.color, round: b.round, skin: b.skin, project: b.project ?? "none", permMode: b.permMode, ws: b.workspaces }
  const effort = fm.reasoningEffort ?? b.reasoningEffort ?? ""
  const set = (p: any) => setForm({ ...fm, reasoningEffort: effort, ...p })
  const mine = st.items.filter((i) => i.b === k && !closedSt(i.st) && !i.arch)
  const tabs = BOT_TABS.map(([v, l]) => [v, t(`manage:detail.${l}`)] as const)
  return (
    <Page back={["#/bots", "Bot"]} max={720} toolbarTop toolbar={<OwnerChip objectType="bot" objectId={k} />}
      lead={<Robot k={k} size={56} className="max-sm:size-12!" />}
      title={<span className="block truncate">{b.main ? b.n : t("manage:bot.name", { name: b.n })}</span>}
      sub={<div><div className="mb-1 text-muted-foreground">{b.role || t("manage:detail.noRole")}</div><Meta><span>{b.h}</span><span>{b.m}</span><span>{t("manage:detail.pers", { v: b.pers || t("manage:detail.unset") })}</span><span>{t("manage:detail.pro", { v: proLabel(t, b.pro) })}</span></Meta><RecentStats bot={k} className="mt-0.5 block type-footnote" /></div>}
      actions={<>{!b.main && <><span className="hidden type-footnote text-muted-foreground sm:inline">{b.on ? t("manage:bot.enabled") : t("manage:bot.disabled")}</span><SetSwitch testId="bot-on" checked={b.on} onChange={(v) => void saveBot(k, { on: v }, v ? t("manage:bot.enabled") : t("manage:bot.disabled"))} label={t("manage:bot.enable")} /></>}<BotMenu k={k} /></>}>
      <Strip className="mb-8" watch={botTab}><Segmented full className="min-w-max" value={botTab} onChange={(v) => { botTab = v; r() }} options={tabs} /></Strip>
      {botTab === "概览" && (mine.length ? <Panel className="anim-in">{mine.map((i) => <ItemRow key={i.id} i={i} bot={false} />)}</Panel> : <Empty title={t("manage:detail.noRunning")} />)}
      {/* the channels page's list, laid into this page's section and panel */}
      {botTab === "对外身份" && <div className="anim-in"><PersonasSection bot={k} panel /></div>}
      {botTab === "定时与流程" && <BotSchedules k={k} />}
      {botTab === "能力" && <BotCaps k={k} onPerm={() => { botTab = "设置"; r() }} />}
      {botTab === "系统提示词" && (
        <div className="flex flex-col gap-3 anim-in" data-prompt>
          <Segmented size="sm" value={b.prompt != null || pdraft != null ? "自己写" : "自动生成"} onChange={(v) => { if (v === "自己写") setPdraft(b.prompt ?? prompt?.generated ?? ""); else { setPdraft(null); if (b.prompt != null) void saveBot(k, { prompt: null }, t("manage:prompt.revertedToast")) } }} options={[["自动生成", t("manage:prompt.auto")], ["自己写", t("manage:prompt.custom")]] as const} />
          {b.prompt == null && pdraft == null
            ? <><pre className="whitespace-pre-wrap rounded-xl bg-muted/60 px-4 py-3.5 type-code text-foreground/90">{prompt?.generated ?? t("manage:prompt.loading")}</pre><Note className="mt-0">{t("manage:prompt.autoHint")}</Note></>
            : <><TextArea minRows={14} maxRows={40} inputClassName="type-code font-mono!" aria-label={t("manage:detail.tabPrompt")} value={pdraft ?? b.prompt ?? ""} onChange={(e) => setPdraft(e.target.value)} data-prompt-edit />
              <div className="flex items-center gap-2"><Button size="sm" disabled={pdraft == null || pdraft === b.prompt} onClick={() => { void saveBot(k, { prompt: pdraft ?? "" }); setPdraft(null) }}>{t("common:action.save")}</Button><span className="type-footnote text-muted-foreground">{t("manage:prompt.customHint")}</span></div></>}
        </div>
      )}
      {botTab === "记忆" && <BotMemories k={k} />}
      {botTab === "设置" && (
        <FieldGroup className="gap-8 anim-in">
          <AskSteve key={k} ctx={{ kind: "bot", id: k }} placeholder={t("manage:form.ask", { name: b.main ? b.n : t("manage:bot.name", { name: b.n }) })} />
          <Section label={t("manage:form.identity")}><Card className="flex flex-col gap-5 p-5">
            <F label={t("manage:form.avatar")} hint={t("manage:form.avatarHint")}><AvatarPicker look={{ color: fm.color, round: fm.round, skin: fm.skin }} onChange={(p) => set(p)} /></F>
            <F label={t("manage:form.name")} htmlFor="bot-name"><TextField id="bot-name" value={fm.n} onChange={(e) => set({ n: e.target.value })} data-bot-name /></F>
            <F label={t("manage:form.role")} htmlFor="bot-role" hint={t("manage:form.roleHint")}><TextField id="bot-role" value={fm.role} onChange={(e) => set({ role: e.target.value })} data-bot-role /></F>
          </Card></Section>
          <Section label={t("manage:form.personality")}><Card className="flex flex-col gap-5 p-5">
            <F label={t("manage:form.voice")} htmlFor="bot-voice" hint={t("manage:form.voiceHint")}><TextArea id="bot-voice" minRows={3} maxRows={10} value={fm.pers} onChange={(e) => set({ pers: e.target.value })} /></F>
            <F label={t("manage:form.pro")} hint={t("manage:form.proHint")}><Strip><Segmented size="sm" className="w-max" value={fm.pro} onChange={(v) => set({ pro: v })} options={PRO.map((v, i) => [v, proLabel(t, i)] as const)} /></Strip></F>
          </Card></Section>
          <Section label={t("manage:form.work")}><Card className="flex flex-col gap-5 p-5">
            <F label={t("manage:form.rule")} htmlFor="bot-rule" hint={t("manage:form.ruleHint")}><TextArea id="bot-rule" minRows={3} maxRows={12} value={fm.rule} onChange={(e) => set({ rule: e.target.value })} /></F>
          </Card></Section>
          {/* affectUI's Agent / Permissions: what it may do without asking, one choice each, said in plain words */}
          <Section label={t("manage:form.permSection")} data-bot-perms><AutonomyChoice testId="bot-perm" label={t("manage:form.permSection")} value={fm.permMode} onChange={(v) => set({ permMode: v })}
            options={permModes(fm.harness).map((v) => ({ value: v, name: permName(t, v), text: (PERM_KEYS as readonly string[]).includes(v) ? t(`manage:form.permText.${v as (typeof PERM_KEYS)[number]}`) : permLabel(t, v), icon: PERM_ICON[v] }))}
            means={<><ShieldAlertIcon aria-hidden />{t("manage:form.permHint")}</>} /></Section>
          <Section label={t("manage:form.acc")} data-bot-acc><AutonomyChoice testId="bot-acc" label={t("manage:form.acc")} value={fm.acc} onChange={(v) => set({ acc: v })}
            options={ACC_KEYS.map((key) => ({ value: ACC[key], name: accLabel(t, key), text: t(`manage:form.accText.${key}`), icon: ACC_ICON[key] }))} /></Section>
          <Section label={t("manage:form.run")}><Card className="flex flex-col gap-5 p-5">
            <div className="grid gap-4 sm:grid-cols-2">
              <F label="Harness"><SetSelect wide testId="bot-harness" label="Harness" value={fm.harness} onChange={(v) => set({ harness: v, model: "", reasoningEffort: "" })} options={st.runtimes.map((x) => ({ v: x.id, l: x.ok ? x.name : t("manage:form.unavailable", { name: x.name }) }))} /></F>
              <F label={t("manage:form.model")}><ModelPicker field harness={fm.harness} value={fm.model ?? ""} allowDefault testId="bot-model" onChange={(m) => set({ model: m })} /></F>
              <F label={t("manage:form.effort")} hint={b.main ? t("manage:form.effortHintMain") : t("manage:form.effortHint")}><SetSelect wide testId="bot-effort" label={t("manage:form.effort")} value={effort || "default"} onChange={(v) => set({ reasoningEffort: v === "default" ? "" : v })} options={[{ v: "default", l: t("manage:effort.default") }, ...harnessEfforts(fm.harness).map((v) => ({ v, l: isEffortKey(v) ? t(`manage:effort.${v}`) : v }))]} /></F>
            </div>
            {b.main && <SteveJudges harness={fm.harness} model={fm.model ?? ""} />}
            <F label={t("manage:form.device")} hint={t("manage:form.deviceHint")}><SetSelect wide testId="bot-dev" label={t("manage:form.device")} value="hub" onChange={() => {}} options={st.devices.map((d) => ({ v: d.id, l: d.name }))} /></F>
            <F label={t("manage:form.project")} hint={t("manage:form.projectHint")}><SetSelect wide testId="bot-project" label={t("manage:form.project")} value={fm.project} onChange={(v) => set({ project: v })} options={[{ v: "none", l: t("manage:form.projectInfer") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} /></F>
            {!b.main && <F label={t("manage:form.workspaces")} hint={t("manage:form.workspacesHint")}>
              <div className="flex flex-col gap-1.5">{st.workspaces.map((w) => <label key={w.id} className="flex items-center gap-2 type-callout"><Checkbox checked={fm.ws.includes(w.id)} disabled={fm.ws.length === 1 && fm.ws.includes(w.id)} onCheckedChange={(v) => set({ ws: v ? [...fm.ws, w.id] : fm.ws.filter((x: string) => x !== w.id) })} />{w.name}</label>)}</div></F>}
          </Card></Section>
          <div className="flex gap-2"><Button data-save-bot onClick={async () => {
            await saveBot(k, { n: fm.n.trim() || b.n, role: fm.role, pers: fm.pers, pro: PRO.indexOf(fm.pro), rule: fm.rule, acc: fm.acc, harness: fm.harness, model: fm.model || null, reasoningEffort: effort || null, color: fm.color, round: fm.round, skin: fm.skin, project: fm.project === "none" ? null : fm.project, permMode: fm.permMode })
            if (!b.main && JSON.stringify([...fm.ws].sort()) !== JSON.stringify([...b.workspaces].sort())) setBotWorkspaces(k, fm.ws)
            setForm(null); botTab = "概览"; r() }}>{t("common:action.save")}</Button><Button variant="ghost" onClick={() => { setForm(null); botTab = "概览"; r() }}>{t("common:action.cancel")}</Button></div>
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
  const b = st.bots[k]
  const [gone, setGone] = useState<string[]>([])
  const timers = useRef(new Map<string, number>())
  /* leaving the view, or closing or reloading the page, deletes what is still waiting for its Undo */
  useEffect(() => {
    const flush = () => { for (const [id, h] of timers.current) { clearTimeout(h); delMem(k, id) } timers.current.clear() }
    addEventListener("pagehide", flush)
    return () => { removeEventListener("pagehide", flush); flush() }
  }, [k])
  if (!b) return null
  const general = t("core:affect.memory.general")
  const memories: Memory[] = b.mem.filter(([, , id]) => !gone.includes(id)).map(([text, source, id]) => ({ id, text, source, category: general, canPause: false }))
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
      {!b.memLoaded ? <Loading className="py-8" /> :
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
  ACCESS_CAT,
  VAULT_CAT,
  STATS_CAT,
  /* the audit trail names every member, client and token: the Hub answers the instance's owner only */
  ...(st.owner ? [AUDIT_CAT] : []),
  ["data", t("manage:settings.catData"), DatabaseIcon, t("manage:settings.catDataSub")],
  ["about", t("manage:settings.catAbout"), InfoIcon, t("manage:settings.catAboutSub")],
]
const PH_KEYS: Record<string, "phGeneral" | "phItems" | "phTags" | "phModels" | "phNotify" | "phLook" | "phData"> = {
  general: "phGeneral", items: "phItems", tags: "phTags", models: "phModels", notify: "phNotify", look: "phLook", data: "phData",
}
const Sel = ({ v, o, on, testId, label }: { v: string; o: [string, string][]; on: (v: string) => void; testId?: string; label?: string }) => <SetSelect testId={testId} label={label} value={v} options={o} onChange={on} />
/** a row title with the Later pill after it, which moves to its own line when the two do not fit (phones) */
const Soon = ({ children }: { children: ReactNode }) => { const t = useT(); return <span className="flex flex-wrap items-center gap-x-2 gap-y-1">{children}<Pill>{t("manage:settings.later")}</Pill></span> }
export function SettingsView() {
  const t = useT()
  const [, cat] = useRoute()
  const wide = typeof window !== "undefined" && window.innerWidth >= 768
  const cur = cat ?? (wide ? "general" : "")
  if (["runtime", "accounts", "plugins"].includes(cur)) { setTimeout(() => go(cur === "runtime" ? "#/devices" : cur === "accounts" ? "#/accounts" : "#/plugins")); return null }
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
/* Steve's own tools every Bot has (steve-mcp tools.json), with what each is for; other steve_* tools show once used */
const STEVE_TOOLS = [["steve_update", "toolUpdate", MessageSquareTextIcon], ["steve_ask", "toolAsk", MessageCircleQuestionIcon], ["steve_request_approval", "toolApproval", ShieldCheckIcon], ["steve_deliver", "toolDeliver", PackageCheckIcon], ["steve_publish", "toolPublish", RocketIcon], ["steve_lane_status", "toolLaneStatus", GitMergeIcon]] as const
/** 能力: what a Bot can call, as the tiles of affectUI's Agent / Skills screen — Steve's tools (always there), tools from
 *  other MCP servers and the harness's own, each with how often this Bot called it in the last 30 days (GET
 *  /stats/breakdown?by=tool&bot=). The Hub counts Steve's MCP tools as `mcp:<name>` (dim mcp) and every other tool as
 *  `tool:<full name>` (dim tool), so another MCP server's tools arrive as `mcp__<server>__<tool>` among the harness's.
 *  Harness tools are known only once used: Steve has no list of them. */
const OTHER_MCP = /^mcp__(.+?)__(.+)$/
function BotCaps({ k, onPerm }: { k: string; onPerm: () => void }) {
  const t = useT()
  const b = st.bots[k]
  const [d] = useStats(() => stats.counts({ range: "30d", bot: k }, "tool"), [k])
  if (!b) return null
  const rows = d.s === "ok" ? d.v.rows.filter((x) => x.dim === "tool" || x.dim === "mcp") : []
  const count = new Map(rows.filter((x) => x.dim === "mcp").map((x) => [x.label, x.count]))
  const uses = (n: number | undefined) => (d.s !== "ok" ? undefined : n ? t("manage:caps.uses", { count: n }) : t("manage:caps.unused"))
  const catSteve = t("manage:caps.catSteve"), catMcp = t("manage:caps.catMcp")
  const known = new Set<string>(STEVE_TOOLS.map(([c]) => c))
  const byUse = (xs: (Skill & { n: number })[]) => xs.sort((x, y) => y.n - x.n)
  const steve = byUse([
    ...STEVE_TOOLS.map(([c, l, I]) => { const used = count.get(c); return { id: c, name: c, description: t(`manage:caps.${l}`), category: catSteve, icon: <I />, tone: "blue", source: catSteve, uses: d.s === "ok" && !used ? `${t("manage:caps.always")} · ${t("manage:caps.unused")}` : uses(used), idle: d.s === "ok" && !used, n: used ?? 0 } }),
    ...rows.filter((x) => x.dim === "mcp" && !known.has(x.label)).map((x) => ({ id: x.key, name: x.label, description: t("manage:caps.fromSteve"), category: catSteve, icon: <WrenchIcon />, tone: "blue", source: catSteve, uses: uses(x.count), n: x.count })),
  ])
  const tools = rows.filter((x) => x.dim === "tool")
  const mcp = byUse(tools.flatMap((x) => { const m = OTHER_MCP.exec(x.label); return m ? [{ id: x.key, name: m[2], description: t("manage:caps.fromMcp"), category: catMcp, icon: <PlugIcon />, tone: "amber", source: m[1], uses: uses(x.count), n: x.count }] : [] }))
  const own = byUse(tools.filter((x) => !OTHER_MCP.test(x.label)).map((x) => ({ id: x.key, name: x.label, description: t("manage:caps.fromHarness", { harness: b.h }), category: b.h, icon: <TerminalIcon />, tone: "teal", source: b.h, uses: uses(x.count), n: x.count })))
  const skills: Skill[] = [...steve, ...mcp, ...own].map(({ n: _n, ...s }) => s)
  return (
    <div className="flex flex-col gap-8 anim-in" data-caps>
      <Section label={t("manage:caps.tools")} note={d.s === "absent" ? t("manage:caps.usageAbsent") : t("manage:caps.usageNote", { harness: b.h })}>
        {d.s === "loading" ? <Loading className="py-8" /> : <SkillGrid skills={skills} categories={[catSteve, catMcp, b.h]} labels={{ all: t("manage:caps.all"), categories: t("manage:caps.categories"), empty: t("manage:caps.none") }} />}
      </Section>
      <BotMcpSection bot={k} />
      <Section label={t("manage:caps.perm")} note={t("manage:caps.later")}>
        <Panel><Row title={permName(t, b.permMode)} sub={permLabel(t, b.permMode)} data-caps-perm={b.permMode}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={onPerm}>{t("manage:caps.change")}</Button></Row></Panel></Section>
      <Section label={t("manage:mcp.more")}><PlannedCaps /></Section>
    </div>
  )
}
/** a Bot's schedules, read from 自动化 where they are made and changed */
function BotSchedules({ k }: { k: string }) {
  const t = useT()
  const L = useFlowLoad(() => flowApi.schedules(), [], ["flow", "run"])
  if (L.unwired) return <Empty title={t("manage:detail.scheduleEmpty")} sub={t("manage:detail.scheduleEmptySub")} />
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
  if (id === "access") return <AccessSettingsPane />
  if (id === "general") return <div className="flex flex-col gap-8">
    <Section label={t("manage:general.assistant")}><Panel><Row icon={<BotAvatar k="steve" size={36} />} title={st.bots.steve?.n} sub={t("manage:general.assistantSub")} onClick={() => go("#/bots/steve")}><Chevron /></Row></Panel></Section>
    <Panel>
      <Row title={t("manage:general.home")} sub={t("manage:general.homeSub")}><Sel label={t("manage:general.home")} testId="home" v={c.home} o={[["对话", t("manage:general.homeChat")], ["待处理", t("manage:general.homeInbox")], ["事项", t("manage:general.homeItems")]]} on={(v) => setSetting({ home: v })} /></Row>
      <Row title={t("manage:general.away")} sub={t("manage:general.awaySub")}><Sel label={t("manage:general.away")} testId="away" v={c.awayH} o={[...[2, 4, 8].map((h): [string, string] => [String(h), t("common:dur.hours", { count: h })]), ["0", t("manage:general.awayNever")]]} on={(v) => setSetting({ awayH: v })} /></Row>
    </Panel>
    <Panel>
      <Row title={t("manage:general.onboard")} sub={t("manage:general.onboardSub")}><Button size="sm" variant="outline" onClick={() => { localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }} data-restart-onboard>{t("manage:general.restart")}</Button></Row>
      <Row title={t("manage:general.shortcuts")} sub={t("manage:general.shortcutsSub")} />
    </Panel>
  </div>
  if (id === "items") return <div className="flex flex-col gap-8">
    <Section label={t("manage:items.section")}><Panel>
      <Row title={t("manage:items.accDefault")} sub={t("manage:items.accDefaultSub")}><Segmented size="sm" value={c.accDefault === "manual" ? "manual" : "auto"} onChange={(v) => setSetting({ accDefault: v })} options={[["manual", t("manage:acc.manual")], ["auto", t("manage:acc.auto")]] as const} /></Row>
      <Row title={t("manage:items.prefix")} sub={t("manage:items.prefixSub")}><span className="font-mono type-callout">{st.prefix}-{st.noSeq + 1}</span></Row>
      <Row title={t("manage:items.depsAfter")}><Sel label={t("manage:items.depsAfter")} testId="deps-after" v={c.depsAfter} o={[["自动开始", t("manage:items.depsAuto")], ["提醒我开始", t("manage:items.depsRemind")]]} on={(v) => setSetting({ depsAfter: v })} /></Row>
      <Row title={t("manage:items.archDays")} sub={t("manage:items.archDaysSub")}><Sel label={t("manage:items.archDays")} testId="arch-days" v={c.archDays} o={[...[7, 30, 90].map((d): [string, string] => [String(d), t("common:dur.days", { count: d })]), ["0", t("manage:items.archNever")]]} on={(v) => setSetting({ archDays: v })} /></Row></Panel></Section>
    <Section label={t("manage:items.sessions")}><Panel>
      <Row title={t("manage:items.handoff")} sub={t("manage:items.handoffSub")}><Sel label={t("manage:items.handoff")} testId="handoff" v={c.handoff ?? "0"} o={[["0", t("manage:items.handoffOff")], ["70", "70%"], ["80", "80%"], ["90", "90%"]]} on={(v) => setSetting({ handoff: v })} /></Row></Panel></Section>
  </div>
  if (id === "notify") return <NotifyPane />
  if (id === "look") return <div className="flex flex-col gap-8">
    <Section label={t("manage:look.avatars")} note={t("manage:look.avatarsHint")}>
      <Card className="flex flex-wrap items-start gap-2 p-3" data-avatar-preview>{Object.keys(st.bots).map((k) => <a key={k} href={"#/bots/" + k} className="flex w-20 flex-col items-center gap-1.5 rounded-lg px-1 py-2 hover:bg-accent"><Robot k={k} size={44} /><span className="max-w-full truncate type-footnote text-muted-foreground">{st.bots[k].n}</span></a>)}</Card></Section>
    <Section label={t("manage:look.ui")}><Panel>
      <Row title={t("manage:look.theme")} sub={t("manage:look.themeSub")}><ThemeSwitch labels /></Row>
      <LanguageRow />
      <Row title={t("manage:look.fontSize")}><Segmented size="sm" value={c.fontSize} onChange={(v) => setSetting({ fontSize: v })} options={[["标准", t("manage:look.fontNormal")], ["大", t("manage:look.fontLarge")]]} /></Row>
      <Row title={t("manage:look.reduceMotion")} sub={t("manage:look.reduceMotionSub")}><SetSwitch testId="reduce-motion" label={t("manage:look.reduceMotion")} checked={c.reduceMotion === "开"} onChange={(v) => setSetting({ reduceMotion: v ? "开" : "关" })} /></Row>
    </Panel></Section>
    <AppearanceSection /></div>
  if (id === "data") return <div className="flex flex-col gap-8">
    <HubData />
    <WorkspaceDisk />
    <OutboxDead />
    <Section label={t("manage:data.keep")}><Panel>
      {/* read-only: whole logs are kept for good; a shorter keepSess (set by a confirmed config draft) compacts archived sessions */}
      <Row title={t("manage:data.sessions")} sub={c.keepSess && c.keepSess !== "永久" ? t("manage:data.sessionsCompact", { period: c.keepSess === "1 年" ? t("manage:data.keepYear") : t("common:dur.days", { count: 90 }) }) : t("manage:data.sessionsForever")} data-keep-sess={c.keepSess || "永久"}>
        <span className="type-callout text-muted-foreground">{c.keepSess === "1 年" ? t("manage:data.keepYear") : c.keepSess === "90 天" ? t("common:dur.days", { count: 90 }) : t("manage:data.keepForever")}</span></Row></Panel></Section>
    <Section label={t("manage:data.danger")} data-danger-zone><Panel className="shadow-[var(--lift),0_0_0_1px_color-mix(in_srgb,var(--destructive)_18%,transparent)]">
      <DangerRow testId="clear-mem" title={t("manage:data.clearMem")} sub={t("manage:data.clearMemSub")} label={t("manage:data.clear")} what={t("manage:data.clearWhat")} onOk={() => void api.clearMemories().then(() => toast(t("manage:data.cleared")))} /></Panel></Section>
  </div>
  if (id === "about") return <div className="flex flex-col gap-8">
    <AboutVersions />
    <Section label={t("manage:about.release")}><Panel>
    <Row title={t("manage:about.publish")} sub={t("manage:about.publishSub")} />
    <Row title={t("manage:about.onboard")} sub={t("manage:about.onboardSub")}><Button size="sm" variant="outline" onClick={() => { finishOnboarding(); localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }}>{t("manage:about.start")}</Button></Row>
    </Panel></Section>
  </div>
  return null
}
/* the settings-notifications pattern of affectUI: each kind of message is a switch, and its time unfolds under it
 * while it is on. The stored values stay the Hub's (settings.rs): a time, or 不发 / 关闭 for off. */
const DIGEST_AT = ["08:30", "09:00"], QUIET = ["23:00 – 08:00", "22:00 – 09:00"]
function NotifyPane() {
  const t = useT()
  const c = st.settings
  const digest = c.digestAt !== "不发", quiet = c.quiet !== "关闭"
  /* turning a switch back on brings back the time it had, not the first option */
  const last = useRef({ digestAt: digest ? c.digestAt : DIGEST_AT[0], quiet: quiet ? c.quiet : QUIET[0] })
  if (digest) last.current.digestAt = c.digestAt
  if (quiet) last.current.quiet = c.quiet
  return <div className="flex flex-col gap-8">
    <Notice>{t("manage:notify.none")}</Notice>
    <Section label={t("manage:notify.digest")}><Panel data-notify-digest>
      <Row title={<Soon>{t("manage:notify.digestDaily")}</Soon>} sub={t("manage:notify.digestSub")}><SetSwitch testId="digest" label={t("manage:notify.digestDaily")} checked={digest} onChange={(v) => setSetting({ digestAt: v ? last.current.digestAt : "不发" })} /></Row>
      {digest && <Row className="anim-in" title={t("manage:notify.digestTime")}><Sel testId="digest-at" label={t("manage:notify.digestTime")} v={c.digestAt} o={DIGEST_AT.map((x): [string, string] => [x, x])} on={(v) => setSetting({ digestAt: v })} /></Row>}
    </Panel></Section>
    <Section label={t("manage:notify.quiet")}><Panel data-notify-quiet>
      <Row title={<Soon>{t("manage:notify.quietOn")}</Soon>} sub={t("manage:notify.quietSub")}><SetSwitch testId="quiet" label={t("manage:notify.quietOn")} checked={quiet} onChange={(v) => setSetting({ quiet: v ? last.current.quiet : "关闭" })} /></Row>
      {quiet && <Row className="anim-in" title={t("manage:notify.quietHours")}><Sel testId="quiet-hours" label={t("manage:notify.quietHours")} v={c.quiet} o={QUIET.map((x): [string, string] => [x, x])} on={(v) => setSetting({ quiet: v })} /></Row>}
    </Panel></Section>
  </div>
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
  useEffect(() => {
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
      <Row className={stack} title={t("manage:ws.prefix")} sub={t("manage:ws.prefixHint", { example: `${prefix || "ST"}-${st.noSeq + 1}` })}>
        <TextField size="sm" className="w-full sm:w-28" inputClassName="font-mono uppercase" value={p} maxLength={6} spellCheck={false} aria-label={t("manage:ws.prefix")} error={unsaved && prefix !== w.item_prefix ? unsaved : undefined} onChange={(e) => setP(e.target.value)} onBlur={save} onKeyDown={keys(() => setP(w.item_prefix))} data-prefix /></Row>
    </Panel></Section>
    <Section label={t("manage:ws.desc")}>
      <TextArea minRows={3} maxRows={10} value={d} aria-label={t("manage:ws.desc")} placeholder={t("manage:ws.descPlaceholder")} hint={t("manage:ws.descHint")} error={unsaved && d.trim() !== w.description ? unsaved : undefined} onChange={(e) => setD(e.target.value)} onBlur={save} onKeyDown={keys(() => setD(w.description))} data-ws-desc /></Section>
    <Section label={t("manage:ws.here")}><Panel>
      <Row icon={<Tile I={FolderIcon} />} title={t("manage:ws.projects", { count: st.projects.length })} onClick={() => go("#/projects")}><Chevron /></Row>
      <Row icon={<Tile I={BotIcon} />} title={t("manage:ws.bots", { count: Object.keys(st.bots).length })} onClick={() => go("#/bots")}><Chevron /></Row>
      <Row icon={<Tile I={ListTodoIcon} />} title={t("manage:ws.items", { count: st.items.length })} onClick={() => go("#/items")}><Chevron /></Row></Panel></Section>
    <Section label={t("manage:ws.all", { n: st.workspaces.length })}>
      <Panel>{st.workspaces.map((x) => <Row key={x.id} title={x.name} sub={x.description || undefined}>{x.id === w.id ? <Pill tone="info">{t("manage:ws.current")}</Pill> : <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void import("@/store").then((m) => m.switchWorkspace(x.id))}>{t("manage:ws.switch")}</Button>}</Row>)}</Panel>
      <Button size="sm" variant="outline" className="mt-3" onClick={() => openSheet("newWorkspace")}><PlusIcon />{t("manage:ws.new")}</Button></Section>
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
/* the items whose category Steve filled in unsure: switch it or confirm it, then they leave the list */
function KindsReview() {
  const t = useT()
  const [xs, setXs] = useState<RKindReview[] | null>(null), [busy, setBusy] = useState(false)
  useEffect(() => { void loadKindsReview().then(setXs) }, [])
  const done = async (ids: string[]) => { const left = await kindsReviewed(ids); if (left) setXs(left) }
  const rows = (xs ?? []).filter((x) => st.items.some((i) => i.id === x.id))
  return <Section data-kinds-review right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={async () => { setBusy(true); await kindsBackfill(); setXs(await loadKindsReview()); setBusy(false) }} data-kinds-backfill>{t("manage:kinds.backfill")}</Button>}
    label={rows.length ? t("manage:kinds.pending", { n: rows.length }) : t("manage:kinds.fill")}>
    {rows.length ? <>
      <p className="mb-3 px-1 type-callout text-muted-foreground">{t("manage:kinds.intro")}</p>
      <Panel>{rows.map((x) => { const i = it(x.id); return (
        <Row key={x.id} title={<span className="inline-flex min-w-0 items-center gap-2"><ItemNo id={i.id} /><span className="truncate">{i.t}</span></span>} sub={x.why || undefined} data-kind-review={x.id}>
          <Select quiet testId={"review-kind-" + x.id} value={kindOf(i) ?? ""} onChange={(k) => { setKind(i.id, k); void done([x.id]) }} options={kindTags().map((kt) => ({ v: kt.id, l: <KindTag id={kt.id} className="border-0 bg-transparent px-0" /> }))} />
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void done([x.id])} data-kind-ok={x.id}>{t("manage:kinds.ok")}</Button>
        </Row>) })}</Panel>
      {rows.length > 1 && <div className="mt-2 flex justify-end"><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void done(rows.map((x) => x.id))} data-kind-ok-all>{t("manage:kinds.okAll")}</Button></div>}
    </> : <Blank>{xs ? t("manage:kinds.allSet") : t("manage:kinds.reading")}</Blank>}
  </Section>
}
function TagsPane() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [del, setDel] = useState<string | null>(null), [merge, setMerge] = useState<string | null>(null), [into, setInto] = useState("")
  const cats = ["全部", ...TAG_CATS.filter((c) => st.tags.some((tg) => tg.cat === c))]
  const d = del ? tag(del) : null, m = merge ? tag(merge) : null
  const domain = st.tags.filter((x) => !isKind(x))
  return <div className="flex flex-col gap-8" data-tags-pane>
    <KindsReview />
    <div className="flex flex-wrap items-center gap-2"><Segmented size="sm" value={cats.includes(tagCat) ? tagCat : "全部"} onChange={(v) => { tagCat = v; r() }} options={cats.map((c) => [c, catLabel(t, c)] as const)} /><span className="flex-1" /><Button size="sm" onClick={() => openSheet("tag", {})} data-new-tag-btn><PlusIcon />{t("manage:tags.new")}</Button></div>
    {!st.tags.length && <Empty title={t("manage:tags.empty")} sub={t("manage:tags.emptySub")} />}
    {TAG_CATS.filter((c) => tagCat === "全部" || c === tagCat).map((c) => { const kind = c === KIND_CAT; const xs = kind ? kindTags() : st.tags.filter((tg) => tg.cat === c); return xs.length ? (
      <Section key={c} label={<>{kind ? t("manage:tags.kindSection") : catLabel(t, c)} <span className="tabular-nums">· {xs.length}</span></>} note={kind ? t("manage:tags.intro") : undefined}>
        <Panel>{xs.map((tg) => (
          <Row key={tg.id} title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5">{kind ? <KindTag id={tg.id} /> : <span className="font-medium">{tagLabel(tg)}</span>}<span className="type-footnote tabular-nums text-muted-foreground">{t("manage:tags.uses", { count: tagUses(tg.id) })}</span>{kind ? <Pill>{t("manage:tags.builtin")}</Pill> : tg.by === "Steve" && <span className="type-footnote text-tertiary">{t("manage:tags.bySteve", { created: tg.created })}</span>}</span>} sub={tagDesc(tg)} data-tag-row={tg.name}>
            <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-tag-menu={tg.name} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-40">
                <DropdownMenuItem onClick={() => showTag(tg.id)} data-show-tag>{t("manage:tags.showItems")}</DropdownMenuItem>
                <DropdownMenuItem onClick={() => openSheet("tag", { id: tg.id })} data-edit-tag><PencilIcon />{kind ? t("manage:tags.editDesc") : t("common:action.edit")}</DropdownMenuItem>
                {!kind && domain.length > 1 && <DropdownMenuItem onClick={() => { setMerge(tg.id); setInto(domain.find((x) => x.id !== tg.id && x.cat === tg.cat)?.id ?? domain.find((x) => x.id !== tg.id)!.id) }} data-merge-tag>{t("manage:tags.mergeTo")}</DropdownMenuItem>}
                {!kind && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(tg.id)} data-del-tag><TrashIcon />{t("common:action.delete")}</DropdownMenuItem></>}
              </DropdownMenuContent></DropdownMenu>
          </Row>))}</Panel></Section>) : null })}
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
          <F label={t("manage:tagDialog.cat")} hint={kind ? t("manage:tagDialog.catBuiltin") : undefined}><div>{kind ? <span className="type-callout text-muted-foreground">{t("manage:tagDialog.kind")}</span> : <Select testId="tag-cat" value={c} onChange={setC} options={DOMAIN_CATS.map((x) => ({ v: x, l: catLabel(t, x) }))} />}</div></F>
          <F label={t("manage:tagDialog.descLabel")} htmlFor="tag-desc"><Textarea id="tag-desc" className="min-h-20" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("manage:tagDialog.descPlaceholder")} data-tag-desc /></F>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={!ok} onClick={() => void save()} data-testid="tag-go">{tg ? t("common:action.save") : t("manage:tagDialog.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
