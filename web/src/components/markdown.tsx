import { lazy, Suspense } from "react"
import { cn } from "cn"

/* the markdown stack is ~200 KB gzipped — load it after the first paint and show plain text until then */
const Impl = lazy(() => import("@/components/markdown-impl"))

/** Markdown for messages, drafts and item text: GFM, Shiki code, Mermaid, sandboxed HTML preview */
export function Markdown({ text, className, streaming }: { text: string; className?: string; streaming?: boolean }) {
  return (
    <Suspense fallback={<div className={cn("whitespace-pre-wrap", className)}>{text}</div>}>
      <Impl text={text} className={className} streaming={streaming} />
    </Suspense>
  )
}
