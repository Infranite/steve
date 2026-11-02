/* Loading module data from the Hub: one hook for the accounts and vault pages. A Hub without the module (the TS Hub, or
 * a network error) turns into "unwired", which the page answers with its Unwired placeholder — never sample data. */
import { useCallback, useEffect, useRef, useState } from "react"
import { Unauthenticated, NotWired, HubError } from "@/api"
import { st, emit, useHubEvents, toast } from "@/store"

export type Load<T> = { s: "loading" } | { s: "unwired" } | { s: "error"; err: string; status: number } | { s: "ok"; data: T }

/** report an error from an action: a lost session goes to the login screen, anything else is a toast */
export function report(e: unknown) {
  if (e instanceof Unauthenticated) { st.login = e.login; emit(); return }
  toast((e as Error)?.message || "出错了")
}

/** `fn` runs on mount, when `deps` change, and when the stream says something of `types` changed */
export function useLoad<T>(fn: () => Promise<T>, deps: unknown[], types: string[]): Load<T> & { reload: () => Promise<void> } {
  const [v, setV] = useState<Load<T>>({ s: "loading" })
  const seq = useRef(0)
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const run = useCallback(async () => {
    const n = ++seq.current
    try { const data = await fn(); if (n === seq.current) setV({ s: "ok", data }) }
    catch (e) {
      if (n !== seq.current) return
      if (e instanceof Unauthenticated) { st.login = e.login; emit(); return }
      if (e instanceof NotWired) setV({ s: "unwired" })
      else setV({ s: "error", err: (e as Error).message, status: e instanceof HubError ? e.status : 0 })
    }
  }, deps)
  useEffect(() => { void run() }, [run])
  useHubEvents(types, () => void run(), 150)
  return { ...v, reload: run }
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

/** `Meta` for a Row's subtitle: spans only, since the subtitle is a <p> */
export const Dots = ({ children }: { children: React.ReactNode }) => (
  <span className="inline-flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 [&>*+*]:before:mr-1.5 [&>*+*]:before:text-tertiary [&>*+*]:before:content-['·']">{children}</span>
)
