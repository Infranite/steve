import { useEffect, useRef, useState } from "react"
import { ExternalLinkIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { channelsApi, type FeishuRegistration } from "@/api/channels"
import { vaultApi } from "@/api/vault"
import { isNotWired } from "@/api"
import { go } from "@/ui"
import { Select } from "@/view-shared"
import { officialConfirmationUrl, registrationLabel, registrationPending, remainingSeconds } from "./registration-state"

const errorText = (e: unknown) => e instanceof Error ? e.message : "暂时无法连接，请重试"

/** A server-owned attempt survives navigation. Reopening this page never starts an application. */
export function FeishuRegistrationPanel() {
  const [brand, setBrand] = useState<"feishu" | "lark">("feishu")
  const [r, setRegistration] = useState<FeishuRegistration | null>(null)
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false)
  const [vaultReady, setVaultReady] = useState<boolean | null>(null)
  const [error, setError] = useState(""), [unavailable, setUnavailable] = useState(false)
  const [now, setNow] = useState(Date.now()), [reload, setReload] = useState(0)
  const epoch = useRef(0), mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; epoch.current++ } }, [])
  useEffect(() => {
    let stop = false
    setLoading(true)
    channelsApi.registrationCurrent().then((next) => {
      if (!stop) { setRegistration(next); setError(""); setUnavailable(false) }
    }).catch((e) => {
      if (!stop) { setUnavailable(isNotWired(e)); setError(isNotWired(e) ? "暂时无法读取快速接入状态。请重试，或使用已有应用入口。" : errorText(e)) }
    }).finally(() => { if (!stop) setLoading(false) })
    return () => { stop = true }
  }, [reload])
  useEffect(() => {
    let stop = false
    vaultApi.status().then((s) => { if (!stop) setVaultReady(s.initialized) }, () => {})
    return () => { stop = true }
  }, [reload])
  const pending = r ? registrationPending(r) : false
  useEffect(() => {
    if (!r || !pending) return
    let stop = false, timer: ReturnType<typeof setTimeout>
    const id = r.id, interval = Math.max(2, Math.min(60, r.interval || 5)) * 1000
    const tick = async () => {
      const version = epoch.current
      try {
        const next = await channelsApi.registration(id)
        if (!stop && version === epoch.current) { setRegistration(next); setError("") }
      } catch (e) { if (!stop && version === epoch.current) setError("连接暂时中断，接入进度仍保留。" + errorText(e)) }
      if (!stop) timer = setTimeout(tick, interval)
    }
    timer = setTimeout(tick, interval)
    const clock = setInterval(() => setNow(Date.now()), 1000)
    return () => { stop = true; clearTimeout(timer); clearInterval(clock) }
  }, [r?.id, r?.state, r?.error, r?.interval, pending])

  const act = async (fn: () => Promise<FeishuRegistration>) => {
    if (busy) return
    ++epoch.current // a poll started before this mutation cannot overwrite its result
    setBusy(true); setError("")
    try {
      const next = await fn()
      if (mounted.current) { setRegistration(next); setNow(Date.now()) }
    } catch (e) { if (mounted.current) setError(errorText(e)) }
    finally { ++epoch.current; if (mounted.current) setBusy(false) }
  }
  const url = officialConfirmationUrl(r?.verification_uri_complete)
  const left = remainingSeconds(r?.expires_at ?? 0, now)
  const startAllowed = vaultReady !== false && !loading && !busy && !unavailable && !error && !pending && r?.state !== "credentials_saved"
  const terminal = r && !pending && r.state !== "credentials_saved" && r.state !== "succeeded"
  return <div className="flex flex-col gap-3 rounded-xl border p-4" data-app-registration>
    <div className="type-body font-medium">通过官方页面快速接入</div>
    <p className="type-callout text-muted-foreground">打开飞书或 Lark 的官方页面，确认后 Steve 会自动保存应用凭据并检查连接。无需复制密钥，也不需要公网域名。</p>
    <p className="type-footnote text-muted-foreground">适用于个人智能体。团队共享机器人可使用下方的企业自建应用入口；权限和管理员审批以你的组织规则为准。</p>
    {vaultReady === false && <div role="status" className="flex flex-wrap items-center gap-2 type-callout">先启用保险箱，用于加密保存应用凭据。<Button variant="outline" onClick={() => go("#/vault")}>去启用保险箱</Button></div>}
    {loading ? <div role="status" className="flex items-center gap-2 type-callout"><Spinner />正在恢复接入进度</div> : r && <div className="flex flex-col gap-2 rounded-lg bg-muted/50 p-3" data-registration-state={r.state}>
      <div role="status" className="flex items-center gap-2 type-callout font-medium">{pending && <Spinner />}{registrationLabel(r)}</div>
      {r.state === "pending" && <>
        {url && <Button nativeButton={false} render={<a href={url} target="_blank" rel="noreferrer noopener" />} data-registration-link><ExternalLinkIcon />打开官方确认页面</Button>}
        {!url && <p role="alert" className="type-callout text-destructive">没有收到有效的官方确认地址，请取消后重新开始。</p>}
        {r.user_code && <div className="type-callout">如需输入确认码：<code className="select-all font-mono">{r.user_code}</code></div>}
        <p className="type-footnote text-muted-foreground">{left ? `链接还剩 ${Math.floor(left / 60)} 分 ${left % 60} 秒` : "正在确认链接是否已过期"}。确认后这里会自动继续。</p>
      </>}
      {r.app_may_exist && r.state !== "cancelled" && <p className="type-footnote text-muted-foreground">官方侧可能已创建应用。请先检查已有应用，不要直接重复创建。</p>}
      {r.error && <p role="alert" className="break-words type-callout text-destructive">{r.error}</p>}
      {r.state === "credentials_saved" && r.error && <Button variant="outline" disabled={busy} onClick={() => void act(() => channelsApi.registrationRetry(r.id))}>重试连接检查</Button>}
      {r.state === "succeeded" && <>
        <p className="type-callout text-muted-foreground">机器人接入已完成。可以查看实际权限和连接状态；本人授权可稍后开启。</p>
        {r.connection_id && <Button onClick={() => go("#/channels/acc/" + r.connection_id)}>查看连接与权限</Button>}
      </>}
      {(pending || r.state === "credentials_saved") && <>
        <p className="type-footnote text-muted-foreground">可以离开此页，稍后回来继续。点击取消将停止 Steve 的接入；未绑定的本地凭据也会移除，官方页面上已经创建的应用不会自动删除。</p>
        <Button variant="outline" disabled={busy} onClick={() => void act(() => channelsApi.registrationCancel(r.id))}>取消本次接入</Button>
      </>}
      {r.state === "cancelled" && <p className="type-footnote text-muted-foreground">Steve 已停止接入。如果你已在官方页面确认，应用可能已创建，可通过已有应用入口关联。</p>}
    </div>}
    {error && <div role="alert" className="flex flex-col items-start gap-2 type-callout text-destructive">{error}
      {<Button size="sm" variant="outline" disabled={busy || loading} onClick={() => { ++epoch.current; setReload((x) => x + 1) }}>重新读取进度</Button>}
    </div>}
    {!pending && r?.state !== "credentials_saved" && <div className="flex flex-wrap items-center gap-2">
      <Select testId="registration-brand" value={brand} onChange={(v) => setBrand(v as "feishu" | "lark")} options={[{ v: "feishu", l: "飞书" }, { v: "lark", l: "Lark" }]} />
      <Button disabled={!startAllowed} onClick={() => void act(() => channelsApi.registrationStart(brand))} data-registration-start>{busy && <Spinner />}{terminal ? "重新开始接入" : r?.state === "succeeded" ? "接入另一个应用" : "开始接入"}</Button>
    </div>}
    {terminal && <p className="type-footnote text-muted-foreground">重新开始前，可以先在官方后台确认是否已有应用，避免重复创建。</p>}
  </div>
}
