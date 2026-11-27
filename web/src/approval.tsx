import { useState } from "react"
import { useMutation, useQuery } from "@tanstack/react-query"
import { HandIcon, ShieldAlertIcon, ShieldOffIcon, SparklesIcon } from "lucide-react"
import { approvalApi } from "@/api/approval"
import { AutonomyChoice } from "@/components/affect/agent-permissions"
import { AlertDialog } from "@/components/affect/dialog-clean"
import { HarnessMark } from "@/components/harness-mark"
import { qc, scopeKey } from "@/data/sync"
import type { ApprovalMode, PermissionHarnessSupport } from "@/gen"
import { useT } from "@/i18n"
import { toast, useHubEvents } from "@/store"

function HarnessMatrix({ harnesses }: { harnesses: PermissionHarnessSupport[] }) {
  const t = useT()
  if (harnesses.length === 0) {
    return (
      <p className="px-1 type-footnote text-muted-foreground" data-permission-harnesses-empty>
        {t("core:approval.harnessesEmpty")}
      </p>
    )
  }
  return (
    <div className="overflow-x-auto rounded-lg border border-border/60 bg-card" data-permission-harnesses>
      <table className="w-full border-collapse text-left type-footnote">
        <thead>
          <tr className="border-b border-border/60 text-muted-foreground">
            <th className="px-3 py-2 font-medium">{t("core:approval.tableHarness")}</th>
            <th className="px-3 py-2 font-medium">{t("core:approval.levels.auto.name")}</th>
            <th className="px-3 py-2 font-medium">{t("core:approval.levels.full_access.name")}</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border/50">
          {harnesses.map((h) => (
            <tr key={h.id} data-permission-harness={h.id}>
              <td className="px-3 py-2 font-medium text-foreground">
                <span className="inline-flex items-center gap-1.5">
                  <HarnessMark harness={h.id} className="size-3.5 shrink-0" />
                  <span>{h.name}</span>
                  {!h.nested_sandbox && (
                    <span className="rounded bg-attention/10 px-1.5 py-0.5 text-[11px] font-normal text-attention" data-outside-sandbox>
                      {t("core:approval.outsideSandboxTag")}
                    </span>
                  )}
                </span>
              </td>
              <td className="px-3 py-2 text-muted-foreground" data-harness-auto={h.auto ? "yes" : "no"}>
                {h.auto ? (
                  <span className="text-foreground">
                    {t("core:approval.supported")}
                    {h.auto_mode && <span className="ml-1 font-mono text-[11px] text-muted-foreground">({h.auto_mode})</span>}
                  </span>
                ) : t("core:approval.notSupported")}
              </td>
              <td className="px-3 py-2 text-muted-foreground" data-harness-full-access={h.full_access ? "yes" : "no"}>
                {h.full_access ? (
                  <span className="text-foreground">
                    {t("core:approval.supported")}
                    {h.full_access_mode && <span className="ml-1 font-mono text-[11px] text-muted-foreground">({h.full_access_mode})</span>}
                  </span>
                ) : t("core:approval.notSupported")}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

export function PermissionPolicyPanel() {
  const t = useT()
  const [confirmOpen, setConfirmOpen] = useState(false)
  const key = ["detail", scopeKey(), "permission-policy"]
  const policy = useQuery({
    queryKey: key,
    queryFn: ({ signal }) => approvalApi.getPolicy(signal),
  }, qc)
  useHubEvents(["approval_policy"], () => void policy.refetch())
  const update = useMutation({
    mutationFn: ({ mode, revision, confirmed }: { mode: ApprovalMode; revision: number; confirmed?: boolean }) =>
      approvalApi.setPolicy(mode, revision, confirmed),
    onSuccess: (value) => {
      qc.setQueryData(key, value)
      void qc.invalidateQueries({ queryKey: key })
    },
    onError: (error) => { toast(error.message); void policy.refetch() },
  }, qc)
  if (!policy.data) return null
  const data = policy.data
  const select = (next: ApprovalMode) => {
    if (!data.editable || update.isPending || next === data.policy.mode) return
    if (next === "full_access") {
      setConfirmOpen(true)
      return
    }
    update.mutate({ mode: next, revision: data.policy.revision })
  }
  const unconfinedNames = data.unconfined_harnesses.join(t("shell:list.sep"))
  const outsideSandboxBullet = unconfinedNames
    ? t("core:approval.confirm.outsideSandboxNamed", { harnesses: unconfinedNames })
    : t("core:approval.confirm.outsideSandboxGeneric")
  return (
    <div className="flex flex-col gap-3" data-permission-policy={data.policy.mode}>
      <AutonomyChoice<ApprovalMode>
        testId="permission-policy"
        label={t("core:approval.title")}
        value={data.policy.mode}
        disabled={!data.editable || update.isPending}
        onChange={select}
        options={[
          {
            value: "ask",
            name: t("core:approval.levels.ask.name"),
            text: t("core:approval.levels.ask.text"),
            icon: <HandIcon />,
          },
          {
            value: "auto",
            name: t("core:approval.levels.auto.name"),
            text: t("core:approval.levels.auto.text"),
            icon: <SparklesIcon />,
          },
          {
            value: "full_access",
            name: t("core:approval.levels.full_access.name"),
            text: t("core:approval.levels.full_access.text"),
            icon: <ShieldOffIcon />,
          },
        ]}
        means={
          <>
            <ShieldAlertIcon aria-hidden />
            <span>
              {t("core:approval.description")}
              {!data.editable && ` ${t("core:approval.ownerOnly")}`}
            </span>
          </>
        }
      />
      <HarnessMatrix harnesses={data.harnesses} />
      <AlertDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        danger
        size="md"
        title={t("core:approval.confirm.title")}
        description={t("core:approval.confirm.desc")}
        cancelLabel={t("common:action.cancel")}
        confirmLabel={t("core:approval.confirm.proceed")}
        data-full-access-confirm
        onConfirm={() => update.mutateAsync({ mode: "full_access", revision: data.policy.revision, confirmed: true })}
      >
        <ul className="list-disc space-y-2 pl-5 type-callout text-foreground" data-full-access-risks>
          <li>{t("core:approval.confirm.noPrompts")}</li>
          <li>{t("core:approval.confirm.writeAndNetwork")}</li>
          <li>{t("core:approval.confirm.steveSandbox")}</li>
          <li data-full-access-outside-sandbox>{outsideSandboxBullet}</li>
        </ul>
      </AlertDialog>
    </div>
  )
}

export function SessionPermission({ id, showApplied = true }: { id: string; showApplied?: boolean }) {
  const t = useT()
  const state = useQuery({
    queryKey: ["detail", scopeKey(), "permission-policy", id],
    queryFn: ({ signal }) => approvalApi.sessionMode(id, signal),
  }, qc)
  useHubEvents(["approval_policy", "patch", "session"], () => void state.refetch(), 300, {
    filter: (e) => e.type !== "patch" || e.ops.some((op) => op.type === "session" && op.id === id),
  })
  const d = state.data
  if (!d) return null
  const pending = d.desired ?? (d.applied ? d.target : null)
  const applied = (d.unknown || d.released) ? null : d.applied
  const formatMode = (mode: string | null) => {
    if (!mode) return null
    if ((mode === "auto" || mode === "full_access") && d.native_id && mode === d.policy) {
      return d.native_id
    }
    return mode
  }
  const modeLabel = formatMode(applied)
  const pendingLabel = formatMode(pending)
  const levelLabel = t(`core:approval.levels.${d.policy}.name`)
  return (
    <span data-session-permission={d.status} data-session-policy={d.policy}>
      {t(`core:approval.status.${d.status}`, { level: levelLabel })}
      {showApplied && <>{" · "}{t("core:approval.applied", { mode: modeLabel ?? t("core:approval.unreported") })}</>}
      {!d.refused && pendingLabel && pending !== applied && <>{" · "}{t("core:approval.pending", { mode: pendingLabel })}</>}
      {d.refused && <>{" · "}<span className="text-attention" data-approval-refused>{t("core:approval.refused", { reason: d.refused })}</span></>}
    </span>
  )
}
