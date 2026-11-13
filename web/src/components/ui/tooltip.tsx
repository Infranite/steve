import * as React from "react"
import { mergeProps } from "@base-ui/react/merge-props"
import { useRender } from "@base-ui/react/use-render"
import { Tooltip as TooltipPrimitive } from "@/components/affect/tooltip-clean"
import { cn } from "cn"

/* Tooltip: affectUI's Tooltip (a small label in the top layer, shown after a short rest, at once under keyboard focus and
 * for the neighbours of one that is open) behind the Base UI-shaped parts Steve's call sites use:
 *   <Tooltip><TooltipTrigger render={<Button … />}>…</TooltipTrigger><TooltipContent side="right">…</TooltipContent></Tooltip>
 * The Tooltip picks its TooltipContent out of its children (directly or in a fragment, not inside another component)
 * and hands it to the trigger, which wraps itself in the affectUI Tooltip. */

type Side = "top" | "bottom" | "left" | "right" | "inline-start" | "inline-end"
type Align = "start" | "center" | "end"

const sideOf = (s: Side) => (s === "inline-start" ? "left" : s === "inline-end" ? "right" : s)

type ContentProps = React.ComponentProps<"span"> & { side?: Side; sideOffset?: number; align?: Align }
type RootProps = { children?: React.ReactNode; open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; disabled?: boolean }

const Delay = React.createContext(500)
const Content = React.createContext<{ content: ContentProps | null; root: RootProps }>({ content: null, root: {} })

function TooltipProvider({ delay = 500, children }: { delay?: number; children?: React.ReactNode }) {
  return <Delay.Provider value={delay}>{children}</Delay.Provider>
}

/** The TooltipContent among a Tooltip's children (fragments included) and the rest */
function split(children: React.ReactNode): [ContentProps | null, React.ReactNode[]] {
  let content: ContentProps | null = null
  const rest: React.ReactNode[] = []
  for (const c of React.Children.toArray(children)) {
    if (React.isValidElement(c) && c.type === TooltipContent) content = c.props as ContentProps
    else if (React.isValidElement<{ children?: React.ReactNode }>(c) && c.type === React.Fragment) {
      const [inner, more] = split(c.props.children)
      content ??= inner
      rest.push(...more)
    } else rest.push(c)
  }
  return [content, rest]
}

function Tooltip(root: RootProps) {
  const [content, rest] = split(root.children)
  // the content must be a child of the Tooltip itself (or of a fragment in it): one inside another component is not
  // seen, and the trigger would show no tooltip
  if (import.meta.env.DEV && !content && root.children != null && !root.disabled)
    console.warn("Tooltip: no TooltipContent among its direct children – the trigger shows no tooltip")
  return <Content.Provider value={{ content, root }}>{rest}</Content.Provider>
}

function TooltipTrigger({ render, ref, delay: own, ...props }: useRender.ComponentProps<"button"> & { delay?: number }) {
  const { content, root } = React.useContext(Content)
  const delay = React.useContext(Delay)
  const trigger = useRender({ defaultTagName: "button", render, ref, props: mergeProps<"button">({ "data-slot": "tooltip-trigger" } as React.ComponentProps<"button">, props) })
  if (!content) return trigger
  const { side = "top", sideOffset = 6, align = "center", className, hidden, children } = content
  return (
    <TooltipPrimitive content={children} side={sideOf(side)} align={align} offset={sideOffset} delay={own ?? delay}
      disabled={root.disabled || !!hidden} open={root.open} onOpenChange={root.onOpenChange} className={cn("steve-tip", className)}>
      {trigger}
    </TooltipPrimitive>
  )
}

/** Its props are read by the Tooltip around it; it draws nothing itself */
function TooltipContent(_props: ContentProps): React.ReactNode {
  return null
}

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider }
