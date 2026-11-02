/* Dialogs of the accounts page: record an account, connect one (OAuth, device code or a credential form), and re-enter a
 * connection's credential. Secret inputs are password fields with autocomplete off; their values only go into the request. */
import { useEffect, useMemo, useRef, useState } from "react"
import { ExternalLinkIcon, LockIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Checkbox } from "@/components/ui/checkbox"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Select } from "@/views"
import { go } from "@/ui"
import { toast } from "@/store"
import { accountsApi, capLabel, KIND_LABEL, type RAdapter, type RDevice, type RFieldDef, type RSystem } from "@/api/accounts"
import { useBusy, report } from "@/accounts/load"
import { vaultApi } from "@/api/vault"
import { ImapFields } from "@/mail/connect"

function Modal({ title, desc, onClose, children, footer, w = 480 }: { title: string; desc?: string; onClose: () => void; children: React.ReactNode; footer?: React.ReactNode; w?: number }) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl" style={{ maxWidth: w }}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 其他账号（只记录，不接入） ───────── */
export function NewAccountDialog({ systems, onClose, onDone }: { systems: RSystem[]; onClose: () => void; onDone: (id: string) => void }) {
  const usable = systems.filter((s) => s.id !== "steve")
  const [s, setS] = useState(usable.find((x) => x.id === "aws")?.id ?? usable[0]?.id ?? ""), [kind, setKind] = useState("other"), [h, setH] = useState(""), [n, setN] = useState("")
  const [busy, act] = useBusy()
  const sys = usable.find((x) => x.id === s)
  const submit = async () => {
    const a = await act(() => accountsApi.newAccount({ system_id: s, kind, handle: h.trim(), display_name: n.trim() || undefined }), "已添加账号")
    if (a) onDone(a.id)
  }
  return (
    <Modal title="添加账号" desc="只记录，不接入 Steve。记下来是为了关联保险箱条目，以及让 Steve 知道你有哪些账号。" onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!h.trim() || !s || busy} onClick={() => void submit()} data-testid="na-go">添加</Button></>}>
      <form className="contents" onSubmit={(e) => { e.preventDefault(); if (h.trim() && s && !busy) void submit() }}>
        <FieldGroup className="gap-4">
          <Field orientation="horizontal" className="justify-between"><FieldLabel>系统</FieldLabel><Select testId="na-sys" value={s} onChange={setS} options={usable.map((x) => ({ v: x.id, l: x.name }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>种类</FieldLabel><Select testId="na-kind" value={kind} onChange={setKind} options={Object.entries(KIND_LABEL).map(([v, l]) => ({ v, l }))} /></Field>
          <Field><FieldLabel htmlFor="na-handle">账号</FieldLabel><Input id="na-handle" autoFocus className="rounded-lg" value={h} maxLength={200} onChange={(e) => setH(e.target.value)} placeholder={sys?.family === "card" ? "卡号（只保存后四位）" : sys?.family === "email" ? "邮箱" : "用户名、ID、账号号码"} data-na-handle autoComplete="off" />
            {sys?.family === "card" && <FieldDescription>账号里只记后四位；完整卡号请放进保险箱。</FieldDescription>}</Field>
          <Field><FieldLabel htmlFor="na-name">显示名（可选）</FieldLabel><Input id="na-name" className="rounded-lg" value={n} maxLength={80} onChange={(e) => setN(e.target.value)} placeholder="例如：AWS 根账号" /></Field>
        </FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/* ───────── 接入 Steve ───────── */
function CredentialFields({ fields, vals, setVals, only }: { fields: RFieldDef[]; vals: Record<string, string>; setVals: (v: Record<string, string>) => void; only?: string[] }) {
  const xs = only ? fields.filter((f) => only.includes(f.label) || only.includes(f.key)) : fields
  return <>{xs.map((f) => (
    <Field key={f.key}>
      <FieldLabel htmlFor={"cf-" + f.key}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label="机密" />}{!f.required && <span className="ml-1 font-normal text-muted-foreground">（可选）</span>}</FieldLabel>
      <Input id={"cf-" + f.key} className="rounded-lg font-mono" type={f.secret ? "password" : f.type === "number" ? "number" : "text"} autoComplete="off" spellCheck={false} required={f.required}
        value={vals[f.key] ?? f.default ?? ""} onChange={(e) => setVals({ ...vals, [f.key]: e.target.value })} data-cred-field={f.key} />
      {f.hint && <FieldDescription>{f.hint}</FieldDescription>}
    </Field>))}</>
}
const filled = (fields: RFieldDef[], vals: Record<string, string>, only?: string[]) => fields.filter((f) => !only || only.includes(f.label) || only.includes(f.key)).every((f) => !f.required || (vals[f.key] ?? f.default ?? "").trim())
const withDefaults = (fields: RFieldDef[], vals: Record<string, string>) => { const o: Record<string, string> = {}; for (const f of fields) { const v = vals[f.key] ?? f.default; if (v !== undefined && v !== "") o[f.key] = v } return o }

/** `done`: where to go once connected (default: the new account's page) */
export function ConnectDialog({ adapter, onClose, done }: { adapter: RAdapter; onClose: () => void; done?: (account: string) => void }) {
  const [caps, setCaps] = useState<string[]>(adapter.caps), [vals, setVals] = useState<Record<string, string>>({}), [device, setDevice] = useState<RDevice | null>(null)
  const [busy, act] = useBusy(), [noVault, setNoVault] = useState(false)
  /* tokens and app secrets live in the system vault: connecting needs an enabled vault */
  useEffect(() => { vaultApi.status().then((s) => setNoVault(!s.initialized), () => {}) }, [])
  const fields = adapter.fields ?? []
  const oauth = adapter.auth === "oauth"
  const goOAuth = async () => { const r = await act(() => accountsApi.oauthStart(adapter.id, caps)); if (r?.url) location.href = r.url }
  const goDevice = async () => { const r = await act(() => accountsApi.deviceStart(adapter.id, caps)); if (r) setDevice(r) }
  const goCred = async () => {
    const r = await act(() => accountsApi.connect(adapter.id, withDefaults(fields, vals)), "已接入")
    setVals({}) // the values are not kept once sent
    if (r) { onClose(); done ? done(r.account_id) : go("#/accounts/" + r.account_id) }
  }
  if (device) return <DeviceDialog adapter={adapter} d={device} onClose={onClose} done={done} />
  if (noVault) return (
    <Modal title={`接入 Steve · ${adapter.name}`} desc="接入的令牌和应用密钥加密存在保险箱里。先启用保险箱，再回来接入。" onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button onClick={() => { onClose(); go("#/vault") }} data-go-enable-vault>去启用保险箱</Button></>}><span hidden /></Modal>
  )
  return (
    <Modal title={`接入 Steve · ${adapter.name}`} onClose={onClose}
      desc={oauth ? "授权在浏览器里完成，Steve 只保存访问令牌，不保存密码。" : "凭据加密存进系统保险库，Steve 和 Bot 都看不到原值。"}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button>
        {oauth && adapter.device && <Button variant="outline" disabled={busy} onClick={() => void goDevice()} data-connect-device>用设备码授权</Button>}
        {oauth ? <Button disabled={busy} onClick={() => void goOAuth()} data-connect-go><ExternalLinkIcon />去授权</Button>
          : <Button disabled={busy || !filled(fields, vals)} onClick={() => void goCred()} data-connect-go>{busy && <Spinner />}验证并接入</Button>}</>}>
      <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!oauth && !busy && filled(fields, vals)) void goCred() }}>
        <FieldGroup className="gap-4">
          {!oauth && (adapter.id === "imap" ? <ImapFields fields={fields} vals={vals} setVals={setVals} /> : <CredentialFields fields={fields} vals={vals} setVals={setVals} />)}
          {adapter.caps.length > 0 && <Field><FieldLabel>允许 Steve</FieldLabel>
            <div className="flex flex-col gap-2" role="group" aria-label="允许 Steve">{adapter.caps.map((c) => (
              <label key={c} className="flex items-center gap-2.5 type-callout"><Checkbox checked={caps.includes(c)} onCheckedChange={(v) => setCaps(v ? [...caps, c] : caps.filter((x) => x !== c))} data-cap={c} />{capLabel(c)}</label>))}</div>
            {!oauth && <FieldDescription>接入后可以在账号页随时改。</FieldDescription>}</Field>}
        </FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/** the device flow: show the code, open the provider's page, poll until the Hub says it's done */
function DeviceDialog({ adapter, d, onClose, done }: { adapter: RAdapter; d: RDevice; onClose: () => void; done?: (account: string) => void }) {
  const [err, setErr] = useState(""), [left, setLeft] = useState(d.expires_in)
  const until = useRef(Date.now() + d.expires_in * 1000)
  useEffect(() => {
    let stop = false, t: ReturnType<typeof setTimeout>
    const tick = async () => {
      if (stop) return
      if (Date.now() > until.current) { setErr("设备码过期了，请重新开始"); return }
      try {
        const s = await accountsApi.deviceStatus(d.id)
        if (stop) return
        if (s.status === "ok") { toast("已接入"); onClose(); done ? done(s.account_id) : go("#/accounts/" + s.account_id); return }
        if (s.status === "error") { setErr(s.error); return }
      } catch (e) { report(e); return }
      t = setTimeout(tick, Math.max(2, d.interval) * 1000)
    }
    t = setTimeout(tick, Math.max(2, d.interval) * 1000)
    const c = setInterval(() => setLeft(Math.max(0, Math.round((until.current - Date.now()) / 1000))), 1000)
    return () => { stop = true; clearTimeout(t); clearInterval(c) }
  }, [d.id])
  const url = d.verification_uri_complete ?? d.verification_uri
  return (
    <Modal title={`接入 Steve · ${adapter.name}`} desc="在手机或另一台电脑上打开下面的地址，输入这个代码并同意授权。完成后这里会自动继续。" onClose={onClose} w={420}
      footer={<Button variant="outline" onClick={onClose}>{err ? "关闭" : "取消"}</Button>}>
      <div className="flex flex-col items-center gap-3 py-2" data-device-flow>
        <div className="select-all rounded-xl bg-muted px-5 py-3 font-mono text-[26px] tracking-[0.2em]" aria-label="设备码" data-device-code>{d.user_code}</div>
        <a href={url} target="_blank" rel="noreferrer noopener" className="inline-flex items-center gap-1 type-callout text-blue hover:underline">{d.verification_uri}<ExternalLinkIcon className="size-3.5" /></a>
        <div className="type-footnote text-muted-foreground" role="status">{err ? <span className="text-destructive">{err}</span> : <span className="inline-flex items-center gap-1.5"><Spinner className="size-3" />等你在{adapter.name}上确认 · 还剩 {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")}</span>}</div>
      </div>
    </Modal>
  )
}

/** `PUT /connections/:id/credential`: re-enter the secret fields of a credential connection */
export function CredentialDialog({ adapter, connection, only, onClose, onDone }: { adapter: RAdapter; connection: string; only?: string[]; onClose: () => void; onDone: () => void }) {
  const [vals, setVals] = useState<Record<string, string>>({}), [busy, act] = useBusy()
  const fields = useMemo(() => (adapter.fields ?? []).filter((f) => f.secret && (!only || only.includes(f.label) || only.includes(f.key))), [adapter, only])
  const submit = async () => { const r = await act(() => accountsApi.putCredential(connection, withDefaults(fields, vals)), "凭据已更新并通过验证"); setVals({}); if (r) { onDone(); onClose() } }
  return (
    <Modal title="重新填写凭据" desc={`${adapter.name} 的凭据会重新验证；旧的凭据随即销毁。`} onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={busy || !filled(fields, vals)} onClick={() => void submit()} data-cred-go>{busy && <Spinner />}验证并保存</Button></>}>
      <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!busy && filled(fields, vals)) void submit() }}>
        <FieldGroup className="gap-4"><CredentialFields fields={fields} vals={vals} setVals={setVals} /></FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}
