/* Small shared controls of the knowledge pages: level and visibility pickers, the availability gate. */
import { useEffect, useState, type ReactNode } from "react"
import { Spinner } from "@/components/ui/spinner"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { Page, Empty } from "@/ui"
import { kbAvailable, LEVELS, VIS, levelLabel, visLabel, type Level } from "@/api/knowledge"

/** disclosure level; `inherit` adds "跟随来源" (value "") for documents that follow their source */
export function LevelSel({ v, on, inherit, testId }: { v: Level | ""; on: (l: Level | "") => void; inherit?: Level; testId?: string }) {
  const opts = [...(inherit ? [{ v: "", l: <span className="type-footnote">跟随来源（{levelLabel(inherit)}）</span> }] : []), ...LEVELS.map(([l, t]) => ({ v: l, l: <span className="type-footnote">{t}</span> }))]
  return <Select align="end" quiet testId={testId ?? "level"} value={v} onChange={(x) => on(x as Level | "")} options={opts} />
}
/** who can see it: only me / the whole workspace (team and space scopes are shown, set elsewhere) */
export function VisSel({ v, on, inherit, testId }: { v: string; on: (x: string) => void; inherit?: string; testId?: string }) {
  const base = VIS.map(([x, t]) => ({ v: x, l: <span className="type-footnote">{t}</span> as ReactNode }))
  if (v && !VIS.some(([x]) => x === v)) base.push({ v, l: <span className="type-footnote">{visLabel(v)}</span> })
  const opts = [...(inherit ? [{ v: "", l: <span className="type-footnote">跟随来源（{visLabel(inherit)}）</span> }] : []), ...base]
  return <Select align="end" quiet testId={testId ?? "vis"} value={v} onChange={on} options={opts} />
}

/** renders the page once the Hub is known to serve /api/kb; otherwise the "还没有接入" placeholder */
export function KbGate({ children }: { children: ReactNode }) {
  const [ok, setOk] = useState<boolean | null>(null), [err, setErr] = useState("")
  useEffect(() => { let on = true; kbAvailable().then((x) => on && setOk(x), (e) => on && setErr((e as Error).message)); return () => { on = false } }, [])
  if (err) return <Page title="知识" sub="Steve 记得的事和能查到的资料" max={640}><Empty title="知识库暂时打不开" sub={err} /></Page>
  if (ok === null) return <div className="flex h-full items-center justify-center" role="status" aria-label="正在加载"><Spinner className="size-5 text-muted-foreground" /></div>
  return ok ? <>{children}</> : <Unwired id="knowledge" />
}

export const errMsg = (e: unknown) => (e as Error)?.message ?? String(e)
