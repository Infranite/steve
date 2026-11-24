/* Navigation usage frequency on the Stats page: reads `GET /api/stats/nav?range=7d|30d|all` (backed by
 * durable daily aggregates in `telemetry_daily`, fed by the `web-perf` telemetry pipeline) so sidebar grouping
 * and ordering can be tuned against observed usage across Hub restarts. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { stats, type NavUsage, type NavUsageRange } from "@/api/stats"
import { Panel, PanelFoot } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { useT } from "@/i18n"
import { SIDEBAR_NAV, navMetaOf, type NavGroup } from "@/nav"
import { flushNavOpens } from "@/perf"
import { fmtN, fmtPct, fmtRange } from "@/stats/format"
import { Section, Table, th } from "@/stats/tables"
import { Segmented } from "@/ui"

type State = { s: "loading" } | { s: "ok"; v: NavUsage } | { s: "error"; msg: string }

function groupLabel(t: ReturnType<typeof useT>, group: NavGroup | "account"): string {
  if (group === "primary") return t("stats:v2.nav.groupPrimary")
  if (group === "more") return t("stats:v2.nav.groupFooter")
  if (group === "account") return t("stats:v2.nav.groupAccount")
  return t(`core:navGroup.${group}`)
}

export function NavUsageSection() {
  const t = useT()
  const [range, setRange] = useState<NavUsageRange>("7d")
  const [state, setState] = useState<State>({ s: "loading" })

  useEffect(() => {
    let live = true
    setState({ s: "loading" })
    void flushNavOpens()
      .then(() => stats.nav(range))
      .then(
        (v) => { if (live) setState({ s: "ok", v }) },
        (e) => {
          if (!live) return
          setState({ s: "error", msg: (e as Error).message || t("stats:v2.nav.loadFailed") })
        },
      )
    return () => { live = false }
  }, [range, t])

  const opened = new Set(state.s === "ok" ? state.v.items.map((x) => x.item) : [])
  const unused = state.s === "ok" && state.v.total > 0
    ? SIDEBAR_NAV.filter((x) => !opened.has(x.l)).map((x) => t(`core:nav.${x.l}`))
    : []
  const max = state.s === "ok" ? Math.max(1, ...state.v.items.map((x) => x.opens)) : 1
  const right = (
    <div className="flex flex-wrap items-center gap-2.5" data-nav-usage-controls>
      {state.s === "ok" && state.v.total > 0 && (
        <span className="type-footnote tabular-nums text-tertiary" data-nav-usage-summary>
          {state.v.from
            ? t("stats:v2.nav.summaryRange", { range: fmtRange(state.v.from, state.v.to), count: state.v.total })
            : t("stats:v2.nav.summaryTotal", { count: state.v.total })}
        </span>
      )}
      <Segmented
        size="sm"
        value={range}
        onChange={setRange}
        options={[
          ["7d", t("stats:v2.nav.range.d7")],
          ["30d", t("stats:v2.nav.range.d30")],
          ["all", t("stats:v2.nav.range.all")],
        ]}
      />
    </div>
  )

  return (
    <Section k="nav" title={t("stats:v2.nav.section")} sub={t("stats:v2.nav.sectionSub")} right={right}>
      <Panel k="nav" data-nav-usage={state.s === "ok" ? state.v.range : ""}>
        {state.s === "loading" ? (
          <div className="flex h-28 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        ) : state.s === "error" ? (
          <div className="px-5 py-6 type-callout text-muted-foreground" role="alert">{state.msg}</div>
        ) : !state.v.items.length ? (
          <div className="px-5 py-6 type-callout text-muted-foreground" data-nav-usage-empty>{t("stats:v2.nav.empty")}</div>
        ) : (
          <>
            <div className="pt-1 pb-2">
              <Table label={t("stats:v2.nav.section")} min={420} head={
                <tr>
                  <th className={cn(th, "pr-3")}>{t("stats:v2.nav.item")}</th>
                  <th className={cn(th, "px-3")}>{t("stats:v2.nav.group")}</th>
                  <th className={cn(th, "pl-3 text-right")}>{t("stats:v2.nav.opens")}</th>
                  <th className={cn(th, "w-44 pl-4 text-right")}>{t("stats:v2.nav.share")}</th>
                </tr>
              }>
                {state.v.items.map((row) => {
                  const meta = navMetaOf(row.item)
                  const label = t(`core:nav.${row.item}` as any, { defaultValue: row.item })
                  const share = state.v.total > 0 ? row.opens / state.v.total : 0
                  const widthPct = Math.max(4, Math.round((row.opens / max) * 100))
                  return (
                    <tr key={row.item} data-nav-usage-row={row.item}>
                      <td className="py-2.5 pr-3">
                        {meta ? (
                          <a href={meta.href} className="font-medium text-foreground underline-offset-2 hover:text-primary hover:underline">{label}</a>
                        ) : (
                          <span className="font-medium text-foreground">{label}</span>
                        )}
                      </td>
                      <td className="whitespace-nowrap px-3 type-footnote text-muted-foreground">
                        {meta ? groupLabel(t, meta.group) : "—"}
                      </td>
                      <td className="whitespace-nowrap py-2.5 pl-3 text-right font-medium tabular-nums" data-nav-usage-opens>
                        {fmtN(row.opens)}
                      </td>
                      <td className="whitespace-nowrap py-2.5 pl-4 text-right tabular-nums">
                        <span className="inline-flex w-full items-center justify-end gap-2.5">
                          <span aria-hidden className="h-1.5 w-24 overflow-hidden rounded-full bg-muted">
                            <span className="block h-full rounded-full bg-primary/80" style={{ width: `${widthPct}%` }} />
                          </span>
                          <span className="w-10 text-right type-footnote text-muted-foreground">{fmtPct(share)}</span>
                        </span>
                      </td>
                    </tr>
                  )
                })}
              </Table>
            </div>
            {unused.length > 0 && (
              <PanelFoot meta={t("stats:v2.nav.unused", { items: unused.join(t("stats:word.listSep")) })} />
            )}
          </>
        )}
      </Panel>
    </Section>
  )
}
