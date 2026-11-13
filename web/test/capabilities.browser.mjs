import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
import { agentEntry, capsHub, mcpServer, skillEntry } from './caps-mock.mjs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* 能力 (#/capabilities) against a mocked Hub: the sidebar entry; MCP 服务器 with Steve · 内置 (always on, its tools
 * folded inside), connecting from outside (the address the Hub says, a token made once and put into the client's
 * configuration, the tools such a client gets) and the library with the Bots that use each server, chosen from the
 * card; 技能; A2A with a remote agent added by its card (read first, its skills shown, a key kept by name only) and
 * Steve's own Agent Card. */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0, ...extra })
let bots, hub
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'codex' })]
  hub = capsHub({
    mcp: [
      mcpServer('bmcp_1', 'github', 'stdio', { command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], env: [{ name: 'GITHUB_TOKEN', secret: true }], bots: [{ id: 'dev', enabled: true }, { id: 'doc', enabled: false }] }),
      mcpServer('bmcp_2', 'postgres', 'stdio', { command: 'uvx', args: ['mcp-server-postgres'] }),
    ],
    skill: [skillEntry('bskill_1', 'code-review', '审查一次改动：合并前用。', '# 审查', { bots: [{ id: 'dev', enabled: true }] })],
    a2a: [agentEntry('a2a_1', 'expenses', '报销助手', { skills: [{ id: 'file', name: '提交报销', description: '', tags: [] }], auth: { scheme: 'bearer' }, has_secret: true, bots: [{ id: 'doc', enabled: true }] })],
  })
}
const state = () => ({ version: 'fixture', web_version: 'fixture', workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }, workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [],
  runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '', skills: true }, { id: 'codex', name: 'Codex', ok: true, version: '', path: '', models: [], note: '', skills: true }] })

const run = async ({ width, height, scheme }) => {
  reset()
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    if (p === '/api/state') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(state()) })
    if (await hub.handle(route, method, p)) return
    return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
  })
  const calls = hub.calls
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name, full = true) => { if (shots) await page.screenshot({ path: `${shots}/caps-${name}-${width}-${scheme}.png`, fullPage: full }) }
  const settle = async (n) => { for (let i = 0; i < 50 && calls.length < n; i++) await page.waitForTimeout(100) }
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })
  await page.goto(base + '/#/')

  /* the sidebar has 能力 with the Bots (a phone reaches it from 我的) */
  if (width >= 768) {
    await page.locator('[data-nav=capabilities]').click()
  } else {
    await page.goto(base + '/#/more')
    await page.locator('text=MCP 服务器、技能和远程 Agent').click()
  }
  await page.waitForFunction(() => location.hash === '#/capabilities')
  await page.locator('[data-caps-page=mcp]').waitFor()

  /* Steve · 内置: always on, its tools inside the fold */
  const steve = page.locator('[data-steve-server]')
  await steve.waitFor()
  assert.match(await steve.innerText(), /Steve · 内置[^]*总是启用[^]*6 个工具/)
  await steve.locator('[data-steve-fold] button').first().click()
  await page.locator('[data-steve-tool=steve_memory_list]').waitFor()
  assert.match(await page.locator('[data-steve-tool=steve_update]').innerText(), /汇报进展、更新计划/)

  /* from outside: the address the Hub gives, public first; a token made once fills the client's configuration */
  const out = page.locator('[data-outside]')
  assert.equal(await out.locator('[data-outside-url] [data-copy-value]').innerText(), 'https://hub.example.test/mcp')
  assert.match(await out.locator('[data-outside-snippet]').innerText(), /<你的访问令牌>/)
  await out.locator('[data-outside-token-create]').click()
  await out.locator('[data-outside-token-made]').waitFor()
  await settle(1)
  assert.deepEqual(calls[0], ['TOKEN', { name: 'MCP 客户端' }])
  assert.match(await out.locator('[data-outside-snippet]').innerText(), /"Authorization": "Bearer stt_test_once"/)
  await out.locator('[data-seg=claudeDesktop]').click()
  assert.match(await out.locator('[data-outside-snippet=claudeDesktop]').innerText(), /mcp-remote[^]*STEVE_AUTH/)
  await out.locator('[data-seg=claudeCode]').click()
  assert.match(await out.locator('[data-outside-snippet=claudeCode]').innerText(), /claude mcp add --transport http steve https:\/\/hub\.example\.test\/mcp/)
  await out.locator('[data-seg="1"]').click()
  assert.equal(await out.locator('[data-outside-url] [data-copy-value]').innerText(), 'http://192.168.1.20:8440/mcp')
  await out.locator('[data-outside-scope] button').first().click()
  await out.locator('[data-steve-tool=steve_item_create]').waitFor()
  assert.equal(await out.locator('[data-outside-scope] [data-steve-tool]').count(), 4)
  await fits(); await shot('mcp')

  /* the library: who uses each server; choosing Bots from the card */
  const gh = page.locator('[data-mcp-list] [data-mcp-server=github]')
  assert.equal(await gh.locator('[data-used-by]').getAttribute('data-used-by'), 'dev,doc')
  assert.equal(await page.locator('[data-mcp-server=postgres] [data-used-by]').getAttribute('data-used-by'), '')
  await page.locator('[data-assign=postgres]').click()
  await page.locator('[data-assign-bot=doc]').click()
  await settle(2)
  assert.deepEqual(calls[1], ['PUT', 'doc', 'mcp', 'bmcp_2', { enabled: true }])
  await page.keyboard.press('Escape')
  await page.locator('[data-mcp-server=postgres] [data-used-by=doc]').waitFor()
  await page.locator('[data-mcp-list]').scrollIntoViewIfNeeded(); await page.waitForTimeout(400)
  await fits(); await shot('library', false)

  /* 技能 */
  await page.locator('[data-seg=skills]').click()
  await page.waitForFunction(() => location.hash === '#/capabilities/skills')
  await page.locator('[data-bot-skill=code-review]').waitFor()
  assert.equal(await page.locator('[data-bot-skill=code-review] [data-used-by]').getAttribute('data-used-by'), 'dev')
  await fits(); await shot('skills')

  /* A2A: one added by its card – read first, then kept with its key by name only */
  await page.locator('[data-seg=a2a]').click()
  await page.locator('[data-a2a-agent=expenses]').waitFor()
  assert.match(await page.locator('[data-a2a-agent=expenses]').innerText(), /报销助手[^]*提交报销[^]*steve_a2a_expenses/)
  await page.locator('[data-a2a-own-card] [data-copy-value]').waitFor()
  assert.equal(await page.locator('[data-a2a-own-card] [data-copy-value]').innerText(), 'https://hub.example.test/.well-known/agent-card.json')
  await fits(); await shot('a2a')
  await page.locator('[data-a2a-add]').first().click()
  const dlg = page.locator('[data-a2a-dialog]')
  await dlg.waitFor()
  assert.equal(await dlg.locator('[data-a2a-save]').isDisabled(), true, 'nothing to save before the card is read')
  await dlg.locator('input[data-a2a-url], [data-a2a-url] input').first().fill('https://trips.example.test')
  await dlg.locator('[data-a2a-fetch]').click()
  await dlg.locator('[data-a2a-preview]').waitFor()
  assert.match(await dlg.locator('[data-a2a-preview]').innerText(), /Trip Planner[^]*安排行程/)
  await dlg.locator('[data-seg=header]').click()
  await dlg.locator('input[data-a2a-header], [data-a2a-header] input').first().fill('X-Api-Key')
  assert.equal(await dlg.locator('[data-a2a-save]').isDisabled(), true, 'a key is needed for a header')
  await dlg.locator('input[data-a2a-secret]').fill('agent-KEY')
  await fits(); await shot('a2a-add', false)
  await dlg.locator('[data-a2a-save]').click()
  await page.locator('[data-a2a-agent=trip_planner]').waitFor()
  const post = calls.find((c) => c[0] === 'POST')
  assert.deepEqual(post, ['POST', 'a2a', { auth: { scheme: 'header', header: 'X-Api-Key' }, secret: 'agent-KEY', card_url: 'https://trips.example.test/.well-known/agent-card.json' }])
  assert.doesNotMatch(await page.locator('[data-a2a]').innerText(), /agent-KEY/)

  assert.deepEqual(errors, [])
  await page.close()
}

try {
  const sizes = process.env.STEVE_TEST_ALL ? [[1440, 900], [390, 844]].flatMap(([width, height]) => ['light', 'dark'].map((scheme) => ({ width, height, scheme }))) : [{ width: 1440, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]
  for (const v of sizes) await run(v)
  console.log('capabilities: ok')
} finally { await browser.close() }
