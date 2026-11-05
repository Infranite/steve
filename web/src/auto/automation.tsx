/* #/auto — 自动化: the chain (accounts → flows → signals → schedules), the flow list, schedules and event sources
 * (prototype AutomationView). Routes: #/auto/rules, #/auto/schedules, #/auto/sources, #/auto/rule/:id, #/auto/source/:adapter */
import { useState } from "react"
import { Trans } from "react-i18next"
import { ArrowRightIcon, ChevronRightIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Page, Group, Row, Segmented, Meta, Empty, StatusDot, go } from "@/ui"
import { toast } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, sourceLabel, type FlowSummary, type Load } from "@/api/flow"
import { Ico, srcIcon, Failed } from "@/auto/common"
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

export function AutomationView({ tab }: { tab: Tab }) {
  const flows = useLoad(() => flowApi.flows("flow"), [], ["flow"])
  const sch = useLoad(() => flowApi.schedules(), [], ["flow", "run"])
  const sig = useLoad(() => flowApi.signals({ range: "today" }).then((x) => x.stats.total), [], ["signal"])
  const conns = useLoad(() => flowApi.connections().then((x) => x.length).catch(() => null), [])
  const [dialog, setDialog] = useState<null | "rule" | "schedule">(null)
  const t = useT()
  if (flows.unwired) return <Unwired id="auto" />
  const n = (v: number | null | undefined) => (v === undefined || v === null ? "–" : String(v))
  const c = (v: number | null | undefined) => ({ count: v ?? 0, n: n(v) })
  const chain: [string, string, string][] = [
    [t("auto:chain.accounts", c(conns.data)), t("auto:chain.accountsSub"), "#/accounts"],
    [t("auto:chain.flows", c(flows.data?.filter((f) => f.enabled).length)), t("auto:chain.flowsSub"), "#/auto/rules"],
    [t("auto:chain.signals", c(sig.data)), t("auto:chain.signalsSub"), "#/signals"],
    [t("auto:chain.schedules", c(sch.data?.filter((s) => s.enabled).length)), t("auto:chain.schedulesSub"), "#/auto/schedules"],
  ]
  return (
    <Page title={t("auto:title")} sub={t("auto:sub")} max={760}
      actions={tab === "rules" ? <Button size="sm" onClick={() => setDialog("rule")} data-new-rule><PlusIcon />{t("auto:newFlow")}</Button> : tab === "schedules" ? <Button size="sm" onClick={() => setDialog("schedule")} data-new-schedule><PlusIcon />{t("auto:newSchedule")}</Button> : undefined}
      toolbar={<Segmented value={tab} onChange={(v) => go("#/auto/" + v)} options={[["rules", t("auto:tab.rules")], ["schedules", t("auto:tab.schedules")], ["sources", t("auto:tab.sources")]] as const} />}>
      {dialog === "rule" && <NewRuleDialog onClose={() => setDialog(null)} />}
      {dialog === "schedule" && <NewScheduleDialog onClose={() => setDialog(null)} onSaved={() => void sch.reload()} />}
      <nav aria-label={t("auto:chain.aria")} className="mb-8 grid grid-cols-2 overflow-hidden rounded-xl border bg-card sm:flex sm:items-stretch" data-auto-chain>
        {chain.map(([a, b, h], i) => (
          <a key={h} href={h} className="relative flex min-w-0 flex-1 flex-col px-4 py-3 text-left transition-colors hover:bg-accent max-sm:border-border max-sm:[&:nth-child(even)]:border-l max-sm:[&:nth-child(-n+2)]:border-b">
            {i > 0 && <ArrowRightIcon className="absolute -left-2 top-1/2 hidden size-4 -translate-y-1/2 rounded-full bg-card text-tertiary sm:block" aria-hidden />}
            <span className="truncate type-headline">{a}</span><span className="truncate type-footnote text-muted-foreground">{b}</span>
          </a>
        ))}
      </nav>
      {tab === "rules" && <FlowList L={flows} onNew={() => setDialog("rule")} />}
      {tab === "schedules" && <Schedules L={sch} onNew={() => setDialog("schedule")} />}
      {tab === "sources" && <SourceList />}
    </Page>
  )
}

function FlowList({ L, onNew }: { L: Load<FlowSummary[]>; onNew: () => void }) {
  const [busy, setBusy] = useState<string | null>(null)
  const t = useT()
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  if (!L.data.length) return <><Empty title={t("auto:flows.emptyTitle")} sub={t("auto:flows.emptySub")} /><div className="flex justify-center"><Button variant="outline" onClick={onNew}><PlusIcon />{t("auto:newFlow")}</Button></div></>
  const toggle = async (f: FlowSummary, v: boolean) => {
    setBusy(f.id)
    try { await flowApi.patchFlow(f.id, { enabled: v }); toast(v ? t("auto:flows.enabledToast") : t("auto:flows.disabledToast")); await L.reload() } catch (e) { toast(errText(e)) } finally { setBusy(null) }
  }
  return (
    <Group>{L.data.map((f) => (
      <div key={f.id} className="flex items-center gap-3 px-2 py-3">
        <Ico I={srcIcon(f.trigger?.source)} />
        <button type="button" className="min-w-0 flex-1 text-left" onClick={() => go("#/auto/rule/" + f.id)} data-rule-row={f.id}>
          <div className="truncate type-body">{f.name}</div>
          <Meta className="mt-0.5"><span>{sourceLabel(f.trigger?.source) || t("auto:flows.noTrigger")}{f.trigger?.types?.length ? " · " + f.trigger.types.join(t("auto:sep.list")) : ""}</span><span>{f.uses_ai ? t("auto:usesAI") : t("auto:noAI")}</span><span>{t("auto:flows.outcomes", { count: f.outcomes })}</span><span>{t("auto:flows.runs24", { count: f.runs24 })}</span>{f.origin_label && <span>{f.origin_label}</span>}</Meta>
        </button>
        <Switch checked={f.enabled} disabled={busy === f.id} onCheckedChange={(v) => void toggle(f, v)} aria-label={t("auto:enableName", { name: f.name })} />
      </div>
    ))}</Group>
  )
}

function SourceList() {
  const L = useLoad(() => flowApi.sources(), [], ["signal"])
  const t = useT()
  if (L.error && !L.data) return <Failed title="" msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return null
  const xs = L.data.filter((s) => s.source !== "im")
  return (<>
    {xs.length ? <Group>{xs.map((s) => {
      const bad = s.connections.filter((c) => c.status !== "ok").length, on = s.types.filter((x) => x.on)
      return (
        <Row key={s.adapter} icon={<Ico I={srcIcon(s.source)} />} onClick={() => go("#/auto/source/" + s.adapter)} data-source-row={s.adapter} title={s.name}
          sub={[s.paused ? t("auto:sources.paused") : "", t("auto:sources.accounts", { count: s.connections.length }), t("auto:sources.eventTypes", { count: on.length }), t("auto:sources.events24", { count: on.reduce((n, x) => n + x.n24, 0) })].filter(Boolean).join(" · ")}>
          {bad ? <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s="wait" />{t("auto:sources.needAttention", { count: bad })}</span> : <span className="type-footnote text-muted-foreground">{s.connections.length ? t("auto:ok") : t("auto:sources.noAccounts")}</span>}
          <ChevronRightIcon className="size-4 text-tertiary" aria-hidden />
        </Row>) })}</Group>
      : <Empty title={t("auto:sources.emptyTitle")} sub={t("auto:sources.emptySub")} />}
    <div className="mt-2 px-1 type-footnote text-muted-foreground"><Trans t={t} i18nKey="auto:sources.footer" components={{ integrations: <a href="#/integrations" className="text-blue hover:underline" />, channels: <a href="#/channels" className="text-blue hover:underline" /> }} /></div>
  </>)
}
