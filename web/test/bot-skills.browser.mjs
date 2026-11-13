import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
import { capsHub, skillEntry } from './caps-mock.mjs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* A Bot's 能力 → 技能 against a mocked Hub: skills live in the workspace's library and a Bot is given some. A Bot
 * without any says in one sentence what a skill is, and one whose harness reads none says so; a Bot's skills are cards
 * (description, this Bot's switch saving at once); the dialog edits a skill with a Markdown preview, a pasted SKILL.md
 * fills a new one that goes into the library and to this Bot, and taking one off the Bot leaves it in the library.
 * Steve · 内置's tools each say what they do, with this Bot's calls in 30 days. */
const bot = (id, name, extra = {}) => ({ id, name, role: name + '的职责', main: id === 'steve', harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0, ...extra })
let bots, hub
const reset = () => {
  bots = [bot('steve', 'Steve'), bot('dev', '开发'), bot('doc', '文档', { harness: 'my-agent' })]
  hub = capsHub({
    mcp: [], a2a: [],
    skill: [
      skillEntry('bskill_1', 'code-review', '审查一次改动：合并前、或者有人请你看 PR 时用。', '# 审查\n\n1. 先读 diff。\n2. 跑测试。', { bots: [{ id: 'dev', enabled: true }] }),
      skillEntry('bskill_2', 'release-notes', 'Write the release notes for a change the user wants released, in the project\'s own format.', '## Steps\n- Read the changes', { bots: [{ id: 'dev', enabled: true }] }),
    ],
  })
}
const state = () => ({ version: 'fixture', web_version: 'fixture', workspace: { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }, workspaces: [{ id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [],
  runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '', skills: true }, { id: 'codex', name: 'Codex', ok: true, version: '', path: '', models: [], note: '', skills: true }, { id: 'my-agent', name: 'My Agent', ok: true, version: '', path: '', models: [], note: '', skills: false, custom: true }] })
const toolRows = [
  { key: 'mcp:steve_update', label: 'steve_update', count: 12, dim: 'mcp' },
  { key: 'mcp:steve_items_search', label: 'steve_items_search', count: 5, dim: 'mcp' },
]

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
    if (p === '/api/stats/breakdown') return json(200, { by: 'tool', rows: u.searchParams.get('bot') === 'dev' ? toolRows : [] })
    if (await hub.handle(route, method, p)) return
    return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
  })
  const calls = hub.calls
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/botskills-${name}-${width}-${scheme}.png`, fullPage: true }) }
  const shotDialog = async (name) => { if (shots) await page.screenshot({ path: `${shots}/botskills-${name}-${width}-${scheme}.png` }) }
  const caps = async (id) => {
    await page.goto(base + '/#/bots/' + id)
    await page.locator('[data-slot=tabs-trigger]:has-text("能力")').click()
    await page.locator('[data-skills]').waitFor()
  }
  const settle = async (n) => { for (let i = 0; i < 50 && calls.length < n; i++) await page.waitForTimeout(100) }
  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })

  /* no skills, on a harness that reads none: what a skill is, and that this harness won't use them */
  await caps('doc')
  await page.locator('[data-skills-empty]').waitFor()
  assert.match(await page.locator('[data-skills-empty]').innerText(), /技能是一份写给 Bot 的做事说明/)
  assert.match(await page.locator('[data-skills-unread]').innerText(), /My Agent 不读技能/)
  assert.equal(await page.locator('[data-planned]').count(), 0)
  await page.locator('[data-skills]').scrollIntoViewIfNeeded(); await page.waitForTimeout(600)
  await fits(); await shot('empty')

  /* Steve · 内置's tools say what they do, with this Bot's calls; one Steve added later says what the Hub says */
  await caps('dev')
  await page.locator('[data-steve-fold] button, [data-steve-fold] summary').first().click()
  const tool = (id) => page.locator(`[data-steve-tool="${id}"]`)
  await tool('steve_items_search').waitFor()
  assert.match(await tool('steve_items_search').innerText(), /按编号或关键词查找事项[^]*30 天 5 次/)
  assert.match(await tool('steve_memory_list').innerText(), /列出你的记忆/)
  assert.match(await tool('steve_ask').innerText(), /30 天没用过/)
  assert.match(await tool('steve_brand_new').innerText(), /A tool added later\./)
  await fits(); await shot('tools')

  /* the Bot's skills: a card each, no warning on a harness that reads them */
  await page.locator('[data-bot-skill=code-review]').waitFor()
  assert.equal(await page.locator('[data-skills] [data-bot-skill]').count(), 2)
  assert.equal(await page.locator('[data-skills-unread]').count(), 0)
  assert.match(await page.locator('[data-bot-skill=code-review]').innerText(), /合并前/)
  await page.locator('[data-skills]').scrollIntoViewIfNeeded(); await page.waitForTimeout(600)
  await fits(); await shot('list')

  /* the switch is this Bot's, and saves at once */
  await page.locator('[data-bot-skill=release-notes] [data-isc-switch]').click()
  await settle(1)
  assert.deepEqual(calls[0], ['PUT', 'dev', 'skill', 'bskill_2', { enabled: false }])
  await page.locator('[data-bot-skill=release-notes][data-bot-skill-on=false]').waitFor()

  /* edit, with the Markdown preview */
  await page.locator('[data-cap-edit=code-review]').click()
  const dlg = page.locator('[data-skill-dialog]')
  await dlg.waitFor()
  assert.equal(await dlg.locator('input[data-skill-name], [data-skill-name] input').first().inputValue(), 'code-review')
  await dlg.locator('textarea[data-skill-content], [data-skill-content] textarea').first().fill('# 审查\n\n1. 先读 diff。\n2. 跑测试。\n3. 写下结论。')
  await dlg.locator('[data-seg=preview]').click()
  await dlg.locator('[data-skill-preview] h1').waitFor()
  assert.match(await dlg.locator('[data-skill-preview]').innerText(), /写下结论/)
  await fits(); await shotDialog('edit')
  await dlg.locator('[data-skill-save]').click()
  await settle(2)
  assert.deepEqual(calls[1], ['PATCH', 'skill', 'bskill_1', { name: 'code-review', description: '审查一次改动：合并前、或者有人请你看 PR 时用。', content: '# 审查\n\n1. 先读 diff。\n2. 跑测试。\n3. 写下结论。' }])
  await dlg.waitFor({ state: 'detached' })

  /* a pasted SKILL.md fills a new skill, which goes into the library and to this Bot */
  await page.locator('[data-cap-add=skill]').click()
  await page.locator('[data-cap-import=skill]').click()
  const imp = page.locator('[data-skill-import-dialog]')
  await imp.waitFor()
  await imp.locator('textarea[data-skill-paste]').fill('---\nname: Deploy Check\ndescription: >\n  Check a deploy\n  before announcing it.\nlicense: MIT\n---\n\n# Deploy check\n\n- Look at the dashboard.\n')
  await fits(); await shotDialog('import')
  await imp.locator('[data-skill-import-next]').click()
  await dlg.waitFor()
  assert.equal(await dlg.locator('input[data-skill-name], [data-skill-name] input').first().inputValue(), 'deploy-check')
  assert.equal(await dlg.locator('textarea[data-skill-description-edit], [data-skill-description-edit] textarea').first().inputValue(), 'Check a deploy before announcing it.')
  await dlg.locator('[data-skill-save]').click()
  await page.locator('[data-skills] [data-bot-skill=deploy-check]').waitFor()
  assert.deepEqual(calls[2], ['POST', 'skill', { name: 'deploy-check', description: 'Check a deploy before announcing it.', content: '# Deploy check\n\n- Look at the dashboard.', bot: 'dev' }])

  /* a name a harness cannot take is caught before saving */
  await page.locator('[data-cap-add=skill]').click()
  await page.locator('[data-cap-new=skill]').click()
  await dlg.waitFor()
  await dlg.locator('input[data-skill-name], [data-skill-name] input').first().fill('Bad Name')
  await dlg.locator('textarea[data-skill-description-edit], [data-skill-description-edit] textarea').first().fill('x')
  assert.equal(await dlg.locator('[data-skill-save]').isDisabled(), true)
  await dlg.getByRole('button', { name: '取消' }).click()
  await dlg.waitFor({ state: 'detached' })

  /* off this Bot: it stays in the library */
  await page.locator('[data-cap-drop=deploy-check]').click()
  await page.locator('[data-skills] [data-bot-skill=deploy-check]').waitFor({ state: 'detached' })
  assert.deepEqual(calls[3], ['UNASSIGN', 'dev', 'skill', 'bskill_101'])
  await page.locator('[data-skills]').scrollIntoViewIfNeeded(); await page.waitForTimeout(600)
  await fits(); await shot('caps')

  assert.deepEqual(errors, [])
  await page.close()
}

try {
  const sizes = process.env.STEVE_TEST_ALL ? [[1440, 900], [390, 844]].flatMap(([width, height]) => ['light', 'dark'].map((scheme) => ({ width, height, scheme }))) : [{ width: 1440, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]
  for (const v of sizes) await run(v)
  console.log('bot-skills: ok')
} finally { await browser.close() }
