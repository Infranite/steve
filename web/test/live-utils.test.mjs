import assert from "node:assert/strict"
import { test } from "node:test"
import { debounceMax, bounded, matchesSpace } from "../src/live-utils.ts"

test("steady events refresh at maxWait; quiet events trail and disposal cancels", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"] })
  let calls = 0
  const d = debounceMax(() => calls++, 250)
  for (let n = 0; n < 10; n++) { d.schedule(); t.mock.timers.tick(100) }
  assert.equal(calls, 1)
  d.schedule(); t.mock.timers.tick(249); assert.equal(calls, 1)
  t.mock.timers.tick(1); assert.equal(calls, 2)
  d.schedule(); d.cancel(); t.mock.timers.tick(1000); assert.equal(calls, 2)
})

test("resync workers are bounded and drain every resource", async () => {
  let active = 0, peak = 0
  const seen = []
  await bounded(Array.from({ length: 13 }, (_, n) => n), async (n) => {
    peak = Math.max(peak, ++active)
    await new Promise((resolve) => setImmediate(resolve))
    seen.push(n); active--
  })
  assert.equal(peak, 3)
  assert.deepEqual(seen.sort((a, b) => a - b), Array.from({ length: 13 }, (_, n) => n))
})

test("only explicit foreign space ids are filtered", () => {
  assert.equal(matchesSpace({ space_id: "other" }, "here"), false)
  assert.equal(matchesSpace({ space_id: "here" }, "here"), true)
  assert.equal(matchesSpace({}, "here"), true)
})
