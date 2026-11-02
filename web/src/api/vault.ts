/* The vault (accounts-vault.md §4, §5): status and keys, items with masked fields, grants, the use log, passkeys and the
 * reveal ceremony. No response here carries a secret value except `revealFinish`, whose value the caller only shows. */
import { mget, mpost, mpatch, mput, mdel } from "@/api/accounts"
import { qs as q } from "@/api"

export type Sensitivity = "normal" | "sensitive" | "high"
/** where the master keys live (accounts-vault.md §3.3); a Hub that doesn't say uses the macOS keychain */
export type RKeyStore = { kind: "keychain" | "secret-service" | "dir" | "file"; label: string }
export type RVaultStatus = { initialized: boolean; keychain: "ok" | "locked" | "missing" | "denied"; keystore?: RKeyStore; kek_version: number | null; passkeys: number; rotating?: boolean }
export type RVault = { id: string; name: string; scope: "personal" | "shared" | "system"; backend: string; items: number }
export type RTemplate = { id: string; name: string; fields: { label: string; type: string; secret: boolean }[]; default_sensitivity: Sensitivity; source: string }
/** a secret field's `value` is always null; card numbers carry `•••• 4821` in `hint` */
export type RField = { id: string; label: string; type: string; secret: boolean; hint: string; value: string | null }
export type RGrant = { id: string; bot_id: string; scope: { project_id?: string; site?: string; scratch?: boolean }; scope_label: string; until: number }
export type RItem = {
  id: string; vault: { id: string; name: string; scope: string }; template: string; title: string; site: string; account: { id: string; display_name: string } | null
  sensitivity: Sensitivity; backend: string; fields?: RField[]; grants: RGrant[]; created: number; updated: number
}
export type RUse = { id: string; item_id: string; item_title: string; actor: { type: string; id: string; name: string }; purpose: string; mode: string; status: string; work_item_id: string | null; exit_code?: number | null; created: number; finished?: number | null }
export type RPasskey = { id: string; name: string; rp_id: string; created: number; last_used: number | null }

const e = encodeURIComponent
const fld = (id: string, fid: string) => `/vault/items/${e(id)}/fields/${e(fid)}`

export const vaultApi = {
  status: () => mget<RVaultStatus>("/vault/status"),
  /** the recovery code comes back once (cache-control: no-store); the caller shows it and drops it */
  init: () => mpost<{ recovery_code: string }>("/vault/init"),
  recover: (code: string) => mpost<{ ok: true; kek_version: number }>("/vault/recover", { code }),
  rotate: () => mpost<{ job_id: string; kek_version: number; recovery_code: string }>("/vault/rotate"),
  vaults: () => mget<RVault[]>("/vaults"),
  templates: () => mget<RTemplate[]>("/vault/templates"),
  items: (p: { vault?: string; template?: string; q?: string; account?: string } = {}) => mget<RItem[]>("/vault/items" + q(p)),
  item: (id: string) => mget<RItem>("/vault/items/" + e(id)),
  newItem: (b: { vault_id: string; template: string; title: string; site?: string; account_id?: string; sensitivity?: Sensitivity; fields: { label: string; type: string; secret: boolean; value: string }[] }) => mpost<RItem>("/vault/items", b),
  patchItem: (id: string, b: { title?: string; site?: string; account_id?: string | null; sensitivity?: Sensitivity }) => mpatch<RItem>("/vault/items/" + e(id), b),
  deleteItem: (id: string) => mdel<{ ok: true; revoked_grants: number }>("/vault/items/" + e(id)),
  /** replaces the whole value (re-encrypted); there is no read-then-edit */
  putField: (id: string, fid: string, value: string) => mput<RItem>(fld(id, fid), { value }),
  revealStart: (id: string, fid: string) => mpost<any>(fld(id, fid) + "/reveal/start"),
  revealFinish: (id: string, fid: string, assertion: unknown, copy: boolean) => mpost<{ value: string; ttl: number }>(fld(id, fid) + "/reveal/finish", { assertion, copy }),
  grant: (id: string, b: { bot_id: string; scope: RGrant["scope"]; hours: 1 | 8 | 24 }) => mpost<RGrant>(`/vault/items/${e(id)}/grants`, b),
  revokeGrant: (id: string) => mdel<{ ok: true }>("/grants/" + e(id)),
  uses: (p: { item?: string; bot?: string; limit?: number } = {}) => mget<RUse[]>("/vault/uses" + q(p)),
  passkeys: () => mget<RPasskey[]>("/passkeys"),
  registerStart: (name: string) => mpost<any>("/passkeys/register/start", { name }),
  registerFinish: (credential: unknown) => mpost<RPasskey>("/passkeys/register/finish", { credential }),
  deletePasskey: (id: string) => mdel<{ ok: true }>("/passkeys/" + e(id)),
}

export const SENS_LABEL: Record<Sensitivity, string> = { normal: "普通", sensitive: "敏感", high: "高密" }
export const MODE_LABEL: Record<string, string> = { env: "环境变量注入", file: "临时文件注入", stdin: "标准输入注入", internal: "连接内部使用", view: "本机查看", copy: "本机复制", browser_fill: "浏览器代填" }
export const USE_STATUS: Record<string, string> = { pending: "等你批准", denied: "没有批准", running: "执行中", ok: "完成", failed: "失败", expired: "已过期" }
/** the 400 when this device has no passkey (§4.2), verbatim from the Hub */
export const NEED_PASSKEY = "先在这台设备上登记 Touch ID 或通行密钥"

/** the store holding the master key, as a place: "Mac mini 的钥匙串", "Linux 密钥环", "密钥目录 /srv/steve-keys" */
export function keyStoreWhere(s: RVaultStatus): string {
  const k = s.keystore
  if (!k || k.kind === "keychain") return "Mac mini 的钥匙串"
  if (k.kind === "secret-service") return "Linux 密钥环"
  return k.label
}
/** the master key's state in its store, worded for that store; the macOS texts are the Hub's own 503 messages */
export function keyStoreState(s: RVaultStatus): string {
  const kind = s.keystore?.kind ?? "keychain", where = keyStoreWhere(s)
  switch (s.keychain) {
    case "ok": return "正常"
    case "missing": return kind === "keychain" ? "钥匙串里没有主密钥，请用恢复码恢复" : `${where}里没有主密钥，请用恢复码恢复`
    case "locked": return kind === "keychain" ? "Mac mini 的钥匙串被锁了（Mac mini 重启后还没有登录）" : kind === "secret-service" ? "Linux 密钥环被锁了（登录桌面解锁后再试）" : `${where}被锁了`
    case "denied": return kind === "keychain" ? "钥匙串没有授权这个版本的 Steve 读取主密钥（发布时没有用固定签名）" : kind === "secret-service" ? "Linux 密钥环拒绝了 Steve（授权提示被关掉或没人响应）" : `${where}拒绝了 Steve 读取主密钥`
    default: return s.keychain
  }
}
