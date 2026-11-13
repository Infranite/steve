/* What a ```a2ui block needs from where it is shown (ST-46), kept apart from the renderer so the chat loads without it:
 * the reply it lives in (to submit, and what was submitted already), and whether it is already inside an interface. */
import { createContext, useContext, useMemo, type ReactNode } from "react"
import type { A2uiRecord } from "@/api"
import { useT } from "@/i18n"

export type Host = { msg: string; records: Record<string, A2uiRecord>; readonly?: string }
export const HostCtx = createContext<Host | null>(null)
/** around a reply's text: lets its interfaces submit, and tells them what was submitted already; `readonly`: why they can't */
export function A2uiHost({ msg, records, readonly, children }: { msg?: string; records?: Record<string, A2uiRecord>; readonly?: string; children: ReactNode }) {
  const v = useMemo(() => (msg ? { msg, records: records ?? {}, readonly } : null), [msg, records, readonly])
  return <HostCtx.Provider value={v}>{children}</HostCtx.Provider>
}
/* a block inside an interface's Text is shown as code, never drawn again */
export const Nested = createContext(false)
export const useA2uiNested = () => useContext(Nested)

/** while the block is still being written, or (`loading`) a finished one waits for the renderer */
export function A2uiPending({ loading }: { loading?: boolean }) {
  const t = useT()
  return (
  <div className="my-3 flex items-center gap-2.5 rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground anim-in" data-a2ui-pending={loading ? undefined : true} data-a2ui-loading={loading || undefined}>
    <span className="size-1.5 rounded-full bg-blue dot-running" aria-hidden />{loading ? t("misc:a2ui.loading") : t("misc:a2ui.generating")}
  </div>
  )
}
