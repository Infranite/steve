import type { CodeHighlighterPlugin, HighlightOptions, HighlightResult } from "@streamdown/code"
import { bundledLanguagesInfo } from "shiki/langs"

export type ColorRequest = { id: number; options: HighlightOptions }
export type ColorReply = { id: number; result: HighlightResult }
const languages = bundledLanguagesInfo.map(({ id }) => id as HighlightOptions["language"])
const names = new Set(bundledLanguagesInfo.flatMap(({ id, aliases }) => [id, ...(aliases ?? [])]))
const supportsLanguage = (language: string) => names.has(language.trim().toLowerCase())
const pending = new Map<number, (result: HighlightResult) => void>()
let worker: Worker | null | undefined
let requestId = 0

function getWorker() {
  if (worker !== undefined) return worker
  try {
    const instance = new Worker(new URL("./markdown-highlight.worker.ts", import.meta.url), { type: "module" })
    const fail = () => {
      instance.terminate()
      pending.clear()
      worker = null
    }
    instance.onmessage = ({ data: { id, result } }: MessageEvent<ColorReply>) => {
      const resolve = pending.get(id)
      pending.delete(id)
      resolve?.(result)
    }
    instance.onerror = instance.onmessageerror = fail
    return worker = instance
  } catch {
    return worker = null
  }
}

export const code: CodeHighlighterPlugin = {
  name: "shiki",
  type: "code-highlighter",
  getSupportedLanguages: () => languages,
  getThemes: () => ["github-light", "github-dark"],
  supportsLanguage,
  highlight(options, callback) {
    // Raw code renders immediately. Don't enqueue a separate grammar pass
    // for each intermediate revision of an unfinished fence.
    if (options.isIncomplete || !callback || !supportsLanguage(options.language)) return null
    const target = getWorker()
    if (!target) return null
    const id = ++requestId
    pending.set(id, callback)
    try {
      target.postMessage({ id, options } satisfies ColorRequest)
    } catch {
      pending.delete(id)
    }
    return null
  },
}
