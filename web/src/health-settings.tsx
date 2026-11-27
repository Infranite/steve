import { useEffect, useState } from "react"
import { ActivityIcon } from "lucide-react"
import { HubError } from "@/api"
import { systemApi } from "@/api/system"
import { Badge } from "@/components/ui/badge"
import { Row, Panel, Section, Notice, Chevron, Tile, go } from "@/ui"
import { useT } from "@/i18n"
import { checkCounts, overallStatus, sortedWarnings, warningText, type HubHealth } from "@/health"

/** Settings · 关于 · 运行状况摘要: concise instance-health status linking to Devices · Monitor (#/devices/monitor).
 *  Owner-only (the Hub answers 403 to anyone else, and the section is hidden). */
export function HubHealthSection() {
  const [h, setH] = useState<HubHealth | null>(null), [error, setError] = useState(""), t = useT()
  useEffect(() => {
    let live = true
    const load = () => systemApi.health("15m").then((v) => { if (live) { setH(v); setError("") } })
      .catch((e) => { if (!live) return; if (e instanceof HubError && e.status === 403) { setH(null); setError("") } else setError((e as Error).message) })
    void load()
    const timer = setInterval(load, 60_000)
    return () => { live = false; clearInterval(timer) }
  }, [])
  if (!h) return error ? <Section label={t("shell:health.title")}><Notice role="alert" tone="destructive">{error}</Notice></Section> : null
  const past = sortedWarnings(h)
  const status = overallStatus(h)
  const counts = checkCounts(h)
  const minutes = Math.max(1, Math.round(h.window_seconds / 60))
  const badgeTone = status === "critical" ? "danger" : status === "warning" ? "warning" : "success"
  const title = status === "critical"
    ? t("shell:health.summary.criticalTitle", { count: past.length })
    : status === "warning"
      ? t("shell:health.summary.warnTitle", { count: past.length })
      : t("shell:health.summary.okTitle")
  const badgeLabel = status === "critical"
    ? t("shell:health.summary.criticalBadge")
    : status === "warning"
      ? t("shell:health.summary.warnBadge")
      : t("shell:health.summary.okBadge")
  const sub = past.length === 0
    ? t("shell:health.summary.okSub", { minutes, measured: counts.measured })
    : past.length === 1
      ? t("shell:health.summary.warnSingle", { detail: warningText(past[0]) })
      : t("shell:health.summary.warnMore", { detail: warningText(past[0]), count: past.length })
  return (
    <Section data-hub-health label={t("shell:health.title")}>
      {error && <Notice role="alert" tone="destructive" className="mb-3">{error}</Notice>}
      <Panel>
        <Row
          icon={<Tile I={ActivityIcon} />}
          title={<span className="inline-flex flex-wrap items-center gap-2"><span>{title}</span><Badge tone={badgeTone} size="sm" dot data-health-summary-badge={status}>{badgeLabel}</Badge></span>}
          sub={sub}
          onClick={() => go("#/devices/monitor")}
          data-health-summary-row
        >
          <span className="mr-1 type-footnote text-muted-foreground">{t("shell:health.summary.openMonitor")}</span>
          <Chevron />
        </Row>
      </Panel>
    </Section>
  )
}
