/* Bot 菜单与设置：Bot 是独立菜单；设置是分类页。所有改动都写到 Hub，也都可以直接告诉 Steve。 */
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
  st, botName, PRO, ACC, PERM, HARNESS, saveBot, makeBot, draftBot, deleteBot, dupBot, addMem, editMem, delMem, loadMemories, setBotWorkspaces, tag, tagUses, TAG_CATS, makeTag, saveTag, deleteTag, mergeTag, setTags, it,
  setSetting, toast, finishOnboarding, emit,
} from "@/store"
import { api } from "@/api"
import { go, Page, BotAvatar, Group, Row, SectionLabel, Segmented, Meta, Empty, openSheet, closeSheet, ThemeSwitch, useRoute } from "@/ui"
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
export function BotsView() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const ks = Object.keys(st.bots).filter((k) => !st.bots[k].main && (botFilter === "全部" || (botFilter === "启用") === st.bots[k].on))
  const S = st.bots.steve
  const runOf = (k: string) => st.items.filter((i) => i.b === k && ["run", "wait", "block"].includes(i.st) && !i.arch).length
  const others = st.allBots.filter((b) => !st.bots[b.id])
  return (
    <Page title="Bot" sub="Steve 调用的能力。日常不用管它们，Steve 会选合适的 Bot；这里是管理入口。" max={720}
      actions={<Button size="sm" onClick={() => openSheet("newBot")} data-new-bot><PlusIcon />新建 Bot</Button>}>
      {S && <section className="mb-8"><SectionLabel>主 Bot</SectionLabel>
        <Group><Row icon={<BotAvatar k="steve" size={44} />} onClick={() => go("#/bots/steve")} data-bot-row="steve"
          title={<span className="type-headline">{S.n}</span>} sub={`${S.role} · ${S.h} · ${S.m} · ${runOf("steve")} 件进行中`}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group>
      </section>}
      <section>
        <SectionLabel right={<Segmented size="sm" value={botFilter} onChange={(v) => { botFilter = v; r() }} options={["全部", "启用", "停用"] as const} />}>「{st.workspace?.name}」里的其他 Bot · {Object.keys(st.bots).length - 1}</SectionLabel>
        {ks.length ? <Group>{ks.map((k) => { const b = st.bots[k]; return (
          <div key={k} className="group/bot flex items-center gap-3 rounded-lg px-2 py-2.5 transition-colors hover:bg-accent">
            <BotAvatar k={k} size={36} className={cn(!b.on && "opacity-50")} />
            <button type="button" className="min-w-0 flex-1 text-left" onClick={() => go("#/bots/" + k)} data-bot-row={k}>
              <div className="type-body">{b.n} Bot <span className="text-muted-foreground">· {b.role}</span></div>
              <Meta className="mt-0.5"><span>{b.h} · {b.m}</span><span>{runOf(k)} 件进行中</span>{b.project && <span>默认项目 {b.ws}</span>}<span>{b.memCount} 条记忆</span>{b.workspaces.length > 1 && <span>参与 {b.workspaces.length} 个工作空间</span>}{!b.on && <span>已停用</span>}</Meta>
            </button>
            <Switch checked={b.on} onCheckedChange={(v) => void saveBot(k, { on: v }, v ? `已启用 ${b.n} Bot` : `已停用 ${b.n} Bot，Steve 不再派活给它`)} aria-label={"启用" + b.n} />
            <BotMenu k={k} />
          </div>) })}</Group> : <Empty title={Object.keys(st.bots).length > 1 ? "没有符合条件的 Bot" : "这个工作空间里只有 Steve"} sub="新建一个 Bot，或者把别的工作空间里的 Bot 加进来。" />}
        {others.length > 0 && <div className="mt-6"><SectionLabel>在别的工作空间里 · {others.length}</SectionLabel>
          <Group>{others.map((b) => <Row key={b.id} title={b.name + " Bot"} sub={`参与 ${b.workspaces.length} 个工作空间`}><Button size="xs" variant="outline" onClick={() => setBotWorkspaces(b.id, [...b.workspaces, st.workspace!.id])}><PlusIcon />加入这里</Button></Row>)}</Group></div>}
        <div className="mt-3 px-1 type-footnote text-muted-foreground">一个 Bot 可以参与多个工作空间；在哪个空间干活，就只看得到那个空间的事项、项目和记忆。也可以在对话里说“以后这类事用某个模型、按某种方式做”，Steve 会起草改动请你确认。输入 <span className="font-mono">@</span> 可以直接指定 Bot。</div>
      </section>
    </Page>
  )
}
function BotMenu({ k, align = "end" }: { k: string; align?: "end" | "start" }) {
  const b = st.bots[k]
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" data-bot-menu={k} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align={align} className="min-w-40">
        <DropdownMenuItem onClick={() => { botTab = "设置"; go("#/bots/" + k) }}><PencilIcon />编辑</DropdownMenuItem>
        <DropdownMenuItem onClick={async () => { const nk = await dupBot(k); if (nk) { botTab = "设置"; go("#/bots/" + nk) } }} data-dup-bot><CopyIcon />复制</DropdownMenuItem>
        {!b?.main && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => openSheet("deleteBot", { k })} data-del-bot={k}><TrashIcon />删除</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
export function DeleteBotDialog({ k }: { k: string }) {
  const b = st.bots[k]
  const [to, setTo] = useState("steve"), [mem, setMem] = useState<"删除" | "导出">("导出")
  if (!b) return null
  const mine = st.items.filter((i) => i.b === k && !["done", "drop"].includes(i.st))
  return (
    <AlertDialog open onOpenChange={(o) => !o && closeSheet()}>
      <AlertDialogContent className="sm:max-w-[480px]!">
        <AlertDialogHeader><AlertDialogTitle>删除 {b.n} Bot？</AlertDialogTitle>
          <AlertDialogDescription>它手上有 {mine.length} 件进行中的事项，删除后交给下面选的 Bot，正在跑的会话会停下，由新 Bot 带交接重新开始；已完成的事项和会话记录保留。它会从所有工作空间里移除。</AlertDialogDescription></AlertDialogHeader>
        <FieldGroup className="gap-4">
          <Field orientation="horizontal" className="justify-between"><FieldLabel>交给</FieldLabel>
            <Select align="end" testId="del-to" value={to} onChange={setTo} options={Object.keys(st.bots).filter((x) => x !== k && st.bots[x].on).map((x) => ({ v: x, l: botName(x) }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>它的 {b.memCount} 条记忆</FieldLabel><Segmented size="sm" value={mem} onChange={setMem} options={["导出", "删除"] as const} /></Field>
        </FieldGroup>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel>
          <AlertDialogAction variant="destructive" data-del-bot-confirm onClick={() => { closeSheet(); go("#/bots"); void deleteBot(k, to, mem) }}>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
const TEMPLATES: [string, string, string, string, string][] = [["评审", "界面与交互评审：截图对照视觉规范，列出问题和改法", "claude-code", "claude-opus-5-5", "先看整体再看细节；每个问题写清在哪、为什么、怎么改。"], ["研究", "资料检索与调研：查资料、对比方案、给结论", "claude-code", "claude-opus-5-5", "结论先行，附来源链接；区分事实和判断。"], ["写作", "文档、周报、对外文案", "claude-code", "claude-opus-5-5", "用用户的语气写；先给提纲再成文。"]]
export function NewBotDialog() {
  const [mode, setMode] = useState<"tpl" | "desc" | "blank">("tpl"), [tpl, setTpl] = useState(TEMPLATES[0][0]), [desc, setDesc] = useState(""), [busy, setBusy] = useState(false)
  const create = async () => {
    setBusy(true); let k: string | undefined
    if (mode === "tpl") { const t = TEMPLATES.find((x) => x[0] === tpl)!; k = await makeBot(t[0], t[1], t[2], t[3]); if (k) await saveBot(k, { rule: t[4] }, "") }
    else if (mode === "blank") k = await makeBot("新 Bot", "")
    else k = await draftBot(desc.trim())
    setBusy(false); if (k) { closeSheet(); botTab = "设置"; go("#/bots/" + k) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-headline">新建 Bot</DialogTitle><DialogDescription className="type-callout">Bot 是 Steve 调用的能力，有自己的 harness、模型、性格和工作规范。新建的 Bot 加入当前工作空间「{st.workspace?.name}」。</DialogDescription></DialogHeader>
        <ChoiceCards value={mode} onChange={setMode} cols={1} options={[{ v: "tpl", t: "从模板开始", d: "评审、研究、写作" }, { v: "desc", t: "描述给 Steve，让它起草", d: "一句话说清这个 Bot 做什么、怎么做" }, { v: "blank", t: "空白", d: "所有设置自己填" }]} />
        {mode === "tpl" && <Group>{TEMPLATES.map(([n, r, h, m]) => <Row key={n} title={n} sub={`${r} · ${HARNESS[h]} · ${m}`} onClick={() => setTpl(n)} data-tpl={n}>{tpl === n && <CheckIcon className="size-4 text-blue" />}</Row>)}</Group>}
        {mode === "desc" && <Textarea autoFocus className="min-h-24 rounded-lg" placeholder="例如：帮我审合同，先列风险条款，再给修改建议；所有结论要引用原文" value={desc} onChange={(e) => setDesc(e.target.value)} data-bot-desc />}
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>取消</Button><Button disabled={(mode === "desc" && !desc.trim()) || busy} onClick={() => void create()} data-testid="bot-go">{busy ? <Spinner className="size-3.5" /> : mode === "desc" && <SparklesIcon />}{mode === "desc" ? "起草" : "创建"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ═════════ 头像：Agent Robot Avatar，颜色、头型、皮肤 ═════════ */
const SERIES: [string, string, number, number][] = [["", "纯色", 0, 0], ["glaze", "彩色玻璃", 101, 6], ["ice", "淡彩玻璃", 201, 6], ["gummy", "软糖", 301, 12], ["candy", "糖果玻璃", 401, 6], ["acrylic", "亚克力", 501, 12], ["jelly", "果冻", 601, 12], ["gradient", "渐变", 701, 12], ["frosted", "磨砂玻璃", 801, 6], ["metal", "磨砂金属", 901, 30], ["gem", "宝石与陶瓷", 1001, 12], ["wood", "木", 1101, 6], ["ball", "球类", 1201, 10], ["gloss", "透明与流光", 1301, 5], ["fabric", "织物与皮革", 1401, 12], ["food", "甜点与水果", 1501, 12], ["nature", "自然", 1601, 12], ["pattern", "图案与科技", 1701, 17]]
const SHAPES: [string, number][] = [["方", 15], ["圆角", 45], ["圆润", 70], ["圆", 95]]
function AvatarPicker({ look, onChange }: { look: { color: string; round: number; skin: string | null }; onChange: (p: Partial<{ color: string; round: number; skin: string | null }>) => void }) {
  const cur = look.skin ? SERIES.find(([, , a, n]) => +look.skin! >= a && +look.skin! < a + n)?.[0] ?? "" : ""
  const [series, setSeries] = useState(cur)
  const sr = SERIES.find((x) => x[0] === series)!
  return (
    <div className="flex flex-wrap items-start gap-6" data-avatar-picker>
      <Face look={look} size={88} />
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">颜色</span>
          {AVATAR_COLORS.map(([c, nm]) => <Tooltip key={c}><TooltipTrigger render={<button type="button" aria-label={nm} aria-pressed={look.color === c && !look.skin} data-color={c} onClick={() => onChange({ color: c, skin: null })}
            className={cn("size-6 rounded-full ring-offset-2 ring-offset-background transition-shadow", look.color === c && !look.skin ? "ring-2 ring-foreground/70" : "hover:ring-2 hover:ring-border")} style={{ background: c }} />} /><TooltipContent>{nm}</TooltipContent></Tooltip>)}
          <label className="relative size-6 cursor-pointer overflow-hidden rounded-full ring-1 ring-border" title="自定义颜色" style={{ background: "conic-gradient(red, yellow, lime, aqua, blue, magenta, red)" }}>
            <input type="color" className="absolute inset-0 cursor-pointer opacity-0" value={look.color} onChange={(e) => onChange({ color: e.target.value, skin: null })} data-color-custom /></label>
        </div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">头型</span>
          <Segmented size="sm" value={SHAPES.reduce((a, b) => (Math.abs(b[1] - look.round) < Math.abs(a[1] - look.round) ? b : a))[0]} onChange={(v) => onChange({ round: SHAPES.find((x) => x[0] === v)![1] })} options={SHAPES.map((x) => x[0])} />
          <input type="range" min={0} max={100} value={look.round} onChange={(e) => onChange({ round: +e.target.value })} className="w-28 accent-foreground" aria-label="圆度" data-round /></div>
        <div className="flex flex-wrap items-center gap-2"><span className="w-10 type-footnote text-muted-foreground">材质</span>
          <Select testId="skin-series" value={series} onChange={(v) => { setSeries(v); if (!v) onChange({ skin: null }) }} options={SERIES.map(([id, n]) => ({ v: id, l: n }))} /></div>
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
export function BotView({ k }: { k: string }) {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const b = st.bots[k]
  const [form, setForm] = useState<any>(null), [memTxt, setMemTxt] = useState(""), [editMem, setEditMem] = useState<string | null>(null)
  const [prompt, setPrompt] = useState<{ generated: string; custom: string | null } | null>(null), [pdraft, setPdraft] = useState<string | null>(null)
  if (lastBot !== k) { lastBot = k; if (botTab !== "设置") botTab = "概览" }
  useEffect(() => { if (b && botTab === "记忆" && !b.memLoaded) void loadMemories(k) }, [k, botTab, b?.memLoaded])
  useEffect(() => { if (b && botTab === "系统提示词") api.botPrompt(k).then(setPrompt).catch(() => {}) }, [k, botTab, b?.prompt, b?.memCount])
  if (!b) return <Page title="Bot" back={["#/bots", "Bot"]}><Empty title="Bot 不存在" sub="可能已经删除，或者不在当前工作空间里。" /></Page>
  const fm = form && form.k === k ? form : { k, n: b.n, role: b.role, pers: b.pers, pro: PRO[b.pro], rule: b.rule, acc: b.acc, harness: b.harness, model: b.model ?? "", color: b.color, round: b.round, skin: b.skin, project: b.project ?? "none", permMode: b.permMode, ws: b.workspaces }
  const effort = fm.reasoningEffort ?? b.reasoningEffort ?? ""
  const set = (p: any) => setForm({ ...fm, reasoningEffort: effort, ...p })
  const mine = st.items.filter((i) => i.b === k && !["done", "drop"].includes(i.st) && !i.arch)
  const tabs = ["概览", "设置", "能力", "系统提示词", "记忆", "对外身份", "定时与流程"] as const
  return (
    <Page title={b.main ? b.n : b.n + " Bot"} back={["#/bots", "Bot"]} max={720}
      actions={<>{!b.main && <><span className="hidden type-footnote text-muted-foreground sm:inline">{b.on ? "已启用" : "已停用"}</span><Switch checked={b.on} onCheckedChange={(v) => void saveBot(k, { on: v }, v ? "已启用" : "已停用")} aria-label="启用" /></>}<BotMenu k={k} /></>}>
      <div className="-mt-2 mb-6 flex items-center gap-4">
        <Robot k={k} size={64} />
        <div className="min-w-0"><div className="type-body">{b.role || "还没有填写职责"}</div><div className="mt-0.5 type-footnote text-muted-foreground">{b.h} · {b.m} · 性格：{b.pers || "未填写"} · 主动程度：{PRO[b.pro]}</div><RecentStats bot={k} className="mt-0.5 block" /><OwnerChip objectType="bot" objectId={k} className="mt-1.5" /></div>
      </div>
      <div className="-mx-4 mb-6 overflow-x-auto px-4"><Segmented value={botTab} onChange={(v) => { botTab = v; r() }} options={tabs} /></div>
      {botTab === "概览" && (mine.length ? <Group>{mine.map((i) => <ItemRow key={i.id} i={i} bot={false} />)}</Group> : <Empty title="没有进行中的事项" />)}
      {botTab === "对外身份" && <PersonasSection bot={k} />}
      {botTab === "定时与流程" && <Empty title="定时任务和流程还没有接入" sub="接入后可以让这个 Bot 按时间做事，或者先由它判断某类事件。" />}
      {botTab === "能力" && (
        <div className="flex flex-col gap-7" data-caps>
          <section><SectionLabel>Steve 的工具 · 总是可用</SectionLabel>
            <Group>{([["steve_update", "汇报进展、更新计划"], ["steve_ask", "向你提问"], ["steve_request_approval", "对外操作前请你批准"], ["steve_deliver", "交付，请你验收"], ["steve_publish", "验收后申请发布（只在配置了发布的项目里）"]] as const).map(([c, l]) => <Row key={c} title={l} sub={<span className="font-mono">{c}</span>}><CheckIcon className="size-4 text-success" /></Row>)}</Group></section>
          <section><SectionLabel>{b.h} 自带的工具</SectionLabel>
            <Group><Row title="读写文件、执行命令、搜索网页" sub={`由 ${b.h} 提供；能不能直接用，看下面的权限`} /><Row title="权限" sub={b.perm}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => { botTab = "设置"; r() }}>修改</Button></Row></Group></section>
          <div className="px-1 type-footnote text-muted-foreground">按工具单独开关、插件提供的能力（GitHub、邮件、日程）要等插件接入后才有。</div>
        </div>
      )}
      {botTab === "系统提示词" && (
        <div className="flex flex-col gap-3" data-prompt>
          <Segmented size="sm" value={b.prompt != null || pdraft != null ? "自己写" : "自动生成"} onChange={(v) => { if (v === "自己写") setPdraft(b.prompt ?? prompt?.generated ?? ""); else { setPdraft(null); if (b.prompt != null) void saveBot(k, { prompt: null }, "已改回自动生成") } }} options={["自动生成", "自己写"] as const} />
          {b.prompt == null && pdraft == null
            ? <><pre className="whitespace-pre-wrap rounded-xl bg-muted/70 px-4 py-3 font-mono text-[12.5px] leading-[20px] text-foreground/90">{prompt?.generated ?? "载入中…"}</pre><div className="px-1 type-footnote text-muted-foreground">由职责、性格、主动程度、工作规范和这个工作空间里的记忆生成；改这些设置会同步更新。每次开会话时，Steve 再在前面加上事项、项目和工具的说明。</div></>
            : <><Textarea className="min-h-72 rounded-xl font-mono text-[12.5px]" value={pdraft ?? b.prompt ?? ""} onChange={(e) => setPdraft(e.target.value)} data-prompt-edit />
              <div className="flex items-center gap-2"><Button size="sm" disabled={pdraft == null || pdraft === b.prompt} onClick={() => { void saveBot(k, { prompt: pdraft ?? "" }); setPdraft(null) }}>保存</Button><span className="type-footnote text-muted-foreground">自己写的提示词会完全替换自动生成的部分；记忆仍会附在最后。安全规则（对外发送、花钱、删除需批准）总是生效。</span></div></>}
        </div>
      )}
      {botTab === "记忆" && (
        <div className="flex flex-col gap-3" data-mem>
          <div className="px-1 type-footnote text-muted-foreground">只显示在「{st.workspace?.name}」里的记忆；别的工作空间里的记忆分开存，这里看不到。</div>
          <div className="flex gap-2"><Input className="rounded-lg" placeholder="添加一条记忆，例如：周五下午不安排部署" value={memTxt} onChange={(e) => setMemTxt(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && memTxt.trim()) { addMem(k, memTxt.trim()); setMemTxt("") } }} data-mem-input /><Button variant="outline" disabled={!memTxt.trim()} onClick={() => { addMem(k, memTxt.trim()); setMemTxt("") }}>添加</Button></div>
          {!b.memLoaded ? <div className="px-1 type-callout text-muted-foreground">载入中…</div> : <Group>{b.mem.map(([t, src, id]) => <Row key={id} title={editMem === id ? <Input autoFocus className="h-8 rounded-lg" defaultValue={t} onBlur={(e) => { const v = e.target.value.trim(); if (v && v !== t) editMem && editMemFn(k, id, v); setEditMem(null) }} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} /> : t} sub={src}>
            <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEditMem(id)}>编辑</Button><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => delMem(k, id)}>删除</Button></Row>)}{!b.mem.length && <Row title="暂无记忆" sub="你手动加的偏好和规则会写在这里，开会话时带给它" />}</Group>}
        </div>
      )}
      {botTab === "设置" && (
        <FieldGroup className="gap-8">
          <AskSteve key={k} ctx={{ kind: "bot", id: k }} placeholder={`告诉 Steve ${b.main ? b.n : b.n + " Bot"} 怎么配，例如：改用 opus，做完自动验收，说话更简洁`} />
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">身份</SectionLabel>
            <F label="头像" hint="Agent Robot Avatar：选一个颜色或材质，再调头型。头像是静止的"><AvatarPicker look={{ color: fm.color, round: fm.round, skin: fm.skin }} onChange={(p) => set(p)} /></F>
            <F label="名字" htmlFor="bot-name"><Input id="bot-name" className="rounded-lg" value={fm.n} onChange={(e) => set({ n: e.target.value })} /></F>
            <F label="职责" hint="Steve 按职责决定把什么事交给它"><Input className="rounded-lg" value={fm.role} onChange={(e) => set({ role: e.target.value })} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">性格</SectionLabel>
            <F label="说话方式" hint="会写进系统提示词"><Textarea className="min-h-20 rounded-lg" value={fm.pers} onChange={(e) => set({ pers: e.target.value })} /></F>
            <F label="主动程度" hint="多久打扰你一次"><Segmented size="sm" value={fm.pro} onChange={(v) => set({ pro: v })} options={PRO} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">工作方式</SectionLabel>
            <F label="工作规范" hint="怎么做事"><Textarea className="min-h-20 rounded-lg" value={fm.rule} onChange={(e) => set({ rule: e.target.value })} /></F>
            <F label="交付验收"><Segmented size="sm" value={fm.acc} onChange={(v) => set({ acc: v })} options={Object.values(ACC)} /></F>
          </section>
          <section className="flex flex-col gap-4"><SectionLabel className="mb-0">运行</SectionLabel>
            <div className="grid gap-4 sm:grid-cols-2">
              <F label="Harness"><div><Select value={fm.harness} onChange={(v) => set({ harness: v, model: "", reasoningEffort: "" })} options={st.runtimes.map((x) => ({ v: x.id, l: `${x.name}${x.ok ? "" : "（不可用）"}` }))} /></div></F>
              <F label="模型"><div><ModelPicker harness={fm.harness} value={fm.model ?? ""} allowDefault testId="bot-model" onChange={(m) => set({ model: m })} /></div></F>
              <F label="思考强度" hint={b.main ? "Steve 的判断和它的执行会话都用这个强度。" : "对新会话生效；实际可用档位由所选模型与 Harness 决定。"}><div><Select value={effort || "default"} onChange={(v) => set({ reasoningEffort: v === "default" ? "" : v })} options={[{ v: "default", l: "跟随模型默认" }, ...(fm.harness === "codex" ? ["none", "minimal", "low", "medium", "high", "xhigh"] : ["low", "medium", "high", "max"]).map((v) => ({ v, l: ({ none: "关闭 · none", minimal: "最低 · minimal", low: "低 · low", medium: "中 · medium", high: "高 · high", xhigh: "极高 · xhigh", max: "最大 · max" } as Record<string, string>)[v] }))]} /></div></F>
            </div>
            {b.main && <SteveJudges harness={fm.harness} model={fm.model ?? ""} />}
            <F label="运行设备" hint="现在只有 Hub 这一台执行节点；配对更多设备后可以固定在某台上"><div><Select testId="bot-dev" value="hub" onChange={() => {}} options={st.devices.map((d) => ({ v: d.id, l: d.name }))} /></div></F>
            <F label="默认项目" hint="没指定项目的事项，交给它时默认放进这个项目"><div><Select value={fm.project} onChange={(v) => set({ project: v })} options={[{ v: "none", l: "按事项推断" }, ...st.projects.map((p) => ({ v: p.id, l: p.name }))]} /></div></F>
            <F label="权限" hint="对外发送、花钱、删除总是需要你批准"><div><Select value={fm.permMode} onChange={(v) => set({ permMode: v })} options={Object.entries(PERM).map(([v, l]) => ({ v, l }))} /></div></F>
            {!b.main && <F label="参与的工作空间" hint="在哪个空间干活，就只看得到那个空间的内容；记忆按空间分开">
              <div className="flex flex-col gap-1.5">{st.workspaces.map((w) => <label key={w.id} className="flex items-center gap-2 type-callout"><Checkbox checked={fm.ws.includes(w.id)} disabled={fm.ws.length === 1 && fm.ws.includes(w.id)} onCheckedChange={(v) => set({ ws: v ? [...fm.ws, w.id] : fm.ws.filter((x: string) => x !== w.id) })} />{w.name}</label>)}</div></F>}
          </section>
          <div className="flex gap-2"><Button data-save-bot onClick={async () => {
            await saveBot(k, { n: fm.n.trim() || b.n, role: fm.role, pers: fm.pers, pro: PRO.indexOf(fm.pro), rule: fm.rule, acc: fm.acc, harness: fm.harness, model: fm.model || null, reasoningEffort: effort || null, color: fm.color, round: fm.round, skin: fm.skin, project: fm.project === "none" ? null : fm.project, permMode: fm.permMode })
            if (!b.main && JSON.stringify([...fm.ws].sort()) !== JSON.stringify([...b.workspaces].sort())) setBotWorkspaces(k, fm.ws)
            setForm(null); botTab = "概览"; r() }}>保存</Button><Button variant="ghost" onClick={() => { setForm(null); botTab = "概览"; r() }}>取消</Button></div>
        </FieldGroup>
      )}
    </Page>
  )
}
const editMemFn = (k: string, id: string, t: string) => editMem(k, id, t)

/* ═════════ 设置：分类页 ═════════ */
const CATS: [string, string, any, string][] = [
  ["general", "通用", SlidersHorizontalIcon, "启动页、离开期间汇报、引导"],
  ["workspace", "工作空间", LayersIcon, "名称、说明、编号前缀"],
  ["items", "事项与验收", ListChecksIcon, "验收、前置、交接、归档"],
  ["tags", "标签", TagIcon, "事项标签：名称、分类、描述"],
  ["notify", "通知", BellIcon, "推送渠道、免打扰"],
  ["look", "外观", SunMoonIcon, "主题、字号、动态效果"],
  MODELS_CAT,
  VAULT_CAT,
  STATS_CAT,
  ["data", "数据与隐私", DatabaseIcon, "数据位置、备份、清理"],
  ["about", "关于", InfoIcon, "版本"],
]
const PH: Record<string, string> = {
  general: "告诉 Steve 你想怎么改，例如：离开 8 小时以上再汇报", items: "例如：默认改成自动验收，完成 7 天后自动归档",
  tags: "例如：加一个叫「法务」的标签，管合同和合规审查", models: "例如：对话回答改用 Codex，研究想深一点", notify: "例如：免打扰改成 22 点到 9 点", look: "例如：字号大一点", data: "例如：会话记录保留一年",
}
const Sel = ({ v, o, on, testId }: { v: string; o: [string, string][]; on: (v: string) => void; testId?: string }) => <Select align="end" testId={testId} value={v} onChange={on} options={o.map(([x, l]) => ({ v: x, l }))} />
const Later = () => <span className="ml-1.5 rounded bg-muted px-1.5 py-px text-[11px] font-normal text-muted-foreground">接入推送后生效</span>
export function SettingsView() {
  const [, cat] = useRoute()
  const wide = typeof window !== "undefined" && window.innerWidth >= 768
  const cur = cat ?? (wide ? "general" : "")
  if (["runtime", "accounts", "plugins"].includes(cur)) { setTimeout(() => go(cur === "runtime" ? "#/devices" : cur === "accounts" ? "#/accounts" : "#/plugins")); return null }
  const C = CATS.find((c) => c[0] === cur)
  const nav = (
    <nav className="flex flex-col gap-0.5" data-settings-nav>
      {CATS.map(([id, l, I, d]) => (
        <a key={id} href={"#/settings/" + id} data-settings-cat={id} className={cn("flex items-center gap-3 rounded-lg px-2.5 py-2 type-callout transition-colors hover:bg-accent", cur === id && "bg-accent font-medium")}>
          <I className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} /><span className="min-w-0 flex-1"><span className="block">{l}</span><span className="block truncate type-footnote font-normal text-muted-foreground md:hidden">{d}</span></span><ChevronRightIcon className="size-4 text-tertiary md:hidden" />
        </a>))}
    </nav>
  )
  if (!wide && !C) return <Page title="设置" max={640}>{nav}</Page>
  if (!C) return null
  return (
    <Page title={wide ? "设置" : C[1]} back={wide ? undefined : ["#/settings", "设置"]} max={960}>
      <div className="grid gap-10 md:grid-cols-[200px_minmax(0,1fr)]">
        {wide && <div className="-mt-1">{nav}</div>}
        <div className="min-w-0 max-w-[640px]" data-settings-pane={cur}>
          {wide && <h2 className="mb-5 type-title">{C[1]}</h2>}
          {PH[cur] && <AskSteve key={cur} className="mb-8" ctx={cur === "tags" || cur === "models" ? { kind: cur as "tags" | "models" } : { kind: "settings", id: cur }} placeholder={PH[cur]} />}
          <Pane id={cur} />
        </div>
      </div>
    </Page>
  )
}
function Pane({ id }: { id: string }) {
  const c = st.settings
  if (id === "tags") return <TagsPane />
  if (id === "workspace") return <WorkspacePane />
  if (id === "vault") return <VaultSettingsPane />
  if (id === "stats") return <StatsSettingsPane />
  if (id === "models") return <ModelsSettingsPane />
  if (id === "general") return <div className="flex flex-col gap-8">
    <section><SectionLabel>助手</SectionLabel><Group><Row icon={<BotAvatar k="steve" size={36} />} title={st.bots.steve?.n} sub="名字、性格、头像在 Bot 里修改" onClick={() => go("#/bots/steve")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>
    <Group>
      <Row title="打开时进入" sub="之后从通知进入时，直接打开对应的事"><Sel v={c.home} o={["对话", "待处理", "事项"].map((x) => [x, x])} on={(v) => setSetting({ home: v })} /></Row>
      <Row title="离开期间汇报" sub="离开超过这么久再打开对话时，Steve 先汇报"><Sel v={c.awayH} o={[["2", "2 小时"], ["4", "4 小时"], ["8", "8 小时"], ["0", "不汇报"]]} on={(v) => setSetting({ awayH: v })} /></Row>
    </Group>
    <Group>
      <Row title="首次引导" sub="运行环境、项目、交代第一件事"><Button size="sm" variant="outline" onClick={() => { localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }} data-restart-onboard>重新开始</Button></Row>
      <Row title="快捷键" sub="⌘K 搜索 · # 引用事项 · @ 指定 Bot · / 命令" />
    </Group>
  </div>
  if (id === "items") return <div className="flex flex-col gap-8">
    <section><SectionLabel>验收</SectionLabel><Group>
      <Row title="默认验收方式" sub="可在 Bot 或单个事项上覆盖。自动验收只在 Bot 的自检全部通过时生效"><Segmented size="sm" value={c.accDefault === "auto" ? "自动验收" : "需要你验收"} onChange={(v) => setSetting({ accDefault: v === "自动验收" ? "auto" : "manual" })} options={["需要你验收", "自动验收"]} /></Row></Group></section>
    <section><SectionLabel>编号与依赖</SectionLabel><Group>
      <Row title="事项编号前缀" sub="在 设置 · 工作空间 里改"><span className="font-mono type-callout">{st.prefix}-{st.noSeq + 1}</span></Row>
      <Row title="前置完成后"><Sel v={c.depsAfter} o={["自动开始", "提醒我开始"].map((x) => [x, x])} on={(v) => setSetting({ depsAfter: v })} /></Row></Group></section>
    <section><SectionLabel>会话</SectionLabel><Group>
      <Row title="交接阈值" sub="上下文超过这个比例后，下一条补充会带交接摘要开新会话"><Sel v={c.handoff} o={[["70", "70%"], ["80", "80%"], ["90", "90%"]]} on={(v) => setSetting({ handoff: v })} /></Row></Group></section>
    <section><SectionLabel>归档</SectionLabel><Group>
      <Row title="完成后自动归档" sub="完成或放弃的事项过了这么久移到“已归档”"><Sel testId="arch-days" v={c.archDays} o={[["7", "7 天"], ["30", "30 天"], ["90", "90 天"], ["0", "不自动归档"]]} on={(v) => setSetting({ archDays: v })} /></Row></Group></section>
  </div>
  if (id === "notify") return <div className="flex flex-col gap-8">
    <div className="rounded-xl border px-4 py-3 type-callout text-muted-foreground">现在还没有推送渠道（手机 App、邮件、IM 都没有接入），需要你决定的事都在“待处理”里。下面的设置先存着，接入后生效。</div>
    <section><SectionLabel>时间</SectionLabel><Group>
      <Row title={<>免打扰<Later /></>} sub="期间只记录，不推送；异常除外"><Sel v={c.quiet} o={["23:00 – 08:00", "22:00 – 09:00", "关闭"].map((x) => [x, x])} on={(v) => setSetting({ quiet: v })} /></Row>
      <Row title={<>每天汇报<Later /></>} sub="现在可以在对话里输入 /汇报"><Sel v={c.digestAt} o={["08:30", "09:00", "不发"].map((x) => [x, x])} on={(v) => setSetting({ digestAt: v })} /></Row></Group></section>
  </div>
  if (id === "look") return <div className="flex flex-col gap-8">
    <section><SectionLabel>Bot 头像</SectionLabel>
      <div className="mb-3 flex flex-wrap items-center gap-4 rounded-xl bg-muted/50 px-4 py-3" data-avatar-preview>{Object.keys(st.bots).map((k) => <a key={k} href={"#/bots/" + k} className="flex flex-col items-center gap-1"><Robot k={k} size={44} /><span className="type-footnote text-muted-foreground">{st.bots[k].n}</span></a>)}</div>
      <div className="px-1 type-footnote text-muted-foreground">所有 Bot 都用 Agent Robot Avatar。颜色、材质和头型在每个 Bot 的设置里改。</div></section>
    <section><SectionLabel>界面</SectionLabel><Group>
      <Row title="主题" sub="只影响这台设备"><ThemeSwitch labels /></Row>
      <Row title="字号"><Segmented size="sm" value={c.fontSize} onChange={(v) => setSetting({ fontSize: v })} options={["标准", "大"]} /></Row>
      <Row title="减弱动态效果" sub="也会跟随系统设置"><Switch checked={c.reduceMotion === "开"} onCheckedChange={(v) => setSetting({ reduceMotion: v ? "开" : "关" })} /></Row>
    </Group></section></div>
  if (id === "data") return <div className="flex flex-col gap-8">
    <section><SectionLabel>Hub 上的数据</SectionLabel><Group>
      <Row title="数据位置" sub={<span className="font-mono">~/.steve/data/steve.sqlite3</span>} />
      <Row title="备份" sub="每天 03:30 备份到 ~/.steve/backups，保留 14 天；每次发布前也会备份一次" /></Group></section>
    <section><SectionLabel>保留期限</SectionLabel><Group>
      <Row title={<>会话记录<span className="ml-1.5 rounded bg-muted px-1.5 py-px text-[11px] font-normal text-muted-foreground">清理任务还没做</span></>}><Sel v={c.keepSess} o={["90 天", "1 年", "永久"].map((x) => [x, x])} on={(v) => setSetting({ keepSess: v })} /></Row></Group></section>
    <section><SectionLabel>清理</SectionLabel><Group>
      <Row title="清除这个工作空间里所有 Bot 的记忆" sub="其他工作空间的记忆不受影响"><DangerBtn label="清除" what="这个工作空间里所有 Bot 的记忆会被删除，不能恢复。" onOk={() => void api.clearMemories().then(() => toast("已清除"))} /></Row></Group></section>
  </div>
  if (id === "about") return <div className="flex flex-col gap-8">
    <AboutVersions />
    <section><SectionLabel>发布与引导</SectionLabel><Group>
    <Row title="发布" sub="开发 Bot 在事项里申请，你批准后上线；检查不通过会自动回到上一个版本" />
    <Row title="重新看首次引导" sub="在对话里重新走一遍"><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { finishOnboarding(); localStorage.removeItem("steve-onboarded"); st.onboard = 0; emit(); go("#/") }}>开始</Button></Row>
    </Group></section>
  </div>
  return null
}
function WorkspacePane() {
  const w = st.workspace!
  const [n, setN] = useState(w.name), [d, setD] = useState(w.description), [p, setP] = useState(w.item_prefix)
  useEffect(() => { setN(w.name); setD(w.description); setP(w.item_prefix) }, [w.id])
  const prefix = p.toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6)
  const dirty = n.trim() !== w.name || d.trim() !== w.description || prefix !== w.item_prefix
  return <div className="flex flex-col gap-8">
    <p className="type-callout text-muted-foreground">工作空间是数据的边界：事项、项目、对话、标签、设置和 Bot 的记忆都按工作空间分开。左上角可以切换。</p>
    <FieldGroup className="gap-4">
      <F label="名称"><Input className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} /></F>
      <F label="说明"><Textarea className="min-h-16 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} /></F>
      <F label="事项编号前缀" hint={`新事项的编号，例如 ${prefix || "ST"}-${st.noSeq + 1}；已有编号不变`}><Input className="w-28 rounded-lg font-mono uppercase" value={p} maxLength={6} onChange={(e) => setP(e.target.value)} data-prefix /></F>
      {dirty && <div className="flex gap-2"><Button size="sm" disabled={!n.trim() || !prefix} onClick={() => void api.patchWs(w.id, { name: n.trim(), description: d.trim(), item_prefix: prefix }).then(() => { toast("已保存"); return import("@/store").then((m) => m.refresh()) }).catch((e) => toast(e.message))}>保存</Button><Button size="sm" variant="ghost" onClick={() => { setN(w.name); setD(w.description); setP(w.item_prefix) }}>取消</Button></div>}
    </FieldGroup>
    <section><SectionLabel>这里有</SectionLabel><Group>
      <Row icon={<FolderIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />} title={`${st.projects.length} 个项目`} onClick={() => go("#/projects")}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
      <Row title={`${Object.keys(st.bots).length} 个 Bot`} onClick={() => go("#/bots")}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
      <Row title={`${st.items.length} 件事项`} onClick={() => go("#/items")}><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>
    <section><SectionLabel>所有工作空间 · {st.workspaces.length}</SectionLabel>
      <Group>{st.workspaces.map((x) => <Row key={x.id} title={x.name} sub={x.description || undefined}>{x.id === w.id ? <span className="type-footnote text-muted-foreground">当前</span> : <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void import("@/store").then((m) => m.switchWorkspace(x.id))}>切换</Button>}</Row>)}</Group>
      <Button size="sm" variant="outline" className="mt-3" onClick={() => openSheet("newWorkspace")}><PlusIcon />新建工作空间</Button></section>
  </div>
}
function DangerBtn({ label, what, onOk }: { label: string; what: string; onOk: () => void }) {
  const [o, setO] = useState(false)
  return <>
    <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setO(true)}>{label}</Button>
    <AlertDialog open={o} onOpenChange={setO}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{label}？</AlertDialogTitle><AlertDialogDescription>{what}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { setO(false); onOk() }}>{label}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </>
}

/* ═════════ 标签 ═════════ */
let tagCat = "全部"
function TagsPane() {
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [del, setDel] = useState<string | null>(null), [merge, setMerge] = useState<string | null>(null), [into, setInto] = useState("")
  const cats = ["全部", ...TAG_CATS.filter((c) => st.tags.some((t) => t.cat === c))]
  const d = del ? tag(del) : null, m = merge ? tag(merge) : null
  return <div className="flex flex-col gap-6" data-tags-pane>
    <p className="type-callout text-muted-foreground">建事项时，Steve 会把事项目标和这里所有标签的名称、分类、描述一起交给模型，先从已有标签里选；都不合适才新建一个，并写好分类和描述。描述写得越具体，归类越准。标签按工作空间分开。</p>
    <div className="flex flex-wrap items-center gap-2"><Segmented size="sm" value={cats.includes(tagCat) ? tagCat : "全部"} onChange={(v) => { tagCat = v; r() }} options={cats} /><span className="flex-1" /><Button size="sm" onClick={() => openSheet("tag", {})} data-new-tag-btn><PlusIcon />新建标签</Button></div>
    {!st.tags.length && <Empty title="还没有标签" sub="建事项时 Steve 会按需要新建，也可以自己先建几个。" />}
    {TAG_CATS.filter((c) => tagCat === "全部" || c === tagCat).map((c) => { const xs = st.tags.filter((t) => t.cat === c); return xs.length ? (
      <section key={c}><SectionLabel>{c} · {xs.length}</SectionLabel>
        <Group>{xs.map((t) => (
          <Row key={t.id} title={<span className="inline-flex items-center gap-2">{t.name}<span className="type-footnote text-muted-foreground">{tagUses(t.id)} 件事项</span>{t.by === "Steve" && <span className="type-footnote text-tertiary">Steve 建的 · {t.created}</span>}</span>} sub={t.desc} data-tag-row={t.name}>
            <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" data-tag-menu={t.name} />}><MoreHorizontalIcon /></DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-40">
                <DropdownMenuItem onClick={() => showTag(t.id)} data-show-tag>查看事项</DropdownMenuItem>
                <DropdownMenuItem onClick={() => openSheet("tag", { id: t.id })} data-edit-tag><PencilIcon />编辑</DropdownMenuItem>
                {st.tags.length > 1 && <DropdownMenuItem onClick={() => { setMerge(t.id); setInto(st.tags.find((x) => x.id !== t.id && x.cat === t.cat)?.id ?? st.tags.find((x) => x.id !== t.id)!.id) }} data-merge-tag>合并到…</DropdownMenuItem>}
                <DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(t.id)} data-del-tag><TrashIcon />删除</DropdownMenuItem>
              </DropdownMenuContent></DropdownMenu>
          </Row>))}</Group></section>) : null })}
    <AlertDialog open={!!d} onOpenChange={(o) => !o && setDel(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>删除标签「{d?.name}」？</AlertDialogTitle><AlertDialogDescription>{del ? tagUses(del) : 0} 件事项上的这个标签会被移除，事项本身不受影响。想保留归类，可以先合并到别的标签。</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { deleteTag(del!); setDel(null) }} data-del-tag-confirm>删除</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
    <AlertDialog open={!!m} onOpenChange={(o) => !o && setMerge(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>把「{m?.name}」合并到…</AlertDialogTitle><AlertDialogDescription>{merge ? tagUses(merge) : 0} 件事项会改用目标标签，「{m?.name}」随后删除。</AlertDialogDescription></AlertDialogHeader>
      <Select testId="merge-into" value={into} onChange={setInto} options={st.tags.filter((x) => x.id !== merge).map((x) => ({ v: x.id, l: `${x.name} · ${x.cat}` }))} />
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction onClick={() => { mergeTag(merge!, into); setMerge(null) }} data-merge-confirm>合并</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </div>
}
/** create or edit a tag; from an item's picker the new tag is attached to that item */
export function TagDialog({ id, item, name }: { id?: string; item?: string; name?: string }) {
  const t = id ? tag(id) : undefined
  const [n, setN] = useState(t?.name ?? name ?? ""), [c, setC] = useState(t?.cat ?? "类型"), [d, setD] = useState(t?.desc ?? "")
  const dup = st.tags.find((x) => x.name === n.trim() && x.id !== id)
  const ok = n.trim() && d.trim() && !dup
  const save = async () => {
    if (!ok) return
    if (t) saveTag(t.id, { name: n.trim(), cat: c, desc: d.trim() })
    else { const nid = await makeTag({ name: n.trim(), cat: c, desc: d.trim() }); if (nid && item) setTags(item, [...(it(item).tags ?? []), nid]); if (nid) toast(`已新建标签「${n.trim()}」`) }
    closeSheet()
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-tag-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t ? "编辑标签" : "新建标签"}</DialogTitle><DialogDescription className="type-callout">分类和描述给 Steve 归类用：写清这个标签包括什么、不包括什么。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <F label="名称" htmlFor="tag-name" hint={dup ? `已有同名标签（${dup.cat}）` : undefined}><Input id="tag-name" autoFocus className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder="例如：财务" /></F>
          <F label="分类"><div><Select testId="tag-cat" value={c} onChange={setC} options={TAG_CATS.map((x) => ({ v: x, l: x }))} /></div></F>
          <F label="描述" htmlFor="tag-desc"><Textarea id="tag-desc" className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder="例如：报销、发票、账单、费用和预算；不包括合同" data-tag-desc /></F>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>取消</Button><Button disabled={!ok} onClick={() => void save()} data-testid="tag-go">{t ? "保存" : "新建"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
