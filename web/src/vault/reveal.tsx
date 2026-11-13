/* Viewing or copying a secret value (accounts-vault.md §4.2): a WebAuthn assertion with user verification (Touch ID /
 * Face ID / the device's passkey) per reveal. The value lives only in this component's state and is dropped after the
 * Hub's ttl (30 s), on unmount, and when the tab is hidden. It is never logged. */
import { useCallback, useEffect, useRef, useState } from "react"
import { FingerprintIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { api } from "@/api"
import { HubError } from "@/api"
import { vaultApi, NEED_PASSKEY } from "@/api/vault"
import { toast } from "@/store"
import { t as tr, useT } from "@/i18n"
import { report } from "@/accounts/load"
import { createPasskey, getAssertion, webauthnAvailable, webauthnError } from "@/vault/webauthn"

const deviceName = () => { const u = navigator.userAgent; return /iPhone/.test(u) ? "iPhone" : /iPad/.test(u) ? "iPad" : /Mac/.test(u) ? "Mac" : /Android/.test(u) ? tr("vault:passkey.androidDevice") : /Windows/.test(u) ? tr("vault:passkey.windowsPc") : tr("vault:passkey.thisDevice") }

/** register a passkey for this origin; `onStale` when the login is older than 10 minutes (401 请重新登录后再登记) */
export async function registerPasskey(name: string): Promise<"ok" | "stale" | "failed"> {
  try {
    const ccr = await vaultApi.registerStart(name)
    const cred = await createPasskey(ccr)
    await vaultApi.registerFinish(cred)
    toast(tr("vault:passkey.registered"))
    return "ok"
  } catch (e) {
    if (e instanceof HubError && e.status === 401) return "stale"
    if (e instanceof HubError) report(e); else toast(webauthnError(e))
    return "failed"
  }
}
/** registration wants a fresh login: sign out, and the login screen brings the user back to this page */
export const relogin = async () => { await api.logout().catch(() => {}); location.reload() }

/** `row`: as the last row of a panel (the name field takes the width, the button trails) */
export function RegisterPasskey({ onDone, compact, row }: { onDone: () => void; compact?: boolean; row?: boolean }) {
  const t = useT()
  const [n, setN] = useState(deviceName()), [busy, setBusy] = useState(false), [stale, setStale] = useState(false)
  if (!webauthnAvailable()) return <div className={cn("type-callout text-muted-foreground", row && "px-3 py-2.5")}>{t("vault:passkey.unavailable")}</div>
  if (stale) return <div className="flex flex-col gap-2 rounded-xl bg-attention/12 px-4 py-3" role="alert"><div className="type-callout">{t("vault:passkey.stale")}</div><Button size="sm" className="self-start" onClick={() => void relogin()} data-relogin>{t("vault:passkey.relogin")}</Button></div>
  return (
    <form className={row ? "flex min-w-0 items-center gap-2 px-3 py-2.5" : "flex flex-wrap items-center gap-2"} onSubmit={async (e) => { e.preventDefault(); setBusy(true); const r = await registerPasskey(n.trim() || deviceName()); setBusy(false); if (r === "stale") setStale(true); if (r === "ok") onDone() }}>
      <Input aria-label={t("vault:passkey.name")} className={row ? "h-8 min-w-0 flex-1" : compact ? "h-8 w-40" : "h-8 w-48"} maxLength={60} value={n} onChange={(e) => setN(e.target.value)} data-passkey-name />
      <Button size="sm" variant={row ? "outline" : "default"} type="submit" disabled={busy} data-register-passkey>{busy ? <Spinner /> : <FingerprintIcon />}{t("vault:passkey.register")}</Button>
    </form>
  )
}

type Shown = { value: string; until: number }
export function useReveal(itemId: string, onUsed: () => void) {
  const t = useT()
  const [shown, setShown] = useState<Record<string, Shown>>({}), [busy, setBusy] = useState<string | null>(null), [need, setNeed] = useState<null | { fid: string; copy: boolean }>(null)
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>())
  const hide = useCallback((fid?: string) => {
    if (fid) { clearTimeout(timers.current.get(fid)); timers.current.delete(fid); setShown((s) => { const { [fid]: _, ...rest } = s; return rest }) }
    else { timers.current.forEach(clearTimeout); timers.current.clear(); setShown({}) }
  }, [])
  useEffect(() => { const v = () => { if (document.hidden) hide() }; document.addEventListener("visibilitychange", v); return () => { document.removeEventListener("visibilitychange", v); hide() } }, [hide])
  const reveal = useCallback(async (fid: string, copy: boolean) => {
    if (!webauthnAvailable()) { toast(tr("vault:reveal.unavailable")); return }
    setBusy(fid)
    try {
      let rcr: any
      try { rcr = await vaultApi.revealStart(itemId, fid) } catch (e) {
        if (e instanceof HubError && e.status === 400 && e.message === NEED_PASSKEY) { setNeed({ fid, copy }); return }
        throw e
      }
      const assertion = await getAssertion(rcr)
      const r = await vaultApi.revealFinish(itemId, fid, assertion, copy)
      onUsed()
      if (copy) {
        try { await navigator.clipboard.writeText(r.value); toast(tr("vault:reveal.copied")) } catch { toast(tr("vault:clipboardDenied")) }
        return
      }
      const ttl = Math.min(Math.max(r.ttl || 30, 1), 30) * 1000
      clearTimeout(timers.current.get(fid))
      timers.current.set(fid, setTimeout(() => hide(fid), ttl))
      setShown((s) => ({ ...s, [fid]: { value: r.value, until: Date.now() + ttl } }))
    } catch (e) {
      /* the browser's ceremony fails with a DOMException (cancelled, no authenticator); the Hub with a HubError */
      if (e instanceof DOMException) toast(webauthnError(e)); else report(e)
    } finally { setBusy(null) }
  }, [itemId, onUsed, hide])
  const dialog = (
    <AlertDialog open={!!need} onOpenChange={(o) => !o && setNeed(null)}><AlertDialogContent data-need-passkey>
      <AlertDialogHeader><AlertDialogTitle>{t("vault:reveal.needPasskey")}</AlertDialogTitle>
        <AlertDialogDescription>{t("vault:reveal.needPasskeyBody")}</AlertDialogDescription></AlertDialogHeader>
      <RegisterPasskey onDone={() => { const n = need; setNeed(null); if (n) void reveal(n.fid, n.copy) }} />
      <AlertDialogFooter><AlertDialogCancel>{t("vault:reveal.later")}</AlertDialogCancel></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  )
  return { shown, busy, reveal, hide, dialog }
}

/** seconds left of a shown value, for the label next to it */
export function Countdown({ until }: { until: number }) {
  const t = useT()
  const [, f] = useState(0)
  useEffect(() => { const i = setInterval(() => f((x) => x + 1), 1000); return () => clearInterval(i) }, [])
  return <span className="tabular-nums" aria-live="off">{t("vault:reveal.hidesIn", { count: Math.max(0, Math.ceil((until - Date.now()) / 1000)) })}</span>
}
