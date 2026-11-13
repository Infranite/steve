/* #/auto — 自动化: what needs you, then the flows, schedules and event sources (prototype AutomationView).
 * Routes: #/auto/rules, #/auto/schedules, #/auto/sources, #/auto/rule/:id, #/auto/source/:adapter */
import { useEffect, useState } from "react"
import { Trans } from "react-i18next"
import { CheckIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { Page, Row, Segmented, Empty, go, Panel, Section, Chevron, FieldRow, Tile, Pill, Notice, Disclosure } from "@/ui"
import { cn } from "cn"
import { toast, tm } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, type FlowSummary, type Load, type Schedule, type Signal, type Source } from "@/api/flow"
import { srcIcon, Failed } from "@/auto/common"
import { NewRuleDialog } from "@/auto/new-rule"
import { Schedules, NewScheduleDialog } from "@/auto/schedules"
import { RuleView } from "@/auto/rule"
import { SourceView } from "@/auto/source"
import { useT } from "@/i18n"

type Tab = "rules" | "schedules" | "sources"

/** the whole #/auto/* route (App.tsx renders this for "auto") */
export function AutoRoute({ b, c }: { b?: string; c?: string }) {
  const sub = (b ?? "").split("?")[0], id = (c ?? "").split("?")[0]
  if (sub === "rule" && id) return <RuleView key={id} id={id} />
  if (sub === "source" && id) return <SourceView key={id} adapter={id} />
  return <AutomationView tab={(["rules", "schedules", "sources"].includes(sub) ? sub : "rules") as Tab} />
}

/** event sources you can act on: IM lives in 群聊与私聊 */
const realSources = (xs: Source[] | undefined) => (xs ?? []).filter((s) => s.source !== "im")

export function AutomationView({ tab }: { tab: Tab }) {
  const flows = useLoad(() => flowApi.flows("flow"), [], ["flow"])
  const sch = useLoad(() => flowApi.schedules(), [], ["flow", "run"])
  const sig = useLoad(() => flowApi.signals({ range: "today" }), [], ["signal"])
  const src = useLoad(() => flowApi.sources(), [], ["signal"])
  const [dialog, setDialog] = useState<null | "rule" | "schedule">(null)
  const t = useT()
  if (flows.unwired) return <Unwired id="auto" />
  /* today's signals, in one line under the title: the way to 信号 and the count that matters there */
  const st = sig.data?.stats, failed = (sig.data?.items ?? []).filter((s) => s.outcome === "failed").length
  const pulse = st ? (st.total
    ? <a href="#/signals" className="hover:text-foreground hover:underline" data-auto-pulse>{[t("auto:pulse.signals", { count: st.total }), st.need ? t("auto:pulse.need", { count: st.need }) : "", failed ? t("auto:pulse.failed", { count: failed }) : ""].filter(Boolean).join(t("auto:pulse.sep"))}</a>
    : <a href="#/signals" className="hover:text-foreground hover:underline" data-auto-pulse>{t("auto:pulse.none")}</a>) : t("auto:sub")
  const fresh = flows.data && !flows.data.length && sch.data && !sch.data.length
  return (
    <Page title={t("auto:title")} sub={pulse} max={760}
      actions={tab === "rules" ? <Button size="sm" onClick={() => setDialog("rule")} data-new-rule><PlusIcon />{t("auto:newFlow")}</Button> : tab === "schedules" ? <Button size="sm" onClick={() => setDialog("schedule")} data-new-schedule><PlusIcon />{t("auto:newSchedule")}</Button> : undefined}
      toolbar={<Segmented value={tab} onChange={(v) => go("#/auto/" + v)} options={[["rules", t("auto:tab.rules")], ["schedules", t("auto:tab.schedules")], ["sources", t("auto:tab.sources")]] as const} />}>
      {dialog === "rule" && <NewRuleDialog onClose={() => setDialog(null)} />}
      {dialog === "schedule" && <NewScheduleDialog onClose={() => setDialog(null)} onSaved={() => void sch.reload()} />}
      <div className="flex flex-col gap-8">
        <Attention tab={tab} flows={flows.data} schedules={sch.data} signals={sig.data?.items} sources={src.data} />
        {tab === "rules" && (fresh
          ? <Guide accounts={realSources(src.data).reduce((n, s) => n + s.connections.length, 0)} onFlow={() => setDialog("rule")} onSchedule={() => setDialog("schedule")} />
          : <FlowList L={flows} signals={sig.data?.items} sources={src.data} onNew={() => setDialog("rule")} />)}
        {tab === "rules" && <AllowPrivate />}
        {tab === "schedules" && <Schedules L={sch} onNew={() => setDialog("schedule")} />}
        {tab === "sources" && <SourceList L={src} />}
      </div>
    </Page>
  )
}

/** what needs you in the other tabs (the open tab marks its own rows); nothing at all when all is well */
function Attention({ tab, flows, schedules, signals, sources }: { tab: Tab; flows?: FlowSummary[]; schedules?: Schedule[]; signals?: Signal[]; sources?: Source[] }) {
  const t = useT()
  const xs: { key: string; text: string; href: string }[] = []
  const failedBy = new Map<string, number>()
  for (const s of signals ?? []) if (s.outcome === "failed" && s.flow?.kind === "flow") failedBy.set(s.flow.id, (failedBy.get(s.flow.id) ?? 0) + 1)
  if (tab !== "rules") for (const f of flows ?? []) { const n = failedBy.get(f.id); if (n) xs.push({ key: "f" + f.id, text: t("auto:attn.flowFailed", { name: f.name, count: n }), href: "#/signals" }) }
  if (tab !== "schedules") for (const s of schedules ?? []) if (s.enabled && s.last?.status === "failed") xs.push({ key: "s" + s.id, text: t("auto:attn.scheduleFailed", { name: s.name }), href: "#/auto/schedules" })
  if (tab !== "sources") for (const s of realSources(sources)) {
    const bad = s.connections.filter((c) => c.status !== "ok").length
    if (bad) xs.push({ key: "a" + s.adapter, text: t("auto:attn.reauth", { name: s.name, count: bad }), href: "#/auto/source/" + s.adapter })
  }
  if (!xs.length) return null
  return (
    <Notice tone="attention" data-auto-attention>
      <ul className="flex flex-col gap-1">{xs.map((x) => <li key={x.key}><a href={x.href} className="hover:underline" data-attention={x.key}>{x.text}</a></li>)}</ul>
    </Notice>
  )
}

/** nothing set up yet: the three steps, the first one done once an account is connected */
function Guide({ accounts, onFlow, onSchedule }: { accounts: number; onFlow: () => void; onSchedule: () => void }) {
  const t = useT()
  const steps = [
    { id: "accounts", done: accounts > 0, title: t("auto:guide.accounts"), sub: accounts ? t("auto:guide.accountsDone", { count: accounts }) : t("auto:guide.accountsSub"), act: accounts ? t("auto:guide.view") : t("auto:guide.connect"), on: () => go("#/accounts") },
    { id: "flow", done: false, title: t("auto:guide.flow"), sub: t("auto:guide.flowSub"), act: t("auto:guide.create"), on: onFlow },
    { id: "schedule", done: false, title: t("auto:guide.schedule"), sub: t("auto:guide.scheduleSub"), act: t("auto:guide.create"), on: onSchedule },
  ]
  return (
    <Section label={t("auto:guide.title")} note={t("auto:guide.note")} data-auto-guide>
      <Panel>{steps.map((x, n) => (
        <button key={x.id} type="button" onClick={x.on} data-auto-step={x.id} data-done={x.done || undefined}
          className="group/step flex w-full min-w-0 items-center gap-3 rounded-lg px-3 py-3 text-left transition-colors duration-120 ease-(--ease-out) hover:bg-accent">
          <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-full type-footnote tabular-nums", x.done ? "bg-success/12 text-success" : "bg-muted text-muted-foreground")} aria-hidden>
            {x.done ? <CheckIcon className="size-3.5" strokeWidth={2.5} /> : n + 1}</span>
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className={cn("type-body", x.done && "text-muted-foreground")}>{x.title}</span>
            <span className="min-w-0 type-footnote text-muted-foreground">{x.sub}</span>
          </span>
          <span className="shrink-0 type-footnote text-muted-foreground transition-colors group-hover/step:text-foreground">{x.act}</span>
        </button>
      ))}</Panel>
    </Section>
  )
}

function FlowList({ L, signals, sources, onNew }: { L: Load<FlowSummary[]>; signals?: Signal[]; sources?: Source[]; onNew: () => void }) {
  const [busy, setBusy] = useState<string | null>(null)
  const t = useT()
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  if (!L.data.length) return <Empty title={t("auto:flows.emptyTitle")} sub={t("auto:flows.emptySub")} action={<Button variant="outline" onClick={onNew}><PlusIcon />{t("auto:newFlow")}</Button>} />
  const toggle = async (f: FlowSummary, v: boolean) => {
    setBusy(f.id)
    try { await flowApi.patchFlow(f.id, { enabled: v }); toast(v ? t("auto:flows.enabledToast") : t("auto:flows.disabledToast")); await L.reload() } catch (e) { toast(errText(e)) } finally { setBusy(null) }
  }
  /* an event type's words, from the sources that produce it (“检查失败”, not github.check_failed) */
  const typeLabel = new Map((sources ?? []).flatMap((s) => s.types.map((y) => [y.key, y.label] as const)))
  const trigger = (f: FlowSummary) => {
    if (!f.trigger?.source) return t("auto:flows.noTrigger")
    const ys = (f.trigger.types ?? []).map((k) => typeLabel.get(k) ?? k)
    return ys.length ? t("auto:flows.trigger", { source: sourceLabel(f.trigger.source), types: ys.join(t("auto:sep.list")) }) : sourceLabel(f.trigger.source)
  }
  const failed = (id: string) => (signals ?? []).filter((s) => s.outcome === "failed" && s.flow?.id === id).length
  /* on ones first, then the most recently run */
  const xs = [...L.data].sort((a, b) => Number(b.enabled) - Number(a.enabled) || (b.last_run ?? 0) - (a.last_run ?? 0))
  return (
    <Panel data-flow-list>{xs.map((f) => {
      const bad = failed(f.id)
      return (
        <div key={f.id} className="flex items-center gap-3 rounded-lg px-3 py-3 transition-colors hover:bg-accent anim-in">
          <Tile I={srcIcon(f.trigger?.source)} className={cn(!f.enabled && "opacity-60")} />
          <button type="button" className="min-w-0 flex-1 text-left" onClick={() => go("#/auto/rule/" + f.id)} data-rule-row={f.id}>
            <div className={cn("truncate type-body font-medium", !f.enabled && "text-muted-foreground")}>{f.name}</div>
            <div className="mt-0.5 truncate type-footnote text-muted-foreground">{f.description || trigger(f)}</div>
            {/* on a phone the failures go under the description: a pill beside it leaves the description a few characters */}
            {bad > 0 && <div className="mt-0.5 truncate type-footnote text-destructive sm:hidden" data-flow-failed-line>{t("auto:flows.failedToday", { count: bad })}</div>}
          </button>
          {bad > 0 ? <Pill tone="destructive" className="hidden sm:inline-flex" data-flow-failed>{t("auto:flows.failedToday", { count: bad })}</Pill>
            : <span className="hidden shrink-0 type-footnote tabular-nums text-muted-foreground sm:inline" title={t("auto:flows.runs24", { count: f.runs24 })}>{!f.enabled ? t("auto:disabled") : f.last_run ? t("auto:rule.lastRun", { time: tm(f.last_run) }) : t("auto:flows.neverRan")}</span>}
          <Switch checked={f.enabled} disabled={busy === f.id} onCheckedChange={(v) => void toggle(f, v)} aria-label={t("auto:enableName", { name: f.name })} />
        </div>
      )
    })}</Panel>
  )
}

function SourceList({ L }: { L: Load<Source[]> }) {
  const t = useT()
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  /* connected ones first: a source without an account has nothing to show yet */
  const xs = realSources(L.data).sort((a, b) => Number(b.connections.length > 0) - Number(a.connections.length > 0))
  return (
    <Section note={<Trans t={t} i18nKey="auto:sources.footer" components={{ accounts: <a href="#/accounts" className="text-blue hover:underline" />, channels: <a href="#/channels" className="text-blue hover:underline" /> }} />}>
      {xs.length ? <Panel>{xs.map((s) => {
        const bad = s.connections.filter((c) => c.status !== "ok").length, n24 = s.types.filter((x) => x.on).reduce((n, x) => n + x.n24, 0)
        const on = s.connections.length > 0
        return (
          <Row key={s.adapter} icon={<Tile I={srcIcon(s.source)} className={cn(!on && "opacity-60")} />} onClick={() => go("#/auto/source/" + s.adapter)} data-source-row={s.adapter}
            title={<span className={cn(!on && "text-muted-foreground")}>{s.name}</span>}
            sub={on ? [t("auto:sources.accounts", { count: s.connections.length }), t("auto:sources.events24", { count: n24 })].join(" · ") : t("auto:sources.notConnected")}>
            {bad ? <Pill tone="attention">{t("auto:sources.needAttention", { count: bad })}</Pill> : s.paused && on ? <Pill>{t("auto:sources.paused")}</Pill> : null}
            <Chevron />
          </Row>) })}</Panel>
        : <Empty title={t("auto:sources.emptyTitle")} sub={t("auto:sources.emptySub")} />}
    </Section>
  )
}

/* the hosts http steps may reach on internal addresses (http_allow_private): only you write it, here. A setting
 * few flows need, so it stays folded under the list with its current value in the line */
function AllowPrivate() {
  const t = useT()
  const [v, setV] = useState<string | null>(null), [saved, setSaved] = useState<string[]>([]), [busy, setBusy] = useState(false)
  useEffect(() => { flowApi.allowPrivate().then((x) => { setV(x.hosts.join(", ")); setSaved(x.hosts) }, () => setV(null)) }, [])
  if (v === null) return null
  const save = async () => {
    setBusy(true)
    try { const x = await flowApi.setAllowPrivate(v.split(/[\s,\uff0c]+/).filter(Boolean)); setV(x.hosts.join(", ")); setSaved(x.hosts); toast(t("auto:allowPrivate.saved")) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  return (
    <Disclosure data-allow-private label={<>{t("auto:allowPrivate.title")}<span className="text-tertiary">{" · " + (saved.length ? saved.join(t("auto:sep.list")) : t("auto:allowPrivate.none"))}</span></>} triggerClassName="type-footnote">
      <div className="mt-2 flex flex-col gap-2">
        <Panel><FieldRow onSubmit={(e) => { e.preventDefault(); void save() }}><Input value={v} placeholder={t("auto:allowPrivate.placeholder")} onChange={(e) => setV(e.target.value)} aria-label={t("auto:allowPrivate.title")} />
          <Button size="sm" variant="outline" type="submit" disabled={busy} data-allow-private-save>{t("auto:allowPrivate.save")}</Button></FieldRow></Panel>
        <div className="px-1 type-footnote text-muted-foreground">{t("auto:allowPrivate.sub")}</div>
      </div>
    </Disclosure>
  )
}
