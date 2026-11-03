/* 飞书 / Lark (feishu-lark-cli.md §2.1/§2.4/§4, feishu.md §2.5; prototype channels.tsx AddChannelAccDialog): one way in on
 * the 接入 page. The step-by-step guide for the developer console, the bot's App ID / Secret with the permission check
 * from `verify`, and “授权你本人” through the same app (OAuth or device code). Only this file knows Feishu's flow. */
import { useEffect, useRef, useState } from "react"
import { CheckIcon, CircleHelpIcon, ExternalLinkIcon, LockIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { go, SectionLabel } from "@/ui"
import { Select, ChoiceCards } from "@/view-shared"
import { toast } from "@/store"
import { HubError } from "@/api"
import { useBusy, report } from "@/accounts/load"
import { accountsApi, type RAdapter, type RDevice, type RFieldDef } from "@/api/accounts"
import { vaultApi } from "@/api/vault"
import { channelsApi, FEISHU_APP, type PermissionCheck, type Persona } from "@/api/channels"
import { FeishuRegistrationPanel } from "./registration"
import { officialConfirmationUrl } from "./registration-state"

/* ───────── the permission table (feishu.md §2.5 with feishu-lark-cli.md §3) ─────────
 * What to enable in the developer console, shown before connecting; after `verify` the Hub's check replaces it. */
const LEVEL_LABEL: Record<PermissionCheck["level"], string> = { required: "必须", suggested: "建议", directory: "通讯录同步", optional: "可选" }
export const BOT_PERMS: [string, string, PermissionCheck["level"]][] = [
  ["im:message", "发消息、回复", "required"], ["im:message:send_as_bot", "以应用身份发消息", "required"], ["im:message.p2p_msg:readonly", "收用户发给机器人的私聊", "required"],
  ["im:message.group_at_msg:readonly", "收群里 @机器人 的消息", "required"], ["im:message.group_msg", "收群里所有消息（敏感权限，需要租户管理员审批）", "suggested"],
  ["im:chat:readonly", "群信息、成员、机器人所在的群", "required"], ["im:resource", "取消息里的图片和文件", "required"],
  ["contact:contact.base:readonly", "通讯录同步", "directory"], ["contact:department.base:readonly", "通讯录同步：部门", "directory"], ["contact:user.base:readonly", "通讯录同步：成员", "directory"], ["contact:user.email:readonly", "通讯录同步：邮箱", "directory"],
]
export const USER_PERMS = "offline_access、im:message、im:message:readonly、im:message.p2p_msg:get_as_user、im:message.group_msg:get_as_user、im:chat:read；要以你本人身份发送再开 im:message.send_as_user"
const STATUS: Record<PermissionCheck["status"], [string, any, string]> = { granted: ["已开通", CheckIcon, "text-success"], missing: ["未开通", XIcon, "text-destructive"], unknown: ["以控制台为准", CircleHelpIcon, "text-muted-foreground"], not_requested: ["没有申请", CircleHelpIcon, "text-muted-foreground"] }

export function PermissionList({ checks }: { checks: PermissionCheck[] | null }) {
  const rows: PermissionCheck[] = checks ?? BOT_PERMS.map(([scope, purpose, level]) => ({ scope, purpose, level, status: "unknown" }))
  const missing = rows.filter((r) => r.status === "missing" && r.level === "required")
  return (
    <div className="flex flex-col gap-1.5" data-permissions={checks ? "checked" : "static"}>
      {checks && missing.length > 0 && <div className="rounded-lg bg-destructive/10 px-3 py-2 type-footnote text-destructive" role="alert">有 {missing.length} 个必须的权限没有开通。在开发者后台开通后要重新发布版本，再点“检查”。</div>}
      <ul className="flex flex-col divide-y rounded-lg border" aria-label="权限">{rows.map((r) => { const [l, I, c] = STATUS[r.status] ?? STATUS.unknown; return (
        <li key={r.scope} className="flex items-start gap-2.5 px-3 py-2" data-perm={r.scope} data-perm-status={r.status}>
          {checks ? <I className={"mt-0.5 size-4 shrink-0 " + c} aria-label={l} /> : <span className="mt-1.5 size-1.5 shrink-0 rounded-full bg-tertiary" aria-hidden />}
          <div className="min-w-0 flex-1"><div className="type-callout">{r.purpose}</div><div className="type-footnote text-muted-foreground">{LEVEL_LABEL[r.level]}{checks ? ` · ${l}` : ""}</div><details className="mt-1 type-footnote text-muted-foreground"><summary className="cursor-pointer">权限标识</summary><code className="break-all">{r.scope}</code></details></div>
        </li>) })}</ul>
      {!checks && <div className="px-1 type-footnote text-muted-foreground">接入时 Steve 会逐项检查，能查到的打勾；发消息类的权限不发消息查不出来，以控制台为准。</div>}
    </div>
  )
}

function Modal({ title, desc, onClose, children, footer, w = 520 }: { title: string; desc?: string; onClose: () => void; children: React.ReactNode; footer?: React.ReactNode; w?: number }) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: w }}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}

export const CONSOLE_STEPS = [
  "新建企业自建应用，打开“机器人”能力",
  "在权限管理里开通下面的权限；im:message.group_msg 要管理员审批",
  "在事件与回调里选“使用长连接接收事件”，添加 im.message.receive_v1 和群成员、群信息的事件",
  "本人授权可选，默认使用设备码；只有选择 OAuth 回调时才配置重定向地址",
  "可用范围包含你本人；不要开对外共享或关联组织",
  "创建版本并发布，等管理员审核通过",
]
/** fields the Hub asks for; without the adapter list, App ID and App Secret */
const FALLBACK_FIELDS: RFieldDef[] = [{ key: "app_id", label: "App ID", secret: false, required: true, type: "text" }, { key: "app_secret", label: "App Secret", secret: true, required: true, type: "password" }]

type Done = { connection_id: string; account_id: string; permissions: PermissionCheck[] | null; bound: boolean; webhook: boolean }

export function FeishuConnectDialog({ bots, onClose, initial = "bot" }: { bots: { id: string; name: string }[]; onClose: () => void; initial?: "bot" | "self" }) {
  const [kind, setKind] = useState<"bot" | "self">(initial), [vals, setVals] = useState<Record<string, string>>({}), [persona, setPersona] = useState("")
  const [fields, setFields] = useState<RFieldDef[]>(FALLBACK_FIELDS), [personas, setPersonas] = useState<Persona[]>([]), [noVault, setNoVault] = useState(false)
  const [advanced, setAdvanced] = useState(false)
  const [app, setApp] = useState(bots[0]?.id ?? ""), [done, setDone] = useState<Done | null>(null), [busy, act] = useBusy()
  useEffect(() => {
    vaultApi.status().then((s) => setNoVault(!s.initialized), () => {})
    accountsApi.adapters().then((xs: RAdapter[]) => { const f = xs.find((x) => x.id === FEISHU_APP)?.fields; if (f?.length) setFields(f) }, () => {})
    channelsApi.personas().then((ps) => { setPersonas(ps); setPersona((p) => p || ps.find((x) => x.is_default)?.id || ps[0]?.id || "") }, () => {})
  }, [])
  const val = (f: RFieldDef) => vals[f.key] ?? f.default ?? ""
  const ok = fields.every((f) => !f.required || val(f).trim())
  const submit = async () => {
    const body: Record<string, string> = {}
    for (const f of fields) { const v = val(f).trim(); if (v) body[f.key] = v }
    const r = await act(() => channelsApi.connectBot(body, persona || undefined))
    setVals({}) // secrets are not kept once sent
    if (r) setDone({ ...r, webhook: !!(body.verification_token || body.encrypt_key) })
  }
  if (noVault) return (
    <Modal title="接入飞书" desc="App Secret 和你本人的令牌加密存在保险箱里。先启用保险箱，再回来接入。" onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button onClick={() => { onClose(); go("#/vault") }}>去启用保险箱</Button></>}><span hidden /></Modal>
  )
  if (done) return <Connected d={done} persona={personas.find((p) => p.id === persona)} onClose={onClose} />
  return (
    <Modal title="接入飞书" desc="一个企业自建应用两个身份：应用机器人在群里以自己的名字说话；你本人由 Steve 替你看私聊，默认只起草。" onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button>
        {kind === "bot" ? <Button disabled={busy || !ok} onClick={() => void submit()} data-connect-go>{busy && <Spinner />}验证并接入</Button>
          : <SelfButtons app={app} disabled={!app} />}</>}>
      <ChoiceCards value={kind} onChange={setKind} options={[{ v: "bot", t: "应用机器人", d: "在群里以它自己的名字说话，按规则自动回复", test: "bot" }, { v: "self", t: "你本人", d: "Steve 替你看消息；默认只起草，经你批准才以你的名义发出", test: "self" }]} />
      {kind === "bot" ? <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (ok && !busy) void submit() }}>
        <FieldGroup className="gap-4">
          {fields.filter((f) => advanced || !["verification_token", "encrypt_key"].includes(f.key)).map((f) => (
            <Field key={f.key}>
              <FieldLabel htmlFor={"fs-" + f.key}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label="机密" />}{!f.required && <span className="ml-1 font-normal text-muted-foreground">（可选）</span>}</FieldLabel>
              {f.type === "select" && f.key === "domain" ? <div><Select testId="fs-domain" value={val(f) || "feishu"} onChange={(v) => setVals({ ...vals, domain: v })} options={[{ v: "feishu", l: "飞书" }, { v: "lark", l: "Lark" }]} /></div>
                : <Input id={"fs-" + f.key} className="rounded-lg font-mono" type={f.secret ? "password" : "text"} autoComplete="off" spellCheck={false} required={f.required} value={val(f)} onChange={(e) => setVals({ ...vals, [f.key]: e.target.value })} data-cred-field={f.key} />}
              {f.hint && f.key !== "domain" && <FieldDescription>{f.hint}</FieldDescription>}
            </Field>))}
          <Collapsible open={advanced} onOpenChange={setAdvanced}><CollapsibleTrigger className="rounded type-callout text-blue">高级：备用 webhook 凭据（可选）</CollapsibleTrigger><CollapsibleContent className="mt-2 type-footnote text-muted-foreground">长连接无需这些字段。仅使用 webhook 时填写，并由部署者配置提供商可访问的请求地址。</CollapsibleContent></Collapsible>
          {personas.length > 0 && <Field orientation="horizontal" className="justify-between"><div><FieldLabel>默认对外身份</FieldLabel><FieldDescription>没有单独设置的群都用这个身份</FieldDescription></div>
            <Select align="end" testId="acc-persona" value={persona} onChange={setPersona} options={personas.map((p) => ({ v: p.id, l: p.name }))} /></Field>}
        </FieldGroup>
        <Collapsible><CollapsibleTrigger className="inline-flex items-center gap-1 rounded type-callout text-blue hover:underline" data-console-steps>在飞书开发者后台要做的事</CollapsibleTrigger>
          <CollapsibleContent className="mt-3 flex flex-col gap-3">
            <ol className="list-decimal pl-5 type-callout text-muted-foreground">{CONSOLE_STEPS.map((s) => <li key={s}>{s}</li>)}</ol>
            <PermissionList checks={null} />
            <div className="px-1 type-footnote text-muted-foreground">你本人要的权限：{USER_PERMS}。</div>
            <div className="px-1 type-footnote text-muted-foreground">不要让另一个服务同时用这个应用接收事件：同一个应用的多个长连接会分流消息。</div>
          </CollapsibleContent></Collapsible>
        <button type="submit" hidden />
      </form> : bots.length ? <FieldGroup className="gap-4">
        <Field orientation="horizontal" className="justify-between"><div><FieldLabel>通过哪个应用</FieldLabel><FieldDescription>用这个应用的 App ID 授权，你本人的令牌存进保险箱</FieldDescription></div>
          <Select align="end" testId="self-app" value={app} onChange={setApp} options={bots.map((a) => ({ v: a.id, l: a.name }))} /></Field>
        <div className="type-footnote text-muted-foreground">你可以选择只读取和起草，或另外允许代发已批准回复。消息约 1 分钟同步一次。</div><details className="type-footnote text-muted-foreground"><summary className="cursor-pointer">查看可能申请的权限标识</summary>{USER_PERMS}</details>
      </FieldGroup> : <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground">先接入应用机器人：你本人通过同一个企业自建应用授权。</div>}
    </Modal>
  )
}

function Connected({ d, persona, onClose }: { d: Done; persona?: Persona; onClose: () => void }) {
  return (
    <Modal title="已接入飞书应用机器人" desc="把它拉进群，或者给它发一条私聊，群就会出现在这里，Steve 会问你要不要设置。" onClose={onClose}
      footer={<><SelfButtons app={d.connection_id} variant="outline" label="可选：授权你本人" /><Button onClick={() => { onClose(); go("#/channels/acc/" + d.connection_id) }} data-goto-acc>去设置</Button></>}>
      <div className="flex flex-col gap-4" data-connected={d.connection_id}>
        <section><SectionLabel>权限检查</SectionLabel>
          {d.permissions ? <PermissionList checks={d.permissions} /> : <div className="rounded-lg bg-muted/60 px-3 py-2 type-callout text-muted-foreground" data-permissions="none">这个 Hub 没有返回权限检查的结果，以飞书控制台为准。</div>}</section>
        <div className="type-callout text-muted-foreground">事件走长连接，不需要公网地址：在开发者后台的事件与回调里选“使用长连接接收事件”。{d.webhook && <> 你填了 webhook 的字段，请由部署者另行配置提供商可达的 webhook 地址；当前页面地址不代表外部可达地址。</>}</div>
        {!d.bound && <div className="type-footnote text-muted-foreground">这个 Hub 接入时不绑定默认身份{persona && !persona.is_default ? `，“${persona.name}”没有设上` : ""}；现在用默认身份 Steve，每个群可以在群页单独换。</div>}
      </div>
    </Modal>
  )
}

/** Device authorization works without a public URL. Redirect OAuth is advanced and opt-in. */
const USER_READ_SCOPES = ["offline_access", "im:message", "im:message:readonly", "im:message.p2p_msg:get_as_user", "im:message.group_msg:get_as_user", "im:chat:read"]

export function SelfButtons({ app, disabled, variant = "default", label = "授权你本人", scopes, reauthOf, oauthAvailable = false }: { app: string; disabled?: boolean; variant?: "default" | "outline"; label?: string; scopes?: string[]; reauthOf?: string; oauthAvailable?: boolean }) {
  const [busy, act] = useBusy(), [device, setDevice] = useState<RDevice | null>(null), [showDevice, setShowDevice] = useState(false)
  const [choose, setChoose] = useState(false), [sendAsMe, setSendAsMe] = useState(false), [confirmOAuth, setConfirmOAuth] = useState(false)
  const generation = useRef(0), scopeKey = scopes?.join(" ") ?? ""
  useEffect(() => { ++generation.current; setDevice(null); setShowDevice(false); setChoose(false); setConfirmOAuth(false); return () => { ++generation.current } }, [app, reauthOf, scopeKey])
  const oauth = async () => {
    const g = generation.current
    const r = await act(() => channelsApi.userOAuth(app, scopes))
    if (r?.url && g === generation.current) location.href = r.url
  }
  const start = async (requested?: string[]) => {
    const g = generation.current
    const r = await act(() => channelsApi.userDevice(app, requested, reauthOf))
    if (r && g === generation.current) { setDevice(r); setShowDevice(true); setChoose(false) }
  }
  const code = () => {
    if (device) { setShowDevice(true); return }
    if (reauthOf || scopes?.length) void start(scopes)
    else { setSendAsMe(false); setChoose(true) }
  }
  return <>
    {confirmOAuth && <Modal title="使用回调授权" desc="此备用方式会申请读取消息和以本人身份发送的权限；每次外发仍遵循审批规则。仅需要读取时，可关闭此窗口，使用默认授权并不勾选代发。" onClose={() => setConfirmOAuth(false)} w={460}
      footer={<><Button variant="outline" disabled={busy} onClick={() => setConfirmOAuth(false)}>返回</Button><Button disabled={busy} onClick={() => void oauth()}>{busy && <Spinner />}申请读取与代发权限</Button></>}><span className="type-footnote text-muted-foreground">将跳转到官方页面确认，完成后通过当前实例配置的回调地址返回。</span></Modal>}
    {choose && <Modal title="你希望 Steve 用本人身份做什么" desc="本人身份可随时断开；机器人无需这一步也能使用。" onClose={() => setChoose(false)} w={460}
      footer={<><Button variant="outline" disabled={busy} onClick={() => setChoose(false)}>暂不开启</Button><Button disabled={busy} onClick={() => void start([...USER_READ_SCOPES, ...(sendAsMe ? ["im:message.send_as_user"] : [])])}>{busy && <Spinner />}继续到官方授权</Button></>}>
      <div className="type-callout">读取你可访问的私聊和群消息，帮助整理和起草回复。</div>
      <label className="flex cursor-pointer items-start gap-3 rounded-lg border p-3 type-callout"><input type="checkbox" className="mt-1" checked={sendAsMe} onChange={(e) => setSendAsMe(e.target.checked)} /><span>同时允许以我的名义发送已批准的回复<span className="mt-1 block type-footnote text-muted-foreground">可选。每次外发仍遵循 Steve 的审批规则。</span></span></label>
    </Modal>}
    {device && showDevice && <DeviceDialog d={device} onClose={() => setShowDevice(false)} onFinished={() => { setDevice(null); setShowDevice(false) }} />}
    <Button variant={variant} disabled={busy || disabled} onClick={code} data-self-device>{busy ? <Spinner /> : <ExternalLinkIcon />}{device ? "继续本人授权" : label}</Button>
    {oauthAvailable && <Button variant="outline" disabled={busy || disabled} onClick={() => setConfirmOAuth(true)} data-self-oauth>改用已配置的回调授权</Button>}
  </>
}

function DeviceDialog({ d, onClose, onFinished }: { d: RDevice; onClose: () => void; onFinished: () => void }) {
  const [err, setErr] = useState(""), [terminal, setTerminal] = useState(false), [busy, setBusy] = useState(false)
  const version = useRef(0), mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; ++version.current } }, [])
  useEffect(() => {
    let stop = false, t: ReturnType<typeof setTimeout>
    const tick = async () => {
      const v = version.current
      try {
        const s = await channelsApi.userDeviceStatus(d.id)
        if (stop || v !== version.current) return
        if (s.status === "ok") { toast("已授权你本人"); onFinished(); go("#/channels/acc/" + s.account_id); return }
        if (s.status === "error") { setErr(s.error); setTerminal(true); return }
        if (s.status === "cancelled") { setErr("本次授权接入已取消"); setTerminal(true); return }
        setErr("")
      } catch (e) {
        if (stop || v !== version.current) return
        if (e instanceof HubError && e.status === 404) { setErr("这次授权已失效，请重新开始"); setTerminal(true); return }
        setErr("暂时无法读取进度，正在重试。")
      }
      if (!stop) t = setTimeout(tick, Math.max(2, d.interval) * 1000)
    }
    t = setTimeout(tick, 0)
    return () => { stop = true; clearTimeout(t) }
  }, [d.id, busy])
  const cancel = async () => {
    setBusy(true); ++version.current
    try {
      const s = await channelsApi.userDeviceCancel(d.id)
      if (!mounted.current) return
      if (s.status === "ok") { toast("授权已先完成，可在账号详情断开"); onFinished(); go("#/channels/acc/" + s.account_id) }
      else if (s.status === "cancelled") { toast("已停止本次授权接入；官方页面上已同意的权限不会自动撤销"); onFinished() }
      else if (s.status === "error") { setErr(s.error); setTerminal(true) }
      else setErr("尚未确认取消结果，请稍后重试")
    } catch (e) { report(e) }
    finally { ++version.current; if (mounted.current) setBusy(false) }
  }
  const url = officialConfirmationUrl(d.verification_uri_complete ?? d.verification_uri)
  return <Modal title="授权你本人" desc="打开官方页面并确认。无需配置回调地址，完成后这里会自动继续。" onClose={onClose} w={440}
    footer={terminal ? <Button onClick={onFinished}>关闭并重新开始</Button> : <><Button variant="outline" disabled={busy} onClick={() => void cancel()}>取消授权接入</Button><Button variant="outline" onClick={onClose}>稍后继续</Button></>}>
    <div className="flex flex-col items-center gap-3 py-2" data-device-flow>
      <div className="select-all rounded-xl bg-muted px-5 py-3 font-mono text-[26px] tracking-[0.2em]" aria-label="设备码" data-device-code>{d.user_code}</div>
      {!terminal && url && <Button nativeButton={false} render={<a href={url} target="_blank" rel="noreferrer noopener" />}><ExternalLinkIcon />打开官方授权页面</Button>}
      {!url && <p role="alert" className="type-callout text-destructive">没有收到有效的官方授权地址，请取消后重试。</p>}
      <div className="type-footnote text-muted-foreground" role="status">{err ? <span className="text-destructive">{err}</span> : <span className="inline-flex items-center gap-1.5"><Spinner className="size-3" />等待你确认；链接过期后可重新开始</span>}</div>
      <p className="type-footnote text-muted-foreground">取消会停止 Steve 的接入，不会撤销你已在官方页面同意的权限。</p>
    </div>
  </Modal>
}

/* The default journey is official registration; manual configuration is a secondary path. */
type GuideAdapter = { id: string; identity: string; capabilities: { area: string; planned: boolean; note?: string; scopes: { scope: string; purpose: string; required: boolean }[] }[] }

export function FeishuGuide({ bots, selfDone, callbackUrls }: { adapters: GuideAdapter[]; bots: { id: string; name: string; domain?: "feishu" | "lark" }[]; selfDone: boolean; callbackUrls: string[] }) {
  const [dlg, setDlg] = useState(false), [app, setApp] = useState(bots[0]?.id ?? "")
  const selected = bots.some((b) => b.id === app) ? app : bots[0]?.id ?? ""
  return <section className="flex flex-col gap-4" data-feishu-guide>
    {dlg && <FeishuConnectDialog bots={bots} onClose={() => setDlg(false)} />}
    <SectionLabel>{bots.length ? "接入更多应用" : "接入飞书 / Lark"}</SectionLabel>
    <FeishuRegistrationPanel />
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border p-4">
      <div className="min-w-0"><div className="type-callout font-medium">已有应用 / 团队机器人</div><div className="type-footnote text-muted-foreground">关联企业自建应用，使用自己的 App ID 和 App Secret。</div></div>
      <Button variant="outline" onClick={() => setDlg(true)} data-feishu-bot>使用已有应用</Button>
    </div>
    {bots.length > 0 && <div className="flex flex-col gap-3 rounded-xl bg-muted/40 p-4">
      <div className="type-callout font-medium">本人授权（可选）</div>
      <p className="type-footnote text-muted-foreground">机器人已接入，可以直接使用。若希望 Steve 读取你可访问的消息，可以再授权本人身份；以你的名义发出前仍需批准。{selfDone && " 你已有本人身份接入，可在对应应用详情中管理。"}</p>
      <div className="flex flex-wrap items-center gap-2">
        {bots.length > 1 && <Select testId="guide-self-app" value={selected} onChange={setApp} options={bots.map((b) => ({ v: b.id, l: b.name }))} />}
        <SelfButtons app={selected} variant="outline" />
      </div>
    </div>}
    <Collapsible><CollapsibleTrigger className="rounded text-left type-callout text-blue">高级：OAuth 回调（可选）</CollapsibleTrigger>
      <CollapsibleContent className="mt-3 flex flex-col gap-2 type-footnote text-muted-foreground">
        <p>默认接入、设备码授权和消息长连接均不需要回调地址。只有选择浏览器回调授权时，才由部署者配置自己的地址，并在应用后台登记。</p>
        {callbackUrls.length ? <>
          <p>当前实例配置的回调地址（请确认浏览器可返回，且提供商接受此地址）：</p>
          {callbackUrls.map((u) => <code key={u} className="select-all break-all rounded bg-muted p-2" data-redirect-url>{u}</code>)}
          {selected && (bots.find((b) => b.id === selected)?.domain === "feishu" ? <SelfButtons app={selected} variant="outline" oauthAvailable /> : <p>当前应用请使用设备码授权；Lark 的回调授权暂未开放。</p>)}
        </> : <p>尚未配置回调地址，可以继续使用默认设备码授权。</p>}
        <p>部署者可通过 STEVE_PUBLIC_URL / STEVE_PUBLIC_URLS 配置实例地址；这些设置也用于登录等功能，修改后需按部署方式重启服务。</p>
      </CollapsibleContent>
    </Collapsible>
  </section>
}
