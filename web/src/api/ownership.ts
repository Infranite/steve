/* 归属与负责人 (steve-rs-plan/modules/ownership.md §4). Shapes mirror steve-modules/src/contacts/stewards.rs;
 * the TS Hub has none of these routes (404 → NotWired), so callers hide their parts when these fail. */
import { modReq as req, qs } from "@/api"

export type ObjectType = "account" | "connection" | "device" | "bot" | "persona" | "party" | "space"
export type StewardRole = "owner" | "maintainer" | "user"
export type ObjectInfo = { object_type: ObjectType; object_id: string; type_label: string; name: string; kind: string | null; detail: string; party_id: string | null }
export type Steward = { person_id: string; person_name: string; is_me: boolean; role: StewardRole; role_label: string; source: string; status: "confirmed" | "proposed"; confidence: number; decision_id: string | null; updated: number }
export type ObjectStewards = { object: ObjectInfo; stewards: Steward[] }
/** a write's answer: `undo` is a changeset for `POST /api/config/:id/undo` (null when nothing changed) */
export type Changed = ObjectStewards & { ok: true; undo: string | null }
export type AccountRef = { id: string; handle: string; name: string; kind: string; kind_label: string }
export type Holding = { object: ObjectInfo; role: StewardRole; role_label: string; source: string; status: "confirmed" | "proposed"; decision_id: string | null; others: Steward[] }
export type Holdings = { person_id: string; person_name: string; is_bot: boolean; accounts: { system_id: string; system: string; accounts: AccountRef[] }[]; responsible: Holding[]; stewards: Steward[] }

const e = encodeURIComponent
const path = (t: ObjectType, id: string) => `/stewards/${e(t)}/${e(id)}`

export const ownership = {
  holdings: (person: string) => req<Holdings>("GET", `/people/${e(person)}/holdings`),
  unowned: () => req<{ objects: ObjectInfo[] }>("GET", "/stewards/unowned").then((r) => r.objects),
  of: (t: ObjectType, id: string) => req<ObjectStewards>("GET", path(t, id)),
  /** sets the 负责人 (replacing the others of the role unless `replace: false`) */
  set: (t: ObjectType, id: string, person_id: string, o: { role?: StewardRole; replace?: boolean } = {}) => req<Changed>("PUT", path(t, id), { person_id, ...o }),
  remove: (t: ObjectType, id: string, o: { person_id?: string; role?: StewardRole } = {}) => req<Changed>("DELETE", path(t, id) + qs(o)),
  undo: (changeset: string) => req<{ ok: true }>("POST", `/config/${e(changeset)}/undo`),
}

/** where an object's own page is */
export function objectHref(o: ObjectInfo): string | null {
  switch (o.object_type) {
    case "bot": return "#/bots/" + o.object_id
    case "device": return "#/devices/" + o.object_id
    case "persona": return "#/personas/" + o.object_id
    case "party": return "#/contacts/" + o.object_id
    case "account": return "#/accounts/" + o.object_id
    case "space": return "#/auto/space/" + o.object_id
    default: return null
  }
}
