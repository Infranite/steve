import type { Said } from "../gen/Said.ts"
import { t } from "./index.ts"

export function decisionText(said: Said): string {
  if (said.kind === "raw") return said.text
  const prompt = said.template
  switch (prompt.code) {
    case "turn_repeated_failure": return t("views:decision.turnRepeatedFailure")
    case "turn_no_progress": return t("views:decision.turnNoProgress")
    case "merge_pull_request": return t("views:decision.mergePullRequest", { number: prompt.number, repo: prompt.repo })
    case "no_executor": return t("views:decision.noExecutor", { count: prompt.items.length })
  }
}
