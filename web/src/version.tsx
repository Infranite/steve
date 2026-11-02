import { HubError, Unauthenticated } from "@/api"
import { useEffect, useRef, useState } from "react"
import { updates, updateActive, updateKey, repairKey, sameReleaseVersion, updatePhaseLabel, type UpdateState, type UpdateOperation } from "@/updates"
import { CopyIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Group, Row, SectionLabel } from "@/ui"
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
      <p className="text-muted-foreground" data-upgrade-capability>{!state.capability.managed ? "当前为源码或非托管运行，仅支持检查更新。请按部署方式更新并重启服务。" : !state.capability.can_execute ? "当前不能自动升级。请查看下方原因。" : "升级将备份数据并自动重启服务；未完成任务由服务恢复流程继续处理。"}</p>
      {state.capability.reason && <p className="type-footnote text-muted-foreground">{state.capability.reason}</p>}
      {check && <div data-upgrade-check-status={check.status}>
        <p>{messages[check.status]}</p>
        {check.error && <p className="mt-1 text-destructive">{check.error}</p>}
        <p className="mt-1 type-footnote text-muted-foreground">检查时间：{new Date(check.checked_at).toLocaleString()}</p>
        {releaseURL && <a className="underline underline-offset-4" href={releaseURL} target="_blank" rel="noopener noreferrer">查看 Release 说明</a>}
        {check.notes && <details className="mt-2"><summary className="cursor-pointer">更新内容</summary><p className="mt-2 whitespace-pre-wrap break-words text-muted-foreground">{check.notes}</p></details>}
        {canExecute && repair && !active && <Button className="mt-3" size="sm" disabled={busy || !!connection} onClick={() => void action("repair")} data-upgrade-repair>重新安装 {check.version}，对齐前后端并重启</Button>}
        {canExecute && !repair && check.status === "available" && check.version && !active && op?.version !== check.version && <Button className="mt-3" size="sm" disabled={busy || !!connection} onClick={() => void action("execute")} data-upgrade-execute>升级到 {check.version} 并重启</Button>}
      </div>}
      {op && <div className="rounded-lg border p-3 space-y-2" data-upgrade-phase={op.phase} role="status">
        <p>{updatePhaseLabel[op.phase]} · <span className="font-mono break-all">{op.version}</span></p>
        {op.phase === "awaiting_approval" && <><p className="text-muted-foreground">{op.requested_by} 请求升级。批准后将更新前后端并重启服务。</p>{canExecute && <div className="flex flex-wrap gap-2"><Button size="sm" disabled={busy || !!connection} onClick={() => void action("approve")} data-upgrade-approve>批准升级并重启</Button><Button size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("reject")} data-upgrade-reject>拒绝请求</Button></div>}</>}
        {active && op.phase !== "awaiting_approval" && <p className="text-muted-foreground">重启期间连接可能暂时中断，页面会自动恢复进度。你可以离开此页，稍后继续查看。</p>}
        {canExecute && !repair && ["failed", "rolled_back"].includes(op.phase) && check?.status === "available" && check.version === op.version && <Button size="sm" variant="outline" disabled={busy || !!connection} onClick={() => void action("retry")} data-upgrade-retry>重试升级并重启</Button>}
        {op.error && <p className="text-destructive break-words">{op.error}</p>}
        {op.phase === "rolled_back" && <p className="text-muted-foreground">本次升级未成功，已恢复到旧版本。请查看错误原因后处理。</p>}
        {op.phase === "succeeded" && <><p className="text-muted-foreground">新版本已运行。当前页面不会自动刷新，输入草稿会保留；准备好后重新载入。</p><Button size="sm" variant="outline" onClick={() => location.reload()} data-upgrade-refresh>重新载入页面</Button></>}
      </div>}
    </div>}
    {error && <p className="mt-2 type-footnote text-destructive" role="alert">{error}</p>}
  </div>
}
