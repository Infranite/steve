/* Shared pieces of the contacts pages: labels for the API codes, the avatar, the disclosure-level picker,
 * the data hook that refetches on `party` / `mm` events, and the gate that falls back to the Unwired page. */
import { useEffect, useRef, useState, type ReactNode } from "react"
import { Spinner } from "@/components/ui/spinner"
import { useHubEvents, toast, st, emit } from "@/store"
import { Unauthenticated } from "@/api"
import { SectionLabel } from "@/ui"
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { usePeopleOn, type Level, type Visibility } from "@/api/people"
import { t } from "@/i18n"
import { monthDayShort } from "@/i18n/format"
import zhPeople from "@/i18n/locales/zh-CN/people"
export { usePeopleOn }

/* ───────── codes → labels (contacts.md §0.3: the API speaks codes, the web keeps the labels) ───────── */
type Labels = typeof zhPeople
type Area = "rel" | "kind" | "orgKind" | "teamKind" | "role" | "itemRole" | "memKind" | "touch" | "level" | "source"
/** the label of an API code in the UI language; an unknown code shows as itself. Call it while rendering. */
export const label = (area: Area, code: string): string =>
  Object.hasOwn(zhPeople[area], code) ? t(`people:${area}.${code}` as `people:${Area}.${keyof Labels[Area] & string}`) : code
/** the codes an area knows, in display order */
export const codes = (area: Area) => Object.keys(zhPeople[area])
export const relLabel = (r: string) => label("rel", r)
export const LEVELS = codes("level") as Level[]
/** the names in a list, joined the way the UI language does: "Sarah、Tom" */
export const joinList = (xs: string[]) => xs.join(t("people:shared.list"))
export const visLabel = (v: Visibility) => (v === "workspace" ? t("people:vis.workspace") : v === "private" ? t("people:vis.private") : v.startsWith("team:") ? t("people:vis.team") : v.startsWith("space:") ? t("people:vis.space") : v)

export { ContactAvatar } from "@/people/avatar"

export function LevelSel({ v, on, testId }: { v: Level; on: (l: Level) => void; testId?: string }) {
  return <Select align="end" quiet testId={testId} value={v} onChange={(x) => on(x as Level)} options={LEVELS.map((l) => ({ v: l, l: <span className="type-footnote">{label("level", l)}</span> }))} />
}
export function Section({ label, right, children }: { label: string; right?: ReactNode; children: ReactNode }) {
  return <section><SectionLabel right={right}>{label}</SectionLabel>{children}</section>
}

/* ───────── data ───────── */
/** load with `f`, again whenever a contacts event arrives (debounced) or `deps` change; `null` while loading.
 *  `err` is the last failure (with the HTTP status when there was one). */
export function useLoad<T>(f: () => Promise<T>, deps: unknown[], events: string[] = ["party", "mm", "decision"]) {
  const [data, setData] = useState<T | null>(null), [err, setErr] = useState<(Error & { status?: number }) | null>(null)
  const fn = useRef(f); fn.current = f
  const n = useRef(0)
  const load = () => {
    const k = ++n.current
    fn.current().then((d) => { if (k === n.current) { setData(d); setErr(null) } }, (e) => {
      if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit(); return }
      if (k === n.current) setErr(e)
    })
  }
  useEffect(() => { setData(null); setErr(null); load() }, deps) // eslint-disable-line react-hooks/exhaustive-deps
  useHubEvents(events, load, 150)
  return { data, err, reload: load, set: setData }
}

/** run a write; report failures as a toast; the caller's `useLoad` refreshes from the SSE event */
export async function act<T>(p: Promise<T>, ok?: string): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); return r }
  catch (e) { if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit() } else toast((e as Error).message || t("people:shared.failed")); return undefined }
}

/** the module's pages: the Unwired placeholder when the Hub has no contacts module, never sample data */
export function PeopleGate({ children }: { children: () => ReactNode }) {
  const on = usePeopleOn()
  if (on === null) return <div className="flex h-full items-center justify-center"><Spinner className="size-5 text-muted-foreground" /></div>
  if (!on) return <Unwired id="contacts" />
  return <>{children()}</>
}
/** ui's Meta as inline elements: a Row's sub is a <p>, which cannot hold Meta's <div> */
export const SubMeta = ({ children }: { children: ReactNode }) => (
  <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 type-footnote text-muted-foreground [&>*+*]:before:mr-1.5 [&>*+*]:before:text-tertiary [&>*+*]:before:content-['·']">{children}</span>
)
export const Loading = () => <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
/** "YYYY-MM" or a date → "10-08" style */
export const day = (ts: number) => monthDayShort(ts)
