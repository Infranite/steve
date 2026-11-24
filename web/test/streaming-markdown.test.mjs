import assert from "node:assert/strict"
import { test } from "node:test"
import { parseMarkdownIntoBlocks } from "streamdown"
import { streamingBlockParser } from "../src/streaming-markdown.ts"

test("incremental blocks match the full parser for growing and repaired Markdown", () => {
  const parse = streamingBlockParser(parseMarkdownIntoBlocks)
  const text = "## Heading\n\nOne paragraph.\n\n> quote\n> continues\n\n- One\n\n- Two\n\n" +
    "```ts\nx\n\ny\n```\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n" +
    "A **partial fragment** and [link](https://example.invalid) with [reference].\n\n" +
    "[reference]: https://example.invalid/ref\n\nA footnote[^a].\n\n[^a]: Something else."
  for (let i = 0; i <= text.length; i++) {
    const input = text.slice(0, i)
    // Interleave another message: Streamdown's built-in single cache can be evicted.
    parseMarkdownIntoBlocks("Another message.\n\nWith several blocks.\n\nDone.")
    assert.deepEqual(parse(input), parseMarkdownIntoBlocks(input), input)
  }
  for (const input of ["fresh input", "# CrLf\r\n\r\nText", "one\n\ntwo\n\nthree\n\n**fou**", "one\n\ntwo\n\nthree\n\n**four**"]) {
    assert.deepEqual(parse(input), parseMarkdownIntoBlocks(input))
  }
})

test("only lexes a short tail after a block boundary, including a rewritten suffix", () => {
  const inputs = [], parse = streamingBlockParser(input => { inputs.push(input); return parseMarkdownIntoBlocks(input) })
  const prefix = "An earlier completed paragraph.\n\n".repeat(30)
  parse(prefix + "**provisional**")
  const next = prefix + "**provisional continuation**"
  assert.deepEqual(parse(next), parseMarkdownIntoBlocks(next))
  assert.ok(inputs.at(-1).length < 120)
})
