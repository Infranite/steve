import * as React from "react"
import { cn } from "cn"

import { formFieldCleanCss } from "@/components/affect/form-field-clean"

/* Steve's Label in affectUI's Form Field look: its label's weight and ink (field.tsx's FieldLabel is this one) */
function Label({ className, ...props }: React.ComponentProps<"label">) {
  return (
    <>
      <style href="ui-form-field-clean" precedence="default">{formFieldCleanCss}</style>
      <label
        data-slot="label"
        className={cn(
          "ui-fldc__label flex items-center gap-2 select-none group-data-[disabled=true]:pointer-events-none group-data-[disabled=true]:opacity-50 peer-disabled:cursor-not-allowed peer-disabled:opacity-50",
          className
        )}
        {...props}
      />
    </>
  )
}

export { Label }
