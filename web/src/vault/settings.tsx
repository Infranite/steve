/* 设置 · 保险箱 (accounts-vault.md §8): passkeys for local verification, the key store and its state, recovery and key rotation. */
import { useState } from "react"
import { LockIcon, TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Group, Row, SectionLabel, Empty, go } from "@/ui"
import { tm } from "@/store"
import { vaultApi, keyStoreState, keyStoreWhere } from "@/api/vault"
import { useLoad, useBusy } from "@/accounts/load"
import { RegisterPasskey } from "@/vault/reveal"
import { RecoveryCode } from "@/vault/index"
import { lazy } from "@/api/accounts"
import { t as tr, useT } from "@/i18n"

/** the settings category: [id, label, icon, description] like manage.tsx CATS; label and description are read at render time */
export const VAULT_CAT: [string, string, any, string] = lazy<[string, string, any, string]>([() => "vault", () => tr("vault:settings.cat"), () => LockIcon, () => tr("vault:settings.catSub")])

export function VaultSettingsPane() {
  const t = useT()
  const L = useLoad(async () => {
    const status = await vaultApi.status()
    return { status, passkeys: status.initialized ? await vaultApi.passkeys().catch(() => null) : null }
  }, [], ["vault"])
  const [busy, act] = useBusy(), [code, setCode] = useState(""), [rotated, setRotated] = useState<string | null>(null), [ask, setAsk] = useState(false), [rm, setRm] = useState<{ id: string; name: string } | null>(null)
  if (L.s === "loading") return <div className="flex justify-center py-12"><Spinner className="size-5 text-muted-foreground" /></div>
  if (L.s === "unwired") return <Empty title={t("vault:settings.unwired")} sub={t("vault:settings.unwiredSub")} />
  if (L.s === "error") return <Empty title={L.err} />
  const { status: s, passkeys } = L.data
  if (!s.initialized) return <Group><Row title={t("vault:settings.notEnabled")} sub={t("vault:settings.notEnabledSub")} onClick={() => go("#/vault")} controls><Button size="sm" variant="outline" onClick={() => go("#/vault")}>{t("vault:settings.goEnable")}</Button></Row></Group>
  if (rotated) return <RecoveryCode title={t("vault:recovery.newTitle")} code={rotated} onDone={() => { setRotated(null); void L.reload() }} />
  return (
    <div className="flex flex-col gap-8" data-vault-settings-pane>
      <AlertDialog open={!!rm} onOpenChange={(o) => !o && setRm(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("vault:settings.deletePasskeyTitle", { name: rm?.name })}</AlertDialogTitle><AlertDialogDescription>{t("vault:settings.deletePasskeyBody")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (rm && await act(() => vaultApi.deletePasskey(rm.id), t("vault:item.deleted"))) void L.reload() }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <AlertDialog open={ask} onOpenChange={setAsk}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("vault:settings.rotateTitle")}</AlertDialogTitle><AlertDialogDescription>{t("vault:settings.rotateBody")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction onClick={async () => { const r = await act(() => vaultApi.rotate()); if (r) setRotated(r.recovery_code) }} data-rotate-confirm>{t("vault:settings.rotate")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <section><SectionLabel>{t("vault:settings.localVerify")}</SectionLabel>
        {passkeys?.length ? <Group>{passkeys.map((p) => <Row key={p.id} title={p.name} sub={p.last_used ? t("vault:settings.passkeySubUsed", { rp: p.rp_id, created: tm(p.created), used: tm(p.last_used) }) : t("vault:settings.passkeySub", { rp: p.rp_id, created: tm(p.created) })} data-passkey={p.name}>
          <Button variant="ghost" size="icon-xs" aria-label={t("vault:settings.deletePasskey", { name: p.name })} className="text-muted-foreground" onClick={() => setRm(p)}><TrashIcon /></Button></Row>)}</Group>
          : <div className="px-1 type-callout text-muted-foreground">{t("vault:settings.noPasskeys")}</div>}
        <div className="mt-3"><RegisterPasskey onDone={() => void L.reload()} /></div>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("vault:settings.passkeyNote")}</div>
      </section>
      <section><SectionLabel>{t("vault:settings.masterKey")}</SectionLabel><Group>
        <Row title={t("vault:settings.location")} sub={s.keystore?.label ?? t("vault:settings.macKeychain")} data-keystore={s.keystore?.kind ?? "keychain"} />
        <Row title={t("vault:settings.state")} sub={s.keychain === "ok" ? t("vault:settings.keyPresent", { label: s.keystore?.label ?? t("vault:settings.macKeychain") }) : keyStoreState(s)} data-keychain-state={s.keychain} />
        <Row title={t("vault:settings.version")} sub={s.rotating ? t("vault:settings.rotating") : t("vault:settings.onlyIn", { where: keyStoreWhere(s) })}><span className="type-callout tabular-nums text-muted-foreground">v{s.kek_version}</span></Row>
        <Row title={t("vault:settings.rotateRow")} sub={t("vault:settings.rotateRowSub")}><Button size="sm" variant="outline" disabled={busy || s.rotating || s.keychain !== "ok"} onClick={() => setAsk(true)} data-rotate>{t("vault:settings.rotate")}</Button></Row>
      </Group></section>
      <section><SectionLabel>{t("vault:settings.recoverSection")}</SectionLabel>
        <form className="flex flex-wrap items-center gap-2" autoComplete="off" onSubmit={async (e) => { e.preventDefault(); const r = await act(() => vaultApi.recover(code.trim()), t("vault:settings.recovered")); setCode(""); if (r) void L.reload() }}>
          <Input aria-label={t("vault:recovery.label")} type="password" autoComplete="off" spellCheck={false} className="h-8 min-w-0 flex-1 rounded-lg font-mono" value={code} onChange={(e) => setCode(e.target.value)} placeholder={t("vault:settings.recoverPlaceholder")} data-recover-code />
          <Button size="sm" variant="outline" type="submit" disabled={busy || !code.trim()} data-recover>{t("vault:settings.recover")}</Button>
        </form>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("vault:settings.recoverNote")}</div>
      </section>
    </div>
  )
}
