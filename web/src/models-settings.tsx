/* 设置 · 模型：Steve 是一个 Bot。它自己的判断（分流、对话回答、研究、起标题……）都用 Steve 这个 Bot 的 harness、模型和
 * 推理强度，在 Bot 页改。「高级」里可以给某一类判断单独指定 harness 和模型（默认收起，每行都写明现在用的是什么、最近实际用了什么）。
 * 最下面是新会话在每个 harness 上的默认模型。 */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ArrowDownIcon, ArrowUpIcon, ChevronDownIcon, ChevronRightIcon, CpuIcon, TriangleAlertIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { modReq, isNotWired } from "@/api"
import { st, toast, useStore } from "@/store"
import { Group, Row, SectionLabel, Segmented } from "@/ui"
import { Select } from "@/views"
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
/** `Claude Code · claude-haiku-5-5 → claude-sonnet-5-5` */
export function ChoiceLine({ c, className, wrap }: { c: Choice; className?: string; wrap?: boolean }) {
  return (
    <span className={cn("block min-w-0", wrap ? "break-words" : "truncate", className)} data-choice>
      <span className="font-medium text-foreground">{harnessName(c.harness)}</span>
      <span className="text-muted-foreground"> · </span>
      <span className="font-mono text-[12.5px] text-foreground/85">{c.models.length ? c.models.join(" → ") : "默认模型"}</span>
      {c.effort && <span className="text-muted-foreground"> · 推理{EFFORT[c.effort] ?? c.effort}</span>}
    </span>
  )
}
const same = (a: Choice | null, b: Choice | null) => !!a && !!b && a.harness === b.harness && a.models.join() === b.models.join() && (a.effort ?? null) === (b.effort ?? null)
const ago = (t: number) => { const m = Math.round((Date.now() - t) / 60000); return m < 60 ? `${Math.max(m, 1)} 分钟前` : m < 1440 ? `${Math.round(m / 60)} 小时前` : `${Math.round(m / 1440)} 天前` }

/** what a purpose actually ran on this past week */
function RecentLine({ r }: { r: Recent | null }) {
  if (!r?.used.length) return <span className="block type-footnote font-normal text-tertiary" data-recent="none">最近 7 天没有调用</span>
  return (
    <span className="block min-w-0 break-words type-footnote font-normal text-muted-foreground" data-recent title={r.used.map((u) => `${harnessName(u.harness)} · ${u.model} ×${u.calls}${u.failed ? `（失败 ${u.failed}）` : ""}`).join("，")}>
      最近 7 天实际用：{r.used.slice(0, 2).map((u, i) => <span key={i}>{i > 0 && "，"}{harnessName(u.harness)} · <span className="font-mono">{short(u.model)}</span> ×{u.calls}{u.failed ? <span className="text-attention">（失败 {u.failed}）</span> : null}</span>)}
      {r.used.length > 2 && ` 等 ${r.used.length} 种`}{r.last && ` · ${ago(r.last.at)}`}
    </span>
  )
}

export function ModelsSettingsPane() {
  useStore()
  const [v, setV] = useState<View | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [adv, setAdv] = useState(() => location.hash.includes("advanced"))
  const [open, setOpen] = useState<string | null>(null)
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
  const steveChoice: Choice = { harness: s.runs_on, models: s.models, effort: s.effort }
  return (
    <div className="flex flex-col gap-10" data-models-settings>
      <section data-steve-runtime>
        <SectionLabel>Steve 自己的判断</SectionLabel>
        <div className="rounded-xl border bg-card px-4 py-3.5">
          <div className="flex items-start gap-3">
            <Robot k={s.bot_id ?? "steve"} size={40} className="mt-0.5 shrink-0" />
            <div className="min-w-0 flex-1">
              <p className="type-body text-foreground" data-steve-says>Steve 自己的判断都由 Steve 这个 Bot 来做，用它的 harness 和模型。</p>
              <p className="mt-1 type-callout text-muted-foreground" data-steve-now>判断现在用：<span className="font-medium text-foreground">{harnessName(s.runs_on)}</span> · <span className="font-mono text-[12.5px] text-foreground">{s.models[0] ?? "默认模型"}</span>{s.model_note && `（${s.model_note}）`}</p>
              <div className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 type-callout">
                <span className="text-muted-foreground">Harness</span><span className="font-medium" data-steve-harness>{harnessName(s.runs_on)}</span>
                <span className="text-muted-foreground">模型</span>
                <span className="min-w-0" data-steve-model>
                  <span className="block break-words font-mono text-[12.5px]">{s.models.length ? s.models.join(" → ") : `${harnessName(s.runs_on)} 默认模型`}</span>
                  {s.model_note && <span className="block type-footnote text-muted-foreground" data-model-note>{s.model_note}{s.harness === "codex" && !s.model ? "；要固定模型，在 Bot 页选一个" : ""}</span>}
                  {s.models.length > 1 && <span className="block type-footnote text-muted-foreground">前一个出错或超时才换下一个</span>}
                </span>
                <span className="text-muted-foreground">推理强度</span><span>{s.effort ? EFFORT[s.effort] ?? s.effort : "跟随模型默认"}</span>
              </div>
              {s.note && <div className="mt-2 flex items-start gap-1.5 type-footnote text-attention" role="note"><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" />{s.note}</div>}
            </div>
          </div>
          <div className="mt-3 flex flex-wrap items-center justify-between gap-2 border-t pt-3">
            <span className="type-footnote text-muted-foreground">在 Steve 的 Bot 页改 harness、模型和推理强度，跟其他 Bot 一样。</span>
            <Button size="sm" variant="outline" onClick={() => editBot(s.bot_id ?? "steve")} data-edit-steve>在 Bot 页修改<ChevronRightIcon /></Button>
          </div>
        </div>
      </section>

      <section data-models-advanced={adv ? "open" : "closed"}>
        <button type="button" className="mb-2 flex w-full items-center justify-between gap-2 rounded-lg py-1 text-left" onClick={() => { setAdv(!adv); setOpen(null) }} aria-expanded={adv} data-advanced-toggle>
          <span className="flex min-w-0 flex-col">
            <span className="type-label text-muted-foreground">高级 · 按用途单独指定</span>
            <span className="type-footnote text-muted-foreground" data-advanced-summary>
              {own.length ? `${v.purposes.length - own.length} 类跟随 Steve，${own.length} 类单独指定（${own.map((p) => p.label).join("、")}）` : `${v.purposes.length} 类判断都跟随 Steve`}
            </span>
          </span>
          <ChevronDownIcon className={cn("size-4 shrink-0 text-tertiary transition-transform", adv && "rotate-180")} />
        </button>
        {adv && <>
          <p className="mb-3 type-callout text-muted-foreground">没单独指定的跟随 Steve（{harnessName(s.runs_on)} · <span className="font-mono text-[12.5px]">{s.models[0] ?? "默认模型"}</span>）。单独指定的可以换 harness、按顺序回退几个模型、设推理强度；改了下一次调用就生效。</p>
          <Group>{v.purposes.map((p) => {
            const on = open === p.key
            const rec = !p.follows && same(p.value, p.recommended)
            return (
              <div key={p.key} data-purpose={p.key}>
                <Row title={<span className="block min-w-0">
                  <span className="flex items-center gap-2">{p.label}
                    {p.follows ? <span className="rounded bg-muted px-1.5 py-px text-[11px] text-muted-foreground" data-follows>跟随 Steve</span>
                      : <span className="rounded bg-blue/10 px-1.5 py-px text-[11px] font-medium text-blue" data-custom>{rec ? "单独指定 · 推荐" : "单独指定"}</span>}
                  </span>
                  <span className="block type-footnote font-normal text-muted-foreground">{p.desc}</span>
                  <ChoiceLine c={p.effective} wrap className="mt-1 type-callout" />
                  <RecentLine r={p.recent} />
                </span>}
                  onClick={() => setOpen(on ? null : p.key)} aria-expanded={on}>
                  <ChevronDownIcon className={cn("size-4 text-tertiary transition-transform", on && "rotate-180")} />
                </Row>
                {on && <Editor p={p} v={v} steve={steveChoice} onSave={async (c, ok) => { if (await save({ purposes: { [p.key]: c } }, ok)) setOpen(null) }} onCancel={() => setOpen(null)} />}
              </div>
            )
          })}</Group>
        </>}
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
    </div>
  )
}

function Editor({ p, v, steve, onSave, onCancel }: { p: Purpose; v: View; steve: Choice; onSave: (c: Choice | null, ok: string) => void; onCancel: () => void }) {
  const [mode, setMode] = useState<"follow" | "own">(p.follows ? "follow" : "own")
  const [harness, setHarness] = useState(p.value?.harness ?? p.effective.harness)
  const [list, setList] = useState<string[]>(p.value?.models.length ? p.value.models : p.effective.models)
  const [effort, setEffort] = useState<string>(p.value?.effort ?? "")
  const [busy, setBusy] = useState(false)
  const h = v.harnesses.find((x) => x.id === harness)
  const switchHarness = (x: string) => {
    setHarness(x); setEffort("")
    setList(x === steve.harness ? steve.models : v.harnesses.find((y) => y.id === x)?.default_models ?? [])
  }
  const move = (i: number, d: number) => { const n = [...list]; const [x] = n.splice(i, 1); n.splice(i + d, 0, x); setList(n) }
  const next: Choice | null = mode === "follow" ? null : { harness, models: list, effort: effort || null }
  const dirty = mode === "follow" ? !p.follows : !same(next, p.value)
  const run = async (c: Choice | null, ok: string) => { setBusy(true); await onSave(c, ok); setBusy(false) }
  return (
    <div className="mx-2 mb-3 mt-1 flex flex-col gap-4 rounded-xl border bg-card px-3.5 py-3.5" data-models-editor={p.key}>
      <Segmented size="sm" value={mode} onChange={setMode} options={[["follow", "跟随 Steve"], ["own", "单独指定"]] as const} />
      {mode === "follow" ? (
        <div className="type-callout text-muted-foreground">用 Steve 的：<ChoiceLine c={steve} className="inline" /></div>
      ) : <>
        <div className="grid gap-3 sm:grid-cols-[140px_minmax(0,1fr)] sm:items-center">
          <span className="type-footnote text-muted-foreground">Harness</span>
          <Select value={harness} testId={`harness-${p.key}`} onChange={switchHarness} options={v.harnesses.map((x) => ({ v: x.id, l: x.name }))} />
        </div>
        {harness === "codex" && p.codex_note && <div className="flex items-start gap-1.5 type-footnote text-muted-foreground" data-codex-note><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" />{p.codex_note}</div>}
        <div>
          <div className="mb-1.5 type-footnote text-muted-foreground">模型（按顺序，前一个出错或超时才换下一个，最多 {v.max} 个）</div>
          {list.length > 0 ? <ol className="flex flex-col gap-1">{list.map((m, i) => (
            <li key={m} className="flex items-center gap-2 rounded-lg bg-muted/60 py-1 pl-2.5 pr-1" data-model-item={m}>
              <span className="w-4 shrink-0 text-right type-footnote tabular-nums text-muted-foreground">{i + 1}</span>
              <span className="min-w-0 flex-1 truncate font-mono text-[13px]" title={m}>{m}</span>
              <Button size="icon-xs" variant="ghost" aria-label={`上移 ${m}`} disabled={i === 0} onClick={() => move(i, -1)}><ArrowUpIcon /></Button>
              <Button size="icon-xs" variant="ghost" aria-label={`下移 ${m}`} disabled={i === list.length - 1} onClick={() => move(i, 1)}><ArrowDownIcon /></Button>
              <Button size="icon-xs" variant="ghost" aria-label={`移除 ${m}`} onClick={() => setList(list.filter((x) => x !== m))}><XIcon /></Button>
            </li>))}
          </ol> : <div className="rounded-lg bg-muted/60 px-2.5 py-1.5 type-callout text-muted-foreground">{harnessName(harness)} 的默认模型</div>}
          {list.length < v.max && <div className="mt-2 max-w-sm"><ModelPicker harness={harness} value="" placeholder="添加模型…" exclude={list} testId={`add-${p.key}`} onChange={(m) => m && !list.includes(m) && setList([...list, m])} /></div>}
        </div>
        <div className="grid gap-3 sm:grid-cols-[140px_minmax(0,1fr)] sm:items-center">
          <span className="type-footnote text-muted-foreground">推理强度</span>
          <Select value={effort || "default"} testId={`effort-${p.key}`} onChange={(x) => setEffort(x === "default" ? "" : x)} options={[{ v: "default", l: harnessName(harness) === harnessName(steve.harness) && steve.effort ? `跟随 Steve（${EFFORT[steve.effort] ?? steve.effort}）` : "跟随模型默认" }, ...(h?.efforts ?? []).map((e) => ({ v: e.key, l: e.label }))]} />
        </div>
      </>}
      <div className="flex flex-wrap items-center gap-2 border-t pt-3">
        <span className="min-w-0 flex-1 type-footnote text-muted-foreground">{p.recommended && <>推荐：<ChoiceLine c={p.recommended} className="inline" /></>}</span>
        {p.recommended && !same(p.value, p.recommended) && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void run(p.recommended, `${p.label}改用推荐`)} data-models-recommended>用推荐</Button>}
        <Button size="sm" variant="ghost" onClick={onCancel}>取消</Button>
        <Button size="sm" disabled={busy || !dirty || (mode === "own" && !harness)} onClick={() => void run(next, next ? `${p.label}改好了，下一次就用` : `${p.label}跟随 Steve`)} data-models-save>{busy && <Spinner />}保存</Button>
      </div>
    </div>
  )
}

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
        {own.length ? `${own.map((p) => p.label).join("、")}单独指定了，不跟随这里。` : "所有判断都跟随这里。"}
        <a href="#/settings/models/advanced" className="ml-1 text-blue hover:underline" data-goto-advanced>设置 · 模型 · 高级</a>
      </div>}
    </div>
  )
}
