/* Model settings, Steve primary model setup, and Hub harness model catalog. */
import { modReq, type ModuleScope } from "@/api"
import type { EffortMapping, EffortMode, ReasoningEffort } from "@/gen"

export type ModelChoice = { harness: string; models: string[]; effort?: ReasoningEffort | null }
export type ModelUsedRow = { harness: string; model: string; calls: number; failed: number; at: number }
export type ModelRecent = { last: { harness: string; model: string; at: number } | null; used: ModelUsedRow[] }
export type ModelPurpose = {
  role?: "primary" | "lightweight"
  source?: "primary" | "lightweight" | "chat" | "override"
  key: string
  label: string
  desc: string
  value: ModelChoice | null
  follows: boolean
  effective: ModelChoice
  recommended: ModelChoice | null
  recent: ModelRecent | null
}
export type ModelSteve = {
  bot_id: string | null
  name: string
  harness: string
  model: string | null
  bot_effort: ReasoningEffort | null
  runs_on: string
  models: string[]
  effort: ReasoningEffort | null
  note: string | null
  model_note: string | null
}
export type ModelHarness = { id: string; name: string; efforts: { key: ReasoningEffort; label: string }[]; effort_mode?: EffortMode; effort_map?: EffortMapping[]; default_models: string[] }
export type ModelAgent = { harness: string; name: string; models: string[]; default: string | null; value: string | null }
export type ModelsSettingsView = {
  lightweight?: ModelChoice | null
  lightweight_effective?: ModelChoice
  steve: ModelSteve
  purposes: ModelPurpose[]
  agents: ModelAgent[]
  harnesses: ModelHarness[]
  max: number
}

export type ModelSetupView = {
  steve: { harness: string; model: string | null; models: string[] }
  lightweight?: ModelChoice | null
  lightweight_effective?: ModelChoice
  bot_revision: number
  harnesses: { id: string; name: string; ready: boolean | null; judgments: boolean }[]
  purposes: { key: string; label: string; role?: "primary" | "lightweight"; follows: boolean; effective: { harness: string; models: string[] } }[]
}

export type Used = { calls: number; ok_at: number | null; failed_at: number | null }
export type CatalogModel = { id: string; label: string; family: string; via: string | null; note: string | null; efforts: string[]; effort_map?: EffortMapping[]; used?: Used }
export type Catalog = { harness: string; name: string; source: string; default: string | null; efforts?: string[]; effort_mode?: EffortMode; effort_map?: EffortMapping[]; families: string[]; models: CatalogModel[] }

export const modelsApi = {
  get: () => modReq<ModelsSettingsView>("GET", "/settings/models"),
  patch: (b: { purposes?: Record<string, ModelChoice | null>; agents?: Record<string, string | null> }) =>
    modReq<ModelsSettingsView>("PATCH", "/settings/models", b),
  catalog: (harness: string, fresh = false) =>
    modReq<Catalog>("GET", `/system/models?harness=${encodeURIComponent(harness)}${fresh ? "&fresh=1" : ""}`),
  refreshCatalog: (scope?: ModuleScope) =>
    modReq("GET", "/system/models?fresh=1", undefined, undefined, scope),
}

export const modelSetupApi = {
  get: () => modReq<ModelSetupView>("GET", "/settings/models"),
  select: (choice: { harness: string; model: string | null; expected_revision: number; follow_purposes: boolean; lightweight?: ModelChoice | null }) =>
    modReq<ModelSetupView>("POST", "/settings/models/select", choice),
}
