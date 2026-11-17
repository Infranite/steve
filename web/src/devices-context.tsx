/* Personal agent config: whether a Node owner's sessions on that machine load the owner's personal global config
 * (user-level AGENTS.md / CLAUDE.md, skills, subagents, commands). The Hub has one default for every Node (the Hub
 * page; a Node serves every workspace, so it is not a workspace setting) and each Node may follow it or choose for
 * itself (the Node's page). The project's own files and what Steve adds always load; a change applies to new sessions. */
import { useEffect, useState } from "react"
import { UserCogIcon } from "lucide-react"
import { api } from "@/api"
import { SetSelect } from "@/settings-controls"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { useDevices } from "@/devices-data"
import { DCard, DRow } from "@/devices-ui"
import type { NodeView } from "@/device-model"

type Choice = "inherit" | "on" | "off"
const choiceOf = (v: boolean | null | undefined): Choice => (v == null ? "inherit" : v ? "on" : "off")

/** a Node's own choice: follow the Hub's default, load, or don't */
export function NodeContext({ v, editable }: { v: NodeView; editable: boolean }) {
  const t = useT(), { contextDefault, reloadNodes } = useDevices()
  const value = choiceOf(v.node?.owner_context)
  const [choice, setChoice] = useState<Choice>(value), [error, setError] = useState("")
  useEffect(() => setChoice(value), [value])
  if (!v.node || v.state === "revoked") return null
  const save = async (next: string) => {
    setError("")
    const prev = choice
    setChoice(next as Choice)
    try { await api.nodeOwnerContext(v.id, next as Choice); reloadNodes() } catch (e) { setChoice(prev); setError((e as Error).message) }
  }
  const def = contextDefault ? t(`shell:devices.ownerContext.${contextDefault}`) : null
  return (
    <DCard icon={<UserCogIcon />} title={t("shell:devices.ownerContext.title")} sub={t("shell:devices.ownerContext.sub")} data-node-owner-context={choice}>
      <div className="border-t border-border/70">
        <DRow title={t("devices:context.choice")} className="flex-wrap">
          <SetSelect testId="node-owner-context" label={t("shell:devices.ownerContext.title")} value={choice} onChange={(x) => void save(x)} disabled={!editable} className="w-52"
            options={[["inherit", def ? t("shell:devices.ownerContext.inherit", { value: def }) : t("devices:context.inheritPlain")], ["on", t("shell:devices.ownerContext.on")], ["off", t("shell:devices.ownerContext.off")]]} />
        </DRow>
        <p className="-mt-1 px-5 pb-3 type-footnote text-pretty text-muted-foreground">{t("shell:devices.ownerContext.effect")}</p>
      </div>
      {error && <div className="px-5 pb-3 type-footnote text-destructive" role="alert">{error}</div>}
    </DCard>
  )
}

/** the Hub's default, for every Node without a choice of its own (the instance owner sets it) */
export function ContextDefaultRow({ editable }: { editable: boolean }) {
  const t = useT(), { contextDefault, reloadDefaults } = useDevices()
  const [v, setV] = useState(contextDefault)
  useEffect(() => setV(contextDefault), [contextDefault])
  if (!v) return null
  const save = async (next: string) => {
    const prev = v
    setV(next as "on" | "off")
    try { await api.setNodeContextDefault(next as "on" | "off"); reloadDefaults() } catch (e) { setV(prev); toast((e as Error).message) }
  }
  return (
    <DRow title={t("shell:devices.ownerContext.title")} sub={t("devices:context.defaultSub")} className="flex-wrap" data-context-default={v}>
      <SetSelect testId="node-context-default" label={t("shell:devices.ownerContext.title")} value={v} onChange={(x) => void save(x)} disabled={!editable}
        options={[["on", t("shell:devices.ownerContext.on")], ["off", t("shell:devices.ownerContext.off")]]} />
    </DRow>
  )
}
