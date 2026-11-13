import type { ComponentProps } from "react"
import { Badge as AffectBadge, type BadgeProps as AffectBadgeProps, type BadgeTone } from "@/components/affect/badge-clean"
import { cn } from "cn"

/* Steve's Badge, drawn by affectUI's Badge (components/affect/badge-clean.tsx), behind the export and variants of the
 * shadcn badge it replaces, so every call site keeps working. secondary and destructive are affectUI's neutral and
 * danger tones; default, outline, ghost and link have no tone there and keep Steve's look on top of the neutral one.
 * A call site's className (a tint like bg-attention/12 text-attention) still wins: affectUI's styles sit in the
 * components layer. affectUI's own props (tone, dot, live, icon, count) pass through. */
type Variant = "default" | "secondary" | "destructive" | "outline" | "ghost" | "link"

const TONE: Record<Variant, BadgeTone> = { default: "neutral", secondary: "neutral", destructive: "danger", outline: "neutral", ghost: "neutral", link: "neutral" }
const LOOK: Partial<Record<Variant, string>> = {
  default: "bg-primary text-primary-foreground",
  outline: "bg-transparent text-foreground shadow-[inset_0_0_0_1px_var(--border)]",
  ghost: "bg-transparent hover:bg-muted",
  link: "bg-transparent text-primary underline-offset-4 hover:underline",
}

type BadgeProps = ComponentProps<"span"> & { variant?: Variant | null } & Pick<AffectBadgeProps, "tone" | "size" | "dot" | "live" | "icon" | "count" | "max">

function Badge({ className, variant, tone, size = "md", ...props }: BadgeProps) {
  const v = variant ?? "default"
  return <AffectBadge data-slot="badge" data-variant={v} tone={tone ?? TONE[v]} size={size} className={cn(!tone && LOOK[v], className)} {...props} />
}

export { Badge }
