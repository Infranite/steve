import type { Said } from "../gen/Said.ts"
import { t } from "./index.ts"

export function decisionText(said: Said): string {
  if (said.kind === "raw") return said.text
  const prompt = said.template
  switch (prompt.code) {
    case "turn_repeated_failure": return t("views:decision.turnRepeatedFailure")
    case "turn_no_progress": return t("views:decision.turnNoProgress")
    case "merge_pull_request": return t("views:decision.mergePullRequest", { number: prompt.number, repo: prompt.repo })
    case "workflow_script_fault": return t("views:decision.workflowScriptFault", { faultCode: prompt.fault_code })
    case "workflow_non_deterministic_replay": return t("views:decision.workflowNonDeterministicReplay")
    case "workflow_budget_exhausted": return t("views:decision.workflowBudgetExhausted", { scope: prompt.scope, used: prompt.used, limit: prompt.limit })
    case "workflow_placement_blocked": return t("views:decision.workflowPlacementBlocked", { callTitle: prompt.call_title, reason: prompt.reason })
    case "workflow_review_blocked": return t("views:decision.workflowReviewBlocked", { reason: prompt.reason })
    case "workflow_single_session_override": return t("views:decision.workflowSingleSessionOverride")
  }
}
