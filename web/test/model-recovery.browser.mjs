import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, {recursive:true})
const errors = [], unexpected = []
try {
  for (const lang of ["zh-CN", "en"]) for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({locale:lang, viewport:{width,height:width===390?844:900}, colorScheme:theme, reducedMotion:"reduce"})
    const saves=[]
    let revision=1, fail=false, delay=false, pending, markDelay
    const reachedDelay = new Promise(resolve => markDelay = resolve)
    const purposes = ["route","chat","research","title","extract","disclosure","yolo","draft","flow","mail","compose"]
    const lightKeys = ["route","title","disclosure","flow"]
    const view = () => ({lightweight:null,steve:{harness:"claude-code",model:"old-model",models:["old-model"]}, bot_revision:revision, harnesses:[{id:"claude-code",name:"Claude Code",ready:true,judgments:true},{id:"codex",name:"Codex",ready:true,judgments:true},{id:"pi",name:"Pi",ready:true,judgments:false}], purposes:purposes.map(key=>({key,label:key,role:lightKeys.includes(key)?"lightweight":"primary",follows:!lightKeys.includes(key),effective:{harness:"claude-code",models:["old-model"]}}))})
    page.on("pageerror", e=>errors.push(e.message))
    await page.addInitScript(l=>localStorage.setItem("steve-lang",l),lang)
    await page.route("**/*", async r=>{
      const u=new URL(r.request().url())
      if(u.origin!==base){unexpected.push(u.href);return r.abort()}
      if(!u.pathname.startsWith("/api/")) return r.continue()
      if(u.pathname==="/api/settings/models" && r.request().method()==="GET") return r.fulfill({json:view()})
      if(u.pathname==="/api/system/models") { const light=u.searchParams.get("harness")==="claude-code"; return r.fulfill({json:{harness:u.searchParams.get("harness"),name:"Codex",source:"Fixture",default:"new-model",families:["test"],models:[{id:light?"small-model":"new-model",label:light?"Small model":"New model",family:"test",via:null,note:null,efforts:[]}]}}) }
      if(u.pathname==="/api/settings/models/select") {
        saves.push(r.request().postDataJSON())
        if(delay) { markDelay(); await new Promise(resolve=>pending=resolve) }
        return r.fulfill(fail?{status:409,json:{error:"Changed elsewhere"}}:{json:{...view(),bot_revision:++revision,steve:{harness:"codex",model:"new-model",models:["new-model"]}}})
      }
      if(u.pathname==="/api/state") return r.fulfill({status:503,json:{error:"fixture state refresh disabled"}})
      // Refresh and history are read-only fixture background traffic, isolated from a real Hub.
      if(r.request().method()==="GET") return r.fulfill({json:{messages:[],more:false}})
      unexpected.push(u.pathname);return r.abort()
    })
    await page.goto(base+"/test/fixtures/model-recovery.html")
    await page.locator('[data-steve-model-control="retry"]').click()
    const sheet=page.locator("[data-steve-model-sheet]")
    await sheet.locator("[data-current-model]").waitFor()
    assert.match(await sheet.innerText(),/old-model/)
    await sheet.locator('[data-select="steve-harness"]').click()
    assert.equal(await page.getByRole("option",{name:"Pi",exact:true}).count(),0,"native-only adapters cannot run read-only judgments")
    await page.getByRole("option",{name:"Codex",exact:true}).click()
    await sheet.locator('[data-select="steve-harness"]').click()
    await page.keyboard.press("Escape")
    assert.equal(await sheet.isVisible(), true, "Escape closes the harness menu before the parent sheet")
    await sheet.locator('.ui-mdpc__trigger').click()
    await page.getByRole("option",{name:/New model/}).click()
    await sheet.locator('[data-lightweight-enable]').check()
    await sheet.locator('[data-select="lightweight-harness"]').click()
    await page.getByRole("option",{name:"Claude Code",exact:true}).click()
    await sheet.locator('[data-model-picker="lightweight-model"]').click()
    await page.locator('[data-model-opt="small-model"]').click()
    await sheet.locator('[data-role-preview-toggle]').click()
    assert.equal(await sheet.locator('[data-role-preview-purpose]').count(),11)
    for(const key of purposes) assert.match(await sheet.locator(`[data-role-preview-purpose="${key}"]`).innerText(),lightKeys.includes(key)?/small-model/:/new-model/)
    await sheet.locator('[data-role-preview-toggle]').click()
    assert.equal(saves.length,0,"browsing models does not save or run inference")
    assert.deepEqual(await page.evaluate(()=>window.__say),[])
    // Failure to save must not send the original message using the old provider.
    fail=true
    await sheet.locator('[data-steve-model-save]').click()
    await sheet.locator('[data-steve-model-error]').waitFor()
    assert.deepEqual(await page.evaluate(()=>window.__say),[])
    fail=false
    if(shots) await page.screenshot({path:`${shots}/model-recovery-${lang}-${width}-${theme}.png`})
    await sheet.locator('[data-steve-model-save]').click()
    await sheet.waitFor({state:"hidden"})
    const calls=await page.evaluate(()=>window.__say)
    assert.equal(calls.length,1)
    assert.equal(calls[0][0],"用来构建 AI Native 的生活与工作")
    assert.deepEqual(calls[0][1],["file-1"])
    assert.equal(calls[0][3].reply_to,"question")
    assert.equal(calls[0][3].item,undefined)
    assert.deepEqual(saves.at(-1),{harness:"codex",model:"new-model",expected_revision:1,follow_purposes:true,lightweight:{harness:"claude-code",models:["small-model"]}})
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),"no horizontal overflow")
    // If the user changes workspace while a preference write is in flight, its eventual success
    // must never resend the old question in the new workspace.
    await page.locator('[data-steve-model-control="retry"]').click()
    await sheet.locator('[data-steve-model-save]').waitFor()
    delay=true
    await sheet.locator('[data-steve-model-save]').click()
    await reachedDelay
    await page.evaluate(()=>window.dispatchEvent(new Event("fixture-scope")))
    await sheet.waitFor({state:"hidden"})
    pending()
    assert.equal((await page.evaluate(()=>window.__say)).length,1)
    await page.close()
  }
  assert.deepEqual(errors,[])
  assert.deepEqual(unexpected,[])
  console.log("model recovery: 8 width/theme/language combinations passed; no real model requests")
} finally {await browser.close()}
