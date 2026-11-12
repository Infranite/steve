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
import { t as tr, useT } from "@/i18n"

const back = (): [string, string] => ["#/auto/sources", tr("auto:title")]
const modes = (): { v: ScopeMode; l: string }[] => [{ v: "receive", l: tr("auto:source.modeReceive") }, { v: "record", l: tr("auto:source.modeRecord") }, { v: "ignore", l: tr("auto:source.modeIgnore") }]

export function SourceView({ adapter }: { adapter: string }) {
  const L = useLoad(() => flowApi.sources(), [], ["signal"])
  const flows = useLoad(() => flowApi.flows("flow"), [], ["flow"])
  const s = L.data?.find((x) => x.adapter === adapter)
  const recent = useLoad(() => (s ? flowApi.signals({ range: "7d", source: s.source }) : Promise.resolve(null)), [s?.source], ["signal"])
  const [busy, setBusy] = useState(false), [newRule, setNewRule] = useState(false)
  const t = useT(), BACK = back()
  if (L.unwired) return <Unwired id="auto" />
  if (L.error && !L.data) return <Failed title={t("auto:source.page")} back={BACK} msg={errText(L.error)} retry={() => void L.reload()} />
  if (!L.data) return <Loading title={t("auto:source.page")} back={BACK} />
  if (adapter === "im" || s?.source === "im") return (
    <Page title="IM" back={BACK}><Empty title={t("auto:source.imMoved")} sub={t("auto:source.imMovedSub")} action={<Button onClick={() => go("#/channels")}>{t("auto:source.toChannels")}</Button>} /></Page>
  )
  if (!s) return <Page title={t("auto:source.page")} back={BACK}><Empty title={t("auto:source.notFound")} sub={t("auto:source.notFoundSub")} /></Page>
  const patch = async (p: SourcePatch, ok?: string) => {
    setBusy(true)
    try { const v = await flowApi.patchSource(s.adapter, p); L.set(L.data!.map((x) => (x.adapter === v.adapter ? v : x))); if (ok) toast(ok) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const refresh = async () => { setBusy(true); try { const v = await flowApi.refreshScopes(s.adapter); L.set(L.data!.map((x) => (x.adapter === v.adapter ? v : x))); toast(t("auto:source.refreshed")) } catch (e) { toast(errText(e)) } finally { setBusy(false) } }
  const mine = (flows.data ?? []).filter((f) => f.trigger?.source === s.source)
  const triage = s.triage_bot ?? mainBot()
  const connName = (id: string) => s.connections.find((c) => c.id === id)?.name ?? ""
  return (
    <Page title={s.name} back={BACK} max={680} sub={s.paused ? t("auto:source.pausedSub") : t("auto:source.sub", { count: s.connections.length })}
      actions={<><span className="hidden type-footnote text-muted-foreground sm:inline">{s.paused ? t("auto:sources.paused") : t("auto:source.receiving")}</span>
        <Switch checked={!s.paused} disabled={busy || !s.connections.length} onCheckedChange={(v) => void patch({ paused: !v }, v ? t("auto:source.resumed") : t("auto:source.pausedToast"))} aria-label={t("auto:source.receiveAria")} data-source-on /></>}>
      {newRule && <NewRuleDialog source={s.source} onClose={() => setNewRule(false)} />}
      <div className="flex flex-col gap-8">
        <section><SectionLabel>{t("auto:source.triage")}</SectionLabel>
          <Group><Row icon={<BotAvatar k={triage} size={28} />} title={botName(triage)} sub={t("auto:source.triageSub")}>
            <Pick label={t("auto:source.triage")} testId="src-bot" value={s.triage_bot ?? "_"} disabled={busy || !s.connections.length} options={[{ v: "_", l: t("auto:source.mainBot", { name: botName(mainBot()) }) }, ...botOptions()]} onChange={(v) => void patch({ triage_bot: v === "_" ? null : v }, t("auto:source.triageSet", { name: botName(v === "_" ? mainBot() : v) }))} /></Row></Group>
        </section>
        <section><SectionLabel>{t("auto:source.accounts")}</SectionLabel>
          <Group>
            {s.connections.map((c) => <Row key={c.id} title={c.name} sub={c.last_sync ? t("auto:source.syncedAt", { time: tm(c.last_sync) }) : t("auto:source.notSynced")} data-source-conn={c.id}>
              <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={c.status === "ok" ? "done" : "wait"} />{c.status === "ok" ? t("auto:ok") : t("auto:source.reauth")}</span></Row>)}
            <Row icon={<Ico I={PlusIcon} />} title={<span className="text-blue">{t("auto:source.addAccount")}</span>} sub={t("auto:source.addAccountSub")} onClick={() => go("#/accounts")} data-add-account />
          </Group>
        </section>
        {s.connections.length > 0 && <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void refresh()}><RefreshCwIcon />{t("common:action.refresh")}</Button>}>{t("auto:source.scopes")}</SectionLabel>
          {s.scopes.length ? <Group>{s.scopes.map((x) => (
            <Row key={x.connection_id + x.ext_id} title={x.name || x.ext_id} sub={[s.connections.length > 1 ? connName(x.connection_id) : "", x.mode === "record" ? t("auto:source.scopeRecord") : x.mode === "ignore" ? t("auto:source.modeIgnore") : ""].filter(Boolean).join(" · ") || undefined} data-scope={x.ext_id}>
              <Pick label={t("auto:source.scopeAria", { name: x.name || x.ext_id })} testId={x.ext_id} value={x.mode} options={modes()} disabled={busy} onChange={(v) => void patch({ scopes: [{ connection_id: x.connection_id, ext_id: x.ext_id, mode: v }] })} /></Row>))}</Group>
            : <div className="rounded-xl border border-dashed px-4 py-6 text-center type-callout text-muted-foreground">{t("auto:source.noScopes")}</div>}
        </section>}
        <section><SectionLabel>{t("auto:source.types")}</SectionLabel>
          <Group>{s.types.map((y) => <Row key={y.key} title={y.label} sub={[y.desc, t("auto:sources.events24", { count: y.n24 }), y.flows ? t("auto:source.flowsUsing", { count: y.flows }) : t("auto:source.noFlowsRecord")].filter(Boolean).join(" · ")} data-event-type={y.key}>
            {y.param !== null && <ParamInput v={y.param} label={t("auto:source.param", { label: y.label })} disabled={busy || !s.connections.length} onSave={(p) => void patch({ types: { [y.key]: { param: p } } }, t("common:state.saved"))} />}
            <Switch checked={y.on} disabled={busy || !s.connections.length} onCheckedChange={(v) => void patch({ types: { [y.key]: { on: v } } }, v ? t("auto:source.typeOn", { label: y.label }) : t("auto:source.typeOff", { label: y.label }))} aria-label={y.label} data-event-toggle={y.key} /></Row>)}</Group>
          {!s.connections.length && <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("auto:source.addFirst")}</div>}
        </section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewRule(true)}><PlusIcon />{t("auto:newFlow")}</Button>}>{t("auto:source.handling")}</SectionLabel>
          {mine.length ? <Group>{mine.map((f) => <Row key={f.id} icon={<Ico I={WorkflowIcon} />} title={f.name} sub={f.description || f.origin_label} onClick={() => go("#/auto/rule/" + f.id)}><span className="type-footnote text-muted-foreground">{f.enabled ? t("auto:source.on") : t("auto:source.off")}</span><ChevronRightIcon className="size-4 text-tertiary" aria-hidden /></Row>)}</Group>
            : <div className="rounded-xl border border-dashed px-4 py-6 text-center type-callout text-muted-foreground">{t("auto:source.noFlows")}</div>}
        </section>
        <section><SectionLabel right={<a href="#/signals" className="font-normal hover:text-foreground">{t("auto:source.allSignals")}</a>}>{t("auto:source.recent")}</SectionLabel>
          {recent.data?.items.length ? <Group>{recent.data.items.slice(0, 6).map((e) => <Row key={e.id} title={e.summary} sub={<>{e.outcome_label || t("auto:source.processing")}{e.flow && <> · {t("auto:flowRef", { name: e.flow.name })}</>}</>} onClick={() => go("#/signals?e=" + e.id)}><span className="type-footnote tabular-nums text-muted-foreground">{tm(e.received)}</span></Row>)}</Group>
            : <div className="px-1 type-callout text-muted-foreground">{recent.data ? t("auto:source.noRecent") : t("auto:loading")}</div>}
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

