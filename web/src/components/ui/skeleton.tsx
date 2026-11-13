import type { ComponentProps } from "react"
import { Skeleton as AffectSkeleton } from "@/components/affect/skeleton-clean"
import { cn } from "cn"

/* Steve's Skeleton, drawn by affectUI's Skeleton (components/affect/skeleton-clean.tsx): a muted shape with the soft
 * sheen that sweeps every skeleton in step. Same export and props: the call site sizes it with classes (h-7 w-40), so
 * affectUI's inline width and height are left empty. */
function Skeleton({ className, ...props }: Omit<ComponentProps<"span">, "children">) {
  return <AffectSkeleton data-slot="skeleton" width="" height="" className={cn("rounded-lg", className)} {...props} />
}

export { Skeleton }
