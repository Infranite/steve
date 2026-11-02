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
import { FlowCanvas, traceOf } from "@/auto/canvas"
import { Inspector, toYaml, kindOf } from "@/auto/nodes"
import { Loading, Failed, Modal, Json, ms, RUN_STATUS } from "@/auto/common"

const BACK: [string, string] = ["#/auto/rules", "自动化"]
const runParam = () => new URLSearchParams(location.hash.split("?")[1] ?? "").get("run")
const authorLabel = (a: string) => (a === "steve" ? "Steve" : a.startsWith("plugin:") ? "插件" : "你")

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
  const [view, setView] = useState<"画布" | "代码">("画布")
  const [busy, setBusy] = useState<string | null>(null)
  const [confirmDel, setConfirmDel] = useState(false), [rename, setRename] = useState(false), [sampleOpen, setSampleOpen] = useState(false)
  const [sample, setSample] = useState("")
  const f = L.data

  /* the saved graph replaces the working copy unless you are editing it */
  useEffect(() => { if (f && !dirty) { setWork(f.graph); setRev((x) => x + 1); if (!f.graph.nodes.some((n) => n.id === sel)) setSel(f.graph.nodes[0]?.id ?? null) } }, [f])
  /* #/auto/rule/<id>?run=<run>: show that run's path */
  useEffect(() => { const r = runParam(); if (r) void openRun(r) }, [id])

  if (L.unwired) return <Unwired id="auto" />
  if (L.error && !f) return L.error instanceof HubError && L.error.status === 404
    ? <Page title="流程" back={BACK}><Empty title="流程不存在" sub="可能已经删除。" /></Page>
    : <Failed title="流程" back={BACK} msg={errText(L.error)} retry={() => void L.reload()} />
  if (!f || !work) return <Loading title="流程" back={BACK} />
  if (f.kind === "schedule") { setTimeout(() => go("#/auto/schedules")); return null }

  const n = work.nodes.find((x) => x.id === sel)
  const edit = (g: Graph, s?: string) => { setWork(g); setDirty(true); setTrace(null); if (s) { setSel(s); setFresh(s) } }
  const act = async <T,>(key: string, p: () => Promise<T>, ok?: string | ((v: T) => string)) => {
    setBusy(key)
    try { const v = await p(); if (ok) toast(typeof ok === "string" ? ok : ok(v)); return v } catch (e) { toast(errText(e)); return undefined } finally { setBusy(null) }
  }
  const save = async () => {
    const r = await act("save", () => flowApi.saveFlow(id, work, undo?.note), (v) => `已保存为 v${v.version}`)
    if (r) { setDirty(false); setUndo(null); setFresh(null); L.set(r); void runs.reload() }
  }
  const discard = () => { setWork(f.graph); setDirty(false); setUndo(null); setFresh(null); setTrace(null); setRev((x) => x + 1) }
  const dry = async (sampleObj?: object) => {
    const r = await act("dry", () => flowApi.dryRun(id, { graph: dirty ? work : undefined, sample: sampleObj }))
    if (r) { const t = traceOf(r.steps ?? []); setTrace({ ...t, run: r, label: sampleObj ? "你给的样例" : r.steps?.[0]?.output?.summary ? `最近一条事件「${String(r.steps[0].output.summary).slice(0, 40)}」` : "空事件（还没有匹配的事件）" }); setSel(t.path[t.path.length - 1] ?? sel); toast("试运行完成，没有执行任何对外动作") }
  }
  async function openRun(rid: string) {
    try { const r = await flowApi.run(rid); const t = traceOf(r.steps ?? []); setTrace({ ...t, run: r, label: `${tm(r.started)} 的运行` }); setView("画布") } catch (e) { toast(errText(e)) }
  }
  const steve = async (text: string) => {
    const before = work
    const r = await act("steve", () => flowApi.editDraft(id, text, work))
    if (r) { setWork(r.graph); setDirty(true); setTrace(null); setUndo({ g: before, note: r.note }); setRev((x) => x + 1); const added = r.graph.nodes.find((x) => !before.nodes.some((y) => y.id === x.id)); if (added) { setSel(added.id); setFresh(added.id) } toast("Steve 改好了，检查后保存") }
  }
  const usesAI = work.nodes.some((x) => x.kind === "bot")
  const step = trace && n ? [...(trace.run.steps ?? [])].reverse().find((s) => s.node_id === n.id) : undefined

  return (
    <Page title={f.name} back={BACK} max={1180}
      sub={<Meta><span>v{f.version} · {f.origin_label || "你画的"}</span><span>{sourceLabel(f.trigger?.source)}</span><span>{usesAI ? "含 AI 判断" : "不用 AI"}</span><span>24 小时运行 {f.runs24} 次</span>{f.last_run && <span>最近 {tm(f.last_run)}</span>}</Meta>}
      actions={<>
        <span className="hidden type-footnote text-muted-foreground sm:inline">{f.enabled ? "已启用" : "未启用"}</span>
        <Switch checked={f.enabled} disabled={busy === "on"} onCheckedChange={(v) => void act("on", () => flowApi.patchFlow(id, { enabled: v }), v ? "流程已启用" : "流程已停用").then((r) => r && L.set(r))} aria-label="启用流程" data-rule-on />
        <Button size="sm" variant="outline" disabled={!!busy} onClick={() => void dry()} data-dry><PlayIcon /><span className="max-sm:sr-only">试运行</span></Button>
        <Button size="sm" disabled={!dirty || !!busy} onClick={() => void save()} data-save-rule>保存</Button>
        <DropdownMenu><DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" data-rule-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44">
            <DropdownMenuItem onClick={() => setSampleOpen(true)}><FlaskConicalIcon />用样例试运行…</DropdownMenuItem>
            <DropdownMenuItem onClick={() => setRename(true)}><PencilIcon />改名和说明</DropdownMenuItem>
            <DropdownMenuItem onClick={() => void act("dup", () => flowApi.duplicate(id), "已复制，默认停用").then((r) => r && go("#/auto/rule/" + r.id))} data-dup-rule><CopyIcon />复制</DropdownMenuItem>
            {dirty && <DropdownMenuItem onClick={discard}>放弃未保存的修改</DropdownMenuItem>}
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={() => setConfirmDel(true)} data-del-rule><TrashIcon />删除</DropdownMenuItem>
          </DropdownMenuContent></DropdownMenu></>}>
      <AlertDialog open={confirmDel} onOpenChange={setConfirmDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>删除流程「{f.name}」？</AlertDialogTitle><AlertDialogDescription>之后同类事件只记录在信号页，不再处理。已经由它建出来的事项和运行记录不受影响。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => void act("del", () => flowApi.deleteFlow(id), "已删除流程").then((r) => r && go("#/auto/rules"))} data-del-rule-confirm>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {rename && <RenameDialog f={f} onClose={() => setRename(false)} onSaved={(r) => { L.set(r); setRename(false) }} />}
      {sampleOpen && <Modal title="用样例试运行" desc="写一个事件对象（JSON），例如 {&quot;summary&quot;: &quot;…&quot;, &quot;payload&quot;: {…}}。留空就用最近一条匹配的事件。对外动作不会执行。" onClose={() => setSampleOpen(false)}
        footer={<><Button variant="outline" onClick={() => setSampleOpen(false)}>取消</Button><Button disabled={busy === "dry"} onClick={() => { let o: object | undefined; if (sample.trim()) { try { o = JSON.parse(sample) } catch { toast("不是合法的 JSON"); return } } setSampleOpen(false); void dry(o) }} data-sample-go><PlayIcon />试运行</Button></>}>
        <Textarea aria-label="样例事件 JSON" className="min-h-40 rounded-lg bg-muted/50 font-mono text-[12px]" value={sample} onChange={(e) => setSample(e.target.value)} placeholder={'{\n  "summary": "Sarah（Acme）：周五评审能改到下午 2 点吗？",\n  "payload": { "from": "sarah@acme.com" }\n}'} data-sample />
      </Modal>}
      {undo && (
        <div className="mb-4 flex flex-wrap items-center gap-3 rounded-xl border px-4 py-2.5 type-callout anim-in" data-steve-change>
          <SparklesIcon className="size-4 text-blue" aria-hidden /><span className="min-w-0 flex-1">Steve 改了：{undo.note}。检查后保存才会成为新版本。</span>
          <Button size="sm" variant="ghost" onClick={() => { setWork(undo.g); setUndo(null); setFresh(null); setRev((x) => x + 1); setDirty(JSON.stringify(undo.g) !== JSON.stringify(f.graph)) }}>撤销</Button>
        </div>
      )}
      {dirty && !undo && <div className="mb-4 flex items-center gap-2 type-footnote text-muted-foreground" data-dirty><StatusDot s="wait" />有未保存的修改，保存后成为 v{f.version + 1}<button type="button" className="text-blue hover:underline" onClick={discard}>放弃</button></div>}
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="flex min-w-0 flex-col gap-4">
          <div className="flex flex-wrap items-center gap-2">
            <Segmented size="sm" value={view} onChange={setView} options={["画布", "代码"] as const} />
            {trace && <span className="inline-flex flex-wrap items-center gap-1.5 type-footnote text-muted-foreground" data-trace-banner><StatusDot s={trace.run.status === "failed" ? "fail" : "done"} />{trace.run.dry_run ? "试运行" : "这次运行"}走了 {trace.path.length} 步，用的是{trace.label}<button type="button" className="text-blue hover:underline" onClick={() => setTrace(null)}>清除</button></span>}
          </div>
          {view === "画布" ? <FlowCanvas g={work} sel={sel} onSel={(x) => { setSel(x); setFresh(null) }} fresh={fresh} trace={trace} />
            : <div className="flex flex-col gap-2" data-code-view>
                <Textarea aria-label="流程的文本形式" className="min-h-[460px] rounded-xl bg-muted/50 font-mono text-[12px] leading-[19px]" value={toYaml(f.name, f.version, work)} readOnly />
                <div className="flex items-center gap-2 px-1 type-footnote text-muted-foreground"><span className="flex-1">流程的文本形式，和画布完全等价，只读。新建流程时可以粘贴 JSON 形式。</span>
                  <Button size="xs" variant="ghost" onClick={() => void navigator.clipboard.writeText(JSON.stringify(work, null, 2)).then(() => toast("已复制 JSON"))}><CopyIcon />复制 JSON</Button></div>
              </div>}
          <Composer placeholder="让 Steve 修改这条流程，例如：带合同或发票的邮件单独建事项" onSend={(t) => void steve(t)} testId="rule" disabled={busy === "steve"} />
          {trace && (
            <section className="anim-in" data-dry-result>
              <SectionLabel right={trace.run.outcome_label ? <span className="font-normal">{trace.run.outcome_label}</span> : undefined}>{trace.run.dry_run ? "试运行" : "运行"} · 每一步的输入和输出</SectionLabel>
              <Group>{(trace.run.steps ?? []).map((s) => { const x = work.nodes.find((y) => y.id === s.node_id); return (
                <Row key={s.seq} onClick={() => { setSel(s.node_id); setView("画布") }} data-step={s.node_id}
                  title={<span><span className="mr-2 font-mono text-[12px] text-muted-foreground">{s.seq}</span>{x?.title ?? s.node_id}{s.branch && <span className="ml-1.5 type-footnote text-muted-foreground">→ {s.branch}</span>}</span>}
                  sub={s.error ? <span className="text-destructive">{s.error}</span> : s.output?.skipped ? String(s.output.skipped) : s.status === "waiting" ? "等待中" : JSON.stringify(s.output ?? null).slice(0, 100)}>
                  <span className="type-footnote tabular-nums text-muted-foreground">{ms(s.ms)}</span></Row>) })}</Group>
              {trace.run.error && <div className="mt-2 px-1 type-footnote text-destructive">{trace.run.error}</div>}
            </section>
          )}
          <section>
            <SectionLabel right={<a href="#/signals" className="font-normal hover:text-foreground">在信号页查看</a>}>最近运行</SectionLabel>
            {runs.data?.length ? <Group>{runs.data.map((r) => (
              <Row key={r.id} onClick={() => void openRun(r.id)} data-run-row={r.id}
                title={<span className="inline-flex items-center gap-1.5"><StatusDot s={RUN_STATUS[r.status]?.[1] ?? "idle"} />{r.outcome_label || RUN_STATUS[r.status]?.[0] || r.status}</span>}
                sub={`${r.dry_run ? "试运行 · " : ""}v${r.flow_version}${r.error ? " · " + r.error : ""} · 点击查看每一步`}>
                {(r.status === "running" || r.status === "waiting") && <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={(e) => { e.stopPropagation(); void act("cancel", () => flowApi.cancelRun(r.id), "已取消这次运行").then(() => runs.reload()) }}>取消</Button>}
                <span className="type-footnote tabular-nums text-muted-foreground">{tm(r.started)}</span></Row>))}</Group>
              : <Empty title={runs.data ? "还没有运行" : "载入中…"} />}
          </section>
          <section data-versions>
            <SectionLabel>版本历史</SectionLabel>
            <Group>{f.versions.map((v) => (
              <Row key={v.version} title={`v${v.version}${v.version === f.version ? " · 当前" : ""}`} sub={[tm(v.created), authorLabel(v.author), v.note].filter(Boolean).join(" · ")} data-version={v.version}>
                {v.version !== f.version && <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={!!busy} onClick={() => void act("rb", () => flowApi.rollback(id, v.version), (r) => `已回到 v${v.version} 的内容，保存为 v${r.version}`).then((r) => { if (r) { setDirty(false); setUndo(null); L.set(r) } })} data-rollback={v.version}>回到这个版本</Button>}
              </Row>))}</Group>
          </section>
        </div>
        <aside className="flex flex-col gap-4 lg:sticky lg:top-2 lg:max-h-[calc(100dvh-4rem)] lg:self-start lg:overflow-y-auto lg:border-l lg:pl-6" aria-label="节点设置">
          {n ? <Inspector key={n.id + ":" + rev} g={work} n={n} onChange={edit} onSelect={setSel} flows={flows.data ?? []} sources={sources.data ?? []} step={step} flowId={id} />
            : <div className="px-1 type-callout text-muted-foreground">点画布上的节点查看和修改。</div>}
          {n && trace && !step && <div className="px-1 type-footnote text-muted-foreground">这次{trace.run.dry_run ? "试运行" : "运行"}没有经过「{n.title || kindOf(n.kind)[0]}」。</div>}
          {n?.kind === "trigger" && trace?.run.dry_run && trace.run.steps?.[0] && <div className="px-1"><div className="mb-1 type-footnote text-muted-foreground">用的事件</div><Json v={trace.run.steps[0].input} /></div>}
          <div className="mt-auto border-t pt-4 type-footnote text-muted-foreground">决策表、脚本、HTTP、等待这些步骤不调用 AI，结果可复现；只有“Bot 判断”会用模型。每次运行都记在信号页，能看到每一步的输入和输出。</div>
        </aside>
      </div>
    </Page>
  )
}

function RenameDialog({ f, onClose, onSaved }: { f: Flow; onClose: () => void; onSaved: (f: Flow) => void }) {
  const [name, setName] = useState(f.name), [desc, setDesc] = useState(f.description), [busy, setBusy] = useState(false)
  const ok = useMemo(() => name.trim().length > 0 && name.trim().length <= 40, [name])
  return (
    <Modal title="改名和说明" onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!ok || busy} onClick={async () => { setBusy(true); try { onSaved(await flowApi.patchFlow(f.id, { name: name.trim(), description: desc.trim() })); toast("已保存") } catch (e) { toast(errText(e)) } finally { setBusy(false) } }}>保存</Button></>}>
      <FieldGroup className="gap-4">
        <Field><FieldLabel htmlFor="flow-name">名称</FieldLabel><Input id="flow-name" className="rounded-lg" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} /></Field>
        <Field><FieldLabel htmlFor="flow-desc">说明</FieldLabel><Textarea id="flow-desc" className="min-h-16 rounded-lg" value={desc} onChange={(e) => setDesc(e.target.value)} /></Field>
      </FieldGroup>
    </Modal>
  )
}
