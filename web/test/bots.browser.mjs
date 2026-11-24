import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
import { capsHub, mcpServer } from './caps-mock.mjs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* The Bot pages against a mocked Hub: the list as affectUI cards (switch and title as their own targets), 能力 as
 * Steve · 内置 (its tools folded inside with this Bot's calls over GET /stats/breakdown?by=tool&bot=) and the library
 * entries the Bot uses, 设置's permission choices (Agent / Permissions) saved
 * with the form, and 记忆 as the Memory Panel with add and forget/undo. */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 1, ...extra })
let bots, teams, memories, patches, deletes, posts, hub
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'codex', enabled: false })]
  teams = { default: 'daily', editable: true, teams: [
    { id: 'daily', name: '日常', members: [{ bot: 'dev', nodes: null }] },
    { id: 'delivery', name: '跨项目交付与质量检查协作 Team', members: [{ bot: 'dev', nodes: null }] },
  ] }
  memories = [{ id: 'm1', text: '周五下午不安排部署', source: '你加的', created: 0 }]
  patches = []; deletes = []; posts = []
  hub = capsHub({ mcp: [mcpServer('bmcp_1', 'github', 'stdio', { command: 'npx', args: ['-y', 'server-github'], bots: [{ id: 'dev', enabled: true }] })], skill: [], a2a: [] })
}
const state = () => ({ version: 'fixture', web_version: 'fixture', workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }, workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [],
  runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '', modes: ['plan', 'ask', 'accept_edits', 'auto', 'full_access'] }, { id: 'codex', name: 'Codex', ok: true, version: '', path: '', models: [], note: '' }] })
const breakdown = { by: 'tool', rows: [
  { key: 'exec', label: 'exec', dim: 'kind', count: 40 }, { key: 'tool:Bash', label: 'Bash', dim: 'tool', count: 40 },
  { key: 'mcp:steve_update', label: 'steve_update', dim: 'mcp', count: 3 }, { key: 'mcp:steve_deliver', label: 'steve_deliver', dim: 'mcp', count: 2 },
  { key: 'tool:mcp__lark__send', label: 'mcp__lark__send', dim: 'tool', count: 1 },
] }

const run = async ({ width, height, scheme }) => {
  reset()
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  let statsBot = null
  let releaseTeams, teamFailure = false
  const teamReady = new Promise(resolve => { releaseTeams = resolve })
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort()
    if (u.pathname === '/auth/hello') return route.fulfill({ json: { name: 'steve', version: 'fixture', api: 2, login: { token: false, oidc: false } } })
    if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    let data = {}
    if (await hub.handle(route, method, p)) return
    if (p === '/api/runtimes') data = state().runtimes
    else if (p === '/api/shell') data = state()
    else if (p === '/api/snapshot') data = { epoch: 'fixture', rev: 1, cursor: 'fixture:1', views: { item: [], session: [], decision: [] } }
    else if (p === '/api/summary') data = { ws: 'default', workspace_name: '默认', thinking: false, hold: null }
    else if (p === '/api/yolo') data = { on: false, active: false, forced_off: null, forced_reason: null, paused: null, window: null, scopes: [], counts: { decided: 0, declined: 0, scheduled: 0 } }
    else if (p === '/api/bot-teams' && method === 'GET') {
      await teamReady
      if (teamFailure) return route.fulfill({ status: 503, json: { error: 'Team fixture unavailable' } })
      data = teams
    }
    else if (/^\/api\/bot-teams\/[^/]+$/.test(p) && method === 'PUT') {
      const team = teams.teams.find(t => t.id === p.split('/').pop())
      Object.assign(team, route.request().postDataJSON()); data = team
    }
    else if (/^\/api\/bots\/[^/]+\/execution-nodes$/.test(p)) data = { editable: true, bindings: [], candidates: [], unavailable_bindings: 0 }
    else if (p === '/api/stats/breakdown') { statsBot = u.searchParams.get('bot'); data = breakdown }
    else if (p.startsWith('/api/stats/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
    else if (/^\/api\/bots\/[^/]+$/.test(p) && method === 'PATCH') {
      const id = p.split('/').pop(), b = route.request().postDataJSON(); patches.push({ id, ...b })
      const x = bots.find((y) => y.id === id)
      Object.assign(x, b.enabled !== undefined ? { enabled: b.enabled } : {}, b.perm_mode ? { perm_mode: b.perm_mode } : {}, b.accept_mode ? { accept_mode: b.accept_mode } : {}); data = x
    } else if (/^\/api\/bots\/[^/]+\/memories$/.test(p) && method === 'POST') { const b = route.request().postDataJSON(); posts.push(b.text); memories.push({ id: 'm' + (memories.length + 1), text: b.text, source: '你加的', created: 0 }); data = { ok: true } }
    else if (/^\/api\/bots\/[^/]+\/memories$/.test(p)) data = memories
    else if (/^\/api\/memories\//.test(p) && method === 'DELETE') { deletes.push(p.split('/').pop()); data = { ok: true } }
    else if (/^\/api\/bots\/[^/]+\/prompt$/.test(p)) data = { generated: '生成的提示词', custom: null }
    else return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/bots-${name}-${width}-${scheme}.png`, fullPage: true }) }
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })
  await page.goto(base + '/#/bots')

  /* the list: one card per Bot besides Steve; the switch saves on its own, the title opens the Bot */
  await page.locator('[data-bot-card=dev]').waitFor()
  assert.equal(await page.locator('[data-bot-cards] [data-bot-card]').count(), 2)
  assert.equal(await page.locator('[data-bot-card=doc]').getAttribute('data-off'), 'true')
  const membership = k => page.locator(`[data-bot-card=${k}] [data-bot-teams]`)
  assert.match(await membership('dev').innerText(), /正在加载 Team/)
  assert.doesNotMatch(await membership('doc').innerText(), /未加入 Team/, 'loading is not an empty membership')
  releaseTeams()
  await membership('dev').getByText('日常', { exact: true }).waitFor()
  assert.deepEqual(await membership('dev').locator('.ui-bdgc').allTextContents(), teams.teams.map(t => t.name))
  assert.match(await membership('doc').innerText(), /未加入 Team/)
  assert.match(await membership('steve').innerText(), /未加入 Team/)

  /* Both creation controls belong to their section headers and use the same style. */
  const newBot = page.locator('[data-new-bot]'), newTeam = page.locator('[data-new-team]')
  assert.equal(await newBot.count(), 1)
  assert.equal(await newBot.evaluate(el => !!el.closest('section')?.querySelector('[data-bot-cards]')), true)
  for (const attr of ['data-steve-size', 'data-steve-variant']) assert.equal(await newBot.getAttribute(attr), await newTeam.getAttribute(attr))
  const createBox = await newBot.boundingBox(), teamBox = await newTeam.boundingBox(), cardsBox = await page.locator('[data-bot-cards]').boundingBox()
  assert.ok(createBox.y + createBox.height <= cardsBox.y, 'New Bot is above the list')
  assert.ok(Math.abs(createBox.x + createBox.width - teamBox.x - teamBox.width) <= 1, 'the creation buttons align on the right')
  await fits(); await shot('list')
  if (shots && width < 640) { await page.locator('[data-bot-card=doc]').evaluate(el => el.scrollIntoView({ block: 'end', behavior: 'instant' })); await shot('list-bots') }
  await newBot.click()
  await page.getByRole('dialog').getByRole('heading', { name: '新建 Bot', exact: true }).waitFor()
  await page.getByRole('dialog').getByRole('button', { name: '取消', exact: true }).click()
  await page.locator('[data-seg="启用"]').click()
  await page.locator('[data-bot-card=doc]').waitFor({ state: 'detached' })
  assert.equal(await page.locator('[data-bot-cards] [data-bot-card]').count(), 1)
  await page.locator('[data-seg="全部"]').click()
  await page.locator('[data-bot-card=doc]').waitFor()

  /* Renaming a Team and moving its membership updates both cards without a page reload, even for a disabled Bot. */
  await page.locator('[data-edit-team=daily]').click()
  const editor = page.locator('[data-team-editor]')
  await editor.getByRole('textbox').fill('日常协作')
  await editor.locator('[data-team-bot=doc]').check()
  await editor.locator('[data-team-bot=dev]').uncheck()
  await editor.locator('[data-save-team]').click()
  await editor.waitFor({ state: 'detached' })
  await membership('doc').getByText('日常协作', { exact: true }).waitFor()
  assert.deepEqual(await membership('doc').locator('.ui-bdgc').allTextContents(), ['日常协作'])
  assert.deepEqual(await membership('dev').locator('.ui-bdgc').allTextContents(), ['跨项目交付与质量检查协作 Team'])
  await page.locator('[data-switch="bot-on-dev"]').click()
  for (let i = 0; i < 50 && !patches.length; i++) await page.waitForTimeout(100)
  assert.deepEqual(patches[0], { id: 'dev', enabled: false })
  assert.equal(await page.evaluate(() => location.hash), '#/bots', 'the switch does not open the Bot')
  await fits()
  await page.locator('[data-bot-row=dev]').click()
  await page.waitForFunction(() => location.hash === '#/bots/dev')

  /* 能力: Steve · 内置 always, its tools folded inside with this Bot's calls; then the servers the Bot uses */
  await page.locator('[data-slot=tabs-trigger]:has-text("能力")').click()
  // The card renders before its separate capabilities request finishes; wait for the loaded inventory.
  await page.locator('[data-caps] [data-steve-server]').filter({ hasText: /6 个工具/ }).waitFor()
  assert.equal(statsBot, 'dev')
  assert.match(await page.locator('[data-steve-server]').innerText(), /Steve · 内置[^]*总是启用[^]*6 个工具/)
  await page.locator('[data-steve-fold] button').first().click()
  await page.locator('[data-steve-tool="steve_update"]').waitFor()
  assert.match(await page.locator('[data-steve-tool="steve_update"]').innerText(), /汇报进展[^]*30 天 3 次/)
  assert.match(await page.locator('[data-steve-tool="steve_ask"]').innerText(), /30 天没用过/)
  assert.equal(await page.locator('[data-skill]').count(), 0, 'no flat grid of tool tiles any more')
  assert.equal(await page.locator('[data-mcp] [data-mcp-server=github]').count(), 1)
  assert.equal(await page.locator('[data-caps-perm]').getAttribute('data-caps-perm'), 'default')
  await fits(); await shot('caps')

  /* 设置: the permission mode and acceptance as one choice each, saved with the form */
  await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
  const perm = page.locator('[data-choices="bot-perm"]')
  await perm.waitFor()
  assert.deepEqual(await perm.locator('[role=radio]').evaluateAll((choices) => choices.map((choice) => choice.getAttribute('data-choice')).sort()), ['default', 'plan'])
  assert.equal(await perm.locator('[aria-checked=true]').getAttribute('data-choice'), 'default')
  await perm.locator('[data-choice=plan]').click()
  await page.locator('[data-choices="bot-acc"] [data-choice="自动验收"]').click()
  assert.equal(await perm.locator('[aria-checked=true]').getAttribute('data-choice'), 'plan')
  await fits(); await shot('settings')
  await page.locator('[data-save-bot]').click()
  for (let i = 0; i < 50 && patches.length < 2; i++) await page.waitForTimeout(100)
  assert.equal(patches[1].perm_mode, 'plan')
  assert.equal(patches[1].accept_mode, 'auto')

  /* 记忆: the Memory Panel — added through the Hub, forgotten with Undo before the Hub deletes it */
  await page.locator('[data-slot=tabs-trigger]:has-text("记忆")').click()
  await page.locator('[data-memory=m1]').waitFor()
  await page.locator('[data-mem-new]').click()
  await page.locator('[data-mem-input]').fill('提交前跑测试')
  await page.locator('[data-mem-input]').press('Enter')
  await page.locator('[data-memory=m2]').waitFor()
  assert.deepEqual(posts, ['提交前跑测试'])
  await page.locator('[data-memory=m1]').hover()
  await page.locator('[data-memory=m1] [data-mem-del]').click()
  await page.locator('.ui-mpc__action--undo').click()
  await page.locator('[data-memory=m1]').waitFor()
  await page.waitForTimeout(300)
  assert.deepEqual(deletes, [], 'Undo keeps the memory')
  await fits(); await shot('memory')
  /* leaving the tab while Undo still shows lets the Hub delete it */
  await page.locator('[data-memory=m2]').hover()
  await page.locator('[data-memory=m2] [data-mem-del]').click()
  await page.locator('[data-slot=tabs-trigger]:has-text("概览")').click()
  for (let i = 0; i < 50 && !deletes.length; i++) await page.waitForTimeout(100)
  assert.deepEqual(deletes, ['m2'])

  /* An unavailable Team service must not claim that a Bot belongs to no Team. */
  teamFailure = true
  await page.goto(base + '/#/bots')
  await membership('dev').getByText('Team 加载失败', { exact: true }).waitFor()
  assert.doesNotMatch(await membership('dev').innerText(), /未加入 Team/)
  await fits()
  assert.deepEqual(errors, [])
  await page.close()
}

try {
  for (const v of [{ width: 1440, height: 900, scheme: 'light' }, { width: 320, height: 844, scheme: 'dark' }]) await run(v)
  console.log('bots: ok')
} finally { await browser.close() }
