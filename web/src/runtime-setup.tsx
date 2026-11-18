import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react"
import { ChevronDownIcon, CpuIcon, DownloadIcon, RefreshCwIcon } from "lucide-react"
import { api, type RRuntime } from "@/api"
import { IntegrationSettings, IntegrationSection } from "@/components/affect/integration-settings-clean"
import { Badge } from "@/components/affect/badge-clean"
import { HarnessMark } from "@/components/harness-mark"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Panel } from "@/panel"
import { RuntimeBadge } from "@/runtime-access"
import { canInstallRuntime, componentStatus, supportsPermission } from "@/runtime-status"
import { refresh, st, tm, toast, useStore } from "@/store"
import { go, openSheet } from "@/ui"
import { useT } from "@/i18n"

/** This inventory belongs to the Hub's local Node. It does not describe another device's capabilities. */
export function RuntimeSetupPanel({ children }: { children?: ReactNode }) {
  useStore()
  const t = useT()
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [pending, setPending] = useState<string[]>([])
  const [messages, setMessages] = useState<Record<string, string>>({})
  const [restarts, setRestarts] = useState<string[]>([])
  const [expanded, setExpanded] = useState(false)
  const inventoryId = useId()
  const installing = useRef(new Set<string>())
  const installEpoch = useRef(0)
  const reading = useRef<ReturnType<typeof api.devices> | null>(null)
  const reload = useCallback(async () => {
    // Manual refresh and installation polling share one probe; repeated clicks cannot duplicate it.
    if (reading.current) return reading.current
    const epoch = installEpoch.current
    setBusy(true)
    const load = (async () => {
      try {
        const result = await api.devices(true)
        await refresh()
        setError("")
        // A probe started before an install cannot retire that install's local pending state.
        if (epoch === installEpoch.current) setPending(ids => ids.filter(id => installing.current.has(id) || result.runtimes.find(r => r.id === id)?.setup?.state === "installing"))
        return result
      } catch (e) { setError((e as Error).message); throw e }
      finally { reading.current = null; setBusy(false) }
    })()
    reading.current = load
    return load
  }, [])
  const polling = pending.length > 0 || st.runtimes.some(r => r.setup?.state === "installing")
  useEffect(() => {
    if (!polling) return
    let live = true, timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      if (!live) return
      if (!document.hidden) { try { await reload() } catch { /* shown beside refresh; keep retrying while installing */ } }
      if (live) timer = setTimeout(() => void poll(), 1500)
    }
    timer = setTimeout(() => void poll(), 1500)
    return () => { live = false; clearTimeout(timer) }
  }, [polling, reload])
  const install = async (r: RRuntime) => {
    if (!canInstallRuntime(r, messages[r.id]) || r.setup?.state === "installing" || installing.current.has(r.id)) return
    installEpoch.current++
    installing.current.add(r.id)
    setPending(ids => [...new Set([...ids, r.id])])
    setMessages(ms => ({ ...ms, [r.id]: "" }))
    try {
      const result = await api.installHarness(r.id)
      if (result.restart) setRestarts(ids => [...new Set([...ids, r.id])])
      if (!result.accepted) setMessages(ms => ({ ...ms, [r.id]: t("shell:runtimeSetup.notAccepted") }))
    } catch (e) { setMessages(ms => ({ ...ms, [r.id]: (e as Error).message })) }
    finally {
      installing.current.delete(r.id)
      // Do not count an older manual/polling read as the post-install probe. Drain it, then read anew.
      try { await reading.current } catch { /* the fresh probe below can recover */ }
      try { await reload() } catch { /* preserve the operation error as well as the refresh error */ }
    }
  }
  // Keep operation feedback outside the folded inventory, without changing the probe's availability verdict.
  const available = st.runtimes.filter(r => r.ok)
  const needsAttention = (r: RRuntime) => pending.includes(r.id) || r.setup?.state === "installing" || !!messages[r.id] || !!r.setup?.error || restarts.includes(r.id)
  const attention = st.runtimes.filter(r => !r.ok && needsAttention(r))
  const unavailable = st.runtimes.filter(r => !r.ok && !needsAttention(r))
  const entryId = (r: RRuntime) => `${inventoryId}-${encodeURIComponent(r.id)}`
  // One keyed sibling list preserves card state and focused controls as an install moves between groups.
  const cards: ReactNode[] = [...available, ...attention, ...unavailable].map(r => <div key={`runtime:${r.id}`} id={entryId(r)} hidden={!expanded && !r.ok && !needsAttention(r)}>
    <RuntimeCard runtime={r} pending={pending.includes(r.id)} error={messages[r.id]} restart={restarts.includes(r.id)} install={() => void install(r)} />
  </div>)
  if (unavailable.length) cards.splice(available.length + attention.length, 0,
    <Button key="unavailable-toggle" variant="ghost" className="min-h-11 w-full justify-start" aria-expanded={expanded} aria-controls={unavailable.map(entryId).join(" ")} onClick={() => setExpanded(v => !v)} data-runtime-unavailable-toggle>
      <ChevronDownIcon className={expanded ? "rotate-180" : undefined} />{t("shell:runtimeSetup.unavailableCount", { count: unavailable.length })}
    </Button>)
  return <Panel k="runtimes" icon={<CpuIcon />} title={t("shell:devices.runtimes")} sub={t("shell:devices.runtimesSub", { ok: st.runtimes.filter(r => r.ok).length, count: st.runtimes.length })}
    right={<Button size="sm" variant="ghost" disabled={busy} onClick={() => void reload().then(() => toast(t("shell:devices.rechecked"))).catch(() => {})} data-runtime-refresh>{busy ? <Spinner /> : <RefreshCwIcon />}{t("shell:devices.recheck")}</Button>}>
    {error && <details className="break-words px-5 pb-3 type-callout text-attention">
      <summary className="cursor-pointer"><span role="alert" data-runtime-refresh-error>{t("shell:runtimeSetup.refreshFailed")}</span></summary>
      <p className="mt-2 whitespace-pre-wrap break-all" data-runtime-refresh-error-detail>{error}</p>
    </details>}
    {st.runtimes.length ? <div className="flex flex-col gap-3 px-3 pb-3" data-runtime-inventory>{cards}</div>
      : <div className="px-5 py-4 type-callout text-muted-foreground">{t("shell:devices.noRuntimes")}</div>}
    {children && <div className="px-5 pt-1 pb-4">{children}</div>}
  </Panel>
}

function RuntimeCard({ runtime: r, pending, error, restart, install }: { runtime: RRuntime; pending: boolean; error?: string; restart: boolean; install: () => void }) {
  const t = useT(), setup = r.setup, active = pending || setup?.state === "installing"
  const [detailsOpen, setDetailsOpen] = useState(false)
  const detailsId = useId()
  const canInstall = canInstallRuntime(r, error)
  const help = !(r.ok && setup?.state === "ready") && (setup?.help || ((!setup || canInstall) && r.install_hint))
  const bot = Object.values(st.bots).find(b => b.on && b.harness === r.id && supportsPermission(r, b.permMode))
  const failed = !active && !!(error || setup?.error)
  const summary = !active && !failed && !r.ok ? t(setup?.state === "unknown" ? "shell:runtimeSetup.unknownSummary" : setup?.state === "not_installed" ? "shell:runtimeSetup.missingSummary" : canInstall ? "shell:runtimeSetup.incompleteSummary" : "shell:runtimeSetup.unavailableSummary") : ""
  return <IntegrationSettings name={r.name} logo={<HarnessMark harness={r.id} className="size-5" />} meta={<span className="flex flex-wrap items-center gap-2" aria-live="polite">
    {(r.version || r.adapter_version) && <span className="break-all">{r.version || r.adapter_version}</span>}
    {/* Complete components alone do not establish usability. Keep a failed probe visibly unavailable. */}
    {setup?.state === "ready" && !r.ok ? <Badge dot tone="warning" data-runtime-state="unavailable">{t("shell:runtimeSetup.state.unavailable")}</Badge> : <RuntimeBadge runtime={r} />}
  </span>}
    data-runtime={r.id}>
    <IntegrationSection>
      {summary && <p className="mb-2 type-footnote text-muted-foreground" data-runtime-summary>{summary}</p>}
      {active && <p className="mb-2 type-footnote text-muted-foreground" role="status">{t(setup?.state === "installing" ? "shell:runtimeSetup.polling" : "shell:runtimeSetup.requesting")}</p>}
      {restart && <p className="mb-2 break-words type-callout text-attention" role="status" data-runtime-restart>{t("shell:runtimeSetup.restart")}</p>}
      {failed && <p className="mb-2 type-callout text-attention" role="alert" data-runtime-install-error>{t("shell:runtimeSetup.installFailed")}</p>}
      <div className="flex flex-wrap items-center gap-2">
        {(canInstall || active) && <Button size="sm" disabled={active || !canInstall} onClick={install} data-runtime-install={r.id}>
          {active ? <Spinner /> : <DownloadIcon />}{t(active ? "shell:runtimeSetup.state.installing" : setup?.error || error ? "shell:runtimeSetup.retry" : "shell:runtimeSetup.install")}
        </Button>}
        {r.ok && !active && <Button size="sm" variant="outline" onClick={() => bot ? openSheet("newItem", { bot: bot.id }) : go("#/bots")} data-runtime-use={r.id}>{t(bot ? "shell:runtimeSetup.start" : "shell:runtimeSetup.configureBot")}</Button>}
        <Button size="sm" variant="ghost" aria-expanded={detailsOpen} aria-controls={detailsId} aria-label={t("shell:runtimeSetup.detailsFor", { name: r.name })} onClick={() => setDetailsOpen(v => !v)} data-runtime-details-toggle>
          <ChevronDownIcon className={detailsOpen ? "rotate-180" : undefined} />{t(detailsOpen ? "shell:runtimeSetup.hideDetails" : "shell:runtimeSetup.details")}
        </Button>
      </div>
    </IntegrationSection>
    <div id={detailsId} hidden={!detailsOpen} data-runtime-diagnostics>
      <IntegrationSection>
        <div className="flex min-w-0 flex-col gap-2 type-footnote text-muted-foreground">
          <span>{t(r.custom ? "shell:devices.harness.custom" : "shell:devices.harness.builtin")}</span>
          {r.auth && r.auth.length > 0 && <span data-runtime-auth>{t("shell:devices.harness.auth", { methods: r.auth.map(a => a.name).join(t("shell:list.sep")) })}</span>}
          {r.note && <span className="break-words whitespace-pre-wrap">{r.note}</span>}
          {r.path && <span className="break-all font-mono">{r.path}</span>}
          {setup?.reason && <span className="break-words whitespace-pre-wrap" data-runtime-reason>{setup.reason}</span>}
          {[...new Set([setup?.error, !r.ok && r.error].filter(Boolean))].map(msg => <span key={String(msg)} className="break-words whitespace-pre-wrap text-attention" data-runtime-error>{msg}</span>)}
          {error && <span className="break-words whitespace-pre-wrap text-attention" data-runtime-install-error-detail>{error}</span>}
          {!r.ok && !setup?.reason && !setup?.error && !r.error && <span>{t("shell:runtimeSetup.probeUnavailable")}</span>}
          {help && <span className="break-words whitespace-pre-wrap" data-runtime-help>{help}</span>}
          {setup?.state === "ready" && !r.ok && <span className="text-attention">{t("shell:runtimeSetup.probeUnavailable")}</span>}
          <AdapterUpdate runtime={r} />
          <RuntimeChips runtime={r} />
        </div>
      </IntegrationSection>
      {!!setup?.components.length && <IntegrationSection label={t("shell:runtimeSetup.components")}>
        <ul className="flex min-w-0 flex-col gap-3">{setup.components.map(c => {
          const status = componentStatus(c, r)
          return <li key={c.id} className="flex min-w-0 flex-col gap-1 type-footnote" data-runtime-component={c.id}>
          <span className="flex flex-wrap items-center gap-2"><span className="font-medium">{c.name}</span><Badge tone={status === "installed" ? "success" : status === "unknown" ? "neutral" : "warning"} data-component-installed={String(c.installed)} data-component-status={status}>{t(status === "installed" ? "shell:runtimeSetup.installed" : status === "unknown" ? "shell:runtimeSetup.componentUnknown" : "shell:runtimeSetup.state.not_installed")}</Badge>{c.version && <span className="font-mono text-muted-foreground">{c.version}</span>}</span>
          <span className="break-all font-mono text-muted-foreground">{c.package}</span>
          {c.path && <span className="break-all text-muted-foreground" data-component-target>{t("shell:runtimeSetup.componentTarget")} <span className="font-mono">{c.path}</span></span>}
          {status === "unknown" && <span className="break-words text-muted-foreground" data-component-evidence>{t("shell:runtimeSetup.componentUnknownHelp")}</span>}
          {c.detected && <span className="flex min-w-0 flex-col gap-1 text-muted-foreground" data-component-detected>
            <span className="break-words">{t("shell:runtimeSetup.otherInstallation")}</span>
            <span className="break-all font-mono">{c.detected.path}</span>
            {c.detected.version && <span className="break-all font-mono">{c.detected.version}</span>}
            {status !== "installed" && <span className="break-words">{t("shell:runtimeSetup.targetIncomplete")}</span>}
          </span>}
        </li>})}</ul>
      </IntegrationSection>}
    </div>
  </IntegrationSettings>
}

function AdapterUpdate({ runtime: r }: { runtime: RRuntime }) {
  const t = useT(), u = r.update
  if (!u || u.policy === "off") return null
  const newer = !!u.latest && u.latest !== u.installed
  const text = u.error ? t("shell:devices.harness.updateError", { error: u.error }) : !u.latest ? t("shell:devices.harness.updateUnchecked")
    : !newer ? t("shell:devices.harness.upToDate", { version: u.installed ?? "", at: u.checked ? tm(u.checked) : "" })
    : u.policy === "auto" ? t("shell:devices.harness.updatePending", { from: u.installed ?? "—", to: u.latest }) : t("shell:devices.harness.updateAvailable", { to: u.latest })
  return <span className={newer || u.error ? "text-attention" : undefined} data-adapter-update>{text}</span>
}

function RuntimeChips({ runtime: r }: { runtime: RRuntime }) {
  const t = useT(), caps = r.caps
  const chips = caps ? ([["sessions", caps.sessions], ["judge", caps.jobs], ["images", caps.images], ["resume", caps.resume], ["history", caps.history]] as const).filter(([, on]) => on).map(([k]) => t(`shell:devices.harness.cap.${k}`)) : []
  const modes = (r.modes ?? []).map(m => t(`shell:devices.harness.mode.${m}`, { defaultValue: m }))
  return <span className="flex flex-wrap gap-1" data-runtime-caps>{[...chips, ...modes].map(c => <Badge key={c}>{c}</Badge>)}</span>
}
