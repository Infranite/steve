import assert from 'node:assert/strict'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const page = await browser.newPage({ viewport: { width: 390, height: 844 } })
page.setDefaultTimeout(10000)

const errors = []; page.on('pageerror', e => errors.push(e.message))
let pending = Array.from({length: 125}, (_, i) => ({ id: 'p'+i, name: '用户'+i, label: '用户'+i, nameless: i < 120, aliases: [], rels: [], tags: [], accounts: 1, items: 0, kind: 'unconfirmed', org: null, avatar: null }))
let fail = true, orgFail = true, accounts = []
let org = { id: 'o1', name: '旧组织', name_state: 'workspace', name_reason: 'permission', name_scope: 'tenant:tenant:readonly', kind: 'self', domains: [], aliases: [], tags: [], note: '', attrs: [], people: [], revision: 1 }
let contact = { id:'c1', name:'同事', kind:'real', aliases:[], rels:[], tags:[], org:null, avatar:null, note:'', status:'active', visibility:'workspace', profile:{checked:1,next_check:null,source:'chat',fields:{name:{state:'known'},org:{state:'missing',reason:'pending'},title:{state:'missing',reason:'external'},email:{state:'missing',reason:'permission',scope:'contact:user.email:readonly'}}},jobs:[], attrs:[], teams:[], merges:[], reports_to:[] }
await page.route('**/*', async route => {
 const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
 let data = [], status = 200; const method = route.request().method(), body = method === 'PATCH' || method === 'POST' ? route.request().postDataJSON() : null
 if (u.pathname === '/api/merges') data = {proposals:[],unconfirmed:pending}
 else if (u.pathname.startsWith('/api/contacts/p') && method === 'PATCH') { const id = u.pathname.split('/').at(-1); if (id === 'p0' && fail) { status=500; data={error:'模拟失败'}; fail=false } else { pending=pending.filter(x=>x.id!==id); data={ok:true} } }
 else if (u.pathname === '/api/me/contact') data = contact
 else if (u.pathname === '/api/orgs/o1') { if(method==='PATCH') { if(orgFail) {orgFail=false; status=500; data={error:'模拟保存失败'} } else {assert.equal(body.kind,undefined); org={...org,...body}; data=org} } else data=org }
 else if (u.pathname === '/api/accounts') { if(method==='POST') {accounts.push({id:'a1',system:{id:'mail',name:'邮件'},display_name:'',source_label:'手动',...body});contact.profile.fields.email={state:'known'};data=accounts[0]} else data=accounts }
 else if (u.pathname === '/api/people/c1/holdings') data={accounts:[],responsible:[]}
 else if (u.pathname === '/api/orgs') data=[org]
 else if (u.pathname === '/api/contacts/c1') { if(method==='PATCH') contact={...contact,...body}; data=contact }
 else if (u.pathname === '/api/contacts/c1/jobs') {contact={...contact,jobs:[{...body,org_name:org.name,to:null}]};data=contact}
 return route.fulfill({status,contentType:'application/json',body:JSON.stringify(data)})
})
const fits = async () => assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true)
try {
 await page.goto(base+'/test/fixtures/people-management.html'); await page.getByRole('button',{name:'全选筛选结果（125）'}).waitFor()
 await fits()
 await page.getByLabel('联系人筛选',{exact:true}).selectOption('nameless')
 await page.getByRole('button',{name:'全选筛选结果（120）'}).click()
 await page.getByRole('button',{name:'忽略所选（120）'}).click()
 await page.getByRole('alert').filter({hasText:'1 个操作失败'}).waitFor()
 assert.equal(pending.length,6)
 await page.getByRole('button',{name:'重试失败项',exact:true}).click()
 await page.getByText('没有匹配的联系人',{exact:true}).waitFor(); assert.equal(pending.length,5)
 await page.getByLabel('联系人筛选',{exact:true}).selectOption('all')
 await page.getByRole('button',{name:'全选筛选结果（5）'}).click(); await page.getByRole('button',{name:'取消选择',exact:true}).click()
 assert.equal(await page.getByRole('button',{name:'忽略所选（0）'}).isDisabled(),true)
 await page.getByLabel('搜索待确认联系人').fill('no-match'); await page.getByText('没有匹配的联系人',{exact:true}).waitFor()
 await page.goto(base+'/test/fixtures/people-management.html?org'); await page.getByRole('button',{name:'编辑组织',exact:true}).click()
 assert.equal(await page.getByLabel('组织类型',{exact:true}).isDisabled(),true)
 await page.getByLabel('组织名称',{exact:true}).fill('新组织'); await page.getByRole('button',{name:'保存',exact:true}).click()
 await page.waitForTimeout(200); assert.equal(await page.getByLabel('组织名称',{exact:true}).inputValue(),'新组织')
 await page.getByRole('button',{name:'保存',exact:true}).click(); await page.getByRole('heading',{name:'新组织',exact:true}).waitFor(); await fits()
 await page.goto(base+'/test/fixtures/people-management.html?contact'); await page.getByText('飞书不向应用提供外部联系人的此项资料',{exact:true}).waitFor(); await page.getByRole('button',{name:'编辑资料',exact:true}).click()
 await page.getByLabel('名字',{exact:true}).fill('新名字'); await page.getByRole('button',{name:'保存',exact:true}).click(); await page.getByRole('heading',{name:'新名字',exact:true}).waitFor()
 await page.getByRole('button',{name:'编辑任职'}).click(); await page.getByLabel('任职组织').selectOption('o1'); await page.getByLabel('部门',{exact:true}).fill('研发部'); await page.getByLabel('职位',{exact:true}).fill('工程师'); await page.getByRole('button',{name:'保存',exact:true}).click(); await page.getByText('新组织 · 研发部 · 工程师',{exact:true}).waitFor(); await fits()
 await page.getByRole('button',{name:'添加邮箱',exact:true}).click(); await page.getByLabel('邮箱地址').fill('colleague@example.com'); await page.getByRole('button',{name:'保存',exact:true}).click(); await page.getByText('colleague@example.com',{exact:true}).waitFor(); assert.equal(contact.profile.fields.email.state,'known'); await fits()
 await page.setViewportSize({width:1280,height:900}); await fits()
 assert.deepEqual(errors,[]); console.log('PASS: 125 contacts, filtered selection, partial failure/retry, zero results, org save retry/self kind, contact edit/jobs, 390px/1280px layout')
} catch(e) { console.error(await page.locator("body").innerText(),errors); throw e } finally {await browser.close()}
