import { Children, isValidElement, type ReactNode } from "react"

import { ToggleGroup as AffectToggleGroup, type ToggleGroupItem as AffectItem } from "@/components/affect/toggle-group-clean"

/* Steve's ToggleGroup (base-ui's API: a value array, onValueChange, ToggleGroupItem children), drawn by affectUI's
 * Toggle Group. The items are read from the children; an item's data-* attributes, aria-label, title and disabled
 * go onto its button. affectUI styles the items itself, so an item's className is not used. multiple (or base-ui's
 * toggleMultiple) lets several be on; otherwise one at most, as base-ui's default. Both variants are affectUI's separate
 * chips (its "buttons"), the look Steve's filters had. */

type ItemProps = {
  value: string
  children?: ReactNode
  disabled?: boolean
  title?: string
  className?: string
  "aria-label"?: string
  [data: `data-${string}`]: string | undefined
}

/** Only read by ToggleGroup – renders nothing on its own */
function ToggleGroupItem(_: ItemProps): ReactNode {
  return null
}

type GroupProps = {
  value?: readonly string[]
  defaultValue?: readonly string[]
  onValueChange?: (value: string[]) => void
  multiple?: boolean
  toggleMultiple?: boolean
  disabled?: boolean
  size?: "sm" | "default" | "lg"
  variant?: "default" | "outline"
  spacing?: number
  orientation?: "horizontal" | "vertical"
  className?: string
  children?: ReactNode
  "aria-label"?: string
  "aria-labelledby"?: string
  [data: `data-${string}`]: string | boolean | undefined
}

function ToggleGroup({ value, defaultValue, onValueChange, multiple, toggleMultiple, disabled, size, variant, className, children, "aria-label": ariaLabel, "aria-labelledby": ariaLabelledby, spacing: _spacing, orientation: _orientation, ...rest }: GroupProps) {
  const items: AffectItem[] = []
  Children.forEach(children, (c) => {
    if (!isValidElement<ItemProps>(c) || c.type !== ToggleGroupItem) return
    const { value: v, children: label, disabled: off, title, "aria-label": name, className: _c, ...data } = c.props
    items.push({ value: String(v), label, disabled: off, title, ariaLabel: name, data: data as AffectItem["data"] })
  })
  const data = Object.fromEntries(Object.entries(rest).filter(([k]) => k.startsWith("data-")).map(([k, v]) => [k, v === true ? "" : v || undefined]))
  const shared = {
    items,
    // Steve's sm (28) and default (32) are affectUI's md and lg
    size: size === "sm" ? "md" as const : "lg" as const,
    variant: "buttons" as const,
    disabled,
    className,
    "aria-label": ariaLabel,
    "aria-labelledby": ariaLabelledby,
    data: { "data-slot": "toggle-group", ...data },
  }
  if (multiple || toggleMultiple)
    return <AffectToggleGroup {...shared} type="multiple" value={value ? [...value] : undefined} defaultValue={defaultValue ? [...defaultValue] : undefined} onChange={(v) => onValueChange?.(v)} />
  return <AffectToggleGroup {...shared} type="single" value={value === undefined ? undefined : value[0] ?? null} defaultValue={defaultValue?.[0] ?? null} onChange={(v) => onValueChange?.(v == null ? [] : [v])} />
}

export { ToggleGroup, ToggleGroupItem }
