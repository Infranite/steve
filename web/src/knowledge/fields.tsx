/* Small shared controls of the knowledge pages: level and visibility pickers, the availability gate. */
import { useEffect, useState, type ReactNode } from "react"
import { Spinner } from "@/components/ui/spinner"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { Page, Empty } from "@/ui"
import { useT } from "@/i18n"
import { kbAvailable, LEVELS, VIS, levelLabel, visLabel, type Level } from "@/api/knowledge"

/** disclosure level; `inherit` adds "跟随来源" (value "") for documents that follow their source */
/** a picker that sits in a row's meta line: footnote-sized and muted like the text around it, its chevron saying it opens */
export const META_SELECT = "ml-0 h-5! gap-0.5 rounded-md px-0 text-muted-foreground hover:bg-transparent hover:text-foreground [&>svg]:size-3! [&>svg]:opacity-70!"
export function LevelSel({ v, on, inherit, testId, className }: { v: Level | ""; on: (l: Level | "") => void; inherit?: Level; testId?: string; className?: string }) {
  const t = useT()
  const opts = [...(inherit ? [{ v: "", l: <span className="type-footnote">{t("knowledge:field.inherit", { label: levelLabel(inherit) })}</span> }] : []), ...LEVELS.map(([l, lab]) => ({ v: l, l: <span className="type-footnote">{lab}</span> }))]
  return <Select align="end" quiet testId={testId ?? "level"} value={v} onChange={(x) => on(x as Level | "")} options={opts} className={className} />
}
/** who can see it: only me / the whole workspace (team and space scopes are shown, set elsewhere) */
export function VisSel({ v, on, inherit, testId, className }: { v: string; on: (x: string) => void; inherit?: string; testId?: string; className?: string }) {
  const t = useT()
  const base = VIS.map(([x, lab]) => ({ v: x, l: <span className="type-footnote">{lab}</span> as ReactNode }))
  if (v && !VIS.some(([x]) => x === v)) base.push({ v, l: <span className="type-footnote">{visLabel(v)}</span> })
  const opts = [...(inherit ? [{ v: "", l: <span className="type-footnote">{t("knowledge:field.inherit", { label: visLabel(inherit) })}</span> }] : []), ...base]
  return <Select align="end" quiet testId={testId ?? "vis"} value={v} onChange={on} options={opts} className={className} />
}

/** renders the page once the Hub is known to serve /api/kb; otherwise the "还没有接入" placeholder */
export function KbGate({ children }: { children: ReactNode }) {
  const t = useT()
  const [ok, setOk] = useState<boolean | null>(null), [err, setErr] = useState("")
  useEffect(() => { let on = true; kbAvailable().then((x) => on && setOk(x), (e) => on && setErr((e as Error).message)); return () => { on = false } }, [])
  if (err) return <Page title={t("knowledge:page.title")} sub={t("knowledge:page.sub")} max={640}><Empty title={t("knowledge:page.unavailable")} sub={err} /></Page>
  if (ok === null) return <div className="flex h-full items-center justify-center" role="status" aria-label={t("knowledge:page.loading")}><Spinner className="size-5 text-muted-foreground" /></div>
  return ok ? <>{children}</> : <Unwired id="knowledge" />
}

export const errMsg = (e: unknown) => (e as Error)?.message ?? String(e)
