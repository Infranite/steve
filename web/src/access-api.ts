/* Access to the Hub (A4): access tokens, the signed-in clients with the route each last came by, client pairing and
 * single sign-on. A token's text is returned once, when it is made. The instance owner may list (`all`) and sign out
 * every member's clients; anyone else sees only their own. */
import { modReq } from "@/api"

export type AccessToken = { id: string; name: string; created: number; expires: number | null; last_used: number | null; revoked: number | null }
/** `token_id`: the access token it signed in with; `online`: it holds the live stream now (`streams` of them, since `connected_since`) */
export type AccessClient = { id: string; user_id?: string; token_id?: string | null; kind: string; name: string; platform: string; version: string; origin: string | null; created: number; last_seen: number | null; last_route: string | null; last_ip: string | null; revoked: number | null; online?: boolean; streams?: number; connected_since?: number | null }
export type AccessView = {
  tokens: AccessToken[]
  clients: AccessClient[]
  current: string | null
  route: string | null
  hub: { version: string }
  /** with `all`: the caller and whether every member's clients are listed, and the members' names by id */
  me?: string
  all?: boolean
  users?: Record<string, string>
}
export type Oidc = { source: "env" | "settings" | "none"; issuer: string | null; client_id: string | null; has_secret: boolean; callback: string; enabled: boolean }
export type ClientPairing = {
  id: string
  code: string
  name: string
  expires: number
  public_url: string | null
  lan_urls: string[]
  loopback_only: boolean
  /** one pairing link per address of the Hub, the public one first */
  links: { url: string; kind: "public" | "lan"; link: string }[]
}
export type PairingStatus = {
  id: string
  status: "pending" | "claimed" | "expired"
  expires: number
  client: { id: string; name: string; kind: string; platform: string } | null
}

export const access = {
  get: (all = false) => modReq<AccessView>("GET", "/access" + (all ? "?all=1" : "")),
  create: (name: string) => modReq<{ token: string; item: AccessToken }>("POST", "/access/tokens", { name }),
  revokeToken: (id: string) => modReq("DELETE", `/access/tokens/${encodeURIComponent(id)}`),
  revokeClient: (id: string) => modReq("DELETE", `/access/clients/${encodeURIComponent(id)}`),
  createPairing: () => modReq<ClientPairing>("POST", "/access/pairings", {}),
  pairingStatus: (id: string) => modReq<PairingStatus>("GET", `/access/pairings/${encodeURIComponent(id)}`),
  oidc: () => modReq<Oidc>("GET", "/access/oidc"),
  saveOidc: (b: { issuer: string; client_id: string; secret?: string }) => modReq<Oidc>("PUT", "/access/oidc", b),
  dropOidc: () => modReq<Oidc>("DELETE", "/access/oidc"),
}
