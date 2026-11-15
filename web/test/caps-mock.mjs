/* A mocked Hub's 能力 for the browser tests: the library (MCP servers, skills, remote agents) with the Bots that use
 * each entry, a Bot's own view of it, Steve's built-in server and the access token an outside client gets. A secret
 * never comes back with its value, as on the real Hub. `calls` records every write, in order. */
export const steveServer = (base = 'https://hub.example.test') => ({
  tools: [
    { name: 'steve_update', description: 'Report what you are doing now.' }, { name: 'steve_ask', description: 'Ask the user a question.' },
    { name: 'steve_deliver', description: 'Deliver for acceptance.' }, { name: 'steve_items_search', description: 'Search items.' },
    { name: 'steve_memory_list', description: 'List the user memories.' }, { name: 'steve_brand_new', description: 'A tool added later.' },
  ],
  outside: [
    { name: 'steve_items_search', description: 'Search items.' }, { name: 'steve_item_read', description: 'Read one item.' },
    { name: 'steve_item_create', description: 'Hand Steve a new piece of work.' }, { name: 'steve_memory_list', description: 'List memories.' },
  ],
  endpoints: [{ url: `${base}/mcp`, kind: 'public' }, { url: 'http://192.168.1.20:8440/mcp', kind: 'lan' }, { url: 'http://127.0.0.1:8440/mcp', kind: 'local' }],
  agent_card: `${base}/.well-known/agent-card.json`,
  a2a: `${base}/a2a`,
})

const hide = (vs) => vs.map(({ name, value, secret }) => (secret ? { name, secret } : { name, value }))
const shownMcp = (s) => ({ ...s, env: hide(s.env), headers: hide(s.headers), secrets: undefined })
const shownA2a = (a) => { const { secret, ...rest } = a; return { ...rest, has_secret: !!secret || !!a.has_secret } }
const PATH = { mcp: 'mcp-servers', skill: 'skills', a2a: 'a2a-agents' }
const KIND = { 'mcp-servers': 'mcp', skills: 'skill', 'a2a-agents': 'a2a' }

export const mcpServer = (id, name, transport, extra = {}) => ({ id, name, transport, command: null, args: [], env: [], url: null, headers: [], created: 0, updated: 0, bots: [], ...extra })
export const skillEntry = (id, name, description, content, extra = {}) => ({ id, name, description, content, created: 0, updated: 0, bots: [], ...extra })
export const agentEntry = (id, name, title, extra = {}) => ({ id, name, tool: 'steve_a2a_' + name, card_url: 'https://agent.example.test/.well-known/agent-card.json', title, description: title + ' 的说明', url: 'https://agent.example.test/a2a', version: '1.0.0', provider: 'Example', protocol: '0.3.0', skills: [], auth: { scheme: 'none' }, has_secret: false, fetched: Date.now() - 3600_000, created: 0, updated: 0, bots: [], ...extra })

export function capsHub(lib) {
  const calls = []
  let n = 100
  const list = (kind) => lib[kind]
  const out = (kind, e) => (kind === 'mcp' ? shownMcp(e) : kind === 'a2a' ? shownA2a(e) : e)
  const forBot = (kind, bot) => list(kind).filter((e) => e.bots.some((b) => b.id === bot)).map((e) => ({ ...out(kind, e), bots: undefined, enabled: e.bots.find((b) => b.id === bot).enabled }))
  const assign = (kind, bot, id, enabled) => { const e = list(kind).find((x) => x.id === id); const b = e.bots.find((x) => x.id === bot); if (b) b.enabled = enabled; else e.bots.push({ id: bot, enabled }) }
  /** answers one /api request when it is 能力's; false otherwise */
  async function handle(route, method, p) {
    const json = async (status, data) => { await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(data) }); return true }
    const body = () => route.request().postDataJSON()
    if (p === '/api/capabilities/steve') return json(200, steveServer())
    if (p === '/api/access/tokens' && method === 'POST') { const b = body(); calls.push(['TOKEN', b]); return json(200, { token: 'stt_test_once', item: { id: 'tok_1', name: b.name } }) }
    let m = /^\/api\/bots\/([^/]+)\/tools$/.exec(p)
    if (m && method === 'GET') return json(200, { core: ['steve_update', 'steve_ask', 'steve_request_approval', 'steve_deliver', 'steve_items_search', 'steve_knowledge_search', 'steve_tools_search', 'steve_tool_call'], pins: [], usage: [], days: 30 })
    m = /^\/api\/bots\/([^/]+)\/capabilities$/.exec(p)
    if (m) return json(200, { mcp: forBot('mcp', m[1]), skills: forBot('skill', m[1]), a2a: forBot('a2a', m[1]) })
    m = /^\/api\/bots\/([^/]+)\/capabilities\/(mcp|skill|a2a)\/([^/]+)$/.exec(p)
    if (m && method === 'PUT') { const b = body(); calls.push(['PUT', m[1], m[2], m[3], b]); assign(m[2], m[1], m[3], b.enabled !== false); return json(200, { ok: true }) }
    if (m && method === 'DELETE') { calls.push(['UNASSIGN', m[1], m[2], m[3]]); const e = list(m[2]).find((x) => x.id === m[3]); e.bots = e.bots.filter((x) => x.id !== m[1]); return json(200, { ok: true }) }
    if (p === '/api/capabilities/a2a-agents/preview') { const b = body(); calls.push(['PREVIEW', b]); return json(200, { card_url: b.card_url.replace(/\/$/, '') + '/.well-known/agent-card.json', card: { name: 'Trip Planner', description: '规划行程。', url: 'https://trips.example.test/a2a', version: '2.1', skills: [{ id: 'plan', name: '安排行程', description: '按天排' }] } }) }
    m = /^\/api\/capabilities\/(mcp-servers|skills|a2a-agents)$/.exec(p)
    if (m && method === 'GET') return json(200, list(KIND[m[1]]).map((e) => out(KIND[m[1]], e)))
    if (m && method === 'POST') {
      const kind = KIND[m[1]], b = body(); calls.push(['POST', kind, b])
      const { bot, ...rest } = b
      const e = kind === 'mcp' ? mcpServer('bmcp_' + ++n, rest.name, rest.transport, rest) : kind === 'skill' ? skillEntry('bskill_' + ++n, rest.name, rest.description, rest.content ?? '') : agentEntry('a2a_' + ++n, 'trip_planner', 'Trip Planner', { card_url: rest.card_url, auth: rest.auth ?? { scheme: 'none' }, has_secret: !!rest.secret, skills: [{ id: 'plan', name: '安排行程', description: '按天排', tags: [] }] })
      if (bot) e.bots.push({ id: bot, enabled: true })
      list(kind).push(e); list(kind).sort((x, y) => x.name.localeCompare(y.name))
      return json(200, out(kind, e))
    }
    m = /^\/api\/capabilities\/(mcp-servers|skills|a2a-agents)\/([^/]+)$/.exec(p)
    if (m && method === 'PATCH') { const kind = KIND[m[1]], b = body(); calls.push(['PATCH', kind, m[2], b]); const e = list(kind).find((x) => x.id === m[2]); const { secret, refresh, ...rest } = b; Object.assign(e, rest, secret ? { has_secret: true } : {}); return json(200, out(kind, e)) }
    if (m && method === 'DELETE') { const kind = KIND[m[1]]; calls.push(['DELETE', kind, m[2]]); lib[kind] = list(kind).filter((x) => x.id !== m[2]); return json(200, { ok: true }) }
    return false
  }
  return { handle, calls, lib }
}

export { PATH }
