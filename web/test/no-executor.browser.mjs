// Browser smoke for Steve coordinator mode: no_executor AskOwner decision card, "later"
// settlement, and coordinator_deferred placement display on Item detail.
// Fixture shapes match web/src/gen/{DecisionView,DecisionPrompt,ItemView,PlacementExplanation}.ts.
import assert from 'node:assert/strict'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || 'http://127.0.0.1:5195'

const ws = { id: 'default', name: '默认', description: '', item_prefix: 'ST', sort: 0, created: 0 }
const bots = [
  { id: 'steve', name: 'Steve', role: '工作空间协调者', main: true, harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#8b7cf6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0 },
  { id: 'dev', name: '开发', role: '代码开发', main: false, harness: 'claude-code', model: null, reasoning_effort: null, rules: '', persona: '', proactive: 1, accept_mode: 'inherit', perm_mode: 'default', avatar: { color: '#14b8a6' }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ['default'], memories: 0 },
]
const noExecutorDecision = {
  id: 'dec_no_exec_1',
  ws: 'default',
  kind: 'ask',
  subject_type: 'no_executor',
  subject_id: 'default',
  item_id: null,
  session_id: null,
  question: {
    kind: 'key',
    template: {
      code: 'no_executor',
      items: [
        { id: 'ST-1', title: '修复支付回调超时' },
        { id: 'ST-2', title: '补齐压测报告' },
      ],
      rejected: [
        { bot: 'dev', reason: 'offline' },
      ],
      reason: 'offline',
      team: { id: 'team-1', name: '核心交付' },
      proposal: {
        id: 'bp_1',
        revision: 1,
        name: '压测',
        role: '负责压测和性能回归',
        specialties: ['perf'],
        harness: 'claude-code',
        model: null,
        node: 'local',
        team: { id: 'team-1', name: '核心交付' },
      },
    },
  },
  draft: '',
  options: ['assign_bot', 'confirm_proposal', 'later'],
  status: 'open',
  result: null,
  requested_by: 'steve',
  created: Date.now() - 60_000,
  resolved: null,
  undo_until: null,
  space_id: null,
  tier: 'decide',
  folded: null,
  revision: 1,
}
const deferredPlacement = {
  bot: 'steve',
  reason: 'coordinator_deferred',
  matched: [],
  rejected: [{ bot: 'dev', reason: 'offline' }],
  fallback: 'offline',
  deferred_by: 'card',
}
const deferredItemView = {
  placement: deferredPlacement,
  id: 'ST-1',
  ws: 'default',
  no: 1,
  title: '修复支付回调超时',
  state: { kind: 'active' },
  cycle: 1,
  revision: 1,
  delivery_revision: 0,
  delivery: [],
  bot_id: 'steve',
  priority: '中',
  source: '对话',
  created: Date.now() - 60_000,
  updated: Date.now(),
  goals: 1,
  tags: [],
  detail_stamp: 1,
}

const errors = []
const resolvedCalls = []
try {
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1200, height: 860 }, reducedMotion: 'reduce' })
  page.setDefaultTimeout(10000)
  page.on('pageerror', (e) => errors.push(e.message))
  let settled = false
  let settings = { noExecutor: 'ask' }

  await page.route('**/*', async (route) => {
    const u = new URL(route.request().url())
    if (u.origin !== base) return route.abort()
    if (u.pathname === '/auth/hello') return route.fulfill({ json: { name: 'steve', version: 'fixture', api: 2, login: { token: false, oidc: false } } })
    if (!u.pathname.startsWith('/api/')) return route.continue()
    const method = route.request().method(), p = u.pathname
    if (p === '/api/shell') {
      return route.fulfill({ json: {
        version: 'fixture', web_version: 'fixture', owner: true, workspace: ws, workspaces: [ws],
        bots, allBots: bots.map(b => ({ id: b.id, name: b.name, workspaces: b.workspaces })),
        projects: [], tags: [], tagUses: {}, settings, devices: [],
        runtimes: [{ id: 'claude-code', name: 'Claude Code', ok: true, version: '', path: '', models: [], note: '', modes: ['default'] }],
      } })
    }
    if (p === '/api/snapshot') {
      return route.fulfill({ json: {
        epoch: 'fixture', rev: settled ? 2 : 1, cursor: settled ? 'fixture:2' : 'fixture:1',
        views: {
          item: [deferredItemView],
          session: [],
          decision: settled ? [] : [noExecutorDecision],
        },
      } })
    }
    if (p === '/api/summary') return route.fulfill({ json: { ws: 'default', workspace_name: '默认', thinking: false, hold: null } })
    if (p === '/api/views/page') return route.fulfill({ json: { rev: settled ? 2 : 1, views: [], next: null } })
    if (p === '/api/views') return route.fulfill({ json: { rev: settled ? 2 : 1, views: [settled ? { ...noExecutorDecision, status: 'resolved', result: { choice: 'later', bot: 'steve', policy: 'steve', items: ['ST-1', 'ST-2'] }, resolved: Date.now() } : noExecutorDecision], missing: [] } })
    if (p === '/api/decisions/dec_no_exec_1' && method === 'POST') {
      const body = route.request().postDataJSON()
      resolvedCalls.push(body)
      settled = true
      settings = { noExecutor: 'steve' }
      return route.fulfill({ json: { ok: true, id: 'dec_no_exec_1', revision: 1, rev: 2 } })
    }
    if (p === '/api/items/ST-1') {
      return route.fulfill({ json: {
        id: 'ST-1',
        source: ['对话', 'chat'],
        cwd: null,
        branch: null,
        plan: [],
        goalsList: [{ version: 1, text: '排查超时原因', created: Date.now() - 60_000 }],
        planRounds: [],
        timeline: [],
        artifacts: [],
        decisions: [],
        dependents: [],
        annotations: [],
        dispatch: { team: { id: 'team-1', name: '核心交付', source: 'default' }, blocked: null },
      } })
    }
    if (p === '/api/bot-teams') return route.fulfill({ json: { default: 'team-1', editable: true, teams: [{ id: 'team-1', name: '核心交付', members: [{ bot: 'dev', nodes: null }] }] } })
    if (p.startsWith('/api/stats/')) return route.fulfill({ status: 404, json: {} })
    return route.fulfill({ status: 200, json: {} })
  })

  await page.addInitScript(() => { localStorage.setItem('steve-lang', 'zh-CN') })
  await page.goto(base + '/#/inbox')

  // 1. Inbox displays the single deduplicated no_executor card with both waiting items,
  //    per-Bot rejection reason, proposal confirmation, and "晚点再说" button.
  const wrap = page.locator('[data-decision-wrap="dec_no_exec_1"]')
  const card = wrap.locator('[data-no-executor-card]')
  await card.waitFor()
  assert.match(await card.locator('[data-no-executor-items]').innerText(), /ST-1[^]*ST-2/)
  assert.match(await card.locator('[data-no-executor-reasons]').innerText(), /开发 Bot[^]*离线/)
  assert.equal(await card.locator('[data-no-executor-assign]').count(), 1)
  assert.equal(await card.locator('[data-no-executor-confirm]').count(), 1)
  assert.equal(await wrap.locator('[data-no-executor-later]').count(), 1)

  // Click "晚点再说" -> posts choice="later" through the unified W3.3 resolve endpoint.
  await wrap.locator('[data-no-executor-later]').click()
  for (let i = 0; i < 40 && !resolvedCalls.length; i++) await page.waitForTimeout(50)
  assert.equal(resolvedCalls.length, 1)
  assert.equal(resolvedCalls[0].choice, 'later')

  // 2. Item detail shows explicit coordinator_deferred placement explanation ("Steve 自己做：因为…").
  await page.goto(base + '/#/items/ST-1')
  const why = page.locator('[data-dispatch-reason="coordinator_deferred"]')
  await why.waitFor()
  assert.match(await why.innerText(), /Steve 自己做：因为/)
  assert.match(await why.innerText(), /晚点再说/)
  assert.match(await why.innerText(), /开发 Bot/)

  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ['no_executor card', 'later settlement', 'coordinator_deferred placement'] }))
} finally {
  await browser.close()
}
