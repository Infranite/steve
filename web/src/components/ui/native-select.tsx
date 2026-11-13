import type { ComponentProps } from "react"
import { ChevronDownIcon } from "lucide-react"
import { cn } from "cn"

/** a native select (keyboard, mobile pickers and form value as the platform gives them) dressed as the system's select trigger */
export function NativeSelect({ className, children, ...props }: ComponentProps<"select">) {
  return <span className={cn("relative inline-flex", className)}>
    <select {...props} className="h-8 w-full min-w-0 cursor-pointer appearance-none rounded-lg border border-transparent bg-muted pr-8 pl-3 type-callout text-foreground outline-none transition-[background-color,box-shadow] duration-120 ease-(--ease-out) hover:bg-accent focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-50">{children}</select>
    <ChevronDownIcon aria-hidden className="pointer-events-none absolute top-1/2 right-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
  </span>
}
