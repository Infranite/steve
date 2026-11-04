import { HubError, Unauthenticated } from "@/api"
import { useEffect, useRef, useState } from "react"
import { updates, updateActive, updateKey, repairKey, sameReleaseVersion, updatePhaseLabel, type UpdateBackup, type UpdateState, type UpdateOperation } from "@/updates"
import { installLabel, stageIndex, upgradeGuide, upgradeStages, verifyUpgrade, type Health, type Install } from "@/upgrade-guide"
import { CheckIcon, CircleAlertIcon, CopyIcon, LoaderCircleIcon, RefreshCwIcon } from "lucide-react"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { CodeBlock, CodeBlockContent, CodeBlockCopyButton } from "@/components/ui/code-block"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Group, Row, SectionLabel } from "@/ui"
import { cn } from "cn"
import { LOADED_WEB_VERSION, st, toast, useStore } from "@/store"

/** Release builds are named `YYYYMMDD-HHMMSS-<commit>`; anything else (dev, a hand-set STEVE_VERSION) is shown as is. */
export function readableVersion(v: string): { when?: string; commit?: string; raw: string } {
  const m = /^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})\d{2}-([0-9a-f]{6,40})(.*)$/i.exec(v)
  return m ? { when: `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}`, commit: m[6].slice(0, 8) + m[7], raw: v } : { raw: v }
}

function VersionValue({ v, label }: { v: string; label: string }) {
  if (!v) return <span className="type-callout text-muted-foreground">正在获取…</span>
  const r = readableVersion(v)
  return <span className="flex min-w-0 items-center gap-1">
    <span className="type-callout tabular-nums text-muted-foreground" title={r.raw} data-version-value>
      {r.when ? <>{r.when}<span className="mx-1.5 text-tertiary">·</span><span className="font-mono text-[12.5px]">{r.commit}</span></> : <span className="font-mono text-[12.5px]">{r.raw}</span>}
    </span>
    <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={`复制${label}`} className="text-muted-foreground" onClick={() => { navigator.clipboard?.writeText(r.raw).catch(() => {}); toast(`已复制 ${r.raw}`) }} />}><CopyIcon /></TooltipTrigger><TooltipContent>复制完整版本号</TooltipContent></Tooltip>
  </span>
}

/** Settings · 关于. The frontend is the build this tab loaded, independent of the Hub's running build. */
export function AboutVersions() {
  useStore()
  const newer = !!st.webVersion && st.webVersion !== LOADED_WEB_VERSION
  const differ = !!st.version && st.version !== LOADED_WEB_VERSION
  return <section data-version-info>
    <SectionLabel>版本</SectionLabel>
    <Group>
      <Row title="前端" sub="当前页面"><VersionValue v={LOADED_WEB_VERSION} label="前端版本" /></Row>
      <Row title="后端" sub="Hub"><VersionValue v={st.version} label="后端版本" /></Row>
      {newer && <Row title="可用前端" sub="已经发布，重新载入后生效" data-web-update>
        <span className="flex items-center gap-2"><VersionValue v={st.webVersion} label="可用前端版本" /><Button size="sm" variant="outline" onClick={() => location.reload()}><RefreshCwIcon />重新载入</Button></span></Row>}
    </Group>
    <UpgradeSettings />
    {differ && !newer && <p className="mt-2 type-footnote text-muted-foreground" data-version-differ>前端和后端不是同一次构建。建议先保存工作，再重新载入与后端匹配的前端版本。</p>}
  </section>
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
  const [state, setState] = useState<UpdateState | null>(null)
  const [busy, setBusy] = useState(false)
  const [connection, setConnection] = useState("")
  const [error, setError] = useState("")
  const pendingReceipt = useRef<PendingReceipt | null>(null)
  const generation = useRef(0), mounted = useRef(false), mutating = useRef(false)
  useEffect(() => {
    mounted.current = true
    let stopped = false, timer: ReturnType<typeof setTimeout>
    const read = async () => {
      const epoch = generation.current
      if (!mutating.current) {
        try {
          const next = await updates.status()
          if (!stopped && epoch === generation.current) { setState(next); setConnection(""); if (confirmsReceipt(pendingReceipt.current, next.operation)) { pendingReceipt.current = null; setError("") } }
        } catch {
          if (!stopped && epoch === generation.current) setConnection("暂时无法连接，正在重新读取升级状态。不会重复提交升级。")
        }
      }
      if (!stopped) timer = setTimeout(read, 2000)
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
        if (mounted.current) setError(e instanceof Unauthenticated ? "登录已过期，请重新登录后再操作。" : e instanceof Error ? e.message : "请求失败，请重试")
      } else if (mounted.current) {
        setConnection("正在确认提交结果，请稍候…")
        setError("提交结果尚未确认，正在读取服务端记录。请先等待相同目标的回执，页面不会自动重复提交。")
      }
    } finally { mutating.current = false; if (mounted.current) setBusy(false) }
  }
  const op = state?.operation ?? null, active = updateActive(op)
  const canExecute = state?.capability.managed && state.capability.can_execute
  const check = state?.check
  const install = state?.install
  // the instance manager gets this machine's commands; others only learn who upgrades
  const manual = !!install && install.kind !== "managed" && install.home !== undefined
  const [verifying, setVerifying] = useState<string | null>(null)
  const [verified, setVerified] = useState<string | null>(null)
  const pairMismatch = !!state && !!check?.version && (!sameReleaseVersion(state.current.hub, check.version) || !sameReleaseVersion(state.current.web, check.version))
  const repair = !!state && check?.status === "available" && !!check.version && pairMismatch && (sameReleaseVersion(state.current.hub, check.version) || (op?.phase === "succeeded" && sameReleaseVersion(op.version, check.version)))
  const messages = {
    available: repair ? `前后端版本未对齐，可重新安装 ${check?.version ?? ""} 的成对升级包。` : `发现新版本 ${check?.version ?? ""}`,
    up_to_date: "已是最新发布版本", ahead: "当前版本领先于最新 Release，不会降级。",
    unknown_current: "当前版本无法与 Release 比较。请使用可识别版本的托管安装后升级。",
    no_release: "仓库尚未发布可用 Release", error: "版本检查失败", unsupported_platform: "当前平台暂无可用升级包",
  }
  const releaseURL = (() => { try { const u = new URL(check?.release_url ?? ""); return u.protocol === "https:" ? u.href : undefined } catch { return undefined } })()
  return <div className="mt-5" data-upgrade-settings>
    <SectionLabel>检查与升级</SectionLabel>
    <Group>
      <Row title="GitHub Release" sub={state?.repository ?? "正在读取升级信息…"}>
        <Button size="sm" variant="outline" disabled={!state || busy || active || !!connection} onClick={() => void action("check")} data-upgrade-check><RefreshCwIcon />{busy ? "请稍候…" : "检查更新"}</Button>
      </Row>
    </Group>
    {connection && <p role="status" className="mt-2 type-footnote text-muted-foreground" data-upgrade-connection>{connection}</p>}
    {state && <div className="mt-3 space-y-3 type-callout">
      <p className="text-muted-foreground" data-upgrade-capability data-install-kind={install?.kind}>{install && install.kind !== "managed" ? `${installLabel[install.kind]}：${manual ? "这里检查更新，升级需要按下方步骤在这台机器上完成。" : "升级需由实例管理员在这台机器上完成。"}` : !state.capability.managed ? "当前为源码或非托管运行，仅支持检查更新。请按部署方式更新并重启服务。" : !state.capability.can_execute ? "当前不能自动升级。请查看下方原因。" : "托管安装：可在这里升级。升级前备份数据库，新版本验证不通过会自动恢复旧版本和数据。"}</p>
      {state.capability.reason && state.capability.managed && !state.capability.can_execute && <p className="type-footnote text-muted-foreground">{state.capability.reason}</p>}
      {check && <div data-upgrade-check-status={check.status}>
        <p>{messages[check.status]}</p>
        {check.error && <p className="mt-1 text-destructive">{check.error}</p>}
        <p className="mt-1 type-footnote text-muted-foreground">检查时间：{new Date(check.checked_at).toLocaleString()}</p>
        {releaseURL && <a className="underline underline-offset-4" href={releaseURL} target="_blank" rel="noopener noreferrer">查看 Release 说明</a>}
        {check.notes && <details className="mt-2"><summary className="cursor-pointer">更新内容</summary><div className="mt-2 max-h-[60vh] overflow-auto break-words text-muted-foreground" data-release-notes><Markdown text={check.notes} className="type-callout [&_h1]:mt-0 [&_h1]:text-[17px] [&_h2]:text-[15px] [&_h3]:text-[14px]" /></div></details>}
        {canExecute && repair && !active && <Button className="mt-3" size="sm" disabled={busy || !!connection} onClick={() => void action("repair")} data-upgrade-repair>重新安装 {check.version}，对齐前后端并重启</Button>}
        {canExecute && !repair && check.status === "available" && check.version && !active && op?.version !== check.version && <Button className="mt-3" size="sm" disabled={busy || !!connection} onClick={() => void action("execute")} data-upgrade-execute>升级到 {check.version} 并重启</Button>}
      </div>}
      {manual && install && check?.status === "available" && check.version && <ManualGuide install={install} version={check.version} onVerify={() => setVerifying(check.version)} />}
      {verifying && <UpgradeVerify target={verifying} onClose={() => setVerifying(null)} />}
      {op && <div className="rounded-lg border p-3 space-y-2" data-upgrade-phase={op.phase} role="status">
        <p>{updatePhaseLabel[op.phase]} · <span className="font-mono break-all">{op.version}</span></p>
        {op.phase === "awaiting_approval" && <><p className="text-muted-foreground">{op.requested_by} 请求升级。批准后将更新前后端并重启服务。</p>{canExecute && <div className="flex flex-wrap gap-2"><Button size="sm" disabled={busy || !!connection} onClick={() => void action("approve")} data-upgrade-approve>批准升级并重启</Button><Button size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("reject")} data-upgrade-reject>拒绝请求</Button></div>}</>}
        {(active || op.phase === "succeeded") && stageIndex(op.phase) >= 0 && <UpgradeProgress phase={op.phase} verified={verified === op.id} />}
        {active && op.phase !== "awaiting_approval" && <p className="text-muted-foreground">重启期间连接可能暂时中断，页面会自动恢复进度。你可以离开此页，稍后继续查看。</p>}
        {canExecute && !repair && ["failed", "rolled_back"].includes(op.phase) && check?.status === "available" && check.version === op.version && <Button size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("retry")} data-upgrade-retry>重试升级并重启</Button>}
        {op.error && <p className="text-destructive break-words">{op.error}</p>}
        {op.phase !== "awaiting_approval" && op.phase !== "succeeded" && <UpgradeRecovery op={op} backup={state.backup ?? null} install={install} />}
        {op.phase === "succeeded" && <UpgradeVerify target={op.version} embedded onResult={done => setVerified(done ? op.id : null)} />}
      </div>}
    </div>}
    {error && <p className="mt-2 type-footnote text-destructive" role="alert">{error}</p>}
  </div>
}

function Command({ command }: { command: string }) {
  return <CodeBlock size="sm" className="mt-1.5 bg-muted/70 ring-0" data-upgrade-command>
    <CodeBlockContent wrap className="pe-10! type-code">{command}</CodeBlockContent>
    <CodeBlockCopyButton value={command} aria-label="复制命令" className="absolute end-1.5 top-1.5 text-muted-foreground" />
  </CodeBlock>
}

/** Installations Steve does not switch itself: this machine's commands, what is backed up, how to recover. */
function ManualGuide({ install, version, onVerify }: { install: Install; version: string; onVerify: () => void }) {
  const g = upgradeGuide(install, version)
  return <div className="rounded-lg border p-3 space-y-3" data-upgrade-guide={install.kind}>
    <div>
      <p className="font-medium">升级到 {version.replace(/^v/, "")} · {g.label}</p>
      <p className="mt-1 text-muted-foreground">{g.summary}</p>
    </div>
    <ol className="space-y-3">
      {g.steps.map((step, n) => <li key={n} className="flex gap-2.5" data-upgrade-step>
        <span className="mt-px flex size-5 shrink-0 items-center justify-center rounded-full bg-muted type-footnote tabular-nums">{n + 1}</span>
        <div className="min-w-0 flex-1">
          <p>{step.title}</p>
          {step.command && <Command command={step.command} />}
          {step.note && <p className="mt-1 type-footnote text-muted-foreground">{step.note}</p>}
        </div>
      </li>)}
    </ol>
    <div data-upgrade-backup>
      <p className="type-label text-muted-foreground">备份</p>
      <p className="mt-1 type-footnote text-muted-foreground break-words">{g.backup}</p>
    </div>
    <div data-upgrade-recovery>
      <p className="type-label text-muted-foreground">失败时如何恢复</p>
      <ul className="mt-1 list-disc space-y-1 ps-5 type-footnote text-muted-foreground break-words">{g.recovery.map((r, n) => <li key={n}>{r}</li>)}</ul>
    </div>
    <Button size="sm" variant="outline" onClick={onVerify} data-upgrade-verify-start>我已完成升级，核对版本</Button>
  </div>
}

/** The supervisor's journal as steps; the last one is this page's own check after reconnecting. */
function UpgradeProgress({ phase, verified }: { phase: string; verified: boolean }) {
  const at = verified ? upgradeStages.length : stageIndex(phase)
  return <ol className="grid gap-1.5 sm:grid-cols-2" data-upgrade-progress aria-label="升级进度">
    {upgradeStages.map((s, n) => {
      const state = n < at ? "done" : n === at ? "current" : "pending"
      return <li key={s.label} className={cn("flex items-center gap-2 type-footnote", state === "pending" && "text-muted-foreground")} data-stage={state}>
        {state === "done" ? <CheckIcon className="size-3.5 text-success" /> : state === "current" ? <LoaderCircleIcon className="size-3.5 animate-spin text-primary" /> : <span className="size-3.5 flex items-center justify-center"><span className="size-1.5 rounded-full bg-muted-foreground/40" /></span>}
        {s.label}
      </li>
    })}
  </ol>
}

/** Where a managed upgrade's data is and what happened to it; the instance manager gets the paths. */
function UpgradeRecovery({ op, backup, install }: { op: UpdateOperation; backup: UpdateBackup | null; install?: Install }) {
  const db = backup?.database, home = install?.home
  const committed = op.phase === "failed" && !!op.error?.includes("升级已提交")
  const lines: string[] = []
  if (db) lines.push(`升级前数据库备份（已校验）：${db}`)
  else if (["staging", "ready", "draining", "stopping"].includes(op.phase)) lines.push("停止旧版本后会先备份数据库并校验，再切换版本。")
  if (op.phase === "rolling_back" || op.phase === "rolled_back_pending_health") lines.push("新版本没有通过验证，正在把版本和数据库恢复到升级前。")
  if (op.phase === "rolled_back") lines.push(`已自动恢复：版本切回升级前${op.previous ? `（${op.previous}）` : ""}，数据库恢复为升级前的备份。失败的新数据库另存在 ${backup?.dir ?? "升级记录目录"} 中，名称含 failed，可用于排查；问题处理后可以重试。`)
  else if (committed) lines.push(`新版本已切换并开始处理业务，按设计不自动回退，新数据库保留。先查看 ${home ? `${home}/logs/hub.log` : "Hub 日志"} 并重启服务；必须回到旧版本时，停止服务，把 current 指回 ${op.previous ?? "旧版本目录"}，再用上面的备份覆盖数据库（升级后产生的数据会丢失）。`)
  else if (op.phase === "failed") lines.push(db ? "升级没有完成，旧版本和数据库仍在使用；备份保留供查验。可以处理错误后重试。" : "升级在切换版本前停止，旧版本和数据库没有改动，可以处理错误后重试。")
  if (!lines.length) return null
  return <div className="space-y-1 type-footnote text-muted-foreground break-words" data-upgrade-recovery={op.phase}>{lines.map((l, n) => <p key={n}>{l}</p>)}</div>
}

/** After the restart, read back what actually runs; only all four passing is "已升级". Never refreshes the page itself. */
export function UpgradeVerify({ target, embedded, onClose, onResult }: { target: string; embedded?: boolean; onClose?: () => void; onResult?: (done: boolean) => void }) {
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
  return <div className={cn("space-y-2", !embedded && "rounded-lg border p-3")} data-upgrade-verify={result.done ? "passed" : checking ? "checking" : "failed"} role="status">
    <p className={cn("font-medium", result.done && "text-success")}>
      {result.done ? `已升级到 ${v}：后端、前端和服务健康均已核对` : result.needsReload ? `服务已是 ${v}，重新载入页面后完成核对` : checking ? "正在核对版本与服务健康…" : `还没有核对通过：服务未全部到 ${v}`}
    </p>
    <ul className="space-y-1">
      {result.items.map(i => <li key={i.key} className="flex items-center gap-2 type-footnote" data-verify-item={i.key} data-ok={i.ok}>
        {i.ok ? <CheckIcon className="size-3.5 text-success" /> : <CircleAlertIcon className={cn("size-3.5", checking ? "text-muted-foreground" : "text-destructive")} />}
        <span>{i.label}</span><span className="min-w-0 truncate font-mono text-muted-foreground">{i.detail}</span>
      </li>)}
    </ul>
    {result.needsReload && <p className="type-footnote text-muted-foreground">当前页面不会自动刷新，输入草稿会保留；准备好后重新载入。</p>}
    {!result.done && !checking && !result.needsReload && <p className="type-footnote text-muted-foreground">如果刚重启，稍等后重新核对；仍不一致时查看 Hub 日志，并按上面的恢复方式处理。</p>}
    <div className="flex flex-wrap gap-2">
      {result.needsReload && <Button size="sm" variant="outline" onClick={() => location.reload()} data-upgrade-refresh>重新载入页面</Button>}
      {!result.done && <Button size="sm" variant="ghost" disabled={checking} onClick={() => setRound(r => r + 1)} data-upgrade-verify-again>重新核对</Button>}
      {onClose && <Button size="sm" variant="ghost" onClick={onClose}>收起</Button>}
    </div>
  </div>
}
