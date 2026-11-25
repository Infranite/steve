import assert from 'node:assert/strict'
import { mkdirSync } from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

/* Redefined knowledge page: 人 · 组织 · 项目 · 偏好与规则 · 资料 · 待确认 · 整理,
 * separated knowledge vs records search, deterministic steward threshold, and project AGENTS.md proposals. */
const mem = (id, kind, text, extra = {}) => ({ id, kind, text, source_label: 'ST-33 交付时', level: 'conclusion', status: 'confirmed', visibility: 'private', about: [], created: 1, ...extra })
const mems = [
  mem('m1', 'preference', '主会话收到需求时直接给出方案，不再每次询问是在当前会话出方案还是创建卡片。', { about: [{ type: 'party', id: 'ct_1', name: '李总' }], used: { count: 3, last: 1 },
    origin: { kind: 'conversation', type: 'message', id: 'chat:2026-11-04', label: '11-04 的对话', href: '', writer: 'steve', writer_name: 'Steve', evidence: '直接出方案就行' } }),
  mem('m2', 'rule', '给 Acme 发邮件一律用英文。', { about: [{ type: 'party', id: 'org_1', name: 'Acme' }], source_label: 'ST-26 交付时' }),
  mem('m3', 'fact', 'Acme 每年三月续签年度合同。', { about: [{ type: 'party', id: 'org_1', name: 'Acme' }], source_label: 'ST-27 交付时' }),
]
const candidates = [
  { mem: mem('c1', 'rule', 'Agent 出现异常时，先由 Steve 判断能否处理，处理不了再通知用户。', { status: 'candidate', source_label: '11-04 的对话' }), origin: 'chat', origin_label: '11-04 的对话', evidence: '后续这种 agent 的异常，最好是先找 steve 判断能否处理，处理不了再通知用户', supersedes: null, decision_id: 'd1', record: '长期记忆', why: '以后设计异常通知、自动续跑时都按这个顺序', scope: 'Agent 停下、失败等异常' },
  { mem: mem('c2', 'fact', '李总负责华东区关键客户续签。', { status: 'candidate', about: [{ type: 'party', id: 'ct_1', name: '李总' }], source_label: 'ST-16 交付时' }), origin: 'item', origin_label: 'ST-16 交付时', evidence: '华东区续签找李总拍板', supersedes: null, decision_id: 'd2', record: '知识条目', why: '以后跟进续签时知道找谁', scope: '华东区客户续签' },
]
const expired = [{ mem: mem('x1', 'rule', '周报每周五下午发给团队。', { status: 'stale', source_label: '10-01 的对话' }), origin_label: '10-01 的对话', evidence: '以后周报周五下午发', expired: 1 }]
const subjects = [
  { type: 'party', id: 'ct_1', name: '李总', href: '#/contacts/ct_1', category: 'person', party_kind: 'contact', mems: 2, candidates: 1, docs: 0, proposals: 0, last: 1 },
  { type: 'party', id: 'org_1', name: 'Acme', href: '#/contacts/org_1', category: 'org', party_kind: 'org', mems: 2, candidates: 0, docs: 1, proposals: 0, last: 1 },
  { type: 'project', id: 'prj_1', name: 'steve', href: '#/projects/prj_1', category: 'project', party_kind: null, mems: 0, candidates: 0, docs: 2, proposals: 2, last: 1 },
]
const sources = [
  { id: 'ks_notes', kind: 'notes', name: '我的笔记', visibility: 'private', level: 'conclusion', paused: false, builtin: true, status: 'ok', last_sync: 1, last_error: '', stats: { docs: 1, chunks: 2, skipped: 0 } },
  { id: 'ks_files', kind: 'files', name: '上传的文件', visibility: 'private', level: 'conclusion', paused: false, builtin: true, status: 'ok', last_sync: 1, last_error: '', stats: { docs: 0, chunks: 0, skipped: 0 } },
  { id: 'ks_mems', kind: 'mems', name: '记忆', visibility: 'private', level: 'conclusion', paused: false, builtin: true, status: 'ok', last_sync: 1, last_error: '', stats: { docs: 0, chunks: 3, skipped: 0 } },
]
const docs = [
  { id: 'doc1', title: 'Acme 合同说明', source: { id: 'ks_notes', kind: 'notes', name: '我的笔记' }, path: null, visibility: 'private', level: 'conclusion', status: 'active', chunks: 1, updated: 1 },
]
const providers = { memory: [{ kind: 'you', writer: '', writer_name: null, count: 2, confirmed: 2, candidates: 0, stale: 0, last: 1 }, { kind: 'conversation', writer: 'steve', writer_name: 'Steve', count: 1, confirmed: 0, candidates: 1, stale: 0, last: 1 }], store: 'openviking' }
const runs = [{ run_id: 'ST-44', href: '#/items/ST-44', title: '整理知识', writer: 'bot_k', writer_name: '知识管家', at: 1, changes: [
  { id: 'ch1', op: 'merge', state: 'pending', reason: '说的是同一件事', mem: mem('mm_new', 'rule', '提交信息用中文写', { status: 'candidate' }), targets: [mem('a', 'rule', '提交信息用中文'), mem('b', 'rule', 'commit message 写中文')], after: {}, created: 1 },
  { id: 'ch2', op: 'flag', state: 'open', reason: '看不出哪条对', mem: mem('f1', 'rule', '周报周五发'), targets: [mem('f2', 'rule', '周报周一发')], after: {}, created: 2 },
  { id: 'ch3', op: 'stale', state: 'applied', reason: '早就不用了', mem: mem('s1', 'rule', '用 Jenkins 构建', { status: 'stale' }), targets: [], after: {}, created: 3 },
] }]
const schedule = { id: 'fl_1', name: '整理知识', when: '每周日 03:00', describe: '每周日 03:00', cron: '0 3 * * 0', tz: 'Asia/Shanghai', what: 'x', bot: 'bot_k', mode: 'item_auto', project_id: null, tags: [], missed: 'catch_up_once', overlap: 'skip', budget_usd: 5, enabled: true, next_at: Date.now() + 86400000, last: null, version: 1, created: 1, updated: 1 }

const run = async ({ width, height, scheme, locale = "zh-CN" }) => {
  const added = [], restored = [], patched = [], deleted = [], acted = [], stewardPuts = [], proposalActs = []
  let stewardThreshold = 3
  const proposals = [
    { id: 'prop_1', project_id: 'prj_1', project_name: 'steve', kind: 'agents_md', text: 'PR 合并前先运行 cargo check --workspace --all-targets', reason: '避免未编译警告进主分支', evidence: '合并前靠本地检查把关', status: 'pending', origin_kind: 'item', origin_id: 'ST-27', origin_label: 'ST-27 交付时', origin_href: '#/items/ST-27', writer: 'steve', decision_id: 'dp1', created: 1, updated: 1 },
    { id: 'prop_2', project_id: 'prj_1', project_name: 'steve', kind: 'agents_md', text: '发版走 GitHub Release 流程，不从本机直接打 tag', reason: '统一走自动发布流水线', evidence: '只有推送版本标签才触发发布流程', status: 'pending', origin_kind: 'item', origin_id: 'ST-26', origin_label: 'ST-26 交付时', origin_href: '#/items/ST-26', writer: 'steve', decision_id: 'dp2', created: 2, updated: 2 },
  ]
  const page = await browser.newPage({ locale, viewport: { width, height }, colorScheme: scheme, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  const errors = []; page.on('pageerror', (e) => errors.push(e.message))
  await page.addInitScript(() => {
    const f = window.fetch
    window.fetch = (u, o) => {
      if (!o?.keepalive) return f(u, o)
      sessionStorage.setItem('kb-leave', JSON.stringify([...JSON.parse(sessionStorage.getItem('kb-leave') || '[]'), [o.method, String(u)]]))
      return Promise.resolve(new Response('{"ok":true}', { headers: { 'content-type': 'application/json' } }))
    }
  })
  const leaveDeletes = () => page.evaluate(() => { const x = JSON.parse(sessionStorage.getItem('kb-leave') || '[]'); sessionStorage.removeItem('kb-leave'); return x.map(([m, u]) => m + ' ' + new URL(u, location.href).pathname) })
  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method()
    let data = []
    if (u.pathname === '/api/kb/mems' && method === 'POST') { const b = route.request().postDataJSON(); added.push(b); data = mem('n' + added.length, b.kind, b.text) }
    else if (u.pathname === '/api/kb/mems') data = mems
    else if (u.pathname.startsWith('/api/mems/') && method === 'PATCH') { const id = decodeURIComponent(u.pathname.slice(10)); const b = route.request().postDataJSON(); patched.push([id, b]); data = { ...mems.find((m) => m.id === id), ...b } }
    else if (u.pathname.startsWith('/api/mems/') && method === 'DELETE') { deleted.push(decodeURIComponent(u.pathname.slice(10))); data = { ok: true } }
    else if (u.pathname === '/api/kb/candidates') data = candidates
    else if (u.pathname === '/api/kb/subjects') data = subjects
    else if (u.pathname === '/api/kb/sources') data = sources
    else if (u.pathname === '/api/kb/docs') data = docs
    else if (u.pathname === '/api/kb/providers') data = providers
    else if (u.pathname === '/api/kb/changes') data = runs
    else if (u.pathname === '/api/kb/steward' && method === 'PUT') {
      const b = route.request().postDataJSON()
      stewardPuts.push(b)
      if (typeof b.threshold === 'number') stewardThreshold = b.threshold
      data = { schedule_id: 'fl_1', threshold: stewardThreshold, signals: { new_candidates: 2, conflicts: 1, total: 3 }, last_run_at: 1, skipped_runs: 4 }
    }
    else if (u.pathname === '/api/kb/steward') data = { schedule_id: 'fl_1', threshold: stewardThreshold, signals: { new_candidates: 2, conflicts: 1, total: 3 }, last_run_at: 1, skipped_runs: 4 }
    else if (u.pathname === '/api/kb/proposals' && method === 'GET') {
      const pid = u.searchParams.get('project_id')
      data = pid ? proposals.filter((p) => p.project_id === pid) : proposals
    }
    else if (u.pathname.startsWith('/api/kb/proposals/') && u.pathname.endsWith('/resolve') && method === 'POST') {
      const id = decodeURIComponent(u.pathname.slice('/api/kb/proposals/'.length, -'/resolve'.length))
      const b = route.request().postDataJSON()
      proposalActs.push([id, b])
      const target = proposals.find((p) => p.id === id)
      if (target) target.status = b.choice === 'yes' ? 'accepted' : 'dismissed'
      data = { ok: true, proposal: target }
    }
    else if (u.pathname === '/api/schedules') data = [schedule]
    else if (u.pathname.startsWith('/api/kb/changes/') || u.pathname === '/api/kb/candidates/resolve') { acted.push([u.pathname, route.request().postDataJSON?.() ?? null]); data = { ok: true, done: 1 } }
    else if (u.pathname === '/api/kb/candidates/expired') data = restored.length ? [] : expired
    else if (u.pathname.endsWith('/restore') && method === 'POST') { restored.push(u.pathname); data = { ok: true } }
    else if (u.pathname === '/api/kb/search') {
      const q = u.searchParams.get('q')
      const scope = u.searchParams.get('scope') || 'knowledge'
      const hits = q === '续签' && scope !== 'records'
        ? [{ n: 1, type: 'chunk', id: 'doc1', seq: 0, title: 'Acme 合同说明', heading: null, text: 'Acme 的合同每年三月续签', label: '笔记', level: 'conclusion', href: '#/knowledge/doc/doc1' }]
        : []
      const records = q === '续签' && scope !== 'knowledge'
        ? [
          { record_kind: 'item', id: 'ST-12', title: '给 Acme 写续签邮件', text: '续签邮件草稿已完成', href: '#/items/ST-12', updated: 1 },
          { record_kind: 'chat', id: 'msg_1', title: '主线对话', text: '下周确认续签条款', href: '#/chat', updated: 1 },
        ]
        : []
      data = { hits, records, took_ms: 3, semantic_down: true }
    }
    else if (u.pathname === '/api/stats/summary') {
      data = { throughput: { created: 0, done: 0, wip: 0 }, quality: { fails: {}, first_pass: 0, first_pass_n: 0 }, spend: { est: 0, reported: 0 }, per_item: { est_avg: 0 }, usage: { turns: 0, turn_ms: 0 }, time: { wait: { n: 0, sum: 0 } }, human: { decisions: 0 } }
    }
    else if (u.pathname === '/api/github') {
      data = { repos: [], installations: [] }
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
  try {
    await page.goto(base + '/test/fixtures/knowledge.html'); await page.locator('[data-kb-tab="rules"]').waitFor()
    // 偏好与规则: preferences and rules only (facts do not appear here)
    await page.locator('[data-mem="m1"]').waitFor()
    assert.equal(await page.locator('[data-mem]').count(), 2)
    assert.equal(await page.locator('[data-mem="m3"]').count(), 0)
    // tab ids are locale-independent: in English the same tabs work
    if (locale !== "zh-CN") {
      await page.locator('[data-seg="people"]').click(); await page.locator('[data-subject="ct_1"]').waitFor()
      await page.locator('[data-seg="projects"]').click(); await page.locator('[data-subject="prj_1"]').waitFor()
      assert.deepEqual(errors, [])
      return
    }
    await page.getByText('一次性要求留在那件事的记录里', { exact: false }).waitFor()
    await fits(); await shot('kb-rules')
    // Memory panel: reword in place, mark stale, delete with undo
    await page.locator('[data-mem="m1"]').hover(); await page.locator('[data-mem="m1"] [data-mem-edit]').click()
    await page.locator('[data-mem="m1"] [data-mem-edit-text]').fill('主会话收到需求时直接给出方案。'); await page.locator('[data-mem="m1"] [data-mem-edit-text]').press('Enter')
    await page.waitForFunction(() => document.querySelector('[data-mem="m1"]')?.textContent?.startsWith('主会话收到需求时直接给出方案。'))
    await page.locator('[data-mem="m2"]').hover(); await page.locator('[data-mem="m2"] [data-mem-pause]').click()
    await page.waitForFunction(() => document.querySelector('[data-mem="m2"]')?.hasAttribute('data-paused'))
    assert.deepEqual(patched.splice(0), [['m1', { text: '主会话收到需求时直接给出方案。' }], ['m2', { status: 'stale' }]])
    await page.locator('[data-mem="m1"]').hover(); await page.locator('[data-mem="m1"] [data-mem-del]').click()
    await page.waitForFunction(() => !document.querySelector('[data-mem="m1"]'))
    await page.getByRole('button', { name: '撤销' }).click()
    await page.locator('[data-mem="m1"]').waitFor()
    assert.deepEqual(deleted, [], 'undo keeps it on the Hub')
    await page.locator('[data-mem="m2"]').hover(); await page.locator('[data-mem="m2"] [data-mem-del]').click()
    await page.waitForFunction(() => !document.querySelector('[data-mem="m2"]'))
    assert.deepEqual(deleted, [])

    // 人 · 组织 · 项目 tabs
    await page.locator('[data-seg="people"]').click(); await page.locator('[data-subject="ct_1"]').waitFor()
    assert.equal(await page.locator('[data-subject]').count(), 1)
    assert.deepEqual(deleted.splice(0), ['m2'], 'leaving rules tab deletes what was forgotten')
    assert.match(await page.locator('[data-subject="ct_1"]').innerText(), /联系人.*2 条记忆.*1 条待确认/s)
    await fits(); await shot('kb-people')

    await page.locator('[data-seg="orgs"]').click(); await page.locator('[data-subject="org_1"]').waitFor()
    assert.equal(await page.locator('[data-subject]').count(), 1)
    assert.match(await page.locator('[data-subject="org_1"]').innerText(), /组织.*2 条记忆.*1 份资料/s)

    await page.locator('[data-seg="projects"]').click(); await page.locator('[data-subject="prj_1"]').waitFor()
    assert.equal(await page.locator('[data-subject]').count(), 1)
    assert.match(await page.locator('[data-subject="prj_1"] [data-subject-proposals]').innerText(), /2 条 AGENTS\.md 提议/)
    await fits(); await shot('kb-projects')

    // 资料: documents, sources, and memory providers
    await page.locator('[data-seg="materials"]').click(); await page.locator('[data-doc="doc1"]').waitFor()
    await page.locator('[data-provider="conversation"]').waitFor()
    assert.match(await page.locator('[data-provider="conversation"]').innerText(), /从主线对话里提取.*由 Steve 记下/s)
    assert.equal(await page.locator('[data-kb-store="openviking"]').count(), 1)
    await fits(); await shot('kb-materials')

    // 待确认: only knowledge candidates (never AGENTS.md proposals)
    await page.locator('[data-seg="pending"]').click()
    await page.locator('[data-candidate="c1"]').waitFor()
    assert.deepEqual(await page.locator('[data-cand-record]').allInnerTexts(), ['长期记忆', '知识条目'])
    assert.match(await page.locator('[data-candidate="c1"] [data-cand-why]').innerText(), /为什么值得保留：以后设计异常通知/)
    assert.equal(await page.locator('[data-cand-yes]').first().innerText(), '保存')
    await fits(); await shot('kb-candidates')
    await page.locator('[data-cand-expired-toggle]').click()
    await page.locator('[data-cand-expired-row="x1"] [data-cand-restore]').click()
    await page.waitForFunction(() => !document.querySelector('[data-cand-expired]'))
    assert.deepEqual(restored.splice(0), ['/api/kb/candidates/x1/restore'])

    // Search: knowledge and records strictly separated with scope filter chips
    await page.locator('[data-kb-search]').fill('续签'); await page.locator('[data-kb-search]').press('Enter')
    await page.locator('[data-search-knowledge] [data-hit="doc1"]').waitFor()
    await page.locator('[data-search-records] [data-record="ST-12"]').waitFor()
    await page.locator('[data-search-records] [data-record="msg_1"]').waitFor()
    assert.match(await page.locator('[data-kb-keywords-only]').innerText(), /只按关键词/)
    await page.getByRole('button', { name: '仅知识' }).click()
    await page.locator('[data-search-knowledge] [data-hit="doc1"]').waitFor()
    assert.equal(await page.locator('[data-search-records]').count(), 0)
    await page.getByRole('button', { name: '仅记录' }).click()
    await page.locator('[data-search-records] [data-record="ST-12"]').waitFor()
    assert.equal(await page.locator('[data-search-knowledge]').count(), 0)
    await page.locator('[data-kb-search]').fill('没有的东西'); await page.locator('[data-kb-search]').press('Enter')
    await page.getByText('没有找到').waitFor()
    await fits(); await shot('kb-search')

    // App hidden during undo window flushes delete immediately; Undo writes it back
    await page.locator('[data-seg="rules"]').click(); await page.locator('[data-kb-tab="rules"] [data-mem="m1"]').waitFor()
    await page.locator('[data-mem="m1"]').hover(); await page.locator('[data-mem="m1"] [data-mem-del]').click()
    await page.waitForFunction(() => !document.querySelector('[data-mem="m1"]'))
    await page.evaluate(() => { Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true }); document.dispatchEvent(new Event('visibilitychange')) })
    assert.deepEqual(await leaveDeletes(), ['DELETE /api/mems/m1'], 'hiding the app deletes what was forgotten')
    await page.evaluate(() => { delete document.visibilityState })
    const before = added.length
    await page.getByRole('button', { name: '撤销' }).click()
    for (let n = 0; added.length === before && n < 50; n++) await page.waitForTimeout(100)
    assert.deepEqual(added.slice(before).map((b) => [b.text, b.kind]), [[mems[0].text, mems[0].kind]], 'undo after an early delete writes it back')
    await page.locator('[data-mem="m2"]').hover(); await page.locator('[data-mem="m2"] [data-mem-del]').click()
    await page.waitForFunction(() => !document.querySelector('[data-mem="m2"]'))
    await page.reload(); await page.locator('[data-kb-tab="rules"]').waitFor()
    assert.deepEqual(await leaveDeletes(), ['DELETE /api/mems/m2'], 'reloading within the undo window deletes it')
    assert.deepEqual(deleted, [], 'and only once')
    assert.equal(await page.locator('[data-mem="m1"] [data-about-link="ct_1"]').getAttribute('href'), '#/knowledge/about/party:ct_1')
    assert.match(await page.locator('[data-mem="m1"]').innerText(), /用过 3 次/)

    // 整理: steward threshold, signals, skipped count, and run actions
    await page.locator('[data-seg="steward"]').click(); await page.locator('[data-change="ch1"]').waitFor()
    assert.equal(await page.locator('[data-steward="on"]').count(), 1)
    assert.match(await page.locator('[data-steward-signals]').innerText(), /新候选 2.*冲突 1.*阈值 3/)
    assert.match(await page.locator('[data-steward-skipped]').innerText(), /已跳过 4 次空跑/)
    await page.locator('[data-select="steward-threshold"]').click()
    await page.getByRole('option', { name: '≥ 5 条触发' }).click()
    for (let n = 0; stewardPuts.length < 1 && n < 50; n++) await page.waitForTimeout(100)
    assert.deepEqual(stewardPuts, [{ threshold: 5 }])
    await page.locator('[data-change-confirm="ch1"]').click()
    await page.locator('[data-change-keep="f2"]').click()
    await page.locator('[data-change-undo="ch3"]').click()
    for (let n = 0; acted.length < 3 && n < 50; n++) await page.waitForTimeout(100)
    assert.deepEqual(acted.splice(0), [['/api/kb/candidates/resolve', { ids: ['mm_new'], choice: 'yes' }], ['/api/kb/changes/ch2/keep', { keep: 'f2' }], ['/api/kb/changes/ch3/undo', null]])
    await fits(); await shot('kb-steward')

    // Deep link to 待确认
    await page.goto(base + '/test/fixtures/knowledge.html?via=link#/knowledge?tab=pending'); await page.locator('[data-kb-tab="pending"] [data-candidate="c1"]').waitFor()
    // 待处理 card
    await page.goto(base + '/test/fixtures/knowledge.html?inbox'); await page.locator('[data-fixture-cards]').waitFor()
    assert.deepEqual(await page.locator('[data-card-ask]').allInnerTexts(), ['存为长期记忆吗？', '记住这条吗？'])
    assert.match(await page.locator('[data-fixture-card="0"] [data-mem-confirm]').innerText(), /^主会话收到需求时/)
    assert.match(await page.locator('[data-fixture-card="1"] [data-mem-confirm]').innerText(), /^用户希望卡片详情/)
    await fits(); await shot('inbox-card')

    // Project page: AGENTS.md proposals appear on the project overview and can be accepted / dismissed
    await page.goto(base + '/test/fixtures/knowledge.html?project')
    await page.locator('[data-proj-proposals] [data-proposal="prop_1"]').waitFor()
    await page.locator('[data-proj-proposals] [data-proposal="prop_2"]').waitFor()
    assert.match(await page.locator('[data-proj-proposals-pending]').innerText(), /2 条待确认/)
    await fits(); await shot('proj-proposals')
    await page.locator('[data-proposal="prop_1"] [data-proposal-yes]').click()
    await page.waitForFunction(() => !document.querySelector('[data-proposal="prop_1"]'))
    await page.locator('[data-proposal="prop_2"] [data-proposal-no]').click()
    await page.waitForFunction(() => !document.querySelector('[data-proposal="prop_2"]'))
    assert.deepEqual(proposalActs, [['prop_1', { choice: 'yes' }], ['prop_2', { choice: 'no' }]])
    await page.locator('[data-proj-proposals-handled-toggle]').click()
    await page.locator('[data-proposal-handled="prop_1"]').waitFor()
    await page.locator('[data-proposal-handled="prop_2"]').waitFor()
    assert.deepEqual(errors, [])
  } finally { await page.close() }
}
try {
  for (const v of [{ width: 1280, height: 900, scheme: 'light' }, { width: 390, height: 844, scheme: 'dark' }, { width: 1280, height: 900, scheme: 'light', locale: 'en' }]) await run(v)
  console.log('knowledge ok')
} finally { await browser.close() }
