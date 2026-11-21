import assert from "node:assert/strict"
import { test } from "node:test"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { checkCounts, readCounts } from "./support/guard-ratchet.mjs"

const fixture = (t) => {
  const scratch = fileURLToPath(new URL("../../.scratch/", import.meta.url))
  mkdirSync(scratch, { recursive: true })
  const dir = mkdtempSync(join(scratch, "rg2-ratchet-"))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  const file = join(dir, "inventory.txt")
  return { file, columns: 1, header: "# fixture\n", bless: false, init: false }
}
const counts = (n) => new Map([["fixture.ts", n]])

test("unchanged passes, and shrink or deletion requires BLESS", (t) => {
  const options = fixture(t)
  writeFileSync(options.file, "# fixture\nfixture.ts\t2\n")
  checkCounts(counts(2), options)
  for (const now of [counts(1), new Map()]) {
    assert.throws(() => checkCounts(now, options), /shorten the list/)
    assert.equal(readCounts(options.file, 1).get("fixture.ts"), 2)
  }
  checkCounts(counts(1), { ...options, bless: true })
  assert.deepEqual(readCounts(options.file, 1), counts(1))
  checkCounts(new Map(), { ...options, bless: true })
  assert.equal(readFileSync(options.file, "utf8"), "# fixture\n")
})

test("new keys and growth fail even with BLESS and preserve the file", (t) => {
  const options = fixture(t), text = "fixture.ts\t1\n"
  writeFileSync(options.file, text)
  for (const now of [counts(2), new Map([["new.ts", 1]])]) for (const bless of [false, true]) {
    assert.throws(() => checkCounts(now, { ...options, bless }), /inventory can only shrink/)
    assert.equal(readFileSync(options.file, "utf8"), text)
  }
})

test("missing inventories require INIT and INIT cannot overwrite or combine with BLESS", (t) => {
  const options = fixture(t)
  for (const bless of [false, true]) {
    assert.throws(() => checkCounts(counts(1), { ...options, bless }), /missing inventory/)
    assert.equal(existsSync(options.file), false)
  }
  assert.throws(() => checkCounts(counts(1), { ...options, bless: true, init: true }), /not both/)
  checkCounts(counts(1), { ...options, init: true })
  const text = readFileSync(options.file, "utf8")
  assert.throws(() => checkCounts(counts(2), { ...options, init: true }), /INIT cannot overwrite/)
  assert.equal(readFileSync(options.file, "utf8"), text)
  rmSync(options.file)
  assert.throws(() => checkCounts(counts(99), { ...options, bless: true }), /missing inventory/)
  assert.equal(existsSync(options.file), false)
})

test("malformed and duplicate rows cannot be blessed away", (t) => {
  const options = fixture(t)
  for (const text of ["f\t0\n", "f\t-1\n", "f\t1e3\n", "f\t1.5\n", "f\tx\n", "f\n", "f\tx\t1\n", "\t1\n", "f\t1\nf\t2\n"]) {
    writeFileSync(options.file, text)
    assert.throws(() => checkCounts(counts(1), { ...options, bless: true }), undefined, text)
    assert.equal(readFileSync(options.file, "utf8"), text)
  }
})

test("switches require the exact value 1", (t) => {
  const options = fixture(t), before = { ...process.env }
  t.after(() => {
    for (const key of ["STEVE_GUARD_BLESS", "STEVE_GUARD_INIT"]) {
      if (before[key] === undefined) delete process.env[key]
      else process.env[key] = before[key]
    }
  })
  const { bless, init, ...fromEnv } = options
  process.env.STEVE_GUARD_INIT = "0"
  process.env.STEVE_GUARD_BLESS = "0"
  assert.throws(() => checkCounts(counts(2), fromEnv), /missing inventory/)
  process.env.STEVE_GUARD_INIT = "1"
  checkCounts(counts(2), fromEnv)
  delete process.env.STEVE_GUARD_INIT
  assert.throws(() => checkCounts(counts(1), fromEnv), /shorten the list/)
  process.env.STEVE_GUARD_BLESS = "1"
  checkCounts(counts(1), fromEnv)
  assert.deepEqual(readCounts(options.file, 1), counts(1))
})
