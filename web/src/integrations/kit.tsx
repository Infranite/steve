/* Pieces the connect pages (integrations, channels, mail, GitHub, contacts, knowledge, signals) share so one idea
 * looks the same on each of them: the "nothing connected yet" prompt and the in-place disclosure. */
import type { ReactNode } from "react"
import { ChevronRightIcon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { cn } from "cn"

/** "nothing connected yet": a lifted card, a quiet icon tile, the title, one line of why, then the one thing to do */
export function ConnectPrompt({ icon, title, sub, action, children, className, ...rest }: { icon?: ReactNode; title: ReactNode; sub?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string; [k: `data-${string}`]: unknown }) {
  return (
    <section className={cn("flex items-start gap-3 rounded-xl bg-card p-5 shadow-[var(--lift)]", className)} {...rest}>
      {icon && <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4">{icon}</div>}
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-col gap-1">
          <div className="type-headline text-foreground">{title}</div>
          {sub && <div className="type-callout text-muted-foreground">{sub}</div>}
        </div>
        {children}
        {action && <div className="flex flex-wrap items-center gap-2">{action}</div>}
      </div>
    </section>
  )
}

/** the height transition every in-place disclosure on these pages uses */
/** a chevron that turns when its trigger (aria-expanded) or details (open) opens */
export const Chevron = ({ className }: { className?: string }) => (
  <ChevronRightIcon aria-hidden className={cn("size-3.5 shrink-0 transition-transform duration-180 ease-(--ease-out) group-aria-expanded/disc:rotate-90 group-open/disc:rotate-90", className)} />
)
/** a disclosure trigger reads as one, not as a link: muted text with a turning chevron */
export const DISC_TRIGGER = "group/disc inline-flex items-center gap-1 rounded-md type-callout text-muted-foreground outline-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
/** a native <summary> styled like the disclosure trigger, browser marker hidden; put `group/disc` on its <details> */
export const SUMMARY = "inline-flex cursor-pointer list-none items-center gap-1 rounded-md text-muted-foreground outline-none select-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden"

/** content that opens in place, its height easing both ways */
export function Disclosure({ label, children, open, onOpenChange, defaultOpen, className, triggerClassName, ...rest }: { label: ReactNode; children: ReactNode; open?: boolean; onOpenChange?: (o: boolean) => void; defaultOpen?: boolean; className?: string; triggerClassName?: string; [k: `data-${string}`]: unknown }) {
  return (
    <Collapsible open={open} onOpenChange={onOpenChange ? (o) => onOpenChange(o) : undefined} defaultOpen={defaultOpen} className={className} {...rest}>
      <CollapsibleTrigger className={cn(DISC_TRIGGER, triggerClassName)}><Chevron />{label}</CollapsibleTrigger>
      <CollapsibleContent>{children}</CollapsibleContent>
    </Collapsible>
  )
}
