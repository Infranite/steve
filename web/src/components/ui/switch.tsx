import type { ChangeEvent, ComponentPropsWithRef, ReactNode } from "react"

import { Switch as AffectSwitch } from "@/components/affect/switch-clean"

/* Steve's Switch, drawn by affectUI's: a real checkbox with role="switch" whose thumb stretches as it is pressed – the
 * same switch the settings pages use. Props stay base-ui's – checked, defaultChecked, onCheckedChange(checked),
 * size "sm" | "default" – plus affectUI's own label, description and placement for a switch with its text. className
 * goes on the wrapper; id, name, aria-* and data-* on the input. */

type SwitchProps = Omit<ComponentPropsWithRef<"input">, "type" | "size" | "role" | "onChange" | "checked" | "defaultChecked"> & {
  checked?: boolean
  defaultChecked?: boolean
  onCheckedChange?: (checked: boolean, event: ChangeEvent<HTMLInputElement>) => void
  size?: "sm" | "default"
  label?: ReactNode
  description?: ReactNode
  placement?: "start" | "end"
}

function Switch({ size = "default", onCheckedChange, ...props }: SwitchProps) {
  return (
    <AffectSwitch
      data-slot="switch"
      {...props}
      size={size === "sm" ? "sm" : "md"}
      onChange={(e) => onCheckedChange?.(e.target.checked, e)}
    />
  )
}

export { Switch }
