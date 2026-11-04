import assert from "node:assert/strict"
import { test } from "node:test"

const entries = new Map()
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
  getItem: (k) => entries.get(k) ?? null,
  setItem: (k, v) => entries.set(k, v),
  removeItem: (k) => entries.delete(k),
} })
const { topicsOf, snippet, readPos, savePos, TOPIC_GAP } = await import("../src/thread/topics.ts")
const at = (h, m = 0) => new Date(2026, 10, 4, h, m).getTime()

test("a pause or a new day starts a topic, named by the first thing you said", () => {
  const ts = topicsOf([
    { id: "a", k: "steve", t: "早上好", created: at(9) },
    { id: "b", k: "me", t: "帮我看下 **ST-12** 的发布", created: at(9, 1) },
    { id: "c", k: "steve", t: "好的", created: at(9, 2), item: "ST-12" },
    { id: "n", k: "steve", t: "发布失败", created: at(9, 3), notice: { kind: "release", level: "fail" } },
    { id: "d", k: "me", t: "历史怎么找？", created: at(9, 2) + TOPIC_GAP + 1 },
    { k: "thinking" },
    { id: "e", k: "me", t: "（附件）", created: new Date(2026, 10, 5, 0, 5).getTime() },
  ])
  assert.deepEqual(ts.map((t) => [t.id, t.title, t.count, t.items]), [
    ["e", "（附件）", 1, []],
    ["d", "历史怎么找？", 1, []],
    ["a", "帮我看下 ST-12 的发布", 3, ["ST-12"]],
  ])
})

test("a snippet keeps the match in view and marks it", () => {
  const long = "前面有很多很多很多很多很多很多很多很多很多很多很多很多很多很多很多的话，这里讨论定位目标的细节，后面也还有不少内容要说一说说一说说一说"
  const s = snippet(long, "定位目标", 30)
  assert.equal(s.text.slice(s.at, s.at + s.len), "定位目标")
  assert.ok(s.text.startsWith("…") && s.text.endsWith("…"))
  assert.equal(snippet("ABC def", "abc").at, 0)
  assert.equal(snippet("没有", "定位").at, -1)
})

test("the reading position survives as written and rejects anything else", () => {
  assert.equal(readPos("ws"), null)
  savePos("ws", { bottom: false, id: "m1", created: 5, offset: -12 })
  assert.deepEqual(readPos("ws"), { bottom: false, id: "m1", created: 5, offset: -12 })
  savePos("ws", { bottom: true })
  assert.deepEqual(readPos("ws"), { bottom: true })
  entries.set("steve:read:v1:ws", "{broken")
  assert.equal(readPos("ws"), null)
  entries.set("steve:read:v1:ws", JSON.stringify({ id: 3 }))
  assert.equal(readPos("ws"), null)
})
