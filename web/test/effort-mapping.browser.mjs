import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
import { sessionView, snapshotViews, viewsResponse } from './fixtures/wire-views.ts'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* Verifies Steve's canonical 5-level reasoning effort UX across all three mapping strategies:
 *  1) acp_option (Codex 6-level: high -> xhigh, max -> ultra)
 *  2) unsupported (Antigravity gemini-pro-orchestrator-neon: explicit levels greyed out + model-decides note)
 *  3) model_variant (sibling-suffixed family: max -> gemini-4-neon-xhigh)
 *  4) Session view & panel showing recorded effort_mapping + transcript system event */
const mapAcp6 = [
  { requested: 'default', effective: 'default', strategy: 'acp_option', native_effort: null, target_model: null, supported: true },
  { requested: 'low', effective: 'low', strategy: 'acp_option', native_effort: 'low', target_model: null, supported: true },
  { requested: 'medium', effective: 'medium', strategy: 'acp_option', native_effort: 'medium', target_model: null, supported: true },
  { requested: 'high', effective: 'high', strategy: 'acp_option', native_effort: 'xhigh', target_model: null, supported: true },
  { requested: 'max', effective: 'max', strategy: 'acp_option', native_effort: 'ultra', target_model: null, supported: true },
]
const mapUnsupported = [
  { requested: 'default', effective: 'default', strategy: 'unsupported', native_effort: null, target_model: null, supported: true },
  { requested: 'low', effective: 'default', strategy: 'unsupported', native_effort: null, target_model: null, supported: false },
  { requested: 'medium', effective: 'default', strategy: 'unsupported', native_effort: null, target_model: null, supported: false },
  { requested: 'high', effective: 'default', strategy: 'unsupported', native_effort: null, target_model: null, supported: false },
  { requested: 'max', effective: 'default', strategy: 'unsupported', native_effort: null, target_model: null, supported: false },
]
const mapVariant = [
  { requested: 'default', effective: 'default', strategy: 'model_variant', native_effort: null, target_model: 'gemini-4-neon', supported: true },
  { requested: 'low', effective: 'low', strategy: 'model_variant', native_effort: null, target_model: 'gemini-4-neon-low', supported: true },
  { requested: 'medium', effective: 'medium', strategy: 'model_variant', native_effort: null, target_model: 'gemini-4-neon-medium', supported: true },
  { requested: 'high', effective: 'high', strategy: 'model_variant', native_effort: null, target_model: 'gemini-4-neon-high', supported: true },
  { requested: 'max', effective: 'max', strategy: 'model_variant', native_effort: null, target_model: 'gemini-4-neon-xhigh', supported: true },
]

const bots = [
  { id: 'steve', name: 'Steve', role: '工作空间协调者', main: true, harness: 'codex', model: 'gpt-6.2-sol', reasoning_effort: 'high', fallback_surfaces: [], rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'ask', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'] },
  { id: 'dev', name: '开发', role: '代码开发', main: false, harness: 'codex', model: 'gpt-6.2-sol', reasoning_effort: 'default', fallback_surfaces: [], rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'ask', avatar: { color: '#14b8a6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'] },
]
const runtimes = [
  { id: 'codex', name: 'Codex', ok: true, version: '0.170.0', path: '/usr/bin/codex', models: ['gpt-6.2-sol'], default_model: 'gpt-6.2-sol', efforts: ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'], effort_mode: 'acp_option', effort_map: mapAcp6, modes: ['plan', 'ask', 'auto', 'full_access'], note: '' },
  { id: 'antigravity', name: 'Antigravity', ok: true, version: '1.5.17', path: '/usr/bin/agy', models: ['gemini-pro-orchestrator-neon', 'gemini-4-neon'], default_model: 'gemini-pro-orchestrator-neon', efforts: [], effort_mode: 'acp_option', effort_map: mapUnsupported, modes: ['plan', 'ask', 'auto', 'full_access'], note: '' },
]
const catalogs = {
  codex: {
    harness: 'codex', name: 'Codex', source: 'adapter', default: 'gpt-6.2-sol',
    efforts: ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'], effort_mode: 'acp_option', effort_map: mapAcp6,
    families: ['GPT'],
    models: [{ id: 'gpt-6.2-sol', label: 'gpt-6.2-sol', family: 'GPT', via: null, note: null, efforts: ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'], effort_map: mapAcp6 }],
  },
  antigravity: {
    harness: 'antigravity', name: 'Antigravity', source: 'adapter', default: 'gemini-pro-orchestrator-neon',
    efforts: [], effort_mode: 'acp_option', effort_map: mapUnsupported,
    families: ['Gemini'],
    models: [
      { id: 'gemini-pro-orchestrator-neon', label: 'gemini-pro-orchestrator-neon', family: 'Gemini', via: null, note: null, efforts: [], effort_map: mapUnsupported },
      { id: 'gemini-4-neon', label: 'gemini-4-neon', family: 'Gemini', via: null, note: null, efforts: [], effort_map: mapVariant },
    ],
  },
}
const now = Date.now()
const sessions = {
  's-codex': sessionView({
    id: 's-codex', no: 1, bot_id: 'dev', harness: 'codex', model: 'gpt-6.2-sol',
    reasoning_effort: 'high', effort_mapping: mapAcp6[3],
    cwd: '/tmp/work', device_id: 'hub', node_id: 'local', title: 'Codex 映射验证',
    source: 'Steve', status: 'idle', created: now - 60000, updated: now - 5000,
  }),
  's-agy': sessionView({
    id: 's-agy', no: 2, bot_id: 'steve', harness: 'antigravity', model: 'gemini-pro-orchestrator-neon',
    reasoning_effort: 'high', effort_mapping: mapUnsupported[3],
    cwd: '/tmp/work', device_id: 'hub', node_id: 'local', title: 'Antigravity 降级验证',
    source: 'Steve', status: 'idle', created: now - 60000, updated: now - 5000,
  }),
}
const transcripts = {
  's-codex': {
    events: [
      { seq: 1, ts: now - 55000, kind: 'system', data: { code: 'effort_mapped', text: '思考强度：高 → codex xhigh', effort_mapping: mapAcp6[3] } },
      { seq: 2, ts: now - 50000, kind: 'user', data: { text: '检查构建状态', via: 'me' } },
      { seq: 3, ts: now - 40000, kind: 'text', data: { role: 'assistant', text: '已确认一切正常。' } },
      { seq: 4, ts: now - 39000, kind: 'turn_end', data: { stopReason: 'end_turn' } },
    ],
    next: null,
  },
  's-agy': {
    events: [
      { seq: 1, ts: now - 55000, kind: 'system', data: { code: 'effort_mapped', text: '思考强度：高 → 由模型自己决定', effort_mapping: mapUnsupported[3] } },
      { seq: 2, ts: now - 50000, kind: 'user', data: { text: '切换模型后继续', via: 'me' } },
      { seq: 3, ts: now - 40000, kind: 'text', data: { role: 'assistant', text: '会话正常运行，按模型默认思考强度处理。' } },
      { seq: 4, ts: now - 39000, kind: 'turn_end', data: { stopReason: 'end_turn' } },
    ],
    next: null,
  },
}

const shell = () => ({
  version: 'fixture', web_version: 'fixture', owner: true,
  workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 },
  workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }],
  bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })),
  projects: [], tags: [], settings: {},
  harnessNames: { codex: 'Codex', antigravity: 'Antigravity' },
})

const patches = []
const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, colorScheme: 'light', reducedMotion: 'reduce' })
page.setDefaultTimeout(10000)
const errors = []
page.on('pageerror', (e) => errors.push(e.message))

await page.route('**/*', async (route) => {
  const u = new URL(route.request().url())
  if (u.origin !== base) return route.abort()
  if (u.pathname === '/auth/hello') return route.fulfill({ json: { name: 'steve', version: 'fixture', api: 2, login: { token: false, oidc: false } } })
  if (!u.pathname.startsWith('/api/')) return route.continue()
  const method = route.request().method(), p = u.pathname
  let data = {}
  if (p === '/api/shell') data = shell()
  else if (p === '/api/runtimes') data = runtimes
  else if (p === '/api/snapshot') data = snapshotViews({ item: [], session: Object.values(sessions), decision: [] })
  else if (p === '/api/views') data = viewsResponse(Object.values(sessions))
  else if (p === '/api/summary') data = { ws: 'default', workspace_name: '默认', thinking: false, hold: null }
  else if (p === '/api/devices') data = { devices: [{ id: 'hub', name: '本机', node_id: 'local', roles: ['Hub'], os: 'macOS', version: 'fixture', status: 'online', max_sessions: 8, seen: now, created: 0 }] }
  else if (p === '/api/system/models') data = catalogs[u.searchParams.get('harness')] ?? catalogs.codex
  else if (p === '/api/bot-teams') data = { default: null, editable: true, teams: [] }
  else if (/^\/api\/bots\/[^/]+\/execution-nodes$/.test(p)) data = { editable: true, bindings: [], candidates: [], unavailable_bindings: 0 }
  else if (/^\/api\/bots\/[^/]+$/.test(p) && method === 'PATCH') {
    const id = p.split('/').pop(), body = route.request().postDataJSON()
    patches.push({ id, ...body })
    const b = bots.find((x) => x.id === id)
    Object.assign(b, body)
    data = b
  }
  else if (/^\/api\/sessions\/[^/]+\/events$/.test(p)) {
    const sid = p.split('/')[3]
    data = transcripts[sid] ?? { events: [], next: null }
  }
  else if (/^\/api\/sessions\/[^/]+\/queued$/.test(p)) data = []
  else if (/^\/api\/sessions\/[^/]+\/resume$/.test(p)) data = { harness: 'codex', id: null, cwd: '/tmp/work', cwd_exists: true, command: null, transcript: null }
  else if (p === '/api/kb/uses') data = { hits: [] }
  else if (p.startsWith('/api/stats/')) return route.fulfill({ status: 404, json: {} })
  else return route.fulfill({ status: 404, json: {} })
  return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
})

const shot = async (name) => {
  if (!shots) return
  await page.waitForTimeout(80)
  await page.screenshot({ path: `${shots}/${name}.png`, fullPage: false, animations: 'disabled' })
}

try {
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })
  await page.goto(base + '/#/bots/dev')
  await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
  await page.locator('[data-bot-settings-tabs] [data-seg="model"]').click()
  const effortBtn = page.locator('[data-select="bot-effort"]')
  await effortBtn.waitFor()

  // 1) Codex 6-level ACP option mapping: high -> xhigh, max -> ultra
  await effortBtn.scrollIntoViewIfNeeded()
  await effortBtn.click()
  const options = page.getByRole('option')
  assert.equal(await options.count(), 5, 'only Steve 5 canonical levels are shown')
  const texts = await options.allInnerTexts()
  assert.match(texts[3], /高[\s\S]*→ xhigh/, `expected high -> xhigh, got ${texts[3]}`)
  assert.match(texts[4], /最高[\s\S]*→ ultra/, `expected max -> ultra, got ${texts[4]}`)
  await shot('effort-codex')
  await options.nth(3).click()
  assert.match(await page.locator('[data-effort-picker="bot-effort"] [data-effort-note="mapped"]').innerText(), /实际生效：xhigh/)

  // Save canonical "high" (never native "xhigh"), waiting for saveBot + refresh to settle and leave Settings
  await page.locator('[data-save-bot]').click()
  await page.locator('[data-save-bot]').waitFor({ state: 'detached' })
  assert.equal(patches.length, 1)
  assert.equal(patches[0].reasoning_effort, 'high', 'saves canonical level, not native xhigh')

  // 2) Switch to Antigravity gemini-pro-orchestrator-neon: explicit levels greyed out + model-decides note
  await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
  await page.locator('[data-bot-settings-tabs] [data-seg="model"]').click()
  await page.locator('[data-select="bot-harness"]').scrollIntoViewIfNeeded()
  await page.locator('[data-select="bot-harness"]').click()
  await page.getByRole('option', { name: /Antigravity/ }).click()
  await page.locator('[data-effort-picker="bot-effort"] [data-effort-note="unsupported"]').waitFor()
  const unsupportedNote = await page.locator('[data-effort-picker="bot-effort"] [data-effort-note="unsupported"]').innerText()
  assert.match(unsupportedNote, /当前模型不支持单独设置思考强度/, unsupportedNote)
  await effortBtn.scrollIntoViewIfNeeded()
  await effortBtn.click()
  await page.getByRole('listbox').waitFor({ state: 'visible' })
  const agyOpts = page.getByRole('option')
  assert.equal(await agyOpts.count(), 5)
  assert.equal(await agyOpts.nth(0).getAttribute('aria-disabled'), null, 'default remains selectable')
  for (const idx of [1, 2, 3, 4]) {
    assert.equal(await agyOpts.nth(idx).getAttribute('aria-disabled'), 'true', `explicit option ${idx} is greyed out`)
  }
  await shot('effort-agy-unsupported')
  await page.keyboard.press('Escape')

  // 3) ModelVariant strategy on gemini-4-neon: max -> gemini-4-neon-xhigh
  await page.locator('[data-model-picker="bot-model"]').click()
  await page.locator('[data-model-opt="gemini-4-neon"]').click()
  await page.locator('[data-model-menu="bot-model"]').waitFor({ state: 'detached' })
  await effortBtn.click()
  await page.getByRole('listbox').waitFor({ state: 'visible' })
  const varOpts = page.getByRole('option')
  const varTexts = await varOpts.allInnerTexts()
  assert.match(varTexts[4], /最高[\s\S]*→ gemini-4-neon-xhigh/, `expected max -> gemini-4-neon-xhigh, got ${varTexts[4]}`)
  await shot('effort-model-variant')
  await varOpts.nth(4).click()
  assert.match(await page.locator('[data-effort-picker="bot-effort"] [data-effort-note="mapped"]').innerText(), /实际生效：gemini-4-neon-xhigh/)

  // 4a) Session view & panel showing recorded Codex effort mapping + transcript system event
  await page.goto(base + '/#/sessions/s-codex')
  await page.locator('[data-session-effort-mapping]').waitFor()
  assert.equal(await page.locator('[data-session-effort-mapping]').innerText(), '高 → Codex xhigh')
  await page.locator('[data-session-panel] [data-runtime-toggle]').click()
  assert.equal(await page.locator('[data-session-panel] [data-session-panel-effort-mapping]').innerText(), '高 → Codex xhigh')
  assert.match(await page.locator('body').innerText(), /思考强度：高 → codex xhigh/)
  await shot('effort-session-codex')

  // 4b) Session view showing recorded Antigravity unsupported fallback mapping
  await page.goto(base + '/#/sessions/s-agy')
  await page.locator('[data-session-effort-mapping]').waitFor()
  assert.equal(await page.locator('[data-session-effort-mapping]').innerText(), '高 → 由模型自己决定')
  assert.match(await page.locator('body').innerText(), /思考强度：高 → 由模型自己决定/)
  await shot('effort-session-agy-fallback')

  assert.deepEqual(errors, [])
  console.log('effort-mapping: ok')
} finally {
  await browser.close()
}
