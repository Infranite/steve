import type { ComponentPropsWithRef } from "react"
import { cn } from "cn"

import { TextArea } from "@/components/affect/text-area-clean"

/* Steve's Textarea, drawn by affectUI's Text Area: Text Field's box over several lines, growing with the text (rows is
 * where it starts, then up to maxRows before it scrolls). Props stay a <textarea>'s and land on it; className is
 * split as for Input – the text's classes and min-h/max-h go on the textarea, the rest (width, margins) on the field.
 * affectUI's extras are there too: maxRows, expandable, onSubmitShortcut (⌘/Ctrl+Enter). */

type TextareaProps = Omit<ComponentPropsWithRef<"textarea">, "rows"> & {
  rows?: number
  maxRows?: number
  expandable?: boolean
  onSubmitShortcut?: (value: string) => void
}

const AREA = /^(?:[a-z0-9-]+:)*!?(?:type-|font-|text-|tabular-nums|uppercase|tracking-|leading-|placeholder:|min-h-|max-h-|\[-webkit-text-security)/

function Textarea({ className, rows = 2, maxRows = 16, ...props }: TextareaProps) {
  const parts = (className ?? "").split(/\s+/).filter(Boolean)
  const area = parts.filter((c) => AREA.test(c))
  return (
    <TextArea
      data-slot="textarea"
      {...props}
      minRows={rows}
      maxRows={Math.max(maxRows, rows)}
      className={cn("w-full", parts.filter((c) => !area.includes(c)).join(" "))}
      inputClassName={area.join(" ") || undefined}
    />
  )
}

export { Textarea }
