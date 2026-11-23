/** Providers, their adapters and current connections. */
import { modReq } from "@/api"
import { lazy } from "@/api/accounts"
import { t } from "@/i18n"
import type { ConnStatus, RAdapter, RFieldDef } from "@/api/accounts"
import type { ChannelSummary, Pairing } from "@/integrations/sides"

export type AreaId = "channel" | "docs" | "sheets" | "calendar" | "tasks" | "approval" | "minutes" | "mail" | "repo" | "directory" | string
export type Identity = "bot" | "self" | "app" | "account" | string
export type ScopeSpec = { scope: string; purpose: string; required: boolean }
export type CapSpec = { area: AreaId; caps: string[]; scopes: ScopeSpec[]; planned: boolean; note?: string }
export type CapState = { area: AreaId; state: "on" | "missing" | "off" | "planned"; missing_scopes: string[]; suggested_scopes: string[]; caps: string[]; note?: string }
export type IAdapter = {
  id: string; name: string; identity: Identity; auth: RAdapter["auth"]; device: boolean; fields?: RFieldDef[]; capabilities: CapSpec[]
  /** whom its connections pair with (`null`: nobody); what its channel side declares (`null`: not a channel side) */
  pairing: Pairing | null; channel: ChannelSummary | null
}
export type IConnection = {
  id: string; account_id: string; adapter: string; adapter_name: string; identity: Identity; name: string; handle: string
  status: ConnStatus; health: { last_ok: number; last_error: string; error_at: number }; last_sync: number | null; capabilities: CapState[]; warning?: string
  /** provider-specific facts (Feishu: the app, the console links, what is 未开通); absent when the provider has none */
  detail?: FeishuAppDetail | FeishuUserDetail | Record<string, unknown>
  /** the Hub has its adapter */
  installed: boolean
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
  id: string; name: string; status: "supported"; adapters: IAdapter[]
  areas: { area: AreaId; state: "supported" | "planned" }[]; connections: IConnection[]
}
export type Integrations = { areas: { id: AreaId; label: string }[]; providers: IProvider[]; callback_urls: string[]; installations: IInstallation[] }

export const integrationsApi = {
  all: () => modReq<Integrations>("GET", "/connections/integrations"),
}

/* wording the Hub leaves to the web */
const AREA_LABEL: Record<string, string> = lazy({
  channel: () => t("api:area.channel"), send_as_you: () => t("api:area.sendAsYou"), docs: () => t("api:area.docs"), sheets: () => t("api:area.sheets"), calendar: () => t("api:area.calendar"), tasks: () => t("api:area.tasks"),
  approval: () => t("api:area.approval"), minutes: () => t("api:area.minutes"), mail: () => t("api:area.mail"), repo: () => t("api:area.repo"), directory: () => t("api:area.directory"),
})
export const areaLabel = (a: string) => AREA_LABEL[a] ?? a
export const IDENTITY_LABEL: Record<string, string> = lazy({ bot: () => t("api:identity.bot"), self: () => t("api:identity.self"), app: () => t("api:identity.app"), account: () => t("api:identity.account") })
export const identityLabel = (i: string) => IDENTITY_LABEL[i] ?? i
/** an adapter of the integrations list in the shape the generic connect dialog takes */
export const asAdapter = (a: IAdapter, system: string): RAdapter => ({ id: a.id, system, name: a.name, auth: a.auth, caps: a.capabilities.flatMap((c) => c.caps), device: a.device, fields: a.fields })
/** the channel-capable connections' provider, by adapter id (the 渠道 view's badges and filters) */
export function providerByAdapter(x: Integrations): Record<string, { id: string; name: string }> {
  const out: Record<string, { id: string; name: string }> = {}
  for (const p of x.providers) for (const a of p.adapters) out[a.id] = { id: p.id, name: p.name }
  return out
}

/** a connection listed under `p` though its adapter is not one of `p`'s: a channel service that names this platform
 * (hello `platform`), on its own, beside and independent of the provider's bot */
export const viaService = (c: { adapter: string }, p: { adapters: { id: string }[] }) => !p.adapters.some((a) => a.id === c.adapter)

/** the installations of a provider */
export const installationsOf = (x: Integrations, provider: string): IInstallation[] => x.installations.filter((i) => i.provider === provider)
/** the open-platform page of app `appId` with `scopes` preselected; `tenant` for the bot's scopes (the form Feishu puts into 99991672 answers) */
export const feishuGrantUrl = (domain: string, appId: string, scopes: string[], tenant: boolean) =>
  `${domain === "lark" ? "https://open.larksuite.com" : "https://open.feishu.cn"}/app/${encodeURIComponent(appId)}/auth?q=${scopes.join(",")}&op_from=openapi${tenant ? "&token_type=tenant" : ""}`
