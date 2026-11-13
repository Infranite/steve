import type { ChangeEvent, ComponentPropsWithRef, ReactNode } from "react"

import { Checkbox as AffectCheckbox } from "@/components/affect/checkbox-clean"

/* Steve's Checkbox, drawn by affectUI's: a real checkbox whose check draws itself. Props stay base-ui's – checked,
 * defaultChecked, onCheckedChange(checked), indeterminate – plus affectUI's own label and description for a box with
 * its text. className goes on the wrapper; id, name, aria-* and data-* on the input, where a test or a <label> finds
 * it. aria-invalid marks it invalid. */

type CheckboxProps = Omit<ComponentPropsWithRef<"input">, "type" | "size" | "onChange" | "checked" | "defaultChecked"> & {
  checked?: boolean
  defaultChecked?: boolean
  onCheckedChange?: (checked: boolean, event: ChangeEvent<HTMLInputElement>) => void
  indeterminate?: boolean
  label?: ReactNode
  description?: ReactNode
}

function Checkbox({ onCheckedChange, "aria-invalid": invalid, ...props }: CheckboxProps) {
  return (
    <AffectCheckbox
      data-slot="checkbox"
      {...props}
      error={invalid === true || invalid === "true" ? true : undefined}
      onChange={(e) => onCheckedChange?.(e.target.checked, e)}
    />
  )
}

export { Checkbox }
