// Shared count inventories: additions fail, and removals need an explicit shrink of an existing list.
import assert from "node:assert/strict"
import { closeSync, existsSync, ftruncateSync, openSync, readFileSync, writeFileSync } from "node:fs"

const valid = (counts, columns) => {
  for (const [key, count] of counts) {
    const fields = key.split("\t")
    assert.ok(fields.length === columns && fields.every((field) => field && !/[\n\r]/.test(field)) && !key.startsWith("#"), `bad inventory key: ${key}`)
    assert.ok(Number.isSafeInteger(count) && count > 0, `bad count: ${key}: ${count}`)
  }
}

export const readCounts = (file, columns) => {
  const counts = new Map()
  for (const line of readFileSync(file, "utf8").split(/\r?\n/).filter((line) => line && !line.startsWith("#"))) {
    const fields = line.split("\t")
    assert.equal(fields.length, columns + 1, `bad inventory row: ${line}`)
    const number = fields.pop(), key = fields.join("\t")
    assert.match(number, /^\d+$/, `bad count: ${line}`)
    assert.ok(!counts.has(key), `duplicate inventory key: ${key}`)
    counts.set(key, Number(number))
  }
  valid(counts, columns)
  return counts
}

export const differences = (now, listed) => [...new Set([...now.keys(), ...listed.keys()])].sort().flatMap((key) => {
  const actual = now.get(key) ?? 0, old = listed.get(key) ?? 0
  return actual === old ? [] : [`${key}: found ${actual}, listed ${old}; ${actual < old ? "shorten the list" : "inventory can only shrink"}`]
})

export const checkCounts = (now, {
  file, columns, header, help = "remove inventory debt",
  bless = process.env.STEVE_GUARD_BLESS === "1", init = process.env.STEVE_GUARD_INIT === "1",
}) => {
  valid(now, columns)
  assert.ok(!(bless && init), "choose STEVE_GUARD_BLESS or STEVE_GUARD_INIT, not both")
  if (init) assert.ok(!existsSync(file), `inventory already exists: ${file}; INIT cannot overwrite it`)
  else assert.ok(existsSync(file), `missing inventory ${file}; initialize explicitly with STEVE_GUARD_INIT=1`)
  const listed = init ? new Map() : readCounts(file, columns)
  if (!init) {
    const problems = differences(now, listed)
    const growth = [...now].filter(([key, count]) => count > (listed.get(key) ?? 0))
    assert.ok(bless ? !growth.length : !problems.length, `${file}: ${help}\n${problems.join("\n")}`)
  }
  if (init || bless) {
    const body = [...now].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, count]) => `${key}\t${count}\n`).join("")
    // r+ cannot recreate a deleted baseline; wx cannot overwrite an existing one.
    const fd = openSync(file, init ? "wx" : "r+")
    try {
      writeFileSync(fd, header + body)
      ftruncateSync(fd, Buffer.byteLength(header + body))
    } finally { closeSync(fd) }
  }
}
