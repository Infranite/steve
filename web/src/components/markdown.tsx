import { memo } from "react"
import { lazy, Suspense } from "react"
import { cn } from "cn"

/* the markdown stack is ~200 KB gzipped — load it after the first paint and show plain text until then */
const Impl = lazy(() => import("@/components/markdown-impl"))

/** Markdown for messages, drafts and item text: GFM, Shiki code, Mermaid, sandboxed HTML preview */
/** `resolve`: how a relative image or link of this document is found (a package's own files); null drops it */
export const Markdown = memo(function Markdown({ text, className, streaming, resolve, replyFiles, settled }: { text: string; className?: string; streaming?: boolean; settled?: boolean; replyFiles?: boolean; resolve?: (ref: string) => string | null }) {
  return (
    <Suspense fallback={<div className={cn("whitespace-pre-wrap", className)}>{text}</div>}>
      <Impl text={text} className={className} streaming={streaming} resolve={resolve} replyFiles={replyFiles} settled={settled} />
    </Suspense>
  )
})
