// Run against an isolated real Hub with controlled persisted cases; no API interception.
import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const base=process.env.STEVE_TEST_WEB, out=process.env.STEVE_TEST_SHOTS
assert.ok(base && out, 'STEVE_TEST_WEB and STEVE_TEST_SHOTS are required')
await fs.mkdir(out,{recursive:true})
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})})
const context=await browser.newContext({locale:'zh-CN'}), page=await context.newPage(), checks=[], errors=[]
page.on('pageerror',e=>errors.push(e.message))
const check=(name, fn)=>Promise.resolve().then(fn).then(()=>checks.push({name,passed:true}))
const goto=async(hash)=>{await page.goto(base+'/#'+hash);await page.reload();await page.locator(hash.startsWith('/devices/nodes/')?'[data-node-work]':'[data-nodes]').waitFor()}
const scale=async(root='body')=>page.locator(root).evaluate(el=>{const els=[el,...el.querySelectorAll('*')].filter(e=>e instanceof HTMLElement); const sizes=els.map(e=>parseFloat(getComputedStyle(e).fontSize));els.forEach((e,i)=>e.style.fontSize=sizes[i]*2+'px')})
try {
 await page.goto(base+'/auth/dev')
 await goto('/devices/nodes/local')
 const state=await (await page.request.get(base+'/api/state')).json()
 await fs.writeFile(out+'/api-evidence.json',JSON.stringify({devices:state.devices,sessions:state.sessions.filter(s=>s.id.startsWith('case-')).map(s=>({id:s.id,item_id:s.item_id,status:s.status,device_id:s.device_id,readonly:s.readonly,busy:s.busy,queue:s.queued.map(q=>q.id)})),decisions:state.decisions.map(d=>({id:d.id,session_id:d.session_id,item_id:d.item_id,status:d.status}))},null,2))
 await check('real state retains idle/running requests, busy, startup, error and readonly evidence',()=>{
  for(const [sid,status] of [['case-idle-request','idle'],['case-running','running'],['case-starting','starting'],['case-error','error']]) assert.equal(state.sessions.find(s=>s.id===sid).status,status)
  assert.equal(state.sessions.find(s=>s.id==='case-busy').busy,true)
  assert.equal(state.sessions.find(s=>s.id==='case-readonly').readonly,true)
 })
 await check('exclusive groups from the actual API: answer-first and current rows, nothing unassigned',async()=>{
  assert.equal(await page.locator('[data-node-work] > div [data-work-session]').count(),6)
  assert.equal(await page.locator('[data-work-session=case-idle-request]').count(),1)
  assert.ok(await page.locator('[data-work-session]').first().locator('[data-open-request]').count()>0,'a session waiting for an answer comes first')
  await page.locator('[data-work-session=case-queued]').getByText('条消息在等',{exact:false}).waitFor()
  assert.equal(await page.locator('[data-work-session=case-unassigned]').count(),0)
 })
 for(const width of [1440,390,320]) for(const percent of [100,200]) {
  await page.setViewportSize({width,height:1000});await goto('/devices/nodes/local')
  if(percent===200) await scale()
  for(const request of ['case-request-running','case-request-idle']) {
   await check(`${width}/${percent} request ${request} opens its decision and returns`,async()=>{
    await page.locator(`[data-open-request=${request}]`).click();const dlg=page.locator('[data-slot=dialog-content]');await dlg.waitFor()
    if(percent===200) await scale('[data-slot=dialog-content]')
    await page.keyboard.press('Escape');await dlg.waitFor({state:'detached'})
   })
  }
  await check(`${width}/${percent} figures and the day's charts fit the viewport`,async()=>{
   await page.locator('[data-vital=disk]').getByText('剩',{exact:false}).waitFor()
   assert.equal(await page.locator('[data-vital=rtt]').count(),0,'local has no remote RTT')
   assert.equal(await page.locator('[data-node-trends] .ui-kpic').count(),2,'load and memory; no RTT for the Hub\'s own Node')
   const boxes=await page.locator('[data-node-trends] .ui-kpic').evaluateAll(cs=>cs.map(c=>{const r=c.getBoundingClientRect();return {left:r.left,right:r.right}}))
   assert.ok(boxes.every(b=>b.left>=0&&b.right<=width+1),JSON.stringify(boxes))
   assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'page overflow')
   await page.screenshot({path:`${out}/device-${width}-${percent}.png`,fullPage:true})
  })
 }
 await page.setViewportSize({width:1440,height:1000});await goto('/devices/nodes/local')
 await page.locator('[data-work-recent] > summary').click();await page.locator('[data-work-session=case-readonly]').waitFor();await page.locator('[data-work-session=case-ended]').waitFor()
 checks.push({name:'collapsed recent readonly/ended session entries open',passed:true})
 const limit=page.locator('[data-session-limit-input]'), original=Number(await limit.getAttribute('aria-valuenow'))
 await limit.fill(String(original===3?2:3));await limit.press('Enter');await page.locator('[data-session-limit-save]').click()
 await page.locator(`[data-session-limit="${original===3?2:3}"]`).waitFor();checks.push({name:'the limit is saved through the device row with its revision',passed:true})
 const latest=(await (await page.request.get(base+'/api/devices')).json()).devices.find(d=>d.node_id==='local')
 const conflict=await page.request.patch(base+'/api/devices/'+latest.id,{data:{revision:latest.revision,max_sessions:4}});assert.equal(conflict.status(),200)
 await limit.fill('5');await limit.press('Enter');await page.locator('[data-session-limit-save]').click();await page.locator('[data-config-conflict]').waitFor()
 await page.locator('[data-config-latest]').click();await page.locator('[data-config-conflict]').waitFor({state:'detached'});await page.locator('[data-session-limit="4"]').waitFor()
 checks.push({name:'a change made elsewhere is said in place, never written over; "use the latest" reads it',passed:true})
 for(const width of [1440,390,320]) for(const percent of [100,200]) {
  await page.setViewportSize({width,height:1000});await goto('/devices/nodes');await page.locator('[data-node-row=case-remote]').click();await page.locator('[data-node-detail]').waitFor()
  if(percent===200) await scale('[data-node-detail]')
  await page.locator('[data-node-trends]').waitFor()
  assert.equal(await page.locator('[data-node-trends] .ui-kpic').count(),3,'a remote Node also charts its round trip')
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'page overflow')
  await page.screenshot({path:`${out}/remote-${width}-${percent}.png`,fullPage:true})
  checks.push({name:`actual remote identity: load/memory/RTT at ${width}px/${percent}% fit`,passed:true})
 }
 await goto('/devices/nodes')
 await page.locator('[data-node-row=case-no-samples]').click();await page.locator('[data-sample-state=none]').waitFor();await page.locator('[data-back]').click()
 await page.locator('[data-node-row=case-stale]').click();await page.locator('[data-vitals-stale]').getByText('数据停在',{exact:false}).waitFor();await page.locator('[data-back]').click()
 checks.push({name:'real no-sample response and stale snapshot produce separate monitoring states',passed:true})
 await page.locator('[data-pair]').click();await page.locator('[data-pair-dialog]').waitFor({timeout:3000}).catch(()=>page.getByRole('dialog').waitFor());await page.keyboard.press('Escape');checks.push({name:'existing pairing entry opens and closes on actual backend',passed:true})
 assert.deepEqual(errors,[])
 await fs.writeFile(out+'/browser-results.json',JSON.stringify({source:'isolated real HTTP API with controlled persisted edge cases; physical sampling recorded separately; no API routes mocked',checks,errors},null,2))
 console.log(`${checks.length} real API browser checks passed`)
} catch(e) { await fs.writeFile(out+'/browser-failure.txt',String(e.stack));await page.screenshot({path:out+'/browser-failure.png',fullPage:true});throw e }
finally {await browser.close()}
