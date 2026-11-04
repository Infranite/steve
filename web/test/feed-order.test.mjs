import assert from "node:assert/strict"
import { test } from "node:test"
import { blockSlots } from "../src/feed-order.ts"

const line = (ts) => ({ ts })
const comment = (ts, until) => ({ ts, fence: true, until })
const session = (ts, until = Infinity) => ({ ts, until })

test("a running session sits below the lines written while it runs", () => {
  /* 开始会话, 更新了计划, 换了一版计划: the session block goes after the last line, not after its start */
  assert.deepEqual(blockSlots([line(0), session(1), line(2), line(3)]), [-1, 3, -1, -1])
})

test("a finished session keeps the lines written after it ended below it", () => {
  assert.deepEqual(blockSlots([session(1, 5), line(4), line(6)]), [1, -1, -1])
})

test("a comment closes the session's part: a new request stays apart from the work before it", () => {
  /* a comment without a reply, and one with a reply that still runs */
  assert.deepEqual(blockSlots([session(1), line(2), comment(3), line(4)]), [1, -1, -1, -1])
  assert.deepEqual(blockSlots([session(1), line(2), comment(3, Infinity), line(4), line(5)]), [1, -1, 4, -1, -1])
})

test("the next session closes the one before it", () => {
  assert.deepEqual(blockSlots([session(1), line(2), session(3), line(4)]), [1, -1, 3, -1])
})

test("a reply that ended keeps later lines below it", () => {
  assert.deepEqual(blockSlots([comment(1, 3), line(2), line(4)]), [1, -1, -1])
})
