/* A Node's version card (Hub-managed Node upgrades): what device-model's upgradeCard says, drawn as one of its states.
 * On the Hub's build: a badge and the last upgrade. Behind: what upgrading from here does and the owner's two buttons.
 * Under way: five steps, the one it is on moving, and what it waits for. Failed and gone back: the step that failed,
 * the plain error and "try again". Upgraded only on its own machine: the command to run there. Offline: its last
 * version. The Hub decides whether it can (the Node says so); the page only shows it. Every state keeps the policy. */
import { useState } from "react"
import { ArrowUpCircleIcon, CopyIcon, RotateCcwIcon, TriangleAlertIcon, ActivityIcon, InfoIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Badge } from "@/components/ui/badge"
import { Select } from "@/view-shared"
import { api, type RNodeUpgradeRun, type RUpgradePolicy } from "@/api"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { date, span } from "@/i18n/format"
import { agoText, upgradeCard, UPGRADE_STEPS, type NodeView, type StepState } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard, DRow } from "@/devices-ui"
import { cn } from "cn"

const BAR: Record<StepState, string> = { done: "bg-success", active: "bg-blue device-step-live", todo: "bg-muted", bad: "bg-destructive", back: "bg-attention" }
const WORD: Record<StepState, string> = { done: "text-muted-foreground", active: "font-medium text-foreground", todo: "text-tertiary", bad: "text-destructive", back: "text-attention" }

/** five segments with their names: done green, the current one blue and moving, a failed one red, the way back amber */
function Steps({ steps, from }: { steps: StepState[]; from?: string | null }) {
  const t = useT()
  return <ol className="grid grid-cols-5 gap-1.5 px-5" data-upgrade-steps>
    {UPGRADE_STEPS.map((k, i) => <li key={k} className="flex min-w-0 flex-col gap-1.5" data-step={k} data-step-state={steps[i]}>
      <span className={cn("h-1 rounded-full", BAR[steps[i]])} aria-hidden />
      <span className={cn("type-footnote [overflow-wrap:anywhere]", WORD[steps[i]])}>{steps[i] === "back" ? t("devices:upgrade.back", { version: from ?? "?" }) : t(`devices:upgrade.step.${k}`)}</span>
    </li>)}
  </ol>
}

/** "6 分钟前 · 由 owner 发起" */
function RunLine({ run }: { run: RNodeUpgradeRun }) {
  const t = useT()
  return <span title={date(run.updated, { dateStyle: "medium", timeStyle: "medium" })}>{agoText(run.updated)} · {t("devices:upgrade.by", { who: run.requested_by === "hub" ? t("devices:upgrade.byHub") : run.requested_by })}</span>
}

export function NodeVersion({ v, editable }: { v: NodeView; editable: boolean }) {
  const t = useT(), { hubVersion, upgradeDefault, reloadNodes } = useDevices()
  const [busy, setBusy] = useState(false)
  const c = upgradeCard(v, hubVersion), node = v.node, u = node?.upgrade
  /* the Hub's own Node is the Hub's build: it is upgraded with the Hub */
  if (c.kind === "builtIn") return <DCard icon={<ArrowUpCircleIcon />} title={t("device:upgrade.title")} sub={<span data-node-build>{t("devices:upgrade.builtIn", { version: hubVersion || "?" })}</span>}
    action={<a className="type-footnote text-blue hover:underline" href="#/settings/about">{t("devices:upgrade.hubUpdates")}</a>} data-node-upgrade="builtIn" />
  if (c.kind === "none" || !node || !u) return null
  const health = upgradeDefault?.health_secs, drain = upgradeDefault?.drain_secs
  const policies = (["auto", "notify", "off"] as const).map((p) => ({ v: p, l: t(`device:upgrade.policy.${p}`) }))
  const setPolicy = async (p: string) => {
    try { await api.nodeUpgradePolicy(node.node_id, p === "default" ? "" : (p as RUpgradePolicy)); reloadNodes() } catch (e) { toast((e as Error).message) }
  }
  const upgrade = async (force: boolean) => {
    setBusy(true)
    try { await api.upgradeNode(node.node_id, force); toast(t("device:upgrade.started")); reloadNodes() } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  /* the upgrade under way stops waiting: its own route, never a second start */
  const switchNow = async () => {
    setBusy(true)
    try { await api.switchNodeUpgrade(node.node_id); toast(t("devices:upgrade.switching")); reloadNodes() } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  const sub = c.kind === "running" ? t("devices:upgrade.runningSub", { from: c.run.from_version ?? "?", to: c.run.version, ago: agoText(c.run.created) })
    : c.kind === "offline" ? t("devices:upgrade.offlineSub", { version: c.last ?? "?" })
    : c.kind === "current" ? (c.last ? t("devices:upgrade.currentLast", { version: v.version ?? "", ago: agoText(c.last.updated), from: c.last.from_version ?? "?", to: c.last.version }) : (v.version ?? ""))
    : c.kind === "devHub" ? t("device:upgrade.reason.hub_dev_build")
    : c.kind === "unknown" ? t("device:upgrade.reason.unknown")
    : t("devices:upgrade.runs", { v: v.version ?? "?", hub: hubVersion || "?" })
  const badge = c.kind === "current" ? <Badge tone="success" size="sm" dot>{t("devices:upgrade.same")}</Badge>
    : c.kind === "outdated" ? <Badge tone="warning" size="sm" icon={<ArrowUpCircleIcon />}>{t("devices:upgrade.can")}</Badge>
    : c.kind === "running" ? <Badge tone="info" size="sm" dot live>{t("devices:upgrade.upgrading")}</Badge>
    : c.kind === "failed" ? <Badge tone="danger" size="sm" icon={<RotateCcwIcon />}>{t("devices:upgrade.rolledBack")}</Badge>
    : c.kind === "manual" && c.behind ? <Badge tone="danger" size="sm">{t("devices:upgrade.manualBadge")}</Badge>
    : c.kind === "offline" ? <Badge tone="neutral" size="sm">{t("devices:upgrade.unknownBadge")}</Badge> : null
  const running = node.running ?? 0
  return (
    <DCard icon={<ArrowUpCircleIcon />} title={t("device:upgrade.title")} sub={<span data-node-build>{sub}</span>} action={badge} data-node-upgrade={c.kind} data-node-upgrade-reason={u.reason}>
      <div className="flex flex-col gap-4 pb-4">
        {c.kind === "outdated" && <div className="mx-5 flex gap-2.5 rounded-lg bg-attention/10 px-3.5 py-2.5 type-callout text-foreground" data-upgrade-note>
          <ArrowUpCircleIcon className="mt-0.5 size-4 shrink-0 text-attention" aria-hidden />
          <span>{t("devices:upgrade.outdatedNote", { from: c.from ?? "?", health: health ? span(health * 1000) : "—" })}</span>
        </div>}
        {(c.kind === "running" || (c.kind === "failed" && c.steps)) && <Steps steps={c.kind === "running" ? c.steps : c.steps!} from={c.kind === "failed" ? c.run.from_version : null} />}
        {c.kind === "running" && <div className="mx-5 flex gap-2.5 rounded-lg bg-blue/8 px-3.5 py-2.5 type-callout text-foreground" data-upgrade-drain>
          <ActivityIcon className="mt-0.5 size-4 shrink-0 text-blue" aria-hidden />
          <span className="flex flex-col gap-0.5">
            <span>{["staged", "draining"].includes(c.run.phase) && running > 0 ? t("devices:upgrade.drainNote", { count: running }) : t(`device:upgrade.phase.${c.run.phase as "draining"}`, { defaultValue: c.run.phase })}</span>
            {health ? <span className="type-footnote text-muted-foreground">{t("devices:upgrade.healthNote", { health: span(health * 1000), from: c.run.from_version ?? "?" })}</span> : null}
            {c.run.phase === "draining" && drain ? <span className="type-footnote text-muted-foreground">{t("devices:upgrade.drainLimit", { drain: span(drain * 1000) })}</span> : null}
          </span>
        </div>}
        {c.kind === "failed" && <div className="mx-5 flex gap-2.5 rounded-lg bg-destructive/8 px-3.5 py-3 type-callout" role="alert" data-upgrade-failed>
          <TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
          <span className="flex min-w-0 flex-col gap-1.5">
            <strong className="font-semibold text-foreground">{t("devices:upgrade.failedTitle", { to: c.run.version, from: c.run.from_version ?? "?" })}</strong>
            {c.run.error && <code className="type-code whitespace-pre-wrap text-muted-foreground [overflow-wrap:anywhere]" data-node-upgrade-error>{c.run.error}</code>}
            <span className="type-footnote text-muted-foreground"><RunLine run={c.run} /> · {t("devices:upgrade.noRetry")}</span>
          </span>
        </div>}
        {c.kind === "manual" && <div className="mx-5 flex gap-2.5 rounded-lg bg-muted/70 px-3.5 py-2.5 type-callout" data-upgrade-manual>
          <InfoIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <span className="flex min-w-0 flex-col gap-1.5"><span>{t(c.behind ? "devices:upgrade.manualBehind" : "devices:upgrade.manual")}</span><code className="type-code select-all">steve node update</code></span>
        </div>}
        <div className="border-t border-border/70">
          <DRow title={t("device:upgrade.policyTitle")} sub={t("devices:upgrade.policySub")} className="flex-wrap">
            <Select align="end" testId="node-upgrade-policy" label={t("device:upgrade.policyTitle")} value={u.own_policy ?? "default"} onChange={(p) => void setPolicy(p)} disabled={!editable}
              options={[{ v: "default", l: t("device:upgrade.followDefault", { policy: t(`device:upgrade.policy.${upgradeDefault?.policy ?? u.policy}`) }) }, ...policies]} />
          </DRow>
        </div>
        {editable && (c.kind === "outdated" || c.kind === "failed" || (c.kind === "running" && ["staged", "draining"].includes(c.run.phase) && running > 0)) && <div className="flex flex-wrap items-center justify-end gap-2 px-5">
          {c.kind === "failed" && c.run.error && <Button size="sm" variant="ghost" onClick={() => void navigator.clipboard?.writeText(c.run.error ?? "").then(() => toast(t("devices:upgrade.copied")))} data-upgrade-copy><CopyIcon />{t("devices:upgrade.copy")}</Button>}
          {c.kind === "outdated" && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void upgrade(true)} data-node-upgrade-force>{t("device:upgrade.force")}</Button>}
          {c.kind === "running" && <Button size="sm" variant="ghost" className="text-destructive" disabled={busy} onClick={() => void switchNow()} data-node-upgrade-switch>{t("devices:upgrade.switchNow", { count: running })}</Button>}
          {u.can && c.kind !== "running" && <Button size="sm" disabled={busy} onClick={() => void upgrade(false)} data-node-upgrade-start>{busy ? <Spinner className="size-3" /> : null}{c.kind === "failed" ? t("devices:upgrade.retry") : t("devices:upgrade.to", { version: c.kind === "outdated" ? c.to : hubVersion })}</Button>}
        </div>}
      </div>
    </DCard>
  )
}
