/* #/auto/schedules — 定时任务 (schedules.md §9, prototype Schedules / NewScheduleDialog) */
import { useEffect, useState, type ReactNode } from "react"
import { CheckIcon, ChevronRightIcon, ClockIcon, HistoryIcon, PlayIcon, PlusIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Group, Meta, Empty, StatusDot, BotAvatar, go } from "@/ui"
import { st, botName, toast } from "@/store"
import { flowApi, errText, type Load, type Parsed, type Schedule, type ScheduleBody, type ScheduleMode, type ScheduleRun } from "@/api/flow"
import { HubError } from "@/api"
import { Ico, Modal, Pick, Failed, when, botOptions, mainBot, RUN_STATUS } from "@/auto/common"

export const MODE_LABEL: Record<ScheduleMode, string> = { item_auto: "建事项并开始，自动验收", item: "建事项并开始", propose: "先给方案", report: "在对话里发一条汇报" }
const MISSED_LABEL = { catch_up_once: "补跑一次", skip: "跳过" } as const
const OVERLAP_LABEL = { skip: "跳过这一次", run: "照常运行" } as const
const TRIGGER_LABEL: Record<string, string> = { time: "按时", manual: "手动", catch_up: "补跑" }
const TZS = ["Asia/Taipei", "Asia/Shanghai", "Asia/Tokyo", "UTC", "Europe/London", "America/Los_Angeles", "America/New_York"]
const usd = (n: number) => (n ? `$${n.toFixed(2)}` : "不限")

export function Schedules({ L, onNew }: { L: Load<Schedule[]>; onNew: () => void }) {
  const [open, setOpen] = useState<string | null>(null), [del, setDel] = useState<Schedule | null>(null), [edit, setEdit] = useState<Schedule | null>(null), [busy, setBusy] = useState<string | null>(null)
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  const act = async (key: string, f: () => Promise<unknown>, ok: string) => {
    setBusy(key)
    try { await f(); toast(ok); await L.reload() } catch (e) { toast(errText(e)) } finally { setBusy(null) }
  }
  return (<>
    <AlertDialog open={!!del} onOpenChange={(o) => !o && setDel(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>删除定时任务「{del?.name}」？</AlertDialogTitle><AlertDialogDescription>之后不再触发；运行记录和已建的事项保留。只是想暂时不跑，可以关掉开关。</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const d = del!; setDel(null); setOpen(null); void act("del", () => flowApi.deleteSchedule(d.id), "已删除定时任务") }} data-del-schedule-confirm>删除</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
    {edit && <NewScheduleDialog edit={edit} onClose={() => setEdit(null)} onSaved={() => void L.reload()} />}
    {!L.data.length ? <><Empty title="还没有定时任务" sub="每天、每周或按时间点交给某个 Bot 做一件事，比如离开期间汇报、每周五整理周报。" /><div className="flex justify-center"><Button variant="outline" onClick={onNew}><PlusIcon />新建定时任务</Button></div></>
    : <Group>{L.data.map((s) => (
      <Collapsible key={s.id} open={open === s.id} onOpenChange={(o) => setOpen(o ? s.id : null)}>
        <div className="flex items-center gap-3 px-2 py-3">
          <Ico I={ClockIcon} />
          <CollapsibleTrigger className="min-w-0 flex-1 text-left" data-schedule-row={s.id}>
            <div className="truncate type-body">{s.name}</div>
            <Meta className="mt-0.5"><span>{s.describe}</span><span>{s.enabled ? (s.next_at ? "下次 " + when(s.next_at) : "按计划") : "已停用"}</span>
              {s.last && <span className="inline-flex items-center gap-1">{s.last.status === "failed" ? <XIcon className="size-3 text-destructive" aria-label="上次失败" /> : <CheckIcon className="size-3 text-success" aria-label="上次成功" />}{s.last.text || RUN_STATUS[s.last.status]?.[0]}</span>}</Meta>
          </CollapsibleTrigger>
          <BotAvatar k={s.bot} size={22} className="hidden sm:block" />
          <Button size="xs" variant="ghost" className="hidden text-muted-foreground sm:inline-flex" disabled={busy === "run" + s.id} onClick={() => void act("run" + s.id, () => flowApi.runSchedule(s.id), "已开始运行 " + s.name)} data-run-now-row={s.id}><PlayIcon />立即运行</Button>
          <Switch checked={s.enabled} disabled={busy === "on" + s.id} onCheckedChange={(v) => void act("on" + s.id, () => flowApi.patchSchedule(s.id, { enabled: v }), v ? "已启用" : "已停用，之后不再触发")} aria-label={"启用" + s.name} />
        </div>
        <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden transition-[height] duration-200 data-ending-style:h-0 data-starting-style:h-0">
          {open === s.id && <ScheduleDetail s={s} busy={busy} onRun={() => void act("run" + s.id, () => flowApi.runSchedule(s.id), "已开始运行 " + s.name)} onEdit={() => setEdit(s)} onDelete={() => setDel(s)} />}
        </CollapsibleContent>
      </Collapsible>
    ))}</Group>}
  </>)
}

function ScheduleDetail({ s, busy, onRun, onEdit, onDelete }: { s: Schedule; busy: string | null; onRun: () => void; onEdit: () => void; onDelete: () => void }) {
  const [runs, setRuns] = useState<ScheduleRun[] | null>(null), [more, setMore] = useState(true)
  const load = async (before?: number) => {
    try { const xs = await flowApi.scheduleRuns(s.id, before, 20); setRuns((r) => (before ? [...(r ?? []), ...xs] : xs)); setMore(xs.length === 20) } catch (e) { if (!(e instanceof HubError && e.status === 404)) toast(errText(e)); setRuns((r) => r ?? []) }
  }
  useEffect(() => { void load() }, [s.id, s.last?.at, busy])
  const rows: [string, ReactNode][] = [
    ["时间", <>{s.describe} <span className="font-mono text-[12px] text-muted-foreground">{s.cron}</span> <span className="text-muted-foreground">· {s.tz}</span></>],
    ["做什么", <span className="whitespace-pre-wrap">{s.what}</span>],
    ["执行", <span className="inline-flex items-center gap-1.5"><BotAvatar k={s.bot} size={16} />{botName(s.bot)} · {MODE_LABEL[s.mode]}</span>],
    ["错过时", MISSED_LABEL[s.missed]], ["上次没结束", OVERLAP_LABEL[s.overlap]], ["每次预算", usd(s.budget_usd)],
  ]
  return (
    <div className="grid gap-4 px-2 pb-4 sm:grid-cols-2 sm:pl-12" data-schedule-detail={s.id}>
      <dl className="grid grid-cols-[72px_1fr] gap-x-3 gap-y-1.5 type-callout">
        {rows.map(([k, v]) => <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="min-w-0">{v}</dd></div>)}
      </dl>
      <div>
        <div className="mb-1.5 flex items-center gap-1.5 type-label text-muted-foreground"><HistoryIcon className="size-3.5" aria-hidden />运行记录</div>
        {!runs ? <div className="type-footnote text-muted-foreground">载入中…</div> : !runs.length ? <div className="type-footnote text-muted-foreground">还没有运行</div> : <ul className="flex max-h-64 flex-col overflow-y-auto" data-schedule-runs>
          {runs.map((r) => {
            const go_ = r.item_id ? () => go("#/items/" + r.item_id) : undefined
            return (
              <li key={r.id}><button type="button" disabled={!go_} onClick={go_} className="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left type-footnote enabled:hover:bg-accent" title={r.error ?? undefined} data-schedule-run={r.id}>
                <StatusDot s={RUN_STATUS[r.status]?.[1] ?? "idle"} /><span className="w-24 shrink-0 tabular-nums text-muted-foreground">{when(r.at)}</span>
                <span className="min-w-0 flex-1 truncate">{r.error ? <span className="text-destructive">{r.error}</span> : r.outcome_label || RUN_STATUS[r.status]?.[0]}</span>
                <span className="shrink-0 text-muted-foreground">{TRIGGER_LABEL[r.trigger] ?? r.trigger}{r.cost_usd ? " · " + usd(r.cost_usd) : ""}</span>
                {go_ && <ChevronRightIcon className="size-3.5 shrink-0 text-tertiary" aria-hidden />}
              </button></li>)
          })}
          {more && <li><button type="button" className="px-1 py-1 type-footnote text-blue hover:underline" onClick={() => void load(runs[runs.length - 1]?.started)}>更早的</button></li>}
        </ul>}
      </div>
      <div className="flex flex-wrap gap-2 sm:col-span-2"><Button size="sm" variant="outline" disabled={busy === "run" + s.id} onClick={onRun} data-run-now><PlayIcon />立即运行</Button><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={onEdit} data-edit-schedule>编辑</Button><span className="flex-1" /><Button size="sm" variant="ghost" className="text-destructive" onClick={onDelete} data-del-schedule>删除</Button></div>
    </div>
  )
}

/* the prototype's four templates; they only fill the form */
const TEMPLATES = (): { n: string; when: string; what: string; mode: ScheduleMode; bot: string; show: boolean }[] => [
  { n: "离开期间汇报", when: "每天 08:30", what: "在对话里汇报离开期间的事：完成了什么、等我决定的、出了问题的。", mode: "report", bot: mainBot(), show: true },
  { n: "整理本周周报", when: "每周五 09:00", what: "看本周完成和进行中的事项，整理成周报。", mode: "item_auto", bot: mainBot(), show: true },
  { n: "清理已结束会话的项目目录", when: "每天 23:00", what: "清理已结束会话留下的项目工作目录，先列出要删的，再删除。", mode: "item", bot: Object.keys(st.bots).find((k) => /运维|ops/i.test(st.bots[k].n + k)) ?? mainBot(), show: true },
  { n: "月度账单汇总", when: "每月 1 日 10:00", what: "汇总上个月邮件里的账单和发票，列出金额和到期日。", mode: "report", bot: Object.keys(st.bots).find((k) => /邮件|mail/i.test(st.bots[k].n + k)) ?? "", show: Object.keys(st.bots).some((k) => /邮件|mail/i.test(st.bots[k].n + k)) },
]

export function NewScheduleDialog({ edit, bot, onClose, onSaved }: { edit?: Schedule; bot?: string; onClose: () => void; onSaved: (s: Schedule) => void }) {
  const [name, setName] = useState(edit?.name ?? ""), [w, setW] = useState(edit?.when ?? "每周一 09:00"), [what, setWhat] = useState(edit?.what ?? "")
  const [b, setB] = useState(edit?.bot ?? bot ?? mainBot()), [mode, setMode] = useState<ScheduleMode>(edit?.mode ?? "item_auto")
  const [missed, setMissed] = useState<Schedule["missed"]>(edit?.missed ?? "catch_up_once"), [overlap, setOverlap] = useState<Schedule["overlap"]>(edit?.overlap ?? "skip")
  const [budget, setBudget] = useState(String(edit?.budget_usd ?? 1)), [adv, setAdv] = useState(false)
  const [cron, setCron] = useState(""), [tz, setTz] = useState(edit?.tz ?? st.settings.tz ?? "")
  const [parsed, setParsed] = useState<Parsed | null>(null), [perr, setPerr] = useState(""), [busy, setBusy] = useState(false)
  /* live parse, 300 ms after you stop typing */
  useEffect(() => {
    if (cron.trim() || !w.trim()) { setParsed(null); setPerr(""); return }
    const t = setTimeout(() => { flowApi.parseWhen(w.trim(), tz || undefined).then((p) => { setParsed(p); setPerr("") }, (e) => { setParsed(null); setPerr(e instanceof HubError ? e.message : errText(e)) }) }, 300)
    return () => clearTimeout(t)
  }, [w, tz, cron])
  const bud = Number(budget), budOk = budget.trim() !== "" && bud >= 0 && bud <= 50
  const ok = name.trim() && name.trim().length <= 40 && (cron.trim() || (w.trim() && !perr)) && what.trim() && b && budOk
  const submit = async () => {
    const body: ScheduleBody = { name: name.trim(), what: what.trim(), bot: b, mode, missed, overlap, budget_usd: bud, ...(tz ? { tz } : {}) }
    if (cron.trim()) body.cron = cron.trim()
    if (!edit || w.trim() !== edit.when || cron.trim()) { if (w.trim()) body.when = w.trim() }
    setBusy(true)
    try { const s = edit ? await flowApi.patchSchedule(edit.id, body) : await flowApi.newSchedule(body); toast(edit ? "已保存" : `已创建定时任务，${s.next_at ? "下次 " + when(s.next_at) : "按计划运行"}`); onSaved(s); onClose() }
    catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const tpl = TEMPLATES().filter((t) => t.show)
  return (
    <Modal title={edit ? "编辑定时任务" : "新建定时任务"} desc="定时任务是按时间触发的事件：建事项交给 Bot 做，或者在对话里发一条汇报。" onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!ok || busy} onClick={() => void submit()} data-testid="schedule-go">{edit ? "保存" : "创建"}</Button></>}>
      {!edit && <div className="flex flex-wrap gap-1.5" role="group" aria-label="模板">{tpl.map((t) => <Button key={t.n} size="xs" variant="outline" onClick={() => { setName(t.n); setW(t.when); setWhat(t.what); setMode(t.mode); if (t.bot) setB(t.bot); setCron("") }} data-tpl={t.n}>{t.n}</Button>)}</div>}
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="sch-name">名称</FieldLabel><Input id="sch-name" className="rounded-lg" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} placeholder="例如：周一整理本周计划" data-schedule-name /></Field>
        <Field><FieldLabel htmlFor="sch-when">什么时候</FieldLabel><Input id="sch-when" className="rounded-lg" maxLength={60} value={w} onChange={(e) => setW(e.target.value)} placeholder="例如：工作日早上九点半" aria-describedby="sch-when-desc" data-schedule-when disabled={!!cron.trim()} />
          <FieldDescription id="sch-when-desc" aria-live="polite" data-parse>{cron.trim() ? "按下面的 cron 运行" : perr ? <span className="text-destructive">{perr}</span> : parsed ? <span className="flex flex-col gap-0.5">
            <span><span className="text-foreground">{parsed.describe}</span> <span className="font-mono text-[12px]">（{parsed.cron}）</span>{parsed.by === "model" && " · Steve 理解的"}</span>
            <span>接下来：{parsed.next.map(when).join("、")}</span>
            {parsed.assumed && <span>{parsed.assumed}</span>}{parsed.notes?.map((x) => <span key={x}>{x}</span>)}</span> : "说法例如：每天 08:30、工作日 09:00、每周一和周四 10:00、每月 1 日 10:00、每隔 2 小时"}</FieldDescription></Field>
        <button type="button" className="-mt-2 self-start type-footnote text-muted-foreground hover:text-foreground" aria-expanded={adv} onClick={() => setAdv(!adv)}>{adv ? "收起高级设置" : "高级：直接写 cron、时区"}</button>
        {adv && <div className="grid grid-cols-2 gap-3">
          <Field><FieldLabel htmlFor="sch-cron">cron（5 段，留空用上面的说法）</FieldLabel><Input id="sch-cron" className="rounded-lg font-mono text-[12px]" value={cron} onChange={(e) => setCron(e.target.value)} placeholder={parsed?.cron ?? edit?.cron ?? "30 9 * * 1-5"} /></Field>
          <Field><FieldLabel>时区</FieldLabel><Pick label="时区" value={tz || "_"} options={[{ v: "_", l: "工作空间默认" }, ...TZS.map((v) => ({ v, l: v }))]} onChange={(v) => setTz(v === "_" ? "" : v)} className="w-full" /></Field>
        </div>}
        <Field><FieldLabel htmlFor="sch-what">做什么</FieldLabel><Textarea id="sch-what" maxLength={2000} className="min-h-16 rounded-lg" value={what} onChange={(e) => setWhat(e.target.value)} placeholder="例如：看日程和进行中的事项，列出本周最重要的三件事" data-schedule-what /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>交给</FieldLabel><Pick label="交给哪个 Bot" value={b} options={botOptions()} onChange={setB} testId="sch-bot" /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>方式</FieldLabel><Pick label="方式" value={mode} options={(Object.keys(MODE_LABEL) as ScheduleMode[]).map((v) => ({ v, l: MODE_LABEL[v] }))} onChange={setMode} testId="sch-mode" /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>错过了怎么办</FieldLabel><Pick label="错过了怎么办" value={missed} options={[{ v: "catch_up_once", l: "开机后补跑一次" }, { v: "skip", l: "跳过" }]} onChange={setMissed} /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>上一次还没结束</FieldLabel><Pick label="上一次还没结束" value={overlap} options={[{ v: "skip", l: "跳过这一次" }, { v: "run", l: "照常运行" }]} onChange={setOverlap} /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel htmlFor="sch-budget">每次预算（美元，0 = 不限）</FieldLabel><Input id="sch-budget" type="number" min={0} max={50} step={0.5} className="w-24 rounded-lg" value={budget} onChange={(e) => setBudget(e.target.value)} aria-invalid={!budOk} /></Field>
      </FieldGroup>
    </Modal>
  )
}
