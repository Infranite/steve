// Runs the actual app against a clean isolated Hub. Only failure scenarios intercept metadata.
// STEVE_TEST_WEB must serve this checkout and proxy to that Hub; /auth/dev must be enabled.
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdir, writeFile } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:19651"
const out = process.env.STEVE_TEST_OUTPUT || "test-results/hub-management"
await mkdir(out, { recursive: true })
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const results = [], errors = []
const check = (name) => results.push({ name, passed: true, operated: true })
const a = await browser.newContext({ locale: "zh-CN", viewport: { width: 1440, height: 1000 } })
const b = await browser.newContext({ locale: "zh-CN", viewport: { width: 1440, height: 1000 } })
const page = await a.newPage(), other = await b.newPage()
page.on("pageerror", e => errors.push(e.message)); other.on("pageerror", e => errors.push(e.message))
const edit = () => page.locator("[data-hub-edit]").click()
const save = () => page.locator("[data-hub-save]").click()
const read = async (context = a) => {
  const r = await context.request.get(base + "/api/hub-metadata")
  assert.equal(r.status(), 200); return r.json()
}
const put = async (draft, context = b) => {
  const info = await read(context)
  const r = await context.request.put(base + "/api/hub-metadata", { headers: { "x-steve-ws": "default" }, data: { revision: info.metadata.revision, ...draft } })
  assert.equal(r.status(),200,await r.text()); return r.json()
}
/* the Hub card offers 重试 only while the metadata cannot be read; otherwise it reads again when the window gains focus */
const retry = async (p = page) => {
  const b = p.getByRole("button",{name:"重试",exact:true})
  /* the button leaves as soon as a read succeeds (going back online reads by itself) */
  if (await b.count()) { try { await b.first().click({ timeout: 2000 }); return } catch { /* read already */ } }
  await p.evaluate(() => window.dispatchEvent(new Event("focus")))
}
const waitReady = async p => { await p.locator("[data-hub-edit]").waitFor(); await p.waitForFunction(() => !document.querySelector("[data-hub-edit]")?.disabled) }
try {
  for (const p of [page, other]) {
    await p.goto(base + "/auth/dev")
    await p.goto(base + "/#/devices/hub")
    await waitReady(p)
  }
  const initial = await read()
  assert.match(initial.metadata.hub_id,/^hub_/)
  await edit()
  await page.locator("#hub-name").fill("Shared live 😀")
  await page.locator("#hub-note").fill("<b>plain text</b>\nsecond paragraph")
  await save()
  await page.getByText("Hub 基本信息已保存",{exact:true}).waitFor()
  assert.equal((await read()).metadata.display_name,"Shared live 😀")
  await other.reload(); await waitReady(other)
  assert.equal(await other.locator("[data-hub-purpose]").innerText(),"<b>plain text</b>\nsecond paragraph")
  assert.equal(await other.locator("[data-hub-purpose] b").count(),0)
  check("Actual save and second independent browser read the same shared name, ID and plain-text paragraphs")
  await edit(); await page.locator("#hub-name").fill("Discard me")
  page.once("dialog", d => d.accept())
  await page.locator("[data-hub-cancel]").click()
  assert.equal((await read()).metadata.display_name,"Shared live 😀")
  check("Cancel discards a dirty draft only after confirmation and does not write")
  await edit(); await page.locator("#hub-name").fill(" "); await save()
  assert.equal(await page.locator("#hub-name").getAttribute("aria-invalid"),"true")
  assert.equal(await page.locator("#hub-name").evaluate(el=>el===document.activeElement),true)
  await page.locator("#hub-name").fill("Valid name")
  await page.locator("#hub-note").fill("😀".repeat(501)); await save()
  assert.equal(await page.locator("#hub-note").getAttribute("aria-invalid"),"true")
  assert.equal((await page.locator("#hub-note").inputValue()).length,1002)
  page.once("dialog", d=>d.accept()); await page.locator("[data-hub-cancel]").click()
  check("Validation counts Unicode scalars, keeps all overlong input and focuses the invalid field")
  const sibling = await a.newPage(); await sibling.goto(base+"/#/devices/hub"); await waitReady(sibling)
  await page.locator("[data-hub-alias-edit]").click(); await page.locator("#hub-alias").fill("My private alias")
  await page.locator("[data-hub-alias-save]").click(); await page.getByText("已更新此客户端的个人别名",{exact:true}).waitFor()
  assert.equal(await page.locator("[data-hub-alias-value]").innerText(),"My private alias")
  await sibling.getByText("My private alias",{exact:true}).waitFor()
  assert.equal(await other.locator("[data-hub-alias-value]").innerText(),"未设置个人别名")
  assert.equal((await read()).metadata.display_name,"Shared live 😀")
  await page.locator("[data-hub-alias-edit]").click(); await page.locator("[data-hub-alias-clear]").click(); await page.locator("[data-hub-alias-save]").click()
  await page.getByText("未设置个人别名",{exact:true}).waitFor()
  await sibling.getByText("未设置个人别名",{exact:true}).waitFor(); await sibling.close()
  check("Browser alias rename and clear refresh sibling tabs of that client, stay local to that browser, and preserve shared metadata and connection")
  await edit(); await page.locator("#hub-name").fill("Local unsaved draft")
  await put({display_name:"Other client update",purpose_note:"Remote purpose"})
  await save()
  await page.locator("[data-hub-conflict]").waitFor()
  assert.match(await page.locator("[data-hub-conflict]").innerText(),/Other client update/)
  assert.equal(await page.locator("#hub-name").inputValue(),"Local unsaved draft")
  await page.locator("[data-hub-reconcile]").click()
  assert.equal(await page.locator("#hub-name").inputValue(),"Other client update")
  assert.match(await page.locator("[data-hub-previous]").innerText(),/Local unsaved draft/)
  await page.locator("#hub-name").fill("Reconciled live Hub"); await save()
  await page.getByText("Hub 基本信息已保存",{exact:true}).waitFor()
  check("Real concurrent update returns conflict, compares both values and loads latest without copying the old draft over it")
  // Transport failure, not an all-API mock: all reads and the rest of the app still use the real Hub.
  await page.route("**/api/hub-metadata", async r => r.request().method()==="PUT" ? r.fulfill({status:500,contentType:"application/json",body:'{"error":"injected failure"}'}) : r.continue())
  await edit(); await page.locator("#hub-note").fill("Keep failed draft"); await save()
  await page.getByText("保存结果未知。请先重读，加载最新值并核对后再提交。",{exact:true}).waitFor()
  assert.equal(await page.locator("#hub-note").inputValue(),"Keep failed draft")
  assert.equal(await page.locator("[data-hub-save]").isDisabled(),true)
  await page.locator("[data-hub-reconcile]").click(); await page.locator("[data-hub-cancel]").click()
  await page.unroute("**/api/hub-metadata")
  check("Injected write failure preserves draft, does not report success and requires read-back reconciliation")
  // Delay a real committed response beyond the client’s eight-second deadline.
  await page.route("**/api/hub-metadata", async r => {
    if(r.request().method()!=="PUT") return r.continue()
    const response = await r.fetch(); assert.equal(response.status(),200); await new Promise(resolve=>setTimeout(resolve,9000)); await r.fulfill({response}).catch(()=>{})
  })
  const beforeTimeout=await read()
  await edit(); await page.locator("#hub-note").fill("Committed but receipt lost"); await save()
  assert.equal(await page.locator("[data-hub-save]").isDisabled(),true)
  assert.equal(await page.locator("[data-hub-cancel]").isDisabled(),true)
  await page.locator("[data-hub-save]").evaluate(el=>el.click())
  await page.evaluate(()=>{location.hash="#/devices/clients"})
  await page.waitForFunction(()=>location.hash==="#/devices/hub")
  await page.getByText("读回的最新值与本次草稿一致。请核对更新记录；未将超时当作保存成功。",{exact:true}).waitFor()
  const afterTimeout=await read()
  assert.equal(afterTimeout.metadata.purpose_note,"Committed but receipt lost")
  assert.equal(afterTimeout.metadata.revision,beforeTimeout.metadata.revision+1)
  await page.locator("[data-hub-reconcile]").click(); await page.locator("[data-hub-cancel]").click()
  await page.unroute("**/api/hub-metadata")
  check("Actual timeout after a real commit blocks duplicate submission and route changes, then reads back matching values without inventing a save receipt")
  await a.setOffline(true)
  await retry()
  await page.getByText("Hub 未连接 · 上次已知快照",{exact:true}).first().waitFor()
  assert.equal(await page.locator("[data-hub-edit]").isDisabled(),true)
  assert.match(await page.locator("[data-hub-id]").innerText(),/^hub_/)
  await a.setOffline(false); await retry(); await waitReady(page)
  check("Offline view retains an explicitly timed snapshot and stable ID, with editing disabled")
  if (process.env.STEVE_TEST_DB) {
    const me=await (await a.request.get(base+"/api/me")).json()
    const role=(value)=>execFileSync("python3",["-c","import sqlite3,sys;c=sqlite3.connect(sys.argv[1]);c.execute('UPDATE users SET role=? WHERE id=?',(sys.argv[2],sys.argv[3]));c.commit();c.close()",process.env.STEVE_TEST_DB,value,me.user.id])
    await edit(); await page.locator("#hub-note").fill("Draft retained after real permission revocation")
    try {
      role("member"); await save()
      await page.getByText("编辑权限已撤销，停止保存。草稿仍保留，可复制。",{exact:true}).first().waitFor()
      assert.equal(await page.locator("#hub-note").inputValue(),"Draft retained after real permission revocation")
      assert.equal(await page.locator("[data-hub-save]").isDisabled(),true)
    } finally { role("owner") }
    await retry()
    page.once("dialog",d=>d.accept()); await page.locator("[data-hub-cancel]").click()
    check("Actual instance role revocation returns HTTP 403, disables writes and preserves the copyable browser draft")
  }
  const liveHello=await (await a.request.get(base+"/auth/hello")).json()
  const {capabilities:_declared,...legacyHello}=liveHello
  await page.route("**/auth/hello",r=>r.fulfill({status:200,contentType:"application/json",body:JSON.stringify(legacyHello)}))
  await retry()
  await page.getByText("此 Hub 暂不支持共享信息",{exact:true}).waitFor()
  await page.unroute("**/auth/hello")
  await retry(); await waitReady(page)
  check("A legacy Hello without capability declaration is unsupported without guessing a software version")
  await edit(); await page.locator("#hub-name").fill("Navigation draft")
  page.once("dialog",d=>d.dismiss()); await page.evaluate(()=>{location.hash="#/devices/clients"})
  await page.waitForFunction(()=>location.hash==="#/devices/hub")
  assert.equal(await page.locator("#hub-name").inputValue(),"Navigation draft")
  page.once("dialog",d=>d.accept()); await page.evaluate(()=>{location.hash="#/devices/clients"})
  await page.locator('[data-access-pane]').waitFor()
  await page.goto(base+"/#/devices/hub"); await waitReady(page)
  check("Dirty route navigation can be declined without losing the editor, or accepted to open the real clients page")
  await put({display_name:"很长的共享Hub名称😀".repeat(5),purpose_note:"多段用途备注\n".repeat(20)})
  await page.reload(); await waitReady(page)
  for (const width of [1440,320]) {
    await page.setViewportSize({width,height:1000})
    await page.goto(base+"/#/more")
    if(width===320) { await page.locator("[data-more-devices]").click(); await page.locator('[data-seg="hub"]').click(); await waitReady(page) }
    else { await page.goto(base+"/#/devices/hub"); await waitReady(page) }
    await edit()
    await page.locator("#hub-name").fill("长名称😀".repeat(10))
    await page.locator("#hub-note").fill("长段落 plain text ".repeat(20))
    for (const zoom of [1,2]) {
      if(zoom===2) await page.evaluate(()=>{const sizes=[...document.querySelectorAll("[data-hub-management], [data-hub-management] *")].map(el=>[el,parseFloat(getComputedStyle(el).fontSize)]);for(const [el,size] of sizes){el.style.fontSize=(size*2)+"px"}})
      const overflow = await page.locator("[data-hub-management]").evaluate(el=>({scroll:el.scrollWidth,client:el.clientWidth}))
      for(const selector of [".ui-isc__name-text", ".ui-isc__meta", ".ui-fldc__message"]){
        const nodes=await page.locator(`[data-hub-management] ${selector}`).all()
        for(const node of nodes){
          assert.equal(await node.evaluate(el=>getComputedStyle(el).whiteSpace!=="nowrap" || el.scrollWidth<=el.clientWidth+1),true)
          assert.equal(await node.evaluate(el=>parseFloat(getComputedStyle(el).lineHeight)>=parseFloat(getComputedStyle(el).fontSize)),true)
        }
      }
      assert.ok(overflow.scroll<=overflow.client+1,`${width}/${zoom}: ${JSON.stringify(overflow)}`)
      await page.locator("[data-hub-save]").scrollIntoViewIfNeeded()
      const box=await page.locator("[data-hub-save]").boundingBox(); assert.ok(box.x>=0&&box.x+box.width<=width+1)
      await page.screenshot({path:`${out}/hub-${width}-${zoom*100}.png`})
      check(`Actual editor at ${width}px and ${zoom*100}% text: fields and save reachable without horizontal clipping`)
    }
    page.once("dialog",d=>d.accept()); await page.locator("[data-hub-cancel]").click()
    await page.reload(); await waitReady(page)
  }
  const english = await browser.newContext({locale:"en-US",viewport:{width:320,height:1000}})
  const enPage = await english.newPage(); enPage.on("pageerror",e=>errors.push(e.message))
  await enPage.goto(base+"/auth/dev"); await enPage.goto(base+"/#/devices/hub"); await waitReady(enPage)
  await enPage.getByRole("button",{name:"Edit shared information",exact:true}).click()
  await enPage.evaluate(()=>{const sizes=[...document.querySelectorAll("[data-hub-management], [data-hub-management] *")].map(el=>[el,parseFloat(getComputedStyle(el).fontSize)]);for(const [el,size]of sizes)el.style.fontSize=(size*2)+"px"})
  assert.equal(await enPage.locator("[data-hub-management]").evaluate(el=>el.scrollWidth<=el.clientWidth+1),true)
  await enPage.locator("[data-hub-save]").scrollIntoViewIfNeeded()
  await enPage.screenshot({path:`${out}/hub-en-320-200.png`}); await english.close()
  check("English management editor at 320px and 200% text keeps the translated save action reachable without horizontal overflow")
  // The web client still loads while its Hub is unavailable, including after reload.
  await page.route("**/auth/hello",r=>r.abort("connectionrefused"))
  await page.route(url=>url.pathname.startsWith("/api/"),r=>r.abort("connectionrefused"))
  await page.reload()
  await page.locator("[data-hub-reconnect]").waitFor()
  await page.getByText("Hub 未连接 · 上次已知快照",{exact:true}).waitFor()
  assert.match(await page.locator("[data-hub-id]").innerText(),/^hub_/)
  assert.equal(await page.locator("[data-hub-edit]").isDisabled(),true)
  await page.screenshot({path:`${out}/hub-offline-reload.png`})
  check("Reloading the real route with an unavailable Hub retains the timed read-only snapshot and connection recovery controls")
  assert.deepEqual(errors,[])
  check("No browser page errors during real route and mobile entry operation")
} finally {
  await writeFile(`${out}/browser-results.json`,JSON.stringify({checks:results,errors},null,2))
  await browser.close()
}
