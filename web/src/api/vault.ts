/* The vault (accounts-vault.md §4, §5): status and keys, items with masked fields, grants, the use log, passkeys and the
 * reveal ceremony. No response here carries a secret value except `revealFinish`, whose value the caller only shows. */
import { mget, mpost, mpatch, mput, mdel, lazy } from "@/api/accounts"
import { qs as q } from "@/api"
import { t } from "@/i18n"

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
  rotate: () => mpost<{ kek_version: number; recovery_code: string }>("/vault/rotate"),
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

export const SENS_LABEL: Record<Sensitivity, string> = lazy({ normal: () => t("api:sens.normal"), sensitive: () => t("api:sens.sensitive"), high: () => t("api:sens.high") })
export const MODE_LABEL: Record<string, string> = lazy({
  env: () => t("api:mode.env"), file: () => t("api:mode.file"), stdin: () => t("api:mode.stdin"), internal: () => t("api:mode.internal"),
  view: () => t("api:mode.view"), copy: () => t("api:mode.copy"), browser_fill: () => t("api:mode.browserFill"),
})
export const USE_STATUS: Record<string, string> = lazy({
  pending: () => t("api:useStatus.pending"), denied: () => t("api:useStatus.denied"), running: () => t("api:useStatus.running"),
  ok: () => t("api:useStatus.ok"), failed: () => t("api:useStatus.failed"), expired: () => t("api:useStatus.expired"),
})
/** the 400 when this device has no passkey (§4.2), verbatim from the Hub */
export const NEED_PASSKEY = "先在这台设备上登记 Touch ID 或通行密钥"

/** the store holding the master key, as a place: "Mac mini 的钥匙串", "Linux 密钥环", "密钥目录 /srv/steve-keys" */
export function keyStoreWhere(s: RVaultStatus): string {
  const k = s.keystore
  if (!k || k.kind === "keychain") return t("api:keystore.keychain")
  if (k.kind === "secret-service") return t("api:keystore.secretService")
  return k.label
}
/** the master key's state in its store, worded for that store; the macOS texts are the Hub's own 503 messages */
export function keyStoreState(s: RVaultStatus): string {
  const kind = s.keystore?.kind ?? "keychain", where = keyStoreWhere(s)
  switch (s.keychain) {
    case "ok": return t("api:keystore.ok")
    case "missing": return kind === "keychain" ? t("api:keystore.keychainMissing") : t("api:keystore.missing", { where })
    case "locked": return kind === "keychain" ? t("api:keystore.keychainLocked") : kind === "secret-service" ? t("api:keystore.secretServiceLocked") : t("api:keystore.locked", { where })
    case "denied": return kind === "keychain" ? t("api:keystore.keychainDenied") : kind === "secret-service" ? t("api:keystore.secretServiceDenied") : t("api:keystore.denied", { where })
    default: return s.keychain
  }
}
