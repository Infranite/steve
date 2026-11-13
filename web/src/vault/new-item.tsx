/* 新建保险箱条目 (prototype NewVaultDialog): fields come from GET /vault/templates; secret fields are password inputs with
 * autocomplete off, and their values only go into the POST. */
import { useEffect, useState } from "react"
import { LockIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Select } from "@/view-shared"
import { go, Segmented } from "@/ui"
import { Loading } from "@/settings-kit"
import { vaultApi, SENS_LABEL, type RTemplate, type RVault, type Sensitivity } from "@/api/vault"
import { useBusy, report } from "@/accounts/load"
import { useT } from "@/i18n"

export function NewVaultDialog({ account, onClose }: { account?: { id: string; display_name: string }; onClose: () => void }) {
  const t = useT()
  const [tpls, setTpls] = useState<RTemplate[] | null>(null), [vaults, setVaults] = useState<RVault[]>([])
  const [tpl, setTpl] = useState("login"), [title, setTitle] = useState(""), [site, setSite] = useState(""), [vals, setVals] = useState<Record<string, string>>({})
  const [scope, setScope] = useState<"personal" | "shared">("personal"), [sens, setSens] = useState<Sensitivity | "">("")
  const [busy, act] = useBusy()
  useEffect(() => { Promise.all([vaultApi.templates(), vaultApi.vaults()]).then(([ts, v]) => { setTpls(ts); setVaults(v); if (!ts.some((x) => x.id === "login") && ts[0]) setTpl(ts[0].id) }).catch((e) => { report(e); onClose() }) }, [])
  const T = tpls?.find((x) => x.id === tpl)
  const vault = vaults.find((v) => v.scope === scope)
  const level: Sensitivity = sens || T?.default_sensitivity || "sensitive"
  const submit = async () => {
    if (!T || !vault) return
    const r = await act(() => vaultApi.newItem({ vault_id: vault.id, template: T.id, title: title.trim(), site: site.trim() || undefined, account_id: account?.id, sensitivity: level,
      fields: T.fields.map((f) => ({ label: f.label, type: f.type, secret: f.secret, value: vals[f.label] ?? "" })).filter((f) => f.value !== "") }), t("common:state.saved"))
    setVals({}) // drop the typed values whatever happened
    if (r) { onClose(); go("#/vault/" + r.id) }
  }
  const ok = !!T && !!vault && !!title.trim() && !busy
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto sm:max-w-[480px]">
        <DialogHeader><DialogTitle className="type-title">{t("vault:newItem.title")}</DialogTitle><DialogDescription className="type-callout">{account ? t("vault:newItem.descAccount", { name: account.display_name }) : t("vault:newItem.desc")}</DialogDescription></DialogHeader>
        {!tpls ? <Loading className="py-8" /> :
        <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (ok) void submit() }}>
          <FieldGroup className="gap-4">
            <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("vault:newItem.template")}</FieldLabel><Select testId="nv-tpl" value={tpl} onChange={(v) => { setTpl(v); setVals({}); setSens("") }} options={tpls.map((x) => ({ v: x.id, l: x.name }))} /></Field>
            <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("vault:newItem.vault")}</FieldLabel><Segmented size="sm" value={scope} onChange={setScope} options={[["personal", t("vault:scope.personal")], ["shared", t("vault:scope.shared")]] as const} /></Field>
            <Field><FieldLabel htmlFor="nv-title">{t("vault:newItem.itemTitle")}</FieldLabel><Input id="nv-title" autoFocus maxLength={80} value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("vault:newItem.titlePlaceholder")} data-nv-title /></Field>
            <Field><FieldLabel htmlFor="nv-site">{t("vault:newItem.site")}</FieldLabel><Input id="nv-site" value={site} onChange={(e) => setSite(e.target.value)} placeholder="github.com" data-nv-site /></Field>
            {T?.fields.map((f) => (
              <Field key={f.label}><FieldLabel htmlFor={"nv-f-" + f.label}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label={t("vault:newItem.secret")} />}</FieldLabel>
                {f.secret && /私钥|恢复码|内容/.test(f.label)
                  ? <Textarea id={"nv-f-" + f.label} className="min-h-20 font-mono [-webkit-text-security:disc]" autoComplete="off" spellCheck={false} value={vals[f.label] ?? ""} onChange={(e) => setVals({ ...vals, [f.label]: e.target.value })} data-nv-field={f.label} />
                  : <Input id={"nv-f-" + f.label} type={f.secret ? "password" : "text"} autoComplete={f.secret ? "new-password" : "off"} spellCheck={false} value={vals[f.label] ?? ""} onChange={(e) => setVals({ ...vals, [f.label]: e.target.value })} data-nv-field={f.label} />}
              </Field>))}
            <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("vault:newItem.sensitivity")}</FieldLabel>
              {T?.id === "bank_card" ? <span className="type-callout text-muted-foreground">{SENS_LABEL.high}</span>
                : <Select testId="nv-sens" value={level} onChange={(v) => setSens(v as Sensitivity)} options={(["normal", "sensitive", "high"] as const).map((x) => ({ v: x, l: SENS_LABEL[x] }))} />}</Field>
            <FieldDescription className="type-footnote!">{level === "high" ? t("vault:newItem.highNote") : t("vault:newItem.defaultNote")}</FieldDescription>
          </FieldGroup>
          <button type="submit" hidden />
        </form>}
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!ok} onClick={() => void submit()} data-testid="nv-go">{busy && <Spinner />}{t("common:action.save")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
