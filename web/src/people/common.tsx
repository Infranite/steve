/* Shared pieces of the contacts pages: labels for the API codes, the avatar, and the disclosure-level picker. */
export { Section, Loading } from "@/ui"
import { toast, st, emit } from "@/store"
import { Unauthenticated } from "@/api"
import { Select } from "@/view-shared"
import type { ContactBrief, Level, Visibility } from "@/api/people"
import { t } from "@/i18n"
import { monthDayShort } from "@/i18n/format"
import zhPeople from "@/i18n/locales/zh-CN/people"

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

export function LevelSel({ v, on, testId }: { v: Level; on: (l: Level) => void; testId?: string }) {
  return <Select align="end" quiet testId={testId} value={v} onChange={(x) => on(x as Level)} options={LEVELS.map((l) => ({ v: l, l: <span className="type-footnote">{label("level", l)}</span> }))} />
}

/* ───────── data ───────── */
export const PEOPLE_EVENTS = ["party", "mm", "patch"] as const

/** run a write; report failures as a toast; queries refresh from the SSE event */
export async function act<T>(p: Promise<T>, ok?: string): Promise<T | undefined> {
  try { const r = await p; if (ok) toast(ok); return r }
  catch (e) { if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit() } else toast((e as Error).message || t("people:shared.failed")); return undefined }
}

/** "YYYY-MM" or a date → "10-08" style */
export const day = (ts: number) => monthDayShort(ts)

/** “公司 · 部门路径 · 职位” */
export const orgLine = (c: Pick<ContactBrief, "org">) => [c.org?.name, c.org?.dept, c.org?.title].filter(Boolean).join(" · ")
