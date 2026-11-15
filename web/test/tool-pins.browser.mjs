import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5199'
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const core = ['steve_update', 'steve_ask', 'steve_request_approval', 'steve_deliver', 'steve_items_search', 'steve_knowledge_search', 'steve_tools_search', 'steve_tool_call']
try {
  for (const scheme of ['light', 'dark']) for (const width of [1440, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 960 }, colorScheme: scheme, reducedMotion: 'reduce' })
    page.setDefaultTimeout(10000)
    const errors = [], calls = []
    page.on('pageerror', (e) => errors.push(e.message))
    let pins = [], failPin = false, full = false, failList = false
    await page.addInitScript((theme) => { localStorage.setItem('steve-lang', 'zh-CN'); localStorage.setItem('steve-theme', theme) }, scheme)
    await page.route('**/*', async (route) => {
      const u = new URL(route.request().url())
      if (u.origin !== base) return route.abort()
      if (!u.pathname.startsWith('/api/')) return route.continue()
      const p = u.pathname, m = route.request().method()
      const json = (status, data) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(data) })
      if (p === '/api/bots/writer/tools') {
        if (failList) return json(500, { error: 'Fixture list unavailable' })
        return json(200, { core, pins: full ? Array.from({ length: 32 }, (_, i) => ({ kind: 'tool', target: `known_${i}` })) : pins, usage: [{ tool: 'steve_remember', searched: 12, called: 8 }], days: 30 })
      }
      if (p === '/api/capabilities/steve') return json(200, { tools: [{ name: 'steve_remember', description: 'Save a memory' }, { name: 'steve_memory_list', description: 'Read memories' }], outside: [], endpoints: [], agent_card: '', a2a: '' })
      const match = /^\/api\/bots\/writer\/tool-pins\/(tool|mcp|a2a)\/([^/]+)$/.exec(p)
      if (match) {
        calls.push([m, match[1], decodeURIComponent(match[2])])
        if (failPin) { failPin = false; return json(400, { error: 'Fixture pin rejected' }) }
        const pin = { kind: match[1], target: decodeURIComponent(match[2]) }
        pins = m === 'PUT' ? [...pins, pin] : pins.filter((x) => x.kind !== pin.kind || x.target !== pin.target)
        return json(200, { pinned: m === 'PUT' })
      }
      return json(404, {})
    })
    const open = async () => {
      await page.goto(base + '/test/fixtures/tool-pins.html')
      await page.locator('[data-tool-pin-browse] > button').click()
      await page.locator('[data-tool-pin-search]').waitFor()
    }
    await open()
    assert.match(await page.locator('[data-tool-pins]').innerText(), /钉住不是授权/)
    assert.match(await page.locator('[data-tool-pin-count]').innerText(), /8 个核心工具.*0 \/ 32/)
    assert.equal(await page.locator('[data-tool-pin="tool:steve_update"]').count(), 0)
    const search = page.locator('[data-tool-pin-search]')
    await search.fill('memory')
    await page.locator('[data-tool-pin="tool:steve_memory_list"]').waitFor()
    await search.fill('steve_remember')
    const pin = page.locator('[data-tool-pin="tool:steve_remember"]')
    await pin.click()
    await page.locator('[data-tool-pin="tool:steve_remember"][aria-pressed=true]').waitFor()
    assert.deepEqual(calls.at(-1), ['PUT', 'tool', 'steve_remember'])
    await pin.click()
    await page.locator('[data-tool-pin="tool:steve_remember"][aria-pressed=false]').waitFor()
    assert.deepEqual(calls.at(-1), ['DELETE', 'tool', 'steve_remember'])
    failPin = true
    await pin.click()
    await page.locator('[data-tool-pin-error]').waitFor()
    assert.match(await page.locator('[data-tool-pin-error]').innerText(), /Fixture pin rejected/)
    assert.equal(await pin.getAttribute('aria-pressed'), 'false')
    await pin.click()
    await page.locator('[data-tool-pin="tool:steve_remember"][aria-pressed=true]').waitFor()
    assert.equal(await page.locator('[data-tool-pin-error]').count(), 0)
    assert.match(await page.locator('[data-tool-pins]').innerText(), /搜索 12 次 · 调用 8 次/)
    await search.fill('Team Calendar')
    await page.locator('[data-tool-pin="mcp:m1"]').click()
    await page.locator('[data-tool-pin="mcp:m1"][aria-pressed=true]').waitFor()
    assert.deepEqual(calls.at(-1), ['PUT', 'mcp', 'm1'])
    await search.fill('Disabled Calendar')
    assert.equal(await page.locator('[data-tool-pin="mcp:m2"]').isDisabled(), true)
    await search.fill('Trip Planner')
    await page.locator('[data-tool-pin="a2a:a1"]').click()
    await page.locator('[data-tool-pin="a2a:a1"][aria-pressed=true]').waitFor()
    assert.deepEqual(calls.at(-1), ['PUT', 'a2a', 'a1'])
    await page.locator('[data-tool-pin="a2a:a1"]').click()
    await page.locator('[data-tool-pin="a2a:a1"][aria-pressed=false]').waitFor()
    assert.deepEqual(calls.at(-1), ['DELETE', 'a2a', 'a1'])
    await search.fill('not-a-real-tool')
    assert.match(await page.locator('[data-tool-pins]').innerText(), /没有匹配的工具/)
    await search.fill('')
    await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))))
    assert.equal(await page.evaluate(() => document.documentElement.classList.contains('dark')), scheme === 'dark')
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    if (shots) {
      await page.screenshot({ path: `${shots}/tool-pins-${scheme}-${width}.png` })
      await search.fill('steve_remember')
      await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))))
      await page.screenshot({ path: `${shots}/tool-pins-action-${scheme}-${width}.png` })
    }
    full = true
    await open()
    await page.locator('[data-tool-pin-search]').fill('steve_remember')
    assert.equal(await page.locator('[data-tool-pin="tool:steve_remember"]').isDisabled(), true)
    await page.locator('[data-tool-pin-search]').fill('known_0')
    assert.equal(await page.locator('[data-tool-pin="tool:known_0"]').isEnabled(), true, 'full slots still permit removing a pin')
    full = false; failList = true
    await page.reload()
    await page.getByText('Fixture list unavailable', { exact: false }).waitFor()
    failList = false
    await page.getByRole('button', { name: '重试', exact: true }).click()
    await page.locator('[data-tool-pins]').waitFor()
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log('tool pins: pin/unpin, server pins, errors/retry, core, usage, limits and four layouts passed')
} finally { await browser.close() }
