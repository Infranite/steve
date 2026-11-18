import { useState, type ReactNode } from "react"
import { CpuIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { IntegrationSettings, IntegrationSection } from "@/components/affect/integration-settings-clean"
import { HarnessMark } from "@/components/harness-mark"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import type { NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard } from "@/devices-ui"
import { harnessState, harnessSummary } from "@/node-harness-model"

/** All data and refreshes belong to v.id. Local maintenance remains explicitly local. */
export function NodeHarnesses({ v, children }: { v: NodeView; children?: ReactNode }) {
  const t = useT(), { harnesses, harnessesError, refreshHarnesses } = useDevices()
  const r = harnesses?.[v.id], [busy, setBusy] = useState(false), [error, setError] = useState("")
  const stale = !v.linked || !!r?.stale || !!harnessesError || r?.state !== "reported"
  const refresh = async () => {
    setBusy(true); setError("")
    try { await refreshHarnesses(v.id) } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  return <DCard icon={<CpuIcon />} title={t("devices:harness.title")} sub={harnessSummary(v, r)} data-node-harnesses={v.id}
    action={<Button size="sm" variant="ghost" disabled={busy || !v.linked || v.legacy || v.state === "pending" || v.state === "revoked"} onClick={() => void refresh()} data-node-harness-refresh>{busy || r?.refreshing ? <Spinner /> : <RefreshCwIcon />}{t("devices:harness.refresh")}</Button>}>
    <div className="flex min-w-0 flex-col gap-3 px-3 pb-3" aria-live="polite">
      {r?.checked_at && <p className="px-2 type-footnote text-muted-foreground" data-node-harness-checked>{t("devices:harness.checked", { at: date(r.checked_at, { dateStyle: "short", timeStyle: "medium" }) })}</p>}
      {(error || harnessesError) && <p role="alert" className="px-2 type-footnote text-attention" data-node-harness-error>{t("devices:harness.readFailed")}</p>}
      {(error || harnessesError || r?.error) && <details className="px-2 type-footnote text-muted-foreground" data-node-harness-error-details><summary className="cursor-pointer">{t("devices:harness.details")}</summary><p className="mt-1 break-words whitespace-pre-wrap">{error || harnessesError || r?.error}</p></details>}
      {r?.harnesses.map(h => {
        const state = harnessState(h), details = [h.setup?.reason, h.setup?.error, h.error, h.setup?.help].filter(Boolean)
        return <IntegrationSettings key={h.id} name={h.name} logo={<HarnessMark harness={h.id} className="size-5" />} data-node-harness-id={h.id}
          badge={<Badge tone={stale || state === "unknown" ? "neutral" : h.available ? "success" : "warning"} data-node-harness-state={stale ? "stale" : state}>{t(`devices:harness.state.${state}`)}{stale && ` · ${t("devices:harness.lastReport")}`}</Badge>}
          meta={<span className="flex flex-wrap gap-x-3 gap-y-1" data-node-harness-versions><span>CLI {h.version || t("devices:harness.versionUnknown")}</span><span>ACP {h.adapter_version || t("devices:harness.versionUnknown")}</span></span>}>
          <IntegrationSection label={t(stale || !h.available ? "devices:harness.reportedModels" : "devices:harness.models")}>
            <p className="break-words type-footnote text-muted-foreground" data-node-harness-models>{h.models.length ? h.models.join(" · ") : t("devices:harness.noModels")}</p>
          </IntegrationSection>
          {(details.length > 0 || !!h.setup?.components.length) && <IntegrationSection><details className="type-footnote text-muted-foreground" data-node-harness-details><summary className="cursor-pointer">{t("devices:harness.details")}</summary>
            <div className="mt-2 flex flex-col gap-2 break-words whitespace-pre-wrap">{[...new Set(details)].map(d => <p key={d}>{d}</p>)}
              {h.setup?.components.map(c => <div key={c.id} data-node-harness-component={c.id}><span>{c.name} · {c.version || t("devices:harness.versionUnknown")} · {t(c.status === "installed" || (!c.status && c.installed) ? "devices:harness.componentInstalled" : c.status === "unknown" ? "devices:harness.state.unknown" : "devices:harness.state.not_installed")}</span>{c.path && <p className="break-all font-mono">{c.path}</p>}</div>)}
            </div>
          </details></IntegrationSection>}
        </IntegrationSettings>
      })}
      {children && <div className="mt-1" data-node-harness-manage><p className="mb-3 px-2 type-footnote text-muted-foreground">{t("devices:harness.manageLocal")}</p>{children}</div>}
    </div>
  </DCard>
}
