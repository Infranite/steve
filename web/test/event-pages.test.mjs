import assert from "node:assert/strict"
import { test } from "node:test"
import { readEventPages } from "../src/event-pages.ts"

test("byte-limited short pages continue to EOF without dropping later events", async () => {
  const seen = []
  const pages = new Map([[0, [{ seq: 2 }]], [2, [{ seq: 5 }, { seq: 8 }]], [8, []]])
  const events = await readEventPages(0, async (after) => { seen.push(after); return pages.get(after) })
  assert.deepEqual(events, [{ seq: 2 }, { seq: 5 }, { seq: 8 }])
  assert.deepEqual(seen, [0, 2, 8])
})

test("catch-up uses its starting cursor and propagates failures for retry", async () => {
  assert.deepEqual(await readEventPages(42, async (after) => after === 42 ? [{ seq: 50 }] : []), [{ seq: 50 }])
  await assert.rejects(readEventPages(42, async () => { throw new Error("offline") }), /offline/)
})

test("bad pagination fails instead of looping forever or accepting duplicates", async () => {
  for (const page of [[{ seq: 10 }], [{ seq: 11 }, { seq: 11 }], [{ seq: NaN }]]) {
    await assert.rejects(readEventPages(10, async () => page), /游标没有前进/)
  }
})
