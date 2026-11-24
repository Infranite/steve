import { code } from "@streamdown/code"
import type { ColorReply, ColorRequest } from "./markdown-highlight"

// Shiki's first grammar compilation/tokenization can monopolize a main-thread
// microtask. Run the same plugin here; its cache stays with the highlighter.
self.onmessage = ({ data: { id, options } }: MessageEvent<ColorRequest>) => {
  const reply = (result: ColorReply["result"]) => {
    // The grammar state is a class with methods used only inside Shiki.
    self.postMessage({ id, result: { ...result, grammarState: undefined } } satisfies ColorReply)
  }
  const hit = code.highlight(options, reply)
  if (hit) reply(hit)
}
