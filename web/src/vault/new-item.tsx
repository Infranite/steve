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
import { vaultApi, SENS_LABEL, type RTemplate, type RVault, type Sensitivity } from "@/api/vault"
import { useBusy, report } from "@/accounts/load"

export function NewVaultDialog({ account, onClose }: { account?: { id: string; display_name: string }; onClose: () => void }) {
  const [tpls, setTpls] = useState<RTemplate[] | null>(null), [vaults, setVaults] = useState<RVault[]>([])
  const [tpl, setTpl] = useState("login"), [title, setTitle] = useState(""), [site, setSite] = useState(""), [vals, setVals] = useState<Record<string, string>>({})
  const [scope, setScope] = useState<"个人" | "共享">("个人"), [sens, setSens] = useState<Sensitivity | "">("")
  const [busy, act] = useBusy()
  useEffect(() => { Promise.all([vaultApi.templates(), vaultApi.vaults()]).then(([t, v]) => { setTpls(t); setVaults(v); if (!t.some((x) => x.id === "login") && t[0]) setTpl(t[0].id) }).catch((e) => { report(e); onClose() }) }, [])
  const T = tpls?.find((t) => t.id === tpl)
  const vault = vaults.find((v) => v.scope === (scope === "个人" ? "personal" : "shared"))
  const level: Sensitivity = sens || T?.default_sensitivity || "sensitive"
  const submit = async () => {
    if (!T || !vault) return
    const r = await act(() => vaultApi.newItem({ vault_id: vault.id, template: T.id, title: title.trim(), site: site.trim() || undefined, account_id: account?.id, sensitivity: level,
      fields: T.fields.map((f) => ({ label: f.label, type: f.type, secret: f.secret, value: vals[f.label] ?? "" })).filter((f) => f.value !== "") }), "已保存")
    setVals({}) // drop the typed values whatever happened
    if (r) { onClose(); go("#/vault/" + r.id) }
  }
  const ok = !!T && !!vault && !!title.trim() && !busy
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl sm:max-w-[480px]">
        <DialogHeader><DialogTitle className="type-headline">新建保险箱条目</DialogTitle><DialogDescription className="type-callout">机密字段加密保存；Bot 只能申请使用，拿不到原值。{account && `关联账号：${account.display_name}。`}</DialogDescription></DialogHeader>
        {!tpls ? <div className="flex justify-center py-8"><Spinner className="size-5 text-muted-foreground" /></div> :
        <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (ok) void submit() }}>
          <FieldGroup className="gap-4">
            <Field orientation="horizontal" className="justify-between"><FieldLabel>模板</FieldLabel><Select testId="nv-tpl" value={tpl} onChange={(v) => { setTpl(v); setVals({}); setSens("") }} options={tpls.map((x) => ({ v: x.id, l: x.name }))} /></Field>
            <Field orientation="horizontal" className="justify-between"><FieldLabel>保险库</FieldLabel><Segmented size="sm" value={scope} onChange={setScope} options={["个人", "共享"] as const} /></Field>
            <Field><FieldLabel htmlFor="nv-title">标题</FieldLabel><Input id="nv-title" autoFocus className="rounded-lg" maxLength={80} value={title} onChange={(e) => setTitle(e.target.value)} placeholder="例如：AWS 根账号" data-nv-title /></Field>
            <Field><FieldLabel htmlFor="nv-site">网站（可选）</FieldLabel><Input id="nv-site" className="rounded-lg" value={site} onChange={(e) => setSite(e.target.value)} placeholder="github.com" data-nv-site /></Field>
            {T?.fields.map((f) => (
              <Field key={f.label}><FieldLabel htmlFor={"nv-f-" + f.label}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label="机密" />}</FieldLabel>
                {f.secret && /私钥|恢复码|内容/.test(f.label)
                  ? <Textarea id={"nv-f-" + f.label} className="min-h-20 rounded-lg font-mono [-webkit-text-security:disc]" autoComplete="off" spellCheck={false} value={vals[f.label] ?? ""} onChange={(e) => setVals({ ...vals, [f.label]: e.target.value })} data-nv-field={f.label} />
                  : <Input id={"nv-f-" + f.label} type={f.secret ? "password" : "text"} autoComplete={f.secret ? "new-password" : "off"} spellCheck={false} className="rounded-lg" value={vals[f.label] ?? ""} onChange={(e) => setVals({ ...vals, [f.label]: e.target.value })} data-nv-field={f.label} />}
              </Field>))}
            <Field orientation="horizontal" className="justify-between"><FieldLabel>敏感度</FieldLabel>
              {T?.id === "bank_card" ? <span className="type-callout text-muted-foreground">高密</span>
                : <Select testId="nv-sens" value={level} onChange={(v) => setSens(v as Sensitivity)} options={(["normal", "sensitive", "high"] as const).map((x) => ({ v: x, l: SENS_LABEL[x] }))} />}</Field>
            <FieldDescription>{level === "high" ? "高密条目不允许授权，Bot 每次使用都要你批准。" : "Bot 使用前默认每次都要你批准，也可以在条目页给某个 Bot 短时授权。"}</FieldDescription>
          </FieldGroup>
          <button type="submit" hidden />
        </form>}
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!ok} onClick={() => void submit()} data-testid="nv-go">{busy && <Spinner />}保存</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
