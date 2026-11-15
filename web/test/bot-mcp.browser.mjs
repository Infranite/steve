import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
import { capsHub, mcpServer } from './caps-mock.mjs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* A Bot's 能力 → MCP 服务器 against a mocked Hub: the servers come from the workspace's library (/capabilities/*) and
 * a Bot is given some (/bots/:id/capabilities). A Bot without any says what MCP servers are; one with some shows a
 * card each with this Bot's own switch (a secret by name only); 添加 picks one from the library or makes a new one,
 * which goes into the library and to this Bot at once; editing keeps a secret left empty; taking one off the Bot
 * leaves it in the library. Steve · 内置 is the first card, always on. */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0, ...extra })
let bots, hub
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'codex' })]
  hub = capsHub({
    mcp: [
      mcpServer('bmcp_1', 'github', 'stdio', { command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], env: [{ name: 'GITHUB_TOKEN', secret: true }, { name: 'LOG', value: 'info' }], bots: [{ id: 'dev', enabled: true }] }),
      mcpServer('bmcp_2', 'linear', 'sse', { url: 'https://mcp.linear.app/sse', headers: [{ name: 'Authorization', secret: true }], bots: [{ id: 'dev', enabled: true }] }),
      mcpServer('bmcp_3', 'postgres', 'stdio', { command: 'uvx', args: ['mcp-server-postgres'] }),
    ],
    skill: [], a2a: [],
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
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort()
    if (u.pathname === '/auth/hello') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ name: 'steve', api: 1, version: 'fixture' }) })
    if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    if (p === '/api/state') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(state()) })
    if (p === '/api/stats/breakdown') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ by: 'tool', rows: [] }) })
    if (await hub.handle(route, method, p)) return
    return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
  })
  const calls = hub.calls
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/botmcp-${name}-${width}-${scheme}.png`, fullPage: true }) }
  const settle = async (n) => { for (let i = 0; i < 50 && calls.length < n; i++) await page.waitForTimeout(100) }
  const caps = async (id) => {
    await page.goto(base + '/#/bots/' + id)
    try { await page.getByRole('tab', { name: '能力', exact: true }).click() }
    catch (e) { console.error({ pageErrors: errors, body: await page.locator('body').innerText() }); throw e }
    await page.locator('[data-mcp]').waitFor()
  }
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })

  /* no servers: Steve · 内置 first, then what MCP servers are and the way to add one */
  await caps('doc')
  await page.locator('[data-steve-server]').waitFor()
  await page.locator('[data-tool-pins]').waitFor()
  assert.match(await page.locator('[data-tool-pin-count]').innerText(), /8 个核心工具.*0 \/ 32/)
  assert.match(await page.locator('[data-steve-server]').innerText(), /Steve · 内置[^]*总是启用/)
  await page.locator('[data-mcp-empty]').waitFor()
  assert.match(await page.locator('[data-mcp-empty]').innerText(), /MCP 服务器给 Bot 增加工具/)
  assert.equal(await page.locator('[data-planned]').count(), 0, 'nothing is marked planned any more')
  await fits(); await shot('empty')

  /* the Bot's servers: one card each, a secret by name only; the one it does not use is not here */
  await caps('dev')
  await page.locator('[data-mcp-server=github]').waitFor()
  assert.equal(await page.locator('[data-mcp] [data-mcp-server]').count(), 2)
  assert.equal(await page.locator('[data-mcp-server=github] [data-mcp-transport]').getAttribute('data-mcp-transport'), 'stdio')
  assert.match(await page.locator('[data-mcp-server=github]').innerText(), /npx -y @modelcontextprotocol\/server-github/)
  assert.equal(await page.locator('[data-mcp-server=github] [data-mcp-var=GITHUB_TOKEN]').getAttribute('data-secret'), 'true')
  assert.match(await page.locator('[data-mcp-server=github] [data-mcp-var=LOG]').innerText(), /LOG=info/)
  await fits(); await shot('list')

  /* the switch is this Bot's, and saves at once */
  await page.locator('[data-mcp-server=linear] [data-isc-switch]').click()
  await settle(1)
  assert.deepEqual(calls[0], ['PUT', 'dev', 'mcp', 'bmcp_2', { enabled: false }])
  await page.locator('[data-mcp-server=linear][data-mcp-on=false]').waitFor()

  /* edit: the secret's value stays empty and is kept */
  await page.locator('[data-cap-edit=github]').click()
  const dlg = page.locator('[data-mcp-dialog]')
  await dlg.waitFor()
  const secretRow = dlg.locator('[data-mcp-var-row="0"]')
  assert.equal(await secretRow.locator('input[type=password]').inputValue(), '')
  assert.equal(await secretRow.locator('input[type=password]').getAttribute('placeholder'), '已存在保险箱，留空不改')
  await dlg.locator('textarea[data-mcp-args]').fill('-y\n@modelcontextprotocol/server-github@2')
  await dlg.locator('[data-mcp-save]').click()
  await settle(2)
  assert.deepEqual(calls[1], ['PATCH', 'mcp', 'bmcp_1', { name: 'github', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-github@2'], env: [{ name: 'GITHUB_TOKEN', secret: true }, { name: 'LOG', value: 'info' }] }])
  await dlg.waitFor({ state: 'detached' })

  /* 添加 from the library: only what the Bot does not use yet */
  await page.locator('[data-cap-add=mcp]').click()
  await page.locator('[data-cap-take=postgres]').waitFor()
  assert.equal(await page.locator('[data-cap-take]').count(), 1)
  await fits(); await shot('add-menu')
  await page.locator('[data-cap-take=postgres]').click()
  await page.locator('[data-mcp] [data-mcp-server=postgres]').waitFor()
  assert.deepEqual(calls[2], ['PUT', 'dev', 'mcp', 'bmcp_3', { enabled: true }])

  /* a new one goes into the library and to this Bot: an http server with a secret header */
  await page.locator('[data-cap-add=mcp]').click()
  await page.locator('[data-cap-new=mcp]').click()
  await dlg.waitFor()
  await dlg.locator('input[data-mcp-name], [data-mcp-name] input').first().fill('sentry')
  await dlg.locator('button:has-text("HTTP")').first().click()
  await dlg.locator('input[data-mcp-url], [data-mcp-url] input').first().fill('https://mcp.sentry.dev/mcp')
  await dlg.locator('[data-mcp-var-add]').click()
  await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-name]').fill('Authorization')
  await dlg.locator('[data-mcp-var-row="0"] [data-mcp-var-secret]').check()
  await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-value]').fill('Bearer sk-test')
  assert.equal(await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-value]').getAttribute('type'), 'password')
  await fits(); await shot('add')
  await dlg.locator('[data-mcp-save]').click()
  await page.locator('[data-mcp] [data-mcp-server=sentry]').waitFor()
  assert.deepEqual(calls[3], ['POST', 'mcp', { name: 'sentry', transport: 'http', url: 'https://mcp.sentry.dev/mcp', headers: [{ name: 'Authorization', value: 'Bearer sk-test', secret: true }], bot: 'dev' }])
  assert.doesNotMatch(await page.locator('[data-mcp]').innerText(), /sk-test/, 'a secret is never shown back')

  /* off this Bot: it stays in the library */
  await page.locator('[data-cap-drop=sentry]').click()
  await page.locator('[data-mcp] [data-mcp-server=sentry]').waitFor({ state: 'detached' })
  assert.deepEqual(calls[4], ['UNASSIGN', 'dev', 'mcp', 'bmcp_101'])
  assert.equal(hub.lib.mcp.some((x) => x.name === 'sentry'), true)
  await page.locator('[data-mcp-server=linear]').scrollIntoViewIfNeeded(); await page.waitForTimeout(600)
  await fits(); await shot('caps')

  assert.deepEqual(errors, [])
  await page.close()
}

try {
  const sizes = process.env.STEVE_TEST_ALL ? [[1440, 900], [390, 844]].flatMap(([width, height]) => ['light', 'dark'].map((scheme) => ({ width, height, scheme }))) : [{ width: 1440, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]
  for (const v of sizes) await run(v)
  console.log('bot-mcp: ok')
} finally { await browser.close() }
