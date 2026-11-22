// Protocol/UI fixture: real Web and localhost HTTP, no Pi process or physical remote Node.
import assert from 'node:assert/strict'
import http from 'node:http'
import { mkdir, writeFile } from 'node:fs/promises'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:19714'
const port = Number(process.env.STEVE_TEST_API_PORT || 19715)
const before = process.env.STEVE_SCOPE_BEFORE === '1'
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const ws = { id: 'fixture', name: 'Scope fixture', description: '', item_prefix: 'ST', sort: 0, created: 0 }
const device = { id: 'hub', name: 'Fixture Hub', roles: ['Hub'], node_id: 'local', connected: true, can_configure: true, revision: 0, os: 'fixture', version: 'fixture', status: '正常', max_sessions: 4, seen: 0, created: 0 }
const runtime = state => ({ id: 'registry-fixture', name: 'Registry fixture', ok: false, models: [], version: '', path: '', modes: state === 'unavailable' ? [] : ['full_access'], error: `Local probe: ${state}`, setup: { state, can_install: state === 'not_installed', reason: `Local probe: ${state}`, help: 'Review local Node setup', components: [] } })
const bot = (id = 'scope-bot') => ({ id, name: id, role: 'Fixture Bot', main: false, harness: 'registry-fixture', model: 'existing-target-model', rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'bypassPermissions', avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['fixture'], memories: 0 })
let f
const requests = [], results = []
const view = () => ({ steve: { bot_id: 'scope-bot', name: 'scope-bot', harness: 'registry-fixture', runs_on: 'registry-fixture', model: null, models: [], effort: null, note: null }, max: 3,
  purposes: [{ key: 'chat', label: 'Chat', desc: 'Fixture purpose', follows: false, value: f.choice, effective: f.choice, recent: null, recommended: null }],
  agents: [{ harness: 'registry-fixture', name: 'Registry fixture', models: [], default: null, value: f.agent }],
  // This server-side purpose allowlist is independent of local runtime readiness.
  harnesses: [{ id: 'registry-fixture', name: 'Registry fixture', efforts: [], default_models: [] }] })
const state = () => ({ version: 'fixture', web_version: 'fixture', owner: false, workspace: ws, workspaces: [ws], items: f.items, decisions: [], recent: [], bots: f.bots, allBots: f.bots.map(b => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: { onboard: '1' }, sessions: [], devices: [device], runtimes: [f.runtime, { ...f.runtime, id: 'not-for-purpose', name: 'Excluded purpose' }] })
const server = http.createServer(async (req, res) => {
  try {
    const u = new URL(req.url, 'http://fixture'), p = u.pathname
    let bytes = ''; for await (const chunk of req) bytes += chunk
    const body = bytes ? JSON.parse(bytes) : null
    requests.push({ scenario: f?.scenario, method: req.method, path: req.url, body })
    let data
    if (p === '/auth/hello') data = { name: 'steve', version: 'fixture', api: 1, login: { token: false, oidc: false } }
    else if (p === '/api/shell') data = state()
    else if (p === '/api/devices') data = { devices: [device], runtimes: [f.runtime] }
    else if (p === '/api/nodes') data = { nodes: [] } // No linked/name eligibility inference.
    else if (p === '/api/vitals') data = { nodes: {} }
    else if (p === '/api/node-tags') data = { tags: {} }
    else if (p === '/api/settings/models') {
      if (req.method === 'PATCH') {
        if (body.purposes?.chat) f.choice = body.purposes.chat
        if (body.agents) f.agent = body.agents['registry-fixture']
      }
      data = view()
    } else if (p === '/api/system/models') {
      res.writeHead(503, { 'content-type': 'application/json' }); res.end(JSON.stringify({ error: 'Local catalog unavailable' })); return
    } else if (p === '/api/bots' && req.method === 'POST') { const b = { ...bot('created-bot'), ...body }; f.bots.push(b); data = b }
    else if (p.startsWith('/api/bots/') && req.method === 'PATCH') { const b = f.bots.find(b => b.id === p.split('/').at(-1)); Object.assign(b, body); data = b }
    else if (p === '/api/items' && req.method === 'POST') {
      // Only acknowledge the submitted protocol. A proposal has no execution/session at all.
      assert.ok(['start', 'propose'].includes(body.mode))
      const item = { id: `ST-${900 + f.items.length}`, workspace_id: 'fixture', no: 900 + f.items.length, title: body.goal, goal: body.goal, status: body.mode === 'propose' ? 'wait' : 'queued', stage: '', bot_id: body.bot || 'scope-bot', project_id: body.project || null, cwd: '', branch: null, plan: [], now: 'HTTP fixture receipt only', accept_mode: null, priority: '中', archived: false, star: false, due: null, snooze: null, source: ['web', ''], parent: null, created: 0, updated: 0, goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: false }
      f.items.push(item); data = item
    } else if (p.startsWith('/api/items/ST-')) data = { ...f.items.find(i => i.id === p.split('/')[3]), goalsList: [], timeline: [], artifacts: [], decisions: [], dependents: [] }
    else if (p.endsWith('/events')) data = []
    else { res.writeHead(404, { 'content-type': 'application/json' }); res.end('{}'); return }
    res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(data))
  } catch (e) { res.writeHead(500); res.end(JSON.stringify({ error: e.message })); console.error(e) }
})
await new Promise(resolve => server.listen(port, '127.0.0.1', resolve))
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const fits = async p => assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no horizontal overflow')
const shot = async (p, name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }
const mutations = path => requests.filter(r => r.scenario === f.scenario && r.path === path && r.method !== 'GET')
try {
  for (const local of ['not_installed', 'unavailable']) for (const lang of before ? ['en'] : ['en', 'zh-CN']) {
    f = { scenario: `${local}-${lang}`, runtime: runtime(local), bots: [bot()], items: [], choice: { harness: 'registry-fixture', models: ['existing-target-model'], effort: null }, agent: null }
    const p = await browser.newPage({ viewport: { width: lang === 'en' ? 320 : 375, height: 900 }, locale: lang, reducedMotion: 'reduce' })
    p.setDefaultTimeout(12000)
    const errors = []; p.on('pageerror', e => errors.push(e.message))
    await p.addInitScript(lang => { localStorage.setItem('steve-lang', lang); localStorage.setItem('steve-onboarded', '1') }, lang)
    const result = { local, lang, scope: 'Protocol/UI fixture only; no remote execution', errors }
    const open = async hash => { await p.goto(base + '/' + hash); await p.locator('[data-new-item], [data-slot=tabs-trigger], [data-models-default]').first().waitFor() }
    const newItem = async () => { await open('#/items'); await p.locator('[data-new-item]').click(); await p.locator('#new-goal').fill('Scope regression request') }
    try {
      await newItem()
      result.submitDisabled = await p.locator('[data-testid=new-go]').isDisabled()
      await p.locator('[data-select=new-bot]').click()
      result.botOptionDisabled = await p.getByRole('option', { name: /scope-bot/ }).getAttribute('aria-disabled') === 'true'
      if (!before) { assert.equal(result.botOptionDisabled, false); await p.getByRole('option', { name: /scope-bot/ }).click() }
      else await p.keyboard.press('Escape')
      if (!before) {
        assert.equal(result.submitDisabled, false)
        await p.locator('[data-select=new-project]').click()
        await p.getByRole('option', { name: lang === 'en' ? 'No project' : '不放进项目', exact: true }).click()
        await fits(p); await shot(p, `${f.scenario}-start`)
        await p.locator('[data-testid=new-go]').click()
        await p.waitForFunction(() => location.hash.includes('/ST-'))
        assert.equal(mutations('/api/items').at(-1).body.mode, 'start')
        assert.equal(mutations('/api/items').at(-1).body.bot, 'scope-bot')
        assert.equal(mutations('/api/items').at(-1).body.project, null)
      }
      await newItem()
      await p.getByRole('radio', { name: lang === 'en' ? /Wait for my OK/ : /先等我确认/ }).click()
      result.proposeDisabled = await p.locator('[data-testid=new-go]').isDisabled()
      if (!before) {
        assert.equal(result.proposeDisabled, false)
        await p.locator('[data-testid=new-go]').click(); await p.waitForFunction(() => location.hash.includes('/ST-'))
        assert.equal(mutations('/api/items').at(-1).body.mode, 'propose')
        assert.equal(f.items.at(-1).status, 'wait'); assert.deepEqual(f.items.at(-1).sessionIds, [])
        await newItem(); await p.locator('[data-testid=new-go]').click(); await p.waitForFunction(() => location.hash.includes('/ST-'))
        const auto = mutations('/api/items').at(-1).body
        assert.equal(auto.mode, 'start'); assert.ok(auto.bot == null); assert.ok(auto.project == null)
      }
      await p.keyboard.press('Escape')
      await open('#/bots/scope-bot')
      await p.locator('[data-slot=tabs-trigger]').filter({ hasText: lang === 'en' ? /^Settings$/ : /^设置$/ }).click()
      await p.locator('[data-select=bot-harness]').click()
      result.runtimeOptionDisabled = await p.getByRole('option', { name: /Registry fixture/ }).getAttribute('aria-disabled') === 'true'
      await p.keyboard.press('Escape')
      result.modelPickerHidden = await p.locator('[data-model-picker=bot-model]').count() === 0
      result.botSaveDisabled = await p.locator('[data-save-bot]').isDisabled()
      if (!before) {
        assert.equal(result.runtimeOptionDisabled, false); assert.equal(result.modelPickerHidden, false); assert.equal(result.botSaveDisabled, false)
        assert.match(await p.locator('[data-model-picker=bot-model]').innerText(), /existing-target-model/)
        await p.locator('[data-bot-name]').fill('Saved while local unavailable')
        await p.locator('[data-model-picker=bot-model]').click()
        await p.locator('[data-model-opt=""]').waitFor()
        assert.match(await p.locator('[data-model-scope]').innerText(), lang === 'en' ? /Hub local Node/ : /Hub 本机 Node/)
        await p.locator('[data-model-other]').click(); await p.locator('[data-model-free]').fill('target/manual-model'); await p.locator('[data-model-free]').press('Enter')
        await p.locator('[data-save-bot]').click()
        await p.locator('[data-save-bot]').waitFor({ state: 'detached' })
        assert.equal(mutations('/api/bots/scope-bot').at(-1).body.model, 'target/manual-model')
        assert.equal(mutations('/api/bots/scope-bot').at(-1).body.perm_mode, 'bypassPermissions')
      }
      await shot(p, `${f.scenario}-bot`)
      await open('#/bots'); await p.locator('[data-new-bot]').click(); await p.locator('[data-choices=new-bot] [data-choice=blank]').click()
      if (!before) {
        assert.equal(await p.locator('[data-testid=bot-go]').isDisabled(), local === 'not_installed', 'known permission contract requires explicit choice; unknown keeps default')
        if (local === 'unavailable') await p.locator('[data-runtime-permission-unknown]').waitFor()
      }
      // Select required full_access explicitly; never infer or upgrade it automatically.
      if (!before || f.runtime.modes.length) await p.locator('[data-select=new-bot-perm]').click()
      const full = p.getByRole('option', { name: lang === 'en' ? /Never ask/ : /不再询问/ })
      if (await full.count()) await full.click()
      result.newBotDisabled = await p.locator('[data-testid=bot-go]').isDisabled()
      if (!before) {
        assert.equal(result.newBotDisabled, false)
        await p.locator('[data-testid=bot-go]').click(); await p.locator('[data-save-bot]').waitFor()
        assert.equal(mutations('/api/bots').at(-1).body.perm_mode, 'bypassPermissions')
      }
      await p.keyboard.press('Escape')
      await open('#/settings/models'); await p.locator('[data-models-advanced-toggle]').click(); if (!await p.locator('[data-purpose=chat]').isVisible()) await p.locator('[data-follows-toggle]').click(); await p.locator('[data-purpose=chat]').click(); await p.locator('[data-models-sheet]').waitFor()
      result.purposePickerHidden = await p.locator('[data-model-picker=model-chat]').count() === 0
      if (!before) {
        assert.equal(result.purposePickerHidden, false)
        await p.locator('[data-select=harness-chat]').click()
        assert.equal(await p.getByRole('option', { name: /Excluded purpose/ }).getAttribute('aria-disabled'), 'true', 'server purpose restriction survives')
        await p.getByRole('option', { name: /Registry fixture/ }).click()
        await p.locator('[data-model-picker=model-chat]').click(); await p.locator('[data-model-opt=""]').click()
        assert.equal(await p.locator('[data-models-save]').isEnabled(), true)
        await fits(p); await shot(p, `${f.scenario}-default`)
        await p.locator('[data-models-save]').click(); await p.locator('[data-models-sheet]').waitFor({ state: 'detached' })
        assert.deepEqual(mutations('/api/settings/models').at(-1).body.purposes.chat.models, [])
        await p.locator('[data-purpose=chat]').click()
        await p.locator('[data-model-picker=model-chat]').click(); await p.locator('[data-model-other]').click()
        await p.locator('[data-model-free]').fill('target/purpose-model'); await p.locator('[data-model-free]').press('Enter')
        await p.locator('[data-models-save]').click(); await p.locator('[data-models-sheet]').waitFor({ state: 'detached' })
        assert.deepEqual(mutations('/api/settings/models').at(-1).body.purposes.chat.models, ['target/purpose-model'])
        const agentSaved = p.waitForResponse(r => r.url().includes('/api/settings/models') && r.request().method() === 'PATCH')
        await p.locator('[data-model-picker=agent-registry-fixture]').click(); await p.locator('[data-model-other]').click(); await p.locator('[data-model-free]').fill('target/agent-model'); await p.locator('[data-model-free]').press('Enter')
        await agentSaved
        assert.equal(mutations('/api/settings/models').at(-1).body.agents['registry-fixture'], 'target/agent-model')
        assert.deepEqual(errors, [])
      }
      result.postedItems = mutations('/api/items').length
      results.push(result)
      console.log(JSON.stringify(result))
    } catch (e) { console.error(await p.locator('body').innerText(), errors); await shot(p, `${f.scenario}-failure`); throw e } finally { await p.close() }
  }
  if (before) assert.ok(results.every(r => r.submitDisabled && r.botOptionDisabled && r.proposeDisabled && r.modelPickerHidden && r.purposePickerHidden && r.newBotDisabled && r.postedItems === 0), 'captured pre-fix blockers')
} finally {
  if (shots) await writeFile(`${shots}/observations.json`, JSON.stringify({ before, scope: 'Protocol/UI fixture, not physical remote Pi acceptance', results, requests }, null, 2))
  await browser.close(); await new Promise(resolve => server.close(resolve))
}
