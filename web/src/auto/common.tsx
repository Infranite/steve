/* Small pieces shared by the signal, flow, schedule and source pages */
import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "@/lib/utils"
import { BellIcon, CalendarIcon, ClockIcon, GitPullRequestIcon, HandIcon, ListTodoIcon, MailIcon, MessageSquareIcon, RadioIcon, SirenIcon, type LucideIcon } from "lucide-react"
import { msDur, nearTime } from "@/i18n/format"
import { Select as UISelect, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Page, Empty } from "@/ui"
import { Spinner } from "@/components/ui/spinner"
import { st } from "@/store"
import { t, useT } from "@/i18n"

export const SOURCE_ICON: Record<string, LucideIcon> = { mail: MailIcon, cal: CalendarIcon, im: MessageSquareIcon, github: GitPullRequestIcon, webhook: SirenIcon, time: ClockIcon, item: ListTodoIcon, manual: HandIcon, notify: BellIcon }
export const srcIcon = (s: string | null | undefined) => (s && SOURCE_ICON[s]) || RadioIcon

/** compact select with an accessible name */
export function Pick<T extends string>({ value, options, onChange, label, testId, className, disabled }: { value: T; options: readonly { v: T; l: ReactNode }[]; onChange: (v: T) => void; label: string; testId?: string; className?: string; disabled?: boolean }) {
  return (
    <UISelect items={Object.fromEntries(options.map((o) => [o.v, o.l]))} value={value} disabled={disabled} onValueChange={(v) => v != null && onChange(v as T)}>
      <SelectTrigger size="sm" aria-label={label} className={cn("rounded-lg border-border bg-transparent font-normal", className)} data-pick={testId}><SelectValue /></SelectTrigger>
      <SelectContent align="end" className="min-w-36">{options.map((o) => <SelectItem key={o.v} value={o.v}>{o.l}</SelectItem>)}</SelectContent>
    </UISelect>
  )
}

export function Modal({ title, desc, children, footer, wide, onClose }: { title: string; desc?: string; children: ReactNode; footer?: ReactNode; wide?: boolean; onClose: () => void }) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className={cn("max-h-[90dvh] gap-4 overflow-y-auto", wide ? "sm:max-w-[640px]" : "sm:max-w-[480px]")}>
        <DialogHeader><DialogTitle className="type-title">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}

export function Loading({ title, back }: { title: string; back?: [string, string] }) {
  const t = useT()
  return <Page title={title} back={back}><div className="flex justify-center py-16 anim-in" role="status" aria-label={t("auto:loadingAria")}><Spinner className="size-5 text-muted-foreground" /></div></Page>
}
export function Failed({ title, back, msg, retry }: { title: string; back?: [string, string]; msg: string; retry?: () => void }) {
  const t = useT()
  return <Page title={title} back={back}><Empty title={t("auto:loadFailed")} sub={msg} />{retry && <div className="flex justify-center"><button type="button" className="type-callout text-blue hover:underline" onClick={retry}>{t("common:action.retry")}</button></div>}</Page>
}

/** bots of this workspace, the main one first */
export const botOptions = () => Object.keys(st.bots).filter((k) => st.bots[k].on || st.bots[k].main).sort((a, b) => Number(!!st.bots[b].main) - Number(!!st.bots[a].main)).map((k) => ({ v: k, l: st.bots[k].main ? st.bots[k].n : t("auto:bot.named", { name: st.bots[k].n }) }))
export const mainBot = () => Object.keys(st.bots).find((k) => st.bots[k].main) ?? "steve"

/** a future time: “今天 08:30”“明天 08:30”“周五 09:00”“11-12 09:00” */
export const when = (ts: number | null | undefined) => nearTime(ts)
export const ms = (n: number) => msDur(n)
export const dur = (a: number, b: number | null) => (b ? ms(b - a) : "")

export function Json({ v, className }: { v: unknown; className?: string }) {
  return <pre className={cn("max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted/60 px-3 py-2 type-code text-foreground/85", className)}>{typeof v === "string" ? v : JSON.stringify(v, null, 2)}</pre>
}

/** a run status → [label, StatusDot state]; the label is read in the current language each time */
const RUN_DOT = { running: "run", waiting: "wait", done: "done", failed: "fail", cancelled: "idle", skipped: "idle" } as const
export const RUN_STATUS: Record<string, [string, string]> = Object.defineProperties({}, Object.fromEntries(Object.entries(RUN_DOT).map(([k, dot]) =>
  [k, { enumerable: true, get: (): [string, string] => [t(`auto:runStatus.${k as keyof typeof RUN_DOT}`), dot] }])))

/** long text (an error, a reason) as at most `lines` lines; “全文” opens the rest in place when it is cut */
export function Clamp({ text, lines = 2, className }: { text: string; lines?: 1 | 2 | 3; className?: string }) {
  const t = useT(), ref = useRef<HTMLSpanElement>(null)
  const [open, setOpen] = useState(false), [cut, setCut] = useState(false)
  useLayoutEffect(() => { const el = ref.current; if (el && !open) setCut(el.scrollHeight > el.clientHeight + 1) }, [text, open, lines])
  const lc = lines === 1 ? "line-clamp-1" : lines === 2 ? "line-clamp-2" : "line-clamp-3"
  return (
    <span className={cn("block min-w-0", className)} data-clamp>
      <span ref={ref} className={cn("block [overflow-wrap:anywhere]", !open && lc)}>{text}</span>
      {(cut || open) && <button type="button" className="type-footnote text-muted-foreground hover:text-foreground hover:underline" onClick={(e) => { e.stopPropagation(); setOpen(!open) }} aria-expanded={open} data-clamp-toggle>{open ? t("auto:less") : t("auto:more")}</button>}
    </span>
  )
}
