import assert from "node:assert/strict"
import { test } from "node:test"
import { modeLine, modeLines } from "../src/yolo/modes.ts"

const i18n = (await import("../src/i18n/index.ts")).default
const row = (o) => ({ session_id: "s1", readonly: false, status: "applied", wanted: "full_access", applied: "full_access", desired: null, busy: false, refused: null, residual: false, ...o })

test("only what the Node confirmed reads as in effect", () => {
  assert.equal(modeLine(row({})).tone, "ok")
  const wait = modeLine(row({ status: "after_turn", applied: "auto", desired: "full_access", busy: true, residual: true }))
  assert.deepEqual([wait.key, wait.tone, wait.applied, wait.residual], ["after_turn", "wait", "auto", true])
  for (const status of ["sending", "next_open", "not_sent"]) assert.equal(modeLine(row({ status, applied: "auto" })).tone, "wait", status)
  const refused = modeLine(row({ status: "refused", applied: "auto", refused: "this node's owner allows at most permission mode auto" }))
  assert.deepEqual([refused.tone, refused.reason], ["bad", "this node's owner allows at most permission mode auto"])
  assert.equal(modeLine(row({ status: "unknown" })).tone, "bad")
})

test("read-only sessions and sessions nothing runs are left out, and equal lines read once", () => {
  assert.equal(modeLine(row({ readonly: true })), null)
  assert.equal(modeLine(row({ status: "not_running" })), null)
  assert.equal(modeLines([row({}), row({ session_id: "s2" }), row({ session_id: "s3", status: "sending", applied: "auto" })]).length, 2)
})

test("every line has its text in both languages, with the modes named", () => {
  for (const lang of ["zh-CN", "en"]) {
    for (const key of ["applied", "after_turn", "sending", "next_open", "refused", "unknown", "not_sent", "not_running", "residual", "detail", "label", "none"])
      assert.ok(i18n.exists(`yolo:mode.${key}`, { lng: lang }), `${lang} ${key}`)
    for (const m of ["plan", "ask", "accept_edits", "auto", "full_access"]) assert.ok(i18n.exists(`yolo:mode.name.${m}`, { lng: lang }), `${lang} ${m}`)
  }
  const zh = i18n.t("yolo:mode.after_turn", { lng: "zh-CN", wanted: "完全放开", applied: "由执行器判断" })
  assert.ok(zh.includes("本回合结束后") && zh.includes("完全放开") && zh.includes("由执行器判断"), zh)
})
