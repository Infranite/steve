/* Accounts and connections (accounts-vault.md §5): the Rust Hub's /api/systems, /api/accounts, /api/match-rules and
 * /api/connections. The TS Hub doesn't have them; `NotWired` tells a page to fall back to its Unwired placeholder. */
import { modReq, qs as q } from "@/api"
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
export type RConnection = { id: string; account_id: string; adapter: string; auth: "oauth" | "credential" | "none"; status: ConnStatus; caps: string[]; available_caps: string[]; health: { last_ok: number; last_error: string; error_at: number }; last_sync: number | null }
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

/* wording the Hub leaves to the web */
export const CAP_LABEL: Record<string, string> = {
  "msg.receive": "收消息", "msg.send": "发消息", "msg.send_as_user": "以你的名义发消息", "mail.read": "读邮件", "mail.draft": "起草邮件", "mail.send": "发邮件",
  "cal.read": "读日程", "cal.write": "改日程", "directory.read": "读通讯录", "repo.read": "读代码仓库", "repo.write": "改代码仓库", events: "接收事件",
}
export const capLabel = (c: string) => CAP_LABEL[c] ?? c
export const KIND_LABEL: Record<string, string> = {
  personal: "个人", work_email: "工作邮箱", app_bot: "应用机器人", shared_mailbox: "共享邮箱", bank_card: "银行卡", service: "服务账号", phone: "手机号", bot: "机器人", calendar: "日历", root: "根账号", other: "其他",
}
export const kindLabel = (k: string) => KIND_LABEL[k] ?? k
export const CONN_STATUS: Record<ConnStatus, [string, string]> = { ok: ["正常", "done"], reauth: ["要重新授权", "wait"], error: ["出错了", "fail"], paused: ["已暂停", "pause"], revoked: ["已断开", "drop"] }
