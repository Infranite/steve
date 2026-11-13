import type { ComponentProps } from "react"
import { Card as AffectCard, CardContent as AffectCardContent, CardFooter as AffectCardFooter, type CardProps as AffectCardProps } from "@/components/affect/card-clean"
import { cn } from "cn"

/* Steve's Card, drawn by affectUI's Card (components/affect/card-clean.tsx): its surface, hairline, lift, radius and
 * the interactive / selected states, behind the composable exports of the shadcn card it replaces. The parts keep
 * Steve's spacing contract (each part pads itself by --card-spacing, the card pads only top and bottom), so the call
 * sites that pass gap-0 py-0 and pad their own header, content and footer read as before. Title, description, content
 * and footer use affectUI's part classes; the header keeps Steve's grid with an action column, which affectUI's
 * prop-based header (title=, action=) cannot take as children. */
function Card({ className, size = "default", ...props }: Omit<AffectCardProps, "as"> & { size?: "default" | "sm"; as?: AffectCardProps["as"] }) {
  return (
    <AffectCard
      data-slot="card"
      data-size={size}
      className={cn("group/card gap-(--card-spacing) overflow-hidden px-0 py-(--card-spacing) text-sm [--card-spacing:--spacing(5)] has-[>img:first-child]:pt-0 data-[size=sm]:[--card-spacing:--spacing(4)] *:[img:first-child]:rounded-t-xl *:[img:last-child]:rounded-b-xl", className)}
      {...props}
    />
  )
}

function CardHeader({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="card-header"
      className={cn("group/card-header @container/card-header grid auto-rows-min items-start gap-1.5 px-(--card-spacing) has-data-[slot=card-action]:grid-cols-[1fr_auto] has-data-[slot=card-description]:grid-rows-[auto_auto] [.border-b]:pb-(--card-spacing)", className)}
      {...props}
    />
  )
}

function CardTitle({ className, ...props }: ComponentProps<"div">) {
  return <div data-slot="card-title" className={cn("ui-crdc__title font-heading text-base font-medium", className)} {...props} />
}

function CardDescription({ className, ...props }: ComponentProps<"div">) {
  return <div data-slot="card-description" className={cn("ui-crdc__desc", className)} {...props} />
}

function CardAction({ className, ...props }: ComponentProps<"div">) {
  return <div data-slot="card-action" className={cn("col-start-2 row-span-2 row-start-1 self-start justify-self-end", className)} {...props} />
}

function CardContent({ className, ...props }: ComponentProps<"div">) {
  return <AffectCardContent data-slot="card-content" className={cn("px-(--card-spacing)", className)} {...props} />
}

function CardFooter({ className, ...props }: ComponentProps<"div">) {
  return <AffectCardFooter data-slot="card-footer" className={cn("px-(--card-spacing) [.border-t]:pt-(--card-spacing)", className)} {...props} />
}

export { Card, CardHeader, CardFooter, CardTitle, CardAction, CardDescription, CardContent }
