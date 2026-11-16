/* A paired Node's build against the Hub's, its upgrade policy, the last upgrade and the instance
 * owner's "upgrade to the Hub's version". The Hub decides whether it can (the Node says so); the page
 * only shows it. */
import { useEffect, useState } from "react"
import { ArrowUpCircleIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Panel, PanelRow, Tag, type Tone } from "@/panel"
import { Select } from "@/view-shared"
import { api, type RBuild, type RNode, type RUpgradePolicy } from "@/api"
import { toast, tm } from "@/store"
import { useT } from "@/i18n"

const short = (b?: RBuild | null) => (b ? `${b.version} · ${b.commit.slice(0, 7)}` : "—")

const phaseTone = (phase: string): Tone => (phase === "succeeded" ? "ok" : ["failed", "rolled_back", "unknown"].includes(phase) ? "bad" : phase === "abandoned" ? "muted" : "run")

export function NodeUpgradePanel({ node, reload }: { node: RNode; reload: () => void }) {
  const t = useT()
  const [hub, setHub] = useState<{ policy: RUpgradePolicy; hub?: RBuild | null } | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => { api.nodeUpgradeDefault().then(setHub).catch(() => setHub(null)) }, [node.node_id])
  const u = node.upgrade
  if (!u || node.state === "revoked") return null
  const latest = u.latest
  const running = !!latest && !["succeeded", "failed", "rolled_back", "abandoned", "unknown"].includes(latest.phase)
  const policies = (["auto", "notify", "off"] as const).map(v => ({ v, l: t(`device:upgrade.policy.${v}`) }))
  const setPolicy = async (v: string) => {
    try { await api.nodeUpgradePolicy(node.node_id, v === "default" ? "" : v as RUpgradePolicy); reload() } catch (e) { toast((e as Error).message) }
  }
  const setDefault = async (v: string) => {
    try { setHub(await api.setNodeUpgradeDefault(v as RUpgradePolicy).then(r => ({ ...hub, policy: r.policy }))); reload() } catch (e) { toast((e as Error).message) }
  }
  const upgrade = async (force: boolean) => {
    setBusy(true)
    try { await api.upgradeNode(node.node_id, force); toast(t("device:upgrade.started")); reload() } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  return <Panel k="node-upgrade" icon={<ArrowUpCircleIcon />} title={t("device:upgrade.title")} sub={t(`device:upgrade.reason.${u.reason}`, { defaultValue: u.reason })} data-node-upgrade={u.reason}>
    <PanelRow title={t("device:upgrade.nodeBuild")}><span className="type-callout font-mono tabular-nums text-muted-foreground" data-node-build>{short(u.build)}</span></PanelRow>
    <PanelRow title={t("device:upgrade.hubBuild")}><span className="type-callout font-mono tabular-nums text-muted-foreground" data-hub-build>{short(hub?.hub)}</span></PanelRow>
    <PanelRow title={t("device:upgrade.policyTitle")} sub={t("device:upgrade.policySub")}>
      <Select align="end" testId="node-upgrade-policy" value={u.own_policy ?? "default"} onChange={setPolicy}
        options={[{ v: "default", l: t("device:upgrade.followDefault", { policy: t(`device:upgrade.policy.${hub?.policy ?? u.policy}`) }) }, ...policies]} />
    </PanelRow>
    {hub && <PanelRow title={t("device:upgrade.defaultTitle")} sub={t("device:upgrade.defaultSub")}>
      <Select align="end" testId="node-upgrade-default" value={hub.policy} onChange={setDefault} options={policies} />
    </PanelRow>}
    {latest && <PanelRow title={t("device:upgrade.last")} sub={<span className="break-all whitespace-normal" data-node-upgrade-error>{tm(latest.updated)} · {latest.from_version ?? "?"} → {latest.version}{latest.error ? ` · ${latest.error}` : ""}</span>}>
      <Tag tone={phaseTone(latest.phase)} data-node-upgrade-phase={latest.phase}>{t(`device:upgrade.phase.${latest.phase}`, { defaultValue: latest.phase })}</Tag>
    </PanelRow>}
    {u.can && !running && <div className="flex flex-wrap items-center justify-end gap-2 px-5 py-3">
      <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void upgrade(true)} data-node-upgrade-force>{t("device:upgrade.force")}</Button>
      <Button size="sm" disabled={busy} onClick={() => void upgrade(false)} data-node-upgrade-start>{busy ? <Spinner className="size-3" /> : null}{t("device:upgrade.start")}</Button>
    </div>}
    <p className="px-5 pb-3 type-footnote text-muted-foreground">{t("device:upgrade.how")}</p>
  </Panel>
}
