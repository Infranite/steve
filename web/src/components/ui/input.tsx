import type { ComponentPropsWithRef, CSSProperties, ReactNode } from "react"
import { cn } from "cn"

import { TextField as AffectTextField, type TextFieldProps } from "@/components/affect/text-field-clean"
import { useT } from "@/i18n"

/* Steve's Input, drawn by affectUI's Text Field: the box with its hairline edge, a firmer edge on focus, invalid in
 * place. The props stay an <input>'s, and everything a call site passes – id, name, value, ref, aria-*, data-* – lands
 * on the input itself. className is split: classes about the text (type-*, font-*, text-*, tabular-nums, placeholder:,
 * uppercase, …) go on the input, the rest (width, flex, margins) on the field; h-7 / h-9 pick affectUI's sm / lg and
 * rounded-full a pill. affectUI's extras are there too: prefix, suffix, clearable, copyable. */

type InputProps = Omit<ComponentPropsWithRef<"input">, "size" | "prefix"> & {
  prefix?: ReactNode
  suffix?: ReactNode
  clearable?: boolean
  copyable?: boolean
  onClear?: () => void
}

const TEXT = /^(?:[a-z0-9-]+:)*!?(?:type-|font-|text-|tabular-nums|uppercase|lowercase|capitalize|tracking-|leading-|placeholder:|caret-|selection:)/
const PLACEHOLDER = /(^|:)placeholder:/

/** A call site's className: the text's classes for the input, the rest for the field, and the size it asks for */
export function splitFieldClass(className?: string) {
  const parts = (className ?? "").split(/\s+/).filter(Boolean)
  const input = parts.filter((c) => TEXT.test(c) || PLACEHOLDER.test(c))
  const field = parts.filter((c) => !input.includes(c))
  const size: "sm" | "md" | "lg" = field.includes("h-7") || field.includes("h-6") ? "sm" : field.includes("h-9") || field.includes("h-10") ? "lg" : "md"
  const pill = field.includes("rounded-full")
  return { input: input.join(" "), field: field.filter((c) => !/^h-(6|7|8|9|10)$/.test(c) && c !== "rounded-full").join(" "), size, pill }
}

/** the clear, reveal and copy buttons' names in the page's language, for every Text Field in Steve */
export function useFieldLabels(): NonNullable<TextFieldProps["labels"]> {
  const t = useT()
  return { clear: t("common:picker.clear"), show: t("common:picker.showPassword"), hide: t("common:picker.hidePassword"), copy: t("common:picker.copy"), copied: t("common:picker.copied"), optional: t("common:picker.optional") }
}

/** affectUI's Text Field with its buttons named in the page's language: use this one, not the file in components/affect */
function TextField(props: TextFieldProps) {
  return <AffectTextField labels={useFieldLabels()} {...props} />
}

function Input({ className, style, ...props }: InputProps) {
  const cls = splitFieldClass(className)
  const labels = useFieldLabels()
  return (
    <AffectTextField
      data-slot="input"
      labels={labels}
      {...props}
      size={cls.size}
      className={cn("w-full", cls.field)}
      inputClassName={cls.input || undefined}
      style={style}
      fieldStyle={cls.pill ? ({ "--radius": "9999px" } as CSSProperties) : undefined}
    />
  )
}

export { Input, TextField }
export type { InputProps }
