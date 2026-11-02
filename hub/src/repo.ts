/* Repository: the only module that writes SQL. Returns plain objects shaped like the domain model.
 * Lists are always scoped to one workspace (the data boundary); single objects are fetched by their global id. */
import { randomUUID, createHash } from "node:crypto"
import { db, now, json } from "./db.js"
import { publish } from "./bus.js"

export const WS = "default"
export const uid = (p: string) => `${p}_${randomUUID().replace(/-/g, "").slice(0, 16)}`
export const sha = (s: string) => createHash("sha256").update(s).digest("hex")

/* ── types ── */
export type ItemStatus = "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block"
export type Stage = "接收" | "分流" | "定义" | "规划" | "准备" | "执行" | "交付与验收" | "收尾"
/** left: not done when the item finished (left open on delivery, or nobody ticked it off) */
export type PlanStep = { step: string; done: boolean; left?: boolean }
/** an earlier plan: replaced (its open steps were abandoned) or done (a new round followed); kept in revisions as item_plan */
export type PlanRound = { round: number; steps: PlanStep[]; end: "done" | "replaced"; created: number }
export type Parent = { item?: string | null; session?: string | null; at?: number | null }
export type Item = {
  id: string; workspace_id: string; no: number; kind: "task" | "chat"; title: string; status: ItemStatus; stage: Stage; bot_id: string | null; project_id: string | null
  cwd: string | null; branch: string | null; plan: PlanStep[]; now: string; accept_mode: string | null; priority: string; archived: boolean; star: boolean; due: string | null
  snooze: { label: string; until: number } | null; source: [string, string]; parent: Parent | null; created: number; updated: number
}
export type Avatar = { color?: string; skin?: string | null; round?: number }
export type Bot = {
  id: string; workspace_id: string; name: string; role: string; main: boolean; harness: string; model: string | null; reasoning_effort: string | null; rules: string; persona: string; proactive: number
  accept_mode: string; perm_mode: string; avatar: Avatar; default_project: string | null; prompt: string | null; device: string | null; enabled: boolean; revision: number; updated: number
}
export type Project = { id: string; workspace_id: string; name: string; description: string; device_id: string; path: string; kind: "git" | "dir"; worktree: boolean; default_bot: string | null; release: { kind: "script"; cmd: string } | null; archived: boolean; created: number; updated: number }
export type Workspace = { id: string; name: string; description: string; item_prefix: string; sort: number; created: number }
export type SessionStatus = "starting" | "running" | "idle" | "ended" | "error"
export type Session = { id: string; workspace_id: string; item_id: string | null; no: number; bot_id: string | null; harness: string; model: string | null; reasoning_effort?: string | null; cwd: string; acp_session_id: string | null; status: SessionStatus; used: number | null; size: number | null; cost: number | null; token_hash: string | null; title: string; device_id: string; created: number; updated: number }
export type DecisionKind = "accept" | "ask" | "approve" | "fail" | "propose"
export type Decision = { id: string; workspace_id: string; kind: DecisionKind; subject_type: string; subject_id: string; item_id: string | null; session_id: string | null; question: string; draft: string; options: string[]; status: "open" | "resolved" | "expired"; result: any; requested_by: string; created: number; resolved: number | null; undo: any }
export type Message = { id: string; workspace_id: string; space: string; author: "me" | "steve"; text: string; item_id: string | null; meta: any; created: number }
export type Tag = { id: string; workspace_id: string; name: string; cat: string; description: string; created_by: string; created: number }

const rowItem = (r: any): Item => r && { ...r, plan: json(r.plan, []), archived: !!r.archived, star: !!r.star, snooze: json(r.snooze, null), source: json(r.source, ["对话", "chat"]), parent: json(r.parent, null) }
const rowBot = (r: any): Bot => r && { ...r, main: !!r.main, enabled: !!r.enabled, avatar: json(r.avatar, {}), project_dir: undefined, worktree: undefined, deleted: undefined }
const rowProject = (r: any): Project => r && { ...r, worktree: !!r.worktree, archived: !!r.archived, release: json(r.release, null) }
const rowDec = (r: any): Decision => r && { ...r, options: json(r.options, []), result: json(r.result, null), undo: json(r.undo, null) }
const setClause = (p: Record<string, unknown>, enc: Record<string, (v: any) => unknown> = {}) => {
  const ks = Object.keys(p).filter((k) => p[k] !== undefined)
  return { sql: ks.map((k) => `${k} = ?`).join(", "), vals: ks.map((k) => (enc[k] ? enc[k](p[k]) : typeof p[k] === "boolean" ? (p[k] ? 1 : 0) : p[k])), n: ks.length }
}

export function audit(actor: string, action: string, target: string, detail: unknown = {}, ws = WS) {
  db.prepare("INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)").run(ws, now(), actor, action, target, JSON.stringify(detail))
}
/** counters are global per name, so item numbers with the same prefix never collide across workspaces */
function nextCounter(name: string) {
  const r = db.prepare("INSERT INTO counters (workspace_id, name, value) VALUES ('*', ?, 1) ON CONFLICT(workspace_id, name) DO UPDATE SET value = value + 1 RETURNING value").get(name) as { value: number }
  return r.value
}
export const peekCounter = (name: string) => ((db.prepare("SELECT value FROM counters WHERE workspace_id = '*' AND name = ?").get(name) as { value: number } | undefined)?.value ?? 0)
function revision(ws: string, type: string, id: string, version: number, patch: unknown, author: string) {
  db.prepare("INSERT INTO revisions (workspace_id, object_type, object_id, version, patch, author, created) VALUES (?, ?, ?, ?, ?, ?, ?)").run(ws, type, id, version, JSON.stringify(patch), author, now())
}

/* ── workspaces, users, auth sessions ── */
export function ensureWorkspace() {
  db.prepare("INSERT OR IGNORE INTO workspaces (id, name, created) VALUES (?, ?, ?)").run(WS, "个人", now())
  db.prepare("UPDATE workspaces SET name = '个人' WHERE id = ? AND name = '默认工作空间'").run(WS)
  /* item numbers used to be counted per workspace; carry the old count over once */
  const old = db.prepare("SELECT value FROM counters WHERE workspace_id = ? AND name = 'item'").get(WS) as { value: number } | undefined
  if (old) { db.prepare("INSERT INTO counters (workspace_id, name, value) VALUES ('*', 'item:ST', ?) ON CONFLICT(workspace_id, name) DO UPDATE SET value = max(value, excluded.value)").run(old.value); db.prepare("DELETE FROM counters WHERE workspace_id = ? AND name = 'item'").run(WS) }
}
export const workspaces = {
  get: (id: string) => db.prepare("SELECT * FROM workspaces WHERE id = ?").get(id) as Workspace | undefined,
  ofUser: (userId: string) => db.prepare("SELECT w.*, m.role FROM workspaces w JOIN workspace_members m ON m.workspace_id = w.id WHERE m.user_id = ? ORDER BY w.sort, w.created").all(userId) as (Workspace & { role: string })[],
  all: () => db.prepare("SELECT * FROM workspaces ORDER BY sort, created").all() as Workspace[],
  create: (p: { name: string; description?: string; item_prefix?: string }, ownerId: string) => {
    const id = uid("ws"), t = now()
    db.prepare("INSERT INTO workspaces (id, name, description, item_prefix, sort, created) VALUES (?, ?, ?, ?, (SELECT coalesce(max(sort), 0) + 1 FROM workspaces), ?)").run(id, p.name, p.description ?? "", p.item_prefix ?? "ST", t)
    db.prepare("INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, 'owner', ?)").run(id, ownerId, t)
    publish({ type: "workspace", id }); return workspaces.get(id)!
  },
  patch: (id: string, p: Partial<Pick<Workspace, "name" | "description" | "item_prefix">>) => {
    const c = setClause(p); if (c.n) db.prepare(`UPDATE workspaces SET ${c.sql} WHERE id = ?`).run(...c.vals, id)
    publish({ type: "workspace", id }); return workspaces.get(id)!
  },
  isMember: (id: string, userId: string) => !!db.prepare("SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?").get(id, userId),
}
export const users = {
  count: () => (db.prepare("SELECT count(*) n FROM users").get() as { n: number }).n,
  bySubject: (sub: string) => db.prepare("SELECT * FROM users WHERE subject = ?").get(sub) as any,
  get: (id: string) => db.prepare("SELECT * FROM users WHERE id = ?").get(id) as any,
  create: (sub: string, name: string, email: string | null, role: string) => {
    const id = uid("usr"), t = now()
    db.prepare("INSERT INTO users (id, workspace_id, subject, name, email, role, created) VALUES (?, ?, ?, ?, ?, ?, ?)").run(id, WS, sub, name, email, role, t)
    db.prepare("INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, ?, ?)").run(WS, id, role, t)
    return id
  },
}
export const authSessions = {
  create: (userId: string, token: string, ttlMs: number) => db.prepare("INSERT INTO auth_sessions (token_hash, user_id, created, expires) VALUES (?, ?, ?, ?)").run(sha(token), userId, now(), now() + ttlMs),
  user: (token: string) => db.prepare("SELECT u.* FROM auth_sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires > ?").get(sha(token), now()) as any,
  drop: (token: string) => db.prepare("DELETE FROM auth_sessions WHERE token_hash = ?").run(sha(token)),
}

/* ── settings: one JSON value per key and workspace ── */
export const settings = {
  all: (ws: string) => Object.fromEntries((db.prepare("SELECT key, value FROM settings WHERE workspace_id = ?").all(ws) as { key: string; value: string }[]).map((r) => [r.key, json(r.value, null)])),
  get: <T>(ws: string, key: string, d: T): T => { const r = db.prepare("SELECT value FROM settings WHERE workspace_id = ? AND key = ?").get(ws, key) as { value: string } | undefined; return r ? json(r.value, d) : d },
  set: (ws: string, p: Record<string, unknown>, author: string) => {
    const st = db.prepare("INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated")
    db.transaction(() => { for (const [k, v] of Object.entries(p)) st.run(ws, k, JSON.stringify(v), now()) })()
    revision(ws, "settings", ws, 0, p, author); publish({ type: "settings", id: ws })
  },
}

/* ── projects ── */
export const projects = {
  list: (ws: string, archived = false) => (db.prepare("SELECT * FROM projects WHERE workspace_id = ? AND archived = ? ORDER BY name").all(ws, archived ? 1 : 0) as any[]).map(rowProject),
  get: (id: string | null | undefined) => (id ? rowProject(db.prepare("SELECT * FROM projects WHERE id = ?").get(id)) : undefined) as Project | undefined,
  byPath: (ws: string, path: string) => rowProject(db.prepare("SELECT * FROM projects WHERE workspace_id = ? AND path = ?").get(ws, path)) as Project | undefined,
  create: (ws: string, p: Omit<Project, "id" | "workspace_id" | "archived" | "created" | "updated">) => {
    const id = uid("prj"), t = now()
    db.prepare("INSERT INTO projects (id, workspace_id, name, description, device_id, path, kind, worktree, default_bot, release, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
      .run(id, ws, p.name, p.description, p.device_id, p.path, p.kind, p.worktree ? 1 : 0, p.default_bot, p.release ? JSON.stringify(p.release) : null, t, t)
    publish({ type: "project", id }); return projects.get(id)!
  },
  patch: (id: string, p: Partial<Omit<Project, "id" | "workspace_id" | "created" | "updated">>, author: string) => {
    const cur = projects.get(id); if (!cur) throw new Error("没有这个项目")
    const c = setClause(p as any, { release: (v) => (v ? JSON.stringify(v) : null) })
    if (c.n) db.prepare(`UPDATE projects SET ${c.sql}, updated = ? WHERE id = ?`).run(...c.vals, now(), id)
    revision(cur.workspace_id, "project", id, 0, p, author); publish({ type: "project", id }); return projects.get(id)!
  },
}

/* ── bots: owned by the user, members of workspaces ── */
export const bots = {
  list: (ws?: string) => (ws
    ? db.prepare("SELECT b.* FROM bots b JOIN bot_workspaces m ON m.bot_id = b.id WHERE m.workspace_id = ? AND b.deleted = 0 ORDER BY b.main DESC, b.name").all(ws)
    : db.prepare("SELECT * FROM bots WHERE deleted = 0 ORDER BY main DESC, name").all() as any[]).map(rowBot),
  get: (id: string) => rowBot(db.prepare("SELECT * FROM bots WHERE id = ? AND deleted = 0").get(id)),
  main: () => rowBot(db.prepare("SELECT * FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1").get()),
  workspaces: (id: string) => db.prepare("SELECT workspace_id, duty FROM bot_workspaces WHERE bot_id = ?").all(id) as { workspace_id: string; duty: string }[],
  inWorkspace: (id: string, ws: string) => !!db.prepare("SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?").get(id, ws),
  setWorkspaces: (id: string, wss: string[]) => db.transaction(() => {
    db.prepare(`DELETE FROM bot_workspaces WHERE bot_id = ? AND workspace_id NOT IN (${wss.map(() => "?").join(",") || "''"})`).run(id, ...wss)
    for (const w of wss) db.prepare("INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, ?)").run(id, w, now())
    publish({ type: "bot", id })
  })(),
  /** seed-only insert: never overwrites what the user changed */
  seed: (b: Partial<Bot> & { id: string; name: string; harness: string }, ws: string) => {
    db.prepare(`INSERT INTO bots (id, workspace_id, name, role, main, harness, model, reasoning_effort, rules, persona, proactive, accept_mode, perm_mode, avatar, default_project, enabled, revision, updated)
      VALUES (@id, @ws, @name, @role, @main, @harness, @model, @reasoning_effort, @rules, @persona, @proactive, @accept_mode, @perm_mode, @avatar, @default_project, 1, 1, @updated) ON CONFLICT(id) DO NOTHING`)
      .run({ role: "", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "default", default_project: null, ...b, avatar: JSON.stringify(b.avatar ?? {}), ws, main: b.main ? 1 : 0, updated: now() })
    db.prepare("INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, ?)").run(b.id, ws, now())
  },
  create: (ws: string, b: Partial<Bot> & { name: string; harness: string }, author: string) => {
    const id = uid("bot"); bots.seed({ ...b, id, main: false }, ws); audit(author, "bot.create", id, {}, ws); publish({ type: "bot", id }); return bots.get(id)
  },
  patch: (id: string, p: Partial<Bot>, author: string) => {
    const cur = bots.get(id); if (!cur) throw new Error("没有这个 Bot")
    const allowed = ["name", "role", "harness", "model", "reasoning_effort", "rules", "persona", "proactive", "accept_mode", "perm_mode", "avatar", "default_project", "prompt", "device", "enabled"] as const
    const q: Record<string, unknown> = {}; for (const k of allowed) if (k in p) q[k] = (p as any)[k]
    const c = setClause(q, { avatar: (v) => JSON.stringify(v ?? {}) })
    if (c.n) db.prepare(`UPDATE bots SET ${c.sql}, revision = revision + 1, updated = ? WHERE id = ?`).run(...c.vals, now(), id)
    revision(cur.workspace_id, "bot", id, cur.revision + 1, { before: Object.fromEntries(Object.keys(q).map((k) => [k, (cur as any)[k]])), after: q }, author)
    publish({ type: "bot", id }); return bots.get(id)
  },
  remove: (id: string) => { db.prepare("UPDATE bots SET deleted = 1, enabled = 0, updated = ? WHERE id = ? AND main = 0").run(now(), id); publish({ type: "bot", id }) },
  restore: (id: string) => { db.prepare("UPDATE bots SET deleted = 0, enabled = 1, updated = ? WHERE id = ?").run(now(), id); publish({ type: "bot", id }) },
}
export const memories = {
  get: (id: string) => db.prepare("SELECT * FROM bot_memories WHERE id = ?").get(id) as { id: string; bot_id: string; workspace_id: string; text: string } | undefined,
  list: (bot: string, ws: string) => db.prepare("SELECT * FROM bot_memories WHERE bot_id = ? AND workspace_id = ? ORDER BY created DESC").all(bot, ws) as { id: string; text: string; source: string; created: number }[],
  count: (bot: string, ws: string) => (db.prepare("SELECT count(*) n FROM bot_memories WHERE bot_id = ? AND workspace_id = ?").get(bot, ws) as { n: number }).n,
  add: (bot: string, ws: string, text: string, source: string) => { const id = uid("mem"); db.prepare("INSERT INTO bot_memories (id, bot_id, workspace_id, text, source, created) VALUES (?, ?, ?, ?, ?, ?)").run(id, bot, ws, text, source, now()); publish({ type: "bot", id: bot }); return id },
  edit: (id: string, text: string) => { const r = db.prepare("UPDATE bot_memories SET text = ? WHERE id = ? RETURNING bot_id").get(text, id) as any; if (r) publish({ type: "bot", id: r.bot_id }) },
  remove: (id: string) => { const r = db.prepare("DELETE FROM bot_memories WHERE id = ? RETURNING bot_id").get(id) as any; if (r) publish({ type: "bot", id: r.bot_id }) },
  clear: (ws: string) => { db.prepare("DELETE FROM bot_memories WHERE workspace_id = ?").run(ws); publish({ type: "bot", id: "*" }) },
}

/* ── tags ── */
export const tags = {
  list: (ws: string) => db.prepare("SELECT * FROM tags WHERE workspace_id = ? ORDER BY cat, name").all(ws) as Tag[],
  get: (id: string) => db.prepare("SELECT * FROM tags WHERE id = ?").get(id) as Tag | undefined,
  byName: (ws: string, name: string) => db.prepare("SELECT * FROM tags WHERE workspace_id = ? AND name = ?").get(ws, name) as Tag | undefined,
  create: (ws: string, p: { name: string; cat: string; description: string }, by: string) => {
    const ex = tags.byName(ws, p.name); if (ex) return ex
    const id = uid("tag"); db.prepare("INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?)").run(id, ws, p.name, p.cat, p.description, by, now())
    publish({ type: "tag", id }); return tags.get(id)!
  },
  patch: (id: string, p: Partial<Pick<Tag, "name" | "cat" | "description">>) => { const c = setClause(p); if (c.n) db.prepare(`UPDATE tags SET ${c.sql} WHERE id = ?`).run(...c.vals, id); publish({ type: "tag", id }); return tags.get(id) },
  remove: (id: string) => { db.transaction(() => { db.prepare("DELETE FROM item_tags WHERE tag_id = ?").run(id); db.prepare("DELETE FROM tags WHERE id = ?").run(id) })(); publish({ type: "tag", id }) },
  merge: (from: string, into: string) => {
    db.transaction(() => {
      db.prepare("INSERT OR IGNORE INTO item_tags (item_id, tag_id) SELECT item_id, ? FROM item_tags WHERE tag_id = ?").run(into, from)
      db.prepare("DELETE FROM item_tags WHERE tag_id = ?").run(from); db.prepare("DELETE FROM tags WHERE id = ?").run(from)
    })(); publish({ type: "tag", id: into })
  },
  uses: (ws: string) => Object.fromEntries((db.prepare("SELECT t.id, count(it.item_id) n FROM tags t LEFT JOIN item_tags it ON it.tag_id = t.id WHERE t.workspace_id = ? GROUP BY t.id").all(ws) as { id: string; n: number }[]).map((r) => [r.id, r.n])),
  ofItem: (item: string) => (db.prepare("SELECT tag_id FROM item_tags WHERE item_id = ?").all(item) as { tag_id: string }[]).map((r) => r.tag_id),
  setItem: (item: string, ids: string[]) => { db.transaction(() => { db.prepare("DELETE FROM item_tags WHERE item_id = ?").run(item); for (const t of ids) db.prepare("INSERT OR IGNORE INTO item_tags (item_id, tag_id) VALUES (?, ?)").run(item, t) })(); publish({ type: "item", id: item }) },
}

/* ── items ── */
export const items = {
  list: (ws: string, opts: { archived?: boolean } = {}) => (db.prepare("SELECT * FROM items WHERE workspace_id = ? AND kind = 'task' AND archived = ? ORDER BY updated DESC").all(ws, opts.archived ? 1 : 0) as any[]).map(rowItem),
  all: (ws: string) => (db.prepare("SELECT * FROM items WHERE workspace_id = ? AND kind = 'task' ORDER BY updated DESC").all(ws) as any[]).map(rowItem),
  get: (id: string) => rowItem(db.prepare("SELECT * FROM items WHERE id = ?").get(id)),
  create: (ws: string, p: { title: string; goal: string; bot_id: string; project_id: string | null; kind?: "task" | "chat"; status?: ItemStatus; stage?: Stage; source?: [string, string]; parent?: Parent | null }) => {
    const kind = p.kind ?? "task", prefix = workspaces.get(ws)?.item_prefix ?? "ST"
    const no = kind === "task" ? nextCounter("item:" + prefix) : 0
    const id = kind === "task" ? `${prefix}-${no}` : uid("chat"), t = now()
    db.prepare("INSERT INTO items (id, workspace_id, no, kind, title, status, stage, bot_id, project_id, source, parent, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
      .run(id, ws, no, kind, p.title, p.status ?? "run", p.stage ?? "定义", p.bot_id, p.project_id, JSON.stringify(p.source ?? ["对话", "chat"]), p.parent ? JSON.stringify(p.parent) : null, t, t)
    db.prepare("INSERT INTO item_goals (item_id, version, text, created) VALUES (?, 1, ?, ?)").run(id, p.goal, t)
    publish({ type: "item", id }); return items.get(id)
  },
  patch: (id: string, p: Partial<Pick<Item, "title" | "status" | "stage" | "now" | "plan" | "cwd" | "branch" | "priority" | "archived" | "accept_mode" | "bot_id" | "project_id" | "star" | "due" | "snooze">>) => {
    const c = setClause(p as any, { plan: (v) => JSON.stringify(v), snooze: (v) => (v ? JSON.stringify(v) : null) })
    if (!c.n) return items.get(id)
    db.prepare(`UPDATE items SET ${c.sql}, updated = ? WHERE id = ?`).run(...c.vals, now(), id)
    publish({ type: "item", id }); return items.get(id)
  },
  remove: (id: string) => {
    db.transaction(() => {
      db.prepare("UPDATE sessions SET item_id = NULL, no = 0 WHERE item_id = ?").run(id)
      for (const t of ["item_tags", "item_goals", "timeline", "artifacts"]) db.prepare(`DELETE FROM ${t} WHERE item_id = ?`).run(id)
      db.prepare("DELETE FROM item_deps WHERE item_id = ? OR dep_id = ?").run(id, id)
      db.prepare("UPDATE decisions SET item_id = NULL WHERE item_id = ?").run(id)
      db.prepare("DELETE FROM items WHERE id = ?").run(id)
    })(); publish({ type: "item", id })
  },
  goals: (id: string) => db.prepare("SELECT version, text, created FROM item_goals WHERE item_id = ? ORDER BY version").all(id) as { version: number; text: string; created: number }[],
  goal: (id: string) => (db.prepare("SELECT text FROM item_goals WHERE item_id = ? ORDER BY version DESC LIMIT 1").get(id) as { text: string } | undefined)?.text ?? "",
  planRounds: (id: string): PlanRound[] => (db.prepare("SELECT version, patch, created FROM revisions WHERE object_type = 'item_plan' AND object_id = ? ORDER BY version").all(id) as any[])
    .map((r) => { const p = json<any>(r.patch, {}); return { round: r.version, steps: p.steps ?? [], end: p.end ?? "replaced", created: r.created } }),
  addPlanRound: (ws: string, id: string, steps: PlanStep[], end: PlanRound["end"], author: string) => {
    const v = ((db.prepare("SELECT MAX(version) AS v FROM revisions WHERE object_type = 'item_plan' AND object_id = ?").get(id) as { v: number | null }).v ?? 0) + 1
    revision(ws, "item_plan", id, v, { steps, end }, author); publish({ type: "item", id }); return v
  },
  addGoal: (id: string, text: string) => { const v = (items.goals(id).at(-1)?.version ?? 0) + 1; db.prepare("INSERT INTO item_goals (item_id, version, text, created) VALUES (?, ?, ?, ?)").run(id, v, text, now()); publish({ type: "item", id }); return v },
}
export const deps = {
  of: (item: string) => (db.prepare("SELECT dep_id FROM item_deps WHERE item_id = ? ORDER BY created").all(item) as { dep_id: string }[]).map((r) => r.dep_id),
  dependents: (item: string) => (db.prepare("SELECT item_id FROM item_deps WHERE dep_id = ?").all(item) as { item_id: string }[]).map((r) => r.item_id),
  add: (item: string, dep: string) => { db.prepare("INSERT OR IGNORE INTO item_deps (item_id, dep_id, created) VALUES (?, ?, ?)").run(item, dep, now()); publish({ type: "item", id: item }) },
  remove: (item: string, dep: string) => { db.prepare("DELETE FROM item_deps WHERE item_id = ? AND dep_id = ?").run(item, dep); publish({ type: "item", id: item }) },
}
export const timeline = {
  add: (item_id: string, actor: string, kind: string, text: string, session_id: string | null = null, files: FileRef[] = []) => { db.prepare("INSERT INTO timeline (item_id, ts, actor, kind, text, session_id, files) VALUES (?, ?, ?, ?, ?, ?, ?)").run(item_id, now(), actor, kind, text, session_id, files.length ? JSON.stringify(files) : null); publish({ type: "item", id: item_id }) },
  list: (item_id: string) => (db.prepare("SELECT id, ts, actor, kind, text, session_id, files FROM timeline WHERE item_id = ? ORDER BY id").all(item_id) as any[]).map((r) => ({ ...r, files: json<FileRef[]>(r.files, []) })),
}
export const artifacts = {
  add: (item_id: string, kind: string, title: string, location: string | null) => { db.prepare("INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES (?, ?, ?, ?, ?, ?)").run(uid("art"), item_id, kind, title, location, now()); publish({ type: "item", id: item_id }) },
  list: (item_id: string) => db.prepare("SELECT * FROM artifacts WHERE item_id = ? ORDER BY created").all(item_id) as any[],
}

/* ── sessions and their event log ── */
export const sessions = {
  get: (id: string) => db.prepare("SELECT * FROM sessions WHERE id = ?").get(id) as Session | undefined,
  byToken: (token: string) => db.prepare("SELECT * FROM sessions WHERE token_hash = ?").get(sha(token)) as Session | undefined,
  ofItem: (item_id: string) => db.prepare("SELECT * FROM sessions WHERE item_id = ? ORDER BY no").all(item_id) as Session[],
  list: (ws?: string) => (ws ? db.prepare("SELECT * FROM sessions WHERE workspace_id = ? ORDER BY updated DESC LIMIT 300").all(ws) : db.prepare("SELECT * FROM sessions ORDER BY updated DESC LIMIT 1000").all()) as Session[],
  create: (p: { workspace_id: string; item_id: string | null; bot_id: string | null; harness: string; model: string | null; reasoning_effort?: string | null; cwd: string; title: string; token: string }) => {
    const id = uid("ses"), t = now(), no = p.item_id ? sessions.ofItem(p.item_id).length + 1 : 1
    db.prepare("INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, model, reasoning_effort, cwd, status, token_hash, title, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?, ?, ?)")
      .run(id, p.workspace_id, p.item_id, no, p.bot_id, p.harness, p.model, p.reasoning_effort ?? null, p.cwd, sha(p.token), p.title, t, t)
    publish({ type: "session", id, item_id: p.item_id }); return sessions.get(id)!
  },
  patch: (id: string, p: Partial<Pick<Session, "status" | "acp_session_id" | "used" | "size" | "cost" | "model" | "reasoning_effort" | "title" | "item_id" | "no">>) => {
    const c = setClause(p as any); if (!c.n) return
    db.prepare(`UPDATE sessions SET ${c.sql}, updated = ? WHERE id = ?`).run(...c.vals, now(), id)
    const s = sessions.get(id); publish({ type: "session", id, item_id: s?.item_id })
  },
  /** sessions that have a prompt history (for search) */
  search: (ws: string, q: string) => db.prepare(`SELECT DISTINCT e.session_id FROM events e JOIN sessions s ON s.id = e.session_id WHERE s.workspace_id = ? AND e.data LIKE ? LIMIT 200`).all(ws, `%${q}%`).map((r: any) => r.session_id as string),
}
export const events = {
  add: (session_id: string, kind: string, data: unknown) => {
    const r = db.prepare("INSERT INTO events (session_id, ts, kind, data) VALUES (?, ?, ?, ?)").run(session_id, now(), kind, JSON.stringify(data))
    const seq = Number(r.lastInsertRowid); publish({ type: "event", session_id, seq, kind, data }); return seq
  },
  after: (session_id: string, after = 0, limit = 5000) => (db.prepare("SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq > ? ORDER BY seq LIMIT ?").all(session_id, after, limit) as any[]).map((r) => ({ ...r, data: json(r.data, null) })),
  upTo: (session_id: string, seq: number) => (db.prepare("SELECT seq, ts, kind, data FROM events WHERE session_id = ? AND seq <= ? ORDER BY seq").all(session_id, seq) as any[]).map((r) => ({ ...r, data: json(r.data, null) })),
}
export const queued = {
  get: (id: string) => db.prepare("SELECT * FROM queued WHERE id = ?").get(id) as { id: string; session_id: string } | undefined,
  add: (session_id: string, text: string, via: string, files: FileRef[] = []) => { db.prepare("INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)").run(uid("q"), session_id, text, via, now(), files.length ? JSON.stringify(files) : null); publish({ type: "session", id: session_id }) },
  list: (session_id: string) => (db.prepare("SELECT * FROM queued WHERE session_id = ? ORDER BY created").all(session_id) as any[]).map((r) => ({ ...r, files: json<FileRef[]>(r.files, []) })) as { id: string; text: string; via: string; created: number; files: FileRef[] }[],
  take: (session_id: string) => { const xs = queued.list(session_id); db.prepare("DELETE FROM queued WHERE session_id = ?").run(session_id); return xs },
  drop: (id: string) => { const r = db.prepare("DELETE FROM queued WHERE id = ? RETURNING session_id").get(id) as any; if (r) publish({ type: "session", id: r.session_id }) },
}

/* ── decisions: the single source of 待处理 ── */
export const decisions = {
  open: (ws: string) => (db.prepare("SELECT * FROM decisions WHERE workspace_id = ? AND status = 'open' ORDER BY created").all(ws) as any[]).map(rowDec),
  recent: (ws: string, since: number) => (db.prepare("SELECT * FROM decisions WHERE workspace_id = ? AND status != 'open' AND resolved > ? ORDER BY resolved DESC LIMIT 100").all(ws, since) as any[]).map(rowDec),
  get: (id: string) => rowDec(db.prepare("SELECT * FROM decisions WHERE id = ?").get(id)),
  ofItem: (item_id: string) => (db.prepare("SELECT * FROM decisions WHERE item_id = ? ORDER BY created").all(item_id) as any[]).map(rowDec),
  create: (p: { workspace_id: string; kind: DecisionKind; subject_type: string; subject_id: string; item_id?: string | null; session_id?: string | null; question: string; draft?: string; options?: string[]; requested_by: string }) => {
    const id = uid("dec")
    db.prepare("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, session_id, question, draft, options, status, requested_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?, ?)")
      .run(id, p.workspace_id, p.kind, p.subject_type, p.subject_id, p.item_id ?? null, p.session_id ?? null, p.question, p.draft ?? "", JSON.stringify(p.options ?? []), p.requested_by, now())
    publish({ type: "decision", id, item_id: p.item_id }); return decisions.get(id)
  },
  resolve: (id: string, result: unknown, status: "resolved" | "expired" = "resolved", undo: unknown = null) => {
    db.prepare("UPDATE decisions SET status = ?, result = ?, resolved = ?, undo = ? WHERE id = ? AND status = 'open'").run(status, JSON.stringify(result), now(), undo ? JSON.stringify(undo) : null, id)
    const d = decisions.get(id); publish({ type: "decision", id, item_id: d?.item_id }); return d
  },
  reopen: (id: string) => { db.prepare("UPDATE decisions SET status = 'open', result = NULL, resolved = NULL, undo = NULL WHERE id = ?").run(id); const d = decisions.get(id); publish({ type: "decision", id, item_id: d?.item_id }); return d },
}

/* ── attachments: bytes live under data/files, the row is the descriptor everything else copies ── */
export type FileRow = { id: string; workspace_id: string; sha256: string; name: string; mime: string; size: number; created_by: string; created: number }
/** what messages, comments, queued prompts and session events carry for each attachment */
export type FileRef = { id: string; name: string; mime: string; size: number }
export const files = {
  get: (id: string) => db.prepare("SELECT * FROM files WHERE id = ?").get(id) as FileRow | undefined,
  same: (ws: string, sha256: string, name: string) => db.prepare("SELECT * FROM files WHERE workspace_id = ? AND sha256 = ? AND name = ? LIMIT 1").get(ws, sha256, name) as FileRow | undefined,
  create: (p: Omit<FileRow, "created">) => { db.prepare("INSERT INTO files (id, workspace_id, sha256, name, mime, size, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)").run(p.id, p.workspace_id, p.sha256, p.name, p.mime, p.size, p.created_by, now()); return files.get(p.id)! },
}
export const fileRef = (f: FileRow): FileRef => ({ id: f.id, name: f.name, mime: f.mime, size: f.size })

/* ── main thread with Steve, one per workspace ── */
export const messages = {
  list: (ws: string, space = "steve", limit = 200) => (db.prepare("SELECT * FROM messages WHERE workspace_id = ? AND space = ? ORDER BY created DESC LIMIT ?").all(ws, space, limit) as any[]).map((r) => ({ ...r, meta: json(r.meta, null) }) as Message).reverse(),
  add: (ws: string, author: "me" | "steve", text: string, item_id: string | null = null, meta: unknown = null, space = "steve") => { const id = uid("msg"); db.prepare("INSERT INTO messages (id, workspace_id, space, author, text, item_id, meta, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)").run(id, ws, space, author, text, item_id, meta ? JSON.stringify(meta) : null, now()); publish({ type: "message", id }); return id },
  get: (id: string) => { const r = db.prepare("SELECT * FROM messages WHERE id = ?").get(id) as any; return r ? ({ ...r, meta: json(r.meta, null) } as Message) : undefined },
  patchMeta: (id: string, meta: unknown) => { db.prepare("UPDATE messages SET meta = ? WHERE id = ?").run(JSON.stringify(meta), id); publish({ type: "message", id }) },
  lastOf: (ws: string, author: "me" | "steve") => db.prepare("SELECT * FROM messages WHERE workspace_id = ? AND space = 'steve' AND author = ? ORDER BY created DESC LIMIT 1").get(ws, author) as Message | undefined,
}

/* ── links: semantic many-to-many relations with a source ── */
export const links = {
  add: (ws: string, s: [string, string], predicate: string, o: [string, string], source: string, props: unknown = {}) =>
    db.prepare("INSERT OR IGNORE INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)").run(ws, s[0], s[1], predicate, o[0], o[1], JSON.stringify(props), source, now()),
  from: (ws: string, type: string, id: string) => db.prepare("SELECT * FROM links WHERE workspace_id = ? AND subject_type = ? AND subject_id = ?").all(ws, type, id) as any[],
}

/* ── devices: the Hub registers itself; paired nodes come later ── */
export const devices = {
  list: () => (db.prepare("SELECT * FROM devices ORDER BY created").all() as any[]).map((r) => ({ ...r, roles: json(r.roles, []) })),
  upsert: (d: { id: string; name: string; roles: string[]; os: string; version: string }) => {
    db.prepare("INSERT INTO devices (id, name, roles, os, version, seen, created) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET os = excluded.os, version = excluded.version, seen = excluded.seen")
      .run(d.id, d.name, JSON.stringify(d.roles), d.os, d.version, now(), now())
  },
  patch: (id: string, p: { name?: string; status?: string; max_sessions?: number }) => { const c = setClause(p); if (c.n) db.prepare(`UPDATE devices SET ${c.sql} WHERE id = ?`).run(...c.vals, id); publish({ type: "device", id }) },
}
