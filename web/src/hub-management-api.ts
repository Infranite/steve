import { modReq } from "./api"
import { hello, hubBase, setSharedHubName } from "./hub"

import { parseHubInfo, normalizeHubDraft, UnsupportedHubMetadata, type HubInfo, type HubDraft } from "./hub-management-model.ts"
export * from "./hub-management-model.ts"

export async function readHubInfo(): Promise<HubInfo> {
  // Support is explicitly declared, independently of software version and API generation.
  // A failed metadata read on a declaring Hub is a read failure, never an inferred legacy service.
  const h = await hello(hubBase())
  if (h.capabilities?.hub_metadata !== 1) throw new UnsupportedHubMetadata()
  const raw = await modReq<unknown>("GET", "/hub-metadata", undefined, AbortSignal.timeout(8000))
  const info = parseHubInfo(raw)
  setSharedHubName(info.metadata.display_name, info.metadata)
  return info
}
export async function writeHubInfo(revision: number, draft: HubDraft): Promise<HubInfo> {
  const info = parseHubInfo(await modReq<unknown>("PUT", "/hub-metadata", { revision, ...normalizeHubDraft(draft) }, AbortSignal.timeout(8000)))
  setSharedHubName(info.metadata.display_name, info.metadata)
  return info
}
