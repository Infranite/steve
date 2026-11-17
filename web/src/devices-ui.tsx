/* The pieces the devices pages are laid out with: a card (affectUI's Card) with an icon tile, a title, one quiet line
 * and an action; rows with hairlines between them, words on the left and a control on the right; a status badge
 * (affectUI's Badge) for the one state device-model gives a Node; and a figure with its bar. */
import type { ReactNode } from "react"
import { ChevronRightIcon } from "lucide-react"
import { cn } from "cn"
import { Card } from "@/components/affect/card-clean"
import { Badge } from "@/components/ui/badge"
import { Meter } from "@/panel"
import { fillTone, statusText, type NodeStatus } from "@/device-model"

type Data = { [k: `data-${string}`]: unknown }

/** a card of the devices pages: the head (icon tile, title, one line, an action), then rows or content */
export function DCard({ icon, title, sub, action, children, className, id, ...rest }: { icon?: ReactNode; title?: ReactNode; sub?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string; id?: string } & Data) {
  return (
    <Card as="section" id={id} className={cn("steve-card min-w-0 overflow-hidden p-0", className)} {...rest}>
      {title != null && (
        <header className="flex items-start gap-3 px-5 pt-4 pb-3">
          {icon && <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4 [&_svg]:stroke-[1.75]" aria-hidden>{icon}</span>}
          <div className="min-w-0 flex-1">
            <h2 className="type-headline text-foreground [overflow-wrap:anywhere]">{title}</h2>
            {sub && <div className="type-footnote text-pretty text-muted-foreground">{sub}</div>}
          </div>
          {action && <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">{action}</div>}
        </header>
      )}
      {children}
    </Card>
  )
}

/** a row of a card: icon, title, a line of facts, and a control (or a chevron when the whole row opens something) */
export function DRow({ icon, title, sub, children, onClick, className, ...rest }: { icon?: ReactNode; title: ReactNode; sub?: ReactNode; children?: ReactNode; onClick?: () => void; className?: string } & Data) {
  const body = <>
    {icon && <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4 [&_svg]:stroke-[1.75]" aria-hidden>{icon}</span>}
    <span className="flex min-w-0 flex-1 flex-col gap-0.5">
      <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 type-body text-foreground [overflow-wrap:anywhere]">{title}</span>
      {sub && <span className="flex min-w-0 flex-wrap gap-x-2.5 gap-y-0.5 type-footnote text-muted-foreground">{sub}</span>}
    </span>
    {children && <span className="flex shrink-0 flex-wrap items-center justify-end gap-2">{children}</span>}
    {onClick && <ChevronRightIcon className="size-4 shrink-0 text-tertiary" aria-hidden />}
  </>
  const cls = cn("flex w-full min-w-0 items-center gap-3 border-border/70 px-5 py-3 text-left [&+&]:border-t", onClick && "cursor-pointer transition-colors duration-120 ease-(--ease-out) hover:bg-accent/60", className)
  return onClick ? <button type="button" onClick={onClick} className={cls} {...rest}>{body}</button> : <div className={cls} {...rest}>{body}</div>
}

/** a small label beside a title ("Hub 内置", "你") */
export const Chip = ({ children, tone = "neutral" }: { children: ReactNode; tone?: "neutral" | "info" | "warning" }) =>
  <Badge tone={tone} size="sm" className="font-normal">{children}</Badge>

const BADGE: Record<NodeStatus["tone"], "success" | "warning" | "danger" | "info" | "neutral"> = { ok: "success", warn: "warning", bad: "danger", info: "info", off: "neutral", neutral: "neutral" }
/** the one state of a Node, as a badge; `quiet`: an online Node shows nothing */
export function StatusBadge({ status, quiet = false, now }: { status: NodeStatus; quiet?: boolean; now?: number }) {
  if (quiet && status.kind === "online") return null
  return <Badge tone={BADGE[status.tone]} size="sm" dot live={status.kind === "upgrading"} data-node-status={status.kind}>{statusText(status, now)}</Badge>
}

/** the dot before a Node's name: green online, amber when it wants you, red when it went wrong, grey otherwise */
export function StatusDot({ status }: { status: NodeStatus }) {
  const c = { ok: "bg-success", warn: "bg-attention", bad: "bg-destructive", info: "bg-blue", off: "bg-muted-foreground/50", neutral: "bg-muted-foreground" }[status.tone]
  return <span aria-hidden className={cn("size-2 shrink-0 rounded-full", c)} />
}

/** a figure: its label, the value large, a bar for a fill, and a line under it */
export function Figure({ label, value, unit, fill, sub, title, ...rest }: { label: ReactNode; value: ReactNode; unit?: ReactNode; fill?: number | null; sub?: ReactNode; title?: string } & Data) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5" title={title} {...rest}>
      <span className="type-footnote text-muted-foreground">{label}</span>
      <span className="type-title tabular-nums text-foreground">{value}{unit && <small className="ml-0.5 type-footnote font-normal text-muted-foreground">{unit}</small>}</span>
      {fill !== undefined && <Meter value={fill} tone={fillTone(fill) ?? "run"} />}
      {sub && <span className="type-footnote tabular-nums text-muted-foreground">{sub}</span>}
    </div>
  )
}
