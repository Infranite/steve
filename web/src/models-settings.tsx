/* 设置 · 模型：Steve 是一个 Bot。它自己的判断（分流、对话回答、研究、起标题……）默认都用 Steve 这个 Bot 的 harness、模型和
 * 思考强度（在 Bot 页改）。页面只铺开「默认配置」和少数单独配置的用途；跟随默认的收起，编辑在侧栏里做，备用模型放进高级选项。
 * 最近 7 天实际调用了什么在「运行记录」里看，跟当前配置分开。最下面是新会话在每个 harness 上的默认模型。 */
import { ago as fmtAgo } from "@/i18n/format"
import { useEffect, useRef, useState, type ReactNode, type Ref } from "react"
import { cn } from "cn"
import { ArrowDownIcon, ArrowUpIcon, ChevronDownIcon, ChevronRightIcon, CpuIcon, PlusIcon, TriangleAlertIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { modReq, isNotWired } from "@/api"
import { st, toast, useStore } from "@/store"
import { Group, Row, SectionLabel, Segmented } from "@/ui"
import { Select } from "@/view-shared"
import { Robot } from "@/robot"
import { ModelPicker, harnessName } from "@/model-picker"
import { editBot } from "@/manage"

export const MODELS_CAT: [string, string, any, string] = ["models", "模型", CpuIcon, "Steve 用哪个 harness 和模型做判断、新会话默认"]

type Choice = { harness: string; models: string[]; effort?: string | null }
type UsedRow = { harness: string; model: string; calls: number; failed: number; at: number }
type Recent = { last: { harness: string; model: string; at: number } | null; used: UsedRow[] }
type Purpose = { key: string; label: string; desc: string; value: Choice | null; follows: boolean; effective: Choice; recommended: Choice | null; codex_note: string | null; recent: Recent | null }
type Steve = { bot_id: string | null; name: string; harness: string; model: string | null; bot_effort: string | null; runs_on: string; models: string[]; effort: string | null; note: string | null; model_note: string | null }
type Harness = { id: string; name: string; efforts: { key: string; label: string }[]; default_models: string[] }
type Agent = { harness: string; name: string; models: string[]; default: string | null; value: string | null }
type View = { steve: Steve; purposes: Purpose[]; agents: Agent[]; harnesses: Harness[]; max: number }

const api = {
  get: () => modReq<View>("GET", "/settings/models"),
  patch: (b: { purposes?: Record<string, Choice | null>; agents?: Record<string, string | null> }) => modReq<View>("PATCH", "/settings/models", b),
}
const short = (m: string) => m.replace(/^claude-/, "")
const EFFORT: Record<string, string> = { none: "关闭", minimal: "最低", low: "低", medium: "中", high: "高", xhigh: "很高", max: "最高" }
const effortName = (e?: string | null) => e ? EFFORT[e] ?? e : "跟随模型默认"
/** `Claude Code · claude-haiku-5-5 → claude-sonnet-5-5 · 思考高` */
export function ChoiceLine({ c, className, wrap }: { c: Choice; className?: string; wrap?: boolean }) {
  return (
    <span className={cn("block min-w-0", wrap ? "break-words" : "truncate", className)} data-choice>
      <span className="font-medium text-foreground">{harnessName(c.harness)}</span>
      <span className="text-muted-foreground"> · </span>
      <span className="font-mono text-[12.5px] text-foreground/85">{c.models.length ? c.models.join(" → ") : "默认模型"}</span>
      {c.effort && <span className="text-muted-foreground"> · 思考{effortName(c.effort)}</span>}
    </span>
  )
}
/** one line for a list row: `Claude Code · haiku-5-5 · 备用 1 个 · 思考高` (the fallbacks are an advanced detail) */
function Brief({ c }: { c: Choice }) {
  return (
    <span className="block min-w-0 truncate" data-brief title={`${harnessName(c.harness)} · ${c.models.join(" → ") || "默认模型"}`}>
      {harnessName(c.harness)} · <span className="font-mono text-[12.5px]">{c.models[0] ? short(c.models[0]) : "默认模型"}</span>
      {c.models.length > 1 && ` · 备用 ${c.models.length - 1} 个`}{c.effort && ` · 思考${effortName(c.effort)}`}
    </span>
  )
}
const same = (a: Choice | null, b: Choice | null) => !!a && !!b && a.harness === b.harness && a.models.join() === b.models.join() && (a.effort ?? null) === (b.effort ?? null)
const ago = (t: number) => fmtAgo(t, { justNow: false })
const failedOf = (p: Purpose) => p.recent?.used.reduce((n, u) => n + u.failed, 0) ?? 0
const callsOf = (p: Purpose) => p.recent?.used.reduce((n, u) => n + u.calls, 0) ?? 0

export function ModelsSettingsPane() {
  useStore()
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
  if (err === "absent") return <div className="rounded-xl border px-4 py-3.5 type-callout text-muted-foreground" data-models-unwired>这个 Hub 还不能设置模型，换成新版 Hub 后可以在这里改。</div>
  if (err) return <div className="type-callout text-muted-foreground" role="alert">{err}</div>
  if (!v) return <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
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
    <div className="flex flex-col gap-10" data-models-settings>
      <section data-models-default>
        <SectionLabel>默认配置</SectionLabel>
        <div className="flex items-start gap-3 rounded-xl border bg-card px-4 py-3.5">
          <Robot k={s.bot_id ?? "steve"} size={36} className="mt-0.5 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="type-callout text-muted-foreground">没有单独配置的用途都用 {s.name} 的</p>
            <ChoiceLine c={{ ...steveChoice, effort: null }} wrap className="mt-0.5 type-body" />
            <p className="mt-0.5 type-footnote text-muted-foreground" data-steve-effort>思考强度：{effortName(s.effort)}{s.model_note && ` · ${s.model_note}`}{s.models.length > 1 && " · 前一个模型出错或超时才换下一个"}</p>
            {s.note && <div className="mt-1.5 flex items-start gap-1.5 type-footnote text-attention" role="note"><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" />{s.note}</div>}
          </div>
          <Button size="sm" variant="outline" className="shrink-0" onClick={() => editBot(s.bot_id ?? "steve")} data-edit-steve>修改</Button>
        </div>
      </section>

      <section data-models-overrides>
        <SectionLabel right={rest.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-add-override />}><PlusIcon />添加单独配置</DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-40">{rest.map((p) => <DropdownMenuItem key={p.key} onClick={() => setEdit({ key: p.key, own: true })} data-add-purpose={p.key}>{p.label}</DropdownMenuItem>)}</DropdownMenuContent>
          </DropdownMenu>
        )}>单独配置</SectionLabel>
        {own.length ? <Group>{own.map((p) => (
          <Row key={p.key} data-purpose={p.key} data-custom title={p.label} sub={<Brief c={p.effective} />} onClick={() => setEdit({ key: p.key, own: true })} aria-label={`编辑${p.label}`}>
            <span className="type-footnote text-muted-foreground">编辑</span><ChevronRightIcon className="size-4 text-tertiary" />
          </Row>
        ))}</Group> : <p className="type-callout text-muted-foreground" data-no-overrides>所有用途都跟随默认配置。</p>}
        {rest.length > 0 && <div className="mt-2" data-models-follows={more ? "open" : "closed"}>
          <button type="button" className="-mx-2 flex w-[calc(100%+1rem)] items-center gap-2 rounded-lg px-2 py-2 text-left type-callout text-muted-foreground hover:bg-accent" onClick={() => setMore(!more)} aria-expanded={more} data-follows-toggle>
            <ChevronRightIcon className={cn("size-4 shrink-0 text-tertiary transition-transform", more && "rotate-90")} />
            <span className="min-w-0 flex-1">其余 {rest.length} 项跟随默认</span>
          </button>
          {more && <Group className="mt-1">{rest.map((p) => (
            <Row key={p.key} data-purpose={p.key} data-follows title={p.label} sub={p.desc} onClick={() => setEdit({ key: p.key, own: false })} aria-label={`配置${p.label}`}>
              <span className="type-footnote text-tertiary">跟随默认</span><ChevronRightIcon className="size-4 text-tertiary" />
            </Row>
          ))}</Group>}
        </div>}
        {failed > 0 && <button type="button" className="mt-3 flex w-full items-start gap-2 rounded-lg border border-attention/30 bg-attention/5 px-3 py-2 text-left type-footnote text-attention hover:bg-attention/10" onClick={showLog} data-models-failures>
          <TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" />
          <span className="min-w-0 flex-1">最近 7 天有 {failed} 次调用失败（{failing.map((p) => `${p.label} ${failedOf(p)}`).join("、")}）</span>
          <span className="shrink-0 whitespace-nowrap underline-offset-2 hover:underline">查看记录</span>
        </button>}
      </section>

      <section>
        <SectionLabel>新会话的默认模型</SectionLabel>
        <p className="mb-3 type-callout text-muted-foreground">Bot 没有选模型时，新开的 Agent 会话用这里的模型。Bot 自己选了模型的，以 Bot 为准。</p>
        <Group>{v.agents.map((a) => (
          <div key={a.harness} data-agent={a.harness}>
            <Row title={a.name} sub={a.value ? "已改" : "跟随 Hub 的默认"}>
              <div className="w-[min(240px,52vw)]"><ModelPicker harness={a.harness} value={a.value ?? ""} allowDefault defaultLabel={a.default ? `默认（${a.default}）` : undefined} testId={`agent-${a.harness}`}
                onChange={(m) => void save({ agents: { [a.harness]: m || null } }, m ? `${a.name} 新会话默认用 ${m}` : `${a.name} 恢复默认`)} /></div>
            </Row>
          </div>
        ))}</Group>
      </section>

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
  const used = v.purposes.filter((p) => p.recent?.used.length)
  const idle = v.purposes.filter((p) => !p.recent?.used.length)
  const calls = v.purposes.reduce((n, p) => n + callsOf(p), 0)
  const failed = v.purposes.reduce((n, p) => n + failedOf(p), 0)
  const current = (p: Purpose, u: UsedRow) => u.harness === p.effective.harness && (!p.effective.models.length || p.effective.models.some((m) => short(m) === short(u.model)))
  return (
    <section ref={ref} className="scroll-mt-4" data-models-log={open ? "open" : "closed"}>
      <button type="button" className="mb-2 flex w-full items-center justify-between gap-2 rounded-lg py-1 text-left" onClick={() => setOpen(!open)} aria-expanded={open} data-log-toggle>
        <span className="flex min-w-0 flex-col">
          <span className="type-label text-muted-foreground">运行记录</span>
          <span className="type-footnote text-muted-foreground" data-log-summary>最近 7 天 {calls} 次调用{failed > 0 && <span className="text-attention"> · 失败 {failed} 次</span>}</span>
        </span>
        <ChevronDownIcon className={cn("size-4 shrink-0 text-tertiary transition-transform", open && "rotate-180")} />
      </button>
      {open && <>
        <p className="mb-3 type-footnote text-muted-foreground">这里是已经发生的调用，用的是当时的配置；与现在配置不同的标为「非当前配置」。按用途的完整用量见<a href="#/stats" className="ml-0.5 whitespace-nowrap text-blue hover:underline">统计</a>。</p>
        {used.length ? <Group>{used.map((p) => (
          <div key={p.key} className="px-2 py-2.5" data-log-purpose={p.key}>
            <div className="flex items-baseline justify-between gap-3">
              <span className="type-body">{p.label}</span>
              <span className="shrink-0 type-footnote text-muted-foreground">{p.recent?.last && ago(p.recent.last.at)}</span>
            </div>
            <ul className="mt-1 flex flex-col gap-0.5">{p.recent!.used.map((u, i) => (
              <li key={i} className="flex min-w-0 flex-wrap items-baseline gap-x-2 type-footnote text-muted-foreground" data-log-used={u.model}>
                <span className="min-w-0 break-words">{harnessName(u.harness)} · <span className="font-mono">{short(u.model)}</span> ×{u.calls}</span>
                {u.failed > 0 && <span className="text-attention">失败 {u.failed}</span>}
                {!current(p, u) && <span className="rounded bg-muted px-1.5 py-px text-[11px]" data-log-stale>非当前配置</span>}
              </li>
            ))}</ul>
          </div>
        ))}</Group> : <p className="type-callout text-muted-foreground">最近 7 天没有调用。</p>}
        {used.length > 0 && idle.length > 0 && <p className="mt-2 type-footnote text-tertiary" data-log-idle>没有调用：{idle.map((p) => p.label).join("、")}</p>}
      </>}
    </section>
  )
}

/** The side-panel editor of one purpose: follow the default, or its own harness, model and effort; the
 *  fallback models and the recommended choice sit under 高级选项. */
function Editor({ p, v, steve, startOwn, onSave, onCancel }: { p: Purpose; v: View; steve: Choice; startOwn: boolean; onSave: (c: Choice | null, ok: string) => Promise<void>; onCancel: () => void }) {
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
  const inherited = onSteve ? `跟随默认（${steve.models[0] ?? "默认模型"}）` : `${harnessName(harness)} 默认（${h?.default_models[0] ?? "它自己的"}）`
  return (
    <div className="flex min-h-full flex-col" data-models-editor={p.key}>
      <SheetHeader className="gap-1 px-5 pb-4 pt-5 pr-14">
        <SheetTitle className="type-title">{p.label}</SheetTitle>
        <SheetDescription className="type-footnote">{p.desc}</SheetDescription>
      </SheetHeader>
      <div className="flex flex-col gap-5 px-5">
        <Segmented size="sm" value={mode} onChange={setMode} options={[["follow", "跟随默认"], ["own", "单独配置"]] as const} />
        {mode === "follow" ? (
          <div className="rounded-lg bg-muted/60 px-3 py-2.5 type-callout" data-follow-preview>
            <span className="block type-footnote text-muted-foreground">用默认配置，默认改了这里跟着变</span>
            <ChoiceLine c={steve} wrap className="mt-0.5" />
          </div>
        ) : <>
          <Field label="Harness"><Select className="w-full" value={harness} testId={`harness-${p.key}`} onChange={switchHarness} options={v.harnesses.map((x) => ({ v: x.id, l: x.name }))} /></Field>
          {harness === "codex" && p.codex_note && <div className="-mt-3 flex items-start gap-1.5 type-footnote text-muted-foreground" data-codex-note><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" />{p.codex_note}</div>}
          <Field label="模型"><ModelPicker harness={harness} value={primary} allowDefault defaultLabel={inherited} testId={`model-${p.key}`} onChange={setPrimary} /></Field>
          <Field label="思考强度"><Select className="w-full" value={effort || "default"} testId={`effort-${p.key}`} onChange={(x) => setEffort(x === "default" ? "" : x)}
            options={[{ v: "default", l: onSteve && steve.effort ? `跟随默认（${effortName(steve.effort)}）` : "跟随模型默认" }, ...(h?.efforts ?? []).map((e) => ({ v: e.key, l: e.label }))]} /></Field>
          <div className="border-t pt-3" data-models-adv={adv ? "open" : "closed"}>
            <button type="button" className="flex w-full items-center gap-1.5 py-1 text-left type-callout text-muted-foreground hover:text-foreground" onClick={() => setAdv(!adv)} aria-expanded={adv} data-adv-toggle>
              <ChevronRightIcon className={cn("size-4 shrink-0 transition-transform", adv && "rotate-90")} />
              <span className="min-w-0 flex-1">高级选项</span>
              {!adv && backups.length > 0 && <span className="type-footnote">备用 {backups.length} 个</span>}
            </button>
            {adv && <div className="mt-2 flex flex-col gap-4">
              <div>
                <div className="mb-1.5 type-footnote text-muted-foreground">备用模型：主模型出错或超时时，按顺序换下一个（最多 {v.max - 1} 个）</div>
                {!primary ? <div className="rounded-lg bg-muted/60 px-2.5 py-1.5 type-footnote text-muted-foreground">先选一个模型，才能加备用模型</div> : <>
                  {backups.length > 0 && <ol className="flex flex-col gap-1">{backups.map((m, i) => (
                    <li key={m} className="flex items-center gap-2 rounded-lg bg-muted/60 py-1 pl-2.5 pr-1" data-model-item={m}>
                      <span className="w-4 shrink-0 text-right type-footnote tabular-nums text-muted-foreground">{i + 1}</span>
                      <span className="min-w-0 flex-1 truncate font-mono text-[13px]" title={m}>{m}</span>
                      <Button size="icon-xs" variant="ghost" aria-label={`上移 ${m}`} disabled={i === 0} onClick={() => move(i, -1)}><ArrowUpIcon /></Button>
                      <Button size="icon-xs" variant="ghost" aria-label={`下移 ${m}`} disabled={i === backups.length - 1} onClick={() => move(i, 1)}><ArrowDownIcon /></Button>
                      <Button size="icon-xs" variant="ghost" aria-label={`移除 ${m}`} onClick={() => setList(list.filter((x) => x !== m))}><XIcon /></Button>
                    </li>))}
                  </ol>}
                  {list.length < v.max && <div className={cn(backups.length > 0 && "mt-2")}><ModelPicker harness={harness} value="" placeholder="添加备用模型…" exclude={list} testId={`add-${p.key}`} onChange={(m) => m && !list.includes(m) && setList([...list, m])} /></div>}
                </>}
              </div>
              {p.recommended && <div className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground" data-models-recommended-line>
                <span className="min-w-0 flex-1">推荐：<ChoiceLine c={p.recommended} wrap className="inline" /></span>
                {!same(next, p.recommended) && <Button size="xs" variant="outline" onClick={() => { setHarness(p.recommended!.harness); setList(p.recommended!.models); setEffort(p.recommended!.effort ?? "") }} data-models-recommended>填入推荐</Button>}
              </div>}
            </div>}
          </div>
        </>}
      </div>
      <SheetFooter className="mt-auto flex-row justify-end gap-2 border-t px-5 py-3">
        <Button size="sm" variant="ghost" onClick={onCancel}>取消</Button>
        <Button size="sm" disabled={busy || !dirty || (mode === "own" && !harness)} onClick={() => void run(next, next ? `${p.label}改好了，下一次调用就用` : `${p.label}改回跟随默认`)} data-models-save>{busy && <Spinner />}保存</Button>
      </SheetFooter>
    </div>
  )
}
const Field = ({ label, children }: { label: string; children: ReactNode }) => (
  <div className="flex flex-col gap-1.5" data-field={label}><span className="type-footnote text-muted-foreground">{label}</span>{children}</div>
)

/** On Steve's Bot page: the judgments run on this bot; what they use now, and which purposes are set apart. */
export function SteveJudges({ harness, model }: { harness: string; model: string }) {
  const [v, setV] = useState<View | null>(null)
  const b = st.bots.steve
  useEffect(() => { api.get().then(setV, () => setV(null)) }, [b?.harness, b?.model, b?.reasoningEffort])
  const changed = !!b && (harness !== b.harness || (model || null) !== (b.model ?? null))
  const own = v?.purposes.filter((p) => !p.follows) ?? []
  return (
    <div className="rounded-xl border bg-muted/40 px-3.5 py-3 type-callout" data-steve-judges>
      <p className="text-foreground">Steve 自己的判断都由 Steve 这个 Bot 来做，用它的 harness 和模型。</p>
      {v && <div className="mt-1.5 text-muted-foreground">
        {changed ? <>保存后，判断改用 <span className="font-medium text-foreground">{harnessName(harness)}</span> · <span className="font-mono text-[12.5px] text-foreground/85">{model || "默认模型"}</span></>
          : <>现在：<ChoiceLine c={{ harness: v.steve.runs_on, models: v.steve.models, effort: v.steve.effort }} className="inline" />{v.steve.model_note && `（${v.steve.model_note}）`}</>}
      </div>}
      {harness === "codex" && <div className="mt-1.5 type-footnote text-muted-foreground">Codex 通过 codex exec 做判断：回答写完后一次显示；研究在只读沙箱里进行。</div>}
      {v && <div className="mt-1.5 type-footnote text-muted-foreground">
        {own.length ? `${own.map((p) => p.label).join("、")}单独配置了，不跟随这里。` : "所有判断都跟随这里。"}
        <a href="#/settings/models" className="ml-1 text-blue hover:underline" data-goto-advanced>设置 · 模型</a>
      </div>}
    </div>
  )
}
