import type { ComponentProps } from "react"
import { Spinner as AffectSpinner } from "@/components/affect/spinner-clean"
import { useT } from "@/i18n"
import { cn } from "cn"

/* Steve's Spinner, drawn by affectUI's Spinner (components/affect/spinner-clean.tsx): an arc turning on a faint ring
 * in the text color. Same export and props as before: 16px unless a size-* class says otherwise, and announced as a
 * status ("Loading") unless the call site names it or hides it. */
function Spinner({ className, "aria-label": label, ...props }: Omit<ComponentProps<"svg">, "children" | "width" | "height" | "viewBox">) {
  const t = useT()
  const hidden = props["aria-hidden"] === true || props["aria-hidden"] === "true"
  return <AffectSpinner data-slot="spinner" size={16} label={hidden ? undefined : label ?? t("common:a11y.loading")} className={cn("size-4", className)} {...props} />
}

export { Spinner }
