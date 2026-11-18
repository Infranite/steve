/* Remote policy is a reported fact, independent of the account's Hub capability switch. */
import { IntegrationRow } from "@/components/affect/integration-settings-clean"
import { useT } from "@/i18n"
import { tm } from "@/store"

export function hasExternalWritePolicy(detail?: Record<string, unknown>): boolean {
  return !!detail && "external_write_policy" in detail
}

export function ServiceWritePolicy({ detail }: { detail?: Record<string, unknown> }) {
  const t = useT()
  if (!hasExternalWritePolicy(detail)) return null
  const reported = detail?.external_write_policy as { policy?: unknown; observed_at?: unknown } | null
  const policy = reported?.policy
  const state = policy === "allowed" || policy === "restricted" || policy === "forbidden" ? policy : "unknown"
  return <IntegrationRow title={t("accounts:writePolicy.service")} hint={<span className="flex flex-col gap-1">
    <span>{t(`accounts:writePolicy.${state}Note`)}</span>
    {typeof reported?.observed_at === "number" && <span>{t("accounts:writePolicy.observedAt", { time: tm(reported.observed_at) })}</span>}
  </span>} data-service-write-policy={state}>
    <span className={"type-footnote " + (state === "forbidden" ? "text-attention" : "text-muted-foreground")}>{t(`accounts:writePolicy.${state}`)}</span>
  </IntegrationRow>
}
