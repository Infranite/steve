/* Real Chromium interactions with Steve's actual cards against an intercepted, fixture-only API.
 * No Hub or external messaging service receives a request. Set STEVE_TEST_SHOTS to save screenshots and receipts. */
import assert from "node:assert/strict"
import { mkdirSync, writeFileSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const receipts = []
let assertions = 0
const eq = (actual, expected, message) => { assert.deepEqual(actual, expected, message); assertions++ }
const ok = (value, message) => { assert.ok(value, message); assertions++ }
const context = (id) => ({ space: { id: id === "cm_next" ? "sp_next" : "sp_product", name: id === "cm_next" ? "另一个聊天" : "产品讨论群", kind: "group", external: true }, as_you: id === "cm_next", persona: id === "cm_next" ? null : "值班助手", text: "正式草稿", reply_to: { id: "cm_in", sender: "陈同学", text: "你是谁啊？\n我想了解你的职责。", at: 1 }, before: [{ sender: "林同学", text: "欢迎新同学。", at: 0 }] })
const snapshot = { version: "fixture", web_version: "fixture", workspace: { id: "ws_fixture", item_prefix: "ST" }, workspaces: [], items: [], sessions: [], projects: [], decisions: [], recent: [], bots: [], allBots: [], tags: [], tagUses: {}, thread: [], settings: {}, devices: [], runtimes: [] }
const openSnapshot = { ...snapshot, decisions: [{ id: "dc_reply", kind: "approve", subject_type: "chan_message", subject_id: "cm_reply", item_id: null, session_id: null, question: "以值班助手回复「产品讨论群」的消息", draft: "我是值班助手。", options: ["yes", "no"], status: "open", result: null, requested_by: "steve", created: 1, resolved: null, undo: null }] }

async function open(kind, { width = 1280, lang = "zh-CN", dark = true, fail = false, refreshFails = true, missingContext = false, delayedContext = false, staleRead = false } = {}) {
  const page = await browser.newPage({ locale: lang, viewport: { width, height: 1000 } })
  page.setDefaultTimeout(30_000)
  const errors = [], calls = []
  let release, contextRelease, stateRelease, failNext = fail, stateReads = 0
  let stateResponse = refreshFails ? null : snapshot
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript((lang) => localStorage.setItem("steve-lang", lang), lang)
  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url())
    if (u.origin !== base) return route.abort()
    if (!u.pathname.startsWith("/api/")) return route.continue()
    const body = route.request().method() === "POST" ? route.request().postDataJSON() : null
    if (u.pathname.startsWith("/api/decisions/") && body) {
      calls.push({ path: u.pathname, body })
      await new Promise((resolve) => { release = resolve })
      const failed = failNext; failNext = false
      receipts.push({ scenario: kind, width, lang, request: calls.at(-1), response: failed ? { status: 503, error: "fixture: channel unavailable" } : { status: 200, result: { choice: body.choice, send_at: body.choice === "yes" ? 2000 : undefined } } })
      return failed ? route.fulfill({ status: 503, json: { error: "fixture: channel unavailable" } }) : route.fulfill({ json: { id: "dc_reply", status: "resolved", result: { choice: body.choice, ...(body.choice === "yes" ? { send_at: 2000 } : {}) } } })
    }
    if (u.pathname.endsWith("/context")) {
      const id = u.pathname.split("/")[3]
      if (delayedContext && id === "cm_next") await new Promise((resolve) => { contextRelease = resolve })
      return missingContext ? route.fulfill({ status: 404, json: { error: "fixture: old Hub" } }) : route.fulfill({ json: context(id) })
    }
    if (u.pathname === "/api/shell") {
      stateReads++
      if (staleRead && stateReads === 1) { await new Promise((resolve) => { stateRelease = resolve }); return route.fulfill({ json: openSnapshot }) }
      return stateResponse ? route.fulfill({ json: stateResponse }) : route.fulfill({ status: 503, json: { error: "fixture: state refresh unavailable" } })
    }
    return route.fulfill({ status: 404, json: { error: "fixture: unavailable" } })
  })
  await page.goto(`${base}/test/fixtures/channel-reply.html?case=${kind}${dark ? "&dark" : ""}`)
  await page.locator(kind === "composer" ? "[data-pending-strip]" : "[data-decision-id]").first().waitFor()
  return {
    page, calls,
    async started(n = 1) { await page.waitForFunction(() => document.querySelector('[aria-busy="true"]')); eq(calls.length, n, "exactly one decision request per click") },
    release() { assert.ok(release, "a request is waiting"); release() },
    authoritativeOpen() { stateResponse = openSnapshot },
    authoritativeClosed() { stateResponse = snapshot },
    async stateStarted() { for (let n = 0; n < 100 && !stateRelease; n++) await page.waitForTimeout(10); assert.ok(stateRelease) },
    releaseState() { assert.ok(stateRelease); stateRelease() },
    async shot(name) { if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true, animations: "disabled" }) },
    async close() { eq(errors, [], "no browser runtime errors"); ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow"); await page.close() },
    async releaseContext() { for (let n = 0; n < 100 && !contextRelease; n++) await page.waitForTimeout(10); assert.ok(contextRelease); contextRelease() },
  }
}

try {
  for (const [width, lang, dark] of [[1280, "zh-CN", true], [390, "zh-CN", false], [390, "en", true]]) {
    const x = await open("draft", { width, lang, dark })
    const p = x.page
    await p.locator("[data-out-reply-to]").waitFor()
    ok((await p.locator("[data-out-reply-to]").textContent()).includes("你是谁啊？\n"), "original message keeps its lines")
    ok((await p.locator("[data-out-context]").textContent()).includes("产品讨论群"), "target chat shown")
    ok((await p.locator("[data-out-context]").textContent()).includes("值班助手"), "outward identity shown")
    ok((await p.locator("[data-reply-draft]").textContent()).includes("\n"), "formal draft keeps its lines")
    eq(await p.locator("[data-reply-choice='to_item']").count(), 1)
    eq(await p.locator("[data-reply-choice='no']").count(), 1)
    const buttonStyle = await p.locator("[data-reply-send]").evaluate((b) => { const s = getComputedStyle(b); return { background: s.backgroundColor, padding: parseFloat(s.paddingLeft), height: b.getBoundingClientRect().height } })
    ok(buttonStyle.background !== "rgba(0, 0, 0, 0)" && buttonStyle.padding > 0 && buttonStyle.height >= 28, "embedded affectUI send button retains its theme and hit area")
    await x.shot(`review-${width}-${lang}`)
    await p.locator("[data-reply-send]").evaluate((b) => { b.click(); b.click() })
    await x.started()
    eq(await p.locator("[data-reply-result]").count(), 0, "pending request has no successful receipt")
    ok(await p.locator("[data-reply-send]").isDisabled(), "send disabled in flight")
    ok(await p.locator("[data-reply-choice='no']").isDisabled(), "other actions disabled in flight")
    await x.shot(`submitting-${width}-${lang}`)
    eq(x.calls[0].body, { choice: "yes" }, "original approval uses yes without replacing text")
    x.release()
    await p.locator("[data-reply-result='yes']").waitFor()
    ok((await p.locator("[data-reply-result]").textContent()).includes(lang === "en" ? "waiting" : "等待"), "approval stays queued, never sent")
    eq(await p.locator("[data-reply-error]").count(), 0, "refresh failure is not a failed submission")
    eq(await p.locator("[data-reply-send]").count(), 0, "accepted snapshot cannot be sent again")
    await x.shot(`queued-${width}-${lang}`)
    await x.close()
  }

  const edit = await open("draft", { fail: true })
  await edit.page.locator("[data-reply-edit]").click()
  const editor = edit.page.locator("[data-reply-editor]")
  ok((await editor.inputValue()).startsWith("我是值班助手"), "editing starts with formal draft")
  await editor.fill("   ")
  ok(await edit.page.locator("[data-reply-send]").isDisabled(), "blank edits cannot silently send the old draft")
  await editor.fill("我是项目答疑助手。\n我可以帮你整理问题。")
  await edit.page.locator("[data-reply-send]").click()
  await edit.started()
  eq(edit.calls[0].body, { choice: "yes", text: "我是项目答疑助手。\n我可以帮你整理问题。" }, "edited body accompanies yes")
  edit.release()
  await edit.page.locator("[data-reply-error]").waitFor()
  eq(await editor.inputValue(), "我是项目答疑助手。\n我可以帮你整理问题。", "failed submission preserves edits")
  eq(await edit.page.locator("[data-reply-result]").count(), 0, "failure is not shown as queued or sent")
  await edit.shot("edited-failure")
  await edit.page.locator("[data-reply-send]").click()
  await edit.started(2)
  edit.release()
  await edit.page.locator("[data-reply-result='yes']").waitFor()
  eq(edit.calls[1].body, edit.calls[0].body, "retry sends preserved edits")
  await edit.close()

  for (const choice of ["no", "to_item", "rewrite"]) {
    const x = await open("draft")
    await x.page.locator(`[data-reply-choice='${choice}']`).click()
    await x.started()
    eq(x.calls[0].body, { choice }, "actual option id is submitted")
    x.release()
    await x.page.locator(`[data-reply-result='${choice}']`).waitFor()
    await x.close()
  }

  for (const kind of ["handoff", "legacy-ask"]) {
    const x = await open(kind)
    eq(await x.page.locator("[data-reply-send], [data-approve]").count(), 0, "owner question never gains a send button")
    eq(await x.page.locator("[data-option='to_item']").count(), 1)
    eq(await x.page.locator("[data-option='done']").count(), 1)
    await x.page.locator("[data-option-words='draft_reply']").fill("我是答疑助手。")
    await x.page.locator("[data-option='draft_reply']").click()
    await x.started()
    eq(x.calls[0].body, { choice: "draft_reply", text: "我是答疑助手。" }, "handoff creates a draft, not a send")
    x.release()
    await x.shot(kind)
    await x.close()
  }

  const restricted = await open("restricted")
  eq(await restricted.page.locator("[data-reply-send], [data-reply-edit], [data-rewrite]").count(), 0, "absent options cannot become actions")
  await restricted.close()
  for (const kind of ["legacy", "draft"]) {
    const x = await open(kind, { missingContext: true })
    await x.page.locator("[data-out-context-state='unavailable']").waitFor()
    eq(await x.page.locator("[data-reply-send]").count(), 1, "old cards can still review their draft")
    await x.shot(`context-unavailable-${kind}`)
    await x.close()
  }
  const empty = await open("empty")
  eq(await empty.page.locator("[data-reply-editor]").inputValue(), "")
  ok(await empty.page.locator("[data-reply-send]").isDisabled())
  await empty.page.locator("[data-reply-editor]").fill("手填的正式回复")
  await empty.page.locator("[data-reply-send]").click()
  await empty.started()
  eq(empty.calls[0].body, { choice: "yes", text: "手填的正式回复" })
  empty.release()
  await empty.page.locator("[data-reply-result='yes']").waitFor()
  await empty.close()

  const duplicate = await open("duplicate")
  const sends = duplicate.page.locator("[data-reply-send]")
  await sends.first().evaluate((b) => b.click())
  await sends.last().evaluate((b) => b.click())
  await duplicate.started()
  duplicate.release()
  await duplicate.page.locator("[data-reply-result='yes']").first().waitFor()
  eq(await sends.count(), 0, "accepted result locks every view of the same snapshot")
  eq(duplicate.calls.length, 1, "two views cannot cause duplicate submission")
  await duplicate.page.locator("[data-fixture-rebuild]").click()
  eq(await sends.count(), 0, "a render rebuild cannot unlock an accepted decision")
  duplicate.authoritativeOpen()
  await Promise.all([duplicate.page.waitForResponse((r) => new URL(r.url()).pathname === "/api/shell"), duplicate.page.locator("[data-fixture-fresh]").click()])
  eq(await sends.count(), 0, "a stale open snapshot cannot unlock an accepted approval")
  duplicate.authoritativeClosed()
  await duplicate.page.locator("[data-fixture-fresh]").click()
  await duplicate.page.locator("[data-channel-reply]").first().waitFor({ state: "detached" })
  duplicate.authoritativeOpen()
  await duplicate.page.locator("[data-fixture-fresh]").click()
  await duplicate.page.locator("[data-reply-send]").first().waitFor()
  eq(duplicate.calls.length, 1, "fresh/undo snapshot can be reviewed without resubmitting on render")
  await duplicate.close()

  const stale = await open("draft", { staleRead: true })
  await stale.page.locator("[data-fixture-fresh]").click()
  await stale.stateStarted()
  await stale.page.locator("[data-reply-send]").click()
  await stale.started()
  stale.release()
  await stale.page.locator("[data-reply-result='yes']").waitFor()
  stale.releaseState()
  await stale.page.waitForFunction(() => !document.querySelector('[data-decision-id][aria-busy="true"]'))
  eq(await stale.page.locator("[data-reply-result='yes']").count(), 1, "a stale pre-approval state read cannot erase the accepted result")
  eq(await stale.page.locator("[data-reply-send]").count(), 0, "failed follow-up read keeps the stale card locked")
  eq(stale.calls.length, 1)
  await stale.close()

  const composer = await open("composer", { delayedContext: true })
  await composer.page.getByRole("button", { name: "展开", exact: true }).click()
  await composer.page.locator("[data-out-reply-to]").waitFor()
  await composer.page.locator("[data-step='next']").click()
  await composer.page.locator("[data-out-context-state='loading']").waitFor()
  eq(await composer.page.locator("[data-out-context]").count(), 0, "previous chat is hidden before next context arrives")
  await composer.releaseContext()
  await composer.page.locator("[data-out-context]").getByText("另一个聊天", { exact: true }).waitFor()
  ok((await composer.page.locator("[data-out-context]").textContent()).includes("以你本人"), "next card shows its own sending identity")
  await composer.shot("composer-next-context")
  await composer.close()

  for (const kind of ["plain", "tool", "failed"]) {
    const x = await open(kind)
    eq(await x.page.locator("[data-channel-reply]").count(), 0, "only channel approvals use the special editor")
    if (kind === "plain") { eq(await x.page.locator("[data-approve]").count(), 1); await x.page.locator("[data-reject-reason]").fill("不允许这项操作"); await x.page.locator("[data-reject]").click() }
    if (kind === "tool") { eq(await x.page.locator("[data-deny]").count(), 1); await x.page.locator("[data-allow]").click() }
    if (kind === "failed") { eq(await x.page.locator("[data-fail='discard']").count(), 1); await x.page.locator("[data-fail='retry']").click() }
    await x.started()
    eq(x.calls[0].body, kind === "plain" ? { choice: "no", text: "不允许这项操作" } : { choice: kind === "tool" ? "once" : "retry" }, "other card types keep their original actions")
    x.release()
    if (kind === "failed") {
      await x.page.waitForFunction(() => window.__steve.pendingActions.size === 0)
      ok(await x.page.locator("[data-fail='retry']").isDisabled(), "accepted recovery stays locked when state refresh fails")
    } else await x.page.waitForFunction(() => !document.querySelector('[aria-busy="true"]'))
    await x.close()
  }
  const refreshed = await open("draft", { refreshFails: false })
  await refreshed.page.locator("[data-reply-send]").click()
  await refreshed.started()
  refreshed.release()
  await refreshed.page.locator("[data-channel-reply]").waitFor({ state: "detached" })
  eq(await refreshed.page.locator("[data-reply-error]").count(), 0, "successful refresh removes the resolved card")
  await refreshed.close()
} finally {
  if (shots) writeFileSync(`${shots}/fixture-receipts.json`, JSON.stringify({ assertions, note: "Intercepted fixture API only; these are not provider send receipts.", receipts }, null, 2))
  await browser.close()
}
console.log(`channel-reply: ${assertions} assertions passed (fixture API; no external send)`)
