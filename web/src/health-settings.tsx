import { useEffect, useState } from "react"
import { HubError } from "@/api"
import { systemApi } from "@/api/system"
import { Row, Panel, Section, Notice } from "@/ui"
import { useT } from "@/i18n"
import { grouped, label, limitText, valueText, warnings, warningText, type HubHealth } from "@/health"

/** Settings · 关于 · 运行状况: how the Hub, its Nodes and plugins, and this web client have been doing over the last
 * window, each figure beside its limit, and what is past a limit said first. The instance owner's (the Hub answers
 * 403 to anyone else, and the section is not shown). */
export function HubHealthSection() {
  const [h, setH] = useState<HubHealth | null>(null), [error, setError] = useState(""), t = useT()
  useEffect(() => {
    let live = true
    const load = () => systemApi.health().then((v) => { if (live) { setH(v); setError("") } })
      .catch((e) => { if (!live) return; if (e instanceof HubError && e.status === 403) { setH(null); setError("") } else setError((e as Error).message) })
    void load()
    const timer = setInterval(load, 60_000)
    return () => { live = false; clearInterval(timer) }
  }, [])
  if (!h) return error ? <Section label={t("shell:health.title")}><Notice role="alert" tone="destructive">{error}</Notice></Section> : null
  const past = warnings(h)
  return <Section data-hub-health label={t("shell:health.title")} note={t("shell:health.note", { minutes: Math.max(1, Math.round(h.window_seconds / 60)) })}>
    {error && <Notice role="alert" tone="destructive">{error}</Notice>}
    {past.length > 0 && <Notice tone="attention" className="mb-3" data-health-warnings>
      <ul className="flex flex-col gap-1">{past.map((c) => <li key={c.key + (c.subject ?? "")} data-health-warning={c.key}>{warningText(c)}</li>)}</ul>
    </Notice>}
    <div className="flex flex-col gap-4">{grouped(h).map((g) => <Panel key={g.title} data-health-group={g.title}>
      {g.checks.map((c) => {
        const bad = past.some((w) => w.key === c.key && w.subject === c.subject)
        return <Row key={c.key + (c.subject ?? "")} title={label(c)} sub={limitText(c) ?? undefined} data-health-check={c.key}>
          <span className={bad ? "type-callout font-medium text-attention" : "type-callout text-muted-foreground"}>{valueText(c)}</span>
        </Row>
      })}
    </Panel>)}</div>
  </Section>
}
