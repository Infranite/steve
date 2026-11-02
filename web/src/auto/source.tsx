/* #/auto/source/:adapter — one event source: pause, who triages, accounts, scopes, event types, flows, recent events
 * (signals-automation.md §9, prototype SourceView) */
import { useState } from "react"
import { ChevronRightIcon, PlusIcon, RefreshCwIcon, WorkflowIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import { Page, Group, Row, SectionLabel, Empty, StatusDot, BotAvatar, go } from "@/ui"
import { tm, toast, botName } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, type ScopeMode, type SourcePatch } from "@/api/flow"
import { Ico, Pick, Loading, Failed, botOptions, mainBot } from "@/auto/common"
import { NewRuleDialog } from "@/auto/new-rule"

const BACK: [string, string] = ["#/auto/sources", "自动化"]
const MODES: { v: ScopeMode; l: string }[] = [{ v: "receive", l: "接收" }, { v: "record", l: "只记录" }, { v: "ignore", l: "不接收" }]

export function SourceView({ adapter }: { adapter: string }) {
  const L = useLoad(() => flowApi.sources(), [], ["signal"])
  const flows = useLoad(() => flowApi.flows("flow"), [], ["flow"])
  const s = L.data?.find((x) => x.adapter === adapter)
  const recent = useLoad(() => (s ? flowApi.signals({ range: "7d", source: s.source }) : Promise.resolve(null)), [s?.source], ["signal"])
  const [busy, setBusy] = useState(false), [newRule, setNewRule] = useState(false)
  if (L.unwired) return <Unwired id="auto" />
  if (L.error && !L.data) return <Failed title="事件源" back={BACK} msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return <Loading title="事件源" back={BACK} />
  if (adapter === "im" || s?.source === "im") return (
    <Page title="IM" back={BACK}><Empty title="IM 已经移到“渠道”" sub="各个渠道的账号、群、身份和响应规则都在渠道里管理。" /><div className="flex justify-center"><Button onClick={() => go("#/channels")}>去渠道</Button></div></Page>
  )
  if (!s) return <Page title="事件源" back={BACK}><Empty title="没有这个事件源" sub="可能是它的插件已经停用。" /></Page>
  const patch = async (p: SourcePatch, ok?: string) => {
    setBusy(true)
    try { const v = await flowApi.patchSource(s.adapter, p); L.set(L.data!.map((x) => (x.adapter === v.adapter ? v : x))); if (ok) toast(ok) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const refresh = async () => { setBusy(true); try { const v = await flowApi.refreshScopes(s.adapter); L.set(L.data!.map((x) => (x.adapter === v.adapter ? v : x))); toast("已刷新") } catch (e) { toast(errText(e)) } finally { setBusy(false) } }
  const mine = (flows.data ?? []).filter((f) => f.trigger?.source === s.source)
  const triage = s.triage_bot ?? mainBot()
  const connName = (id: string) => s.connections.find((c) => c.id === id)?.name ?? ""
  return (
    <Page title={s.name} back={BACK} max={680} sub={s.paused ? "已暂停：期间的事件不接收，恢复后不补收" : `${s.connections.length} 个账号 · 收到的事件按流程处理`}
      actions={<><span className="hidden type-footnote text-muted-foreground sm:inline">{s.paused ? "已暂停" : "接收中"}</span>
        <Switch checked={!s.paused} disabled={busy || !s.connections.length} onCheckedChange={(v) => void patch({ paused: !v }, v ? "已恢复接收" : "已暂停，期间的事件不接收，恢复后不补收")} aria-label="接收事件" data-source-on /></>}>
      {newRule && <NewRuleDialog source={s.source} onClose={() => setNewRule(false)} />}
      <div className="flex flex-col gap-8">
        <section><SectionLabel>先交给谁判断</SectionLabel>
          <Group><Row icon={<BotAvatar k={triage} size={28} />} title={botName(triage)} sub="收到事件后，先由它按流程判断；没有流程的事件只记录">
            <Pick label="先交给谁判断" testId="src-bot" value={s.triage_bot ?? "_"} disabled={busy || !s.connections.length} options={[{ v: "_", l: "主 Bot（" + botName(mainBot()) + "）" }, ...botOptions()]} onChange={(v) => void patch({ triage_bot: v === "_" ? null : v }, "已改为交给" + botName(v === "_" ? mainBot() : v))} /></Row></Group>
        </section>
        <section><SectionLabel>账号</SectionLabel>
          <Group>
            {s.connections.map((c) => <Row key={c.id} title={c.name} sub={c.last_sync ? "同步于 " + tm(c.last_sync) : "还没有同步"} data-source-conn={c.id}>
              <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={c.status === "ok" ? "done" : "wait"} />{c.status === "ok" ? "正常" : "要重新授权"}</span></Row>)}
            <Row icon={<Ico I={PlusIcon} />} title={<span className="text-blue">添加账号</span>} sub="在账号里授权，授权后自动出现在这里" onClick={() => go("#/accounts")} data-add-account />
          </Group>
        </section>
        {s.connections.length > 0 && <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void refresh()}><RefreshCwIcon />刷新</Button>}>接收范围</SectionLabel>
          {s.scopes.length ? <Group>{s.scopes.map((x) => (
            <Row key={x.connection_id + x.ext_id} title={x.name || x.ext_id} sub={[s.connections.length > 1 ? connName(x.connection_id) : "", x.mode === "record" ? "进信号页，不触发流程" : x.mode === "ignore" ? "不接收" : ""].filter(Boolean).join(" · ") || undefined} data-scope={x.ext_id}>
              <Pick label={"接收范围：" + (x.name || x.ext_id)} testId={x.ext_id} value={x.mode} options={MODES} disabled={busy} onChange={(v) => void patch({ scopes: [{ connection_id: x.connection_id, ext_id: x.ext_id, mode: v }] })} /></Row>))}</Group>
            : <div className="rounded-xl border border-dashed px-4 py-6 text-center type-callout text-muted-foreground">这个事件源没有可选的范围，全部接收</div>}
        </section>}
        <section><SectionLabel>产生的事件</SectionLabel>
          <Group>{s.types.map((t) => <Row key={t.key} title={t.label} sub={`${t.desc ? t.desc + " · " : ""}24 小时 ${t.n24} 条 · ${t.flows ? t.flows + " 条流程在用" : "没有流程，只记录"}`} data-event-type={t.key}>
            {t.param !== null && <ParamInput v={t.param} label={t.label + " 的参数"} disabled={busy || !s.connections.length} onSave={(p) => void patch({ types: { [t.key]: { param: p } } }, "已保存")} />}
            <Switch checked={t.on} disabled={busy || !s.connections.length} onCheckedChange={(v) => void patch({ types: { [t.key]: { on: v } } }, v ? `已开始接收「${t.label}」` : `不再接收「${t.label}」`)} aria-label={t.label} data-event-toggle={t.key} /></Row>)}</Group>
          {!s.connections.length && <div className="mt-2 px-1 type-footnote text-muted-foreground">先添加账号，才能改接收设置。</div>}
        </section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewRule(true)}><PlusIcon />新建流程</Button>}>收到之后怎么处理</SectionLabel>
          {mine.length ? <Group>{mine.map((f) => <Row key={f.id} icon={<Ico I={WorkflowIcon} />} title={f.name} sub={f.description || f.origin_label} onClick={() => go("#/auto/rule/" + f.id)}><span className="type-footnote text-muted-foreground">{f.enabled ? "启用" : "停用"}</span><ChevronRightIcon className="size-4 text-tertiary" aria-hidden /></Row>)}</Group>
            : <div className="rounded-xl border border-dashed px-4 py-6 text-center type-callout text-muted-foreground">还没有流程，事件只记录在信号页</div>}
        </section>
        <section><SectionLabel right={<a href="#/signals" className="font-normal hover:text-foreground">看全部信号</a>}>最近事件</SectionLabel>
          {recent.data?.items.length ? <Group>{recent.data.items.slice(0, 6).map((e) => <Row key={e.id} title={e.summary} sub={<>{e.outcome_label || "处理中"}{e.flow && <> · 流程「{e.flow.name}」</>}</>} onClick={() => go("#/signals?e=" + e.id)}><span className="type-footnote tabular-nums text-muted-foreground">{tm(e.received)}</span></Row>)}</Group>
            : <div className="px-1 type-callout text-muted-foreground">{recent.data ? "7 天里没有事件" : "载入中…"}</div>}
        </section>
      </div>
    </Page>
  )
}

/** a type's parameter (会前提醒的提前量 “30m”): saved on blur or Enter */
function ParamInput({ v, label, disabled, onSave }: { v: string; label: string; disabled?: boolean; onSave: (v: string) => void }) {
  const [x, setX] = useState(v)
  const save = () => { if (x.trim() && x.trim() !== v) onSave(x.trim()) }
  return <Input aria-label={label} className="h-7 w-20 rounded-lg text-[12.5px]" value={x} disabled={disabled} onChange={(e) => setX(e.target.value)} onBlur={save} onKeyDown={(e) => e.key === "Enter" && save()} />
}

