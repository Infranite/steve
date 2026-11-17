/* Personal reply reading uses a server snapshot, never a timestamp or the viewport's local new-message counter. */
import { modReq, qs } from "@/api"
import { currentHub } from "@/hub"

export type AttentionKind = "replies" | "pending" | "feed" | "channels"
export type AttentionEntry = {
  id: string; event_id: string; created: number; order?: number
  target: { route: string; entity_id: string }; source_id?: string; group_key?: string; notify: boolean
}
export type AttentionCategory = {
  kind: AttentionKind; count: number; unit: "messages" | "cards" | "entries" | "conversations"
  available: boolean; revision: string; unchanged: boolean; entries: AttentionEntry[]
  next_cursor?: string | null; snapshot?: string | null
}
export type AttentionPage = {
  v: number; epoch: string; user_id: string; workspace_id: string; history_policy: string
  categories: AttentionCategory[]
}
export const attentionApi = {
  forWorkspace(workspace: string) {
    const scope = { workspace, hub: currentHub() }
    const req = <T>(method: string, path: string, body: unknown, signal: AbortSignal) =>
      modReq<T>(method, path, body, AbortSignal.any([signal, AbortSignal.timeout(30_000)]), scope)
    return {
      replies: (cursor: string | undefined, signal: AbortSignal) => req<AttentionPage>("GET", "/me/attention" + qs({ kind: "replies", limit: 100, cursor }), undefined, signal),
      reply: (id: string, signal: AbortSignal) => req<{ id: string; created: number }>("GET", `/me/attention/replies/${encodeURIComponent(id)}`, undefined, signal),
      read: (snapshot: string, through_id: string, signal: AbortSignal) => req<unknown>("POST", "/me/attention/read", { snapshot, through_id }, signal),
    }
  },
}
export type AttentionClient = ReturnType<typeof attentionApi.forWorkspace>
