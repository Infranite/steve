import assert from "node:assert/strict"
import { test } from "node:test"
import { copyText } from "../src/lib/clipboard.ts"

test("copyText returns ok when clipboard.writeText resolves", async () => {
  const seen = []
  const result = await copyText("hello", {
    writeText: async (text) => {
      seen.push(text)
    },
  })
  assert.deepEqual(result, { ok: true })
  assert.deepEqual(seen, ["hello"])
})

test("copyText returns error when clipboard is missing", async () => {
  const result = await copyText("hello", null)
  assert.equal(result.ok, false)
  assert.ok(result.error instanceof Error)
})

test("copyText returns rejection reason when clipboard.writeText fails", async () => {
  const boom = new DOMException("NotAllowedError")
  const result = await copyText("secret", {
    writeText: async () => {
      throw boom
    },
  })
  assert.equal(result.ok, false)
  assert.equal(result.error, boom)
})
