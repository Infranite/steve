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
export { usePeopleOn }

/* ───────── codes → 中文 (contacts.md §0.3: the API speaks codes, the web keeps the labels) ───────── */
export const REL: Record<string, string> = { me: "我", colleague: "同事", manager: "上级", customer: "客户", partner: "合作方", friend: "朋友", family: "家人", supplier: "供应商" }
export const relLabel = (r: string) => REL[r] ?? r
export const KIND: Record<string, string> = { real: "真人", virtual: "虚拟账号", unconfirmed: "未确认", bot: "机器人" }
export const ORG_KIND: Record<string, string> = { self: "本公司", customer: "客户", supplier: "供应商", partner: "合作方", other: "其他" }
export const TEAM_KIND: Record<string, string> = { chart: "组织架构", virtual: "虚拟团队" }
export const ROLE: Record<string, string> = { lead: "负责人", member: "成员" }
export const ITEM_ROLE: Record<string, string> = { raised_by: "提出人", related: "相关", waiting_on: "在等", about: "关于", attendee: "参会人" }
export const MEM_KIND: Record<string, string> = { fact: "事实", preference: "偏好", rule: "规则", event: "事件" }
export const TOUCH: Record<string, string> = { mail: "邮件", message: "消息", meeting: "会议", item: "事项", event: "事件" }
export const LEVEL: Record<Level, string> = { public: "公开", conclusion: "可转述结论", reasoning: "仅用于推理", never: "不可使用" }
export const LEVELS = Object.keys(LEVEL) as Level[]
export const visLabel = (v: Visibility) => (v === "workspace" ? "共享" : v === "private" ? "只有我" : v.startsWith("team:") ? "团队可见" : v.startsWith("space:") ? "群成员可见" : v)

export { ContactAvatar } from "@/people/avatar"

export function LevelSel({ v, on, testId }: { v: Level; on: (l: Level) => void; testId?: string }) {
  return <Select align="end" quiet testId={testId} value={v} onChange={(x) => on(x as Level)} options={LEVELS.map((l) => ({ v: l, l: <span className="type-footnote">{LEVEL[l]}</span> }))} />
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
  catch (e) { if (e instanceof Unauthenticated) { st.login = e.login ?? "/auth/login"; emit() } else toast((e as Error).message || "出错了"); return undefined }
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
export const day = (ts: number) => { const d = new Date(ts); return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}` }
