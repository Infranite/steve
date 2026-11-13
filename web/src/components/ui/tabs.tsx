import { Children, createContext, isValidElement, useContext, useId, useState, type ComponentProps, type ReactNode } from "react"
import { cn } from "cn"

import { SegmentedControl, type SegmentOption } from "@/components/affect/segmented-control-clean"

/* Steve's Tabs (base-ui's API: Tabs, TabsList, TabsTrigger, TabsContent with a value of any type), drawn by affectUI's
 * Segmented Control as a tablist: a raised pill sliding along a muted track, arrow keys between the tabs, each tab
 * tied to its panel. The triggers are read from TabsList's children; their data-* attributes go on the tab. */

type Value = string | number
type Ctx = { value: Value | undefined; set: (v: Value) => void; base: string; keys: Map<string, Value> }
const TabsContext = createContext<Ctx | null>(null)
const key = (v: Value) => String(v)

type TabsProps = Omit<ComponentProps<"div">, "defaultValue" | "onChange"> & {
  value?: Value
  defaultValue?: Value
  onValueChange?: (value: Value) => void
  orientation?: "horizontal" | "vertical"
}

function Tabs({ value, defaultValue, onValueChange, orientation = "horizontal", className, ...props }: TabsProps) {
  const [own, setOwn] = useState<Value | undefined>(defaultValue)
  const base = useId()
  const [keys] = useState(() => new Map<string, Value>())
  const current = value !== undefined ? value : own
  const set = (v: Value) => {
    if (value === undefined) setOwn(v)
    onValueChange?.(v)
  }
  return (
    <TabsContext.Provider value={{ value: current, set, base, keys }}>
      <div data-slot="tabs" data-orientation={orientation} className={cn("flex flex-col gap-2", className)} {...props} />
    </TabsContext.Provider>
  )
}

type TriggerProps = { value: Value; children?: ReactNode; disabled?: boolean; className?: string; title?: string; "aria-label"?: string; [data: `data-${string}`]: string | undefined }

/** Only read by TabsList – renders nothing on its own */
function TabsTrigger(_: TriggerProps): ReactNode {
  return null
}

function TabsList({ className, children, variant = "default", "aria-label": ariaLabel, size = "md" }: { className?: string; children?: ReactNode; variant?: "default" | "line"; "aria-label"?: string; size?: "sm" | "md" | "lg" }) {
  const ctx = useContext(TabsContext)
  if (!ctx) throw new Error("TabsList outside Tabs")
  const options: SegmentOption[] = []
  Children.forEach(children, (c) => {
    if (!isValidElement<TriggerProps>(c) || c.type !== TabsTrigger) return
    const { value, children: label, disabled, title, "aria-label": name, className: _c, ...data } = c.props
    ctx.keys.set(key(value), value)
    options.push({ value: key(value), label, disabled, title, ariaLabel: name, id: `${ctx.base}-tab-${key(value)}`, controls: `${ctx.base}-panel-${key(value)}`, data: { "data-slot": "tabs-trigger", ...data } })
  })
  return (
    <SegmentedControl options={options} value={ctx.value === undefined ? undefined : key(ctx.value)} onChange={(v) => ctx.set(ctx.keys.get(v) ?? v)} kind="tabs"
      variant={variant === "line" ? "underline" : "tray"} size={size} aria-label={ariaLabel} className={className} trackData={{ "data-slot": "tabs-list" }} />
  )
}

function TabsContent({ value, className, children, ...props }: Omit<ComponentProps<"div">, "value"> & { value: Value }) {
  const ctx = useContext(TabsContext)
  if (!ctx || ctx.value === undefined || key(ctx.value) !== key(value)) return null
  return (
    <div role="tabpanel" id={`${ctx.base}-panel-${key(value)}`} aria-labelledby={`${ctx.base}-tab-${key(value)}`} data-slot="tabs-content" className={cn("flex-1 outline-none", className)} {...props}>
      {children}
    </div>
  )
}

export { Tabs, TabsList, TabsTrigger, TabsContent }
