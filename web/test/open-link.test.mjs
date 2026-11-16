import assert from "node:assert/strict"
import { test } from "node:test"

const { openable, openInApp } = await import("../src/channels/open-link.ts")
const FEISHU = "https://applink.feishu.cn/client/chat/open?openChatId=oc_0000test0001"
const LARK = "https://applink.larksuite.com/client/chat/open?openChatId=oc_0000test0001"

test("only an https link from the Hub is opened; a missing or other one shows nothing", () => {
  assert.equal(openable(FEISHU), FEISHU)
  assert.equal(openable(LARK), LARK)
  for (const x of [undefined, null, "", "not a url", "javascript:alert(1)", "http://applink.feishu.cn/client/chat/open?openChatId=oc_x", "lark://client/chat/1"]) assert.equal(openable(x), undefined, String(x))
})

test("opening follows it as a link in a new tab, never window.open", () => {
  const made = []
  const doc = { createElement: (tag) => { const a = { tag, clicked: 0, click() { this.clicked++ } }; made.push(a); return a } }
  openInApp(FEISHU, doc)
  assert.equal(made.length, 1)
  assert.deepEqual({ ...made[0], click: undefined }, { tag: "a", href: FEISHU, target: "_blank", rel: "noopener noreferrer", clicked: 1, click: undefined })
  openInApp("javascript:alert(1)", doc)
  assert.equal(made.length, 1, "an unsafe link is not followed")
})
