import { Children, isValidElement, type ReactElement, type ReactNode } from "react"

import { Select as AffectSelect, type SelectOption } from "@/components/affect/select-clean"

/* Steve's Select (base-ui's compound API: Select › SelectTrigger › SelectValue, SelectContent › SelectItem), drawn by
 * affectUI's Select – the field in Text Field's look and its list in the top layer with a gliding highlight and
 * type-ahead. The parts are read from the children: the trigger's className, aria-label and data-* go on affectUI's
 * trigger, SelectValue's placeholder is its placeholder, each SelectItem an option. An item whose children are more
 * than text (a bot's avatar and name) shows them as they are; its text, or its value, names it for type-ahead.
 * SelectContent's align (start or end; center reads as start) and a min-w-* in its className place and size the list. */

type Value = string | null

type TriggerProps = { className?: string; size?: "sm" | "default"; children?: ReactNode; "aria-label"?: string; [data: `data-${string}`]: string | undefined }
type ItemProps = { value: string; children?: ReactNode; disabled?: boolean }

/** Only read by Select – these render nothing on their own */
function SelectTrigger(_: TriggerProps): ReactNode { return null }
function SelectValue(_: { placeholder?: string; className?: string; children?: ReactNode }): ReactNode { return null }
function SelectContent(_: { children?: ReactNode; className?: string; align?: "start" | "center" | "end"; side?: string; sideOffset?: number; alignItemWithTrigger?: boolean }): ReactNode { return null }
function SelectItem(_: ItemProps): ReactNode { return null }
function SelectGroup(_: { children?: ReactNode; className?: string }): ReactNode { return null }
function SelectLabel(_: { children?: ReactNode; className?: string }): ReactNode { return null }
function SelectSeparator(_: { className?: string }): ReactNode { return null }

/** The plain text in a node, for type-ahead and the tooltip */
function textOf(n: ReactNode): string {
  if (n == null || typeof n === "boolean") return ""
  if (typeof n === "string" || typeof n === "number") return String(n)
  if (Array.isArray(n)) return n.map(textOf).join("")
  if (isValidElement<{ children?: ReactNode }>(n)) return textOf(n.props.children)
  return ""
}

type Found = { trigger?: TriggerProps; placeholder?: string; items: SelectOption[]; align?: "start" | "end"; minWidth?: number }

/** min-w-44 → 176, min-w-[200px] → 200 */
function minWidthOf(className?: string): number | undefined {
  const m = className?.match(/(?:^|\s)min-w-(?:(\d+(?:\.\d+)?)|\[(\d+)px\])(?:\s|$)/)
  return m ? (m[1] ? Number(m[1]) * 4 : Number(m[2])) : undefined
}

function walk(node: ReactNode, found: Found) {
  Children.forEach(node, (c) => {
    if (!isValidElement(c)) return
    const el = c as ReactElement<{ children?: ReactNode }>
    if (el.type === SelectTrigger) { found.trigger = el.props as TriggerProps; walk(el.props.children, found) }
    else if (el.type === SelectContent) {
      const p = el.props as { align?: string; className?: string; children?: ReactNode }
      if (p.align === "end") found.align = "end"
      found.minWidth = minWidthOf(p.className)
      walk(p.children, found)
    }
    else if (el.type === SelectValue) found.placeholder = (el.props as { placeholder?: string }).placeholder
    else if (el.type === SelectItem) {
      const { value, children, disabled } = el.props as ItemProps
      const text = textOf(children)
      const rich = typeof children !== "string" && typeof children !== "number"
      found.items.push({ value: String(value), label: text || String(value), content: rich ? children : undefined, disabled })
    } else walk(el.props.children, found)
  })
}

type SelectProps = {
  value?: Value
  defaultValue?: Value
  onValueChange?: (value: string | null) => void
  disabled?: boolean
  name?: string
  required?: boolean
  /** base-ui's value → label map; the items carry their labels here */
  items?: unknown
  children?: ReactNode
}

function Select({ value, defaultValue, onValueChange, disabled, name, required, children }: SelectProps) {
  const found: Found = { items: [] }
  walk(children, found)
  const { className, size, children: _c, "aria-label": ariaLabel, ...data } = found.trigger ?? {}
  return (
    <AffectSelect
      {...data}
      data-slot="select-trigger"
      options={found.items}
      value={value === undefined ? undefined : value}
      defaultValue={defaultValue ?? null}
      onChange={(v) => onValueChange?.(v)}
      placeholder={found.placeholder ?? ""}
      size={size === "default" ? "md" : "sm"}
      disabled={disabled}
      name={name}
      required={required}
      aria-label={ariaLabel}
      className="steve-select min-w-0 max-w-full"
      triggerClassName={className}
      align={found.align}
      listMinWidth={found.minWidth}
    />
  )
}

export { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectSeparator, SelectTrigger, SelectValue }
