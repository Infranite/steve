/* 接入 (integrations): the Rust Hub's `GET /api/connections/integrations` — every provider Steve can connect, its ways
 * in (adapters) and planned ones, the caller's connections, and per connection the state of each capability area.
 * The TS Hub doesn't have it; `NotWired` makes the page fall back to its Unwired placeholder. */
import { modReq } from "@/api"
import type { ConnStatus, RAdapter, RFieldDef } from "@/api/accounts"

export type AreaId = "channel" | "docs" | "sheets" | "calendar" | "tasks" | "approval" | "minutes" | "mail" | "repo" | "directory" | string
export type Identity = "bot" | "self" | "app" | "account" | string
export type ScopeSpec = { scope: string; purpose: string; required: boolean }
export type CapSpec = { area: AreaId; caps: string[]; scopes: ScopeSpec[]; planned: boolean; note?: string }
export type CapState = { area: AreaId; state: "on" | "missing" | "off" | "planned"; missing_scopes: string[]; suggested_scopes: string[]; caps: string[]; note?: string }
export type IAdapter = { id: string; name: string; identity: Identity; auth: RAdapter["auth"]; device: boolean; fields?: RFieldDef[]; capabilities: CapSpec[] }
export type IConnection = {
  id: string; account_id: string; adapter: string; adapter_name: string; identity: Identity; name: string; handle: string
  status: ConnStatus; health: { last_ok: number; last_error: string; error_at: number }; last_sync: number | null; capabilities: CapState[]; warning?: string
  /** provider-specific facts (Feishu: the app, the console links, what is 未开通); absent on older Hubs */
  detail?: FeishuAppDetail | FeishuUserDetail | Record<string, unknown>
}
/** a permission row 未开通, from the check at connect time or a call Feishu refused (`refused_at`) */
export type MissingRow = { scope: string; purpose: string; level: "required" | "suggested" | "directory" | "optional"; status: "missing"; refused_at?: number }
export type FeishuAppDetail = {
  app_id: string; domain: "feishu" | "lark"; app_name: string | null; console_url: string; permissions_url: string; publish_url: string
  missing: MissingRow[]; grant: { url: string; from: "feishu" | "steve" } | null; checked_at: number | null; refused_at: number | null
}
export type FeishuUserDetail = { app_connection: string | null; missing: MissingRow[]; requested: string[]; refused_at: number | null }
/** one installation (a Feishu app, a GitHub App, a mailbox) with the identities Steve works through: itself, and you */
export type IIdentity = { kind: "steve" | "you"; connection: IConnection }
export type IInstallation = { id: string; provider: string; provider_name: string; name: string; status: ConnStatus; missing: number; identities: IIdentity[] }
export type IProvider = {
  id: string; name: string; status: "supported" | "planned"; adapters: IAdapter[]; planned: { name: string; areas: AreaId[]; note?: string }[]
  areas: { area: AreaId; state: "supported" | "planned" }[]; connections: IConnection[]
}
export type Integrations = { areas: { id: AreaId; label: string }[]; providers: IProvider[]; callback_urls: string[]; installations?: IInstallation[] }

export const integrationsApi = {
  all: () => modReq<Integrations>("GET", "/connections/integrations"),
}

/* wording the Hub leaves to the web */
const AREA_LABEL: Record<string, string> = { channel: "渠道", send_as_you: "以你的名义发送", docs: "文档/知识库", sheets: "表格/多维表格", calendar: "日历", tasks: "任务", approval: "审批", minutes: "妙记", mail: "邮件", repo: "代码仓库", directory: "通讯录" }
export const areaLabel = (a: string) => AREA_LABEL[a] ?? a
export const IDENTITY_LABEL: Record<string, string> = { bot: "应用机器人", self: "你本人", app: "已安装的应用", account: "账号" }
export const identityLabel = (i: string) => IDENTITY_LABEL[i] ?? i
/** an adapter of the integrations list in the shape the generic connect dialog takes */
export const asAdapter = (a: IAdapter, system: string): RAdapter => ({ id: a.id, system, name: a.name, auth: a.auth, caps: a.capabilities.flatMap((c) => c.caps), device: a.device, fields: a.fields })
/** the channel-capable connections' provider, by adapter id (the 渠道 view's badges and filters) */
export function providerByAdapter(x: Integrations): Record<string, { id: string; name: string; identity: Identity; channel: boolean }> {
  const out: Record<string, { id: string; name: string; identity: Identity; channel: boolean }> = {}
  for (const p of x.providers) for (const a of p.adapters) out[a.id] = { id: p.id, name: p.name, identity: a.identity, channel: a.capabilities.some((c) => c.area === "channel" && !c.planned) }
  return out
}

/** the installations of a provider; on a Hub without `installations`, one per connection (Feishu: you join your app) */
export function installationsOf(x: Integrations, provider: string): IInstallation[] {
  if (x.installations) return x.installations.filter((i) => i.provider === provider)
  const p = x.providers.find((y) => y.id === provider)
  if (!p) return []
  const out: IInstallation[] = []
  for (const c of p.connections) {
    const key = (c.detail as FeishuUserDetail | undefined)?.app_connection ?? c.id
    const kind = c.identity === "bot" || c.identity === "app" ? "steve" : "you"
    let i = out.find((y) => y.id === key)
    if (!i) out.push((i = { id: key, provider, provider_name: p.name, name: c.name || c.handle, status: c.status, missing: 0, identities: [] }))
    i.identities.push({ kind, connection: c })
    if (kind === "steve") i.name = c.name || c.handle
    i.missing += c.capabilities.reduce((n, a) => n + a.missing_scopes.length, 0)
  }
  out.forEach((i) => i.identities.sort((a, b) => +(a.kind !== "steve") - +(b.kind !== "steve")))
  return out
}
/** the open-platform page of app `appId` with `scopes` preselected; `tenant` for the bot's scopes (the form Feishu puts into 99991672 answers) */
export const feishuGrantUrl = (domain: string, appId: string, scopes: string[], tenant: boolean) =>
  `${domain === "lark" ? "https://open.larksuite.com" : "https://open.feishu.cn"}/app/${encodeURIComponent(appId)}/auth?q=${scopes.join(",")}&op_from=openapi${tenant ? "&token_type=tenant" : ""}`
