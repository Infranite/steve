import assert from "node:assert/strict"
import { test } from "node:test"
import { bytes, diskRows, entryState, pressureText, reclaimText } from "../src/disk.ts"

const GB = 2 ** 30
const policy = { floor: 20 * GB, low: 60 * GB, idle_ms: 24 * 3_600_000 }
const entry = (item, o = {}) => ({ item, workspace: "w", title: item, status: "run", phase: "active", archived: false, branch: `steve/${item}`, path: `/h/worktrees/${item}`, exists: true, bytes: GB, cache_bytes: GB / 2, caches: ["target"], busy: false, in_use: false, updated: 0, ...o })
const report = (o = {}) => ({ root: "/h/worktrees", free: 100 * GB, total: 1000 * GB, pressure: "ok", policy, git: "2.54", trash: 0, entries: [], orphans: [], measured_at: 0, ...o })

test("sizes read in binary GB like the Hub's notes", () => {
  assert.equal(bytes(17 * GB + 1), "17 GB")
  assert.equal(bytes(3.44 * GB), "3.4 GB")
  assert.equal(bytes(512 * 2 ** 20), "0.5 GB")
  assert.equal(bytes(50 * 2 ** 20), "50 MB")
  assert.equal(bytes(null), "—")
})

test("the list keeps worktrees on disk, largest first, and counts what the next pass gives back", () => {
  const r = report({ entries: [
    entry("ST-1", { phase: "closed", bytes: 2 * GB, cache_bytes: 1.5 * GB }),
    entry("ST-2", { bytes: 9 * GB, cache_bytes: 8 * GB, busy: true }),
    entry("ST-3", { phase: "closed", bytes: 5 * GB, cache_bytes: 4 * GB, in_use: true }),
    entry("ST-4", { exists: false, bytes: 0, cache_bytes: 0 }),
  ] })
  const d = diskRows(r)
  assert.deepEqual(d.rows.map(e => e.item), ["ST-2", "ST-3", "ST-1"])
  assert.equal(d.total, 16 * GB)
  assert.equal(d.reclaimable, 1.5 * GB, "a closed worktree with a process inside stays")
  assert.equal(d.gone, 1)
  assert.deepEqual(r.entries.map(entryState), ["closed", "busy", "in_use", "gone"])
})

test("the disk line names the threshold it is under", () => {
  assert.match(pressureText(report()), /充足/)
  assert.match(pressureText(report({ free: 40 * GB, pressure: "low" })), /低于 60 GB：回收闲置 24 小时以上/)
  assert.match(pressureText(report({ free: 5 * GB, pressure: "floor" })), /只剩 5\.0 GB，低于下限 20 GB：暂停新建工作区/)
  assert.match(pressureText(report({ free: null, pressure: null })), /读不到/)
})

test("the reclaim toast says what was freed, removed and kept", () => {
  assert.equal(reclaimText({ skipped: true, free_before: null, free_after: null, freed: [], removed: [], kept: [] }), "已有一次回收在进行，稍后再看")
  assert.equal(
    reclaimText({ skipped: false, free_before: 0, free_after: 0, freed: [{ item: "ST-1", caches: ["target"], bytes: 12 * GB }], removed: ["ST-1"], kept: [{ item: "ST-2", reason: "有未提交的改动" }] }),
    "释放了 12 GB，删除了 1 个工作区（分支都保留），1 个按规则保留",
  )
  assert.equal(reclaimText({ skipped: false, free_before: 0, free_after: 0, freed: [], removed: [], kept: [] }), "没有可回收的缓存")
})
