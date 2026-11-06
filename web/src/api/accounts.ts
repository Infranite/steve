/* Accounts and connections (accounts-vault.md §5): the Rust Hub's /api/systems, /api/accounts, /api/match-rules and
 * /api/connections. The TS Hub doesn't have them; `NotWired` tells a page to fall back to its Unwired placeholder. */
import { modReq, qs as q } from "@/api"
import { t } from "@/i18n"
export const mget = <T>(p: string) => modReq<T>("GET", p)
export const mpost = <T = any>(p: string, b: unknown = {}) => modReq<T>("POST", p, b)
export const mpatch = <T = any>(p: string, b: unknown) => modReq<T>("PATCH", p, b)
export const mput = <T = any>(p: string, b: unknown) => modReq<T>("PUT", p, b)
export const mdel = <T = any>(p: string) => modReq<T>("DELETE", p)
const e = encodeURIComponent

export type RSystem = { id: string; name: string; kind: string; family: string; plugin: string | null; handle_rule: any; builtin: boolean }
export type RConnBrief = { id: string; status: ConnStatus; caps: string[]; last_sync: number | null }
export type RAccount = {
  id: string; system: { id: string; name: string; kind: string }; party: { id: string; name: string; type: string; is_me: boolean }
  kind: string; handle: string; display_name: string; verified: boolean; confidence: number; source: string; source_label: string
  connection: RConnBrief | null; vault_items: { id: string; title: string }[]; revision: number
}
export type ConnStatus = "ok" | "reauth" | "error" | "paused" | "revoked"
export type RConnection = { id: string; account_id: string; adapter: string; auth: "oauth" | "credential" | "none"; status: ConnStatus; caps: string[]; available_caps: string[]; health: { last_ok: number; last_error: string; error_at: number }; last_sync: number | null
  /** the Hub has its adapter (absent on older Hubs: installed) */
  installed?: boolean }
export type RFieldDef = { key: string; label: string; secret: boolean; required: boolean; type: "text" | "password" | "number" | "select"; default?: string; hint?: string }
export type RAdapter = { id: string; system: string; name: string; auth: "oauth" | "credential" | "managed" | "none"; caps: string[]; device: boolean; fields?: RFieldDef[] }
export type RMatchRule = { id: string; family: string; kind: string; config: { same_org_only: boolean; exclude: string[]; min_len: number }; enabled: boolean; updated: number }
export type RUsage = { flows: { id: string; name: string }[]; spaces: number; sources: { id: string; name: string }[] }
export type RDevice = { id: string; user_code: string; verification_uri: string; verification_uri_complete: string | null; expires_in: number; interval: number }
export type RDeviceStatus = { status: "pending" } | { status: "cancelled" } | { status: "ok"; account_id: string } | { status: "error"; error: string }

export const accountsApi = {
  systems: () => mget<RSystem[]>("/systems"),
  accounts: (p: { scope?: "mine" | "company" | "others"; party?: string; system?: string } = {}) => mget<RAccount[]>("/accounts" + q(p)),
  account: (id: string) => mget<RAccount>("/accounts/" + e(id)),
  newAccount: (b: { system_id: string; party_id?: string; kind: string; handle: string; display_name?: string }) => mpost<RAccount>("/accounts", b),
  patchAccount: (id: string, b: { display_name?: string; kind?: string; party_id?: string; verified?: true }) => mpatch<RAccount>("/accounts/" + e(id), b),
  deleteAccount: (id: string) => mdel<{ ok: true }>("/accounts/" + e(id)),
  rules: () => mget<RMatchRule[]>("/match-rules"),
  patchRule: (id: string, b: { enabled?: boolean; config?: Partial<RMatchRule["config"]> }) => mpatch<RMatchRule>("/match-rules/" + e(id), b),
  connections: () => mget<RConnection[]>("/connections"),
  connection: (id: string) => mget<RConnection>("/connections/" + e(id)),
  adapters: () => mget<RAdapter[]>("/connections/adapters"),
  oauthStart: (adapter: string, caps?: string[]) => mpost<{ url: string }>("/connections/oauth/start", { adapter, caps }),
  deviceStart: (adapter: string, caps?: string[]) => mpost<RDevice>("/connections/device/start", { adapter, caps }),
  deviceStatus: (id: string) => mget<RDeviceStatus>("/connections/device/" + e(id)),
  /** credential connections: the form's values go straight into the body, never anywhere else */
  connect: (adapter: string, fields: Record<string, string>) => mpost<RConnection>("/connections", { adapter, fields }),
  patchConnection: (id: string, b: { paused?: boolean; caps?: string[] }) => mpatch<RConnection>("/connections/" + e(id), b),
  reauth: (id: string) => mpost<{ url: string } | { fields: string[] }>(`/connections/${e(id)}/reauth`),
  putCredential: (id: string, fields: Record<string, string>) => mput<RConnection>(`/connections/${e(id)}/credential`, { fields }),
  test: (id: string) => mpost<RConnection>(`/connections/${e(id)}/test`),
  usage: (id: string) => mget<RUsage>(`/connections/${e(id)}/usage`),
  disconnect: (id: string) => mdel<{ ok: true; usage: RUsage }>("/connections/" + e(id)),
}

/** a table (object or array) whose entries are read through getters, so a label is translated each time it is read
 *  and a language switch shows on the next render; keys, Object.keys/entries, find and map work as on a plain table */
export function lazy<T extends object>(src: { [K in keyof T]: () => T[K] }): T {
  const out = (Array.isArray(src) ? [] : {}) as T
  for (const k of Object.keys(src)) Object.defineProperty(out, k, { get: (src as Record<string, () => unknown>)[k], enumerable: true })
  return out
}

/* wording the Hub leaves to the web */
export const CAP_LABEL: Record<string, string> = lazy({
  "msg.receive": () => t("api:cap.msgReceive"), "msg.send": () => t("api:cap.msgSend"), "msg.send_as_user": () => t("api:cap.msgSendAsUser"), "mail.read": () => t("api:cap.mailRead"), "mail.draft": () => t("api:cap.mailDraft"), "mail.send": () => t("api:cap.mailSend"),
  "cal.read": () => t("api:cap.calRead"), "cal.write": () => t("api:cap.calWrite"), "directory.read": () => t("api:cap.directoryRead"), "repo.read": () => t("api:cap.repoRead"), "repo.write": () => t("api:cap.repoWrite"), events: () => t("api:cap.events"),
})
export const capLabel = (c: string) => CAP_LABEL[c] ?? c
export const KIND_LABEL: Record<string, string> = lazy({
  personal: () => t("api:accountKind.personal"), work_email: () => t("api:accountKind.workEmail"), app_bot: () => t("api:accountKind.appBot"), shared_mailbox: () => t("api:accountKind.sharedMailbox"), bank_card: () => t("api:accountKind.bankCard"), service: () => t("api:accountKind.service"),
  phone: () => t("api:accountKind.phone"), bot: () => t("api:accountKind.bot"), calendar: () => t("api:accountKind.calendar"), root: () => t("api:accountKind.root"), other: () => t("api:accountKind.other"),
})
export const kindLabel = (k: string) => KIND_LABEL[k] ?? k
export const CONN_STATUS: Record<ConnStatus, [string, string]> = lazy({
  ok: () => [t("api:connStatus.ok"), "done"], reauth: () => [t("api:connStatus.reauth"), "wait"], error: () => [t("api:connStatus.error"), "fail"], paused: () => [t("api:connStatus.paused"), "pause"], revoked: () => [t("api:connStatus.revoked"), "drop"],
})
/** a connection's status label and dot; 适配器未安装 when the Hub doesn't have its adapter */
export const connStatusOf = (c: { status: ConnStatus | string; installed?: boolean }): [string, string] =>
  c.installed === false ? [t("api:connStatus.notInstalled"), "fail"] : CONN_STATUS[c.status as ConnStatus] ?? [c.status, "idle"]
