/* The mail part of the accounts page's connect dialog (email.md §1, §11): the IMAP form. Picking a provider fills the
 * IMAP / SMTP hosts, ports and encryption and says how to get an app password; 其他 opens the server settings. The
 * password field is a password input with autocomplete off; values only go into the `POST /connections` body. */
import { useState } from "react"
import { ChevronDownIcon, ExternalLinkIcon, LockIcon } from "lucide-react"
import { cn } from "cn"
import { Input } from "@/components/ui/input"
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field"
import { Chips } from "@/ui"
import { Select } from "@/view-shared"
import { IMAP_PRESETS, type ImapPreset } from "@/api/mail"
import type { RFieldDef } from "@/api/accounts"

const SERVER = ["imap_host", "imap_port", "imap_security", "smtp_host", "smtp_port", "smtp_security", "username"]
const SECURITY = [{ v: "tls", l: "TLS" }, { v: "starttls", l: "STARTTLS" }]

/** the values a preset puts into the form */
const presetVals = (p: ImapPreset) => ({ imap_host: p.imap_host, imap_port: String(p.imap_port), imap_security: "tls", smtp_host: p.smtp_host, smtp_port: String(p.smtp_port), smtp_security: p.smtp_security })

export function ImapFields({ fields, vals, setVals }: { fields: RFieldDef[]; vals: Record<string, string>; setVals: (v: Record<string, string>) => void }) {
  const [preset, setPreset] = useState<ImapPreset | null>(null), [open, setOpen] = useState(false)
  const pick = (name: string) => {
    const p = IMAP_PRESETS.find((x) => x.name === name)!; setPreset(p)
    setVals({ ...vals, ...presetVals(p) })
    if (p.id === "custom") setOpen(true)
  }
  const f = (k: string) => fields.find((x) => x.key === k)
  const v = (k: string) => vals[k] ?? f(k)?.default ?? ""
  const set = (k: string, x: string) => setVals({ ...vals, [k]: x })
  const input = (k: string, extra?: { placeholder?: string; mono?: boolean; autoFocus?: boolean }) => {
    const d = f(k); if (!d) return null
    return (
      <Field key={k}>
        <FieldLabel htmlFor={k.endsWith("_security") ? undefined : "imap-" + k}>{d.label}{d.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label="机密" />}{!d.required && <span className="ml-1 font-normal text-muted-foreground">（可选）</span>}</FieldLabel>
        {k.endsWith("_security")
          ? <div role="group" aria-label={d.label}><Select testId={"imap-" + k} value={v(k) || "tls"} onChange={(x) => set(k, x)} options={SECURITY} /></div>
          : <Input id={"imap-" + k} className={cn("rounded-lg", extra?.mono !== false && "font-mono")} autoFocus={extra?.autoFocus} placeholder={extra?.placeholder}
              type={d.secret ? "password" : d.type === "number" ? "number" : k === "address" ? "email" : "text"} inputMode={d.type === "number" ? "numeric" : undefined}
              autoComplete="off" spellCheck={false} required={d.required} value={v(k)} onChange={(e) => set(k, e.target.value)} data-cred-field={k} />}
        {d.hint && !k.endsWith("_security") && <FieldDescription>{d.hint}</FieldDescription>}
      </Field>
    )
  }
  const server = SERVER.filter((k) => f(k))
  return (
    <div className="flex flex-col gap-4" data-imap-form>
      <Field>
        <FieldLabel>邮箱服务商</FieldLabel>
        <Chips value={preset?.name ?? ""} onChange={pick} options={IMAP_PRESETS.map((p) => p.name)} className="flex-wrap" />
        <FieldDescription data-imap-hint>{preset
          ? <>{preset.hint}{preset.help && <> · <a href={preset.help} target="_blank" rel="noreferrer noopener" className="inline-flex items-center gap-0.5 text-blue hover:underline">怎么拿应用专用密码<ExternalLinkIcon className="size-3" aria-hidden /></a></>}</>
          : "选一个服务商，服务器地址和端口会自动填好。Outlook.com 个人邮箱不支持密码方式，请用 Outlook 授权接入。"}</FieldDescription>
      </Field>
      {input("address", { placeholder: "you@example.com" })}
      {input("password")}
      <div>
        <button type="button" className="inline-flex items-center gap-1 type-callout text-muted-foreground hover:text-foreground" aria-expanded={open} onClick={() => setOpen(!open)} data-imap-server-toggle>
          服务器设置<ChevronDownIcon className={cn("size-4 transition-transform", open && "rotate-180")} aria-hidden />
          {!open && v("imap_host") && <span className="ml-1 font-mono text-[12px]">{v("imap_host")}:{v("imap_port")} · {v("smtp_host")}:{v("smtp_port")}</span>}
        </button>
        {open && <div className="mt-3 grid grid-cols-1 gap-4 sm:grid-cols-[1fr_96px_120px]" data-imap-server>
          {server.filter((k) => k.startsWith("imap")).map((k) => input(k))}
          {server.filter((k) => k.startsWith("smtp")).map((k) => input(k))}
          <div className="sm:col-span-3">{input("username")}</div>
        </div>}
      </div>
    </div>
  )
}
