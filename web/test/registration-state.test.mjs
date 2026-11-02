import assert from "node:assert/strict"
import { test } from "node:test"
import { registrationPending, registrationLabel, officialConfirmationUrl, remainingSeconds } from "../src/integrations/registration-state.ts"

test("saved credentials with failed verification stay retryable rather than polling forever", () => {
  const saved = { state: "credentials_saved", expires_at: 0 }
  assert.equal(registrationPending(saved), true)
  assert.equal(registrationPending({ ...saved, error: "检查失败" }), false)
  assert.match(registrationLabel({ ...saved, error: "检查失败" }), /凭据已保存/)
  for (const state of ["succeeded", "denied", "expired", "failed", "cancelled"]) assert.equal(registrationPending({ state, expires_at: 0 }), false)
})

test("confirmation links allow only official HTTPS hosts, with no credentials or alternate ports", () => {
  assert.equal(officialConfirmationUrl("https://open.feishu.cn/page/cli?user_code=abc"), "https://open.feishu.cn/page/cli?user_code=abc")
  assert.ok(officialConfirmationUrl("https://open.larksuite.com/page/cli?user_code=abc"))
  for (const url of [undefined, "javascript:alert(1)", "https://open.feishu.cn.evil.test", "http://open.feishu.cn", "https://evil@open.feishu.cn", "https://open.feishu.cn:8443", "https://example.com/callback", "/callback"]) assert.equal(officialConfirmationUrl(url), undefined)
})

test("expired countdown never goes negative or extends the server expiry", () => {
  assert.equal(remainingSeconds(20_000, 15_001), 5)
  assert.equal(remainingSeconds(20_000, 20_001), 0)
})
