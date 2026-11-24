// Every web/test/*.browser.mjs file must be owned by either SMOKE or EXTENDED in browser-smoke.mjs.
// No unmaintained manual-only third tier is allowed.
import assert from "node:assert/strict"
import { readdirSync, readFileSync } from "node:fs"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { CI_SMOKE_THRESHOLD_S, EXTENDED, EXTENDED_SHARDS, parseShard, selectShard, SMOKE } from "./browser-smoke.mjs"

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

test("EXTENDED sharding covers every extended test exactly once and matches CI workflow", () => {
  assert.ok(Number.isInteger(CI_SMOKE_THRESHOLD_S) && CI_SMOKE_THRESHOLD_S > 0)
  assert.ok(Number.isInteger(EXTENDED_SHARDS) && EXTENDED_SHARDS >= 1)

  const shards = Array.from({ length: EXTENDED_SHARDS }, (_, i) => selectShard(EXTENDED, i + 1, EXTENDED_SHARDS))
  const counts = shards.map((s) => s.length)
  assert.ok(Math.max(...counts) - Math.min(...counts) <= 1, `shards must stay balanced by test count: ${counts}`)
  assert.deepEqual(shards.flat().sort(), [...EXTENDED].sort(), "shards must cover EXTENDED exactly once")

  assert.deepEqual(parseShard("1/2"), { index: 1, total: 2 })
  for (const bad of ["0/2", "3/2", "1/0", "abc", "1/2/3"]) {
    assert.throws(() => parseShard(bad), `invalid shard spec "${bad}" must throw`)
  }

  const ci = readFileSync(fileURLToPath(new URL("../../.github/workflows/ci.yml", import.meta.url)), "utf8")
  const expectedShards = Array.from({ length: EXTENDED_SHARDS }, (_, i) => i + 1).join(", ")
  assert.match(ci, new RegExp(`shard:\\s*\\[${expectedShards}\\]`), "ci.yml browser-extended matrix must list 1..EXTENDED_SHARDS")
  assert.match(ci, new RegExp(`shards:\\s*\\[${EXTENDED_SHARDS}\\]`), "ci.yml browser-extended matrix shards count must match EXTENDED_SHARDS")
})
