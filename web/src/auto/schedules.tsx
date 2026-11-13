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
import { Meta, Empty, StatusDot, BotAvatar, go, Panel } from "@/ui"
import { st, botName, toast } from "@/store"
import { flowApi, errText, type Load, type Parsed, type Schedule, type ScheduleBody, type ScheduleMode, type ScheduleRun } from "@/api/flow"
import { HubError } from "@/api"
import { Ico, Modal, Pick, Failed, when, botOptions, mainBot, RUN_STATUS } from "@/auto/common"
import { t, useT } from "@/i18n"
import { num } from "@/i18n/format"

const MODES: ScheduleMode[] = ["item_auto", "item", "propose", "report"]
/** how a schedule runs, in the current language */
export const modeLabel = (m: ScheduleMode) => t(`auto:schedule.mode.${m}`)
const TRIGGERS = ["time", "manual", "catch_up"] as const
const triggerLabel = (x: string) => ((TRIGGERS as readonly string[]).includes(x) ? t(`auto:schedule.trigger.${x as (typeof TRIGGERS)[number]}`) : x)
const TZS = ["Asia/Taipei", "Asia/Shanghai", "Asia/Tokyo", "UTC", "Europe/London", "America/Los_Angeles", "America/New_York"]
const usd = (n: number) => (n ? t("auto:schedule.usd", { n: num(n, { minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: false }) }) : t("auto:schedule.unlimited"))

export function Schedules({ L, onNew }: { L: Load<Schedule[]>; onNew: () => void }) {
  const [open, setOpen] = useState<string | null>(null), [del, setDel] = useState<Schedule | null>(null), [edit, setEdit] = useState<Schedule | null>(null), [busy, setBusy] = useState<string | null>(null)
  const t = useT()
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  const act = async (key: string, f: () => Promise<unknown>, ok: string) => {
    setBusy(key)
    try { await f(); toast(ok); await L.reload() } catch (e) { toast(errText(e)) } finally { setBusy(null) }
  }
  return (<>
    <AlertDialog open={!!del} onOpenChange={(o) => !o && setDel(null)}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{t("auto:schedule.deleteTitle", { name: del?.name ?? "" })}</AlertDialogTitle><AlertDialogDescription>{t("auto:schedule.deleteDesc")}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => { const d = del!; setDel(null); setOpen(null); void act("del", () => flowApi.deleteSchedule(d.id), t("auto:schedule.deleted")) }} data-del-schedule-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
    {edit && <NewScheduleDialog edit={edit} onClose={() => setEdit(null)} onSaved={() => void L.reload()} />}
    {!L.data.length ? <Empty title={t("auto:schedule.emptyTitle")} sub={t("auto:schedule.emptySub")} action={<Button variant="outline" onClick={onNew}><PlusIcon />{t("auto:newSchedule")}</Button>} />
    : <Panel>{L.data.map((s) => (
      <Collapsible key={s.id} open={open === s.id} onOpenChange={(o) => setOpen(o ? s.id : null)}>
        <div className="flex items-center gap-3 rounded-lg px-3 py-3 transition-colors hover:bg-accent">
          <Ico I={ClockIcon} />
          <CollapsibleTrigger className="min-w-0 flex-1 text-left" data-schedule-row={s.id}>
            <div className="truncate type-body font-medium">{s.name}</div>
            <Meta className="mt-0.5"><span>{s.describe}</span><span>{s.enabled ? (s.next_at ? t("auto:schedule.next", { time: when(s.next_at) }) : t("auto:schedule.onPlan")) : t("auto:disabled")}</span>
              {s.last && <span className="inline-flex items-center gap-1">{s.last.status === "failed" ? <XIcon className="size-3 text-destructive" aria-label={t("auto:schedule.lastFailed")} /> : <CheckIcon className="size-3 text-success" aria-label={t("auto:schedule.lastOk")} />}{s.last.text || RUN_STATUS[s.last.status]?.[0]}</span>}</Meta>
          </CollapsibleTrigger>
          <BotAvatar k={s.bot} size={22} className="hidden sm:block" />
          <Button size="xs" variant="ghost" className="hidden text-muted-foreground sm:inline-flex" disabled={busy === "run" + s.id} onClick={() => void act("run" + s.id, () => flowApi.runSchedule(s.id), t("auto:schedule.started", { name: s.name }))} data-run-now-row={s.id}><PlayIcon />{t("auto:schedule.runNow")}</Button>
          <Switch checked={s.enabled} disabled={busy === "on" + s.id} onCheckedChange={(v) => void act("on" + s.id, () => flowApi.patchSchedule(s.id, { enabled: v }), v ? t("auto:enabled") : t("auto:schedule.disabledToast"))} aria-label={t("auto:enableName", { name: s.name })} />
        </div>
        <CollapsibleContent>
          {open === s.id && <ScheduleDetail s={s} busy={busy} onRun={() => void act("run" + s.id, () => flowApi.runSchedule(s.id), t("auto:schedule.started", { name: s.name }))} onEdit={() => setEdit(s)} onDelete={() => setDel(s)} />}
        </CollapsibleContent>
      </Collapsible>
    ))}</Panel>}
  </>)
}

function ScheduleDetail({ s, busy, onRun, onEdit, onDelete }: { s: Schedule; busy: string | null; onRun: () => void; onEdit: () => void; onDelete: () => void }) {
  const [runs, setRuns] = useState<ScheduleRun[] | null>(null), [more, setMore] = useState(true)
  const t = useT()
  const load = async (before?: number) => {
    try { const xs = await flowApi.scheduleRuns(s.id, before, 20); setRuns((r) => (before ? [...(r ?? []), ...xs] : xs)); setMore(xs.length === 20) } catch (e) { if (!(e instanceof HubError && e.status === 404)) toast(errText(e)); setRuns((r) => r ?? []) }
  }
  useEffect(() => { void load() }, [s.id, s.last?.at, busy])
  const rows: [string, ReactNode][] = [
    [t("auto:schedule.rowTime"), <>{s.describe} <span className="type-code text-muted-foreground">{s.cron}</span> <span className="text-muted-foreground">· {s.tz}</span></>],
    [t("auto:schedule.rowWhat"), <span className="whitespace-pre-wrap">{s.what}</span>],
    [t("auto:schedule.rowRun"), <span className="inline-flex items-center gap-1.5"><BotAvatar k={s.bot} size={16} />{botName(s.bot)} · {modeLabel(s.mode)}</span>],
    [t("auto:schedule.rowMissed"), t(`auto:schedule.missed.${s.missed}`)], [t("auto:schedule.rowOverlap"), t(`auto:schedule.overlap.${s.overlap}`)], [t("auto:schedule.rowBudget"), usd(s.budget_usd)],
  ]
  return (
    <div className="grid gap-4 px-3 pb-4 sm:grid-cols-2 sm:pl-14" data-schedule-detail={s.id}>
      <dl className="grid grid-cols-[72px_1fr] gap-x-3 gap-y-1.5 type-callout">
        {rows.map(([k, v]) => <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="min-w-0">{v}</dd></div>)}
      </dl>
      <div>
        <div className="mb-1.5 flex items-center gap-1.5 type-label text-tertiary"><HistoryIcon className="size-3.5" aria-hidden />{t("auto:schedule.runs")}</div>
        {!runs ? <div className="type-footnote text-muted-foreground">{t("auto:loading")}</div> : !runs.length ? <div className="type-footnote text-muted-foreground">{t("auto:noRuns")}</div> : <ul className="flex max-h-64 flex-col overflow-y-auto" data-schedule-runs>
          {runs.map((r) => {
            const go_ = r.item_id ? () => go("#/items/" + r.item_id) : undefined
            return (
              <li key={r.id}><button type="button" disabled={!go_} onClick={go_} className="flex w-full items-center gap-2 rounded-md px-1 py-1 text-left type-footnote enabled:hover:bg-accent" title={r.error ?? undefined} data-schedule-run={r.id}>
                <StatusDot s={RUN_STATUS[r.status]?.[1] ?? "idle"} /><span className="w-24 shrink-0 tabular-nums text-muted-foreground">{when(r.at)}</span>
                <span className="min-w-0 flex-1 truncate">{r.error ? <span className="text-destructive">{r.error}</span> : r.outcome_label || RUN_STATUS[r.status]?.[0]}</span>
                <span className="shrink-0 text-muted-foreground">{triggerLabel(r.trigger)}{r.cost_usd ? " · " + usd(r.cost_usd) : ""}</span>
                {go_ && <ChevronRightIcon className="size-3.5 shrink-0 text-tertiary" aria-hidden />}
              </button></li>)
          })}
          {more && <li><button type="button" className="px-1 py-1 type-footnote text-blue hover:underline" onClick={() => void load(runs[runs.length - 1]?.started)}>{t("auto:schedule.earlier")}</button></li>}
        </ul>}
      </div>
      <div className="flex flex-wrap gap-2 sm:col-span-2"><Button size="sm" variant="outline" disabled={busy === "run" + s.id} onClick={onRun} data-run-now><PlayIcon />{t("auto:schedule.runNow")}</Button><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={onEdit} data-edit-schedule>{t("common:action.edit")}</Button><span className="flex-1" /><Button size="sm" variant="ghost" className="text-destructive" onClick={onDelete} data-del-schedule>{t("common:action.delete")}</Button></div>
    </div>
  )
}

/* the prototype's four templates; they only fill the form. `when` stays in Chinese: it is the phrase the hub's
 * rule parser reads (anything else goes to the slower model fallback). */
const TEMPLATES = (): { n: string; when: string; what: string; mode: ScheduleMode; bot: string; show: boolean }[] => [
  { n: t("auto:schedule.tplReport"), when: "每天 08:30", what: t("auto:schedule.tplReportWhat"), mode: "report", bot: mainBot(), show: true },
  { n: t("auto:schedule.tplWeekly"), when: "每周五 09:00", what: t("auto:schedule.tplWeeklyWhat"), mode: "item_auto", bot: mainBot(), show: true },
  { n: t("auto:schedule.tplCleanup"), when: "每天 23:00", what: t("auto:schedule.tplCleanupWhat"), mode: "item", bot: Object.keys(st.bots).find((k) => /运维|ops/i.test(st.bots[k].n + k)) ?? mainBot(), show: true },
  { n: t("auto:schedule.tplBills"), when: "每月 1 日 10:00", what: t("auto:schedule.tplBillsWhat"), mode: "report", bot: Object.keys(st.bots).find((k) => /邮件|mail/i.test(st.bots[k].n + k)) ?? "", show: Object.keys(st.bots).some((k) => /邮件|mail/i.test(st.bots[k].n + k)) },
]

/* a new schedule's budget follows its mode until you type one: an agent run costs dollars, a report cents */
export const defaultBudget = (m: ScheduleMode) => (m === "item" || m === "item_auto" ? 20 : 1)

export function NewScheduleDialog({ edit, bot, onClose, onSaved }: { edit?: Schedule; bot?: string; onClose: () => void; onSaved: (s: Schedule) => void }) {
  const [name, setName] = useState(edit?.name ?? ""), [w, setW] = useState(edit?.when ?? "每周一 09:00"), [what, setWhat] = useState(edit?.what ?? "")
  const [b, setB] = useState(edit?.bot ?? bot ?? mainBot()), [mode, setMode] = useState<ScheduleMode>(edit?.mode ?? "item_auto")
  const [missed, setMissed] = useState<Schedule["missed"]>(edit?.missed ?? "catch_up_once"), [overlap, setOverlap] = useState<Schedule["overlap"]>(edit?.overlap ?? "skip")
  const [budget, setBudget] = useState(String(edit?.budget_usd ?? defaultBudget(edit?.mode ?? "item_auto"))), [budTouched, setBudTouched] = useState(false), [adv, setAdv] = useState(false)
  const pickMode = (m: ScheduleMode) => { setMode(m); if (!edit && !budTouched) setBudget(String(defaultBudget(m))) }
  const [cron, setCron] = useState(""), [tz, setTz] = useState(edit?.tz ?? st.settings.tz ?? "")
  const [parsed, setParsed] = useState<Parsed | null>(null), [perr, setPerr] = useState(""), [busy, setBusy] = useState(false)
  const t = useT()
  /* live parse, 300 ms after you stop typing */
  useEffect(() => {
    if (cron.trim() || !w.trim()) { setParsed(null); setPerr(""); return }
    const timer = setTimeout(() => { flowApi.parseWhen(w.trim(), tz || undefined).then((p) => { setParsed(p); setPerr("") }, (e) => { setParsed(null); setPerr(e instanceof HubError ? e.message : errText(e)) }) }, 300)
    return () => clearTimeout(timer)
  }, [w, tz, cron])
  const bud = Number(budget), budOk = budget.trim() !== "" && bud >= 0 && bud <= 50
  const ok = name.trim() && name.trim().length <= 40 && (cron.trim() || (w.trim() && !perr)) && what.trim() && b && budOk
  const submit = async () => {
    const body: ScheduleBody = { name: name.trim(), what: what.trim(), bot: b, mode, missed, overlap, budget_usd: bud, ...(tz ? { tz } : {}) }
    if (cron.trim()) body.cron = cron.trim()
    if (!edit || w.trim() !== edit.when || cron.trim()) { if (w.trim()) body.when = w.trim() }
    setBusy(true)
    try { const s = edit ? await flowApi.patchSchedule(edit.id, body) : await flowApi.newSchedule(body); toast(edit ? t("common:state.saved") : s.next_at ? t("auto:schedule.createdNext", { time: when(s.next_at) }) : t("auto:schedule.createdPlan")); onSaved(s); onClose() }
    catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const tpl = TEMPLATES().filter((x) => x.show)
  return (
    <Modal title={edit ? t("auto:schedule.editTitle") : t("auto:newSchedule")} desc={t("auto:schedule.desc")} onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!ok || busy} onClick={() => void submit()} data-testid="schedule-go">{edit ? t("common:action.save") : t("auto:schedule.create")}</Button></>}>
      {!edit && <div className="flex flex-wrap gap-1.5" role="group" aria-label={t("auto:schedule.templates")}>{tpl.map((x) => <Button key={x.n} size="xs" variant="outline" onClick={() => { setName(x.n); setW(x.when); setWhat(x.what); pickMode(x.mode); if (x.bot) setB(x.bot); setCron("") }} data-tpl={x.n}>{x.n}</Button>)}</div>}
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="sch-name">{t("auto:schedule.name")}</FieldLabel><Input id="sch-name" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} placeholder={t("auto:schedule.namePlaceholder")} data-schedule-name /></Field>
        <Field><FieldLabel htmlFor="sch-when">{t("auto:schedule.when")}</FieldLabel><Input id="sch-when" maxLength={60} value={w} onChange={(e) => setW(e.target.value)} placeholder={t("auto:schedule.whenPlaceholder")} aria-describedby="sch-when-desc" data-schedule-when disabled={!!cron.trim()} />
          <FieldDescription id="sch-when-desc" aria-live="polite" data-parse>{cron.trim() ? t("auto:schedule.byCron") : perr ? <span className="text-destructive">{perr}</span> : parsed ? <span className="flex flex-col gap-0.5">
            <span><span className="text-foreground">{parsed.describe}</span> <span className="font-mono">{t("auto:schedule.cronParen", { cron: parsed.cron })}</span>{parsed.by === "model" && " · " + t("auto:schedule.byModel")}</span>
            <span>{t("auto:schedule.upcoming", { times: parsed.next.map(when).join(t("auto:sep.list")) })}</span>
            {parsed.assumed && <span>{parsed.assumed}</span>}{parsed.notes?.map((x) => <span key={x}>{x}</span>)}</span> : t("auto:schedule.whenHint")}</FieldDescription></Field>
        <button type="button" className="-mt-2 self-start type-footnote text-muted-foreground hover:text-foreground" aria-expanded={adv} onClick={() => setAdv(!adv)}>{adv ? t("auto:schedule.advHide") : t("auto:schedule.advShow")}</button>
        {adv && <div className="grid grid-cols-2 gap-3 anim-in">
          <Field><FieldLabel htmlFor="sch-cron">{t("auto:schedule.cron")}</FieldLabel><Input id="sch-cron" className="type-code" value={cron} onChange={(e) => setCron(e.target.value)} placeholder={parsed?.cron ?? edit?.cron ?? "30 9 * * 1-5"} /></Field>
          <Field><FieldLabel>{t("auto:schedule.tz")}</FieldLabel><Pick label={t("auto:schedule.tz")} value={tz || "_"} options={[{ v: "_", l: t("auto:schedule.tzDefault") }, ...TZS.map((v) => ({ v, l: v }))]} onChange={(v) => setTz(v === "_" ? "" : v)} className="w-full" /></Field>
        </div>}
        <Field><FieldLabel htmlFor="sch-what">{t("auto:schedule.what")}</FieldLabel><Textarea id="sch-what" maxLength={2000} className="min-h-16" value={what} onChange={(e) => setWhat(e.target.value)} placeholder={t("auto:schedule.whatPlaceholder")} data-schedule-what /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("auto:schedule.handTo")}</FieldLabel><Pick label={t("auto:schedule.handToAria")} value={b} options={botOptions()} onChange={setB} testId="sch-bot" /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("auto:schedule.modeLabel")}</FieldLabel><Pick label={t("auto:schedule.modeLabel")} value={mode} options={MODES.map((v) => ({ v, l: modeLabel(v) }))} onChange={pickMode} testId="sch-mode" /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("auto:schedule.missedLabel")}</FieldLabel><Pick label={t("auto:schedule.missedLabel")} value={missed} options={[{ v: "catch_up_once", l: t("auto:schedule.missedCatchUp") }, { v: "skip", l: t("auto:schedule.missed.skip") }]} onChange={setMissed} /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("auto:schedule.overlapLabel")}</FieldLabel><Pick label={t("auto:schedule.overlapLabel")} value={overlap} options={[{ v: "skip", l: t("auto:schedule.overlap.skip") }, { v: "run", l: t("auto:schedule.overlap.run") }]} onChange={setOverlap} /></Field>
        <Field orientation="horizontal" className="justify-between"><FieldLabel htmlFor="sch-budget">{t("auto:schedule.budget")}</FieldLabel><Input id="sch-budget" type="number" min={0} max={50} step={0.5} className="w-24 tabular-nums" value={budget} onChange={(e) => { setBudget(e.target.value); setBudTouched(true) }} aria-invalid={!budOk} /></Field>
      </FieldGroup>
    </Modal>
  )
}
