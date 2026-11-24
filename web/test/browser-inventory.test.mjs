// Every web/test/*.browser.mjs file must be owned by either SMOKE or EXTENDED in browser-smoke.mjs.
// No unmaintained manual-only third tier is allowed.
import assert from "node:assert/strict"
import { readdirSync } from "node:fs"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { EXTENDED, SMOKE } from "./browser-smoke.mjs"

test("every browser test is listed exactly once in SMOKE or EXTENDED", () => {
  const dir = fileURLToPath(new URL(".", import.meta.url))
  const files = readdirSync(dir)
    .filter((name) => name.endsWith(".browser.mjs"))
    .map((name) => name.slice(0, -".browser.mjs".length))
    .sort()

  const smokeSet = new Set(SMOKE)
  const extendedSet = new Set(EXTENDED)
  assert.equal(smokeSet.size, SMOKE.length, "SMOKE must not contain duplicates")
  assert.equal(extendedSet.size, EXTENDED.length, "EXTENDED must not contain duplicates")

  const overlap = SMOKE.filter((name) => extendedSet.has(name))
  assert.deepEqual(overlap, [], "a browser test cannot belong to both SMOKE and EXTENDED")

  const listed = [...SMOKE, ...EXTENDED].sort()
  const unlisted = files.filter((name) => !smokeSet.has(name) && !extendedSet.has(name))
  const missing = listed.filter((name) => !files.includes(name))
  assert.deepEqual(unlisted, [], "unlisted browser tests are forbidden (add to SMOKE or EXTENDED, or delete)")
  assert.deepEqual(missing, [], "browser-smoke.mjs references missing *.browser.mjs files")
})
