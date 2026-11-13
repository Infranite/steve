import { HubError, Unauthenticated } from "@/api"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { readableVersion, updates, updateActive, updateKey, repairKey, sameReleaseVersion, updatePhaseLabel, checkStale, committedFailure, type UpdateBackup, type UpdateState, type UpdateOperation } from "@/updates"
import { downloadPercent, enableOneClick, installLabel, serviceCommands, stageIndex, stageLabel, upgradeGuide, upgradeStages, verifyUpgrade, type Health, type Install } from "@/upgrade-guide"
import { CheckIcon, CircleAlertIcon, CopyIcon, LoaderCircleIcon, RefreshCwIcon, ServerIcon } from "lucide-react"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { CodeBlock, CodeBlockContent, CodeBlockCopyButton } from "@/components/ui/code-block"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Row, go } from "@/ui"
import { Panel, Section, Card, Notice, Note, Chevron, Tile, LIFT, EASE } from "@/settings-kit"
import { ChevronRightIcon } from "lucide-react"
import { cn } from "cn"
import { LOADED_WEB_VERSION, st, toast, useStore } from "@/store"
import { useT } from "@/i18n"
import { clockDur, date, num } from "@/i18n/format"

function VersionValue({ v, label }: { v: string; label: string }) {
  const t = useT()
  if (!v) return <span className="type-callout text-muted-foreground">{t("pages:version.fetching")}</span>
  const r = readableVersion(v)
  return <span className="flex min-w-0 items-center gap-1">
    <span className="type-callout tabular-nums text-muted-foreground" title={r.raw} data-version-value>
      {r.when ? <>{r.when}<span className="mx-1.5 text-tertiary">·</span><span className="type-code">{r.commit}</span></> : <span className="type-code">{r.raw}</span>}
    </span>
    <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("pages:version.copyAria", { label })} className="text-muted-foreground" onClick={() => { navigator.clipboard?.writeText(r.raw).catch(() => {}); toast(t("pages:version.copiedValue", { value: r.raw })) }} />}><CopyIcon /></TooltipTrigger><TooltipContent>{t("pages:version.copyFull")}</TooltipContent></Tooltip>
  </span>
}

/** Settings · 关于. The frontend is the build this tab loaded, independent of the Hub's running build. */
export function AboutVersions() {
  useStore()
  const t = useT()
  const newer = !!st.webVersion && st.webVersion !== LOADED_WEB_VERSION
  const differ = !!st.version && st.version !== LOADED_WEB_VERSION
  return <div className="flex flex-col gap-8"><Section data-version-info label={t("pages:version.section")}>
    <Panel>
      <Row title={t("pages:version.frontend")} sub={t("pages:version.currentPage")}><VersionValue v={LOADED_WEB_VERSION} label={t("pages:version.frontendVersion")} /></Row>
      <Row title={t("pages:version.backend")} sub="Hub"><VersionValue v={st.version} label={t("pages:version.backendVersion")} /></Row>
      {newer && <Row title={t("pages:version.availableFrontend")} sub={t("pages:version.availableSub")} data-web-update>
        <span className="flex items-center gap-2"><VersionValue v={st.webVersion} label={t("pages:version.availableFrontendVersion")} /><Button size="sm" variant="outline" onClick={() => location.reload()}><RefreshCwIcon />{t("pages:version.reload")}</Button></span></Row>}
    </Panel>
    {differ && !newer && <Notice tone="attention" className="mt-3" data-version-differ>{t("pages:version.differ")}</Notice>}
    </Section>
    <UpgradeSettings />
  </div>
}


type PendingReceipt = { kind: "execute"; version: string; key: string } | { kind: "approve" | "reject"; id: string }
function confirmsReceipt(pending: PendingReceipt | null, operation: UpdateOperation | null) {
  if (!pending || !operation) return false
  if (pending.kind === "execute") return operation.version === pending.version && operation.idempotency_key === pending.key
  if (operation.id !== pending.id) return false
  if (pending.kind === "reject") return operation.phase === "failed" && !!operation.error?.includes("拒绝")
  return operation.phase !== "awaiting_approval"
}

/** The journal is authoritative. Reconnecting only reads it; it never repeats an upgrade POST. */
export function UpgradeSettings() {
  const t = useT()
  const [state, setState] = useState<UpdateState | null>(null)
  const [busy, setBusy] = useState(false)
  const [connection, setConnection] = useState("")
  const [error, setError] = useState("")
  const pendingReceipt = useRef<PendingReceipt | null>(null)
  const generation = useRef(0), mounted = useRef(false), mutating = useRef(false), autoChecked = useRef(false)
  useEffect(() => {
    mounted.current = true
    let stopped = false, timer: ReturnType<typeof setTimeout>
    const read = async () => {
      const epoch = generation.current
      if (!mutating.current) {
        try {
          const next = await updates.status()
          if (!stopped && epoch === generation.current) {
            setState(next); setConnection(""); if (confirmsReceipt(pendingReceipt.current, next.operation)) { pendingReceipt.current = null; setError("") }
            // the instance manager opening this page sees a current answer without clicking 检查更新
            if (!autoChecked.current && checkStale(next)) { autoChecked.current = true; void checkQuietly() }
          }
        } catch {
          if (!stopped && epoch === generation.current) setConnection(t("pages:upgrade.reconnecting"))
        }
      }
      if (!stopped) timer = setTimeout(read, 2000)
    }
    const checkQuietly = async () => {
      if (mutating.current) return
      mutating.current = true; setBusy(true)
      try { const check = await updates.check(); if (!stopped) setState(s => s && ({ ...s, check })) }
      catch { /* the cached answer stays; 检查更新 shows the error */ }
      finally { mutating.current = false; if (!stopped) setBusy(false) }
    }
    void read()
    return () => { stopped = true; mounted.current = false; clearTimeout(timer) }
  }, [])
  const action = async (kind: "check" | "execute" | "approve" | "retry" | "reject" | "repair") => {
    if (mutating.current || !state) return
    mutating.current = true; generation.current++; pendingReceipt.current = null; setBusy(true); setError("")
    try {
      if (kind === "check") {
        const check = await updates.check()
        if (mounted.current) setState(s => s && ({ ...s, check }))
      } else {
        const key = kind === "repair" ? repairKey(state, state.check!.version!) : updateKey(state.repository, state.check?.version ?? "", kind === "retry" ? state.operation!.id : undefined)
        pendingReceipt.current = kind === "approve" || kind === "reject" ? { kind, id: state.operation!.id } : { kind: "execute", version: state.check!.version!, key }
        const operation = kind === "reject" ? await updates.reject(state.operation!.id) : kind === "approve"
          ? await updates.approve(state.operation!.id)
          : await updates.execute(state.check!.version!, key)
        pendingReceipt.current = null
        if (mounted.current) setState(s => s && ({ ...s, operation }))
      }
    } catch (e) {
      const definite = e instanceof Unauthenticated || (e instanceof HubError && e.status >= 400 && e.status < 500 && e.status !== 408)
      if (definite || kind === "check") {
        pendingReceipt.current = null
        if (mounted.current) setError(e instanceof Unauthenticated ? t("pages:upgrade.expired") : e instanceof Error ? e.message : t("pages:upgrade.requestFailed"))
      } else if (mounted.current) {
        setConnection(t("pages:upgrade.confirming"))
        setError(t("pages:upgrade.unconfirmed"))
      }
    } finally { mutating.current = false; if (mounted.current) setBusy(false) }
  }
  const op = state?.operation ?? null, active = updateActive(op)
  const canExecute = !!state?.capability.can_execute
  const owner = state?.capability.owner !== false
  const check = state?.check
  const install = state?.install
  // the instance manager gets this machine's commands; others only learn who upgrades
  const manual = !!install && install.kind !== "managed" && install.home !== undefined
  const [verifying, setVerifying] = useState<string | null>(null)
  const [verified, setVerified] = useState<string | null>(null)
  const pairMismatch = !!state && !!check?.version && (!sameReleaseVersion(state.current.hub, check.version) || !sameReleaseVersion(state.current.web, check.version))
  const repair = !!state && check?.status === "available" && !!check.version && pairMismatch && (sameReleaseVersion(state.current.hub, check.version) || (op?.phase === "succeeded" && sameReleaseVersion(op.version, check.version)))
  const version = check?.version ?? ""
  const messages = {
    available: repair ? t("pages:upgrade.status.repair", { version }) : t("pages:upgrade.status.available", { version }),
    up_to_date: t("pages:upgrade.status.up_to_date"), ahead: t("pages:upgrade.status.ahead"),
    unknown_current: t("pages:upgrade.status.unknown_current"),
    no_release: t("pages:upgrade.status.no_release"), error: t("pages:upgrade.status.error"), unsupported_platform: t("pages:upgrade.status.unsupported_platform"),
  }
  const releaseURL = (() => { try { const u = new URL(check?.release_url ?? ""); return u.protocol === "https:" ? u.href : undefined } catch { return undefined } })()
  const checked = check && t("pages:upgrade.checkedAt", { time: date(check.checked_at, { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" }) })
  return <Section data-upgrade-settings label={t("pages:upgrade.section")}>
    {state && !canExecute && <UpgradeUnavailable state={state} manual={manual} />}
    <Panel className="anim-in">
      <Row title="GitHub Release" sub={state?.repository ?? t("pages:upgrade.reading")}>
        <Button size="sm" variant="outline" disabled={!state || !owner || busy || active || !!connection} onClick={() => void action("check")} data-upgrade-check><RefreshCwIcon />{busy ? t("pages:upgrade.wait") : t("pages:upgrade.check")}</Button>
      </Row>
      {state && check && <div className="flex flex-col gap-2 px-3 py-3 type-callout" data-upgrade-check-status={check.status}>
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="type-body">{messages[check.status]}</p>
            <p className="mt-0.5 type-footnote text-muted-foreground">{checked}</p>
          </div>
          {releaseURL && <a className="shrink-0 pt-0.5 type-footnote text-blue hover:underline" href={releaseURL} target="_blank" rel="noopener noreferrer">{t("pages:upgrade.releaseNotes")}</a>}
        </div>
        {check.error && <p className="text-destructive">{check.error}</p>}
        {check.notes && <Fold label={t("pages:upgrade.changes")}><div className="max-h-[60vh] overflow-auto break-words text-muted-foreground" data-release-notes><Markdown text={check.notes} className="type-callout [&_h1]:mt-0 [&_h1]:type-headline [&_h2]:type-headline [&_h3]:type-body [&_h3]:font-semibold" /></div></Fold>}
        {canExecute && repair && !active && <Button className="self-start" size="sm" disabled={busy || !!connection} onClick={() => void action("repair")} data-upgrade-repair>{t("pages:upgrade.repairBtn", { version: check.version })}</Button>}
        {canExecute && !repair && check.status === "available" && check.version && !active && op?.version !== check.version && <Button className="self-start" size="sm" disabled={busy || !!connection} onClick={() => void action("execute")} data-upgrade-execute>{t("pages:upgrade.executeBtn", { version: check.version })}</Button>}
      </div>}
      {state && op && <div className="flex flex-col gap-2 px-3 py-3 type-callout" data-upgrade-phase={op.phase} role="status">
        <p className="type-body font-medium">{updatePhaseLabel[op.phase]} · <span className="type-code break-all">{op.version}</span></p>
        {op.phase === "awaiting_approval" && <><p className="text-muted-foreground">{t("pages:upgrade.requested", { who: op.requested_by })}</p>{canExecute && <div className="flex flex-wrap gap-2"><Button size="sm" disabled={busy || !!connection} onClick={() => void action("approve")} data-upgrade-approve>{t("pages:upgrade.approve")}</Button><Button size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("reject")} data-upgrade-reject>{t("pages:upgrade.reject")}</Button></div>}</>}
        {(active || op.phase === "succeeded") && stageIndex(op.phase) >= 0 && <UpgradeProgress phase={op.phase} verified={verified === op.id} runnerKind={op.runner?.kind} />}
        {op.progress && <UpgradeActivity op={op} connected={!connection} />}
        {op.phase === "draining" && op.drain && <UpgradeWaiting drain={op.drain} />}
        {active && op.phase !== "awaiting_approval" && <p className="text-muted-foreground">{t("pages:upgrade.restartNote")}</p>}
        {canExecute && !repair && ["failed", "rolled_back"].includes(op.phase) && check?.status === "available" && check.version === op.version && <Button className="self-start" size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("retry")} data-upgrade-retry>{t("pages:upgrade.retry")}</Button>}
        {op.error && <p className="text-destructive break-words">{op.error}</p>}
        {op.phase !== "awaiting_approval" && op.phase !== "succeeded" && <UpgradeRecovery op={op} backup={state.backup ?? null} install={install} />}
        {op.phase === "succeeded" && <UpgradeVerify target={op.version} embedded onResult={done => setVerified(done ? op.id : null)} />}
      </div>}
      {state && <div data-upgrade-services>
        <Row icon={<Tile I={ServerIcon} />} title={t("pages:upgrade.services.title")} sub={t("pages:upgrade.services.devices")} onClick={() => go("#/devices")}><Chevron /></Row>
        {owner && install?.home && ["managed", "local_release"].includes(install.kind) && <div className="px-3 pb-3 type-callout">
          <Fold label={t("pages:upgrade.services.terminal")} quiet>
            <p className="text-muted-foreground">{t("pages:upgrade.services.note")}</p>
            {serviceCommands(install.home).map(command => <Command key={command} command={command} />)}
          </Fold>
        </div>}
      </div>}
    </Panel>
    {connection && <p role="status" className="mt-2 px-1 type-footnote text-muted-foreground" data-upgrade-connection>{connection}</p>}
    {state && canExecute && <Note data-upgrade-capability data-install-kind={install?.kind}>{install?.kind === "local_release" ? t("pages:upgrade.local") : t("pages:upgrade.managed")}</Note>}
    {state && !canExecute && manual && install && check?.status === "available" && check.version && <ManualGuide install={install} version={check.version} onVerify={() => setVerifying(check.version)} />}
    {verifying && <UpgradeVerify target={verifying} onClose={() => setVerifying(null)} />}
    {error && <p className="mt-2 px-1 type-footnote text-destructive" role="alert">{error}</p>}
  </Section>
}

/** a disclosure inside a panel: a quiet summary with a chevron that turns, and a body whose height eases open */
function Fold({ label, quiet, children }: { label: string; quiet?: boolean; children: ReactNode }) {
  return <details className="group/fold">
    <summary className={cn("inline-flex cursor-pointer list-none items-center gap-1 rounded-md type-callout [&::-webkit-details-marker]:hidden", quiet ? "text-muted-foreground hover:text-foreground" : "text-blue hover:text-blue/80")}>
      <ChevronRightIcon className={cn("size-3.5 shrink-0 transition-transform group-open/fold:rotate-90", EASE)} aria-hidden />{label}
    </summary>
    <div className="flex flex-col gap-1.5 pt-2">{children}</div>
  </details>
}

/** No upgrade button here: say so, the Hub's reason for this installation, and what to do instead. */
function UpgradeUnavailable({ state, manual }: { state: UpdateState; manual: boolean }) {
  const t = useT()
  const install = state.install, check = state.check
  const kind = install?.kind ?? (state.capability.managed ? "managed" : undefined)
  const next = !manual ? null
    : check?.status === "available" ? t("pages:upgrade.unavailable.nextAvailable")
    : check ? t("pages:upgrade.unavailable.nextLater") : t("pages:upgrade.unavailable.nextUnchecked")
  const enable = manual && install ? enableOneClick(install.kind) : undefined
  return <Notice className="mb-3" data-upgrade-capability data-upgrade-unavailable data-install-kind={kind}>
    <p className="font-medium">{kind ? t("pages:upgrade.unavailable.titleKind", { kind: installLabel(kind) }) : t("pages:upgrade.unavailable.title")}</p>
    {state.capability.reason && <p className="mt-0.5 text-muted-foreground" data-upgrade-reason>{state.capability.reason}</p>}
    {next && <p className="mt-0.5 text-muted-foreground">{next}</p>}
    {enable && <p className="mt-0.5 type-footnote text-muted-foreground" data-upgrade-enable>{enable}</p>}
  </Notice>
}

function Command({ command }: { command: string }) {
  const t = useT()
  return <CodeBlock size="sm" className="mt-1.5 bg-muted/70 ring-0" data-upgrade-command>
    <CodeBlockContent wrap className="pe-10! type-code">{command}</CodeBlockContent>
    <CodeBlockCopyButton value={command} aria-label={t("pages:upgrade.copyCommand")} className="absolute end-1.5 top-1.5 text-muted-foreground" />
  </CodeBlock>
}

/** Installations Steve does not switch itself: this machine's commands, what is backed up, how to recover. */
function ManualGuide({ install, version, onVerify }: { install: Install; version: string; onVerify: () => void }) {
  const t = useT()
  const g = upgradeGuide(install, version)
  return <Card className="mt-3 space-y-4 type-callout" data-upgrade-guide={install.kind}>
    <div>
      <p className="font-medium">{t("pages:upgrade.guideTitle", { version: version.replace(/^v/, ""), label: g.label })}</p>
      <p className="mt-1 text-muted-foreground">{g.summary}</p>
    </div>
    <ol className="space-y-3">
      {g.steps.map((step, n) => <li key={n} className="flex gap-2.5" data-upgrade-step>
        <span className="mt-px flex size-5 shrink-0 items-center justify-center rounded-full bg-blue/10 type-label tabular-nums text-blue">{n + 1}</span>
        <div className="min-w-0 flex-1">
          <p>{step.title}</p>
          {step.command && <Command command={step.command} />}
          {step.note && <p className="mt-1 type-footnote text-muted-foreground">{step.note}</p>}
        </div>
      </li>)}
    </ol>
    <div data-upgrade-backup>
      <p className="type-label text-tertiary">{t("pages:upgrade.backup")}</p>
      <p className="mt-1 type-footnote text-muted-foreground break-words">{g.backup}</p>
    </div>
    <div data-upgrade-recovery>
      <p className="type-label text-tertiary">{t("pages:upgrade.recoveryTitle")}</p>
      <ul className="mt-1 list-disc space-y-1 ps-5 type-footnote text-muted-foreground break-words">{g.recovery.map((r, n) => <li key={n}>{r}</li>)}</ul>
    </div>
    <Button size="sm" variant="outline" onClick={onVerify} data-upgrade-verify-start>{t("pages:upgrade.doneCheck")}</Button>
  </Card>
}

/** The supervisor's journal as steps; the last one is this page's own check after reconnecting. */
function UpgradeProgress({ phase, verified, runnerKind }: { phase: string; verified: boolean; runnerKind?: string }) {
  const t = useT()
  const at = verified ? upgradeStages.length : stageIndex(phase)
  return <ol className="grid gap-1.5 sm:grid-cols-2" data-upgrade-progress aria-label={t("pages:upgrade.progressAria")}>
    {upgradeStages.map((s, n) => {
      const state = n < at ? "done" : n === at ? "current" : "pending"
      return <li key={s.key} className={cn("flex items-center gap-2 type-footnote", state === "pending" && "text-muted-foreground")} data-stage={state}>
        {state === "done" ? <CheckIcon className="size-3.5 text-success" /> : state === "current" ? <LoaderCircleIcon className="size-3.5 animate-spin text-primary" /> : <span className="size-3.5 flex items-center justify-center"><span className="size-1.5 rounded-full bg-muted-foreground/40" /></span>}
        {stageLabel(s.key, runnerKind)}
      </li>
    })}
  </ol>
}

const activityLabels = {
  resolving: "resolving", downloading: "downloading", verifying: "verifying", unpacking: "unpacking",
  signing: "signing", preflight: "preflight", probing: "probing", waiting: "waiting", stopping: "stopping",
  backing_up: "backingUp", switching: "switching", starting: "starting", checking: "checking",
  complete: "complete", rollback: "rollback",
} as const

function byteSize(bytes: number) {
  const unit = bytes >= 1024 ** 3 ? 3 : bytes >= 1024 ** 2 ? 2 : bytes >= 1024 ? 1 : 0
  return `${num(bytes / 1024 ** unit, { maximumFractionDigits: unit ? 1 : 0 })} ${["B", "KiB", "MiB", "GiB"][unit]}`
}

/** A persisted observation, not an estimate of the whole upgrade. Disconnection preserves it. */
function UpgradeActivity({ op, connected }: { op: UpdateOperation; connected: boolean }) {
  const t = useT(), p = op.progress!, active = updateActive(op)
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!active) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [active])
  const key = activityLabels[p.step as keyof typeof activityLabels] ?? "unknown"
  const label = t(`pages:upgrade.activity.${key}`)
  const percent = downloadPercent(p)
  const measuring = percent !== undefined && !!p.download
  return <div className="space-y-1.5 rounded-lg bg-muted/60 px-3.5 py-3" data-upgrade-activity={p.step}>
    <p className="flex items-center gap-2 font-medium">
      {active && connected && p.step !== "complete" && <LoaderCircleIcon className="size-3.5 shrink-0 animate-spin" />}
      {active ? label : t("pages:upgrade.activity.last", { step: label })}
    </p>
    {p.detail && <p className="break-words type-footnote text-muted-foreground">{p.detail}</p>}
    {measuring && <>
      <div role="progressbar" aria-label={t("pages:upgrade.activity.downloadProgress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent} className="h-1.5 overflow-hidden rounded-full bg-muted" data-upgrade-download>
        <div className="h-full rounded-full bg-primary transition-[width] duration-260 ease-(--ease-out)" style={{ width: `${percent}%` }} />
      </div>
      <p className="type-footnote tabular-nums">{byteSize(p.download!.received_bytes)} / {byteSize(p.download!.total_bytes)} · {percent}%</p>
    </>}
    <p className="type-footnote text-muted-foreground">
      {t("pages:upgrade.activity.updated", { time: date(p.updated_at, { hour: "numeric", minute: "2-digit", second: "2-digit" }) })}
      {active && <span> · {t("pages:upgrade.activity.elapsed", { time: clockDur(Math.max(0, now - p.started_at)) })}</span>}
    </p>
    {!connected && <p className="type-footnote text-muted-foreground">{t("pages:upgrade.activity.disconnected")}</p>}
  </div>
}

function UpgradeWaiting({ drain }: { drain: NonNullable<UpdateOperation["drain"]> }) {
  const t = useT()
  const running = Object.values(drain.running).filter(n => Number.isFinite(n) && n > 0).reduce((sum, n) => sum + n, 0)
  const blockers = drain.blockers ?? []
  const nodeStateUnknown = blockers.some(b => b.reason === "node_state_unavailable")
  const unlistedNodes = Math.max(0, drain.node - blockers.filter(b => b.kind === "node_turn" || b.kind === "node_session").length)
  const reason = (b: typeof blockers[number]) => {
    switch (b.reason) {
      case "waiting_for_node_work": return t("pages:upgrade.drain.nodeWork")
      case "node_state_unavailable": return t("pages:upgrade.drain.nodeUnknown")
      case "waiting_for_activity": return t("pages:upgrade.drain.activities", { count: b.count ?? 1 })
      case "waiting_for_source_release": return t("pages:upgrade.drain.release")
      default: return b.reason
    }
  }
  return <div className="space-y-1.5 type-footnote" data-upgrade-waiting>
    <p className="font-medium">{t(drain.ready ? "pages:upgrade.drain.ready" : "pages:upgrade.drain.waiting")}</p>
    {!drain.ready && <ul className="list-disc space-y-1 pl-4 text-muted-foreground">
      {blockers.length ? <>{blockers.map((b, n) => <li key={`${b.kind}:${b.attempt_id ?? b.session_id ?? n}`}>
        {b.item_id ? <a href={`#/items/${encodeURIComponent(b.item_id)}`} className="underline underline-offset-4">{b.item_id}</a> : b.session_id ? <a href={`#/sessions/${encodeURIComponent(b.session_id)}`} className="underline underline-offset-4">{b.session_id}</a> : null}
        {(b.item_id || b.session_id) && " · "}{reason(b)}
      </li>)}
        {!nodeStateUnknown && unlistedNodes > 0 && <li>{t("pages:upgrade.drain.unlistedNodes", { count: unlistedNodes, total: drain.node })}</li>}
      </> : <>
        {drain.node > 0 && <li>{t("pages:upgrade.drain.node", { count: drain.node })}</li>}
        {running > 0 && <li>{t("pages:upgrade.drain.activities", { count: running })}</li>}
        {drain.source_release_busy && <li>{t("pages:upgrade.drain.release")}</li>}
      </>}
      {drain.acp > 0 && <li>{t("pages:upgrade.drain.adapters", { count: drain.acp })}</li>}
      {!drain.blockers?.length && !drain.node && !running && !drain.acp && !drain.source_release_busy && <li>{t("pages:upgrade.drain.confirming")}</li>}
    </ul>}
    {drain.node > 0 && <a href="#/devices" className="inline-block underline underline-offset-4">{t("pages:upgrade.drain.devices")}</a>}
    <p className="text-muted-foreground">{t("pages:upgrade.drain.note")}</p>
  </div>
}

/** Where an upgrade's data is and what happened to it; the instance manager gets the paths. */
function UpgradeRecovery({ op, backup, install }: { op: UpdateOperation; backup: UpdateBackup | null; install?: Install }) {
  const t = useT()
  const db = backup?.database, home = install?.home
  const committed = committedFailure(op)
  const lines: string[] = []
  if (db) lines.push(t("pages:upgrade.recovery.backup", { db }))
  else if (["staging", "ready", "draining", "stopping"].includes(op.phase)) lines.push(t("pages:upgrade.recovery.beforeStop"))
  if (op.phase === "rolling_back" || op.phase === "rolled_back_pending_health") lines.push(t("pages:upgrade.recovery.rollingBack"))
  if (op.phase === "rolled_back") lines.push(t("pages:upgrade.recovery.rolledBack", { prev: op.previous ? t("pages:upgrade.recovery.prevParen", { v: op.previous }) : "", dir: backup?.dir ?? t("pages:upgrade.recovery.dirFallback") }))
  else if (committed) lines.push(t("pages:upgrade.recovery.committed", { log: home ? `${home}/logs/hub.log` : t("pages:upgrade.recovery.logFallback"), prev: op.previous ?? t("pages:upgrade.recovery.prevFallback") }))
  else if (op.phase === "failed") lines.push(db ? t("pages:upgrade.recovery.failedWithDb") : t("pages:upgrade.recovery.failedNoDb"))
  if (op.runner?.log && (updateActive(op) || ["failed", "rolled_back"].includes(op.phase))) lines.push(t("pages:upgrade.runnerLog", { log: op.runner.log }))
  if (!lines.length) return null
  return <div className="space-y-1 type-footnote text-muted-foreground break-words" data-upgrade-recovery={op.phase}>{lines.map((l, n) => <p key={n}>{l}</p>)}</div>
}

/** After the restart, read back what actually runs; only all four passing is "已升级". Never refreshes the page itself. */
export function UpgradeVerify({ target, embedded, onClose, onResult }: { target: string; embedded?: boolean; onClose?: () => void; onResult?: (done: boolean) => void }) {
  const t = useT()
  const [health, setHealth] = useState<Health | null>(null)
  const [round, setRound] = useState(0)
  const [checking, setChecking] = useState(true)
  const result = verifyUpgrade(target, health, LOADED_WEB_VERSION)
  useEffect(() => { onResult?.(result.done) }, [result.done])
  useEffect(() => {
    let stopped = false, tries = 0, timer: ReturnType<typeof setTimeout>
    const read = async () => {
      setChecking(true)
      let h: Health | null = null
      try { h = await updates.health() } catch { h = null }
      if (stopped) return
      setHealth(h)
      // a restart takes a moment: keep reading while the served pair isn't there yet, about two minutes
      const served = verifyUpgrade(target, h, LOADED_WEB_VERSION)
      if (!served.done && !served.needsReload && ++tries < 40) timer = setTimeout(read, 3000)
      else setChecking(false)
    }
    void read()
    return () => { stopped = true; clearTimeout(timer) }
  }, [target, round])
  const v = target.replace(/^v/, "")
  return <div className={cn("space-y-2", !embedded && cn(LIFT, "mt-3 p-4 type-callout"))} data-upgrade-verify={result.done ? "passed" : checking ? "checking" : "failed"} role="status">
    <p className={cn("font-medium", result.done && "text-success")}>
      {result.done ? t("pages:upgrade.verify.done", { v }) : result.needsReload ? t("pages:upgrade.verify.needsReload", { v }) : checking ? t("pages:upgrade.verify.checking") : t("pages:upgrade.verify.notYet", { v })}
    </p>
    <ul className="space-y-1">
      {result.items.map(i => <li key={i.key} className="flex items-center gap-2 type-footnote" data-verify-item={i.key} data-ok={i.ok}>
        {i.ok ? <CheckIcon className="size-3.5 text-success" /> : <CircleAlertIcon className={cn("size-3.5", checking ? "text-muted-foreground" : "text-destructive")} />}
        <span>{i.label}</span><span className="min-w-0 truncate font-mono text-muted-foreground">{i.detail}</span>
      </li>)}
    </ul>
    {result.needsReload && <p className="type-footnote text-muted-foreground">{t("pages:upgrade.verify.reloadNote")}</p>}
    {!result.done && !checking && !result.needsReload && <p className="type-footnote text-muted-foreground">{t("pages:upgrade.verify.retryNote")}</p>}
    <div className="flex flex-wrap gap-2">
      {result.needsReload && <Button size="sm" variant="outline" onClick={() => location.reload()} data-upgrade-refresh>{t("pages:upgrade.verify.reloadPage")}</Button>}
      {!result.done && <Button size="sm" variant="ghost" disabled={checking} onClick={() => setRound(r => r + 1)} data-upgrade-verify-again>{t("pages:upgrade.verify.again")}</Button>}
      {onClose && <Button size="sm" variant="ghost" onClick={onClose}>{t("pages:upgrade.verify.collapse")}</Button>}
    </div>
  </div>
}
