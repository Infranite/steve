/* Bot 菜单与设置：Bot 是独立菜单；设置是分类页。所有改动都写到 Hub，也都可以直接告诉 Steve。 */
import { useT } from "@/i18n"
import { Trans } from "react-i18next"
import { WorkspaceDisk } from "@/disk-settings"
import { OutboxDead } from "@/outbox-settings"
import { useEffect, useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, MoreHorizontalIcon, CopyIcon, TrashIcon, SparklesIcon, SlidersHorizontalIcon, ListChecksIcon, TagIcon, BellIcon, SunMoonIcon, DatabaseIcon, InfoIcon, PencilIcon, CheckIcon, LayersIcon, FolderIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Checkbox } from "@/components/ui/checkbox"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Robot, Face, AVATAR_COLORS } from "@/robot"
import {
  st, botName, PRO, ACC, PERM, HARNESS, saveBot, makeBot, draftBot, deleteBot, dupBot, addMem, editMem, delMem, loadMemories, setBotWorkspaces, tag, tagUses, TAG_CATS, KIND_CAT, DOMAIN_CATS, isKind, kindOf, kindTags, setKind, loadKindsReview, kindsReviewed, kindsBackfill, makeTag, saveTag, deleteTag, mergeTag, setTags, it,
  setSetting, toast, finishOnboarding, emit,
} from "@/store"
import { api, type RKindReview } from "@/api"
import { go, Page, BotAvatar, Group, ItemNo, KindTag, Row, SectionLabel, Segmented, Meta, Empty, openSheet, closeSheet, ThemeSwitch, LanguageSwitch, useRoute } from "@/ui"
import { PersonasSection } from "@/channels/persona"
import { OwnerChip } from "@/people/owner-chip"
import { Select, ChoiceCards, ItemRowExport as ItemRow } from "@/view-shared"
import { showTag } from "@/views"
import { AskSteve } from "@/config-ui"
import { VAULT_CAT, VaultSettingsPane } from "@/vault/settings"
import { STATS_CAT, StatsSettingsPane } from "@/stats/prices"
import { MODELS_CAT, ModelsSettingsPane, SteveJudges } from "@/models-settings"
import { ModelPicker } from "@/model-picker"
import { RecentStats } from "@/stats/drill"
import { AboutVersions } from "@/version"

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
/* tag categories are stored as Chinese words (store TAG_CATS); only the label is translated */
const TAG_CAT_KEYS: Record<string, "all" | "project" | "kind" | "domain" | "client" | "topic"> = { 全部: "all", 项目: "project", 类型: "kind", 领域: "domain", 客户: "client", 主题: "topic" }
const catLabel = (t: T, c: string) => (TAG_CAT_KEYS[c] ? t(`manage:tagCat.${TAG_CAT_KEYS[c]}`) : c)

export function BotsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const ks = Object.keys(st.bots).filter((k) => !st.bots[k].main && (botFilter === "全部" || (botFilter === "启用") === st.bots[k].on))
  const S = st.bots.steve
  const runOf = (k: string) => st.items.filter((i) => i.b === k && ["run", "wait", "block"].includes(i.st) && !i.arch).length
  const others = st.allBots.filter((b) => !st.bots[b.id])
  return (
    <Page title="Bot" sub={t("manage:list.sub")} max={720}
      actions={<Button size="sm" onClick={() => openSheet("newBot")} data-new-bot><PlusIcon />{t("manage:list.newBot")}</Button>}>
      {S && <section className="mb-8"><SectionLabel>{t("manage:list.main")}</SectionLabel>
        <Group><Row icon={<BotAvatar k="steve" size={44} />} onClick={() => go("#/bots/steve")} data-bot-row="steve"
          title={<span className="type-headline">{S.n}</span>} sub={t("manage:list.mainSub", { role: S.role, harness: S.h, model: S.m, count: runOf("steve") })}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
      </section>}
      <section>
        <SectionLabel right={<Segmented size="sm" value={botFilter} onChange={(v) => { botFilter = v; r() }} options={[["全部", t("manage:list.filterAll")], ["启用", t("manage:list.filterOn")], ["停用", t("manage:list.filterOff")]] as const} />}>{t("manage:list.others", { ws: st.workspace?.name, n: Object.keys(st.bots).length - 1 })}</SectionLabel>
        {ks.length ? <Group>{ks.map((k) => { const b = st.bots[k]; return (
          <div key={k} className="group/bot flex items-center gap-3 rounded-lg px-2 py-2.5 transition-colors hover:bg-accent">
            <BotAvatar k={k} size={36} className={cn(!b.on && "opacity-50")} />
            <button type="button" className="min-w-0 flex-1 text-left" onClick={() => go("#/bots/" + k)} data-bot-row={k}>
              <div className="type-body">{t("manage:bot.name", { name: b.n })} <span className="text-muted-foreground">· {b.role}</span></div>
              <Meta className="mt-0.5"><span>{b.h} · {b.m}</span><span>{t("manage:bot.running", { count: runOf(k) })}</span>{b.project && <span>{t("manage:list.defaultProject", { name: b.ws })}</span>}<span>{t("manage:bot.memories", { count: b.memCount })}</span>{b.workspaces.length > 1 && <span>{t("manage:list.inWorkspaces", { count: b.workspaces.length })}</span>}{!b.on && <span>{t("manage:bot.disabled")}</span>}</Meta>
            </button>
            <Switch checked={b.on} onCheckedChange={(v) => void saveBot(k, { on: v }, v ? t("manage:list.enabledToast", { name: b.n }) : t("manage:list.disabledToast", { name: b.n }))} aria-label={t("manage:bot.enableNamed", { name: b.n })} />
            <BotMenu k={k} />
          </div>) })}</Group> : <Empty title={Object.keys(st.bots).length > 1 ? t("manage:list.noMatch") : t("manage:list.onlySteve")} sub={t("manage:list.emptySub")} />}
        {others.length > 0 && <div className="mt-6"><SectionLabel>{t("manage:list.elsewhere", { n: others.length })}</SectionLabel>
          <Group>{others.map((b) => <Row key={b.id} title={t("manage:bot.name", { name: b.name })} sub={t("manage:list.inWorkspaces", { count: b.workspaces.length })}><Button size="xs" variant="outline" onClick={() => setBotWorkspaces(b.id, [...b.workspaces, st.workspace!.id])}><PlusIcon />{t("manage:list.joinHere")}</Button></Row>)}</Group></div>}
        <div className="mt-3 px-1 type-footnote text-muted-foreground"><Trans ns="manage" i18nKey="list.hint" components={{ code: <span className="font-mono" /> }} /></div>
      </section>
    </Page>
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
  const mine = st.items.filter((i) => i.b === k && !["done", "drop"].includes(i.st))
  return (
    <AlertDialog open onOpenChange={(o) => !o && closeSheet()}>
      <AlertDialogContent className="sm:max-w-[480px]!">
        <AlertDialogHeader><AlertDialogTitle>{t("manage:del.title", { name: b.n })}</AlertDialogTitle>
          <AlertDialogDescription>{t("manage:del.desc", { count: mine.length })}</AlertDialogDescription></AlertDialogHeader>
        <FieldGroup className="gap-4">
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.handTo")}</FieldLabel>
            <Select align="end" testId="del-to" value={to} onChange={setTo} options={Object.keys(st.bots).filter((x) => x !== k && st.bots[x].on).map((x) => ({ v: x, l: botName(x) }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("manage:del.itsMemories", { count: b.memCount })}</FieldLabel><Segmented size="sm" value={mem} onChange={setMem} options={[["导出", t("manage:del.export")], ["删除", t("common:action.delete")]] as const} /></Field>
        </FieldGroup>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" data-del-bot-confirm onClick={() => { closeSheet(); go("#/bots"); void deleteBot(k, to, mem) }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
/* starter bots: [id, harness, model]; the name, role and rules come from manage:tpl in the UI language */
const TEMPLATES = [["review", "claude-code", "claude-opus-5-5"], ["research", "claude-code", "claude-opus-5-5"], ["writing", "claude-code", "claude-opus-5-5"]] as const
type TplId = (typeof TEMPLATES)[number][0]
const tplText = (t: T, id: TplId) => ({ name: t(`manage:tpl.${id}Name`), role: t(`manage:tpl.${id}Role`), rule: t(`manage:tpl.${id}Rule`) })
export function NewBotDialog() {
  const t = useT()
  const [mode, setMode] = useState<"tpl" | "desc" | "blank">("tpl"), [tpl, setTpl] = useState<TplId>(TEMPLATES[0][0]), [desc, setDesc] = useState(""), [busy, setBusy] = useState(false)
  const create = async () => {
    setBusy(true); let k: string | undefined
    if (mode === "tpl") { const [id, h, m] = TEMPLATES.find((x) => x[0] === tpl)!, x = tplText(t, id); k = await makeBot(x.name, x.role, h, m); if (k) await saveBot(k, { rule: x.rule }, "") }
    else if (mode === "blank") k = await makeBot(t("manage:newBot.blankName"), "")
    else k = await draftBot(desc.trim())
    setBusy(false); if (k) { closeSheet(); botTab = "设置"; go("#/bots/" + k) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-headline">{t("manage:list.newBot")}</DialogTitle><DialogDescription className="type-callout">{t("manage:newBot.desc", { ws: st.workspace?.name })}</DialogDescription></DialogHeader>
        <ChoiceCards value={mode} onChange={setMode} cols={1} options={[{ v: "tpl", t: t("manage:newBot.fromTpl"), d: t("manage:newBot.fromTplSub") }, { v: "desc", t: t("manage:newBot.fromDesc"), d: t("manage:newBot.fromDescSub") }, { v: "blank", t: t("manage:newBot.blank"), d: t("manage:newBot.blankSub") }]} />
        {mode === "tpl" && <Group>{TEMPLATES.map(([id, h, m]) => { const x = tplText(t, id); return <Row key={id} title={x.name} sub={`${x.role} · ${HARNESS[h]} · ${m}`} onClick={() => setTpl(id)} data-tpl={id}>{tpl === id && <CheckIcon className="size-4 text-blue" />}</Row> })}</Group>}
        {mode === "desc" && <Textarea autoFocus className="min-h-24 rounded-lg" placeholder={t("manage:newBot.descPlaceholder")} value={desc} onChange={(e) => setDesc(e.target.value)} data-bot-desc />}
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
    <div className="flex flex-wrap items-start gap-6" data-avatar-picker>
      <Face look={look} size={88} />
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.color")}</span>
          {AVATAR_COLORS.map(([c, nm]) => <Tooltip key={c}><TooltipTrigger render={<button type="button" aria-label={nm} aria-pressed={look.color === c && !look.skin} data-color={c} onClick={() => onChange({ color: c, skin: null })}
            className={cn("size-6 rounded-full ring-offset-2 ring-offset-background transition-shadow", look.color === c && !look.skin ? "ring-2 ring-foreground/70" : "hover:ring-2 hover:ring-border")} style={{ background: c }} />} /><TooltipContent>{nm}</TooltipContent></Tooltip>)}
          <label className="relative size-6 cursor-pointer overflow-hidden rounded-full ring-1 ring-border" title={t("manage:avatar.customColor")} style={{ background: "conic-gradient(red, yellow, lime, aqua, blue, magenta, red)" }}>
            <input type="color" className="absolute inset-0 cursor-pointer opacity-0" value={look.color} onChange={(e) => onChange({ color: e.target.value, skin: null })} data-color-custom /></label>
        </div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.shape")}</span>
          <Segmented size="sm" value={SHAPES.reduce<(typeof SHAPES)[number]>((a, b) => (Math.abs(b[1] - look.round) < Math.abs(a[1] - look.round) ? b : a), SHAPES[0])[0]} onChange={(v) => onChange({ round: SHAPES.find((x) => x[0] === v)![1] })} options={SHAPES.map((x) => [x[0], t(`manage:avatar.${x[0]}`)] as const)} />
          <input type="range" min={0} max={100} value={look.round} onChange={(e) => onChange({ round: +e.target.value })} className="w-28 accent-foreground" aria-label={t("manage:avatar.roundness")} data-round /></div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">{t("manage:avatar.skin")}</span>
          <Select testId="skin-series" value={series} onChange={(v) => { setSeries(v); if (!v) onChange({ skin: null }) }} options={SERIES.map(([id, n]) => ({ v: id, l: t(`manage:series.${n}`) }))} /></div>
        {sr[3] > 0 && <div className="flex flex-wrap gap-1.5 pl-12" data-skins>{Array.from({ length: sr[3] }, (_, n) => String(sr[2] + n)).map((sk) => (
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
export function BotView({ k }: { k: string }) {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const b = st.bots[k]
  const [form, setForm] = useState<any>(null), [memTxt, setMemTxt] = useState(""), [editMem, setEditMem] = useState<string | null>(null)
  const [prompt, setPrompt] = useState<{ generated: string; custom: string | null } | null>(null), [pdraft, setPdraft] = useState<string | null>(null)
  if (lastBot !== k) { lastBot = k; if (botTab !== "设置") botTab = "概览" }
  useEffect(() => { if (b && botTab === "记忆" && !b.memLoaded) void loadMemories(k) }, [k, botTab, b?.memLoaded])
  useEffect(() => { if (b && botTab === "系统提示词") api.botPrompt(k).then(setPrompt).catch(() => {}) }, [k, botTab, b?.prompt, b?.memCount])
  if (!b) return <Page title="Bot" back={["#/bots", "Bot"]}><Empty title={t("manage:detail.missing")} sub={t("manage:detail.missingSub")} /></Page>
  const fm = form && form.k === k ? form : { k, n: b.n, role: b.role, pers: b.pers, pro: PRO[b.pro], rule: b.rule, acc: b.acc, harness: b.harness, model: b.model ?? "", color: b.color, round: b.round, skin: b.skin, project: b.project ?? "none", permMode: b.permMode, ws: b.workspaces }
  const effort = fm.reasoningEffort ?? b.reasoningEffort ?? ""
  const set = (p: any) => setForm({ ...fm, reasoningEffort: effort, ...p })
  const mine = st.items.filter((i) => i.b === k && !["done", "drop"].includes(i.st) && !i.arch)
  const tabs = BOT_TABS.map(([v, l]) => [v, t(`manage:detail.${l}`)] as const)
  return (
    <Page title={b.main ? b.n : t("manage:bot.name", { name: b.n })} back={["#/bots", "Bot"]} max={720}
      actions={<>{!b.main && <><span className="hidden type-footnote text-muted-foreground sm:inline">{b.on ? t("manage:bot.enabled") : t("manage:bot.disabled")}</span><Switch checked={b.on} onCheckedChange={(v) => void saveBot(k, { on: v }, v ? t("manage:bot.enabled") : t("manage:bot.disabled"))} aria-label={t("manage:bot.enable")} /></>}<BotMenu k={k} /></>}>
      <div className="-mt-2 mb-6 flex items-center gap-4">
        <Robot k={k} size={64} />
        <div className="min-w-0"><div className="type-body">{b.role || t("manage:detail.noRole")}</div><div className="mt-0.5 type-footnote text-muted-foreground">{t("manage:detail.meta", { harness: b.h, model: b.m, pers: b.pers || t("manage:detail.unset"), pro: proLabel(t, b.pro) })}</div><RecentStats bot={k} className="mt-0.5 block" /><OwnerChip objectType="bot" objectId={k} className="mt-1.5" /></div>
      </div>
      <div className="-mx-4 mb-6 overflow-x-auto px-4"><Segmented value={botTab} onChange={(v) => { botTab = v; r() }} options={tabs} /></div>
      {botTab === "概览" && (mine.length ? <Group>{mine.map((i) => <ItemRow key={i.id} i={i} bot={false} />)}</Group> : <Empty title={t("manage:detail.noRunning")} />)}
      {botTab === "对外身份" && <PersonasSection bot={k} />}
      {botTab === "定时与流程" && <Empty title={t("manage:detail.scheduleEmpty")} sub={t("manage:detail.scheduleEmptySub")} />}
      {botTab === "能力" && (
        <div className="flex flex-col gap-7" data-caps>
          <section><SectionLabel>{t("manage:caps.steveTools")}</SectionLabel>
            <Group>{([["steve_update", "toolUpdate"], ["steve_ask", "toolAsk"], ["steve_request_approval", "toolApproval"], ["steve_deliver", "toolDeliver"], ["steve_publish", "toolPublish"], ["steve_lane_status", "toolLaneStatus"]] as const).map(([c, l]) => <Row key={c} title={t(`manage:caps.${l}`)} sub={<span className="font-mono">{c}</span>}><CheckIcon className="size-4 text-success" /></Row>)}</Group></section>
          <section><SectionLabel>{t("manage:caps.harnessTools", { harness: b.h })}</SectionLabel>
            <Group><Row title={t("manage:caps.builtin")} sub={t("manage:caps.builtinSub", { harness: b.h })} /><Row title={t("manage:caps.perm")} sub={permLabel(t, b.permMode)}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { botTab = "设置"; r() }}>{t("manage:caps.change")}</Button></Row></Group></section>
          <div className="px-1 type-footnote text-muted-foreground">{t("manage:caps.later")}</div>
        </div>
      )}
      {botTab === "系统提示词" && (
        <div className="flex flex-col gap-3" data-prompt>
          <Segmented size="sm" value={b.prompt != null || pdraft != null ? "自己写" : "自动生成"} onChange={(v) => { if (v === "自己写") setPdraft(b.prompt ?? prompt?.generated ?? ""); else { setPdraft(null); if (b.prompt != null) void saveBot(k, { prompt: null }, t("manage:prompt.revertedToast")) } }} options={[["自动生成", t("manage:prompt.auto")], ["自己写", t("manage:prompt.custom")]] as const} />
          {b.prompt == null && pdraft == null
            ? <><pre className="whitespace-pre-wrap rounded-xl bg-muted/70 px-4 py-3 font-mono text-[12.5px] leading-[20px] text-foreground/90">{prompt?.generated ?? t("manage:prompt.loading")}</pre><div className="px-1 type-footnote text-muted-foreground">{t("manage:prompt.autoHint")}</div></>
            : <><Textarea className="min-h-72 rounded-xl font-mono text-[12.5px]" value={pdraft ?? b.prompt ?? ""} onChange={(e) => setPdraft(e.target.value)} data-prompt-edit />
              <div className="flex items-center gap-2"><Button size="sm" disabled={pdraft == null || pdraft === b.prompt} onClick={() => { void saveBot(k, { prompt: pdraft ?? "" }); setPdraft(null) }}>{t("common:action.save")}</Button><span className="type-footnote text-muted-foreground">{t("manage:prompt.customHint")}</span></div></>}
        </div>
      )}
      {botTab === "记忆" && (
        <div className="flex flex-col gap-3" data-mem>
          <div className="px-1 type-footnote text-muted-foreground">{t("manage:mem.scope", { ws: st.workspace?.name })}</div>
          <div className="flex gap-2"><Input className="rounded-lg" placeholder={t("manage:mem.placeholder")} value={memTxt} onChange={(e) => setMemTxt(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && memTxt.trim()) { addMem(k, memTxt.trim()); setMemTxt("") } }} data-mem-input /><Button variant="outline" disabled={!memTxt.trim()} onClick={() => { addMem(k, memTxt.trim()); setMemTxt("") }}>{t("common:action.add")}</Button></div>
          {!b.memLoaded ? <div className="px-1 type-callout text-muted-foreground">{t("manage:prompt.loading")}</div> : <Group>{b.mem.map(([txt, src, id]) => <Row key={id} title={editMem === id ? <Input autoFocus className="h-8 rounded-lg" defaultValue={txt} onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== txt) editMem && editMemFn(k, id, v); setEditMem(null) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : txt} sub={src}>
            <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEditMem(id)}>{t("common:action.edit")}</Button><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => delMem(k, id)}>{t("common:action.delete")}</Button></Row>)}{!b.mem.length && <Row title={t("manage:mem.empty")} sub={t("manage:mem.emptySub")} />}</Group>}
        </div>
      )}
      {botTab === "设置" && (
        <FieldGroup className="gap-8">
          <AskSteve key={k} ctx={{ kind: "bot", id: k }} placeholder={t("manage:form.ask", { name: b.main ? b.n : t("manage:bot.name", { name: b.n }) })} />
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">{t("manage:form.identity")}</SectionLabel>
            <F label={t("manage:form.avatar")} hint={t("manage:form.avatarHint")}><AvatarPicker look={{ color: fm.color, round: fm.round, skin: fm.skin }} onChange={(p) => set(p)} /></F>
            <F label={t("manage:form.name")} htmlFor="bot-name"><Input id="bot-name" className="rounded-lg" value={fm.n} onChange={(e) => set({ n: e.target.value })} /></F>
            <F label={t("manage:form.role")} hint={t("manage:form.roleHint")}><Input className="rounded-lg" value={fm.role} onChange={(e) => set({ role: e.target.value })} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">{t("manage:form.personality")}</SectionLabel>
            <F label={t("manage:form.voice")} hint={t("manage:form.voiceHint")}><Textarea className="min-h-20 rounded-lg" value={fm.pers} onChange={(e) => set({ pers: e.target.value })} /></F>
            <F label={t("manage:form.pro")} hint={t("manage:form.proHint")}><Segmented size="sm" value={fm.pro} onChange={(v) => set({ pro: v })} options={PRO.map((v, i) => [v, proLabel(t, i)] as const)} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">{t("manage:form.work")}</SectionLabel>
            <F label={t("manage:form.rule")} hint={t("manage:form.ruleHint")}><Textarea className="min-h-20 rounded-lg" value={fm.rule} onChange={(e) => set({ rule: e.target.value })} /></F>
            <F label={t("manage:form.acc")}><Segmented size="sm" value={fm.acc} onChange={(v) => set({ acc: v })} options={Object.entries(ACC).map(([key, v]) => [v, accLabel(t, key)] as const)} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">{t("manage:form.run")}</SectionLabel>
            <div className="grid gap-4 sm:grid-cols-2">
              <F label="Harness"><div><Select value={fm.harness} onChange={(v) => set({ harness: v, model: "", reasoningEffort: "" })} options={st.runtimes.map((x) => ({ v: x.id, l: x.ok ? x.name : t("manage:form.unavailable", { name: x.name }) }))} /></div></F>
              <F label={t("manage:form.model")}><div><ModelPicker harness={fm.harness} value={fm.model ?? ""} allowDefault testId="bot-model" onChange={(m) => set({ model: m })} /></div></F>
              <F label={t("manage:form.effort")} hint={b.main ? t("manage:form.effortHintMain") : t("manage:form.effortHint")}><div><Select value={effort || "default"} onChange={(v) => set({ reasoningEffort: v === "default" ? "" : v })} options={[{ v: "default", l: t("manage:effort.default") }, ...(fm.harness === "codex" ? (["none", "minimal", "low", "medium", "high", "xhigh"] as const) : (["low", "medium", "high", "max"] as const)).map((v: (typeof EFFORT_KEYS)[number]) => ({ v, l: t(`manage:effort.${v}`) }))]} /></div></F>
            </div>
            {b.main && <SteveJudges harness={fm.harness} model={fm.model ?? ""} />}
            <F label={t("manage:form.device")} hint={t("manage:form.deviceHint")}><div><Select testId="bot-dev" value="hub" onChange={() => {}} options={st.devices.map((d) => ({ v: d.id, l: d.name }))} /></div></F>
            <F label={t("manage:form.project")} hint={t("manage:form.projectHint")}><div><Select value={fm.project} onChange={(v) => set({ project: v })} options={[{ v: "none", l: t("manage:form.projectInfer") }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} /></div></F>
            <F label={t("manage:form.perm")} hint={t("manage:form.permHint")}><div><Select value={fm.permMode} onChange={(v) => set({ permMode: v })} options={Object.keys(PERM).map((v) => ({ v, l: permLabel(t, v) }))} /></div></F>
            {!b.main && <F label={t("manage:form.workspaces")} hint={t("manage:form.workspacesHint")}>
              <div className="flex flex-col gap-1.5">{st.workspaces.map((w) => <label key={w.id} className="flex items-center gap-2 type-callout"><Checkbox checked={fm.ws.includes(w.id)} disabled={fm.ws.length === 1 && fm.ws.includes(w.id)} onCheckedChange={(v) => set({ ws: v ? [...fm.ws, w.id] : fm.ws.filter((x: string) => x !== w.id) })} />{w.name}</label>)}</div></F>}
          </section>
          <div className="flex gap-2"><Button data-save-bot onClick={async () => {
            await saveBot(k, { n: fm.n.trim() || b.n, role: fm.role, pers: fm.pers, pro: PRO.indexOf(fm.pro), rule: fm.rule, acc: fm.acc, harness: fm.harness, model: fm.model || null, reasoningEffort: effort || null, color: fm.color, round: fm.round, skin: fm.skin, project: fm.project === "none" ? null : fm.project, permMode: fm.permMode })
            if (!b.main && JSON.stringify([...fm.ws].sort()) !== JSON.stringify([...b.workspaces].sort())) setBotWorkspaces(k, fm.ws)
            setForm(null); botTab = "概览"; r() }}>{t("common:action.save")}</Button><Button variant="ghost" onClick={() => { setForm(null); botTab = "概览"; r() }}>{t("common:action.cancel")}</Button></div>
        </FieldGroup>
      )}
    </Page>
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
  ["data", t("manage:settings.catData"), DatabaseIcon, t("manage:settings.catDataSub")],
  ["about", t("manage:settings.catAbout"), InfoIcon, t("manage:settings.catAboutSub")],
]
const PH_KEYS: Record<string, "phGeneral" | "phItems" | "phTags" | "phModels" | "phNotify" | "phLook" | "phData"> = {
  general: "phGeneral", items: "phItems", tags: "phTags", models: "phModels", notify: "phNotify", look: "phLook", data: "phData",
}
const Sel = ({ v, o, on, testId }: { v: string; o: [string, string][]; on: (v: string) => void; testId?: string }) => <Select align="end" testId={testId} value={v} onChange={on} options={o.map(([x, l]) => ({ v: x, l }))} />
const Later = () => { const t = useT(); return <span className="ml-1.5 rounded bg-muted px-1.5 py-px text-[11px] font-normal text-muted-foreground">{t("manage:settings.later")}</span> }
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
        <a key={id} href={"#/settings/" + id} data-settings-cat={id} className={cn("flex items-center gap-3 rounded-lg px-2.5 py-2 type-callout transition-colors hover:bg-accent", cur === id && "bg-accent font-medium")}>
          <I className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} /><span className="min-w-0 flex-1"><span className="flex items-center gap-1.5">{l}{id === "about" && st.update && <span className="size-1.5 rounded-full bg-attention" data-settings-update-dot />}</span><span className="block truncate type-footnote font-normal text-muted-foreground md:hidden">{d}</span></span><ChevronRightIcon className="size-4 text-tertiary md:hidden" />
        </a>))}
    </nav>
  )
  if (!wide && !C) return <Page title={t("manage:settings.title")} max={640}>{nav}</Page>
  if (!C) return null
  return (
    <Page title={wide ? t("manage:settings.title") : C[1]} back={wide ? undefined : ["#/settings", t("manage:settings.title")]} max={960}>
      <div className="grid gap-10 md:grid-cols-[200px_minmax(0,1fr)]">
        {wide && <div className="-mt-1">{nav}</div>}
        <div className="min-w-0 max-w-[640px]" data-settings-pane={cur}>
          {wide && <h2 className="mb-5 type-title">{C[1]}</h2>}
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
function Pane({ id }: { id: string }) {
  const t = useT()
  const c = st.settings
  if (id === "tags") return <TagsPane />
  if (id === "workspace") return <WorkspacePane />
  if (id === "vault") return <VaultSettingsPane />
  if (id === "stats") return <StatsSettingsPane />
  if (id === "models") return <ModelsSettingsPane />
  if (id === "general") return <div className="flex flex-col gap-8">
    <section><SectionLabel>{t("manage:general.assistant")}</SectionLabel><Group><Row icon={<BotAvatar k="steve" size={36} />} title={st.bots.steve?.n} sub={t("manage:general.assistantSub")} onClick={() => go("#/bots/steve")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>
    <Group>
      <Row title={t("manage:general.home")} sub={t("manage:general.homeSub")}><Sel v={c.home} o={[["对话", t("manage:general.homeChat")], ["待处理", t("manage:general.homeInbox")], ["事项", t("manage:general.homeItems")]]} on={(v) => setSetting({ home: v })} /></Row>
      <Row title={t("manage:general.away")} sub={t("manage:general.awaySub")}><Sel v={c.awayH} o={[...[2, 4, 8].map((h): [string, string] => [String(h), t("common:dur.hours", { count: h })]), ["0", t("manage:general.awayNever")]]} on={(v) => setSetting({ awayH: v })} /></Row>
    </Group>
    <Group>
      <Row title={t("manage:general.onboard")} sub={t("manage:general.onboardSub")}><Button size="sm" variant="outline" onClick={() => { localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }} data-restart-onboard>{t("manage:general.restart")}</Button></Row>
      <Row title={t("manage:general.shortcuts")} sub={t("manage:general.shortcutsSub")} />
    </Group>
  </div>
  if (id === "items") return <div className="flex flex-col gap-8">
    <section><SectionLabel>{t("manage:items.acc")}</SectionLabel><Group>
      <Row title={t("manage:items.accDefault")} sub={t("manage:items.accDefaultSub")}><Segmented size="sm" value={c.accDefault === "manual" ? "manual" : "auto"} onChange={(v) => setSetting({ accDefault: v })} options={[["manual", t("manage:acc.manual")], ["auto", t("manage:acc.auto")]] as const} /></Row></Group></section>
    <section><SectionLabel>{t("manage:items.numbering")}</SectionLabel><Group>
      <Row title={t("manage:items.prefix")} sub={t("manage:items.prefixSub")}><span className="font-mono type-callout">{st.prefix}-{st.noSeq + 1}</span></Row>
      <Row title={t("manage:items.depsAfter")}><Sel v={c.depsAfter} o={[["自动开始", t("manage:items.depsAuto")], ["提醒我开始", t("manage:items.depsRemind")]]} on={(v) => setSetting({ depsAfter: v })} /></Row></Group></section>
    <section><SectionLabel>{t("manage:items.sessions")}</SectionLabel><Group>
      <Row title={t("manage:items.handoff")} sub={t("manage:items.handoffSub")}><Sel v={c.handoff} o={[["70", "70%"], ["80", "80%"], ["90", "90%"]]} on={(v) => setSetting({ handoff: v })} /></Row></Group></section>
    <section><SectionLabel>{t("manage:items.archive")}</SectionLabel><Group>
      <Row title={t("manage:items.archDays")} sub={t("manage:items.archDaysSub")}><Sel testId="arch-days" v={c.archDays} o={[...[7, 30, 90].map((d): [string, string] => [String(d), t("common:dur.days", { count: d })]), ["0", t("manage:items.archNever")]]} on={(v) => setSetting({ archDays: v })} /></Row></Group></section>
  </div>
  if (id === "notify") return <div className="flex flex-col gap-8">
    <div className="rounded-xl border px-4 py-3 type-callout text-muted-foreground">{t("manage:notify.none")}</div>
    <section><SectionLabel>{t("manage:notify.time")}</SectionLabel><Group>
      <Row title={<>{t("manage:notify.quiet")}<Later /></>} sub={t("manage:notify.quietSub")}><Sel v={c.quiet} o={[["23:00 – 08:00", "23:00 – 08:00"], ["22:00 – 09:00", "22:00 – 09:00"], ["关闭", t("manage:notify.quietOff")]]} on={(v) => setSetting({ quiet: v })} /></Row>
      <Row title={<>{t("manage:notify.digest")}<Later /></>} sub={t("manage:notify.digestSub")}><Sel v={c.digestAt} o={[["08:30", "08:30"], ["09:00", "09:00"], ["不发", t("manage:notify.digestNever")]]} on={(v) => setSetting({ digestAt: v })} /></Row></Group></section>
  </div>
  if (id === "look") return <div className="flex flex-col gap-8">
    <section><SectionLabel>{t("manage:look.avatars")}</SectionLabel>
      <div className="mb-3 flex flex-wrap items-center gap-4 rounded-xl bg-muted/50 px-4 py-3" data-avatar-preview>{Object.keys(st.bots).map((k) => <a key={k} href={"#/bots/" + k} className="flex flex-col items-center gap-1"><Robot k={k} size={44} /><span className="type-footnote text-muted-foreground">{st.bots[k].n}</span></a>)}</div>
      <div className="px-1 type-footnote text-muted-foreground">{t("manage:look.avatarsHint")}</div></section>
    <section><SectionLabel>{t("manage:look.ui")}</SectionLabel><Group>
      <Row title={t("manage:look.theme")} sub={t("manage:look.themeSub")}><ThemeSwitch labels /></Row>
      <LanguageRow />
      <Row title={t("manage:look.fontSize")}><Segmented size="sm" value={c.fontSize} onChange={(v) => setSetting({ fontSize: v })} options={[["标准", t("manage:look.fontNormal")], ["大", t("manage:look.fontLarge")]]} /></Row>
      <Row title={t("manage:look.reduceMotion")} sub={t("manage:look.reduceMotionSub")}><Switch checked={c.reduceMotion === "开"} onCheckedChange={(v) => setSetting({ reduceMotion: v ? "开" : "关" })} /></Row>
    </Group></section></div>
  if (id === "data") return <div className="flex flex-col gap-8">
    <section><SectionLabel>{t("manage:data.hub")}</SectionLabel><Group>
      <Row title={t("manage:data.location")} sub={<span className="font-mono">~/.steve/data/steve.sqlite3</span>} />
      <Row title={t("manage:data.backup")} sub={t("manage:data.backupSub")} /></Group></section>
    <WorkspaceDisk />
    <OutboxDead />
    <section><SectionLabel>{t("manage:data.retention")}</SectionLabel><Group>
      {/* read-only: whole logs are kept for good; a shorter keepSess (set by a confirmed config draft) compacts archived sessions */}
      <Row title={t("manage:data.sessions")} sub={c.keepSess && c.keepSess !== "永久" ? t("manage:data.sessionsCompact", { period: c.keepSess === "1 年" ? t("manage:data.keepYear") : t("common:dur.days", { count: 90 }) }) : t("manage:data.sessionsForever")} data-keep-sess={c.keepSess || "永久"}>
        <span className="type-callout text-muted-foreground">{c.keepSess === "1 年" ? t("manage:data.keepYear") : c.keepSess === "90 天" ? t("common:dur.days", { count: 90 }) : t("manage:data.keepForever")}</span></Row></Group></section>
    <section><SectionLabel>{t("manage:data.clean")}</SectionLabel><Group>
      <Row title={t("manage:data.clearMem")} sub={t("manage:data.clearMemSub")}><DangerBtn label={t("manage:data.clear")} what={t("manage:data.clearWhat")} onOk={() => void api.clearMemories().then(() => toast(t("manage:data.cleared")))} /></Row></Group></section>
  </div>
  if (id === "about") return <div className="flex flex-col gap-8">
    <AboutVersions />
    <section><SectionLabel>{t("manage:about.release")}</SectionLabel><Group>
    <Row title={t("manage:about.publish")} sub={t("manage:about.publishSub")} />
    <Row title={t("manage:about.onboard")} sub={t("manage:about.onboardSub")}><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { finishOnboarding(); localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }}>{t("manage:about.start")}</Button></Row>
    </Group></section>
  </div>
  return null
}
function WorkspacePane() {
  const t = useT()
  const w = st.workspace!
  const [n, setN] = useState(w.name), [d, setD] = useState(w.description), [p, setP] = useState(w.item_prefix)
  useEffect(() => { setN(w.name); setD(w.description); setP(w.item_prefix) }, [w.id])
  const prefix = p.toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6)
  const dirty = n.trim() !== w.name || d.trim() !== w.description || prefix !== w.item_prefix
  return <div className="flex flex-col gap-8">
    <p className="type-callout text-muted-foreground">{t("manage:ws.intro")}</p>
    <FieldGroup className="gap-4">
      <F label={t("manage:ws.name")}><Input className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} /></F>
      <F label={t("manage:ws.desc")}><Textarea className="min-h-16 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} /></F>
      <F label={t("manage:ws.prefix")} hint={t("manage:ws.prefixHint", { example: `${prefix || "ST"}-${st.noSeq + 1}` })}><Input className="w-28 rounded-lg font-mono uppercase" value={p} maxLength={6} onChange={(e) => setP(e.target.value)} data-prefix /></F>
      {dirty && <div className="flex gap-2"><Button size="sm" disabled={!n.trim() || !prefix} onClick={() => void api.patchWs(w.id, { name: n.trim(), description: d.trim(), item_prefix: prefix }).then(() => { toast(t("common:state.saved")); return import("@/store").then((m) => m.refresh()) }).catch((e) => toast(e.message))}>{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={() => { setN(w.name); setD(w.description); setP(w.item_prefix) }}>{t("common:action.cancel")}</Button></div>}
    </FieldGroup>
    <section><SectionLabel>{t("manage:ws.here")}</SectionLabel><Group>
      <Row icon={<FolderIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />} title={t("manage:ws.projects", { count: st.projects.length })} onClick={() => go("#/projects")}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
      <Row title={t("manage:ws.bots", { count: Object.keys(st.bots).length })} onClick={() => go("#/bots")}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
      <Row title={t("manage:ws.items", { count: st.items.length })} onClick={() => go("#/items")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>
    <section><SectionLabel>{t("manage:ws.all", { n: st.workspaces.length })}</SectionLabel>
      <Group>{st.workspaces.map((x) => <Row key={x.id} title={x.name} sub={x.description || undefined}>{x.id === w.id ? <span className="type-footnote text-muted-foreground">{t("manage:ws.current")}</span> : <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void import("@/store").then((m) => m.switchWorkspace(x.id))}>{t("manage:ws.switch")}</Button>}</Row>)}</Group>
      <Button size="sm" variant="outline" className="mt-3" onClick={() => openSheet("newWorkspace")}><PlusIcon />{t("manage:ws.new")}</Button></section>
  </div>
}
function DangerBtn({ label, what, onOk }: { label: string; what: string; onOk: () => void }) {
  const t = useT()
  const [o, setO] = useState(false)
  return <>
    <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setO(true)}>{label}</Button>
    <AlertDialog open={o} onOpenChange={setO}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("manage:danger.title", { label })}</AlertDialogTitle><AlertDialogDescription>{what}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { setO(false); onOk() }}>{label}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </>
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
  return <section data-kinds-review>
    <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={async () => { setBusy(true); await kindsBackfill(); setXs(await loadKindsReview()); setBusy(false) }} data-kinds-backfill>{t("manage:kinds.backfill")}</Button>}>
      {rows.length ? t("manage:kinds.pending", { n: rows.length }) : t("manage:kinds.fill")}</SectionLabel>
    {rows.length ? <>
      <p className="mb-2 type-footnote text-muted-foreground">{t("manage:kinds.intro")}</p>
      <Group>{rows.map((x) => { const i = it(x.id); return (
        <Row key={x.id} title={<span className="inline-flex min-w-0 items-center gap-2"><ItemNo id={i.id} /><span className="truncate">{i.t}</span></span>} sub={x.why || undefined} data-kind-review={x.id}>
          <Select quiet testId={"review-kind-" + x.id} value={kindOf(i) ?? ""} onChange={(k) => { setKind(i.id, k); void done([x.id]) }} options={kindTags().map((kt) => ({ v: kt.id, l: <KindTag id={kt.id} className="border-0 bg-transparent px-0" /> }))} />
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void done([x.id])} data-kind-ok={x.id}>{t("manage:kinds.ok")}</Button>
        </Row>) })}</Group>
      {rows.length > 1 && <div className="mt-2 flex justify-end"><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void done(rows.map((x) => x.id))} data-kind-ok-all>{t("manage:kinds.okAll")}</Button></div>}
    </> : <p className="type-footnote text-muted-foreground">{xs ? t("manage:kinds.allSet") : t("manage:kinds.reading")}</p>}
  </section>
}
function TagsPane() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [del, setDel] = useState<string | null>(null), [merge, setMerge] = useState<string | null>(null), [into, setInto] = useState("")
  const cats = ["全部", ...TAG_CATS.filter((c) => st.tags.some((tg) => tg.cat === c))]
  const d = del ? tag(del) : null, m = merge ? tag(merge) : null
  const domain = st.tags.filter((x) => !isKind(x))
  return <div className="flex flex-col gap-6" data-tags-pane>
    <p className="type-callout text-muted-foreground">{t("manage:tags.intro")}</p>
    <KindsReview />
    <div className="flex flex-wrap items-center gap-2"><Segmented size="sm" value={cats.includes(tagCat) ? tagCat : "全部"} onChange={(v) => { tagCat = v; r() }} options={cats.map((c) => [c, catLabel(t, c)] as const)} /><span className="flex-1" /><Button size="sm" onClick={() => openSheet("tag", {})} data-new-tag-btn><PlusIcon />{t("manage:tags.new")}</Button></div>
    {!st.tags.length && <Empty title={t("manage:tags.empty")} sub={t("manage:tags.emptySub")} />}
    {TAG_CATS.filter((c) => tagCat === "全部" || c === tagCat).map((c) => { const kind = c === KIND_CAT; const xs = kind ? kindTags() : st.tags.filter((tg) => tg.cat === c); return xs.length ? (
      <section key={c}><SectionLabel>{kind ? t("manage:tags.kindSection") : catLabel(t, c)} · {xs.length}</SectionLabel>
        <Group>{xs.map((tg) => (
          <Row key={tg.id} title={<span className="inline-flex items-center gap-2">{kind ? <KindTag id={tg.id} /> : tg.name}<span className="type-footnote text-muted-foreground">{t("manage:tags.uses", { count: tagUses(tg.id) })}</span>{kind ? <span className="type-footnote text-tertiary">{t("manage:tags.builtin")}</span> : tg.by === "Steve" && <span className="type-footnote text-tertiary">{t("manage:tags.bySteve", { created: tg.created })}</span>}</span>} sub={tg.desc} data-tag-row={tg.name}>
            <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-tag-menu={tg.name} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-40">
                <DropdownMenuItem onClick={() => showTag(tg.id)} data-show-tag>{t("manage:tags.showItems")}</DropdownMenuItem>
                <DropdownMenuItem onClick={() => openSheet("tag", { id: tg.id })} data-edit-tag><PencilIcon />{kind ? t("manage:tags.editDesc") : t("common:action.edit")}</DropdownMenuItem>
                {!kind && domain.length > 1 && <DropdownMenuItem onClick={() => { setMerge(tg.id); setInto(domain.find((x) => x.id !== tg.id && x.cat === tg.cat)?.id ?? domain.find((x) => x.id !== tg.id)!.id) }} data-merge-tag>{t("manage:tags.mergeTo")}</DropdownMenuItem>}
                {!kind && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(tg.id)} data-del-tag><TrashIcon />{t("common:action.delete")}</DropdownMenuItem></>}
              </DropdownMenuContent></DropdownMenu>
          </Row>))}</Group></section>) : null })}
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
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-tag-dialog>
        <DialogHeader><DialogTitle className="type-headline">{tg ? t("manage:tagDialog.edit") : t("manage:tagDialog.new")}</DialogTitle><DialogDescription className="type-callout">{t("manage:tagDialog.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <F label={t("manage:tagDialog.name")} htmlFor="tag-name" hint={dup ? t("manage:tagDialog.dup", { cat: catLabel(t, dup.cat) }) : undefined}><Input id="tag-name" autoFocus={!kind} disabled={kind} className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder={t("manage:tagDialog.namePlaceholder")} /></F>
          <F label={t("manage:tagDialog.cat")} hint={kind ? t("manage:tagDialog.catBuiltin") : undefined}><div>{kind ? <span className="type-callout text-muted-foreground">{t("manage:tagDialog.kind")}</span> : <Select testId="tag-cat" value={c} onChange={setC} options={DOMAIN_CATS.map((x) => ({ v: x, l: catLabel(t, x) }))} />}</div></F>
          <F label={t("manage:tagDialog.descLabel")} htmlFor="tag-desc"><Textarea id="tag-desc" className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("manage:tagDialog.descPlaceholder")} data-tag-desc /></F>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={!ok} onClick={() => void save()} data-testid="tag-go">{tg ? t("common:action.save") : t("manage:tagDialog.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
