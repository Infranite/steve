import { useMutation, useQuery } from "@tanstack/react-query"
import { approvalApi } from "@/api/approval"
import { Switch } from "@/components/affect/switch-clean"
import { qc, scopeKey } from "@/data/sync"
import type { ApprovalPolicy, ApprovalMode } from "@/gen"
import { useT } from "@/i18n"
import { toast, useHubEvents } from "@/store"

export function ApprovalSwitch() {
  const t = useT()
  const key = ["detail", scopeKey(), "approval-policy"]
  const policy = useQuery({
    queryKey: key,
    queryFn: ({ signal }) => approvalApi.getPolicy(signal),
  }, qc)
  useHubEvents(["approval_policy"], () => void policy.refetch())
  const update = useMutation({
    mutationFn: ({ mode, revision }: Pick<ApprovalPolicy, "mode" | "revision">) =>
      approvalApi.setPolicy(mode, revision),
    onSuccess: (value) => {
      qc.setQueryData(key, value)
      void qc.invalidateQueries({ queryKey: key })
    },
    onError: (error) => { toast(error.message); void policy.refetch() },
  }, qc)
  if (!policy.data) return null
  const data = policy.data
  const toggle = (on: boolean) => {
    const mode: ApprovalMode = on ? "harness_auto" : "ask_owner"
    update.mutate({ mode, revision: data.policy.revision })
  }
  return <Switch data-approval-switch placement="end" label={t("core:approval.title")}
    description={t("core:approval.description")} checked={data.policy.mode === "harness_auto"}
    disabled={!data.editable || update.isPending} onChange={(e) => toggle(e.currentTarget.checked)} />
}

export function SessionApproval({ id }: { id: string }) {
  const t = useT()
  const state = useQuery({
    queryKey: ["detail", scopeKey(), "approval-policy", id],
    queryFn: ({ signal }) => approvalApi.sessionMode(id, signal),
  }, qc)
  useHubEvents(["approval_policy", "patch", "session"], () => void state.refetch(), 300, {
    filter: (e) => e.type !== "patch" || e.ops.some((op) => op.type === "session" && op.id === id),
  })
  const d = state.data
  if (!d) return null
  const pending = d.desired ?? (d.applied ? d.target : null)
  const applied = (d.unknown || d.released) ? null : d.applied
  const modeLabel = applied === "auto" ? d.native_id ?? applied : applied
  return <span data-session-approval={d.status} title={d.refused ?? undefined}>
    {t(`core:approval.${d.status}`)}{" · "}
    {t("core:approval.applied", { mode: modeLabel ?? t("core:approval.unreported") })}
    {!d.refused && pending && pending !== applied && <>{" · "}{t("core:approval.pending", { mode: pending === "auto" ? d.native_id ?? pending : pending })}</>}
    {d.refused && <>{" · "}{t("core:approval.refused")}</>}
  </span>
}
