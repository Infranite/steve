/* 设置 · 模型：Steve 是一个 Bot。它自己的判断（分流、对话回答、研究、起标题……）默认都用 Steve 这个 Bot 的 harness、模型和
 * 思考强度（在 Bot 页改）。页面只铺开「默认配置」和少数单独配置的用途；跟随默认的收起，编辑在侧栏里做，备用模型放进高级选项。
 * 最近 7 天实际调用了什么在「运行记录」里看，跟当前配置分开。最下面是新会话在每个 harness 上的默认模型。 */
import { ago as fmtAgo } from "@/i18n/format"
import { t as tr, useT } from "@/i18n"
import { Trans } from "react-i18next"
import { useEffect, useRef, useState, type ReactNode, type Ref } from "react"
import { cn } from "cn"
import { ArrowDownIcon, ArrowUpIcon, CpuIcon, PlusIcon, TriangleAlertIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { modReq, isNotWired } from "@/api"
import { st, toast, useStore } from "@/store"
import { Row, Segmented } from "@/ui"
import { Panel, Section, Card, Blank, Notice, Pill, Chevron, Loading, Reveal, Turn } from "@/settings-kit"
import { Select } from "@/view-shared"
import { Robot } from "@/robot"
import { ModelPicker, harnessName } from "@/model-picker"
import { editBot } from "@/manage"

/* the label and description are read when the settings list renders, so they follow the UI language */
export const MODELS_CAT = Object.defineProperties(["models", "", CpuIcon, ""], {
  1: { get: () => tr("pages:models.cat.title"), enumerable: true },
  3: { get: () => tr("pages:models.cat.desc"), enumerable: true },
}) as [string, string, any, string]

type Choice = { harness: string; models: string[]; effort?: string | null }
type UsedRow = { harness: string; model: string; calls: number; failed: number; at: number }
type Recent = { last: { harness: string; model: string; at: number } | null; used: UsedRow[] }
type Purpose = { key: string; label: string; desc: string; value: Choice | null; follows: boolean; effective: Choice; recommended: Choice | null; recent: Recent | null }
type Steve = { bot_id: string | null; name: string; harness: string; model: string | null; bot_effort: string | null; runs_on: string; models: string[]; effort: string | null; note: string | null; model_note: string | null }
type Harness = { id: string; name: string; efforts: { key: string; label: string }[]; default_models: string[] }
type Agent = { harness: string; name: string; models: string[]; default: string | null; value: string | null }
type View = { steve: Steve; purposes: Purpose[]; agents: Agent[]; harnesses: Harness[]; max: number }

const api = {
  get: () => modReq<View>("GET", "/settings/models"),
  patch: (b: { purposes?: Record<string, Choice | null>; agents?: Record<string, string | null> }) => modReq<View>("PATCH", "/settings/models", b),
}
/** the name of a model without its provider path or family prefix: `claude/claude-haiku-5-5` → `haiku-5-5` */
const short = (m: string) => m.replace(/^.*\//, "").replace(/^claude-/, "")
const EFFORTS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"] as const
const effortName = (e?: string | null) => !e ? tr("pages:models.followModel") : (EFFORTS as readonly string[]).includes(e) ? tr(`pages:models.effort.${e as (typeof EFFORTS)[number]}`) : e
const defaultModel = () => tr("pages:models.defaultModel")
const listOf = (xs: string[]) => xs.join(tr("pages:listSep"))
/** `Claude Code · claude-haiku-5-5 → claude-sonnet-5-5 · 思考高` */
export function ChoiceLine({ c, className, wrap }: { c: Choice; className?: string; wrap?: boolean }) {
  return (
    <span className={cn("block min-w-0", wrap ? "break-words" : "truncate", className)} data-choice>
      <span className="font-medium text-foreground">{harnessName(c.harness)}</span>
      <span className="text-muted-foreground"> · </span>
      <span className="type-code text-foreground/85">{c.models.length ? c.models.join(" → ") : defaultModel()}</span>
      {c.effort && <span className="text-muted-foreground"> · {tr("pages:models.thinking", { effort: effortName(c.effort) })}</span>}
    </span>
  )
}
/** one line for a list row: `Claude Code · haiku-5-5 · 备用 1 个 · 思考高` (the fallbacks are an advanced detail) */
function Brief({ c }: { c: Choice }) {
  return (
    <span className="block min-w-0 truncate" data-brief title={`${harnessName(c.harness)} · ${c.models.join(" → ") || defaultModel()}`}>
      {harnessName(c.harness)} · <span className="type-code">{c.models[0] ? short(c.models[0]) : defaultModel()}</span>
      {c.models.length > 1 && ` · ${tr("pages:models.backups", { count: c.models.length - 1 })}`}{c.effort && ` · ${tr("pages:models.thinking", { effort: effortName(c.effort) })}`}
    </span>
  )
}
const same = (a: Choice | null, b: Choice | null) => !!a && !!b && a.harness === b.harness && a.models.join() === b.models.join() && (a.effort ?? null) === (b.effort ?? null)
const ago = (t: number) => fmtAgo(t, { justNow: false })
const failedOf = (p: Purpose) => p.recent?.used.reduce((n, u) => n + u.failed, 0) ?? 0
const callsOf = (p: Purpose) => p.recent?.used.reduce((n, u) => n + u.calls, 0) ?? 0

export function ModelsSettingsPane() {
  useStore()
  const t = useT()
  const [v, setV] = useState<View | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [more, setMore] = useState(() => location.hash.includes("advanced"))
  const [log, setLog] = useState(false)
  const [edit, setEdit] = useState<{ key: string; own: boolean } | null>(null)
  const logRef = useRef<HTMLElement>(null)
  /* a change made elsewhere (the Bot page, a Steve draft, another tab) arrives as a settings or bot event */
  const b = st.bots.steve
  const stamp = JSON.stringify([Object.entries(st.settings ?? {}).filter(([k]) => k.startsWith("models.")), b?.harness, b?.model, b?.reasoningEffort])
  useEffect(() => { api.get().then((x) => { setV(x); setErr(null) }, (e) => setErr(isNotWired(e) ? "absent" : e instanceof Error ? e.message : String(e))) }, [stamp])
  if (err === "absent") return <Blank data-models-unwired>{t("pages:models.unwired")}</Blank>
  if (err) return <Notice tone="destructive" role="alert">{err}</Notice>
  if (!v) return <Loading />
  const save = async (body: Parameters<typeof api.patch>[0], ok: string) => {
    try { setV(await api.patch(body)); toast(ok); return true } catch (e) { toast(e instanceof Error ? e.message : String(e)); return false }
  }
  const s = v.steve
  const own = v.purposes.filter((p) => !p.follows)
  const rest = v.purposes.filter((p) => p.follows)
  const steveChoice: Choice = { harness: s.runs_on, models: s.models, effort: s.effort }
  const failing = v.purposes.filter((p) => failedOf(p) > 0)
  const failed = failing.reduce((n, p) => n + failedOf(p), 0)
  const editing = edit && v.purposes.find((p) => p.key === edit.key)
  const showLog = () => { setLog(true); requestAnimationFrame(() => logRef.current?.scrollIntoView({ behavior: "smooth", block: "start" })) }
  return (
    <div className="flex flex-col gap-8" data-models-settings>
      <Section data-models-default label={t("pages:models.defaultSection")} note={t("pages:models.defaultUses", { name: s.name })}>
        <Card className="flex flex-col gap-3">
          <div className="flex items-center gap-3.5">
            <Robot k={s.bot_id ?? "steve"} size={40} className="shrink-0" />
            <div className="min-w-0 flex-1">
              <ChoiceLine c={{ ...steveChoice, effort: null }} wrap className="type-body" />
              <p className="mt-0.5 type-footnote text-muted-foreground" data-steve-effort>{t("pages:models.effortLine", { effort: effortName(s.effort) })}{s.model_note && ` · ${s.model_note}`}{s.models.length > 1 && ` · ${t("pages:models.fallbackHint")}`}</p>
            </div>
            <Button size="sm" variant="outline" className="shrink-0" onClick={() => editBot(s.bot_id ?? "steve")} data-edit-steve>{t("pages:models.change")}</Button>
          </div>
          {s.note && <Notice tone="attention" role="note" className="py-2.5">{s.note}</Notice>}
        </Card>
      </Section>

      <Section data-models-overrides label={t("pages:models.overrides")} right={rest.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-add-override />}><PlusIcon />{t("pages:models.addOverride")}</DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-40">{rest.map((p) => <DropdownMenuItem key={p.key} onClick={() => setEdit({ key: p.key, own: true })} data-add-purpose={p.key}>{p.label}</DropdownMenuItem>)}</DropdownMenuContent>
          </DropdownMenu>
        )}>
        {own.length ? <Panel>{own.map((p) => (
          <Row key={p.key} data-purpose={p.key} data-custom title={p.label} sub={<Brief c={p.effective} />} onClick={() => setEdit({ key: p.key, own: true })} aria-label={t("pages:models.editAria", { label: p.label })}>
            <Chevron />
          </Row>
        ))}</Panel> : <Blank data-no-overrides>{t("pages:models.noOverrides")}</Blank>}
        {rest.length > 0 && <div className="mt-2" data-models-follows={more ? "open" : "closed"}>
          <button type="button" className="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left type-callout text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => setMore(!more)} aria-expanded={more} data-follows-toggle>
            <Turn open={more} />
            <span className="min-w-0 flex-1">{t("pages:models.restFollow", { count: rest.length })}</span>
          </button>
          <Reveal open={more}><Panel className="mb-1 mt-1">{rest.map((p) => (
            <Row key={p.key} data-purpose={p.key} data-follows title={p.label} sub={p.desc} onClick={() => setEdit({ key: p.key, own: false })} aria-label={t("pages:models.configureAria", { label: p.label })}>
              <span className="type-footnote text-muted-foreground">{t("pages:models.followDefault")}</span><Chevron />
            </Row>
          ))}</Panel></Reveal>
        </div>}
        {failed > 0 && <button type="button" className="mt-3 flex w-full items-start gap-2.5 rounded-xl bg-attention/10 px-4 py-3 text-left type-callout text-foreground/90 hover:bg-attention/14" onClick={showLog} data-models-failures>
          <TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" />
          <span className="min-w-0 flex-1">{t("pages:models.failures", { count: failed, list: listOf(failing.map((p) => t("pages:models.failureItem", { label: p.label, count: failedOf(p) }))) })}</span>
          <span className="shrink-0 whitespace-nowrap font-medium text-attention underline-offset-2 hover:underline">{t("pages:models.viewLog")}</span>
        </button>}
      </Section>

      <Section label={t("pages:models.agentsSection")} note={t("pages:models.agentsHint")}>
        <Panel>{v.agents.map((a) => (
          <div key={a.harness} data-agent={a.harness}>
            <Row title={a.name} sub={a.value ? t("pages:models.agentChanged") : t("pages:models.agentFollowsHub")}>
              <div className="w-[min(240px,52vw)]"><ModelPicker harness={a.harness} value={a.value ?? ""} allowDefault defaultLabel={a.default ? t("pages:models.defaultWith", { model: a.default }) : undefined} testId={`agent-${a.harness}`}
                onChange={(m) => void save({ agents: { [a.harness]: m || null } }, m ? t("pages:models.agentSet", { name: a.name, model: m }) : t("pages:models.agentReset", { name: a.name }))} /></div>
            </Row>
          </div>
        ))}</Panel>
      </Section>

      <RunLog v={v} open={log} setOpen={setLog} ref={logRef} />

      <Sheet open={!!editing} onOpenChange={(o) => { if (!o) setEdit(null) }}>
        <SheetContent side="right" className="w-[92vw] max-w-[420px]! gap-0 overflow-y-auto" data-models-sheet={editing?.key}>
          {editing && <Editor key={editing.key} p={editing} v={v} steve={steveChoice} startOwn={edit!.own}
            onSave={async (c, ok) => { if (await save({ purposes: { [editing.key]: c } }, ok)) setEdit(null) }} onCancel={() => setEdit(null)} />}
        </SheetContent>
      </Sheet>
    </div>
  )
}

/** 运行记录: what each purpose actually ran on in the past 7 days, kept apart from the configuration.
 *  A call on a harness or model the purpose no longer uses is marked, so history is not read as config. */
function RunLog({ v, open, setOpen, ref }: { v: View; open: boolean; setOpen: (o: boolean) => void; ref: Ref<HTMLElement> }) {
  const t = useT()
  const used = v.purposes.filter((p) => p.recent?.used.length)
  const idle = v.purposes.filter((p) => !p.recent?.used.length)
  const calls = v.purposes.reduce((n, p) => n + callsOf(p), 0)
  const failed = v.purposes.reduce((n, p) => n + failedOf(p), 0)
  const current = (p: Purpose, u: UsedRow) => u.harness === p.effective.harness && (!p.effective.models.length || p.effective.models.some((m) => short(m) === short(u.model)))
  return (
    <section ref={ref} className="scroll-mt-4" data-models-log={open ? "open" : "closed"}>
      <button type="button" className="mb-2 flex w-full items-center justify-between gap-2 rounded-lg px-1 py-1 text-left" onClick={() => setOpen(!open)} aria-expanded={open} data-log-toggle>
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="type-label text-tertiary">{t("pages:models.log.title")}</span>
          <span className="type-footnote text-muted-foreground" data-log-summary>{t("pages:models.log.summary", { count: calls })}{failed > 0 && <span className="text-attention"> · {t("pages:models.log.failedTotal", { count: failed })}</span>}</span>
        </span>
        <Turn open={open} flip />
      </button>
      <Reveal open={open}><div className="pb-1">
        <p className="mb-3 px-1 type-footnote text-muted-foreground"><Trans t={t} i18nKey="pages:models.log.note" components={{ a: <a href="#/stats" className="ml-0.5 whitespace-nowrap text-blue hover:underline" /> }} /></p>
        {used.length ? <Panel>{used.map((p) => (
          <div key={p.key} className="px-3 py-2.5" data-log-purpose={p.key}>
            <div className="flex items-baseline justify-between gap-3">
              <span className="type-body">{p.label}</span>
              <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{p.recent?.last && ago(p.recent.last.at)}</span>
            </div>
            <ul className="mt-1 flex flex-col gap-0.5">{p.recent!.used.map((u, i) => (
              <li key={i} className="flex min-w-0 flex-wrap items-baseline gap-x-2 type-footnote text-muted-foreground" data-log-used={u.model}>
                <span className="min-w-0 break-words">{harnessName(u.harness)} · <span className="font-mono">{short(u.model)}</span> <span className="tabular-nums">×{u.calls}</span></span>
                {u.failed > 0 && <span className="text-attention">{t("pages:models.log.failed", { count: u.failed })}</span>}
                {!current(p, u) && <Pill data-log-stale>{t("pages:models.log.stale")}</Pill>}
              </li>
            ))}</ul>
          </div>
        ))}</Panel> : <Blank>{t("pages:models.log.none")}</Blank>}
        {used.length > 0 && idle.length > 0 && <p className="mt-2 px-1 type-footnote text-muted-foreground" data-log-idle>{t("pages:models.log.idle", { list: listOf(idle.map((p) => p.label)) })}</p>}
      </div></Reveal>
    </section>
  )
}

/** The side-panel editor of one purpose: follow the default, or its own harness, model and effort; the
 *  fallback models and the recommended choice sit under 高级选项. */
function Editor({ p, v, steve, startOwn, onSave, onCancel }: { p: Purpose; v: View; steve: Choice; startOwn: boolean; onSave: (c: Choice | null, ok: string) => Promise<void>; onCancel: () => void }) {
  const t = useT()
  const [mode, setMode] = useState<"follow" | "own">(!p.follows || startOwn ? "own" : "follow")
  const [harness, setHarness] = useState(p.value?.harness ?? steve.harness)
  const [list, setList] = useState<string[]>(p.value?.models ?? [])
  const [effort, setEffort] = useState<string>(p.value?.effort ?? "")
  const [adv, setAdv] = useState(false)
  const [busy, setBusy] = useState(false)
  const h = v.harnesses.find((x) => x.id === harness)
  const onSteve = harness === steve.harness
  const switchHarness = (x: string) => { setHarness(x); setEffort(""); setList([]) }
  const primary = list[0] ?? ""
  const backups = list.slice(1)
  const setPrimary = (m: string) => setList(m ? [m, ...backups.filter((x) => x !== m)] : [])
  const move = (i: number, d: number) => { const n = [...backups]; const [x] = n.splice(i, 1); n.splice(i + d, 0, x); setList([primary, ...n]) }
  const next: Choice | null = mode === "follow" ? null : { harness, models: list, effort: effort || null }
  const dirty = mode === "follow" ? !p.follows : p.follows || !same(next, p.value)
  const run = async (c: Choice | null, ok: string) => { setBusy(true); await onSave(c, ok); setBusy(false) }
  const inherited = onSteve ? t("pages:models.editor.inheritSteve", { model: steve.models[0] ?? defaultModel() }) : t("pages:models.editor.inheritHarness", { harness: harnessName(harness), model: h?.default_models[0] ?? t("pages:models.editor.itsOwn") })
  return (
    <div className="flex min-h-full flex-col" data-models-editor={p.key}>
      <SheetHeader className="gap-1 px-5 pb-4 pt-5 pr-14">
        <SheetTitle className="type-title">{p.label}</SheetTitle>
        <SheetDescription className="type-callout">{p.desc}</SheetDescription>
      </SheetHeader>
      <div className="flex flex-col gap-5 px-5">
        <Segmented size="sm" value={mode} onChange={setMode} options={[["follow", t("pages:models.followDefault")], ["own", t("pages:models.overrides")]] as const} />
        {mode === "follow" ? (
          <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout anim-in" data-follow-preview>
            <span className="block type-footnote text-muted-foreground">{t("pages:models.editor.followPreview")}</span>
            <ChoiceLine c={steve} wrap className="mt-0.5" />
          </div>
        ) : <>
          <Field id="Harness" label="Harness"><Select className="w-full" value={harness} testId={`harness-${p.key}`} onChange={switchHarness} options={v.harnesses.map((x) => ({ v: x.id, l: x.name }))} /></Field>
          <Field id="model" label={t("pages:models.editor.model")}><ModelPicker harness={harness} value={primary} allowDefault defaultLabel={inherited} testId={`model-${p.key}`} onChange={setPrimary} /></Field>
          <Field id="effort" label={t("pages:models.editor.effort")}><Select className="w-full" value={effort || "default"} testId={`effort-${p.key}`} onChange={(x) => setEffort(x === "default" ? "" : x)}
            options={[{ v: "default", l: onSteve && steve.effort ? t("pages:models.editor.followDefaultEffort", { effort: effortName(steve.effort) }) : t("pages:models.followModel") }, ...(h?.efforts ?? []).map((e) => ({ v: e.key, l: e.label }))]} /></Field>
          <div className="border-t pt-3 anim-in" data-models-adv={adv ? "open" : "closed"}>
            <button type="button" className="flex w-full items-center gap-1.5 py-1 text-left type-callout text-muted-foreground hover:text-foreground" onClick={() => setAdv(!adv)} aria-expanded={adv} data-adv-toggle>
              <Turn open={adv} className="text-current" />
              <span className="min-w-0 flex-1">{t("pages:models.editor.advanced")}</span>
              {!adv && backups.length > 0 && <span className="type-footnote">{t("pages:models.backups", { count: backups.length })}</span>}
            </button>
            <Reveal open={adv}><div className="flex flex-col gap-4 pb-1 pt-2">
              <div>
                <div className="mb-1.5 type-footnote text-muted-foreground">{t("pages:models.editor.backupsHint", { count: v.max - 1 })}</div>
                {!primary ? <div className="rounded-lg bg-muted/60 px-2.5 py-1.5 type-footnote text-muted-foreground">{t("pages:models.editor.pickFirst")}</div> : <>
                  {backups.length > 0 && <ol className="flex flex-col gap-1">{backups.map((m, i) => (
                    <li key={m} className="flex items-center gap-2 rounded-lg bg-muted/60 py-1 pl-2.5 pr-1" data-model-item={m}>
                      <span className="w-4 shrink-0 text-right type-footnote tabular-nums text-muted-foreground">{i + 1}</span>
                      <span className="min-w-0 flex-1 truncate type-code" title={m}>{m}</span>
                      <Button size="icon-xs" variant="ghost" aria-label={t("pages:models.editor.moveUp", { model: m })} disabled={i === 0} onClick={() => move(i, -1)}><ArrowUpIcon /></Button>
                      <Button size="icon-xs" variant="ghost" aria-label={t("pages:models.editor.moveDown", { model: m })} disabled={i === backups.length - 1} onClick={() => move(i, 1)}><ArrowDownIcon /></Button>
                      <Button size="icon-xs" variant="ghost" aria-label={t("pages:models.editor.remove", { model: m })} onClick={() => setList(list.filter((x) => x !== m))}><XIcon /></Button>
                    </li>))}
                  </ol>}
                  {list.length < v.max && <div className={cn(backups.length > 0 && "mt-2")}><ModelPicker harness={harness} value="" placeholder={t("pages:models.editor.addBackup")} exclude={list} testId={`add-${p.key}`} onChange={(m) => m && !list.includes(m) && setList([...list, m])} /></div>}
                </>}
              </div>
              {p.recommended && <div className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground" data-models-recommended-line>
                <span className="min-w-0 flex-1">{t("pages:models.editor.recommended")}<ChoiceLine c={p.recommended} wrap className="inline" /></span>
                {!same(next, p.recommended) && <Button size="xs" variant="outline" onClick={() => { setHarness(p.recommended!.harness); setList(p.recommended!.models); setEffort(p.recommended!.effort ?? "") }} data-models-recommended>{t("pages:models.editor.fillRecommended")}</Button>}
              </div>}
            </div></Reveal>
          </div>
        </>}
      </div>
      <SheetFooter className="mt-auto flex-row justify-end gap-2 border-t px-5 py-3">
        <Button size="sm" variant="ghost" onClick={onCancel}>{t("common:action.cancel")}</Button>
        <Button size="sm" disabled={busy || !dirty || (mode === "own" && !harness)} onClick={() => void run(next, next ? t("pages:models.editor.saved", { label: p.label }) : t("pages:models.editor.reverted", { label: p.label }))} data-models-save>{busy && <Spinner />}{t("common:action.save")}</Button>
      </SheetFooter>
    </div>
  )
}
/** `id`: a stable name for the field, whatever language the label is in */
const Field = ({ id, label, children }: { id: string; label: string; children: ReactNode }) => (
  <div className="flex flex-col gap-1.5" data-field={id}><span className="type-label text-muted-foreground">{label}</span>{children}</div>
)

/** On Steve's Bot page: the judgments run on this bot; what they use now, and which purposes are set apart. */
export function SteveJudges({ harness, model }: { harness: string; model: string }) {
  const t = useT()
  const [v, setV] = useState<View | null>(null)
  const b = st.bots.steve
  useEffect(() => { api.get().then(setV, () => setV(null)) }, [b?.harness, b?.model, b?.reasoningEffort])
  const changed = !!b && (harness !== b.harness || (model || null) !== (b.model ?? null))
  const own = v?.purposes.filter((p) => !p.follows) ?? []
  return (
    <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout" data-steve-judges>
      <p className="text-foreground">{t("pages:models.judges.intro")}</p>
      {v && <div className="mt-1.5 text-muted-foreground">
        {changed ? <Trans t={t} i18nKey="pages:models.judges.afterSave" values={{ harness: harnessName(harness), model: model || defaultModel() }} components={{ h: <span className="font-medium text-foreground" />, m: <span className="type-code text-foreground/85" /> }} />
          : <>{t("pages:models.judges.now")}<ChoiceLine c={{ harness: v.steve.runs_on, models: v.steve.models, effort: v.steve.effort }} className="inline" />{v.steve.model_note && t("pages:models.judges.noteParen", { note: v.steve.model_note })}</>}
      </div>}
      {v && <div className="mt-1.5 type-footnote text-muted-foreground">
        {own.length ? t("pages:models.judges.someOwn", { list: listOf(own.map((p) => p.label)) }) : t("pages:models.judges.allFollow")}
        <a href="#/settings/models" className="ml-1 text-blue hover:underline" data-goto-advanced>{t("pages:models.judges.link")}</a>
      </div>}
    </div>
  )
}
