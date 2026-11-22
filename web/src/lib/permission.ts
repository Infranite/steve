import type { DecisionView } from "../gen/DecisionView.ts"
import { t } from "../i18n/index.ts"

/** Missing old decisions are historical records, never inferred approvals or pending requests. */
export function permissionStatus(d?: DecisionView) {
  if (!d) return { label: t("misc:permission.status.history"), pending: false }
  if (d.status === "open") return { label: t("misc:permission.status.open"), pending: true }
  if (d.status === "expired") return { label: t("misc:permission.status.expired"), pending: false }
  const r = (d.result ?? {}) as { dismissed?: boolean; optionId?: string }
  if (r.dismissed) return { label: t("misc:permission.status.dismissed"), pending: false }
  const option = d.options.find((o) => o.split("|")[0] === r.optionId)
  const rejected = option?.split("|")[2]?.startsWith("reject")
  return { label: r.optionId && !rejected ? t("misc:permission.status.allowed") : t("misc:permission.status.rejected"), pending: false }
}
