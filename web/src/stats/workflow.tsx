/* 工作流（#242）：工作图怎么跑、交付和回复花多久，改工作流时拿来对比的基线。数字全取自 GET /stats/summary 的 `workflow`
 * （和 `quality.reworks`）。三行指标：返工（验收返工和单元重试并排）、主控、交付与回复；下面是收尾作废和交付被拒的原因构成
 * （affectUI Compact Breakdown）。旧 Hub 没有这一节就不显示。 */
import { Panel, PanelSection } from "@/panel"
import type { P, Summary, WorkflowStats } from "@/api/stats"
import { CompactBreakdown } from "@/components/affect/compact-breakdown"
import { Kpi, kpiRow } from "@/stats/kpi"
import { useT } from "@/i18n"
import { fmtMs, fmtN, fmtPct } from "@/stats/format"

/** the median of a distribution, its p85 as the foot when there is one */
const median = (p: P) => (p.p50 == null ? "—" : fmtMs(p.p50))
const sum = (m: Record<string, number>) => Object.values(m).reduce((a, n) => a + n, 0)

/** anything to show: a graph, a delivery, a reply or a session */
export const hasWorkflow = (w?: WorkflowStats) =>
  !!w && (w.graphs > 0 || w.delivered > 0 || w.rejected > 0 || sum(w.invalidations) > 0 || w.reply_latency.n > 0 || w.first_tool.n > 0 || w.no_tool > 0)

export function WorkflowSection({ s }: { s: Summary }) {
  const t = useT()
  const w = s.workflow
  if (!w) return null
  if (!hasWorkflow(w)) return <Panel k="workflow" data-workflow-empty><div className="px-5 py-6 type-callout text-muted-foreground">{t("stats:v2.workflow.empty")}</div></Panel>
  const review = w.first_try.review
  const p85 = (p: P) => (p.p85 == null ? undefined : t("stats:v2.workflow.p85", { v: fmtMs(p.p85) }))
  const invalid = sum(w.invalidations)
  const parts = (m: Record<string, number>, group: "void" | "refuse") =>
    Object.entries(m).filter(([, n]) => n > 0).map(([k, n]) => ({ id: k, label: t(`stats:v2.workflow.${group}.${k}`, { defaultValue: k }), value: n }))
  const times = (n: number) => t("stats:v2.workflow.times", { count: Math.round(n) })
  return (
    <Panel k="workflow" data-workflow>
      <PanelSection k="figures" flush>
        <div className={kpiRow(4)} data-workflow-row="rework">
          <Kpi k="rework" label={t("stats:v2.workflow.rework")} value={s.quality.reworks} tone={s.quality.reworks ? "wait" : undefined} def={t("stats:v2.workflow.reworkDef")} />
          <Kpi k="unit_retries" label={t("stats:v2.workflow.unitRetries")} value={s.quality.unit_retries ?? w.retries} tone={w.retries ? "wait" : undefined}
            foot={w.attempts ? `${fmtPct(w.retry_rate)} · ${t("stats:v2.workflow.unitRetriesFoot", { count: w.attempts })}` : undefined} def={t("stats:v2.workflow.unitRetriesDef")} />
          <Kpi k="attempts_per_unit" label={t("stats:v2.workflow.attempts")} value={w.attempts_per_unit.avg == null ? "—" : w.attempts_per_unit.avg.toFixed(1)}
            foot={w.units ? t("stats:v2.workflow.attemptsFoot", { count: w.units }) : undefined} def={t("stats:v2.workflow.attemptsDef")} />
          <Kpi k="first_review" label={t("stats:v2.workflow.firstReview")} value={fmtPct(review?.rate)}
            foot={review?.delivered ? t("stats:v2.workflow.firstReviewFoot", { count: review.delivered }) : undefined} def={t("stats:v2.workflow.firstReviewDef")} />
        </div>
        <div className={kpiRow(4)} data-workflow-row="coordinator">
          <Kpi k="coord_share" label={t("stats:v2.workflow.coordShare")} value={fmtPct(w.coordinator_share)}
            foot={w.coordinator_tokens + w.unit_tokens > 0 ? t("stats:v2.workflow.coordShareFoot", { a: fmtN(w.coordinator_tokens), b: fmtN(w.unit_tokens) }) : undefined} def={t("stats:v2.workflow.coordShareDef")} />
          <Kpi k="coord_turn" label={t("stats:v2.workflow.coordTurn")} value={median(w.coordinator_turn_ms)} tone={w.coordinator_long_turns ? "wait" : undefined}
            foot={w.coordinator_turn_ms.n ? t("stats:v2.workflow.coordTurnFoot", { count: w.coordinator_long_turns, limit: fmtMs(w.long_turn_ms) }) : undefined} def={t("stats:v2.workflow.coordTurnDef")} />
          <Kpi k="sessions_per_item" label={t("stats:v2.workflow.sessions")} value={w.sessions_per_item.p50 ?? "—"}
            foot={w.sessions_per_item.n ? t("stats:v2.workflow.sessionsFoot", { count: w.sessions_per_item.n }) : undefined} def={t("stats:v2.workflow.sessionsDef")} />
          <Kpi k="cold_start" label={t("stats:v2.workflow.coldStart")} value={median(w.first_tool)}
            foot={w.no_tool ? t("stats:v2.workflow.coldStartFoot", { count: w.no_tool }) : p85(w.first_tool)} def={t("stats:v2.workflow.coldStartDef")} />
        </div>
        <div className={kpiRow(4)} data-workflow-row="delivery">
          <Kpi k="reject" label={t("stats:v2.workflow.reject")} value={fmtPct(w.reject_rate)} tone={w.rejected ? "wait" : undefined}
            foot={w.rejected + w.delivered > 0 ? t("stats:v2.workflow.rejectFoot", { n: w.rejected, total: w.rejected + w.delivered }) : undefined} def={t("stats:v2.workflow.rejectDef")} />
          <Kpi k="invalid" label={t("stats:v2.workflow.invalid")} value={invalid} tone={invalid ? "wait" : undefined} def={t("stats:v2.workflow.invalidDef")} />
          <Kpi k="to_merge" label={t("stats:v2.workflow.toMerge")} value={median(w.accept_to_merge)}
            foot={w.merged ? t("stats:v2.workflow.toMergeFoot", { count: w.merged }) : undefined} def={t("stats:v2.workflow.toMergeDef")} />
          <Kpi k="reply" label={t("stats:v2.workflow.reply")} value={median(w.reply_latency)}
            foot={w.reply_latency.n ? t("stats:v2.workflow.replyFoot", { count: w.reply_latency.n }) : undefined} def={t("stats:v2.workflow.replyDef")} />
        </div>
      </PanelSection>
      {(invalid > 0 || w.rejected > 0) && (
        <PanelSection k="reasons">
          <div className="grid gap-4 @3xl/stats:grid-cols-2">
            {invalid > 0 && <CompactBreakdown bare items={parts(w.invalidations, "void")} title={t("stats:v2.workflow.invalidations")} totalLabel={t("stats:v2.workflow.invalid")} format={times} rowProps={(x) => ({ "data-workflow-void": x.key })} />}
            {w.rejected > 0 && <CompactBreakdown bare items={parts(w.rejects, "refuse")} title={t("stats:v2.workflow.rejects")} totalLabel={t("stats:v2.workflow.reject")} format={times} rowProps={(x) => ({ "data-workflow-refuse": x.key })} />}
          </div>
        </PanelSection>
      )}
    </Panel>
  )
}
