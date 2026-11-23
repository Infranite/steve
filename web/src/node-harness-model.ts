import type { RNodeHarness, RNodeHarnessReport } from "./api.ts"
import type { NodeView } from "./device-model.ts"
import { t } from "./i18n/index.ts"

export function harnessState(h: RNodeHarness): "ready" | "authentication_required" | "unknown" | "not_installed" | "installing" | "unavailable" {
  if (h.degraded_reason === "authentication_required") return "authentication_required"
  if (h.available) return "ready"
  if (h.available == null) return "unknown"
  return h.setup?.state === "not_installed" ? "not_installed" : h.setup?.state === "installing" ? "installing" : "unavailable"
}

/** The target Node's own evidence. Offline and failures never look like a confirmed empty list. */
export function harnessSummary(v: Pick<NodeView, "linked">, r?: RNodeHarnessReport | null): string {
  if (!v.linked || r?.state === "offline") return t(r?.checked_at ? "devices:harness.offlineCached" : "devices:harness.offline")
  if (!r || r.state === "unreported") return t("devices:harness.unreported")
  if (r.state === "unsupported") return t("devices:harness.unsupported")
  if (r.state === "failed") return t(r.checked_at ? "devices:harness.failedCached" : "devices:harness.failed")
  const ready = r.harnesses.filter(h => h.available)
  const text = ready.length ? t("devices:harness.available", { names: ready.map(h => [h.name, h.version].filter(Boolean).join(" ")).join(" · ") })
    : t(r.harnesses.some(h => h.available == null) ? "devices:harness.unprobed" : "devices:harness.none")
  return r.stale ? `${t("devices:harness.stale")} · ${text}` : text
}
