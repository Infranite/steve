/* Action helpers shared by the accounts, channels, plugins, and vault pages. */
import { useCallback, useState } from "react"
import { Unauthenticated } from "@/api"
import { st, emit, toast } from "@/store"
import { t } from "@/i18n"

/** report an error from an action: a lost session goes to the login screen, anything else is a toast */
export function report(e: unknown) {
  if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit(); return }
  toast((e as Error)?.message || t("accounts:error.generic"))
}

/** run an action with a busy flag; errors are reported, the result (or undefined) returned */
export function useBusy() {
  const [busy, setBusy] = useState(false)
  const act = useCallback(async <R,>(p: () => Promise<R>, ok?: string): Promise<R | undefined> => {
    setBusy(true)
    try { const r = await p(); if (ok) toast(ok); return r } catch (e) { report(e); return undefined } finally { setBusy(false) }
  }, [])
  return [busy, act] as const
}

