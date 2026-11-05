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
import { Select } from "@/view-shared"
import { go } from "@/ui"
import { toast } from "@/store"
import { accountsApi, capLabel, KIND_LABEL, type RAdapter, type RDevice, type RFieldDef, type RSystem } from "@/api/accounts"
import { useBusy, report } from "@/accounts/load"
import { vaultApi } from "@/api/vault"
import { ImapFields } from "@/mail/connect"
import { useT } from "@/i18n"

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
  const t = useT()
  const usable = systems.filter((s) => s.id !== "steve")
  const [s, setS] = useState(usable.find((x) => x.id === "aws")?.id ?? usable[0]?.id ?? ""), [kind, setKind] = useState("other"), [h, setH] = useState(""), [n, setN] = useState("")
  const [busy, act] = useBusy()
  const sys = usable.find((x) => x.id === s)
  const submit = async () => {
    const a = await act(() => accountsApi.newAccount({ system_id: s, kind, handle: h.trim(), display_name: n.trim() || undefined }), t("accounts:newAccount.added"))
    if (a) onDone(a.id)
  }
  return (
    <Modal title={t("accounts:newAccount.title")} desc={t("accounts:newAccount.desc")} onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!h.trim() || !s || busy} onClick={() => void submit()} data-testid="na-go">{t("common:action.add")}</Button></>}>
      <form className="contents" onSubmit={(e) => { e.preventDefault(); if (h.trim() && s && !busy) void submit() }}>
        <FieldGroup className="gap-4">
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("accounts:newAccount.system")}</FieldLabel><Select testId="na-sys" value={s} onChange={setS} options={usable.map((x) => ({ v: x.id, l: x.name }))} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("accounts:newAccount.kind")}</FieldLabel><Select testId="na-kind" value={kind} onChange={setKind} options={Object.entries(KIND_LABEL).map(([v, l]) => ({ v, l }))} /></Field>
          <Field><FieldLabel htmlFor="na-handle">{t("accounts:newAccount.handle")}</FieldLabel><Input id="na-handle" autoFocus className="rounded-lg" value={h} maxLength={200} onChange={(e) => setH(e.target.value)} placeholder={sys?.family === "card" ? t("accounts:newAccount.handleCard") : sys?.family === "email" ? t("accounts:newAccount.handleEmail") : t("accounts:newAccount.handleOther")} data-na-handle autoComplete="off" />
            {sys?.family === "card" && <FieldDescription>{t("accounts:newAccount.cardNote")}</FieldDescription>}</Field>
          <Field><FieldLabel htmlFor="na-name">{t("accounts:newAccount.displayName")}</FieldLabel><Input id="na-name" className="rounded-lg" value={n} maxLength={80} onChange={(e) => setN(e.target.value)} placeholder={t("accounts:newAccount.displayNamePlaceholder")} /></Field>
        </FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/* ───────── 接入 Steve ───────── */
function CredentialFields({ fields, vals, setVals, only }: { fields: RFieldDef[]; vals: Record<string, string>; setVals: (v: Record<string, string>) => void; only?: string[] }) {
  const t = useT()
  const xs = only ? fields.filter((f) => only.includes(f.label) || only.includes(f.key)) : fields
  return <>{xs.map((f) => (
    <Field key={f.key}>
      <FieldLabel htmlFor={"cf-" + f.key}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label={t("accounts:connect.secret")} />}{!f.required && <span className="ml-1 font-normal text-muted-foreground">{t("accounts:connect.optional")}</span>}</FieldLabel>
      <Input id={"cf-" + f.key} className="rounded-lg font-mono" type={f.secret ? "password" : f.type === "number" ? "number" : "text"} autoComplete="off" spellCheck={false} required={f.required}
        value={vals[f.key] ?? f.default ?? ""} onChange={(e) => setVals({ ...vals, [f.key]: e.target.value })} data-cred-field={f.key} />
      {f.hint && <FieldDescription>{f.hint}</FieldDescription>}
    </Field>))}</>
}
const filled = (fields: RFieldDef[], vals: Record<string, string>, only?: string[]) => fields.filter((f) => !only || only.includes(f.label) || only.includes(f.key)).every((f) => !f.required || (vals[f.key] ?? f.default ?? "").trim())
const withDefaults = (fields: RFieldDef[], vals: Record<string, string>) => { const o: Record<string, string> = {}; for (const f of fields) { const v = vals[f.key] ?? f.default; if (v !== undefined && v !== "") o[f.key] = v } return o }

/** `done`: where to go once connected (default: the new account's page) */
export function ConnectDialog({ adapter, onClose, done }: { adapter: RAdapter; onClose: () => void; done?: (account: string) => void }) {
  const t = useT()
  const [caps, setCaps] = useState<string[]>(adapter.caps), [vals, setVals] = useState<Record<string, string>>({}), [device, setDevice] = useState<RDevice | null>(null)
  const [busy, act] = useBusy(), [noVault, setNoVault] = useState(false)
  /* tokens and app secrets live in the system vault: connecting needs an enabled vault */
  useEffect(() => { vaultApi.status().then((s) => setNoVault(!s.initialized), () => {}) }, [])
  const fields = adapter.fields ?? []
  const oauth = adapter.auth === "oauth"
  const goOAuth = async () => { const r = await act(() => accountsApi.oauthStart(adapter.id, caps)); if (r?.url) location.href = r.url }
  const goDevice = async () => { const r = await act(() => accountsApi.deviceStart(adapter.id, caps)); if (r) setDevice(r) }
  const goCred = async () => {
    const r = await act(() => accountsApi.connect(adapter.id, withDefaults(fields, vals)), t("accounts:connect.connected"))
    setVals({}) // the values are not kept once sent
    if (r) { onClose(); done ? done(r.account_id) : go("#/accounts/" + r.account_id) }
  }
  if (device) return <DeviceDialog adapter={adapter} d={device} onClose={onClose} done={done} />
  if (noVault) return (
    <Modal title={t("accounts:connect.title", { name: adapter.name })} desc={t("accounts:connect.noVault")} onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button onClick={() => { onClose(); go("#/vault") }} data-go-enable-vault>{t("accounts:connect.enableVault")}</Button></>}><span hidden /></Modal>
  )
  return (
    <Modal title={t("accounts:connect.title", { name: adapter.name })} onClose={onClose}
      desc={oauth ? t("accounts:connect.descOAuth") : t("accounts:connect.descCred")}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
        {oauth && adapter.device && <Button variant="outline" disabled={busy} onClick={() => void goDevice()} data-connect-device>{t("accounts:connect.useDevice")}</Button>}
        {oauth ? <Button disabled={busy} onClick={() => void goOAuth()} data-connect-go><ExternalLinkIcon />{t("accounts:connect.authorize")}</Button>
          : <Button disabled={busy || !filled(fields, vals)} onClick={() => void goCred()} data-connect-go>{busy && <Spinner />}{t("accounts:connect.verifyConnect")}</Button>}</>}>
      <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!oauth && !busy && filled(fields, vals)) void goCred() }}>
        <FieldGroup className="gap-4">
          {!oauth && (adapter.id === "imap" ? <ImapFields fields={fields} vals={vals} setVals={setVals} /> : <CredentialFields fields={fields} vals={vals} setVals={setVals} />)}
          {adapter.caps.length > 0 && <Field><FieldLabel>{t("accounts:connect.allow")}</FieldLabel>
            <div className="flex flex-col gap-2" role="group" aria-label={t("accounts:connect.allow")}>{adapter.caps.map((c) => (
              <label key={c} className="flex items-center gap-2.5 type-callout"><Checkbox checked={caps.includes(c)} onCheckedChange={(v) => setCaps(v ? [...caps, c] : caps.filter((x) => x !== c))} data-cap={c} />{capLabel(c)}</label>))}</div>
            {!oauth && <FieldDescription>{t("accounts:connect.capsLater")}</FieldDescription>}</Field>}
        </FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/** the device flow: show the code, open the provider's page, poll until the Hub says it's done */
function DeviceDialog({ adapter, d, onClose, done }: { adapter: RAdapter; d: RDevice; onClose: () => void; done?: (account: string) => void }) {
  const t = useT()
  const [err, setErr] = useState(""), [left, setLeft] = useState(d.expires_in)
  const until = useRef(Date.now() + d.expires_in * 1000)
  useEffect(() => {
    let stop = false, timer: ReturnType<typeof setTimeout>
    const tick = async () => {
      if (stop) return
      if (Date.now() > until.current) { setErr(t("accounts:device.expired")); return }
      try {
        const s = await accountsApi.deviceStatus(d.id)
        if (stop) return
        if (s.status === "ok") { toast(t("accounts:connect.connected")); onClose(); done ? done(s.account_id) : go("#/accounts/" + s.account_id); return }
        if (s.status === "error") { setErr(s.error); return }
      } catch (e) { report(e); return }
      timer = setTimeout(tick, Math.max(2, d.interval) * 1000)
    }
    timer = setTimeout(tick, Math.max(2, d.interval) * 1000)
    const c = setInterval(() => setLeft(Math.max(0, Math.round((until.current - Date.now()) / 1000))), 1000)
    return () => { stop = true; clearTimeout(timer); clearInterval(c) }
  }, [d.id])
  const url = d.verification_uri_complete ?? d.verification_uri
  return (
    <Modal title={t("accounts:connect.title", { name: adapter.name })} desc={t("accounts:device.desc")} onClose={onClose} w={420}
      footer={<Button variant="outline" onClick={onClose}>{err ? t("common:action.close") : t("common:action.cancel")}</Button>}>
      <div className="flex flex-col items-center gap-3 py-2" data-device-flow>
        <div className="select-all rounded-xl bg-muted px-5 py-3 font-mono text-[26px] tracking-[0.2em]" aria-label={t("accounts:device.code")} data-device-code>{d.user_code}</div>
        <a href={url} target="_blank" rel="noreferrer noopener" className="inline-flex items-center gap-1 type-callout text-blue hover:underline">{d.verification_uri}<ExternalLinkIcon className="size-3.5" /></a>
        <div className="type-footnote text-muted-foreground" role="status">{err ? <span className="text-destructive">{err}</span> : <span className="inline-flex items-center gap-1.5"><Spinner className="size-3" />{t("accounts:device.waiting", { name: adapter.name, left: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` })}</span>}</div>
      </div>
    </Modal>
  )
}

/** `PUT /connections/:id/credential`: re-enter the secret fields of a credential connection */
export function CredentialDialog({ adapter, connection, only, onClose, onDone }: { adapter: RAdapter; connection: string; only?: string[]; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [vals, setVals] = useState<Record<string, string>>({}), [busy, act] = useBusy()
  const fields = useMemo(() => (adapter.fields ?? []).filter((f) => f.secret && (!only || only.includes(f.label) || only.includes(f.key))), [adapter, only])
  const submit = async () => { const r = await act(() => accountsApi.putCredential(connection, withDefaults(fields, vals)), t("accounts:cred.updated")); setVals({}); if (r) { onDone(); onClose() } }
  return (
    <Modal title={t("accounts:cred.title")} desc={t("accounts:cred.desc", { name: adapter.name })} onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={busy || !filled(fields, vals)} onClick={() => void submit()} data-cred-go>{busy && <Spinner />}{t("accounts:cred.verifySave")}</Button></>}>
      <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!busy && filled(fields, vals)) void submit() }}>
        <FieldGroup className="gap-4"><CredentialFields fields={fields} vals={vals} setVals={setVals} /></FieldGroup>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}
