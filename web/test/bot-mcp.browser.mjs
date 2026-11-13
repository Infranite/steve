import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* 能力 → MCP 服务器 against a mocked Hub (/bots/:id/mcp-servers): a Bot without servers shows what MCP servers are;
 * one with servers shows a card each (transport, a secret by name only, the switch saving at once); the dialog edits a
 * server keeping its secret when the value is left empty, adds an http server with a secret header, and a delete asks
 * first. A2A is named as planned, nothing more (技能 is bot-skills.browser.mjs). */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0, ...extra })
const server = (id, name, transport, extra = {}) => ({ id, bot_id: 'dev', name, transport, command: null, args: [], env: [], url: null, headers: [], enabled: true, created: 0, updated: 0, ...extra })
let bots, servers, calls
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'codex' })]
  servers = {
    dev: [
      server('bmcp_1', 'github', 'stdio', { command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], env: [{ name: 'GITHUB_TOKEN', secret: true }, { name: 'LOG', value: 'info' }] }),
      server('bmcp_2', 'linear', 'sse', { url: 'https://mcp.linear.app/sse', headers: [{ name: 'Authorization', secret: true }] }),
    ],
    doc: [],
  }
  calls = []
}
const state = () => ({ version: 'fixture', web_version: 'fixture', workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }, workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [],
  runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '' }, { id: 'codex', name: 'Codex', ok: true, version: '', path: '', models: [], note: '' }] })
/* what the Hub answers: a secret never comes back with its value */
const shown = (s) => ({ ...s, env: s.env.map(({ name, value, secret }) => (secret ? { name, secret } : { name, value })), headers: s.headers.map(({ name, value, secret }) => (secret ? { name, secret } : { name, value })) })

const run = async ({ width, height, scheme }) => {
  reset()
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    const json = (status, data) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(data) })
    if (p === '/api/state') return json(200, state())
    if (p === '/api/stats/breakdown') return json(200, { by: 'tool', rows: [] })
    let m = /^\/api\/bots\/([^/]+)\/mcp-servers$/.exec(p)
    if (m && method === 'GET') return json(200, (servers[m[1]] ?? []).map(shown))
    if (m && method === 'POST') {
      const b = route.request().postDataJSON(); calls.push(['POST', m[1], b])
      const s = server('bmcp_' + (Object.values(servers).flat().length + 1), b.name, b.transport, { ...b, bot_id: m[1] })
      ;(servers[m[1]] ??= []).push(s); return json(200, shown(s))
    }
    m = /^\/api\/bots\/([^/]+)\/mcp-servers\/([^/]+)$/.exec(p)
    if (m && method === 'PATCH') {
      const b = route.request().postDataJSON(); calls.push(['PATCH', m[2], b])
      const s = servers[m[1]].find((x) => x.id === m[2]); Object.assign(s, b); return json(200, shown(s))
    }
    if (m && method === 'DELETE') { calls.push(['DELETE', m[2]]); servers[m[1]] = servers[m[1]].filter((x) => x.id !== m[2]); return json(200, { ok: true }) }
    return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/botmcp-${name}-${width}-${scheme}.png`, fullPage: true }) }
  const caps = async (id) => {
    await page.goto(base + '/#/bots/' + id)
    await page.locator('[data-slot=tabs-trigger]:has-text("能力")').click()
    await page.locator('[data-mcp]').waitFor()
  }
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })

  /* no servers: what MCP servers are, and the way to add one; A2A is planned only */
  await caps('doc')
  await page.locator('[data-mcp-empty]').waitFor()
  assert.match(await page.locator('[data-mcp-empty]').innerText(), /MCP 服务器给 Bot 增加工具/)
  assert.equal(await page.locator('[data-planned=skills]').count(), 0, 'skills are no longer planned')
  for (const k of ['a2a']) {
    const r = page.locator(`[data-planned=${k}]`)
    assert.match(await r.innerText(), /规划中/)
    assert.equal(await r.locator('button, input, a').count(), 0, `${k} has nothing to press`)
  }
  await fits(); await shot('empty')
  await page.locator('[data-caps-planned]').scrollIntoViewIfNeeded(); await page.waitForTimeout(800); await shot('planned')

  /* the Bot's servers: one card each, a secret by name only */
  await caps('dev')
  await page.locator('[data-mcp-server=github]').waitFor()
  assert.equal(await page.locator('[data-mcp-server]').count(), 2)
  assert.equal(await page.locator('[data-mcp-server=github] [data-mcp-transport]').getAttribute('data-mcp-transport'), 'stdio')
  assert.equal(await page.locator('[data-mcp-server=linear] [data-mcp-transport]').getAttribute('data-mcp-transport'), 'sse')
  assert.match(await page.locator('[data-mcp-server=github]').innerText(), /npx -y @modelcontextprotocol\/server-github/)
  assert.equal(await page.locator('[data-mcp-server=github] [data-mcp-var=GITHUB_TOKEN]').getAttribute('data-secret'), 'true')
  assert.match(await page.locator('[data-mcp-server=github] [data-mcp-var=LOG]').innerText(), /LOG=info/)
  await fits(); await shot('list')

  /* the switch saves at once */
  await page.locator('[data-mcp-server=linear] [data-isc-switch]').click()
  for (let i = 0; i < 50 && !calls.length; i++) await page.waitForTimeout(100)
  assert.deepEqual(calls[0], ['PATCH', 'bmcp_2', { enabled: false }])
  await page.locator('[data-mcp-server=linear][data-mcp-on=false]').waitFor()

  /* edit: the secret's value stays empty and is kept */
  await page.locator('[data-mcp-edit=github]').click()
  const dlg = page.locator('[data-mcp-dialog]')
  await dlg.waitFor()
  assert.equal(await dlg.locator('[data-mcp-name] input, input[data-mcp-name]').first().inputValue(), 'github')
  const secretRow = dlg.locator('[data-mcp-var-row="0"]')
  assert.equal(await secretRow.locator('input[type=password]').inputValue(), '')
  assert.equal(await secretRow.locator('input[type=password]').getAttribute('placeholder'), '已存在保险箱，留空不改')
  await dlg.locator('textarea[data-mcp-args]').fill('-y\n@modelcontextprotocol/server-github@2')
  await fits(); await shot('edit')
  await dlg.locator('[data-mcp-save]').click()
  for (let i = 0; i < 50 && calls.length < 2; i++) await page.waitForTimeout(100)
  assert.deepEqual(calls[1], ['PATCH', 'bmcp_1', { name: 'github', transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-github@2'], env: [{ name: 'GITHUB_TOKEN', secret: true }, { name: 'LOG', value: 'info' }] }])
  await dlg.waitFor({ state: 'detached' })

  /* add an http server with a secret header */
  await page.locator('[data-mcp-add]').first().click()
  await dlg.waitFor()
  await dlg.locator('input[data-mcp-name], [data-mcp-name] input').first().fill('sentry')
  await dlg.getByRole('radio', { name: 'HTTP' }).or(dlg.getByRole('tab', { name: 'HTTP' })).or(dlg.locator('button:has-text("HTTP")')).first().click()
  await dlg.locator('input[data-mcp-url], [data-mcp-url] input').first().fill('https://mcp.sentry.dev/mcp')
  await dlg.locator('[data-mcp-var-add]').click()
  await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-name]').fill('Authorization')
  await dlg.locator('[data-mcp-var-row="0"] [data-mcp-var-secret]').check()
  await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-value]').fill('Bearer sk-test')
  assert.equal(await dlg.locator('[data-mcp-var-row="0"] input[data-mcp-var-value]').getAttribute('type'), 'password')
  await fits(); await shot('add')
  await dlg.locator('[data-mcp-save]').click()
  await page.locator('[data-mcp-server=sentry]').waitFor()
  assert.deepEqual(calls[2], ['POST', 'dev', { name: 'sentry', transport: 'http', url: 'https://mcp.sentry.dev/mcp', headers: [{ name: 'Authorization', value: 'Bearer sk-test', secret: true }] }])
  assert.doesNotMatch(await page.locator('[data-mcp]').innerText(), /sk-test/, 'a secret is never shown back')

  /* delete asks first */
  await page.locator('[data-mcp-remove=sentry]').click()
  await page.locator('[data-mcp-remove-confirm]').click()
  await page.locator('[data-mcp-server=sentry]').waitFor({ state: 'detached' })
  assert.deepEqual(calls[3], ['DELETE', 'bmcp_3'])
  await page.locator('[data-mcp-server=linear]').scrollIntoViewIfNeeded(); await page.waitForTimeout(800)
  await fits(); await shot('caps')

  assert.deepEqual(errors, [])
  await page.close()
}

try {
  const sizes = process.env.STEVE_TEST_ALL ? [[1440, 900], [390, 844]].flatMap(([width, height]) => ['light', 'dark'].map((scheme) => ({ width, height, scheme }))) : [{ width: 1440, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]
  for (const v of sizes) await run(v)
  console.log('bot-mcp: ok')
} finally { await browser.close() }
