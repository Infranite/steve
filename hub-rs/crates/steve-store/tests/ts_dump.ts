/* Dumps what hub/src/repo.ts returns for every row of a database, for tests/prod_copy.rs to compare against.
 * Run on a COPY (db.ts migrates on import):
 *   STEVE_HOME=<dir with data/steve.sqlite3> STEVE_TS_HUB=~/Agents/steve/hub <hub>/../node_modules/.bin/tsx tests/ts_dump.ts > dump.json */
const hub = process.env.STEVE_TS_HUB
if (!hub) throw new Error("STEVE_TS_HUB not set")
const r = await import(hub + "/src/repo.ts")
const { db } = await import(hub + "/src/db.ts")

const wss = r.workspaces.all()
const out: Record<string, unknown> = { workspaces: wss, devices: r.devices.list(), bots_all: r.bots.list(), sessions_all: r.sessions.list() }
const per: Record<string, unknown> = {}
for (const w of wss) {
  const ws = w.id
  const items = r.items.all(ws)
  per[ws] = {
    items,
    items_open: r.items.list(ws),
    items_archived: r.items.list(ws, { archived: true }),
    item_detail: Object.fromEntries(items.map((i: any) => [i.id, {
      goals: r.items.goals(i.id), goal: r.items.goal(i.id), timeline: r.timeline.list(i.id), artifacts: r.artifacts.list(i.id),
      deps: r.deps.of(i.id), dependents: r.deps.dependents(i.id), tags: r.tags.ofItem(i.id), decisions: r.decisions.ofItem(i.id), sessions: r.sessions.ofItem(i.id),
    }])),
    bots: r.bots.list(ws),
    projects: r.projects.list(ws),
    projects_archived: r.projects.list(ws, true),
    tags: r.tags.list(ws),
    tag_uses: r.tags.uses(ws),
    decisions_open: r.decisions.open(ws),
    decisions_recent: r.decisions.recent(ws, 0),
    messages: r.messages.list(ws, "steve", 100000),
    sessions: r.sessions.list(ws),
    settings: r.settings.all(ws),
    memories: Object.fromEntries(r.bots.list().map((b: any) => [b.id, r.memories.list(b.id, ws)])),
  }
}
out.per_workspace = per
out.events = Object.fromEntries((db.prepare("SELECT id FROM sessions").all() as { id: string }[]).map((s) => [s.id, r.events.after(s.id, 0, 1000000)]))
out.users = (db.prepare("SELECT id FROM users").all() as { id: string }[]).map((u) => r.users.get(u.id))
out.member_workspaces = Object.fromEntries((db.prepare("SELECT id FROM users").all() as { id: string }[]).map((u) => [u.id, r.workspaces.ofUser(u.id)]))
out.bot_workspaces = Object.fromEntries(r.bots.list().map((b: any) => [b.id, r.bots.workspaces(b.id)]))
out.main_bot = r.bots.main()
out.counter_item_st = r.peekCounter("item:ST")
process.stdout.write(JSON.stringify(out))
