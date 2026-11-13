import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* The Bot pages against a mocked Hub: the list as affectUI cards (switch and title as their own targets), 能力 as the
 * Agent / Skills tiles over GET /stats/breakdown?by=tool&bot=, 设置's permission choices (Agent / Permissions) saved
 * with the form, and 记忆 as the Memory Panel with add and forget/undo. */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 1, ...extra })
let bots, memories, patches, deletes, posts
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'codex', enabled: false })]
  memories = [{ id: 'm1', text: '周五下午不安排部署', source: '你加的', created: 0 }]
  patches = []; deletes = []; posts = []
}
const state = () => ({ version: 'fixture', web_version: 'fixture', workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }, workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [],
  runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '' }, { id: 'codex', name: 'Codex', ok: true, version: '', path: '', models: [], note: '' }] })
const breakdown = { by: 'tool', rows: [
  { key: 'exec', label: 'exec', dim: 'kind', count: 40 }, { key: 'tool:Bash', label: 'Bash', dim: 'tool', count: 40 },
  { key: 'mcp:steve_update', label: 'steve_update', dim: 'mcp', count: 3 }, { key: 'mcp:steve_workflow', label: 'steve_workflow', dim: 'mcp', count: 2 },
  { key: 'tool:mcp__lark__send', label: 'mcp__lark__send', dim: 'tool', count: 1 },
] }

const run = async ({ width, height, scheme }) => {
  reset()
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  let statsBot = null
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    let data = {}
    if (p === '/api/state') data = state()
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
  await page.locator('[data-switch="bot-on-dev"]').click()
  for (let i = 0; i < 50 && !patches.length; i++) await page.waitForTimeout(100)
  assert.deepEqual(patches[0], { id: 'dev', enabled: false })
  assert.equal(await page.evaluate(() => location.hash), '#/bots', 'the switch does not open the Bot')
  await fits(); await shot('list')
  await page.locator('[data-bot-row=dev]').click()
  await page.waitForFunction(() => location.hash === '#/bots/dev')

  /* 能力: Steve's tools always, other MCP and harness tools once used, each with its calls in 30 days */
  await page.locator('[data-slot=tabs-trigger]:has-text("能力")').click()
  await page.locator('[data-caps] [data-skill="steve_update"]').waitFor()
  assert.equal(statsBot, 'dev')
  assert.match(await page.locator('[data-skill="steve_update"]').innerText(), /30 天 3 次/)
  assert.match(await page.locator('[data-skill="steve_ask"]').innerText(), /30 天没用过/)
  for (const k of ['mcp:steve_workflow', 'tool:mcp__lark__send', 'tool:Bash']) assert.equal(await page.locator(`[data-skill="${k}"]`).count(), 1, k)
  /* the Hub counts another MCP server's tools as tool:mcp__<server>__<tool>: they go under 其他 MCP, named by tool, chip by server */
  assert.match(await page.locator('[data-skill="tool:mcp__lark__send"]').innerText(), /^send\n[^]*\blark\b[^]*30 天 1 次/)
  await page.locator('[data-skill-cat="其他 MCP"]').click()
  assert.deepEqual(await page.locator('[data-caps] [data-skill]').evaluateAll((xs) => xs.map((x) => x.getAttribute('data-skill'))), ['tool:mcp__lark__send'], 'other MCP tools are not built into the harness')
  await shot('caps-mcp')
  assert.equal(await page.locator('[data-skill="exec"]').count(), 0, 'tool kinds are not tools')
  await page.locator('[data-skill-cat="Claude Code"]').click()
  assert.equal(await page.locator('[data-skill-cat="Claude Code"]').getAttribute('aria-selected'), 'true')
  assert.deepEqual(await page.locator('[data-caps] [data-skill]').evaluateAll((xs) => xs.map((x) => x.getAttribute('data-skill'))), ['tool:Bash'], 'the harness filter shows its tools only')
  assert.equal(await page.locator('[data-caps-perm]').getAttribute('data-caps-perm'), 'default')
  await fits(); await shot('caps')

  /* 设置: the permission mode and acceptance as one choice each, saved with the form */
  await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
  const perm = page.locator('[data-choices="bot-perm"]')
  await perm.waitFor()
  assert.equal(await perm.locator('[role=radio]').count(), 5)
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

  assert.deepEqual(errors, [])
  await page.close()
}

try {
  for (const v of [{ width: 1440, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]) await run(v)
  console.log('bots: ok')
} finally { await browser.close() }
