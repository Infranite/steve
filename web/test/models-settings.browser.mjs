import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* a mock of GET/PATCH /api/settings/models that resolves like the Rust Hub (models.rs `resolve`): no override
 * follows Steve; an override with no models uses Steve's models on Steve's harness, else that harness's defaults;
 * with no effort, Steve's effort on Steve's harness. */
const now = Date.now(), min = 60000
const PURPOSES = [['route', '分流'], ['chat', '对话回答'], ['research', '就地研究'], ['title', '起标题'], ['extract', '记忆提取'], ['disclosure', '披露检查'], ['yolo', 'YOLO 代答'], ['draft', '草稿/配置'], ['flow', '流程 Bot 节点'], ['mail', '邮件起草'], ['channel', '渠道回复']]
const fast = { harness: 'claude-code', models: ['claude-haiku-5-5', 'claude-sonnet-5-5'], effort: null }
const steve = { bot_id: 'steve', name: 'Steve', harness: 'codex', model: 'gpt-6-astra', bot_effort: null, runs_on: 'codex', models: ['gpt-6-astra'], effort: null, note: null, model_note: null }
const harnesses = [
  { id: 'claude-code', name: 'Claude Code', efforts: [{ key: 'low', label: '低' }, { key: 'medium', label: '中' }, { key: 'high', label: '高' }, { key: 'max', label: '最高' }], default_models: ['claude-sonnet-5-5'] },
  { id: 'codex', name: 'Codex', efforts: [{ key: 'low', label: '低' }, { key: 'medium', label: '中' }, { key: 'high', label: '高' }], default_models: ['gpt-6-astra'] },
]
let custom = { route: fast, title: fast, disclosure: fast, flow: fast }
const recent = {
  route: { last: { harness: 'claude-code', model: 'claude-haiku-5-5', at: now - 3 * min }, used: [{ harness: 'claude-code', model: 'claude-haiku-5-5', calls: 29, failed: 0, at: now - 3 * min }, { harness: 'claude-code', model: 'claude-sonnet-5-5', calls: 1, failed: 1, at: now - 9 * min }] },
  chat: { last: { harness: 'codex', model: 'gpt-6-astra', at: now - 240 * min }, used: [{ harness: 'codex', model: 'gpt-6-astra', calls: 2, failed: 0, at: now - 240 * min }] },
  research: { last: { harness: 'codex', model: 'gpt-6-astra', at: now - 5 * min }, used: [{ harness: 'codex', model: 'gpt-6-astra', calls: 7, failed: 0, at: now - 5 * min }, { harness: 'claude-code', model: 'gpt-6-astra', calls: 4, failed: 4, at: now - 50 * min }, { harness: 'claude-code', model: 'claude-sonnet-5-5', calls: 1, failed: 0, at: now - 900 * min }] },
  extract: { last: { harness: 'codex', model: 'gpt-6-astra', at: now - 120 * min }, used: [{ harness: 'codex', model: 'gpt-6-astra', calls: 5, failed: 0, at: now - 120 * min }, { harness: 'claude-code', model: 'claude-sonnet-5-5', calls: 3, failed: 0, at: now - 600 * min }] },
}
const resolve = (k) => {
  const c = custom[k]; if (!c) return { harness: steve.runs_on, models: steve.models, effort: steve.effort }
  const same = c.harness === steve.runs_on
  return { harness: c.harness, models: c.models.length ? c.models : same ? steve.models : harnesses.find((h) => h.id === c.harness).default_models, effort: c.effort ?? (same ? steve.effort : null) }
}
const view = () => ({
  steve, harnesses, max: 3,
  purposes: PURPOSES.map(([key, label]) => ({ key, label, desc: `${label}的说明`, value: custom[key] ?? null, follows: !custom[key], effective: resolve(key), recommended: ['route', 'title', 'disclosure', 'flow'].includes(key) ? fast : null, codex_note: null, recent: recent[key] ?? null })),
  agents: [{ harness: 'claude-code', name: 'Claude Code', models: [], default: 'claude-sonnet-5-5', value: null }, { harness: 'codex', name: 'Codex', models: [], default: null, value: null }],
})
const catalog = (h) => ({ harness: h, name: h, source: 'mock', default: harnesses.find((x) => x.id === h).default_models[0], families: ['main'], models: (h === 'codex' ? ['gpt-6-astra', 'gpt-7'] : ['claude-haiku-5-5', 'claude-sonnet-5-5', 'claude-opus-5-5']).map((id) => ({ id, label: id, family: 'main', via: null, note: null, efforts: [] })) })
const patches = []

const run = async ({ width, height, scheme, flow }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method()
    let data = {}
    if (u.pathname === '/api/settings/models' && method === 'PATCH') {
      const b = route.request().postDataJSON(); patches.push(b)
      for (const [k, c] of Object.entries(b.purposes ?? {})) { if (c) custom[k] = c; else delete custom[k] }
      data = view()
    } else if (u.pathname === '/api/settings/models') data = view()
    else if (u.pathname === '/api/system/models') data = catalog(u.searchParams.get('harness'))
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
  const sheet = page.locator('[data-slot="sheet-content"]')
  try {
    await page.goto(base + '/test/fixtures/models-settings.html'); await page.locator('[data-models-default]').waitFor()
    // 默认配置 + 4 exceptions; the 7 followers folded; history not in the config list
    assert.match(await page.locator('[data-models-default]').innerText(), /Codex · gpt-6-astra/)
    assert.equal(await page.locator('[data-models-overrides] [data-custom]').count(), 4)
    assert.equal(await page.locator('[data-follows-toggle]').innerText(), '其余 7 项跟随默认')
    assert.equal(await page.locator('[data-models-overrides] [data-follows]').count(), 0)
    assert.equal(await page.locator('[data-models-overrides]').getByText('最近 7 天').count(), 1, 'only the failure notice mentions history')
    assert.equal(await page.getByText('推荐', { exact: false }).count(), 0)
    assert.match(await page.locator('[data-purpose=route] [data-brief]').innerText(), /Claude Code · haiku-5-5 · 备用 1 个/)
    assert.match(await page.locator('[data-models-failures]').innerText(), /5 次调用失败（分流 1、就地研究 4）/)
    assert.equal(await page.locator('[data-models-log]').getAttribute('data-models-log'), 'closed')
    await fits(); await shot('models')
    await page.locator('[data-follows-toggle]').click(); assert.equal(await page.locator('[data-models-overrides] [data-follows]').count(), 7); await fits()
    await page.locator('[data-follows-toggle]').click()

    // the side panel: fallback models are under 高级选项
    await page.locator('[data-purpose=route]').click(); await sheet.waitFor()
    assert.equal(await sheet.locator('[data-model-item]').count(), 0, 'fallbacks hidden until 高级选项 opens')
    assert.match(await sheet.locator('[data-adv-toggle]').innerText(), /备用 1 个/)
    await fits(); await shot('models-sheet')
    await sheet.locator('[data-adv-toggle]').click(); await sheet.locator('[data-model-item="claude-sonnet-5-5"]').waitFor(); await shot('models-sheet-advanced')
    if (!flow) { assert.deepEqual(errors, []); return }
    await sheet.getByRole('button', { name: '移除 claude-sonnet-5-5' }).click()
    await sheet.locator('[data-models-save]').click(); await sheet.waitFor({ state: 'detached' })
    assert.deepEqual(patches.at(-1), { purposes: { route: { harness: 'claude-code', models: ['claude-haiku-5-5'], effort: null } } })
    assert.equal(await page.locator('[data-purpose=route] [data-brief]').innerText(), 'Claude Code · haiku-5-5')

    // a new override on Steve's harness that only sets the effort keeps following Steve's model
    await page.locator('[data-add-override]').click(); await page.locator('[data-add-purpose=chat]').click(); await sheet.waitFor()
    assert.equal(await sheet.locator('[data-seg=own]').getAttribute('aria-selected'), 'true')
    assert.match(await sheet.locator('[data-model-picker=model-chat]').innerText(), /跟随默认（gpt-6-astra）/)
    await sheet.locator('[data-select=effort-chat]').click(); await page.getByRole('option', { name: '高', exact: true }).click()
    await sheet.locator('[data-models-save]').click(); await sheet.waitFor({ state: 'detached' })
    assert.deepEqual(patches.at(-1), { purposes: { chat: { harness: 'codex', models: [], effort: 'high' } } })
    assert.equal(await page.locator('[data-purpose=chat] [data-brief]').innerText(), 'Codex · gpt-6-astra · 思考高')
    assert.equal(await page.locator('[data-follows-toggle]').innerText(), '其余 6 项跟随默认')

    // change the default: followers and the model-less override move with it, explicit overrides do not
    steve.model = 'gpt-7'; steve.models = ['gpt-7']; await page.reload(); await page.locator('[data-models-default]').waitFor()
    assert.match(await page.locator('[data-models-default]').innerText(), /Codex · gpt-7/)
    assert.equal(await page.locator('[data-purpose=chat] [data-brief]').innerText(), 'Codex · gpt-7 · 思考高')
    assert.equal(await page.locator('[data-purpose=route] [data-brief]').innerText(), 'Claude Code · haiku-5-5')
    await page.locator('[data-follows-toggle]').click(); await page.locator('[data-purpose=research]').click(); await sheet.waitFor()
    assert.match(await sheet.locator('[data-follow-preview]').innerText(), /Codex · gpt-7/); await page.keyboard.press('Escape'); await sheet.waitFor({ state: 'detached' })

    // back to the default
    await page.locator('[data-purpose=title]').click(); await sheet.waitFor()
    await sheet.locator('[data-seg=follow]').click(); await sheet.locator('[data-models-save]').click(); await sheet.waitFor({ state: 'detached' })
    assert.deepEqual(patches.at(-1), { purposes: { title: null } })
    assert.equal(await page.locator('[data-models-overrides] [data-custom][data-purpose=title]').count(), 0)
    assert.equal(await page.locator('[data-follows-toggle]').innerText(), '其余 7 项跟随默认')

    // 运行记录: the failure notice opens it; history on a harness/model no longer configured is marked
    await page.locator('[data-models-failures]').click(); await page.locator('[data-models-log=open]').waitFor()
    assert.equal(await page.locator('[data-log-purpose=research] [data-log-stale]').count(), 3, 'the default moved to gpt-7, so every research call ran on something else')
    assert.equal(await page.locator('[data-log-purpose=chat] [data-log-stale]').count(), 1, 'gpt-6-astra is no longer what 对话回答 uses')
    assert.match(await page.locator('[data-log-idle]').innerText(), /没有调用：起标题/)
    await fits(); await shot('models-log')
    assert.deepEqual(errors, [])
  } catch (e) { console.error(await page.locator('body').innerText(), errors); throw e } finally { await page.close() }
}
try {
  await run({ width: 1280, height: 900, scheme: 'light', flow: true })
  steve.model = 'gpt-6-astra'; steve.models = ['gpt-6-astra']; custom = { route: fast, title: fast, disclosure: fast, flow: fast }
  for (const [width, height] of [[1280, 900], [390, 844]]) for (const scheme of ['light', 'dark']) await run({ width, height, scheme, flow: false })
  console.log('PASS: default + exceptions, folded followers, side panel with fallbacks under 高级选项, save/inherit/revert, 运行记录 apart from config, 1280/390 × light/dark')
} finally { await browser.close() }
