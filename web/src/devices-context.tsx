/* 本机个人配置：这台设备上的 agent 是否加载设备主人的个人全局配置（用户级 AGENTS.md / CLAUDE.md、技能、子代理、命令）。
   跟随工作空间设置，或单独为这台设备打开 / 关闭；项目自己的文件和 Steve 注入的内容不受影响，新会话生效。 */
import { useEffect, useState } from "react"
import { UserCogIcon } from "lucide-react"
import { api, type RNode } from "@/api"
import { Panel, PanelRow } from "@/panel"
import { SetSelect } from "@/settings-controls"
import { st } from "@/store"
import { useT } from "@/i18n"

type Choice = "inherit" | "on" | "off"
const choiceOf = (v: boolean | null | undefined): Choice => v == null ? "inherit" : v ? "on" : "off"

export function NodeContext({ node, value }: { node: string; value?: RNode["owner_context"] }) {
  const t = useT()
  const [v, setV] = useState<Choice | null>(value === undefined ? null : choiceOf(value))
  const [error, setError] = useState("")
  useEffect(() => {
    if (value !== undefined) { setV(choiceOf(value)); return }
    let live = true
    void api.nodes().then(r => { const n = r.nodes.find(x => x.node_id === node); if (live && n) setV(choiceOf(n.owner_context)) }).catch(() => {})
    return () => { live = false }
  }, [node, value])
  if (v === null) return null
  const inherited = st.cfg.ownerContext === "off" ? t("shell:devices.ownerContext.off") : t("shell:devices.ownerContext.on")
  const save = async (next: string) => {
    setError("")
    const prev = v
    setV(next as Choice)
    try { await api.nodeOwnerContext(node, next as Choice) } catch (e) { setV(prev); setError((e as Error).message) }
  }
  return (
    <Panel k="owner-context" data-node-owner-context icon={<UserCogIcon />} title={t("shell:devices.ownerContext.title")} sub={t("shell:devices.ownerContext.sub")}>
      <PanelRow title={t("shell:devices.ownerContext.title")} sub={t("shell:devices.ownerContext.effect")}>
        <SetSelect testId="node-owner-context" label={t("shell:devices.ownerContext.title")} value={v} onChange={save}
          options={[["inherit", t("shell:devices.ownerContext.inherit", { value: inherited })], ["on", t("shell:devices.ownerContext.on")], ["off", t("shell:devices.ownerContext.off")]]} />
      </PanelRow>
      {error && <div className="px-5 pb-3 type-footnote text-destructive" role="alert">{error}</div>}
    </Panel>
  )
}
