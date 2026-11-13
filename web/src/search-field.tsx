/* one search field for the conversation pages (sessions, 历史, 群聊和私聊): a quiet wash rather than a frame, the icon inside,
 * a soft ring on focus without a border jump. Kept here until the base components have one. */
import type { InputHTMLAttributes, ReactNode, Ref } from "react"
import { SearchIcon } from "lucide-react"
import { cn } from "cn"

export function SearchField({ className, trailing, inputRef, ...input }: InputHTMLAttributes<HTMLInputElement> & { trailing?: ReactNode; inputRef?: Ref<HTMLInputElement> }) {
  return (
    <label className={cn("flex h-9 min-w-0 items-center gap-2 rounded-lg bg-foreground/5 px-3 transition-[background-color,box-shadow] duration-[180ms] ease-[var(--ease-out)] focus-within:bg-transparent focus-within:ring-1 focus-within:ring-ring/45 focus-within:shadow-[0_0_0_4px_color-mix(in_srgb,var(--ring)_12%,transparent)]", className)}>
      <SearchIcon className="size-4 shrink-0 text-tertiary" aria-hidden />
      <input ref={inputRef} {...input} className="min-w-0 flex-1 bg-transparent type-body outline-none placeholder:text-tertiary" />
      {trailing}
    </label>
  )
}
