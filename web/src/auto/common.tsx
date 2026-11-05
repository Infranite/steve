/* Small pieces shared by the signal, flow, schedule and source pages */
import type { ReactNode } from "react"
import { cn } from "@/lib/utils"
import { BellIcon, CalendarIcon, ClockIcon, GitPullRequestIcon, HandIcon, ListTodoIcon, MailIcon, MessageSquareIcon, RadioIcon, SirenIcon, type LucideIcon } from "lucide-react"
import { msDur, nearTime } from "@/i18n/format"
import { Select as UISelect, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Page, Empty } from "@/ui"
import { Spinner } from "@/components/ui/spinner"
import { st } from "@/store"

export const SOURCE_ICON: Record<string, LucideIcon> = { mail: MailIcon, cal: CalendarIcon, im: MessageSquareIcon, github: GitPullRequestIcon, webhook: SirenIcon, time: ClockIcon, item: ListTodoIcon, manual: HandIcon, notify: BellIcon }
export const srcIcon = (s: string | null | undefined) => (s && SOURCE_ICON[s]) || RadioIcon
export const Ico = ({ I, dark }: { I: LucideIcon; dark?: boolean }) => (
  <span className={cn("flex size-7 shrink-0 items-center justify-center rounded-md", dark ? "bg-foreground text-background" : "bg-muted text-muted-foreground")} aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
)

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
      <DialogContent className={cn("max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl", wide ? "sm:max-w-[640px]" : "sm:max-w-[480px]")}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}

export const Loading = ({ title, back }: { title: string; back?: [string, string] }) => (
  <Page title={title} back={back}><div className="flex justify-center py-16" role="status" aria-label="载入中"><Spinner className="size-5 text-muted-foreground" /></div></Page>
)
export const Failed = ({ title, back, msg, retry }: { title: string; back?: [string, string]; msg: string; retry?: () => void }) => (
  <Page title={title} back={back}><Empty title="载入失败" sub={msg} />{retry && <div className="flex justify-center"><button type="button" className="type-callout text-blue hover:underline" onClick={retry}>重试</button></div>}</Page>
)

/** bots of this workspace, the main one first */
export const botOptions = () => Object.keys(st.bots).filter((k) => st.bots[k].on || st.bots[k].main).sort((a, b) => Number(!!st.bots[b].main) - Number(!!st.bots[a].main)).map((k) => ({ v: k, l: st.bots[k].main ? st.bots[k].n : st.bots[k].n + " Bot" }))
export const mainBot = () => Object.keys(st.bots).find((k) => st.bots[k].main) ?? "steve"

/** a future time: “今天 08:30”“明天 08:30”“周五 09:00”“11-12 09:00” */
export const when = (ts: number | null | undefined) => nearTime(ts)
export const ms = (n: number) => msDur(n)
export const dur = (a: number, b: number | null) => (b ? ms(b - a) : "")

export function Json({ v, className }: { v: unknown; className?: string }) {
  return <pre className={cn("max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted/70 px-3 py-2 font-mono text-[11.5px] leading-[18px] text-foreground/85", className)}>{typeof v === "string" ? v : JSON.stringify(v, null, 2)}</pre>
}

export const RUN_STATUS: Record<string, [string, string]> = { running: ["运行中", "run"], waiting: ["等待中", "wait"], done: ["完成", "done"], failed: ["失败", "fail"], cancelled: ["已取消", "idle"], skipped: ["跳过", "idle"] }
