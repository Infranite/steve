import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/backup.ts")
const status = (daily, last) => ({ database: "/h/data/steve.sqlite3", dir: "/h/backups", daily, keep_days: 14, last })
const copy = { name: "daily-20260102.sqlite3", at: 1, bytes: 2 }

test("the backup row says the plan, and the newest daily copy or that none was made yet", () => {
  assert.deepEqual([m.planKey(status(true, copy)), m.lastKey(status(true, copy))], ["backupDaily", "backupLast"])
  assert.deepEqual([m.planKey(status(true, null)), m.lastKey(status(true, null))], ["backupDaily", "backupNone"])
})

test("turned off, it says so, and still shows a copy that is left", () => {
  assert.deepEqual([m.planKey(status(false, null)), m.lastKey(status(false, null))], ["backupOff", null])
  assert.equal(m.lastKey(status(false, copy)), "backupLast")
})
