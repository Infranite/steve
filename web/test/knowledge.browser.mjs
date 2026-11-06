import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* ST-41: 长期记忆 and 知识条目 are separate tabs; a candidate says what it would become and why it is worth keeping */
const mem = (id, kind, text, extra = {}) => ({ id, kind, text, source_label: 'ST-33 交付时', level: 'conclusion', status: 'confirmed', visibility: 'private', about: [], created: 1, ...extra })
const mems = [
  mem('m1', 'preference', '主会话收到需求时直接给出方案，不再每次询问是在当前会话出方案还是创建卡片。'),
  mem('m2', 'rule', 'Steve 发版走 GitHub 正常的 Release 流程，不在本机发版，也不调用 steve_publish。', { source_label: 'ST-26 交付时' }),
  mem('m3', 'fact', 'Infranite/steve 仓库的 PR 不跑 CI，只有推送版本标签才触发发布流程；合并前靠本地检查把关。', { source_label: 'ST-27 交付时' }),
]
const candidates = [
  { mem: mem('c1', 'rule', 'Agent 出现异常时，先由 Steve 判断能否处理，处理不了再通知用户。', { status: 'candidate', source_label: '11-04 的对话' }), origin: 'chat', origin_label: '11-04 的对话', evidence: '后续这种 agent 的异常，最好是先找 steve 判断能否处理，处理不了再通知用户', supersedes: null, decision_id: 'd1', record: '长期记忆', why: '以后设计异常通知、自动续跑时都按这个顺序', scope: 'Agent 停下、失败等异常' },
  { mem: mem('c2', 'fact', '事项页排队列表把每条都按评论渲染；遇到“评论排版很丑”，先确认那段是不是任务说明。', { status: 'candidate', source_label: 'ST-16 交付时' }), origin: 'item', origin_label: 'ST-16 交付时', evidence: '排队列表把每一条都当用户评论，按 Markdown 整段平铺出来', supersedes: null, decision_id: 'd2', record: '知识条目', why: '再有类似排版反馈时先判断原因，避免误改渲染', scope: '排查事项页评论排版问题' },
]
const expired = [{ mem: mem('x1', 'rule', '周报每周五下午发给团队。', { status: 'stale', source_label: '10-01 的对话' }), origin_label: '10-01 的对话', evidence: '以后周报周五下午发', expired: 1 }]
const added = [], restored = []
const run = async ({ width, height, scheme }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method()
    let data = []
    if (u.pathname === '/api/kb/mems' && method === 'POST') { const b = route.request().postDataJSON(); added.push(b); data = mem('n' + added.length, b.kind, b.text) }
    else if (u.pathname === '/api/kb/mems') data = mems
    else if (u.pathname === '/api/kb/candidates') data = candidates
    else if (u.pathname === '/api/kb/candidates/expired') data = restored.length ? [] : expired
    else if (u.pathname.endsWith('/restore') && method === 'POST') { restored.push(u.pathname); data = { ok: true } }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
  try {
    await page.goto(base + '/test/fixtures/knowledge.html'); await page.locator('[data-kb-tab="长期记忆"]').waitFor()
    // 长期记忆: preferences and rules only
    await page.locator('[data-mem="m1"]').waitFor()
    assert.equal(await page.locator('[data-mem]').count(), 2)
    assert.equal(await page.locator('[data-mem="m3"]').count(), 0)
    await page.getByText('一次性要求留在那件事的记录里', { exact: false }).waitFor()
    await fits(); await shot('kb-long-term')
    // 知识条目: facts only; what you add there is a fact
    await page.locator('[data-seg="知识条目"]').click()
    await page.locator('[data-kb-tab="知识条目"] [data-mem="m3"]').waitFor()
    assert.equal(await page.locator('[data-mem]').count(), 1)
    await page.locator('[data-mem-input]').fill('Acme 的合同每年 3 月续签'); await page.locator('[data-mem-add]').click()
    await page.waitForFunction(() => !document.querySelector('[data-mem-input]')?.value)
    assert.equal(added.at(-1)?.kind, 'fact')
    await fits(); await shot('kb-knowledge')
    // 待确认: each candidate says what it becomes and why
    await page.locator('[data-seg="待确认"]').click()
    await page.locator('[data-candidate="c1"]').waitFor()
    assert.deepEqual(await page.locator('[data-cand-record]').allInnerTexts(), ['长期记忆', '知识条目'])
    assert.match(await page.locator('[data-candidate="c1"] [data-cand-why]').innerText(), /为什么值得保留：以后设计异常通知/)
    assert.equal(await page.locator('[data-cand-yes]').first().innerText(), '保存')
    await fits(); await shot('kb-candidates')
    // expired ones are folded away below, and one click puts one back
    await page.locator('[data-cand-expired-toggle]').click()
    await page.locator('[data-cand-expired-row="x1"] [data-cand-restore]').click()
    await page.waitForFunction(() => !document.querySelector('[data-cand-expired]'))
    assert.deepEqual(restored.splice(0), ['/api/kb/candidates/x1/restore'])
    // the link from 待处理 opens 待确认 directly
    await page.goto(base + '/test/fixtures/knowledge.html?via=link#/knowledge?tab=pending'); await page.locator('[data-kb-tab="待确认"] [data-candidate="c1"]').waitFor()
    // the 待处理 card: the question names the record; an old decision still reads
    await page.goto(base + '/test/fixtures/knowledge.html?inbox'); await page.locator('[data-fixture-cards]').waitFor()
    assert.deepEqual(await page.locator('[data-card-ask]').allInnerTexts(), ['存为长期记忆吗？', '记住这条吗？'])
    assert.match(await page.locator('[data-fixture-card="0"] [data-mem-confirm]').innerText(), /^主会话收到需求时/)
    assert.match(await page.locator('[data-fixture-card="1"] [data-mem-confirm]').innerText(), /^用户希望卡片详情/)
    await fits(); await shot('inbox-card')
    assert.deepEqual(errors, [])
  } finally { await page.close() }
}
try {
  for (const v of [{ width: 1280, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }]) await run(v)
  console.log('knowledge ok')
} finally { await browser.close() }
