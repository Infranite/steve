/* Bot teams, per-Bot execution node bindings, and Bot creation proposal confirmation. */
import { modReq, type ModuleScope } from "@/api"
import type { PlacementBlocked, PlacementRejection } from "@/gen"

export type TeamMember = { bot: string; nodes: string[] | null }
export type BotTeam = { id: string; name: string; members: TeamMember[] }
export type TeamList = { default: string | null; teams: BotTeam[]; editable: boolean }
export type CandidateNode = { id: string; name: string | null; state: "candidate" | "pending" | "unavailable"; reason: PlacementRejection | null; checked_at: number | null }
export type NodeList = { editable: boolean; bindings: string[]; candidates: CandidateNode[]; unavailable_bindings: number }
export type ItemDispatch = {
  team: { id: string; name: string; source: "default" | "explicit" | "inherited" } | null
  blocked?: PlacementBlocked | null
}

export const botDispatchApi = {
  teams: (scope?: ModuleScope, signal?: AbortSignal) => modReq<TeamList>("GET", "/bot-teams", undefined, signal, scope),
  saveTeam: (teamId: string | undefined, body: { name: string; members: TeamMember[] }, scope?: ModuleScope) =>
    modReq<BotTeam>(teamId ? "PUT" : "POST", teamId ? `/bot-teams/${encodeURIComponent(teamId)}` : "/bot-teams", body, undefined, scope),
  setDefaultTeam: (team: string, scope?: ModuleScope) => modReq("PUT", "/bot-teams/default", { team }, undefined, scope),
  executionNodes: (bot: string, scope?: ModuleScope, signal?: AbortSignal) =>
    modReq<NodeList>("GET", `/bots/${encodeURIComponent(bot)}/execution-nodes`, undefined, signal, scope),
  setExecutionNodes: (bot: string, nodes: string[], scope?: ModuleScope) =>
    modReq("PUT", `/bots/${encodeURIComponent(bot)}/execution-nodes`, { nodes }, undefined, scope),
  confirmProposal: (
    proposalId: string,
    body: { expected_revision: number; name: string; role: string; specialties: string[]; harness: string; model: string | null },
    scope?: ModuleScope,
  ) => modReq<{ id: string }>("POST", `/bot-proposals/${encodeURIComponent(proposalId)}/confirm`, body, undefined, scope),
}
