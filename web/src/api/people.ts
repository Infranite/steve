/** Contacts, organizations and teams. */
import { modReq as req, Unauthenticated, HubError, qs, wsHeaders } from "@/api"
import { avatarSrc } from "@/actor"
import { hubUrl } from "@/hub"
import { t } from "@/i18n"
import { notePersonIdentity } from "@/store"
import type { AvatarRef } from "@/gen"

export { avatarSrc }
export type { AvatarRef }
export type Level = "public" | "conclusion" | "reasoning" | "never"
export type Visibility = string // workspace | private | team:<id> | space:<id>
export type ContactKind = "real" | "virtual" | "unconfirmed" | "bot"
export type OrgRef = { id: string; name: string; title: string | null; dept: string | null }
type Head = { id: string; name: string; aliases: string[]; kind: ContactKind | null; rels: string[]; important: boolean; org: OrgRef | null; tags: string[]; visibility: Visibility; is_me: boolean; avatar: AvatarRef | null | unknown; updated: number }
export type ContactBrief = Head & { accounts: number; items: number }
export type Job = { org_id: string; org_name: string; title: string | null; dept: string | null; from: string | null; to: string | null; source: string }
export type Attr = { id: string; key: string; label: string; value: string; source_label: string; source?: string; level: Level; status: "confirmed" | "candidate" | "stale" | string }
export type TeamRef = { id: string; name: string; kind: "chart" | "virtual"; role: "lead" | "member" }
export type ProfileField = { state: "known" | "missing"; manual?: boolean; reason?: "pending" | "external" | "permission" | "not_visible" | "not_provided" | "no_source"; scope?: string }
export type ContactProfile = { checked: number | null; next_check: number | null; source: string | null; fields: Record<"name" | "org" | "title" | "email", ProfileField> }
export type Contact = Head & {
  profile?: ContactProfile
  note: string; status: "active" | "merged" | "deleted"; merged_into: string | null; source: string; revision: number; created: number
  jobs: Job[]; attrs: Attr[]; teams: TeamRef[]; reports_to: { id: string; name: string }[]; merges: { id: string; from_name: string; created: number; splittable: boolean }[]
}
export type Org = { name_state?: "manual" | "provider" | "placeholder" | "workspace"; name_reason?: "external_unavailable" | "permission" | null; name_scope?: string | null; tenant?: { key: string; own: boolean } | null; id: string; name: string; aliases: string[]; kind: string | null; domains: string[]; note: string; tags: string[]; attrs: Attr[]; people?: { id: string; name: string; title: string | null; avatar?: unknown }[]; revision: number }
export type TeamMember = { contact_id: string; name: string; role: "lead" | "member"; external: boolean; source: string; avatar?: unknown }
export type Team = {
  id: string; name: string; kind: "chart" | "virtual"; status: "active" | "dissolved"; parent_id: string | null; org_id: string | null; purpose: string; space_id: string | null
  tags: string[]; visibility: Visibility; source_label: string; headcount: number; members?: TeamMember[]; children: string[]; depth?: number; revision: number
}
export type TeamList = { chart: Team[]; virtual: Team[]; sync: { connection_id: string; label: string; at: number } | null; directory?: DirectoryStatus[] }
export type Mem = { id: string; kind: string; text: string; source_label: string; level: Level; status: "confirmed" | "candidate" | "stale" | string; visibility: Visibility; about: { type: string; id: string }[]; created: number }
export type PartyItem = { item_id: string; title?: string; role: string; source: string; confidence: number }
export type TimelineEntry = { ts: number; kind: "mail" | "message" | "meeting" | "item" | "event"; text: string; href: string }
export type ItemPerson = { party_id: string; name: string; type: "contact" | "org" | "team"; role: string; source: string; confidence: number; avatar?: unknown }
export type PartyOption = { id: string; name: string; type: "contact" | "org" | "team"; sub: string; avatar?: unknown }
export type MentionOptions = { bots: { id: string; name: string }[]; parties: PartyOption[] }
export type Mention = { kind: "party" | "bot"; id: string }
export type MergeProposal = { id: string; from: ContactBrief; into: ContactBrief; account: { id: string; handle: string; system: string } | null; reason: string; decision_id: string | null }
/** where an unconfirmed contact was seen in the channels (the Hub's `SpacePort::seen`) */
export type SeenSpace = { id: string; name: string; kind: string; attention: "realtime" | "quiet" | "blocked" | string }
export type Seen = { spaces: SeenSpace[]; last: { text: string; created: number; space_id: string; space_name: string } | null; messages: number; last_at: number | null; quiet: boolean }
/** an unconfirmed contact of 需要你判断: `label` is never a raw id (“未知飞书用户” while there is no name) */
export type PendingContact = ContactBrief & { label?: string; nameless?: boolean; account?: { system: string; system_name: string; handle: string } | null; seen?: Seen | null }
export type Pending = { proposals: MergeProposal[]; unconfirmed: PendingContact[]; quiet?: number }
/** a directory connection's last sync, for the 组织架构 header */
export type DirectoryStatus = { connection_id: string; label: string; at: number; ok: boolean; error?: string | null; scope?: { whole: boolean; departments: number; users: number } | null; departments: number; users: number; missing_scopes: string[]; note: string | null }
/** an account row of the accounts module (only what the contact page shows) */
export type AccountBrief = { id: string; system: { id: string; name: string; kind: string }; kind: string; handle: string; display_name: string; verified: boolean; source_label: string }

const get = <T>(p: string) => req<T>("GET", p), post = <T = { ok: true }>(p: string, b: unknown = {}) => req<T>("POST", p, b)
const patch = <T = { ok: true }>(p: string, b: unknown) => req<T>("PATCH", p, b), del = <T = { ok: true }>(p: string) => req<T>("DELETE", p)
const e = encodeURIComponent

export const people = {
  contacts: (f: { q?: string; rel?: string; kind?: string; org?: string; tag?: string; limit?: number } = {}) => get<ContactBrief[]>("/contacts" + qs(f)),
  contact: (id: string) => get<Contact>("/contacts/" + e(id)).then(noteMine),
  me: () => get<Contact>("/me/contact").then(noteMine),
  newContact: (b: { name: string; aliases?: string[]; rels?: string[]; org_id?: string; title?: string; kind?: "real" | "virtual"; note?: string; visibility?: Visibility }) => post<Contact>("/contacts", b),
  patchContact: (id: string, b: Partial<{ name: string; aliases: string[]; rels: string[]; important: boolean; note: string; kind: "real" | "virtual"; visibility: Visibility; tags: string[] }>) => patch<Contact>("/contacts/" + e(id), b).then(noteMine),
  confirm: (id: string) => post<Contact>(`/contacts/${e(id)}/confirm`),
  deleteContact: (id: string) => del("/contacts/" + e(id)),
  addJob: (id: string, b: { org_id: string; title?: string; dept?: string; from?: string }) => post<Contact>(`/contacts/${e(id)}/jobs`, b),
  patchJob: (id: string, org: string, b: Partial<{ title: string | null; dept: string | null; from: string | null; to: string | null }>) => patch<Contact>(`/contacts/${e(id)}/jobs/${e(org)}`, b),
  deleteJob: (id: string, org: string) => del(`/contacts/${e(id)}/jobs/${e(org)}`),
  addAttr: (party: string, b: { key: string; label?: string; value: string; level?: Level }) => post<Contact | Org>(`/parties/${e(party)}/attrs`, b),
  patchAttr: (id: string, b: Partial<{ value: string; level: Level; status: "confirmed" | "stale" }>) => patch(`/attrs/${e(id)}`, b),
  deleteAttr: (id: string) => del(`/attrs/${e(id)}`),
  attrDefs: () => get<{ id: string; key: string; label: string; applies: string; type: string; default_level: Level }[]>("/attr-defs"),
  orgs: () => get<Org[]>("/orgs"),
  org: (id: string) => get<Org>("/orgs/" + e(id)),
  patchOrg: (id: string, b: Partial<{ name: string; kind: string; domains: string[]; aliases: string[]; note: string }>) => patch<Org>("/orgs/" + e(id), b),
  newOrg: (b: { name: string; kind?: string; domains?: string[]; aliases?: string[]; note?: string }) => post<Org>("/orgs", b),
  teams: () => get<TeamList>("/teams"),
  team: (id: string) => get<Team>("/teams/" + e(id)),
  newTeam: (b: { name: string; purpose?: string; members: { contact_id: string; role?: string }[]; visibility?: Visibility }) => post<Team>("/teams", b),
  patchTeam: (id: string, b: Partial<{ name: string; purpose: string; space_id: string | null; tags: string[]; visibility: Visibility }>) => patch<Team>("/teams/" + e(id), b),
  putMembers: (id: string, ms: { contact_id: string; role: string }[]) => req<Team>("PUT", `/teams/${e(id)}/members`, ms),
  dissolve: (id: string) => del("/teams/" + e(id)),
  syncTeams: () => post<{ job_id: string }>("/teams/sync"),
  items: (party: string) => get<PartyItem[]>(`/parties/${e(party)}/items`),
  timeline: (party: string, limit = 30) => get<TimelineEntry[]>(`/parties/${e(party)}/timeline?limit=${limit}`),
  memories: (party: string) => get<Mem[]>(`/parties/${e(party)}/memories`),
  addMemory: (party: string, b: { text: string; kind?: string; level?: Level }) => post<Mem>(`/parties/${e(party)}/memories`, b),
  patchMem: (id: string, b: Partial<{ text: string; level: Level; status: "confirmed" | "stale"; visibility: Visibility }>) => patch<Mem>(`/mems/${e(id)}`, b),
  deleteMem: (id: string) => del(`/mems/${e(id)}`),
  pending: () => get<Pending>("/merges"),
  merge: (from: string, into: string) => post<{ merge_id: string; into: Contact }>(`/contacts/${e(from)}/merge`, { into }),
  split: (merge: string) => post(`/merges/${e(merge)}/split`),
  itemPeople: (item: string) => get<ItemPerson[]>(`/items/${e(item)}/people`),
  addItemPerson: (item: string, party_id: string, role = "related") => post<ItemPerson[]>(`/items/${e(item)}/people`, { party_id, role }),
  removeItemPerson: (item: string, party: string, role?: string) => del(`/items/${e(item)}/people/${e(party)}${qs({ role })}`),
  mentionOptions: (q: string) => get<MentionOptions>("/mention-options" + qs({ q })),
  /** ⌘K: contacts (我 included), organizations and teams, at most 10 each; an empty `q` finds nothing */
  search: (q: string) => get<{ contacts: PartyOption[]; orgs: PartyOption[]; teams: PartyOption[] }>("/parties/search" + qs({ q })),
  /** a contact's own picture (PNG, JPEG or WebP, at most 10 MB): the Hub cuts the centre square and stores 256px */
  setAvatar: async (id: string, f: Blob): Promise<Contact> => {
    const r = await fetch(hubUrl(`/api/contacts/${e(id)}/avatar`), { method: "PUT", body: f, headers: wsHeaders({ "content-type": "application/octet-stream" }), credentials: "include" })
    const j: any = await r.json().catch(() => undefined)
    if (r.status === 401) throw new Unauthenticated(j?.login ?? null, j?.token === true)
    if (!r.ok) throw new HubError(r.status, j?.error ?? t("people:avatar.failed"))
    if (!j) throw new Error(t("shell:http.incomplete"))
    return noteMine(j as Contact)
  },
  clearAvatar: (id: string) => del<Contact>(`/contacts/${e(id)}/avatar`).then(noteMine),
  accounts: (party: string) => get<AccountBrief[]>("/accounts" + qs({ party })),
  addAccount: (b: { system_id: string; party_id: string; kind: string; handle: string }) => post<AccountBrief>("/accounts", b),
}

/** Updates unified person identity in the store when a contact read or edit returns a person. */
function noteMine<C extends { id?: string; name?: string; is_me: boolean; avatar: unknown }>(c: C): C {
  notePersonIdentity(c)
  return c
}

/** a provider id that identification fell back to as a name (`ou_…`, `on_…`, `cli_…`) — never shown as one */
export const looksLikeHandle = (n: string) => !n.trim() || /^(ou|on|cli|oc|u|uid)_[A-Za-z0-9]{6,}$/.test(n.trim())
/** what to call a contact: its name, or “未知用户” while the name is still a raw id */
export const nameOf = (c: { name: string; label?: string }) => c.label ?? (looksLikeHandle(c.name) ? t("api:person.unknown") : c.name)
