import assert from "node:assert/strict"
import { test } from "node:test"
import { formatRichText, isRichText } from "../src/channels/rich-text.ts"
import { channelConversationHref, openable } from "../src/channels/open-link.ts"

const i18n = await import("../src/i18n/index.ts")
const inLang = async (l, fn) => {
  await i18n.default.changeLanguage(l)
  try { return fn() } finally { await i18n.default.changeLanguage("zh-CN") }
}

test("rich-text helper detects markdown/links and leaves plain chat messages on fast path", () => {
  assert.equal(isRichText("好的，收到"), false)
  assert.equal(isRichText("明天下午三点开会，带上架构图"), false)
  assert.equal(isRichText("发布清单 (https://wiki.example.com/release)"), true)
  assert.equal(isRichText("请看 [文档](https://example.com/doc)"), true)
  assert.equal(isRichText("**重点**：今晚 8 点发布"), true)
  assert.equal(isRichText("```ts\nconst x = 1\n```"), true)
})

test("formatRichText converts provider link pairs to Markdown outside code fences", () => {
  const input = [
    "上线检查",
    "发布清单 (https://wiki.example.com/release)，有问题 @林知远",
    "```",
    "示例 (https://inside-code.example.com/keep)",
    "```",
    "恶意 (javascript:alert(1))",
  ].join("\n")
  const out = formatRichText(input)
  assert.ok(out.includes("[发布清单](https://wiki.example.com/release)"))
  assert.ok(out.includes("示例 (https://inside-code.example.com/keep)"))
  assert.ok(!out.includes("[恶意](javascript:"))
})

test("openable allows only https URLs for generic provider open-in-app links", () => {
  assert.equal(openable("https://applink.feishu.cn/client/chat/open?openChatId=oc_1"), "https://applink.feishu.cn/client/chat/open?openChatId=oc_1")
  assert.equal(openable("https://acme.example/chat/room_42"), "https://acme.example/chat/room_42")
  assert.equal(openable("javascript:alert(1)"), undefined)
  assert.equal(openable("http://insecure.example/chat"), undefined)
})

test("channelConversationHref routes space draft clicks directly to #/c/chan:<id> rather than #/inbox", () => {
  assert.equal(channelConversationHref("sp_ops"), "#/c/chan%3Asp_ops")
  assert.equal(channelConversationHref("spc_123/45"), "#/c/chan%3Aspc_123%2F45")
  assert.notEqual(channelConversationHref("sp_ops"), "#/inbox")
})

test("provider open-in-app and ai_mark i18n keys render cleanly in zh-CN and en", async () => {
  assert.equal(i18n.t("channels:word.openInFeishu"), "在飞书中打开")
  assert.equal(i18n.t("channels:word.openInProvider", { provider: "Slack" }), "在 Slack 中打开")
  assert.equal(i18n.t("channels:word.openInSourceApp"), "在原应用中打开")
  assert.equal(i18n.t("channels:card.imageInApp", { app: "飞书" }), "卡片内图片请在飞书中查看")
  assert.equal(i18n.t("channels:unavailable.folder", { app: "飞书" }), "文件夹请在飞书中打开")
  await inLang("en", () => {
    assert.equal(i18n.t("channels:word.openInFeishu"), "Open in Feishu")
    assert.equal(i18n.t("channels:word.openInProvider", { provider: "Slack" }), "Open in Slack")
    assert.equal(i18n.t("channels:word.openInSourceApp"), "Open in source app")
    assert.equal(i18n.t("channels:card.imageInApp", { app: "Slack" }), "View card images in Slack")
  })
})
