/* Channels and external personas (channels-personas.md §5, feishu-lark-cli.md §2.4): the Rust Hub's /api/spaces,
 * /api/personas, /api/channels/feishu/* and the accounts module's /api/connections (with /connections/integrations
 * for which provider a connection belongs to and whether it can carry messages). The TS Hub has none of them;
 * `NotWired` makes a page fall back to its Unwired placeholder. Nothing here sends a message. */
import { modReq, qs, isNotWired } from "@/api"
import { accountsApi, type RAccount, type RConnection, type RDevice, type RDeviceStatus } from "@/api/accounts"
import { integrationsApi, providerByAdapter } from "@/api/integrations"

const e = encodeURIComponent
const get = <T>(p: string) => modReq<T>("GET", p)
const post = <T = any>(p: string, b: unknown = {}) => modReq<T>("POST", p, b)
const patch = <T = any>(p: string, b: unknown) => modReq<T>("PATCH", p, b)
const del = <T = any>(p: string) => modReq<T>("DELETE", p)
/** `undefined` when this Hub has no such route (an optional, newer route), the error otherwise */
const optional = <T>(p: Promise<T>) => p.catch((x) => (isNotWired(x) ? undefined : Promise.reject(x)))

/* ───────── shapes (§5.1) ───────── */
export type PersonaRef = { id: string; name: string; inherited?: boolean }
export type When =
  | { kind: "mention_or_dm" | "mention" | "dm" | "reply_to_me" | "external" | "all" | "other_group" }
  | { kind: "keyword"; words: string[] }
  | { kind: "intent"; text: string }
  | { kind: "member"; parties: string[]; teams: string[] }
  | { kind: "hours"; outside: boolean; start: string; end: string; days: number[] }
export type Act = "reply" | "draft" | "record" | "create_item" | "handoff"
export type Rule = { when: When; act: Act; mode?: "start" | "propose" }
export type Level = "public" | "conclusion" | "reasoning" | "never"
/** your attention to a space (domain-model.md §1.4): 实时 · 不提醒 · 屏蔽 */
export type Attention = "realtime" | "quiet" | "blocked"
/** Steve's role there, from the voice identity's rules (`muted` = 只记录) */
export type Role = "answer" | "draft" | "record" | "observe"
export type SpaceIdentity = { connection_id: string; mode: "bot" | "self"; name: string; account_id: string }
/** a setting that is on but does not apply here (or what applies instead), next to that setting */
export type Conflict = { key: "rules" | "flow" | "trigger" | "voice" | string; level: "conflict" | "info"; text: string }
export type Space = {
  /** `connection_id` is the voice: the identity Steve acts through here */
  id: string; connection_id: string; kind: "group" | "topic_group" | "dm" | "self_dm" | "channel" | "thread" | "mail_thread" | "meeting" | string; name: string; member_count: number; has_external: boolean; status: "active" | "left" | "archived" | string
  persona: PersonaRef | null; duty: string; rules: Rule[] | null; effective_rules: Rule[]; flow_id: string | null; muted: boolean; collab: boolean; memory_scope: "space" | "persona"; disclosure: Level
  item_defaults: { tags: string[]; project_id: string | null; bot_id: string | null }; configured: boolean; teams: string[]; revision: number; last_message: number | null
  /* newer Hubs (019); older ones leave them out */
  attention?: Attention; role?: Role; voice?: (SpaceIdentity & { chosen: boolean }) | null; identities?: SpaceIdentity[]
  title?: string; counterpart?: { account_id: string; party_id: string | null; name: string; is_bot: boolean } | null; self_chat?: boolean
  suggest_quiet?: string | null; conflicts?: Conflict[]
}
export type BasisEntry = { source_type: string; source_id: string; source_label: string; level: Level; effective: Level; used: string; out: "verbatim" | "conclusion" | "reasoning" | "blocked" }
export type Basis = { verdict: "pass" | "rewritten" | "drafted"; entries: BasisEntry[]; rewrites: { from: string; to: string; reason: string }[] }
export type ChanMessage = {
  id: string; space_id: string; direction: "in" | "out"; sender: { name: string; party_id: string | null; account_id?: string | null }; persona_id: string | null; on_behalf: "none" | "persona" | "proxy"
  text: string | null; status: "received" | "draft" | "pending" | "sent" | "failed" | "recalled" | "blocked"; work_item_id: string | null; decision_id: string | null; created: number; sent: number | null; purged: number | null; basis: Basis | null
}
export type SpaceDetail = Space & { recent: ChanMessage[] }
export type Reveal = { name: boolean; role: boolean; ai: boolean; steve: boolean; owner: boolean }
export type Persona = {
  id: string; name: string; avatar: { seed?: string }; bot_id: string; intro: string; soul: string; reveal: Reveal; signature: string; is_default: boolean; intro_preview: string
  used_by: { accounts: { account_id: string; name: string }[]; spaces: { id: string; name: string; own: boolean }[] }; revision: number
}
export type PersonaInput = { name?: string; bot_id?: string; intro?: string; soul?: string; reveal?: Partial<Reveal>; signature?: string }
export type SpacePatch = Partial<{ persona_id: string | null; duty: string; rules: Rule[] | null; muted: boolean; attention: Attention; voice: string | null; collab: boolean; memory_scope: "space" | "persona"; disclosure: Level; item_defaults: Space["item_defaults"] }>
export type Trigger = { on: boolean; words: string[]; where: string[]; mark: boolean }
/** a permission row the Hub checked when it verified the bot (feishu.md §2.5) */
export type PermissionCheck = { scope: string; purpose: string; level: "required" | "suggested" | "directory" | "optional"; status: "granted" | "missing" | "unknown" | "not_requested" }

/** 接入账号: a connection that receives messages, with what the channel module knows about it */
export type ChannelAccount = {
  connection: RConnection; account: RAccount; mode: "bot" | "self"; name: string
  /** where it comes from (飞书 / Lark, Slack…); from the integrations list, else the adapter id */
  provider: { id: string; name: string }
  /** the bot's default persona (`null` for your own account, which speaks as you) */
  persona: PersonaRef | null; personaBot: string | null
  /** the account's default rules; `null` when this Hub doesn't say (no `GET /channels/:id` yet) */
  rules: Rule[] | null; trigger: Trigger | null
  /** whether this Hub can change the account's persona, rules and trigger (`PATCH /channels/:id`) */
  editable: boolean
  spaces: Space[]; permissions: PermissionCheck[] | null
}

export type FeishuRegistration = {
  id: string; brand: "feishu" | "lark"
  state: "pending" | "credentials_saved" | "verifying" | "succeeded" | "denied" | "expired" | "failed" | "cancelled"
  verification_uri_complete?: string; user_code?: string; expires_at: number; interval: number
  connection_id?: string; account_id?: string; error?: string; error_kind?: string; app_may_exist?: boolean
}

export const FEISHU_APP = "feishu_app", FEISHU_USER = "feishu_user"
type Providers = ReturnType<typeof providerByAdapter>
/** the integrations list's view of the adapters; `{}` on a Hub without it */
const providers = (): Promise<Providers> => integrationsApi.all().then(providerByAdapter, () => ({}))
const MSG = ["msg.receive", "msg.send", "msg.send_as_user"]
/** a connection that carries messages: its adapter has the 渠道 area, or (older Hubs) it has a message cap */
export const isChannelConn = (c: RConnection, ps: Providers = {}) => ps[c.adapter]?.channel ?? [...c.caps, ...c.available_caps].some((x) => MSG.includes(x))
const modeOf = (c: RConnection, ps: Providers): "bot" | "self" => (ps[c.adapter]?.identity === "self" || c.adapter === FEISHU_USER ? "self" : "bot")
/** the verify result, wherever the Hub puts it on a connection */
export const permissionsOf = (x: unknown): PermissionCheck[] | null => { const o = x as any; const p = o?.permissions ?? o?.config?.permissions; return Array.isArray(p) ? p : null }

/** a persona's default binding to an account, from the persona's own `used_by` */
function personaOfAccount(ps: Persona[], account: string): PersonaRef | null {
  const p = ps.find((x) => x.used_by.accounts.some((a) => a.account_id === account))
  return p ? { id: p.id, name: p.name } : null
}
/** the account's rules as the spaces that follow it see them */
const inheritedRules = (ss: Space[]) => ss.find((s) => s.rules === null)?.effective_rules ?? null

type SpecAccount = { connection_id: string; persona: PersonaRef | null; rules: Rule[]; trigger: Trigger | null }

async function compose(c: RConnection, ps: Persona[], all: Space[], pv: Providers): Promise<ChannelAccount> {
  const [account, spec] = await Promise.all([accountsApi.account(c.account_id), optional(get<SpecAccount>("/channels/" + e(c.id)))])
  const spaces = all.filter((s) => s.connection_id === c.id), mode = modeOf(c, pv)
  const provider = pv[c.adapter] ? { id: pv[c.adapter].id, name: pv[c.adapter].name } : { id: c.adapter, name: account.system?.name || c.adapter }
  const persona = mode === "self" ? null : spec ? spec.persona : personaOfAccount(ps, c.account_id)
  return {
    connection: c, account, mode, name: account.display_name || account.handle, provider, persona, personaBot: persona ? ps.find((p) => p.id === persona.id)?.bot_id ?? null : null,
    rules: spec?.rules ?? inheritedRules(spaces), trigger: spec?.trigger ?? null, editable: !!spec, spaces, permissions: permissionsOf(spec) ?? permissionsOf(c),
  }
}

export const channelsApi = {
  /** every channel account with its spaces (connections + spaces + personas; `/channels/:id` when the Hub has it) */
  accounts: async (): Promise<ChannelAccount[]> => {
    const [conns, spaces, ps, pv] = await Promise.all([accountsApi.connections(), channelsApi.spaces(), channelsApi.personas(), providers()])
    return Promise.all(conns.filter((c) => isChannelConn(c, pv)).map((c) => compose(c, ps, spaces, pv)))
  },
  /** one account by its connection or account id (the accounts page links with the account id) */
  account: async (id: string): Promise<ChannelAccount | null> => {
    const [conns, ps, pv] = await Promise.all([accountsApi.connections(), channelsApi.personas(), providers()])
    const c = conns.find((x) => x.id === id || x.account_id === id)
    if (!c) return null
    return compose(c, ps, await channelsApi.spaces({ connection: c.id }), pv)
  },
  patchAccount: (connection: string, b: { duty?: string; rules?: Rule[]; trigger?: Partial<Trigger>; persona_id?: string }) => patch<SpecAccount>("/channels/" + e(connection), b),
  /** connect the Feishu bot: `POST /channels` when the Hub has it (it also binds the default persona), else the plain connection */
  connectBot: async (fields: Record<string, string>, persona_id?: string): Promise<{ connection_id: string; account_id: string; permissions: PermissionCheck[] | null; bound: boolean }> => {
    const r = await optional(post<any>("/channels", { adapter: FEISHU_APP, fields, persona_id }))
    if (r) return { connection_id: r.connection_id, account_id: r.account_id, permissions: permissionsOf(r), bound: true }
    const c = await accountsApi.connect(FEISHU_APP, fields)
    return { connection_id: c.id, account_id: c.account_id, permissions: permissionsOf(c), bound: false }
  },
  /** “授权你本人”: the browser flow on the bot's app (merges a contact the bot already made for you into “我”) */
  userOAuth: (app_connection: string, scopes?: string[]) => post<{ url: string }>("/channels/feishu/user/oauth/start", scopes?.length ? { app_connection, scopes } : { app_connection }),
  userDevice: (app_connection: string, scopes?: string[], reauth_of?: string) => post<RDevice>("/channels/feishu/device/start", { app_connection, ...(scopes?.length ? { scopes } : {}), ...(reauth_of ? { reauth_of } : {}) }),
  userDeviceStatus: (id: string) => get<RDeviceStatus>("/channels/feishu/device/" + e(id)),

  userDeviceCancel: (id: string) => post<RDeviceStatus>("/channels/feishu/device/" + e(id) + "/cancel"),
  registrationStart: (brand: "feishu" | "lark") => post<FeishuRegistration>("/channels/feishu/registrations", { brand }),
  registrationCurrent: () => get<FeishuRegistration | null>("/channels/feishu/registrations/current"),
  registration: (id: string) => get<FeishuRegistration>("/channels/feishu/registrations/" + e(id)),
  registrationCancel: (id: string) => post<FeishuRegistration>("/channels/feishu/registrations/" + e(id) + "/cancel"),
  registrationRetry: (id: string) => post<FeishuRegistration>("/channels/feishu/registrations/" + e(id) + "/retry"),

  spaces: (p: { connection?: string; q?: string } = {}) => get<Space[]>("/spaces" + qs(p)),
  space: (id: string) => get<SpaceDetail>("/spaces/" + e(id)),
  patchSpace: (id: string, b: SpacePatch) => patch<Space>("/spaces/" + e(id), b),
  messages: (id: string, before?: number, limit = 30) => get<ChanMessage[]>(`/spaces/${e(id)}/messages` + qs({ before, limit })),
  toFlow: (id: string) => post<Space>(`/spaces/${e(id)}/rules/to-flow`),
  backToRules: (id: string) => del<Space>(`/spaces/${e(id)}/flow`),

  personas: () => get<Persona[]>("/personas"),
  persona: (id: string) => get<Persona>("/personas/" + e(id)),
  newPersona: (b: PersonaInput & { name: string; bot_id: string }) => post<Persona>("/personas", b),
  patchPersona: (id: string, b: PersonaInput) => patch<Persona>("/personas/" + e(id), b),
  deletePersona: (id: string) => del<{ ok: true; moved: { accounts: number; spaces: number } }>("/personas/" + e(id)),
}

/* ───────── wording the Hub leaves to the web ───────── */
export const LEVEL: Record<Level, [string, string]> = {
  public: ["公开", "原文和细节都能说"], conclusion: ["可转述结论", "只说结论，不带来源、人名、原话、数字"],
  reasoning: ["仅用于推理", "能用来推理，回复里不出现可辨认的内容"], never: ["不可使用", "不检索，不使用"],
}
export const OUT_LABEL: Record<BasisEntry["out"], [string, string]> = { verbatim: ["原样使用", "done"], conclusion: ["改写为结论", "prop"], reasoning: ["仅用于推理", "idle"], blocked: ["已拦下", "wait"] }
export const SPACE_KIND: Record<string, string> = { group: "群", topic_group: "话题群", dm: "私聊", self_dm: "和自己的对话", channel: "话题群", thread: "话题", mail_thread: "邮件往来", meeting: "会议" }
/** 实时 · 不提醒 · 屏蔽, with what each means */
export const ATTENTION: Record<Attention, [string, string]> = {
  realtime: ["实时", "照常处理：按 Steve 的角色回答、起草或转给你"],
  quiet: ["不提醒", "只记录、可搜索；不进待处理、不起草、不提醒你"],
  blocked: ["屏蔽", "不处理，也不保存新消息；列表里默认隐藏"],
}
export const ROLE: Record<Role, [string, string]> = {
  answer: ["回答", "按规则直接回复"], draft: ["起草", "起草回复，等你批准再发"], record: ["只记录", "不发言，只记录；转给你的仍进待处理"], observe: ["不参与", "没有能发言的身份"],
}
export const attentionOf = (s: Pick<Space, "attention">): Attention => s.attention ?? "realtime"
export const isDm = (s: Pick<Space, "kind">) => s.kind === "dm" || s.kind === "self_dm"
/** what a list or header calls the space: a DM after the other party, your chat with yourself 你自己 */
export const spaceTitle = (s: Pick<Space, "title" | "name" | "kind" | "self_chat">) => s.title || (s.self_chat || s.kind === "self_dm" ? "你自己" : s.name || (isDm(s) ? "私聊" : "群"))
export const TRIGGER_EVERYWHERE = "*", TRIGGER_ALL_DMS = "dm:*"
export const MSG_STATUS: Partial<Record<ChanMessage["status"], string>> = { draft: "草稿，等你批准", pending: "发送中", failed: "没发出去", recalled: "已撤回", blocked: "已拦下" }

/* ───────── what an outbound message answers (its approval card) ───────── */
export type ROutContext = {
  space: { id: string; name: string; kind: string; external: boolean }
  as_you: boolean; persona: string | null; text: string | null
  reply_to: { id: string; sender: string; text: string | null; at: number } | null
  before: { sender: string; text: string | null; at: number }[]
}
export const outContext = (id: string) => optional(get<ROutContext>(`/chan-messages/${e(id)}/context`))
