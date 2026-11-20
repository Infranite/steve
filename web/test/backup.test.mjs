import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/backup.ts")
const status = (daily, last) => ({ database: "/h/data/steve.sqlite3", dir: "/h/backups", daily, keep_days: 7, settings: { enabled: daily, keep_days: 7 }, available: true, last })
const copy = { name: "daily-20260102.sqlite3", at: 1, bytes: 2 }

test("the backup row says the plan, and the newest daily copy or that none was made yet", () => {
  assert.deepEqual([m.planKey(status(true, copy)), m.lastKey(status(true, copy))], ["backupDaily", "backupLast"])
  assert.deepEqual([m.planKey(status(true, null)), m.lastKey(status(true, null))], ["backupDaily", "backupNone"])
})

test("turned off, it says so, and still shows a copy that is left", () => {
  assert.deepEqual([m.planKey(status(false, null)), m.lastKey(status(false, null))], ["backupOff", null])
  assert.equal(m.lastKey(status(false, copy)), "backupLast")
})


test("backup settings save both fields and return the persisted values", async () => {
  const calls = []
  const request = async (...args) => { calls.push(args); return args[2] }
  const first = await m.saveBackupSettings({ enabled: false, keep_days: 30 }, request)
  const second = await m.saveBackupSettings({ ...first, enabled: true, keep_days: 7 }, request)
  assert.deepEqual(calls, [
    ["PATCH", "/system/backup", { enabled: false, keep_days: 30 }],
    ["PATCH", "/system/backup", { enabled: true, keep_days: 7 }],
  ])
  assert.deepEqual(second, { enabled: true, keep_days: 7 })
  for (const keep_days of [0, -1, 1.5, 366, NaN]) {
    await assert.rejects(() => m.saveBackupSettings({ enabled: true, keep_days }, request))
  }
  assert.equal(calls.length, 2)
})
