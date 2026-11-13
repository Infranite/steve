/* #/auto/source/:adapter — one event source: pause, who triages, accounts, scopes, event types, flows, recent events
 * (signals-automation.md §9, prototype SourceView) */
import { useState } from "react"
import { PlusIcon, RefreshCwIcon, WorkflowIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import { Page, Row, Empty, StatusDot, BotAvatar, go, Panel, Section, SectionLabel, Blank, Chevron, Tile, Pill, Notice, ConnectPrompt } from "@/ui"
import { tm, toast, botName } from "@/store"
import { Unwired } from "@/unwired"
import { flowApi, useLoad, errText, type ScopeMode, type SourcePatch } from "@/api/flow"
import { Pick, Loading, Failed, botOptions, mainBot } from "@/auto/common"
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
  const on = s.connections.length > 0
  return (
    <Page title={s.name} back={BACK} max={680} sub={on ? t("auto:source.sub", { count: s.connections.length }) : undefined}
      actions={on ? <><span className="hidden type-footnote text-muted-foreground sm:inline">{s.paused ? t("auto:sources.paused") : t("auto:source.receiving")}</span>
        <Switch checked={!s.paused} disabled={busy} onCheckedChange={(v) => void patch({ paused: !v }, v ? t("auto:source.resumed") : t("auto:source.pausedToast"))} aria-label={t("auto:source.receiveAria")} data-source-on /></> : undefined}>
      {newRule && <NewRuleDialog source={s.source} onClose={() => setNewRule(false)} />}
      <div className="flex flex-col gap-8">
        {on && s.paused && <Notice tone="attention" data-source-paused action={<Button size="sm" variant="outline" disabled={busy} onClick={() => void patch({ paused: false }, t("auto:source.resumed"))}>{t("auto:source.resume")}</Button>}>{t("auto:source.pausedSub")}</Notice>}
        {/* no account yet: the one thing to do, and what it will bring in */}
        {!on ? <ConnectPrompt icon={<PlusIcon />} title={t("auto:source.connectTitle")} sub={t("auto:source.connectSub", { types: s.types.map((y) => y.label).join(t("auto:sep.list")) })}
            action={<Button size="sm" onClick={() => go("#/accounts")} data-add-account>{t("auto:source.addAccount")}</Button>} data-source-connect />
          : <Section label={t("auto:source.accounts")}>
            <Panel>
              {s.connections.map((c) => <Row key={c.id} title={c.name} sub={c.last_sync ? t("auto:source.syncedAt", { time: tm(c.last_sync) }) : t("auto:source.notSynced")} data-source-conn={c.id}>
                {c.status === "ok" ? <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s="done" />{t("auto:ok")}</span> : <Pill tone="attention">{t("auto:source.reauth")}</Pill>}</Row>)}
              <Row icon={<Tile I={PlusIcon} />} title={<span className="text-blue">{t("auto:source.addAccount")}</span>} onClick={() => go("#/accounts")} data-add-account />
            </Panel>
          </Section>}
        {on && <Section label={t("auto:source.recent")} right={<a href="#/signals" className="font-normal text-blue hover:underline">{t("auto:source.allSignals")}</a>}>
          {recent.data?.items.length ? <Panel>{recent.data.items.slice(0, 5).map((e) => <Row key={e.id} title={<span className="block truncate">{e.summary}</span>} sub={e.outcome_label || t("auto:source.processing")} onClick={() => go("#/signals?e=" + e.id)}><span className="type-footnote tabular-nums text-muted-foreground">{tm(e.received)}</span></Row>)}</Panel>
            : <Blank>{recent.data ? t("auto:source.noRecent") : t("auto:loading")}</Blank>}
        </Section>}
        <Section label={t("auto:source.handling")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewRule(true)}><PlusIcon />{t("auto:newFlow")}</Button>}>
          {mine.length ? <Panel>{mine.map((f) => <Row key={f.id} icon={<Tile I={WorkflowIcon} />} title={f.name} sub={f.description || f.origin_label || undefined} onClick={() => go("#/auto/rule/" + f.id)}><span className="type-footnote text-muted-foreground">{f.enabled ? t("auto:source.on") : t("auto:source.off")}</span><Chevron /></Row>)}</Panel>
            : <Blank>{t("auto:source.noFlows")}</Blank>}
        </Section>
        {on && <Section label={t("auto:source.settings")} data-source-settings>
          <div className="flex flex-col gap-4">
            <Panel><Row icon={<BotAvatar k={triage} size={28} />} title={t("auto:source.triage")} sub={t("auto:source.triageSub")}>
              <Pick label={t("auto:source.triage")} testId="src-bot" value={s.triage_bot ?? "_"} disabled={busy} options={[{ v: "_", l: t("auto:source.mainBot", { name: botName(mainBot()) }) }, ...botOptions()]} onChange={(v) => void patch({ triage_bot: v === "_" ? null : v }, t("auto:source.triageSet", { name: botName(v === "_" ? mainBot() : v) }))} /></Row></Panel>
            {s.scopes.length > 0 && <div>
              <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void refresh()}><RefreshCwIcon />{t("common:action.refresh")}</Button>}>{t("auto:source.scopes")}</SectionLabel>
              <Panel>{s.scopes.map((x) => (
                <Row key={x.connection_id + x.ext_id} title={x.name || x.ext_id} sub={[s.connections.length > 1 ? connName(x.connection_id) : "", x.mode === "record" ? t("auto:source.scopeRecord") : ""].filter(Boolean).join(" · ") || undefined} data-scope={x.ext_id}>
                  <Pick label={t("auto:source.scopeAria", { name: x.name || x.ext_id })} testId={x.ext_id} value={x.mode} options={modes()} disabled={busy} onChange={(v) => void patch({ scopes: [{ connection_id: x.connection_id, ext_id: x.ext_id, mode: v }] })} /></Row>))}</Panel>
            </div>}
            <div>
              <SectionLabel>{t("auto:source.types")}</SectionLabel>
              <Panel>{s.types.map((y) => <Row key={y.key} title={y.label} sub={[y.desc, y.flows ? t("auto:source.flowsUsing", { count: y.flows }) : ""].filter(Boolean).join(" · ")} data-event-type={y.key}>
                {y.n24 > 0 && <span className="hidden type-footnote tabular-nums text-muted-foreground sm:inline">{t("auto:sources.events24", { count: y.n24 })}</span>}
                {y.param !== null && <ParamInput v={y.param} label={t("auto:source.param", { label: y.label })} disabled={busy} onSave={(p) => void patch({ types: { [y.key]: { param: p } } }, t("common:state.saved"))} />}
                <Switch checked={y.on} disabled={busy} onCheckedChange={(v) => void patch({ types: { [y.key]: { on: v } } }, v ? t("auto:source.typeOn", { label: y.label }) : t("auto:source.typeOff", { label: y.label }))} aria-label={y.label} data-event-toggle={y.key} /></Row>)}</Panel>
            </div>
          </div>
        </Section>}
      </div>
    </Page>
  )
}

/** a type's parameter (会前提醒的提前量 “30m”): saved on blur or Enter */
function ParamInput({ v, label, disabled, onSave }: { v: string; label: string; disabled?: boolean; onSave: (v: string) => void }) {
  const [x, setX] = useState(v)
  const save = () => { if (x.trim() && x.trim() !== v) onSave(x.trim()) }
  return <Input aria-label={label} className="h-7 w-20 type-callout" value={x} disabled={disabled} onChange={(e) => setX(e.target.value)} onBlur={save} onKeyDown={(e) => e.key === "Enter" && save()} />
}

