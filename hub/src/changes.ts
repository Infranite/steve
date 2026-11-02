/* Conversational configuration (变更集): the user says what they want, Steve drafts a field-level change,
 * the user applies it, and every applied change can be undone. Drafts live in memory; applied ones are revisions. */
import { bots, projects, settings, tags, workspaces, audit, uid, type Bot } from "./repo.js"
import { draftFields, claudePrint, firstJson, TAG_CATS, type FieldSpec } from "./gateway.js"

/* ── settings schema, shared with the API ── */
export type SettingSpec = { key: string; label: string; values: string[]; def: string; live: boolean; labels?: Record<string, string> }
export const SETTINGS: Record<string, SettingSpec[]> = {
  general: [
    { key: "lang", label: "语言", values: ["简体中文", "English"], def: "简体中文", live: false },
    { key: "home", label: "打开时进入", values: ["对话", "待处理", "事项"], def: "对话", live: true },
    { key: "awayH", label: "离开期间汇报", values: ["2", "4", "8", "0"], def: "4", live: true, labels: { "2": "2 小时", "4": "4 小时", "8": "8 小时", "0": "不汇报" } },
  ],
  items: [
    { key: "accDefault", label: "默认验收方式", values: ["manual", "auto"], def: "manual", live: true, labels: { manual: "需要你验收", auto: "自动验收" } },
    { key: "depsAfter", label: "前置完成后", values: ["自动开始", "提醒我开始"], def: "自动开始", live: true },
    { key: "handoff", label: "交接阈值", values: ["70", "80", "90"], def: "80", live: true, labels: { "70": "70%", "80": "80%", "90": "90%" } },
    { key: "archDays", label: "完成后自动归档", values: ["7", "30", "90", "0"], def: "30", live: true, labels: { "7": "7 天", "30": "30 天", "90": "90 天", "0": "不自动归档" } },
  ],
  notify: [
    { key: "quiet", label: "免打扰", values: ["23:00 – 08:00", "22:00 – 09:00", "关闭"], def: "23:00 – 08:00", live: false },
    { key: "digestAt", label: "每天汇报", values: ["08:30", "09:00", "不发"], def: "08:30", live: false },
  ],
  look: [
    { key: "fontSize", label: "字号", values: ["标准", "大"], def: "标准", live: true },
    { key: "reduceMotion", label: "减弱动态效果", values: ["开", "关"], def: "关", live: true },
  ],
  data: [
    { key: "keepSess", label: "会话记录保留", values: ["90 天", "1 年", "永久"], def: "永久", live: false },
  ],
}
export const settingDefaults = () => Object.fromEntries(Object.values(SETTINGS).flat().map((s) => [s.key, s.def]))
const ACC: Record<string, string> = { inherit: "跟随全局", manual: "需要你验收", auto: "自动验收" }
export const PERM: Record<string, string> = { plan: "只读：只出方案，不改文件", default: "每次改文件或运行命令都问你", acceptEdits: "可以改项目目录里的文件，运行命令要问你", auto: "可以改文件、运行命令；对外操作仍需你批准" }
const PRO = ["只在必要时", "适中", "积极"]

export type Target = { kind: "bot" | "project" | "settings" | "tags" | "workspace" | "global"; id?: string }
export type Change = { label: string; from: string; to: string }
export type Proposal = { id: string; ws: string; title: string; target: Target; href: string; changes: Change[]; patch: Record<string, unknown>; created: number }
const drafts = new Map<string, Proposal>()
const applied = new Map<string, { target: Target; before: Record<string, unknown>; ws: string }>()

const show = (v: unknown) => (v == null || v === "" ? "（空）" : String(v).length > 40 ? String(v).slice(0, 40) + "…" : String(v))
function botFields(b: Bot, ws: string): (FieldSpec & { enc?: (v: any) => unknown; dec?: (v: any) => string })[] {
  const ps = projects.list(ws)
  return [
    { key: "name", label: "名字", type: "text", current: b.name },
    { key: "role", label: "职责", type: "text", current: b.role },
    { key: "persona", label: "说话方式", type: "text", current: b.persona },
    { key: "proactive", label: "主动程度", type: "enum", values: PRO, current: PRO[b.proactive], enc: (v) => Math.max(0, PRO.indexOf(String(v))), dec: (v) => PRO[v] },
    { key: "rules", label: "工作规范", type: "text", current: b.rules },
    { key: "accept_mode", label: "交付验收", type: "enum", values: Object.values(ACC), current: ACC[b.accept_mode], enc: (v) => Object.keys(ACC).find((k) => ACC[k] === v) ?? "inherit", dec: (v) => ACC[v] },
    { key: "model", label: "模型（opus、sonnet、haiku，留空为默认）", type: "text", current: b.model ?? "" , enc: (v) => (v ? String(v) : null) },
    { key: "reasoning_effort", label: "思考强度", type: "enum", values: ["默认", ...(b.harness === "codex" ? ["none", "minimal", "low", "medium", "high", "xhigh"] : ["low", "medium", "high", "max"])], current: b.reasoning_effort ?? "默认", enc: (v) => v === "默认" ? null : String(v), dec: (v) => v ?? "默认" },
    { key: "perm_mode", label: "权限", type: "enum", values: Object.values(PERM), current: PERM[b.perm_mode], enc: (v) => Object.keys(PERM).find((k) => PERM[k] === v) ?? b.perm_mode, dec: (v) => PERM[v] },
    { key: "default_project", label: "默认项目", type: "enum", values: ["无", ...ps.map((p) => p.name)], current: ps.find((p) => p.id === b.default_project)?.name ?? "无", enc: (v) => ps.find((p) => p.name === v)?.id ?? null, dec: (v) => ps.find((p) => p.id === v)?.name ?? "无" },
  ]
}

export async function draft(ws: string, target: Target, request: string): Promise<Proposal> {
  if (target.kind === "global") target = await pickTarget(ws, request)
  let title = "", href = "", changes: Change[] = [], patch: Record<string, unknown> = {}
  if (target.kind === "bot") {
    const b = bots.get(target.id!); if (!b || !bots.inWorkspace(b.id, ws)) throw new Error("没有这个 Bot")
    const fs = botFields(b, ws), out = await draftFields(`the bot "${b.name}"`, fs, request)
    for (const c of out) { const f = fs.find((x) => x.key === c.key)!; const v = f.enc ? f.enc(c.value) : c.value; if (JSON.stringify(v) === JSON.stringify((b as any)[f.key])) continue; patch[f.key] = v; changes.push({ label: f.label, from: show(f.current), to: show(f.dec ? f.dec(v) : v) }) }
    title = `${b.main ? b.name : b.name + " Bot"} 的设置`; href = "#/bots/" + b.id
  } else if (target.kind === "project") {
    const p = projects.get(target.id!); if (!p || p.workspace_id !== ws) throw new Error("没有这个项目")
    const bs = bots.list(ws)
    const fs: FieldSpec[] = [{ key: "name", label: "名称", type: "text", current: p.name }, { key: "description", label: "说明", type: "text", current: p.description },
      { key: "default_bot", label: "默认 Bot", type: "enum", values: ["无", ...bs.map((b) => b.name)], current: bs.find((b) => b.id === p.default_bot)?.name ?? "无" },
      { key: "worktree", label: "每个事项单独开 worktree 分支", type: "enum", values: ["开", "关"], current: p.worktree ? "开" : "关" }]
    for (const c of await draftFields(`the project "${p.name}"`, fs, request)) {
      const f = fs.find((x) => x.key === c.key)!
      const v = c.key === "default_bot" ? bs.find((b) => b.name === c.value)?.id ?? null : c.key === "worktree" ? c.value === "开" : c.value
      if (JSON.stringify(v) === JSON.stringify((p as any)[c.key])) continue
      patch[c.key] = v; changes.push({ label: f.label, from: show(f.current), to: show(c.value) })
    }
    title = `项目 ${p.name}`; href = "#/projects/" + p.id
  } else if (target.kind === "settings") {
    const cat = target.id ?? "general", specs = cat === "items" ? [...SETTINGS.items] : SETTINGS[cat] ?? []
    const cur = { ...settingDefaults(), ...settings.all(ws) } as Record<string, unknown>
    const w = workspaces.get(ws)!
    const lab = (k: string, v: unknown) => specs.find((x) => x.key === k)?.labels?.[String(v)] ?? String(v)
    const fs: FieldSpec[] = specs.map((s) => ({ key: s.key, label: s.label + (s.labels ? `（值：${Object.entries(s.labels).map(([k, v]) => `${k}=${v}`).join("，")}）` : ""), type: "enum", values: s.values, current: cur[s.key] }))
    if (cat === "items") fs.push({ key: "item_prefix", label: "事项编号前缀（大写字母）", type: "text", current: w.item_prefix })
    for (const c of await draftFields(`Steve settings · ${cat}`, fs, request)) {
      const f = fs.find((x) => x.key === c.key)!
      const v = c.key === "item_prefix" ? String(c.value).toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6) : String(c.value)
      if (f.values && !f.values.includes(v)) continue
      if (!v || v === String(f.current)) continue
      patch[c.key] = v; changes.push({ label: f.label.replace(/（值：.*）$/, ""), from: show(lab(c.key, f.current)), to: show(lab(c.key, v)) })
    }
    title = "设置 · " + ({ general: "通用", items: "事项与验收", notify: "通知", look: "外观", data: "数据与隐私" } as Record<string, string>)[cat]; href = "#/settings/" + cat
  } else if (target.kind === "tags") {
    const ts = tags.list(ws)
    const prompt = [`Manage item tags in Steve. Existing tags (name · category · description):`, ...ts.map((t) => `  ${t.name} · ${t.cat} · ${t.description}`),
      `Categories: ${TAG_CATS.join("/")}. Request: ${JSON.stringify(request)}`,
      'Reply with ONLY JSON: {"create":[{"name":"","cat":"","description":""}],"edit":[{"name":"<existing name>","description":"<new description>"}]}'].join("\n")
    const r = firstJson(await claudePrint(prompt, "smart", 90_000)) ?? {}
    const create = (r.create ?? []).filter((t: any) => t?.name && TAG_CATS.includes(t.cat) && !ts.some((x) => x.name === t.name))
    const edit = (r.edit ?? []).filter((e: any) => ts.some((x) => x.name === e.name) && e.description)
    for (const t of create) changes.push({ label: "新建标签 · " + t.cat, from: "—", to: `${t.name}：${t.description}` })
    for (const e of edit) changes.push({ label: "描述 · " + e.name, from: show(ts.find((x) => x.name === e.name)!.description), to: show(e.description) })
    patch = { create, edit }; title = "标签"; href = "#/settings/tags"
  }
  const p: Proposal = { id: uid("chg"), ws, title, target, href, changes, patch, created: Date.now() }
  drafts.set(p.id, p); return p
}

async function pickTarget(ws: string, request: string): Promise<Target> {
  const bs = bots.list(ws), ps = projects.list(ws)
  const opts = [...bs.map((b) => `bot:${b.id} — ${b.name}（${b.role}）`), ...ps.map((p) => `project:${p.id} — 项目 ${p.name}`), "settings:general — 语言、启动页、汇报", "settings:items — 验收、编号、前置、交接、归档", "settings:notify — 通知、免打扰", "settings:look — 字号、动态效果", "settings:data — 保留期限", "tags — 事项标签"]
  const r = firstJson(await claudePrint(`Which Steve configuration does this request change? Options:\n${opts.join("\n")}\nRequest: ${JSON.stringify(request)}\nReply with ONLY JSON {"target":"<option before the dash>"}`, "fast", 60_000).catch(() => "{}"))
  const t = String(r?.target ?? "settings:general").trim(), [kind, id] = t.split(":")
  return kind === "tags" ? { kind: "tags" } : { kind: kind as Target["kind"], id }
}

export function apply(id: string, actor: string, ws: string) {
  const p = drafts.get(id); if (!p || p.ws !== ws) throw new Error("这份草稿已经失效，请重新说一次")
  drafts.delete(id)
  let before: Record<string, unknown> = {}
  if (p.target.kind === "bot") { const b = bots.get(p.target.id!)!; before = Object.fromEntries(Object.keys(p.patch).map((k) => [k, (b as any)[k]])); bots.patch(b.id, p.patch as any, actor) }
  if (p.target.kind === "project") { const x = projects.get(p.target.id!)!; before = Object.fromEntries(Object.keys(p.patch).map((k) => [k, (x as any)[k]])); projects.patch(x.id, p.patch as any, actor) }
  if (p.target.kind === "settings") {
    const cur = { ...settingDefaults(), ...settings.all(p.ws) } as Record<string, unknown>, { item_prefix, ...rest } = p.patch
    before = Object.fromEntries(Object.keys(rest).map((k) => [k, cur[k]]))
    if (Object.keys(rest).length) settings.set(p.ws, rest, actor)
    if (item_prefix) { before.item_prefix = workspaces.get(p.ws)!.item_prefix; workspaces.patch(p.ws, { item_prefix: String(item_prefix) }) }
  }
  if (p.target.kind === "tags") {
    const made: string[] = []
    for (const t of (p.patch.create as any[]) ?? []) made.push(tags.create(p.ws, { name: t.name, cat: t.cat, description: t.description }, "Steve").id)
    const edits: Record<string, string> = {}
    for (const e of (p.patch.edit as any[]) ?? []) { const t = tags.byName(p.ws, e.name)!; edits[t.id] = t.description; tags.patch(t.id, { description: e.description }) }
    before = { made, edits }
  }
  applied.set(id, { target: p.target, before, ws: p.ws }); audit(actor, "config.apply", id, { target: p.target, patch: p.patch }, p.ws)
  return { ok: true, undo: id }
}
export function undo(id: string, actor: string, ws: string) {
  const a = applied.get(id); if (!a || a.ws !== ws) throw new Error("这次改动已经不能撤销了")
  applied.delete(id)
  if (a.target.kind === "bot") bots.patch(a.target.id!, a.before as any, actor)
  if (a.target.kind === "project") projects.patch(a.target.id!, a.before as any, actor)
  if (a.target.kind === "settings") { const { item_prefix, ...rest } = a.before; if (Object.keys(rest).length) settings.set(a.ws, rest, actor); if (item_prefix) workspaces.patch(a.ws, { item_prefix: String(item_prefix) }) }
  if (a.target.kind === "tags") { for (const t of (a.before.made as string[]) ?? []) tags.remove(t); for (const [t, d] of Object.entries((a.before.edits as Record<string, string>) ?? {})) tags.patch(t, { description: d }) }
  audit(actor, "config.undo", id, {}, a.ws); return { ok: true }
}
