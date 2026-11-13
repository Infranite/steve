"use client"

import { Collapsible as CollapsiblePrimitive } from "@base-ui/react/collapsible"
import { cn } from "cn"

function Collapsible({ ...props }: CollapsiblePrimitive.Root.Props) {
  return <CollapsiblePrimitive.Root data-slot="collapsible" {...props} />
}

function CollapsibleTrigger({ ...props }: CollapsiblePrimitive.Trigger.Props) {
  return (
    <CollapsiblePrimitive.Trigger data-slot="collapsible-trigger" {...props} />
  )
}

/** the panel eases its height open and shut (Base UI measures it into --collapsible-panel-height): 260ms, the
 *  system's expand duration */
export const COLLAPSE = "h-(--collapsible-panel-height) overflow-hidden transition-[height] duration-260 ease-(--ease-out) data-ending-style:h-0 data-starting-style:h-0"

function CollapsibleContent({ className, ...props }: CollapsiblePrimitive.Panel.Props) {
  return (
    <CollapsiblePrimitive.Panel data-slot="collapsible-content" className={typeof className === "function" ? className : cn(COLLAPSE, className)} {...props} />
  )
}

export { Collapsible, CollapsibleTrigger, CollapsibleContent }
