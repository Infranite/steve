import assert from 'node:assert/strict'
import { mkdir } from 'node:fs/promises'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const page = await browser.newPage({ locale: 'zh-CN' })
const errors = []; page.on('pageerror', e => errors.push(e.message))
const puts = []
let png
const attr = (id, key, value, source = 'me', status = 'confirmed') => ({id, key, label:key, value, source, source_label: source === 'me' ? '你确认' : 'Hub 用户资料', status, level:'public'})
let contact = {id:'me1', name:'Morgan', is_me:true, kind:'real', aliases:[], rels:['me'], tags:[], org:null, avatar:null, note:'Long original note remains verbatim. '.repeat(30), status:'active', visibility:'private', profile:{checked:1,next_check:null,source:null,fields:{}},jobs:[],attrs:[attr('a1','call','Morgan'),attr('a2','call_external','Morgan Smith'),attr('a3','email','morgan@example.test','profile'),attr('a4','role','Engineer','agent','candidate'),attr('a5','project','Portable tools','agent','candidate')],teams:[],merges:[],reports_to:[]}
await page.route('**/api/**', async route => {
 const u = new URL(route.request().url()), method = route.request().method()
 if (!u.pathname.startsWith('/api/')) return route.continue()
 let data = []
 if (u.pathname === '/api/me/contact' || u.pathname === '/api/contacts/me') data=contact
 else if (u.pathname === '/api/contacts/me1/avatar' && method === 'PUT') {
  puts.push(route.request().postDataBuffer());contact={...contact,avatar:{file:'avatar1'}};data=contact
 } else if (u.pathname === '/api/contacts/me1/avatar' && method === 'DELETE') {
  contact={...contact,avatar:null};data=contact
 } else if (u.pathname === '/api/files/avatar1') return route.fulfill({contentType:'image/png',body:png})
 else if (u.pathname.startsWith('/api/attrs/') && method === 'PATCH') {
  const id=u.pathname.split('/').at(-1), body=route.request().postDataJSON()
  contact={...contact, attrs:contact.attrs.map(a=>a.id===id?{...a,...body,source:body.status==='confirmed'?'me':a.source,source_label:body.status==='confirmed'?'你确认':a.source_label}:a)};data={ok:true}
 } else if (u.pathname === '/api/parties/me1/attrs' && method === 'POST') {
  const body=route.request().postDataJSON();contact={...contact,attrs:[...contact.attrs,attr('scope1',body.key,body.value)]};data={ok:true}
 } else if(u.pathname==='/api/personas') data=[{id:'persona1',name:'Project helper'}]
 else if(u.pathname==='/api/spaces') data=[{id:'space1',name:'Project room',status:'active'}]
 else if(u.pathname==='/api/accounts') data=[{id:'mail1', system:{id:'mail',name:'Email'},handle:'morgan@example.test',source_label:'Connected',source:'manual'}, {id:'mail2',system:{id:'mail',name:'Email'},handle:'work@example.test',source_label:'Connected',source:'manual'}]
 else if(u.pathname.includes('/holdings')) data={accounts:[],responsible:[]}
 return route.fulfill({contentType:'application/json',body:JSON.stringify(data)})
})
const fits = async () => assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'no horizontal overflow')
try {
 await page.goto(base+'/test/fixtures/people-management.html?me')
 await page.locator('[data-me-facts]').waitFor()
 await page.locator('[data-platform-account="mail1"]').waitFor()
 assert.equal(await page.getByText('morgan@example.test',{exact:true}).count(),1,'matching account and fact share one address row')
 assert.equal(await page.locator('[data-attr="邮箱"] [data-platform-account="mail1"]').getAttribute('href'),'#/accounts/mail1')
 assert.match(await page.locator('[data-platform-account="mail2"]').innerText(),/work@example\.test/,'a different account address remains visible')
 // Keep the original avatar contract covered while the profile layout changes.
 png=Buffer.from((await page.evaluate(()=>{const c=document.createElement('canvas');c.width=c.height=64;return c.toDataURL('image/png')})).split(',')[1],'base64')
 await page.locator('[data-avatar-input]').setInputFiles({name:'wrong.gif',mimeType:'image/gif',buffer:Buffer.from('GIF89a')})
 await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)))
 assert.equal(puts.length,0)
 assert.equal(await page.locator('[data-avatar-input]').inputValue(),'')
 await page.locator('[data-avatar-input]').setInputFiles({name:'me.png',mimeType:'image/png',buffer:png})
 await page.locator('[data-avatar-edit] img').waitFor()
 await page.waitForFunction(()=>document.querySelector('[data-avatar-edit] img')?.naturalWidth>0)
 assert.equal(puts.length,1);assert.ok(puts[0].equals(png))
 await page.locator('[data-avatar-edit]').click()
 await page.getByRole('menuitem',{name:'移除头像',exact:true}).click()
 await page.locator('[data-avatar-edit="pick"]').waitFor()
 assert.equal(await page.locator('[data-avatar-edit] img').count(),0)
 assert.equal(await page.locator('[data-fact-group]').count(),4)
 assert.equal(await page.locator('[data-original-note]').getAttribute('open'),null)
 await page.locator('[data-fact-source]').filter({hasText:'账号资料'}).first().waitFor()
 await page.locator('[data-fact-source]').filter({hasText:'推断'}).first().waitFor()
 await page.locator('[data-attr="角色"] [data-fact-confirm]').click()
 await page.locator('[data-attr="角色"] [data-fact-source]').filter({hasText:'你确认'}).waitFor()
 await page.locator('[data-attr="主力项目"] [data-fact-reject]').click()
 await page.locator('[data-attr="主力项目"]').waitFor({state:'detached'})
 await page.getByRole('button',{name:'编辑 Steve 叫你',exact:true}).click()
 await page.getByLabel('修改 Steve 叫你').fill('Mo');await page.keyboard.press('Enter')
 await page.locator('[data-value="Steve 叫你"]').filter({hasText:'Mo'}).waitFor()
 await page.locator('[data-address-overrides] summary').click()
 await page.getByLabel('选择身份或会话',{exact:true}).click()
 await page.getByRole('option',{name:'Project helper',exact:true}).click()
 await page.getByLabel('在这里介绍你的称呼',{exact:true}).fill('Morgan from Project')
 await page.locator('[data-address-overrides]').getByRole('button',{name:'保存',exact:true}).click()
 await page.locator('[data-value]').filter({hasText:'Morgan from Project'}).waitFor()
 assert.deepEqual(JSON.parse(contact.attrs.find(a=>a.key==='call_ext_scope').value),{kind:'persona',id:'persona1',name:'Morgan from Project'})
 await page.locator('[data-address-overrides]').evaluate(el => { el.open = false })
 await page.emulateMedia({ reducedMotion: 'reduce' })
 if(process.env.STEVE_TEST_SCREENSHOTS) await mkdir(process.env.STEVE_TEST_SCREENSHOTS,{recursive:true})
 for(const theme of ['light','dark']) for(const width of [1440,1066,390]) {
  await page.setViewportSize({width,height:1000})
  await page.evaluate(async theme => {
   document.documentElement.classList.toggle('dark',theme==='dark')
   const scroll = document.querySelector('[data-page-bar]')?.nextElementSibling
   if (scroll) scroll.scrollTop = 0
   await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
  },theme)
  await fits()
  assert.equal(await page.locator('h1').count(),1)
  if(process.env.STEVE_TEST_SCREENSHOTS) {
   await page.screenshot({path:`${process.env.STEVE_TEST_SCREENSHOTS}/me-${theme}-${width}.png`,animations:'disabled'})
   await page.locator('[data-fact-group="preferences"]').scrollIntoViewIfNeeded()
   await page.screenshot({path:`${process.env.STEVE_TEST_SCREENSHOTS}/me-${theme}-${width}-details.png`,animations:'disabled'})
   await page.locator('[data-address-facts]').scrollIntoViewIfNeeded()
  }
 }
 await page.locator('[data-original-note] summary').click();await page.getByText(contact.note,{exact:true}).waitFor()
 await page.goto(base+'/test/fixtures/people-management.html?proposal')
 await page.locator('[data-change-counts]').filter({hasText:'新增 1 条 · 修改 1 条 · 移除 1 条'}).waitFor()
 assert.equal(await page.locator('[data-word-diff]').count(),0)
 await page.locator('[data-long-change] summary').click();await page.locator('[data-word-diff] ins').first().waitFor();await fits()
 assert.deepEqual(errors,[])
 console.log('PASS: avatar type rejection/raw upload/removal, grouped facts, sources, candidate confirmation/rejection, inline names, folded original notes, six layouts, lazy word diff and field counts')
} finally { await browser.close() }
