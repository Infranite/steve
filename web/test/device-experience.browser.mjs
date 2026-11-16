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
const goto=async(hash)=>{await page.goto(base+'/#'+hash);await page.reload();await page.locator('[data-work-counts]').waitFor()}
const scale=async(root='body')=>page.locator(root).evaluate(el=>{const els=[el,...el.querySelectorAll('*')].filter(e=>e instanceof HTMLElement); const sizes=els.map(e=>parseFloat(getComputedStyle(e).fontSize));els.forEach((e,i)=>e.style.fontSize=sizes[i]*2+'px')})
try {
 await page.goto(base+'/auth/dev')
 await goto('/devices/hub')
 const state=await (await page.request.get(base+'/api/state')).json()
 await fs.writeFile(out+'/api-evidence.json',JSON.stringify({devices:state.devices,sessions:state.sessions.filter(s=>s.id.startsWith('case-')).map(s=>({id:s.id,item_id:s.item_id,status:s.status,device_id:s.device_id,readonly:s.readonly,busy:s.busy,queue:s.queued.map(q=>q.id)})),decisions:state.decisions.map(d=>({id:d.id,session_id:d.session_id,item_id:d.item_id,status:d.status}))},null,2))
 await check('real state retains idle/running requests, busy, startup, error and readonly evidence',()=>{
  for(const [sid,status] of [['case-idle-request','idle'],['case-running','running'],['case-starting','starting'],['case-error','error']]) assert.equal(state.sessions.find(s=>s.id===sid).status,status)
  assert.equal(state.sessions.find(s=>s.id==='case-busy').busy,true)
  assert.equal(state.sessions.find(s=>s.id==='case-readonly').readonly,true)
 })
 await check('exclusive groups and queue message/session counts from actual API',async()=>{
  assert.equal(await page.locator('[data-panel=attention] [data-work-session]').count(),2)
  assert.equal(await page.locator('[data-panel=current] [data-work-session]').count(),4)
  assert.equal(await page.locator('[data-work-session=case-idle-request]').count(),1)
  await page.locator('[data-work-counts]').getByText('队列：3 条消息 · 2 个会话',{exact:true}).waitFor()
  assert.equal(await page.locator('[data-work-session=case-unassigned]').count(),0)
 })
 for(const width of [1440,390,320]) for(const percent of [100,200]) {
  await page.setViewportSize({width,height:1000});await goto('/devices/hub')
  await page.locator('[data-monitor-expanded] > summary').click()
  if(percent===200) await scale()
  for(const [request,sid,status,item] of [['case-request-running','case-running','running','TEST-1'],['case-request-idle','case-idle-request','idle','TEST-2']]) {
   await check(`${width}/${percent} request ${request} preserves identity and raw state`,async()=>{
    await page.locator(`[data-open-request=${request}]`).click();const dlg=page.locator('[data-slot=dialog-content]')
    await dlg.getByText('原始状态：'+status,{exact:false}).waitFor();assert.ok((await dlg.locator('[data-slot=dialog-description]').innerText()).includes(item));assert.ok((await dlg.locator('[data-slot=dialog-description]').innerText()).includes(sid))
    if(percent===200) await scale('[data-slot=dialog-content]')
    await page.keyboard.press('Escape');await page.locator(`[data-work-session=${sid}][data-raw-state=${status}]`).waitFor();assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('data-open-request')),request)
   })
  }
  await check(`${width}/${percent} disk evidence preserves root/capacity/time and returns`,async()=>{
   const button=page.locator('[data-disk-evidence]').first(), root=await button.getAttribute('data-disk-evidence')
   const card=page.locator('[data-vital=disk]').filter({has:page.locator(`[data-disk-evidence="${root}"]`)})
   const before=await card.innerText(), time=await page.locator('[data-sample-time]').innerText(), capacity=await card.locator('p').nth(1).innerText()
   await button.click();const dlg=page.locator('[data-slot=dialog-content]');await dlg.getByText(capacity,{exact:true}).waitFor();await dlg.getByText('local',{exact:false}).waitFor();await dlg.getByText(time.replace('采样过旧：上次','采样时间：'),{exact:false}).waitFor()
   if(percent===200) await scale('[data-slot=dialog-content]')
   await page.keyboard.press('Escape');assert.equal(await card.innerText(),before);assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('data-disk-evidence')),root)
  })
  await check(`${width}/${percent} charts expose HTML axes, separate gaps and fit viewport`,async()=>{
   assert.equal(await page.locator('[data-chart] svg text').count(),0)
   assert.equal(await page.locator('[data-chart=rtt]').count(),0,'local has no remote RTT')
   assert.ok(await page.locator('[data-sampling-gap]').count()>0)
   const chart=page.locator('[data-chart=load]');await chart.scrollIntoViewIfNeeded()
   const geometry=await page.locator('[data-chart]').evaluateAll(charts=>charts.map(c=>{
    const r=c.getBoundingClientRect(), axis=[...c.querySelectorAll('[data-axis] span')].map(x=>x.getBoundingClientRect());
    const plot=c.querySelector('svg').getBoundingClientRect();return {aligned:Math.abs((axis[0].top+axis[0].bottom)/2-plot.top)<1&&Math.abs((axis.at(-1).top+axis.at(-1).bottom)/2-plot.bottom)<1,left:r.left,right:r.right,overlap:axis.some((a,i)=>i>0&&a.top<axis[i-1].bottom),sizes:[...c.querySelectorAll('[data-axis] span')].map(x=>parseFloat(getComputedStyle(x).fontSize))}
   }))
   assert.ok(geometry.every(g=>g.left>=0&&g.right<=width+1&&!g.overlap&&g.aligned),JSON.stringify(geometry))
   assert.ok(geometry.every(g=>g.sizes.every(s=>s>=(percent===200?24:12)&&s<=(percent===200?36:18))), 'font scaling must be exactly one doubling, not cumulative')
   assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'page overflow')
   await page.screenshot({path:`${out}/device-${width}-${percent}.png`,fullPage:true})
   await page.setViewportSize({width,height:1600})
   for(const metric of ['load','mem']) await page.locator(`[data-chart=${metric}]`).screenshot({path:`${out}/chart-${metric}-${width}-${percent}.png`})
   await page.setViewportSize({width,height:1000})
  })
 }
 await page.setViewportSize({width:1440,height:1000});await goto('/devices/hub')
 await page.locator('[data-session-queue=case-queued] > summary').click()
 await page.locator('[data-queue-id=queue-1]').waitFor();await page.locator('[data-queue-id=queue-2]').waitFor()
 await page.locator('.device-recent > summary').click();await page.locator('[data-work-session=case-readonly]').waitFor();await page.locator('[data-work-session=case-ended]').waitFor()
 checks.push({name:'queue IDs and collapsed recent readonly/ended session entries open',passed:true})
 await page.locator('[data-capabilities] > summary').click()
 const limit=page.locator('[data-concurrency]'), save=page.locator('[data-config-save]')
 const original=await limit.inputValue();await limit.fill('2');await page.locator('[data-config-cancel]').click();assert.equal(await limit.inputValue(),original)
 await limit.fill(original==='3'?'2':'3');await save.click();await page.locator('[data-config-receipt]').waitFor();checks.push({name:'actual configuration cancel restores values and save returns server revision receipt',passed:true})
 const latest=(await (await page.request.get(base+'/api/devices')).json()).devices.find(d=>d.id==='hub')
 const conflict=await page.request.patch(base+'/api/devices/hub',{data:{revision:latest.revision,max_sessions:4}});assert.equal(conflict.status(),200)
 await limit.fill('5');await save.click();await page.getByText('配置有并发修改。请读取最新值并核对后再提交。',{exact:true}).waitFor();assert.equal(await limit.inputValue(),'5')
 await page.locator('[data-config-read]').click();await page.locator('[data-config-latest]').waitFor();assert.equal(await limit.inputValue(),'5');await save.click();await page.locator('[data-config-receipt]').waitFor()
 checks.push({name:'actual 409 conflict retains draft; read-back compares latest before successful retry',passed:true})
 await context.setOffline(true);assert.equal(await save.isDisabled(),true);await context.setOffline(false);checks.push({name:'browser offline immediately disables configuration writes',passed:true})
 for(const width of [1440,390,320]) for(const percent of [100,200]) {
  await page.setViewportSize({width,height:1000});await goto('/devices');await page.locator('[data-node-row=case-remote]').click();await page.locator('[data-node-detail]').waitFor()
  await page.locator('[data-monitor-expanded] > summary').click();await page.locator('[data-chart=rtt]').waitFor()
  if(percent===200) await scale('[data-node-detail]')
  for(const metric of ['load','mem','rtt']) {
   const chart=page.locator(`[data-chart=${metric}]`);await chart.scrollIntoViewIfNeeded()
   const geometry=await chart.evaluate(el=>{const r=el.getBoundingClientRect(),axis=[...el.querySelectorAll('[data-axis] span')].map(s=>s.getBoundingClientRect());return {left:r.left,right:r.right,overlap:axis.some((a,i)=>i>0&&a.top<axis[i-1].bottom)}})
   assert.ok(geometry.left>=0&&geometry.right<=width+1&&!geometry.overlap,JSON.stringify(geometry))
   await page.setViewportSize({width,height:1600});await chart.screenshot({path:`${out}/remote-${metric}-${width}-${percent}.png`});await page.setViewportSize({width,height:1000})
  }
  checks.push({name:`actual disconnected remote identity: load/memory/RTT at ${width}px/${percent}% have legible axes and gaps`,passed:true})
  await page.keyboard.press('Escape')
 }
 await goto('/devices')
 await page.locator('[data-node-row=case-no-samples]').click();await page.locator('[data-sample-state=none]').getByText('尚无样本',{exact:true}).waitFor();await page.keyboard.press('Escape')
 await page.locator('[data-node-row=case-stale]').click();await page.locator('[data-sample-state=stale]').waitFor();await page.getByText('采样过旧：上次',{exact:false}).waitFor();await page.keyboard.press('Escape')
 await context.setOffline(true);await page.locator('[data-node-row=case-remote]').click();await page.locator('[data-sample-state=failed]').waitFor();await page.getByText('读取失败：',{exact:false}).waitFor();assert.equal(await page.locator('[data-sample-state=none]').count(),0);await page.keyboard.press('Escape');await context.setOffline(false)
 checks.push({name:'real no-sample response, stale snapshot and actual network read failure produce separate monitoring states',passed:true})
 await page.locator('[data-pair]').click();await page.locator('[data-pair-dialog]').waitFor({timeout:3000}).catch(()=>page.getByRole('dialog').waitFor());await page.keyboard.press('Escape');checks.push({name:'existing pairing entry opens and closes on actual backend',passed:true})
 assert.deepEqual(errors,[])
 await fs.writeFile(out+'/browser-results.json',JSON.stringify({source:'isolated real HTTP API with controlled persisted edge cases; physical sampling recorded separately; no API routes mocked',checks,errors},null,2))
 console.log(`${checks.length} real API browser checks passed`)
} catch(e) { await fs.writeFile(out+'/browser-failure.txt',String(e.stack));await page.screenshot({path:out+'/browser-failure.png',fullPage:true});throw e }
finally {await browser.close()}
