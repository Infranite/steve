/* #/auto/rule/:id — one flow: canvas and code views, inspector, dry run, Steve edits, versions, runs (prototype RuleView) */
import { useEffect, useMemo, useState } from "react"
import { CopyIcon, MoreHorizontalIcon, PencilIcon, PlayIcon, SparklesIcon, TrashIcon, FlaskConicalIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { Input } from "@/components/ui/input"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Page, Group, Row, SectionLabel, Segmented, StatusDot, Meta, Empty, Composer, go } from "@/ui"
import { tm, toast } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, type Flow, type Graph, type Run } from "@/api/flow"
import { HubError } from "@/api"
import { traceOf } from "@/auto/trace"
import { lazyView } from "@/lazy"
const FlowCanvas = lazyView(() => import("@/auto/canvas").then((m) => ({ default: m.FlowCanvas })))
import { Inspector, toYaml, kindOf } from "@/auto/nodes"
import { Loading, Failed, Modal, Json, ms, RUN_STATUS } from "@/auto/common"
import { t as tr, useT } from "@/i18n"

const back = (): [string, string] => ["#/auto/rules", tr("auto:title")]
const runParam = () => new URLSearchParams(location.hash.split("?")[1] ?? "").get("run")
const authorLabel = (a: string) => (a === "steve" ? "Steve" : a.startsWith("plugin:") ? tr("auto:rule.authorPlugin") : tr("auto:rule.authorYou"))

type Trace = { path: string[]; ran: Record<string, any>; run: Run; label: string }

export function RuleView({ id }: { id: string }) {
  const L = useLoad(() => flowApi.flow(id), [id], ["flow"])
  const flows = useLoad(() => flowApi.flows("flow"), [])
  const sources = useLoad(() => flowApi.sources().catch(() => []), [])
  const runs = useLoad(() => flowApi.flowRuns(id, 10), [id], ["run"])
  const [work, setWork] = useState<Graph | null>(null), [dirty, setDirty] = useState(false), [rev, setRev] = useState(0)
  const [sel, setSel] = useState<string | null>("t"), [fresh, setFresh] = useState<string | null>(null)
  const [trace, setTrace] = useState<Trace | null>(null)
  const [undo, setUndo] = useState<{ g: Graph; note: string } | null>(null)
  const [view, setView] = useState<"canvas" | "code">("canvas")
  const [busy, setBusy] = useState<string | null>(null)
  const [confirmDel, setConfirmDel] = useState(false), [rename, setRename] = useState(false), [sampleOpen, setSampleOpen] = useState(false)
  const [sample, setSample] = useState("")
  const f = L.data
  const t = useT(), BACK = back()

  /* the saved graph replaces the working copy unless you are editing it */
  useEffect(() => { if (f && !dirty) { setWork(f.graph); setRev((x) => x + 1); if (!f.graph.nodes.some((n) => n.id === sel)) setSel(f.graph.nodes[0]?.id ?? null) } }, [f])
  /* #/auto/rule/<id>?run=<run>: show that run's path */
  useEffect(() => { const r = runParam(); if (r) void openRun(r) }, [id])

  if (L.unwired) return <Unwired id="auto" />
  if (L.error && !f) return L.error instanceof HubError && L.error.status === 404
    ? <Page title={t("auto:rule.page")} back={BACK}><Empty title={t("auto:rule.notFound")} sub={t("auto:rule.notFoundSub")} /></Page>
    : <Failed title={t("auto:rule.page")} back={BACK} msg={errText(L.error)} retry={() => void L.reload()} />
  if (!f || !work) return <Loading title={t("auto:rule.page")} back={BACK} />
  if (f.kind === "schedule") { setTimeout(() => go("#/auto/schedules")); return null }

  const n = work.nodes.find((x) => x.id === sel)
  const edit = (g: Graph, s?: string) => { setWork(g); setDirty(true); setTrace(null); if (s) { setSel(s); setFresh(s) } }
  const act = async <T,>(key: string, p: () => Promise<T>, ok?: string | ((v: T) => string)) => {
    setBusy(key)
    try { const v = await p(); if (ok) toast(typeof ok === "string" ? ok : ok(v)); return v } catch (e) { toast(errText(e)); return undefined } finally { setBusy(null) }
  }
  const save = async () => {
    const r = await act("save", () => flowApi.saveFlow(id, work, undo?.note), (v) => t("auto:rule.savedAs", { v: v.version }))
    if (r) { setDirty(false); setUndo(null); setFresh(null); L.set(r); void runs.reload() }
  }
  const discard = () => { setWork(f.graph); setDirty(false); setUndo(null); setFresh(null); setTrace(null); setRev((x) => x + 1) }
  const dry = async (sampleObj?: object) => {
    const r = await act("dry", () => flowApi.dryRun(id, { graph: dirty ? work : undefined, sample: sampleObj }))
    if (r) { const tc = traceOf(r.steps ?? []); setTrace({ ...tc, run: r, label: sampleObj ? t("auto:rule.labelSample") : r.steps?.[0]?.output?.summary ? t("auto:rule.labelLatest", { summary: String(r.steps[0].output.summary).slice(0, 40) }) : t("auto:rule.labelEmpty") }); setSel(tc.path[tc.path.length - 1] ?? sel); toast(t("auto:rule.dryDone")) }
  }
  async function openRun(rid: string) {
    try { const r = await flowApi.run(rid); const tc = traceOf(r.steps ?? []); setTrace({ ...tc, run: r, label: tr("auto:rule.labelRun", { time: tm(r.started) }) }); setView("canvas") } catch (e) { toast(errText(e)) }
  }
  const steve = async (text: string) => {
    const before = work
    const r = await act("steve", () => flowApi.editDraft(id, text, work))
    if (r) { setWork(r.graph); setDirty(true); setTrace(null); setUndo({ g: before, note: r.note }); setRev((x) => x + 1); const added = r.graph.nodes.find((x) => !before.nodes.some((y) => y.id === x.id)); if (added) { setSel(added.id); setFresh(added.id) } toast(t("auto:rule.steveDone")) }
  }
  const usesAI = work.nodes.some((x) => x.kind === "bot")
  const step = trace && n ? [...(trace.run.steps ?? [])].reverse().find((s) => s.node_id === n.id) : undefined

  return (
    <Page title={f.name} back={BACK} max={1180}
      sub={<Meta><span>v{f.version} · {f.origin_label || t("auto:rule.drawnByYou")}</span><span>{sourceLabel(f.trigger?.source)}</span><span>{usesAI ? t("auto:usesAI") : t("auto:noAI")}</span><span>{t("auto:rule.runs24", { count: f.runs24 })}</span>{f.last_run && <span>{t("auto:rule.lastRun", { time: tm(f.last_run) })}</span>}</Meta>}
      actions={<>
        <span className="hidden type-footnote text-muted-foreground sm:inline">{f.enabled ? t("auto:enabled") : t("auto:rule.notEnabled")}</span>
        <Switch checked={f.enabled} disabled={busy === "on"} onCheckedChange={(v) => void act("on", () => flowApi.patchFlow(id, { enabled: v }), v ? t("auto:flows.enabledToast") : t("auto:flows.disabledToast")).then((r) => r && L.set(r))} aria-label={t("auto:rule.enableAria")} data-rule-on />
        <Button size="sm" variant="outline" disabled={!!busy} onClick={() => void dry()} data-dry><PlayIcon /><span className="max-sm:sr-only">{t("auto:rule.dryRun")}</span></Button>
        <Button size="sm" disabled={!dirty || !!busy} onClick={() => void save()} data-save-rule>{t("common:action.save")}</Button>
        <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-rule-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44">
            <DropdownMenuItem onClick={() => setSampleOpen(true)}><FlaskConicalIcon />{t("auto:rule.dryWithSample")}</DropdownMenuItem>
            <DropdownMenuItem onClick={() => setRename(true)}><PencilIcon />{t("auto:rule.rename")}</DropdownMenuItem>
            <DropdownMenuItem onClick={() => void act("dup", () => flowApi.duplicate(id), t("auto:rule.duplicated")).then((r) => r && go("#/auto/rule/" + r.id))} data-dup-rule><CopyIcon />{t("common:action.copy")}</DropdownMenuItem>
            {dirty && <DropdownMenuItem onClick={discard}>{t("auto:rule.discardChanges")}</DropdownMenuItem>}
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={() => setConfirmDel(true)} data-del-rule><TrashIcon />{t("common:action.delete")}</DropdownMenuItem>
          </DropdownMenuContent></DropdownMenu></>}>
      <AlertDialog open={confirmDel} onOpenChange={setConfirmDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("auto:rule.deleteTitle", { name: f.name })}</AlertDialogTitle><AlertDialogDescription>{t("auto:rule.deleteDesc")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => void act("del", () => flowApi.deleteFlow(id), t("auto:rule.deleted")).then((r) => r && go("#/auto/rules"))} data-del-rule-confirm>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {rename && <RenameDialog f={f} onClose={() => setRename(false)} onSaved={(r) => { L.set(r); setRename(false) }} />}
      {sampleOpen && <Modal title={t("auto:rule.sampleTitle")} desc={t("auto:rule.sampleDesc", { example: '{"summary": "…", "payload": {…}}' })} onClose={() => setSampleOpen(false)}
        footer={<><Button variant="outline" onClick={() => setSampleOpen(false)}>{t("common:action.cancel")}</Button><Button disabled={busy === "dry"} onClick={() => { let o: object | undefined; if (sample.trim()) { try { o = JSON.parse(sample) } catch { toast(t("auto:notJson")); return } } setSampleOpen(false); void dry(o) }} data-sample-go><PlayIcon />{t("auto:rule.dryRun")}</Button></>}>
        <Textarea aria-label={t("auto:rule.sampleAria")} className="min-h-40 rounded-lg bg-muted/50 font-mono text-[12px]" value={sample} onChange={(e) => setSample(e.target.value)} placeholder={t("auto:rule.samplePlaceholder")} data-sample />
      </Modal>}
      {undo && (
        <div className="mb-4 flex flex-wrap items-center gap-3 rounded-xl border px-4 py-2.5 type-callout anim-in" data-steve-change>
          <SparklesIcon className="size-4 text-blue" aria-hidden /><span className="min-w-0 flex-1">{t("auto:rule.steveChanged", { note: undo.note })}</span>
          <Button size="sm" variant="ghost" onClick={() => { setWork(undo.g); setUndo(null); setFresh(null); setRev((x) => x + 1); setDirty(JSON.stringify(undo.g) !== JSON.stringify(f.graph)) }}>{t("auto:rule.undo")}</Button>
        </div>
      )}
      {dirty && !undo && <div className="mb-4 flex items-center gap-2 type-footnote text-muted-foreground" data-dirty><StatusDot s="wait" />{t("auto:rule.dirty", { v: f.version + 1 })}<button type="button" className="text-blue hover:underline" onClick={discard}>{t("auto:rule.discard")}</button></div>}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="flex min-w-0 flex-col gap-4">
          <div className="flex flex-wrap items-center gap-2">
            <Segmented size="sm" value={view} onChange={setView} options={[["canvas", t("auto:rule.viewCanvas")], ["code", t("auto:rule.viewCode")]] as const} />
            {trace && <span className="inline-flex flex-wrap items-center gap-1.5 type-footnote text-muted-foreground" data-trace-banner><StatusDot s={trace.run.status === "failed" ? "fail" : "done"} />{t(trace.run.dry_run ? "auto:rule.traceDry" : "auto:rule.traceRun", { count: trace.path.length, label: trace.label })}<button type="button" className="text-blue hover:underline" onClick={() => setTrace(null)}>{t("auto:rule.clear")}</button></span>}
          </div>
          {view === "canvas" ? <FlowCanvas g={work} sel={sel} onSel={(x) => { setSel(x); setFresh(null) }} fresh={fresh} trace={trace} />
            : <div className="flex flex-col gap-2" data-code-view>
                <Textarea aria-label={t("auto:rule.codeAria")} className="min-h-[460px] rounded-xl bg-muted/50 font-mono text-[12px] leading-[19px]" value={toYaml(f.name, f.version, work)} readOnly />
                <div className="flex items-center gap-2 px-1 type-footnote text-muted-foreground"><span className="flex-1">{t("auto:rule.codeHint")}</span>
                  <Button size="xs" variant="ghost" onClick={() => void navigator.clipboard.writeText(JSON.stringify(work, null, 2)).then(() => toast(t("auto:rule.copiedJson")))}><CopyIcon />{t("auto:rule.copyJson")}</Button></div>
              </div>}
          <Composer placeholder={t("auto:rule.composer")} onSend={(x) => void steve(x)} testId="rule" disabled={busy === "steve"} />
          {trace && (
            <section className="anim-in" data-dry-result>
              <SectionLabel right={trace.run.outcome_label ? <span className="font-normal">{trace.run.outcome_label}</span> : undefined}>{trace.run.dry_run ? t("auto:rule.stepsDry") : t("auto:rule.stepsRun")}</SectionLabel>
              <Group>{(trace.run.steps ?? []).map((s) => { const x = work.nodes.find((y) => y.id === s.node_id); return (
                <Row key={s.seq} onClick={() => { setSel(s.node_id); setView("canvas") }} data-step={s.node_id}
                  title={<span><span className="mr-2 font-mono text-[12px] text-muted-foreground">{s.seq}</span>{x?.title ?? s.node_id}{s.branch && <span className="ml-1.5 type-footnote text-muted-foreground">→ {s.branch}</span>}</span>}
                  sub={s.error ? <span className="text-destructive">{s.error}</span> : s.output?.skipped ? String(s.output.skipped) : s.status === "waiting" ? t("auto:runStatus.waiting") : JSON.stringify(s.output ?? null).slice(0, 100)}>
                  <span className="type-footnote tabular-nums text-muted-foreground">{ms(s.ms)}</span></Row>) })}</Group>
              {trace.run.error && <div className="mt-2 px-1 type-footnote text-destructive">{trace.run.error}</div>}
            </section>
          )}
          <section>
            <SectionLabel right={<a href="#/signals" className="font-normal hover:text-foreground">{t("auto:rule.viewInSignals")}</a>}>{t("auto:rule.recentRuns")}</SectionLabel>
            {runs.data?.length ? <Group>{runs.data.map((r) => (
              <Row key={r.id} onClick={() => void openRun(r.id)} data-run-row={r.id}
                title={<span className="inline-flex items-center gap-1.5"><StatusDot s={RUN_STATUS[r.status]?.[1] ?? "idle"} />{r.outcome_label || RUN_STATUS[r.status]?.[0] || r.status}</span>}
                sub={[r.dry_run ? t("auto:rule.dryRun") : "", "v" + r.flow_version, r.error ?? "", t("auto:rule.clickSteps")].filter(Boolean).join(" · ")}>
                {(r.status === "running" || r.status === "waiting") && <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={(e) => { e.stopPropagation(); void act("cancel", () => flowApi.cancelRun(r.id), t("auto:rule.runCancelled")).then(() => runs.reload()) }}>{t("common:action.cancel")}</Button>}
                <span className="type-footnote tabular-nums text-muted-foreground">{tm(r.started)}</span></Row>))}</Group>
              : <Empty title={runs.data ? t("auto:noRuns") : t("auto:loading")} />}
          </section>
          <section data-versions>
            <SectionLabel>{t("auto:rule.versions")}</SectionLabel>
            <Group>{f.versions.map((v) => (
              <Row key={v.version} title={v.version === f.version ? `v${v.version} · ${t("auto:rule.current")}` : `v${v.version}`} sub={[tm(v.created), authorLabel(v.author), v.note].filter(Boolean).join(" · ")} data-version={v.version}>
                {v.version !== f.version && <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={!!busy} onClick={() => void act("rb", () => flowApi.rollback(id, v.version), (r) => t("auto:rule.rolledBack", { from: v.version, to: r.version })).then((r) => { if (r) { setDirty(false); setUndo(null); L.set(r) } })} data-rollback={v.version}>{t("auto:rule.rollback")}</Button>}
              </Row>))}</Group>
          </section>
        </div>
        <aside className="flex flex-col gap-4 lg:sticky lg:top-2 lg:max-h-[calc(100dvh-4rem)] lg:self-start lg:overflow-y-auto lg:border-l lg:pl-6" aria-label={t("auto:rule.nodeSettings")}>
          {n ? <Inspector key={n.id + ":" + rev} g={work} n={n} onChange={edit} onSelect={setSel} flows={flows.data ?? []} sources={sources.data ?? []} step={step} flowId={id} />
            : <div className="px-1 type-callout text-muted-foreground">{t("auto:rule.pickNode")}</div>}
          {n && trace && !step && <div className="px-1 type-footnote text-muted-foreground">{t(trace.run.dry_run ? "auto:rule.notPassedDry" : "auto:rule.notPassedRun", { name: n.title || kindOf(n.kind)[0] })}</div>}
          {n?.kind === "trigger" && trace?.run.dry_run && trace.run.steps?.[0] && <div className="px-1"><div className="mb-1 type-footnote text-muted-foreground">{t("auto:rule.eventUsed")}</div><Json v={trace.run.steps[0].input} /></div>}
          <div className="mt-auto border-t pt-4 type-footnote text-muted-foreground">{t("auto:rule.footer")}</div>
        </aside>
      </div>
    </Page>
  )
}

function RenameDialog({ f, onClose, onSaved }: { f: Flow; onClose: () => void; onSaved: (f: Flow) => void }) {
  const [name, setName] = useState(f.name), [desc, setDesc] = useState(f.description), [busy, setBusy] = useState(false)
  const ok = useMemo(() => name.trim().length > 0 && name.trim().length <= 40, [name])
  const t = useT()
  return (
    <Modal title={t("auto:rule.rename")} onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!ok || busy} onClick={async () => { setBusy(true); try { onSaved(await flowApi.patchFlow(f.id, { name: name.trim(), description: desc.trim() })); toast(t("common:state.saved")) } catch (e) { toast(errText(e)) } finally { setBusy(false) } }}>{t("common:action.save")}</Button></>}>
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="flow-name">{t("auto:rule.name")}</FieldLabel><Input id="flow-name" className="rounded-lg" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} /></Field>
        <Field><FieldLabel htmlFor="flow-desc">{t("auto:rule.description")}</FieldLabel><Textarea id="flow-desc" className="min-h-16 rounded-lg" value={desc} onChange={(e) => setDesc(e.target.value)} /></Field>
      </FieldGroup>
    </Modal>
  )
}
